pub mod prometheus;
pub mod stats;

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;
use std::collections::VecDeque;
use std::sync::Mutex;

pub struct MetricsCollector {
    pub registry: Registry,

    // Query metrics
    pub queries_total: Counter,
    pub query_duration: Histogram,

    // Collection metrics (labeled by collection name)
    pub collection_docs: Family<CollectionLabels, Gauge>,

    // Memory metrics
    pub memory_allocated: Gauge,
    pub memory_resident: Gauge,

    // Ingest metrics
    pub ingest_sessions_total: Family<IngestLabels, Counter>,
    pub ingest_duration: Family<CollectionLabels, Histogram>,
    pub ingest_active_sessions: Gauge,

    // In-flight background work (quiesce drains these before a stop)
    pub index_builds_in_flight: Gauge,
    pub index_saves_in_flight: Gauge,

    // HTTP error metrics
    pub http_errors_total: Family<StatusLabels, Counter>,

    // Per-collection query metrics (18f-2)
    pub query_collection_total: Family<QueryLabels, Counter>,
    pub query_collection_duration: Family<QueryLabels, Histogram>,

    // Connection metrics (18f-3)
    pub tcp_connections_total: Counter,
    pub http_requests_total: Family<HttpRequestLabels, Counter>,

    // Ingest error metrics (18f-4)
    pub ingest_parse_errors_total: Family<CollectionLabels, Counter>,
    pub ingest_schema_mismatches_total: Family<CollectionLabels, Counter>,

    // Index memory component metrics (18f-5)
    pub index_memory_bytes: Family<IndexMemoryLabels, Gauge>,

    // GPU VRAM metrics (18f-1)
    pub gpu_vram_total_bytes: Gauge,
    pub gpu_vram_allocated_bytes: Gauge,

    // Cumulative docs indexed counter
    pub ingest_docs_total: Family<CollectionLabels, Counter>,

    // Bitmap compression metrics (18f-6)
    pub bitmap_dense_count: Family<CollectionLabels, Gauge>,
    pub bitmap_sparse_count: Family<CollectionLabels, Gauge>,
    pub bitmap_dense_bytes: Family<CollectionLabels, Gauge>,
    pub bitmap_sparse_bytes: Family<CollectionLabels, Gauge>,

    // Realtime metrics
    pub backpressure_events_total: Counter,
    pub compaction_duration: Family<CollectionLabels, Histogram>,
    pub compaction_total: Family<CollectionLabels, Counter>,
    pub memory_docs: Family<CollectionLabels, Gauge>,
    pub deleted_docs: Family<CollectionLabels, Gauge>,

    // Persistence timing metrics
    pub persistence_save_duration: Family<CollectionLabels, Histogram>,
    pub persistence_load_duration: Family<CollectionLabels, Histogram>,

    // Latency tracker for /admin/stats percentiles
    pub latency_tracker: LatencyTracker,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct CollectionLabels {
    pub collection: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct IngestLabels {
    pub collection: String,
    pub status: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct StatusLabels {
    pub status_code: u16,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct QueryLabels {
    pub collection: String,
    pub query_type: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct HttpRequestLabels {
    pub method: String,
    pub path: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct IndexMemoryLabels {
    pub collection: String,
    pub component: String,
}

impl MetricsCollector {
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let queries_total = Counter::default();
        registry.register("torque_queries", "Total queries served", queries_total.clone());

        let query_duration = Histogram::new(
            [0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1].into_iter(),
        );
        registry.register(
            "torque_query_duration_seconds",
            "Query latency in seconds",
            query_duration.clone(),
        );

        let collection_docs = Family::<CollectionLabels, Gauge>::default();
        registry.register(
            "torque_collection_documents",
            "Documents per collection",
            collection_docs.clone(),
        );

        let memory_allocated = Gauge::default();
        registry.register(
            "torque_memory_allocated_bytes",
            "jemalloc allocated bytes",
            memory_allocated.clone(),
        );

        let memory_resident = Gauge::default();
        registry.register(
            "torque_memory_resident_bytes",
            "jemalloc resident bytes",
            memory_resident.clone(),
        );

        let ingest_sessions_total = Family::<IngestLabels, Counter>::default();
        registry.register(
            "torque_ingest_sessions",
            "Total ingest sessions by status",
            ingest_sessions_total.clone(),
        );

        let ingest_duration = Family::<CollectionLabels, Histogram>::new_with_constructor(|| {
            Histogram::new([0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0].into_iter())
        });
        registry.register(
            "torque_ingest_duration_seconds",
            "Ingest build duration in seconds",
            ingest_duration.clone(),
        );

        let ingest_active_sessions = Gauge::default();
        registry.register(
            "torque_ingest_active_sessions",
            "Currently active ingest sessions",
            ingest_active_sessions.clone(),
        );

        let index_builds_in_flight = Gauge::default();
        registry.register(
            "torque_index_builds_in_flight",
            "Async index builds currently running (import acked, index not yet swapped)",
            index_builds_in_flight.clone(),
        );

        let index_saves_in_flight = Gauge::default();
        registry.register(
            "torque_index_saves_in_flight",
            "Background index saves currently running (index swapped, not yet durable)",
            index_saves_in_flight.clone(),
        );

        let http_errors_total = Family::<StatusLabels, Counter>::default();
        registry.register(
            "torque_http_errors",
            "HTTP error responses by status code",
            http_errors_total.clone(),
        );

        // 18f-2: Per-collection query metrics
        let query_collection_total = Family::<QueryLabels, Counter>::default();
        registry.register(
            "torque_query_collection",
            "Queries per collection and type",
            query_collection_total.clone(),
        );

        let query_collection_duration =
            Family::<QueryLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(
                    [0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1].into_iter(),
                )
            });
        registry.register(
            "torque_query_collection_duration_seconds",
            "Query latency per collection and type",
            query_collection_duration.clone(),
        );

        // 18f-3: Connection metrics
        let tcp_connections_total = Counter::default();
        registry.register(
            "torque_tcp_connections",
            "Total TCP ingest connections accepted",
            tcp_connections_total.clone(),
        );

        let http_requests_total = Family::<HttpRequestLabels, Counter>::default();
        registry.register(
            "torque_http_requests",
            "HTTP requests by method and path pattern",
            http_requests_total.clone(),
        );

        // 18f-4: Ingest error metrics
        let ingest_parse_errors_total = Family::<CollectionLabels, Counter>::default();
        registry.register(
            "torque_ingest_parse_errors",
            "Ingest document parse errors by collection",
            ingest_parse_errors_total.clone(),
        );

        let ingest_schema_mismatches_total = Family::<CollectionLabels, Counter>::default();
        registry.register(
            "torque_ingest_schema_mismatches",
            "Ingest schema mismatch errors by collection",
            ingest_schema_mismatches_total.clone(),
        );

        // 18f-5: Index memory component metrics
        let index_memory_bytes = Family::<IndexMemoryLabels, Gauge>::default();
        registry.register(
            "torque_index_memory_bytes",
            "Index memory per collection and component",
            index_memory_bytes.clone(),
        );

        // 18f-1: GPU VRAM metrics
        let gpu_vram_total_bytes = Gauge::default();
        registry.register(
            "torque_gpu_vram_total_bytes",
            "Total GPU VRAM in bytes",
            gpu_vram_total_bytes.clone(),
        );

        let gpu_vram_allocated_bytes = Gauge::default();
        registry.register(
            "torque_gpu_vram_allocated_bytes",
            "GPU VRAM allocated for dense bitmaps",
            gpu_vram_allocated_bytes.clone(),
        );

        // Cumulative docs indexed
        let ingest_docs_total = Family::<CollectionLabels, Counter>::default();
        registry.register(
            "torque_ingest_docs_total",
            "Total documents indexed cumulatively",
            ingest_docs_total.clone(),
        );

        // 18f-6: Bitmap compression metrics
        let bitmap_dense_count = Family::<CollectionLabels, Gauge>::default();
        registry.register(
            "torque_bitmap_dense_count",
            "Dense bitmap terms per collection (GPU)",
            bitmap_dense_count.clone(),
        );

        let bitmap_sparse_count = Family::<CollectionLabels, Gauge>::default();
        registry.register(
            "torque_bitmap_sparse_count",
            "Sparse bitmap terms per collection (CPU)",
            bitmap_sparse_count.clone(),
        );

        let bitmap_dense_bytes = Family::<CollectionLabels, Gauge>::default();
        registry.register(
            "torque_bitmap_dense_bytes",
            "Dense bitmap bytes per collection",
            bitmap_dense_bytes.clone(),
        );

        let bitmap_sparse_bytes = Family::<CollectionLabels, Gauge>::default();
        registry.register(
            "torque_bitmap_sparse_bytes",
            "Sparse bitmap bytes per collection",
            bitmap_sparse_bytes.clone(),
        );

        // Realtime metrics
        let backpressure_events_total = Counter::default();
        registry.register(
            "torque_backpressure_events_total",
            "Total backpressure events returned to clients",
            backpressure_events_total.clone(),
        );

        let compaction_duration =
            Family::<CollectionLabels, Histogram>::new_with_constructor(|| {
                Histogram::new([0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0].into_iter())
            });
        registry.register(
            "torque_compaction_duration_seconds",
            "Realtime flush duration in seconds",
            compaction_duration.clone(),
        );

        let compaction_total = Family::<CollectionLabels, Counter>::default();
        registry.register(
            "torque_compaction_total",
            "Total flushes of a realtime collection's segments in memory, per collection",
            compaction_total.clone(),
        );

        let memory_docs = Family::<CollectionLabels, Gauge>::default();
        registry.register(
            "torque_memory_documents",
            "Documents in a realtime collection's segments in memory, not yet flushed",
            memory_docs.clone(),
        );

        let deleted_docs = Family::<CollectionLabels, Gauge>::default();
        registry.register(
            "torque_deleted_documents",
            "Copies of documents deleted or written again, still in their segments until a merge",
            deleted_docs.clone(),
        );

        let persistence_save_duration =
            Family::<CollectionLabels, Histogram>::new_with_constructor(|| {
                Histogram::new([0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0].into_iter())
            });
        registry.register(
            "torque_persistence_save_duration_seconds",
            "Index persistence save duration in seconds",
            persistence_save_duration.clone(),
        );

        let persistence_load_duration =
            Family::<CollectionLabels, Histogram>::new_with_constructor(|| {
                Histogram::new([0.1, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0].into_iter())
            });
        registry.register(
            "torque_persistence_load_duration_seconds",
            "Index persistence load duration in seconds",
            persistence_load_duration.clone(),
        );

        Self {
            registry,
            queries_total,
            query_duration,
            collection_docs,
            memory_allocated,
            memory_resident,
            ingest_sessions_total,
            ingest_duration,
            ingest_active_sessions,
            index_builds_in_flight,
            index_saves_in_flight,
            http_errors_total,
            query_collection_total,
            query_collection_duration,
            tcp_connections_total,
            http_requests_total,
            ingest_parse_errors_total,
            ingest_schema_mismatches_total,
            index_memory_bytes,
            gpu_vram_total_bytes,
            gpu_vram_allocated_bytes,
            ingest_docs_total,
            bitmap_dense_count,
            bitmap_sparse_count,
            bitmap_dense_bytes,
            bitmap_sparse_bytes,
            backpressure_events_total,
            compaction_duration,
            compaction_total,
            memory_docs,
            deleted_docs,
            persistence_save_duration,
            persistence_load_duration,
            latency_tracker: LatencyTracker::new(10_000),
        }
    }

    /// Drop every series of a deleted collection. A nightly upload makes a new collection each
    /// time, so the series of the ones it replaces would otherwise pile up in /metrics.
    pub fn remove_collection(&self, collection: &str) {
        let labels = CollectionLabels { collection: collection.into() };
        for family in [
            &self.collection_docs,
            &self.bitmap_dense_count,
            &self.bitmap_sparse_count,
            &self.bitmap_dense_bytes,
            &self.bitmap_sparse_bytes,
            &self.memory_docs,
            &self.deleted_docs,
        ] {
            family.remove(&labels);
        }
        for family in [
            &self.ingest_parse_errors_total,
            &self.ingest_schema_mismatches_total,
            &self.ingest_docs_total,
            &self.compaction_total,
        ] {
            family.remove(&labels);
        }
        for family in [
            &self.ingest_duration,
            &self.compaction_duration,
            &self.persistence_save_duration,
            &self.persistence_load_duration,
        ] {
            family.remove(&labels);
        }
        for status in ["success", "error"] {
            self.ingest_sessions_total.remove(&IngestLabels { collection: collection.into(), status: status.into() });
        }
        // the query types of `classify_query_type` (server::routes)
        for query_type in ["text", "vector", "hybrid", "filter_only"] {
            let labels = QueryLabels { collection: collection.into(), query_type: query_type.into() };
            self.query_collection_total.remove(&labels);
            self.query_collection_duration.remove(&labels);
        }
        // the components of `stats::refresh_metrics`
        for component in ["total", "docs", "inverted", "value", "stored_docs", "geo", "vector", "all_docs_bitmap"] {
            self.index_memory_bytes.remove(&IndexMemoryLabels { collection: collection.into(), component: component.into() });
        }
    }
}

/// Increments `gauge` now, decrements when dropped. Move it into a spawned
/// task so the in-flight count survives panics and early returns.
pub struct InFlightGuard(Gauge);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.dec();
    }
}

pub fn track_in_flight(gauge: &Gauge) -> InFlightGuard {
    gauge.inc();
    InFlightGuard(gauge.clone())
}

pub struct LatencyTracker {
    window: Mutex<VecDeque<f64>>,
    max_window: usize,
}

pub struct LatencyPercentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl LatencyTracker {
    pub fn new(max_window: usize) -> Self {
        Self {
            window: Mutex::new(VecDeque::with_capacity(max_window)),
            max_window,
        }
    }

    pub fn record(&self, duration_ms: f64) {
        let mut w = self.window.lock().unwrap();
        if w.len() >= self.max_window {
            w.pop_front();
        }
        w.push_back(duration_ms);
    }

    pub fn percentiles(&self) -> LatencyPercentiles {
        let w = self.window.lock().unwrap();
        if w.is_empty() {
            return LatencyPercentiles {
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                max: 0.0,
            };
        }

        let mut sorted: Vec<f64> = w.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let len = sorted.len();
        let p50 = sorted[(len as f64 * 0.50) as usize];
        let p95 = sorted[((len as f64 * 0.95) as usize).min(len - 1)];
        let p99 = sorted[((len as f64 * 0.99) as usize).min(len - 1)];
        let max = sorted[len - 1];

        LatencyPercentiles { p50, p95, p99, max }
    }

    pub fn count(&self) -> usize {
        self.window.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_collector_creates_successfully() {
        let mc = MetricsCollector::new();
        mc.queries_total.inc();
        mc.queries_total.inc();
        assert_eq!(mc.queries_total.get(), 2);
    }

    #[test]
    fn histogram_observe() {
        let mc = MetricsCollector::new();
        mc.query_duration.observe(0.003);
        mc.query_duration.observe(0.007);
        // Just verify no panic
    }

    #[test]
    fn collection_docs_family() {
        let mc = MetricsCollector::new();
        mc.collection_docs
            .get_or_create(&CollectionLabels {
                collection: "products".into(),
            })
            .set(1500);
        let val = mc
            .collection_docs
            .get_or_create(&CollectionLabels {
                collection: "products".into(),
            })
            .get();
        assert_eq!(val, 1500);
    }

    #[test]
    fn latency_tracker_empty() {
        let lt = LatencyTracker::new(100);
        let p = lt.percentiles();
        assert_eq!(p.p50, 0.0);
        assert_eq!(p.max, 0.0);
    }

    #[test]
    fn latency_tracker_percentiles() {
        let lt = LatencyTracker::new(1000);
        // Insert 100 values: 1.0, 2.0, ..., 100.0
        for i in 1..=100 {
            lt.record(i as f64);
        }
        let p = lt.percentiles();
        assert!((p.p50 - 50.0).abs() < 2.0, "p50 should be ~50, got {}", p.p50);
        assert!((p.p95 - 95.0).abs() < 2.0, "p95 should be ~95, got {}", p.p95);
        assert!((p.p99 - 99.0).abs() < 2.0, "p99 should be ~99, got {}", p.p99);
        assert_eq!(p.max, 100.0);
    }

    #[test]
    fn latency_tracker_rolling_window() {
        let lt = LatencyTracker::new(5);
        for i in 1..=10 {
            lt.record(i as f64);
        }
        // Should only keep last 5: 6, 7, 8, 9, 10
        assert_eq!(lt.count(), 5);
        let p = lt.percentiles();
        assert_eq!(p.max, 10.0);
        // Min should be 6
        assert!((p.p50 - 8.0).abs() < 1.0);
    }

    #[test]
    fn ingest_labels_family() {
        let mc = MetricsCollector::new();
        mc.ingest_sessions_total
            .get_or_create(&IngestLabels {
                collection: "test".into(),
                status: "success".into(),
            })
            .inc();
        let val = mc
            .ingest_sessions_total
            .get_or_create(&IngestLabels {
                collection: "test".into(),
                status: "success".into(),
            })
            .get();
        assert_eq!(val, 1);
    }

    #[test]
    fn http_errors_family() {
        let mc = MetricsCollector::new();
        mc.http_errors_total
            .get_or_create(&StatusLabels { status_code: 404 })
            .inc();
        mc.http_errors_total
            .get_or_create(&StatusLabels { status_code: 404 })
            .inc();
        mc.http_errors_total
            .get_or_create(&StatusLabels { status_code: 500 })
            .inc();
        let val_404 = mc
            .http_errors_total
            .get_or_create(&StatusLabels { status_code: 404 })
            .get();
        let val_500 = mc
            .http_errors_total
            .get_or_create(&StatusLabels { status_code: 500 })
            .get();
        assert_eq!(val_404, 2);
        assert_eq!(val_500, 1);
    }
}
