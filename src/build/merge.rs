//! `finish`: the frozen pieces, in (run key, piece) order, merged into one segment file. Doc ids
//! are the pieces' local ids offset by the documents of the pieces before. Every field merges on
//! its own (fields run in parallel, in windows written in field order); terms and values merge
//! k-way, so postings are concatenations in doc order. The sections come from the import's
//! encoders, so they equal an import's byte for byte.

use super::run::{FrozenOrds, FrozenRun, FrozenText, FrozenValues, NONE};
use super::{invalid, BuildReport, Shared, ValuePlan, MAX_SAMPLE_BYTES};
use crate::index::geo::GeoIndex;
use crate::index::inverted::{build_norms, encode_text, split_raw, TermSource};
use crate::index::values::{numbers_dict, strings_dict, BoolIndex, DocOrds, Ords, ValueDocs};
use crate::schema::DocId;
use crate::tqbf::container::{header_flags, kind, StreamingSegmentWriter, SEGMENT_WIDE};
use crate::tqbf::record::Cursor;
use rayon::prelude::*;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;
use std::path::Path;
use std::time::Instant;

pub(crate) fn finish(shared: &Shared, path: &Path) -> io::Result<BuildReport> {
    let start = Instant::now();
    let mut pieces = std::mem::take(&mut *shared.pieces.lock().unwrap());
    pieces.sort_unstable_by_key(|p| (p.key, p.piece));
    shared.load_spilled(&mut pieces)?;
    let mut base = Vec::with_capacity(pieces.len());
    let mut total = 0u64;
    for p in &pieces {
        base.push(total as u32);
        total += u64::from(p.docs);
    }
    if total == 0 {
        return Err(invalid("no documents to build a segment from"));
    }
    if total >= u64::from(u32::MAX) {
        return Err(invalid("a segment holds fewer than 2^32 - 1 documents"));
    }
    let n_docs = total as u32;
    let frozen_bytes = shared.frozen_bytes.load(std::sync::atomic::Ordering::Relaxed);

    let run = || -> io::Result<u64> {
        let docs = docs_section(shared, &pieces, &base, n_docs)?;
        let ids = ids_section(&pieces, &base)?;
        let fields = shared.registry.lock().unwrap().encode();
        let schema_hash = xxhash_rust::xxh3::xxh3_64(&fields);

        let mut w = StreamingSegmentWriter::create(path)?;
        w.set_flag(header_flags::HAS_INDEX);
        w.add(kind::SCHEMA, SEGMENT_WIDE, &shared.schema_json)?;
        w.add(kind::FIELDS, SEGMENT_WIDE, &fields)?;
        w.add_parts(kind::DOCS, SEGMENT_WIDE, docs.iter().map(|p| p.as_ref()))?;
        drop(docs);
        w.add(kind::IDS, SEGMENT_WIDE, &ids)?;
        drop(ids);

        // Field sections in field order, fields merged in parallel a window at a time.
        let plan = &shared.plan;
        let n_fields = shared.schema.fields.len();
        let window = rayon::current_num_threads().max(1) * 2;
        for from in (0..n_fields).step_by(window) {
            let to = (from + window).min(n_fields);
            let out: Vec<io::Result<Vec<(u16, Vec<u8>)>>> = (from..to)
                .into_par_iter()
                .map(|field| {
                    let mut secs = Vec::new();
                    if let Some(t) = plan.text.iter().position(|p| p.field == field) {
                        secs.extend(text_sections(&pieces, &base, t, n_docs, shared.schema.fields[field].name_matching)?);
                    }
                    if let Some(v) = plan.values.iter().position(|p| p.field == field) {
                        secs.extend(value_sections(&pieces, &base, v, &plan.values[v], n_docs)?);
                    }
                    if let Some(b) = plan.bools.iter().position(|p| p.field == field) {
                        let (mut t, mut f) = (Vec::new(), Vec::new());
                        for (p, &b0) in pieces.iter().zip(&base) {
                            t.extend(p.bools[b].0.iter().map(|&d| b0 + d));
                            f.extend(p.bools[b].1.iter().map(|&d| b0 + d));
                        }
                        if !t.is_empty() || !f.is_empty() || plan.bools[b].sort {
                            secs.push((kind::BOOL, BoolIndex::build(&t, &f)?.section().to_vec()));
                        }
                    }
                    if let Some(g) = plan.geos.iter().position(|p| p.field == field) {
                        let mut pts: Vec<(DocId, i32, i32)> = Vec::new();
                        for (p, &b0) in pieces.iter().zip(&base) {
                            pts.extend(p.geos[g].chunks_exact(3).map(|t| (b0 + t[0], t[1] as i32, t[2] as i32)));
                        }
                        if !pts.is_empty() {
                            secs.push((kind::GEO, GeoIndex::build(&pts, n_docs, plan.geos[g].multi)?.section().to_vec()));
                        }
                    }
                    Ok(secs)
                })
                .collect();
            for (field, secs) in (from..to).zip(out) {
                for (k, bytes) in secs? {
                    w.add(k, field as u16, &bytes)?;
                }
            }
        }
        w.finish(n_docs, schema_hash)
    };
    let bytes = if shared.opts.threads > 0 {
        rayon::ThreadPoolBuilder::new().num_threads(shared.opts.threads).build().map_err(io::Error::other)?.install(run)?
    } else {
        run()?
    };
    let n_pieces = pieces.len();
    // The pieces may map the spill file; it is removed with the builder once they are gone.
    drop(pieces);
    Ok(BuildReport {
        docs: n_docs,
        pieces: n_pieces,
        rejected: shared.rejected.lock().unwrap().clone(),
        bytes,
        frozen_bytes,
        merge_millis: start.elapsed().as_millis(),
    })
}

// -- stored documents ------------------------------------------------------------------------------

/// The records of a raw block (u32 n, u32 offsets[n + 1], records).
fn block_records(block: &[u8]) -> impl Iterator<Item = &[u8]> {
    let n = u32::from_le_bytes(block[..4].try_into().unwrap()) as usize;
    let off = |i: usize| u32::from_le_bytes(block[4 + 4 * i..8 + 4 * i].try_into().unwrap()) as usize;
    let body = 4 + 4 * (n + 1);
    (0..n).map(move |i| &block[body + off(i)..body + off(i + 1)])
}

/// A piece's raw blocks.
fn raw_blocks(p: &FrozenRun) -> impl Iterator<Item = &[u8]> {
    let b = &p.blocks;
    (0..b.raw_ends.len()).map(move |i| {
        let start = if i == 0 { 0 } else { b.raw_ends[i - 1] as usize };
        &b.raw[start..b.raw_ends[i] as usize]
    })
}

/// A part of the DOCS section: bytes built here, or a piece's frames.
enum Part<'a> {
    Owned(Vec<u8>),
    Frames(&'a [u8]),
}

impl AsRef<[u8]> for Part<'_> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Part::Owned(v) => v,
            Part::Frames(f) => f,
        }
    }
}

/// The DOCS section as parts: header and tables, the dictionary, every block in doc order.
/// Blocks still raw (their pieces ended before the dictionary existed) are compressed here.
fn docs_section<'a>(shared: &Shared, pieces: &'a [FrozenRun], base: &[u32], n_docs: u32) -> io::Result<Vec<Part<'a>>> {
    if shared.dictionary().is_none() {
        // Nothing is compressed yet: train on the first records in doc id order.
        let mut sample = Vec::new();
        let mut bytes = 0usize;
        'outer: for p in pieces {
            for block in raw_blocks(p) {
                for r in block_records(block) {
                    if bytes >= MAX_SAMPLE_BYTES {
                        break 'outer;
                    }
                    bytes += r.len();
                    sample.push(r.to_vec());
                }
            }
        }
        shared.train_dictionary(&sample);
    }
    let dict = shared.dictionary().unwrap_or_default();
    let level = shared.opts.docs_level;
    let late: Vec<Vec<Vec<u8>>> = pieces
        .par_iter()
        .map(|p| {
            if p.blocks.raw_ends.is_empty() {
                return Ok(Vec::new());
            }
            let mut comp = if dict.is_empty() { zstd::bulk::Compressor::new(level)? } else { zstd::bulk::Compressor::with_dictionary(level, &dict)? };
            raw_blocks(p).map(|b| comp.compress(b)).collect::<io::Result<Vec<_>>>()
        })
        .collect::<io::Result<_>>()?;

    let n_blocks: usize = pieces.iter().map(|p| p.blocks.firsts.len()).sum();
    let mut head = Vec::with_capacity(16 + 12 * (n_blocks + 1));
    head.extend_from_slice(&(n_blocks as u32).to_le_bytes());
    head.extend_from_slice(&(dict.len() as u32).to_le_bytes());
    for (p, &b0) in pieces.iter().zip(base) {
        for &f in p.blocks.firsts.iter() {
            head.extend_from_slice(&(b0 + f).to_le_bytes());
        }
    }
    head.extend_from_slice(&n_docs.to_le_bytes());
    while head.len() % 8 != 0 {
        head.push(0);
    }
    let mut off = 0u64;
    head.extend_from_slice(&off.to_le_bytes());
    let mut parts = Vec::with_capacity(2 + 2 * pieces.len());
    for (p, late) in pieces.iter().zip(&late) {
        let mut prev = 0u64;
        for &end in p.blocks.frame_ends.iter() {
            off += end - prev;
            prev = end;
            head.extend_from_slice(&off.to_le_bytes());
        }
        for f in late {
            off += f.len() as u64;
            head.extend_from_slice(&off.to_le_bytes());
        }
    }
    parts.push(Part::Owned(head));
    parts.push(Part::Owned(dict.to_vec()));
    for (p, late) in pieces.iter().zip(late) {
        parts.push(Part::Frames(&p.blocks.frames));
        parts.extend(late.into_iter().map(Part::Owned));
    }
    Ok(parts)
}

// -- ids -------------------------------------------------------------------------------------------

/// The IDS section: every external id to its doc id, k-way merged from the pieces' sorted ids.
fn ids_section(pieces: &[FrozenRun], base: &[u32]) -> io::Result<Vec<u8>> {
    let mut heap: BinaryHeap<Reverse<(&[u8], usize, usize)>> = BinaryHeap::with_capacity(pieces.len());
    for (k, p) in pieces.iter().enumerate() {
        if !p.id_docs.is_empty() {
            heap.push(Reverse((p.id(0), k, 0)));
        }
    }
    let mut b = fst::MapBuilder::memory_sized(pieces.iter().map(|p| p.id_docs.len()).sum());
    let mut last: Option<&[u8]> = None;
    while let Some(Reverse((id, k, i))) = heap.pop() {
        if last == Some(id) {
            return Err(invalid(format!("document id `{}` is added twice", String::from_utf8_lossy(id))));
        }
        last = Some(id);
        b.insert(id, u64::from(base[k] + pieces[k].id_docs[i])).map_err(io::Error::other)?;
        if i + 1 < pieces[k].id_docs.len() {
            heap.push(Reverse((pieces[k].id(i + 1), k, i + 1)));
        }
    }
    b.into_inner().map_err(io::Error::other)
}

// -- merged dictionaries ---------------------------------------------------------------------------

/// Distinct keys merged from the pieces: per merged key the (piece, local index) entries in piece
/// order, and per piece the merged index of each local key.
struct Merged {
    first: Vec<(u32, u32)>,
    refs_at: Vec<u32>,
    refs: Vec<(u32, u32)>,
    local: Vec<Vec<u32>>,
}

impl Merged {
    fn len(&self) -> usize {
        self.first.len()
    }

    fn refs(&self, m: usize) -> &[(u32, u32)] {
        &self.refs[self.refs_at[m] as usize..self.refs_at[m + 1] as usize]
    }
}

/// K-way merge of sorted, unique key lists (`len(piece)`, `key(piece, i)`).
fn merge_keys<K: Ord + Copy>(n_pieces: usize, len: impl Fn(usize) -> usize, key: impl Fn(usize, usize) -> K) -> Merged {
    let mut heap = BinaryHeap::with_capacity(n_pieces);
    let mut local: Vec<Vec<u32>> = Vec::with_capacity(n_pieces);
    let mut total = 0usize;
    for k in 0..n_pieces {
        let n = len(k);
        total += n;
        local.push(vec![NONE; n]);
        if n > 0 {
            heap.push(Reverse((key(k, 0), k, 0usize)));
        }
    }
    let mut m = Merged { first: Vec::new(), refs_at: Vec::new(), refs: Vec::with_capacity(total), local };
    let mut last: Option<K> = None;
    while let Some(Reverse((kv, k, i))) = heap.pop() {
        if last != Some(kv) {
            m.refs_at.push(m.refs.len() as u32);
            m.first.push((k as u32, i as u32));
            last = Some(kv);
        }
        m.refs.push((k as u32, i as u32));
        m.local[k][i] = (m.first.len() - 1) as u32;
        if i + 1 < len(k) {
            heap.push(Reverse((key(k, i + 1), k, i + 1)));
        }
    }
    m.refs_at.push(m.refs.len() as u32);
    m
}

// -- text ------------------------------------------------------------------------------------------

struct MergedText<'a> {
    parts: Vec<&'a FrozenText>,
    base: &'a [u32],
    m: Merged,
    df: Vec<u32>,
}

impl TermSource for MergedText<'_> {
    type Scratch = ();

    fn len(&self) -> usize {
        self.m.len()
    }

    fn term(&self, i: usize) -> &[u8] {
        let (k, l) = self.m.first[i];
        self.parts[k as usize].term(l as usize)
    }

    fn df(&self, i: usize) -> u32 {
        self.df[i]
    }

    fn postings(&self, i: usize, _: &mut (), docs: &mut Vec<DocId>, tfs: &mut Vec<u32>) {
        docs.clear();
        tfs.clear();
        for &(k, l) in self.m.refs(i) {
            self.parts[k as usize].postings_into(l as usize, self.base[k as usize], docs, tfs);
        }
    }
}

fn text_sections(pieces: &[FrozenRun], base: &[u32], t: usize, n_docs: u32, names: bool) -> io::Result<Vec<(u16, Vec<u8>)>> {
    let parts: Vec<&FrozenText> = pieces.iter().map(|p| &p.text[t]).collect();
    let m = merge_keys(parts.len(), |k| parts[k].len(), |k, i| parts[k].term(i));
    if m.len() == 0 {
        return Ok(Vec::new());
    }
    let mut df = vec![0u32; m.len()];
    for (i, d) in df.iter_mut().enumerate() {
        *d = m.refs(i).iter().map(|&(k, l)| parts[k as usize].df[l as usize]).sum();
    }

    let raw_m = merge_keys(parts.len(), |k| parts[k].raw_len(), |k, i| parts[k].raw_key(i));
    let raw: Vec<(&[u8], &[u8])> = raw_m.first.iter().filter_map(|&(k, i)| split_raw(parts[k as usize].raw_key(i as usize))).collect();

    let mut lengths = vec![0u32; n_docs as usize];
    for (p, &b0) in parts.iter().zip(base) {
        let mut c = Cursor::new(&p.lens);
        let mut doc = 0u32;
        let mut first = true;
        while c.remaining() > 0 {
            let gap = c.varint().map_err(|e| invalid(e.to_string()))? as u32;
            doc = if first { gap } else { doc + gap };
            first = false;
            lengths[(b0 + doc) as usize] = c.varint().map_err(|e| invalid(e.to_string()))? as u32;
        }
    }
    let norms = build_norms(&lengths);
    drop(lengths);

    let src = MergedText { parts: parts.clone(), base, m, df };
    let s = encode_text(&src, n_docs, &raw, norms)?;
    let keys = if names {
        let terms = fst::Map::new(s.terms.as_slice()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        Some(crate::names::Keys::encode(&terms)?)
    } else {
        None
    };
    let mut secs = vec![(kind::TERMS, s.terms)];
    secs.extend(s.raw.map(|r| (kind::RAWTERMS, r)));
    secs.push((kind::POSTINGS, s.postings));
    secs.push((kind::FREQS, s.freqs));
    secs.push((kind::NORMS, s.norms));
    secs.extend(keys.map(|k| (kind::KEYS, k)));
    Ok(secs)
}

// -- values ----------------------------------------------------------------------------------------

fn value_sections(pieces: &[FrozenRun], base: &[u32], v: usize, plan: &ValuePlan, n_docs: u32) -> io::Result<Vec<(u16, Vec<u8>)>> {
    let parts: Vec<&FrozenValues> = pieces.iter().map(|p| &p.values[v]).collect();
    let m = if plan.strings {
        merge_keys(parts.len(), |k| parts[k].len(), |k, i| parts[k].str_at(i))
    } else {
        merge_keys(parts.len(), |k| parts[k].len(), |k, i| parts[k].num_at(i))
    };
    let n = m.len();
    if n == 0 && plan.ords == Ords::None {
        return Ok(Vec::new());
    }
    let (dict_kind, dict) = if plan.strings {
        (kind::VALUES, strings_dict(m.first.iter().map(|&(k, i)| parts[k as usize].str_at(i as usize)))?)
    } else {
        let keys: Vec<i64> = m.first.iter().map(|&(k, i)| parts[k as usize].num_at(i as usize)).collect();
        (kind::NUM_VALUES, numbers_dict(&keys, plan.float))
    };
    let docs = ValueDocs::write_with(n, |ord, docs| {
        docs.clear();
        for &(k, l) in m.refs(ord) {
            parts[k as usize].postings_into(l as usize, base[k as usize], docs);
        }
    });
    let remap = |k: usize, local: u64| if local as usize >= parts[k].len() { n as u64 } else { u64::from(m.local[k][local as usize]) };
    let ords = match plan.ords {
        Ords::None => None,
        Ords::Single => {
            let mut ords = Vec::with_capacity(n_docs as usize);
            for (k, (p, piece)) in parts.iter().zip(pieces).enumerate() {
                let FrozenOrds::Single { bits, width } = &p.ords else { return Err(invalid("single ordinals expected")) };
                for d in 0..u64::from(piece.docs) {
                    ords.push(remap(k, crate::tqbf::packed::read(bits, d * u64::from(*width), *width)));
                }
            }
            Some(DocOrds::write_single(&ords, n as u64))
        }
        Ords::Multi => {
            let mut present: Vec<DocId> = Vec::new();
            let mut offsets: Vec<u64> = Vec::with_capacity(n_docs as usize + 1);
            let mut elems: Vec<u64> = Vec::new();
            for (k, ((p, piece), &b0)) in parts.iter().zip(pieces).zip(base).enumerate() {
                let FrozenOrds::Multi { counts, bits, width } = &p.ords else { return Err(invalid("multi ordinals expected")) };
                let mut c = Cursor::new(counts);
                let mut pos = 0u64;
                for d in 0..piece.docs {
                    let count = c.varint().map_err(|e| invalid(e.to_string()))?;
                    offsets.push(elems.len() as u64);
                    if count == 0 {
                        continue;
                    }
                    present.push(b0 + d);
                    for _ in 1..count {
                        elems.push(remap(k, crate::tqbf::packed::read(bits, pos * u64::from(*width), *width)));
                        pos += 1;
                    }
                }
            }
            offsets.push(elems.len() as u64);
            Some(DocOrds::write_multi(&present, &offsets, elems.into_iter(), n as u64))
        }
    };
    let mut out = vec![(dict_kind, dict), (kind::VALUE_DOCS, docs)];
    if let Some(o) = ords {
        out.push((kind::DOC_ORDS, o));
    }
    Ok(out)
}
