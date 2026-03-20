use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use nativelink_error::{Error, ResultExt};
use nativelink_metric::{MetricsComponent, RootMetricsComponent};
use nativelink_util::action_messages::{ActionInfo, OperationId};
use nativelink_util::known_platform_property_provider::KnownPlatformPropertyProvider;
use nativelink_util::operation_state_manager::{
    ActionStateResult, ActionStateResultStream, ClientStateManager, OperationFilter,
};

#[derive(MetricsComponent)]
pub struct PropertyRouterScheduler {
    property_name: String,
    #[metric(group = "routes")]
    routes: HashMap<String, Arc<dyn ClientStateManager>>,
    #[metric(group = "default_scheduler")]
    default_scheduler: Arc<dyn ClientStateManager>,
}

impl core::fmt::Debug for PropertyRouterScheduler {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PropertyRouterScheduler")
            .field("property_name", &self.property_name)
            .finish_non_exhaustive()
    }
}

impl PropertyRouterScheduler {
    pub fn new(
        property_name: &str,
        routes: HashMap<String, Arc<dyn ClientStateManager>>,
        default_scheduler: Arc<dyn ClientStateManager>,
    ) -> Self {
        Self {
            property_name: property_name.to_string(),
            routes,
            default_scheduler,
        }
    }

    async fn inner_add_action(
        &self,
        client_operation_id: OperationId,
        action_info: Arc<ActionInfo>,
    ) -> Result<Box<dyn ActionStateResult>, Error> {
        let scheduler = action_info
            .platform_properties
            .get(&self.property_name)
            .and_then(|value| self.routes.get(value))
            .unwrap_or(&self.default_scheduler);

        scheduler.add_action(client_operation_id, action_info).await
    }

    async fn inner_filter_operations(
        &self,
        filter: OperationFilter,
    ) -> Result<ActionStateResultStream<'_>, Error> {
        let mut streams = Vec::with_capacity(self.routes.len() + 1);
        for scheduler in self.routes.values() {
            streams.push(scheduler.filter_operations(filter.clone()).await?);
        }
        streams.push(self.default_scheduler.filter_operations(filter).await?);
        Ok(Box::pin(futures::stream::select_all(streams)))
    }

    async fn inner_get_known_properties(&self, instance_name: &str) -> Result<Vec<String>, Error> {
        let mut all_props = HashSet::new();
        for scheduler in self.routes.values() {
            if let Some(p) = scheduler.as_known_platform_property_provider() {
                for prop in p
                    .get_known_properties(instance_name)
                    .await
                    .err_tip(|| "In PropertyRouterScheduler::get_known_properties for route")?
                {
                    all_props.insert(prop);
                }
            }
        }
        if let Some(p) = self.default_scheduler.as_known_platform_property_provider() {
            for prop in p
                .get_known_properties(instance_name)
                .await
                .err_tip(|| "In PropertyRouterScheduler::get_known_properties for default")?
            {
                all_props.insert(prop);
            }
        }
        Ok(all_props.into_iter().collect())
    }
}

#[async_trait]
impl KnownPlatformPropertyProvider for PropertyRouterScheduler {
    async fn get_known_properties(&self, instance_name: &str) -> Result<Vec<String>, Error> {
        self.inner_get_known_properties(instance_name).await
    }
}

#[async_trait]
impl ClientStateManager for PropertyRouterScheduler {
    async fn add_action(
        &self,
        client_operation_id: OperationId,
        action_info: Arc<ActionInfo>,
    ) -> Result<Box<dyn ActionStateResult>, Error> {
        self.inner_add_action(client_operation_id, action_info)
            .await
    }

    async fn filter_operations<'a>(
        &'a self,
        filter: OperationFilter,
    ) -> Result<ActionStateResultStream<'a>, Error> {
        self.inner_filter_operations(filter).await
    }

    fn as_known_platform_property_provider(&self) -> Option<&dyn KnownPlatformPropertyProvider> {
        Some(self)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl RootMetricsComponent for PropertyRouterScheduler {}
