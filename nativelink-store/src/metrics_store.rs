use async_trait::async_trait;
use nativelink_error::Error;
use nativelink_metric::MetricsComponent;
use nativelink_util::buf_channel::{DropCloserReadHalf, DropCloserWriteHalf};
use nativelink_util::health_utils::{HealthStatus, HealthStatusIndicator};
use nativelink_util::metrics::{StoreMetricAttrs, StoreType, STORE_METRICS};
use nativelink_util::store_trait::{
    RemoveItemCallback, Store, StoreDriver, StoreKey, StoreLike, UploadSizeInfo,
};
use std::borrow::Cow;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

#[derive(MetricsComponent, Debug)]
pub struct MetricsStore {
    inner: Arc<Store>,
    attrs: StoreMetricAttrs,
}

impl MetricsStore {
    #[must_use]
    pub fn new(inner: Arc<Store>, name: &str, store_type: StoreType) -> Arc<Self> {
        Arc::new(Self {
            inner: inner.clone(),
            attrs: StoreMetricAttrs::new_with_name(store_type, name),
        })
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
                    .add(1, &self.attrs.cache_hit());
                STORE_METRICS
                    .store_operation_duration
                    .record(duration_ms as f64, &self.attrs.cache_hit());
            } else {
                STORE_METRICS
                    .store_operations
                    .add(1, &self.attrs.cache_miss());
                STORE_METRICS
                    .store_operation_duration
                    .record(duration_ms as f64, &self.attrs.cache_miss());
            }
        }

        result
    }

    async fn update(
        self: Pin<&Self>,
        key: StoreKey<'_>,
        reader: DropCloserReadHalf,
        upload_size: UploadSizeInfo,
    ) -> Result<(), Error> {
        let start = Instant::now();
        let result = self.inner.update(key, reader, upload_size).await;
        let duration_ms = start.elapsed().as_millis();
        if result.is_ok() {
            STORE_METRICS
                .store_operations
                .add(1, &self.attrs.write_success());
            STORE_METRICS
                .store_operation_duration
                .record(duration_ms as f64, &self.attrs.write_success());
        } else {
            STORE_METRICS.store_operations.add(1, &self.attrs.write_error());
            STORE_METRICS
                .store_operation_duration
                .record(duration_ms as f64, &self.attrs.write_error());
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
                .add(1, &self.attrs.read_success());
            STORE_METRICS
                .store_operation_duration
                .record(duration_ms as f64, &self.attrs.read_success());
        } else {
            STORE_METRICS
                .store_operations
                .add(1, &self.attrs.read_error());
            STORE_METRICS
                .store_operation_duration
                .record(duration_ms as f64, &self.attrs.read_error());
        }

        result
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
