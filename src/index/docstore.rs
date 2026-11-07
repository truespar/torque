//! A segment's stored documents: the FIELDS, DOCS and IDS sections of its segment file (TQBF spec
//! §5). Hits and GET by id render from these records; highlighting, document updates and rebuilds
//! read typed field values decoded from them. Filters, facets and sorting never touch them.

use crate::schema::{DocId, Document, Schema};
use crate::tqbf::registry::FieldRegistry;
use crate::tqbf::segment::RecordStore;
use std::io;
use std::sync::Arc;

pub struct DocStore {
    records: Option<Arc<RecordStore>>,
    total: u32,
}

/// The document's record, or one rendered from its typed fields (typed TCP ingest has no JSON).
fn record_of(doc: &Document, schema: &Schema) -> io::Result<Arc<[u8]>> {
    if let Some(r) = &doc.record {
        return Ok(Arc::clone(r));
    }
    let mut json = crate::index::document_to_json(doc, schema);
    // the language its words were analyzed in, which the record's readers (filters, rebuilds) need
    if doc.language != schema.default_language {
        if let (Some(obj), Ok(language)) = (json.as_object_mut(), serde_json::to_value(doc.language)) {
            obj.entry("language").or_insert(language);
        }
    }
    schema
        .registry
        .encode(&json, &doc.external_id)
        .map(Into::into)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("document `{}`: {e}", doc.external_id)))
}

/// zstd level for documents compressed by this server; offline segment builds use 19.
static DOCS_LEVEL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// zstd level of stored documents for builds in this process (offline builds use 19); 0 restores
/// the default (`TORQUE_DOCS_ZSTD_LEVEL`, else 3).
pub fn set_docs_level(level: i32) {
    DOCS_LEVEL.store(level, std::sync::atomic::Ordering::Relaxed);
}

thread_local! {
    /// Builds on this thread train no dictionary (`untrained`).
    static UNTRAINED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with the stored documents of the builds it runs on this thread compressed without a
/// dictionary trained on them: a realtime write's segment, merged soon, and compressed then with
/// the collection's (`DocStore::merge`); training one costs far more than the write.
pub fn untrained<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNTRAINED.set(self.0);
        }
    }
    let _restore = Restore(UNTRAINED.replace(true));
    f()
}

fn docs_options() -> crate::tqbf::docstore::DocsOptions {
    let set = DOCS_LEVEL.load(std::sync::atomic::Ordering::Relaxed);
    let level = if set != 0 {
        set
    } else {
        std::env::var("TORQUE_DOCS_ZSTD_LEVEL").ok().and_then(|v| v.parse().ok()).unwrap_or(3)
    };
    let mut opts = crate::tqbf::docstore::DocsOptions { level, ..Default::default() };
    if UNTRAINED.get() {
        opts.dict_size = 0;
    }
    opts
}

impl DocStore {
    pub fn empty() -> Self {
        Self { records: None, total: 0 }
    }

    /// Store the documents of a segment, given in doc id order `0..docs.len()`.
    pub fn build(docs: &[Document], schema: &Schema) -> io::Result<Self> {
        if docs.is_empty() {
            return Ok(Self::empty());
        }
        let records: Vec<Arc<[u8]>> = docs.iter().map(|d| record_of(d, schema)).collect::<io::Result<_>>()?;
        let refs: Vec<&[u8]> = records.iter().map(|r| &**r).collect();
        let store = RecordStore::build(&schema.registry.load(), &refs, &docs_options())?;
        Ok(Self::from_records(store))
    }

    pub fn from_records(records: RecordStore) -> Self {
        let total = records.total_docs();
        Self { records: Some(Arc::new(records)), total }
    }

    #[inline]
    pub fn total_docs(&self) -> u32 {
        self.total
    }

    pub fn records(&self) -> Option<&Arc<RecordStore>> {
        self.records.as_ref()
    }

    /// Doc id of an external id (the IDS FST).
    #[inline]
    pub fn doc_id(&self, external_id: &str) -> Option<DocId> {
        self.records.as_ref()?.doc_id(external_id)
    }

    /// External id of a document (the first item of its record).
    pub fn external_id(&self, doc_id: DocId) -> Option<String> {
        let rec = self.records.as_ref()?.record(doc_id).ok()??;
        crate::tqbf::record::record_id(&rec).ok().map(str::to_string)
    }

    /// The document with its typed field values, decoded from its record.
    pub fn get_doc(&self, doc_id: DocId, schema: &Schema) -> Option<Document> {
        self.typed_doc(doc_id, schema, false).unwrap_or_else(|e| {
            tracing::error!(doc_id, "unreadable stored document: {e}");
            None
        })
    }

    /// The document's record as JSON under `registry`, without typing its values. None when the
    /// store has no record of it.
    pub fn record_json(&self, doc_id: DocId, registry: &FieldRegistry) -> Result<Option<serde_json::Value>, String> {
        let Some(records) = self.records.as_ref() else { return Ok(None) };
        let Some(rec) = records.record(doc_id).map_err(|e| e.to_string())? else { return Ok(None) };
        crate::tqbf::record::to_value(&rec, registry).map(Some).map_err(|e| e.to_string())
    }

    /// The document decoded from its record and typed against `schema`: validated as a new write
    /// is when `strict` (a schema change types every document against the new schema), else with
    /// the fields that fit. None when the store has no record of it.
    pub fn typed_doc(&self, doc_id: DocId, schema: &Schema, strict: bool) -> Result<Option<Document>, String> {
        let Some(records) = self.records.as_ref() else { return Ok(None) };
        let Some(rec) = records.record(doc_id).map_err(|e| e.to_string())? else { return Ok(None) };
        let json = crate::tqbf::record::to_value(&rec, &records.registry).map_err(|e| e.to_string())?;
        let (external_id, language, fields) = crate::ingest::typed_fields(&json, schema, strict)?;
        Ok(Some(Document { id: doc_id, external_id, language, fields, record: Some(Arc::from(&*rec)) }))
    }

    /// The stored documents of a merge (`index::merge`), under the collection's registry (which
    /// extends every source's: ids are append-only). The records move over in merged order,
    /// compressed with `dictionary` (the collection's: its segments share one), else the largest
    /// source's, else one trained on them when they are enough (`trains`). A full block of a source
    /// (cut at the block size) compressed with that dictionary whose documents all come over, side
    /// by side, is copied as it is (Lucene's bulk copy of clean chunks); the other records are
    /// compressed again in blocks of the block size, so small segments' blocks do not pile up in
    /// merged ones. The external ids are the union of the sources' IDS, renumbered.
    pub fn merge(schema: &Schema, sources: &[&crate::index::Segment], docs: &crate::index::merge::DocMap, dictionary: Option<&[u8]>) -> io::Result<Self> {
        use crate::index::merge::{invalid, GONE};
        use crate::schema::DocId;
        use crate::tqbf::docstore::{self as section, Part};
        let stores: Vec<Option<&RecordStore>> = sources.iter().map(|s| s.doc_store.records().map(|r| &**r)).collect();
        let store = |s: u32| stores[s as usize].ok_or_else(|| invalid(format!("source {s} keeps no stored documents")));
        let opts = docs_options();
        let order = docs.order();
        let reuse = dictionary
            .filter(|d| !d.is_empty())
            .or_else(|| stores.iter().flatten().map(|s| s.docs()).filter(|d| !d.dictionary().is_empty()).max_by_key(|d| d.total_docs()).map(|d| d.dictionary()));
        // none to use: one trained when the records are enough (their size at most every block's)
        let raw_bytes = || stores.iter().flatten().map(|s| (0..s.docs().block_count()).map(|b| s.docs().block_raw_len(b)).sum::<usize>()).sum();
        let train = reuse.is_none() && section::trains(order.len(), raw_bytes(), opts.dict_size);
        let target = reuse.unwrap_or(&[]);
        // the sources whose blocks are compressed as the merged segment's
        let same: Vec<bool> = stores.iter().map(|s| !train && s.is_some_and(|s| s.docs().dictionary() == target)).collect();

        // the merged segment's blocks: copied, or records to compress, each record in its source
        // block (decompressed once: `blocks`)
        enum Piece {
            Records(std::ops::Range<usize>),
            Block(u32, usize),
        }
        let mut pieces: Vec<Piece> = Vec::new();
        let mut wanted: Vec<(u32, u32)> = Vec::new();
        let mut blocks: Vec<(u32, usize)> = Vec::new();
        let mut block_at: rustc_hash::FxHashMap<(u32, usize), u32> = Default::default();
        let mut k = 0;
        while k < order.len() {
            let (s, d) = order[k];
            let st = store(s)?.docs();
            if d >= st.total_docs() {
                return Err(invalid(format!("source {s} has no record of document {d}")));
            }
            let b = st.block_of(d);
            let r = st.block_docs(b);
            let len = (r.end - r.start) as usize;
            let whole = |run: &[(u32, DocId)]| run.iter().zip(r.clone()).all(|(&o, d)| o == (s, d));
            if same[s as usize] && r.start == d && st.block_raw_len(b) >= opts.block_target && order.get(k..k + len).is_some_and(whole) {
                pieces.push(Piece::Block(s, b));
                k += len;
                continue;
            }
            let at = *block_at.entry((s, b)).or_insert_with(|| {
                blocks.push((s, b));
                blocks.len() as u32 - 1
            });
            wanted.push((at, d - r.start));
            match pieces.last_mut() {
                Some(Piece::Records(r)) if r.end == wanted.len() - 1 => r.end += 1,
                _ => pieces.push(Piece::Records(wanted.len() - 1..wanted.len())),
            }
            k += 1;
        }
        // every block read into one buffer (its blocks freed at once), in parallel when several
        fn read<'a>(stores: &[Option<&'a RecordStore>], decs: &mut [Option<zstd::bulk::Decompressor<'a>>], (s, b): (u32, usize), out: &mut [u8]) -> io::Result<()> {
            let st = stores[s as usize].expect("a store").docs();
            if decs[s as usize].is_none() {
                decs[s as usize] = Some(st.decompressor()?);
            }
            st.read_block_into(b, decs[s as usize].as_mut().expect("a decompressor"), out)
        }
        let mut raw_at = Vec::with_capacity(blocks.len() + 1);
        raw_at.push(0usize);
        for &(s, b) in &blocks {
            raw_at.push(raw_at.last().unwrap() + stores[s as usize].expect("a store").docs().block_raw_len(b));
        }
        let mut arena = vec![0u8; *raw_at.last().unwrap()];
        let mut outs: Vec<&mut [u8]> = Vec::with_capacity(blocks.len());
        let mut rest = arena.as_mut_slice();
        for w in raw_at.windows(2) {
            let (out, tail) = rest.split_at_mut(w[1] - w[0]);
            outs.push(out);
            rest = tail;
        }
        let contexts = || -> Vec<Option<zstd::bulk::Decompressor<'_>>> { (0..stores.len()).map(|_| None).collect() };
        if blocks.len() > 1 && *raw_at.last().unwrap() >= section::PARALLEL_BYTES {
            use rayon::prelude::*;
            outs.par_iter_mut().zip(blocks.par_iter()).try_for_each_init(contexts, |decs, (out, &b)| read(&stores, decs, b, out))?;
        } else {
            let mut decs = contexts();
            for (out, &b) in outs.iter_mut().zip(&blocks) {
                read(&stores, &mut decs, b, out)?;
            }
        }
        drop(outs);
        let raw: Vec<&[u8]> = raw_at.windows(2).map(|w| &arena[w[0]..w[1]]).collect();
        let refs: Vec<&[u8]> = wanted.iter().map(|&(at, i)| section::block_record(raw[at as usize], i as usize)).collect::<io::Result<_>>()?;
        let trained = if train { section::train_dictionary(&refs, opts.dict_size) } else { Vec::new() };
        let dict = if train { trained.as_slice() } else { target };
        let parts: Vec<Part<'_>> = pieces
            .iter()
            .map(|p| match p {
                Piece::Records(r) => Part::Records(&refs[r.clone()]),
                &Piece::Block(s, b) => {
                    let d = stores[s as usize].expect("a store").docs();
                    Part::Block { frame: d.frame(b), docs: d.block_docs(b).len() as u32 }
                }
            })
            .collect();
        let docs_section = section::build_parts(&parts, dict, &opts)?;

        let mut ids = fst::MapBuilder::memory_sized(docs.len() as usize);
        let mut op = fst::map::OpBuilder::new();
        let with: Vec<usize> = (0..stores.len()).filter(|&s| stores[s].is_some()).collect();
        for &s in &with {
            op = op.add(stores[s].expect("a store").ids().map());
        }
        let mut union = op.union();
        let mut n = 0u32;
        while let Some((id, ivs)) = fst::Streamer::next(&mut union) {
            let mut live = ivs.iter().map(|iv| docs.get(with[iv.index], iv.value as crate::schema::DocId)).filter(|&k| k != GONE);
            let Some(k) = live.next() else { continue };
            if live.next().is_some() {
                return Err(invalid(format!("document `{}` is merged twice", String::from_utf8_lossy(id))));
            }
            ids.insert(id, u64::from(k)).map_err(io::Error::other)?;
            n += 1;
        }
        if n != docs.len() {
            return Err(invalid(format!("{n} ids for {} documents", docs.len())));
        }
        let ids = ids.into_inner().map_err(io::Error::other)?;
        Ok(Self::from_records(RecordStore::assemble(&schema.registry.load(), docs_section, ids, docs.len())?))
    }

    /// The dictionary the stored documents are compressed with (empty: none).
    pub fn dictionary(&self) -> &[u8] {
        self.records.as_ref().map_or(&[], |r| r.docs().dictionary())
    }

    /// Every document in doc id order (rebuilds, export).
    pub fn iter_docs<'a>(&'a self, schema: &'a Schema) -> impl Iterator<Item = Document> + 'a {
        (0..self.total).filter_map(move |id| self.get_doc(id, schema))
    }

    /// Heap bytes beyond the record sections.
    pub fn memory_usage(&self) -> usize {
        std::mem::size_of::<Self>()
    }

    /// Bytes of the stored-document sections (compressed records; heap until persisted, then mapped).
    pub fn record_store_bytes(&self) -> usize {
        self.records.as_ref().map_or(0, |r| r.stored_bytes())
    }
}
