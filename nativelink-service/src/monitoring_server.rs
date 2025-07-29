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

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use nativelink_error::{make_input_err, Error, ResultExt};
use nativelink_util::action_messages::{ActionStage, OperationId, WorkerId};
use nativelink_util::operation_state_manager::{
    ClientStateManager, OperationFilter, OperationStageFlags,
};
use nativelink_util::platform_properties::PlatformProperties;
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tracing::{event, Level};

use nativelink_scheduler::worker_scheduler::WorkerScheduler;

#[derive(Debug, Serialize)]
pub struct WorkerInfo {
    pub id: String,
    pub platform_properties: HashMap<String, String>,
    pub last_update_timestamp: u64,
    pub is_paused: bool,
    pub is_draining: bool,
    pub can_accept_work: bool,
    pub running_operations: Vec<String>,
    pub connected_timestamp: u64,
    pub actions_completed: u64,
}

#[derive(Debug, Serialize)]
pub struct OperationInfo {
    pub client_operation_id: String,
    pub stage: String,
    pub worker_id: Option<String>,
    pub action_digest: String,
    pub command_digest: String,
    pub input_root_digest: String,
    pub priority: i32,
    pub timeout: u64,
    pub platform_properties: HashMap<String, String>,
    pub load_timestamp: u64,
    pub insert_timestamp: u64,
    pub is_finished: bool,
}

#[derive(Debug, Serialize)]
pub struct SchedulerStatus {
    pub total_workers: usize,
    pub active_workers: usize,
    pub paused_workers: usize,
    pub draining_workers: usize,
    pub total_operations: usize,
    pub queued_operations: usize,
    pub executing_operations: usize,
    pub completed_operations: usize,
    pub uptime_seconds: u64,
}

#[derive(Debug, Serialize)]
pub struct SystemHealth {
    pub status: String,
    pub timestamp: u64,
    pub uptime_seconds: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OperationQuery {
    pub stage: Option<String>,
    pub worker_id: Option<String>,
    pub limit: Option<usize>,
}

pub struct MonitoringServer {
    worker_scheduler: Arc<dyn WorkerScheduler>,
    client_state_manager: Arc<dyn ClientStateManager>,
    start_time: u64,
}

impl MonitoringServer {
    pub fn new(
        worker_scheduler: Arc<dyn WorkerScheduler>,
        client_state_manager: Arc<dyn ClientStateManager>,
    ) -> Self {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self {
            worker_scheduler,
            client_state_manager,
            start_time,
        }
    }

    pub fn into_router(self) -> Router {
        let state = Arc::new(self);

        Router::new()
            .route("/api/v1/workers", get(get_workers))
            .route("/api/v1/workers/:worker_id", get(get_worker))
            .route("/api/v1/operations", get(get_operations))
            .route("/api/v1/operations/:operation_id", get(get_operation))
            .route("/api/v1/scheduler/status", get(get_scheduler_status))
            .route("/api/v1/scheduler/metrics", get(get_scheduler_metrics))
            .route("/api/v1/system/health", get(get_system_health))
            .with_state(state)
    }

    async fn get_workers_internal(&self) -> Result<Vec<WorkerInfo>, Error> {
        let mut workers = Vec::new();

        // Get all workers info from the worker scheduler
        let workers_info = self
            .worker_scheduler
            .get_all_workers_info()
            .await
            .err_tip(|| "Failed to get workers info")?;

        // Convert to the monitoring API format
        for (worker_id, worker_info) in workers_info {
            workers.push(WorkerInfo {
                id: worker_id.to_string(),
                platform_properties: worker_info.platform_properties,
                last_update_timestamp: worker_info.last_update_timestamp,
                is_paused: worker_info.is_paused,
                is_draining: worker_info.is_draining,
                can_accept_work: worker_info.can_accept_work,
                running_operations: worker_info
                    .running_operations
                    .into_iter()
                    .map(|op_id| op_id.to_string())
                    .collect(),
                connected_timestamp: worker_info.connected_timestamp,
                actions_completed: worker_info.actions_completed,
            });
        }

        Ok(workers)
    }

    async fn get_operations_internal(
        &self,
        query: Option<OperationQuery>,
    ) -> Result<Vec<OperationInfo>, Error> {
        let mut operations = Vec::new();

        // Build filter based on query parameters
        let mut filter = OperationFilter::default();
        if let Some(query) = query.clone() {
            if let Some(stage) = query.stage {
                filter.stages = match stage.as_str() {
                    "queued" => OperationStageFlags::Queued,
                    "executing" => OperationStageFlags::Executing,
                    "completed" => OperationStageFlags::Completed,
                    "cache_check" => OperationStageFlags::CacheCheck,
                    _ => OperationStageFlags::Any,
                };
            }
            if let Some(worker_id) = query.worker_id {
                filter.worker_id = Some(WorkerId::from(worker_id));
            }
        }

        // Get operations from the client state manager
        let mut operation_stream = self
            .client_state_manager
            .filter_operations(filter)
            .await
            .err_tip(|| "Failed to filter operations")?;

        let mut count = 0;
        let limit = query.clone().and_then(|q| q.limit).unwrap_or(1000);

        while let Some(action_result) = operation_stream.next().await {
            if count >= limit {
                break;
            }

            let action_state = action_result
                .as_state()
                .await
                .err_tip(|| "Failed to get action state")?;

            let action_info = action_result
                .as_action_info()
                .await
                .err_tip(|| "Failed to get action info")?;

            operations.push(OperationInfo {
                client_operation_id: action_state.0.client_operation_id.to_string(),
                stage: format!("{:?}", action_state.0.stage),
                worker_id: query.clone().unwrap().worker_id,
                action_digest: action_state.0.action_digest.to_string(),
                command_digest: action_info.0.command_digest.to_string(),
                input_root_digest: action_info.0.input_root_digest.to_string(),
                priority: action_info.0.priority,
                timeout: action_info.0.timeout.as_secs(),
                platform_properties: action_info.0.platform_properties.clone(),
                load_timestamp: action_info
                    .0
                    .load_timestamp
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                insert_timestamp: action_info
                    .0
                    .insert_timestamp
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                is_finished: action_state.0.stage.is_finished(),
            });

            count += 1;
        }

        Ok(operations)
    }

    async fn get_scheduler_status_internal(&self) -> Result<SchedulerStatus, Error> {
        let workers = self.get_workers_internal().await?;
        let operations = self.get_operations_internal(None).await?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let total_workers = workers.len();
        let active_workers = workers.iter().filter(|w| w.can_accept_work).count();
        let paused_workers = workers.iter().filter(|w| w.is_paused).count();
        let draining_workers = workers.iter().filter(|w| w.is_draining).count();

        let total_operations = operations.len();
        let queued_operations = operations
            .iter()
            .filter(|op| !op.is_finished && op.worker_id.is_none())
            .count();
        let executing_operations = operations
            .iter()
            .filter(|op| !op.is_finished && op.worker_id.is_some())
            .count();
        let completed_operations = operations.iter().filter(|op| op.is_finished).count();

        Ok(SchedulerStatus {
            total_workers,
            active_workers,
            paused_workers,
            draining_workers,
            total_operations,
            queued_operations,
            executing_operations,
            completed_operations,
            uptime_seconds: now - self.start_time,
        })
    }
}

async fn get_workers(
    State(state): State<Arc<MonitoringServer>>,
) -> Result<Json<Vec<WorkerInfo>>, (StatusCode, String)> {
    match state.get_workers_internal().await {
        Ok(workers) => Ok(Json(workers)),
        Err(err) => {
            event!(Level::ERROR, ?err, "Failed to get workers");
            Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
        }
    }
}

async fn get_worker(
    State(state): State<Arc<MonitoringServer>>,
    Path(worker_id): Path<String>,
) -> Result<Json<WorkerInfo>, (StatusCode, String)> {
    let workers = state.get_workers_internal().await.map_err(|err| {
        event!(Level::ERROR, ?err, "Failed to get workers");
        (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
    })?;

    let worker = workers
        .into_iter()
        .find(|w| w.id == worker_id)
        .ok_or_else(|| {
            let err = make_input_err!("Worker {} not found", worker_id);
            (StatusCode::NOT_FOUND, err.to_string())
        })?;

    Ok(Json(worker))
}

async fn get_operations(
    State(state): State<Arc<MonitoringServer>>,
    Query(query): Query<OperationQuery>,
) -> Result<Json<Vec<OperationInfo>>, (StatusCode, String)> {
    match state.get_operations_internal(Some(query)).await {
        Ok(operations) => Ok(Json(operations)),
        Err(err) => {
            event!(Level::ERROR, ?err, "Failed to get operations");
            Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
        }
    }
}

async fn get_operation(
    State(state): State<Arc<MonitoringServer>>,
    Path(operation_id): Path<String>,
) -> Result<Json<OperationInfo>, (StatusCode, String)> {
    let operations = state.get_operations_internal(None).await.map_err(|err| {
        event!(Level::ERROR, ?err, "Failed to get operations");
        (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
    })?;

    let operation = operations
        .into_iter()
        .find(|op| op.client_operation_id == operation_id)
        .ok_or_else(|| {
            let err = make_input_err!("Operation {} not found", operation_id);
            (StatusCode::NOT_FOUND, err.to_string())
        })?;

    Ok(Json(operation))
}

async fn get_scheduler_status(
    State(state): State<Arc<MonitoringServer>>,
) -> Result<Json<SchedulerStatus>, (StatusCode, String)> {
    match state.get_scheduler_status_internal().await {
        Ok(status) => Ok(Json(status)),
        Err(err) => {
            event!(Level::ERROR, ?err, "Failed to get scheduler status");
            Err((StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
        }
    }
}

async fn get_scheduler_metrics(
    State(state): State<Arc<MonitoringServer>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // For now, return basic metrics. In the future, this could include
    // more detailed metrics from the metrics system.
    let status = state.get_scheduler_status_internal().await.map_err(|err| {
        event!(Level::ERROR, ?err, "Failed to get scheduler status");
        (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
    })?;

    let metrics = serde_json::json!({
        "workers": {
            "total": status.total_workers,
            "active": status.active_workers,
            "paused": status.paused_workers,
            "draining": status.draining_workers,
        },
        "operations": {
            "total": status.total_operations,
            "queued": status.queued_operations,
            "executing": status.executing_operations,
            "completed": status.completed_operations,
        },
        "uptime_seconds": status.uptime_seconds,
    });

    Ok(Json(metrics))
}

async fn get_system_health(
    State(state): State<Arc<MonitoringServer>>,
) -> Result<Json<SystemHealth>, (StatusCode, String)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let health = SystemHealth {
        status: "healthy".to_string(),
        timestamp: now,
        uptime_seconds: now - state.start_time,
    };

    Ok(Json(health))
}
