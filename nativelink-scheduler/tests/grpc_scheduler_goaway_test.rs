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

//! Reproduces the h2 `GOAWAY(too_many_internal_resets)` between a proxy
//! `GrpcScheduler` and its upstream scheduler.
//!
//! The upstream finishes each action at once (like a cache hit) and ends the
//! stream a moment later. A TCP proxy adds latency, so the trailers are still
//! in flight when the client drops the stream and h2 sends `RST_STREAM`. When
//! more than 50 streams wait for reset expiration, h2 forgets the extra ones,
//! and each late frame for a forgotten stream counts as a local error reset.
//! After 1024 of those, h2 sends GOAWAY and kills every stream on the
//! connection, and the proxy reports "Channel closed" for actions in flight.

use core::pin::Pin;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;
use std::sync::Arc;
use std::time::SystemTime;

use futures::{Stream, StreamExt};
use nativelink_config::schedulers::GrpcSpec;
use nativelink_macro::nativelink_test;
use nativelink_proto::build::bazel::remote::execution::v2::execution_server::{
    Execution, ExecutionServer,
};
use nativelink_proto::build::bazel::remote::execution::v2::{ExecuteRequest, WaitExecutionRequest};
use nativelink_proto::google::longrunning::Operation;
use nativelink_scheduler::grpc_scheduler::GrpcScheduler;
use nativelink_util::action_messages::{ActionResult, ActionStage, ActionState, OperationId};
use nativelink_util::background_spawn;
use nativelink_util::common::DigestInfo;
use nativelink_util::operation_state_manager::ClientStateManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, sleep_until};
use tonic::transport::Server;
use tonic::transport::server::TcpIncoming;
use tonic::{Request, Response, Status};

mod utils {
    pub(crate) mod scheduler_utils;
}
use utils::scheduler_utils::make_base_action_info;

const ACTIONS: usize = 3000;
const CONCURRENCY: usize = 200;
const ONE_WAY_LATENCY: Duration = Duration::from_millis(20);
const TRAILERS_DELAY: Duration = Duration::from_millis(2);
const SLOW_ACTION: Duration = Duration::from_millis(300);

type OpStream = Pin<Box<dyn Stream<Item = Result<Operation, Status>> + Send>>;

struct InstantCompleteExecution;

#[tonic::async_trait]
impl Execution for InstantCompleteExecution {
    type ExecuteStream = OpStream;
    type WaitExecutionStream = OpStream;

    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> Result<Response<OpStream>, Status> {
        let operation = |stage| {
            Ok(ActionState {
                stage,
                last_transition_timestamp: SystemTime::now(),
                client_operation_id: OperationId::default(),
                action_digest: DigestInfo::zero_digest(),
            }
            .as_operation(OperationId::default()))
        };
        let completed =
            futures::stream::iter([operation(ActionStage::Completed(ActionResult::default()))]);
        // The pause puts the trailers in a separate TCP segment.
        let trailers_later =
            futures::stream::once(sleep(TRAILERS_DELAY)).filter_map(|()| async { None });
        // Every 10th action runs for a while, so a GOAWAY catches it in flight.
        let size = request.into_inner().action_digest.unwrap().size_bytes;
        if size % 10 == 0 {
            let running = futures::stream::iter([operation(ActionStage::Queued)])
                .chain(futures::stream::once(sleep(SLOW_ACTION)).filter_map(|()| async { None }));
            return Ok(Response::new(Box::pin(
                running.chain(completed).chain(trailers_later),
            )));
        }
        Ok(Response::new(Box::pin(completed.chain(trailers_later))))
    }

    async fn wait_execution(
        &self,
        _request: Request<WaitExecutionRequest>,
    ) -> Result<Response<OpStream>, Status> {
        Err(Status::unimplemented(""))
    }
}

/// Copies bytes from `from` to `to`, each chunk `ONE_WAY_LATENCY` late.
fn delayed_pipe(mut from: tokio::net::tcp::OwnedReadHalf, mut to: tokio::net::tcp::OwnedWriteHalf) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    background_spawn!("delay_proxy_read", async move {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n @ 1..) = from.read(&mut buf).await {
            if tx
                .send((Instant::now() + ONE_WAY_LATENCY, buf[..n].to_vec()))
                .is_err()
            {
                return;
            }
        }
    });
    background_spawn!("delay_proxy_write", async move {
        while let Some((at, bytes)) = rx.recv().await {
            sleep_until(at).await;
            if to.write_all(&bytes).await.is_err() {
                return;
            }
        }
    });
}

/// Starts the upstream behind a latency proxy. Returns the proxy port and
/// the number of TCP connections the proxy accepted.
async fn start_upstream_behind_latency() -> (u16, Arc<AtomicUsize>) {
    let upstream = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    background_spawn!("upstream_scheduler", async move {
        Server::builder()
            .add_service(ExecutionServer::new(InstantCompleteExecution))
            .serve_with_incoming(upstream)
            .await
            .unwrap();
    });

    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = proxy.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = connections.clone();
    background_spawn!("latency_proxy", async move {
        while let Ok((client, _)) = proxy.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let server = TcpStream::connect(upstream_addr).await.unwrap();
            let (client_read, client_write) = client.into_split();
            let (server_read, server_write) = server.into_split();
            delayed_pipe(client_read, server_write);
            delayed_pipe(server_read, client_write);
        }
    });
    (port, connections)
}

#[nativelink_test]
async fn finished_actions_do_not_kill_upstream_connection() {
    let (port, connections) = start_upstream_behind_latency().await;
    let spec: GrpcSpec = serde_json::from_value(serde_json::json!({
        "endpoint": { "address": format!("grpc://127.0.0.1:{port}") },
        "connections_per_endpoint": 1,
    }))
    .unwrap();
    let scheduler = Arc::new(GrpcScheduler::new(&spec).unwrap());

    let errors: Vec<String> = futures::stream::iter(0..ACTIONS)
        .map(|i| {
            let scheduler = scheduler.clone();
            async move {
                let action_info =
                    make_base_action_info(SystemTime::now(), DigestInfo::new([0u8; 32], i as u64));
                let mut result = scheduler
                    .add_action(OperationId::default(), action_info)
                    .await
                    .map_err(|err| format!("add_action: {err:?}"))?;
                loop {
                    let (state, _) = result
                        .changed()
                        .await
                        .map_err(|err| format!("changed: {err:?}"))?;
                    // Like execution_server.rs: drop the result after the final state.
                    if state.stage.is_finished() {
                        return Ok(());
                    }
                }
            }
        })
        .buffer_unordered(CONCURRENCY)
        .filter_map(|res: Result<(), String>| async move { res.err() })
        .collect()
        .await;

    let connections = connections.load(Ordering::SeqCst);
    tracing::info!(
        "actions: {ACTIONS}, failed: {}, upstream connections: {connections}",
        errors.len()
    );
    for err in errors.iter().take(3) {
        tracing::info!("  {err}");
    }
    assert!(
        errors.is_empty(),
        "{} of {ACTIONS} actions failed",
        errors.len()
    );
    assert_eq!(
        connections, 1,
        "the upstream connection was killed and reopened"
    );
}
