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

use core::sync::atomic::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use nativelink_config::schedulers::WorkerAllocationStrategy;
use nativelink_error::Error;
use nativelink_macro::nativelink_test;
use nativelink_metric::MetricsComponent;
use nativelink_proto::com::github::trace_machina::nativelink::remote_execution::UpdateForWorker;
use nativelink_scheduler::api_worker_scheduler::ApiWorkerScheduler;
use nativelink_scheduler::platform_property_manager::PlatformPropertyManager;
use nativelink_scheduler::worker::Worker;
use nativelink_scheduler::worker_registry::WorkerRegistry;
use nativelink_scheduler::worker_scheduler::WorkerScheduler;
use nativelink_util::action_messages::{OperationId, WorkerId};
use nativelink_util::operation_state_manager::{UpdateOperationType, WorkerStateManager};
use nativelink_util::platform_properties::{PlatformProperties, PlatformPropertyValue};
use pretty_assertions::assert_eq;
use tokio::sync::{Notify, mpsc};
use tonic::async_trait;

const NOW_TIME: u64 = 10_000;
const WORKER_TIMEOUT_S: u64 = 100;

#[derive(Debug, Default, MetricsComponent)]
struct NoopWorkerStateManager {
    #[metric(help = "Number of updates that this manager dropped.")]
    updates: u64,
}

#[async_trait]
impl WorkerStateManager for NoopWorkerStateManager {
    async fn update_operation(
        &self,
        _operation_id: &OperationId,
        _worker_id: &WorkerId,
        _update: UpdateOperationType,
    ) -> Result<(), Error> {
        Ok(())
    }
}

fn make_scheduler(allocation_strategy: WorkerAllocationStrategy) -> Arc<ApiWorkerScheduler> {
    ApiWorkerScheduler::new(
        Arc::new(NoopWorkerStateManager::default()),
        Arc::new(PlatformPropertyManager::new(HashMap::new())),
        allocation_strategy,
        Arc::new(Notify::new()),
        WORKER_TIMEOUT_S,
        Arc::new(WorkerRegistry::new()),
        "test_scheduler",
        None,
    )
}

/// Adds a worker to the pool and keeps the worker channel open.
async fn add_worker(
    scheduler: &ApiWorkerScheduler,
    worker_id: &str,
    properties: PlatformProperties,
    max_inflight_tasks: u64,
) -> Result<mpsc::UnboundedReceiver<UpdateForWorker>, Error> {
    let (tx, rx) = mpsc::unbounded_channel();
    let worker = Worker::new(
        WorkerId(worker_id.to_string()),
        properties,
        tx,
        NOW_TIME,
        max_inflight_tasks,
    );
    scheduler.add_worker(worker).await?;
    Ok(rx)
}

fn minimum_properties(name: &str, value: u64) -> PlatformProperties {
    PlatformProperties::new(HashMap::from([(
        name.to_string(),
        PlatformPropertyValue::Minimum(value),
    )]))
}

#[nativelink_test]
async fn batch_match_respects_max_inflight_tasks() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::default());
    let _rx = add_worker(&scheduler, "worker", PlatformProperties::default(), 2).await?;

    // Five actions with no properties, one worker that takes two of them.
    let empty = PlatformProperties::default();
    let actions: Vec<&PlatformProperties> = vec![&empty; 5];
    let matches = scheduler
        .batch_find_workers_for_actions(&actions, false)
        .await;

    assert_eq!(
        matches.iter().flatten().count(),
        2,
        "A worker must never take more than max_inflight_tasks actions in one batch"
    );
    // The two matches must be the first two actions of the queue.
    assert!(matches[0].is_some());
    assert!(matches[1].is_some());
    assert_eq!(matches[2], None);
    Ok(())
}

#[nativelink_test]
async fn batch_match_reduces_minimum_properties() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::default());
    // No inflight limit, so only the `Minimum` property limits the worker.
    let _rx = add_worker(&scheduler, "worker", minimum_properties("cpu", 8), 0).await?;

    let action_properties = minimum_properties("cpu", 3);
    let actions: Vec<&PlatformProperties> = vec![&action_properties; 5];
    let matches = scheduler
        .batch_find_workers_for_actions(&actions, false)
        .await;

    assert_eq!(
        matches.iter().flatten().count(),
        2,
        "A worker with 8 cpu must take two actions that ask for 3 cpu"
    );
    Ok(())
}

#[nativelink_test]
async fn batch_match_follows_least_recently_used() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::LeastRecentlyUsed);
    let _rx_first = add_worker(&scheduler, "first", PlatformProperties::default(), 1).await?;
    let _rx_second = add_worker(&scheduler, "second", PlatformProperties::default(), 1).await?;

    let empty = PlatformProperties::default();
    let actions: Vec<&PlatformProperties> = vec![&empty; 2];
    let matches = scheduler
        .batch_find_workers_for_actions(&actions, false)
        .await;

    // `first` joined first, so it is the least recently used worker.
    assert_eq!(matches[0], Some(WorkerId("first".to_string())));
    assert_eq!(matches[1], Some(WorkerId("second".to_string())));
    Ok(())
}

#[nativelink_test]
async fn batch_match_follows_most_recently_used() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::MostRecentlyUsed);
    let _rx_first = add_worker(&scheduler, "first", PlatformProperties::default(), 1).await?;
    let _rx_second = add_worker(&scheduler, "second", PlatformProperties::default(), 1).await?;

    let empty = PlatformProperties::default();
    let actions: Vec<&PlatformProperties> = vec![&empty; 2];
    let matches = scheduler
        .batch_find_workers_for_actions(&actions, false)
        .await;

    // `second` joined last, so it is the most recently used worker.
    assert_eq!(matches[0], Some(WorkerId("second".to_string())));
    assert_eq!(matches[1], Some(WorkerId("first".to_string())));
    Ok(())
}

#[nativelink_test]
async fn batch_match_and_sequential_match_pick_the_same_worker() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::default());
    let _rx_small = add_worker(&scheduler, "small", minimum_properties("cpu", 2), 0).await?;
    let _rx_large = add_worker(&scheduler, "large", minimum_properties("cpu", 16), 0).await?;

    let action_properties = minimum_properties("cpu", 8);
    let sequential = scheduler
        .find_worker_for_action(&action_properties, false)
        .await;
    let batch = scheduler
        .batch_find_workers_for_actions(&[&action_properties], false)
        .await;

    assert_eq!(sequential, Some(WorkerId("large".to_string())));
    assert_eq!(batch[0], sequential);
    Ok(())
}

#[nativelink_test]
async fn available_capacity_counts_free_slots() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::default());
    let capacity = scheduler.available_capacity().await;
    assert_eq!(capacity.available_workers, 0);
    assert_eq!(capacity.free_slots, Some(0));

    let _rx_first = add_worker(&scheduler, "first", PlatformProperties::default(), 2).await?;
    let _rx_second = add_worker(&scheduler, "second", PlatformProperties::default(), 3).await?;
    let capacity = scheduler.available_capacity().await;
    assert_eq!(capacity.available_workers, 2);
    assert_eq!(capacity.free_slots, Some(5));

    // A worker without an inflight limit makes the free capacity unbounded.
    let _rx_third = add_worker(&scheduler, "third", PlatformProperties::default(), 0).await?;
    let capacity = scheduler.available_capacity().await;
    assert_eq!(capacity.available_workers, 3);
    assert_eq!(capacity.free_slots, None);
    Ok(())
}

#[nativelink_test]
async fn batch_match_counts_only_the_workers_it_examines() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::default());
    #[allow(
        clippy::collection_is_never_read,
        reason = "Holds the worker channels open for the length of the test"
    )]
    let mut receivers = Vec::new();
    for name in ["first", "second", "third"] {
        receivers.push(add_worker(&scheduler, name, PlatformProperties::default(), 1).await?);
    }

    // Six actions with no properties, three workers with one slot each.
    let empty = PlatformProperties::default();
    let actions: Vec<&PlatformProperties> = vec![&empty; 6];
    let matches = scheduler
        .batch_find_workers_for_actions(&actions, false)
        .await;
    assert_eq!(matches.iter().flatten().count(), 3);

    let metrics = scheduler.get_metrics();
    assert_eq!(metrics.find_worker_hits.load(Ordering::Relaxed), 3);
    // The pool filled up after three actions, so the pass never examined the
    // last three actions.
    assert_eq!(metrics.find_worker_misses.load(Ordering::Relaxed), 3);
    // The pass reads the three workers once for the snapshot, and then examines
    // one worker per match. The product of workers and actions would be 18.
    assert_eq!(metrics.workers_iterated.load(Ordering::Relaxed), 6);
    Ok(())
}

#[nativelink_test]
async fn batch_match_reports_a_full_pool_without_a_scan() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::default());
    let worker_id = WorkerId("worker".to_string());
    let _rx = add_worker(&scheduler, "worker", PlatformProperties::default(), 1).await?;
    scheduler.set_drain_worker(&worker_id, true).await?;

    let empty = PlatformProperties::default();
    let actions: Vec<&PlatformProperties> = vec![&empty; 4];
    let matches = scheduler
        .batch_find_workers_for_actions(&actions, false)
        .await;
    assert_eq!(matches.iter().flatten().count(), 0);

    let metrics = scheduler.get_metrics();
    assert_eq!(metrics.find_worker_hits.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.find_worker_misses.load(Ordering::Relaxed), 4);
    // The pass reads the worker once for the snapshot, and then stops.
    assert_eq!(metrics.workers_iterated.load(Ordering::Relaxed), 1);
    Ok(())
}

#[nativelink_test]
async fn sequential_match_counts_only_the_workers_it_examines() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::LeastRecentlyUsed);
    #[allow(
        clippy::collection_is_never_read,
        reason = "Holds the worker channels open for the length of the test"
    )]
    let mut receivers = Vec::new();
    for name in ["first", "second", "third"] {
        receivers.push(add_worker(&scheduler, name, PlatformProperties::default(), 1).await?);
    }

    let worker_id = scheduler
        .find_worker_for_action(&PlatformProperties::default(), false)
        .await;
    assert_eq!(worker_id, Some(WorkerId("first".to_string())));

    let metrics = scheduler.get_metrics();
    assert_eq!(metrics.find_worker_hits.load(Ordering::Relaxed), 1);
    // The capacity check stops at the first worker of the pool, and the scan
    // stops at the first match. The whole pool would be three workers.
    assert_eq!(metrics.workers_iterated.load(Ordering::Relaxed), 2);
    Ok(())
}

#[nativelink_test]
async fn available_capacity_ignores_drained_workers() -> Result<(), Error> {
    let scheduler = make_scheduler(WorkerAllocationStrategy::default());
    let worker_id = WorkerId("worker".to_string());
    let _rx = add_worker(&scheduler, "worker", PlatformProperties::default(), 4).await?;

    scheduler.set_drain_worker(&worker_id, true).await?;
    let capacity = scheduler.available_capacity().await;
    assert_eq!(capacity.available_workers, 0);
    assert_eq!(capacity.free_slots, Some(0));

    scheduler.set_drain_worker(&worker_id, false).await?;
    let capacity = scheduler.available_capacity().await;
    assert_eq!(capacity.available_workers, 1);
    assert_eq!(capacity.free_slots, Some(4));
    Ok(())
}
