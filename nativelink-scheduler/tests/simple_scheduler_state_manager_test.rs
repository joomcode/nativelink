use core::time::Duration;
use std::sync::Arc;
use std::time::SystemTime;

use nativelink_error::Error;
use nativelink_macro::nativelink_test;
use nativelink_scheduler::default_scheduler_factory::memory_awaited_action_db_factory;
use nativelink_scheduler::simple_scheduler_state_manager::{
    SchedulerMetrics, SimpleSchedulerStateManager,
};
use nativelink_util::action_messages::{OperationId, WorkerId};
use nativelink_util::instant_wrapper::MockInstantWrapped;
use nativelink_util::metrics::{
    EXECUTION_IDENTITY, EXECUTION_IDENTITY_OTHER, EXECUTION_IDENTITY_UNKNOWN, EXECUTION_INSTANCE,
    set_identity_allowlist,
};
use nativelink_util::operation_state_manager::{UpdateOperationType, WorkerStateManager};
use opentelemetry::KeyValue;
use tokio::sync::Notify;

#[nativelink_test]
async fn drops_missing_actions() -> Result<(), Error> {
    let task_change_notify = Arc::new(Notify::new());
    let awaited_action_db = memory_awaited_action_db_factory(
        0,
        &task_change_notify.clone(),
        MockInstantWrapped::default,
    );
    let state_manager = SimpleSchedulerStateManager::new(
        0,
        Duration::from_secs(10),
        Duration::from_secs(10),
        Duration::ZERO,
        awaited_action_db,
        SystemTime::now,
        None,
        "test_scheduler",
    );
    state_manager
        .update_operation(
            &OperationId::Uuid(uuid::Uuid::parse_str(
                "c458c1f4-136e-486d-b9cd-cea07460cde4",
            )?),
            &WorkerId::default(),
            UpdateOperationType::ExecutionComplete,
        )
        .await
        .unwrap();

    assert!(logs_contain(
        "Unable to update action due to it being missing, probably dropped operation_id=c458c1f4-136e-486d-b9cd-cea07460cde4"
    ));
    Ok(())
}

#[test]
fn attributes_execution_metrics_to_identity() {
    fn value_of(attrs: &[KeyValue], key: &str) -> Option<String> {
        attrs
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    }

    // Only allowlisted identities are reported, so declare the ones asserted on
    // below. Another test in this binary may have installed them already.
    let _ = set_identity_allowlist(["ci".to_string(), "local".to_string()]);

    let metrics = SchedulerMetrics::new("test_scheduler");

    let ci = metrics.attrs_for_identity("ci");
    let local = metrics.attrs_for_identity("local");
    let absent = metrics.attrs_for_identity("");
    let unlisted = metrics.attrs_for_identity("dev@example.com");

    // Each identity gets its own attribute set, on every stage/result combination.
    assert_eq!(
        value_of(ci.queued(), EXECUTION_IDENTITY).as_deref(),
        Some("ci")
    );
    assert_eq!(
        value_of(local.queued(), EXECUTION_IDENTITY).as_deref(),
        Some("local")
    );
    assert_eq!(
        value_of(ci.completed_success(), EXECUTION_IDENTITY).as_deref(),
        Some("ci")
    );
    assert_eq!(
        value_of(ci.completed_timeout(), EXECUTION_IDENTITY).as_deref(),
        Some("ci")
    );
    // Non-stage-scoped metrics (e.g. total duration) carry the identity too.
    assert_eq!(
        value_of(ci.base(), EXECUTION_IDENTITY).as_deref(),
        Some("ci")
    );

    // A request without `enduser.id` is labelled explicitly, not left blank, and
    // an identity outside the allowlist is bucketed rather than reported.
    assert_eq!(
        value_of(absent.queued(), EXECUTION_IDENTITY).as_deref(),
        Some(EXECUTION_IDENTITY_UNKNOWN)
    );
    assert_eq!(
        value_of(unlisted.queued(), EXECUTION_IDENTITY).as_deref(),
        Some(EXECUTION_IDENTITY_OTHER)
    );

    // The instance name is still reported alongside the new attribute.
    assert_eq!(
        value_of(ci.queued(), EXECUTION_INSTANCE).as_deref(),
        Some("test_scheduler")
    );

    // Repeat lookups reuse the cached attribute set rather than rebuilding it.
    assert!(Arc::ptr_eq(&ci, &metrics.attrs_for_identity("ci")));
}
