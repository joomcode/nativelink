use core::pin::Pin;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use nativelink_error::Error;
use nativelink_metric::MetricsComponent;
use nativelink_util::buf_channel::{DropCloserReadHalf, DropCloserWriteHalf};
use nativelink_util::health_utils::{HealthStatus, HealthStatusIndicator};
use nativelink_util::metrics::{STORE_METRICS, StoreMetricAttrs, StoreType};
use nativelink_util::store_trait::{
    RemoveItemCallback, Store, StoreDriver, StoreKey, StoreLike, UploadSizeInfo,
};

#[derive(MetricsComponent, Debug)]
pub struct MetricsStore {
    inner: Arc<Store>,
    attrs: Arc<StoreMetricAttrs>,
}

impl MetricsStore {
    /// Eviction counts and store size are not recorded here. The store factory
    /// registers those as observable metrics, for every store that owns a
    /// cache instead of only for the filesystem store.
    #[must_use]
    pub fn new(inner: Arc<Store>, name: &str, store_type: StoreType) -> Arc<Self> {
        let attrs = Arc::new(StoreMetricAttrs::new_with_name(store_type, name));
        Arc::new(Self { inner, attrs })
    }
}

#[async_trait]
impl StoreDriver for MetricsStore {
    async fn has_with_results(
        self: Pin<&Self>,
        digests: &[StoreKey<'_>],
        results: &mut [Option<u64>],
    ) -> Result<(), Error> {
        let start = Instant::now();
        let result = self.inner.has_with_results(digests, results).await;
        let duration_ms = start.elapsed().as_millis();
        for res in results {
            if res.is_some() {
                STORE_METRICS
                    .store_operations
                    .add(1, self.attrs.cache_hit());
                STORE_METRICS
                    .store_operation_duration
                    .record(duration_ms as f64, self.attrs.cache_hit());
            } else {
                STORE_METRICS
                    .store_operations
                    .add(1, self.attrs.cache_miss());
                STORE_METRICS
                    .store_operation_duration
                    .record(duration_ms as f64, self.attrs.cache_miss());
            }
        }

        result
    }

    async fn update(
        self: Pin<&Self>,
        key: StoreKey<'_>,
        reader: DropCloserReadHalf,
        upload_size: UploadSizeInfo,
    ) -> Result<u64, Error> {
        let start = Instant::now();
        let result = self.inner.update(key, reader, upload_size).await;
        let duration_ms = start.elapsed().as_millis();
        if result.is_ok() {
            STORE_METRICS
                .store_operations
                .add(1, self.attrs.write_success());
            STORE_METRICS
                .store_operation_duration
                .record(duration_ms as f64, self.attrs.write_success());
        } else {
            STORE_METRICS
                .store_operations
                .add(1, self.attrs.write_error());
            STORE_METRICS
                .store_operation_duration
                .record(duration_ms as f64, self.attrs.write_error());
        }

        result
    }

    async fn get_part(
        self: Pin<&Self>,
        key: StoreKey<'_>,
        writer: &mut DropCloserWriteHalf,
        offset: u64,
        length: Option<u64>,
    ) -> Result<(), Error> {
        let start = Instant::now();
        let result = self.inner.get_part(key, writer, offset, length).await;
        let duration_ms = start.elapsed().as_millis();
        if result.is_ok() {
            STORE_METRICS
                .store_operations
                .add(1, self.attrs.read_success());
            STORE_METRICS
                .store_operation_duration
                .record(duration_ms as f64, self.attrs.read_success());
        } else {
            STORE_METRICS
                .store_operations
                .add(1, self.attrs.read_error());
            STORE_METRICS
                .store_operation_duration
                .record(duration_ms as f64, self.attrs.read_error());
        }

        result
    }

    fn inner_store(&self, digest: Option<StoreKey>) -> &'_ dyn StoreDriver {
        self.inner.inner_store(digest)
    }

    async fn post_init(self: Arc<Self>) -> Result<(), Error> {
        (*self.inner).clone().into_inner().post_init().await?;
        Ok(())
    }

    fn as_any<'a>(&'a self) -> &'a (dyn core::any::Any + Sync + Send + 'static) {
        self
    }

    fn as_any_arc(self: Arc<Self>) -> Arc<dyn core::any::Any + Sync + Send + 'static> {
        self
    }

    fn register_remove_callback(
        self: Arc<Self>,
        callback: Arc<dyn RemoveItemCallback>,
    ) -> Result<(), Error> {
        self.inner.clone().register_remove_callback(callback)
    }
}

#[async_trait]
impl HealthStatusIndicator for MetricsStore {
    fn get_name(&self) -> &'static str {
        "MetricsStore"
    }

    async fn check_health(&self, _namespace: Cow<'static, str>) -> HealthStatus {
        self.inner.check_health(_namespace).await
    }
}
