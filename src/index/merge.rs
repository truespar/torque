//! Segments merged from their sections (Lucene's segment merge): a merged segment's sections are
//! made from the sources' sections, every document renumbered to its place in the merged order and
//! the deleted ones left out. Stored records, text postings with their frequencies and field
//! lengths, value dictionaries with their postings and per-document ordinals, bools, geo points and
//! vectors move over as they are; no document is parsed, typed or tokenized again. The merged
//! segment is the one a build of the same documents in the same order makes, section for section,
//! but for two: the graph of the largest source without deletions is kept and the other vectors are
//! inserted into it (`vector::VectorIndex::merge`), and RAWTERMS may keep a token form only deleted
//! copies held (`InvertedIndex::merge`).

use crate::bitmap::Bitmap;
use crate::index::values::{self, FieldValues};
use crate::index::{DocStore, Segment};
use crate::schema::{DocId, FieldId, FieldType, Schema, VecDist};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::io;

/// A document not carried over (deleted).
pub const GONE: DocId = DocId::MAX;

/// Below this many documents a merge runs on the calling thread (`ingest::builder`'s
/// SMALL_BUILD_DOCS): a pool's start costs more than the work.
const SMALL_MERGE_DOCS: usize = 4096;

/// The documents of a merge: per source, each of its documents' number in the merged segment
/// (`GONE`: left out), and in merged order where each comes from.
pub struct DocMap {
    maps: Vec<Vec<DocId>>,
    order: Vec<(u32, DocId)>,
    /// Per source with documents left out: the ones carried over (None: every one).
    live: Vec<Option<Bitmap>>,
}

impl DocMap {
    /// `order` lists the merged segment's documents in order: (source, document there), each
    /// document of a source at most once.
    pub fn new(sources: &[&Segment], order: &[(u32, DocId)]) -> io::Result<Self> {
        let mut maps: Vec<Vec<DocId>> = sources.iter().map(|s| vec![GONE; s.total_docs() as usize]).collect();
        for (k, &(s, d)) in order.iter().enumerate() {
            let slot = maps
                .get_mut(s as usize)
                .and_then(|m| m.get_mut(d as usize))
                .ok_or_else(|| invalid(format!("document {d} of source {s} is past its segment")))?;
            if *slot != GONE {
                return Err(invalid(format!("document {d} of source {s} is merged twice")));
            }
            *slot = k as DocId;
        }
        let live = maps
            .iter()
            .map(|m| m.contains(&GONE).then(|| m.iter().enumerate().filter(|&(_, &k)| k != GONE).map(|(d, _)| d as DocId).collect()))
            .collect();
        Ok(Self { maps, order: order.to_vec(), live })
    }

    /// Documents in the merged segment.
    #[inline]
    pub fn len(&self) -> u32 {
        self.order.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// In merged order, (source, document there).
    #[inline]
    pub fn order(&self) -> &[(u32, DocId)] {
        &self.order
    }

    /// Source `s`'s document `d` in the merged segment (`GONE`: left out).
    #[inline]
    pub fn get(&self, s: usize, d: DocId) -> DocId {
        self.maps[s].get(d as usize).copied().unwrap_or(GONE)
    }

    /// Per document of source `s`, its number in the merged segment (`GONE`: left out).
    #[inline]
    pub fn map(&self, s: usize) -> &[DocId] {
        &self.maps[s]
    }

    /// Every document of source `s` is carried over.
    #[inline]
    pub fn whole(&self, s: usize) -> bool {
        self.live[s].is_none()
    }

    /// How many of `docs` (documents of source `s`) are carried over.
    #[inline]
    pub fn live_count(&self, s: usize, docs: &Bitmap) -> u64 {
        match &self.live[s] {
            None => docs.cardinality(),
            Some(live) => docs.and_cardinality(live),
        }
    }

    /// Whether any of `docs` (documents of source `s`) is carried over.
    #[inline]
    pub fn any_live(&self, s: usize, docs: &Bitmap) -> bool {
        match &self.live[s] {
            None => !docs.is_empty(),
            Some(live) => docs.intersects(live),
        }
    }

    /// Append `docs` (documents of source `s`) carried over, renumbered, in their order.
    #[inline]
    pub fn push_mapped(&self, s: usize, docs: &Bitmap, out: &mut Vec<DocId>) {
        let map = &self.maps[s];
        out.extend(docs.iter().filter_map(|d| map.get(d as usize).copied().filter(|&k| k != GONE)));
    }
}

/// Sort `docs` unless sorted already (postings gathered source after source: runs the stable sort
/// merges).
#[inline]
pub(crate) fn sort_docs(docs: &mut [DocId]) {
    if !docs.is_sorted() {
        docs.sort();
    }
}

pub(crate) fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("segment merge: {}", msg.into()))
}

/// Run `f` over `items`, in parallel when `big`.
fn each<T: Sync, R: Send>(big: bool, items: &[T], f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
    if big {
        items.par_iter().map(f).collect()
    } else {
        items.iter().map(f).collect()
    }
}

/// `sources` merged into one segment holding the documents `order` lists ((source, document
/// there), in the merged segment's order): the segment a build of those documents in that order
/// makes (`ingest::build_index`), its sections made from the sources' sections. Its stored
/// documents are compressed with `dictionary` (the collection's), else as `DocStore::merge` says.
/// Its base and `_seq_id`s are the caller's (`collection::write::merge`).
pub fn merge(schema: &Schema, sources: &[&Segment], order: &[(u32, DocId)], dictionary: Option<&[u8]>) -> io::Result<Segment> {
    if order.is_empty() {
        return Ok(Segment::empty());
    }
    let docs = DocMap::new(sources, order)?;
    let n = docs.len();
    let big = n as usize >= SMALL_MERGE_DOCS;

    let field_values = || -> io::Result<Vec<(FieldId, FieldValues)>> {
        let merged = each(big, &schema.fields, |f| values::merge_field(f, sources, &docs).map(|v| v.map(|v| (f.id, v))));
        Ok(merged.into_iter().collect::<io::Result<Vec<_>>>()?.into_iter().flatten().collect())
    };
    let (doc_store, merged_values) = if big {
        rayon::join(|| DocStore::merge(schema, sources, &docs, dictionary), field_values)
    } else {
        (DocStore::merge(schema, sources, &docs, dictionary), field_values())
    };
    let (doc_store, merged_values) = (doc_store?, merged_values?);
    let (mut values, mut bools) = (FxHashMap::default(), FxHashMap::default());
    for (fid, v) in merged_values {
        match v {
            FieldValues::Values(v) => {
                values.insert(fid, v);
            }
            FieldValues::Bools(b) => {
                bools.insert(fid, b);
            }
        }
    }

    // Geo points and vectors.
    let mut geo_indexes = FxHashMap::default();
    for f in schema.fields.iter().filter(|f| matches!(f.field_type, FieldType::GeoPoint | FieldType::GeoPointArray)) {
        let srcs: Vec<_> = sources.iter().map(|s| s.geo_indexes.get(&f.id)).collect();
        if let Some(g) = crate::index::geo::GeoIndex::merge(&srcs, &docs, f.field_type == FieldType::GeoPointArray)? {
            geo_indexes.insert(f.id, g);
        }
    }
    let mut vector_indexes = FxHashMap::default();
    for f in &schema.fields {
        let FieldType::FloatArray { num_dim } = f.field_type else { continue };
        if num_dim == 0 {
            continue;
        }
        let srcs: Vec<_> = sources.iter().map(|s| s.vector_indexes.get(&f.id)).collect();
        // every source with documents has one (an empty segment, none)
        if docs.order().iter().any(|&(s, _)| srcs[s as usize].is_none()) {
            return Err(invalid(format!("field `{}` has no vector index in a source with documents", f.name)));
        }
        let metric = f.vec_dist.unwrap_or(VecDist::Cosine);
        vector_indexes.insert(f.id, crate::vector::VectorIndex::merge(&srcs, &docs, num_dim, metric)?);
    }

    let text_fields: Vec<_> = schema.fields.iter().filter(|f| f.index && f.field_type.is_searchable()).collect();
    let inverted = each(big, &text_fields, |f| {
        let srcs: Vec<_> = sources.iter().enumerate().filter_map(|(s, seg)| seg.inverted.get(&f.id).map(|i| (s, i))).collect();
        let merged = crate::index::inverted::InvertedIndex::merge(&srcs, &docs)?;
        merged
            .map(|mut i| {
                if f.name_matching {
                    i.add_keys()?;
                }
                Ok((f.id, i))
            })
            .transpose()
    });
    let inverted: FxHashMap<_, _> = inverted.into_iter().collect::<io::Result<Vec<_>>>()?.into_iter().flatten().collect();

    let reference_indexes = crate::index::reference::ReferenceIndex::for_segment(schema, &values);
    let mut all_docs = Bitmap::new();
    all_docs.add_range(0..n);
    let mut segment = Segment::from_parts(
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
    segment.compact();
    Ok(segment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::write::{self, Writes};
    use crate::index::IndexBundle;
    use crate::schema::Document;
    use crate::tqbf::container::{kind, SegmentFile};
    use std::sync::Arc;

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n.max(1)
        }

        fn chance(&mut self, pct: u64) -> bool {
            self.below(100) < pct
        }

        fn unit(&mut self) -> f64 {
            self.below(1 << 20) as f64 / f64::from(1 << 20)
        }
    }

    /// Every kind of field a segment indexes.
    fn schema() -> Arc<Schema> {
        let mut s: Schema = serde_json::from_value(serde_json::json!({
            "name": "m",
            "default_sorting_field": "rank",
            "fields": [
                {"name": "title", "type": "string"},
                {"name": "body", "type": "string", "optional": true},
                {"name": "tags", "type": "string[]", "facet": true, "optional": true},
                {"name": "labels", "type": "string[]", "optional": true},
                {"name": "aliases", "type": "string[]", "name_matching": true, "optional": true},
                {"name": "city", "type": "string", "facet": true, "optional": true},
                {"name": "name", "type": "string", "sort": true, "optional": true},
                {"name": "rank", "type": "int32"},
                {"name": "big", "type": "int64", "facet": true, "optional": true},
                {"name": "price", "type": "float", "optional": true},
                {"name": "score", "type": "float", "sort": true, "optional": true},
                {"name": "nums", "type": "int32[]", "facet": true, "optional": true},
                {"name": "bigs", "type": "int64[]", "optional": true},
                {"name": "fl", "type": "float[]", "optional": true},
                {"name": "ok", "type": "bool", "optional": true},
                {"name": "oks", "type": "bool[]", "optional": true},
                {"name": "loc", "type": "geopoint", "optional": true},
                {"name": "locs", "type": "geopoint[]", "optional": true},
                {"name": "owner", "type": "string", "reference": "people.id", "optional": true},
                {"name": "vec", "type": "float[]", "num_dim": 8, "optional": true},
                {"name": "emb", "type": "float[]", "num_dim": 64, "vec_dist": "ip", "optional": true},
                {"name": "note", "type": "string", "index": false, "optional": true}
            ]
        }))
        .unwrap();
        s.assign_field_ids();
        Arc::new(s)
    }

    const WORDS: [&str; 24] = [
        "bil", "båt", "hus", "skog", "stad", "väg", "göteborg", "malmö", "aktie", "bolag", "skuld", "konkurs",
        "styrelse", "ägare", "fordon", "räkning", "lån", "ränta", "hyra", "mark", "fastighet", "tomt", "lager", "kassa",
    ];

    /// `min` words and up to `spread` - 1 more.
    fn words(rng: &mut Rng, min: u64, spread: u64) -> String {
        let n = min + rng.below(spread);
        (0..n).map(|_| WORDS[rng.below(WORDS.len() as u64) as usize]).collect::<Vec<_>>().join(" ")
    }

    fn doc(rng: &mut Rng, id: usize, schema: &Schema) -> Document {
        let mut j = serde_json::json!({"id": format!("d{id}"), "title": words(rng, 1, 6), "rank": rng.below(40) as i64 - 10});
        let o = j.as_object_mut().unwrap();
        let mut put = |k: &str, v: serde_json::Value| {
            o.insert(k.to_string(), v);
        };
        if rng.chance(70) {
            put("body", words(rng, 0, 40).into());
        }
        if rng.chance(60) {
            put("tags", (0..rng.below(5)).map(|_| WORDS[rng.below(6) as usize]).collect::<Vec<_>>().into());
        }
        if rng.chance(40) {
            put("labels", (0..rng.below(3)).map(|_| words(rng, 2, 1)).collect::<Vec<_>>().into());
        }
        if rng.chance(40) {
            put("aliases", (0..rng.below(3)).map(|_| words(rng, 1, 3)).collect::<Vec<_>>().into());
        }
        if rng.chance(50) {
            put("city", ["Lund", "Umeå", "Visby", "Kiruna", "Ystad"][rng.below(5) as usize].into());
        }
        if rng.chance(50) {
            put("name", words(rng, 2, 1).into());
        }
        if rng.chance(50) {
            put("big", ((rng.below(1 << 40) as i64) - (1 << 39)).into());
        }
        if rng.chance(50) {
            put("price", [0.0, -0.0, 2.5, -1.25, 1e9, rng.unit()][rng.below(6) as usize].into());
        }
        if rng.chance(50) {
            put("score", (rng.unit() * 100.0).into());
        }
        if rng.chance(50) {
            put("nums", (0..rng.below(4)).map(|_| rng.below(20) as i64).collect::<Vec<_>>().into());
        }
        if rng.chance(30) {
            put("bigs", (0..rng.below(3)).map(|_| rng.below(1 << 50) as i64).collect::<Vec<_>>().into());
        }
        if rng.chance(30) {
            put("fl", (0..rng.below(3)).map(|_| rng.unit() - 0.5).collect::<Vec<_>>().into());
        }
        if rng.chance(50) {
            put("ok", rng.chance(50).into());
        }
        if rng.chance(40) {
            put("oks", (0..rng.below(3)).map(|_| rng.chance(50)).collect::<Vec<_>>().into());
        }
        if rng.chance(60) {
            put("loc", serde_json::json!([55.0 + rng.unit() * 10.0, 11.0 + rng.unit() * 12.0]));
        }
        if rng.chance(40) {
            put("locs", (0..rng.below(3)).map(|_| serde_json::json!([55.0 + rng.unit() * 10.0, 11.0 + rng.unit() * 12.0])).collect::<Vec<_>>().into());
        }
        if rng.chance(50) {
            put("owner", format!("p{}", rng.below(30)).into());
        }
        if rng.chance(80) {
            put("vec", (0..8).map(|_| rng.unit() - 0.5).collect::<Vec<_>>().into());
        }
        if rng.chance(80) {
            put("emb", (0..64).map(|_| rng.unit() - 0.5).collect::<Vec<_>>().into());
        }
        if rng.chance(30) {
            put("note", words(rng, 3, 1).into());
        }
        crate::ingest::parse_json_document(&j, schema, 0).unwrap()
    }

    /// A collection after `writes` writes of up to `per` documents: new ones, ones written again
    /// (which keep their place) and deletions.
    fn written(schema: &Arc<Schema>, seed: u64, writes: usize, per: u64) -> IndexBundle {
        let mut rng = Rng(seed);
        let mut b = IndexBundle::empty(Arc::clone(schema));
        let mut next = 0usize;
        for _ in 0..writes {
            let mut w = Writes::default();
            for _ in 0..1 + rng.below(per) {
                let id = if next > 0 && rng.chance(30) {
                    rng.below(next as u64) as usize
                } else {
                    next += 1;
                    next - 1
                };
                w.upsert(doc(&mut rng, id, schema));
            }
            if next > 0 && rng.chance(40) {
                for _ in 0..1 + rng.below(3) {
                    w.delete(&format!("d{}", rng.below(next as u64)));
                }
            }
            b = write::apply(&b, w);
        }
        b
    }

    /// Every section of a segment's file but its vectors (graphs differ), stored documents (blocks
    /// are copied whole) and token forms (`raw_forms`), by (kind, field).
    fn sections(seg: &Segment) -> Vec<((u16, u16), Vec<u8>)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tqbf");
        crate::persistence::index::write_segment(seg, None, &path).unwrap();
        let file = SegmentFile::open(&path).unwrap();
        let mut out: Vec<_> = file
            .sections
            .iter()
            .filter(|e| e.kind != kind::VECTORS && e.kind != kind::DOCS && e.kind != kind::RAWTERMS)
            .map(|e| ((e.kind, e.field_id), file.section(e.kind, e.field_id).unwrap().to_vec()))
            .collect();
        out.sort();
        out
    }

    /// The keys of a field's RAWTERMS.
    fn raw_forms(seg: &Segment, field: FieldId) -> Vec<Vec<u8>> {
        use fst::Streamer;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tqbf");
        crate::persistence::index::write_segment(seg, None, &path).unwrap();
        let file = SegmentFile::open(&path).unwrap();
        let Some(raw) = file.section(kind::RAWTERMS, field) else { return Vec::new() };
        let map = fst::Map::new(raw).unwrap();
        let mut stream = map.stream();
        let mut out = Vec::new();
        while let Some((k, _)) = stream.next() {
            out.push(k.to_vec());
        }
        out
    }

    /// The live documents of `range`, in `_seq_id` order, built as one segment.
    fn rebuilt(b: &IndexBundle, range: std::ops::Range<usize>) -> Segment {
        let mut live: Vec<(u64, Document)> = Vec::new();
        for i in range {
            let seg = &b.segments[i];
            for d in 0..seg.total_docs() {
                if !b.deleted_for(i).is_some_and(|m| m.contains(d)) {
                    live.push((seg.seq_id(d), seg.get_doc(d, &b.schema).unwrap()));
                }
            }
        }
        live.sort_by_key(|l| l.0);
        crate::ingest::build_index(&b.schema, live.into_iter().map(|l| l.1).collect())
    }

    /// The stored records of a segment, in order.
    fn records(seg: &Segment) -> Vec<Vec<u8>> {
        let store = seg.doc_store.records().unwrap();
        (0..seg.total_docs()).map(|d| store.record(d).unwrap().unwrap().to_vec()).collect()
    }

    /// A merge of `range` is the segment a build of its live documents makes: every section the
    /// same, the same records, and the same vectors, codes and norms. Its token forms hold the
    /// build's, and may keep ones only deleted copies held.
    fn check(b: &IndexBundle, range: std::ops::Range<usize>) {
        let mut merged = write::merge(b, range.clone()).unwrap().segment;
        merged.seq = crate::index::SeqIds::default();
        let built = rebuilt(b, range);
        assert_eq!(merged.total_docs(), built.total_docs());
        let (m, r) = (sections(&merged), sections(&built));
        assert_eq!(m.iter().map(|s| s.0).collect::<Vec<_>>(), r.iter().map(|s| s.0).collect::<Vec<_>>(), "the same sections");
        for ((key, a), (_, b)) in m.iter().zip(&r) {
            assert!(a == b, "section kind {} of field {} differs", key.0, key.1);
        }
        assert!(records(&merged) == records(&built), "the same records");
        for f in &b.schema.fields {
            let (kept, wanted) = (raw_forms(&merged, f.id), raw_forms(&built, f.id));
            assert!(wanted.iter().all(|k| kept.contains(k)), "token forms of field `{}`", f.name);
        }
        if merged.total_docs() >= 30 {
            for k in [kind::FIELDS, kind::IDS, kind::TERMS, kind::POSTINGS, kind::FREQS, kind::NORMS, kind::VALUES, kind::NUM_VALUES, kind::VALUE_DOCS, kind::DOC_ORDS, kind::BOOL, kind::GEO, kind::KEYS] {
                assert!(m.iter().any(|s| s.0 .0 == k), "a section of kind {k} compared");
            }
        }
        assert_eq!(merged.vector_indexes.len(), built.vector_indexes.len());
        for (fid, v) in &merged.vector_indexes {
            let w = &built.vector_indexes[fid];
            assert_eq!(&v.vectors[..], &w.vectors[..], "vectors of field {fid}");
            assert_eq!(v.codes.as_deref(), w.codes.as_deref(), "RaBitQ codes of field {fid}");
            assert_eq!(v.norms.as_deref(), w.norms.as_deref(), "RaBitQ norms of field {fid}");
            assert_eq!((v.num_dim, v.metric, v.effective_metric()), (w.num_dim, w.metric, w.effective_metric()));
        }
    }

    #[test]
    fn a_merge_is_the_build_of_its_live_documents() {
        let s = schema();
        for seed in 1..6 {
            let b = written(&s, seed, 14, 12);
            assert!(b.deleted_per_segment.iter().any(|d| !d.is_empty()), "some copies deleted");
            check(&b, 0..b.segments.len());
            check(&b, 2..9);
            check(&b, 5..6);
        }
    }

    #[test]
    fn a_merge_of_merges_is_the_build_too() {
        let s = schema();
        let mut b = written(&s, 11, 30, 8);
        for range in [0..10, 5..15] {
            let snapshot = b.clone();
            let merged = write::merge(&snapshot, range.clone()).unwrap();
            b = write::publish(&snapshot, &snapshot, range, merged, true).unwrap();
        }
        check(&b, 0..b.segments.len());
    }

    #[test]
    fn many_documents_merge_on_many_threads_the_same() {
        let s = schema();
        let b = written(&s, 21, 60, 300);
        assert!(b.physical_total_docs() as usize > SMALL_MERGE_DOCS, "past the threshold");
        check(&b, 0..b.segments.len());
    }

    #[test]
    fn a_merge_of_deleted_documents_only_is_empty() {
        let s = schema();
        let mut b = written(&s, 3, 3, 4);
        let mut w = Writes::default();
        for i in 0..20 {
            w.delete(&format!("d{i}"));
        }
        b = write::apply(&b, w);
        assert_eq!(b.total_docs, 0);
        let merged = write::merge(&b, 0..b.segments.len()).unwrap();
        assert_eq!(merged.segment.total_docs(), 0);
    }

    /// The blocks of a segment's stored documents.
    fn frames(seg: &Segment) -> Vec<Vec<u8>> {
        let d = seg.doc_store.records().unwrap().docs();
        (0..d.block_count()).map(|b| d.frame(b).to_vec()).collect()
    }

    /// The full blocks of a segment's stored documents (cut at the block size).
    fn full_frames(seg: &Segment) -> Vec<Vec<u8>> {
        let d = seg.doc_store.records().unwrap().docs();
        (0..d.block_count()).filter(|&b| d.block_raw_len(b) >= 16 * 1024).map(|b| d.frame(b).to_vec()).collect()
    }

    #[test]
    fn small_segments_blocks_are_compressed_again_together() {
        // no dictionary anywhere: the blocks of one-document segments are not copied one by one
        let s = schema();
        let mut rng = Rng(5);
        let segs: Vec<Segment> = (0..40).map(|i| crate::ingest::build_index(&s, vec![doc(&mut rng, i, &s)])).collect();
        let sources: Vec<&Segment> = segs.iter().collect();
        let order: Vec<(u32, DocId)> = (0..40).map(|s| (s, 0)).collect();
        let merged = merge(&s, &sources, &order, None).unwrap();
        assert!(merged.doc_store.dictionary().is_empty());
        assert!(frames(&merged).len() < 10, "{} blocks for 40 small records", frames(&merged).len());
        assert!(records(&merged) == sources.iter().flat_map(|g| records(g)).collect::<Vec<_>>());
    }

    #[test]
    fn stored_blocks_come_over_as_they_are() {
        // the collection's dictionary: trained on enough records
        let s = schema();
        let mut rng = Rng(77);
        let seg = |from: usize, n: usize, rng: &mut Rng| crate::ingest::build_index(&s, (from..from + n).map(|i| doc(rng, i, &s)).collect());
        let big = seg(0, 3000, &mut rng);
        let dict = big.doc_store.dictionary().to_vec();
        assert!(!dict.is_empty(), "a dictionary trained");
        let (small, more) = (seg(3000, 40, &mut rng), seg(3040, 500, &mut rng));
        let sources = [&big, &small, &more];
        let order: Vec<(u32, DocId)> = sources.iter().enumerate().flat_map(|(s, g)| (0..g.total_docs()).map(move |d| (s as u32, d))).collect();
        let merged = merge(&s, &sources, &order, None).unwrap();
        assert_eq!(merged.doc_store.dictionary(), &dict[..], "the largest source's dictionary");
        let f = frames(&merged);
        let full = full_frames(&big);
        assert!(full.len() + 1 == frames(&big).len(), "every block of the source but its last full");
        assert!(full.iter().all(|b| f.contains(b)), "the full blocks of the source with that dictionary copied");
        assert!(!f.contains(frames(&big).last().unwrap()), "its last block compressed again with the records after it");
        let all: Vec<Vec<u8>> = sources.iter().flat_map(|g| records(g)).collect();
        assert!(records(&merged) == all);

        // with the collection's dictionary, the other sources' records are compressed again
        let again = merge(&s, &[&small, &more], &order[3000..].iter().map(|&(s, d)| (s - 1, d)).collect::<Vec<_>>(), Some(&dict)).unwrap();
        assert_eq!(again.doc_store.dictionary(), &dict[..]);
        let whole = merge(&s, &[&big, &again], &order.iter().map(|&(s, d)| if s == 0 { (0, d) } else { (1, d + if s == 2 { 40 } else { 0 }) }).collect::<Vec<_>>(), Some(&dict)).unwrap();
        let f = frames(&whole);
        assert!(full_frames(&big).iter().chain(full_frames(&again).iter()).all(|b| f.contains(b)), "every full block copied");
        assert!(records(&whole) == all);

        // a block with a document left out is compressed again
        let mut gone = order.clone();
        gone.remove(5);
        let less = merge(&s, &sources, &gone, None).unwrap();
        assert_eq!(less.total_docs(), 3539);
        assert!(!frames(&less).contains(&frames(&big)[0]) && frames(&less).contains(&frames(&big)[1]));
    }

    fn vector_schema(metric: &str) -> Schema {
        let mut s: Schema = serde_json::from_value(serde_json::json!({
            "name": "v",
            "fields": [{"name": "v", "type": "float[]", "num_dim": 16, "vec_dist": metric}]
        }))
        .unwrap();
        s.assign_field_ids();
        s
    }

    /// Segments of `sizes` documents with random vectors.
    fn vector_segments(s: &Schema, sizes: &[usize], seed: u64) -> Vec<Segment> {
        let mut rng = Rng(seed);
        let mut next = 0;
        sizes
            .iter()
            .map(|&n| {
                let docs = (0..n)
                    .map(|_| {
                        next += 1;
                        let v: Vec<f64> = (0..16).map(|_| rng.unit() - 0.5).collect();
                        crate::ingest::parse_json_document(&serde_json::json!({"id": format!("v{next}"), "v": v}), s, 0).unwrap()
                    })
                    .collect();
                crate::ingest::build_index(s, docs)
            })
            .collect()
    }

    /// The share of the true 10 nearest neighbours (brute force) the graph finds, over queries at
    /// the index's own vectors.
    fn recall(v: &crate::vector::VectorIndex, n: u32) -> f64 {
        let (mut found, mut total) = (0usize, 0usize);
        for q in (0..n).step_by((n as usize / 60).max(1)) {
            let query = v.get_vector(q).to_vec();
            let truth: Vec<DocId> = v.flat_search(&query, 10, None).into_iter().map(|r| r.0).collect();
            let got: Vec<DocId> = v.search(&query, 10, 64, None).into_iter().map(|r| r.0).collect();
            found += truth.iter().filter(|t| got.contains(t)).count();
            total += truth.len();
        }
        found as f64 / total as f64
    }

    #[test]
    fn a_merge_keeps_the_largest_graph_and_finds_the_neighbours() {
        // sequential and parallel insertion, the base first and not
        for (metric, sizes) in [("cosine", vec![900, 150, 150, 100]), ("l2", vec![2600, 300, 300, 200]), ("ip", vec![500, 2000, 400])] {
            let sc = vector_schema(metric);
            let segs = vector_segments(&sc, &sizes, 5);
            let sources: Vec<&Segment> = segs.iter().collect();
            // the second source loses every third document: the largest of the rest is the base
            let order: Vec<(u32, DocId)> = sources
                .iter()
                .enumerate()
                .flat_map(|(s, seg)| (0..seg.total_docs()).filter(move |d| s != 1 || d % 3 != 0).map(move |d| (s as u32, d)))
                .collect();
            let merged = merge(&sc, &sources, &order, None).unwrap();
            let r = recall(&merged.vector_indexes[&0], merged.total_docs());
            let built = crate::ingest::build_index(&sc, order.iter().map(|&(s, d)| sources[s as usize].get_doc(d, &sc).unwrap()).collect());
            let rb = recall(&built.vector_indexes[&0], built.total_docs());
            assert!(r >= 0.95 && r >= rb - 0.03, "{metric}: recall {r}, a build's {rb}");
        }
    }

    #[test]
    fn a_source_without_documents_merges() {
        let sc = vector_schema("cosine");
        let segs = vector_segments(&sc, &[50], 3);
        let empty = Segment::empty();
        let order: Vec<(u32, DocId)> = (0..50).map(|d| (1, d)).collect();
        let merged = merge(&sc, &[&empty, &segs[0]], &order, None).unwrap();
        assert_eq!(merged.total_docs(), 50);
        assert_eq!(&merged.vector_indexes[&0].vectors[..], &segs[0].vector_indexes[&0].vectors[..]);
        assert!(records(&merged) == records(&segs[0]));
    }

    #[test]
    fn a_whole_segment_merged_alone_keeps_its_graph() {
        let sc = vector_schema("cosine");
        let segs = vector_segments(&sc, &[700], 9);
        let order: Vec<(u32, DocId)> = (0..700).map(|d| (0, d)).collect();
        let merged = merge(&sc, &[&segs[0]], &order, None).unwrap();
        let section = |seg: &Segment| {
            let mut w = crate::tqbf::container::SegmentWriter::new();
            seg.vector_indexes[&0].add_sections(0, &mut w);
            SegmentFile::from_vec(w.finish(seg.total_docs(), 0)).unwrap().section(kind::VECTORS, 0).unwrap().to_vec()
        };
        assert!(section(&merged) == section(&segs[0]), "the same vectors and graph");
    }
}
