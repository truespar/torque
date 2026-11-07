pub mod alias;
pub mod compaction;
#[cfg(test)]
mod crash_tests;
pub mod write;

use crate::index::{Index, IndexBundle, Segment};
use crate::ingest::{build_index, build_index_with_backing};
use crate::persistence::wal::{Commit, Entry, Logged, Wal, WriteOp};
use write::Writes;
use crate::schema::{Document, Schema, validate_schema};
use arc_swap::ArcSwap;
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// -- Collection --

/// A realtime collection's segments in memory (`IndexBundle::memory`) are flushed into one for the
/// disk once they hold this many documents.
pub const FLUSH_DOCS: u32 = 1_000;
/// Writes are refused (429) while the segments in memory hold this many documents: flushes are
/// not keeping up.
pub const MAX_MEMORY_DOCS: u32 = 10_000;

pub struct Collection {
    pub name: String,
    pub schema: Arc<Schema>,
    /// The collection's index data, stored as an `IndexBundle` (a vector of
    /// segments + collection-wide metadata). For backward compatibility,
    /// `snapshot()` extracts the primary segment so existing call sites
    /// continue to work; new callers should use `snapshot_bundle()` to get
    /// the full bundle and iterate over segments.
    pub index: ArcSwap<IndexBundle>,
    pub created_at: u64,
    /// The version of the schema: how many changes of its fields the collection has had. A
    /// change's rebuilt bundle commits it, and every log record holds it, so a restart between the
    /// saves of a change finishes it (`CollectionRegistry::change_schema`).
    pub schema_version: u64,
    pub last_rebuilt_at: AtomicU64,
    pub last_compaction_at: AtomicU64,
    /// Monotonically increasing generation counter. Each commit increments this;
    /// async builds use the generation at commit time and only swap if still current.
    pub generation: AtomicU64,
    /// Unix epoch seconds of the last successful index save (0 = never saved).
    pub last_save_at: AtomicU64,
    /// The write-ahead log of a realtime collection with persistence (`persistence::wal`): one
    /// record per write, acknowledged once on disk. Carried over to a schema change's successor.
    pub wal: std::sync::Mutex<Option<Arc<Wal>>>,
    /// Paths of the field registry the log holds or the saved segment set's manifest does: a
    /// record carries those the registry gained since (its documents' records may use them).
    pub logged_paths: AtomicUsize,
    /// Data directory for disk-backed text columns (TextMapped).
    pub data_dir: Option<std::path::PathBuf>,
    /// Held by whatever replaces segments (a flush, a merge, a rebuild: `try_begin_publish`), one
    /// at a time per collection.
    pub compacting: AtomicBool,
    /// A flush waits for `compacting` (`begin_publish`): a merge in memory stops for it.
    pub flush_waiting: AtomicBool,
    /// The task merging and flushing the segments in memory in the background runs
    /// (`compaction::maintain`): one per collection.
    pub maintaining: AtomicBool,
    /// Held by every change of the bundle (a write, a merge publishing), so no change is lost to
    /// another landing between its read and its store, and the log receives writes in the order
    /// they apply.
    pub writes: std::sync::Mutex<()>,
    /// Writes waiting for `writes`: the writer that takes it applies every one queued by then
    /// together (`Collection::apply_writes`).
    pub(crate) queue: std::sync::Mutex<std::collections::VecDeque<Arc<QueuedWrite>>>,
    /// Set once a schema change replaced the collection with one holding its documents
    /// (`CollectionRegistry::change_schema`): a write that still reaches this one goes there, and
    /// this one saves nothing more.
    pub successor: OnceLock<Arc<Collection>>,
    /// Held by every save of the collection's bundle (`save`), and shared with the collections a
    /// schema change makes of it: one save of the name at a time.
    pub saves: Arc<std::sync::Mutex<()>>,
}

impl std::fmt::Debug for Collection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collection")
            .field("name", &self.name)
            .field("num_docs", &self.index.load().total_docs())
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// A collection's documents as one request sees them (`Collection::view`): its segments with
/// their deletions, a realtime collection's writes in it as soon as they are made. Every document
/// has one live copy, so an id names at most one live document.
pub struct CollectionView {
    pub bundle: Arc<IndexBundle>,
}

impl CollectionView {
    pub fn len(&self) -> usize {
        self.bundle.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Segment `i` and its deleted documents (None when it has none).
    pub fn segment(&self, i: usize) -> (&Segment, Option<&crate::bitmap::Bitmap>) {
        (&self.bundle.segments[i], self.bundle.deleted_for(i).filter(|d| !d.is_empty()))
    }

    /// Every segment with its deleted documents.
    pub fn segments(&self) -> Vec<(&Segment, Option<&crate::bitmap::Bitmap>)> {
        (0..self.len()).map(|i| self.segment(i)).collect()
    }

    /// What a search of the collection searches.
    pub fn target(&self) -> crate::search::SearchTarget<'_> {
        crate::search::SearchTarget::Bundle(&self.bundle)
    }

    /// Live documents in all.
    pub fn total_docs(&self) -> u32 {
        self.bundle.total_docs()
    }

    /// The live document with id `ext_id`: its segment and doc id there.
    pub fn lookup(&self, ext_id: &str) -> Option<(usize, crate::schema::DocId)> {
        (0..self.len()).find_map(|i| {
            let (seg, deleted) = self.segment(i);
            let doc = seg.ext_id_lookup(ext_id)?;
            (!deleted.is_some_and(|d| d.contains(doc))).then_some((i, doc))
        })
    }

    /// The segment and doc id of the document a hit's `seq` (segment base + doc id) names.
    pub fn locate(&self, seq: u64) -> Option<(usize, crate::schema::DocId)> {
        (0..self.len()).find_map(|i| {
            let seg = self.segment(i).0;
            let base = u64::from(seg.base_doc_id);
            (seq >= base && seq < base + u64::from(seg.total_docs())).then(|| (i, (seq - base) as crate::schema::DocId))
        })
    }
}

/// Exclusive right to publish a new bundle for one collection, released on
/// drop (including on panic). See [`Collection::try_begin_publish`].
pub struct PublishGuard<'a>(&'a AtomicBool);

impl Drop for PublishGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Collection {
    /// Returns the primary segment for backward compatibility with the
    /// pre-segment-refactor API. New callers should prefer
    /// `snapshot_bundle()` and iterate over segments.
    pub fn snapshot(&self) -> Arc<Segment> {
        self.index.load().primary_segment_arc()
    }

    /// Returns the full IndexBundle. Use this when you need to iterate over
    /// all segments (search fan-out, persistence, observability).
    pub fn snapshot_bundle(&self) -> Arc<IndexBundle> {
        self.index.load_full()
    }

    /// The collection as a request sees it.
    pub fn view(&self) -> CollectionView {
        CollectionView { bundle: self.snapshot_bundle() }
    }

    /// Swap the collection's data with a freshly built single-segment Index.
    /// Wraps the segment in an `IndexBundle` of one for storage.
    pub fn swap_index(&self, new_index: Index) {
        let bundle = IndexBundle::single(new_index, Arc::clone(&self.schema));
        self.swap_bundle(bundle);
    }

    /// Take the exclusive right to publish a new bundle for this collection,
    /// or `None` if another publisher already holds it.
    ///
    /// Every incremental publisher must hold this for the WHOLE
    /// snapshot-build-swap sequence, not just the swap. `swap_bundle` is a
    /// blind store, so two publishers that each snapshot, build, and then store
    /// will silently discard whichever landed first, acknowledged documents
    /// with it. That is exactly how a 46s `messages_vec` segment merge
    /// swallowed a flush that completed inside its window.
    ///
    /// Skipping is always safe for both publishers: a skipped flush leaves the
    /// segments in memory for the next one, and a skipped merge is pure
    /// optimisation that the next scheduler tick retries. Neither can starve
    /// the other, because neither spins.
    pub fn try_begin_publish(&self) -> Option<PublishGuard<'_>> {
        self.compacting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| PublishGuard(&self.compacting))
    }

    /// The right to replace the collection's data, taken in a flush's order: the publish guard
    /// (a flush or segment merge in progress finishes first), then the writes lock.
    pub fn exclusive(&self) -> (PublishGuard<'_>, std::sync::MutexGuard<'_, ()>) {
        let publishing = self.begin_publish();
        (publishing, self.writes.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// `try_begin_publish`, waiting for the publisher holding it to finish: a merge of the
    /// segments in memory stops at its next merge for it (`flush_waiting`).
    pub fn begin_publish(&self) -> PublishGuard<'_> {
        loop {
            if let Some(guard) = self.try_begin_publish() {
                return guard;
            }
            self.flush_waiting.store(true, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Save the bundle as it is now (`persistence::index::save_bundle`: the segments not on disk
    /// and the deletions changed since the last save), one save of the collection's name at a
    /// time; the segments it wrote then serve from their files (`adopt_saved`). A collection a
    /// schema change replaced saves nothing (None): its successor's data is the collection's.
    pub fn save(&self, data_dir: &std::path::Path) -> std::io::Result<Option<Duration>> {
        let saved = {
            let _saving = self.saves.lock().unwrap_or_else(|e| e.into_inner());
            self.save_locked(data_dir)?
        };
        // (the writes lock, after `saves`: a schema change takes them the other way)
        Ok(saved.map(|saved| {
            self.adopt_saved(saved.mapped);
            saved.elapsed
        }))
    }

    /// `save` with `saves` held, its segments left as they are (the next save maps them).
    fn save_locked(&self, data_dir: &std::path::Path) -> std::io::Result<Option<crate::persistence::index::Saved>> {
        if self.successor.get().is_some() {
            return Ok(None);
        }
        let bundle = self.snapshot_bundle();
        let saved = crate::persistence::index::save_bundle(data_dir, &self.name, &bundle, &self.schema, self.stamp())?;
        self.last_save_at.store(now_epoch(), Ordering::Relaxed);
        // the log's records up to the bundle's LSN are in its saved segments now
        if let Some(wal) = self.wal() {
            wal.checkpoint(bundle.lsn);
        }
        Ok(Some(saved))
    }

    /// Put segments a save read back from their files (mapped) in place of the same ones built in
    /// memory, wherever the collection still has them: the same documents and sections, their
    /// memory gone once the searches holding the old ones finish. A merge under way knows them by
    /// id, so it publishes still.
    fn adopt_saved(&self, mapped: Vec<Arc<Segment>>) {
        if mapped.is_empty() {
            return;
        }
        let by_id: rustc_hash::FxHashMap<u64, Arc<Segment>> = mapped.into_iter().map(|s| (s.id, s)).collect();
        let _writing = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let current = self.index.load_full();
            let mut next: Option<IndexBundle> = None;
            for (i, seg) in current.segments.iter().enumerate() {
                if let Some(m) = by_id.get(&seg.id).filter(|m| !Arc::ptr_eq(m, seg)) {
                    next.get_or_insert_with(|| (*current).clone()).segments[i] = Arc::clone(m);
                }
            }
            let Some(next) = next else { return };
            // (a blind store, `swap_bundle`, takes no lock: only a bundle unchanged is replaced)
            let prev = self.index.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&prev, &current) {
                return;
            }
        }
    }

    /// The collection's write-ahead log, if it has one.
    pub fn wal(&self) -> Option<Arc<Wal>> {
        self.wal.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Written to through a write-ahead log: a realtime collection with persistence and a data
    /// directory. One whose log could not be opened takes no writes.
    fn logged(&self) -> bool {
        self.schema.index_mode == crate::schema::IndexMode::Realtime && self.schema.persistence_enabled && self.data_dir.is_some()
    }

    /// Which schema of the collection its bundle is saved under.
    pub fn stamp(&self) -> crate::persistence::index::SchemaStamp {
        crate::persistence::index::SchemaStamp { created_at: self.created_at, version: self.schema_version }
    }

    /// Every live document, the segments in memory included, in `_seq_id` order,
    /// each typed against `schema` (`DocStore::typed_doc`; `strict` validates it as a write).
    /// Fails on one it cannot read or type rather than leave it out.
    pub fn live_documents_as(&self, schema: &Schema, strict: bool) -> Result<Vec<Document>, String> {
        use rayon::prelude::*;
        let view = self.view();
        let mut docs: Vec<(u64, Document)> = Vec::with_capacity(view.total_docs() as usize);
        for (seg, deleted) in view.segments() {
            let typed = (0..seg.total_docs())
                .into_par_iter()
                .filter(|&doc| !deleted.is_some_and(|d| d.contains(doc)))
                .map(|doc| match seg.doc_store.typed_doc(doc, schema, strict) {
                    Ok(Some(d)) => Ok((seg.seq_id(doc), d)),
                    Ok(None) => Err(format!("A document of `{}` has no stored JSON.", self.name)),
                    Err(e) => Err(e),
                })
                .collect::<Result<Vec<_>, String>>()?;
            docs.extend(typed);
        }
        // segments in order of their bases are in order unless one holds documents written again
        if !docs.windows(2).all(|w| w[0].0 < w[1].0) {
            docs.sort_by_key(|d| d.0);
        }
        Ok(docs.into_iter().map(|d| d.1).collect())
    }

    /// Swap the collection's data with a freshly built IndexBundle.
    /// This is the path the per-segment streaming builder will use.
    ///
    /// Callers that derive `new_bundle` from an earlier `snapshot_bundle` must
    /// hold [`Self::try_begin_publish`] across both - this is a blind store and
    /// will discard anything published in between.
    pub fn swap_bundle(&self, new_bundle: IndexBundle) {
        self.index.store(Arc::new(new_bundle));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.last_rebuilt_at.store(now, Ordering::Relaxed);
    }

    /// Atomically swap index only if the generation hasn't advanced since the build started.
    /// Returns true if the swap succeeded, false if a newer build superseded this one.
    pub fn swap_index_if_generation(&self, new_index: Index, expected_gen: u64) -> bool {
        if self.generation.load(Ordering::Acquire) != expected_gen {
            return false;
        }
        self.swap_index(new_index);
        true
    }

    /// Increment and return the new generation number (called at commit time).
    pub fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Convenience: build index from documents and swap atomically.
    pub fn ingest_documents(&self, docs: Vec<Document>) {
        let index = if let Some(ref dir) = self.data_dir {
            build_index_with_backing(&self.schema, docs, dir, &self.name)
        } else {
            build_index(&self.schema, docs)
        };
        self.swap_index(index);
    }

    // -- Realtime writes --

    /// Apply a single upsert. Returns the live document count.
    pub fn apply_upsert(&self, doc: Document) -> u32 {
        self.apply_writes(vec![doc], &[]).expect("a document of the collection's schema");
        self.index.load().total_docs()
    }

    /// Apply a single delete. Returns the live document count.
    pub fn apply_delete(&self, external_id: &str) -> u32 {
        self.apply_writes(Vec::new(), &[external_id.to_string()]).expect("a delete");
        self.index.load().total_docs()
    }

    /// Upsert documents and then delete ids as one write: its documents indexed once, as a segment
    /// of their own after the collection's (`write::apply`), the copies they replace hidden, and
    /// a record of the write-ahead log, ordered with every other write. Writers queue: the one
    /// that takes the lock applies every write queued by then together (one segment, one record
    /// holding each in order; LevelDB's write group), so concurrent writes share the build and
    /// the sync. Visible at once; durable once the returned acknowledgement resolves (None
    /// without a log). A write reaching a collection a schema change replaced goes to its
    /// successor, typed against the new schema: an error when that refuses a document, or when
    /// the log refuses the write (then it is not applied).
    pub fn apply_writes(&self, docs: Vec<Document>, deletes: &[String]) -> Result<Option<Commit>, WriteError> {
        let queued = Arc::new(QueuedWrite { write: std::sync::Mutex::new(Some((docs, deletes.to_vec()))), result: std::sync::Mutex::new(None) });
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).push_back(Arc::clone(&queued));
        let writing = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = queued.take_result() {
            // a writer before took it along
            return result;
        }
        let group: Vec<Arc<QueuedWrite>> = self.queue.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        self.apply_group(&group);
        drop(writing);
        queued.take_result().expect("the group answered every write")
    }

    /// Apply queued writes together, `writes` held: each gets its answer.
    fn apply_group(&self, group: &[Arc<QueuedWrite>]) {
        let writes: Vec<(Vec<Document>, Vec<String>)> = group.iter().map(|q| q.write.lock().unwrap_or_else(|e| e.into_inner()).take().expect("a write not yet applied")).collect();
        if let Some(next) = self.successor.get() {
            // a schema change replaced the collection while they waited: each goes to the new one
            for (q, (docs, deletes)) in group.iter().zip(writes) {
                let result = docs
                    .iter()
                    .map(|d| crate::ingest::retype(d, &self.schema, &next.schema))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(WriteError::Invalid)
                    .and_then(|docs| next.apply_writes(docs, &deletes));
                q.answer(result);
            }
            return;
        }
        let wal = self.wal();
        if wal.is_none() && self.logged() {
            let e = format!("The write-ahead log of `{}` is not open: no writes until a restart recovers it.", self.name);
            for q in group {
                q.answer(Err(WriteError::Log(e.clone())));
            }
            return;
        }
        let mut ops = Vec::with_capacity(if wal.is_some() { writes.len() } else { 0 });
        let mut batch = Writes::default();
        for (docs, deletes) in writes {
            if wal.is_some() {
                // the record holds the documents as written: their records, every path of them
                let upserts = docs
                    .iter()
                    .map(|d| match &d.record {
                        Some(r) => Logged::Record(Arc::clone(r)),
                        None => Logged::Typed(crate::ingest::protocol::serialize_document(d, &self.schema)),
                    })
                    .collect();
                ops.push(WriteOp { upserts, deletes: deletes.clone() });
            }
            for doc in docs {
                batch.upsert(doc);
            }
            for id in &deletes {
                batch.delete(id);
            }
        }
        let mut next = write::apply(&self.index.load(), batch);
        let Some(w) = wal else {
            self.index.store(Arc::new(next));
            for q in group {
                q.answer(Ok(None));
            }
            return;
        };
        // the paths the registry gained since the record before: these records may use them
        let registry = self.schema.registry.load();
        let from = self.logged_paths.load(Ordering::Relaxed).min(registry.len());
        let paths = (from..registry.len()).map(|i| (i as u16, registry.entry(i as u16).clone())).collect();
        match w.append_for(self.schema_version, &Entry::Writes { paths, ops }, group.len()) {
            Ok((lsn, commits)) => {
                self.logged_paths.store(registry.len(), Ordering::Relaxed);
                next.lsn = lsn;
                self.index.store(Arc::new(next));
                for (q, commit) in group.iter().zip(commits) {
                    q.answer(Ok(Some(commit)));
                }
            }
            Err(e) => {
                for q in group {
                    q.answer(Err(WriteError::Log(e.clone())));
                }
            }
        }
    }

    /// The segments in memory hold as many documents as they may (`MAX_MEMORY_DOCS`): further
    /// writes wait for a flush (429).
    pub fn memory_full(&self) -> bool {
        self.schema.index_mode == crate::schema::IndexMode::Realtime && self.index.load().memory_docs() >= MAX_MEMORY_DOCS
    }

    /// The segments in memory hold enough documents to flush (`FLUSH_DOCS`).
    pub fn wants_flush(&self) -> bool {
        self.schema.index_mode == crate::schema::IndexMode::Realtime && self.index.load().memory_docs() >= FLUSH_DOCS
    }

    /// The segments in memory include a run to merge (`write::merge_run`).
    pub fn wants_merge(&self) -> bool {
        let bundle = self.index.load();
        self.schema.index_mode == crate::schema::IndexMode::Realtime && write::merge_run(&bundle, bundle.segments.len() - bundle.memory..bundle.segments.len()).is_some()
    }

    pub fn realtime_stats(&self) -> RealtimeStats {
        let bundle = self.snapshot_bundle();
        RealtimeStats {
            memory_documents: bundle.memory_docs(),
            deleted_documents: bundle.deleted_per_segment.iter().map(|d| d.cardinality()).sum(),
            segments: bundle.segments.len(),
            last_flush_at: self.last_compaction_at.load(Ordering::Relaxed),
            wal_bytes: self.data_dir.as_deref().map_or(0, |d| crate::persistence::wal::disk_size(d, &self.name)),
        }
    }

    /// The live document `ext_id`.
    pub fn document(&self, ext_id: &str) -> Option<Document> {
        self.index.load().live_document(ext_id)
    }

    /// A live document `ext_id` exists.
    pub fn contains(&self, ext_id: &str) -> bool {
        self.index.load().live_lookup(ext_id).is_some()
    }

    /// Publish a merge of `snapshot`'s segments `range` (`write::publish`): false when a publisher
    /// replaced them since.
    fn publish_merge(&self, snapshot: &IndexBundle, range: std::ops::Range<usize>, merged: write::Merged, in_memory: bool) -> bool {
        let _writing = self.writes.lock().unwrap_or_else(|e| e.into_inner());
        match write::publish(&self.index.load(), snapshot, range, merged, in_memory) {
            Some(bundle) => {
                self.index.store(Arc::new(bundle));
                true
            }
            None => false,
        }
    }

    /// Merge the segments realtime writes built in memory as they pile up: while the newest run
    /// of `write::MERGE_FACTOR` of one size tier is there, it merges into one, in memory (writes go
    /// on meanwhile). Returns how many merges ran; none while another publisher holds the
    /// collection.
    pub fn merge_memory(&self) -> usize {
        let Some(_publishing) = self.try_begin_publish() else { return 0 };
        self.flush_waiting.store(false, Ordering::Relaxed);
        let mut merges = 0;
        loop {
            if self.flush_waiting.load(Ordering::Relaxed) {
                // a flush (or truncate) waits: it takes these segments whole
                break;
            }
            let snapshot = self.snapshot_bundle();
            let tail = snapshot.segments.len() - snapshot.memory..snapshot.segments.len();
            let Some(range) = write::merge_run(&snapshot, tail) else { break };
            let Some(merged) = self.merge(&snapshot, range.clone()) else { break };
            if !self.publish_merge(&snapshot, range, merged, true) {
                break;
            }
            merges += 1;
        }
        merges
    }

    /// `write::merge`, a failure logged (the segments stay as they are).
    fn merge(&self, snapshot: &IndexBundle, range: std::ops::Range<usize>) -> Option<write::Merged> {
        write::merge(snapshot, range.clone())
            .inspect_err(|e| tracing::error!(collection = %self.name, segments = ?range, error = %e, "Segment merge failed"))
            .ok()
    }

    /// Merge the segments flushed to the disk as they pile up, as `merge_memory` does the ones in
    /// memory, then each whose deleted copies reach `write::RECLAIM_SHARE` of it on its own
    /// (`write::reclaim`). Returns how many merges ran.
    pub fn merge_flushed(&self) -> usize {
        if self.schema.index_mode != crate::schema::IndexMode::Realtime {
            return 0;
        }
        let Some(_publishing) = self.try_begin_publish() else { return 0 };
        let mut merges = 0;
        loop {
            let snapshot = self.snapshot_bundle();
            let flushed = 0..snapshot.segments.len() - snapshot.memory;
            let Some(range) = write::merge_run(&snapshot, flushed.clone()).or_else(|| write::reclaim(&snapshot, flushed)) else { break };
            let Some(merged) = self.merge(&snapshot, range.clone()) else { break };
            if !self.publish_merge(&snapshot, range, merged, false) {
                break;
            }
            merges += 1;
        }
        merges
    }

    /// A flushed segment's deleted copies reach `write::RECLAIM_SHARE` of it (`write::reclaim`).
    pub fn wants_reclaim(&self) -> bool {
        let bundle = self.index.load();
        self.schema.index_mode == crate::schema::IndexMode::Realtime && write::reclaim(&bundle, 0..bundle.segments.len() - bundle.memory).is_some()
    }

    /// Flush the segments realtime writes built in memory into one for the disk (the save that
    /// follows writes it; `compaction::flush`), writes going on meanwhile; a merge in progress
    /// finishes first (`begin_publish`). Nothing to flush for a collection without segments in
    /// memory (a batch collection), or one a schema change replaced.
    pub fn compact(&self) -> CompactResult {
        if self.successor.get().is_some() || self.index.load().memory == 0 {
            return CompactResult {
                docs_compacted: 0,
                total_docs: self.index.load().total_docs() as usize,
                compact_ms: 0,
            };
        }
        let _publish_guard = self.begin_publish();
        let start = std::time::Instant::now();
        let snapshot = self.snapshot_bundle();
        if snapshot.memory == 0 {
            return CompactResult { docs_compacted: 0, total_docs: snapshot.total_docs() as usize, compact_ms: 0 };
        }
        let range = snapshot.segments.len() - snapshot.memory..snapshot.segments.len();
        let docs_compacted = snapshot.memory_docs() as usize;
        if range.len() == 1 {
            // one segment in memory: it is the flushed one as it is
            let _writing = self.writes.lock().unwrap_or_else(|e| e.into_inner());
            let current = self.index.load_full();
            if range.end <= current.segments.len() && current.segments[range.start].id == snapshot.segments[range.start].id {
                let mut flushed = (*current).clone();
                flushed.memory = current.segments.len() - range.end;
                self.index.store(Arc::new(flushed));
            }
        } else {
            let Some(merged) = self.merge(&snapshot, range.clone()) else {
                return CompactResult { docs_compacted: 0, total_docs: snapshot.total_docs() as usize, compact_ms: start.elapsed().as_millis() as u64 };
            };
            self.publish_merge(&snapshot, range, merged, false);
        }
        self.last_compaction_at.store(now_epoch(), Ordering::Relaxed);
        CompactResult {
            docs_compacted,
            total_docs: self.index.load().total_docs() as usize,
            compact_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Delete every document at once: an empty segment set (one record of
    /// the log, which a restart replays as the same), instead of a deletion per document. Waits
    /// for a segment merge in progress. Durable once the returned acknowledgement resolves.
    pub fn truncate(&self) -> Result<Option<Commit>, WriteError> {
        let (_publishing, _writing) = self.exclusive();
        let wal = self.wal();
        if wal.is_none() && self.logged() {
            return Err(WriteError::Log(format!("The write-ahead log of `{}` is not open: no writes until a restart recovers it.", self.name)));
        }
        let current = self.snapshot_bundle();
        let mut empty = IndexBundle::empty(Arc::clone(&self.schema));
        empty.lsn = current.lsn;
        // a document written after gets an id after every one the collection had
        empty.next_seq = current.next_seq();
        let commit = match wal {
            Some(w) => {
                let (lsn, commit) = w.append(self.schema_version, &Entry::Clear).map_err(WriteError::Log)?;
                empty.lsn = lsn;
                Some(commit)
            }
            None => None,
        };
        self.swap_bundle(empty);
        Ok(commit)
    }

    /// Resolves once every write appended to the collection's log before is on disk. No-op for a
    /// collection without one.
    pub async fn wal_barrier(&self) {
        if let Some(wal) = self.wal() {
            wal.barrier().await;
        }
    }

    /// Stop the collection's log once everything appended is on disk (the collection goes:
    /// deleted, suspended, the server stopping).
    pub fn close_wal(&self) {
        if let Some(wal) = self.wal.lock().unwrap_or_else(|e| e.into_inner()).take() {
            wal.shutdown();
        }
    }

}

/// A realtime collection with persistence after a restart: its saved bundle with the records of
/// its write-ahead log after the bundle's checkpoint applied, as one write (`write::apply`: the
/// documents in the order they had; a truncation empties the segments too), and the log to go on
/// writing to. A log that cannot be read (damaged before its last record) leaves the bundle as
/// saved and the collection without a log, which then refuses writes
/// (`Collection::apply_writes`): its records stay on disk.
/// A write waiting in `Collection::queue`, and its answer once applied.
pub(crate) struct QueuedWrite {
    write: std::sync::Mutex<Option<(Vec<Document>, Vec<String>)>>,
    result: std::sync::Mutex<Option<Result<Option<Commit>, WriteError>>>,
}

impl QueuedWrite {
    fn answer(&self, result: Result<Option<Commit>, WriteError>) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
    }

    fn take_result(&self) -> Option<Result<Option<Commit>, WriteError>> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

pub fn recover(
    data_dir: &std::path::Path,
    name: &str,
    schema: &Arc<Schema>,
    schema_version: u64,
    bundle: Arc<IndexBundle>,
) -> (Arc<IndexBundle>, Option<Arc<Wal>>) {
    let (wal, records) = match Wal::open(data_dir, name, bundle.lsn) {
        Ok(opened) => opened,
        Err(e) => {
            tracing::error!(collection = name, error = %e, "The write-ahead log could not be read: the collection takes no writes until it is repaired");
            return (bundle, None);
        }
    };
    let saved = Arc::clone(&bundle);
    let mut bundle = bundle;
    let mut batch = Writes::default();
    let mut lsn = bundle.lsn;
    let (mut writes, mut skipped) = (0usize, 0usize);
    for record in records {
        if record.schema_version != schema_version {
            // written under another schema the saved segments already replace (a change cut short)
            skipped += 1;
            continue;
        }
        match record.entry {
            Entry::Writes { paths, ops } => {
                let read = schema.registry.extend_with(&paths).and_then(|()| {
                    ops.iter().flat_map(|op| &op.upserts).map(|doc| logged_document(doc, schema)).collect::<Result<Vec<_>, String>>()
                });
                let docs = match read {
                    Ok(docs) => docs,
                    Err(e) => {
                        // (its writes stay on disk: dropped, they would be lost for good)
                        tracing::error!(collection = name, lsn = record.lsn, error = %e, "The write-ahead log could not be read: the collection takes no writes until it is repaired");
                        return (saved, None);
                    }
                };
                let mut docs = docs.into_iter();
                for op in &ops {
                    for doc in docs.by_ref().take(op.upserts.len()) {
                        batch.upsert(doc);
                    }
                    for id in &op.deletes {
                        batch.delete(id);
                    }
                }
                writes += ops.len();
            }
            Entry::Clear => {
                batch = Writes::default();
                let mut empty = IndexBundle::empty(Arc::clone(schema));
                empty.next_seq = bundle.next_seq();
                bundle = Arc::new(empty);
            }
        }
        lsn = record.lsn;
    }
    let mut recovered = if batch.is_empty() { (*bundle).clone() } else { write::apply(&bundle, batch) };
    recovered.lsn = lsn;
    if writes > 0 {
        tracing::info!(collection = name, writes, docs = recovered.memory_docs(), lsn, "Replayed the write-ahead log");
    }
    if skipped > 0 {
        tracing::warn!(collection = name, skipped, "Skipped records of the write-ahead log written under another schema version");
    }
    (Arc::new(recovered), Some(Arc::new(wal)))
}

/// A document of the write-ahead log: typed from its record (against the registry with the record's
/// paths), or read from its typed fields.
fn logged_document(doc: &Logged, schema: &Schema) -> Result<Document, String> {
    match doc {
        Logged::Record(rec) => {
            let json = crate::tqbf::record::to_value(rec, &schema.registry.load()).map_err(|e| e.to_string())?;
            let (external_id, language, fields) = crate::ingest::typed_fields(&json, schema, false)?;
            Ok(Document { id: 0, external_id, language, fields, record: Some(Arc::clone(rec)) })
        }
        Logged::Typed(bytes) => crate::ingest::protocol::deserialize_document(bytes, schema, &mut 0, 0).map_err(|e| e.to_string()),
    }
}

/// A batch collection's documents after a write (its rebuild's input): each written document in
/// the place of the live copy it replaces (Typesense keeps a document's sequence id), the new ones
/// after all others in the order written, the deleted ones gone. `live` is in `_seq_id` order;
/// a document written twice keeps the last version, upserts apply before deletes.
pub fn merge_writes(live: Vec<Document>, upserts: Vec<Document>, deletes: &[String]) -> Vec<Document> {
    let deleted: rustc_hash::FxHashSet<&str> = deletes.iter().map(String::as_str).collect();
    let mut written: rustc_hash::FxHashMap<String, Document> = rustc_hash::FxHashMap::default();
    let mut new_ids: Vec<String> = Vec::new();
    for doc in upserts {
        let id = doc.external_id.clone();
        if written.insert(id.clone(), doc).is_none() {
            new_ids.push(id);
        }
    }
    let mut out = Vec::with_capacity(live.len() + written.len());
    for doc in live {
        let replacement = written.remove(&doc.external_id);
        if !deleted.contains(doc.external_id.as_str()) {
            out.push(replacement.unwrap_or(doc));
        }
    }
    for id in new_ids {
        if let Some(doc) = written.remove(&id).filter(|_| !deleted.contains(id.as_str())) {
            out.push(doc);
        }
    }
    out
}

/// A realtime collection's segments under writes (`Collection::realtime_stats`).
#[derive(serde::Serialize)]
pub struct RealtimeStats {
    /// Documents in the segments realtime writes built in memory, not yet flushed.
    pub memory_documents: u32,
    /// Copies of documents deleted or written again, still in their segments until a merge.
    pub deleted_documents: u64,
    pub segments: usize,
    /// Unix seconds of the last flush (0: none).
    pub last_flush_at: u64,
    /// Bytes of the write-ahead log.
    pub wal_bytes: u64,
}

/// Why a write was not applied.
#[derive(Debug)]
pub enum WriteError {
    /// The write itself: a document the schema refuses.
    Invalid(String),
    /// The collection's write-ahead log could not take it (closed, or a write to disk failed).
    Log(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(e) | Self::Log(e) => f.write_str(e),
        }
    }
}

pub struct CompactResult {
    pub docs_compacted: usize,
    pub total_docs: usize,
    pub compact_ms: u64,
}

// -- Errors --

#[derive(Debug)]
pub enum CollectionError {
    AlreadyExists(String),
    NotFound(String),
    InvalidSchema(String),
    LimitExceeded(String),
    /// The data directory did not take the change: the collection is as it was.
    Storage(String),
}

impl std::fmt::Display for CollectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists(name) => write!(f, "Collection `{name}` already exists."),
            Self::NotFound(name) => write!(f, "Collection `{name}` not found."),
            Self::InvalidSchema(msg) => write!(f, "Invalid schema: {msg}"),
            Self::LimitExceeded(msg) | Self::Storage(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CollectionError {}

// -- Registry --

/// Maximum number of collections. Prevents unbounded metadata memory growth.
const MAX_COLLECTIONS: usize = 10_000;

pub struct CollectionRegistry {
    collections: DashMap<String, Arc<Collection>>,
    /// Collections of collections.json not loaded: suspended, or not loadable from their files
    /// (each start tries again). Their files are kept, and resume loads one.
    unloaded: std::sync::Mutex<Vec<crate::persistence::PersistedCollection>>,
    pub data_dir: Option<std::path::PathBuf>,
}

impl CollectionRegistry {
    pub fn new(data_dir: Option<std::path::PathBuf>) -> Self {
        Self {
            collections: DashMap::new(),
            unloaded: Default::default(),
            data_dir,
        }
    }

    fn unloaded_list(&self) -> std::sync::MutexGuard<'_, Vec<crate::persistence::PersistedCollection>> {
        self.unloaded.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The collections of collections.json not loaded.
    pub fn unloaded(&self) -> Vec<crate::persistence::PersistedCollection> {
        self.unloaded_list().clone()
    }

    /// Keep a collection of collections.json without loading it.
    pub fn set_unloaded(&self, entry: crate::persistence::PersistedCollection) {
        let mut list = self.unloaded_list();
        list.retain(|e| e.schema.name != entry.schema.name);
        list.push(entry);
    }

    /// Take the collection `name` out of those not loaded.
    pub fn take_unloaded(&self, name: &str) -> Option<crate::persistence::PersistedCollection> {
        let mut list = self.unloaded_list();
        let i = list.iter().position(|e| e.schema.name == name)?;
        Some(list.remove(i))
    }

    pub fn create(&self, mut schema: Schema) -> Result<Arc<Collection>, CollectionError> {
        // Validate schema
        validate_schema(&schema).map_err(|e| CollectionError::InvalidSchema(e.message))?;

        // Check collection count limit
        if self.collections.len() >= MAX_COLLECTIONS {
            return Err(CollectionError::LimitExceeded(format!(
                "Maximum collection limit ({MAX_COLLECTIONS}) reached."
            )));
        }

        // Check for duplicates (a suspended collection is one too: its files are kept)
        if self.collections.contains_key(&schema.name) || self.unloaded_list().iter().any(|e| e.schema.name == schema.name) {
            return Err(CollectionError::AlreadyExists(schema.name.clone()));
        }

        // Assign field IDs
        schema.assign_field_ids();

        let name = schema.name.clone();
        // the files of an earlier collection of the name (a deletion a crash cut short) hold nothing
        // of this one: gone for good before anything holds it
        if let (Some(data_dir), true) = (&self.data_dir, schema.persistence_enabled) {
            crate::persistence::index::delete_index(data_dir, &name)
                .map_err(|e| CollectionError::Storage(format!("Could not remove the files of an earlier collection `{name}`: {e}")))?;
        }
        let wal = match (&self.data_dir, schema.index_mode == crate::schema::IndexMode::Realtime && schema.persistence_enabled) {
            // (as is a log of an earlier collection of the name)
            (Some(data_dir), true) => match Wal::create(data_dir, &name) {
                Ok(wal) => Some(Arc::new(wal)),
                Err(e) => return Err(CollectionError::Storage(format!("Could not create the collection's write-ahead log: {e}"))),
            },
            _ => None,
        };
        let schema_arc = Arc::new(schema);
        let collection = Arc::new(Collection {
            name: name.clone(),
            schema: Arc::clone(&schema_arc),
            index: ArcSwap::new(Arc::new(IndexBundle::empty(Arc::clone(&schema_arc)))),
            created_at: now_epoch(),
            schema_version: 0,
            last_rebuilt_at: AtomicU64::new(0),
            last_compaction_at: AtomicU64::new(0),
            last_save_at: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            wal: std::sync::Mutex::new(wal),
            logged_paths: AtomicUsize::new(0),
            data_dir: self.data_dir.clone(),
            compacting: std::sync::atomic::AtomicBool::new(false),
            flush_waiting: std::sync::atomic::AtomicBool::new(false),
            maintaining: std::sync::atomic::AtomicBool::new(false),
            writes: std::sync::Mutex::new(()),
            queue: Default::default(),
            successor: Default::default(),
            saves: Default::default(),
        });

        // Use entry API to avoid TOCTOU race
        use dashmap::mapref::entry::Entry;
        match self.collections.entry(name) {
            Entry::Occupied(_) => {
                Err(CollectionError::AlreadyExists(collection.name.clone()))
            }
            Entry::Vacant(entry) => {
                entry.insert(Arc::clone(&collection));
                Ok(collection)
            }
        }
    }

    /// Insert a collection directly (used for loading from persistence).
    pub fn insert(&self, collection: Arc<Collection>) {
        self.collections.insert(collection.name.clone(), collection);
    }

    pub fn get(&self, name: &str) -> Option<Arc<Collection>> {
        self.collections.get(name).map(|r| Arc::clone(r.value()))
    }

    pub fn list(&self) -> Vec<Arc<Collection>> {
        self.collections.iter().map(|r| Arc::clone(r.value())).collect()
    }

    pub fn delete(&self, name: &str) -> Result<Arc<Collection>, CollectionError> {
        self.collections
            .remove(name)
            .map(|(_, v)| v)
            .ok_or_else(|| CollectionError::NotFound(name.to_string()))
    }

    /// Change a collection's schema (Typesense's collection PATCH) and persist it.
    ///
    /// With field changes the collection is made again: every live document (each segment's, the
    /// segments in memory included, in `_seq_id` order) typed against the new schema and validated as a
    /// write is, so a document the new schema refuses fails the change with nothing changed
    /// (Typesense validates every document), then built by `build` into one segment. Without
    /// them the new collection keeps the data as it is.
    ///
    /// The change holds the collection as a flush does (a flush or segment merge in progress
    /// finishes first) and the writes of both collections until it is persisted, in this order:
    /// the new bundle, its manifest holding the new schema and its version and checkpointing the
    /// write-ahead log at the last write the old collection applied (one file replace, the
    /// change's commit; the log carries over: the new collection logs on under the new version);
    /// collections.json. A restart after the commit finishes the change from the manifest
    /// (`persistence::load_collections`): a record of the old version is skipped (the bundle
    /// holds its writes). A write that reaches the old collection after goes to the new one
    /// (`Collection::successor`), and saves of the old one stop.
    ///
    /// It saves on the calling thread, so not from an async task.
    pub fn change_schema(
        &self,
        name: &str,
        field_changes: Vec<crate::schema::FieldDef>,
        metadata: Option<serde_json::Value>,
        synonym_sets: Option<Vec<String>>,
        curation_sets: Option<Vec<String>>,
        build: impl FnOnce(&Schema, Vec<Document>) -> Index,
    ) -> Result<Arc<Collection>, CollectionError> {
        let old = self.get(name).ok_or_else(|| CollectionError::NotFound(name.to_string()))?;
        let (publishing, writing) = old.exclusive();
        if old.successor.get().is_some() {
            // another change replaced it while this one waited: change what it made
            drop((publishing, writing));
            return self.change_schema(name, field_changes, metadata, synonym_sets, curation_sets, build);
        }

        let mut new_fields = old.schema.fields.clone();
        for change in &field_changes {
            // Validate field name for add/modify operations
            if !change.drop && change.name.is_empty() {
                return Err(CollectionError::InvalidSchema(
                    "Field name cannot be empty.".into(),
                ));
            }

            if change.drop {
                // Remove field by name
                new_fields.retain(|f| f.name != change.name);
            } else if let Some(existing) = new_fields.iter_mut().find(|f| f.name == change.name) {
                // Modify existing field — reject immutable property changes
                if existing.field_type != change.field_type {
                    return Err(CollectionError::InvalidSchema(format!(
                        "Cannot change type of field `{}` from `{}` to `{}`. Drop and re-add the field instead.",
                        change.name, existing.field_type, change.field_type
                    )));
                }
                if existing.num_dim != change.num_dim {
                    return Err(CollectionError::InvalidSchema(format!(
                        "Cannot change `num_dim` of field `{}`. Drop and re-add the field instead.",
                        change.name
                    )));
                }
                if existing.vec_dist != change.vec_dist {
                    return Err(CollectionError::InvalidSchema(format!(
                        "Cannot change `vec_dist` of field `{}`. Drop and re-add the field instead.",
                        change.name
                    )));
                }
                // Apply mutable properties
                existing.facet = change.facet;
                existing.sort = change.sort;
                existing.index = change.index;
                existing.optional = change.optional;
                existing.stem = change.stem;
                existing.locale = change.locale.clone();
                existing.name_matching = change.name_matching;
            } else {
                // Add new field
                new_fields.push(change.clone());
            }
        }

        if new_fields.is_empty() {
            return Err(CollectionError::InvalidSchema(
                "Cannot drop all fields from a collection.".into(),
            ));
        }

        let mut schema = Schema {
            name: old.schema.name.clone(),
            fields: new_fields,
            default_sorting_field: old.schema.default_sorting_field.clone(),
            enable_nested_fields: old.schema.enable_nested_fields,
            symbols_to_index: old.schema.symbols_to_index.clone(),
            token_separators: old.schema.token_separators.clone(),
            default_language: old.schema.default_language,
            persistence_enabled: old.schema.persistence_enabled,
            index_mode: old.schema.index_mode,
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            metadata: metadata.or_else(|| old.schema.metadata.clone()),
            synonym_sets: synonym_sets.or_else(|| old.schema.synonym_sets.clone()),
            curation_sets: curation_sets.or_else(|| old.schema.curation_sets.clone()),
            registry: old.schema.registry.clone(),
        };
        schema.assign_field_ids();
        let schema = Arc::new(schema);

        let rebuilt = !field_changes.is_empty();
        let current = old.snapshot_bundle();
        let bundle = if rebuilt {
            let docs = old.live_documents_as(&schema, true).map_err(|e| CollectionError::InvalidSchema(alter_error(e)))?;
            let mut bundle = IndexBundle::single(build(&schema, docs), Arc::clone(&schema));
            // the rebuilt segment holds every write: the log's checkpoint once saved
            bundle.lsn = current.lsn;
            bundle
        } else {
            IndexBundle { schema: Arc::clone(&schema), ..(*current).clone() }
        };
        let new = Arc::new(Collection {
            name: old.name.clone(),
            schema: Arc::clone(&schema),
            index: ArcSwap::new(Arc::new(bundle)),
            created_at: old.created_at,
            schema_version: old.schema_version + u64::from(rebuilt),
            last_rebuilt_at: AtomicU64::new(if rebuilt { now_epoch() } else { old.last_rebuilt_at.load(Ordering::Relaxed) }),
            last_compaction_at: AtomicU64::new(old.last_compaction_at.load(Ordering::Relaxed)),
            last_save_at: AtomicU64::new(old.last_save_at.load(Ordering::Relaxed)),
            generation: AtomicU64::new(0),
            // the log goes on in the new collection (records after the change are typed under it)
            wal: std::sync::Mutex::new(old.wal()),
            logged_paths: AtomicUsize::new(old.logged_paths.load(Ordering::Relaxed)),
            data_dir: old.data_dir.clone(),
            compacting: AtomicBool::new(false),
            flush_waiting: AtomicBool::new(false),
            maintaining: AtomicBool::new(false),
            writes: std::sync::Mutex::new(()),
            queue: Default::default(),
            successor: OnceLock::new(),
            saves: Arc::clone(&old.saves),
        });

        let _new_writing = new.writes.lock().unwrap_or_else(|e| e.into_inner());
        let _saving = old.saves.lock().unwrap_or_else(|e| e.into_inner());
        let data_dir = self.data_dir.as_deref().filter(|_| schema.persistence_enabled);
        if let (true, Some(dir)) = (rebuilt, data_dir) {
            new.save_locked(dir).map_err(|e| CollectionError::Storage(format!("Could not save the collection: {e}")))?;
        }
        match self.collections.get_mut(name) {
            Some(mut entry) if Arc::ptr_eq(entry.value(), &old) => *entry = Arc::clone(&new),
            _ => return Err(CollectionError::NotFound(name.to_string())),
        }
        let _ = old.successor.set(Arc::clone(&new));
        old.wal.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(dir) = self.data_dir.as_deref() {
            if let Err(e) = crate::persistence::save_collections(dir, self) {
                tracing::error!(collection = name, "Failed to persist collections after a schema change: {e}");
            }
        }
        drop(publishing);
        // the writes of both collections go on as the guards drop
        Ok(Arc::clone(&new))
    }
}

/// A document's validation error as Typesense words it for a schema change.
fn alter_error(e: String) -> String {
    match e.strip_suffix("but is not found in the document.") {
        Some(head) => format!(
            "{head}but is not found in the documents already present in the collection. If you still want to add this field, set it as `optional: true`."
        ),
        None => format!("Schema change is incompatible with the type of documents already stored in this collection. error: {e}"),
    }
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{FieldDef, FieldType, FieldValue, Language};

    fn make_field(name: &str, field_type: FieldType) -> FieldDef {
        FieldDef {
            name: name.into(),
            field_type,
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
            reference: None,
            drop: false,
            stem_dictionary: None,
            embed: None,
            name_matching: false,
        }
    }

    fn make_schema(name: &str, fields: Vec<FieldDef>) -> Schema {
        Schema {
            name: name.into(),
            fields,
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        }
    }

    #[test]
    fn create_and_get() {
        let reg = CollectionRegistry::new(None);
        let schema = make_schema("products", vec![make_field("title", FieldType::String)]);
        let col = reg.create(schema).unwrap();
        assert_eq!(col.name, "products");
        assert_eq!(col.schema.fields.len(), 1);
        assert_eq!(col.schema.fields.get(0).unwrap().id, 0);

        let got = reg.get("products").unwrap();
        assert_eq!(got.name, "products");
    }

    #[test]
    fn create_duplicate_fails() {
        let reg = CollectionRegistry::new(None);
        let schema = make_schema("products", vec![make_field("title", FieldType::String)]);
        reg.create(schema).unwrap();

        let schema2 = make_schema("products", vec![make_field("title", FieldType::String)]);
        let err = reg.create(schema2).unwrap_err();
        assert!(matches!(err, CollectionError::AlreadyExists(_)));
    }

    #[test]
    fn delete_then_not_found() {
        let reg = CollectionRegistry::new(None);
        let schema = make_schema("products", vec![make_field("title", FieldType::String)]);
        reg.create(schema).unwrap();
        reg.delete("products").unwrap();
        assert!(reg.get("products").is_none());
    }

    #[test]
    fn delete_missing_fails() {
        let reg = CollectionRegistry::new(None);
        let err = reg.delete("nope").unwrap_err();
        assert!(matches!(err, CollectionError::NotFound(_)));
    }

    #[test]
    fn list_collections() {
        let reg = CollectionRegistry::new(None);
        reg.create(make_schema("a", vec![make_field("f", FieldType::String)])).unwrap();
        reg.create(make_schema("b", vec![make_field("f", FieldType::String)])).unwrap();
        let list = reg.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn merge_writes_keeps_each_written_document_in_its_place() {
        let doc = |id: &str, title: &str| Document {
            id: 0,
            external_id: id.into(),
            language: Language::default(),
            fields: vec![FieldValue::Text(title.into())].into(),
            record: None,
        };
        let live = vec![doc("a", "1"), doc("b", "1"), doc("c", "1")];
        let upserts = vec![doc("new", "1"), doc("b", "2"), doc("c", "2"), doc("b", "3"), doc("gone", "1")];
        let out = merge_writes(live, upserts, &["c".into(), "gone".into(), "zz".into()]);
        let ids: Vec<&str> = out.iter().map(|d| d.external_id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "new"]);
        assert!(matches!(out[1].fields.get(0), Some(FieldValue::Text(t)) if t == "3"), "the last version written");
    }

    #[test]
    fn patch_add_field() {
        let reg = CollectionRegistry::new(None);
        reg.create(make_schema("products", vec![make_field("title", FieldType::String)])).unwrap();

        let mut new_field = make_field("price", FieldType::Float64);
        new_field.optional = true;
        let updated = reg.change_schema("products", vec![new_field], None, None, None, build_index).unwrap();
        assert_eq!(updated.schema.fields.len(), 2);
        assert_eq!(updated.schema.fields.get(1).unwrap().name, "price");
        assert_eq!(updated.schema.fields.get(1).unwrap().id, 1);
        assert_eq!(updated.snapshot_bundle().total_docs(), 0);
    }

    #[test]
    fn patch_drop_field() {
        let reg = CollectionRegistry::new(None);
        reg.create(make_schema(
            "products",
            vec![
                make_field("title", FieldType::String),
                make_field("price", FieldType::Float64),
            ],
        )).unwrap();

        let mut drop = make_field("price", FieldType::Float64);
        drop.drop = true;
        let updated = reg.change_schema("products", vec![drop], None, None, None, build_index).unwrap();
        assert_eq!(updated.schema.fields.len(), 1);
        assert_eq!(updated.schema.fields.get(0).unwrap().name, "title");
    }

    #[test]
    fn patch_missing_collection() {
        let reg = CollectionRegistry::new(None);
        let err = reg.change_schema("nope", vec![], None, None, None, build_index).unwrap_err();
        assert!(matches!(err, CollectionError::NotFound(_)));
    }

    #[test]
    fn index_snapshot_and_swap() {
        let reg = CollectionRegistry::new(None);
        let col = reg.create(make_schema("x", vec![make_field("f", FieldType::String)])).unwrap();
        assert_eq!(col.snapshot().total_docs(), 0);

        let doc = Document {
            id: 0,
            external_id: "a".into(),
            language: Language::default(),
            fields: vec![FieldValue::Text("hello".into())].into(),
            record: None,
        };
        col.ingest_documents(vec![doc]);
        assert_eq!(col.snapshot().total_docs(), 1);
    }

    #[test]
    fn patch_modify_field_properties() {
        let reg = CollectionRegistry::new(None);
        let mut field = make_field("title", FieldType::String);
        field.sort = false;
        field.facet = false;
        reg.create(make_schema("products", vec![field])).unwrap();

        // Ingest a document so we can verify docs are returned
        let col = reg.get("products").unwrap();
        let doc = Document {
            id: 0,
            external_id: "1".into(),
            language: Language::default(),
            fields: vec![FieldValue::Text("hello".into())].into(),
            record: None,
        };
        col.ingest_documents(vec![doc]);
        assert_eq!(col.snapshot().total_docs(), 1);

        // Modify: enable sort and facet
        let mut modify = make_field("title", FieldType::String);
        modify.sort = true;
        modify.facet = true;
        let updated = reg.change_schema("products", vec![modify], None, None, None, build_index).unwrap();

        assert_eq!(updated.schema.fields.len(), 1);
        assert!(updated.schema.fields.get(0).unwrap().sort);
        assert!(updated.schema.fields.get(0).unwrap().facet);
        assert_eq!(updated.snapshot_bundle().total_docs(), 1, "the document rebuilt");
        assert!(updated.snapshot().values.contains_key(&0), "now faceted");
    }

    #[test]
    fn patch_modify_rejects_type_change() {
        let reg = CollectionRegistry::new(None);
        reg.create(make_schema("products", vec![make_field("title", FieldType::String)])).unwrap();

        // Try to change type from String to Int32
        let modify = make_field("title", FieldType::Int32);
        let err = reg.change_schema("products", vec![modify], None, None, None, build_index).unwrap_err();
        assert!(matches!(err, CollectionError::InvalidSchema(_)));
    }

    #[test]
    fn patch_rejects_empty_field_name() {
        let reg = CollectionRegistry::new(None);
        reg.create(make_schema("products", vec![make_field("title", FieldType::String)])).unwrap();

        let empty_field = make_field("", FieldType::String);
        let err = reg.change_schema("products", vec![empty_field], None, None, None, build_index).unwrap_err();
        assert!(matches!(err, CollectionError::InvalidSchema(_)));
    }

    #[test]
    fn patch_modify_empty_collection_returns_no_docs() {
        let reg = CollectionRegistry::new(None);
        let mut field = make_field("title", FieldType::String);
        field.sort = false;
        reg.create(make_schema("products", vec![field])).unwrap();

        // Modify on empty collection
        let mut modify = make_field("title", FieldType::String);
        modify.sort = true;
        let updated = reg.change_schema("products", vec![modify], None, None, None, build_index).unwrap();

        assert!(updated.schema.fields.get(0).unwrap().sort);
        assert_eq!(updated.snapshot_bundle().total_docs(), 0);
    }

    /// A realtime collection of three fields: two flushed segments (a document written again and
    /// one deleted since) and a write's segment in memory.
    fn layered(reg: &CollectionRegistry) -> Arc<Collection> {
        let mut schema = make_schema(
            "layers",
            vec![make_field("title", FieldType::String), make_field("tag", FieldType::String), make_field("n", FieldType::Int32)],
        );
        schema.persistence_enabled = false;
        schema.index_mode = crate::schema::IndexMode::Realtime;
        let col = reg.create(schema).unwrap();
        let doc = |id: &str, title: &str, n: i64| {
            let json = serde_json::json!({"id": id, "title": title, "tag": format!("t{n}"), "n": n});
            crate::ingest::parse_json_document(&json, &col.schema, 0).unwrap()
        };
        col.apply_writes(vec![doc("a", "first", 1), doc("b", "second", 2), doc("c", "third", 3)], &[]).unwrap();
        col.compact();
        col.apply_writes(vec![doc("d", "fourth", 4), doc("b", "second again", 20)], &["c".into()]).unwrap();
        col.compact();
        col.apply_writes(vec![doc("e", "fifth", 5), doc("a", "first again", 10)], &["d".into()]).unwrap();
        assert_eq!((col.snapshot_bundle().segment_count(), col.snapshot_bundle().memory), (3, 1));
        col
    }

    /// The documents a search of `*` finds, in `_seq_id` order, as (id, title, n).
    fn contents(col: &Collection) -> Vec<(String, String, i64)> {
        let mut out = Vec::new();
        for d in col.live_documents_as(&col.schema, false).unwrap() {
            let title = col.schema.name_to_id.get("title").and_then(|&f| match d.fields.get(f as usize) {
                Some(FieldValue::Text(t)) => Some(t.clone()),
                _ => None,
            });
            let n = col.schema.name_to_id.get("n").and_then(|&f| match d.fields.get(f as usize) {
                Some(FieldValue::Int32(n)) => Some(i64::from(*n)),
                _ => None,
            });
            out.push((d.external_id.clone(), title.unwrap_or_default(), n.unwrap_or(-1)));
        }
        out
    }

    #[test]
    fn a_schema_change_keeps_every_live_document_in_its_place() {
        let reg = CollectionRegistry::new(None);
        let col = layered(&reg);
        let want = vec![
            ("a".to_string(), "first again".to_string(), 10),
            ("b".to_string(), "second again".to_string(), 20),
            ("e".to_string(), "fifth".to_string(), 5),
        ];
        assert_eq!(contents(&col), want);
        // drop the field between the other two: their values stay theirs
        let mut drop = make_field("tag", FieldType::String);
        drop.drop = true;
        let new = reg.change_schema("layers", vec![drop], None, None, None, build_index).unwrap();
        assert_eq!(new.schema.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["title", "n"]);
        assert_eq!(contents(&new), want, "every segment's, in place");
        assert_eq!(new.snapshot_bundle().memory, 0, "the rebuilt segment holds the writes");
        assert!(Arc::ptr_eq(&reg.get("layers").unwrap(), &new));
        // a write reaching the old collection goes to the new one, typed against its schema
        let late = crate::ingest::parse_json_document(&serde_json::json!({"id": "f", "title": "late", "tag": "x", "n": 6}), &col.schema, 0).unwrap();
        col.apply_writes(vec![late], &["a".into()]).unwrap();
        let ids: Vec<String> = contents(&new).into_iter().map(|d| d.0).collect();
        assert_eq!(ids, ["b", "e", "f"]);
        assert_eq!(col.compact().docs_compacted, 0, "the old collection flushes nothing");
    }

    /// A schema change of a persistent collection saves the rebuilt segment set and the schema,
    /// checkpoints the log past the writes the segment now holds and logs on: a restart finds
    /// every document typed against the new schema, and the writes made after.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_schema_change_persists_and_logs_on() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(CollectionRegistry::new(Some(dir.path().to_path_buf())));
        let mut schema = make_schema(
            "pers",
            vec![make_field("title", FieldType::String), make_field("tag", FieldType::String), make_field("n", FieldType::Int32)],
        );
        schema.index_mode = crate::schema::IndexMode::Realtime;
        let col = reg.create(schema).unwrap();
        let doc = |col: &Collection, id: &str, title: &str, n: i64| {
            crate::ingest::parse_json_document(&serde_json::json!({"id": id, "title": title, "tag": "t", "n": n}), &col.schema, 0).unwrap()
        };
        let metrics = crate::observability::MetricsCollector::new();
        col.apply_writes(vec![doc(&col, "a", "first", 1), doc(&col, "b", "second", 2), doc(&col, "c", "third", 3)], &[]).unwrap();
        compaction::flush(&col, Some(dir.path()), &metrics);
        col.apply_writes(vec![doc(&col, "d", "fourth", 4), doc(&col, "b", "second again", 20)], &["c".into()]).unwrap();
        col.wal_barrier().await;

        let mut drop = make_field("tag", FieldType::String);
        drop.drop = true;
        let changing = Arc::clone(&reg);
        let new = tokio::task::spawn_blocking(move || changing.change_schema("pers", vec![drop], None, None, None, build_index))
            .await
            .unwrap()
            .unwrap();
        new.apply_writes(vec![doc(&new, "e", "late", 5)], &[]).unwrap();
        new.wal_barrier().await;
        new.close_wal();

        let restarted = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        crate::persistence::load_collections(dir.path(), &restarted, None).unwrap();
        let back = restarted.get("pers").unwrap();
        assert_eq!(back.schema.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["title", "n"]);
        let want = [("a", "first", 1), ("b", "second again", 20), ("d", "fourth", 4), ("e", "late", 5)];
        assert_eq!(contents(&back), want.map(|(i, t, n)| (i.to_string(), t.to_string(), n)));
        assert_eq!(back.snapshot_bundle().memory_docs(), 1, "only the write after the change is replayed");
    }

    /// A restart right after a schema change committed (its bundle's manifest saved), the rest of
    /// its saves cut short: collections.json still of the old schema, the log still holding the
    /// writes typed under the old fields.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_restart_finishes_a_schema_change_a_crash_cut_short() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(CollectionRegistry::new(Some(dir.path().to_path_buf())));
        let mut schema = make_schema(
            "pers",
            vec![make_field("title", FieldType::String), make_field("tag", FieldType::String), make_field("n", FieldType::Int32)],
        );
        schema.index_mode = crate::schema::IndexMode::Realtime;
        let col = reg.create(schema).unwrap();
        let doc = |id: &str, title: &str, n: i64| {
            crate::ingest::parse_json_document(&serde_json::json!({"id": id, "title": title, "tag": "t", "n": n}), &col.schema, 0).unwrap()
        };
        let metrics = crate::observability::MetricsCollector::new();
        col.apply_writes(vec![doc("a", "first", 1), doc("b", "second", 2), doc("c", "third", 3)], &[]).unwrap();
        compaction::flush(&col, Some(dir.path()), &metrics);
        col.apply_writes(vec![doc("d", "fourth", 4), doc("b", "second again", 20)], &["c".into()]).unwrap();
        // the log as the crash leaves it: writes under the old schema
        col.close_wal();
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        let before = std::fs::read(dir.path().join("collections.json")).unwrap();

        let mut drop = make_field("tag", FieldType::String);
        drop.drop = true;
        let changing = Arc::clone(&reg);
        let new = tokio::task::spawn_blocking(move || changing.change_schema("pers", vec![drop], None, None, None, build_index))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(new.schema_version, 1);
        std::fs::write(dir.path().join("collections.json"), &before).unwrap();

        let want = [("a", "first", 1), ("b", "second again", 20), ("d", "fourth", 4)].map(|(i, t, n)| (i.to_string(), t.to_string(), n));
        for restart in 0..2 {
            let restarted = CollectionRegistry::new(Some(dir.path().to_path_buf()));
            crate::persistence::load_collections(dir.path(), &restarted, None).unwrap();
            let back = restarted.get("pers").unwrap();
            assert_eq!(back.schema.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["title", "n"], "restart {restart}");
            assert_eq!(back.schema_version, 1);
            assert_eq!(contents(&back), want, "the rebuilt segments, under the schema they were saved under");
            assert_eq!(back.snapshot_bundle().memory, 0, "the old log's writes are the segments'");
            back.close_wal();
        }
        // collections.json finished too
        let persisted = crate::persistence::load_persisted(dir.path(), "pers").unwrap().unwrap();
        assert_eq!((persisted.schema_version, persisted.schema.fields.len()), (1, 2));
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join("collections.json")).unwrap()).unwrap();
        assert_eq!(saved[0]["schema_version"], 1);
        assert_eq!(saved[0]["fields"].as_array().unwrap().len(), 2);
    }

    fn realtime(reg: &CollectionRegistry, name: &str) -> Arc<Collection> {
        let mut schema = make_schema(name, vec![make_field("title", FieldType::String), make_field("n", FieldType::Int32)]);
        schema.index_mode = crate::schema::IndexMode::Realtime;
        schema.persistence_enabled = true;
        reg.create(schema).unwrap()
    }

    fn rt_doc(col: &Collection, id: &str, title: &str, n: i64) -> Document {
        crate::ingest::parse_json_document(&serde_json::json!({"id": id, "title": title, "n": n}), &col.schema, 0).unwrap()
    }

    /// Apply a write and wait until its log has it on disk.
    async fn durable_write(col: &Collection, docs: Vec<Document>, deletes: &[&str]) {
        let deletes: Vec<String> = deletes.iter().map(|d| d.to_string()).collect();
        col.apply_writes(docs, &deletes).unwrap().expect("a logged collection").wait().await.unwrap();
    }

    fn restart(dir: &std::path::Path, name: &str) -> Arc<Collection> {
        let restarted = CollectionRegistry::new(Some(dir.to_path_buf()));
        crate::persistence::load_collections(dir, &restarted, None).unwrap();
        restarted.get(name).unwrap()
    }

    fn ids(col: &Collection) -> Vec<String> {
        contents(col).into_iter().map(|d| d.0).collect()
    }

    /// Every acknowledged write is on disk: a restart with nothing saved replays them all from
    /// the log, and the log goes on numbering after them.
    #[tokio::test(flavor = "multi_thread")]
    async fn acknowledged_writes_survive_a_restart_with_nothing_saved() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let col = realtime(&reg, "rt");
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        durable_write(&col, vec![rt_doc(&col, "a", "one", 1), rt_doc(&col, "b", "two", 2)], &[]).await;
        durable_write(&col, vec![rt_doc(&col, "c", "three", 3)], &["a"]).await;
        durable_write(&col, vec![rt_doc(&col, "b", "two again", 20)], &[]).await;
        col.close_wal();

        let back = restart(dir.path(), "rt");
        assert_eq!(contents(&back), [("b".to_string(), "two again".to_string(), 20), ("c".to_string(), "three".to_string(), 3)]);
        assert_eq!(back.snapshot_bundle().lsn, 3);
        durable_write(&back, vec![rt_doc(&back, "d", "four", 4)], &[]).await;
        assert_eq!(back.snapshot_bundle().lsn, 4, "numbering goes on after the replayed records");
        back.close_wal();
        assert_eq!(ids(&restart(dir.path(), "rt")), ["b", "c", "d"]);
    }

    /// A write the log replays after a restart is its document as written: every path, those
    /// outside the schema and the unindexed ones, and a path the field registry gained after the
    /// last save (the log record carries it; the manifest holds those before).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_replayed_write_keeps_every_path_of_its_document() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let mut hidden = make_field("hidden", FieldType::String);
        hidden.index = false;
        hidden.optional = true;
        let mut schema = make_schema("paths", vec![make_field("title", FieldType::String), hidden]);
        schema.index_mode = crate::schema::IndexMode::Realtime;
        let col = crate::persistence::create_collection(dir.path(), &reg, schema).unwrap();
        let doc = |col: &Collection, v: &serde_json::Value| crate::ingest::parse_json_document(v, &col.schema, 0).unwrap();
        let a = serde_json::json!({"id": "a", "title": "one", "hidden": "h", "note": {"x": 1, "y": [1.5, "z"]}});
        durable_write(&col, vec![doc(&col, &a)], &[]).await;
        // saved, the log checkpointed past `a`
        compaction::flush(&col, Some(dir.path()), &crate::observability::MetricsCollector::new());
        col.wal_barrier().await;
        let b = serde_json::json!({"id": "b", "title": "two", "added": [true, null]});
        let c = serde_json::json!({"id": "c", "title": "three", "note": {"x": 2}});
        durable_write(&col, vec![doc(&col, &b), doc(&col, &c)], &[]).await;
        col.close_wal();
        let back = restart(dir.path(), "paths");
        assert_eq!(back.snapshot_bundle().memory_docs(), 2, "b and c replayed");
        for (id, want) in [("a", a), ("b", b), ("c", c)] {
            let got = crate::index::document_to_json(&back.document(id).unwrap(), &back.schema);
            assert_eq!(got, want, "{id}");
        }
        back.close_wal();
    }

    /// Replayed, the writes give the order they gave before: a document written again keeps its
    /// place, one deleted and written again is new.
    #[tokio::test(flavor = "multi_thread")]
    async fn recovery_keeps_the_write_order() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let col = realtime(&reg, "order");
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        let metrics = crate::observability::MetricsCollector::new();
        durable_write(&col, vec![rt_doc(&col, "a", "1", 1), rt_doc(&col, "b", "1", 1), rt_doc(&col, "c", "1", 1)], &[]).await;
        compaction::flush(&col, Some(dir.path()), &metrics);
        durable_write(&col, vec![rt_doc(&col, "d", "1", 1)], &["b"]).await;
        durable_write(&col, vec![rt_doc(&col, "b", "2", 2), rt_doc(&col, "a", "2", 2)], &[]).await;
        let before = ids(&col);
        assert_eq!(before, ["a", "c", "d", "b"]);
        col.close_wal();
        assert_eq!(ids(&restart(dir.path(), "order")), before);
    }

    /// A truncation is a record of its own: a restart empties the saved segments too, and keeps
    /// the writes after it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_truncation_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let col = realtime(&reg, "tr");
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        let metrics = crate::observability::MetricsCollector::new();
        durable_write(&col, vec![rt_doc(&col, "a", "1", 1), rt_doc(&col, "b", "1", 1)], &[]).await;
        compaction::flush(&col, Some(dir.path()), &metrics);
        durable_write(&col, vec![rt_doc(&col, "c", "1", 1)], &[]).await;
        col.truncate().unwrap().unwrap().wait().await.unwrap();
        durable_write(&col, vec![rt_doc(&col, "d", "1", 1)], &[]).await;
        assert_eq!(ids(&col), ["d"]);
        col.close_wal();

        let back = restart(dir.path(), "tr");
        assert_eq!(ids(&back), ["d"]);
        let bundle = back.snapshot_bundle();
        assert_eq!((bundle.segments.len(), bundle.memory), (1, 1), "the saved segments are gone, the write after replayed");
    }

    /// A flush waits for a publisher holding the collection (a merge stops for it), then flushes.
    #[test]
    fn a_flush_waits_for_a_merge_under_way() {
        let reg = CollectionRegistry::new(None);
        let col = realtime(&reg, "wait");
        col.apply_writes(vec![rt_doc(&col, "a", "x", 1)], &[]).unwrap();
        let held = col.try_begin_publish().expect("uncontended");
        let flushing = Arc::clone(&col);
        let flusher = std::thread::spawn(move || flushing.compact().docs_compacted);
        std::thread::sleep(Duration::from_millis(30));
        assert!(col.flush_waiting.load(Ordering::Relaxed), "the flush waits");
        assert_eq!(col.merge_memory(), 0, "a merge does not start meanwhile");
        drop(held);
        assert_eq!(flusher.join().unwrap(), 1);
        assert_eq!(col.snapshot_bundle().memory, 0);
    }

    /// A flushed segment whose deleted copies reach `write::RECLAIM_SHARE` of it is merged on its
    /// own, which drops them: the documents as they were, saved, and so after a restart.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_segment_with_many_deleted_copies_is_merged_alone() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let col = realtime(&reg, "reclaim");
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        durable_write(&col, (0..20).map(|i| rt_doc(&col, &format!("d{i}"), "word", i)).collect(), &[]).await;
        assert_eq!(col.compact().docs_compacted, 20);
        // three of twenty written again: 15%
        durable_write(&col, (0..3).map(|i| rt_doc(&col, &format!("d{i}"), "again", 100 + i)).collect(), &[]).await;
        assert!(!col.wants_reclaim());
        // and one deleted: 20%
        durable_write(&col, vec![], &["d5"]).await;
        assert!(col.wants_reclaim());
        let before = contents(&col);
        let metrics = crate::observability::MetricsCollector::new();
        assert_eq!(compaction::reclaim(&col, Some(dir.path()), &metrics), 1);
        assert!(!col.wants_reclaim());
        let b = col.snapshot_bundle();
        assert_eq!((b.segments[0].total_docs(), b.deleted_for(0).unwrap().cardinality(), b.memory), (16, 0, 1));
        assert_eq!(contents(&col), before);
        col.close_wal();
        assert_eq!(contents(&restart(dir.path(), "reclaim")), before);
    }

    /// A save puts the segments it wrote, read from their files (mapped), in place of those built
    /// in memory: the same answers; a merge of the ones it replaced, begun before, still
    /// publishes (segments are known by id); a restart reads the same.
    #[tokio::test(flavor = "multi_thread")]
    async fn saved_segments_serve_from_their_files() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let col = realtime(&reg, "mapped");
        for i in 0..6 {
            durable_write(&col, vec![rt_doc(&col, &format!("d{i}"), "word", i)], &[]).await;
        }
        durable_write(&col, vec![rt_doc(&col, "d1", "again", 10)], &["d3"]).await;
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        let before = contents(&col);
        let snapshot = col.snapshot_bundle();
        assert!(snapshot.segments.iter().all(|s| !s.is_mapped()));

        col.save(dir.path()).unwrap();
        let after = col.snapshot_bundle();
        assert_eq!(after.segments.len(), snapshot.segments.len());
        assert!(after.segments.iter().all(|s| s.is_mapped() && s.is_saved()));
        assert_eq!(contents(&col), before);
        // nothing changed since: the next save writes nothing and maps nothing
        let again = crate::persistence::index::save_bundle(dir.path(), "mapped", &after, &col.schema, col.stamp()).unwrap();
        assert_eq!((again.segments_written, again.masks_written, again.mapped.len()), (0, 0, 0));

        let range = 0..snapshot.segments.len();
        let merged = write::merge(&snapshot, range.clone()).unwrap();
        assert!(col.publish_merge(&snapshot, range, merged, false));
        assert_eq!(col.snapshot_bundle().segments.len(), 1);
        assert_eq!(contents(&col), before);
        col.save(dir.path()).unwrap();
        col.close_wal();
        assert_eq!(contents(&restart(dir.path(), "mapped")), before);
    }

    /// A schema change saves its rebuilt segments without mapping them (it holds the writes): the
    /// next save maps them.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_save_maps_segments_an_earlier_save_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let reg = Arc::new(CollectionRegistry::new(Some(dir.path().to_path_buf())));
        let col = realtime(&reg, "later");
        durable_write(&col, vec![rt_doc(&col, "a", "x", 1), rt_doc(&col, "b", "y", 2)], &[]).await;
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        let mut extra = make_field("extra", FieldType::String);
        extra.optional = true;
        let changing = Arc::clone(&reg);
        let new = tokio::task::spawn_blocking(move || changing.change_schema("later", vec![extra], None, None, None, build_index))
            .await
            .unwrap()
            .unwrap();
        let rebuilt = new.snapshot_bundle();
        assert!(rebuilt.segments.iter().all(|s| s.is_saved() && !s.is_mapped()));
        new.save(dir.path()).unwrap();
        assert!(new.snapshot_bundle().segments.iter().all(|s| s.is_mapped()));
        assert_eq!(ids(&new), ["a", "b"]);
        new.close_wal();
    }

    /// Writers applied in groups (the writer holding the lock takes those queued behind it): each
    /// id's writes still apply in its writer's order, live and replayed from the log.
    #[tokio::test(flavor = "multi_thread")]
    async fn writes_applied_in_groups_keep_each_writers_order() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let col = realtime(&reg, "grp");
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        let threads: Vec<_> = (0..16)
            .map(|t| {
                let col = Arc::clone(&col);
                std::thread::spawn(move || {
                    for i in 0..20 {
                        let (a, b) = (format!("a{t}-{i}"), format!("b{t}-{i}"));
                        let w = |docs: Vec<Document>, deletes: &[&String]| {
                            let deletes: Vec<String> = deletes.iter().map(|d| d.to_string()).collect();
                            col.apply_writes(docs, &deletes).unwrap().unwrap().wait_blocking().unwrap();
                        };
                        w(vec![rt_doc(&col, &a, "first", 1), rt_doc(&col, &b, "first", 1)], &[]);
                        w(vec![], &[&a]);
                        w(vec![rt_doc(&col, &a, "again", 2)], &[&b]);
                        w(vec![rt_doc(&col, &b, "late", 3)], &[]);
                        w(vec![], &[&b]);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let check = |c: &Collection| {
            let docs = contents(c);
            assert_eq!(docs.len(), 16 * 20, "every a, no b");
            assert!(docs.iter().all(|d| d.0.starts_with('a') && d.1 == "again" && d.2 == 2));
        };
        check(&col);
        // 960 of the writes carry documents: grouped, fewer segments
        assert!(col.snapshot_bundle().segments.len() < 16 * 20 * 3, "writes shared segments");
        col.close_wal();
        check(&restart(dir.path(), "grp"));
    }

    /// A log damaged before its last record is not replayed past the damage: the collection keeps
    /// its saved segments and refuses writes until the log is repaired.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_damaged_log_refuses_writes() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
        let col = realtime(&reg, "dmg");
        crate::persistence::save_collections(dir.path(), &reg).unwrap();
        durable_write(&col, vec![rt_doc(&col, "a", "1", 1)], &[]).await;
        col.wal().unwrap().checkpoint(0);
        durable_write(&col, vec![rt_doc(&col, "b", "1", 1)], &[]).await;
        col.close_wal();
        let wal_dir = crate::persistence::wal::wal_dir(dir.path(), "dmg");
        let mut logs: Vec<_> = std::fs::read_dir(&wal_dir).unwrap().map(|e| e.unwrap().path()).collect();
        logs.sort();
        assert_eq!(logs.len(), 2);
        let mut data = std::fs::read(&logs[0]).unwrap();
        let last = data.len() - 1;
        data[last] ^= 0xff;
        std::fs::write(&logs[0], data).unwrap();

        let back = restart(dir.path(), "dmg");
        assert!(back.wal().is_none());
        assert!(matches!(back.apply_writes(vec![rt_doc(&back, "c", "1", 1)], &[]), Err(WriteError::Log(_))));
        assert!(ids(&back).is_empty(), "nothing replayed past the damage");
    }

    #[test]
    fn a_schema_change_the_documents_refuse_changes_nothing() {
        let reg = CollectionRegistry::new(None);
        let col = layered(&reg);
        let before = contents(&col);
        // a required field the documents do not have
        let err = reg.change_schema("layers", vec![make_field("price", FieldType::Int32)], None, None, None, build_index).unwrap_err();
        assert!(matches!(err, CollectionError::InvalidSchema(_)), "{err}");
        assert!(Arc::ptr_eq(&reg.get("layers").unwrap(), &col));
        assert!(col.successor.get().is_none());
        assert_eq!(contents(&col), before);
        // metadata alone keeps the data as it is, the segments in memory included
        let new = reg.change_schema("layers", vec![], Some(serde_json::json!({"k": 1})), None, None, build_index).unwrap();
        assert_eq!(new.schema.metadata, Some(serde_json::json!({"k": 1})));
        assert_eq!((new.snapshot_bundle().segment_count(), new.snapshot_bundle().memory), (3, 1));
        assert_eq!(contents(&new), before);
    }

    fn doc(ext_id: &str, body: &str) -> Document {
        Document {
            id: 0,
            external_id: ext_id.into(),
            language: Language::default(),
            fields: vec![FieldValue::Text(body.into())].into(),
            record: None,
        }
    }

    fn make_realtime_collection() -> Arc<Collection> {
        let reg = CollectionRegistry::new(None);
        let mut schema = make_schema("rt", vec![make_field("body", FieldType::Text)]);
        schema.persistence_enabled = false;
        schema.index_mode = crate::schema::IndexMode::Realtime;
        reg.create(schema).unwrap()
    }

    #[test]
    fn flush_replaces_empty_placeholder_then_appends() {
        let col = make_realtime_collection();

        // Initial: bundle is the empty placeholder (one segment, zero docs).
        assert_eq!(col.snapshot_bundle().segment_count(), 1);
        assert_eq!(col.snapshot_bundle().total_docs(), 0);

        // First flush replaces the placeholder, keeping segment count at 1.
        col.apply_upsert(doc("a", "alpha"));
        col.compact();
        assert_eq!(col.snapshot_bundle().segment_count(), 1);
        assert_eq!(col.snapshot_bundle().total_docs(), 1);

        // Second flush appends a NEW segment instead of rewriting.
        col.apply_upsert(doc("b", "beta"));
        col.compact();
        assert_eq!(col.snapshot_bundle().segment_count(), 2);
        assert_eq!(col.snapshot_bundle().total_docs(), 2);

        // Third flush appends another.
        col.apply_upsert(doc("c", "gamma"));
        col.compact();
        assert_eq!(col.snapshot_bundle().segment_count(), 3);
        assert_eq!(col.snapshot_bundle().total_docs(), 3);
    }

    #[test]
    fn flush_marks_obsoleted_doc_in_old_segment() {
        let col = make_realtime_collection();

        // Two flushes: doc "a" lives in segment 0, doc "b" in segment 1.
        col.apply_upsert(doc("a", "v1"));
        col.compact();
        col.apply_upsert(doc("b", "beta"));
        col.compact();
        let bundle = col.snapshot_bundle();
        assert_eq!(bundle.segment_count(), 2);
        assert_eq!(bundle.total_docs(), 2);
        assert!(bundle.deleted_for(0).map(|bm| bm.is_empty()).unwrap_or(true));

        // Re-upsert "a" — this should obsolete the copy in segment 0 and add
        // a fresh copy in segment 2.
        col.apply_upsert(doc("a", "v2"));
        col.compact();
        let bundle = col.snapshot_bundle();
        assert_eq!(bundle.segment_count(), 3);
        // segment 0's "a" must now be in the deletion mask
        let mask0 = bundle.deleted_for(0).expect("segment 0 has a deletion mask");
        let local_a = bundle.segments[0].ext_id_lookup("a").expect("segment 0 has 'a'");
        assert!(mask0.contains(local_a));
        // Live total: 2 (b in seg 1, a in seg 2). Segment 0's a is masked.
        assert_eq!(bundle.total_docs(), 2);
    }

    #[test]
    fn search_filters_obsoleted_docs_across_segments() {
        // End-to-end Lucene-style live-docs proof: build a multi-segment
        // bundle by flushing realtime writes several times, then verify
        // search through `execute_with_stores` returns only the live copies.
        use crate::search;

        let col = make_realtime_collection();

        // Segment 0: doc "a" (v1) — will later be obsoleted
        col.apply_upsert(doc("a", "alpha v1"));
        col.compact();
        // Segment 1: doc "b"
        col.apply_upsert(doc("b", "beta"));
        col.compact();
        // Segment 2: doc "a" (v2) obsoletes the copy in segment 0; doc "c" added
        col.apply_upsert(doc("a", "alpha v2"));
        col.apply_upsert(doc("c", "gamma"));
        col.compact();

        let bundle = col.snapshot_bundle();
        assert_eq!(bundle.segment_count(), 3);
        // Live count: a (latest), b, c = 3. Segment 0's obsolete a is masked.
        assert_eq!(bundle.total_docs(), 3);

        // Wildcard search returns only live docs.
        let mut params = search::SearchParams {
            q: "*".into(),
            query_by: vec!["body".into()],
            per_page: 10,
            page: 1,
            ..Default::default()
        };
        let resp = search::execute_with_stores(&bundle, &col.schema, &params, None, None).unwrap();
        let result = match resp {
            search::SearchResponse::Ungrouped(r) => r,
            _ => panic!("expected ungrouped"),
        };
        assert_eq!(result.found, 3, "wildcard must return 3 live docs");
        assert_eq!(result.out_of, 3, "out_of reflects live total");
        let mut ids: Vec<String> = result
            .hits
            .iter()
            .map(|h| h.document["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);

        // Search for the v1 token — should NOT find anything because the
        // only doc that ever contained it is now masked.
        params.q = "v1".into();
        let resp = search::execute_with_stores(&bundle, &col.schema, &params, None, None).unwrap();
        let result = match resp {
            search::SearchResponse::Ungrouped(r) => r,
            _ => panic!("expected ungrouped"),
        };
        assert_eq!(result.found, 0, "obsolete v1 token must be hidden");

        // Search for v2 — should find the live copy of "a" in segment 2.
        params.q = "v2".into();
        let resp = search::execute_with_stores(&bundle, &col.schema, &params, None, None).unwrap();
        let result = match resp {
            search::SearchResponse::Ungrouped(r) => r,
            _ => panic!("expected ungrouped"),
        };
        assert_eq!(result.found, 1);
        assert_eq!(result.hits[0].document["id"].as_str().unwrap(), "a");
    }

    #[test]
    fn flush_pure_tombstone_marks_old_segment_only() {
        let col = make_realtime_collection();

        // Flush a doc into segment 0
        col.apply_upsert(doc("a", "alpha"));
        col.compact();
        assert_eq!(col.snapshot_bundle().segment_count(), 1);

        // A delete alone, then a flush: segment 0's "a" is masked.
        col.apply_delete("a");
        col.compact();
        let bundle = col.snapshot_bundle();
        assert_eq!(bundle.segment_count(), 1, "no new segment for pure-delete flush");
        let mask = bundle.deleted_for(0).expect("segment 0 has mask");
        let local_a = bundle.segments[0].ext_id_lookup("a").expect("segment 0 still has 'a'");
        assert!(mask.contains(local_a));
        assert_eq!(bundle.total_docs(), 0);
    }
}
