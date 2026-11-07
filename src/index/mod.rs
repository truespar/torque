pub mod docstore;
pub mod facet;
pub mod geo;
pub mod inverted;
pub mod merge;
pub mod reference;
pub mod sort;
pub mod values;

use crate::schema::{DocId, Document, FieldId, FieldValue, Schema};
pub use docstore::DocStore;

use crate::vector::VectorIndex;
use geo::GeoIndex;
use inverted::InvertedIndex;
use reference::ReferenceIndex;
use values::{BoolIndex, ValueIndex};
use crate::bitmap::Bitmap;
use rustc_hash::FxHashMap;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// The next segment id: above every one this process gave out or loaded (`note_segment_id`),
/// begun from the clock (microseconds) so a restarted process begins above its predecessor's.
static NEXT_SEGMENT_ID: AtomicU64 = AtomicU64::new(0);

/// A new segment's id (`Segment::id`), unique among a data directory's segments.
pub fn next_segment_id() -> u64 {
    if NEXT_SEGMENT_ID.load(Ordering::Relaxed) == 0 {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_micros() as u64);
        NEXT_SEGMENT_ID.fetch_max(now, Ordering::Relaxed);
    }
    NEXT_SEGMENT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A segment loaded with `id`: ids given out after are above it.
pub fn note_segment_id(id: u64) {
    NEXT_SEGMENT_ID.fetch_max(id + 1, Ordering::Relaxed);
}

/// Heap bytes of a bitmap (0 when it is read in place from a segment file).
#[inline]
pub(crate) fn roaring_heap_bytes(bm: &Bitmap) -> usize {
    std::mem::size_of::<Bitmap>() + bm.heap_bytes()
}

/// One immutable, self-contained mini-index covering a contiguous range of
/// document ids. A `Collection` holds a vector of these.
pub struct Segment {
    /// Global doc id of this segment's local doc 0: the document count of the
    /// segments before it.
    pub base_doc_id: DocId,

    /// Stored documents (TQBF FIELDS + DOCS + IDS): hits, GET by id and external id lookups.
    pub doc_store: DocStore,

    /// Per-field text indexes (only for searchable+indexed fields), read in place from the
    /// segment's TERMS/RAWTERMS/POSTINGS/FREQS/NORMS sections.
    pub inverted: FxHashMap<FieldId, InvertedIndex>,

    /// Per-field value indexes of string and numeric fields (VALUES or NUM_VALUES, VALUE_DOCS,
    /// DOC_ORDS): exact and range filters, facets, sorting and group_by.
    pub values: FxHashMap<FieldId, ValueIndex>,
    /// Per-field bool indexes (BOOL).
    pub bools: FxHashMap<FieldId, BoolIndex>,

    /// Per-field geo indexes of geopoint and geopoint[] fields (GEO sections).
    pub geo_indexes: FxHashMap<FieldId, GeoIndex>,

    /// Per-field vector indexes of float[] fields with num_dim (VECTORS sections).
    pub vector_indexes: FxHashMap<FieldId, VectorIndex>,

    /// Bitmap of all valid doc IDs.
    pub all_docs: Bitmap,

    /// Per-field reference indexes for JOIN operations: views of the reference fields' value
    /// sections.
    pub reference_indexes: FxHashMap<FieldId, ReferenceIndex>,

    /// The documents' `_seq_id`s where they are not `base_doc_id + doc`: those of a segment the
    /// realtime write path built (a write, or a merge of such segments), each document's place in
    /// the order documents were first written, which writing it again keeps (Typesense keeps a
    /// document's sequence id). Saved in the segment's file (SEQ); empty for every other segment.
    pub seq: SeqIds,

    /// The segment's id (`next_segment_id`), its file's name once saved
    /// (`persistence::index`): a segment is written once, and a save keeps the file of every
    /// segment it already wrote. Merges know a segment by it (a saved segment is replaced by the
    /// same, read from its file).
    pub id: u64,
    /// Its file holds it: read from it, or written by a save.
    saved: AtomicBool,
    /// Read from its file (mapped), not built in memory.
    mapped: bool,
    /// Per text field the tokens and the documents with tokens of the deleted copies, and how many
    /// copies those are (`deleted_lengths`).
    deleted_lengths: std::sync::Mutex<FxHashMap<FieldId, (u64, u64, u64)>>,
}

/// A segment's `_seq_id`s in runs of consecutive ids: run `k` holds the documents from
/// `docs[k]` on (`docs[0] == 0`, ascending), with ids from `ids[k]` on, each next document the
/// next id. A segment's documents are in `_seq_id` order, so a merged segment, whose ids are
/// consecutive but for the documents deleted or written again since, takes a few runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeqIds {
    pub docs: Vec<u32>,
    pub ids: Vec<u64>,
}

impl SeqIds {
    /// The runs of the ids of a segment's documents, in document order.
    pub fn from_ids(ids: impl IntoIterator<Item = u64>) -> Self {
        let mut runs = Self::default();
        let mut prev: Option<u64> = None;
        for (doc, id) in ids.into_iter().enumerate() {
            if prev.is_none_or(|p| id != p + 1) {
                runs.docs.push(doc as u32);
                runs.ids.push(id);
            }
            prev = Some(id);
        }
        runs
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// `[u32 runs][runs × u32 first doc][runs × u64 first id]`, little-endian (TQBF SEQ section).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.docs.len() * 12);
        out.extend_from_slice(&(self.docs.len() as u32).to_le_bytes());
        for d in &self.docs {
            out.extend_from_slice(&d.to_le_bytes());
        }
        for i in &self.ids {
            out.extend_from_slice(&i.to_le_bytes());
        }
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        let n = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize;
        if b.len() != 4 + n * 12 {
            return None;
        }
        let docs = b[4..4 + n * 4].as_chunks::<4>().0.iter().map(|&c| u32::from_le_bytes(c)).collect();
        let ids = b[4 + n * 4..].as_chunks::<8>().0.iter().map(|&c| u64::from_le_bytes(c)).collect();
        Some(Self { docs, ids })
    }

    /// Document `doc`'s id (None: the segment keeps no ids).
    #[inline]
    pub fn get(&self, doc: DocId) -> Option<u64> {
        if self.docs.is_empty() {
            return None;
        }
        let k = self.docs.partition_point(|&d| d <= doc) - 1;
        Some(self.ids[k] + u64::from(doc - self.docs[k]))
    }
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segment")
            .field("base_doc_id", &self.base_doc_id)
            .field("num_docs", &self.total_docs())
            .finish()
    }
}

/// The build path's name for a segment.
pub type Index = Segment;

impl Segment {
    pub fn empty() -> Self {
        Self {
            base_doc_id: 0,
            doc_store: DocStore::empty(),
            inverted: FxHashMap::default(),
            values: FxHashMap::default(),
            bools: FxHashMap::default(),
            geo_indexes: FxHashMap::default(),
            vector_indexes: FxHashMap::default(),
            all_docs: Bitmap::new(),
            reference_indexes: FxHashMap::default(),
            seq: SeqIds::default(),
            id: next_segment_id(),
            saved: AtomicBool::new(false),
            mapped: false,
            deleted_lengths: Default::default(),
        }
    }

    /// A segment of these parts, as a build or a read of its file makes it (`mapped`: read from
    /// it, so saved).
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        doc_store: DocStore,
        inverted: FxHashMap<FieldId, InvertedIndex>,
        values: FxHashMap<FieldId, ValueIndex>,
        bools: FxHashMap<FieldId, BoolIndex>,
        geo_indexes: FxHashMap<FieldId, GeoIndex>,
        vector_indexes: FxHashMap<FieldId, VectorIndex>,
        all_docs: Bitmap,
        reference_indexes: FxHashMap<FieldId, ReferenceIndex>,
        seq: SeqIds,
        id: u64,
        mapped: bool,
    ) -> Self {
        Self {
            base_doc_id: 0,
            doc_store,
            inverted,
            values,
            bools,
            geo_indexes,
            vector_indexes,
            all_docs,
            reference_indexes,
            seq,
            id,
            saved: AtomicBool::new(mapped),
            mapped,
            deleted_lengths: Default::default(),
        }
    }

    /// The tokens of field `field` in the `deleted` documents and how many of those have tokens,
    /// for text statistics of the live documents. A segment's deletions only grow, so their number
    /// tells its masks apart, and each is summed once.
    pub fn deleted_lengths(&self, field: FieldId, inv: &InvertedIndex, deleted: &Bitmap) -> (u64, u64) {
        let count = deleted.cardinality();
        let mut memo = self.deleted_lengths.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&(n, tokens, docs)) = memo.get(&field)
            && n == count
        {
            return (tokens, docs);
        }
        drop(memo);
        let (mut tokens, mut docs) = (0u64, 0u64);
        for d in deleted.iter() {
            let len = inv.doc_length(d);
            tokens += u64::from(len);
            docs += u64::from(len > 0);
        }
        memo = self.deleted_lengths.lock().unwrap_or_else(|e| e.into_inner());
        memo.insert(field, (count, tokens, docs));
        (tokens, docs)
    }

    /// Read from its file, not built in memory.
    pub fn is_mapped(&self) -> bool {
        self.mapped
    }

    /// Its file holds it (`persistence::index::save_bundle` writes it otherwise).
    pub fn is_saved(&self) -> bool {
        self.saved.load(Ordering::Acquire)
    }

    /// Its file is written.
    pub fn mark_saved(&self) {
        self.saved.store(true, Ordering::Release);
    }

    pub fn total_docs(&self) -> u32 {
        self.doc_store.total_docs()
    }

    /// A document's `_seq_id`: its place in the order documents were first written, which writing
    /// it again keeps. A segment's documents are in this order.
    #[inline]
    pub fn seq_id(&self, doc: DocId) -> u64 {
        self.seq.get(doc).unwrap_or(u64::from(self.base_doc_id) + u64::from(doc))
    }

    /// The highest `_seq_id` of the segment's documents + 1 (the segment's base when it has none).
    pub fn seq_end(&self) -> u64 {
        match self.total_docs() {
            0 => u64::from(self.base_doc_id),
            n => self.seq_id(n - 1) + 1,
        }
    }

    pub fn get_doc(&self, id: DocId, schema: &Schema) -> Option<Document> {
        self.doc_store.get_doc(id, schema)
    }

    /// `document_to_json` of `get_doc`, the record decoded once under `registry` (the schema's)
    /// without typing its values; a record that does not decode so goes the typed way.
    pub fn doc_json(&self, id: DocId, schema: &Schema, registry: &crate::tqbf::registry::FieldRegistry) -> Option<serde_json::Value> {
        match self.doc_store.record_json(id, registry) {
            Ok(v) => v,
            Err(_) => self.get_doc(id, schema).map(|d| document_to_json(&d, schema)),
        }
    }

    /// Look up DocId from external ID.
    #[inline]
    pub fn ext_id_lookup(&self, ext_id: &str) -> Option<DocId> {
        self.doc_store.doc_id(ext_id)
    }

    /// Find a reference index by target collection name.
    pub fn reference_for_collection(&self, target_collection: &str) -> Option<(&FieldId, &ReferenceIndex)> {
        self.reference_indexes
            .iter()
            .find(|(_, ri)| ri.target_collection == target_collection)
    }

    /// Check if an external ID exists.
    #[inline]
    pub fn ext_id_exists(&self, ext_id: &str) -> bool {
        self.doc_store.doc_id(ext_id).is_some()
    }

    /// A single-valued numeric field's value for `doc` as i64 (floats truncated), from its ordinals.
    #[inline]
    pub fn number_i64(&self, field: FieldId, doc: DocId) -> Option<i64> {
        self.values.get(&field)?.doc_i64(doc)
    }

    /// A single-valued numeric field's value for `doc` as f64, from its ordinals.
    #[inline]
    pub fn number_f64(&self, field: FieldId, doc: DocId) -> Option<f64> {
        self.values.get(&field)?.doc_f64(doc)
    }

    /// Compact the heap structures left after build (every index section is exact already).
    pub fn compact(&mut self) {
        self.all_docs.run_optimize();
    }

    pub fn memory_usage(&self) -> IndexMemoryBreakdown {
        let docs_bytes = self.doc_store.memory_usage();
        let stored_docs_bytes = self.doc_store.record_store_bytes();

        // Index sections (heap until the segment is persisted, mapped after)
        let inverted_bytes: usize = self.inverted.values().map(InvertedIndex::section_bytes).sum();
        let value_bytes: usize = self.values.values().map(ValueIndex::section_bytes).sum::<usize>()
            + self.bools.values().map(BoolIndex::section_bytes).sum::<usize>();
        let geo_bytes: usize = self.geo_indexes.values().map(GeoIndex::section_bytes).sum();
        let vector_bytes: usize = self.vector_indexes.values().map(VectorIndex::section_bytes).sum();

        let all_docs_bytes = roaring_heap_bytes(&self.all_docs);

        // Top-level FxHashMap overhead for index maps
        let map_overhead = hashmap_base_overhead() * 6; // inverted, values, bools, geo, vector, reference

        let total = docs_bytes + all_docs_bytes + map_overhead;

        IndexMemoryBreakdown {
            total_bytes: total,
            docs_bytes,
            inverted_bytes,
            value_bytes,
            geo_bytes,
            vector_bytes,
            all_docs_bitmap_bytes: all_docs_bytes,
            stored_docs_bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// IndexBundle — collection-level wrapper around a vector of segments.
//
// `Collection::snapshot()` returns the first segment, for what needs only one;
// everything that serves a collection uses `Collection::snapshot_bundle()`.
// ---------------------------------------------------------------------------

/// A collection's index data: a vector of immutable segments plus
/// collection-wide metadata. Held by `Collection` via `ArcSwap`.
///
/// **Live docs vs physical docs**: each segment is immutable, so deletes and
/// upserts that supersede an existing doc cannot mutate the segment in place.
/// Instead the bundle keeps a parallel `deleted_per_segment` vector of
/// roaring bitmaps holding the LOCAL doc ids of segments that have been
/// obsoleted by later flushes. Search applies these masks at query time
/// (Lucene-style "live docs"); background segment compaction rewrites
/// segments to physically drop the deleted docs.
#[derive(Clone)]
pub struct IndexBundle {
    /// Schema is replicated here so segments can be served without holding
    /// a reference back to the Collection.
    pub schema: Arc<Schema>,
    /// Segments in doc-id order.
    pub segments: Vec<Arc<Segment>>,
    /// Per-segment deletion bitmaps in lockstep with `segments`. Each bitmap
    /// holds the LOCAL doc ids of that segment that should be hidden from
    /// search (because a later flush upserted or deleted that external id).
    /// Empty bitmap = no deletions for that segment.
    pub deleted_per_segment: Vec<Arc<Bitmap>>,
    /// Live (post-deletion) doc count across all segments. Cached because
    /// computing it from segments on every call would do an O(N) sum.
    pub total_docs: u32,
    /// The LSN of the last write of a realtime collection's write-ahead log the segments hold
    /// (`persistence::wal`): saved, the log's checkpoint. 0 for none.
    pub lsn: u64,
    /// The `_seq_id` the next new document gets: above every id the collection's documents ever
    /// had (a merge keeps ids, so the segments alone may not show the highest given out). 0: not
    /// known, the segments' highest + 1 (`next_seq`).
    pub next_seq: u64,
    /// How many segments at the end realtime writes built in memory (one per write, merged among
    /// themselves); a flush makes them one segment for the disk.
    pub memory: usize,
}

impl IndexBundle {
    /// Empty bundle: holds exactly one empty segment with zero docs.
    ///
    /// We preserve the invariant that every bundle has at least one segment
    /// so `primary_segment()` is always callable. An "empty bundle" still
    /// has a usable Segment that consumers can dispatch search/filter calls
    /// against; everything just returns zero hits.
    pub fn empty(schema: Arc<Schema>) -> Self {
        Self {
            schema,
            segments: vec![Arc::new(Segment::empty())],
            deleted_per_segment: vec![Arc::new(Bitmap::new())],
            total_docs: 0,
            lsn: 0,
            next_seq: 0,
            memory: 0,
        }
    }

    /// Wrap a single segment as a bundle.
    pub fn single(segment: Segment, schema: Arc<Schema>) -> Self {
        let total = segment.total_docs();
        Self {
            schema,
            segments: vec![Arc::new(segment)],
            deleted_per_segment: vec![Arc::new(Bitmap::new())],
            total_docs: total,
            lsn: 0,
            next_seq: 0,
            memory: 0,
        }
    }

    /// Wrap a single Arc<Segment> as a bundle (no clone).
    pub fn single_arc(segment: Arc<Segment>, schema: Arc<Schema>) -> Self {
        let total = segment.total_docs();
        Self {
            schema,
            segments: vec![segment],
            deleted_per_segment: vec![Arc::new(Bitmap::new())],
            total_docs: total,
            lsn: 0,
            next_seq: 0,
            memory: 0,
        }
    }

    /// Live (post-deletion) doc count across all segments. This is the
    /// number users see in `out_of` for an unfiltered query.
    pub fn total_docs(&self) -> u32 {
        self.total_docs
    }

    /// Physical doc count (sum of `segment.total_docs()` ignoring deletion
    /// masks). Used by background compaction so it knows how many bytes of
    /// segment data it might need to rewrite.
    pub fn physical_total_docs(&self) -> u32 {
        self.segments.iter().map(|s| s.total_docs()).sum()
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Returns the deletion mask for `seg_idx`, or `None` when the bundle
    /// has no entry for that index. Used by the search fan-out to apply
    /// per-segment live-docs filtering.
    pub fn deleted_for(&self, seg_idx: usize) -> Option<&Bitmap> {
        self.deleted_per_segment.get(seg_idx).map(|d| &**d)
    }

    /// The `_seq_id` the next new document gets (`next_seq`, else above every segment's).
    pub fn next_seq(&self) -> u64 {
        if self.next_seq > 0 {
            return self.next_seq;
        }
        self.segments.iter().map(|s| s.seq_end()).max().unwrap_or(0)
    }

    /// The documents of the segments realtime writes built in memory (`memory`), deleted ones too.
    pub fn memory_docs(&self) -> u32 {
        self.segments[self.segments.len() - self.memory..].iter().map(|s| s.total_docs()).sum()
    }

    /// Compute the live (post-deletion) doc count from segments and masks.
    /// Used by `compact()` flush logic and tests to recompute `total_docs`
    /// after the deletion masks change.
    pub fn compute_live_total_docs(
        segments: &[Arc<Segment>],
        deleted_per_segment: &[Arc<Bitmap>],
    ) -> u32 {
        segments
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                let deleted = deleted_per_segment
                    .get(i)
                    .map(|bm| bm.cardinality() as u32)
                    .unwrap_or(0);
                seg.total_docs().saturating_sub(deleted)
            })
            .sum()
    }

    /// Sum the per-segment memory breakdowns into a collection-wide total.
    /// Used by `GET /admin/stats` so multi-segment collections report the
    /// memory of every segment, not just segment 0.
    pub fn memory_usage(&self) -> IndexMemoryBreakdown {
        let mut total = IndexMemoryBreakdown {
            total_bytes: 0,
            docs_bytes: 0,
            inverted_bytes: 0,
            value_bytes: 0,
            geo_bytes: 0,
            vector_bytes: 0,
            all_docs_bitmap_bytes: 0,
            stored_docs_bytes: 0,
        };
        for seg in &self.segments {
            let s = seg.memory_usage();
            total.total_bytes += s.total_bytes;
            total.docs_bytes += s.docs_bytes;
            total.inverted_bytes += s.inverted_bytes;
            total.value_bytes += s.value_bytes;
            total.geo_bytes += s.geo_bytes;
            total.vector_bytes += s.vector_bytes;
            total.all_docs_bitmap_bytes += s.all_docs_bitmap_bytes;
            total.stored_docs_bytes += s.stored_docs_bytes;
        }
        total
    }

    /// The first segment.
    pub fn primary_segment(&self) -> &Arc<Segment> {
        self.segments
            .first()
            .expect("IndexBundle::primary_segment called on empty bundle")
    }

    /// Returns the primary segment as an owned Arc (for ArcSwap consumers
    /// that need to outlive the load guard).
    pub fn primary_segment_arc(&self) -> Arc<Segment> {
        Arc::clone(self.primary_segment())
    }

    /// Look up an external ID across all segments. Returns the first
    /// matching `(segment_index, local_doc_id)` pair, or None.
    pub fn ext_id_lookup(&self, ext_id: &str) -> Option<(usize, DocId)> {
        for (i, seg) in self.segments.iter().enumerate() {
            if let Some(local) = seg.ext_id_lookup(ext_id) {
                return Some((i, local));
            }
        }
        None
    }

    /// Returns true if any segment contains a doc with this external ID.
    pub fn ext_id_exists(&self, ext_id: &str) -> bool {
        self.segments.iter().any(|seg| seg.ext_id_exists(ext_id))
    }

    /// The base of a segment after the bundle's.
    pub fn next_base(&self) -> DocId {
        self.segments.iter().map(|s| s.base_doc_id + s.total_docs()).max().unwrap_or(0)
    }

    /// The live document `ext_id` (a document has one live copy): its segment and doc id there.
    pub fn live_lookup(&self, ext_id: &str) -> Option<(usize, DocId)> {
        self.segments.iter().enumerate().find_map(|(i, seg)| {
            let doc = seg.ext_id_lookup(ext_id)?;
            (!self.deleted_for(i).is_some_and(|d| d.contains(doc))).then_some((i, doc))
        })
    }

    /// The live document `ext_id`.
    pub fn live_document(&self, ext_id: &str) -> Option<Document> {
        let (i, doc) = self.live_lookup(ext_id)?;
        self.segments[i].get_doc(doc, &self.schema)
    }

    /// Every live document in `_seq_id` order.
    pub fn live_documents(&self) -> Vec<Document> {
        let mut docs: Vec<(u64, Document)> = Vec::with_capacity(self.total_docs as usize);
        for (i, seg) in self.segments.iter().enumerate() {
            let deleted = self.deleted_for(i).filter(|d| !d.is_empty());
            for doc in 0..seg.total_docs() {
                if deleted.is_some_and(|d| d.contains(doc)) {
                    continue;
                }
                if let Some(d) = seg.get_doc(doc, &self.schema) {
                    docs.push((seg.seq_id(doc), d));
                }
            }
        }
        // segments in order of their bases are in order unless one holds documents written again
        if !docs.windows(2).all(|w| w[0].0 < w[1].0) {
            docs.sort_by_key(|d| d.0);
        }
        docs.into_iter().map(|d| d.1).collect()
    }
}

impl std::fmt::Debug for IndexBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBundle")
            .field("segments", &self.segments.len())
            .field("total_docs", &self.total_docs)
            .finish()
    }
}

#[derive(Serialize)]
pub struct IndexMemoryBreakdown {
    pub total_bytes: usize,
    pub docs_bytes: usize,
    /// Text index sections (TQBF TERMS, RAWTERMS, POSTINGS, FREQS, NORMS); memory-mapped
    /// once persisted, so not in `total_bytes`.
    pub inverted_bytes: usize,
    /// Value sections (VALUES, NUM_VALUES, VALUE_DOCS, DOC_ORDS, BOOL: filters, facets, sorting,
    /// group_by, JOIN references); memory-mapped once persisted, so not in `total_bytes`.
    pub value_bytes: usize,
    /// GEO sections; memory-mapped once persisted, so not in `total_bytes`.
    pub geo_bytes: usize,
    /// VECTORS sections; memory-mapped once persisted, so not in `total_bytes`.
    pub vector_bytes: usize,
    pub all_docs_bitmap_bytes: usize,
    /// Compressed stored documents (TQBF DOCS); memory-mapped once persisted, so not in
    /// `total_bytes`.
    pub stored_docs_bytes: usize,
}

/// Estimated base overhead for an FxHashMap (bucket array, control bytes, struct).
/// hashbrown minimum capacity is 4 slots; the struct itself is 3 words.
#[inline]
fn hashmap_base_overhead() -> usize {
    64
}

/// Convert a FieldValue to a serde_json::Value.
#[inline]
fn field_value_to_json(value: &FieldValue) -> serde_json::Value {
    match value {
        FieldValue::Null => serde_json::Value::Null,
        FieldValue::Bool(b) => serde_json::Value::Bool(*b),
        FieldValue::Int32(n) => serde_json::json!(*n),
        FieldValue::Int64(n) => serde_json::json!(*n),
        FieldValue::Float64(f) => serde_json::json!(*f),
        FieldValue::Text(s) => serde_json::Value::String(s.clone()),
        FieldValue::StringArray(arr) => serde_json::json!(arr),
        FieldValue::Int32Array(arr) => serde_json::json!(arr),
        FieldValue::Int64Array(arr) => serde_json::json!(arr),
        FieldValue::FloatArray(arr) => serde_json::json!(arr),
        FieldValue::BoolArray(arr) => serde_json::json!(arr),
        FieldValue::GeoPointArray(arr) => serde_json::json!(arr
            .iter()
            .map(|&(lat, lng)| serde_json::json!([lat, lng]))
            .collect::<Vec<_>>()),
        FieldValue::GeoPoint { lat, lng } => serde_json::json!([*lat, *lng]),
    }
}

/// Convert a Document back to JSON for API responses.
pub fn document_to_json(doc: &Document, schema: &Schema) -> serde_json::Value {
    // The stored record is the document exactly as it was sent (every path, indexed or not).
    if let Some(rec) = &doc.record {
        match crate::tqbf::record::to_value(rec, &schema.registry.load()) {
            Ok(v) => return v,
            Err(e) => tracing::error!(id = %doc.external_id, "unreadable stored record: {e}"),
        }
    }

    // Fast path: use precomputed nested layout to build nested JSON directly
    if let Some(layout) = &schema.nested_layout {
        return document_to_json_nested(doc, schema, layout);
    }

    // Flat path: no nested fields, build a simple flat map
    let mut map = serde_json::Map::with_capacity(schema.fields.len() + 1);
    map.insert(
        "id".to_string(),
        serde_json::Value::String(doc.external_id.clone()),
    );

    for (i, field_def) in schema.fields.iter().enumerate() {
        if let Some(value) = doc.fields.get(i) {
            if matches!(value, FieldValue::Null) {
                continue;
            }
            map.insert(field_def.name.clone(), field_value_to_json(value));
        }
    }

    serde_json::Value::Object(map)
}

/// Fast path: build nested JSON directly using precomputed layout.
/// Avoids the BTreeMap + remove + rebuild overhead of unflatten_json.
fn document_to_json_nested(
    doc: &Document,
    schema: &Schema,
    layout: &crate::schema::NestedFieldLayout,
) -> serde_json::Value {
    let mut map =
        serde_json::Map::with_capacity(layout.flat_fields.len() + layout.groups.len() + 1);
    map.insert(
        "id".to_string(),
        serde_json::Value::String(doc.external_id.clone()),
    );

    // 1. Insert flat fields directly (no dot check needed)
    for &idx in &layout.flat_fields {
        if let Some(value) = doc.fields.get(idx) {
            if matches!(value, FieldValue::Null) {
                continue;
            }
            map.insert(schema.fields[idx].name.clone(), field_value_to_json(value));
        }
    }

    // 2. Build nested groups directly from doc.fields
    for group in &layout.groups {
        if group.is_array_group {
            // Zip children arrays into array-of-objects
            let max_len = group
                .children
                .iter()
                .enumerate()
                .filter(|(child_pos, _)| !group.geopoint_child_indices.contains(child_pos))
                .filter_map(|(_, (idx, _))| doc.fields.get(*idx)?.array_len())
                .max()
                .unwrap_or(0);

            let mut arr = Vec::with_capacity(max_len);
            for i in 0..max_len {
                let mut elem = serde_json::Map::new();
                for (child_pos, (idx, suffix)) in group.children.iter().enumerate() {
                    if group.geopoint_child_indices.contains(&child_pos) {
                        // Geopoint: insert as [lat, lng] directly
                        if let Some(value) = doc.fields.get(*idx) {
                            if !matches!(value, FieldValue::Null) {
                                elem.insert(suffix.clone(), field_value_to_json(value));
                            }
                        }
                    } else if let Some(fv) = doc.fields.get(*idx) {
                        if let Some(json_val) = fv.get_array_element(i) {
                            elem.insert(suffix.clone(), json_val);
                        }
                    }
                }
                arr.push(serde_json::Value::Object(elem));
            }
            map.insert(group.prefix.clone(), serde_json::Value::Array(arr));
        } else {
            // Scalar nested object
            let mut nested = serde_json::Map::new();
            for (idx, suffix) in &group.children {
                if let Some(value) = doc.fields.get(*idx) {
                    if matches!(value, FieldValue::Null) {
                        continue;
                    }
                    nested.insert(suffix.clone(), field_value_to_json(value));
                }
            }
            if !nested.is_empty() {
                map.insert(group.prefix.clone(), serde_json::Value::Object(nested));
            }
        }
    }

    serde_json::Value::Object(map)
}

