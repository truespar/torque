//! Text field index (TQBF spec §6): per field a term FST, frozen roaring postings, bit-packed term
//! frequencies and bit-packed field lengths. Every section is read in place, from the segment file
//! once persisted, from the builder's buffers before that. Exact, prefix (best-first top-k), typo
//! (Levenshtein automaton) and infix lookups all run on the FSTs; nothing is loaded into the heap.
//!
//! Terms are numbered by document frequency, most frequent first (ties in byte order). Ordinals
//! `0..n_postings` are terms in two or more documents and have a posting; the rest occur in one
//! document, stored as a doc id (Lucene's singleton doc id). Because the FST outputs are these
//! ordinals, the most frequent completions of a prefix come out of a best-first walk first.
//!
//! Sections (all integers little-endian):
//!
//! ```text
//! TERMS     FST term -> ordinal
//! RAWTERMS  FST "lowered\0term" -> ordinal of the term, for lower-cased tokens that differ from
//!           their term (accents, stemming); absent when there are none
//! POSTINGS  u64 n_postings, u64 n_singles, u64 offset[n_postings + 1], u32 df[n_postings],
//!           u32 doc_width, u32 0, the single-document terms' doc ids bit-packed at doc_width
//!           (8 zero bytes of slack, padded to 8), frozen roaring bitmaps (each on a 4-byte boundary)
//! FREQS     u64 n_postings, u64 n_singles, u64 group_bit[n_postings + 2], u8 width[n_postings + 1],
//!           padded to 8, then (tf - 1) bit-packed group after group, 8 zero bytes of slack. Group p
//!           is posting p in doc order, group n_postings the single-document terms in ordinal order.
//! NORMS     u64 total_length, u32 doc_count, u32 docs_with_tokens, u8 width, u8 min, u8 sparse,
//!           5 zero bytes; sparse: u32 bitmap_len, u32 0, frozen roaring of the docs with tokens,
//!           padded to 8; then (norm - min) bit-packed at width, per doc (dense) or per doc with
//!           tokens (sparse); 8 zero bytes of slack. Norm = field length as Lucene's
//!           SmallFloat.intToByte4; total_length sums the lengths the norms stand for, so a merge
//!           computes it as a build does.
//! ```
//!
//! A large field's posting terms encode in parallel runs of about `ENCODE_RUN_DOCS` documents: a
//! run's bitmaps start on an 8-byte boundary and its groups on a 64-bit one. Runs are cut by the
//! terms' documents, so the sections are the same on every machine. A merge (`index::merge`)
//! encodes the union of its sources' terms the same way, their postings read in place and
//! renumbered.

use crate::bitmap::{Bitmap, RankCursor};
use crate::index::merge::{DocMap, GONE};
use crate::schema::{DocId, FieldId};
use crate::tqbf::bytes::Bytes;
use crate::tqbf::container::{kind, SegmentFile, SegmentWriter};
use crate::tqbf::packed::{self, BitWriter};
use fst::{IntoStreamer, Streamer};
use levenshtein_automata::{Distance, LevenshteinAutomatonBuilder, DFA};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;
use std::sync::OnceLock;

/// Separates the lower-cased token from its term in RAWTERMS keys. Tokens never contain it (it is
/// no letter, digit or symbol the tokenizer keeps; the encoder skips any that would).
pub(crate) const RAW_SEP: u8 = 0;

/// FST output of a TERMS entry: the term's ordinal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermRef(pub u64);

impl TermRef {
    #[inline]
    pub fn ordinal(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone)]
enum Freqs {
    One(u32),
    Packed { bits: Bytes, start: u64, width: u32 },
}

/// A term's postings: documents plus the term frequency of each.
#[derive(Clone)]
pub struct TermEntry {
    bm: Bitmap,
    freqs: Freqs,
}

impl TermEntry {
    #[inline]
    pub fn bitmap(&self) -> &Bitmap {
        &self.bm
    }

    /// Term frequency in the `rank`-th document of the posting list (0-based).
    #[inline]
    pub fn tf_at(&self, rank: u64) -> u32 {
        match &self.freqs {
            Freqs::One(tf) => *tf,
            Freqs::Packed { bits, start, width } => 1 + read_bits(bits, start + rank * u64::from(*width), *width),
        }
    }

    /// Term frequency in `doc`; 0 when the term does not occur in it.
    #[inline]
    pub fn tf(&self, doc: DocId) -> u32 {
        if self.bm.contains(doc) {
            self.tf_at(self.bm.rank(doc) - 1)
        } else {
            0
        }
    }

    /// Term frequencies read for ascending documents.
    pub fn cursor(&self) -> TermCursor<'_> {
        TermCursor { ranks: self.bm.rank_cursor(), entry: self }
    }
}

/// `TermEntry::tf` for documents in ascending order.
pub struct TermCursor<'a> {
    ranks: RankCursor<'a>,
    entry: &'a TermEntry,
}

impl TermCursor<'_> {
    #[inline]
    pub fn tf(&mut self, doc: DocId) -> u32 {
        self.ranks.rank(doc).map_or(0, |r| self.entry.tf_at(r))
    }
}

#[inline]
fn read_bits(bits: &[u8], pos: u64, width: u32) -> u32 {
    packed::read(bits, pos, width) as u32
}

#[inline]
fn bits_for(max: u64) -> u32 {
    packed::bits_for(max)
}

// -- field lengths (Lucene SmallFloat.intToByte4 / byte4ToInt) ------------------------------------

/// Lengths below this are stored exactly.
const NUM_FREE_VALUES: u32 = 24;

fn long_to_int4(i: u32) -> u32 {
    let bits = 32 - i.leading_zeros();
    if bits < 4 {
        i
    } else {
        let shift = bits - 4;
        ((i >> shift) & 0x07) | (shift + 1) << 3
    }
}

fn int4_to_long(i: u32) -> u32 {
    let bits = i & 0x07;
    let shift = (i >> 3) as i32 - 1;
    if shift == -1 { bits } else { (bits | 0x08) << shift }
}

/// Field length (tokens) to its one-byte norm: exact below 24, then 4 significant bits.
pub fn length_to_norm(len: u32) -> u8 {
    if len < NUM_FREE_VALUES {
        len as u8
    } else {
        (NUM_FREE_VALUES + long_to_int4(len - NUM_FREE_VALUES)).min(255) as u8
    }
}

fn norm_to_length(norm: u8) -> u32 {
    let i = u32::from(norm);
    if i < NUM_FREE_VALUES { i } else { NUM_FREE_VALUES + int4_to_long(i - NUM_FREE_VALUES) }
}

fn norm_table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|b| norm_to_length(b as u8)))
}

/// NORMS section of field lengths: bit-packed at the width the field needs, for every document or,
/// when that is smaller, only for the documents that have tokens (Lucene's dense/sparse norms).
pub(crate) fn build_norms(lengths: &[u32]) -> Vec<u8> {
    let norms: Vec<u8> = lengths.iter().map(|&l| length_to_norm(l)).collect();
    encode_norms(&norms)
}

fn encode_norms(norms: &[u8]) -> Vec<u8> {
    let present: Vec<u32> = (0..norms.len() as u32).filter(|&d| norms[d as usize] > 0).collect();
    let (min, max) = present.iter().fold((u8::MAX, 0u8), |(lo, hi), &d| (lo.min(norms[d as usize]), hi.max(norms[d as usize])));
    let min = if present.is_empty() { 0 } else { min };
    // Dense: every doc at the width of the largest norm (0 = no tokens). Sparse: the docs with
    // tokens as a bitmap, their norms above the smallest one. The smaller layout wins.
    let presence = Bitmap::of(&present);
    let dense_bits = norms.len() as u64 * u64::from(bits_for(u64::from(max)));
    let sparse_bits = 8 * presence.serialized_size() as u64 + present.len() as u64 * u64::from(bits_for(u64::from(max - min)));
    let sparse = sparse_bits < dense_bits;
    let (min, width) = if sparse { (min, bits_for(u64::from(max - min))) } else { (0, bits_for(u64::from(max))) };
    let total: u64 = norms.iter().map(|&n| u64::from(norm_to_length(n))).sum();

    let mut out = Vec::with_capacity(24 + norms.len());
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(&(norms.len() as u32).to_le_bytes());
    out.extend_from_slice(&(present.len() as u32).to_le_bytes());
    out.extend_from_slice(&[width as u8, min, u8::from(sparse), 0, 0, 0, 0, 0]);
    let mut bits = BitWriter::new();
    if sparse {
        let mut bm = Vec::new();
        presence.write_frozen(&mut bm);
        out.extend_from_slice(&(bm.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&bm);
        out.resize(out.len().next_multiple_of(8), 0);
        for &d in &present {
            bits.push(u64::from(norms[d as usize] - min), width);
        }
    } else {
        for &n in norms {
            bits.push(u64::from(n), width);
        }
    }
    out.extend_from_slice(&bits.into_bytes());
    out
}

// -- typo automata ----------------------------------------------------------------------------------

/// Levenshtein DFA for `term` (Damerau: an adjacent transposition costs one edit). Distances
/// above 2 are clamped to 2.
pub fn typo_dfa(term: &str, max_distance: u8) -> DFA {
    static BUILDERS: [OnceLock<LevenshteinAutomatonBuilder>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let d = max_distance.min(2);
    BUILDERS[d as usize].get_or_init(|| LevenshteinAutomatonBuilder::new(d, true)).build_dfa(term)
}

/// Like `typo_dfa`, for terms that start with `term` within the distance.
pub fn prefix_typo_dfa(term: &str, max_distance: u8) -> DFA {
    static BUILDERS: [OnceLock<LevenshteinAutomatonBuilder>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let d = max_distance.min(2);
    BUILDERS[d as usize].get_or_init(|| LevenshteinAutomatonBuilder::new(d, true)).build_prefix_dfa(term)
}

// -- best-first walk --------------------------------------------------------------------------------

/// The keys of an FST that start with a prefix, in increasing output order (ties in byte order).
///
/// The FST builder pushes outputs toward the root (a transition carries the smallest output below
/// it), so the output accumulated at a node is the smallest output of any key through it. A
/// best-first walk on that bound reaches the k smallest keys in about k times the key length
/// steps instead of scanning every key under the prefix (the technique of Lucene's FST suggesters).
struct BestFirst<'a> {
    fst: &'a fst::raw::Fst<Bytes>,
    /// (output so far, key so far, node address, the key ends here)
    heap: BinaryHeap<Reverse<(u64, Vec<u8>, usize, bool)>>,
}

impl<'a> BestFirst<'a> {
    fn new(fst: &'a fst::raw::Fst<Bytes>, prefix: &[u8]) -> Self {
        let mut heap = BinaryHeap::new();
        let mut node = fst.root();
        let mut out = 0u64;
        let mut found = true;
        for &b in prefix {
            match node.find_input(b) {
                Some(i) => {
                    let t = node.transition(i);
                    out += t.out.value();
                    node = fst.node(t.addr);
                }
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found {
            heap.push(Reverse((out, prefix.to_vec(), node.addr(), false)));
        }
        Self { fst, heap }
    }
}

impl Iterator for BestFirst<'_> {
    type Item = (Vec<u8>, u64);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(Reverse((out, key, addr, is_final))) = self.heap.pop() {
            if is_final {
                return Some((key, out));
            }
            let node = self.fst.node(addr);
            if node.is_final() {
                self.heap.push(Reverse((out + node.final_output().value(), key.clone(), addr, true)));
            }
            for t in node.transitions() {
                let mut k = Vec::with_capacity(key.len() + 1);
                k.extend_from_slice(&key);
                k.push(t.inp);
                self.heap.push(Reverse((out + t.out.value(), k, t.addr, false)));
            }
        }
        None
    }
}

// -- building ---------------------------------------------------------------------------------------

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("text index: {}", msg.into()))
}

/// A field's terms as `encode_text` reads them: in byte order, unique, each with its document
/// frequency and its postings (doc ids ascending, the term's frequency in each). Read from several
/// threads at once (a large field's terms encode in parallel).
pub(crate) trait TermSource: Sync {
    /// What `postings` needs on a thread.
    type Scratch: Default;
    fn len(&self) -> usize;
    fn term(&self, i: usize) -> &[u8];
    fn df(&self, i: usize) -> u32;
    /// Replace `docs` with the term's documents and `tfs` with its frequency in each.
    fn postings(&self, i: usize, scratch: &mut Self::Scratch, docs: &mut Vec<DocId>, tfs: &mut Vec<u32>);
}

/// The five sections of a text field (RAWTERMS only when some token differs from its term).
pub(crate) struct TextSections {
    pub terms: Vec<u8>,
    pub raw: Option<Vec<u8>>,
    pub postings: Vec<u8>,
    pub freqs: Vec<u8>,
    pub norms: Vec<u8>,
}

/// One FREQS group: the frequencies less one at the width the largest needs; returns the width.
fn write_group(bits: &mut BitWriter, tfs: &[u32]) -> u8 {
    let width = bits_for(u64::from(tfs.iter().copied().max().unwrap_or(1).max(1) - 1));
    for &tf in tfs {
        bits.push(u64::from(tf.max(1) - 1), width);
    }
    width as u8
}

/// The posting terms of a field encode in runs of about this many documents, the runs in parallel
/// when there are several. Runs are cut by the terms' documents, not by the threads, so a field's
/// sections are the same on every machine.
const ENCODE_RUN_DOCS: u64 = 1 << 18;

/// A run of posting terms encoded: their bitmaps (laid out from an 8-aligned position), their FREQS
/// groups (from bit 0), and per term where its bitmap and its group start, its documents and its
/// group's width.
struct EncodedRun {
    bitmaps: Vec<u8>,
    bits: BitWriter,
    at: Vec<u64>,
    df: Vec<u32>,
    group_bit: Vec<u64>,
    widths: Vec<u8>,
}

fn encode_run<S: TermSource>(src: &S, terms: &[usize], scratch: &mut S::Scratch) -> io::Result<EncodedRun> {
    let mut r = EncodedRun {
        bitmaps: Vec::new(),
        bits: BitWriter::new(),
        at: Vec::with_capacity(terms.len()),
        df: Vec::with_capacity(terms.len()),
        group_bit: Vec::with_capacity(terms.len()),
        widths: Vec::with_capacity(terms.len()),
    };
    let (mut docs, mut tfs) = (Vec::new(), Vec::new());
    for &i in terms {
        src.postings(i, scratch, &mut docs, &mut tfs);
        if tfs.len() != docs.len() || docs.len() != src.df(i) as usize {
            return Err(invalid(format!("term {i}: {} documents, {} frequencies", docs.len(), tfs.len())));
        }
        // Frozen bitmaps start on a 4-byte boundary.
        r.bitmaps.resize(r.bitmaps.len().next_multiple_of(4), 0);
        r.at.push(r.bitmaps.len() as u64);
        r.df.push(docs.len() as u32);
        Bitmap::of(&docs).write_frozen(&mut r.bitmaps);
        r.group_bit.push(r.bits.pos());
        r.widths.push(write_group(&mut r.bits, &tfs));
    }
    Ok(r)
}

/// Encode a field's sections. `raw` holds every (lower-cased token, its term) pair, in any order;
/// `norms` is the NORMS section (`build_norms`).
pub(crate) fn encode_text<S: TermSource>(src: &S, doc_count: u32, raw: &[(&[u8], &[u8])], norms: Vec<u8>) -> io::Result<TextSections> {
    use rayon::prelude::*;
    let n_terms = src.len();
    // Ordinals: document frequency descending, ties in term order (the sort is stable).
    let mut order: Vec<usize> = (0..n_terms).collect();
    order.sort_by_key(|&i| Reverse(src.df(i)));
    let mut ordinal = vec![0usize; n_terms];
    for (o, &i) in order.iter().enumerate() {
        ordinal[i] = o;
    }
    let n = order.iter().take_while(|&&i| src.df(i) > 1).count();
    let singles = &order[n..];

    // POSTINGS header and the single-document terms' doc ids, then the bitmaps; FREQS groups.
    let mut scratch = S::Scratch::default();
    let (mut docs, mut tfs) = (Vec::new(), Vec::new());
    let df_at = 16 + 8 * (n + 1);
    let doc_width = bits_for(u64::from(doc_count.saturating_sub(1)));
    let mut single_docs = BitWriter::new();
    let mut single_tfs: Vec<u32> = Vec::with_capacity(singles.len());
    for &i in singles {
        src.postings(i, &mut scratch, &mut docs, &mut tfs);
        if docs.len() != 1 || tfs.len() != 1 {
            return Err(invalid(format!("single-document term {i} has {} documents and {} frequencies", docs.len(), tfs.len())));
        }
        single_docs.push(u64::from(docs[0]), doc_width);
        single_tfs.push(tfs[0]);
    }
    let single_docs = single_docs.into_bytes();
    let single_at = df_at + 4 * n + 8;
    let header = (single_at + single_docs.len()).next_multiple_of(8);
    let mut postings = vec![0u8; header];
    postings[..8].copy_from_slice(&(n as u64).to_le_bytes());
    postings[8..16].copy_from_slice(&(singles.len() as u64).to_le_bytes());
    postings[single_at - 8..single_at - 4].copy_from_slice(&doc_width.to_le_bytes());
    postings[single_at..single_at + single_docs.len()].copy_from_slice(&single_docs);

    // the posting terms in runs of about ENCODE_RUN_DOCS documents
    let mut runs: Vec<&[usize]> = Vec::new();
    let (mut start, mut run_docs) = (0usize, 0u64);
    for (p, &i) in order[..n].iter().enumerate() {
        run_docs += u64::from(src.df(i));
        if run_docs >= ENCODE_RUN_DOCS {
            runs.push(&order[start..=p]);
            (start, run_docs) = (p + 1, 0);
        }
    }
    if start < n {
        runs.push(&order[start..n]);
    }
    let encoded: Vec<EncodedRun> = if runs.len() > 1 {
        runs.par_iter().map_init(S::Scratch::default, |scratch, terms| encode_run(src, terms, scratch)).collect::<io::Result<_>>()?
    } else {
        runs.iter().map(|terms| encode_run(src, terms, &mut scratch)).collect::<io::Result<_>>()?
    };
    let mut bits = BitWriter::new();
    let mut group_bit: Vec<u64> = Vec::with_capacity(n + 2);
    let mut widths: Vec<u8> = Vec::with_capacity(n + 1);
    let mut p = 0usize;
    for (terms, run) in runs.iter().zip(encoded) {
        // a run's bitmaps from an 8-aligned position, its groups from a 64-bit boundary
        postings.resize(postings.len().next_multiple_of(8), 0);
        let base = postings.len() as u64;
        postings.extend_from_slice(&run.bitmaps);
        let bit_base = bits.append_aligned(run.bits);
        for k in 0..terms.len() {
            postings[16 + 8 * p..24 + 8 * p].copy_from_slice(&(base + run.at[k]).to_le_bytes());
            postings[df_at + 4 * p..df_at + 4 * p + 4].copy_from_slice(&run.df[k].to_le_bytes());
            group_bit.push(bit_base + run.group_bit[k]);
            p += 1;
        }
        widths.extend_from_slice(&run.widths);
    }
    let end = postings.len() as u64;
    postings[16 + 8 * n..24 + 8 * n].copy_from_slice(&end.to_le_bytes());
    group_bit.push(bits.pos());
    widths.push(write_group(&mut bits, &single_tfs));
    group_bit.push(bits.pos());

    let packed = bits.into_bytes();
    let table = 16 + 8 * (n + 2) + n + 1;
    let mut freqs = Vec::with_capacity(table.next_multiple_of(8) + packed.len());
    freqs.extend_from_slice(&(n as u64).to_le_bytes());
    freqs.extend_from_slice(&(singles.len() as u64).to_le_bytes());
    for b in &group_bit {
        freqs.extend_from_slice(&b.to_le_bytes());
    }
    freqs.extend_from_slice(&widths);
    freqs.resize(table.next_multiple_of(8), 0);
    freqs.extend_from_slice(&packed);

    let mut terms_fst = fst::MapBuilder::memory_sized(n_terms);
    for (i, &o) in ordinal.iter().enumerate() {
        terms_fst.insert(src.term(i), o as u64).map_err(|e| invalid(e.to_string()))?;
    }

    // RAWTERMS: tokens that differ from their term, keyed "lowered\0term".
    let find = |term: &[u8]| {
        let (mut lo, mut hi) = (0usize, n_terms);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match src.term(mid).cmp(term) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    };
    let mut raw_keys: Vec<(Vec<u8>, u64)> = Vec::new();
    for &(lowered, term) in raw {
        let Some(i) = find(term).filter(|_| lowered != term && !lowered.contains(&RAW_SEP)) else { continue };
        let mut key = Vec::with_capacity(lowered.len() + 1 + term.len());
        key.extend_from_slice(lowered);
        key.push(RAW_SEP);
        key.extend_from_slice(term);
        raw_keys.push((key, ordinal[i] as u64));
    }
    raw_keys.sort_unstable();
    raw_keys.dedup();
    let raw = if raw_keys.is_empty() {
        None
    } else {
        let mut b = fst::MapBuilder::memory_sized(raw_keys.len());
        for (k, v) in &raw_keys {
            b.insert(k, *v).map_err(|e| invalid(e.to_string()))?;
        }
        Some(b.into_inner().map_err(|e| invalid(e.to_string()))?)
    };
    Ok(TextSections { terms: terms_fst.into_inner().map_err(|e| invalid(e.to_string()))?, raw, postings, freqs, norms })
}

/// A RAWTERMS key's lower-cased token and term.
pub(crate) fn split_raw(key: &[u8]) -> Option<(&[u8], &[u8])> {
    let sep = key.iter().position(|&b| b == RAW_SEP)?;
    Some((&key[..sep], &key[sep + 1..]))
}

/// One term of a field being built: its documents (ascending) and its frequency in each.
pub struct TermInput {
    pub term: Vec<u8>,
    pub docs: Vec<DocId>,
    pub freqs: Vec<u32>,
}

struct TermInputs<'a>(&'a [TermInput]);

impl TermSource for TermInputs<'_> {
    type Scratch = ();

    fn len(&self) -> usize {
        self.0.len()
    }

    fn term(&self, i: usize) -> &[u8] {
        &self.0[i].term
    }

    fn df(&self, i: usize) -> u32 {
        self.0[i].docs.len() as u32
    }

    fn postings(&self, i: usize, _: &mut (), docs: &mut Vec<DocId>, tfs: &mut Vec<u32>) {
        let t = &self.0[i];
        docs.clone_from(&t.docs);
        tfs.clone_from(&t.freqs);
    }
}

impl InvertedIndex {
    /// Build a field's sections. `terms` are sorted by term bytes and unique; `raw` holds each
    /// lower-cased token of the field with its term, in any order; `lengths[doc]` is the field
    /// length of each document of the segment in tokens.
    pub fn build(terms: &[TermInput], raw: &[(Vec<u8>, Vec<u8>)], lengths: &[u32]) -> io::Result<Self> {
        for t in terms {
            if t.docs.is_empty() || t.docs.windows(2).any(|w| w[0] >= w[1]) || t.freqs.len() != t.docs.len() {
                return Err(invalid(format!("term `{}` needs ascending documents, a frequency each", String::from_utf8_lossy(&t.term))));
            }
        }
        let pairs: Vec<(&[u8], &[u8])> = raw.iter().map(|(lowered, term)| (lowered.as_slice(), term.as_slice())).collect();
        let s = encode_text(&TermInputs(terms), lengths.len() as u32, &pairs, build_norms(lengths))?;
        Self::from_sections(
            Bytes::from_vec(s.terms),
            s.raw.map(Bytes::from_vec),
            Bytes::from_vec(s.postings),
            Bytes::from_vec(s.freqs),
            Bytes::from_vec(s.norms),
        )
    }
}

// -- merging ----------------------------------------------------------------------------------------

/// A field's terms merged from the sources' text indexes (`index::merge`): the union of their term
/// FSTs, each term with the documents it has left, its postings renumbered with their frequencies.
struct MergedTerms<'a> {
    /// (source, its text index) of the sources that index the field.
    srcs: &'a [(usize, &'a InvertedIndex)],
    docs: &'a DocMap,
    /// Term `i`: `terms[term_at[i]..term_at[i + 1]]`, in byte order.
    terms: Vec<u8>,
    term_at: Vec<usize>,
    df: Vec<u32>,
    /// Term `i`'s (place in `srcs`, term there): `refs[ref_at[i]..ref_at[i + 1]]`.
    refs: Vec<(u32, TermRef)>,
    ref_at: Vec<usize>,
}

/// A source posting's documents and frequencies, and the merged entries to put in order.
#[derive(Default)]
struct MergeScratch {
    docs: Vec<DocId>,
    tfs: Vec<u32>,
    entries: Vec<(DocId, u32)>,
}

impl MergedTerms<'_> {
    /// How many of the term's documents in source `j` are carried over.
    fn live_df(&self, j: usize, r: TermRef, scratch: &mut Vec<DocId>) -> u32 {
        let (s, idx) = self.srcs[j];
        if self.docs.whole(s) {
            return idx.df_of(r);
        }
        scratch.clear();
        idx.posting_docs(r, scratch);
        let map = self.docs.map(s);
        scratch.iter().filter(|&&d| map.get(d as usize).is_some_and(|&k| k != GONE)).count() as u32
    }
}

impl TermSource for MergedTerms<'_> {
    type Scratch = MergeScratch;

    fn len(&self) -> usize {
        self.df.len()
    }

    fn term(&self, i: usize) -> &[u8] {
        &self.terms[self.term_at[i]..self.term_at[i + 1]]
    }

    fn df(&self, i: usize) -> u32 {
        self.df[i]
    }

    fn postings(&self, i: usize, scratch: &mut MergeScratch, docs: &mut Vec<DocId>, tfs: &mut Vec<u32>) {
        docs.clear();
        tfs.clear();
        let MergeScratch { docs: src_docs, tfs: src_tfs, entries } = scratch;
        for &(j, r) in &self.refs[self.ref_at[i]..self.ref_at[i + 1]] {
            let (s, idx) = self.srcs[j as usize];
            let map = self.docs.map(s);
            src_docs.clear();
            src_tfs.clear();
            idx.posting_docs(r, src_docs);
            idx.term_tfs(r, src_tfs);
            for (&d, &tf) in src_docs.iter().zip(src_tfs.iter()) {
                let k = map.get(d as usize).copied().unwrap_or(GONE);
                if k != GONE {
                    docs.push(k);
                    tfs.push(tf);
                }
            }
        }
        if docs.is_sorted() {
            return;
        }
        // sources' documents interleave
        entries.clear();
        entries.extend(docs.iter().copied().zip(tfs.iter().copied()));
        entries.sort_unstable_by_key(|e| e.0);
        for (n, &(d, tf)) in entries.iter().enumerate() {
            docs[n] = d;
            tfs[n] = tf;
        }
    }
}

impl InvertedIndex {
    /// A field's text index merged from the sources' (`srcs`: (source, its index), the sources
    /// that index the field), the documents renumbered as `docs` says: every term with documents
    /// left, its postings and frequencies moved over and each document's length, encoded as a
    /// build encodes the same terms. None when no term has a document left.
    ///
    /// RAWTERMS comes from the sources': which token forms a term's documents hold is not recorded
    /// per document, so a form only deleted copies held stays until the term goes. It can only widen
    /// a prefix search to a term that still has documents.
    pub(crate) fn merge(srcs: &[(usize, &InvertedIndex)], docs: &DocMap) -> io::Result<Option<Self>> {
        let mut m = MergedTerms { srcs, docs, terms: Vec::new(), term_at: vec![0], df: Vec::new(), refs: Vec::new(), ref_at: vec![0] };
        let mut op = fst::map::OpBuilder::new();
        for (_, idx) in srcs {
            op = op.add(&idx.terms);
        }
        let mut union = op.union();
        let mut group: Vec<(u32, TermRef)> = Vec::new();
        let mut scratch = Vec::new();
        while let Some((term, ivs)) = union.next() {
            group.clear();
            group.extend(ivs.iter().map(|iv| (iv.index as u32, TermRef(iv.value))));
            group.sort_unstable_by_key(|g| g.0);
            let mut df = 0u32;
            for &(j, r) in &group {
                let n = m.live_df(j as usize, r, &mut scratch);
                if n > 0 {
                    df += n;
                    m.refs.push((j, r));
                }
            }
            if df == 0 {
                m.refs.truncate(*m.ref_at.last().unwrap());
                continue;
            }
            m.terms.extend_from_slice(term);
            m.term_at.push(m.terms.len());
            m.df.push(df);
            m.ref_at.push(m.refs.len());
        }
        if m.df.is_empty() {
            return Ok(None);
        }
        let mut raw_keys: Vec<Vec<u8>> = Vec::new();
        for (_, idx) in srcs {
            let Some(r) = &idx.raw else { continue };
            let mut stream = r.stream();
            while let Some((key, _)) = stream.next() {
                raw_keys.push(key.to_vec());
            }
        }
        let pairs: Vec<(&[u8], &[u8])> = raw_keys.iter().filter_map(|key| split_raw(key)).collect();
        let mut norms = vec![0u8; docs.len() as usize];
        for (k, &(s, d)) in docs.order().iter().enumerate() {
            if let Some((_, idx)) = srcs.iter().find(|(source, _)| *source == s as usize) {
                norms[k] = idx.norm_of(d);
            }
        }
        let s = encode_text(&m, docs.len(), &pairs, encode_norms(&norms))?;
        Self::from_sections(Bytes::from_vec(s.terms), s.raw.map(Bytes::from_vec), Bytes::from_vec(s.postings), Bytes::from_vec(s.freqs), Bytes::from_vec(s.norms)).map(Some)
    }
}

// -- reading ----------------------------------------------------------------------------------------

/// A text field's index over its sections.
#[derive(Clone)]
pub struct InvertedIndex {
    terms: fst::Map<Bytes>,
    raw: Option<fst::Map<Bytes>>,
    terms_bytes: Bytes,
    raw_bytes: Option<Bytes>,
    postings: Bytes,
    freqs: Bytes,
    /// Packed frequencies (FREQS after its tables).
    bits: Bytes,
    norms: Bytes,
    /// Packed norms (NORMS after its header and presence bitmap).
    norm_bits: Bytes,
    /// Documents with tokens, when the norms are sparse.
    norm_presence: Option<Bitmap>,
    norm_width: u32,
    norm_min: u8,
    /// Doc ids of the single-document terms, bit-packed at `doc_width`.
    single_docs: Bytes,
    doc_width: u32,
    n_postings: usize,
    n_singles: usize,
    /// The field's tokens, as the norms count them, over all of the segment's documents.
    pub total_length: u64,
    /// Documents with tokens.
    pub docs_with_tokens: u32,
    /// Documents in the segment.
    pub total_docs: u32,
    /// A name-matching field's keys (`crate::names`).
    keys: Option<crate::names::Keys>,
}

/// Where a FREQS group lives and how it is packed.
#[derive(Clone, Copy, Debug)]
struct Group {
    at: u64,
    entries: u64,
    width: u32,
}

impl InvertedIndex {
    /// The field's index in `file`, or None when the field has no TERMS section.
    pub fn open(file: &SegmentFile, field: FieldId) -> io::Result<Option<Self>> {
        let Some(terms) = file.section(kind::TERMS, field) else { return Ok(None) };
        let need = |k: u16, name: &str| file.section(k, field).ok_or_else(|| invalid(format!("field {field} has TERMS but no {name}")));
        let mut idx = Self::from_sections(
            terms,
            file.section(kind::RAWTERMS, field),
            need(kind::POSTINGS, "POSTINGS")?,
            need(kind::FREQS, "FREQS")?,
            need(kind::NORMS, "NORMS")?,
        )?;
        if let Some(keys) = file.section(kind::KEYS, field) {
            idx.keys = Some(crate::names::Keys::open(keys)?);
        }
        Ok(Some(idx))
    }

    fn from_sections(terms: Bytes, raw: Option<Bytes>, postings: Bytes, freqs: Bytes, norms: Bytes) -> io::Result<Self> {
        if postings.len() < 16 || freqs.len() < 16 || norms.len() < 24 {
            return Err(invalid("section shorter than its header"));
        }
        let (n, singles) = (postings.u64_at(0) as usize, postings.u64_at(8) as usize);
        let single_at = 16 + 8 * (n + 1) + 4 * n + 8;
        if single_at > postings.len() || postings.u64_at(16 + 8 * n) as usize > postings.len() {
            return Err(invalid("POSTINGS offsets past end"));
        }
        let doc_width = postings.u32_at(single_at - 8);
        let single_bytes = (singles as u64 * u64::from(doc_width)).div_ceil(8) as usize + 8;
        if doc_width > 32 || single_at + single_bytes > postings.len() {
            return Err(invalid("POSTINGS single doc ids past end"));
        }
        let single_docs = postings.slice(single_at, single_bytes);
        let table = 16 + 8 * (n + 2) + n + 1;
        let bits_at = table.next_multiple_of(8);
        if freqs.u64_at(0) as usize != n || freqs.u64_at(8) as usize != singles || bits_at + 8 > freqs.len() {
            return Err(invalid("FREQS does not match POSTINGS"));
        }
        let bits = freqs.slice(bits_at, freqs.len() - bits_at);
        if freqs.u64_at(16 + 8 * (n + 1)).div_ceil(8) as usize + 8 > bits.len() {
            return Err(invalid("FREQS groups past end"));
        }

        let total_length = norms.u64_at(0);
        let total_docs = norms.u32_at(8);
        let docs_with_tokens = norms.u32_at(12);
        let (norm_width, norm_min, sparse) = (u32::from(norms[16]), norms[17], norms[18] != 0);
        let (norm_presence, values_at, values) = if sparse {
            let len = norms.u32_at(24) as usize;
            if norms.len() < 32 + len {
                return Err(invalid("NORMS presence bitmap past end"));
            }
            let bm = Bitmap::from_frozen(norms.slice(32, len))?;
            let at = (32 + len).next_multiple_of(8);
            let count = bm.cardinality();
            // every document has tokens (sparse for its `min` only): a norm's position is the
            // document's, no lookup in the bitmap
            (if count == u64::from(total_docs) { None } else { Some(bm) }, at, count)
        } else {
            (None, 24, u64::from(total_docs))
        };
        if norm_width > 8 || values_at > norms.len() || (values * u64::from(norm_width)).div_ceil(8) as usize + 8 > norms.len() - values_at {
            return Err(invalid("NORMS values past end"));
        }
        let idx = Self {
            terms: fst::Map::new(terms.clone()).map_err(|e| invalid(e.to_string()))?,
            raw: raw.clone().map(fst::Map::new).transpose().map_err(|e| invalid(e.to_string()))?,
            terms_bytes: terms,
            raw_bytes: raw,
            postings,
            freqs,
            bits,
            norm_bits: norms.slice(values_at, norms.len() - values_at),
            norms,
            norm_presence,
            norm_width,
            norm_min,
            single_docs,
            doc_width,
            n_postings: n,
            n_singles: singles,
            total_length,
            docs_with_tokens,
            total_docs,
            keys: None,
        };
        for g in 0..=n {
            idx.group(g)?;
        }
        Ok(idx)
    }

    /// FREQS group `g`, checked against its bit range.
    fn group(&self, g: usize) -> io::Result<Group> {
        let start = self.freqs.u64_at(16 + 8 * g);
        let end = self.freqs.u64_at(24 + 8 * g);
        let width = u32::from(self.freqs[16 + 8 * (self.n_postings + 2) + g]);
        let entries = if g < self.n_postings { u64::from(self.postings.u32_at(16 + 8 * (self.n_postings + 1) + 4 * g)) } else { self.n_singles as u64 };
        if width > 32 || start > end || start + entries * u64::from(width) > end {
            return Err(invalid(format!("FREQS group {g} does not fit its bits")));
        }
        Ok(Group { at: start, entries, width })
    }

    /// Add this field's sections to a segment being written.
    pub fn add_sections(&self, field: FieldId, w: &mut SegmentWriter) {
        w.add_bytes(kind::TERMS, field, self.terms_bytes.clone());
        if let Some(raw) = &self.raw_bytes {
            w.add_bytes(kind::RAWTERMS, field, raw.clone());
        }
        w.add_bytes(kind::POSTINGS, field, self.postings.clone());
        w.add_bytes(kind::FREQS, field, self.freqs.clone());
        w.add_bytes(kind::NORMS, field, self.norms.clone());
        if let Some(keys) = &self.keys {
            w.add_bytes(kind::KEYS, field, keys.bytes().clone());
        }
    }

    /// A name-matching field's keys: computed from its terms (`crate::names`).
    pub fn add_keys(&mut self) -> io::Result<()> {
        let bytes = crate::names::Keys::encode(&self.terms)?;
        self.keys = Some(crate::names::Keys::open(Bytes::from_vec(bytes))?);
        Ok(())
    }

    /// The keys of a name-matching field (None: not one).
    pub fn keys(&self) -> Option<&crate::names::Keys> {
        self.keys.as_ref()
    }

    /// Bytes of all sections (heap before the segment is persisted, mapped after).
    pub fn section_bytes(&self) -> usize {
        self.terms_bytes.len()
            + self.raw_bytes.as_ref().map_or(0, |b| b.len())
            + self.postings.len()
            + self.freqs.len()
            + self.norms.len()
            + self.keys.as_ref().map_or(0, |k| k.bytes().len())
    }

    pub fn term_count(&self) -> usize {
        self.terms.len()
    }

    #[inline]
    pub fn lookup(&self, term: &[u8]) -> Option<TermRef> {
        self.terms.get(term).map(TermRef)
    }

    /// Byte range of posting `p`'s bitmap, None when the section is inconsistent.
    fn posting_range(&self, p: usize) -> Option<(usize, usize)> {
        let (start, end) = (self.postings.u64_at(16 + 8 * p) as usize, self.postings.u64_at(24 + 8 * p) as usize);
        (start <= end && end <= self.postings.len()).then_some((start, end))
    }

    /// Doc id of single-document term `s` (ordinal `n_postings + s`).
    #[inline]
    fn single_doc(&self, s: usize) -> DocId {
        read_bits(&self.single_docs, s as u64 * u64::from(self.doc_width), self.doc_width)
    }

    /// Number of documents containing the term.
    #[inline]
    pub fn df_of(&self, r: TermRef) -> u32 {
        let o = r.ordinal();
        if o < self.n_postings {
            self.postings.u32_at(16 + 8 * (self.n_postings + 1) + 4 * o)
        } else {
            u32::from(o < self.n_postings + self.n_singles)
        }
    }

    /// The term's documents.
    pub fn postings_of(&self, r: TermRef) -> Bitmap {
        let o = r.ordinal();
        if o >= self.n_postings {
            let s = o - self.n_postings;
            if s < self.n_singles && self.single_doc(s) < self.total_docs {
                return Bitmap::of(&[self.single_doc(s)]);
            }
            tracing::error!(ordinal = o, "text index: term ordinal out of range");
            return Bitmap::new();
        }
        let Some((start, end)) = self.posting_range(o) else {
            tracing::error!(ordinal = o, "text index: posting offsets out of range");
            return Bitmap::new();
        };
        Bitmap::from_frozen(self.postings.slice(start, end - start)).unwrap_or_else(|e| {
            tracing::error!("text index: {e}");
            Bitmap::new()
        })
    }

    /// The term's documents and frequencies.
    pub fn entry_of(&self, r: TermRef) -> TermEntry {
        let bm = self.postings_of(r);
        let o = r.ordinal();
        let (g, rank) = if o < self.n_postings { (o, 0) } else { (self.n_postings, (o - self.n_postings) as u64) };
        let Ok(group) = self.group(g) else {
            tracing::error!(ordinal = o, "text index: frequencies do not match postings");
            return TermEntry { bm, freqs: Freqs::One(1) };
        };
        if o >= self.n_postings {
            return TermEntry { bm, freqs: Freqs::One(1 + read_bits(&self.bits, group.at + rank * u64::from(group.width), group.width)) };
        }
        TermEntry { bm, freqs: Freqs::Packed { bits: self.bits.clone(), start: group.at, width: group.width } }
    }

    #[inline]
    pub fn postings(&self, term: &[u8]) -> Option<Bitmap> {
        self.lookup(term).map(|r| self.postings_of(r))
    }

    #[inline]
    pub fn term_entry(&self, term: &[u8]) -> Option<TermEntry> {
        self.lookup(term).map(|r| self.entry_of(r))
    }

    pub fn term_freq(&self, term: &[u8], doc: DocId) -> u32 {
        self.term_entry(term).map_or(0, |e| e.tf(doc))
    }

    pub fn doc_freq(&self, term: &[u8]) -> u32 {
        self.lookup(term).map_or(0, |r| self.df_of(r))
    }

    /// The one-byte norm of `doc`'s field length (0: no tokens).
    #[inline]
    fn norm_of(&self, doc: DocId) -> u8 {
        let i = match &self.norm_presence {
            Some(p) if p.contains(doc) => p.rank(doc) - 1,
            Some(_) => return 0,
            None if doc < self.total_docs => u64::from(doc),
            None => return 0,
        };
        let norm = u32::from(self.norm_min) + read_bits(&self.norm_bits, i * u64::from(self.norm_width), self.norm_width);
        norm.min(255) as u8
    }

    /// Field length of `doc` in tokens (exact below 24, 4 significant bits above).
    #[inline]
    pub fn doc_length(&self, doc: DocId) -> u32 {
        norm_table()[self.norm_of(doc) as usize]
    }

    /// Append the term's documents, ascending, read in place (a merge's: `MergedTerms`).
    fn posting_docs(&self, r: TermRef, out: &mut Vec<DocId>) {
        let o = r.ordinal();
        if o >= self.n_postings {
            if o < self.n_postings + self.n_singles {
                out.push(self.single_doc(o - self.n_postings));
            }
            return;
        }
        let Some((start, end)) = self.posting_range(o) else { return };
        if let Err(e) = crate::bitmap::frozen::decode_into(&self.postings[start..end], out) {
            tracing::error!("text index: {e}");
        }
    }

    /// The term's frequency in each document of its posting, appended (a merge's: `MergedTerms`).
    fn term_tfs(&self, r: TermRef, out: &mut Vec<u32>) {
        let o = r.ordinal();
        let g = o.min(self.n_postings);
        let Ok(group) = self.group(g) else { return };
        if o >= self.n_postings {
            let rank = (o - self.n_postings) as u64;
            out.push(1 + read_bits(&self.bits, group.at + rank * u64::from(group.width), group.width));
            return;
        }
        let at = out.len();
        packed::unpack(&self.bits, group.at, group.width, group.entries as usize, out);
        out[at..].iter_mut().for_each(|tf| *tf += 1);
    }

    /// Visit the terms starting with `prefix`, in byte order, until `f` returns false.
    pub fn for_each_with_prefix(&self, prefix: &[u8], mut f: impl FnMut(TermRef) -> bool) {
        let mut stream = self.terms.range().ge(prefix).into_stream();
        while let Some((k, v)) = stream.next() {
            if !k.starts_with(prefix) || !f(TermRef(v)) {
                break;
            }
        }
    }

    /// Visit every term in byte order until `f` returns false.
    pub fn for_each_term(&self, mut f: impl FnMut(&[u8], TermRef) -> bool) {
        let mut stream = self.terms.stream();
        while let Some((k, v)) = stream.next() {
            if !f(k, TermRef(v)) {
                break;
            }
        }
    }

    /// The terms of the tokens starting with `prefix` (a lower-cased token, accents kept: "trä"
    /// finds "trädgård" but not "transport"; plain letters also find folded and stemmed terms), the
    /// `limit` most frequent first (ties in term order). Best-first walks over TERMS and RAWTERMS,
    /// merged by ordinal.
    pub fn prefix(&self, prefix: &[u8], limit: usize) -> Vec<(Vec<u8>, TermRef)> {
        let mut terms = BestFirst::new(self.terms.as_fst(), prefix).map(|(k, v)| (v as usize, k)).peekable();
        let mut raws = self
            .raw
            .as_ref()
            .map(|raw| BestFirst::new(raw.as_fst(), prefix))
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((v as usize, split_raw(&k)?.1.to_vec())))
            .peekable();
        let mut out: Vec<(Vec<u8>, TermRef)> = Vec::with_capacity(limit.min(64));
        let mut seen: Vec<usize> = Vec::with_capacity(limit.min(64));
        while out.len() < limit {
            let from_terms = match (terms.peek(), raws.peek()) {
                (Some(a), Some(b)) => a.0 <= b.0,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            let (o, term) = if from_terms { terms.next() } else { raws.next() }.unwrap();
            if seen.contains(&o) {
                continue;
            }
            seen.push(o);
            out.push((term, TermRef(o as u64)));
        }
        out
    }

    /// Terms the automaton accepts: (term, edit distance, its ref), in byte order.
    pub fn fuzzy(&self, dfa: &DFA) -> Vec<(Vec<u8>, u8, TermRef)> {
        let mut out = Vec::new();
        let mut stream = self.terms.search_with_state(dfa).into_stream();
        while let Some((k, v, state)) = stream.next() {
            if let Distance::Exact(d) = dfa.distance(state) {
                out.push((k.to_vec(), d, TermRef(v)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(term: &str, docs: &[(u32, u32)]) -> TermInput {
        TermInput { term: term.as_bytes().to_vec(), docs: docs.iter().map(|d| d.0).collect(), freqs: docs.iter().map(|d| d.1).collect() }
    }

    fn raw_pairs() -> Vec<(Vec<u8>, Vec<u8>)> {
        [("ab", "ab"), ("bilar", "bil"), ("trädgård", "tradgard"), ("transport", "transport"), ("unik", "unik"), ("unika", "unika"), ("vanlig", "vanlig")]
            .iter()
            .map(|(a, b)| (a.as_bytes().to_vec(), b.as_bytes().to_vec()))
            .collect()
    }

    fn sample(lengths: &[u32]) -> (InvertedIndex, Vec<TermInput>) {
        let many: Vec<(u32, u32)> = (0..70_000u32).filter(|d| d % 3 == 0).map(|d| (d, d % 5 + 1)).collect();
        let mut terms = vec![
            input("ab", &[(0, 1), (1, 1), (2, 1)]),
            input("bil", &[(3, 2), (4, 1)]),
            input("tradgard", &[(5, 1)]),
            input("transport", &[(1, 3), (5, 1), (7, 70)]),
            input("unik", &[(6, 9)]),
            input("unika", &[(8, 1)]),
            input("vanlig", &many),
        ];
        terms.sort_by(|a, b| a.term.cmp(&b.term));
        (InvertedIndex::build(&terms, &raw_pairs(), lengths).unwrap(), terms)
    }

    fn names(found: Vec<(Vec<u8>, TermRef)>) -> Vec<String> {
        found.into_iter().map(|(t, _)| String::from_utf8(t).unwrap()).collect()
    }

    fn check(idx: &InvertedIndex, terms: &[TermInput], lengths: &[u32]) {
        for t in terms {
            let name = String::from_utf8_lossy(&t.term);
            let e = idx.term_entry(&t.term).unwrap();
            assert_eq!(e.bitmap().to_vec(), t.docs, "{name}");
            assert_eq!(idx.doc_freq(&t.term) as usize, t.docs.len());
            let mut cursor = e.cursor();
            for (i, &d) in t.docs.iter().enumerate() {
                assert_eq!(e.tf(d), t.freqs[i], "{name} doc {d}");
                assert_eq!(cursor.tf(d), t.freqs[i], "{name} doc {d}");
                assert_eq!(idx.term_freq(&t.term, d), t.freqs[i]);
            }
            assert_eq!(e.tf(69_998), 0);
        }
        assert!(idx.postings(b"saknas").is_none());
        for (d, &len) in lengths.iter().enumerate() {
            assert_eq!(idx.doc_length(d as u32), norm_to_length(length_to_norm(len)), "doc {d}");
        }
        assert_eq!(idx.doc_length(lengths.len() as u32 + 5), 0);
        assert_eq!(idx.total_length, lengths.iter().map(|&l| u64::from(norm_to_length(length_to_norm(l)))).sum::<u64>());
        assert_eq!(u64::from(idx.docs_with_tokens), lengths.iter().filter(|&&l| l > 0).count() as u64);
        // prefix: accent kept, stemmed tokens map to their terms, most frequent first
        assert_eq!(names(idx.prefix("trä".as_bytes(), 5)), ["tradgard"]);
        assert_eq!(names(idx.prefix(b"tra", 5)), ["transport", "tradgard"]);
        assert_eq!(names(idx.prefix(b"bila", 5)), ["bil"], "through the token \"bilar\"");
        assert_eq!(names(idx.prefix(b"bil", 5)), ["bil"]);
        assert_eq!(names(idx.prefix(b"uni", 5)), ["unik", "unika"]);
        assert_eq!(names(idx.prefix(b"", 4)), ["vanlig", "ab", "transport", "bil"]);
        // typo
        let fuzzy = idx.fuzzy(&typo_dfa("transprot", 2));
        assert_eq!(fuzzy.iter().map(|f| (f.0.as_slice(), f.1)).collect::<Vec<_>>(), [(&b"transport"[..], 1)]);
        let mut n = 0;
        idx.for_each_term(|_, _| {
            n += 1;
            true
        });
        assert_eq!(n, terms.len());
    }

    fn round_trip(lengths: Vec<u32>) {
        let (idx, terms) = sample(&lengths);
        check(&idx, &terms, &lengths);
        let mut w = SegmentWriter::new();
        idx.add_sections(7, &mut w);
        let file = SegmentFile::from_vec(w.finish(lengths.len() as u32, 0)).unwrap();
        assert!(file.section(kind::TERMS, crate::tqbf::container::SEGMENT_WIDE).is_none());
        let reopened = InvertedIndex::open(&file, 7).unwrap().unwrap();
        assert_eq!(reopened.section_bytes(), idx.section_bytes());
        check(&reopened, &terms, &lengths);
        assert!(InvertedIndex::open(&file, 8).unwrap().is_none());
    }

    #[test]
    fn build_read_and_persist_dense_norms() {
        round_trip((0..70_000u32).map(|d| d % 300).collect());
    }

    #[test]
    fn build_read_and_persist_sparse_norms() {
        let lengths: Vec<u32> = (0..70_000u32).map(|d| if d % 3 == 0 || d < 10 { 3 + d % 4 } else { 0 }).collect();
        let (idx, _) = sample(&lengths);
        assert!(idx.norm_presence.is_some(), "a third of the docs have tokens: sparse");
        assert_eq!(idx.norm_width, 2, "lengths 3..=6 above min 3");
        round_trip(lengths);
    }

    #[test]
    fn unsorted_documents_are_refused() {
        let t = input("x", &[(3, 1), (2, 1)]);
        assert!(InvertedIndex::build(&[t], &[], &[0; 10]).is_err());
    }

    #[test]
    fn best_first_yields_by_output() {
        let mut b = fst::MapBuilder::memory();
        let keys: Vec<(String, u64)> = (0..5000u64).map(|i| (format!("k{:04}", i), (i * 7919) % 5000)).collect();
        for (k, v) in &keys {
            b.insert(k, *v).unwrap();
        }
        let map = fst::Map::new(Bytes::from_vec(b.into_inner().unwrap())).unwrap();
        let mut expected: Vec<(u64, String)> = keys.iter().filter(|(k, _)| k.starts_with("k1")).map(|(k, v)| (*v, k.clone())).collect();
        expected.sort();
        let got: Vec<(u64, String)> = BestFirst::new(map.as_fst(), b"k1").map(|(k, v)| (v, String::from_utf8(k).unwrap())).collect();
        assert_eq!(got, expected);
        assert_eq!(BestFirst::new(map.as_fst(), b"x").count(), 0);
    }

    #[test]
    fn norms_round_trip_like_lucene() {
        for len in 0..NUM_FREE_VALUES + 8 {
            assert_eq!(norm_to_length(length_to_norm(len)), len);
        }
        let mut prev = 0;
        for len in [40, 41, 100, 1000, 65_535, 1 << 20, i32::MAX as u32] {
            let back = norm_to_length(length_to_norm(len));
            assert!(back <= len && back >= prev, "{len} -> {back}");
            assert!(len - back <= len / 8 + 1, "{len} -> {back}");
            prev = back;
        }
        assert_eq!(length_to_norm(u32::MAX), 255, "saturates like Lucene's int range");
    }

    /// Optimal string alignment distance, the metric SymSpell used.
    fn osa(a: &str, b: &str) -> usize {
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for (i, row) in d.iter_mut().enumerate() {
            row[0] = i;
        }
        for j in 0..=b.len() {
            d[0][j] = j;
        }
        for i in 1..=a.len() {
            for j in 1..=b.len() {
                let cost = usize::from(a[i - 1] != b[j - 1]);
                d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
                if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                    d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
                }
            }
        }
        d[a.len()][b.len()]
    }

    #[test]
    fn automaton_distance_is_osa() {
        let alphabet = ['a', 'b', 'c', 'å', 'ö'];
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let word = |s: &mut u64| -> String {
            *s ^= *s << 13;
            *s ^= *s >> 7;
            *s ^= *s << 17;
            let len = (*s % 7) as usize;
            (0..len).map(|i| alphabet[((*s >> (8 + 3 * i)) % 5) as usize]).collect()
        };
        for _ in 0..3000 {
            let (q, t) = (word(&mut s), word(&mut s));
            let dfa = typo_dfa(&q, 2);
            let expected = osa(&q, &t);
            match dfa.eval(&t) {
                Distance::Exact(d) => assert_eq!(usize::from(d), expected, "{q} vs {t}"),
                Distance::AtLeast(_) => assert!(expected > 2, "{q} vs {t}: {expected}"),
            }
        }
    }
}
