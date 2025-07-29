// Copyright 2024 The NativeLink Authors. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use async_trait::async_trait;
use nativelink_error::Error;
use nativelink_metric::RootMetricsComponent;
use nativelink_util::action_messages::{OperationId, WorkerId};
use nativelink_util::operation_state_manager::UpdateOperationType;
use std::collections::HashMap;

use crate::platform_property_manager::PlatformPropertyManager;
use crate::worker::{Worker, WorkerTimestamp};

/// Information about a worker for monitoring purposes.
#[derive(Debug, Clone)]
pub struct WorkerInfo {
    pub platform_properties: HashMap<String, String>,
    pub last_update_timestamp: WorkerTimestamp,
    pub is_paused: bool,
    pub is_draining: bool,
    pub can_accept_work: bool,
    pub running_operations: Vec<OperationId>,
    pub connected_timestamp: u64,
    pub actions_completed: u64,
}

/// WorkerScheduler interface is responsible for interactions between the scheduler
/// and worker related operations.
#[async_trait]
pub trait WorkerScheduler: Sync + Send + Unpin + RootMetricsComponent + 'static {
    /// Returns the platform property manager.
    fn get_platform_property_manager(&self) -> &PlatformPropertyManager;

    /// Adds a worker to the scheduler and begin using it to execute actions (when able).
    async fn add_worker(&self, worker: Worker) -> Result<(), Error>;

    /// Updates the status of an action to the scheduler from the worker.
    async fn update_action(
        &self,
        worker_id: &WorkerId,
        operation_id: &OperationId,
        update: UpdateOperationType,
    ) -> Result<(), Error>;

    /// Event for when the keep alive message was received from the worker.
    async fn worker_keep_alive_received(
        &self,
        worker_id: &WorkerId,
        timestamp: WorkerTimestamp,
    ) -> Result<(), Error>;

    /// Removes worker from pool and reschedule any tasks that might be running on it.
    async fn remove_worker(&self, worker_id: &WorkerId) -> Result<(), Error>;

    /// Removes timed out workers from the pool. This is called periodically by an
    /// external source.
    async fn remove_timedout_workers(&self, now_timestamp: WorkerTimestamp) -> Result<(), Error>;

    /// Sets if the worker is draining or not.
    async fn set_drain_worker(&self, worker_id: &WorkerId, is_draining: bool) -> Result<(), Error>;

    /// Returns a list of worker IDs that are currently connected.
    /// This is used for monitoring purposes.
    async fn get_worker_ids(&self) -> Result<Vec<WorkerId>, Error>;

    /// Returns detailed information about all workers.
    /// This is used for monitoring purposes.
    async fn get_all_workers_info(&self) -> Result<Vec<(WorkerId, WorkerInfo)>, Error>;
}
