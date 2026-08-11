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

use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use std::sync::Arc;
use std::time::{Instant, UNIX_EPOCH};

use async_lock::RwLock;
use lru::LruCache;
use nativelink_config::schedulers::WorkerAllocationStrategy;
use nativelink_error::{Code, Error, ResultExt, error_if, make_err, make_input_err};
use nativelink_metric::{
    MetricFieldData, MetricKind, MetricPublishKnownKindData, MetricsComponent,
    RootMetricsComponent, group,
};
use nativelink_proto::com::github::trace_machina::nativelink::events::{
    Event, OriginEvent, ResponseEvent, event, response_event,
};
use nativelink_proto::com::github::trace_machina::nativelink::remote_execution::ActionResourceUsage;
use nativelink_util::action_messages::{OperationId, WorkerId};
use nativelink_util::metrics::{WORKER_POOL_INSTANCE, WORKER_POOL_METRICS, WorkerPoolMetricAttrs};
use nativelink_util::operation_state_manager::{UpdateOperationType, WorkerStateManager};
use nativelink_util::origin_event::get_node_id;
use nativelink_util::platform_properties::PlatformProperties;
use nativelink_util::shutdown_guard::ShutdownGuard;
use opentelemetry::KeyValue;
use tokio::sync::{Notify, mpsc};
use tonic::async_trait;
use tracing::{error, info, trace, warn};
use uuid::Uuid;

/// Metrics for tracking scheduler performance.
#[derive(Debug, Default)]
pub struct SchedulerMetrics {
    /// Total number of worker additions.
    pub workers_added: AtomicU64,
    /// Total number of worker removals.
    pub workers_removed: AtomicU64,
    /// Total number of `find_worker_for_action` calls.
    pub find_worker_calls: AtomicU64,
    /// Total number of successful worker matches.
    pub find_worker_hits: AtomicU64,
    /// Total number of failed worker matches (no worker found).
    pub find_worker_misses: AtomicU64,
    /// Total time spent in `find_worker_for_action` (nanoseconds).
    pub find_worker_time_ns: AtomicU64,
    /// Total number of workers iterated during find operations.
    pub workers_iterated: AtomicU64,
    /// Total number of action dispatches.
    pub actions_dispatched: AtomicU64,
    /// Total number of keep-alive updates.
    pub keep_alive_updates: AtomicU64,
    /// Total number of worker timeouts.
    pub worker_timeouts: AtomicU64,
}

use crate::platform_property_manager::PlatformPropertyManager;
use crate::worker::{
    ActionInfoWithProps, Worker, WorkerState, WorkerTimestamp, WorkerUpdate,
    reduce_platform_properties,
};
use crate::worker_capability_index::{WorkerCapabilityIndex, WorkerSlot, WorkerSlotSet};
use crate::worker_registry::SharedWorkerRegistry;
use crate::worker_scheduler::WorkerScheduler;

/// How much work the worker pool can accept at a point in time.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkerPoolCapacity {
    /// Number of workers that can accept at least one more action.
    pub available_workers: usize,
    /// Total number of free inflight slots over all available workers.
    /// `None` means that at least one available worker has no inflight limit.
    pub free_slots: Option<u64>,
}

#[derive(Debug)]
pub struct WorkerSchedulerMetrics {
    attrs: WorkerPoolMetricAttrs,
    instance_name: String,
}

impl WorkerSchedulerMetrics {
    #[must_use]
    pub fn new(instance_name: impl Into<String>) -> Self {
        let instance_name = instance_name.into();
        let base_attrs = vec![KeyValue::new(WORKER_POOL_INSTANCE, instance_name.clone())];
        Self {
            attrs: WorkerPoolMetricAttrs::new(&base_attrs),
            instance_name,
        }
    }

    pub fn record_worker_count(&self, count: usize) {
        WORKER_POOL_METRICS
            .worker_count
            .record(count as u64, self.attrs.added());
    }

    pub fn record_worker_added(&self) {
        WORKER_POOL_METRICS.worker_events.add(1, self.attrs.added());
    }

    pub fn record_worker_removed(&self) {
        WORKER_POOL_METRICS
            .worker_events
            .add(1, self.attrs.removed());
    }

    pub fn record_worker_timeout(&self) {
        WORKER_POOL_METRICS
            .worker_events
            .add(1, self.attrs.timeout());
    }

    pub fn record_worker_connection_failed(&self) {
        WORKER_POOL_METRICS
            .worker_events
            .add(1, self.attrs.connection_failed());
    }

    pub fn record_action_dispatched(&self) {
        WORKER_POOL_METRICS
            .worker_actions_dispatched
            .add(1, self.attrs.added());
    }

    pub fn record_action_completed(&self) {
        WORKER_POOL_METRICS
            .worker_actions_completed
            .add(1, self.attrs.removed());
    }

    pub fn record_running_actions_count(&self, count: usize) {
        WORKER_POOL_METRICS
            .worker_actions_running
            .record(count as u64, self.attrs.added());
    }

    pub fn record_dispatch_failure(&self) {
        WORKER_POOL_METRICS
            .worker_dispatch_failures
            .add(1, self.attrs.evicted());
    }

    #[must_use]
    pub fn instance_name(&self) -> &str {
        &self.instance_name
    }
}

#[derive(Debug)]
struct Workers(LruCache<WorkerId, Worker>);

impl Deref for Workers {
    type Target = LruCache<WorkerId, Worker>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Workers {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// Note: This could not be a derive macro because this derive-macro
// does not support LruCache and nameless field structs.
impl MetricsComponent for Workers {
    fn publish(
        &self,
        _kind: MetricKind,
        _field_metadata: MetricFieldData,
    ) -> Result<MetricPublishKnownKindData, nativelink_metric::Error> {
        let _enter = group!("workers").entered();
        for (worker_id, worker) in self.iter() {
            let _enter = group!(worker_id).entered();
            worker.publish(MetricKind::Component, MetricFieldData::default())?;
        }
        Ok(MetricPublishKnownKindData::Component)
    }
}

/// A collection of workers that are available to run tasks.
#[derive(MetricsComponent)]
struct ApiWorkerSchedulerImpl {
    /// A `LruCache` of workers available based on `allocation_strategy`.
    #[metric(group = "workers")]
    workers: Workers,

    /// The worker state manager.
    #[metric(group = "worker_state_manager")]
    worker_state_manager: Arc<dyn WorkerStateManager>,
    /// The allocation strategy for workers.
    allocation_strategy: WorkerAllocationStrategy,
    /// A channel to notify the matching engine that the worker pool has changed.
    worker_change_notify: Arc<Notify>,
    /// Worker registry for tracking worker liveness.
    worker_registry: SharedWorkerRegistry,

    /// Whether the worker scheduler is shutting down.
    shutting_down: bool,

    /// Index for fast worker capability lookup.
    /// Used to accelerate `find_worker_for_action` by filtering candidates
    /// based on properties before doing linear scan.
    capability_index: WorkerCapabilityIndex,
}

impl core::fmt::Debug for ApiWorkerSchedulerImpl {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ApiWorkerSchedulerImpl")
            .field("workers", &self.workers)
            .field("allocation_strategy", &self.allocation_strategy)
            .field("worker_change_notify", &self.worker_change_notify)
            .field(
                "capability_index_size",
                &self.capability_index.worker_count(),
            )
            .field("worker_registry", &self.worker_registry)
            .finish_non_exhaustive()
    }
}

impl ApiWorkerSchedulerImpl {
    /// Refreshes the lifetime of the worker with the given timestamp.
    ///
    /// Instead of sending N keepalive messages (one per operation),
    /// we now send a single worker heartbeat. The worker registry tracks worker liveness,
    /// and timeout detection checks the worker's `last_seen` instead of per-operation timestamps.
    ///
    /// Note: This only updates the local worker state. The worker registry is updated
    /// separately after releasing the inner lock to reduce contention.
    fn refresh_lifetime(
        &mut self,
        worker_id: &WorkerId,
        timestamp: WorkerTimestamp,
    ) -> Result<(), Error> {
        let worker = self.workers.0.peek_mut(worker_id).ok_or_else(|| {
            make_input_err!(
                "Worker not found in worker map in refresh_lifetime() {}",
                worker_id
            )
        })?;
        error_if!(
            worker.last_update_timestamp > timestamp,
            "Worker already had a timestamp of {}, but tried to update it with {}",
            worker.last_update_timestamp,
            timestamp
        );
        worker.last_update_timestamp = timestamp;

        trace!(
            ?worker_id,
            running_operations = worker.running_action_infos.len(),
            "Worker keepalive received"
        );

        Ok(())
    }

    /// Adds a worker to the pool.
    /// Note: This function will not do any task matching.
    fn add_worker(&mut self, worker: Worker) -> Result<(), Error> {
        let worker_id = worker.id.clone();
        let platform_properties = worker.platform_properties.clone();
        self.workers.put(worker_id.clone(), worker);

        // Add to capability index for fast matching
        self.capability_index
            .add_worker(&worker_id, &platform_properties);

        // Worker is not cloneable, and we do not want to send the initial connection results until
        // we have added it to the map, or we might get some strange race conditions due to the way
        // the multi-threaded runtime works.
        let worker = self.workers.peek_mut(&worker_id).unwrap();
        let res = worker
            .send_initial_connection_result()
            .err_tip(|| "Failed to send initial connection result to worker");
        if let Err(err) = &res {
            error!(
                ?worker_id,
                ?err,
                "Worker connection appears to have been closed while adding to pool"
            );
        }
        self.worker_change_notify.notify_one();
        res
    }

    /// Removes worker from pool.
    /// Note: The caller is responsible for any rescheduling of any tasks that might be
    /// running.
    fn remove_worker(&mut self, worker_id: &WorkerId) -> Option<Worker> {
        // Remove from capability index
        self.capability_index.remove_worker(worker_id);

        let result = self.workers.pop(worker_id);
        self.worker_change_notify.notify_one();
        result
    }

    /// Sets if the worker is draining or not.
    async fn set_drain_worker(
        &mut self,
        worker_id: &WorkerId,
        is_draining: bool,
    ) -> Result<(), Error> {
        let worker = self
            .workers
            .get_mut(worker_id)
            .err_tip(|| format!("Worker {worker_id} doesn't exist in the pool"))?;
        worker.is_draining = is_draining;
        self.worker_change_notify.notify_one();
        Ok(())
    }

    fn inner_find_worker_for_action(
        &self,
        platform_properties: &PlatformProperties,
        full_worker_logging: bool,
    ) -> Option<WorkerId> {
        // Do a fast check to see if any workers are available at all for work allocation
        if !self.workers.iter().any(|(_, w)| w.can_accept_work()) {
            if full_worker_logging {
                info!("All workers are fully allocated");
            }
            return None;
        }

        // Use capability index to get candidate workers that match STATIC properties
        // (Exact, Unknown) and have the required property keys (Priority, Minimum).
        // The candidates are a bitmap of worker slots, so this lookup allocates
        // nothing.
        let mut candidates = WorkerSlotSet::new();
        if !self.capability_index.find_matching_slots(
            platform_properties,
            full_worker_logging,
            &mut candidates,
        ) {
            if full_worker_logging {
                info!("No workers in capability index match required properties");
            }
            return None;
        }
        let is_candidate = |worker_id: &WorkerId| -> bool {
            self.capability_index
                .slot_of(worker_id)
                .is_some_and(|slot| candidates.contains(slot))
        };

        // Check function for availability AND dynamic Minimum property verification.
        // The index only does presence checks for Minimum properties since their
        // values change dynamically as jobs are assigned to workers.
        let worker_matches = |(worker_id, w): &(&WorkerId, &Worker)| -> bool {
            if !w.can_accept_work() {
                if full_worker_logging {
                    info!(
                        "Worker {worker_id} cannot accept work: is_paused={}, is_draining={}, inflight={}/{}",
                        w.is_paused,
                        w.is_draining,
                        w.running_action_infos.len(),
                        w.max_inflight_tasks
                    );
                }
                return false;
            }

            // Verify Minimum properties at runtime (their values are dynamic)
            if !platform_properties.is_satisfied_by(&w.platform_properties, full_worker_logging) {
                return false;
            }

            true
        };

        // Now check constraints on filtered candidates.
        // Iterate in LRU order based on allocation strategy.
        let workers_iter = self.workers.iter();

        let worker_id = match self.allocation_strategy {
            // Use rfind to get the least recently used that satisfies the properties.
            WorkerAllocationStrategy::LeastRecentlyUsed => workers_iter
                .rev()
                .filter(|(worker_id, _)| is_candidate(worker_id))
                .find(&worker_matches)
                .map(|(_, w)| w.id.clone()),

            // Use find to get the most recently used that satisfies the properties.
            WorkerAllocationStrategy::MostRecentlyUsed => workers_iter
                .filter(|(worker_id, _)| is_candidate(worker_id))
                .find(&worker_matches)
                .map(|(_, w)| w.id.clone()),
        };
        if full_worker_logging && worker_id.is_none() {
            warn!("No workers matched!");
        }
        worker_id
    }

    /// Returns how much work the pool can accept right now.
    fn inner_available_capacity(&self) -> WorkerPoolCapacity {
        let mut available_workers = 0;
        let mut free_slots: u64 = 0;
        let mut unlimited = false;

        for (_, worker) in self.workers.iter() {
            if !worker.can_accept_work() {
                continue;
            }
            available_workers += 1;
            if worker.max_inflight_tasks == 0 {
                unlimited = true;
                continue;
            }
            let running = u64::try_from(worker.running_action_infos.len()).unwrap_or(u64::MAX);
            free_slots =
                free_slots.saturating_add(worker.max_inflight_tasks.saturating_sub(running));
        }

        WorkerPoolCapacity {
            available_workers,
            free_slots: if unlimited { None } else { Some(free_slots) },
        }
    }

    /// Batch finds workers for multiple actions in a single pass.
    /// This reduces lock contention by acquiring the lock once for all actions.
    /// Returns the matched worker for each action, in the order of `actions`.
    ///
    /// The pass keeps a working copy of the capacity of each worker, so a worker
    /// never receives more work than `max_inflight_tasks` allows and the
    /// `Minimum` properties of the earlier matches limit the later matches.
    fn inner_batch_find_workers_for_actions(
        &self,
        actions: &[&PlatformProperties],
        full_worker_logging: bool,
    ) -> Vec<Option<WorkerId>> {
        /// A worker that can take at least one more action in this pass.
        struct AvailableWorker<'a> {
            slot: WorkerSlot,
            id: &'a WorkerId,
            /// Working copy of the worker properties. The `Minimum` values drop
            /// as this pass assigns work to the worker.
            properties: PlatformProperties,
            /// Remaining inflight slots. `u64::MAX` means no limit.
            free_slots: u64,
        }

        let mut results: Vec<Option<WorkerId>> = vec![None; actions.len()];
        if actions.is_empty() {
            return results;
        }

        // Snapshot the workers that can accept work. The order follows the
        // allocation strategy, because the LRU cache holds the most recently
        // used worker first.
        let ordered: Vec<&Worker> = match self.allocation_strategy {
            WorkerAllocationStrategy::LeastRecentlyUsed => self
                .workers
                .iter()
                .rev()
                .map(|(_, worker)| worker)
                .collect(),
            WorkerAllocationStrategy::MostRecentlyUsed => {
                self.workers.iter().map(|(_, worker)| worker).collect()
            }
        };
        let mut available: Vec<AvailableWorker<'_>> = ordered
            .into_iter()
            .filter(|worker| worker.can_accept_work())
            .filter_map(|worker| {
                let slot = self.capability_index.slot_of(&worker.id)?;
                let free_slots = if worker.max_inflight_tasks == 0 {
                    u64::MAX
                } else {
                    let running =
                        u64::try_from(worker.running_action_infos.len()).unwrap_or(u64::MAX);
                    worker.max_inflight_tasks.saturating_sub(running)
                };
                Some(AvailableWorker {
                    slot,
                    id: &worker.id,
                    properties: worker.platform_properties.clone(),
                    free_slots,
                })
            })
            .collect();

        if available.is_empty() {
            if full_worker_logging {
                info!("All workers are fully allocated");
            }
            return results;
        }

        let mut candidates = WorkerSlotSet::new();
        for (idx, platform_properties) in actions.iter().enumerate() {
            if available.is_empty() {
                // Every worker is full, so the rest of the batch has to wait.
                break;
            }

            if !self.capability_index.find_matching_slots(
                platform_properties,
                full_worker_logging,
                &mut candidates,
            ) {
                continue;
            }

            let Some(position) = available.iter().position(|worker| {
                candidates.contains(worker.slot)
                    && platform_properties.is_satisfied_by(&worker.properties, full_worker_logging)
            }) else {
                if full_worker_logging {
                    info!("No workers matched!");
                }
                continue;
            };

            let worker = &mut available[position];
            reduce_platform_properties(&mut worker.properties, platform_properties);
            results[idx] = Some(worker.id.clone());
            worker.free_slots = worker.free_slots.saturating_sub(1);
            if worker.free_slots == 0 {
                // Keep the remaining workers in allocation-strategy order.
                available.remove(position);
            }
        }

        results
    }

    async fn update_action(
        &mut self,
        worker_id: &WorkerId,
        operation_id: &OperationId,
        update: UpdateOperationType,
    ) -> Result<(), Error> {
        let worker = self.workers.get_mut(worker_id).err_tip(|| {
            format!("Worker {worker_id} does not exist in SimpleScheduler::update_action")
        })?;

        // Ensure the worker is supposed to be running the operation.
        if !worker.running_action_infos.contains_key(operation_id) {
            let err = make_err!(
                Code::Internal,
                "Operation {operation_id} should not be running on worker {worker_id} in SimpleScheduler::update_action"
            );
            return Result::<(), _>::Err(err.clone())
                .merge(self.immediate_evict_worker(worker_id, err, false).await);
        }

        let (is_finished, due_to_backpressure) = match &update {
            UpdateOperationType::UpdateWithActionStage(action_stage) => {
                (action_stage.is_finished(), false)
            }
            UpdateOperationType::KeepAlive => (false, false),
            UpdateOperationType::UpdateWithError(err) => {
                (true, err.code == Code::ResourceExhausted)
            }
            UpdateOperationType::UpdateWithDisconnect => (true, false),
            UpdateOperationType::ExecutionComplete => {
                // No update here, just restoring platform properties.
                worker.execution_complete(operation_id);
                self.worker_change_notify.notify_one();
                return Ok(());
            }
        };

        // Update the operation in the worker state manager.
        {
            let update_operation_res = self
                .worker_state_manager
                .update_operation(operation_id, worker_id, update)
                .await
                .err_tip(|| "in update_operation on SimpleScheduler::update_action");
            if let Err(err) = update_operation_res {
                error!(
                    %operation_id,
                    ?worker_id,
                    ?err,
                    "Failed to update_operation on update_action"
                );
                return Err(err);
            }
        }

        if !is_finished {
            return Ok(());
        }

        // Clear this action from the current worker if finished.
        let complete_action_res = {
            // Note: We need to run this before dealing with backpressure logic.
            let complete_action_res = worker.complete_action(operation_id).await;

            if (due_to_backpressure || !worker.can_accept_work()) && worker.has_actions() {
                worker.is_paused = true;
            }
            complete_action_res
        };

        self.worker_change_notify.notify_one();

        complete_action_res
    }

    /// Notifies the specified worker to run the given action and handles errors by evicting
    /// the worker if the notification fails.
    async fn worker_notify_run_action(
        &mut self,
        worker_id: WorkerId,
        operation_id: OperationId,
        action_info: ActionInfoWithProps,
    ) -> Result<(), Error> {
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            let notify_worker_result = worker
                .notify_update(WorkerUpdate::RunAction(Box::new((
                    operation_id,
                    action_info.clone(),
                ))))
                .await;

            if let Err(notify_worker_result) = notify_worker_result {
                warn!(
                    ?worker_id,
                    ?action_info,
                    ?notify_worker_result,
                    "Worker command failed, removing worker",
                );

                // A slightly nasty way of figuring out that the worker disconnected
                // from send_msg_to_worker without introducing complexity to the
                // code path from here to there.
                let is_disconnect = notify_worker_result.code == Code::Internal
                    && notify_worker_result.messages.len() == 1
                    && notify_worker_result.messages[0] == "Worker Disconnected";

                let err = make_err!(
                    Code::Internal,
                    "Worker command failed, removing worker {worker_id} -- {notify_worker_result:?}",
                );

                return Result::<(), _>::Err(err.clone()).merge(
                    self.immediate_evict_worker(&worker_id, err, is_disconnect)
                        .await,
                );
            }
            Ok(())
        } else {
            warn!(
                ?worker_id,
                %operation_id,
                ?action_info,
                "Worker not found in worker map in worker_notify_run_action"
            );
            // Ensure the operation is put back to queued state.
            self.worker_state_manager
                .update_operation(
                    &operation_id,
                    &worker_id,
                    UpdateOperationType::UpdateWithDisconnect,
                )
                .await
        }
    }

    /// Batch notifies multiple workers to run actions in a single lock hold.
    /// Returns a vector of results for each notification attempt.
    async fn inner_batch_worker_notify_run_action(
        &mut self,
        assignments: Vec<(WorkerId, OperationId, ActionInfoWithProps)>,
    ) -> Vec<Result<(), Error>> {
        let mut results = Vec::with_capacity(assignments.len());
        let mut workers_to_evict: Vec<(WorkerId, Error, bool)> = Vec::new();

        for (worker_id, operation_id, action_info) in assignments {
            if let Some(worker) = self.workers.get_mut(&worker_id) {
                let notify_worker_result = worker
                    .notify_update(WorkerUpdate::RunAction(Box::new((
                        operation_id.clone(),
                        action_info.clone(),
                    ))))
                    .await;

                if let Err(notify_err) = notify_worker_result {
                    warn!(
                        ?worker_id,
                        ?action_info,
                        ?notify_err,
                        "Worker command failed in batch notify, will remove worker",
                    );

                    let is_disconnect = notify_err.code == Code::Internal
                        && notify_err.messages.len() == 1
                        && notify_err.messages[0] == "Worker Disconnected";

                    let err = make_err!(
                        Code::Internal,
                        "Worker command failed, removing worker {worker_id} -- {notify_err:?}",
                    );

                    workers_to_evict.push((worker_id.clone(), err.clone(), is_disconnect));
                    results.push(Err(err));
                } else {
                    results.push(Ok(()));
                }
            } else {
                warn!(
                    ?worker_id,
                    %operation_id,
                    ?action_info,
                    "Worker not found in worker map in batch_worker_notify_run_action"
                );
                // Queue the operation to be put back to queued state
                let update_result = self
                    .worker_state_manager
                    .update_operation(
                        &operation_id,
                        &worker_id,
                        UpdateOperationType::UpdateWithDisconnect,
                    )
                    .await;
                results.push(update_result);
            }
        }

        // Evict failed workers after processing all notifications
        for (worker_id, err, is_disconnect) in workers_to_evict {
            let _ = self
                .immediate_evict_worker(&worker_id, err, is_disconnect)
                .await;
        }

        results
    }

    /// Evicts the worker from the pool and puts items back into the queue if anything was being executed on it.
    async fn immediate_evict_worker(
        &mut self,
        worker_id: &WorkerId,
        err: Error,
        is_disconnect: bool,
    ) -> Result<(), Error> {
        let mut result = Ok(());
        if let Some(mut worker) = self.remove_worker(worker_id) {
            // We don't care if we fail to send message to worker, this is only a best attempt.
            drop(worker.notify_update(WorkerUpdate::Disconnect).await);
            let update = if is_disconnect {
                UpdateOperationType::UpdateWithDisconnect
            } else {
                UpdateOperationType::UpdateWithError(err)
            };
            for (operation_id, _) in worker.running_action_infos.drain() {
                result = result.merge(
                    self.worker_state_manager
                        .update_operation(&operation_id, worker_id, update.clone())
                        .await,
                );
            }
        }
        // Note: Calling this many time is very cheap, it'll only trigger `do_try_match` once.
        // TODO(palfrey) This should be moved to inside the Workers struct.
        self.worker_change_notify.notify_one();
        result
    }

    fn count_running_actions(&self) -> usize {
        self.workers
            .iter()
            .map(|(_, w)| w.running_action_infos.len())
            .sum()
    }
}

#[derive(Debug, MetricsComponent)]
pub struct ApiWorkerScheduler {
    #[metric]
    inner: RwLock<ApiWorkerSchedulerImpl>,
    #[metric(group = "platform_property_manager")]
    platform_property_manager: Arc<PlatformPropertyManager>,

    #[metric(
        help = "Timeout of how long to evict workers if no response in this given amount of time in seconds."
    )]
    worker_timeout_s: u64,
    /// Shared worker registry for checking worker liveness.
    worker_registry: SharedWorkerRegistry,

    /// Performance metrics for observability.
    metrics: Arc<SchedulerMetrics>,

    /// OTEL metrics for tracking worker pool state.
    worker_scheduler_metrics: WorkerSchedulerMetrics,

    /// Channel for publishing origin events such as worker-observed action
    /// resource usage. `None` when origin events are disabled.
    maybe_origin_event_tx: Option<mpsc::Sender<OriginEvent>>,
}

impl ApiWorkerScheduler {
    pub fn new(
        worker_state_manager: Arc<dyn WorkerStateManager>,
        platform_property_manager: Arc<PlatformPropertyManager>,
        allocation_strategy: WorkerAllocationStrategy,
        worker_change_notify: Arc<Notify>,
        worker_timeout_s: u64,
        worker_registry: SharedWorkerRegistry,
        instance_name: impl Into<String>,
        maybe_origin_event_tx: Option<mpsc::Sender<OriginEvent>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(ApiWorkerSchedulerImpl {
                workers: Workers(LruCache::unbounded()),
                worker_state_manager,
                allocation_strategy,
                worker_change_notify,
                worker_registry: worker_registry.clone(),
                shutting_down: false,
                capability_index: WorkerCapabilityIndex::new(),
            }),
            platform_property_manager,
            worker_timeout_s,
            worker_registry,
            metrics: Arc::new(SchedulerMetrics::default()),
            worker_scheduler_metrics: WorkerSchedulerMetrics::new(instance_name),
            maybe_origin_event_tx,
        })
    }

    /// Returns a reference to the worker registry.
    pub const fn worker_registry(&self) -> &SharedWorkerRegistry {
        &self.worker_registry
    }

    /// Returns a reference to the worker scheduler metrics for recording OTEL metrics.
    #[must_use]
    pub const fn workerMetrics(&self) -> &WorkerSchedulerMetrics {
        &self.worker_scheduler_metrics
    }

    pub async fn worker_notify_run_action(
        &self,
        worker_id: WorkerId,
        operation_id: OperationId,
        action_info: ActionInfoWithProps,
    ) -> Result<(), Error> {
        self.metrics
            .actions_dispatched
            .fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.write().await;
        let result = inner
            .worker_notify_run_action(worker_id, operation_id, action_info)
            .await;

        // Record metrics
        if result.is_ok() {
            self.worker_scheduler_metrics.record_action_dispatched();
        } else {
            self.worker_scheduler_metrics.record_dispatch_failure();
        }
        self.worker_scheduler_metrics
            .record_running_actions_count(inner.count_running_actions());

        result
    }

    /// Batch notifies multiple workers to run actions in a single lock acquisition.
    /// This reduces lock contention compared to calling `worker_notify_run_action`
    /// for each action individually.
    ///
    /// Returns a vector of results corresponding to each assignment in the input.
    pub async fn batch_worker_notify_run_action(
        &self,
        assignments: Vec<(WorkerId, OperationId, ActionInfoWithProps)>,
    ) -> Vec<Result<(), Error>> {
        let count = assignments.len();
        self.metrics
            .actions_dispatched
            .fetch_add(count as u64, Ordering::Relaxed);

        let mut inner = self.inner.write().await;
        let results = inner
            .inner_batch_worker_notify_run_action(assignments)
            .await;

        // Record metrics
        let successes = results.iter().filter(|r| r.is_ok()).count();
        let failures = count - successes;

        for _ in 0..successes {
            self.worker_scheduler_metrics.record_action_dispatched();
        }
        for _ in 0..failures {
            self.worker_scheduler_metrics.record_dispatch_failure();
        }
        self.worker_scheduler_metrics
            .record_running_actions_count(inner.count_running_actions());

        results
    }

    pub async fn running_action_info(
        &self,
        worker_id: &WorkerId,
        operation_id: &OperationId,
    ) -> Option<ActionInfoWithProps> {
        let inner = self.inner.read().await;
        inner
            .workers
            .peek(worker_id)
            .and_then(|worker| worker.running_action_infos.get(operation_id))
            .map(|pending_action_info| pending_action_info.action_info.clone())
    }

    /// Returns the scheduler metrics for observability.
    #[must_use]
    pub const fn get_metrics(&self) -> &Arc<SchedulerMetrics> {
        &self.metrics
    }

    /// Attempts to find a worker that is capable of running this action.
    // TODO(palfrey) This algorithm is not very efficient. Simple testing using a tree-like
    // structure showed worse performance on a 10_000 worker * 7 properties * 1000 queued tasks
    // simulation of worst cases in a single threaded environment.
    pub async fn find_worker_for_action(
        &self,
        platform_properties: &PlatformProperties,
        full_worker_logging: bool,
    ) -> Option<WorkerId> {
        let start = Instant::now();
        self.metrics
            .find_worker_calls
            .fetch_add(1, Ordering::Relaxed);

        let inner = self.inner.read().await;
        let worker_count = inner.workers.len() as u64;
        let result = inner.inner_find_worker_for_action(platform_properties, full_worker_logging);

        // Track workers iterated (worst case is all workers)
        self.metrics
            .workers_iterated
            .fetch_add(worker_count, Ordering::Relaxed);

        if result.is_some() {
            self.metrics
                .find_worker_hits
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.metrics
                .find_worker_misses
                .fetch_add(1, Ordering::Relaxed);
        }

        #[allow(clippy::cast_possible_truncation)]
        self.metrics
            .find_worker_time_ns
            .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        result
    }

    /// Returns how much work the pool can accept right now.
    ///
    /// The matching engine uses this to skip a cycle when every worker is full,
    /// and to limit how much of the queue one cycle reads.
    pub async fn available_capacity(&self) -> WorkerPoolCapacity {
        let inner = self.inner.read().await;
        inner.inner_available_capacity()
    }

    /// Batch finds workers for multiple actions in a single lock acquisition.
    /// This reduces lock contention compared to calling `find_worker_for_action`
    /// for each action individually.
    ///
    /// Returns the matched worker for each action, in the order of `actions`.
    /// An action that no worker can take holds `None`.
    pub async fn batch_find_workers_for_actions(
        &self,
        actions: &[&PlatformProperties],
        full_worker_logging: bool,
    ) -> Vec<Option<WorkerId>> {
        let start = Instant::now();
        self.metrics
            .find_worker_calls
            .fetch_add(actions.len() as u64, Ordering::Relaxed);

        let inner = self.inner.read().await;
        let worker_count = inner.workers.len() as u64;
        let results = inner.inner_batch_find_workers_for_actions(actions, full_worker_logging);

        // Track metrics
        self.metrics
            .workers_iterated
            .fetch_add(worker_count * actions.len() as u64, Ordering::Relaxed);

        let hits = results.iter().filter(|result| result.is_some()).count() as u64;
        let misses = actions.len() as u64 - hits;
        self.metrics
            .find_worker_hits
            .fetch_add(hits, Ordering::Relaxed);
        self.metrics
            .find_worker_misses
            .fetch_add(misses, Ordering::Relaxed);

        #[allow(clippy::cast_possible_truncation)]
        self.metrics
            .find_worker_time_ns
            .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);

        results
    }

    /// Checks to see if the worker exists in the worker pool. Should only be used in unit tests.
    #[must_use]
    pub async fn contains_worker_for_test(&self, worker_id: &WorkerId) -> bool {
        let inner = self.inner.read().await;
        inner.workers.contains(worker_id)
    }

    /// A unit test function used to send the keep alive message to the worker from the server.
    pub async fn send_keep_alive_to_worker_for_test(
        &self,
        worker_id: &WorkerId,
    ) -> Result<(), Error> {
        let mut inner = self.inner.write().await;
        let worker = inner.workers.get_mut(worker_id).ok_or_else(|| {
            make_input_err!("WorkerId '{}' does not exist in workers map", worker_id)
        })?;
        worker.keep_alive()
    }

    pub async fn get_workers_state(&self) -> Vec<WorkerState> {
        let inner = self.inner.read().await;
        inner.workers.iter().map(|(_, w)| w.to_state()).collect()
    }
}

#[async_trait]
impl WorkerScheduler for ApiWorkerScheduler {
    fn get_platform_property_manager(&self) -> &PlatformPropertyManager {
        self.platform_property_manager.as_ref()
    }

    async fn record_action_resource_usage(
        &self,
        worker_id: &WorkerId,
        operation_id: &OperationId,
        mut resource_usage: ActionResourceUsage,
    ) -> Result<(), Error> {
        // The worker API talks to this `ApiWorkerScheduler` (it is the
        // `WorkerScheduler` returned by `SimpleScheduler::new`), so the
        // resource-usage origin event must be published here. Previously the
        // only override lived on `SimpleScheduler`, which this path never
        // reaches, so the event was silently dropped by the trait's no-op
        // default and `observed_worker_peak_memory_mib` was never recorded.
        let Some(origin_event_tx) = self.maybe_origin_event_tx.as_ref() else {
            return Ok(());
        };
        let Some(action_info) = self.running_action_info(worker_id, operation_id).await else {
            return Ok(());
        };

        if resource_usage.operation_id.is_empty() {
            resource_usage.operation_id = operation_id.to_string();
        }
        if resource_usage.worker_id.is_empty() {
            resource_usage.worker_id = worker_id.to_string();
        }

        let event = Event {
            event: Some(event::Event::Response(ResponseEvent {
                event: Some(response_event::Event::ActionResourceUsage(resource_usage)),
            })),
        };
        let origin_event = OriginEvent {
            version: 0,
            event_id: Uuid::now_v6(&get_node_id(Some(&event)))
                .hyphenated()
                .to_string(),
            parent_event_id: action_info
                .scheduler_start_execute_event_id
                .clone()
                .unwrap_or_default(),
            bazel_request_metadata: action_info.origin_metadata.bazel_metadata.clone(),
            identity: action_info.origin_metadata.identity,
            event: Some(event),
        };
        // Awaited send (not try_send): apply backpressure when the publisher
        // queue is full instead of silently dropping the resource-usage event,
        // which is what drives action-level resource sizing in the UI.
        if let Err(err) = origin_event_tx.send(origin_event).await {
            warn!(?err, "Failed to publish action resource usage origin event");
        }
        Ok(())
    }

    async fn add_worker(&self, worker: Worker) -> Result<(), Error> {
        let worker_id = worker.id.clone();
        let worker_timestamp = worker.last_update_timestamp;
        let mut inner = self.inner.write().await;
        if inner.shutting_down {
            warn!("Rejected worker add during shutdown: {}", worker_id);
            return Err(make_err!(
                Code::Unavailable,
                "Received request to add worker while shutting down"
            ));
        }
        let result = inner
            .add_worker(worker)
            .err_tip(|| "Error while adding worker, removing from pool");
        if let Err(err) = &result {
            self.worker_scheduler_metrics
                .record_worker_connection_failed();
            return Result::<(), _>::Err(err.clone()).merge(
                inner
                    .immediate_evict_worker(&worker_id, err.clone(), false)
                    .await,
            );
        }

        let now = UNIX_EPOCH + Duration::from_secs(worker_timestamp);
        self.worker_registry.register_worker(&worker_id, now).await;

        self.metrics.workers_added.fetch_add(1, Ordering::Relaxed);
        self.worker_scheduler_metrics.record_worker_added();
        self.worker_scheduler_metrics
            .record_worker_count(inner.workers.len());
        Ok(())
    }

    async fn update_action(
        &self,
        worker_id: &WorkerId,
        operation_id: &OperationId,
        update: UpdateOperationType,
    ) -> Result<(), Error> {
        let is_completion = matches!(
            update,
            UpdateOperationType::UpdateWithActionStage(ref stage) if stage.is_finished()
        ) || matches!(
            update,
            UpdateOperationType::UpdateWithError(_) | UpdateOperationType::UpdateWithDisconnect
        );

        let mut inner = self.inner.write().await;
        let result = inner.update_action(worker_id, operation_id, update).await;

        // Record action completion metric
        if result.is_ok() && is_completion {
            self.worker_scheduler_metrics.record_action_completed();
        }
        self.worker_scheduler_metrics
            .record_running_actions_count(inner.count_running_actions());

        result
    }

    async fn worker_keep_alive_received(
        &self,
        worker_id: &WorkerId,
        timestamp: WorkerTimestamp,
    ) -> Result<(), Error> {
        {
            let mut inner = self.inner.write().await;
            inner
                .refresh_lifetime(worker_id, timestamp)
                .err_tip(|| "Error refreshing lifetime in worker_keep_alive_received()")?;
        }
        let now = UNIX_EPOCH + Duration::from_secs(timestamp);
        self.worker_registry
            .update_worker_heartbeat(worker_id, now)
            .await;
        Ok(())
    }

    async fn remove_worker(&self, worker_id: &WorkerId) -> Result<(), Error> {
        self.worker_registry.remove_worker(worker_id).await;

        let mut inner = self.inner.write().await;
        let result = inner
            .immediate_evict_worker(
                worker_id,
                make_err!(Code::Internal, "Received request to remove worker"),
                false,
            )
            .await;

        // Record worker removal
        self.worker_scheduler_metrics.record_worker_removed();
        self.worker_scheduler_metrics
            .record_worker_count(inner.workers.len());
        result
    }

    async fn shutdown(&self, shutdown_guard: ShutdownGuard) {
        let mut inner = self.inner.write().await;
        inner.shutting_down = true; // should reject further worker registration
        while let Some(worker_id) = inner
            .workers
            .peek_lru()
            .map(|(worker_id, _worker)| worker_id.clone())
        {
            if let Err(err) = inner
                .immediate_evict_worker(
                    &worker_id,
                    make_err!(Code::Internal, "Scheduler shutdown"),
                    true,
                )
                .await
            {
                error!(?err, "Error evicting worker on shutdown.");
            }
        }
        drop(shutdown_guard);
    }

    async fn remove_timedout_workers(&self, now_timestamp: WorkerTimestamp) -> Result<(), Error> {
        // Check worker liveness using both the local timestamp (from LRU)
        // and the worker registry. A worker is alive if either source says it's alive.
        let timeout = Duration::from_secs(self.worker_timeout_s);
        let now = UNIX_EPOCH + Duration::from_secs(now_timestamp);
        let timeout_threshold = now_timestamp.saturating_sub(self.worker_timeout_s);

        // Phase 1: Read-only collection of workers to check
        let workers_to_check: Vec<(WorkerId, bool)> = {
            let inner = self.inner.read().await;
            inner
                .workers
                .iter()
                .map(|(worker_id, worker)| {
                    let local_alive = worker.last_update_timestamp > timeout_threshold;
                    (worker_id.clone(), local_alive)
                })
                .collect()
        };

        let mut worker_ids_to_remove = Vec::new();
        for (worker_id, local_alive) in workers_to_check {
            if local_alive {
                continue;
            }

            let registry_alive = self
                .worker_registry
                .is_worker_alive(&worker_id, timeout, now)
                .await;

            if !registry_alive {
                trace!(
                    ?worker_id,
                    local_alive,
                    registry_alive,
                    timeout_threshold,
                    "Worker timed out - neither local nor registry shows alive"
                );
                worker_ids_to_remove.push(worker_id);
            }
        }

        if worker_ids_to_remove.is_empty() {
            return Ok(());
        }

        // Phase 2: Write lock to remove timed out workers
        let mut inner = self.inner.write().await;
        let mut result = Ok(());

        for worker_id in &worker_ids_to_remove {
            warn!(?worker_id, "Worker timed out, removing from pool");
            result = result.merge(
                inner
                    .immediate_evict_worker(
                        worker_id,
                        make_err!(
                            Code::Internal,
                            "Worker {worker_id} timed out, removing from pool"
                        ),
                        false,
                    )
                    .await,
            );
            self.worker_scheduler_metrics.record_worker_timeout();
        }

        self.worker_scheduler_metrics
            .record_running_actions_count(inner.count_running_actions());
        self.worker_scheduler_metrics
            .record_worker_count(inner.workers.len());

        result
    }

    async fn set_drain_worker(&self, worker_id: &WorkerId, is_draining: bool) -> Result<(), Error> {
        let mut inner = self.inner.write().await;
        inner.set_drain_worker(worker_id, is_draining).await?;
        self.worker_scheduler_metrics
            .record_worker_count(inner.workers.len());
        Ok(())
    }
}

impl RootMetricsComponent for ApiWorkerScheduler {}
