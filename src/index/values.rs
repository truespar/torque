//! Value indexes (TQBF spec §7): what filters, facets, sorting and grouping read. Per field a
//! dictionary of its distinct values, numbered in value order, the documents of each value and,
//! for facet and sort fields, each document's value ordinals. Strings and numbers share the
//! layout; only the dictionary differs. Every section is read in place.
//!
//! ```text
//! VALUES      string field: FST value -> ordinal
//! NUM_VALUES  numeric field: u8 float, 7 x u8 0, monotonic sequence of the sorted keys (§7.4);
//!             key = the value as i64 (floats: order-preserving bits, -0.0 as 0.0), sign bit flipped
//! VALUE_DOCS  u64 n_values, u32 entry_width, u32 0; per ordinal an entry bit-packed at entry_width,
//!             doc << 1 | 1 for a value in one document, else (offset / 4) << 1 of its frozen roaring
//!             (relative to the bitmaps); padded to 8; the frozen roaring bitmaps
//! DOC_ORDS    u64 n_values, u32 doc_count, u8 width, u8 multi, u16 0; ordinal n_values = none;
//!             u64 present_len, frozen roaring of the documents that have the field (an empty
//!             array counts), padded to 8
//!             single: each document's ordinal bit-packed at width
//!             multi:  monotonic element offsets (doc_count + 1), padded to 8; every document's
//!                     elements in document order bit-packed at width (ordinal n_values = null)
//! BOOL        u64 true_len, frozen roaring of the documents with true, padded to 8, frozen roaring
//!             of the documents with false (a bool[] document can be in both)
//! ```

use crate::bitmap::Bitmap;
use crate::index::merge::{sort_docs, DocMap, GONE};
use crate::index::Segment;
use crate::schema::{DocId, Document, FieldDef, FieldId, FieldType, FieldValue, Schema};
use crate::tqbf::bytes::Bytes;
use crate::tqbf::container::{kind, SegmentFile, SegmentWriter};
use crate::tqbf::packed::{self, BitWriter, Monotonic};
use fst::{Automaton, IntoStreamer, Streamer};
use rustc_hash::FxHashMap;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::hash::Hash;
use std::io;
use std::ops::{Bound, Range};

const NONE: u32 = u32::MAX;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("value index: {}", msg.into()))
}

fn pad8(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(8), 0);
}

// -- keys -------------------------------------------------------------------------------------------

/// Order-preserving i64 key of a float: its bits, negative values flipped. -0.0 and 0.0 are equal
/// values and share the key of 0.0.
pub fn f64_key(f: f64) -> i64 {
    let f = if f == 0.0 { 0.0 } else { f };
    let bits = f.to_bits() as i64;
    if bits >= 0 { bits } else { bits ^ i64::MAX }
}

/// Inverse of `f64_key`.
pub fn key_f64(k: i64) -> f64 {
    let bits = if k >= 0 { k } else { k ^ i64::MAX };
    f64::from_bits(bits as u64)
}

#[inline]
fn key_to_u64(k: i64) -> u64 {
    (k as u64) ^ (1 << 63)
}

#[inline]
fn u64_to_key(u: u64) -> i64 {
    (u ^ (1 << 63)) as i64
}

// -- union of postings -------------------------------------------------------------------------------

/// Documents gathered from many postings into a dense bitset sized to the segment.
pub struct DocSet {
    words: Vec<u64>,
    doc_count: u32,
}

impl DocSet {
    pub fn new(doc_count: u32) -> Self {
        Self { words: vec![0; (doc_count as usize).div_ceil(1 << 16) * 1024], doc_count }
    }

    #[inline]
    pub fn add(&mut self, doc: DocId) {
        self.words[(doc >> 6) as usize] |= 1 << (doc & 63);
    }

    pub fn add_bitmap(&mut self, bm: &Bitmap) {
        bm.or_into_words(&mut self.words);
    }

    pub fn finish(self) -> Bitmap {
        debug_assert!(self.words.len() * 64 >= self.doc_count as usize);
        Bitmap::from_words(&self.words)
    }
}

// -- VALUE_DOCS --------------------------------------------------------------------------------------

enum Posting {
    One(DocId),
    Many(Bitmap),
}

#[derive(Clone)]
pub(crate) struct ValueDocs {
    bytes: Bytes,
    entries: Bytes,
    bitmaps_at: usize,
    n: u64,
    width: u32,
    doc_count: u32,
}

impl ValueDocs {
    /// Postings in ordinal order, each sorted, unique and non-empty.
    fn write<'a>(postings: impl ExactSizeIterator<Item = &'a [DocId]>) -> Vec<u8> {
        let postings: Vec<&[DocId]> = postings.collect();
        Self::write_with(postings.len(), |ord, docs| {
            docs.clear();
            docs.extend_from_slice(postings[ord]);
        })
    }

    /// `n` postings in ordinal order; `fill(ord, docs)` replaces `docs` with the documents of
    /// ordinal `ord` (sorted, unique, non-empty).
    pub(crate) fn write_with(n: usize, mut fill: impl FnMut(usize, &mut Vec<DocId>)) -> Vec<u8> {
        let mut bitmaps = Vec::new();
        let mut entries = Vec::with_capacity(n);
        let mut docs = Vec::new();
        for ord in 0..n {
            fill(ord, &mut docs);
            if let [doc] = docs[..] {
                entries.push(u64::from(doc) << 1 | 1);
                continue;
            }
            bitmaps.resize(bitmaps.len().next_multiple_of(4), 0);
            entries.push(((bitmaps.len() / 4) as u64) << 1);
            Bitmap::of(&docs).write_frozen(&mut bitmaps);
        }
        let width = packed::bits_for(entries.iter().copied().max().unwrap_or(0));
        let mut out = Vec::with_capacity(16 + packed::packed_len(n as u64, width) + bitmaps.len() + 8);
        out.extend_from_slice(&(n as u64).to_le_bytes());
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let mut bits = BitWriter::with_bits(n as u64 * u64::from(width));
        for e in entries {
            bits.push(e, width);
        }
        out.extend_from_slice(&bits.into_bytes());
        pad8(&mut out);
        out.extend_from_slice(&bitmaps);
        out
    }

    fn parse(bytes: Bytes, doc_count: u32) -> io::Result<Self> {
        if bytes.len() < 16 {
            return Err(invalid("VALUE_DOCS header truncated"));
        }
        let (n, width) = (bytes.u64_at(0), bytes.u32_at(8));
        if width > 64 || n > 8 * bytes.len() as u64 {
            return Err(invalid("VALUE_DOCS header invalid"));
        }
        let elen = packed::packed_len(n, width);
        let bitmaps_at = (16 + elen).next_multiple_of(8);
        if bitmaps_at > bytes.len() {
            return Err(invalid("VALUE_DOCS entries past end"));
        }
        Ok(Self { entries: bytes.slice(16, elen), bitmaps_at, n, width, doc_count, bytes })
    }

    fn posting(&self, ord: u64) -> Option<Posting> {
        if ord >= self.n {
            return None;
        }
        let e = packed::read(&self.entries, ord * u64::from(self.width), self.width);
        if e & 1 == 1 {
            if e >> 1 < u64::from(self.doc_count) {
                return Some(Posting::One((e >> 1) as DocId));
            }
            tracing::error!(ord, "value index: posting doc id out of range");
            return None;
        }
        let at = self.bitmaps_at + 4 * (e >> 1) as usize;
        if at >= self.bytes.len() {
            tracing::error!(ord, "value index: posting offset out of range");
            return None;
        }
        match Bitmap::from_frozen(self.bytes.slice(at, self.bytes.len() - at)) {
            Ok(bm) if bm.maximum().is_some_and(|m| m < self.doc_count) => Some(Posting::Many(bm)),
            Ok(_) => {
                tracing::error!(ord, "value index: posting empty or past the segment");
                None
            }
            Err(e) => {
                tracing::error!(ord, "value index: {e}");
                None
            }
        }
    }
}

// -- DOC_ORDS ----------------------------------------------------------------------------------------

/// Each document's value ordinals.
#[derive(Clone)]
pub struct DocOrds {
    n_values: u64,
    doc_count: u32,
    width: u32,
    /// Documents that have the field.
    present: Bitmap,
    /// Multi-valued: per document its element range.
    offsets: Option<Monotonic>,
    ords: Bytes,
}

impl DocOrds {
    fn header(n_values: u64, doc_count: u32, width: u32, multi: bool, present: &[DocId]) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        out.extend_from_slice(&n_values.to_le_bytes());
        out.extend_from_slice(&doc_count.to_le_bytes());
        out.extend_from_slice(&[width as u8, u8::from(multi), 0, 0]);
        let mut bm = Vec::new();
        Bitmap::of(present).write_frozen(&mut bm);
        out.extend_from_slice(&(bm.len() as u64).to_le_bytes());
        out.extend_from_slice(&bm);
        pad8(&mut out);
        out
    }

    pub(crate) fn write_single(ords: &[u64], n_values: u64) -> Vec<u8> {
        let doc_count = ords.len() as u32;
        let width = packed::bits_for(n_values);
        let present: Vec<DocId> = (0..doc_count).filter(|&d| ords[d as usize] < n_values).collect();
        let mut out = Self::header(n_values, doc_count, width, false, &present);
        let mut bits = BitWriter::with_bits(u64::from(doc_count) * u64::from(width));
        for &o in ords {
            bits.push(o, width);
        }
        out.extend_from_slice(&bits.into_bytes());
        out
    }

    pub(crate) fn write_multi(present: &[DocId], offsets: &[u64], elems: impl ExactSizeIterator<Item = u64>, n_values: u64) -> Vec<u8> {
        let doc_count = (offsets.len() - 1) as u32;
        let width = packed::bits_for(n_values);
        let mut out = Self::header(n_values, doc_count, width, true, present);
        packed::write_monotonic(offsets, &mut out);
        pad8(&mut out);
        let mut bits = BitWriter::with_bits(elems.len() as u64 * u64::from(width));
        for e in elems {
            bits.push(e, width);
        }
        out.extend_from_slice(&bits.into_bytes());
        out
    }

    fn parse(bytes: Bytes, n_values: u64, doc_count: u32) -> io::Result<Self> {
        if bytes.len() < 16 || bytes.u64_at(0) != n_values || bytes.u32_at(8) != doc_count {
            return Err(invalid("DOC_ORDS does not match its dictionary or segment"));
        }
        let (width, multi) = (u32::from(bytes[12]), bytes[13] != 0);
        if width != packed::bits_for(n_values) || bytes.len() < 24 {
            return Err(invalid("DOC_ORDS header"));
        }
        let plen = bytes.u64_at(16) as usize;
        if plen > bytes.len() - 24 {
            return Err(invalid("DOC_ORDS presence bitmap past end"));
        }
        let present = Bitmap::from_frozen(bytes.slice(24, plen))?;
        if present.maximum().is_some_and(|m| m >= doc_count) {
            return Err(invalid("DOC_ORDS presence past the segment"));
        }
        let at = (24 + plen).next_multiple_of(8);
        if at > bytes.len() {
            return Err(invalid("DOC_ORDS ordinals past end"));
        }
        if !multi {
            let len = packed::packed_len(u64::from(doc_count), width);
            if at + len > bytes.len() {
                return Err(invalid("DOC_ORDS ordinals past end"));
            }
            return Ok(Self { n_values, doc_count, width, present, offsets: None, ords: bytes.slice(at, len) });
        }
        let (offsets, olen) = Monotonic::parse(bytes.slice(at, bytes.len() - at))?;
        if offsets.len() != doc_count as usize + 1 {
            return Err(invalid("DOC_ORDS offsets count"));
        }
        let at = (at + olen).next_multiple_of(8);
        let n_elems = offsets.get(doc_count as usize);
        let len = packed::packed_len(n_elems, width);
        if at + len > bytes.len() {
            return Err(invalid("DOC_ORDS elements past end"));
        }
        Ok(Self { n_values, doc_count, width, present, offsets: Some(offsets), ords: bytes.slice(at, len) })
    }

    #[inline]
    pub fn is_multi(&self) -> bool {
        self.offsets.is_some()
    }

    /// A single-valued field's ordinal for `doc`; None when the document has no value.
    #[inline]
    pub fn ord(&self, doc: DocId) -> Option<u64> {
        if doc >= self.doc_count || self.offsets.is_some() {
            return None;
        }
        let o = packed::read(&self.ords, u64::from(doc) * u64::from(self.width), self.width);
        (o < self.n_values).then_some(o)
    }

    /// Whether the document has the field (for a multi-valued field an empty array counts).
    #[inline]
    pub fn present(&self, doc: DocId) -> bool {
        self.present.contains(doc)
    }

    /// The documents that have the field.
    #[inline]
    pub fn present_docs(&self) -> &Bitmap {
        &self.present
    }

    /// The document's elements in document order; None is a null element. A single-valued field
    /// yields its value, or nothing.
    pub fn elems(&self, doc: DocId) -> Elems<'_> {
        let (pos, end) = match &self.offsets {
            _ if doc >= self.doc_count => (0, 0),
            Some(off) => {
                let (a, b) = (off.get(doc as usize), off.get(doc as usize + 1));
                if a <= b { (a, b) } else { (0, 0) }
            }
            None if self.ord(doc).is_some() => (u64::from(doc), u64::from(doc) + 1),
            None => (0, 0),
        };
        Elems { o: self, pos, end }
    }
}

pub struct Elems<'a> {
    o: &'a DocOrds,
    pos: u64,
    end: u64,
}

impl Iterator for Elems<'_> {
    type Item = Option<u64>;

    #[inline]
    fn next(&mut self) -> Option<Option<u64>> {
        if self.pos >= self.end {
            return None;
        }
        let v = packed::read(&self.o.ords, self.pos * u64::from(self.o.width), self.o.width);
        self.pos += 1;
        Some((v < self.o.n_values).then_some(v))
    }
}

// -- dictionary ----------------------------------------------------------------------------------------

#[derive(Clone)]
enum Dict {
    Str(fst::Map<Bytes>),
    Num { keys: Monotonic, float: bool },
}

/// The key of the FST entry whose output is `ord`. Outputs are ordinals in key order, and the FST
/// builder puts on each transition the smallest output below it, so the walk takes at each node
/// the last transition not past `ord` (Lucene's `Util.getByOutput`).
fn fst_key_of(fst: &fst::raw::Fst<Bytes>, ord: u64) -> Option<Vec<u8>> {
    let mut node = fst.root();
    let mut out = 0u64;
    let mut key = Vec::new();
    loop {
        if node.is_final() && out + node.final_output().value() == ord {
            return Some(key);
        }
        let mut next = None;
        for t in node.transitions() {
            if out + t.out.value() <= ord {
                next = Some(t);
            } else {
                break;
            }
        }
        let t = next?;
        out += t.out.value();
        key.push(t.inp);
        node = fst.node(t.addr);
    }
}

/// Values that start with a lower-cased prefix, compared after ASCII lower-casing of the value.
struct AsciiCiPrefix<'a>(&'a [u8]);

impl Automaton for AsciiCiPrefix<'_> {
    type State = Option<usize>;

    fn start(&self) -> Option<usize> {
        Some(0)
    }

    fn is_match(&self, s: &Option<usize>) -> bool {
        *s == Some(self.0.len())
    }

    fn can_match(&self, s: &Option<usize>) -> bool {
        s.is_some()
    }

    fn will_always_match(&self, s: &Option<usize>) -> bool {
        *s == Some(self.0.len())
    }

    fn accept(&self, s: &Option<usize>, b: u8) -> Option<usize> {
        match *s {
            Some(i) if i == self.0.len() => Some(i),
            Some(i) if b.to_ascii_lowercase() == self.0[i] => Some(i + 1),
            _ => None,
        }
    }
}

// -- value index -------------------------------------------------------------------------------------

/// A field's value index.
#[derive(Clone)]
pub struct ValueIndex {
    dict: Dict,
    dict_bytes: Bytes,
    docs_bytes: Bytes,
    docs: ValueDocs,
    ords_bytes: Option<Bytes>,
    ords: Option<DocOrds>,
}

impl ValueIndex {
    /// The field's index in `file`, or None when the field has no VALUE_DOCS section.
    pub fn open(file: &SegmentFile, field: FieldId) -> io::Result<Option<Self>> {
        let Some(docs) = file.section(kind::VALUE_DOCS, field) else { return Ok(None) };
        let (dict_kind, dict) = match (file.section(kind::VALUES, field), file.section(kind::NUM_VALUES, field)) {
            (Some(d), None) => (kind::VALUES, d),
            (None, Some(d)) => (kind::NUM_VALUES, d),
            _ => return Err(invalid(format!("field {field} needs exactly one of VALUES and NUM_VALUES"))),
        };
        Self::from_sections(dict_kind, dict, docs, file.section(kind::DOC_ORDS, field), file.doc_count).map(Some)
    }

    fn from_sections(dict_kind: u16, dict_bytes: Bytes, docs_bytes: Bytes, ords_bytes: Option<Bytes>, doc_count: u32) -> io::Result<Self> {
        let dict = if dict_kind == kind::VALUES {
            Dict::Str(fst::Map::new(dict_bytes.clone()).map_err(|e| invalid(e.to_string()))?)
        } else {
            if dict_bytes.len() < 8 {
                return Err(invalid("NUM_VALUES truncated"));
            }
            let (keys, _) = Monotonic::parse(dict_bytes.slice(8, dict_bytes.len() - 8))?;
            Dict::Num { keys, float: dict_bytes[0] != 0 }
        };
        let n = match &dict {
            Dict::Str(m) => m.len() as u64,
            Dict::Num { keys, .. } => keys.len() as u64,
        };
        let docs = ValueDocs::parse(docs_bytes.clone(), doc_count)?;
        if docs.n != n {
            return Err(invalid("VALUE_DOCS does not match its dictionary"));
        }
        let ords = ords_bytes.clone().map(|b| DocOrds::parse(b, n, doc_count)).transpose()?;
        Ok(Self { dict, dict_bytes, docs_bytes, docs, ords_bytes, ords })
    }

    /// Add this field's sections to a segment being written.
    pub fn add_sections(&self, field: FieldId, w: &mut SegmentWriter) {
        let dict_kind = if matches!(self.dict, Dict::Str(_)) { kind::VALUES } else { kind::NUM_VALUES };
        w.add_bytes(dict_kind, field, self.dict_bytes.clone());
        w.add_bytes(kind::VALUE_DOCS, field, self.docs_bytes.clone());
        if let Some(o) = &self.ords_bytes {
            w.add_bytes(kind::DOC_ORDS, field, o.clone());
        }
    }

    /// Bytes of all sections (heap before the segment is persisted, mapped after).
    pub fn section_bytes(&self) -> usize {
        self.dict_bytes.len() + self.docs_bytes.len() + self.ords_bytes.as_ref().map_or(0, |b| b.len())
    }

    /// Number of distinct values.
    #[inline]
    pub fn len(&self) -> u64 {
        self.docs.n
    }

    /// Whether the values are strings (else numbers).
    #[inline]
    pub fn is_str(&self) -> bool {
        matches!(self.dict, Dict::Str(_))
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.docs.n == 0
    }

    #[inline]
    pub fn doc_count(&self) -> u32 {
        self.docs.doc_count
    }

    // -- strings --

    pub fn str_ord(&self, value: &str) -> Option<u64> {
        match &self.dict {
            Dict::Str(m) => m.get(value),
            Dict::Num { .. } => None,
        }
    }

    pub fn str_at(&self, ord: u64) -> Option<String> {
        match &self.dict {
            Dict::Str(m) => fst_key_of(m.as_fst(), ord).and_then(|k| String::from_utf8(k).ok()),
            Dict::Num { .. } => None,
        }
    }

    /// Visit every string value in byte (ordinal) order until `f` returns false.
    pub fn for_each_str(&self, mut f: impl FnMut(&str, u64) -> bool) {
        if let Dict::Str(m) = &self.dict {
            let mut stream = m.stream();
            while let Some((k, o)) = stream.next() {
                if let Ok(s) = std::str::from_utf8(k) {
                    if !f(s, o) {
                        break;
                    }
                }
            }
        }
    }

    /// Ordinals of the values starting with `prefix_lower` once ASCII lower-cased, ascending.
    pub fn str_ords_with_prefix(&self, prefix_lower: &str) -> Vec<u64> {
        let Dict::Str(m) = &self.dict else { return Vec::new() };
        let mut stream = m.search(AsciiCiPrefix(prefix_lower.as_bytes())).into_stream();
        let mut out = Vec::new();
        while let Some((_, o)) = stream.next() {
            out.push(o);
        }
        out
    }

    // -- numbers --

    #[inline]
    pub fn is_float(&self) -> bool {
        matches!(self.dict, Dict::Num { float: true, .. })
    }

    /// The key of ordinal `ord` (a numeric field).
    #[inline]
    pub fn key_at(&self, ord: u64) -> Option<i64> {
        match &self.dict {
            Dict::Num { keys, .. } if ord < keys.len() as u64 => Some(u64_to_key(keys.get(ord as usize))),
            _ => None,
        }
    }

    pub fn key_ord(&self, key: i64) -> Option<u64> {
        let r = self.key_ords(Bound::Included(key), Bound::Included(key));
        (!r.is_empty()).then_some(r.start)
    }

    /// Ordinals of the keys inside the bounds.
    pub fn key_ords(&self, lo: Bound<i64>, hi: Bound<i64>) -> Range<u64> {
        let Dict::Num { keys, .. } = &self.dict else { return 0..0 };
        let lower = |k: i64| keys.lower_bound(key_to_u64(k)) as u64;
        let upper = |k: i64| if k == i64::MAX { keys.len() as u64 } else { lower(k + 1) };
        let start = match lo {
            Bound::Included(k) => lower(k),
            Bound::Excluded(k) => upper(k),
            Bound::Unbounded => 0,
        };
        let end = match hi {
            Bound::Included(k) => upper(k),
            Bound::Excluded(k) => lower(k),
            Bound::Unbounded => keys.len() as u64,
        };
        start..end.max(start)
    }

    // -- postings --

    /// Documents with value `ord`.
    pub fn docs(&self, ord: u64) -> Bitmap {
        match self.docs.posting(ord) {
            Some(Posting::One(d)) => Bitmap::of(&[d]),
            Some(Posting::Many(bm)) => bm,
            None => Bitmap::new(),
        }
    }

    /// Number of documents with value `ord`.
    pub fn df(&self, ord: u64) -> u64 {
        match self.docs.posting(ord) {
            Some(Posting::One(_)) => 1,
            Some(Posting::Many(bm)) => bm.cardinality(),
            None => 0,
        }
    }

    /// Documents with value `ord` among `candidates`.
    pub fn count_in(&self, ord: u64, candidates: &Bitmap) -> u64 {
        match self.docs.posting(ord) {
            Some(Posting::One(d)) => u64::from(candidates.contains(d)),
            Some(Posting::Many(bm)) => bm.and_cardinality(candidates),
            None => 0,
        }
    }

    /// Documents with any value whose ordinal is in `ords`.
    pub fn union(&self, ords: Range<u64>) -> Bitmap {
        match ords.end.saturating_sub(ords.start) {
            0 => Bitmap::new(),
            1 => self.docs(ords.start),
            _ => {
                let mut set = DocSet::new(self.docs.doc_count);
                for o in ords {
                    self.add_to(o, &mut set);
                }
                set.finish()
            }
        }
    }

    /// Add the documents of `ord` to a union being built.
    pub fn add_to(&self, ord: u64, set: &mut DocSet) {
        match self.docs.posting(ord) {
            Some(Posting::One(d)) => set.add(d),
            Some(Posting::Many(bm)) => set.add_bitmap(&bm),
            None => {}
        }
    }

    pub fn eq_str(&self, value: &str) -> Option<Bitmap> {
        self.str_ord(value).map(|o| self.docs(o))
    }

    pub fn eq_key(&self, key: i64) -> Option<Bitmap> {
        self.key_ord(key).map(|o| self.docs(o))
    }

    // -- per document --

    #[inline]
    pub fn doc_ords(&self) -> Option<&DocOrds> {
        self.ords.as_ref()
    }

    /// A single-valued field's ordinal for `doc`.
    #[inline]
    pub fn doc_ord(&self, doc: DocId) -> Option<u64> {
        self.ords.as_ref()?.ord(doc)
    }

    /// A single-valued numeric field's key for `doc`.
    #[inline]
    pub fn doc_key(&self, doc: DocId) -> Option<i64> {
        self.key_at(self.doc_ord(doc)?)
    }

    /// A single-valued numeric field's value for `doc` as f64.
    pub fn doc_f64(&self, doc: DocId) -> Option<f64> {
        let k = self.doc_key(doc)?;
        Some(if self.is_float() { key_f64(k) } else { k as f64 })
    }

    /// A single-valued numeric field's value for `doc` as i64 (floats truncated).
    pub fn doc_i64(&self, doc: DocId) -> Option<i64> {
        let k = self.doc_key(doc)?;
        Some(if self.is_float() { key_f64(k) as i64 } else { k })
    }

    // -- building --

    pub fn build_strings(c: ValueCollector<String>) -> io::Result<Self> {
        let doc_count = c.doc_count;
        let (keys, docs, ords) = c.finish();
        let dict = Bytes::from_vec(strings_dict(keys.iter().map(String::as_bytes))?);
        Self::from_sections(kind::VALUES, dict, Bytes::from_vec(docs), ords.map(Bytes::from_vec), doc_count)
    }

    pub fn build_numbers(c: ValueCollector<i64>, float: bool) -> io::Result<Self> {
        let doc_count = c.doc_count;
        let (keys, docs, ords) = c.finish();
        let dict = numbers_dict(&keys, float);
        Self::from_sections(kind::NUM_VALUES, Bytes::from_vec(dict), Bytes::from_vec(docs), ords.map(Bytes::from_vec), doc_count)
    }
}

/// VALUES section: FST of the string values (sorted, unique) to their ordinals.
pub(crate) fn strings_dict<'a>(keys: impl ExactSizeIterator<Item = &'a [u8]>) -> io::Result<Vec<u8>> {
    let mut b = fst::MapBuilder::memory_sized(keys.len());
    for (o, k) in keys.enumerate() {
        b.insert(k, o as u64).map_err(|e| invalid(e.to_string()))?;
    }
    b.into_inner().map_err(|e| invalid(e.to_string()))
}

/// NUM_VALUES section of the sorted, unique keys.
pub(crate) fn numbers_dict(keys: &[i64], float: bool) -> Vec<u8> {
    let mut dict = vec![u8::from(float), 0, 0, 0, 0, 0, 0, 0];
    packed::write_monotonic(&keys.iter().map(|&k| key_to_u64(k)).collect::<Vec<_>>(), &mut dict);
    dict
}

/// Which per-document ordinals a field keeps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ords {
    None,
    Single,
    Multi,
}

/// A field's values gathered document by document, in increasing doc id order.
pub struct ValueCollector<K> {
    doc_count: u32,
    mode: Ords,
    ids: FxHashMap<K, u32>,
    keys: Vec<K>,
    postings: Vec<Vec<DocId>>,
    next_doc: DocId,
    single: Vec<u32>,
    offsets: Vec<u64>,
    elems: Vec<u32>,
    present: Vec<DocId>,
}

impl<K: Hash + Eq + Ord + Clone> ValueCollector<K> {
    pub fn new(doc_count: u32, mode: Ords) -> Self {
        Self {
            doc_count,
            mode,
            ids: FxHashMap::default(),
            keys: Vec::new(),
            postings: Vec::new(),
            next_doc: 0,
            single: Vec::new(),
            offsets: Vec::new(),
            elems: Vec::new(),
            present: Vec::new(),
        }
    }

    fn id(&mut self, k: K, doc: DocId) -> u32 {
        let id = match self.ids.get(&k) {
            Some(&id) => id,
            None => {
                let id = self.keys.len() as u32;
                self.ids.insert(k.clone(), id);
                self.keys.push(k);
                self.postings.push(Vec::new());
                id
            }
        };
        let p = &mut self.postings[id as usize];
        if p.last() != Some(&doc) {
            p.push(doc);
        }
        id
    }

    fn skip_to(&mut self, doc: DocId) {
        while self.next_doc < doc {
            match self.mode {
                Ords::Single => self.single.push(NONE),
                Ords::Multi => self.offsets.push(self.elems.len() as u64),
                Ords::None => {}
            }
            self.next_doc += 1;
        }
    }

    /// The value of a single-valued field.
    pub fn add(&mut self, doc: DocId, value: K) {
        debug_assert!(doc >= self.next_doc && doc < self.doc_count);
        self.skip_to(doc);
        let id = self.id(value, doc);
        match self.mode {
            Ords::Single => self.single.push(id),
            Ords::Multi => {
                self.offsets.push(self.elems.len() as u64);
                self.elems.push(id);
                self.present.push(doc);
            }
            Ords::None => {}
        }
        self.next_doc = doc + 1;
    }

    /// The elements of a multi-valued field the document has (None = null element). A field kept
    /// with single ordinals records the first value.
    pub fn add_many(&mut self, doc: DocId, values: impl IntoIterator<Item = Option<K>>) {
        debug_assert!(doc >= self.next_doc && doc < self.doc_count);
        self.skip_to(doc);
        if self.mode == Ords::Multi {
            self.offsets.push(self.elems.len() as u64);
            self.present.push(doc);
        }
        let mut first = NONE;
        for v in values {
            let id = match v {
                Some(k) => self.id(k, doc),
                None => NONE,
            };
            if self.mode == Ords::Multi {
                self.elems.push(id);
            } else if first == NONE {
                first = id;
            }
        }
        if self.mode == Ords::Single {
            self.single.push(first);
        }
        self.next_doc = doc + 1;
    }

    /// Whether any document had a value.
    pub fn has_values(&self) -> bool {
        !self.keys.is_empty()
    }

    /// Keys in ordinal order, the VALUE_DOCS section and the DOC_ORDS section.
    fn finish(mut self) -> (Vec<K>, Vec<u8>, Option<Vec<u8>>) {
        self.skip_to(self.doc_count);
        let n = self.keys.len();
        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_unstable_by(|&a, &b| self.keys[a as usize].cmp(&self.keys[b as usize]));
        let mut rank = vec![0u32; n];
        for (o, &id) in order.iter().enumerate() {
            rank[id as usize] = o as u32;
        }
        let docs = ValueDocs::write(order.iter().map(|&id| self.postings[id as usize].as_slice()));
        let remap = |id: &u32| if *id == NONE { n as u64 } else { u64::from(rank[*id as usize]) };
        let ords = match self.mode {
            Ords::None => None,
            Ords::Single => Some(DocOrds::write_single(&self.single.iter().map(remap).collect::<Vec<_>>(), n as u64)),
            Ords::Multi => {
                self.offsets.push(self.elems.len() as u64);
                Some(DocOrds::write_multi(&self.present, &self.offsets, self.elems.iter().map(remap), n as u64))
            }
        };
        let mut keys: Vec<Option<K>> = self.keys.into_iter().map(Some).collect();
        let keys = order.iter().map(|&id| keys[id as usize].take().unwrap()).collect();
        (keys, docs, ords)
    }
}

// -- bool --------------------------------------------------------------------------------------------

/// A bool or bool[] field: its documents with true and with false.
#[derive(Clone)]
pub struct BoolIndex {
    bytes: Bytes,
    pub true_docs: Bitmap,
    pub false_docs: Bitmap,
}

impl BoolIndex {
    pub fn build(true_docs: &[DocId], false_docs: &[DocId]) -> io::Result<Self> {
        let mut t = Vec::new();
        Bitmap::of(true_docs).write_frozen(&mut t);
        let mut out = Vec::new();
        out.extend_from_slice(&(t.len() as u64).to_le_bytes());
        out.extend_from_slice(&t);
        pad8(&mut out);
        let mut f = Vec::new();
        Bitmap::of(false_docs).write_frozen(&mut f);
        out.extend_from_slice(&f);
        Self::parse(Bytes::from_vec(out))
    }

    pub fn open(file: &SegmentFile, field: FieldId) -> io::Result<Option<Self>> {
        file.section(kind::BOOL, field).map(Self::parse).transpose()
    }

    fn parse(bytes: Bytes) -> io::Result<Self> {
        if bytes.len() < 8 {
            return Err(invalid("BOOL truncated"));
        }
        let tlen = bytes.u64_at(0) as usize;
        let fat = (8 + tlen).next_multiple_of(8);
        if fat > bytes.len() {
            return Err(invalid("BOOL bitmaps past end"));
        }
        let true_docs = Bitmap::from_frozen(bytes.slice(8, tlen))?;
        let false_docs = Bitmap::from_frozen(bytes.slice(fat, bytes.len() - fat))?;
        Ok(Self { bytes, true_docs, false_docs })
    }

    pub fn add_sections(&self, field: FieldId, w: &mut SegmentWriter) {
        w.add_bytes(kind::BOOL, field, self.bytes.clone());
    }

    /// The BOOL section.
    pub(crate) fn section(&self) -> &Bytes {
        &self.bytes
    }

    pub fn section_bytes(&self) -> usize {
        self.bytes.len()
    }

    #[inline]
    pub fn docs(&self, value: bool) -> &Bitmap {
        if value { &self.true_docs } else { &self.false_docs }
    }

    /// A bool field's value for `doc`.
    #[inline]
    pub fn value(&self, doc: DocId) -> Option<bool> {
        if self.true_docs.contains(doc) {
            Some(true)
        } else if self.false_docs.contains(doc) {
            Some(false)
        } else {
            None
        }
    }
}

// -- building a segment's value indexes from its documents --------------------------------------------

/// The value sections a field gets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Plan {
    Strings(Ords),
    Numbers { ords: Ords, float: bool },
    Bools,
}

pub(crate) fn plan(f: &FieldDef) -> Option<Plan> {
    // A reference field's values are its JOIN index: both directions need postings and ordinals.
    let reference = f.is_reference();
    let filterable = f.index || f.facet || reference;
    let ords = if f.facet || f.sort || reference { Ords::Single } else { Ords::None };
    let multi = if f.facet || reference { Ords::Multi } else { Ords::None };
    match f.field_type {
        // Scalar strings are filtered exactly only when faceted (other string filters use the text
        // index); sort fields need the ordinals.
        FieldType::String | FieldType::Text => (f.facet || f.sort || reference).then_some(Plan::Strings(ords)),
        FieldType::StringArray | FieldType::StringStar => filterable.then_some(Plan::Strings(multi)),
        FieldType::Int32 | FieldType::Int64 => (filterable || f.sort).then_some(Plan::Numbers { ords, float: false }),
        FieldType::Float64 => (filterable || f.sort).then_some(Plan::Numbers { ords, float: true }),
        FieldType::Int32Array | FieldType::Int64Array => filterable.then_some(Plan::Numbers { ords: multi, float: false }),
        FieldType::FloatArray { num_dim: 0 } => filterable.then_some(Plan::Numbers { ords: multi, float: true }),
        FieldType::Bool => (filterable || f.sort).then_some(Plan::Bools),
        FieldType::BoolArray => filterable.then_some(Plan::Bools),
        _ => None,
    }
}

pub enum FieldValues {
    Values(ValueIndex),
    Bools(BoolIndex),
}

fn build_field(docs: &[Document], f: &FieldDef, doc_count: u32) -> io::Result<Option<FieldValues>> {
    let Some(plan) = plan(f) else { return Ok(None) };
    let fid = f.id as usize;
    let values = docs.iter().filter_map(|d| d.fields.get(fid).map(|v| (d.id, v)));
    Ok(match plan {
        Plan::Strings(ords) => {
            let mut c = ValueCollector::<String>::new(doc_count, ords);
            for (doc, v) in values {
                match v {
                    FieldValue::Text(s) => c.add(doc, s.clone()),
                    FieldValue::StringArray(a) => c.add_many(doc, a.iter().cloned()),
                    _ => {}
                }
            }
            (c.has_values() || ords != Ords::None).then(|| ValueIndex::build_strings(c)).transpose()?.map(FieldValues::Values)
        }
        Plan::Numbers { ords, float } => {
            let mut c = ValueCollector::<i64>::new(doc_count, ords);
            for (doc, v) in values {
                match v {
                    FieldValue::Int32(n) => c.add(doc, i64::from(*n)),
                    FieldValue::Int64(n) => c.add(doc, *n),
                    FieldValue::Float64(x) => c.add(doc, f64_key(*x)),
                    FieldValue::Int32Array(a) => c.add_many(doc, a.iter().map(|o| o.map(i64::from))),
                    FieldValue::Int64Array(a) => c.add_many(doc, a.iter().copied()),
                    FieldValue::FloatArray(a) => c.add_many(doc, a.iter().map(|&x| Some(f64_key(f64::from(x))))),
                    _ => {}
                }
            }
            (c.has_values() || ords != Ords::None).then(|| ValueIndex::build_numbers(c, float)).transpose()?.map(FieldValues::Values)
        }
        Plan::Bools => {
            let (mut t, mut fl) = (Vec::new(), Vec::new());
            for (doc, v) in values {
                match v {
                    FieldValue::Bool(true) => t.push(doc),
                    FieldValue::Bool(false) => fl.push(doc),
                    // Match-any: a document with both values satisfies `:=true` and `:=false`.
                    FieldValue::BoolArray(a) => {
                        if a.iter().flatten().any(|b| *b) {
                            t.push(doc);
                        }
                        if a.iter().flatten().any(|b| !*b) {
                            fl.push(doc);
                        }
                    }
                    _ => {}
                }
            }
            (!t.is_empty() || !fl.is_empty() || f.sort).then(|| BoolIndex::build(&t, &fl)).transpose()?.map(FieldValues::Bools)
        }
    })
}

/// Value and bool indexes of every field of a segment. `docs` are the segment's documents with
/// doc ids `0..doc_count` in order.
pub fn build_segment(
    docs: &[Document],
    schema: &Schema,
    doc_count: u32,
) -> io::Result<(FxHashMap<FieldId, ValueIndex>, FxHashMap<FieldId, BoolIndex>)> {
    use rayon::prelude::*;
    debug_assert!(docs.iter().enumerate().all(|(i, d)| d.id as usize == i));
    let built: Vec<(FieldId, FieldValues)> = schema
        .fields
        .par_iter()
        .map(|f| Ok(build_field(docs, f, doc_count)?.map(|v| (f.id, v))))
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    let (mut values, mut bools) = (FxHashMap::default(), FxHashMap::default());
    for (fid, v) in built {
        match v {
            FieldValues::Values(v) => {
                values.insert(fid, v);
            }
            FieldValues::Bools(b) => {
                bools.insert(fid, b);
            }
        }
    }
    Ok((values, bools))
}

// -- merging segments' value indexes ----------------------------------------------------------------

/// A field's dictionary merged from the sources' (`index::merge`): its values with documents left,
/// in key order, each with the (source, ordinal there) it comes from; per source, the merged
/// ordinal of each of its ordinals (`NONE`: none of its documents left).
struct MergedDict {
    refs: Vec<(u32, u64)>,
    /// Value `o`'s sources: `refs[at[o]..at[o + 1]]`.
    at: Vec<usize>,
    ord_maps: Vec<Vec<u32>>,
}

impl MergedDict {
    fn new(srcs: &[Option<&ValueIndex>]) -> Self {
        Self { refs: Vec::new(), at: vec![0], ord_maps: srcs.iter().map(|i| vec![NONE; i.map_or(0, |i| i.len() as usize)]).collect() }
    }

    fn len(&self) -> usize {
        self.at.len() - 1
    }

    /// Add the value the sources `group` ((source, ordinal), ascending) hold, if any of its
    /// documents is carried over.
    fn add(&mut self, group: &[(u32, u64)], srcs: &[Option<&ValueIndex>], docs: &DocMap) -> bool {
        if !group.iter().any(|&(s, o)| srcs[s as usize].is_some_and(|i| i.any_live(o, s as usize, docs))) {
            return false;
        }
        let ord = self.len() as u32;
        for &(s, o) in group {
            self.ord_maps[s as usize][o as usize] = ord;
        }
        self.refs.extend_from_slice(group);
        self.at.push(self.refs.len());
        true
    }

    /// The merged ordinal of source `s`'s ordinal `o` (`n`, none, when no document of it is left).
    #[inline]
    fn get(&self, s: u32, o: u64, n: u64) -> u64 {
        match self.ord_maps[s as usize].get(o as usize) {
            Some(&m) if m != NONE => u64::from(m),
            _ => n,
        }
    }
}

impl ValueIndex {
    /// Whether any document with value `ord` (this index being source `s`'s) is carried over.
    fn any_live(&self, ord: u64, s: usize, docs: &DocMap) -> bool {
        match self.docs.posting(ord) {
            Some(Posting::One(d)) => docs.get(s, d) != GONE,
            Some(Posting::Many(bm)) => docs.any_live(s, &bm),
            None => false,
        }
    }

    /// Append the documents with value `ord` carried over, renumbered.
    fn push_live(&self, ord: u64, s: usize, docs: &DocMap, out: &mut Vec<DocId>) {
        match self.docs.posting(ord) {
            Some(Posting::One(d)) => {
                let k = docs.get(s, d);
                if k != GONE {
                    out.push(k);
                }
            }
            Some(Posting::Many(bm)) => docs.push_mapped(s, &bm, out),
            None => {}
        }
    }
}

/// A field's value or bool index merged from the sources' (`index::merge`): the values with
/// documents left, in key order, with their postings and the documents' ordinals renumbered; the
/// index `build_field` makes of the same documents.
pub(crate) fn merge_field(f: &FieldDef, sources: &[&Segment], docs: &DocMap) -> io::Result<Option<FieldValues>> {
    let Some(plan) = plan(f) else { return Ok(None) };
    let (ords, float) = match plan {
        Plan::Bools => {
            let (mut t, mut fl) = (Vec::new(), Vec::new());
            for (s, seg) in sources.iter().enumerate() {
                if let Some(b) = seg.bools.get(&f.id) {
                    docs.push_mapped(s, &b.true_docs, &mut t);
                    docs.push_mapped(s, &b.false_docs, &mut fl);
                }
            }
            sort_docs(&mut t);
            sort_docs(&mut fl);
            return (!t.is_empty() || !fl.is_empty() || f.sort).then(|| BoolIndex::build(&t, &fl)).transpose().map(|b| b.map(FieldValues::Bools));
        }
        Plan::Strings(ords) => (ords, None),
        Plan::Numbers { ords, float } => (ords, Some(float)),
    };
    let srcs: Vec<Option<&ValueIndex>> = sources.iter().map(|s| s.values.get(&f.id)).collect();
    let mut dict = MergedDict::new(&srcs);
    let mut group: Vec<(u32, u64)> = Vec::new();
    let dict_bytes = match float {
        None => {
            // the union of the sources' value FSTs, in byte order
            let mut with = Vec::new();
            let mut op = fst::map::OpBuilder::new();
            for (s, i) in srcs.iter().enumerate() {
                if let Some(ValueIndex { dict: Dict::Str(m), .. }) = i {
                    with.push(s as u32);
                    op = op.add(m);
                }
            }
            let (mut keys, mut key_at) = (Vec::new(), vec![0usize]);
            let mut union = op.union();
            while let Some((k, ivs)) = union.next() {
                group.clear();
                group.extend(ivs.iter().map(|iv| (with[iv.index], iv.value)));
                group.sort_unstable();
                if dict.add(&group, &srcs, docs) {
                    keys.extend_from_slice(k);
                    key_at.push(keys.len());
                }
            }
            strings_dict(key_at.windows(2).map(|w| &keys[w[0]..w[1]]))?
        }
        Some(float) => {
            // the sources' sorted keys merged
            let mut heap: BinaryHeap<Reverse<(i64, u32, u64)>> = BinaryHeap::new();
            let next = |heap: &mut BinaryHeap<Reverse<(i64, u32, u64)>>, s: u32, o: u64| {
                if let Some(k) = srcs[s as usize].and_then(|i| i.key_at(o)) {
                    heap.push(Reverse((k, s, o)));
                }
            };
            for s in 0..srcs.len() as u32 {
                next(&mut heap, s, 0);
            }
            let mut keys = Vec::new();
            while let Some(Reverse((k, s, o))) = heap.pop() {
                group.clear();
                group.push((s, o));
                next(&mut heap, s, o + 1);
                while let Some(&Reverse((k2, s2, o2))) = heap.peek() {
                    if k2 != k {
                        break;
                    }
                    heap.pop();
                    group.push((s2, o2));
                    next(&mut heap, s2, o2 + 1);
                }
                group.sort_unstable();
                if dict.add(&group, &srcs, docs) {
                    keys.push(k);
                }
            }
            numbers_dict(&keys, float)
        }
    };
    let n = dict.len();
    if n == 0 && ords == Ords::None {
        return Ok(None);
    }
    let value_docs = ValueDocs::write_with(n, |ord, out| {
        out.clear();
        for &(s, o) in &dict.refs[dict.at[ord]..dict.at[ord + 1]] {
            if let Some(i) = srcs[s as usize] {
                i.push_live(o, s as usize, docs, out);
            }
        }
        sort_docs(out);
    });
    let none = n as u64;
    let doc_ords = match ords {
        Ords::None => None,
        Ords::Single => {
            let o: Vec<u64> = docs.order().iter().map(|&(s, d)| srcs[s as usize].and_then(|i| i.doc_ord(d)).map_or(none, |o| dict.get(s, o, none))).collect();
            Some(DocOrds::write_single(&o, none))
        }
        Ords::Multi => {
            let (mut present, mut offsets, mut elems) = (Vec::new(), Vec::with_capacity(docs.len() as usize + 1), Vec::new());
            for (k, &(s, d)) in docs.order().iter().enumerate() {
                offsets.push(elems.len() as u64);
                if let Some(o) = srcs[s as usize].and_then(ValueIndex::doc_ords) {
                    if o.present(d) {
                        present.push(k as DocId);
                    }
                    elems.extend(o.elems(d).map(|e| e.map_or(none, |e| dict.get(s, e, none))));
                }
            }
            offsets.push(elems.len() as u64);
            Some(DocOrds::write_multi(&present, &offsets, elems.into_iter(), none))
        }
    };
    let kind = if float.is_some() { kind::NUM_VALUES } else { kind::VALUES };
    ValueIndex::from_sections(kind, Bytes::from_vec(dict_bytes), Bytes::from_vec(value_docs), doc_ords.map(Bytes::from_vec), docs.len())
        .map(|v| Some(FieldValues::Values(v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(docs: &[Option<Vec<Option<&str>>>], mode: Ords) -> ValueIndex {
        let mut c = ValueCollector::<String>::new(docs.len() as u32, mode);
        for (d, v) in docs.iter().enumerate() {
            if let Some(v) = v {
                if mode == Ords::Single {
                    c.add(d as u32, v[0].unwrap().to_string());
                } else {
                    c.add_many(d as u32, v.iter().map(|s| s.map(str::to_string)));
                }
            }
        }
        ValueIndex::build_strings(c).unwrap()
    }

    #[test]
    fn single_valued_strings() {
        let vals = ["Stockholm", "Göteborg", "Malmö", "Stockholm", "Lund"];
        let mut docs: Vec<Option<Vec<Option<&str>>>> = vals.iter().map(|v| Some(vec![Some(*v)])).collect();
        docs.insert(2, None);
        let idx = strings(&docs, Ords::Single);
        assert_eq!(idx.len(), 4);
        // Ordinals in byte order.
        let all: Vec<String> = (0..4).map(|o| idx.str_at(o).unwrap()).collect();
        assert_eq!(all, ["Göteborg", "Lund", "Malmö", "Stockholm"]);
        assert_eq!(idx.str_at(4), None);
        assert_eq!(idx.eq_str("Stockholm").unwrap().to_vec(), [0, 4]);
        assert_eq!(idx.eq_str("Lund").unwrap().to_vec(), [5]);
        assert!(idx.eq_str("Uppsala").is_none());
        assert_eq!(idx.df(3), 2);
        assert_eq!(idx.doc_ord(0), Some(3));
        assert_eq!(idx.doc_ord(2), None);
        assert_eq!(idx.doc_ord(3), Some(2));
        let cand = Bitmap::of(&[0, 1, 5]);
        assert_eq!(idx.count_in(3, &cand), 1);
        assert_eq!(idx.str_ords_with_prefix("st"), [3]);
        assert_eq!(idx.str_ords_with_prefix("g"), [0]);
        assert!(idx.str_ords_with_prefix("x").is_empty());
        assert_eq!(idx.union(0..4).to_vec(), [0, 1, 3, 4, 5]);
    }

    #[test]
    fn multi_valued_strings_keep_order_nulls_and_empty_arrays() {
        let docs = vec![
            Some(vec![Some("b"), None, Some("a"), Some("b")]),
            None,
            Some(vec![]),
            Some(vec![Some("a")]),
        ];
        let idx = strings(&docs, Ords::Multi);
        let o = idx.doc_ords().unwrap();
        assert!(o.is_multi());
        assert_eq!(o.elems(0).collect::<Vec<_>>(), [Some(1), None, Some(0), Some(1)]);
        assert_eq!(o.elems(1).count(), 0);
        assert!(!o.present(1));
        assert!(o.present(2));
        assert_eq!(o.elems(2).count(), 0);
        assert_eq!(o.elems(3).collect::<Vec<_>>(), [Some(0)]);
        assert_eq!(idx.eq_str("b").unwrap().to_vec(), [0]);
        assert_eq!(idx.eq_str("a").unwrap().to_vec(), [0, 3]);
    }

    #[test]
    fn numbers_ranges_and_values() {
        let n = 5000u32;
        let mut c = ValueCollector::<i64>::new(n, Ords::Single);
        for d in 0..n {
            if d % 7 != 0 {
                c.add(d, i64::from(d % 100) - 50);
            }
        }
        let idx = ValueIndex::build_numbers(c, false).unwrap();
        assert_eq!(idx.len(), 100);
        assert_eq!(idx.key_at(0), Some(-50));
        let r = idx.key_ords(Bound::Excluded(-1), Bound::Included(10));
        assert_eq!((idx.key_at(r.start), idx.key_at(r.end - 1)), (Some(0), Some(10)));
        let got = idx.union(r).to_vec();
        let want: Vec<u32> = (0..n).filter(|d| d % 7 != 0 && (50..=60).contains(&(d % 100))).collect();
        assert_eq!(got, want);
        assert_eq!(idx.doc_i64(1), Some(-49));
        assert_eq!(idx.doc_i64(7), None);
        assert!(idx.eq_key(1000).is_none());
        assert_eq!(idx.key_ords(Bound::Included(i64::MIN), Bound::Included(i64::MAX)), 0..100);
        assert!(idx.key_ords(Bound::Included(60), Bound::Excluded(60)).is_empty());
    }

    #[test]
    fn floats_order_and_negative_zero() {
        let vals = [2.5, -1.0, -0.0, 0.0, 1e300, -1e-300, 11.952819999999999];
        let mut c = ValueCollector::<i64>::new(vals.len() as u32, Ords::Single);
        for (d, &v) in vals.iter().enumerate() {
            c.add(d as u32, f64_key(v));
        }
        let idx = ValueIndex::build_numbers(c, true).unwrap();
        assert_eq!(idx.len(), 6);
        let sorted: Vec<f64> = (0..6).map(|o| key_f64(idx.key_at(o).unwrap())).collect();
        assert_eq!(sorted, [-1.0, -1e-300, 0.0, 2.5, 11.952819999999999, 1e300]);
        assert_eq!(idx.eq_key(f64_key(0.0)).unwrap().to_vec(), [2, 3]);
        assert_eq!(idx.doc_f64(6), Some(11.952819999999999));
        assert_eq!(idx.doc_i64(0), Some(2));
    }

    #[test]
    fn sections_round_trip_through_a_segment_file() {
        let docs = vec![Some(vec![Some("x"), Some("y")]), Some(vec![Some("y")]), None];
        let idx = strings(&docs, Ords::Multi);
        let mut c = ValueCollector::<i64>::new(3, Ords::None);
        c.add(0, 5);
        c.add(2, 5);
        let num = ValueIndex::build_numbers(c, false).unwrap();
        let b = BoolIndex::build(&[0, 2], &[1]).unwrap();
        let mut w = SegmentWriter::new();
        idx.add_sections(7, &mut w);
        num.add_sections(8, &mut w);
        b.add_sections(9, &mut w);
        let file = SegmentFile::from_vec(w.finish(3, 0)).unwrap();
        let idx2 = ValueIndex::open(&file, 7).unwrap().unwrap();
        assert_eq!(idx2.eq_str("y").unwrap().to_vec(), [0, 1]);
        assert_eq!(idx2.doc_ords().unwrap().elems(0).collect::<Vec<_>>(), [Some(0), Some(1)]);
        let num2 = ValueIndex::open(&file, 8).unwrap().unwrap();
        assert_eq!(num2.eq_key(5).unwrap().to_vec(), [0, 2]);
        assert!(num2.doc_ords().is_none());
        let b2 = BoolIndex::open(&file, 9).unwrap().unwrap();
        assert_eq!((b2.value(0), b2.value(1), b2.value(3)), (Some(true), Some(false), None));
        assert!(ValueIndex::open(&file, 9).unwrap().is_none());
        assert_eq!(idx2.section_bytes(), idx.section_bytes());
    }

    #[test]
    fn reverse_lookup_matches_forward_on_many_keys() {
        let mut keys: Vec<String> = (0..3000).map(|i| format!("{:x}{}", i * 7919 % 4093, "abc".repeat(i % 4))).collect();
        keys.sort();
        keys.dedup();
        let mut c = ValueCollector::<String>::new(keys.len() as u32, Ords::None);
        for (d, k) in keys.iter().enumerate() {
            c.add(d as u32, k.clone());
        }
        let idx = ValueIndex::build_strings(c).unwrap();
        for (o, k) in keys.iter().enumerate() {
            assert_eq!(idx.str_ord(k), Some(o as u64));
            assert_eq!(idx.str_at(o as u64).as_deref(), Some(k.as_str()));
        }
    }
}
