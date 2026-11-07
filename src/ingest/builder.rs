use crate::index::geo::GeoIndex;
use crate::index::inverted::InvertedIndex;
use crate::index::reference::ReferenceIndex;
use crate::index::values::{BoolIndex, ValueIndex};
use crate::index::{DocStore, Index, IndexBundle, Segment};
use crate::schema::{DocId, Document, FieldDef, FieldId, FieldType, FieldValue, Schema, VecDist};
use crate::vector::VectorIndex;
use crate::bitmap::Bitmap;
use rustc_hash::FxHashMap;
use std::time::Instant;

/// Write a built segment as its file in the collection's segment set and read it back mapped, so a
/// build holds one segment's working memory and not the collection's. On a failure the heap segment
/// stays, and the save after the build writes it (a save writes every segment not on disk).
fn stream_persist_segment(
    data_dir: Option<&std::path::Path>,
    collection_name: Option<&str>,
    segment_idx: u32,
    segment: Segment,
    schema: &Schema,
) -> Segment {
    let (Some(dir), Some(name)) = (data_dir, collection_name) else {
        return segment;
    };
    match crate::persistence::index::write_and_open(dir, name, &segment, schema) {
        Ok(mapped) => mapped,
        Err(e) => {
            tracing::warn!(segment_idx, error = %e, "stream-persist failed; keeping heap segment");
            segment
        }
    }
}

/// The segments of a streaming build, each written and mapped before the next is built.
struct PersistPipeline<'a> {
    segments: Vec<std::sync::Arc<Segment>>,
    data_dir: Option<&'a std::path::Path>,
    collection_name: Option<&'a str>,
    schema: &'a Schema,
}

impl<'a> PersistPipeline<'a> {
    fn new(
        data_dir: Option<&'a std::path::Path>,
        collection_name: Option<&'a str>,
        schema: &'a Schema,
    ) -> Self {
        Self {
            segments: Vec::new(),
            data_dir,
            collection_name,
            schema,
        }
    }

    fn handoff(&mut self, segment_idx: u32, segment: Segment) {
        let segment = stream_persist_segment(self.data_dir, self.collection_name, segment_idx, segment, self.schema);
        self.segments.push(std::sync::Arc::new(segment));
    }

    fn finish(self) -> Vec<std::sync::Arc<Segment>> {
        self.segments
    }
}

/// Below this many documents a build (a realtime write, a merge of small segments) indexes on the
/// calling thread and leaves the allocator alone: a pool of threads per build, and purging every
/// thread's allocator cache after it, cost more than they save that small.
const SMALL_BUILD_DOCS: usize = 4096;

/// Build a complete Index from documents using rayon parallelism.
pub fn build_index(schema: &Schema, documents: Vec<Document>) -> Index {
    build_index_impl(
        schema,
        documents,
        None,
        None,
        #[cfg(feature = "cuda")]
        None,
    )
}

/// Build a complete Index with disk-backed text for large text fields.
pub fn build_index_with_backing(
    schema: &Schema,
    documents: Vec<Document>,
    data_dir: &std::path::Path,
    collection_name: &str,
) -> Index {
    build_index_impl(
        schema,
        documents,
        Some(data_dir),
        Some(collection_name),
        #[cfg(feature = "cuda")]
        None,
    )
}

/// Build a complete Index, optionally uploading vector data to GPU.
#[cfg(feature = "cuda")]
pub fn build_index_with_gpu(
    schema: &Schema,
    documents: Vec<Document>,
    gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
) -> Index {
    build_index_impl(schema, documents, None, None, gpu_context)
}

/// Build a complete Index with GPU support and disk-backed text.
#[cfg(feature = "cuda")]
pub fn build_index_with_gpu_and_backing(
    schema: &Schema,
    documents: Vec<Document>,
    gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
    data_dir: &std::path::Path,
    collection_name: &str,
) -> Index {
    build_index_impl(schema, documents, Some(data_dir), Some(collection_name), gpu_context)
}

/// Documents per segment of a streaming build: bounds the memory a build holds.
pub(crate) const STREAMING_SEGMENT_DOCS: u32 = 500_000;

/// Split a TQBF payload into one byte chunk per segment (the last may be smaller). The payload is
/// drained as this goes, so peak memory is the payload plus one chunk, and a chunk lives only as
/// long as its segment's build.
pub fn split_payload_into_segment_chunks(
    mut payload: Vec<u8>,
    doc_count: u32,
    schema: &Schema,
) -> Result<Vec<Vec<u8>>, crate::ingest::protocol::ProtocolError> {
    let seg_size = STREAMING_SEGMENT_DOCS;
    let total_docs = doc_count;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut doc_id_base: u32 = 0;
    while doc_id_base < total_docs {
        let seg_count = seg_size.min(total_docs - doc_id_base) as usize;
        // Walk forward through `payload` to find where this segment ends.
        let mut byte_offset = 0usize;
        for _ in 0..seg_count {
            crate::ingest::protocol::skip_document_pub(&payload, schema, &mut byte_offset)?;
        }
        // Drain the segment's bytes into a fresh Vec, leaving the remainder
        // in `payload`. Vec::drain is in-place: peak memory = payload + chunk
        // (briefly, during the drain copy), then payload shrinks.
        let chunk: Vec<u8> = payload.drain(..byte_offset).collect();
        chunks.push(chunk);
        doc_id_base += seg_count as u32;
    }
    drop(payload); // free any remaining capacity
    Ok(chunks)
}

/// Build an `IndexBundle` from per-segment byte chunks (`split_payload_into_segment_chunks`), each
/// dropped once its documents are decoded, so the payload does not sit in memory through the build.
pub fn build_index_streaming_from_chunks(
    mut chunks: Vec<Vec<u8>>,
    total_docs: u32,
    schema: &Schema,
    data_dir: Option<&std::path::Path>,
    collection_name: Option<&str>,
    #[cfg(feature = "cuda")] gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
) -> IndexBundle {
    let schema_arc = std::sync::Arc::new(schema.clone());
    if chunks.is_empty() || total_docs == 0 {
        return IndexBundle::empty(schema_arc);
    }

    let build_start = Instant::now();

    let searchable_fields: Vec<&FieldDef> = schema
        .fields
        .iter()
        .filter(|f| f.index && f.field_type.is_searchable())
        .collect();
    let mut pipeline = PersistPipeline::new(data_dir, collection_name, schema);
    let mut doc_id_base: u32 = 0;
    let mut segment_idx: u32 = 0;

    while !chunks.is_empty() {
        let chunk = chunks.remove(0);

        // The last chunk holds what is left, not a full segment.
        let seg_count = STREAMING_SEGMENT_DOCS.min(total_docs.saturating_sub(doc_id_base));

        let (mut docs, _bytes_consumed) = crate::ingest::protocol::decode_doc_segment(
            &chunk, 0, seg_count, schema, 0,
        ).unwrap_or_else(|e| {
            panic!("Failed to decode segment {segment_idx} from chunk: {e}");
        });
        let actual_count = docs.len() as u32;
        for (i, doc) in docs.iter_mut().enumerate() {
            doc.id = i as u32;
        }
        drop(chunk);

        let partial = build_partial(&docs, schema, &searchable_fields);

        // Stored records and the value, geo, vector and reference indexes.
        let doc_indexes = build_doc_indexes(
            &docs,
            schema,
            actual_count,
            #[cfg(feature = "cuda")]
            gpu_context,
        );
        drop(docs);
        crate::memory::purge_allocator();

        let inverted = convert_partial_to_final(partial, actual_count);
        let segment = build_index_final(
            schema,
            doc_indexes,
            inverted,
            actual_count,
            doc_id_base,
            build_start,
            0, 0, 0,
            #[cfg(feature = "cuda")]
            gpu_context,
        );

        pipeline.handoff(segment_idx, segment);
        crate::memory::purge_allocator();

        doc_id_base += actual_count;
        segment_idx += 1;
    }

    let segments = pipeline.finish();

    let total_ms = build_start.elapsed().as_millis() as u64;
    let final_total_docs = doc_id_base;
    tracing::info!(
        segments = segments.len(),
        docs = final_total_docs,
        total_ms,
        "streaming build complete (chunked, per-segment)"
    );

    let deleted_per_segment = (0..segments.len())
        .map(|_| std::sync::Arc::new(crate::bitmap::Bitmap::new()))
        .collect();
    IndexBundle {
        schema: schema_arc,
        segments,
        deleted_per_segment,
        total_docs: final_total_docs,
        lsn: 0,
        next_seq: 0,
        memory: 0,
    }
}

// ---------------------------------------------------------------------------
// Disk-based segment serialization / deserialization / merge
// ---------------------------------------------------------------------------

/// Convert a single PartialIndex directly to final text indexes without disk round-trip.
fn convert_partial_to_final(partial: PartialIndex, total_docs: u32) -> FxHashMap<FieldId, InvertedIndex> {
    partial.inverted.into_iter().map(|(fid, field)| (fid, build_text_index(field, total_docs))).collect()
}

/// A field's text index from its merged term data.
fn build_text_index(field: FieldPartial, total_docs: u32) -> InvertedIndex {
    use crate::index::inverted::TermInput;
    let mut lengths = vec![0u32; total_docs as usize];
    for &(doc, len) in &field.lengths {
        lengths[doc as usize] = len;
    }
    let mut inputs: Vec<TermInput> = field.terms.into_iter().map(|(term, td)| td.into_input(term)).collect();
    inputs.sort_unstable_by(|a, b| a.term.cmp(&b.term));
    let raw: Vec<(Vec<u8>, Vec<u8>)> = field.raw.into_iter().collect();
    InvertedIndex::build(&inputs, &raw, &lengths).expect("text index sections from consistent term data")
}

/// Merge multiple sub-segment partials into one PartialIndex.
#[allow(unused_variables)]
fn build_index_impl(
    schema: &Schema,
    mut documents: Vec<Document>,
    data_dir: Option<&std::path::Path>,
    collection_name: Option<&str>,
    #[cfg(feature = "cuda")] gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
) -> Index {
    if documents.is_empty() {
        return Index::empty();
    }

    // Assign sequential DocIds
    for (i, doc) in documents.iter_mut().enumerate() {
        doc.id = i as DocId;
    }

    let total_docs = documents.len() as u32;
    let build_start = Instant::now();

    // Find searchable+indexed fields
    let searchable_fields: Vec<&FieldDef> = schema
        .fields
        .iter()
        .filter(|f| f.index && f.field_type.is_searchable())
        .collect();

    let t0 = Instant::now();
    let partials: Vec<PartialIndex> =
        vec![build_partial(&documents, schema, &searchable_fields)];
    let partition_ms = t0.elapsed().as_millis() as u64;

    // === PHASE: build everything that needs the documents, then drop them ===
    let t0 = Instant::now();
    let doc_indexes = build_doc_indexes(
        &documents,
        schema,
        total_docs,
        #[cfg(feature = "cuda")]
        gpu_context,
    );
    drop(documents);
    let docstore_ms = t0.elapsed().as_millis() as u64;
    let small = (total_docs as usize) < SMALL_BUILD_DOCS;
    if !small {
        crate::memory::purge_allocator();
    }

    // Merge partials into final indexes (consumes partials destructively).
    let t0 = Instant::now();
    let inverted = merge_partials(partials, total_docs, &searchable_fields);
    let merge_ms = t0.elapsed().as_millis() as u64;
    if !small {
        crate::memory::purge_allocator();
    }

    build_index_final(
        schema,
        doc_indexes,
        inverted,
        total_docs,
        0, // base_doc_id: batch path always builds one segment starting at doc 0
        build_start,
        partition_ms,
        docstore_ms,
        merge_ms,
        #[cfg(feature = "cuda")]
        gpu_context,
    )
}

/// What a segment builds from its documents while they are in memory: the stored records,
/// the value and bool indexes, and the geo, vector and reference indexes.
struct DocIndexes {
    doc_store: DocStore,
    values: FxHashMap<FieldId, ValueIndex>,
    bools: FxHashMap<FieldId, BoolIndex>,
    geo_indexes: FxHashMap<FieldId, GeoIndex>,
    vector_indexes: FxHashMap<FieldId, VectorIndex>,
    reference_indexes: FxHashMap<FieldId, ReferenceIndex>,
}

/// Build a segment's document-derived indexes. `docs` have doc ids `0..total_docs` in order.
#[allow(unused_variables)]
fn build_doc_indexes(
    docs: &[Document],
    schema: &Schema,
    total_docs: u32,
    #[cfg(feature = "cuda")] gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
) -> DocIndexes {

    let t0 = Instant::now();
    let doc_store = DocStore::build(docs, schema).expect("stored documents from parsed documents");
    let (values, bools) = crate::index::values::build_segment(docs, schema, total_docs)
        .expect("value index sections from parsed documents");
    let values_ms = t0.elapsed().as_millis() as u64;

    // Geo indexes (GEO sections), points truncated to 1e-6 degrees as Typesense keeps them
    let t0 = Instant::now();
    let mut geo_indexes = FxHashMap::default();
    for field_def in schema.fields.iter().filter(|f| {
        matches!(f.field_type, FieldType::GeoPoint | FieldType::GeoPointArray)
    }) {
        let fid = field_def.id;
        let is_array = field_def.field_type == FieldType::GeoPointArray;
        let e6 = crate::index::geo::e6;
        let mut points: Vec<(DocId, i32, i32)> = Vec::new();
        for doc in docs {
            match doc.fields.get(fid as usize) {
                // geopoint[]: every point enters the index under the same
                // doc_id — a doc matches a radius filter if ANY point does.
                Some(FieldValue::GeoPointArray(arr)) if is_array => {
                    points.extend(arr.iter().map(|&(lat, lng)| (doc.id, e6(lat), e6(lng))));
                }
                Some(&FieldValue::GeoPoint { lat, lng }) if !is_array => points.push((doc.id, e6(lat), e6(lng))),
                _ => {}
            }
        }
        if !points.is_empty() {
            let idx = GeoIndex::build(&points, total_docs, is_array).expect("GEO section from parsed documents");
            geo_indexes.insert(fid, idx);
        }
    }
    let geo_ms = t0.elapsed().as_millis() as u64;

    // Vector indexes from the documents' FloatArray values
    let t0 = Instant::now();
    let mut vector_indexes = FxHashMap::default();
    for field_def in &schema.fields {
        if let FieldType::FloatArray { num_dim } = field_def.field_type {
            if num_dim == 0 {
                continue; // regular float[] array, not a vector field
            }
            let fid = field_def.id;
            let dim = num_dim as usize;
            let metric = field_def.vec_dist.unwrap_or(VecDist::Cosine);

            // Guard: check vector memory before allocating
            let vector_bytes = total_docs as usize * dim * std::mem::size_of::<f32>();
            const MAX_VECTOR_BYTES: usize = 16 * 1024 * 1024 * 1024; // 16 GB
            if vector_bytes > MAX_VECTOR_BYTES {
                tracing::error!(
                    field = %field_def.name,
                    docs = total_docs,
                    dim,
                    vector_gb = vector_bytes / (1024 * 1024 * 1024),
                    limit_gb = MAX_VECTOR_BYTES / (1024 * 1024 * 1024),
                    "Vector storage exceeds memory limit, skipping field"
                );
                continue;
            }
            if vector_bytes > MAX_VECTOR_BYTES / 2 {
                tracing::warn!(
                    field = %field_def.name,
                    vector_gb = vector_bytes / (1024 * 1024 * 1024),
                    "Vector storage exceeds 50% of memory limit"
                );
            }

            // Collect vectors into contiguous flat storage
            let mut flat_vectors = vec![0.0f32; total_docs as usize * dim];
            for doc in docs {
                if let Some(FieldValue::FloatArray(vec)) = doc.fields.get(fid as usize) {
                    if vec.len() == dim {
                        let offset = doc.id as usize * dim;
                        flat_vectors[offset..offset + dim].copy_from_slice(vec);
                    }
                }
            }

            #[allow(unused_mut)]
            let mut vec_idx = VectorIndex::build(flat_vectors, total_docs, num_dim, metric);

            // Upload vectors to GPU if available
            #[cfg(feature = "cuda")]
            if let Some(gpu) = gpu_context {
                match gpu.stream.clone_htod(&vec_idx.vectors[..]) {
                    Ok(gpu_vecs) => {
                        vec_idx.gpu_vectors = Some(gpu_vecs);
                        tracing::debug!(field = %field_def.name, "Uploaded vectors to GPU");

                        // Upload RaBitQ codes/norms if quantized
                        if let (Some(codes), Some(norms)) = (&vec_idx.codes, &vec_idx.norms) {
                            if let Some(q) = &vec_idx.quantizer {
                                let words = crate::vector::quantize::RaBitQuantizer::codes_to_u32_words(codes, q.num_dim);
                                if let (Ok(gpu_codes), Ok(gpu_norms)) = (
                                    gpu.stream.clone_htod(&words),
                                    gpu.stream.clone_htod(&norms[..]),
                                ) {
                                    vec_idx.gpu_codes = Some(gpu_codes);
                                    vec_idx.gpu_norms = Some(gpu_norms);
                                    tracing::debug!(field = %field_def.name, "Uploaded RaBitQ codes to GPU");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(field = %field_def.name, "Failed to upload vectors to GPU: {e}");
                    }
                }
            }

            vector_indexes.insert(fid, vec_idx);
        }
    }
    let vector_ms = t0.elapsed().as_millis() as u64;

    // Reference indexes: views of the reference fields' value sections
    let reference_indexes = ReferenceIndex::for_segment(schema, &values);

    tracing::debug!(docs = total_docs, values_ms, geo_ms, vector_ms, "document indexes built");

    DocIndexes { doc_store, values, bools, geo_indexes, vector_indexes, reference_indexes }
}

/// Shared final phase: assemble the Segment.
/// Used by both build_index_impl (batch) and build_index_streaming_from_chunks (streaming, called once per segment).
///
/// `total_docs` is the per-segment doc count (not the collection total).
/// `base_doc_id` is the global doc-id offset of this segment within its
/// collection — 0 for batch builds, the cumulative doc count for streaming.
#[allow(unused_variables, clippy::too_many_arguments)]
fn build_index_final(
    schema: &Schema,
    doc_indexes: DocIndexes,
    mut inverted: FxHashMap<FieldId, InvertedIndex>,
    total_docs: u32,
    base_doc_id: u32,
    build_start: Instant,
    partition_ms: u64,
    docstore_ms: u64,
    merge_ms: u64,
    #[cfg(feature = "cuda")] gpu_context: Option<&std::sync::Arc<crate::gpu::GpuContext>>,
) -> Index {
    let DocIndexes { doc_store, values, bools, geo_indexes, vector_indexes, reference_indexes } = doc_indexes;
    for (fid, inv) in inverted.iter_mut() {
        if schema.fields.iter().any(|f| f.id == *fid && f.name_matching) {
            inv.add_keys().expect("KEYS from the field's terms");
        }
    }

    // Build all_docs bitmap
    let mut all_docs = Bitmap::new();
    all_docs.add_range(0..total_docs);

    let total_ms = build_start.elapsed().as_millis() as u64;

    if (total_docs as usize) < SMALL_BUILD_DOCS {
        tracing::debug!(docs = total_docs, total_ms, "index build timing");
    } else {
        tracing::info!(
            docs = total_docs,
            total_ms,
            partition_ms,
            docstore_ms,
            merge_ms,
            "index build timing"
        );
    }

    let mut index = Index::from_parts(
        doc_store,
        inverted,
        values,
        bools,
        geo_indexes,
        vector_indexes,
        all_docs,
        reference_indexes,
        Default::default(),
        crate::index::next_segment_id(),
        false,
    );
    index.base_doc_id = base_doc_id;

    // Compact the remaining heap structures (the all-docs bitmap).
    index.compact();
    if (total_docs as usize) >= SMALL_BUILD_DOCS {
        crate::memory::purge_allocator();
    }

    index
}

// -- Partial index (per-thread) --

/// A term's postings while building: its documents in the order they were added, each with the
/// term's frequency in it.
#[derive(Default)]
struct TermData {
    docs: Vec<DocId>,
    freqs: Vec<u32>,
}

impl TermData {
    fn append(&mut self, mut other: TermData) {
        self.docs.append(&mut other.docs);
        self.freqs.append(&mut other.freqs);
    }

    /// The postings in doc order (chunks and partials may arrive out of order).
    fn into_input(self, term: Vec<u8>) -> crate::index::inverted::TermInput {
        use crate::index::inverted::TermInput;
        if self.docs.windows(2).all(|w| w[0] < w[1]) {
            return TermInput { term, docs: self.docs, freqs: self.freqs };
        }
        let mut order: Vec<usize> = (0..self.docs.len()).collect();
        order.sort_by_key(|&i| self.docs[i]);
        TermInput { term, docs: order.iter().map(|&i| self.docs[i]).collect(), freqs: order.iter().map(|&i| self.freqs[i]).collect() }
    }
}

/// What a chunk or a partial holds of a field.
#[derive(Default)]
struct FieldPartial {
    terms: FxHashMap<Vec<u8>, TermData>,
    /// (lower-cased token, term) of the tokens that differ from their term.
    raw: rustc_hash::FxHashSet<(Vec<u8>, Vec<u8>)>,
    /// (doc, field length in tokens) of the documents with tokens.
    lengths: Vec<(DocId, u32)>,
}

impl FieldPartial {
    fn absorb(&mut self, other: FieldPartial) {
        for (term, td) in other.terms {
            match self.terms.get_mut(&term) {
                Some(existing) => existing.append(td),
                None => {
                    self.terms.insert(term, td);
                }
            }
        }
        self.raw.extend(other.raw);
        self.lengths.extend(other.lengths);
    }
}

#[derive(Default)]
struct PartialIndex {
    inverted: FxHashMap<FieldId, FieldPartial>,
}

/// Build-phase state: docs ──par_chunks──▶ `build_chunk` per worker (its terms with their
/// postings) ──▶ per field, in parallel, the chunks' postings appended in chunk (doc) order.
type ChunkContribution = PartialIndex;

/// Analyze a slice of docs: per searchable field the terms and their postings.
fn build_chunk(
    docs: &[Document],
    schema: &Schema,
    searchable_fields: &[&FieldDef],
) -> ChunkContribution {

    #[derive(Default)]
    struct Chunk {
        ids: FxHashMap<Vec<u8>, u32>,
        terms: Vec<Vec<u8>>,
        data: Vec<TermData>,
        raw: rustc_hash::FxHashSet<(Vec<u8>, Vec<u8>)>,
        lengths: Vec<(DocId, u32)>,
    }
    let mut fields: Vec<Chunk> = searchable_fields.iter().map(|_| Chunk::default()).collect();
    let mut analyzer = crate::text::Analyzer::default();
    let mut doc_terms: Vec<u32> = Vec::new();
    let (symbols, separators) = (&schema.symbols_to_index, &schema.token_separators);

    for (i, doc) in docs.iter().enumerate() {
        let _ = i;
        for (f, field_def) in searchable_fields.iter().enumerate() {
            let Some(value) = doc.fields.get(field_def.id as usize) else {
                continue;
            };
            let chunk = &mut fields[f];
            doc_terms.clear();
            let mut each = |text: &str| {
                crate::text::tokenize_each(text, symbols, separators, |token| {
                    let Some((term, lowered)) = analyzer.analyze(token, symbols, field_def, doc.language) else { return };
                    let id = match chunk.ids.get(term.as_bytes()) {
                        Some(&id) => id,
                        None => {
                            let id = chunk.terms.len() as u32;
                            chunk.terms.push(term.as_bytes().to_vec());
                            chunk.ids.insert(term.as_bytes().to_vec(), id);
                            chunk.data.push(TermData::default());
                            id
                        }
                    };
                    if lowered != term {
                        chunk.raw.insert((lowered.as_bytes().to_vec(), term.as_bytes().to_vec()));
                    }
                    doc_terms.push(id);
                });
            };
            match value {
                FieldValue::Text(text) => each(text),
                FieldValue::StringArray(arr) => arr.iter().flatten().for_each(|s| each(s)),
                _ => {}
            }
            if doc_terms.is_empty() {
                continue;
            }
            chunk.lengths.push((doc.id, doc_terms.len() as u32));
            doc_terms.sort_unstable();
            for run in doc_terms.chunk_by(|a, b| a == b) {
                let data = &mut chunk.data[run[0] as usize];
                data.docs.push(doc.id);
                data.freqs.push((run.len() as u32).min(u32::from(u16::MAX)));
            }
        }
    }

    let mut contrib = ChunkContribution::default();
    for (field_def, chunk) in searchable_fields.iter().zip(fields) {
        if !chunk.terms.is_empty() {
            let field = FieldPartial { terms: chunk.terms.into_iter().zip(chunk.data).collect(), raw: chunk.raw, lengths: chunk.lengths };
            contrib.inverted.insert(field_def.id, field);
        }
    }
    contrib
}

/// Merge `ChunkContribution`s from all workers into the final `PartialIndex`, per field in
/// parallel; a term's postings are appended in chunk order, which is doc order.
fn merge_contributions(
    contribs: Vec<ChunkContribution>,
    searchable_fields: &[&FieldDef],
) -> PartialIndex {
    use rayon::prelude::*;
    let mut per_field: FxHashMap<FieldId, Vec<FieldPartial>> = FxHashMap::default();
    for mut c in contribs {
        for f in searchable_fields {
            if let Some(m) = c.inverted.remove(&f.id) {
                per_field.entry(f.id).or_default().push(m);
            }
        }
    }
    let inverted = per_field
        .into_par_iter()
        .map(|(fid, maps)| {
            let mut merged = FieldPartial::default();
            for m in maps {
                merged.absorb(m);
            }
            (fid, merged)
        })
        .collect();
    PartialIndex { inverted }
}

/// Top-level build_partial: par_chunks docs → `build_chunk` workers → per-field parallel merge →
/// `PartialIndex`.
///
/// The rayon work runs in a ThreadPool of its own: its workers start at install() and go at the end
/// of this function, so a build leaves no idle pool behind.
fn build_partial(
    docs: &[Document],
    schema: &Schema,
    searchable_fields: &[&FieldDef],
) -> PartialIndex {
    use rayon::prelude::*;

    if docs.is_empty() {
        return PartialIndex::default();
    }

    if docs.len() < SMALL_BUILD_DOCS {
        return merge_contributions(vec![build_chunk(docs, schema, searchable_fields)], searchable_fields);
    }

    let t_partial = Instant::now();

    let num_workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let chunk_size = docs.len().div_ceil(num_workers).max(1);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_workers)
        .thread_name(|i| format!("build-{i}"))
        .build()
        .expect("failed to build per-build rayon pool");

    let (chunk_ms, partial, merge_ms) = pool.install(|| {
        let t_chunk = Instant::now();
        let contribs: Vec<ChunkContribution> = docs
            .par_chunks(chunk_size)
            .map(|c| build_chunk(c, schema, searchable_fields))
            .collect();
        let chunk_ms = t_chunk.elapsed().as_millis();

        let t_merge = Instant::now();
        let partial = merge_contributions(contribs, searchable_fields);
        let merge_ms = t_merge.elapsed().as_millis();
        (chunk_ms, partial, merge_ms)
    });

    eprintln!(
        "[build_partial] {} docs in {}ms — build_chunk={}ms (workers={}, chunk={}) merge={}ms",
        docs.len(),
        t_partial.elapsed().as_millis(),
        chunk_ms,
        num_workers,
        chunk_size,
        merge_ms,
    );

    partial
}

// -- Merge --

fn merge_partials(
    mut partials: Vec<PartialIndex>,
    total_docs: u32,
    searchable_fields: &[&FieldDef],
) -> FxHashMap<FieldId, InvertedIndex> {
    use rayon::prelude::*;
    tracing::debug!(total_docs, searchable = searchable_fields.len(), "Merging partial indexes");

    // Each partial's map for a field is moved out (keys and postings, no copies) and the partial
    // dropped before the next one, so memory is freed as the merge goes.
    let mut per_field: FxHashMap<FieldId, FieldPartial> = FxHashMap::default();
    for mut partial in partials.drain(..) {
        for (fid, field) in partial.inverted.drain() {
            per_field.entry(fid).or_default().absorb(field);
        }
    }
    drop(partials);

    let inverted_result: FxHashMap<FieldId, InvertedIndex> = per_field
        .into_par_iter()
        .map(|(fid, field)| (fid, build_text_index(field, total_docs)))
        .collect();

    if (total_docs as usize) >= SMALL_BUILD_DOCS {
        crate::memory::purge_allocator();
    }

    inverted_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Language;

    fn test_schema() -> Schema {
        let mut schema = Schema {
            name: "test".into(),
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
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();
        schema
    }

    fn make_doc(id: DocId, ext_id: &str, title: &str) -> Document {
        Document {
            id,
            external_id: ext_id.into(),
            language: Language::English,
            fields: vec![FieldValue::Text(title.into())].into(),
            record: None,
        }
    }

    #[test]
    fn build_single_doc() {
        let schema = test_schema();
        let docs = vec![make_doc(0, "1", "hello world")];
        let index = build_index(&schema, docs);

        assert_eq!(index.total_docs(), 1);
        assert!(index.all_docs.contains(0));

        let inv = index.inverted.get(&0).expect("field 0 should have inverted index");
        assert!(inv.postings(b"hello").is_some());
        assert!(inv.postings(b"world").is_some());
        assert_eq!(inv.term_freq(b"world", 0), 1);
        assert_eq!(inv.doc_length(0), 2);
    }

    #[test]
    fn build_multiple_docs_bitmap_union() {
        let schema = test_schema();
        let docs = vec![
            make_doc(0, "1", "hello world"),
            make_doc(0, "2", "hello rust"),
        ];
        let index = build_index(&schema, docs);

        assert_eq!(index.total_docs(), 2);

        let inv = &index.inverted[&0];
        // "hello" appears in both docs
        let hello = inv.postings(b"hello").expect("hello should exist");
        assert!(hello.contains(0));
        assert!(hello.contains(1));
        assert_eq!(hello.cardinality(), 2);

        // "world" only in doc 0
        assert_eq!(inv.postings(b"world").unwrap().cardinality(), 1);
        // "rust" only in doc 1
        assert_eq!(inv.postings(b"rust").unwrap().cardinality(), 1);
    }

    #[test]
    fn build_empty() {
        let schema = test_schema();
        let index = build_index(&schema, vec![]);
        assert_eq!(index.total_docs(), 0);
        assert!(index.inverted.is_empty());
    }

    #[test]
    fn term_frequencies_and_lengths() {
        let schema = test_schema();
        let docs = vec![make_doc(0, "1", "hello hello hello world")];
        let index = build_index(&schema, docs);

        let inv = &index.inverted[&0];
        assert_eq!(inv.term_freq(b"hello", 0), 3);
        assert_eq!(inv.term_freq(b"world", 0), 1);
        assert_eq!(inv.doc_length(0), 4);
    }

    #[test]
    fn filter_indexes_built() {
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![
                FieldDef {
                    name: "color".into(),
                    field_type: FieldType::String,
                    facet: true,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "price".into(),
                    field_type: FieldType::Int32,
                    facet: false,
                    index: true,
                    optional: false,
                    sort: true,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "in_stock".into(),
                    field_type: FieldType::Bool,
                    facet: false,
                    index: true,
                    optional: false,
                    sort: false,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
            ],
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
        };
        schema.assign_field_ids();

        let docs = vec![
            Document {
                id: 0,
                external_id: "1".into(),
                language: Language::English,
                fields: vec![
                    FieldValue::Text("red".into()),
                    FieldValue::Int32(100),
                    FieldValue::Bool(true),
                ].into(),
                record: None,
            },
            Document {
                id: 0,
                external_id: "2".into(),
                language: Language::English,
                fields: vec![
                    FieldValue::Text("blue".into()),
                    FieldValue::Int32(200),
                    FieldValue::Bool(false),
                ].into(),
                record: None,
            },
        ];

        let index = build_index(&schema, docs);

        // String values (facet field: ordinals per document)
        let sv = index.values.get(&0).unwrap();
        assert_eq!(sv.eq_str("red").unwrap().to_vec(), [0]);
        assert_eq!(sv.eq_str("blue").unwrap().to_vec(), [1]);
        assert_eq!(sv.doc_ord(1), sv.str_ord("blue"));

        // Numeric values (sort field: ordinals per document)
        let nv = index.values.get(&1).unwrap();
        assert_eq!(nv.eq_key(100).unwrap().to_vec(), [0]);
        assert_eq!(nv.eq_key(200).unwrap().to_vec(), [1]);
        assert_eq!(nv.doc_i64(1), Some(200));

        // Bool
        let bf = index.bools.get(&2).unwrap();
        assert!(bf.true_docs.contains(0));
        assert!(bf.false_docs.contains(1));

        // Documents decode from their records
        let doc = index.get_doc(1, &schema).unwrap();
        assert_eq!(doc.external_id, "2");
        assert!(matches!(doc.fields.get(1), Some(FieldValue::Int32(200))));
        assert_eq!(index.ext_id_lookup("2"), Some(1));
    }

    #[test]
    fn string_array_filter() {
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![FieldDef {
                name: "tags".into(),
                field_type: FieldType::StringArray,
                facet: true,
                index: true,
                optional: false,
                sort: false,
                stem: false,
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
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();

        let docs = vec![
            Document {
                id: 0,
                external_id: "1".into(),
                language: Language::English,
                fields: vec![FieldValue::StringArray(vec![Some("electronics".into()), Some("sale".into())])].into(),
                record: None,
            },
            Document {
                id: 0,
                external_id: "2".into(),
                language: Language::English,
                fields: vec![FieldValue::StringArray(vec![Some("electronics".into()), Some("premium".into())])].into(),
                record: None,
            },
        ];

        let index = build_index(&schema, docs);
        let sf = index.values.get(&0).unwrap();

        // "electronics" in both docs
        let elec = sf.eq_str("electronics").unwrap();
        assert_eq!(elec.cardinality(), 2);

        // "sale" only in doc 0
        assert_eq!(sf.eq_str("sale").unwrap().to_vec(), [0]);

        // "premium" only in doc 1
        assert_eq!(sf.eq_str("premium").unwrap().to_vec(), [1]);
        assert_eq!(sf.doc_ords().unwrap().elems(1).collect::<Vec<_>>(), [sf.str_ord("electronics"), sf.str_ord("premium")]);
    }

    #[test]
    fn float_filter() {
        let mut schema = Schema {
            name: "test".into(),
            fields: vec![FieldDef {
                name: "rating".into(),
                field_type: FieldType::Float64,
                facet: false,
                index: true,
                optional: false,
                sort: true,
                stem: false,
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
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();

        let docs = vec![
            Document {
                id: 0,
                external_id: "1".into(),
                language: Language::English,
                fields: vec![FieldValue::Float64(3.5)].into(),
                record: None,
            },
            Document {
                id: 0,
                external_id: "2".into(),
                language: Language::English,
                fields: vec![FieldValue::Float64(4.8)].into(),
                record: None,
            },
        ];

        let index = build_index(&schema, docs);
        let nf = index.values.get(&0).unwrap();

        // Both floats in the dictionary, ordinals in value order
        assert_eq!(nf.len(), 2);
        assert!(nf.is_float());
        assert_eq!(nf.doc_f64(0), Some(3.5));
        assert_eq!(nf.doc_ord(1), Some(1), "4.8 sorts after 3.5");
    }
}
