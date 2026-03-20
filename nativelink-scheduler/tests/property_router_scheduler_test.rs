use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

mod utils {
    pub(crate) mod scheduler_utils;
}

use futures::{StreamExt, join};
use nativelink_error::{Error, make_input_err};
use nativelink_macro::nativelink_test;
use nativelink_scheduler::mock_scheduler::MockActionScheduler;
use nativelink_scheduler::property_router_scheduler::PropertyRouterScheduler;
use nativelink_util::action_messages::{ActionStage, ActionState, OperationId};
use nativelink_util::common::DigestInfo;
use nativelink_util::known_platform_property_provider::KnownPlatformPropertyProvider;
use nativelink_util::operation_state_manager::{ClientStateManager, OperationFilter};
use pretty_assertions::assert_eq;
use tokio::sync::watch;
use utils::scheduler_utils::{TokioWatchActionStateResult, make_base_action_info};

struct TestContext {
    compile_scheduler: Arc<MockActionScheduler>,
    default_scheduler: Arc<MockActionScheduler>,
    router: PropertyRouterScheduler,
}

fn make_router() -> TestContext {
    let compile_scheduler = Arc::new(MockActionScheduler::new());
    let default_scheduler = Arc::new(MockActionScheduler::new());
    let mut routes = HashMap::new();
    routes.insert(
        "compile".to_string(),
        compile_scheduler.clone()
            as Arc<dyn nativelink_util::operation_state_manager::ClientStateManager>,
    );
    let router = PropertyRouterScheduler::new(
        "container-image",
        routes,
        default_scheduler.clone() as Arc<dyn ClientStateManager>,
    );
    TestContext {
        compile_scheduler,
        default_scheduler,
        router,
    }
}

#[nativelink_test]
async fn routes_to_matching_scheduler() -> Result<(), Error> {
    let ctx = make_router();
    let mut action_info = make_base_action_info(UNIX_EPOCH, DigestInfo::zero_digest())
        .as_ref()
        .clone();
    action_info
        .platform_properties
        .insert("container-image".to_string(), "compile".to_string());
    let action_info = Arc::new(action_info);

    let (_tx, rx) = watch::channel(Arc::new(ActionState {
        client_operation_id: OperationId::default(),
        stage: ActionStage::Queued,
        action_digest: action_info.unique_qualifier.digest(),
        last_transition_timestamp: SystemTime::now(),
    }));
    let client_operation_id = OperationId::default();

    let (_, (received_op_id, received_action)) =
        join!(
            ctx.router
                .add_action(client_operation_id.clone(), action_info.clone()),
            ctx.compile_scheduler.expect_add_action(Ok(Box::new(
                TokioWatchActionStateResult::new(client_operation_id.clone(), action_info, rx)
            ))),
        );
    assert_eq!(client_operation_id, received_op_id);
    assert_eq!(
        Some(&"compile".to_string()),
        received_action.platform_properties.get("container-image")
    );
    Ok(())
}

#[nativelink_test]
async fn routes_to_default_when_no_match() -> Result<(), Error> {
    let ctx = make_router();
    let mut action_info = make_base_action_info(UNIX_EPOCH, DigestInfo::zero_digest())
        .as_ref()
        .clone();
    action_info.platform_properties.insert(
        "container-image".to_string(),
        "some-other-image".to_string(),
    );
    let action_info = Arc::new(action_info);

    let (_tx, rx) = watch::channel(Arc::new(ActionState {
        client_operation_id: OperationId::default(),
        stage: ActionStage::Queued,
        action_digest: action_info.unique_qualifier.digest(),
        last_transition_timestamp: SystemTime::now(),
    }));
    let client_operation_id = OperationId::default();

    let (_, (received_op_id, received_action)) =
        join!(
            ctx.router
                .add_action(client_operation_id.clone(), action_info.clone()),
            ctx.default_scheduler.expect_add_action(Ok(Box::new(
                TokioWatchActionStateResult::new(client_operation_id.clone(), action_info, rx)
            ))),
        );
    assert_eq!(client_operation_id, received_op_id);
    assert_eq!(
        Some(&"some-other-image".to_string()),
        received_action.platform_properties.get("container-image")
    );
    Ok(())
}

#[nativelink_test]
async fn routes_to_default_when_property_missing() -> Result<(), Error> {
    let ctx = make_router();
    let action_info = make_base_action_info(UNIX_EPOCH, DigestInfo::zero_digest());

    let (_tx, rx) = watch::channel(Arc::new(ActionState {
        client_operation_id: OperationId::default(),
        stage: ActionStage::Queued,
        action_digest: action_info.unique_qualifier.digest(),
        last_transition_timestamp: SystemTime::now(),
    }));
    let client_operation_id = OperationId::default();

    let (_, (received_op_id, received_action)) =
        join!(
            ctx.router
                .add_action(client_operation_id.clone(), action_info.clone()),
            ctx.default_scheduler.expect_add_action(Ok(Box::new(
                TokioWatchActionStateResult::new(client_operation_id.clone(), action_info, rx)
            ))),
        );
    assert_eq!(client_operation_id, received_op_id);
    assert!(
        !received_action
            .platform_properties
            .contains_key("container-image"),
        "Expected no container-image property"
    );
    Ok(())
}

#[nativelink_test]
async fn routes_multiple_values() -> Result<(), Error> {
    let ctx = make_router();

    // First action: routes to compile_scheduler
    {
        let mut action_info = make_base_action_info(UNIX_EPOCH, DigestInfo::zero_digest())
            .as_ref()
            .clone();
        action_info
            .platform_properties
            .insert("container-image".to_string(), "compile".to_string());
        let action_info = Arc::new(action_info);

        let (_tx, rx) = watch::channel(Arc::new(ActionState {
            client_operation_id: OperationId::default(),
            stage: ActionStage::Queued,
            action_digest: action_info.unique_qualifier.digest(),
            last_transition_timestamp: SystemTime::now(),
        }));
        let client_operation_id = OperationId::default();

        let (_, (received_op_id, received_action)) = join!(
            ctx.router
                .add_action(client_operation_id.clone(), action_info.clone()),
            ctx.compile_scheduler.expect_add_action(Ok(Box::new(
                TokioWatchActionStateResult::new(client_operation_id.clone(), action_info, rx)
            ))),
        );
        assert_eq!(client_operation_id, received_op_id);
        assert_eq!(
            Some(&"compile".to_string()),
            received_action.platform_properties.get("container-image")
        );
    }

    // Second action: routes to default_scheduler
    {
        let mut action_info = make_base_action_info(UNIX_EPOCH, DigestInfo::zero_digest())
            .as_ref()
            .clone();
        action_info
            .platform_properties
            .insert("container-image".to_string(), "default-image".to_string());
        let action_info = Arc::new(action_info);

        let (_tx, rx) = watch::channel(Arc::new(ActionState {
            client_operation_id: OperationId::default(),
            stage: ActionStage::Queued,
            action_digest: action_info.unique_qualifier.digest(),
            last_transition_timestamp: SystemTime::now(),
        }));
        let client_operation_id = OperationId::default();

        let (_, (received_op_id, received_action)) = join!(
            ctx.router
                .add_action(client_operation_id.clone(), action_info.clone()),
            ctx.default_scheduler.expect_add_action(Ok(Box::new(
                TokioWatchActionStateResult::new(client_operation_id.clone(), action_info, rx)
            ))),
        );
        assert_eq!(client_operation_id, received_op_id);
        assert_eq!(
            Some(&"default-image".to_string()),
            received_action.platform_properties.get("container-image")
        );
    }

    Ok(())
}

#[nativelink_test]
async fn filter_operations_fans_out_to_all() -> Result<(), Error> {
    let ctx = make_router();
    let filter = OperationFilter {
        client_operation_id: Some(OperationId::default()),
        ..Default::default()
    };

    // The router calls filter_operations sequentially on routes then default.
    // Since HashMap order is arbitrary, we join both expects concurrently.
    let (router_result, compile_filter, default_filter) = join!(
        ctx.router.filter_operations(filter.clone()),
        ctx.compile_scheduler
            .expect_filter_operations(Ok(Box::pin(futures::stream::empty()))),
        ctx.default_scheduler
            .expect_filter_operations(Ok(Box::pin(futures::stream::empty()))),
    );

    assert!(router_result.unwrap().next().await.is_none());
    assert_eq!(filter, compile_filter);
    assert_eq!(filter, default_filter);
    Ok(())
}

#[nativelink_test]
async fn known_properties_unions_all_schedulers() -> Result<(), Error> {
    let ctx = make_router();

    let (known_props, _compile_instance, _default_instance) = join!(
        ctx.router.get_known_properties("my-instance"),
        ctx.compile_scheduler
            .expect_get_known_properties(Ok(vec!["cpu_arch".to_string()])),
        ctx.default_scheduler
            .expect_get_known_properties(Ok(vec!["os".to_string(), "cpu_arch".to_string()])),
    );

    let mut props = known_props.unwrap();
    props.sort();
    assert_eq!(vec!["cpu_arch".to_string(), "os".to_string()], props);
    Ok(())
}

#[nativelink_test]
async fn error_from_nested_scheduler_propagates() -> Result<(), Error> {
    let ctx = make_router();
    let mut action_info = make_base_action_info(UNIX_EPOCH, DigestInfo::zero_digest())
        .as_ref()
        .clone();
    action_info
        .platform_properties
        .insert("container-image".to_string(), "compile".to_string());
    let action_info = Arc::new(action_info);

    let client_operation_id = OperationId::default();
    let (result, _) = join!(
        ctx.router
            .add_action(client_operation_id.clone(), action_info.clone()),
        ctx.compile_scheduler
            .expect_add_action(Err(make_input_err!("Simulated scheduler error"))),
    );

    assert!(
        result.is_err(),
        "Expected error to propagate from nested scheduler"
    );
    Ok(())
}
