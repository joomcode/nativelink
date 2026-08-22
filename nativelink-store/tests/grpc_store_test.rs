use core::pin::Pin;
use core::time::Duration;
use std::collections::HashMap;
use std::sync::Arc;

use async_lock::Mutex;
use bytes::Bytes;
use futures::stream::unfold;
use futures::{Stream, StreamExt};
use nativelink_config::stores::{GrpcEndpoint, GrpcSpec, Retry, StoreType};
use nativelink_error::{Error, ResultExt};
use nativelink_macro::nativelink_test;
use nativelink_proto::build::bazel::remote::execution::v2::content_addressable_storage_server::{
    ContentAddressableStorage, ContentAddressableStorageServer,
};
use nativelink_proto::build::bazel::remote::execution::v2::{
    BatchReadBlobsRequest, BatchReadBlobsResponse, BatchUpdateBlobsRequest,
    BatchUpdateBlobsResponse, Digest, FindMissingBlobsRequest, FindMissingBlobsResponse,
    GetTreeRequest, GetTreeResponse, SpliceBlobRequest, SpliceBlobResponse, SplitBlobRequest,
    SplitBlobResponse, chunking_function, compressor, digest_function,
};
use nativelink_proto::google::bytestream::byte_stream_server::{ByteStream, ByteStreamServer};
use nativelink_proto::google::bytestream::{
    QueryWriteStatusRequest, QueryWriteStatusResponse, ReadRequest, ReadResponse, WriteRequest,
    WriteResponse,
};
use nativelink_store::grpc_store::GrpcStore;
use nativelink_util::background_spawn;
use nativelink_util::buf_channel::make_buf_channel_pair;
use nativelink_util::common::DigestInfo;
use nativelink_util::store_trait::{StoreLike, UploadSizeInfo};
use nativelink_util::telemetry::ClientHeaders;
use nativelink_util::wire_compression::compress;
use opentelemetry::Context;
use regex::Regex;
use tokio::time::timeout;
use tonic::metadata::KeyAndValueRef;
use tonic::transport::Server;
use tonic::transport::server::TcpIncoming;
use tonic::{Request, Response, Status, Streaming};
use tracing::info;

const VALID_HASH: &str = "0123456789abcdef000000000000000000010000000000000123456789abcdef";
const RAW_INPUT: &str = "123";

fn test_spec<T: Into<String>>(endpoint: T, use_legacy_resource_names: bool) -> GrpcSpec {
    GrpcSpec {
        instance_name: String::new(),
        endpoints: vec![GrpcEndpoint {
            address: endpoint.into(),
            tls_config: None,
            concurrency_limit: None,
            connect_timeout_s: 0,
            tcp_keepalive_s: 0,
            http2_keepalive_interval_s: 0,
            http2_keepalive_timeout_s: 0,
        }],
        store_type: StoreType::Cas,
        retry: Retry::default(),
        max_concurrent_requests: 0,
        connections_per_endpoint: 0,
        rpc_timeout_s: 1,
        use_legacy_resource_names,
        headers: HashMap::new(),
        forward_headers: vec![],
        experimental_read_batching: None,
        experimental_remote_cache_compression: Some(false),
        load_balanced_channel: false,
    }
}

#[nativelink_test]
async fn fast_find_missing_blobs() -> Result<(), Error> {
    let spec = test_spec("http://foobar", false);
    let store = GrpcStore::new(&spec).await?;
    let request = Request::new(FindMissingBlobsRequest {
        instance_name: String::new(),
        blob_digests: vec![],
        digest_function: digest_function::Value::Sha256.into(),
    });
    let res = timeout(Duration::from_secs(1), async move {
        store.find_missing_blobs(request).await
    })
    .await??;
    let inner_res = res.into_inner();
    assert_eq!(inner_res.missing_blob_digests.len(), 0);
    Ok(())
}

#[derive(Debug, Clone)]
struct ReadRequestHolder {
    request: ReadRequest,
    metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct FakeStreamServer {
    write_requests: Arc<Mutex<Vec<WriteRequest>>>,
    read_requests: Arc<Mutex<Vec<ReadRequestHolder>>>,
}

impl FakeStreamServer {
    fn new() -> Self {
        Self {
            write_requests: Arc::new(Mutex::new(vec![])),
            read_requests: Arc::new(Mutex::new(vec![])),
        }
    }
}

type ReadStream = Pin<Box<dyn Stream<Item = Result<ReadResponse, Status>> + Send + 'static>>;

struct ReaderState {
    responded: bool,
}

#[tonic::async_trait]
impl ByteStream for FakeStreamServer {
    type ReadStream = ReadStream;

    async fn read(
        &self,
        grpc_request: Request<ReadRequest>,
    ) -> Result<Response<Self::ReadStream>, Status> {
        let mut request_metadata: HashMap<String, String> = HashMap::new();
        for kv in grpc_request.metadata().iter() {
            match kv {
                KeyAndValueRef::Ascii(metadata_key, metadata_value) => {
                    request_metadata.insert(
                        metadata_key.to_string(),
                        metadata_value.to_str().unwrap().to_string(),
                    );
                }
                KeyAndValueRef::Binary(metadata_key, metadata_value) => {
                    request_metadata
                        .insert(metadata_key.to_string(), format!("{metadata_value:#?}"));
                }
            }
        }
        let read_request = grpc_request.into_inner();
        self.read_requests.lock().await.push(ReadRequestHolder {
            request: read_request,
            metadata: request_metadata,
        });

        let folded = unfold(ReaderState { responded: false }, async move |state| {
            if state.responded {
                return None;
            }
            let response = ReadResponse {
                data: RAW_INPUT.as_bytes().into(),
            };
            Some((Ok(response), ReaderState { responded: true }))
        });
        Ok(Response::new(Box::pin(folded)))
    }

    async fn write(
        &self,
        grpc_request: Request<Streaming<WriteRequest>>,
    ) -> Result<Response<WriteResponse>, Status> {
        let write_request = match grpc_request.into_inner().next().await {
            None => {
                return Err(Status::unknown("Client closed stream"));
            }
            Some(Err(err)) => return Err(err),
            Some(Ok(write_request)) => write_request,
        };
        info!(?write_request, "write request");
        let committed_size = write_request.data.len().try_into().unwrap_or(i64::MAX);
        self.write_requests.lock().await.push(write_request);
        Ok(Response::new(WriteResponse { committed_size }))
    }

    #[allow(clippy::unimplemented)]
    async fn query_write_status(
        &self,
        _grpc_request: Request<QueryWriteStatusRequest>,
    ) -> Result<Response<QueryWriteStatusResponse>, Status> {
        unimplemented!();
    }
}

async fn make_fake_bytestream_server() -> (FakeStreamServer, u16) {
    let fake_stream_server = FakeStreamServer::new();
    let server = ByteStreamServer::new(fake_stream_server.clone());
    let listener = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();

    background_spawn!("server", async move {
        Server::builder()
            .add_service(server)
            .serve_with_incoming(listener)
            .await
            .unwrap();
    });

    (fake_stream_server, port)
}

async fn write_update_works_core(
    use_legacy_resource_names: bool,
    upload_pattern: Regex,
) -> Result<(), Error> {
    let (server, port) = make_fake_bytestream_server().await;
    let spec = test_spec(
        format!("http://localhost:{port}"),
        use_legacy_resource_names,
    );
    let store = GrpcStore::new(&spec).await?;
    let digest = DigestInfo::try_new(VALID_HASH, RAW_INPUT.len()).unwrap();

    let (mut tx, rx) = make_buf_channel_pair();
    let send_fut = async move {
        tx.send(RAW_INPUT.into()).await?;
        tx.send_eof()
    };
    let (res1, res2) = futures::join!(
        send_fut,
        store.update(
            digest,
            rx,
            UploadSizeInfo::ExactSize(RAW_INPUT.len().try_into().unwrap())
        )
    );
    res1.merge(res2)?;

    let write_requests = server.write_requests.lock().await;
    assert_eq!(write_requests.len(), 1);
    let write_request = write_requests.first().unwrap();
    assert!(
        upload_pattern.is_match(&write_request.resource_name),
        "resource name: {}",
        write_request.resource_name
    );
    assert_eq!(write_request.data, RAW_INPUT.as_bytes());
    Ok(())
}

#[nativelink_test]
async fn write_update_works() -> Result<(), Error> {
    let upload_pattern = Regex::new("/uploads/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/blobs/sha256/0123456789abcdef000000000000000000010000000000000123456789abcdef/3").unwrap();
    write_update_works_core(false, upload_pattern).await
}

#[nativelink_test]
async fn write_update_works_with_legacy_resource_names() -> Result<(), Error> {
    let upload_pattern = Regex::new("/uploads/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/blobs/0123456789abcdef000000000000000000010000000000000123456789abcdef/3").unwrap();
    write_update_works_core(true, upload_pattern).await
}

async fn read_works_core<F>(
    use_legacy_resource_names: bool,
    upload_pattern: &str,
    edit_spec: F,
) -> Result<ReadRequestHolder, Error>
where
    F: FnOnce(GrpcSpec) -> GrpcSpec,
{
    let (server, port) = make_fake_bytestream_server().await;
    let spec = edit_spec(test_spec(
        format!("http://localhost:{port}"),
        use_legacy_resource_names,
    ));
    let store = GrpcStore::new(&spec).await?;
    let digest = DigestInfo::try_new(VALID_HASH, RAW_INPUT.len()).unwrap();

    let (tx, mut rx) = make_buf_channel_pair();
    store.get_part(digest, tx, 0, None).await.unwrap();
    let bytes = rx.recv().await?;
    assert_eq!(bytes, RAW_INPUT.as_bytes());

    let read_requests = server.read_requests.lock().await;
    assert_eq!(read_requests.len(), 1);
    let read_request = read_requests.first().unwrap();
    assert_eq!(upload_pattern, &read_request.request.resource_name);

    Ok(read_request.clone())
}

#[nativelink_test]
async fn read_works() -> Result<(), Error> {
    let upload_pattern =
        "/blobs/sha256/0123456789abcdef000000000000000000010000000000000123456789abcdef/3";
    read_works_core(false, upload_pattern, core::convert::identity)
        .await
        .unwrap();
    Ok(())
}

#[nativelink_test]
async fn read_works_with_legacy_resource_names() -> Result<(), Error> {
    let upload_pattern =
        "/blobs/0123456789abcdef000000000000000000010000000000000123456789abcdef/3";
    read_works_core(true, upload_pattern, core::convert::identity)
        .await
        .unwrap();
    Ok(())
}

#[nativelink_test]
async fn read_works_with_headers() -> Result<(), Error> {
    fn set_spec(mut spec: GrpcSpec) -> GrpcSpec {
        spec.headers.insert("foo".into(), "bar".into());
        // Testing with mixed case, as it gets lowercased internally
        spec.forward_headers.push("SomeTHING".into());
        spec
    }

    let upload_pattern =
        "/blobs/sha256/0123456789abcdef000000000000000000010000000000000123456789abcdef/3";

    let client_headers = {
        let mut headers: HashMap<String, String> = HashMap::new();
        // We're inserting a lowercase one here as the telemetry insertion uses a lowercase one
        headers.insert("something".to_string(), "From outside".to_string());
        ClientHeaders(Arc::new(headers))
    };

    let cx_guard = Context::map_current(|cx| cx.with_value(client_headers)).attach();

    let read_request = read_works_core(false, upload_pattern, set_spec)
        .await
        .unwrap();
    assert_eq!(read_request.metadata.get("foo"), Some(&"bar".to_string()));
    assert_eq!(
        read_request.metadata.get("something"),
        Some(&"From outside".to_string()),
        "{:#?}",
        read_request.metadata
    );
    drop(cx_guard);

    Ok(())
}

#[derive(Debug, Clone)]
struct FakeCasServer {
    split_requests: Arc<Mutex<Vec<SplitBlobRequest>>>,
    splice_requests: Arc<Mutex<Vec<SpliceBlobRequest>>>,
}

impl FakeCasServer {
    fn new() -> Self {
        Self {
            split_requests: Arc::new(Mutex::new(vec![])),
            splice_requests: Arc::new(Mutex::new(vec![])),
        }
    }
}

type GetTreeStream = Pin<Box<dyn Stream<Item = Result<GetTreeResponse, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl ContentAddressableStorage for FakeCasServer {
    type GetTreeStream = GetTreeStream;

    #[allow(clippy::unimplemented)]
    async fn find_missing_blobs(
        &self,
        _grpc_request: Request<FindMissingBlobsRequest>,
    ) -> Result<Response<FindMissingBlobsResponse>, Status> {
        unimplemented!();
    }

    #[allow(clippy::unimplemented)]
    async fn batch_update_blobs(
        &self,
        _grpc_request: Request<BatchUpdateBlobsRequest>,
    ) -> Result<Response<BatchUpdateBlobsResponse>, Status> {
        unimplemented!();
    }

    #[allow(clippy::unimplemented)]
    async fn batch_read_blobs(
        &self,
        _grpc_request: Request<BatchReadBlobsRequest>,
    ) -> Result<Response<BatchReadBlobsResponse>, Status> {
        unimplemented!();
    }

    #[allow(clippy::unimplemented)]
    async fn get_tree(
        &self,
        _grpc_request: Request<GetTreeRequest>,
    ) -> Result<Response<Self::GetTreeStream>, Status> {
        unimplemented!();
    }

    async fn split_blob(
        &self,
        grpc_request: Request<SplitBlobRequest>,
    ) -> Result<Response<SplitBlobResponse>, Status> {
        let request = grpc_request.into_inner();
        self.split_requests.lock().await.push(request.clone());
        Ok(Response::new(SplitBlobResponse {
            chunk_digests: request.blob_digest.into_iter().collect(),
            chunking_function: request.chunking_function,
        }))
    }

    async fn splice_blob(
        &self,
        grpc_request: Request<SpliceBlobRequest>,
    ) -> Result<Response<SpliceBlobResponse>, Status> {
        let request = grpc_request.into_inner();
        self.splice_requests.lock().await.push(request.clone());
        Ok(Response::new(SpliceBlobResponse {
            blob_digest: request.blob_digest,
        }))
    }
}

async fn make_fake_cas_server() -> (FakeCasServer, u16) {
    let fake_cas_server = FakeCasServer::new();
    let server = ContentAddressableStorageServer::new(fake_cas_server.clone());
    let listener = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();

    background_spawn!("server", async move {
        Server::builder()
            .add_service(server)
            .serve_with_incoming(listener)
            .await
            .unwrap();
    });

    (fake_cas_server, port)
}

#[nativelink_test]
async fn split_and_splice_blob_forward_to_backend() -> Result<(), Error> {
    let (server, port) = make_fake_cas_server().await;
    let mut spec = test_spec(format!("http://localhost:{port}"), false);
    spec.instance_name = "backend_instance".to_string();
    let store = GrpcStore::new(&spec).await?;

    let digest = Digest {
        hash: VALID_HASH.to_string(),
        size_bytes: RAW_INPUT.len().try_into().unwrap_or(i64::MAX),
    };

    let split_response = store
        .split_blob(Request::new(SplitBlobRequest {
            instance_name: "local_instance".to_string(),
            blob_digest: Some(digest.clone()),
            digest_function: digest_function::Value::Sha256.into(),
            chunking_function: chunking_function::Value::FastCdc2020.into(),
        }))
        .await?
        .into_inner();
    assert_eq!(split_response.chunk_digests, vec![digest.clone()]);
    {
        let split_requests = server.split_requests.lock().await;
        assert_eq!(split_requests.len(), 1);
        // The instance name must be rewritten to the backend's.
        assert_eq!(split_requests[0].instance_name, "backend_instance");
    }

    let splice_response = store
        .splice_blob(Request::new(SpliceBlobRequest {
            instance_name: "local_instance".to_string(),
            blob_digest: Some(digest.clone()),
            chunk_digests: vec![digest.clone()],
            digest_function: digest_function::Value::Sha256.into(),
            chunking_function: chunking_function::Value::FastCdc2020.into(),
        }))
        .await?
        .into_inner();
    assert_eq!(splice_response.blob_digest, Some(digest));
    {
        let splice_requests = server.splice_requests.lock().await;
        assert_eq!(splice_requests.len(), 1);
        assert_eq!(splice_requests[0].instance_name, "backend_instance");
    }
    Ok(())
}

/// How the fake CAS breaks a `compressed-blobs/zstd` read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompressedReadFault {
    /// The read never starts: the RPC itself fails.
    FailToStart,
    /// The read starts, delivers one decodable frame, then the connection
    /// dies. This is what a CAS pod restart looks like to a worker.
    InterruptAfterFirstFrame,
}

#[derive(Debug, Clone)]
struct FaultyCompressedServer {
    fault: CompressedReadFault,
    /// A complete zstd frame that decodes to the first half of the payload.
    first_frame: Bytes,
    payload: Bytes,
    identity_reads: Arc<Mutex<Vec<ReadRequest>>>,
}

#[tonic::async_trait]
impl ByteStream for FaultyCompressedServer {
    type ReadStream = ReadStream;

    async fn read(
        &self,
        grpc_request: Request<ReadRequest>,
    ) -> Result<Response<Self::ReadStream>, Status> {
        let request = grpc_request.into_inner();
        if !request.resource_name.contains("compressed-blobs/zstd") {
            self.identity_reads.lock().await.push(request);
            return Ok(Response::new(Box::pin(unfold(
                Some(self.payload.clone()),
                async move |payload| {
                    let payload = payload?;
                    Some((Ok(ReadResponse { data: payload }), None))
                },
            ))));
        }

        if self.fault == CompressedReadFault::FailToStart {
            return Err(Status::unavailable("cas is restarting"));
        }

        Ok(Response::new(Box::pin(unfold(
            (0u8, self.first_frame.clone()),
            async move |(step, frame)| match step {
                0 => Some((
                    Ok(ReadResponse {
                        data: frame.clone(),
                    }),
                    (1, frame),
                )),
                1 => {
                    // Let the frame decode and reach the caller before the
                    // stream dies, so the read really is interrupted after
                    // partial delivery.
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Some((Err(Status::unavailable("connection reset")), (2, frame)))
                }
                _ => None,
            },
        ))))
    }

    #[allow(clippy::unimplemented)]
    async fn write(
        &self,
        _grpc_request: Request<Streaming<WriteRequest>>,
    ) -> Result<Response<WriteResponse>, Status> {
        unimplemented!();
    }

    #[allow(clippy::unimplemented)]
    async fn query_write_status(
        &self,
        _grpc_request: Request<QueryWriteStatusRequest>,
    ) -> Result<Response<QueryWriteStatusResponse>, Status> {
        unimplemented!();
    }
}

async fn make_faulty_compressed_store(
    fault: CompressedReadFault,
) -> Result<(FaultyCompressedServer, Arc<GrpcStore>, DigestInfo, Bytes), Error> {
    // Must be at least WIRE_COMPRESSION_MIN_SIZE_BYTES for the store to take
    // the compressed path at all.
    let payload = Bytes::from(vec![b'a'; 128 * 1024]);
    let first_frame = compress(payload.slice(..payload.len() / 2), compressor::Value::Zstd)?;
    let fake_server = FaultyCompressedServer {
        fault,
        first_frame,
        payload: payload.clone(),
        identity_reads: Arc::new(Mutex::new(vec![])),
    };

    let listener = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();
    let service = ByteStreamServer::new(fake_server.clone());
    background_spawn!("faulty_compressed_server", async move {
        Server::builder()
            .add_service(service)
            .serve_with_incoming(listener)
            .await
            .unwrap();
    });

    let mut spec = test_spec(format!("http://localhost:{port}"), false);
    spec.experimental_remote_cache_compression = Some(true);
    spec.rpc_timeout_s = 0;
    let store = GrpcStore::new(&spec).await?;
    let digest = DigestInfo::try_new(VALID_HASH, payload.len()).unwrap();
    Ok((fake_server, store, digest, payload))
}

#[nativelink_test]
async fn compressed_read_interrupted_after_partial_delivery_fails() -> Result<(), Error> {
    let (server, store, digest, _payload) =
        make_faulty_compressed_store(CompressedReadFault::InterruptAfterFirstFrame).await?;

    let result = store.get_part_unchunked(digest, 0, None).await;

    assert!(
        result.is_err(),
        "A compressed read that died after delivering data must fail, not \
         resume: the delivered byte count is not exact, so an identity read \
         from that offset splices a hole into the blob"
    );
    assert!(
        server.identity_reads.lock().await.is_empty(),
        "No identity read must be stitched onto the partial compressed read"
    );
    Ok(())
}

#[nativelink_test]
async fn compressed_read_that_never_starts_falls_back_to_identity() -> Result<(), Error> {
    let (server, store, digest, payload) =
        make_faulty_compressed_store(CompressedReadFault::FailToStart).await?;

    let data = store.get_part_unchunked(digest, 0, None).await?;

    assert_eq!(data, payload, "The identity fallback must return the blob");
    let identity_reads = server.identity_reads.lock().await;
    assert_eq!(identity_reads.len(), 1);
    assert_eq!(
        identity_reads[0].read_offset, 0,
        "Nothing was delivered, so the fallback must read the whole blob"
    );
    Ok(())
}

/// A `ByteStream` server that consumes an entire write stream and then never
/// answers. Models the state that wedged `CAS_FS_SHARD_STORE` in production:
/// the client finished sending, so the encoder half of
/// `GrpcStore::update_compressed` completed, but the RPC never settled.
#[derive(Clone, Default)]
struct StallingStreamServer;

#[tonic::async_trait]
impl ByteStream for StallingStreamServer {
    type ReadStream = Pin<Box<dyn Stream<Item = Result<ReadResponse, Status>> + Send + 'static>>;

    async fn read(
        &self,
        _grpc_request: Request<ReadRequest>,
    ) -> Result<Response<Self::ReadStream>, Status> {
        Err(Status::unimplemented("read"))
    }

    async fn write(
        &self,
        grpc_request: Request<Streaming<WriteRequest>>,
    ) -> Result<Response<WriteResponse>, Status> {
        let mut stream = grpc_request.into_inner();
        // Drain everything so the client-side encoder reaches EOF and finishes.
        while let Some(Ok(_)) = stream.next().await {}
        // Then never settle the RPC.
        core::future::pending::<()>().await;
        unreachable!()
    }

    async fn query_write_status(
        &self,
        _grpc_request: Request<QueryWriteStatusRequest>,
    ) -> Result<Response<QueryWriteStatusResponse>, Status> {
        Err(Status::unimplemented("query_write_status"))
    }
}

async fn make_stalling_bytestream_server() -> u16 {
    let server = ByteStreamServer::new(StallingStreamServer);
    let listener = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = listener.local_addr().unwrap().port();
    background_spawn!("stalling_server", async move {
        Server::builder()
            .add_service(server)
            .serve_with_incoming(listener)
            .await
            .unwrap();
    });
    port
}

/// A stalled RPC must not permanently consume its `max_concurrent_requests`
/// permit.
///
/// `ConnectionManager` holds one permit for the whole lifetime of an RPC and
/// `ConnectionManager::connection` waits for a permit with no timeout. So once
/// `max_concurrent_requests` RPCs are stuck, every later request to that store
/// parks forever and the store is dead until the process restarts.
///
/// This is what took out `CAS_FS_SHARD_STORE`: those shard stores enable
/// `experimental_remote_cache_compression` but set no `rpc_timeout_s`, and in
/// `update_compressed` the branch where the encoder finishes first awaits the
/// write with no bound. `CAS_S3_GRPC_STORE` runs the same code and survived,
/// because it sets `rpc_timeout_s`.
///
/// `rpc_timeout_s` is therefore load bearing, not an optimisation. Saturate the
/// store with stalled compressed writes, then assert a later write still gets a
/// permit.
#[nativelink_test]
async fn test_stalled_compressed_write_does_not_wedge_the_store() -> Result<(), Error> {
    const MAX_CONCURRENT: usize = 2;
    // Must be >= WIRE_COMPRESSION_MIN_SIZE_BYTES (64KiB) to take the
    // compressed path.
    const BLOB_SIZE: usize = 96 * 1024;

    // A stalled RPC holds its permit for rpc_timeout_s x (max_retries + 1).
    // Keep that short so the test is fast, and derive the assertion budget from
    // it rather than hard-coding one: the invariant under test is "a bounded
    // timeout releases the permit", not any particular production value. With
    // rpc_timeout_s = 0 the hold is unbounded and no budget is ever enough,
    // which is the failure this guards.
    const RPC_TIMEOUT_S: u64 = 1;
    const MAX_RETRIES: usize = 1;
    let budget = Duration::from_secs(RPC_TIMEOUT_S * (MAX_RETRIES as u64 + 1)) * 4;

    let port = make_stalling_bytestream_server().await;
    let mut spec = test_spec(format!("http://localhost:{port}"), false);
    spec.max_concurrent_requests = MAX_CONCURRENT;
    spec.connections_per_endpoint = MAX_CONCURRENT;
    spec.experimental_remote_cache_compression = Some(true);
    spec.rpc_timeout_s = RPC_TIMEOUT_S;
    spec.retry = Retry {
        max_retries: MAX_RETRIES,
        ..Retry::default()
    };
    // `GrpcStore::new` already yields an `Arc`.
    let store = GrpcStore::new(&spec).await?;

    let drive_one_write = |store: Arc<GrpcStore>| async move {
        let digest = DigestInfo::try_new(VALID_HASH, BLOB_SIZE).unwrap();
        let (mut tx, rx) = make_buf_channel_pair();
        background_spawn!("stalled_write_feed", async move {
            if tx.send(vec![0u8; BLOB_SIZE].into()).await.is_err() {
                return;
            }
            drop(tx.send_eof());
        });
        store
            .update(
                digest,
                rx,
                UploadSizeInfo::ExactSize(BLOB_SIZE.try_into().unwrap()),
            )
            .await
    };

    // Occupy every permit with a write the server will never answer.
    let saturators: Vec<_> = (0..MAX_CONCURRENT)
        .map(|_| background_spawn!("stalled_write", drive_one_write(store.clone())))
        .collect();

    // Give them time to acquire their permits and reach the stalled RPC.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The permits must come back. With no rpc_timeout_s they never do, because
    // ConnectionManager::connection waits for a permit without a deadline.
    timeout(budget, drive_one_write(store.clone()))
        .await
        .expect(
            "no permit became available: every max_concurrent_requests permit is still held by \
             a stalled RPC, so the store is wedged rather than merely slow",
        )
        .err();

    for saturator in saturators {
        drop(timeout(budget, saturator).await);
    }
    Ok(())
}
