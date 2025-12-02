use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::sse::{Event, Sse};
use futures::stream::{self, Stream};
use nativelink_scheduler::cache_lookup_scheduler::CacheLookupScheduler;
use nativelink_scheduler::property_modifier_scheduler::PropertyModifierScheduler;
use nativelink_scheduler::simple_scheduler::SimpleScheduler;
use nativelink_util::operation_state_manager::ClientStateManager;
use tracing::error;

pub struct SchedulerStateService {
    schedulers: HashMap<String, Arc<dyn ClientStateManager>>,
}

impl SchedulerStateService {
    pub fn new(schedulers: HashMap<String, Arc<dyn ClientStateManager>>) -> Self {
        Self { schedulers }
    }

    pub async fn get_scheduler_state_handler(
        State(service): State<Arc<SchedulerStateService>>,
        Path(instance_name): Path<String>,
    ) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
        let stream = stream::unfold(
            (service, instance_name),
            |(service, instance_name)| async move {
                tokio::time::sleep(Duration::from_secs(1)).await;

                let event = if let Some(scheduler) = service.schedulers.get(&instance_name) {
                    if let Some(simple_scheduler) = Self::get_simple_scheduler(scheduler.as_ref()) {
                        match simple_scheduler.get_scheduler_state().await {
                            Ok(state) => match serde_json::to_string(&state) {
                                Ok(json) => Event::default().data(json),
                                Err(err) => {
                                    error!("Failed to serialize: {err}");
                                    Event::default().event("error").data("Serialization error")
                                }
                            },
                            Err(err) => {
                                error!("Failed to get state: {err}");
                                Event::default().event("error").data("Internal error")
                            }
                        }
                    } else {
                        Event::default()
                            .event("error")
                            .data("Scheduler does not support state retrieval")
                    }
                } else {
                    Event::default().event("error").data("Scheduler not found")
                };

                Some((Ok(event), (service, instance_name)))
            },
        );

        Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
    }

    fn get_simple_scheduler(scheduler: &dyn ClientStateManager) -> Option<&SimpleScheduler> {
        match scheduler {
            scheduler if scheduler.as_any().is::<SimpleScheduler>() => {
                scheduler.as_any().downcast_ref::<SimpleScheduler>()
            }

            scheduler if scheduler.as_any().is::<PropertyModifierScheduler>() => {
                Self::get_simple_scheduler(
                    scheduler
                        .as_any()
                        .downcast_ref::<PropertyModifierScheduler>()
                        .unwrap()
                        .scheduler
                        .as_ref(),
                )
            }

            scheduler if scheduler.as_any().is::<CacheLookupScheduler>() => {
                Self::get_simple_scheduler(
                    scheduler
                        .as_any()
                        .downcast_ref::<CacheLookupScheduler>()
                        .unwrap()
                        .action_scheduler
                        .as_ref(),
                )
            }

            _ => None,
        }
    }
}
