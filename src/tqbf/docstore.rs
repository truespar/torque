//! DOCS section (spec §5.2): records in ~16 KB blocks, zstd-compressed with a dictionary trained
//! on the collection's own records. Only returned hits, GET by id and highlighting read it.
//!
//! ```text
//! u32 block_count B, u32 dict_len D
//! u32 first_doc[B + 1]            first doc id per block; last entry = doc count
//! pad to 8
//! u64 block_offset[B + 1]         relative to the blocks area; last entry = its end
//! u8  dict[D]
//! blocks                          zstd frames
//! raw block: u32 n, u32 record_offset[n + 1] (relative to the records), records
//! ```

use super::bytes::Bytes;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct DocsOptions {
    /// zstd level. Offline builds use 19 (709 B/doc on company records); server-side builds 3.
    pub level: i32,
    pub block_target: usize,
    pub dict_size: usize,
}

impl Default for DocsOptions {
    fn default() -> Self {
        Self { level: 3, block_target: 16 * 1024, dict_size: 16 * 1024 }
    }
}

const MAX_SAMPLE_BYTES: usize = 8 * 1024 * 1024;

/// Blocks of this many record bytes or more compress (and a merge decompresses them) in parallel;
/// fewer, on the calling thread: waking a pool costs more than the work.
pub const PARALLEL_BYTES: usize = 256 * 1024;

thread_local! {
    /// A thread's compression context and what it compresses with (level, the dictionary's hash,
    /// 0 for none): kept between builds that compress alike (a collection's segments share its
    /// dictionary), as creating one and loading its dictionary costs more than compressing a small
    /// segment's documents.
    static COMPRESSOR: std::cell::RefCell<Option<(i32, u64, zstd::bulk::Compressor<'static>)>> = const { std::cell::RefCell::new(None) };
    /// A thread's decompression context for blocks without a dictionary, likewise.
    static DECOMPRESSOR: std::cell::RefCell<Option<zstd::bulk::Decompressor<'static>>> = const { std::cell::RefCell::new(None) };
}

/// The key of a dictionary in `COMPRESSOR` (0: none).
fn dictionary_key(dict: &[u8]) -> u64 {
    if dict.is_empty() { 0 } else { xxhash_rust::xxh3::xxh3_64(dict).max(1) }
}

/// Compress a block on the thread's context, with `dict` (`key`: `dictionary_key`).
fn compress(level: i32, dict: &[u8], key: u64, block: &[u8]) -> io::Result<Vec<u8>> {
    COMPRESSOR.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().is_none_or(|(l, k, _)| *l != level || *k != key) {
            let comp = if dict.is_empty() { zstd::bulk::Compressor::new(level)? } else { zstd::bulk::Compressor::with_dictionary(level, dict)? };
            *c = Some((level, key, comp));
        }
        c.as_mut().expect("a compressor").2.compress(block)
    })
}

type DecoderDictionaries = FxHashMap<Box<[u8]>, std::sync::Weak<zstd::dict::DecoderDictionary<'static>>>;

/// Decompression dictionaries prepared, by their bytes: the segments compressed with a
/// collection's dictionary share one.
static DECODER_DICTIONARIES: std::sync::LazyLock<Mutex<DecoderDictionaries>> = std::sync::LazyLock::new(Default::default);

fn decoder_dictionary(bytes: &[u8]) -> Arc<zstd::dict::DecoderDictionary<'static>> {
    let mut dicts = DECODER_DICTIONARIES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(d) = dicts.get(bytes).and_then(std::sync::Weak::upgrade) {
        return d;
    }
    dicts.retain(|_, d| d.strong_count() > 0);
    let d = Arc::new(zstd::dict::DecoderDictionary::copy(bytes));
    dicts.insert(bytes.into(), Arc::downgrade(&d));
    d
}

/// Decompress a block without a dictionary on the thread's context.
fn decompress_plain(frame: &[u8], cap: usize) -> io::Result<Vec<u8>> {
    DECOMPRESSOR.with(|d| {
        let mut d = d.borrow_mut();
        if d.is_none() {
            *d = Some(zstd::bulk::Decompressor::new()?);
        }
        d.as_mut().expect("a decompressor").decompress(frame, cap)
    })
}

/// Whether records this many and this large get a trained dictionary (`train_dictionary`): a
/// dictionary of `dict_size` pays from 32 times its size on.
pub fn trains(records: usize, bytes: usize, dict_size: usize) -> bool {
    dict_size > 0 && records >= 64 && bytes >= dict_size * 32
}

/// A dictionary of `dict_size` trained on a sample of the records, or none (empty) when they are
/// too few (`trains`).
pub fn train_dictionary(records: &[&[u8]], dict_size: usize) -> Vec<u8> {
    let total: usize = records.iter().map(|r| r.len()).sum();
    if !trains(records.len(), total, dict_size) {
        return Vec::new();
    }
    let step = (total / MAX_SAMPLE_BYTES).max(1);
    let samples: Vec<&[u8]> = records.iter().step_by(step).copied().collect();
    zstd::dict::from_samples(&samples, dict_size).unwrap_or_default()
}

fn block_ranges(records: &[&[u8]], target: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let (mut start, mut size) = (0usize, 0usize);
    for (i, r) in records.iter().enumerate() {
        size += r.len() + 4;
        if size >= target {
            ranges.push((start, i + 1));
            start = i + 1;
            size = 0;
        }
    }
    if start < records.len() {
        ranges.push((start, records.len()));
    }
    ranges
}

fn raw_block(records: &[&[u8]]) -> Vec<u8> {
    let body: usize = records.iter().map(|r| r.len()).sum();
    let mut out = Vec::with_capacity(4 + 4 * (records.len() + 1) + body);
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    let mut off = 0u32;
    out.extend_from_slice(&off.to_le_bytes());
    for r in records {
        off += r.len() as u32;
        out.extend_from_slice(&off.to_le_bytes());
    }
    for r in records {
        out.extend_from_slice(r);
    }
    out
}

/// Build the DOCS section from records in doc id order, with a dictionary trained on them when
/// they are enough (`trains`).
pub fn build_section(records: &[&[u8]], opts: &DocsOptions) -> io::Result<Vec<u8>> {
    let dict = train_dictionary(records, opts.dict_size);
    build_parts(&[Part::Records(records)], &dict, opts)
}

/// A part of a DOCS section (`build_parts`): records to compress, or a block copied as it is from
/// a section compressed with the same dictionary, holding `docs` records.
pub enum Part<'a> {
    Records(&'a [&'a [u8]]),
    Block { frame: &'a [u8], docs: u32 },
}

/// Build a DOCS section of `parts` in doc id order compressed with `dict` (empty: none): the
/// records of each `Records` part cut into blocks and compressed (in parallel when there are
/// several), each `Block` copied.
pub fn build_parts(parts: &[Part<'_>], dict: &[u8], opts: &DocsOptions) -> io::Result<Vec<u8>> {
    // per block its first document and its frame (None: its records, in `raw`, are compressed)
    let (mut firsts, mut frames, mut raw) = (Vec::new(), Vec::new(), Vec::new());
    let mut doc = 0u32;
    for p in parts {
        match p {
            Part::Records(recs) => {
                for (a, b) in block_ranges(recs, opts.block_target) {
                    firsts.push(doc + a as u32);
                    frames.push(None);
                    raw.push(&recs[a..b]);
                }
                doc += recs.len() as u32;
            }
            Part::Block { frame, docs } => {
                firsts.push(doc);
                frames.push(Some(*frame));
                doc += docs;
            }
        }
    }
    let key = dictionary_key(dict);
    let one = |recs: &&[&[u8]]| compress(opts.level, dict, key, &raw_block(recs));
    let bytes: usize = raw.iter().flat_map(|recs| recs.iter()).map(|r| r.len()).sum();
    let compressed: Vec<Vec<u8>> =
        if raw.len() > 1 && bytes >= PARALLEL_BYTES { raw.par_iter().map(one).collect::<io::Result<_>>()? } else { raw.iter().map(one).collect::<io::Result<_>>()? };
    let mut compressed = compressed.iter();
    let blocks: Vec<&[u8]> = frames.iter().map(|f| f.unwrap_or_else(|| compressed.next().expect("a compressed block"))).collect();

    let b = blocks.len();
    let mut out = Vec::with_capacity(16 + 12 * (b + 1) + dict.len() + blocks.iter().map(|f| f.len()).sum::<usize>());
    out.extend_from_slice(&(b as u32).to_le_bytes());
    out.extend_from_slice(&(dict.len() as u32).to_le_bytes());
    for first in &firsts {
        out.extend_from_slice(&first.to_le_bytes());
    }
    out.extend_from_slice(&doc.to_le_bytes());
    while out.len() % 8 != 0 {
        out.push(0);
    }
    let mut off = 0u64;
    out.extend_from_slice(&off.to_le_bytes());
    for blk in &blocks {
        off += blk.len() as u64;
        out.extend_from_slice(&off.to_le_bytes());
    }
    out.extend_from_slice(dict);
    for blk in &blocks {
        out.extend_from_slice(blk);
    }
    Ok(out)
}

struct BlockCache {
    map: FxHashMap<u32, Arc<[u8]>>,
    order: VecDeque<u32>,
    capacity: usize,
}

/// Record `i` of a decompressed block (`DocStore::read_block`).
pub fn block_record(raw: &[u8], i: usize) -> io::Result<&[u8]> {
    let n = raw.get(..4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let base = 4 + 4 * (n + 1);
    if i >= n || base > raw.len() {
        return Err(invalid("record outside its block"));
    }
    let off = |i: usize| u32::from_le_bytes(raw[4 + 4 * i..8 + 4 * i].try_into().unwrap()) as usize;
    raw.get(base + off(i)..base + off(i + 1)).ok_or_else(|| invalid("record past end of block"))
}

/// Reader over a DOCS section.
pub struct DocStore {
    section: Bytes,
    block_count: usize,
    total_docs: u32,
    block_off_at: usize,
    blocks_at: usize,
    dict: Option<Arc<zstd::dict::DecoderDictionary<'static>>>,
    cache: Mutex<BlockCache>,
}

/// One record inside a decompressed block.
#[derive(Clone)]
pub struct RecordRef {
    block: Arc<[u8]>,
    start: usize,
    end: usize,
}

impl std::ops::Deref for RecordRef {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.block[self.start..self.end]
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("DOCS section: {msg}"))
}

impl DocStore {
    pub fn open(section: Bytes) -> io::Result<Self> {
        if section.len() < 8 {
            return Err(invalid("truncated header"));
        }
        let block_count = section.u32_at(0) as usize;
        let dict_len = section.u32_at(4) as usize;
        let first_end = 8 + 4 * (block_count + 1);
        let block_off_at = first_end.next_multiple_of(8);
        let dict_at = block_off_at + 8 * (block_count + 1);
        let blocks_at = dict_at + dict_len;
        if blocks_at > section.len() {
            return Err(invalid("tables past end"));
        }
        let total_docs = section.u32_at(8 + 4 * block_count);
        if blocks_at as u64 + section.u64_at(block_off_at + 8 * block_count) != section.len() as u64 {
            return Err(invalid("block offsets do not cover the section"));
        }
        let dict = (dict_len > 0).then(|| decoder_dictionary(&section[dict_at..blocks_at]));
        Ok(Self {
            section,
            block_count,
            total_docs,
            block_off_at,
            blocks_at,
            dict,
            cache: Mutex::new(BlockCache { map: FxHashMap::default(), order: VecDeque::new(), capacity: 64 }),
        })
    }

    #[inline]
    pub fn total_docs(&self) -> u32 {
        self.total_docs
    }

    #[inline]
    fn first_doc(&self, block: usize) -> u32 {
        self.section.u32_at(8 + 4 * block)
    }

    pub fn block_count(&self) -> usize {
        self.block_count
    }

    /// The documents of block `block`.
    pub fn block_docs(&self, block: usize) -> std::ops::Range<u32> {
        self.first_doc(block)..self.first_doc(block + 1)
    }

    /// Block `block`'s zstd frame.
    pub fn frame(&self, block: usize) -> &[u8] {
        let a = self.section.u64_at(self.block_off_at + 8 * block) as usize;
        let b = self.section.u64_at(self.block_off_at + 8 * (block + 1)) as usize;
        &self.section[self.blocks_at + a..self.blocks_at + b]
    }

    /// The bytes of the records of block `block` uncompressed (its frame's content size).
    pub fn block_raw_len(&self, block: usize) -> usize {
        zstd::zstd_safe::get_frame_content_size(self.frame(block)).ok().flatten().unwrap_or(0) as usize
    }

    /// A decompression context for the section's blocks (its dictionary loaded).
    pub fn decompressor(&self) -> io::Result<zstd::bulk::Decompressor<'_>> {
        match &self.dict {
            Some(d) => zstd::bulk::Decompressor::with_prepared_dictionary(d),
            None => zstd::bulk::Decompressor::new(),
        }
    }

    /// Block `block` decompressed with `d` (`decompressor`) into `out`, its size
    /// (`block_raw_len`), bypassing the cache: a merge reads each block once.
    pub fn read_block_into(&self, block: usize, d: &mut zstd::bulk::Decompressor<'_>, out: &mut [u8]) -> io::Result<()> {
        if d.decompress_to_buffer(self.frame(block), out)? != out.len() {
            return Err(invalid("block shorter than its frame says"));
        }
        Ok(())
    }

    /// The dictionary the blocks are compressed with (empty: none).
    pub fn dictionary(&self) -> &[u8] {
        &self.section[self.block_off_at + 8 * (self.block_count + 1)..self.blocks_at]
    }

    /// The block holding `doc_id` (one of the section's documents).
    pub fn block_of(&self, doc_id: u32) -> usize {
        let (mut lo, mut hi) = (0usize, self.block_count);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.first_doc(mid) <= doc_id { lo = mid } else { hi = mid }
        }
        lo
    }

    fn load_block(&self, block: usize) -> io::Result<Arc<[u8]>> {
        if let Some(b) = self.cache.lock().unwrap().map.get(&(block as u32)) {
            return Ok(Arc::clone(b));
        }
        let a = self.section.u64_at(self.block_off_at + 8 * block) as usize;
        let b = self.section.u64_at(self.block_off_at + 8 * (block + 1)) as usize;
        let frame = &self.section[self.blocks_at + a..self.blocks_at + b];
        let cap = zstd::zstd_safe::get_frame_content_size(frame)
            .ok()
            .flatten()
            .ok_or_else(|| invalid("block frame has no content size"))? as usize;
        let raw: Arc<[u8]> = match &self.dict {
            Some(d) => zstd::bulk::Decompressor::with_prepared_dictionary(d)?.decompress(frame, cap)?,
            None => decompress_plain(frame, cap)?,
        }
        .into();
        let mut cache = self.cache.lock().unwrap();
        if cache.map.len() >= cache.capacity {
            if let Some(old) = cache.order.pop_front() {
                cache.map.remove(&old);
            }
        }
        cache.order.push_back(block as u32);
        cache.map.insert(block as u32, Arc::clone(&raw));
        Ok(raw)
    }

    /// The record of `doc_id`, or None when out of range.
    pub fn record(&self, doc_id: u32) -> io::Result<Option<RecordRef>> {
        if doc_id >= self.total_docs || self.block_count == 0 {
            return Ok(None);
        }
        let block = self.block_of(doc_id);
        let raw = self.load_block(block)?;
        let local = (doc_id - self.first_doc(block)) as usize;
        let n = u32::from_le_bytes(raw[0..4].try_into().unwrap()) as usize;
        if local >= n {
            return Err(invalid("doc outside its block"));
        }
        let base = 4 + 4 * (n + 1);
        let off = |i: usize| u32::from_le_bytes(raw[4 + 4 * i..8 + 4 * i].try_into().unwrap()) as usize;
        let (start, end) = (base + off(local), base + off(local + 1));
        if end > raw.len() {
            return Err(invalid("record past end of block"));
        }
        Ok(Some(RecordRef { block: raw, start, end }))
    }

    /// Compressed bytes of the section (memory accounting).
    pub fn section_len(&self) -> usize {
        self.section.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_across_blocks_with_and_without_dictionary() {
        for n in [1usize, 50, 3000] {
            let recs: Vec<Vec<u8>> = (0..n)
                .map(|i| format!("record {i} {}", "x".repeat(i % 97)).into_bytes())
                .collect();
            let refs: Vec<&[u8]> = recs.iter().map(Vec::as_slice).collect();
            let opts = DocsOptions { block_target: 2048, ..Default::default() };
            let store = DocStore::open(Bytes::from_vec(build_section(&refs, &opts).unwrap())).unwrap();
            assert_eq!(store.total_docs(), n as u32);
            for i in (0..n).step_by(7).chain([n - 1]) {
                assert_eq!(&*store.record(i as u32).unwrap().unwrap(), recs[i].as_slice());
            }
            assert!(store.record(n as u32).unwrap().is_none());
        }
    }
}
