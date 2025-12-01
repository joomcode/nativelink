// Copyright 2025 The NativeLink Authors. All rights reserved.
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

use std::sync::LazyLock;

use opentelemetry::{InstrumentationScope, KeyValue, Value, global, metrics};
use crate::action_messages::ActionStage;

// Metric attribute keys for remote execution operations.
pub const EXECUTION_STAGE: &str = "execution_stage";
pub const EXECUTION_RESULT: &str = "execution_result";
pub const EXECUTION_INSTANCE: &str = "execution_instance";
pub const EXECUTION_PRIORITY: &str = "execution_priority";
pub const EXECUTION_WORKER_ID: &str = "execution_worker_id";
pub const EXECUTION_EXIT_CODE: &str = "execution_exit_code";

/// Remote execution stages for metrics classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStage {
    /// Unknown stage
    Unknown,
    /// Checking cache for existing results
    CacheCheck,
    /// Action is queued waiting for execution
    Queued,
    /// Action is being executed by a worker
    Executing,
    /// Action execution completed
    Completed,
}

impl From<ExecutionStage> for Value {
    fn from(stage: ExecutionStage) -> Self {
        match stage {
            ExecutionStage::Unknown => Self::from("unknown"),
            ExecutionStage::CacheCheck => Self::from("cache_check"),
            ExecutionStage::Queued => Self::from("queued"),
            ExecutionStage::Executing => Self::from("executing"),
            ExecutionStage::Completed => Self::from("completed"),
        }
    }
}

impl From<ActionStage> for ExecutionStage {
    fn from(stage: ActionStage) -> Self {
        match stage {
            ActionStage::Unknown => ExecutionStage::Unknown,
            ActionStage::CacheCheck => ExecutionStage::CacheCheck,
            ActionStage::Queued => ExecutionStage::Queued,
            ActionStage::Executing => ExecutionStage::Executing,
            ActionStage::Completed(_) | ActionStage::CompletedFromCache(_) => {
                ExecutionStage::Completed
            }
        }
    }
}

impl From<&ActionStage> for ExecutionStage {
    fn from(stage: &ActionStage) -> Self {
        match stage {
            ActionStage::Unknown => ExecutionStage::Unknown,
            ActionStage::CacheCheck => ExecutionStage::CacheCheck,
            ActionStage::Queued => ExecutionStage::Queued,
            ActionStage::Executing => ExecutionStage::Executing,
            ActionStage::Completed(_) | ActionStage::CompletedFromCache(_) => {
                ExecutionStage::Completed
            }
        }
    }
}

/// Results of remote execution operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionResult {
    /// Execution completed successfully
    Success,
    /// Execution failed
    Failure,
    /// Execution was cancelled
    Cancelled,
    /// Execution timed out
    Timeout,
    /// Result was found in cache
    CacheHit,
}

impl From<ExecutionResult> for Value {
    fn from(result: ExecutionResult) -> Self {
        match result {
            ExecutionResult::Success => Self::from("success"),
            ExecutionResult::Failure => Self::from("failure"),
            ExecutionResult::Cancelled => Self::from("cancelled"),
            ExecutionResult::Timeout => Self::from("timeout"),
            ExecutionResult::CacheHit => Self::from("cache_hit"),
        }
    }
}

/// Pre-allocated attribute combinations for efficient remote execution metrics collection.
#[derive(Debug)]
pub struct ExecutionMetricAttrs {
    // Stage transition attributes
    unknown: Vec<KeyValue>,
    cache_check: Vec<KeyValue>,
    queued: Vec<KeyValue>,
    executing: Vec<KeyValue>,
    completed_success: Vec<KeyValue>,
    completed_failure: Vec<KeyValue>,
    completed_cancelled: Vec<KeyValue>,
    completed_timeout: Vec<KeyValue>,
    completed_cache_hit: Vec<KeyValue>,
}

impl ExecutionMetricAttrs {
    /// Creates a new set of pre-computed attributes.
    ///
    /// The `base_attrs` are included in all attribute combinations (e.g., instance
    /// name, worker ID).
    #[must_use]
    pub fn new(base_attrs: &[KeyValue]) -> Self {
        let make_attrs = |stage: ExecutionStage, result: Option<ExecutionResult>| {
            let mut attrs = base_attrs.to_vec();
            attrs.push(KeyValue::new(EXECUTION_STAGE, stage));
            if let Some(result) = result {
                attrs.push(KeyValue::new(EXECUTION_RESULT, result));
            }
            attrs
        };

        Self {
            unknown: make_attrs(ExecutionStage::Unknown, None),
            cache_check: make_attrs(ExecutionStage::CacheCheck, None),
            queued: make_attrs(ExecutionStage::Queued, None),
            executing: make_attrs(ExecutionStage::Executing, None),
            completed_success: make_attrs(
                ExecutionStage::Completed,
                Some(ExecutionResult::Success),
            ),
            completed_failure: make_attrs(
                ExecutionStage::Completed,
                Some(ExecutionResult::Failure),
            ),
            completed_cancelled: make_attrs(
                ExecutionStage::Completed,
                Some(ExecutionResult::Cancelled),
            ),
            completed_timeout: make_attrs(
                ExecutionStage::Completed,
                Some(ExecutionResult::Timeout),
            ),
            completed_cache_hit: make_attrs(
                ExecutionStage::Completed,
                Some(ExecutionResult::CacheHit),
            ),
        }
    }

    // Attribute accessors
    #[must_use]
    pub fn unknown(&self) -> &[KeyValue] {
        &self.unknown
    }
    #[must_use]
    pub fn cache_check(&self) -> &[KeyValue] {
        &self.cache_check
    }
    #[must_use]
    pub fn queued(&self) -> &[KeyValue] {
        &self.queued
    }
    #[must_use]
    pub fn executing(&self) -> &[KeyValue] {
        &self.executing
    }
    #[must_use]
    pub fn completed_success(&self) -> &[KeyValue] {
        &self.completed_success
    }
    #[must_use]
    pub fn completed_failure(&self) -> &[KeyValue] {
        &self.completed_failure
    }
    #[must_use]
    pub fn completed_cancelled(&self) -> &[KeyValue] {
        &self.completed_cancelled
    }
    #[must_use]
    pub fn completed_timeout(&self) -> &[KeyValue] {
        &self.completed_timeout
    }
    #[must_use]
    pub fn completed_cache_hit(&self) -> &[KeyValue] {
        &self.completed_cache_hit
    }
}

/// Global remote execution metrics instruments.
pub static EXECUTION_METRICS: LazyLock<ExecutionMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    ExecutionMetrics {
        execution_stage_duration: meter
            .f64_histogram("execution_stage_duration")
            .with_description("Duration of each execution stage in seconds")
            .with_unit("s")
            .with_boundaries(vec![
                // Sub-second range
                0.001, // 1ms
                0.01,  // 10ms
                0.1,   // 100ms
                0.5,   // 500ms
                1.0,   // 1s
                // Multi-second range
                2.0,    // 2s
                5.0,    // 5s
                10.0,   // 10s
                30.0,   // 30s
                60.0,   // 1 minute
                120.0,  // 2 minutes
                300.0,  // 5 minutes
                600.0,  // 10 minutes
                1800.0, // 30 minutes
                3600.0, // 1 hour
            ])
            .build(),

        execution_total_duration: meter
            .f64_histogram("execution_total_duration")
            .with_description(
                "Total duration of action execution from submission to completion in seconds",
            )
            .with_unit("s")
            .with_boundaries(vec![
                // Sub-second range
                0.01, // 10ms
                0.1,  // 100ms
                0.5,  // 500ms
                1.0,  // 1s
                // Multi-second range
                5.0,    // 5s
                10.0,   // 10s
                30.0,   // 30s
                60.0,   // 1 minute
                300.0,  // 5 minutes
                600.0,  // 10 minutes
                1800.0, // 30 minutes
                3600.0, // 1 hour
                7200.0, // 2 hours
            ])
            .build(),

        execution_queue_time: meter
            .f64_histogram("execution_queue_time")
            .with_description("Time spent waiting in queue before execution in seconds")
            .with_unit("s")
            .with_boundaries(vec![
                0.001, // 1ms
                0.01,  // 10ms
                0.1,   // 100ms
                0.5,   // 500ms
                1.0,   // 1s
                2.0,   // 2s
                5.0,   // 5s
                10.0,  // 10s
                30.0,  // 30s
                60.0,  // 1 minute
                300.0, // 5 minutes
                600.0, // 10 minutes
            ])
            .build(),

        execution_active_count: meter
            .i64_up_down_counter("execution_active_count")
            .with_description("Number of actions currently in each stage")
            .with_unit("{action}")
            .build(),

        execution_completed_count: meter
            .u64_counter("execution_completed_count")
            .with_description("Total number of completed executions by result")
            .with_unit("{action}")
            .build(),

        execution_stage_transitions: meter
            .u64_counter("execution_stage_transitions")
            .with_description("Number of stage transitions")
            .with_unit("{transition}")
            .build(),

        execution_retry_count: meter
            .u64_counter("execution_retry_count")
            .with_description("Number of execution retries")
            .with_unit("{retry}")
            .build(),

        execution_actions_count: meter
            .u64_gauge("execution_actions_count")
            .with_description("Current number of actions in each stage")
            .with_unit("{action}")
            .build(),

        execution_queued_actions_count: meter
            .u64_gauge("execution_queued_actions_count")
            .with_description("Current number of queued actions by platform properties")
            .with_unit("{action}")
            .build(),
    }
});

/// OpenTelemetry metrics instruments for remote execution monitoring.
#[derive(Debug)]
pub struct ExecutionMetrics {
    /// Histogram of stage durations in seconds
    pub execution_stage_duration: metrics::Histogram<f64>,
    /// Histogram of total execution durations in seconds
    pub execution_total_duration: metrics::Histogram<f64>,
    /// Histogram of queue wait times in seconds
    pub execution_queue_time: metrics::Histogram<f64>,
    /// Current number of actions in each stage
    pub execution_active_count: metrics::UpDownCounter<i64>,
    /// Total number of completed executions
    pub execution_completed_count: metrics::Counter<u64>,
    /// Number of stage transitions
    pub execution_stage_transitions: metrics::Counter<u64>,
    /// Counter for execution retries
    pub execution_retry_count: metrics::Counter<u64>,
    /// Gauge of actions by stage
    pub execution_actions_count: metrics::Gauge<u64>,
    // Gauge of queued actions by platform properties
    pub execution_queued_actions_count: metrics::Gauge<u64>,
}

/// Helper function to create attributes for execution metrics
#[must_use]
pub fn make_execution_attributes(
    instance_name: &str,
    worker_id: Option<&str>,
    priority: Option<i32>,
) -> Vec<KeyValue> {
    let mut attrs = vec![KeyValue::new(EXECUTION_INSTANCE, instance_name.to_string())];

    if let Some(worker_id) = worker_id {
        attrs.push(KeyValue::new(EXECUTION_WORKER_ID, worker_id.to_string()));
    }

    if let Some(priority) = priority {
        attrs.push(KeyValue::new(EXECUTION_PRIORITY, i64::from(priority)));
    }

    attrs
}

// Metric attribute keys for worker pool operations.
pub const WORKER_POOL_INSTANCE: &str = "worker_pool_instance";
pub const WORKER_EVENT_TYPE: &str = "worker_pool_event_type";
pub const WORKER_STATE: &str = "worker_pool_state";

/// Worker event types for metrics classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerEventType {
    /// Worker was added to the pool
    Added,
    /// Worker was removed from the pool
    Removed,
    /// Worker timed out
    Timeout,
    /// Worker connection failed
    ConnectionFailed,
    /// Worker was evicted due to error
    Evicted,
}

impl From<WorkerEventType> for Value {
    fn from(event: WorkerEventType) -> Self {
        match event {
            WorkerEventType::Added => Self::from("added"),
            WorkerEventType::Removed => Self::from("removed"),
            WorkerEventType::Timeout => Self::from("timeout"),
            WorkerEventType::ConnectionFailed => Self::from("connection_failed"),
            WorkerEventType::Evicted => Self::from("evicted"),
        }
    }
}

/// Worker state types for metrics classification.
#[derive(Debug, Clone, Copy)]
pub enum WorkerState {
    /// Worker is available and can accept work
    Available,
    /// Worker is paused (backpressure)
    Paused,
    /// Worker is draining (not accepting new work)
    Draining,
}

impl From<WorkerState> for Value {
    fn from(state: WorkerState) -> Self {
        match state {
            WorkerState::Available => Self::from("available"),
            WorkerState::Paused => Self::from("paused"),
            WorkerState::Draining => Self::from("draining"),
        }
    }
}

/// Pre-allocated attribute combinations for efficient worker metrics collection.
#[derive(Debug)]
pub struct WorkerMetricAttrs {
    added: Vec<KeyValue>,
    removed: Vec<KeyValue>,
    timeout: Vec<KeyValue>,
    connection_failed: Vec<KeyValue>,
    evicted: Vec<KeyValue>,
    state_available: Vec<KeyValue>,
    state_paused: Vec<KeyValue>,
    state_draining: Vec<KeyValue>,
}

impl WorkerMetricAttrs {
    #[must_use]
    pub fn new(base_attrs: &[KeyValue]) -> Self {
        let make_event_attrs = |event: WorkerEventType| {
            let mut attrs = base_attrs.to_vec();
            attrs.push(KeyValue::new(WORKER_EVENT_TYPE, event));
            attrs
        };

        let make_state_attrs = |state: WorkerState| {
            let mut attrs = base_attrs.to_vec();
            attrs.push(KeyValue::new(WORKER_STATE, state));
            attrs
        };

        Self {
            added: make_event_attrs(WorkerEventType::Added),
            removed: make_event_attrs(WorkerEventType::Removed),
            timeout: make_event_attrs(WorkerEventType::Timeout),
            connection_failed: make_event_attrs(WorkerEventType::ConnectionFailed),
            evicted: make_event_attrs(WorkerEventType::Evicted),
            state_available: make_state_attrs(WorkerState::Available),
            state_paused: make_state_attrs(WorkerState::Paused),
            state_draining: make_state_attrs(WorkerState::Draining),
        }
    }

    #[must_use]
    pub fn added(&self) -> &[KeyValue] {
        &self.added
    }
    #[must_use]
    pub fn removed(&self) -> &[KeyValue] {
        &self.removed
    }
    #[must_use]
    pub fn timeout(&self) -> &[KeyValue] {
        &self.timeout
    }
    #[must_use]
    pub fn connection_failed(&self) -> &[KeyValue] {
        &self.connection_failed
    }
    #[must_use]
    pub fn evicted(&self) -> &[KeyValue] {
        &self.evicted
    }
    #[must_use]
    pub fn state_available(&self) -> &[KeyValue] {
        &self.state_available
    }
    #[must_use]
    pub fn state_paused(&self) -> &[KeyValue] {
        &self.state_paused
    }
    #[must_use]
    pub fn state_draining(&self) -> &[KeyValue] {
        &self.state_draining
    }
}

/// Global worker pool metrics instruments.
pub static WORKER_METRICS: LazyLock<WorkerPoolMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    WorkerPoolMetrics {
        worker_count: meter
            .u64_gauge("worker_pool_count")
            .with_description("Current number of workers in the pool")
            .with_unit("{worker}")
            .build(),

        worker_events: meter
            .u64_counter("worker_pool_events")
            .with_description("Total worker pool events by type")
            .with_unit("{event}")
            .build(),

        worker_actions_running: meter
            .u64_gauge("worker_pool_actions_running")
            .with_description("Current number of actions running on workers")
            .with_unit("{action}")
            .build(),

        worker_actions_dispatched: meter
            .u64_counter("worker_pool_actions_dispatched")
            .with_description("Total number of actions dispatched to workers")
            .with_unit("{action}")
            .build(),

        worker_actions_completed: meter
            .u64_counter("worker_pool_actions_completed")
            .with_description("Total number of actions completed on workers")
            .with_unit("{action}")
            .build(),

        worker_dispatch_failures: meter
            .u64_counter("worker_pool_dispatch_failures")
            .with_description("Total number of action dispatch failures")
            .with_unit("{failure}")
            .build(),
    }
});

/// OpenTelemetry metrics instruments for worker pool monitoring.
#[derive(Debug)]
pub struct WorkerPoolMetrics {
    /// Current number of workers in the pool
    pub worker_count: metrics::Gauge<u64>,
    /// Counter of worker events by type
    pub worker_events: metrics::Counter<u64>,
    /// Current number of actions running on workers
    pub worker_actions_running: metrics::Gauge<u64>,
    /// Counter of actions dispatched to workers
    pub worker_actions_dispatched: metrics::Counter<u64>,
    /// Counter of actions completed on workers
    pub worker_actions_completed: metrics::Counter<u64>,
    /// Counter of action dispatch failures
    pub worker_dispatch_failures: metrics::Counter<u64>,
}
