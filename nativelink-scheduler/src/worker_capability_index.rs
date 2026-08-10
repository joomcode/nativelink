// Copyright 2024 The NativeLink Authors. All rights reserved.
//
// Licensed under the Functional Source License, Version 1.1, Apache 2.0 Future License (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    See LICENSE file for details
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Worker capability index for fast worker matching.
//!
//! This module provides an index that accelerates worker matching by property.
//! Instead of iterating all workers for each action, we maintain an inverted index
//! that maps property values to sets of workers that have those values.
//!
//! ## Complexity Analysis
//!
//! Without index: O(W × P) where W = workers, P = properties per action
//! With index: O(P × W / 64) word operations for exact properties, plus
//!   O(W' × P') for minimum properties, where W' = filtered workers and
//!   P' = minimum property count (typically small)
//!
//! Each worker holds a dense slot number, so the candidate sets are bitmaps.
//! A caller that matches many actions in one pass reuses one candidate bitmap,
//! so the lookup of a candidate set needs no allocation.

use std::collections::{HashMap, HashSet};

use nativelink_util::action_messages::WorkerId;
use nativelink_util::platform_properties::{PlatformProperties, PlatformPropertyValue};
use tracing::info;

/// The dense number of a worker inside the index.
pub type WorkerSlot = u32;

const BITS_PER_WORD: usize = u64::BITS as usize;

/// A set of worker slots held as a bitmap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkerSlotSet {
    words: Vec<u64>,
}

impl WorkerSlotSet {
    #[must_use]
    pub const fn new() -> Self {
        Self { words: Vec::new() }
    }

    pub fn insert(&mut self, slot: WorkerSlot) {
        let (word, bit) = Self::position(slot);
        if word >= self.words.len() {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= 1u64 << bit;
    }

    pub fn remove(&mut self, slot: WorkerSlot) {
        let (word, bit) = Self::position(slot);
        if let Some(value) = self.words.get_mut(word) {
            *value &= !(1u64 << bit);
        }
    }

    #[must_use]
    pub fn contains(&self, slot: WorkerSlot) -> bool {
        let (word, bit) = Self::position(slot);
        self.words
            .get(word)
            .is_some_and(|value| value & (1u64 << bit) != 0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|word| *word == 0)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.words
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    pub fn clear(&mut self) {
        self.words.clear();
    }

    /// Replaces the content of this set with the content of `other`.
    pub fn copy_from(&mut self, other: &Self) {
        self.words.clear();
        self.words.extend_from_slice(&other.words);
    }

    /// Removes every slot that is not also in `other`.
    pub fn intersect_with(&mut self, other: &Self) {
        if other.words.len() < self.words.len() {
            self.words.truncate(other.words.len());
        }
        for (word, mask) in self.words.iter_mut().zip(other.words.iter()) {
            *word &= *mask;
        }
    }

    /// Iterates the slots of the set in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = WorkerSlot> + '_ {
        self.words.iter().enumerate().flat_map(|(index, word)| {
            let base = WorkerSlot::try_from(index * BITS_PER_WORD).unwrap_or(WorkerSlot::MAX);
            let mut bits = *word;
            core::iter::from_fn(move || {
                if bits == 0 {
                    return None;
                }
                let bit = bits.trailing_zeros();
                bits &= bits - 1;
                Some(base + bit)
            })
        })
    }

    const fn position(slot: WorkerSlot) -> (usize, usize) {
        let slot = slot as usize;
        (slot / BITS_PER_WORD, slot % BITS_PER_WORD)
    }
}

/// A property key-value pair used for indexing.
#[derive(Clone, Hash, Eq, PartialEq, Debug)]
struct PropertyKey {
    name: String,
    value: PlatformPropertyValue,
}

/// Index structure for fast worker capability lookup.
///
/// Maintains an inverted index from property values to worker slots.
/// Only indexes `Exact` and `Priority` properties since `Minimum` properties
/// are dynamic and require runtime comparison.
#[derive(Debug, Default)]
pub struct WorkerCapabilityIndex {
    /// Maps `(property_name, property_value)` -> Set of worker slots with that property.
    /// Only contains `Exact`, `Priority` and `Unknown` properties.
    exact_index: HashMap<PropertyKey, WorkerSlotSet>,

    /// Maps `property_name` -> Set of worker slots that have this property (any value).
    /// Used for fast "has property" checks for `Priority` and `Minimum` properties.
    property_presence: HashMap<String, WorkerSlotSet>,

    /// Set of all indexed worker slots.
    all_workers: WorkerSlotSet,

    /// Maps a worker to its dense slot number.
    worker_to_slot: HashMap<WorkerId, WorkerSlot>,

    /// Maps a dense slot number back to its worker. `None` means the slot is free.
    slot_to_worker: Vec<Option<WorkerId>>,

    /// Slots of workers that left the pool. They are reused by the next worker.
    free_slots: Vec<WorkerSlot>,
}

impl WorkerCapabilityIndex {
    /// Creates a new empty capability index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a worker to the index with their platform properties.
    pub fn add_worker(&mut self, worker_id: &WorkerId, properties: &PlatformProperties) {
        // A worker that reconnects keeps its id, so drop the stale entry first.
        if self.worker_to_slot.contains_key(worker_id) {
            self.remove_worker(worker_id);
        }
        let slot = self.allocate_slot(worker_id);

        for (name, value) in &properties.properties {
            // Track property presence
            self.property_presence
                .entry(name.clone())
                .or_default()
                .insert(slot);

            match value {
                PlatformPropertyValue::Exact(_)
                | PlatformPropertyValue::Priority(_)
                | PlatformPropertyValue::Unknown(_) => {
                    // Index exact-match properties
                    let key = PropertyKey {
                        name: name.clone(),
                        value: value.clone(),
                    };
                    self.exact_index.entry(key).or_default().insert(slot);
                }
                PlatformPropertyValue::Minimum(_) | PlatformPropertyValue::Ignore(_) => {
                    // Minimum properties are tracked via `property_presence` only.
                    // Their actual values are checked at runtime since they're dynamic.

                    // Ignore properties we just drop
                }
            }
        }
    }

    /// Removes a worker from the index.
    pub fn remove_worker(&mut self, worker_id: &WorkerId) {
        let Some(slot) = self.worker_to_slot.remove(worker_id) else {
            return;
        };
        self.all_workers.remove(slot);

        // Remove from exact index
        self.exact_index.retain(|_, workers| {
            workers.remove(slot);
            !workers.is_empty()
        });

        // Remove from presence index
        self.property_presence.retain(|_, workers| {
            workers.remove(slot);
            !workers.is_empty()
        });

        if let Some(entry) = self.slot_to_worker.get_mut(slot as usize) {
            *entry = None;
        }
        self.free_slots.push(slot);
    }

    /// Returns the slot of the given worker.
    #[must_use]
    pub fn slot_of(&self, worker_id: &WorkerId) -> Option<WorkerSlot> {
        self.worker_to_slot.get(worker_id).copied()
    }

    /// Returns the worker that holds the given slot.
    #[must_use]
    pub fn worker_for_slot(&self, slot: WorkerSlot) -> Option<&WorkerId> {
        self.slot_to_worker.get(slot as usize)?.as_ref()
    }

    /// Finds the slots of the workers that can satisfy the given action properties.
    ///
    /// The result replaces the content of `candidates`. The return value is
    /// `true` when at least one candidate was found.
    ///
    /// IMPORTANT: This method returns candidates based on STATIC properties only.
    /// - Exact and Unknown properties are fully matched
    /// - Priority properties just require the key to exist
    /// - Minimum properties return workers that HAVE the property (presence check only)
    ///
    /// The caller MUST still verify Minimum property values at runtime because
    /// worker resources change dynamically as jobs are assigned/completed.
    pub fn find_matching_slots(
        &self,
        action_properties: &PlatformProperties,
        full_worker_logging: bool,
        candidates: &mut WorkerSlotSet,
    ) -> bool {
        candidates.clear();

        if self.all_workers.is_empty() {
            if full_worker_logging {
                info!("No workers available to match!");
            }
            return false;
        }

        if action_properties.properties.is_empty() {
            // No properties required, all workers match
            candidates.copy_from(&self.all_workers);
            return true;
        }

        let mut initialized = false;

        for (name, value) in &action_properties.properties {
            let matching = match value {
                PlatformPropertyValue::Exact(_) | PlatformPropertyValue::Unknown(_) => {
                    // Look up workers with exact match
                    let key = PropertyKey {
                        name: name.clone(),
                        value: value.clone(),
                    };
                    let Some(matching) = self.exact_index.get(&key) else {
                        if full_worker_logging {
                            let values: Vec<_> = self
                                .exact_index
                                .keys()
                                .filter(|property_key| &property_key.name == name)
                                .map(|property_key| property_key.value.clone())
                                .collect();
                            info!(
                                "No candidate workers due to a lack of matching '{name}' = {value:?}. Workers have: {values:?}"
                            );
                        }
                        candidates.clear();
                        return false;
                    };
                    matching
                }
                PlatformPropertyValue::Priority(_) | PlatformPropertyValue::Minimum(_) => {
                    // Priority: just requires the key to exist
                    // Minimum: worker must have the property (value checked at runtime by caller)
                    // We only check presence here because Minimum values are DYNAMIC -
                    // they change as jobs are assigned to workers.
                    let Some(matching) = self.property_presence.get(name) else {
                        if full_worker_logging {
                            info!(
                                "No candidate workers due to a lack of key '{name}'. Job asked for {value:?}"
                            );
                        }
                        candidates.clear();
                        return false;
                    };
                    matching
                }
                PlatformPropertyValue::Ignore(_) => continue,
            };

            if initialized {
                candidates.intersect_with(matching);
            } else {
                candidates.copy_from(matching);
                initialized = true;
            }

            // Early exit if no candidates
            if candidates.is_empty() {
                if full_worker_logging {
                    info!("No candidate workers left after checking '{name}' = {value:?}");
                }
                candidates.clear();
                return false;
            }
        }

        if !initialized {
            // Every property was an `Ignore`, so all workers match.
            candidates.copy_from(&self.all_workers);
        }

        !candidates.is_empty()
    }

    /// Finds workers that can satisfy the given action properties.
    ///
    /// Prefer [`Self::find_matching_slots`] when matching more than one action,
    /// because this method allocates a new set on every call.
    #[must_use]
    pub fn find_matching_workers(
        &self,
        action_properties: &PlatformProperties,
        full_worker_logging: bool,
    ) -> HashSet<WorkerId> {
        let mut candidates = WorkerSlotSet::new();
        if !self.find_matching_slots(action_properties, full_worker_logging, &mut candidates) {
            return HashSet::new();
        }
        candidates
            .iter()
            .filter_map(|slot| self.worker_for_slot(slot).cloned())
            .collect()
    }

    /// Returns the number of indexed workers.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.worker_to_slot.len()
    }

    /// Returns true if the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.worker_to_slot.is_empty()
    }

    fn allocate_slot(&mut self, worker_id: &WorkerId) -> WorkerSlot {
        let slot = if let Some(slot) = self.free_slots.pop() {
            self.slot_to_worker[slot as usize] = Some(worker_id.clone());
            slot
        } else {
            let slot = WorkerSlot::try_from(self.slot_to_worker.len()).unwrap_or(WorkerSlot::MAX);
            self.slot_to_worker.push(Some(worker_id.clone()));
            slot
        };
        self.worker_to_slot.insert(worker_id.clone(), slot);
        self.all_workers.insert(slot);
        slot
    }
}
