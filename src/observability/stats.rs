use super::{CollectionLabels, IndexMemoryLabels, MetricsCollector};
use crate::collection::CollectionRegistry;
use crate::index::IndexMemoryBreakdown;
use crate::persistence;
use crate::schema::IndexMode;
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Serialize)]
pub struct AdminStats {
    pub system: SystemStats,
    pub collections: Vec<CollectionStats>,
    pub queries: QueryStats,
    pub in_flight: InFlightStats,
}

/// Background work not yet durable. All three must be zero before a
/// host-driven stop is provably safe (see `/operations/quiesce`).
#[derive(Serialize)]
pub struct InFlightStats {
    pub index_builds: i64,
    pub index_saves: i64,
    pub ingest_sessions: i64,
}

#[derive(Serialize)]
pub struct SystemStats {
    pub uptime_seconds: u64,
    pub torque_version: String,
    pub memory: MemoryStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskStats>,
    pub cpu: CpuStats,
    pub ingest_server: IngestServerStats,
}

#[derive(Serialize)]
pub struct DiskStats {
    pub free_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Serialize)]
pub struct MemoryStats {
    pub allocated_bytes: i64,
    pub resident_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapped_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_bytes: Option<i64>,
}

#[derive(Serialize)]
pub struct CpuStats {
    pub cores_available: usize,
    pub rayon_threads: usize,
}

#[derive(Serialize)]
pub struct IngestServerStats {
    pub port: u16,
    pub active_sessions: i64,
}

#[derive(Serialize)]
pub struct CollectionStats {
    pub name: String,
    pub num_documents: u32,
    pub num_fields: usize,
    pub last_rebuilt_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<IndexMemoryBreakdown>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence: Option<PersistenceStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime: Option<crate::collection::RealtimeStats>,
}

#[derive(Serialize)]
pub struct PersistenceStats {
    pub enabled: bool,
    pub index_on_disk: bool,
    pub disk_size_bytes: u64,
    pub last_save_at: u64,
}

#[derive(Serialize)]
pub struct QueryStats {
    pub total: u64,
    pub latency_ms: LatencyStats,
}

#[derive(Serialize)]
pub struct LatencyStats {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn build_admin_stats(
    metrics: &MetricsCollector,
    collections: &CollectionRegistry,
    start_time: Instant,
    ingest_port: u16,
    data_dir: &Path,
) -> AdminStats {
    let uptime = start_time.elapsed().as_secs();

    let collection_stats: Vec<CollectionStats> = collections
        .list()
        .iter()
        .map(|c| {
            let bundle = c.snapshot_bundle();
            let memory = Some(bundle.memory_usage());
            let disk_size = persistence::index::index_disk_size(data_dir, &c.name);
            let persistence = Some(PersistenceStats {
                enabled: c.schema.persistence_enabled,
                index_on_disk: persistence::index::has_index(data_dir, &c.name),
                disk_size_bytes: disk_size,
                last_save_at: c.last_save_at.load(Ordering::Relaxed),
            });
            let realtime = (c.schema.index_mode == IndexMode::Realtime).then(|| c.realtime_stats());
            let num_documents = bundle.total_docs();
            CollectionStats {
                name: c.name.clone(),
                num_documents,
                num_fields: c.schema.fields.len(),
                last_rebuilt_at: c.last_rebuilt_at.load(Ordering::Relaxed),
                memory,
                persistence,
                realtime,
            }
        })
        .collect();

    let percentiles = metrics.latency_tracker.percentiles();

    // Read jemalloc stats fresh
    let (jemalloc_active, jemalloc_mapped, jemalloc_retained, jemalloc_metadata, jemalloc_allocated) = {
        #[cfg(feature = "jemalloc")]
        {
            tikv_jemalloc_ctl::epoch::advance().ok();
            crate::memory::purge_allocator();
            tikv_jemalloc_ctl::epoch::advance().ok();
            let allocated = tikv_jemalloc_ctl::stats::allocated::read().unwrap_or(0) as i64;
            let active = tikv_jemalloc_ctl::stats::active::read().unwrap_or(0) as i64;
            let mapped = tikv_jemalloc_ctl::stats::mapped::read().unwrap_or(0) as i64;
            let retained = tikv_jemalloc_ctl::stats::retained::read().unwrap_or(0) as i64;
            let metadata = tikv_jemalloc_ctl::stats::metadata::read().unwrap_or(0) as i64;
            (Some(active), Some(mapped), Some(retained), Some(metadata), allocated)
        }
        #[cfg(not(feature = "jemalloc"))]
        {
            (None, None, None, None, 0i64)
        }
    };

    let rss = crate::memory::allocated_bytes_fresh() as i64;

    let disk = {
        let (free, total) = crate::memory::disk_space();
        if total > 0 {
            Some(DiskStats { free_bytes: free, total_bytes: total })
        } else {
            None
        }
    };

    AdminStats {
        system: SystemStats {
            uptime_seconds: uptime,
            torque_version: env!("CARGO_PKG_VERSION").to_string(),
            memory: MemoryStats {
                allocated_bytes: jemalloc_allocated,
                resident_bytes: rss,
                active_bytes: jemalloc_active,
                mapped_bytes: jemalloc_mapped,
                retained_bytes: jemalloc_retained,
                metadata_bytes: jemalloc_metadata,
            },
            disk,
            cpu: CpuStats {
                cores_available: std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1),
                rayon_threads: rayon::current_num_threads(),
            },
            ingest_server: IngestServerStats {
                port: ingest_port,
                active_sessions: metrics.ingest_active_sessions.get(),
            },
        },
        collections: collection_stats,
        queries: QueryStats {
            total: metrics.queries_total.get(),
            latency_ms: LatencyStats {
                p50: percentiles.p50,
                p95: percentiles.p95,
                p99: percentiles.p99,
                max: percentiles.max,
            },
        },
        in_flight: InFlightStats {
            index_builds: metrics.index_builds_in_flight.get(),
            index_saves: metrics.index_saves_in_flight.get(),
            ingest_sessions: metrics.ingest_active_sessions.get(),
        },
    }
}

pub fn spawn_stats_collector(
    metrics: Arc<MetricsCollector>,
    collections: Arc<CollectionRegistry>,
) {
    std::thread::spawn(move || loop {
        refresh_metrics(&metrics, &collections);
        std::thread::sleep(Duration::from_secs(5));
    });
}

/// One pass of the stats collector: the memory gauges and the series of every collection.
pub fn refresh_metrics(metrics: &MetricsCollector, collections: &CollectionRegistry) {
    // Update memory gauges from real-time RSS reader
    let rss = crate::memory::allocated_bytes_fresh() as i64;
    metrics.memory_resident.set(rss);

    // On jemalloc targets, also report allocator-tracked "allocated" metric
    #[cfg(feature = "jemalloc")]
    {
        tikv_jemalloc_ctl::epoch::advance().ok();
        if let Ok(allocated) = tikv_jemalloc_ctl::stats::allocated::read() {
            metrics.memory_allocated.set(allocated as i64);
        }
    }

    // Update per-collection metrics
    for col in collections.list() {
        let index = col.snapshot();
        let doc_count = index.total_docs();
        let col_labels = CollectionLabels {
            collection: col.name.clone(),
        };

        metrics
            .collection_docs
            .get_or_create(&col_labels)
            .set(doc_count as i64);

        // Realtime: documents in memory, deleted copies awaiting a merge
        if col.schema.index_mode == IndexMode::Realtime {
            let rt = col.realtime_stats();
            metrics.memory_docs.get_or_create(&col_labels).set(i64::from(rt.memory_documents));
            metrics.deleted_docs.get_or_create(&col_labels).set(rt.deleted_documents as i64);
        }

        // 18f-5: Index memory component breakdown
        let mem = index.memory_usage();
        for (component, bytes) in [
            ("total", mem.total_bytes),
            ("docs", mem.docs_bytes),
            ("inverted", mem.inverted_bytes),
            ("value", mem.value_bytes),
            ("stored_docs", mem.stored_docs_bytes),
            ("geo", mem.geo_bytes),
            ("vector", mem.vector_bytes),
            ("all_docs_bitmap", mem.all_docs_bitmap_bytes),
        ] {
            metrics
                .index_memory_bytes
                .get_or_create(&IndexMemoryLabels {
                    collection: col.name.clone(),
                    component: component.into(),
                })
                .set(bytes as i64);
        }
    }
}
