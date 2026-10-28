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

use core::future::Future;
use core::time::Duration;
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::unfold;
use futures::{StreamExt, TryFutureExt};
use nativelink_config::schedulers::GrpcSpec;
use nativelink_error::{Code, Error, ResultExt, error_if, make_err};
use nativelink_metric::{MetricsComponent, RootMetricsComponent};
use nativelink_proto::build::bazel::remote::execution::v2::capabilities_client::CapabilitiesClient;
use nativelink_proto::build::bazel::remote::execution::v2::execution_client::ExecutionClient;
use nativelink_proto::build::bazel::remote::execution::v2::{
    ExecuteRequest, ExecutionPolicy, GetCapabilitiesRequest, WaitExecutionRequest,
};
use nativelink_proto::google::longrunning::Operation;
use nativelink_util::action_messages::{
    ActionInfo, ActionState, ActionUniqueQualifier, DEFAULT_EXECUTION_PRIORITY, OperationId,
};
use nativelink_util::connection_manager::ConnectionManager;
use nativelink_util::known_platform_property_provider::KnownPlatformPropertyProvider;
use nativelink_util::operation_state_manager::{
    ActionStateResult, ActionStateResultStream, ClientStateManager, OperationFilter,
};
use nativelink_util::origin_event::OriginMetadata;
use nativelink_util::retry::{Retrier, RetryResult};
use nativelink_util::telemetry::inject_current_context;
use nativelink_util::{background_spawn, tls_utils};
use parking_lot::Mutex;
use tokio::select;
use tokio::sync::watch;
use tokio::time::sleep;
use tonic::{Request, Streaming};
use tracing::{error, info, warn};

/// How long to wait for the upstream to end the stream after the final state.
const UPSTREAM_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

struct GrpcActionStateResult {
    client_operation_id: OperationId,
    // Err means the upstream stream failed before the action finished.
    rx: watch::Receiver<Result<Arc<ActionState>, Error>>,
}

#[async_trait]
impl ActionStateResult for GrpcActionStateResult {
    async fn as_state(&self) -> Result<(Arc<ActionState>, Option<OriginMetadata>), Error> {
        let mut action_state = self.rx.borrow().clone()?;
        Arc::make_mut(&mut action_state).client_operation_id = self.client_operation_id.clone();
        // TODO(palfrey) We currently don't support OriginMetadata in this implementation, but
        // we should.
        Ok((action_state, None))
    }

    async fn changed(&mut self) -> Result<(Arc<ActionState>, Option<OriginMetadata>), Error> {
        self.rx.changed().await.map_err(|e| {
            Error::from_std_err(Code::Internal, &e)
                .append("Channel closed in GrpcActionStateResult::changed")
        })?;
        let mut action_state = self.rx.borrow().clone()?;
        Arc::make_mut(&mut action_state).client_operation_id = self.client_operation_id.clone();
        // TODO(palfrey) We currently don't support OriginMetadata in this implementation, but
        // we should.
        Ok((action_state, None))
    }

    async fn as_action_info(&self) -> Result<(Arc<ActionInfo>, Option<OriginMetadata>), Error> {
        // TODO(palfrey) We should probably remove as_action_info()
        // or implement it properly.
        return Err(make_err!(
            Code::Unimplemented,
            "as_action_info not implemented for GrpcActionStateResult::as_action_info"
        ));
    }
}

#[derive(Debug, MetricsComponent)]
pub struct GrpcScheduler {
    #[metric(group = "property_managers")]
    supported_props: Mutex<HashMap<String, Vec<String>>>,
    retrier: Retrier,
    connection_manager: ConnectionManager,
}

impl GrpcScheduler {
    pub fn new(spec: &GrpcSpec) -> Result<Self, Error> {
        Self::new_with_jitter(spec, spec.retry.make_jitter_fn())
    }

    pub fn new_with_jitter(
        spec: &GrpcSpec,
        jitter_fn: Arc<dyn Fn(Duration) -> Duration + Send + Sync>,
    ) -> Result<Self, Error> {
        let endpoint = tls_utils::endpoint(&spec.endpoint)?;
        // The balanced path re-resolves DNS per connection and builds
        // per-address endpoints from the same configuration.
        let balanced_options = if spec.load_balanced_channel {
            // schedulers::GrpcSpec has no rpc_timeout_s; dead connections are
            // still detected via the endpoint's HTTP/2 keepalive.
            Some(tls_utils::load_balanced_options(
                &spec.endpoint,
                Duration::ZERO,
            )?)
        } else {
            None
        };
        Ok(Self {
            supported_props: Mutex::new(HashMap::new()),
            retrier: Retrier::new(
                Arc::new(|duration| Box::pin(sleep(duration))),
                jitter_fn.clone(),
                spec.retry.clone(),
            ),
            connection_manager: ConnectionManager::new(
                core::iter::once((endpoint, balanced_options)),
                spec.connections_per_endpoint,
                spec.max_concurrent_requests,
                spec.retry.clone(),
                jitter_fn,
            ),
        })
    }

    async fn perform_request<F, Fut, R, I>(&self, input: I, mut request: F) -> Result<R, Error>
    where
        F: FnMut(I) -> Fut + Send + Copy,
        Fut: Future<Output = Result<R, Error>> + Send,
        R: Send,
        I: Send + Clone,
    {
        self.retrier
            .retry(unfold(input, move |input| async move {
                let input_clone = input.clone();
                Some((
                    request(input_clone)
                        .await
                        .map_or_else(RetryResult::Retry, RetryResult::Ok),
                    input,
                ))
            }))
            .await
    }

    async fn stream_state(
        mut result_stream: Streaming<Operation>,
    ) -> Result<Box<dyn ActionStateResult>, Error> {
        if let Some(initial_response) = result_stream
            .message()
            .await
            .err_tip(|| "Receiving response from upstream scheduler")?
        {
            let client_operation_id = OperationId::from(initial_response.name.as_str());
            // Our operation_id is not needed here is just a place holder to recycle existing object.
            // The only thing that actually matters is the operation_id.
            let operation_id = OperationId::default();
            let action_state =
                ActionState::try_from_operation(initial_response, operation_id.clone())
                    .err_tip(|| "In GrpcScheduler::stream_state")?;
            // After the final state, read the upstream stream to its end even
            // if the client is gone. If we drop it before the trailers arrive,
            // h2 resets the stream, and late frames for reset streams count
            // towards h2's per-connection limit of 1024. At that limit h2 sends
            // GOAWAY(too_many_internal_resets) and kills all streams on the
            // connection.
            let mut finished = action_state.stage.is_finished();
            let (tx, mut rx) = watch::channel(Ok(Arc::new(action_state)));
            rx.mark_changed();
            background_spawn!("grpc_scheduler_stream_state", async move {
                loop {
                    select!(
                        () = tx.closed(), if !finished => {
                            info!(
                                "Client disconnected in GrpcScheduler"
                            );
                            return;
                        }
                        () = sleep(UPSTREAM_DRAIN_TIMEOUT), if finished => {
                            warn!("Upstream did not end the stream after the final state in GrpcScheduler");
                            return;
                        }
                        response = result_stream.message() => {
                            let response = match response {
                                Ok(Some(response)) => response,
                                // Upstream ended the stream after the final state.
                                Ok(None) if finished => return,
                                // The upstream connection failed (for example, an h2 GOAWAY)
                                // or the stream ended early. Send Unavailable, so the client
                                // retries the action instead of failing it.
                                res => {
                                    let mut err = res.err().map_or_else(
                                        || make_err!(Code::Unavailable, "Upstream scheduler closed the stream before the action finished"),
                                        Error::from,
                                    );
                                    err.code = Code::Unavailable;
                                    error!(?err, "Upstream stream failed in GrpcScheduler");
                                    drop(tx.send(Err(err)));
                                    return;
                                }
                            };
                            let maybe_action_state = ActionState::try_from_operation(response, operation_id.clone());
                            match maybe_action_state {
                                Ok(response) => {
                                    finished |= response.stage.is_finished();
                                    if let Err(err) = tx.send(Ok(Arc::new(response)))
                                        && !finished
                                    {
                                        info!(
                                            ?err,
                                            "Client error in GrpcScheduler"
                                        );
                                        return;
                                    }
                                }
                                Err(err) => {
                                    error!(
                                        ?err,
                                        "Error converting response to ActionState in GrpcScheduler"
                                    );
                                },
                            }
                        }
                    );
                }
            });

            return Ok(Box::new(GrpcActionStateResult {
                client_operation_id,
                rx,
            }));
        }
        Err(make_err!(
            Code::Internal,
            "Upstream scheduler didn't accept action."
        ))
    }

    async fn inner_get_known_properties(&self, instance_name: &str) -> Result<Vec<String>, Error> {
        if let Some(supported_props) = self.supported_props.lock().get(instance_name) {
            return Ok(supported_props.clone());
        }

        self.perform_request(instance_name, |instance_name| async move {
            // Not in the cache, lookup the capabilities with the upstream.
            let channel = self
                .connection_manager
                .connection("get_known_properties".into())
                .await
                .err_tip(|| "in get_platform_property_manager()")?;
            let mut capabilities_request = Request::new(GetCapabilitiesRequest {
                instance_name: instance_name.to_string(),
            });
            inject_current_context(&mut capabilities_request);
            let capabilities_result = CapabilitiesClient::new(channel)
                .get_capabilities(capabilities_request)
                .await
                .err_tip(|| "Retrieving upstream GrpcScheduler capabilities");
            let capabilities = capabilities_result?.into_inner();
            let supported_props = capabilities
                .execution_capabilities
                .err_tip(|| "Unable to get execution properties in GrpcScheduler")?
                .supported_node_properties
                .into_iter()
                .collect::<Vec<String>>();

            self.supported_props
                .lock()
                .insert(instance_name.to_string(), supported_props.clone());
            Ok(supported_props)
        })
        .await
    }

    async fn inner_add_action(
        &self,
        _client_operation_id: OperationId,
        action_info: Arc<ActionInfo>,
    ) -> Result<Box<dyn ActionStateResult>, Error> {
        let execution_policy = if action_info.priority == DEFAULT_EXECUTION_PRIORITY {
            None
        } else {
            Some(ExecutionPolicy {
                priority: action_info.priority,
            })
        };
        let skip_cache_lookup = match action_info.unique_qualifier {
            ActionUniqueQualifier::Cacheable(_) => false,
            ActionUniqueQualifier::Uncacheable(_) => true,
        };
        let request = ExecuteRequest {
            instance_name: action_info.instance_name().clone(),
            skip_cache_lookup,
            action_digest: Some(action_info.digest().into()),
            execution_policy,
            // TODO: Get me from the original request, not very important as we ignore it.
            results_cache_policy: None,
            digest_function: action_info
                .unique_qualifier
                .digest_function()
                .proto_digest_func()
                .into(),
        };
        let result_stream = self
            .perform_request(request, |request| async move {
                let channel = self
                    .connection_manager
                    .connection(format!("add_action: {:?}", request.action_digest))
                    .await
                    .err_tip(|| "in add_action()")?;
                // The upstream scheduler is where the action's origin metadata
                // is captured, so the client's identity and Bazel request
                // metadata must ride along with this hop.
                let mut request = Request::new(request);
                inject_current_context(&mut request);
                ExecutionClient::new(channel)
                    .execute(request)
                    .await
                    .err_tip(|| "Sending action to upstream scheduler")
            })
            .await?
            .into_inner();
        Self::stream_state(result_stream).await
    }

    async fn inner_filter_operations(
        &self,
        filter: OperationFilter,
    ) -> Result<ActionStateResultStream<'_>, Error> {
        error_if!(
            filter
                != OperationFilter {
                    client_operation_id: filter.client_operation_id.clone(),
                    ..Default::default()
                },
            "Unsupported filter in GrpcScheduler::filter_operations. Only client_operation_id is supported - {filter:?}"
        );
        let client_operation_id = filter.client_operation_id.ok_or_else(|| {
            make_err!(Code::InvalidArgument, "`client_operation_id` is the only supported filter in GrpcScheduler::filter_operations")
        })?;
        let request = WaitExecutionRequest {
            name: client_operation_id.to_string(),
        };
        let result_stream = self
            .perform_request(request, |request| async move {
                let channel = self
                    .connection_manager
                    .connection(format!("filter_operations: {}", request.name))
                    .await
                    .err_tip(|| "in find_by_client_operation_id()")?;
                let mut request = Request::new(request);
                inject_current_context(&mut request);
                ExecutionClient::new(channel)
                    .wait_execution(request)
                    .await
                    .err_tip(|| "While getting wait_execution stream")
            })
            .and_then(|result_stream| Self::stream_state(result_stream.into_inner()))
            .await;
        match result_stream {
            Ok(result_stream) => Ok(unfold(
                Some(result_stream),
                |maybe_result_stream| async move { maybe_result_stream.map(|v| (v, None)) },
            )
            .boxed()),
            Err(err) => {
                warn!(?err, "Error looking up action with upstream scheduler");
                Ok(futures::stream::empty().boxed())
            }
        }
    }
}

#[async_trait]
impl ClientStateManager for GrpcScheduler {
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

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_known_platform_property_provider(&self) -> Option<&dyn KnownPlatformPropertyProvider> {
        Some(self)
    }
}

#[async_trait]
impl KnownPlatformPropertyProvider for GrpcScheduler {
    async fn get_known_properties(&self, instance_name: &str) -> Result<Vec<String>, Error> {
        self.inner_get_known_properties(instance_name).await
    }
}

impl RootMetricsComponent for GrpcScheduler {}

#[cfg(test)]
mod tests {
    use core::pin::Pin;
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::time::SystemTime;

    use futures::Stream;
    use nativelink_macro::nativelink_test;
    use nativelink_proto::build::bazel::remote::execution::v2::execution_server::{
        Execution, ExecutionServer,
    };
    use nativelink_util::action_messages::{ActionResult, ActionStage};
    use nativelink_util::common::DigestInfo;
    use tonic::transport::server::TcpIncoming;
    use tonic::transport::{Endpoint, Server};
    use tonic::{Response, Status};

    use super::*;

    type OpStream = Pin<Box<dyn Stream<Item = Result<Operation, Status>> + Send>>;

    /// Answers the first Execute call with the given stream.
    struct FakeExecution(Mutex<Option<OpStream>>);

    #[tonic::async_trait]
    impl Execution for FakeExecution {
        type ExecuteStream = OpStream;
        type WaitExecutionStream = OpStream;

        async fn execute(
            &self,
            _request: Request<ExecuteRequest>,
        ) -> Result<Response<OpStream>, Status> {
            Ok(Response::new(self.0.lock().take().unwrap()))
        }

        async fn wait_execution(
            &self,
            _request: Request<WaitExecutionRequest>,
        ) -> Result<Response<OpStream>, Status> {
            Err(Status::unimplemented(""))
        }
    }

    fn operation(stage: ActionStage) -> Operation {
        ActionState {
            stage,
            last_transition_timestamp: SystemTime::now(),
            client_operation_id: OperationId::default(),
            action_digest: DigestInfo::zero_digest(),
        }
        .as_operation(OperationId::from("op"))
    }

    async fn upstream_result(upstream: OpStream) -> Box<dyn ActionStateResult> {
        let listener = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let service = FakeExecution(Mutex::new(Some(upstream)));
        background_spawn!("grpc_scheduler_test_server", async move {
            Server::builder()
                .add_service(ExecutionServer::new(service))
                .serve_with_incoming(listener)
                .await
                .unwrap();
        });
        let channel = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let stream = ExecutionClient::new(channel)
            .execute(ExecuteRequest::default())
            .await
            .unwrap()
            .into_inner();
        GrpcScheduler::stream_state(stream).await.unwrap()
    }

    async fn changed_after_upstream_ends(fail: bool) -> Error {
        let mut items = vec![Ok(operation(ActionStage::Queued))];
        if fail {
            items.push(Err(Status::internal("upstream connection lost")));
        }
        let mut result = upstream_result(Box::pin(futures::stream::iter(items))).await;
        // The initial Queued state.
        result.changed().await.unwrap();
        result.changed().await.unwrap_err()
    }

    #[nativelink_test]
    async fn upstream_error_is_unavailable() {
        let err = changed_after_upstream_ends(true).await;
        assert_eq!(err.code, Code::Unavailable, "{err:?}");
        assert!(
            format!("{err:?}").contains("upstream connection lost"),
            "{err:?}"
        );
    }

    #[nativelink_test]
    async fn upstream_early_end_is_unavailable() {
        let err = changed_after_upstream_ends(false).await;
        assert_eq!(err.code, Code::Unavailable, "{err:?}");
    }

    /// The client drops the result right after the final state. The upstream
    /// ends its stream later. The scheduler must read the stream to its end
    /// and not reset it, or the upstream drops the stream early.
    #[nativelink_test]
    async fn upstream_stream_is_drained_after_final_state() {
        let ended = Arc::new(AtomicBool::new(false));
        let ended_clone = ended.clone();
        let late_end = futures::stream::once(async move {
            sleep(Duration::from_millis(200)).await;
            ended_clone.store(true, Ordering::SeqCst);
        })
        .filter_map(|()| async { None });
        let upstream = futures::stream::iter([
            Ok(operation(ActionStage::Queued)),
            Ok(operation(ActionStage::Completed(ActionResult::default()))),
        ])
        .chain(late_end);

        let mut result = upstream_result(Box::pin(upstream)).await;
        result.changed().await.unwrap();
        let (state, _) = result.changed().await.unwrap();
        assert!(state.stage.is_finished());
        drop(result);

        sleep(Duration::from_millis(500)).await;
        assert!(ended.load(Ordering::SeqCst), "upstream stream was reset");
    }
}
