//! One run: documents with consecutive doc ids, added on one thread and indexed as they arrive
//! into compact accumulators (interned terms and values, (term, doc, tf) triples, ordinals,
//! compressed record blocks). A run freezes into pieces: terms and values sorted, postings
//! delta-coded, ordinals bit-packed, whenever its accumulators reach `piece_bytes` and at its end.
//! The merge reads only frozen pieces, which may live in the builder's spill file.

use super::doc::{str_of, DocValues, KeyMap, Typed};
use super::intern::Interner;
use super::{BuildPlan, Rejected, Shared, MAX_SAMPLE_BYTES};
use crate::index::inverted::RAW_SEP;
use crate::index::values::{f64_key, Ords};
use crate::text::Analyzer;
use crate::tqbf::bytes::{Bytes, Pods};
use crate::tqbf::packed::{self, BitWriter};
use crate::tqbf::record::{self, put_varint};
use rustc_hash::FxHashMap;
use std::io;
use std::sync::Arc;

pub(crate) const NONE: u32 = u32::MAX;

/// Target size of a raw DOCS block (records plus 4 bytes each), as the DOCS section uses.
const BLOCK_TARGET: usize = 16 * 1024;
/// Documents between two checks of a piece's size.
const SIZE_CHECK_DOCS: u32 = 256;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

// -- accumulators ------------------------------------------------------------------------------

#[derive(Default)]
struct TextAcc {
    /// Token (language byte + token bytes) -> its term, analyzed once per piece.
    tokens: Interner,
    token_term: Vec<u32>,
    terms: Interner,
    /// "lowered\0term" of the lower-cased tokens that differ from their term.
    raw: Interner,
    /// (term, doc, tf), in doc order.
    post: Vec<(u32, u32, u32)>,
    /// (doc, field length in tokens) of the documents with tokens.
    lens: Vec<(u32, u32)>,
}

impl TextAcc {
    fn heap_bytes(&self) -> usize {
        self.tokens.heap_bytes()
            + self.terms.heap_bytes()
            + self.raw.heap_bytes()
            + 4 * self.token_term.capacity()
            + 12 * self.post.capacity()
            + 8 * self.lens.capacity()
    }
}

enum Dict {
    Str(Interner),
    Num { ids: FxHashMap<i64, u32>, keys: Vec<i64> },
}

struct ValueAcc {
    dict: Dict,
    mode: Ords,
    /// (value, doc), in doc order, one per value and document.
    post: Vec<(u32, u32)>,
    last_doc: Vec<u32>,
    /// Single: each document's value (NONE = none).
    single: Vec<u32>,
    /// Multi: each document's element count + 1 (0 = field absent), and the elements.
    counts: Vec<u32>,
    elems: Vec<u32>,
}

impl ValueAcc {
    fn new(strings: bool, mode: Ords) -> Self {
        let dict = if strings { Dict::Str(Interner::new()) } else { Dict::Num { ids: FxHashMap::default(), keys: Vec::new() } };
        Self { dict, mode, post: Vec::new(), last_doc: Vec::new(), single: Vec::new(), counts: Vec::new(), elems: Vec::new() }
    }

    #[inline]
    fn posting(&mut self, id: u32, doc: u32) {
        if id as usize == self.last_doc.len() {
            self.last_doc.push(NONE);
        }
        if self.last_doc[id as usize] != doc {
            self.last_doc[id as usize] = doc;
            self.post.push((id, doc));
        }
    }

    #[inline]
    fn str_id(&mut self, s: &[u8], doc: u32) -> u32 {
        let Dict::Str(t) = &mut self.dict else { unreachable!() };
        let id = t.intern(s).0;
        self.posting(id, doc);
        id
    }

    #[inline]
    fn num_id(&mut self, k: i64, doc: u32) -> u32 {
        let Dict::Num { ids, keys } = &mut self.dict else { unreachable!() };
        let id = *ids.entry(k).or_insert_with(|| {
            keys.push(k);
            keys.len() as u32 - 1
        });
        self.posting(id, doc);
        id
    }

    /// The document's value ids (NONE = null element).
    fn add(&mut self, ids: &[u32]) {
        match self.mode {
            Ords::None => {}
            Ords::Single => self.single.push(ids.iter().copied().find(|&i| i != NONE).unwrap_or(NONE)),
            Ords::Multi => {
                self.counts.push(ids.len() as u32 + 1);
                self.elems.extend_from_slice(ids);
            }
        }
    }

    fn absent(&mut self) {
        match self.mode {
            Ords::None => {}
            Ords::Single => self.single.push(NONE),
            Ords::Multi => self.counts.push(0),
        }
    }

    fn len(&self) -> usize {
        match &self.dict {
            Dict::Str(t) => t.len(),
            Dict::Num { keys, .. } => keys.len(),
        }
    }

    fn heap_bytes(&self) -> usize {
        let d = match &self.dict {
            Dict::Str(t) => t.heap_bytes(),
            Dict::Num { ids, keys } => 16 * ids.capacity() + 8 * keys.capacity(),
        };
        d + 8 * self.post.capacity() + 4 * (self.last_doc.capacity() + self.single.capacity() + self.counts.capacity() + self.elems.capacity())
    }
}

/// Records grouped into DOCS blocks: closed blocks compressed once the dictionary exists.
#[derive(Default)]
struct DocsAcc {
    block: Vec<u8>,
    lens: Vec<u32>,
    size: usize,
    /// First doc of every closed block (piece-local).
    firsts: Vec<u32>,
    /// Compressed frames of the first `frame_ends.len()` closed blocks; the others raw.
    frames: Vec<u8>,
    frame_ends: Vec<u64>,
    raw: Vec<u8>,
    raw_ends: Vec<u64>,
}

/// A raw DOCS block: u32 n, u32 record_offset[n + 1], records.
fn raw_block(lens: &[u32], body: &[u8], out: &mut Vec<u8>) {
    out.reserve(4 + 4 * (lens.len() + 1) + body.len());
    out.extend_from_slice(&(lens.len() as u32).to_le_bytes());
    let mut off = 0u32;
    out.extend_from_slice(&off.to_le_bytes());
    for &l in lens {
        off += l;
        out.extend_from_slice(&off.to_le_bytes());
    }
    out.extend_from_slice(body);
}

#[cfg(test)]
pub(crate) fn raw_block_for_test(lens: &[u32], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    raw_block(lens, body, &mut out);
    out
}

/// The accumulators of the piece being built.
struct Piece {
    docs: u32,
    ids: Interner,
    text: Vec<TextAcc>,
    values: Vec<ValueAcc>,
    bools: Vec<(Vec<u32>, Vec<u32>)>,
    geos: Vec<Vec<u32>>,
    store: DocsAcc,
}

impl Piece {
    fn new(plan: &BuildPlan) -> Self {
        Self {
            docs: 0,
            ids: Interner::new(),
            text: plan.text.iter().map(|_| TextAcc::default()).collect(),
            values: plan.values.iter().map(|v| ValueAcc::new(v.strings, v.ords)).collect(),
            bools: plan.bools.iter().map(|_| (Vec::new(), Vec::new())).collect(),
            geos: plan.geos.iter().map(|_| Vec::new()).collect(),
            store: DocsAcc::default(),
        }
    }

    fn heap_bytes(&self) -> usize {
        let s = &self.store;
        self.ids.heap_bytes()
            + self.text.iter().map(TextAcc::heap_bytes).sum::<usize>()
            + self.values.iter().map(ValueAcc::heap_bytes).sum::<usize>()
            + self.bools.iter().map(|(t, f)| 4 * (t.capacity() + f.capacity())).sum::<usize>()
            + self.geos.iter().map(|g| 4 * g.capacity()).sum::<usize>()
            + s.block.capacity()
            + s.frames.capacity()
            + s.raw.capacity()
    }
}

// -- the run -----------------------------------------------------------------------------------

/// Builds one run; see `SegmentBuilder::run`.
pub struct RunBuilder {
    shared: Arc<Shared>,
    key: u64,
    keys: Arc<KeyMap>,
    doc: DocValues,
    analyzer: Analyzer,
    piece: Piece,
    pieces: u32,
    docs: u64,
    offered: u64,
    rejected: Vec<Rejected>,
    /// The run's first records, while the stored documents' dictionary does not exist yet.
    sample: Option<Vec<Vec<u8>>>,
    sample_bytes: usize,
    comp: Option<zstd::bulk::Compressor<'static>>,
    doc_terms: Vec<u32>,
    ids_scratch: Vec<u32>,
    token_key: Vec<u8>,
}

impl RunBuilder {
    pub(crate) fn new(shared: Arc<Shared>, key: u64) -> Self {
        let keys = shared.keys.load_full();
        let sample = shared.dictionary().is_none().then(Vec::new);
        Self {
            piece: Piece::new(&shared.plan),
            keys,
            key,
            doc: DocValues::default(),
            analyzer: Analyzer::default(),
            pieces: 0,
            docs: 0,
            offered: 0,
            rejected: Vec::new(),
            sample,
            sample_bytes: 0,
            comp: None,
            doc_terms: Vec::new(),
            ids_scratch: Vec::new(),
            token_key: Vec::new(),
            shared,
        }
    }

    /// The run's key: runs are laid out in key order.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// Documents accepted so far.
    pub fn doc_count(&self) -> u64 {
        self.docs
    }

    /// Documents refused so far, as a JSONL import would have refused them.
    pub fn rejected(&self) -> &[Rejected] {
        &self.rejected
    }

    /// Add one record (encoded against the builder's registry). A document the import would
    /// refuse is recorded in `rejected` and takes no doc id. Err for an unusable record and for a
    /// document id the run already has.
    pub fn add(&mut self, rec: &[u8]) -> io::Result<()> {
        let index = self.offered;
        self.offered += 1;
        let id = record::record_id(rec).map_err(|e| invalid(e.to_string()))?;
        if self.piece.ids.find(id.as_bytes()).is_some() {
            return Err(invalid(format!("document id `{id}` is added twice")));
        }
        // Paths added since this run's snapshot of the registry.
        if self.keys.len() < self.shared.registry_len() {
            self.keys = self.shared.keys.load_full();
        }
        let shared = Arc::clone(&self.shared);
        if let Err(error) = self.doc.read(rec, &shared.schema, &shared.needed, &self.keys) {
            self.rejected.push(Rejected { run: self.key, index, id: id.to_string(), error });
            return Ok(());
        }
        let doc = self.piece.docs;
        self.piece.docs += 1;
        self.docs += 1;
        self.piece.ids.intern(id.as_bytes());
        self.index_text(rec, doc);
        self.index_values(rec, doc);
        self.store_record(rec, doc)?;
        if self.piece.docs % SIZE_CHECK_DOCS == 0 && self.piece.heap_bytes() >= self.shared.opts.piece_bytes {
            self.freeze_piece()?;
        }
        Ok(())
    }

    /// Add records given as `varint(len) record` one after another.
    pub fn add_batch(&mut self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let mut c = record::Cursor::new(buf);
            let n = c.varint().map_err(|e| invalid(e.to_string()))? as usize;
            let at = buf.len() - c.remaining();
            if at + n > buf.len() {
                return Err(invalid("record batch truncated"));
            }
            self.add(&buf[at..at + n])?;
            buf = &buf[at + n..];
        }
        Ok(())
    }

    /// The document's text fields: per field its terms (interned per piece, each token analyzed
    /// once, keyed by the document language, which stemming depends on), their frequencies and the
    /// field's length.
    fn index_text(&mut self, rec: &[u8], doc: u32) {
        let shared = &*self.shared;
        let language = self.doc.language;
        let symbols = &shared.schema.symbols_to_index;
        for (t, p) in shared.plan.text.iter().enumerate() {
            let f = &shared.schema.fields[p.field];
            let acc = &mut self.piece.text[t];
            let (analyzer, key, terms) = (&mut self.analyzer, &mut self.token_key, &mut self.doc_terms);
            terms.clear();
            let mut each = |s: &str| {
                crate::text::tokenize_each(s, symbols, &shared.schema.token_separators, |tok| {
                    key.clear();
                    key.push(language as u8);
                    key.extend_from_slice(tok.as_bytes());
                    let (tid, new) = acc.tokens.intern(key);
                    if new {
                        let term = match analyzer.analyze(tok, symbols, f, language) {
                            None => NONE,
                            Some((term, lowered)) => {
                                if lowered != term {
                                    key.clear();
                                    key.extend_from_slice(lowered.as_bytes());
                                    key.push(RAW_SEP);
                                    key.extend_from_slice(term.as_bytes());
                                    acc.raw.intern(key);
                                }
                                acc.terms.intern(term.as_bytes()).0
                            }
                        };
                        acc.token_term.push(term);
                    }
                    let term = acc.token_term[tid as usize];
                    if term != NONE {
                        terms.push(term);
                    }
                });
            };
            match self.doc.typed[p.slot] {
                Typed::Str(at, len) => each(str_of(rec, (at, len))),
                Typed::Strs(a, b) => {
                    for i in a..b {
                        if let Some(r) = self.doc.strs[i as usize] {
                            each(str_of(rec, r));
                        }
                    }
                }
                _ => {}
            }
            if terms.is_empty() {
                continue;
            }
            acc.lens.push((doc, terms.len() as u32));
            terms.sort_unstable();
            for run in terms.chunk_by(|a, b| a == b) {
                acc.post.push((run[0], doc, (run.len() as u32).min(u32::from(u16::MAX))));
            }
        }
    }

    fn index_values(&mut self, rec: &[u8], doc: u32) {
        let shared = &*self.shared;
        for (v, p) in shared.plan.values.iter().enumerate() {
            let acc = &mut self.piece.values[v];
            let ids = &mut self.ids_scratch;
            ids.clear();
            match self.doc.typed[p.slot] {
                Typed::Str(at, len) => ids.push(acc.str_id(str_of(rec, (at, len)).as_bytes(), doc)),
                Typed::Strs(a, b) => {
                    for i in a..b {
                        let id = match self.doc.strs[i as usize] {
                            Some(r) => acc.str_id(str_of(rec, r).as_bytes(), doc),
                            None => NONE,
                        };
                        ids.push(id);
                    }
                }
                Typed::Int(n) if !p.strings => ids.push(acc.num_id(n, doc)),
                Typed::Float(x) if !p.strings => ids.push(acc.num_id(f64_key(x), doc)),
                Typed::Ints(a, b) if !p.strings => {
                    for i in a..b {
                        let id = match self.doc.ints[i as usize] {
                            Some(n) => acc.num_id(n, doc),
                            None => NONE,
                        };
                        ids.push(id);
                    }
                }
                Typed::Floats(a, b) if !p.strings => {
                    for i in a..b {
                        ids.push(acc.num_id(f64_key(f64::from(self.doc.floats[i as usize])), doc));
                    }
                }
                _ => {
                    acc.absent();
                    continue;
                }
            }
            acc.add(ids);
        }
        for (b, p) in shared.plan.bools.iter().enumerate() {
            let (t, f) = &mut self.piece.bools[b];
            match self.doc.typed[p.slot] {
                Typed::Bool(true) => t.push(doc),
                Typed::Bool(false) => f.push(doc),
                // Match-any: a document with both values satisfies `:=true` and `:=false`.
                Typed::Bools(a, z) => {
                    let vals = &self.doc.bools[a as usize..z as usize];
                    if vals.iter().flatten().any(|b| *b) {
                        t.push(doc);
                    }
                    if vals.iter().flatten().any(|b| !*b) {
                        f.push(doc);
                    }
                }
                _ => {}
            }
        }
        for (g, p) in shared.plan.geos.iter().enumerate() {
            let e6 = crate::index::geo::e6;
            let pts = &mut self.piece.geos[g];
            match self.doc.typed[p.slot] {
                Typed::Geo(lat, lng) if !p.multi => pts.extend_from_slice(&[doc, e6(lat) as u32, e6(lng) as u32]),
                Typed::Geos(a, z) if p.multi => {
                    for &(lat, lng) in &self.doc.geos[a as usize..z as usize] {
                        pts.extend_from_slice(&[doc, e6(lat) as u32, e6(lng) as u32]);
                    }
                }
                _ => {}
            }
        }
    }

    fn store_record(&mut self, rec: &[u8], doc: u32) -> io::Result<()> {
        let s = &mut self.piece.store;
        if s.lens.is_empty() {
            s.firsts.push(doc);
        }
        s.block.extend_from_slice(rec);
        s.lens.push(rec.len() as u32);
        s.size += rec.len() + 4;
        let full = s.size >= BLOCK_TARGET;
        if self.sample.is_some() {
            if self.shared.dictionary().is_some() {
                self.sample = None;
            } else if let Some(sample) = &mut self.sample {
                sample.push(rec.to_vec());
                self.sample_bytes += rec.len();
                if self.sample_bytes >= MAX_SAMPLE_BYTES {
                    let sample = self.sample.take().unwrap_or_default();
                    self.shared.submit_sample(self.key, sample);
                }
            }
        }
        if full {
            self.close_block()?;
        }
        Ok(())
    }

    fn close_block(&mut self) -> io::Result<()> {
        let s = &mut self.piece.store;
        if s.lens.is_empty() {
            return Ok(());
        }
        raw_block(&s.lens, &s.block, &mut s.raw);
        s.raw_ends.push(s.raw.len() as u64);
        s.block.clear();
        s.lens.clear();
        s.size = 0;
        self.compress_ready()
    }

    /// Compress the piece's raw blocks once the dictionary exists.
    fn compress_ready(&mut self) -> io::Result<()> {
        if self.piece.store.raw_ends.is_empty() {
            return Ok(());
        }
        let Some(dict) = self.shared.dictionary() else { return Ok(()) };
        if self.comp.is_none() {
            let level = self.shared.opts.docs_level;
            self.comp = Some(if dict.is_empty() {
                zstd::bulk::Compressor::new(level)?
            } else {
                zstd::bulk::Compressor::with_dictionary(level, &dict)?
            });
        }
        let comp = self.comp.as_mut().unwrap();
        let s = &mut self.piece.store;
        let mut start = 0usize;
        for &end in &s.raw_ends {
            let frame = comp.compress(&s.raw[start..end as usize])?;
            s.frames.extend_from_slice(&frame);
            s.frame_ends.push(s.frames.len() as u64);
            start = end as usize;
        }
        s.raw.clear();
        s.raw_ends.clear();
        Ok(())
    }

    fn freeze_piece(&mut self) -> io::Result<()> {
        self.close_block()?;
        if self.piece.docs == 0 {
            return Ok(());
        }
        let piece = std::mem::replace(&mut self.piece, Piece::new(&self.shared.plan));
        let frozen = freeze(piece, self.key, self.pieces);
        self.pieces += 1;
        self.shared.add_piece(frozen)
    }

    /// Freeze what is left and hand the run to the builder. Returns the documents it refused.
    pub fn end(mut self) -> io::Result<Vec<Rejected>> {
        if let Some(sample) = self.sample.take() {
            self.shared.submit_sample(self.key, sample);
        }
        self.close_block()?;
        self.compress_ready()?;
        self.freeze_piece()?;
        let rejected = std::mem::take(&mut self.rejected);
        self.shared.add_rejected(&rejected);
        Ok(rejected)
    }
}

// -- frozen pieces -----------------------------------------------------------------------------

fn pods<T: bytemuck::Pod + Send + Sync>(v: Vec<T>) -> Pods<T> {
    Pods::from_vec(v)
}

pub(crate) struct FrozenBlocks {
    /// Piece-local first doc of each block.
    pub firsts: Pods<u32>,
    /// Compressed frames of the first `frame_ends.len()` blocks (ends within `frames`).
    pub frames: Bytes,
    pub frame_ends: Pods<u64>,
    /// The remaining blocks, uncompressed (the dictionary did not exist yet).
    pub raw: Bytes,
    pub raw_ends: Pods<u64>,
}

pub(crate) struct FrozenText {
    /// Terms in byte order: bytes and ends (`starts[i]..starts[i + 1]`).
    pub terms: Bytes,
    pub starts: Pods<u32>,
    pub df: Pods<u32>,
    /// Per term its postings in `post[post_at[i]..post_at[i + 1]]`: varint doc gap, varint tf.
    pub post: Bytes,
    pub post_at: Pods<u32>,
    /// "lowered\0term" keys in byte order: bytes and ends.
    pub raw: Bytes,
    pub raw_starts: Pods<u32>,
    /// varint doc gap, varint length, for the documents with tokens.
    pub lens: Bytes,
}

impl FrozenText {
    #[inline]
    pub fn len(&self) -> usize {
        self.df.len()
    }

    #[inline]
    pub fn term(&self, i: usize) -> &[u8] {
        &self.terms[self.starts[i] as usize..self.starts[i + 1] as usize]
    }

    #[inline]
    pub fn raw_len(&self) -> usize {
        self.raw_starts.len().saturating_sub(1)
    }

    #[inline]
    pub fn raw_key(&self, i: usize) -> &[u8] {
        &self.raw[self.raw_starts[i] as usize..self.raw_starts[i + 1] as usize]
    }

    /// Append the term's postings, doc ids offset by `base`.
    pub fn postings_into(&self, i: usize, base: u32, docs: &mut Vec<u32>, tfs: &mut Vec<u32>) {
        let mut c = record::Cursor::new(&self.post[self.post_at[i] as usize..self.post_at[i + 1] as usize]);
        let mut doc = 0u32;
        for k in 0..self.df[i] {
            let gap = c.varint().unwrap_or(0) as u32;
            doc = if k == 0 { gap } else { doc + gap };
            docs.push(base + doc);
            tfs.push(c.varint().unwrap_or(1) as u32);
        }
    }
}

pub(crate) enum FrozenOrds {
    None,
    /// Each document's value index at `width` bits (len = none).
    Single { bits: Bytes, width: u32 },
    /// varint(element count + 1) per document (0 = absent), then the elements at `width` bits
    /// (len = null).
    Multi { counts: Bytes, bits: Bytes, width: u32 },
}

pub(crate) struct FrozenValues {
    /// String values in byte order (empty for numbers).
    pub strs: Bytes,
    pub str_starts: Pods<u32>,
    /// Numeric keys in order (empty for strings).
    pub nums: Pods<i64>,
    pub df: Pods<u32>,
    /// varint doc gaps per value, in `post[post_at[i]..post_at[i + 1]]`.
    pub post: Bytes,
    pub post_at: Pods<u32>,
    pub ords: FrozenOrds,
}

impl FrozenValues {
    pub fn len(&self) -> usize {
        self.df.len()
    }

    pub fn str_at(&self, i: usize) -> &[u8] {
        &self.strs[self.str_starts[i] as usize..self.str_starts[i + 1] as usize]
    }

    pub fn num_at(&self, i: usize) -> i64 {
        self.nums[i]
    }

    pub fn postings_into(&self, i: usize, base: u32, docs: &mut Vec<u32>) {
        let mut c = record::Cursor::new(&self.post[self.post_at[i] as usize..self.post_at[i + 1] as usize]);
        let mut doc = 0u32;
        for k in 0..self.df[i] {
            let gap = c.varint().unwrap_or(0) as u32;
            doc = if k == 0 { gap } else { doc + gap };
            docs.push(base + doc);
        }
    }
}

pub(crate) struct FrozenRun {
    pub key: u64,
    /// Position among the pieces of its run.
    pub piece: u32,
    pub docs: u32,
    /// External ids in byte order, and the piece-local doc of each.
    pub ids: Bytes,
    pub id_starts: Pods<u32>,
    pub id_docs: Pods<u32>,
    pub blocks: FrozenBlocks,
    pub text: Vec<FrozenText>,
    pub values: Vec<FrozenValues>,
    pub bools: Vec<(Pods<u32>, Pods<u32>)>,
    /// Per geo field (doc, lat, lng) triples, lat and lng as i32 bits.
    pub geos: Vec<Pods<u32>>,
    /// Where each column lives in the spill file, once spilled.
    pub spilled: Option<Vec<(u64, u64)>>,
}

/// One column of a frozen piece, for spilling and reloading.
pub(crate) enum Col<'a> {
    B(&'a mut Bytes),
    U32(&'a mut Pods<u32>),
    U64(&'a mut Pods<u64>),
    I64(&'a mut Pods<i64>),
}

impl Col<'_> {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Col::B(b) => b,
            Col::U32(p) => p.bytes(),
            Col::U64(p) => p.bytes(),
            Col::I64(p) => p.bytes(),
        }
    }

    /// Point the column at `b` (bytes it was spilled as, now mapped); empty `b` frees it.
    pub fn set(&mut self, b: Bytes) -> io::Result<()> {
        match self {
            Col::B(x) => **x = b,
            Col::U32(x) => **x = Pods::new(b)?,
            Col::U64(x) => **x = Pods::new(b)?,
            Col::I64(x) => **x = Pods::new(b)?,
        }
        Ok(())
    }
}

impl FrozenRun {
    #[inline]
    pub fn id(&self, i: usize) -> &[u8] {
        &self.ids[self.id_starts[i] as usize..self.id_starts[i + 1] as usize]
    }

    /// Every column, always in the same order.
    pub fn visit(&mut self, f: &mut dyn FnMut(Col<'_>) -> io::Result<()>) -> io::Result<()> {
        f(Col::B(&mut self.ids))?;
        f(Col::U32(&mut self.id_starts))?;
        f(Col::U32(&mut self.id_docs))?;
        let b = &mut self.blocks;
        f(Col::U32(&mut b.firsts))?;
        f(Col::B(&mut b.frames))?;
        f(Col::U64(&mut b.frame_ends))?;
        f(Col::B(&mut b.raw))?;
        f(Col::U64(&mut b.raw_ends))?;
        for t in &mut self.text {
            f(Col::B(&mut t.terms))?;
            f(Col::U32(&mut t.starts))?;
            f(Col::U32(&mut t.df))?;
            f(Col::B(&mut t.post))?;
            f(Col::U32(&mut t.post_at))?;
            f(Col::B(&mut t.raw))?;
            f(Col::U32(&mut t.raw_starts))?;
            f(Col::B(&mut t.lens))?;
        }
        for v in &mut self.values {
            f(Col::B(&mut v.strs))?;
            f(Col::U32(&mut v.str_starts))?;
            f(Col::I64(&mut v.nums))?;
            f(Col::U32(&mut v.df))?;
            f(Col::B(&mut v.post))?;
            f(Col::U32(&mut v.post_at))?;
            match &mut v.ords {
                FrozenOrds::None => {}
                FrozenOrds::Single { bits, .. } => f(Col::B(bits))?,
                FrozenOrds::Multi { counts, bits, .. } => {
                    f(Col::B(counts))?;
                    f(Col::B(bits))?;
                }
            }
        }
        for (t, fl) in &mut self.bools {
            f(Col::U32(t))?;
            f(Col::U32(fl))?;
        }
        for g in &mut self.geos {
            f(Col::U32(g))?;
        }
        Ok(())
    }

    /// Bytes of all columns.
    pub fn bytes(&mut self) -> usize {
        let mut n = 0;
        let _ = self.visit(&mut |c| {
            n += c.bytes().len();
            Ok(())
        });
        n
    }
}

fn freeze(p: Piece, key: u64, piece: u32) -> FrozenRun {
    let docs = p.docs;
    let id_order = p.ids.sorted_ids();
    let (ids, id_starts) = p.ids.reordered(&id_order);
    let s = p.store;
    FrozenRun {
        key,
        piece,
        docs,
        ids: Bytes::from_vec(ids),
        id_starts: pods(id_starts),
        id_docs: pods(id_order),
        blocks: FrozenBlocks {
            firsts: pods(s.firsts),
            frames: Bytes::from_vec(s.frames),
            frame_ends: pods(s.frame_ends),
            raw: Bytes::from_vec(s.raw),
            raw_ends: pods(s.raw_ends),
        },
        text: p.text.into_iter().map(freeze_text).collect(),
        values: p.values.into_iter().map(|v| freeze_values(v, docs)).collect(),
        bools: p.bools.into_iter().map(|(t, f)| (pods(t), pods(f))).collect(),
        geos: p.geos.into_iter().map(pods).collect(),
        spilled: None,
    }
}

/// Counting sort of (key, doc, ...) postings by the rank of their key, keeping doc order, then
/// delta-coded per key; returns (df, stream, offsets).
fn group_postings<P: Copy + Doc>(n: usize, rank: &[u32], post: &[P], key: impl Fn(&P) -> u32, mut emit: impl FnMut(&mut Vec<u8>, u32, &P)) -> (Vec<u32>, Vec<u8>, Vec<u32>) {
    let mut df = vec![0u32; n];
    for p in post {
        df[rank[key(p) as usize] as usize] += 1;
    }
    let mut at = vec![0usize; n + 1];
    for i in 0..n {
        at[i + 1] = at[i] + df[i] as usize;
    }
    let mut order = vec![0u32; post.len()];
    let mut fill = at.clone();
    for (j, p) in post.iter().enumerate() {
        let r = rank[key(p) as usize] as usize;
        order[fill[r]] = j as u32;
        fill[r] += 1;
    }
    drop(fill);
    let mut stream = Vec::with_capacity(post.len() * 2);
    let mut offsets = Vec::with_capacity(n + 1);
    offsets.push(0u32);
    for i in 0..n {
        let mut prev = 0u32;
        for (k, &j) in order[at[i]..at[i + 1]].iter().enumerate() {
            let p = &post[j as usize];
            emit(&mut stream, if k == 0 { NONE } else { prev }, p);
            prev = p.doc();
        }
        offsets.push(u32::try_from(stream.len()).expect("a piece's postings fit 4 GB"));
    }
    (df, stream, offsets)
}

/// The document of a posting tuple.
trait Doc {
    fn doc(&self) -> u32;
}

impl Doc for (u32, u32, u32) {
    fn doc(&self) -> u32 {
        self.1
    }
}

impl Doc for (u32, u32) {
    fn doc(&self) -> u32 {
        self.1
    }
}

fn gap(prev: u32, doc: u32) -> u64 {
    u64::from(if prev == NONE { doc } else { doc - prev })
}

fn freeze_text(acc: TextAcc) -> FrozenText {
    let order = acc.terms.sorted_ids();
    let mut rank = vec![0u32; order.len()];
    for (r, &id) in order.iter().enumerate() {
        rank[id as usize] = r as u32;
    }
    let (terms, starts) = acc.terms.reordered(&order);
    let n = order.len();
    let (df, post, post_at) = group_postings(n, &rank, &acc.post, |p| p.0, |out, prev, p| {
        put_varint(out, gap(prev, p.doc()));
        put_varint(out, u64::from(p.2));
    });
    let (raw, raw_starts) = acc.raw.reordered(&acc.raw.sorted_ids());
    let mut lens = Vec::with_capacity(acc.lens.len() * 2);
    let mut prev = NONE;
    for &(d, l) in &acc.lens {
        put_varint(&mut lens, gap(prev, d));
        put_varint(&mut lens, u64::from(l));
        prev = d;
    }
    FrozenText {
        terms: Bytes::from_vec(terms),
        starts: pods(starts),
        df: pods(df),
        post: Bytes::from_vec(post),
        post_at: pods(post_at),
        raw: Bytes::from_vec(raw),
        raw_starts: pods(raw_starts),
        lens: Bytes::from_vec(lens),
    }
}

fn freeze_values(acc: ValueAcc, docs: u32) -> FrozenValues {
    let n = acc.len();
    let order: Vec<u32> = match &acc.dict {
        Dict::Str(t) => t.sorted_ids(),
        Dict::Num { keys, .. } => {
            let mut o: Vec<u32> = (0..keys.len() as u32).collect();
            o.sort_unstable_by_key(|&i| keys[i as usize]);
            o
        }
    };
    let mut rank = vec![0u32; n];
    for (r, &id) in order.iter().enumerate() {
        rank[id as usize] = r as u32;
    }
    let (strs, str_starts, nums) = match &acc.dict {
        Dict::Str(t) => {
            let (bytes, starts) = t.reordered(&order);
            (bytes, starts, Vec::new())
        }
        Dict::Num { keys, .. } => (Vec::new(), vec![0u32], order.iter().map(|&i| keys[i as usize]).collect()),
    };
    let (df, post, post_at) = group_postings(n, &rank, &acc.post, |p| p.0, |out, prev, p| put_varint(out, gap(prev, p.doc())));

    let width = packed::bits_for(n as u64);
    let remap = |id: u32| if id == NONE { n as u64 } else { u64::from(rank[id as usize]) };
    let ords = match acc.mode {
        Ords::None => FrozenOrds::None,
        Ords::Single => {
            debug_assert_eq!(acc.single.len(), docs as usize);
            let mut w = BitWriter::with_bits(u64::from(docs) * u64::from(width));
            for &id in &acc.single {
                w.push(remap(id), width);
            }
            FrozenOrds::Single { bits: Bytes::from_vec(w.into_bytes()), width }
        }
        Ords::Multi => {
            debug_assert_eq!(acc.counts.len(), docs as usize);
            let mut counts = Vec::with_capacity(acc.counts.len());
            for &c in &acc.counts {
                put_varint(&mut counts, u64::from(c));
            }
            let mut w = BitWriter::with_bits(acc.elems.len() as u64 * u64::from(width));
            for &id in &acc.elems {
                w.push(remap(id), width);
            }
            FrozenOrds::Multi { counts: Bytes::from_vec(counts), bits: Bytes::from_vec(w.into_bytes()), width }
        }
    };
    FrozenValues {
        strs: Bytes::from_vec(strs),
        str_starts: pods(str_starts),
        nums: pods(nums),
        df: pods(df),
        post: Bytes::from_vec(post),
        post_at: pods(post_at),
        ords,
    }
}
