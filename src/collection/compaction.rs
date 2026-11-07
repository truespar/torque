use std::sync::Arc;
use std::time::{Duration, Instant};

use rustc_hash::FxHashSet;

use crate::collection::{Collection, CollectionRegistry};
use crate::config::Config;
use crate::index::IndexBundle;
use crate::observability::{CollectionLabels, MetricsCollector};
use crate::schema::IndexMode;

/// Trigger segment compaction once a collection's `segment_count` exceeds
/// this threshold. The streaming builder produces one segment per 500K
/// docs, so 8 segments == 4M docs — a reasonable point to start
/// rewriting into fewer larger segments to bound the per-query fan-out
/// cost. Below this, fan-out overhead is negligible compared to the
/// per-segment scoring work and compaction is pure churn.
pub const SEGMENT_COMPACTION_THRESHOLD: usize = 8;

/// Hard cap on the total doc count we'll attempt to compact in one pass.
/// `compact_collection_segments` holds the merged segments' sections and the
/// decompressed blocks of the stored documents it compresses again in memory.
/// For collections far above this size, compaction is skipped: the per-query
/// fan-out cost is preferred over a pass that might run out of memory.
pub const SEGMENT_COMPACTION_MAX_DOCS: usize = 5_000_000;

/// Byte budget for a single merged segment's stored records (uncompressed).
///
/// A few TQBF section offsets are u32 (GEO leaf offsets, HNSW edge counts), so a
/// merged segment's records are capped at 2 GiB. Compaction packs documents into
/// as few segments as fit this budget instead of always merging into one.
pub const SEGMENT_COMPACTION_TARGET_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Spawn a background compaction scheduler that periodically compacts
/// realtime collections based on time interval and/or memory pressure.
pub fn spawn_compaction_scheduler(
    collections: Arc<CollectionRegistry>,
    metrics: Arc<MetricsCollector>,
    config: Arc<Config>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {

    tokio::spawn(async move {
        let interval_secs = config.compaction_interval_secs;
        let mem_threshold_pct = config.compaction_memory_threshold_percent;

        // If both disabled, exit immediately
        if interval_secs == 0 && mem_threshold_pct == 0 {
            tracing::info!("Compaction scheduler disabled (interval=0, memory_threshold=0)");
            return;
        }

        // Tick every min(interval, 30) seconds for responsive memory checks
        let tick_secs = if interval_secs > 0 {
            interval_secs.min(30)
        } else {
            30
        };
        let mut ticker = tokio::time::interval(Duration::from_secs(tick_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut last_scheduled = Instant::now();
        let mut last_memory = Instant::now();

        tracing::info!(
            interval_secs,
            mem_threshold_pct,
            tick_secs,
            "Compaction scheduler started"
        );

        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Compaction scheduler shutting down");
                        return;
                    }
                }
            }

            // Memory pressure compaction
            if mem_threshold_pct > 0 && last_memory.elapsed() > Duration::from_secs(60) {
                let resident = metrics.memory_resident.get();
                let threshold = config.max_memory_bytes as i64 * mem_threshold_pct as i64 / 100;
                if resident > threshold {
                    let compacted = flush_all_blocking(&collections, &metrics, &config).await;
                    if compacted > 0 {
                        tracing::info!(
                            resident_mb = resident / (1024 * 1024),
                            threshold_mb = threshold / (1024 * 1024),
                            collections_compacted = compacted,
                            "Memory pressure compaction"
                        );
                    }
                    last_memory = Instant::now();
                }
            }

            // Scheduled compaction: realtime collections flush their segments
            // in memory, then batch collections with too many segments merge.
            if interval_secs > 0 && last_scheduled.elapsed() >= Duration::from_secs(interval_secs) {
                let compacted = flush_all_blocking(&collections, &metrics, &config).await;
                if compacted > 0 {
                    tracing::info!(
                        collections_compacted = compacted,
                        "Scheduled realtime flush"
                    );
                }
                let segments_compacted = compact_all_segments(&collections, &metrics);
                if segments_compacted > 0 {
                    tracing::info!(
                        collections_compacted = segments_compacted,
                        "Scheduled segment compaction"
                    );
                }
                last_scheduled = Instant::now();
            }
        }
    })
}

/// `compact_all_realtime` off the scheduler's task (its flushes build and save).
async fn flush_all_blocking(collections: &Arc<CollectionRegistry>, metrics: &Arc<MetricsCollector>, config: &Arc<Config>) -> usize {
    let (c, m, cfg) = (Arc::clone(collections), Arc::clone(metrics), Arc::clone(config));
    tokio::task::spawn_blocking(move || compact_all_realtime(&c, &m, &cfg)).await.unwrap_or(0)
}

/// Result of a segment compaction pass on a single collection.
#[derive(Debug, Clone, Copy)]
pub struct SegmentCompactionResult {
    pub original_segments: usize,
    pub new_segments: usize,
    pub docs_processed: u32,
    pub elapsed_ms: u64,
}

/// Walk every collection and rewrite over-fragmented bundles into fewer
/// segments. Returns the number of collections that were actually compacted.
fn compact_all_segments(
    collections: &CollectionRegistry,
    metrics: &MetricsCollector,
) -> usize {
    let mut compacted = 0usize;
    for col in collections.list() {
        match compact_collection_segments(&col) {
            Ok(Some(result)) => {
                tracing::info!(
                    collection = col.name.as_str(),
                    original_segments = result.original_segments,
                    new_segments = result.new_segments,
                    docs = result.docs_processed,
                    elapsed_ms = result.elapsed_ms,
                    "Segment compaction"
                );
                let labels = CollectionLabels {
                    collection: col.name.clone(),
                };
                metrics.compaction_total.get_or_create(&labels).inc();
                metrics
                    .compaction_duration
                    .get_or_create(&labels)
                    .observe(result.elapsed_ms as f64 / 1000.0);

                // Persist the new (compacted) bundle to disk so the next
                // restart loads the merged form, not the old fragmented one.
                if col.schema.persistence_enabled {
                    if let Some(ref data_dir) = collections.data_dir {
                        let data_dir = data_dir.clone();
                        let col_ref = Arc::clone(&col);
                        std::thread::spawn(move || {
                            if let Err(e) = col_ref.save(&data_dir) {
                                tracing::error!(
                                    collection = col_ref.name.as_str(),
                                    error = %e,
                                    "Failed to persist bundle after segment compaction"
                                );
                            }
                        });
                    }
                }

                compacted += 1;
            }
            Ok(None) => {} // skipped — too few segments or too many docs
            Err(e) => {
                tracing::warn!(
                    collection = col.name.as_str(),
                    error = %e,
                    "Segment compaction failed"
                );
            }
        }
    }
    if compacted > 0 {
        crate::memory::purge_allocator();
    }
    compacted
}

/// The uncompressed bytes of a segment's stored records per document: what packs documents into
/// merged segments that stay under `SEGMENT_COMPACTION_TARGET_BYTES` (their sections grow with
/// them).
fn record_bytes_per_doc(seg: &crate::index::Segment) -> usize {
    let Some(r) = seg.doc_store.records() else { return 0 };
    let d = r.docs();
    (0..d.block_count()).map(|b| d.block_raw_len(b)).sum::<usize>() / (seg.total_docs().max(1) as usize)
}

/// Compact a single collection's segments into fewer (larger) ones. Returns `None` when the
/// collection's bundle does not need compaction (segment count below
/// `SEGMENT_COMPACTION_THRESHOLD`, doc count above `SEGMENT_COMPACTION_MAX_DOCS`, or the byte
/// budget cannot produce fewer segments than it already has).
///
/// The live documents, in `_seq_id` order, are packed into groups of at most
/// `SEGMENT_COMPACTION_TARGET_BYTES` of records each; every group is merged from the segments'
/// sections (`index::merge`: no document parsed or indexed again), and the resulting bundle is
/// swapped into the collection.
///
/// The rebuilt segments are built in memory; the save that follows writes them, and puts their
/// files, mapped, in their place (`Collection::save`).
pub fn compact_collection_segments(col: &Collection) -> Result<Option<SegmentCompactionResult>, String> {
    compact_collection_segments_with_target(col, SEGMENT_COMPACTION_TARGET_BYTES)
}

fn compact_collection_segments_with_target(
    col: &Collection,
    target_bytes: usize,
) -> Result<Option<SegmentCompactionResult>, String> {
    // Hold the publish right for the WHOLE snapshot-merge-swap sequence. This
    // pass reads the bundle, merges (seconds for a large collection), then
    // blind-stores the result, discarding any publish inside that window (the
    // messages_vec shortfall once seen). Skipping a merge is free: it is an
    // optimisation, and the next scheduler tick retries.
    // A realtime collection's segments merge by tier as they flush (`Collection::merge_flushed`),
    // keeping every `_seq_id`, while writes go on: this pass renumbers and stores blindly.
    if col.schema.index_mode == IndexMode::Realtime {
        return Ok(None);
    }
    let Some(_publish_guard) = col.try_begin_publish() else {
        tracing::debug!(
            collection = col.name.as_str(),
            "skipping segment compaction: another publisher holds the collection"
        );
        return Ok(None);
    };
    let bundle = col.snapshot_bundle();
    let original_segments = bundle.segment_count();
    if original_segments < SEGMENT_COMPACTION_THRESHOLD {
        return Ok(None);
    }
    let total_docs = bundle.total_docs();
    if total_docs as usize > SEGMENT_COMPACTION_MAX_DOCS {
        tracing::debug!(
            collection = col.name.as_str(),
            segments = original_segments,
            docs = total_docs,
            cap = SEGMENT_COMPACTION_MAX_DOCS,
            "skipping segment compaction: collection larger than the safe rebuild cap"
        );
        return Ok(None);
    }

    let start = Instant::now();

    // The live documents in the order of their `_seq_id`s, which the merged segments hold them in
    // (fresh sequential ids per group). A deleted document, and every older copy of a document
    // written again, is in its segment's deletion mask and stays out: carried over, it would stop
    // being shadowed and come back.
    let sources: Vec<&crate::index::Segment> = bundle.segments.iter().map(|s| &**s).collect();
    let mut live: Vec<(u64, u32, crate::schema::DocId)> = Vec::with_capacity(bundle.total_docs() as usize);
    for (s, seg) in sources.iter().enumerate() {
        let deleted = bundle.deleted_for(s).filter(|d| !d.is_empty());
        live.extend((0..seg.total_docs()).filter(|&d| !deleted.is_some_and(|m| m.contains(d))).map(|d| (seg.seq_id(d), s as u32, d)));
    }
    live.sort_unstable();
    // The same external_id lives unmasked in several segments only in a bundle put together
    // without masks. Merging is where those copies have to collapse, or each becomes a permanently
    // live duplicate: the first in `_seq_id` order stays (the union of the segments' IDS finds
    // them).
    let mut superseded: FxHashSet<(u32, crate::schema::DocId)> = FxHashSet::default();
    {
        use fst::Streamer;
        let stores: Vec<(usize, &crate::tqbf::segment::RecordStore)> = sources.iter().enumerate().filter_map(|(s, seg)| Some((s, &**seg.doc_store.records()?))).collect();
        let mut op = fst::map::OpBuilder::new();
        for (_, r) in &stores {
            op = op.add(r.ids().map());
        }
        let mut union = op.union();
        while let Some((_, ivs)) = union.next() {
            if ivs.len() < 2 {
                continue;
            }
            let mut copies: Vec<(u64, u32, crate::schema::DocId)> = ivs
                .iter()
                .map(|iv| (stores[iv.index].0, iv.value as crate::schema::DocId))
                .filter(|&(s, d)| !bundle.deleted_for(s).is_some_and(|m| m.contains(d)))
                .map(|(s, d)| (sources[s].seq_id(d), s as u32, d))
                .collect();
            if copies.len() > 1 {
                copies.sort_unstable();
                superseded.extend(copies[1..].iter().map(|&(_, s, d)| (s, d)));
            }
        }
    }
    if !superseded.is_empty() {
        live.retain(|&(_, s, d)| !superseded.contains(&(s, d)));
        tracing::info!(
            collection = col.name.as_str(),
            segments = original_segments,
            docs = total_docs,
            deduped = superseded.len(),
            "segment compaction dropped superseded document versions"
        );
    }
    let lsn = bundle.lsn;

    // Greedy sequential packing: cut a new group whenever adding the next doc would push the
    // current group past the byte budget. Group boundaries are end indexes into `live`.
    let per_doc: Vec<usize> = sources.iter().map(|s| record_bytes_per_doc(s)).collect();
    let mut group_ends: Vec<usize> = Vec::new();
    let mut group_size = 0usize;
    for (i, &(_, s, _)) in live.iter().enumerate() {
        let bytes = per_doc[s as usize];
        if group_size > 0 && group_size + bytes > target_bytes {
            group_ends.push(i);
            group_size = 0;
        }
        group_size += bytes;
    }
    group_ends.push(live.len());

    if group_ends.len() >= original_segments {
        tracing::debug!(
            collection = col.name.as_str(),
            segments = original_segments,
            docs = total_docs,
            groups = group_ends.len(),
            "skipping segment compaction: byte budget cannot reduce segment count"
        );
        return Ok(None);
    }

    // One segment merged per group, from every segment's sections: a group takes its documents
    // only (`index::merge`). Fresh sequential ids: the groups' documents are in `_seq_id` order.
    let dictionary = crate::collection::write::dictionary(&bundle);
    let mut segments: Vec<Arc<crate::index::Segment>> = Vec::with_capacity(group_ends.len());
    let mut base_doc_id: u32 = 0;
    let mut from = 0;
    for &end in &group_ends {
        let order: Vec<(u32, crate::schema::DocId)> = live[from..end].iter().map(|&(_, s, d)| (s, d)).collect();
        let mut seg = crate::index::merge::merge(&col.schema, &sources, &order, dictionary).map_err(|e| format!("segment compaction of `{}`: {e}", col.name))?;
        seg.base_doc_id = base_doc_id;
        base_doc_id += seg.total_docs();
        segments.push(Arc::new(seg));
        from = end;
    }
    drop(sources);
    drop(bundle);

    let new_segment_count = segments.len();
    let deleted_per_segment = (0..new_segment_count)
        .map(|_| Arc::new(crate::bitmap::Bitmap::new()))
        .collect();
    let new_bundle = IndexBundle {
        schema: Arc::clone(&col.schema),
        total_docs: segments.iter().map(|s| s.total_docs()).sum(),
        segments,
        deleted_per_segment,
        lsn,
        next_seq: 0,
        memory: 0,
    };

    col.swap_bundle(new_bundle);

    Ok(Some(SegmentCompactionResult {
        original_segments,
        new_segments: new_segment_count,
        docs_processed: total_docs,
        elapsed_ms: start.elapsed().as_millis() as u64,
    }))
}

/// Flush a realtime collection's segments in memory into one for the disk (`Collection::compact`),
/// merge its flushed segments as they pile up (`Collection::merge_flushed`), and save: the one way
/// every flush runs (its size, the schedule, memory pressure, the compact endpoints, the binary
/// protocol). Once the bundle is saved, its write-ahead log keeps only the writes after it.
pub fn flush(col: &Collection, data_dir: Option<&std::path::Path>, metrics: &MetricsCollector) -> crate::collection::CompactResult {
    let result = col.compact();
    if result.docs_compacted == 0 {
        return result;
    }
    let labels = CollectionLabels { collection: col.name.clone() };
    metrics.compaction_total.get_or_create(&labels).inc();
    metrics.compaction_duration.get_or_create(&labels).observe(result.compact_ms as f64 / 1000.0);
    col.merge_flushed();
    save(col, data_dir, metrics);
    result
}

/// Merge a realtime collection's flushed segments that are due (`Collection::merge_flushed`: a run
/// of a tier, or a segment with many deleted copies) and save. Returns how many merges ran.
pub fn reclaim(col: &Collection, data_dir: Option<&std::path::Path>, metrics: &MetricsCollector) -> usize {
    let merges = col.merge_flushed();
    if merges > 0 {
        save(col, data_dir, metrics);
    }
    merges
}

/// Save a collection with persistence after a flush or merge.
fn save(col: &Collection, data_dir: Option<&std::path::Path>, metrics: &MetricsCollector) {
    let (true, Some(data_dir)) = (col.schema.persistence_enabled, data_dir) else { return };
    let labels = CollectionLabels { collection: col.name.clone() };
    let _saving = crate::observability::track_in_flight(&metrics.index_saves_in_flight);
    let start = Instant::now();
    match col.save(data_dir) {
        // a collection a schema change replaced saves nothing, and its log went on
        Ok(None) => {}
        Ok(Some(_)) => {
            metrics.persistence_save_duration.get_or_create(&labels).observe(start.elapsed().as_secs_f64());
        }
        Err(e) => tracing::error!(collection = col.name.as_str(), error = %e, "Failed to persist index after a flush"),
    }
}

/// In the background, merge a realtime collection's segments in memory once a run of them is due
/// (`Collection::wants_merge`), flush them once they hold enough documents
/// (`Collection::wants_flush`), and merge a flushed segment with many deleted copies on its own
/// (`Collection::wants_reclaim`): one task per collection at a time (`Collection::maintaining`),
/// going on while any is due, so writes start no flushes that wait behind each other (each would
/// save the few documents written meanwhile as a segment of their own).
pub fn maintain(col: &Arc<Collection>, data_dir: Option<std::path::PathBuf>, metrics: Arc<MetricsCollector>) {
    if !col.wants_merge() && !col.wants_flush() && !col.wants_reclaim() {
        return;
    }
    if col.maintaining.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let col = Arc::clone(col);
    tokio::task::spawn_blocking(move || {
        let idle = loop {
            if col.wants_flush() {
                let r = flush(&col, data_dir.as_deref(), &metrics);
                crate::memory::purge_allocator();
                if r.docs_compacted == 0 {
                    // replaced by a schema change: nothing more of this collection
                    break false;
                }
                tracing::info!(collection = %col.name, docs_flushed = r.docs_compacted, flush_ms = r.compact_ms, "Flushed");
            } else if col.wants_merge() {
                if col.merge_memory() == 0 {
                    // another publisher holds the collection: the next write starts the task again
                    break false;
                }
            } else if col.wants_reclaim() {
                if reclaim(&col, data_dir.as_deref(), &metrics) == 0 {
                    break false;
                }
            } else {
                break true;
            }
        };
        col.maintaining.store(false, std::sync::atomic::Ordering::Release);
        // a write after the last look found the task running and started none
        if idle {
            maintain(&col, data_dir, metrics);
        }
    });
}

/// Flush every realtime collection with segments in memory. Returns how many flushed.
fn compact_all_realtime(
    collections: &CollectionRegistry,
    metrics: &MetricsCollector,
    _config: &Config,
) -> usize {
    let mut compacted = 0;
    for col in collections.list() {
        if col.schema.index_mode != IndexMode::Realtime || col.snapshot_bundle().memory == 0 {
            continue;
        }
        if flush(&col, collections.data_dir.as_deref(), metrics).docs_compacted > 0 {
            compacted += 1;
        }
    }
    if compacted > 0 {
        crate::memory::purge_allocator();
    }
    compacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Document, FieldDef, FieldType, FieldValue, Language, Schema};

    fn make_schema(name: &str) -> Schema {
        let mut s = Schema {
            name: name.into(),
            fields: vec![FieldDef {
                name: "title".into(),
                field_type: FieldType::String,
                facet: false,
                index: true,
                optional: false,
                sort: false,
                stem: true,
                infix: false,
                locale: None,
                num_dim: None,
                vec_dist: None,
                id: 0,
                reference: None, drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            }],
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: false,
            index_mode: IndexMode::Realtime,
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        s.assign_field_ids();
        s
    }

    #[test]
    fn compact_all_skips_collections_with_nothing_in_memory() {
        let reg = CollectionRegistry::new(None);
        let schema = make_schema("test");
        reg.create(schema).unwrap();
        let metrics = MetricsCollector::new();
        let config = Config {
            api_addr: "0.0.0.0:8108".parse().unwrap(),
            ingest_addr: "127.0.0.1:8109".parse().unwrap(),
            api_key: "test".into(),
            data_dir: std::path::PathBuf::from("./data"),
            log_level: "info".into(),
            log_dir: None,
            quiet: false,
            cors_origins: vec![],
            ingest_allow: vec![],
            gpu_vram_budget_bytes: 0,
            max_vector_memory_bytes: 0,
            max_http_body_bytes: 128 * 1024 * 1024,
            max_memory_bytes: 1024 * 1024 * 1024,
            max_ingest_connections: 64,
            query_timeout_ms: 10_000,
            ai: None,
            studio_dir: None,
            compaction_interval_secs: 1800,
            compaction_memory_threshold_percent: 80,
            tls_cert: None,
            tls_key: None,
            slow_query_ms: 0,
            allow_drop_collection: true,
            studio_enabled: true,
            allowed_ips: String::new(),
        };
        let n = compact_all_realtime(&reg, &metrics, &config);
        assert_eq!(n, 0);
    }

    /// A batch collection's schema: the whole-collection compaction is theirs (`compact_collection_segments`).
    fn batch_schema(name: &str) -> Schema {
        let mut s = make_schema(name);
        s.index_mode = IndexMode::Batch;
        s
    }

    #[test]
    fn compact_all_flushes_the_segments_in_memory() {
        let reg = CollectionRegistry::new(None);
        let schema = make_schema("test");
        let col = reg.create(schema).unwrap();

        // a write: a segment in memory
        col.apply_upsert(Document {
            id: 0,
            external_id: "doc1".into(),
            language: Language::default(),
            fields: vec![FieldValue::Text("hello".into())].into(),
            record: None,
        });
        assert_eq!(col.snapshot_bundle().memory_docs(), 1);

        let metrics = MetricsCollector::new();
        let config = Config {
            api_addr: "0.0.0.0:8108".parse().unwrap(),
            ingest_addr: "127.0.0.1:8109".parse().unwrap(),
            api_key: "test".into(),
            data_dir: std::path::PathBuf::from("./data"),
            log_level: "info".into(),
            log_dir: None,
            quiet: false,
            cors_origins: vec![],
            ingest_allow: vec![],
            gpu_vram_budget_bytes: 0,
            max_vector_memory_bytes: 0,
            max_http_body_bytes: 128 * 1024 * 1024,
            max_memory_bytes: 1024 * 1024 * 1024,
            max_ingest_connections: 64,
            query_timeout_ms: 10_000,
            ai: None,
            studio_dir: None,
            compaction_interval_secs: 1800,
            compaction_memory_threshold_percent: 80,
            tls_cert: None,
            tls_key: None,
            slow_query_ms: 0,
            allow_drop_collection: true,
            studio_enabled: true,
            allowed_ips: String::new(),
        };
        let n = compact_all_realtime(&reg, &metrics, &config);
        assert_eq!(n, 1);
        assert_eq!(col.snapshot_bundle().memory, 0);
        assert_eq!(col.snapshot().total_docs(), 1);
    }

    /// A flush saves the segment set with the `_seq_id`s of documents written again, and its LSN
    /// as the log's checkpoint: a restart finds the segments as they were and nothing to replay.
    #[tokio::test]
    async fn a_flush_saves_seq_ids_and_checkpoints_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let mut schema = make_schema("orig");
        schema.persistence_enabled = true;
        let col = reg.create(schema).unwrap();
        let doc = |id: &str, title: &str| Document {
            id: 0,
            external_id: id.into(),
            language: Language::default(),
            fields: vec![FieldValue::Text(title.into())].into(),
            record: None,
        };
        let metrics = MetricsCollector::new();
        col.apply_writes(vec![doc("a", "1"), doc("b", "1"), doc("c", "1")], &[]).unwrap();
        flush(&col, Some(dir.path()), &metrics);
        col.apply_upsert(doc("b", "2"));
        flush(&col, Some(dir.path()), &metrics);
        assert_eq!(col.snapshot_bundle().segments[1].seq.ids, [1]);

        let loaded = crate::persistence::index::load_bundle(dir.path(), "orig", &col.schema, col.created_at).unwrap().unwrap();
        assert_eq!(loaded.segments[1].seq.ids, [1]);
        assert_eq!(loaded.next_seq(), 3);
        let ids: Vec<String> = loaded.live_documents().into_iter().map(|d| d.external_id).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert!(matches!(loaded.live_document("b").unwrap().fields.get(0), Some(FieldValue::Text(t)) if t == "2"));

        assert_eq!(loaded.lsn, 2, "the two writes are in the saved segments");
        col.close_wal();
        let (_, records) = crate::persistence::wal::Wal::open(dir.path(), "orig", loaded.lsn).unwrap();
        assert!(records.is_empty(), "flushed writes come back on no restart");
    }

    #[test]
    fn segment_compaction_skips_below_threshold() {
        // A freshly-built collection has exactly one segment, which is far
        // below SEGMENT_COMPACTION_THRESHOLD; compaction should skip.
        let reg = CollectionRegistry::new(None);
        let schema = make_schema("test");
        let col = reg.create(schema).unwrap();
        col.apply_upsert(Document {
            id: 0,
            external_id: "doc1".into(),
            language: Language::default(),
            fields: vec![FieldValue::Text("hello".into())].into(),
            record: None,
        });
        col.compact();
        assert_eq!(col.snapshot_bundle().segment_count(), 1);

        let result = compact_collection_segments(&col).unwrap();
        assert!(result.is_none(), "single-segment bundle must not be compacted");
    }

    #[test]
    fn segment_compaction_rebuilds_multi_segment_bundle() {
        // Build a multi-segment bundle by hand: stitch SEGMENT_COMPACTION_THRESHOLD
        // segments together, each with one doc, then compact and verify the
        // result is a single-segment bundle that contains all the docs.
        use crate::index::IndexBundle as IB;
        use std::sync::Arc;

        let reg = CollectionRegistry::new(None);
        let schema = batch_schema("test");
        let col = reg.create(schema.clone()).unwrap();

        let schema_arc = Arc::clone(&col.schema);
        let mut segments: Vec<Arc<crate::index::Segment>> = Vec::new();
        for i in 0..SEGMENT_COMPACTION_THRESHOLD {
            let docs = vec![Document {
                id: 0,
                external_id: format!("doc{i}").into(),
                language: Language::default(),
                fields: vec![FieldValue::Text(format!("hello{i}"))].into(),
                record: None,
            }];
            let seg = crate::ingest::build_index(&schema, docs);
            segments.push(Arc::new(seg));
        }
        let total_docs: u32 = segments.iter().map(|s| s.total_docs()).sum();
        let deleted_per_segment = (0..segments.len())
            .map(|_| Arc::new(crate::bitmap::Bitmap::new()))
            .collect();
        let bundle = IB {
            schema: schema_arc,
            segments,
            deleted_per_segment,
            total_docs,
            lsn: 0,
            next_seq: 0,
            memory: 0,
        };
        col.swap_bundle(bundle);
        assert_eq!(
            col.snapshot_bundle().segment_count(),
            SEGMENT_COMPACTION_THRESHOLD
        );

        let result = compact_collection_segments(&col)
            .unwrap()
            .expect("compaction must run when segment count >= threshold");
        assert_eq!(result.original_segments, SEGMENT_COMPACTION_THRESHOLD);
        assert_eq!(result.new_segments, 1);
        assert_eq!(result.docs_processed as usize, SEGMENT_COMPACTION_THRESHOLD);

        let post = col.snapshot_bundle();
        assert_eq!(post.segment_count(), 1);
        assert_eq!(post.total_docs() as usize, SEGMENT_COMPACTION_THRESHOLD);
    }

    /// Build a THRESHOLD-segment bundle with `docs_per_segment` docs each,
    /// every doc carrying `text_len` bytes of text payload.
    fn stitch_bundle(
        col: &Collection,
        schema: &Schema,
        docs_per_segment: usize,
        text_len: usize,
    ) -> u32 {
        use crate::index::IndexBundle as IB;
        let schema_arc = Arc::clone(&col.schema);
        let mut segments: Vec<Arc<crate::index::Segment>> = Vec::new();
        for s in 0..SEGMENT_COMPACTION_THRESHOLD {
            let docs: Vec<Document> = (0..docs_per_segment)
                .map(|i| Document {
                    id: i as u32,
                    external_id: format!("doc{s}_{i}").into(),
                    language: Language::default(),
                    fields: vec![FieldValue::Text("x".repeat(text_len))].into(),
                    record: None,
                })
                .collect();
            segments.push(Arc::new(crate::ingest::build_index(schema, docs)));
        }
        let total_docs: u32 = segments.iter().map(|s| s.total_docs()).sum();
        let deleted_per_segment = (0..segments.len())
            .map(|_| Arc::new(crate::bitmap::Bitmap::new()))
            .collect();
        col.swap_bundle(IB {
            schema: schema_arc,
            segments,
            deleted_per_segment,
            total_docs,
            lsn: 0,
            next_seq: 0,
            memory: 0,
        });
        total_docs
    }

    #[test]
    fn segment_compaction_respects_byte_budget() {
        // 8 segments x 4 docs of ~200B payload each. A budget of ~5 docs'
        // worth of payload must produce multiple merged segments (not one),
        // while still reducing the segment count.
        let reg = CollectionRegistry::new(None);
        let schema = batch_schema("bytecap");
        let col = reg.create(schema.clone()).unwrap();
        let total_docs = stitch_bundle(&col, &schema, 4, 200);

        let per_doc = 200 + 32 + 16 + "doc0_0".len();
        let result = compact_collection_segments_with_target(&col, per_doc * 5)
            .unwrap()
            .expect("compaction must run: budget still reduces segment count");

        assert_eq!(result.original_segments, SEGMENT_COMPACTION_THRESHOLD);
        assert!(
            result.new_segments > 1,
            "byte budget must forbid a single merged segment (got {})",
            result.new_segments
        );
        assert!(
            result.new_segments < SEGMENT_COMPACTION_THRESHOLD,
            "compaction must still reduce the segment count (got {})",
            result.new_segments
        );

        // Bundle integrity: doc count preserved, base_doc_ids cumulative.
        let post = col.snapshot_bundle();
        assert_eq!(post.total_docs(), total_docs);
        assert_eq!(post.segment_count(), result.new_segments);
        let mut expected_base = 0u32;
        for seg in &post.segments {
            assert_eq!(seg.base_doc_id, expected_base);
            assert!(seg.total_docs() > 0);
            expected_base += seg.total_docs();
        }
        assert_eq!(expected_base, total_docs);
    }

    #[test]
    fn segment_compaction_skips_when_budget_cannot_reduce() {
        // A budget of one doc's payload puts every doc in its own group —
        // more groups than existing segments, so compaction must skip
        // instead of churning (or producing MORE segments).
        let reg = CollectionRegistry::new(None);
        let schema = batch_schema("nogain");
        let col = reg.create(schema.clone()).unwrap();
        stitch_bundle(&col, &schema, 2, 200);

        let result = compact_collection_segments_with_target(&col, 1).unwrap();
        assert!(
            result.is_none(),
            "compaction must skip when the byte budget cannot reduce segment count"
        );
        assert_eq!(
            col.snapshot_bundle().segment_count(),
            SEGMENT_COMPACTION_THRESHOLD,
            "bundle must be untouched after a skipped compaction"
        );
    }

    // Regression: a segment merge reads the bundle, builds for tens of seconds,
    // then blind-stores the result. If another publisher publishes inside that
    // window the store discards it, acknowledged documents with it. Both
    // publishers must therefore hold the same exclusive right for the whole
    // sequence.
    #[test]
    fn segment_compaction_yields_to_a_concurrent_publisher() {
        let reg = CollectionRegistry::new(None);
        let schema = batch_schema("exclusive");
        let col = reg.create(schema.clone()).unwrap();
        stitch_bundle(&col, &schema, 2, 50); // enough segments to be compactable

        // Compaction on its own would run here.
        let before = col.snapshot_bundle().segment_count();
        assert_eq!(before, SEGMENT_COMPACTION_THRESHOLD);

        // Hold the publish right, as an in-flight flush does.
        let held = col.try_begin_publish().expect("uncontended");
        assert!(
            compact_collection_segments(&col).unwrap().is_none(),
            "merge published while another publisher held the collection"
        );
        assert_eq!(
            col.snapshot_bundle().segment_count(),
            before,
            "bundle must be untouched by a yielded merge"
        );

        // Released - the merge proceeds on the next attempt, so yielding
        // cannot starve it.
        drop(held);
        assert!(
            compact_collection_segments(&col).unwrap().is_some(),
            "merge must run once the collection is free"
        );
    }

    // Regression: the same external_id living in several segments is the normal
    // LSM shape - a re-upsert writes a new version into a newer segment and
    // reads resolve it by first-hit-in-segment-order. Merging is where those
    // versions have to collapse. Pre-fix the merge flattened every segment into
    // one vector with no dedup and handed out fresh sequential ids, so each
    // historical version became a permanently live, independently searchable
    // document. Live `messages` had drifted to 449,778 docs against 356,887
    // source rows - 26% duplicates - all inside one merged segment.
    #[test]
    fn segment_compaction_collapses_superseded_versions() {
        use crate::index::IndexBundle as IB;

        let reg = CollectionRegistry::new(None);
        let schema = batch_schema("dedup");
        let col = reg.create(schema.clone()).unwrap();

        let schema_arc = Arc::clone(&col.schema);
        let mut segments: Vec<Arc<crate::index::Segment>> = Vec::new();
        for i in 0..SEGMENT_COMPACTION_THRESHOLD {
            // "dup" is re-upserted into every segment; "uniq{i}" appears once.
            let docs = vec![
                Document {
                    id: 0,
                    external_id: "dup".into(),
                    language: Language::default(),
                    fields: vec![FieldValue::Text(format!("v{i}"))].into(),
                    record: None,
                },
                Document {
                    id: 1,
                    external_id: format!("uniq{i}").into(),
                    language: Language::default(),
                    fields: vec![FieldValue::Text(format!("hello{i}"))].into(),
                    record: None,
                },
            ];
            segments.push(Arc::new(crate::ingest::build_index(&schema, docs)));
        }
        let total_docs: u32 = segments.iter().map(|s| s.total_docs()).sum();
        let deleted_per_segment = (0..segments.len())
            .map(|_| Arc::new(crate::bitmap::Bitmap::new()))
            .collect();
        col.swap_bundle(IB {
            schema: schema_arc,
            segments,
            deleted_per_segment,
            total_docs,
            lsn: 0,
            next_seq: 0,
            memory: 0,
        });

        compact_collection_segments(&col)
            .unwrap()
            .expect("compaction must run when segment count >= threshold");

        // One surviving "dup" plus one "uniq" per segment - not one per version.
        let post = col.snapshot_bundle();
        assert_eq!(
            post.total_docs() as usize,
            SEGMENT_COMPACTION_THRESHOLD + 1,
            "merge carried superseded versions over as duplicates"
        );

        let survivors: Vec<Document> = post
            .segments
            .iter()
            .flat_map(|s| s.doc_store.iter_docs(&post.schema))
            .collect();
        let dups: Vec<&Document> = survivors
            .iter()
            .filter(|d| d.external_id == "dup")
            .collect();
        assert_eq!(dups.len(), 1, "external_id must be unique after a merge");
        // The version a read would have resolved to (first segment) is the one
        // that survives, so the merge does not roll a document backwards.
        match dups[0].fields.get(0).unwrap() {
            FieldValue::Text(s) => assert_eq!(s, "v0"),
            _ => panic!("expected Text"),
        }
    }

    // Regression: merges and flushes running concurrently with ongoing upserts
    // must not drop writes that land mid-merge (the build can take seconds for
    // vector collections): those stay in segments of their own after the
    // merged one (`write::publish`).
    #[test]
    fn flushes_keep_concurrent_writes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;

        let reg = CollectionRegistry::new(None);
        let col = reg.create(make_schema("race")).unwrap();

        let total: u32 = 5000;
        let done = Arc::new(AtomicBool::new(false));

        // Single writer (no writer/writer race): upsert `total` distinct docs.
        let writer_col = Arc::clone(&col);
        let writer = thread::spawn(move || {
            for i in 0..total {
                writer_col.apply_upsert(Document {
                    id: i,
                    external_id: format!("d{i}"),
                    language: Language::default(),
                    fields: vec![FieldValue::Text("x".into())].into(),
                    record: None,
                });
            }
        });

        // Merge and flush the segments in memory repeatedly while the writer runs.
        let compactor_col = Arc::clone(&col);
        let done2 = Arc::clone(&done);
        let compactor = thread::spawn(move || {
            while !done2.load(Ordering::Relaxed) {
                compactor_col.merge_memory();
                compactor_col.compact();
                compactor_col.merge_flushed();
            }
        });

        writer.join().unwrap();
        done.store(true, Ordering::Relaxed);
        compactor.join().unwrap();

        // Flush any remainder, then assert every distinct id survived across the
        // segment bundle.
        col.compact();
        assert_eq!(col.snapshot_bundle().memory, 0);
        assert_eq!(col.snapshot_bundle().total_docs() as usize, total as usize);
        let bundle = col.snapshot_bundle();
        let mut seqs: Vec<u64> = (0..bundle.segments.len())
            .flat_map(|i| {
                let seg = &bundle.segments[i];
                let deleted = bundle.deleted_for(i).cloned().unwrap_or_default();
                (0..seg.total_docs()).filter(move |d| !deleted.contains(*d)).map(|d| seg.seq_id(d)).collect::<Vec<_>>()
            })
            .collect();
        seqs.sort_unstable();
        assert_eq!(seqs, (0..u64::from(total)).collect::<Vec<_>>(), "every document keeps the id it was written with");
    }

    // Regression: multiple flushes running concurrently must not duplicate docs.
    // Pre-fix, racing `compact()` calls clobbered each other's `swap_bundle` and
    // left the same id live in two segments (the observed messages_vec dupes).
    // The per-collection compacting flag serializes flushes, so the delete-old
    // -copy path applies cleanly even with re-pushes under concurrency.
    #[test]
    fn concurrent_flushes_do_not_duplicate() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;

        let reg = CollectionRegistry::new(None);
        let col = reg.create(make_schema("conc")).unwrap();

        const DISTINCT: u32 = 1500;
        let writes: u32 = DISTINCT * 2; // every id upserted twice (re-push)
        let done = Arc::new(AtomicBool::new(false));

        let wc = Arc::clone(&col);
        let writer = thread::spawn(move || {
            for i in 0..writes {
                wc.apply_upsert(Document {
                    id: i,
                    external_id: format!("d{}", i % DISTINCT),
                    language: Language::default(),
                    fields: vec![FieldValue::Text("x".into())].into(),
                    record: None,
                });
            }
        });

        // Four compactors hammering merges and flushes concurrently.
        let mut compactors = Vec::new();
        for _ in 0..4 {
            let cc = Arc::clone(&col);
            let d = Arc::clone(&done);
            compactors.push(thread::spawn(move || {
                while !d.load(Ordering::Relaxed) {
                    cc.merge_memory();
                    cc.compact();
                    cc.merge_flushed();
                }
            }));
        }

        writer.join().unwrap();
        done.store(true, Ordering::Relaxed);
        for c in compactors {
            c.join().unwrap();
        }
        col.compact();

        // Exactly DISTINCT live docs - no duplicates from racing flushes.
        assert_eq!(col.snapshot_bundle().memory, 0);
        assert_eq!(col.snapshot_bundle().total_docs() as usize, DISTINCT as usize);
    }
}
