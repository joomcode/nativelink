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

use core::fmt::Debug;
use core::hash::{Hash, Hasher};
use core::pin::Pin;
use core::time::Duration;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::DefaultHasher;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::{FuturesUnordered, unfold};
use futures::{StreamExt, TryStreamExt};
use nativelink_config::stores::ExperimentalGcsSpec;
use nativelink_error::{Code, Error, ResultExt, make_err};
use nativelink_metric::MetricsComponent;
use nativelink_util::background_spawn;
use nativelink_util::buf_channel::{DropCloserReadHalf, DropCloserWriteHalf};
use nativelink_util::health_utils::{HealthRegistryBuilder, HealthStatus, HealthStatusIndicator};
use nativelink_util::instant_wrapper::InstantWrapper;
use nativelink_util::metrics::{GCS_METRICS, GcsCustomTimeAttrs};
use nativelink_util::retry::{Retrier, RetryResult};
use nativelink_util::store_trait::{
    RemoveItemCallback, StoreDriver, StoreKey, StoreOptimizations, UploadSizeInfo,
};
use parking_lot::Mutex;
use rand::Rng;
use tokio::sync::Semaphore;
use tokio::time::{sleep, timeout};
use tracing::{debug, warn};

use crate::cas_utils::is_zero_digest;
use crate::gcs_client::client::{GcsClient, GcsOperations};
use crate::gcs_client::types::{
    CHUNK_SIZE, DEFAULT_CONCURRENT_UPLOADS, DEFAULT_MAX_RETRY_BUFFER_PER_REQUEST,
    MIN_MULTIPART_SIZE, ObjectPath,
};

/// Upper bound on the number of entries retained in the in-memory
/// `customTime` refresh throttle. The map only suppresses redundant metadata
/// writes, so when it reaches this size it is simply cleared; the only effect
/// is that a few objects may be re-stamped sooner than strictly necessary.
const MAX_CUSTOM_TIME_THROTTLE_ENTRIES: usize = 100_000;

/// Divisor applied to `max_concurrent_uploads` to size the separate budget for
/// `customTime` writes. `customTime` writes are best-effort metadata traffic
/// and must never consume every connection permit, because reads and uploads
/// share the same pool. The budget is at least 1.
const CUSTOM_TIME_CONCURRENCY_DIVISOR: usize = 4;

/// Largest extra fraction, in per-mille, added to the `customTime` refresh
/// interval per object. See [`GcsStore::refresh_interval_for`].
const CUSTOM_TIME_JITTER_PER_MILLE: u64 = 500;

#[derive(MetricsComponent, Debug)]
pub struct GcsStore<Client: GcsOperations, NowFn> {
    client: Arc<Client>,
    now_fn: NowFn,
    #[metric(help = "The bucket name for the GCS store")]
    bucket: String,
    #[metric(help = "The key prefix for the GCS store")]
    key_prefix: String,
    retrier: Retrier,
    #[metric(help = "The number of seconds to consider an object expired")]
    consider_expired_after_s: i64,
    #[metric(help = "The number of bytes to buffer for retrying requests")]
    max_retry_buffer_size: usize,
    #[metric(help = "The size of chunks for resumable uploads")]
    max_chunk_size: usize,
    #[metric(help = "The number of concurrent uploads allowed")]
    max_concurrent_uploads: usize,
    #[metric(
        help = "Minimum seconds between customTime refreshes per object; 0 disables RLU tracking"
    )]
    custom_time_refresh_interval_s: i64,
    /// Tracks the last `customTime` this process stamped per object path, used
    /// to throttle metadata writes on reads. Bounded by
    /// `MAX_CUSTOM_TIME_THROTTLE_ENTRIES`.
    recently_touched: Mutex<HashMap<String, i64>>,
    /// Caps how many `customTime` writes this store has in flight at once, so
    /// that a large `FindMissingBlobs` cannot starve reads and uploads of
    /// connection permits. A touch that cannot get a permit is dropped.
    touch_semaphore: Arc<Semaphore>,
    /// Random per-process value mixed into the per-object refresh deadline.
    /// Two pods that hold the same object therefore cross the refresh
    /// threshold at different times instead of all writing at once.
    refresh_jitter_salt: u64,
    /// Pre-built attribute sets for the `customTime` metrics in
    /// [`nativelink_util::metrics::GCS_METRICS`]. Shared so that the detached
    /// PATCH task can record its own outcome.
    custom_time_attrs: Arc<GcsCustomTimeAttrs>,
}

impl<I, NowFn> GcsStore<GcsClient, NowFn>
where
    I: InstantWrapper,
    NowFn: Fn() -> I + Send + Sync + Unpin + 'static,
{
    pub async fn new(spec: &ExperimentalGcsSpec, now_fn: NowFn) -> Result<Arc<Self>, Error> {
        let client = Arc::new(GcsClient::new(spec).await?);
        Self::new_with_ops(spec, client, now_fn)
    }
}

impl<I, Client, NowFn> GcsStore<Client, NowFn>
where
    I: InstantWrapper,
    Client: GcsOperations + Send + Sync + 'static,
    NowFn: Fn() -> I + Send + Sync + Unpin + 'static,
{
    // Primarily used for injecting a mock or real operations implementation
    pub fn new_with_ops(
        spec: &ExperimentalGcsSpec,
        client: Arc<Client>,
        now_fn: NowFn,
    ) -> Result<Arc<Self>, Error> {
        // Chunks must be a multiple of 256kb according to the documentation.
        const CHUNK_MULTIPLE: usize = 256 * 1024;

        let max_connections = spec
            .common
            .multipart_max_concurrent_uploads
            .unwrap_or(DEFAULT_CONCURRENT_UPLOADS);

        let jitter_amt = spec.common.retry.jitter;
        let jitter_fn = Arc::new(move |delay: Duration| {
            if jitter_amt == 0.0 {
                return delay;
            }
            delay.mul_f32(jitter_amt.mul_add(rand::rng().random::<f32>() - 0.5, 1.))
        });

        let max_chunk_size =
            core::cmp::min(spec.resumable_chunk_size.unwrap_or(CHUNK_SIZE), CHUNK_SIZE);

        let max_chunk_size = if max_chunk_size.is_multiple_of(CHUNK_MULTIPLE) {
            max_chunk_size
        } else {
            ((max_chunk_size + CHUNK_MULTIPLE / 2) / CHUNK_MULTIPLE) * CHUNK_MULTIPLE
        };

        let max_retry_buffer_size = spec
            .common
            .max_retry_buffer_per_request
            .unwrap_or(DEFAULT_MAX_RETRY_BUFFER_PER_REQUEST);

        // The retry buffer should be at least as big as the chunk size.
        let max_retry_buffer_size = if max_retry_buffer_size < max_chunk_size {
            max_chunk_size
        } else {
            max_retry_buffer_size
        };

        Ok(Arc::new(Self {
            client,
            now_fn,
            bucket: spec.bucket.clone(),
            key_prefix: spec
                .common
                .key_prefix
                .as_ref()
                .unwrap_or(&String::new())
                .clone(),
            retrier: Retrier::new(
                Arc::new(|duration| Box::pin(sleep(duration))),
                jitter_fn,
                spec.common.retry.clone(),
            ),
            consider_expired_after_s: i64::from(spec.common.consider_expired_after_s),
            max_retry_buffer_size,
            max_chunk_size,
            max_concurrent_uploads: max_connections,
            custom_time_refresh_interval_s: i64::from(spec.custom_time_refresh_interval_s),
            recently_touched: Mutex::new(HashMap::new()),
            touch_semaphore: Arc::new(Semaphore::new(core::cmp::max(
                1,
                max_connections / CUSTOM_TIME_CONCURRENCY_DIVISOR,
            ))),
            refresh_jitter_salt: rand::rng().random(),
            custom_time_attrs: Arc::new(GcsCustomTimeAttrs::new(&spec.bucket)),
        }))
    }

    async fn has(self: Pin<&Self>, key: &StoreKey<'_>) -> Result<Option<u64>, Error> {
        let object_path = self.make_object_path(key);
        let client = &self.client;
        let consider_expired_after_s = self.consider_expired_after_s;
        let now_fn = &self.now_fn;

        let result = self
            .retrier
            .retry(unfold(object_path.clone(), move |object_path| async move {
                match client.read_object_metadata(&object_path).await.err_tip(|| {
                    format!(
                        "Error while trying to read - bucket: {} path: {}",
                        object_path.bucket, object_path.path
                    )
                }) {
                    Ok(Some(metadata)) => {
                        if consider_expired_after_s != 0
                            && let Some(update_time) = &metadata.update_time
                        {
                            let now_s = now_fn().unix_timestamp() as i64;
                            if update_time.seconds + consider_expired_after_s <= now_s {
                                return Some((RetryResult::Ok(None), object_path));
                            }
                        }

                        if metadata.size >= 0 {
                            // Carry `customTime` out with the size. It is the
                            // authoritative record of when any process last
                            // refreshed this object, so it decides whether this
                            // process needs to write it again.
                            let custom_time_s = metadata.custom_time.map(|t| t.seconds);
                            Some((
                                RetryResult::Ok(Some((metadata.size as u64, custom_time_s))),
                                object_path,
                            ))
                        } else {
                            Some((
                                RetryResult::Err(make_err!(
                                    Code::InvalidArgument,
                                    "Invalid metadata size in GCS: {}",
                                    metadata.size
                                )),
                                object_path,
                            ))
                        }
                    }
                    Ok(None) => Some((RetryResult::Ok(None), object_path)),
                    Err(e) if e.code == Code::NotFound => {
                        Some((RetryResult::Ok(None), object_path))
                    }
                    Err(e) => Some((RetryResult::Retry(e), object_path)),
                }
            }))
            .await?;

        // A present blob returned from an existence check (FindMissingBlobs) is
        // an active-liveness signal: the caller is relying on it right now and
        // will skip re-uploading it. Refresh its recency (throttled) so it is
        // not evicted by the `daysSinceCustomTime` lifecycle rule while still in
        // use, even if it is never re-downloaded.
        let Some((size, remote_custom_time_s)) = result else {
            return Ok(None);
        };
        self.touch_custom_time_if_stale(&object_path, remote_custom_time_s);

        Ok(Some(size))
    }

    fn make_object_path(&self, key: &StoreKey) -> ObjectPath {
        ObjectPath::new(
            self.bucket.clone(),
            &format!("{}{}", self.key_prefix, key.as_str()),
        )
    }

    /// Refresh interval this process applies to one object, spread over
    /// `[interval, interval * 1.5)`.
    ///
    /// Every pod holds the same objects and reads the same `customTime`, so a
    /// single shared threshold makes all of them cross it together and PATCH
    /// the same object at the same instant. GCS allows about one write per
    /// second to one object, so that burst is what times out. The deadline is
    /// derived from the object path and a per-process salt, which spreads the
    /// writes across pods while staying stable for a given object.
    fn refresh_interval_for(&self, path: &str) -> i64 {
        let mut hasher = DefaultHasher::new();
        self.refresh_jitter_salt.hash(&mut hasher);
        path.hash(&mut hasher);
        let per_mille = (hasher.finish() % (CUSTOM_TIME_JITTER_PER_MILLE + 1)) as i64;
        self.custom_time_refresh_interval_s.saturating_add(
            self.custom_time_refresh_interval_s
                .saturating_mul(per_mille)
                / 1000,
        )
    }

    /// Record that `path` carries `stamped_s` as its `customTime`, so that
    /// later reads in this process skip the staleness check.
    fn record_touch(&self, path: &str, stamped_s: i64) {
        let mut recently_touched = self.recently_touched.lock();
        if recently_touched.len() >= MAX_CUSTOM_TIME_THROTTLE_ENTRIES {
            recently_touched.clear();
        }
        recently_touched.insert(path.to_string(), stamped_s);
    }

    /// Refresh an object's `customTime` only if the value GCS reported is
    /// older than this object's jittered refresh interval.
    ///
    /// GCS object metadata is strongly consistent after a write, so a stamp
    /// written by another pod suppresses this pod's write. This keeps the
    /// write rate for one object independent of the number of pods.
    fn touch_custom_time_if_stale(
        self: Pin<&Self>,
        object_path: &ObjectPath,
        remote_custom_time_s: Option<i64>,
    ) {
        if self.custom_time_refresh_interval_s == 0 {
            return;
        }

        let now_s = (self.now_fn)().unix_timestamp() as i64;
        if let Some(last_s) = remote_custom_time_s
            && now_s.saturating_sub(last_s) < self.refresh_interval_for(&object_path.path)
        {
            // Another process already refreshed this object. Seed the local
            // map with the remote value so the next existence check in this
            // process does not re-evaluate it.
            self.record_touch(&object_path.path, last_s);
            GCS_METRICS
                .custom_time_operations
                .add(1, self.custom_time_attrs.skipped_fresh());
            return;
        }

        self.touch_custom_time(object_path, false);
    }

    /// Stamp an object's `customTime` with the current time for "recently last
    /// used" tracking. When `force` is false (reads and existence checks) the
    /// write is throttled to at most once per `custom_time_refresh_interval_s`
    /// per object; when true (writes) the baseline is always set.
    ///
    /// The throttle decision is made synchronously, but the `customTime` PATCH
    /// itself is dispatched to a detached background task so it never adds
    /// latency to the read/write hot path. It is best-effort. A dropped or
    /// failed PATCH self-heals on the next interval, so neither one is
    /// propagated and neither one is logged above debug level.
    fn touch_custom_time(self: Pin<&Self>, object_path: &ObjectPath, force: bool) {
        if self.custom_time_refresh_interval_s == 0 {
            return;
        }

        let now_s = (self.now_fn)().unix_timestamp() as i64;

        // The throttle check, the budget check and the record must all happen
        // under one lock hold. If they do not, a burst of concurrent reads for
        // the same object each passes the check and each dispatches a PATCH.
        // `try_acquire_owned` never blocks and never awaits, so it is safe
        // under the mutex.
        let permit = {
            let mut recently_touched = self.recently_touched.lock();
            if !force
                && let Some(&last_s) = recently_touched.get(&object_path.path)
                && now_s.saturating_sub(last_s) < self.custom_time_refresh_interval_s
            {
                return;
            }

            // Take the permit before recording the touch. If the budget is
            // full, the object must stay stale in the map so that the next
            // existence check retries it.
            let Ok(permit) = Arc::clone(&self.touch_semaphore).try_acquire_owned() else {
                GCS_METRICS
                    .custom_time_operations
                    .add(1, self.custom_time_attrs.dropped());
                debug!(
                    path = object_path.path,
                    "Dropped a customTime refresh because the touch budget was full",
                );
                return;
            };

            // Record optimistically (before dispatching the PATCH) so that a
            // later read in this process is suppressed. A failed PATCH
            // self-heals on the next interval.
            if recently_touched.len() >= MAX_CUSTOM_TIME_THROTTLE_ENTRIES {
                recently_touched.clear();
            }
            recently_touched.insert(object_path.path.clone(), now_s);
            permit
        };

        let client = Arc::clone(&self.client);
        let attrs = Arc::clone(&self.custom_time_attrs);
        let object_path = object_path.clone();
        background_spawn!("gcs_touch_custom_time", async move {
            let start = Instant::now();
            let result = client.update_object_custom_time(&object_path, now_s).await;
            drop(permit);

            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            let attrs = match &result {
                Ok(()) => attrs.written(),
                Err(_) => attrs.failed(),
            };
            GCS_METRICS.custom_time_operations.add(1, attrs);
            GCS_METRICS.custom_time_duration.record(elapsed_ms, attrs);

            if let Err(e) = result {
                debug!(
                    ?e,
                    path = object_path.path,
                    "Failed to update customTime for RLU tracking",
                );
            }
        });
    }
}

#[async_trait]
impl<I, Client, NowFn> StoreDriver for GcsStore<Client, NowFn>
where
    I: InstantWrapper,
    Client: GcsOperations + 'static,
    NowFn: Fn() -> I + Send + Sync + Unpin + 'static,
{
    async fn post_init(self: Arc<Self>) -> Result<(), Error> {
        Ok(())
    }

    async fn has_with_results(
        self: Pin<&Self>,
        keys: &[StoreKey<'_>],
        results: &mut [Option<u64>],
    ) -> Result<(), Error> {
        keys.iter()
            .zip(results.iter_mut())
            .map(|(key, result)| async move {
                if is_zero_digest(key.borrow()) {
                    *result = Some(0);
                    return Ok(());
                }
                *result = self.has(key).await?;
                Ok(())
            })
            .collect::<FuturesUnordered<_>>()
            .try_collect()
            .await
    }

    fn optimized_for(&self, optimization: StoreOptimizations) -> bool {
        matches!(optimization, StoreOptimizations::LazyExistenceOnSync)
    }

    async fn update(
        self: Pin<&Self>,
        digest: StoreKey<'_>,
        mut reader: DropCloserReadHalf,
        upload_size: UploadSizeInfo,
    ) -> Result<u64, Error> {
        if is_zero_digest(digest.borrow()) {
            return reader.recv().await.and_then(|should_be_empty| {
                if should_be_empty.is_empty() {
                    Ok(0)
                } else {
                    Err(make_err!(Code::Internal, "Zero byte hash not empty"))
                }
            });
        }

        let object_path = self.make_object_path(&digest);

        reader.set_max_recent_data_size(
            u64::try_from(self.max_retry_buffer_size)
                .err_tip(|| "Could not convert max_retry_buffer_size to u64")?,
        );

        // For small files with exact size, we'll use simple upload
        if let UploadSizeInfo::ExactSize(size) = upload_size
            && size < MIN_MULTIPART_SIZE
        {
            let content = reader.consume(Some(usize::try_from(size)?)).await?;
            let content_len = content.len() as u64;
            let client = &self.client;

            let written = self
                .retrier
                .retry(unfold(content, |content| async {
                    match client.write_object(&object_path, content.to_vec()).await {
                        Ok(()) => Some((RetryResult::Ok(content_len), content)),
                        Err(e) => Some((RetryResult::Retry(e), content)),
                    }
                }))
                .await?;
            self.touch_custom_time(&object_path, true);
            return Ok(written);
        }

        // For larger files, we'll use resumable upload
        // Stream and upload data in chunks
        let mut offset = 0u64;
        let mut total_size = if let UploadSizeInfo::ExactSize(size) = upload_size {
            Some(size)
        } else {
            None
        };
        let mut upload_id: Option<String> = None;
        let client = &self.client;

        loop {
            let chunk = reader.consume(Some(self.max_chunk_size)).await?;
            if chunk.is_empty() {
                break;
            }
            // If a full chunk wasn't read, then this is the full length.
            if chunk.len() < self.max_chunk_size {
                total_size = Some(offset + chunk.len() as u64);
            }

            let upload_id_ref = if let Some(upload_id_ref) = &upload_id {
                upload_id_ref
            } else {
                // Initiate the upload session on the first non-empty chunk.
                upload_id = Some(
                    self.retrier
                        .retry(unfold((), |()| async {
                            match client.start_resumable_write(&object_path).await {
                                Ok(id) => Some((RetryResult::Ok(id), ())),
                                Err(e) => Some((
                                    RetryResult::Retry(make_err!(
                                        Code::Aborted,
                                        "Failed to start resumable upload: {:?}",
                                        e
                                    )),
                                    (),
                                )),
                            }
                        }))
                        .await?,
                );
                upload_id.as_deref().unwrap()
            };

            let current_offset = offset;
            offset += chunk.len() as u64;

            // Uploading the chunk with a retry
            let object_path_ref = &object_path;
            self.retrier
                .retry(unfold(chunk, |chunk| async move {
                    match client
                        .upload_chunk(
                            upload_id_ref,
                            object_path_ref,
                            chunk.clone(),
                            current_offset,
                            offset,
                            total_size,
                        )
                        .await
                    {
                        Ok(()) => Some((RetryResult::Ok(()), chunk)),
                        Err(e) => Some((RetryResult::Retry(e), chunk)),
                    }
                }))
                .await?;
        }

        // Handle the case that the stream was of unknown length and
        // happened to be an exact multiple of chunk size.
        if let Some(upload_id_ref) = &upload_id {
            if total_size.is_none() {
                let object_path_ref = &object_path;
                self.retrier
                    .retry(unfold((), |()| async move {
                        match client
                            .upload_chunk(
                                upload_id_ref,
                                object_path_ref,
                                Bytes::new(),
                                offset,
                                offset,
                                Some(offset),
                            )
                            .await
                        {
                            Ok(()) => Some((RetryResult::Ok(offset), ())),
                            Err(e) => Some((RetryResult::Retry(e), ())),
                        }
                    }))
                    .await?;
            }
        } else {
            // Handle streamed empty file.
            let written = self
                .retrier
                .retry(unfold((), |()| async {
                    match client.write_object(&object_path, Vec::new()).await {
                        Ok(()) => Some((RetryResult::Ok(0), ())),
                        Err(e) => Some((RetryResult::Retry(e), ())),
                    }
                }))
                .await?;
            self.touch_custom_time(&object_path, true);
            return Ok(written);
        }

        // Verifying if the upload was successful
        self.retrier
            .retry(unfold((), |()| async {
                match client.object_exists(&object_path).await {
                    Ok(true) => Some((RetryResult::Ok(()), ())),
                    Ok(false) => Some((
                        RetryResult::Retry(make_err!(
                            Code::Internal,
                            "Object not found after upload completion"
                        )),
                        (),
                    )),
                    Err(e) => Some((RetryResult::Retry(e), ())),
                }
            }))
            .await?;

        self.touch_custom_time(&object_path, true);
        Ok(offset)
    }

    async fn get_part(
        self: Pin<&Self>,
        key: StoreKey<'_>,
        writer: &mut DropCloserWriteHalf,
        offset: u64,
        length: Option<u64>,
    ) -> Result<(), Error> {
        if is_zero_digest(key.borrow()) {
            writer.send_eof()?;
            return Ok(());
        }

        let object_path = self.make_object_path(&key);
        let end_offset = length.map(|len| offset + len);
        let client = &self.client;

        let object_path_ref = &object_path;
        self.retrier
            .retry(unfold(
                (offset, writer),
                |(mut offset, writer)| async move {
                    let mut stream = match client
                        .read_object_content(object_path_ref, offset, end_offset)
                        .await
                    {
                        Ok(stream) => stream,
                        Err(e) if e.code == Code::NotFound => {
                            return Some((RetryResult::Err(e), (offset, writer)));
                        }
                        Err(e) => return Some((RetryResult::Retry(e), (offset, writer))),
                    };

                    while let Some(next_chunk) = stream.next().await {
                        match next_chunk {
                            Ok(bytes) => {
                                // The GCS download stream can yield empty frames at
                                // chunk boundaries/end-of-body. `writer.send()` rejects
                                // empty buffers ("Cannot send EOF in send()"), and EOF is
                                // signalled explicitly after the loop, so skip empties.
                                if bytes.is_empty() {
                                    continue;
                                }
                                offset += bytes.len() as u64;
                                if let Err(err) = writer.send(bytes).await {
                                    return Some((RetryResult::Err(err), (offset, writer)));
                                }
                            }
                            Err(err) => return Some((RetryResult::Retry(err), (offset, writer))),
                        }
                    }

                    if let Err(err) = writer.send_eof() {
                        return Some((RetryResult::Err(err), (offset, writer)));
                    }

                    Some((RetryResult::Ok(()), (offset, writer)))
                },
            ))
            .await?;

        // Bump "recently last used" recency on a successful read (throttled).
        self.touch_custom_time(&object_path, false);
        Ok(())
    }

    fn inner_store(&self, _digest: Option<StoreKey>) -> &'_ dyn StoreDriver {
        self
    }

    fn as_any<'a>(&'a self) -> &'a (dyn core::any::Any + Sync + Send + 'static) {
        self
    }

    fn as_any_arc(self: Arc<Self>) -> Arc<dyn core::any::Any + Sync + Send + 'static> {
        self
    }

    fn register_health(self: Arc<Self>, registry: &mut HealthRegistryBuilder) {
        registry.register_indicator(self);
    }

    fn register_remove_callback(
        self: Arc<Self>,
        _callback: Arc<dyn RemoveItemCallback>,
    ) -> Result<(), Error> {
        // As we're backed by GCS, this store doesn't actually drop stuff
        // so we can actually just ignore this
        Ok(())
    }
}

#[async_trait]
impl<I, Client, NowFn> HealthStatusIndicator for GcsStore<Client, NowFn>
where
    I: InstantWrapper,
    Client: GcsOperations + 'static,
    NowFn: Fn() -> I + Send + Sync + Unpin + 'static,
{
    fn get_name(&self) -> &'static str {
        "GcsStore"
    }

    /// Lightweight probe: a single `object_exists` against a fixed
    /// never-existing path. Shares no resources with production traffic
    /// and stays well under the `HealthServer` per-indicator budget.
    async fn check_health(&self, _namespace: Cow<'static, str>) -> HealthStatus {
        const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

        let probe_path = ObjectPath::new(
            self.bucket.clone(),
            "__nativelink_health_probe__/does-not-exist",
        );

        let probe = self.client.object_exists(&probe_path);
        match timeout(HEALTH_PROBE_TIMEOUT, probe).await {
            Ok(Ok(_)) => HealthStatus::new_ok(self, "GcsStore::check_health: ok".into()),
            Ok(Err(e)) => {
                warn!(?e, "GcsStore::check_health: object_exists errored");
                HealthStatus::new_failed(
                    self,
                    format!("GcsStore::check_health: object_exists errored: {e}").into(),
                )
            }
            Err(_) => {
                warn!(
                    timeout_secs = HEALTH_PROBE_TIMEOUT.as_secs(),
                    "GcsStore::check_health: probe timed out",
                );
                HealthStatus::Timeout {
                    struct_name: self.struct_name(),
                }
            }
        }
    }
}
