// Copyright 2025 Trace Machina, Inc. All rights reserved.
//
// Licensed under the Business Source License, Version 1.1 (the "License");
// you may not use this file except in compliance with the License.
// You may request a copy of the License by emailing contact@nativelink.com.
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

use core::fmt::{Display, Formatter};
use std::collections::HashSet;
use std::sync::{LazyLock, OnceLock};
use std::time::SystemTime;

use nativelink_error::{Code, make_err};
use opentelemetry::{InstrumentationScope, KeyValue, Value, global, metrics};

use crate::action_messages::{ActionResult, ActionStage};

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
    assert!(
        !QUEUED_ACTIONS_CALLBACK.set(callback).is_err(),
        "Queued actions callback can only be registered once"
    );
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
pub const EXECUTION_OUTCOME: &str = "execution_outcome";
pub const EXECUTION_ACTION_MNEMONIC: &str = "execution_action_mnemonic";
pub const EXECUTION_EXIT_CODE: &str = "execution_exit_code";
pub const EXECUTION_IDENTITY: &str = "execution_identity";

/// `EXECUTION_IDENTITY` value for requests that carry no `enduser.id` baggage.
pub const EXECUTION_IDENTITY_UNKNOWN: &str = "unknown";

/// `EXECUTION_IDENTITY` value for identities seen after the label set is full.
pub const EXECUTION_IDENTITY_OTHER: &str = "other";

/// The identities configured by [`set_identity_allowlist`].
static IDENTITY_ALLOWLIST: OnceLock<HashSet<Box<str>>> = OnceLock::new();

/// Configures which client identities are reported in the `EXECUTION_IDENTITY`
/// metric label, e.g. `["ci", "local"]`.
///
/// The `enduser.id` baggage an identity comes from is client-controlled free
/// text, so it is never used as a label value directly: anything not listed
/// here is reported as [`EXECUTION_IDENTITY_OTHER`]. The label therefore has
/// exactly `identities.len() + 2` possible values no matter what clients send.
/// Empty entries are ignored, since an absent identity is already reported as
/// [`EXECUTION_IDENTITY_UNKNOWN`].
///
/// Call this during startup, before any execution metric is recorded; leaving
/// it unset means no identity is distinguished.
///
/// # Errors
///
/// Returns an error if the allowlist was already configured.
pub fn set_identity_allowlist(
    identities: impl IntoIterator<Item = String>,
) -> Result<(), nativelink_error::Error> {
    let identities: HashSet<Box<str>> = identities
        .into_iter()
        .filter(|identity| !identity.is_empty())
        .map(String::into_boxed_str)
        .collect();
    IDENTITY_ALLOWLIST
        .set(identities)
        .map_err(|_| make_err!(Code::Internal, "set_identity_allowlist already set"))
}

/// Maps a client identity to its `EXECUTION_IDENTITY` metric label value.
///
/// An absent identity becomes [`EXECUTION_IDENTITY_UNKNOWN`] and one that isn't
/// allowlisted becomes [`EXECUTION_IDENTITY_OTHER`], keeping the label's
/// cardinality bounded by configuration rather than by client behavior. See
/// [`set_identity_allowlist`].
#[must_use]
pub fn identity_label(identity: &str) -> &'static str {
    if identity.is_empty() {
        return EXECUTION_IDENTITY_UNKNOWN;
    }
    // The allowlist lives in a `OnceLock` for the rest of the process, so its
    // entries can be handed out as `&'static str` attribute values.
    IDENTITY_ALLOWLIST
        .get()
        .and_then(|allowed| allowed.get(identity))
        .map_or(EXECUTION_IDENTITY_OTHER, |identity| &**identity)
}

// Metric attribute keys for gRPC serving, following OTel rpc semconv.
pub const RPC_SERVICE: &str = "rpc.service";
pub const RPC_METHOD: &str = "rpc.method";
pub const RPC_STATUS_CODE: &str = "rpc.grpc.status_code";

// Metric attribute keys for the scheduler.
pub const SCHEDULER_MATCH_RESULT: &str = "scheduler.match.result";
/// The platform properties no worker could satisfy, comma separated.
pub const SCHEDULER_UNSATISFIABLE_PROPERTIES: &str = "scheduler.unsatisfiable.properties";
pub const SCHEDULER_HOLD_OUTCOME: &str = "scheduler.hold.outcome";
/// The age bucket of a queued or executing action.
pub const SCHEDULER_ACTION_AGE: &str = "scheduler.action.age";

// Metric attribute keys for the worker.
pub const WORKER_PREPARE_OUTCOME: &str = "worker.prepare.outcome";
pub const WORKER_RELINK_KIND: &str = "worker.relink.kind";

// Metric attribute keys for object stores.
pub const STORE_OBJECT_PROVIDER: &str = "store.object.provider";
pub const STORE_OBJECT_OPERATION: &str = "store.object.operation";

// Metric attribute keys for tiered stores.
pub const STORE_TIER: &str = "store.tier";
pub const STORE_RESULT: &str = "store.result";
pub const STORE_DIRECTION: &str = "store.direction";

// Metric attribute keys for connection pools.
pub const CONNECTION_POOL: &str = "connection.pool";
pub const CONNECTION_RESULT: &str = "connection.result";

// Metric attribute keys for health checks.
pub const HEALTH_NAMESPACE: &str = "health.namespace";
pub const HEALTH_STATUS: &str = "health.status";

// Metric attribute keys for the worker fleet.
pub const WORKER_STATE: &str = "worker.state";
pub const WORKER_DISCONNECT_REASON: &str = "worker.disconnect.reason";

/// Why a worker left the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerDisconnectReason {
    /// The worker's connection ended without a drain.
    Disconnected,
    /// The worker announced a drain and its connection ended after it.
    Drained,
    /// No message within `worker_timeout_s`.
    Timeout,
    /// A kill sent to the worker was never acknowledged.
    KillUnacknowledged,
    /// A command to the worker or an update from it failed.
    Error,
    /// Removed through the admin API.
    Removed,
    /// The scheduler is shutting down.
    Shutdown,
}

impl WorkerDisconnectReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Drained => "drained",
            Self::Timeout => "timeout",
            Self::KillUnacknowledged => "kill_unacknowledged",
            Self::Error => "error",
            Self::Removed => "removed",
            Self::Shutdown => "shutdown",
        }
    }
}

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
    read_error: Vec<KeyValue>,

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
            read_error: make_attrs(CacheOperationName::Read, CacheOperationResult::Error),

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
    // Attributes shared by every combination, without stage or result.
    base: Vec<KeyValue>,
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
            base: base_attrs.to_vec(),
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
    /// The attributes shared by every combination, without stage or result.
    ///
    /// Used by metrics that are not scoped to a stage, e.g. total duration.
    #[must_use]
    pub fn base(&self) -> &[KeyValue] {
        &self.base
    }
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

/// Records a net change in a cache's contents, for `cache.size` and
/// `cache.entries`. Deltas are signed: entries leaving a cache, whether
/// evicted, replaced or deleted, pass negative values.
pub fn record_cache_entries_delta(size_delta: i64, entries_delta: i64, attrs: &[KeyValue]) {
    CACHE_METRICS.cache_size.add(size_delta, attrs);
    CACHE_METRICS.cache_entries.add(entries_delta, attrs);
}

/// Converts a size to the signed type the cache instruments take, clamping
/// rather than wrapping on a value too large to represent.
pub fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
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

        execution_cpu_time: meter
            .f64_histogram("execution_cpu_time")
            .with_description("CPU time consumed by an action in seconds")
            .with_unit("s")
            .with_boundaries(vec![
                0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0,
            ])
            .build(),

        execution_peak_memory: meter
            .u64_histogram("execution_peak_memory")
            .with_description("Peak resident memory observed while running an action in bytes")
            .with_unit("By")
            .with_boundaries(vec![
                1_048_576.0,      // 1MiB
                16_777_216.0,     // 16MiB
                67_108_864.0,     // 64MiB
                268_435_456.0,    // 256MiB
                1_073_741_824.0,  // 1GiB
                4_294_967_296.0,  // 4GiB
                17_179_869_184.0, // 16GiB
                68_719_476_736.0, // 64GiB
            ])
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

        do_try_match_duration: meter
            .f64_histogram("do_try_match_duration")
            .with_description("Duration of do_try_match in seconds")
            .with_unit("s")
            .with_boundaries(vec![
                0.01,   // 10ms
                0.1,    // 100ms
                1.0,    // 1s
                10.0,   // 10s
                60.0,   // 1 minute
                300.0,  // 5 minutes
                600.0,  // 10 minutes
                1200.0, // 20 minutes
                1800.0, // 30 minutes
                2400.0, // 40 minutes
                3000.0, // 50 minutes
                3600.0, // 1 hour
            ])
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
    /// Counter for execution retries
    pub execution_retry_count: metrics::Counter<u64>,
    /// Peak memory an action used, as sampled by the worker.
    pub execution_peak_memory: metrics::Histogram<u64>,
    /// CPU time an action used, as sampled by the worker.
    pub execution_cpu_time: metrics::Histogram<f64>,
    /// Gauge of actions by stage
    pub execution_actions_count: metrics::Gauge<u64>,
    // Gauge of queued actions by platform properties
    pub execution_queued_actions_count: metrics::ObservableGauge<u64>,
    /// Duration of `do_try_match` in ms
    pub do_try_match_duration: metrics::Histogram<f64>,
}

/// Records the CPU time a worker observed for an action.
///
/// Wall time is already covered by `execution.stage.duration`. This is the
/// other half: an action pinning eight cores for a minute and one sleeping
/// for a minute look identical by wall time and nothing alike here.
pub fn record_execution_cpu_time(
    cpu_time_ms: u64,
    instance_name: &str,
    action_mnemonic: &str,
    outcome: &str,
) {
    #[expect(clippy::cast_precision_loss)] // Milliseconds; f64 is exact well past any real action.
    let seconds = cpu_time_ms as f64 / 1000.0;
    EXECUTION_METRICS.execution_cpu_time.record(
        seconds,
        &execution_sample_attributes(instance_name, action_mnemonic, outcome),
    );
}

/// Records the peak memory a worker observed while running an action.
///
/// The worker samples this, so it is only present when sampling ran. #2614
/// listed `execution_memory_usage` as unimplementable because
/// `ExecutionMetadata` carries no memory figure; `ActionResourceUsage` does,
/// and this is that number.
pub fn record_execution_peak_memory(
    peak_memory_kb: u64,
    instance_name: &str,
    action_mnemonic: &str,
    outcome: &str,
) {
    EXECUTION_METRICS.execution_peak_memory.record(
        peak_memory_sample(peak_memory_kb),
        &execution_sample_attributes(instance_name, action_mnemonic, outcome),
    );
}

/// Attributes for a per-action resource sample. The mnemonic is attached only
/// when known: an empty value on every sample from an unattributed client
/// would read as a real mnemonic named "".
fn execution_sample_attributes(
    instance_name: &str,
    action_mnemonic: &str,
    outcome: &str,
) -> Vec<KeyValue> {
    let mut attrs = vec![KeyValue::new(EXECUTION_INSTANCE, instance_name.to_string())];
    if !action_mnemonic.is_empty() {
        attrs.push(KeyValue::new(
            EXECUTION_ACTION_MNEMONIC,
            action_mnemonic.to_string(),
        ));
    }
    if !outcome.is_empty() {
        attrs.push(KeyValue::new(EXECUTION_OUTCOME, outcome.to_string()));
    }
    attrs
}

/// Ceiling on a recorded peak-memory sample.
///
/// Well above any real action and far below the point where a histogram's
/// `u64` sum can wrap, so it filters a garbage reading without touching a
/// plausible one. Everything past the largest bucket already shares the same
/// bucket, so this changes no bucket count.
const PEAK_MEMORY_MAX_BYTES: u64 = 1 << 50; // 1 PiB.

/// Converts a worker's peak-memory reading to bytes, bounded.
///
/// The figure crosses a gRPC boundary from a worker, so a malfunctioning one
/// could report something absurd. An unbounded value overflows the
/// histogram's `u64` sum, which panics a debug build and takes the thread
/// with it. Kept separate so it is directly testable: the overflow only
/// happens once a meter provider is installed, which no unit test does.
#[must_use]
pub fn peak_memory_sample(peak_memory_kb: u64) -> u64 {
    peak_memory_kb
        .saturating_mul(1024)
        .min(PEAK_MEMORY_MAX_BYTES)
}

/// Helper function to create attributes for execution metrics
///
/// `identity` is the requesting client's `enduser.id`; it is passed through
/// [`identity_label`] to keep the label's value set bounded.
#[must_use]
pub fn make_execution_attributes(
    instance_name: &str,
    identity: &str,
    priority: Option<i32>,
) -> Vec<KeyValue> {
    let mut attrs = vec![
        KeyValue::new(EXECUTION_INSTANCE, instance_name.to_string()),
        KeyValue::new(EXECUTION_IDENTITY, identity_label(identity)),
    ];

    if let Some(priority) = priority {
        attrs.push(KeyValue::new(EXECUTION_PRIORITY, i64::from(priority)));
    }

    attrs
}

/// Records the histogram metrics derivable from a completed action's result.
pub fn record_completed_execution_metrics(
    action_result: &ActionResult,
    instance_name: &str,
    identity: &str,
    priority: Option<i32>,
) {
    let m = &*EXECUTION_METRICS;
    let md = &action_result.execution_metadata;
    let base = make_execution_attributes(instance_name, identity, priority);

    let record_secs =
        |hist: &metrics::Histogram<f64>, start: SystemTime, end: SystemTime, attrs: &[KeyValue]| {
            if start > SystemTime::UNIX_EPOCH
                && let Ok(d) = end.duration_since(start)
            {
                hist.record(d.as_secs_f64(), attrs);
            }
        };

    // Queue wait (queued -> worker picked it up) and end-to-end duration.
    record_secs(
        &m.execution_queue_time,
        md.queued_timestamp,
        md.worker_start_timestamp,
        &base,
    );
    record_secs(
        &m.execution_total_duration,
        md.queued_timestamp,
        md.worker_completed_timestamp,
        &base,
    );

    // Per-phase stage durations, labeled by phase on the stage attribute.
    for (phase, start, end) in [
        (
            "input_fetch",
            md.input_fetch_start_timestamp,
            md.input_fetch_completed_timestamp,
        ),
        (
            "execution",
            md.execution_start_timestamp,
            md.execution_completed_timestamp,
        ),
        (
            "output_upload",
            md.output_upload_start_timestamp,
            md.output_upload_completed_timestamp,
        ),
    ] {
        let mut attrs = base.clone();
        attrs.push(KeyValue::new(EXECUTION_STAGE, phase));
        record_secs(&m.execution_stage_duration, start, end, &attrs);
    }

    // Total bytes produced: output files plus stdout/stderr.
    m.execution_output_size
        .record(execution_output_bytes(action_result), &base);
}

/// Total output bytes produced by an action: the output file digests plus the
/// stdout and stderr digests.
#[must_use]
pub fn execution_output_bytes(action_result: &ActionResult) -> u64 {
    action_result
        .output_files
        .iter()
        .map(|f| f.digest.size_bytes())
        .sum::<u64>()
        + action_result.stdout_digest.size_bytes()
        + action_result.stderr_digest.size_bytes()
}

/// Input mount counters, emitted by workers rather than schedulers.
pub static INPUT_MOUNT_METRICS: LazyLock<InputMountMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());
    InputMountMetrics {
        cache_hits: meter
            .u64_counter("worker.input_mount.cache_hits")
            .with_description("Cached directories reused for input mount preparation")
            .build(),
        cache_misses: meter
            .u64_counter("worker.input_mount.cache_misses")
            .with_description("Input mount directories requiring cache construction")
            .build(),
        fallbacks: meter
            .u64_counter("worker.input_mount.fallbacks")
            .with_description(
                "Actions using ordinary input preparation despite configured input mounts",
            )
            .build(),
        prepare_failures: meter
            .u64_counter("worker.input_mount.prepare_failures")
            .with_description("Failed input mount preparations retried without mounts")
            .build(),
    }
});

#[derive(Debug)]
pub struct InputMountMetrics {
    /// Cached directories reused for input mount preparation.
    pub cache_hits: metrics::Counter<u64>,
    /// Input mount directories requiring cache construction.
    pub cache_misses: metrics::Counter<u64>,
    /// Actions using ordinary input preparation despite configured input mounts.
    pub fallbacks: metrics::Counter<u64>,
    /// Failed input mount preparations retried without mounts.
    pub prepare_failures: metrics::Counter<u64>,
}

/// Global worker fleet metrics instruments.
pub static WORKER_METRICS: LazyLock<WorkerMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    WorkerMetrics {
        worker_connected_count: meter
            .i64_up_down_counter("worker.connected.count")
            .with_description("Number of workers currently connected to this scheduler instance")
            .with_unit("{worker}")
            .build(),

        worker_connections: meter
            .u64_counter("worker.connections")
            .with_description("Total number of workers that have joined the pool")
            .with_unit("{worker}")
            .build(),

        worker_disconnections: meter
            .u64_counter("worker.disconnections")
            .with_description("Total number of workers that have left the pool, by reason")
            .with_unit("{worker}")
            .build(),

        input_relinks: meter
            .u64_counter("worker.input.relinks")
            .with_description("Input hardlinks made again because the fast store evicted their source while the tree was staged, by kind: plain, executable, or exhausted when the retries ran out")
            .build(),

        prepare_retries: meter
            .u64_counter("worker.prepare.retries")
            .with_description("Input fetches tried again on the same worker after a transient store failure: retried per attempt made again, then one of recovered, exhausted when the download deadline ran out, failed when a later attempt failed for good, or abandoned when a kill or the download timeout ended the fetch mid-retry")
            .build(),

        dispatch_requeues: meter
            .u64_counter("scheduler.dispatch.requeues")
            .with_description("Dispatches sent back to the queue untried, by reason: declined, channel_full, unacknowledged")
            .build(),

        worker_keepalives: meter
            .u64_counter("worker.keepalives")
            .with_description("Total worker keepalives received")
            .with_unit("{keepalive}")
            .build(),

        worker_keepalive_gap: meter
            .f64_histogram("worker.keepalive.gap")
            .with_description(
                "Seconds since the previous message from the same worker, at each message",
            )
            .with_unit("s")
            .with_boundaries(vec![
                1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 15.0, 20.0, 30.0, 60.0,
            ])
            .build(),

        worker_state_count: meter
            .i64_up_down_counter("worker.state.count")
            .with_description("Number of connected workers in each non-default state")
            .with_unit("{worker}")
            .build(),
    }
});

/// Worker fleet metrics.
///
/// These are per-scheduler-instance. A worker only appears in the registry of
/// the instance holding its stream, so with several schedulers behind a load
/// balancer the fleet total is the sum across instances, not any one of them.
///
/// Deliberately not attributed by worker id. Ids are per connection, so a
/// fleet that churns would grow the label set without bound, and the
/// autoscaling and fleet-health questions these answer are all aggregate.
#[derive(Debug)]
pub struct WorkerMetrics {
    /// Workers currently connected.
    pub worker_connected_count: metrics::UpDownCounter<i64>,
    /// Workers that have joined, cumulative.
    pub worker_connections: metrics::Counter<u64>,
    /// Workers that have left, cumulative, by reason.
    pub worker_disconnections: metrics::Counter<u64>,
    /// Input hardlinks made again after their source was evicted mid-staging, by kind.
    pub input_relinks: metrics::Counter<u64>,
    /// Input fetches tried again on the worker after a transient store failure: `retried` per attempt made again, then one of `recovered`, `exhausted`, `failed` or `abandoned`.
    pub prepare_retries: metrics::Counter<u64>,
    /// Dispatches requeued untried, by reason.
    pub dispatch_requeues: metrics::Counter<u64>,
    /// Keepalives received, cumulative.
    pub worker_keepalives: metrics::Counter<u64>,
    /// Seconds between consecutive messages from a worker.
    pub worker_keepalive_gap: metrics::Histogram<f64>,
    /// Connected workers currently paused or draining.
    pub worker_state_count: metrics::UpDownCounter<i64>,
}

/// Records a worker joining the pool.
pub fn record_worker_connected() {
    WORKER_METRICS.worker_connected_count.add(1, &[]);
    WORKER_METRICS.worker_connections.add(1, &[]);
}

/// Records a worker leaving the pool.
///
/// `was_draining` and `was_paused` unwind the state gauges, which would
/// otherwise keep counting a worker that is already gone.
pub fn record_worker_disconnected(
    reason: WorkerDisconnectReason,
    was_draining: bool,
    was_paused: bool,
) {
    WORKER_METRICS.worker_connected_count.add(-1, &[]);
    WORKER_METRICS.worker_disconnections.add(
        1,
        &[KeyValue::new(WORKER_DISCONNECT_REASON, reason.as_str())],
    );
    if was_draining {
        record_worker_state("draining", false);
    }
    if was_paused {
        record_worker_state("paused", false);
    }
}

/// Records a worker entering or leaving `state`.
pub fn record_worker_state(state: &'static str, entered: bool) {
    WORKER_METRICS.worker_state_count.add(
        if entered { 1 } else { -1 },
        &[KeyValue::new(WORKER_STATE, state)],
    );
}

/// Records a dispatch going back to the queue untried: the worker declined
/// it, its channel was full, or it never acknowledged it.
pub fn record_dispatch_requeue(reason: &'static str) {
    WORKER_METRICS
        .dispatch_requeues
        .add(1, &[KeyValue::new("reason", reason)]);
}

/// Records an input hardlink made again because the fast store evicted its
/// source between resolving the tree and linking it, or `exhausted` when the
/// fetch retries ran out. A steady rate says the store runs at its ceiling.
pub fn record_input_relink(kind: &'static str) {
    WORKER_METRICS
        .input_relinks
        .add(1, &[KeyValue::new(WORKER_RELINK_KIND, kind)]);
}

/// Records an input fetch the worker tried again after a transient store
/// failure: `retried` per attempt made again, then one terminal outcome,
/// `recovered`, `exhausted` (the download deadline ran out), `failed` (a
/// later attempt failed for good) or `abandoned` (a kill or the download
/// timeout ended it mid-retry).
pub fn record_prepare_retry(outcome: &'static str) {
    WORKER_METRICS
        .prepare_retries
        .add(1, &[KeyValue::new(WORKER_PREPARE_OUTCOME, outcome)]);
}

/// Records a keepalive from a worker.
pub fn record_worker_keepalive() {
    WORKER_METRICS.worker_keepalives.add(1, &[]);
}

/// Records the gap since the worker's previous message. The distribution
/// is what the eviction margin is judged against.
pub fn record_worker_keepalive_gap(seconds: u64) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "seconds; exact well past any real gap"
    )]
    WORKER_METRICS
        .worker_keepalive_gap
        .record(seconds as f64, &[]);
}

/// Global gRPC serving metrics.
///
/// One duration histogram covers rate, errors and latency: the count gives
/// rate, the status attribute separates errors, and the buckets give latency.
pub static RPC_METRICS: LazyLock<RpcMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    RpcMetrics {
        rpc_server_duration: meter
            .f64_histogram("rpc.server.duration")
            .with_description("Duration of inbound gRPC calls in seconds")
            .with_unit("s")
            .with_boundaries(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
                300.0,
            ])
            .build(),
    }
});

/// gRPC serving metrics.
#[derive(Debug)]
pub struct RpcMetrics {
    /// Duration of inbound gRPC calls, by service, method and status.
    pub rpc_server_duration: metrics::Histogram<f64>,
}

/// Records a served gRPC call.
///
/// `full_path` is the HTTP path tonic routes on, `/package.Service/Method`.
pub fn record_rpc_served(full_path: &str, grpc_status: i32, duration_secs: f64) {
    let (service, method) = split_grpc_path(full_path);
    RPC_METRICS.rpc_server_duration.record(
        duration_secs,
        &[
            KeyValue::new(RPC_SERVICE, service.to_string()),
            KeyValue::new(RPC_METHOD, method.to_string()),
            KeyValue::new(RPC_STATUS_CODE, i64::from(grpc_status)),
        ],
    );
}

/// Splits `/package.Service/Method` into its service and method halves.
///
/// Anything that does not look like a gRPC path is reported whole under an
/// `unknown` method, so an unexpected route still shows up rather than
/// silently vanishing. Both halves come from the route table, not user input,
/// so the label set stays bounded.
#[must_use]
pub fn split_grpc_path(full_path: &str) -> (&str, &str) {
    let trimmed = full_path.strip_prefix('/').unwrap_or(full_path);
    trimmed
        .split_once('/')
        .map_or((trimmed, "unknown"), |(service, method)| (service, method))
}

/// Global scheduler metrics.
pub static SCHEDULER_METRICS: LazyLock<SchedulerOtlpMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    SchedulerOtlpMetrics {
        matching_duration: meter
            .f64_histogram("scheduler.matching.duration")
            .with_description("Duration of one queued-action to worker matching pass in seconds")
            .with_unit("s")
            .with_boundaries(vec![
                0.0001, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ])
            .build(),

        matching_passes: meter
            .u64_counter("scheduler.matching.passes")
            .with_description("Matching passes run, by result")
            .with_unit("{pass}")
            .build(),

        unsatisfiable_queued: meter
            .u64_gauge("scheduler.unsatisfiable.queued")
            .with_description(
                "Queued actions that no worker connected to this scheduler could run even when idle",
            )
            .with_unit("{action}")
            .build(),

        unsatisfiable_failed: meter
            .u64_counter("scheduler.unsatisfiable.failed")
            .with_description("Queued actions failed because no worker could ever run them")
            .with_unit("{action}")
            .build(),

        queue_depth: meter
            .u64_gauge("scheduler.queue.depth")
            .with_description("Queued actions the last matching pass read")
            .with_unit("{action}")
            .build(),

        queue_age: meter
            .u64_gauge("scheduler.queue.age")
            .with_description(
                "Queued actions the last matching pass read, by how long ago their client submitted them",
            )
            .with_unit("{action}")
            .build(),

        queue_oldest_age: meter
            .f64_gauge("scheduler.queue.oldest_age")
            .with_description(
                "Seconds since the client submitted the oldest action the last matching pass read",
            )
            .with_unit("s")
            .build(),

        executing_age: meter
            .u64_gauge("scheduler.executing.age")
            .with_description(
                "Executing actions the last executing sweep read, by how long they have been executing",
            )
            .with_unit("{action}")
            .build(),

        executing_oldest_age: meter
            .f64_gauge("scheduler.executing.oldest_age")
            .with_description(
                "Seconds the longest-executing action the last executing sweep read has been executing",
            )
            .with_unit("s")
            .build(),

        queue_retired: meter
            .u64_counter("scheduler.queue.retired")
            .with_description("Queued actions retired because no client was listening any more")
            .with_unit("{action}")
            .build(),

        parked_dispatched: meter
            .u64_counter("scheduler.matching.parked_dispatched")
            .with_description(
                "Queued actions dispatched onto room that opened while the matching pass was past them, taken ahead of the newer actions the pass was reading",
            )
            .with_unit("{action}")
            .build(),

        worker_holds: meter
            .u64_counter("scheduler.worker.holds")
            .with_description(
                "Workers held for a queued action that kept finding no room, by outcome: held when the hold began, placed when a worker took the action, released when the action or the worker left, declined when the held worker declined the action idle, expired when max_hold_s passed first",
            )
            .with_unit("{hold}")
            .build(),

        held_workers: meter
            .u64_gauge("scheduler.worker.held")
            .with_description("Workers this scheduler holds for a queued action right now")
            .with_unit("{worker}")
            .build(),

        awaited_action_orphans: meter
            .u64_counter("scheduler.awaited_action.orphans")
            .with_description(
                "Operations the queue listed whose record was gone on read, by site; non-zero means the scheduler's store is losing records, usually to eviction",
            )
            .with_unit("{operation}")
            .build(),

        sweep_failures: meter
            .u64_counter("scheduler.sweep.failures")
            .with_description("Abandoned-queue sweeps that ended in an error before finishing")
            .with_unit("{sweep}")
            .build(),

        hint_resolutions: meter
            .u64_counter("scheduler.hints.resolutions")
            .with_description(
                "Actions the historical resource scheduler sized, by where the numbers came from: a hint keyed by target, action digest, command digest or mnemonic, the cold start, or nothing",
            )
            .with_unit("{action}")
            .build(),
    }
});

/// Scheduler matching metrics.
///
/// Named to avoid colliding with the existing `#[metric]` component struct of
/// the same idea in the scheduler crate.
#[derive(Debug)]
pub struct SchedulerOtlpMetrics {
    /// How long a matching pass takes. The saturation signal: this climbing
    /// while the queue is non-empty means matching is the bottleneck.
    pub matching_duration: metrics::Histogram<f64>,
    /// Matching passes, by result.
    pub matching_passes: metrics::Counter<u64>,
    /// Queued actions seen in the last matching pass that no worker
    /// connected to this scheduler could run even when idle. Per scheduler
    /// instance, judged against that instance's own workers.
    pub unsatisfiable_queued: metrics::Gauge<u64>,
    /// Queued actions failed because no worker could ever run them, by
    /// the properties that could not be satisfied.
    pub unsatisfiable_failed: metrics::Counter<u64>,
    /// Queued actions the last matching pass read.
    pub queue_depth: metrics::Gauge<u64>,
    /// Queued actions the last matching pass read, by age bucket.
    pub queue_age: metrics::Gauge<u64>,
    /// Age of the oldest action the last matching pass read, in seconds.
    pub queue_oldest_age: metrics::Gauge<f64>,
    /// Executing actions the last executing sweep read, by age bucket.
    pub executing_age: metrics::Gauge<u64>,
    /// How long the longest-executing action the last sweep read has been
    /// executing, in seconds.
    pub executing_oldest_age: metrics::Gauge<f64>,
    /// Queued actions retired for having no client.
    pub queue_retired: metrics::Counter<u64>,
    /// Actions a pass parked for want of room and dispatched when room
    /// opened before the pass ended, ahead of the newer actions it was reading.
    pub parked_dispatched: metrics::Counter<u64>,
    /// Workers held for a queued action that kept finding no room, by what
    /// began or ended the hold.
    pub worker_holds: metrics::Counter<u64>,
    /// Workers held for such an action right now.
    pub held_workers: metrics::Gauge<u64>,
    /// Listed operations whose record was gone on read, by site.
    pub awaited_action_orphans: metrics::Counter<u64>,
    /// Abandoned-queue sweeps that failed before finishing.
    pub sweep_failures: metrics::Counter<u64>,
    /// Actions sized by the historical resource scheduler, by source.
    pub hint_resolutions: metrics::Counter<u64>,
}

/// Records a completed matching pass.
pub fn record_matching_pass(duration_secs: f64, succeeded: bool) {
    SCHEDULER_METRICS
        .matching_duration
        .record(duration_secs, &[]);
    SCHEDULER_METRICS.matching_passes.add(
        1,
        &[KeyValue::new(
            SCHEDULER_MATCH_RESULT,
            if succeeded { "ok" } else { "error" },
        )],
    );
}

/// Records how many unsatisfiable queued actions a matching pass saw.
pub fn record_unsatisfiable_queued(count: u64) {
    SCHEDULER_METRICS.unsatisfiable_queued.record(count, &[]);
}

/// Records how many queued actions a matching pass read.
pub fn record_queue_depth(count: u64) {
    SCHEDULER_METRICS.queue_depth.record(count, &[]);
}

/// Upper bounds of the action age buckets; anything older falls in the last,
/// open bucket. Matches how an operator asks about a backlog: how much has
/// waited over a minute, five, fifteen, thirty, an hour.
const AGE_BUCKET_BOUNDS: [core::time::Duration; 5] = [
    core::time::Duration::from_mins(1),
    core::time::Duration::from_mins(5),
    core::time::Duration::from_mins(15),
    core::time::Duration::from_mins(30),
    core::time::Duration::from_hours(1),
];

/// The `age` attribute values, one per bucket, in bound order.
pub const AGE_BUCKET_LABELS: [&str; 6] =
    ["lt_1m", "1m_5m", "5m_15m", "15m_30m", "30m_60m", "ge_60m"];

/// Counts actions into age buckets as a pass reads them, for one gauge
/// sample per bucket at the end of the pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AgeBuckets {
    counts: [u64; AGE_BUCKET_LABELS.len()],
    oldest: core::time::Duration,
}

impl AgeBuckets {
    /// Counts one action of the given age.
    pub fn observe(&mut self, age: core::time::Duration) {
        let bucket = AGE_BUCKET_BOUNDS
            .iter()
            .position(|bound| age < *bound)
            .unwrap_or(AGE_BUCKET_BOUNDS.len());
        self.counts[bucket] += 1;
        self.oldest = self.oldest.max(age);
    }

    /// Counts one action from the time its age is measured from. A time
    /// after `now` (clock skew between replicas) counts as no age at all.
    pub fn observe_since(&mut self, since: SystemTime, now: SystemTime) {
        self.observe(now.duration_since(since).unwrap_or_default());
    }

    /// The count in each bucket, in [`AGE_BUCKET_LABELS`] order.
    #[must_use]
    pub const fn counts(&self) -> [u64; AGE_BUCKET_LABELS.len()] {
        self.counts
    }

    /// The oldest age counted, zero when nothing was.
    #[must_use]
    pub const fn oldest(&self) -> core::time::Duration {
        self.oldest
    }
}

/// Records the ages of the queued actions a matching pass read. Every bucket
/// is written, empty ones as zero, so a gauge never holds a count from a
/// backlog that has since drained.
pub fn record_queue_ages(ages: &AgeBuckets) {
    record_age_buckets(
        &SCHEDULER_METRICS.queue_age,
        &SCHEDULER_METRICS.queue_oldest_age,
        ages,
    );
}

/// Records the ages of the executing actions an executing sweep read, the
/// same way as [`record_queue_ages`].
pub fn record_executing_ages(ages: &AgeBuckets) {
    record_age_buckets(
        &SCHEDULER_METRICS.executing_age,
        &SCHEDULER_METRICS.executing_oldest_age,
        ages,
    );
}

fn record_age_buckets(
    by_bucket: &metrics::Gauge<u64>,
    oldest: &metrics::Gauge<f64>,
    ages: &AgeBuckets,
) {
    for (label, count) in AGE_BUCKET_LABELS.iter().zip(ages.counts()) {
        by_bucket.record(count, &[KeyValue::new(SCHEDULER_ACTION_AGE, *label)]);
    }
    oldest.record(ages.oldest().as_secs_f64(), &[]);
}

/// Records queued actions retired by the abandoned sweep.
pub fn record_queue_retired(count: u64) {
    if count > 0 {
        SCHEDULER_METRICS.queue_retired.add(count, &[]);
    }
}

/// Records parked actions a pass sent to a worker onto room that opened
/// mid-pass: confirmed sends, not attempts. A dispatch that failed or lost
/// the assignment race to another scheduler is not counted.
pub fn record_parked_dispatched(count: u64) {
    if count > 0 {
        SCHEDULER_METRICS.parked_dispatched.add(count, &[]);
    }
}

/// Records a worker hold beginning (`held`) or ending: `placed` when a
/// worker took the action, `released` when the action left the queue or
/// the worker left or drained, `declined` when the held worker declined
/// the action idle and another is held instead, `expired` when `max_hold_s`
/// passed first.
pub fn record_worker_hold(outcome: &'static str) {
    SCHEDULER_METRICS
        .worker_holds
        .add(1, &[KeyValue::new(SCHEDULER_HOLD_OUTCOME, outcome)]);
}

/// Records how many workers are held right now.
pub fn record_held_workers(count: usize) {
    SCHEDULER_METRICS
        .held_workers
        .record(u64::try_from(count).unwrap_or(u64::MAX), &[]);
}

/// Records an operation the queue listed whose record was gone when read;
/// `site` is where it was met (`matching`, `sweep`).
pub fn record_awaited_action_orphan(site: &'static str) {
    SCHEDULER_METRICS
        .awaited_action_orphans
        .add(1, &[KeyValue::new("site", site)]);
}

/// Records an abandoned-queue sweep that failed before finishing.
pub fn record_sweep_failure() {
    SCHEDULER_METRICS.sweep_failures.add(1, &[]);
}

/// Records where an action's reservation came from: `target`,
/// `action_digest`, `command_digest`, `mnemonic`, `cold_start` or `none`.
/// The cold-start share of a run is `cold_start` over the total.
pub fn record_hint_resolution(source: &'static str) {
    SCHEDULER_METRICS
        .hint_resolutions
        .add(1, &[KeyValue::new("source", source)]);
}

/// Records a queued action failed for being unsatisfiable. `properties`
/// names the ones no worker could satisfy, sorted and comma separated, so
/// the label set is bounded by the declared platform properties.
pub fn record_unsatisfiable_failed(properties: String) {
    SCHEDULER_METRICS.unsatisfiable_failed.add(
        1,
        &[KeyValue::new(
            SCHEDULER_UNSATISFIABLE_PROPERTIES,
            properties,
        )],
    );
}

/// Global object-store metrics.
pub static STORE_METRICS: LazyLock<StoreMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    StoreMetrics {
        object_throttles: meter
            .u64_counter("store.object.throttles")
            .with_description("Object store responses that asked the client to slow down (HTTP 503 ServerBusy or 429), by provider and operation")
            .with_unit("{response}")
            .build(),

        object_read_limit: meter
            .u64_gauge("store.object.read_limit")
            .with_description("Reads this process keeps in flight against an object store at most, as the adaptive permit pool stands: halved on a throttled response, raised by one after a run of successes")
            .with_unit("{request}")
            .build(),

        object_read_waits: meter
            .u64_counter("store.object.read_waits")
            .with_description("Object store reads that found every read permit taken and waited for one, by provider")
            .with_unit("{request}")
            .build(),
    }
});

/// Object-store metrics shared by the cloud backends.
#[derive(Debug)]
pub struct StoreMetrics {
    /// Throttled responses, by provider and operation.
    pub object_throttles: metrics::Counter<u64>,
    /// The adaptive read limit as it stands, by provider.
    pub object_read_limit: metrics::Gauge<u64>,
    /// Reads that waited for a read permit, by provider.
    pub object_read_waits: metrics::Counter<u64>,
}

/// Records the read limit an object store's permit pool settled on.
pub fn record_object_store_read_limit(provider: &'static str, limit: usize) {
    STORE_METRICS.object_read_limit.record(
        u64::try_from(limit).unwrap_or(u64::MAX),
        &[KeyValue::new(STORE_OBJECT_PROVIDER, provider)],
    );
}

/// Records a read that found no permit free and waited for one.
pub fn record_object_store_read_wait(provider: &'static str) {
    STORE_METRICS
        .object_read_waits
        .add(1, &[KeyValue::new(STORE_OBJECT_PROVIDER, provider)]);
}

/// Records a response from an object store that asked the client to slow down.
pub fn record_object_store_throttle(provider: &'static str, op: &'static str) {
    STORE_METRICS.object_throttles.add(
        1,
        &[
            KeyValue::new(STORE_OBJECT_PROVIDER, provider),
            KeyValue::new(STORE_OBJECT_OPERATION, op),
        ],
    );
}

/// Global tiered-store metrics.
pub static STORE_TIER_METRICS: LazyLock<StoreTierMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    StoreTierMetrics {
        tier_operations: meter
            .u64_counter("store.tier.operations")
            .with_description("Reads served by each tier of a tiered store, by result")
            .with_unit("{operation}")
            .build(),

        tier_io: meter
            .u64_counter("store.tier.io")
            .with_description("Bytes moved through each tier of a tiered store")
            .with_unit("By")
            .build(),
    }
});

/// Tiered-store metrics. The hit ratio is `tier_operations{result="hit"}` over
/// the sum, which is the number worth alerting on for a fast/slow store.
#[derive(Debug)]
pub struct StoreTierMetrics {
    /// Reads served per tier, by result.
    pub tier_operations: metrics::Counter<u64>,
    /// Bytes moved per tier and direction.
    pub tier_io: metrics::Counter<u64>,
}

/// Records a read served, or not served, by a tier.
pub fn record_store_tier_read(tier: &'static str, result: &'static str) {
    STORE_TIER_METRICS.tier_operations.add(
        1,
        &[
            KeyValue::new(STORE_TIER, tier),
            KeyValue::new(STORE_RESULT, result),
        ],
    );
}

/// Records bytes moved through a tier.
pub fn record_store_tier_io(tier: &'static str, direction: &'static str, bytes: u64) {
    STORE_TIER_METRICS.tier_io.add(
        bytes,
        &[
            KeyValue::new(STORE_TIER, tier),
            KeyValue::new(STORE_DIRECTION, direction),
        ],
    );
}

/// Global health-check metrics.
pub static HEALTH_METRICS: LazyLock<HealthMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    HealthMetrics {
        health_checks: meter
            .u64_counter("health.checks")
            .with_description("Health check results, by component and status")
            .with_unit("{check}")
            .build(),
    }
});

/// Health-check metrics.
#[derive(Debug)]
pub struct HealthMetrics {
    /// Health check results, by namespace and status.
    pub health_checks: metrics::Counter<u64>,
}

/// Records one component's health check result.
pub fn record_health_check(namespace: &str, status: &'static str) {
    HEALTH_METRICS.health_checks.add(
        1,
        &[
            KeyValue::new(HEALTH_NAMESPACE, namespace.to_string()),
            KeyValue::new(HEALTH_STATUS, status),
        ],
    );
}

/// Global connection-pool metrics.
pub static CONNECTION_METRICS: LazyLock<ConnectionMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    ConnectionMetrics {
        pool_available: meter
            .u64_histogram("connection.pool.available")
            .with_description("Free slots in a connection pool, sampled when one is taken")
            .with_unit("{connection}")
            .with_boundaries(vec![
                0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0,
            ])
            .build(),

        pool_acquisitions: meter
            .u64_counter("connection.pool.acquisitions")
            .with_description("Connection acquisitions, by pool and whether one was free")
            .with_unit("{acquisition}")
            .build(),

        reconnects: meter
            .u64_counter("connection.reconnects")
            .with_description("Reconnects performed, by pool")
            .with_unit("{reconnect}")
            .build(),
    }
});

/// Connection-pool metrics.
///
/// `pool_available` is the saturation signal: a pool that keeps reporting zero
/// free slots is the bottleneck, whatever the latency elsewhere says. Counting
/// queued acquisitions gives the same answer from the other direction.
#[derive(Debug)]
pub struct ConnectionMetrics {
    /// Free slots at the moment a connection was taken.
    pub pool_available: metrics::Histogram<u64>,
    /// Acquisitions, by pool and result.
    pub pool_acquisitions: metrics::Counter<u64>,
    /// Reconnects, by pool.
    pub reconnects: metrics::Counter<u64>,
}

/// Ceiling on a recorded free-slot count.
///
/// An unbounded pool is a semaphore holding `Semaphore::MAX_PERMITS`, around
/// 2.3e18. Recording that overflows the histogram's `u64` sum after a handful
/// of samples, which panics a debug build. Callers pass `None` for an
/// unbounded pool, and this clamp is the backstop if one ever does not: it
/// sits above the largest bucket boundary, so bucket counts are unchanged.
const POOL_AVAILABLE_MAX: u64 = 1024;

/// Records a connection being taken from `pool`.
///
/// `available` is the free-slot count, or `None` when the pool is unbounded
/// and the figure would be meaningless. `queued` means nothing was free and
/// the caller had to wait, which is the case worth alerting on.
/// Clamps a free-slot count to something a histogram can accumulate.
///
/// Kept separate so it is directly testable: the failure this guards against
/// only appears once a meter provider is installed, so a test that merely
/// calls `record_connection_acquired` cannot catch it.
#[must_use]
pub fn pool_available_sample(available: usize) -> u64 {
    u64::try_from(available)
        .unwrap_or(POOL_AVAILABLE_MAX)
        .min(POOL_AVAILABLE_MAX)
}

pub fn record_connection_acquired(pool: &'static str, available: Option<usize>, queued: bool) {
    if let Some(available) = available {
        CONNECTION_METRICS.pool_available.record(
            pool_available_sample(available),
            &[KeyValue::new(CONNECTION_POOL, pool)],
        );
    }
    CONNECTION_METRICS.pool_acquisitions.add(
        1,
        &[
            KeyValue::new(CONNECTION_POOL, pool),
            KeyValue::new(
                CONNECTION_RESULT,
                if queued { "queued" } else { "immediate" },
            ),
        ],
    );
}

/// Records a reconnect. A rising count means the backend is flapping, which
/// on Redis usually means a failover.
pub fn record_connection_reconnect(pool: &'static str) {
    CONNECTION_METRICS
        .reconnects
        .add(1, &[KeyValue::new(CONNECTION_POOL, pool)]);
}

// Metric attribute keys for retried operations.
pub const RETRY_CLIENT: &str = "retry.client";
pub const RETRY_OUTCOME: &str = "retry.outcome";

/// Global metrics for operations run under a [`crate::retry::Retrier`].
pub static RETRY_METRICS: LazyLock<RetryMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    RetryMetrics {
        outcomes: meter
            .u64_counter("retry.outcomes")
            .with_description(
                "Retried operations by client and outcome: each retry (retried), a success after at least one retry (recovered), retries run out (exhausted), or an error that is never retried, NotFound aside (permanent)",
            )
            .with_unit("{event}")
            .build(),
    }
});

/// Metrics for operations run under a [`crate::retry::Retrier`].
#[derive(Debug)]
pub struct RetryMetrics {
    /// Retry events, by client and outcome.
    pub outcomes: metrics::Counter<u64>,
}

/// Records one retry event. `client` names what is retrying (`azure`, `s3`,
/// `grpc_store`, ...), `outcome` is one of `retried`, `recovered`,
/// `exhausted` or `permanent`. An `exhausted` count rising is a backend that
/// stayed down, or throttled, for longer than the retry policy waits.
pub fn record_retry_outcome(client: &'static str, outcome: &'static str) {
    RETRY_METRICS.outcomes.add(
        1,
        &[
            KeyValue::new(RETRY_CLIENT, client),
            KeyValue::new(RETRY_OUTCOME, outcome),
        ],
    );
}

// Metric attribute keys for worker pool operations.
pub const WORKER_POOL_INSTANCE: &str = "worker_pool_instance";
pub const WORKER_EVENT_TYPE: &str = "worker_pool_event_type";
pub const WORKER_POOL_STATE: &str = "worker_pool_state";

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
            attrs.push(KeyValue::new(WORKER_POOL_STATE, state));
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

        actions_declined: meter
            .u64_counter("worker_actions_declined")
            .with_description(
                "Dispatches this worker declined: at capacity, short of memory, or shutting down",
            )
            .with_unit("{action}")
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
    /// Counter for dispatches this worker declined
    pub actions_declined: metrics::Counter<u64>,
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
    Azure,
    Filesystem,
    S3,
    Gcs,
    Grpc,
    Mongo,
    Redis,
    Oci,
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
    R2,
    Ref,
    Shard,
    Metrics,
}

impl Display for StoreType {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            StoreType::Azure => write!(f, "azure"),
            StoreType::Filesystem => write!(f, "filesystem"),
            StoreType::S3 => write!(f, "s3"),
            StoreType::Grpc => write!(f, "grpc"),
            StoreType::Mongo => write!(f, "mongo"),
            StoreType::Redis => write!(f, "redis"),
            StoreType::Gcs => write!(f, "gcs"),
            StoreType::Oci => write!(f, "oci"),
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
            StoreType::R2 => write!(f, "r2"),
            StoreType::Ref => write!(f, "ref"),
            StoreType::Shard => write!(f, "shard"),
            StoreType::Metrics => write!(f, "metrics"),
        }
    }
}

pub static STORE_OPERATION_METRICS: LazyLock<StoreOperationMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    StoreOperationMetrics {
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
                0.1, // 100μs
                // Sub-millisecond range
                0.5, // 500μs
                1.0, // 1ms
                // Low millisecond range
                5.0,   // 5ms
                10.0,  // 10ms
                50.0,  // 50ms
                100.0, // 100ms
                // Higher latency range
                500.0,   // 500ms
                1000.0,  // 1 second
                5000.0,  // 5 seconds
                10000.0, // 10 seconds
            ])
            .build(),

        eviction_count: meter
            .u64_counter("eviction_count")
            .with_description("Number of evictions")
            .build(),

        store_size: meter
            .u64_gauge("store_size")
            .with_description("Number of items in the store")
            .build(),
    }
});

#[derive(Debug)]
pub struct StoreOperationMetrics {
    /// Histogram of store operation durations in milliseconds
    pub store_operation_duration: metrics::Histogram<f64>,
    /// Counter of store operations by type and result
    pub store_operations: metrics::Counter<u64>,
    /// Counter of evictions
    pub eviction_count: metrics::Counter<u64>,
    /// Counter of items in the store
    pub store_size: metrics::Gauge<u64>,
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
    store_size: Vec<KeyValue>,
}

impl StoreMetricAttrs {
    /// Creates a new set of pre-computed attributes.
    ///
    /// The `base_attrs` are included in all attribute combinations (e.g., store
    /// type, instance ID).
    #[must_use]
    pub fn new_with_name(store_type: StoreType, name: &str) -> Self {
        let base_attrs = vec![
            KeyValue::new(STORE_TYPE, store_type.to_string()),
            KeyValue::new(STORE_NAME, name.to_string()),
        ];
        let make_attrs = |op: CacheOperationName, result: CacheOperationResult| {
            let mut attrs = base_attrs.clone();
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
            store_size: base_attrs.clone(),
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
    #[must_use]
    pub fn store_size(&self) -> &[KeyValue] {
        &self.store_size
    }
}

// Metric attribute keys for the GCS store.
pub const GCS_BUCKET: &str = "gcs.bucket";
pub const GCS_CUSTOM_TIME_RESULT: &str = "gcs.custom_time.result";

/// Outcome of one `customTime` refresh decision in the GCS store.
///
/// The GCS store stamps `customTime` on an object so that a
/// `daysSinceCustomTime` lifecycle rule can evict data that no build has used
/// recently. Every decision produces exactly one of these outcomes, so the four
/// values partition the total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomTimeResult {
    /// The `customTime` write reached GCS.
    Written,
    /// The `customTime` write returned an error.
    Failed,
    /// No write was needed. Another process had already refreshed the object,
    /// or this process had refreshed it inside the interval.
    SkippedFresh,
    /// The write was abandoned because the concurrency budget was full.
    Dropped,
}

impl From<CustomTimeResult> for Value {
    fn from(result: CustomTimeResult) -> Self {
        match result {
            CustomTimeResult::Written => Self::from("written"),
            CustomTimeResult::Failed => Self::from("failed"),
            CustomTimeResult::SkippedFresh => Self::from("skipped_fresh"),
            CustomTimeResult::Dropped => Self::from("dropped"),
        }
    }
}

/// Pre-allocated attribute combinations for GCS `customTime` metrics.
#[derive(Debug)]
pub struct GcsCustomTimeAttrs {
    written: Vec<KeyValue>,
    failed: Vec<KeyValue>,
    skipped_fresh: Vec<KeyValue>,
    dropped: Vec<KeyValue>,
}

impl GcsCustomTimeAttrs {
    /// Creates the attribute sets for one bucket.
    ///
    /// A process serves a small fixed number of buckets, so the bucket name is
    /// a bounded label.
    #[must_use]
    pub fn new(bucket: &str) -> Self {
        let make_attrs = |result: CustomTimeResult| {
            vec![
                KeyValue::new(GCS_BUCKET, bucket.to_string()),
                KeyValue::new(GCS_CUSTOM_TIME_RESULT, result),
            ]
        };

        Self {
            written: make_attrs(CustomTimeResult::Written),
            failed: make_attrs(CustomTimeResult::Failed),
            skipped_fresh: make_attrs(CustomTimeResult::SkippedFresh),
            dropped: make_attrs(CustomTimeResult::Dropped),
        }
    }

    #[must_use]
    pub fn written(&self) -> &[KeyValue] {
        &self.written
    }
    #[must_use]
    pub fn failed(&self) -> &[KeyValue] {
        &self.failed
    }
    #[must_use]
    pub fn skipped_fresh(&self) -> &[KeyValue] {
        &self.skipped_fresh
    }
    #[must_use]
    pub fn dropped(&self) -> &[KeyValue] {
        &self.dropped
    }
}

/// Global GCS store metrics instruments.
pub static GCS_METRICS: LazyLock<GcsMetrics> = LazyLock::new(|| {
    let meter = global::meter_with_scope(InstrumentationScope::builder("nativelink").build());

    GcsMetrics {
        custom_time_operations: meter
            .u64_counter("gcs_custom_time_operations")
            .with_description("Total customTime refresh decisions by outcome")
            .with_unit("{operation}")
            .build(),

        custom_time_duration: meter
            .f64_histogram("gcs_custom_time_duration")
            .with_description("Duration of customTime metadata writes in milliseconds")
            .with_unit("ms")
            // GCS serializes writes to one object at about one per second, so
            // a contended object lands in the seconds range. The upper
            // boundaries must cover the client timeout to make it visible.
            .with_boundaries(vec![
                1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2000.0, 3000.0, 5000.0,
                10000.0,
            ])
            .build(),
    }
});

/// `OpenTelemetry` metrics instruments for the GCS store.
#[derive(Debug)]
pub struct GcsMetrics {
    /// Counter of `customTime` refresh decisions by outcome
    pub custom_time_operations: metrics::Counter<u64>,
    /// Histogram of `customTime` write durations in milliseconds
    pub custom_time_duration: metrics::Histogram<f64>,
}
