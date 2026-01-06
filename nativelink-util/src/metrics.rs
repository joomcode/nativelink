// Copyright 2025 The NativeLink Authors. All rights reserved.
//
// Licensed under the Business Source License, Version 1.1 (the "License");
// you may not use this file except in compliance with the License.
// You may requested a copy of the License by emailing contact@nativelink.com.
//
// Use of this module requires an enterprise license agreement, which can be
// attained by emailing contact@nativelink.com or signing up for Nativelink
// Cloud at app.nativelink.com.
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fmt::{Display, Formatter};
use std::sync::{LazyLock, OnceLock};

use crate::action_messages::ActionStage;
use opentelemetry::{global, metrics, InstrumentationScope, KeyValue, Value};

/// Callback type for observable gauges that report queued action counts.
/// The callback receives an `Observer` that should be used to record values with attributes.
pub type QueuedActionsCallback = Box<dyn Fn(&dyn Fn(u64, &[KeyValue])) + Send + Sync>;

/// Storage for the external callback for queued actions count.
static QUEUED_ACTIONS_CALLBACK: OnceLock<QueuedActionsCallback> = OnceLock::new();

/// Registers an external callback for the `execution_queued_actions_count` observable gauge.
///
/// This function can only be called once. Subsequent calls will panic.
///
/// The callback will be invoked during metrics collection and should report
/// the current count of queued actions by calling the provided observer function
/// with the count and any relevant attributes (e.g., platform properties).
///
/// # Panics
///
/// Panics if the callback has already been registered.
///
/// # Example
/// ```ignore
/// register_queued_actions_callback(Box::new(|observe| {
///     // Report counts for different platform configurations
///     observe(10, &[KeyValue::new("platform", "linux")]);
///     observe(5, &[KeyValue::new("platform", "windows")]);
/// }));
/// ```
pub fn register_queued_actions_callback(callback: QueuedActionsCallback) {
    if QUEUED_ACTIONS_CALLBACK.set(callback).is_err() {
        panic!("Queued actions callback can only be registered once");
    }
}

// Metric attribute keys for cache operations.
pub const CACHE_TYPE: &str = "cache.type";
pub const CACHE_OPERATION: &str = "cache.operation.name";
pub const CACHE_RESULT: &str = "cache.operation.result";
pub const STORE_TYPE: &str = "store.type";
pub const STORE_NAME: &str = "store.name";

// Metric attribute keys for remote execution operations.
pub const EXECUTION_STAGE: &str = "execution_stage";
pub const EXECUTION_RESULT: &str = "execution_result";
pub const EXECUTION_INSTANCE: &str = "execution_instance";
pub const EXECUTION_PRIORITY: &str = "execution_priority";
pub const EXECUTION_EXIT_CODE: &str = "execution_exit_code";

/// Cache operation types for metrics classification.
#[derive(Debug, Clone, Copy)]
pub enum CacheOperationName {
    /// Data retrieval operations (get, peek, contains, etc.)
    Read,
    /// Data storage operations (insert, update, replace, etc.)
    Write,
    /// Explicit data removal operations
    Delete,
    /// Automatic cache maintenance (evictions, TTL cleanup, etc.)
    Evict,
}

impl From<CacheOperationName> for Value {
    fn from(op: CacheOperationName) -> Self {
        match op {
            CacheOperationName::Read => Self::from("read"),
            CacheOperationName::Write => Self::from("write"),
            CacheOperationName::Delete => Self::from("delete"),
            CacheOperationName::Evict => Self::from("evict"),
        }
    }
}

/// Results of cache operations.
///
/// Result semantics vary by operation type:
/// - Read: Hit/Miss/Expired indicate data availability
/// - Write/Delete/Evict: Success/Error indicate completion status
#[derive(Debug, Clone, Copy)]
pub enum CacheOperationResult {
    /// Data found and valid (Read operations)
    Hit,
    /// Data not found (Read operations)
    Miss,
    /// Data found but invalid/expired (Read operations)
    Expired,
    /// Operation completed successfully (Write/Delete/Evict operations)
    Success,
    /// Operation failed (any operation type)
    Error,
}

impl From<CacheOperationResult> for Value {
    fn from(result: CacheOperationResult) -> Self {
        match result {
            CacheOperationResult::Hit => Self::from("hit"),
            CacheOperationResult::Miss => Self::from("miss"),
            CacheOperationResult::Expired => Self::from("expired"),
            CacheOperationResult::Success => Self::from("success"),
            CacheOperationResult::Error => Self::from("error"),
        }
    }
}

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
            ActionStage::Unknown => Self::Unknown,
            ActionStage::CacheCheck => Self::CacheCheck,
            ActionStage::Queued => Self::Queued,
            ActionStage::Executing => Self::Executing,
            ActionStage::Completed(_) | ActionStage::CompletedFromCache(_) => Self::Completed,
        }
    }
}

impl From<&ActionStage> for ExecutionStage {
    fn from(stage: &ActionStage) -> Self {
        match stage {
            ActionStage::Unknown => Self::Unknown,
            ActionStage::CacheCheck => Self::CacheCheck,
            ActionStage::Queued => Self::Queued,
            ActionStage::Executing => Self::Executing,
            ActionStage::Completed(_) | ActionStage::CompletedFromCache(_) => Self::Completed,
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

/// Pre-allocated attribute combinations for efficient cache metrics collection.
///
/// Avoids runtime allocation by pre-computing common attribute combinations
/// for cache operations and results.
#[derive(Debug)]
pub struct CacheMetricAttrs {
    // Read operation attributes
    read_hit: Vec<KeyValue>,
    read_miss: Vec<KeyValue>,
    read_expired: Vec<KeyValue>,

    // Write operation attributes
    write_success: Vec<KeyValue>,
    write_error: Vec<KeyValue>,

    // Delete operation attributes
    delete_success: Vec<KeyValue>,
    delete_miss: Vec<KeyValue>,
    delete_error: Vec<KeyValue>,

    // Evict operation attributes
    evict_success: Vec<KeyValue>,
    evict_expired: Vec<KeyValue>,
}

impl CacheMetricAttrs {
    /// Creates a new set of pre-computed attributes.
    ///
    /// The `base_attrs` are included in all attribute combinations (e.g., cache
    /// type, instance ID).
    #[must_use]
    pub fn new(base_attrs: &[KeyValue]) -> Self {
        let make_attrs = |op: CacheOperationName, result: CacheOperationResult| {
            let mut attrs = base_attrs.to_vec();
            attrs.push(KeyValue::new(CACHE_OPERATION, op));
            attrs.push(KeyValue::new(CACHE_RESULT, result));
            attrs
        };

        Self {
            read_hit: make_attrs(CacheOperationName::Read, CacheOperationResult::Hit),
            read_miss: make_attrs(CacheOperationName::Read, CacheOperationResult::Miss),
            read_expired: make_attrs(CacheOperationName::Read, CacheOperationResult::Expired),

            write_success: make_attrs(CacheOperationName::Write, CacheOperationResult::Success),
            write_error: make_attrs(CacheOperationName::Write, CacheOperationResult::Error),

            delete_success: make_attrs(CacheOperationName::Delete, CacheOperationResult::Success),
            delete_miss: make_attrs(CacheOperationName::Delete, CacheOperationResult::Miss),
            delete_error: make_attrs(CacheOperationName::Delete, CacheOperationResult::Error),

            evict_success: make_attrs(CacheOperationName::Evict, CacheOperationResult::Success),
            evict_expired: make_attrs(CacheOperationName::Evict, CacheOperationResult::Expired),
        }
    }

    // Attribute accessors
    #[must_use]
    pub fn read_hit(&self) -> &[KeyValue] {
        &self.read_hit
    }
    #[must_use]
    pub fn read_miss(&self) -> &[KeyValue] {
        &self.read_miss
    }
    #[must_use]
    pub fn read_expired(&self) -> &[KeyValue] {
        &self.read_expired
    }
    #[must_use]
    pub fn write_success(&self) -> &[KeyValue] {
        &self.write_success
    }
    #[must_use]
    pub fn write_error(&self) -> &[KeyValue] {
        &self.write_error
    }
    #[must_use]
    pub fn delete_success(&self) -> &[KeyValue] {
        &self.delete_success
    }
    #[must_use]
    pub fn delete_miss(&self) -> &[KeyValue] {
        &self.delete_miss
    }
    #[must_use]
    pub fn delete_error(&self) -> &[KeyValue] {
        &self.delete_error
    }
    #[must_use]
    pub fn evict_success(&self) -> &[KeyValue] {
        &self.evict_success
    }
    #[must_use]
    pub fn evict_expired(&self) -> &[KeyValue] {
        &self.evict_expired
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

/// Global cache metrics instruments.
pub static CACHE_METRICS: LazyLock<CacheMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    CacheMetrics {
        cache_operation_duration: meter
            .f64_histogram("cache.operation.duration")
            .with_description("Duration of cache operations in milliseconds")
            .with_unit("ms")
            // The range of these is quite large as a cache might be backed by
            // memory, a filesystem, or network storage. The current values were
            // determined empirically and might need adjustment.
            .with_boundaries(vec![
                // Microsecond range
                0.001, // 1μs
                0.005, // 5μs
                0.01,  // 10μs
                0.05,  // 50μs
                0.1,   // 100μs
                // Sub-millisecond range
                0.2, // 200μs
                0.5, // 500μs
                1.0, // 1ms
                // Low millisecond range
                2.0,   // 2ms
                5.0,   // 5ms
                10.0,  // 10ms
                20.0,  // 20ms
                50.0,  // 50ms
                100.0, // 100ms
                // Higher latency range
                200.0,  // 200ms
                500.0,  // 500ms
                1000.0, // 1 second
                2000.0, // 2 seconds
                5000.0, // 5 seconds
            ])
            .build(),

        cache_operations: meter
            .u64_counter("cache.operations")
            .with_description("Total cache operations by type and result")
            .build(),

        cache_io: meter
            .u64_counter("cache.io")
            .with_description("Total bytes processed by cache operations")
            .with_unit("By")
            .build(),

        cache_size: meter
            .i64_up_down_counter("cache.size")
            .with_description("Current total size of cached data")
            .with_unit("By")
            .build(),

        cache_entries: meter
            .i64_up_down_counter("cache.entries")
            .with_description("Current number of cached entries")
            .with_unit("{entry}")
            .build(),

        cache_entry_size: meter
            .u64_histogram("cache.item.size")
            .with_description("Size distribution of cached entries")
            .with_unit("By")
            .build(),
    }
});

/// OpenTelemetry metrics instruments for cache monitoring.
#[derive(Debug)]
pub struct CacheMetrics {
    /// Histogram of cache operation durations in milliseconds
    pub cache_operation_duration: metrics::Histogram<f64>,
    /// Counter of cache operations by type and result
    pub cache_operations: metrics::Counter<u64>,
    /// Counter of bytes read/written during cache operations
    pub cache_io: metrics::Counter<u64>,
    /// Current total size of all cached data in bytes
    pub cache_size: metrics::UpDownCounter<i64>,
    /// Current number of entries in cache
    pub cache_entries: metrics::UpDownCounter<i64>,
    /// Histogram of individual cache entry sizes in bytes
    pub cache_entry_size: metrics::Histogram<u64>,
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

        execution_output_size: meter
            .u64_histogram("execution_output_size")
            .with_description("Size of execution outputs in bytes")
            .with_unit("By")
            .with_boundaries(vec![
                1_024.0,          // 1KB
                10_240.0,         // 10KB
                102_400.0,        // 100KB
                1_048_576.0,      // 1MB
                10_485_760.0,     // 10MB
                104_857_600.0,    // 100MB
                1_073_741_824.0,  // 1GB
                10_737_418_240.0, // 10GB
            ])
            .build(),

        execution_cpu_time: meter
            .f64_histogram("execution_cpu_time")
            .with_description("CPU time consumed by action execution in seconds")
            .with_unit("s")
            .with_boundaries(vec![
                0.01,   // 10ms
                0.1,    // 100ms
                1.0,    // 1s
                10.0,   // 10s
                60.0,   // 1 minute
                300.0,  // 5 minutes
                600.0,  // 10 minutes
                1800.0, // 30 minutes
                3600.0, // 1 hour
            ])
            .build(),

        execution_memory_usage: meter
            .u64_histogram("execution_memory_usage")
            .with_description("Peak memory usage during execution in bytes")
            .with_unit("By")
            .with_boundaries(vec![
                1_048_576.0,      // 1MB
                10_485_760.0,     // 10MB
                104_857_600.0,    // 100MB
                524_288_000.0,    // 500MB
                1_073_741_824.0,  // 1GB
                5_368_709_120.0,  // 5GB
                10_737_418_240.0, // 10GB
                53_687_091_200.0, // 50GB
            ])
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
            .u64_observable_gauge("execution_queued_actions_count_observable")
            .with_description("Current number of queued actions by platform properties")
            .with_unit("{action}")
            .with_callback(|observer| {
                if let Some(callback) = QUEUED_ACTIONS_CALLBACK.get() {
                    callback(&|value, attrs| {
                        observer.observe(value, attrs);
                    });
                }
            })
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
    /// Histogram of output sizes in bytes
    pub execution_output_size: metrics::Histogram<u64>,
    /// Histogram of CPU time in seconds
    pub execution_cpu_time: metrics::Histogram<f64>,
    /// Histogram of peak memory usage in bytes
    pub execution_memory_usage: metrics::Histogram<u64>,
    /// Counter for execution retries
    pub execution_retry_count: metrics::Counter<u64>,
    /// Gauge of actions by stage
    pub execution_actions_count: metrics::Gauge<u64>,
    // Gauge of queued actions by platform properties
    pub execution_queued_actions_count: metrics::ObservableGauge<u64>,
}

/// Helper function to create attributes for execution metrics
#[must_use]
pub fn make_execution_attributes(
    instance_name: &str,
    priority: Option<i32>,
) -> Vec<KeyValue> {
    let mut attrs = vec![KeyValue::new(EXECUTION_INSTANCE, instance_name.to_string())];

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
pub struct WorkerPoolMetricAttrs {
    added: Vec<KeyValue>,
    removed: Vec<KeyValue>,
    timeout: Vec<KeyValue>,
    connection_failed: Vec<KeyValue>,
    evicted: Vec<KeyValue>,
    state_available: Vec<KeyValue>,
    state_paused: Vec<KeyValue>,
    state_draining: Vec<KeyValue>,
}

impl WorkerPoolMetricAttrs {
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
pub static WORKER_POOL_METRICS: LazyLock<WorkerPoolMetrics> = LazyLock::new(|| {
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

// Metric attribute keys for local worker operations.
pub const WORKER_NAME: &str = "worker.name";
pub const WORKER_OPERATION: &str = "worker.operation";
pub const WORKER_RESULT: &str = "worker.result";

/// Global local worker metrics instruments.
pub static LOCAL_WORKER_METRICS: LazyLock<LocalWorkerMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    LocalWorkerMetrics {
        start_actions_received: meter
            .u64_counter("worker_start_actions_received")
            .with_description("Total number of actions sent to this worker to process")
            .with_unit("{action}")
            .build(),

        disconnects_received: meter
            .u64_counter("worker_disconnects_received")
            .with_description("Total number of disconnects received from the scheduler")
            .with_unit("{disconnect}")
            .build(),

        keep_alives_received: meter
            .u64_counter("worker_keep_alives_received")
            .with_description("Total number of keep-alives received from the scheduler")
            .with_unit("{keepalive}")
            .build(),

        preconditions_calls: meter
            .u64_counter("worker_preconditions_calls")
            .with_description("Total number of precondition check calls")
            .with_unit("{call}")
            .build(),

        preconditions_successes: meter
            .u64_counter("worker_preconditions_successes")
            .with_description("Total number of successful precondition checks")
            .with_unit("{success}")
            .build(),

        preconditions_failures: meter
            .u64_counter("worker_preconditions_failures")
            .with_description("Total number of failed precondition checks")
            .with_unit("{failure}")
            .build(),

        preconditions_duration: meter
            .f64_histogram("worker_preconditions_duration")
            .with_description("Duration of precondition checks in milliseconds")
            .with_unit("ms")
            .with_boundaries(vec![
                0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0,
                5000.0,
            ])
            .build(),
    }
});

/// OpenTelemetry metrics instruments for local worker monitoring.
#[derive(Debug)]
pub struct LocalWorkerMetrics {
    /// Counter for actions received by the worker
    pub start_actions_received: metrics::Counter<u64>,
    /// Counter for disconnects received from scheduler
    pub disconnects_received: metrics::Counter<u64>,
    /// Counter for keep-alives received from scheduler
    pub keep_alives_received: metrics::Counter<u64>,
    /// Counter for precondition check calls
    pub preconditions_calls: metrics::Counter<u64>,
    /// Counter for successful precondition checks
    pub preconditions_successes: metrics::Counter<u64>,
    /// Counter for failed precondition checks
    pub preconditions_failures: metrics::Counter<u64>,
    /// Histogram for precondition check durations
    pub preconditions_duration: metrics::Histogram<f64>,
}

/// Pre-allocated attribute combinations for efficient worker metrics collection.
#[derive(Debug)]
pub struct WorkerMetricAttrs {
    base: Vec<KeyValue>,
}

impl WorkerMetricAttrs {
    /// Creates a new set of pre-computed attributes with the worker name.
    #[must_use]
    pub fn new(worker_name: &str) -> Self {
        Self {
            base: vec![KeyValue::new(WORKER_NAME, worker_name.to_string())],
        }
    }

    #[must_use]
    pub fn base(&self) -> &[KeyValue] {
        &self.base
    }
}

// Metric attribute keys for running actions operations.
pub const RUNNING_ACTION_OPERATION: &str = "running_action.operation";
pub const RUNNING_ACTION_RESULT: &str = "running_action.result";

/// Global running actions metrics instruments.
pub static RUNNING_ACTIONS_METRICS: LazyLock<RunningActionsMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    // Helper to create standard histogram boundaries for operation durations
    let duration_boundaries = vec![
        0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0,
        10000.0, 30000.0, 60000.0,
    ];

    RunningActionsMetrics {
        // Async operation counters
        create_and_add_action_calls: meter
            .u64_counter("running_actions_create_and_add_action_calls")
            .with_description("Total calls to create_and_add_action")
            .with_unit("{call}")
            .build(),
        create_and_add_action_successes: meter
            .u64_counter("running_actions_create_and_add_action_successes")
            .with_description("Successful create_and_add_action operations")
            .with_unit("{success}")
            .build(),
        create_and_add_action_failures: meter
            .u64_counter("running_actions_create_and_add_action_failures")
            .with_description("Failed create_and_add_action operations")
            .with_unit("{failure}")
            .build(),
        create_and_add_action_duration: meter
            .f64_histogram("running_actions_create_and_add_action_duration")
            .with_description("Duration of create_and_add_action operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        cache_action_result_calls: meter
            .u64_counter("running_actions_cache_action_result_calls")
            .with_description("Total calls to cache_action_result")
            .with_unit("{call}")
            .build(),
        cache_action_result_successes: meter
            .u64_counter("running_actions_cache_action_result_successes")
            .with_description("Successful cache_action_result operations")
            .with_unit("{success}")
            .build(),
        cache_action_result_failures: meter
            .u64_counter("running_actions_cache_action_result_failures")
            .with_description("Failed cache_action_result operations")
            .with_unit("{failure}")
            .build(),
        cache_action_result_duration: meter
            .f64_histogram("running_actions_cache_action_result_duration")
            .with_description("Duration of cache_action_result operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        kill_all_calls: meter
            .u64_counter("running_actions_kill_all_calls")
            .with_description("Total calls to kill_all")
            .with_unit("{call}")
            .build(),
        kill_all_duration: meter
            .f64_histogram("running_actions_kill_all_duration")
            .with_description("Duration of kill_all operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        create_action_info_calls: meter
            .u64_counter("running_actions_create_action_info_calls")
            .with_description("Total calls to create_action_info")
            .with_unit("{call}")
            .build(),
        create_action_info_successes: meter
            .u64_counter("running_actions_create_action_info_successes")
            .with_description("Successful create_action_info operations")
            .with_unit("{success}")
            .build(),
        create_action_info_failures: meter
            .u64_counter("running_actions_create_action_info_failures")
            .with_description("Failed create_action_info operations")
            .with_unit("{failure}")
            .build(),
        create_action_info_duration: meter
            .f64_histogram("running_actions_create_action_info_duration")
            .with_description("Duration of create_action_info operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        make_action_directory_calls: meter
            .u64_counter("running_actions_make_action_directory_calls")
            .with_description("Total calls to make_action_directory")
            .with_unit("{call}")
            .build(),
        make_action_directory_successes: meter
            .u64_counter("running_actions_make_action_directory_successes")
            .with_description("Successful make_action_directory operations")
            .with_unit("{success}")
            .build(),
        make_action_directory_failures: meter
            .u64_counter("running_actions_make_action_directory_failures")
            .with_description("Failed make_action_directory operations")
            .with_unit("{failure}")
            .build(),
        make_action_directory_duration: meter
            .f64_histogram("running_actions_make_action_directory_duration")
            .with_description("Duration of make_action_directory operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        prepare_action_calls: meter
            .u64_counter("running_actions_prepare_action_calls")
            .with_description("Total calls to prepare_action")
            .with_unit("{call}")
            .build(),
        prepare_action_successes: meter
            .u64_counter("running_actions_prepare_action_successes")
            .with_description("Successful prepare_action operations")
            .with_unit("{success}")
            .build(),
        prepare_action_failures: meter
            .u64_counter("running_actions_prepare_action_failures")
            .with_description("Failed prepare_action operations")
            .with_unit("{failure}")
            .build(),
        prepare_action_duration: meter
            .f64_histogram("running_actions_prepare_action_duration")
            .with_description("Duration of prepare_action operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        execute_calls: meter
            .u64_counter("running_actions_execute_calls")
            .with_description("Total calls to execute")
            .with_unit("{call}")
            .build(),
        execute_successes: meter
            .u64_counter("running_actions_execute_successes")
            .with_description("Successful execute operations")
            .with_unit("{success}")
            .build(),
        execute_failures: meter
            .u64_counter("running_actions_execute_failures")
            .with_description("Failed execute operations")
            .with_unit("{failure}")
            .build(),
        execute_duration: meter
            .f64_histogram("running_actions_execute_duration")
            .with_description("Duration of execute operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        upload_results_calls: meter
            .u64_counter("running_actions_upload_results_calls")
            .with_description("Total calls to upload_results")
            .with_unit("{call}")
            .build(),
        upload_results_successes: meter
            .u64_counter("running_actions_upload_results_successes")
            .with_description("Successful upload_results operations")
            .with_unit("{success}")
            .build(),
        upload_results_failures: meter
            .u64_counter("running_actions_upload_results_failures")
            .with_description("Failed upload_results operations")
            .with_unit("{failure}")
            .build(),
        upload_results_duration: meter
            .f64_histogram("running_actions_upload_results_duration")
            .with_description("Duration of upload_results operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        cleanup_calls: meter
            .u64_counter("running_actions_cleanup_calls")
            .with_description("Total calls to cleanup")
            .with_unit("{call}")
            .build(),
        cleanup_successes: meter
            .u64_counter("running_actions_cleanup_successes")
            .with_description("Successful cleanup operations")
            .with_unit("{success}")
            .build(),
        cleanup_failures: meter
            .u64_counter("running_actions_cleanup_failures")
            .with_description("Failed cleanup operations")
            .with_unit("{failure}")
            .build(),
        cleanup_duration: meter
            .f64_histogram("running_actions_cleanup_duration")
            .with_description("Duration of cleanup operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        get_finished_result_calls: meter
            .u64_counter("running_actions_get_finished_result_calls")
            .with_description("Total calls to get_finished_result")
            .with_unit("{call}")
            .build(),
        get_finished_result_successes: meter
            .u64_counter("running_actions_get_finished_result_successes")
            .with_description("Successful get_finished_result operations")
            .with_unit("{success}")
            .build(),
        get_finished_result_failures: meter
            .u64_counter("running_actions_get_finished_result_failures")
            .with_description("Failed get_finished_result operations")
            .with_unit("{failure}")
            .build(),
        get_finished_result_duration: meter
            .f64_histogram("running_actions_get_finished_result_duration")
            .with_description("Duration of get_finished_result operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        // Simple counters
        cleanup_waits: meter
            .u64_counter("running_actions_cleanup_waits")
            .with_description("Number of times an action waited for cleanup to complete")
            .with_unit("{wait}")
            .build(),

        stale_removals: meter
            .u64_counter("running_actions_stale_removals")
            .with_description("Number of stale directories removed during action retries")
            .with_unit("{removal}")
            .build(),

        cleanup_wait_timeouts: meter
            .u64_counter("running_actions_cleanup_wait_timeouts")
            .with_description("Number of timeouts while waiting for cleanup to complete")
            .with_unit("{timeout}")
            .build(),

        // Additional async operation metrics
        get_proto_command_from_store_calls: meter
            .u64_counter("running_actions_get_proto_command_from_store_calls")
            .with_description("Total calls to get_proto_command_from_store")
            .with_unit("{call}")
            .build(),
        get_proto_command_from_store_successes: meter
            .u64_counter("running_actions_get_proto_command_from_store_successes")
            .with_description("Successful get_proto_command_from_store operations")
            .with_unit("{success}")
            .build(),
        get_proto_command_from_store_failures: meter
            .u64_counter("running_actions_get_proto_command_from_store_failures")
            .with_description("Failed get_proto_command_from_store operations")
            .with_unit("{failure}")
            .build(),
        get_proto_command_from_store_duration: meter
            .f64_histogram("running_actions_get_proto_command_from_store_duration")
            .with_description("Duration of get_proto_command_from_store operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        download_to_directory_calls: meter
            .u64_counter("running_actions_download_to_directory_calls")
            .with_description("Total calls to download_to_directory")
            .with_unit("{call}")
            .build(),
        download_to_directory_successes: meter
            .u64_counter("running_actions_download_to_directory_successes")
            .with_description("Successful download_to_directory operations")
            .with_unit("{success}")
            .build(),
        download_to_directory_failures: meter
            .u64_counter("running_actions_download_to_directory_failures")
            .with_description("Failed download_to_directory operations")
            .with_unit("{failure}")
            .build(),
        download_to_directory_duration: meter
            .f64_histogram("running_actions_download_to_directory_duration")
            .with_description("Duration of download_to_directory operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        prepare_output_files_calls: meter
            .u64_counter("running_actions_prepare_output_files_calls")
            .with_description("Total calls to prepare_output_files")
            .with_unit("{call}")
            .build(),
        prepare_output_files_successes: meter
            .u64_counter("running_actions_prepare_output_files_successes")
            .with_description("Successful prepare_output_files operations")
            .with_unit("{success}")
            .build(),
        prepare_output_files_failures: meter
            .u64_counter("running_actions_prepare_output_files_failures")
            .with_description("Failed prepare_output_files operations")
            .with_unit("{failure}")
            .build(),
        prepare_output_files_duration: meter
            .f64_histogram("running_actions_prepare_output_files_duration")
            .with_description("Duration of prepare_output_files operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        prepare_output_paths_calls: meter
            .u64_counter("running_actions_prepare_output_paths_calls")
            .with_description("Total calls to prepare_output_paths")
            .with_unit("{call}")
            .build(),
        prepare_output_paths_successes: meter
            .u64_counter("running_actions_prepare_output_paths_successes")
            .with_description("Successful prepare_output_paths operations")
            .with_unit("{success}")
            .build(),
        prepare_output_paths_failures: meter
            .u64_counter("running_actions_prepare_output_paths_failures")
            .with_description("Failed prepare_output_paths operations")
            .with_unit("{failure}")
            .build(),
        prepare_output_paths_duration: meter
            .f64_histogram("running_actions_prepare_output_paths_duration")
            .with_description("Duration of prepare_output_paths operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        child_process_calls: meter
            .u64_counter("running_actions_child_process_calls")
            .with_description("Total calls to child_process")
            .with_unit("{call}")
            .build(),
        child_process_successes: meter
            .u64_counter("running_actions_child_process_successes")
            .with_description("Successful child_process operations")
            .with_unit("{success}")
            .build(),
        child_process_duration: meter
            .f64_histogram("running_actions_child_process_duration")
            .with_description("Duration of child_process operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        child_process_success_error_code: meter
            .u64_counter("running_actions_child_process_success_exit_code")
            .with_description("Number of child processes with success exit code (0)")
            .with_unit("{process}")
            .build(),

        child_process_failure_error_code: meter
            .u64_counter("running_actions_child_process_failure_exit_code")
            .with_description("Number of child processes with non-zero exit code")
            .with_unit("{process}")
            .build(),

        upload_stdout_calls: meter
            .u64_counter("running_actions_upload_stdout_calls")
            .with_description("Total calls to upload_stdout")
            .with_unit("{call}")
            .build(),
        upload_stdout_successes: meter
            .u64_counter("running_actions_upload_stdout_successes")
            .with_description("Successful upload_stdout operations")
            .with_unit("{success}")
            .build(),
        upload_stdout_failures: meter
            .u64_counter("running_actions_upload_stdout_failures")
            .with_description("Failed upload_stdout operations")
            .with_unit("{failure}")
            .build(),
        upload_stdout_duration: meter
            .f64_histogram("running_actions_upload_stdout_duration")
            .with_description("Duration of upload_stdout operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        upload_stderr_calls: meter
            .u64_counter("running_actions_upload_stderr_calls")
            .with_description("Total calls to upload_stderr")
            .with_unit("{call}")
            .build(),
        upload_stderr_successes: meter
            .u64_counter("running_actions_upload_stderr_successes")
            .with_description("Successful upload_stderr operations")
            .with_unit("{success}")
            .build(),
        upload_stderr_failures: meter
            .u64_counter("running_actions_upload_stderr_failures")
            .with_description("Failed upload_stderr operations")
            .with_unit("{failure}")
            .build(),
        upload_stderr_duration: meter
            .f64_histogram("running_actions_upload_stderr_duration")
            .with_description("Duration of upload_stderr operations")
            .with_unit("ms")
            .with_boundaries(duration_boundaries.clone())
            .build(),

        task_timeouts: meter
            .u64_counter("running_actions_task_timeouts")
            .with_description("Total number of task timeouts")
            .with_unit("{timeout}")
            .build(),
    }
});

/// OpenTelemetry metrics instruments for running actions monitoring.
#[derive(Debug)]
pub struct RunningActionsMetrics {
    // create_and_add_action metrics
    pub create_and_add_action_calls: metrics::Counter<u64>,
    pub create_and_add_action_successes: metrics::Counter<u64>,
    pub create_and_add_action_failures: metrics::Counter<u64>,
    pub create_and_add_action_duration: metrics::Histogram<f64>,

    // cache_action_result metrics
    pub cache_action_result_calls: metrics::Counter<u64>,
    pub cache_action_result_successes: metrics::Counter<u64>,
    pub cache_action_result_failures: metrics::Counter<u64>,
    pub cache_action_result_duration: metrics::Histogram<f64>,

    // kill_all metrics
    pub kill_all_calls: metrics::Counter<u64>,
    pub kill_all_duration: metrics::Histogram<f64>,

    // create_action_info metrics
    pub create_action_info_calls: metrics::Counter<u64>,
    pub create_action_info_successes: metrics::Counter<u64>,
    pub create_action_info_failures: metrics::Counter<u64>,
    pub create_action_info_duration: metrics::Histogram<f64>,

    // make_action_directory metrics
    pub make_action_directory_calls: metrics::Counter<u64>,
    pub make_action_directory_successes: metrics::Counter<u64>,
    pub make_action_directory_failures: metrics::Counter<u64>,
    pub make_action_directory_duration: metrics::Histogram<f64>,

    // prepare_action metrics
    pub prepare_action_calls: metrics::Counter<u64>,
    pub prepare_action_successes: metrics::Counter<u64>,
    pub prepare_action_failures: metrics::Counter<u64>,
    pub prepare_action_duration: metrics::Histogram<f64>,

    // execute metrics
    pub execute_calls: metrics::Counter<u64>,
    pub execute_successes: metrics::Counter<u64>,
    pub execute_failures: metrics::Counter<u64>,
    pub execute_duration: metrics::Histogram<f64>,

    // upload_results metrics
    pub upload_results_calls: metrics::Counter<u64>,
    pub upload_results_successes: metrics::Counter<u64>,
    pub upload_results_failures: metrics::Counter<u64>,
    pub upload_results_duration: metrics::Histogram<f64>,

    // cleanup metrics
    pub cleanup_calls: metrics::Counter<u64>,
    pub cleanup_successes: metrics::Counter<u64>,
    pub cleanup_failures: metrics::Counter<u64>,
    pub cleanup_duration: metrics::Histogram<f64>,

    // get_finished_result metrics
    pub get_finished_result_calls: metrics::Counter<u64>,
    pub get_finished_result_successes: metrics::Counter<u64>,
    pub get_finished_result_failures: metrics::Counter<u64>,
    pub get_finished_result_duration: metrics::Histogram<f64>,

    // Simple counters
    pub cleanup_waits: metrics::Counter<u64>,
    pub stale_removals: metrics::Counter<u64>,
    pub cleanup_wait_timeouts: metrics::Counter<u64>,

    // get_proto_command_from_store metrics
    pub get_proto_command_from_store_calls: metrics::Counter<u64>,
    pub get_proto_command_from_store_successes: metrics::Counter<u64>,
    pub get_proto_command_from_store_failures: metrics::Counter<u64>,
    pub get_proto_command_from_store_duration: metrics::Histogram<f64>,

    // download_to_directory metrics
    pub download_to_directory_calls: metrics::Counter<u64>,
    pub download_to_directory_successes: metrics::Counter<u64>,
    pub download_to_directory_failures: metrics::Counter<u64>,
    pub download_to_directory_duration: metrics::Histogram<f64>,

    // prepare_output_files metrics
    pub prepare_output_files_calls: metrics::Counter<u64>,
    pub prepare_output_files_successes: metrics::Counter<u64>,
    pub prepare_output_files_failures: metrics::Counter<u64>,
    pub prepare_output_files_duration: metrics::Histogram<f64>,

    // prepare_output_paths metrics
    pub prepare_output_paths_calls: metrics::Counter<u64>,
    pub prepare_output_paths_successes: metrics::Counter<u64>,
    pub prepare_output_paths_failures: metrics::Counter<u64>,
    pub prepare_output_paths_duration: metrics::Histogram<f64>,

    // child_process metrics
    pub child_process_calls: metrics::Counter<u64>,
    pub child_process_successes: metrics::Counter<u64>,
    pub child_process_duration: metrics::Histogram<f64>,
    pub child_process_success_error_code: metrics::Counter<u64>,
    pub child_process_failure_error_code: metrics::Counter<u64>,

    // upload_stdout metrics
    pub upload_stdout_calls: metrics::Counter<u64>,
    pub upload_stdout_successes: metrics::Counter<u64>,
    pub upload_stdout_failures: metrics::Counter<u64>,
    pub upload_stdout_duration: metrics::Histogram<f64>,

    // upload_stderr metrics
    pub upload_stderr_calls: metrics::Counter<u64>,
    pub upload_stderr_successes: metrics::Counter<u64>,
    pub upload_stderr_failures: metrics::Counter<u64>,
    pub upload_stderr_duration: metrics::Histogram<f64>,

    // Other counters
    pub task_timeouts: metrics::Counter<u64>,
}

/// Global fast/slow store metrics instruments.
pub static FAST_SLOW_STORE_METRICS: LazyLock<FastSlowStoreMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    FastSlowStoreMetrics {
        fast_store_hit_count: meter
            .u64_counter("fast_slow_store.fast_store.hit_count")
            .with_description("Hit count for the fast store")
            .with_unit("{hit}")
            .build(),

        fast_store_downloaded_bytes: meter
            .u64_counter("fast_slow_store.fast_store.downloaded_bytes")
            .with_description("Downloaded bytes from the fast store")
            .with_unit("By")
            .build(),

        slow_store_hit_count: meter
            .u64_counter("fast_slow_store.slow_store.hit_count")
            .with_description("Hit count for the slow store")
            .with_unit("{hit}")
            .build(),

        slow_store_downloaded_bytes: meter
            .u64_counter("fast_slow_store.slow_store.downloaded_bytes")
            .with_description("Downloaded bytes from the slow store")
            .with_unit("By")
            .build(),
    }
});

/// OpenTelemetry metrics instruments for fast/slow store monitoring.
#[derive(Debug)]
pub struct FastSlowStoreMetrics {
    /// Counter of cache hits on the fast store
    pub fast_store_hit_count: metrics::Counter<u64>,
    /// Counter of bytes downloaded from the fast store
    pub fast_store_downloaded_bytes: metrics::Counter<u64>,
    /// Counter of cache hits on the slow store
    pub slow_store_hit_count: metrics::Counter<u64>,
    /// Counter of bytes downloaded from the slow store
    pub slow_store_downloaded_bytes: metrics::Counter<u64>,
}

#[derive(Debug, Copy, Clone)]
pub enum StoreType {
    Filesystem,
    S3,
    Gcs,
    Grpc,
    Mongo,
    Redis,
    OntapS3,
    OntapS3ExistenceCache,
    Memory,
    Noop,
    Compression,
    Dedup,
    ExistenceCache,
    FastSlow,
    SizePartitioning,
    CompletenessChecking,
    Verify,
    Ref,
    Shard,
    Metrics,
}

impl Display for StoreType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreType::Filesystem => write!(f, "filesystem"),
            StoreType::S3 => write!(f, "s3"),
            StoreType::Grpc => write!(f, "grpc"),
            StoreType::Mongo => write!(f, "mongo"),
            StoreType::Redis => write!(f, "redis"),
            StoreType::Gcs => write!(f, "gcs"),
            StoreType::OntapS3 => write!(f, "ontap_s3"),
            StoreType::OntapS3ExistenceCache => write!(f, "ontap_s3_existence_cache"),
            StoreType::Memory => write!(f, "memory"),
            StoreType::Noop => write!(f, "noop"),
            StoreType::Compression => write!(f, "compression"),
            StoreType::Dedup => write!(f, "dedup"),
            StoreType::ExistenceCache => write!(f, "existence_cache"),
            StoreType::FastSlow => write!(f, "fast_slow"),
            StoreType::SizePartitioning => write!(f, "size_partitioning"),
            StoreType::CompletenessChecking => write!(f, "completeness_checking"),
            StoreType::Verify => write!(f, "verify"),
            StoreType::Ref => write!(f, "ref"),
            StoreType::Shard => write!(f, "shard"),
            StoreType::Metrics => write!(f, "metrics"),
        }
    }
}

pub static STORE_METRICS: LazyLock<StoreMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    StoreMetrics {
        store_operations: meter
            .u64_counter("store_operations")
            .with_description("Total cache operations by type and result")
            .build(),

        store_operation_duration: meter
            .f64_histogram("store_operation_duration")
            .with_description("Duration of store operations in milliseconds")
            .with_unit("ms")
            // The range of these is quite large as a store might be backed by
            // memory, a filesystem, or network storage. The current values were
            // determined empirically and might need adjustment.
            .with_boundaries(vec![
                // Microsecond range
                0.001, // 1μs
                0.005, // 5μs
                0.01,  // 10μs
                0.05,  // 50μs
                0.1,   // 100μs
                // Sub-millisecond range
                0.2, // 200μs
                0.5, // 500μs
                1.0, // 1ms
                // Low millisecond range
                2.0,   // 2ms
                5.0,   // 5ms
                10.0,  // 10ms
                20.0,  // 20ms
                50.0,  // 50ms
                100.0, // 100ms
                // Higher latency range
                200.0,  // 200ms
                500.0,  // 500ms
                1000.0, // 1 second
                2000.0, // 2 seconds
                5000.0, // 5 seconds
            ])
            .build(),

        eviction_count: meter
            .u64_counter("eviction_count")
            .with_description("Number of evictions")
            .build(),
    }
});

#[derive(Debug)]
pub struct StoreMetrics {
    /// Histogram of store operation durations in milliseconds
    pub store_operation_duration: metrics::Histogram<f64>,
    /// Counter of store operations by type and result
    pub store_operations: metrics::Counter<u64>,
    /// Counter of evictions
    pub eviction_count: metrics::Counter<u64>,
}

#[derive(Debug, Clone)]
pub struct StoreMetricAttrs {
    cache_hit: Vec<KeyValue>,
    cache_miss: Vec<KeyValue>,

    read_success: Vec<KeyValue>,
    read_error: Vec<KeyValue>,
    write_success: Vec<KeyValue>,
    write_error: Vec<KeyValue>,
    eviction: Vec<KeyValue>,
}

impl StoreMetricAttrs {
    /// Creates a new set of pre-computed attributes.
    ///
    /// The `base_attrs` are included in all attribute combinations (e.g., store
    /// type, instance ID).
    #[must_use]
    pub fn new_with_name(store_type: StoreType, name: &str) -> Self {
        let make_attrs = |op: CacheOperationName, result: CacheOperationResult| {
            let mut attrs = vec![
                KeyValue::new(STORE_TYPE, store_type.to_string()),
                KeyValue::new(STORE_NAME, name.to_string()),
            ];
            attrs.push(KeyValue::new(CACHE_OPERATION, op));
            attrs.push(KeyValue::new(CACHE_RESULT, result));
            attrs
        };

        Self {
            cache_hit: make_attrs(CacheOperationName::Read, CacheOperationResult::Hit),
            cache_miss: make_attrs(CacheOperationName::Read, CacheOperationResult::Miss),

            read_success: make_attrs(CacheOperationName::Read, CacheOperationResult::Success),
            read_error: make_attrs(CacheOperationName::Read, CacheOperationResult::Error),
            write_success: make_attrs(CacheOperationName::Write, CacheOperationResult::Success),
            write_error: make_attrs(CacheOperationName::Write, CacheOperationResult::Error),
            eviction: make_attrs(CacheOperationName::Evict, CacheOperationResult::Success),
        }
    }

    // Attribute accessors
    #[must_use]
    pub fn cache_hit(&self) -> &[KeyValue] {
        &self.cache_hit
    }
    #[must_use]
    pub fn cache_miss(&self) -> &[KeyValue] {
        &self.cache_miss
    }
    #[must_use]
    pub fn read_success(&self) -> &[KeyValue] {
        &self.read_success
    }
    #[must_use]
    pub fn read_error(&self) -> &[KeyValue] {
        &self.read_error
    }
    #[must_use]
    pub fn write_success(&self) -> &[KeyValue] {
        &self.write_success
    }
    #[must_use]
    pub fn write_error(&self) -> &[KeyValue] {
        &self.write_error
    }
    #[must_use]
    pub fn eviction(&self) -> &[KeyValue] {
        &self.eviction
    }
}
