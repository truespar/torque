//! Frozen roaring layout (TQBF spec §6.2), read in place from mapped or heap bytes:
//!
//! ```text
//! u32 n                     containers
//! u32 offset[n]             container start, relative to the bitmap start
//! u16 key[n]                high 16 bits, strictly increasing
//! u16 card_minus_1[n]
//! u8  kind[n]               0 array, 1 bitset, 2 run
//! containers                array: u16[card]; bitset: u64[1024]; run: u16 n_runs, (u16 start, u16 len-1)[n_runs]
//! ```
//! The writer aligns every container absolutely within its buffer (u16 data to 2, bitsets to 8), so a
//! section starting on an 8-byte boundary can be cast to `&[u16]` / `&[u64; 1024]` without copying.
//! Each container is written in whichever of the three encodings is smallest.

use super::store::{CRef, Words, ARRAY_MAX, WORDS};
use crate::tqbf::bytes::Bytes;
use std::io;
use std::sync::Arc;

pub const KIND_ARRAY: u8 = 0;
pub const KIND_BITSET: u8 = 1;
pub const KIND_RUN: u8 = 2;

/// Validated frozen bitmap. `ptr` points into `bytes`, which keeps the (immutable) data alive and
/// in place, so accessors read through it without the dynamic `AsRef` and bounds checks.
#[derive(Clone)]
pub struct Frozen {
    bytes: Bytes,
    ptr: *const u8,
    n: usize,
}

// SAFETY: the pointed-to bytes are owned by `bytes` (an Arc'd buffer or memory map that never moves
// or changes) and are only read.
unsafe impl Send for Frozen {}
unsafe impl Sync for Frozen {}

/// 8-byte aligned heap buffer, used when frozen bytes arrive misaligned.
struct AlignedBuf {
    words: Vec<u64>,
    len: usize,
}

impl AsRef<[u8]> for AlignedBuf {
    fn as_ref(&self) -> &[u8] {
        &bytemuck::cast_slice::<u64, u8>(&self.words)[..self.len]
    }
}

pub fn aligned_copy(src: &[u8]) -> Bytes {
    let mut words = vec![0u64; src.len().div_ceil(8)];
    bytemuck::cast_slice_mut::<u64, u8>(&mut words)[..src.len()].copy_from_slice(src);
    Bytes::from_owner(Arc::new(AlignedBuf { words, len: src.len() }))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("frozen bitmap: {msg}"))
}

#[inline]
fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

#[inline]
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

impl Frozen {
    /// Validate and wrap. Misaligned input is copied once into an aligned buffer.
    pub fn parse(bytes: Bytes) -> io::Result<Self> {
        let bytes = if Self::check(&bytes)? {
            bytes
        } else {
            let copy = aligned_copy(&bytes);
            if !Self::check(&copy)? {
                return Err(invalid("unaligned after copy"));
            }
            copy
        };
        Ok(Self { n: u32_at(&bytes, 0) as usize, ptr: bytes.as_ptr(), bytes })
    }

    /// Ok(true) when valid and aligned, Ok(false) when valid but misaligned.
    fn check(b: &[u8]) -> io::Result<bool> {
        if b.len() < 4 {
            return Err(invalid("truncated"));
        }
        let n = u32_at(b, 0) as usize;
        if n > 65536 {
            return Err(invalid("more than 65536 containers"));
        }
        let header = 4 + 9 * n;
        if header > b.len() {
            return Err(invalid("header past end"));
        }
        let base = b.as_ptr() as usize;
        let mut aligned = (base + 4 + 4 * n) % 2 == 0;
        let mut prev_key: i32 = -1;
        for i in 0..n {
            let off = u32_at(b, 4 + 4 * i) as usize;
            let key = i32::from(u16_at(b, 4 + 4 * n + 2 * i));
            let card = u32::from(u16_at(b, 4 + 6 * n + 2 * i)) + 1;
            let kind = b[4 + 8 * n + i];
            if key <= prev_key {
                return Err(invalid("keys not increasing"));
            }
            prev_key = key;
            if off < header {
                return Err(invalid("container inside header"));
            }
            let len = match kind {
                KIND_ARRAY => {
                    if card as usize > ARRAY_MAX {
                        return Err(invalid("array container over 4096 values"));
                    }
                    2 * card as usize
                }
                KIND_BITSET => WORDS * 8,
                KIND_RUN => {
                    if off + 2 > b.len() {
                        return Err(invalid("run container past end"));
                    }
                    2 + 4 * u16_at(b, off) as usize
                }
                _ => return Err(invalid("unknown container kind")),
            };
            if off + len > b.len() {
                return Err(invalid("container past end"));
            }
            let need = if kind == KIND_BITSET { 8 } else { 2 };
            if (base + off) % need != 0 {
                aligned = false;
            }
            if kind == KIND_RUN {
                let runs = u16_at(b, off) as usize;
                let mut sum = 0u32;
                let mut prev_end: i32 = -2;
                for r in 0..runs {
                    let s = i32::from(u16_at(b, off + 2 + 4 * r));
                    let l = u32::from(u16_at(b, off + 4 + 4 * r));
                    if s <= prev_end + 1 || s as u32 + l > 65535 {
                        return Err(invalid("run container not sorted and disjoint"));
                    }
                    prev_end = s + l as i32;
                    sum += l + 1;
                }
                if sum != card {
                    return Err(invalid("run cardinality mismatch"));
                }
            }
        }
        Ok(aligned)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.n
    }

    #[inline]
    fn read_u16(&self, at: usize) -> u16 {
        // SAFETY: `at` lies inside the header validated by `check`.
        unsafe { self.ptr.add(at).cast::<u16>().read_unaligned() }
    }

    #[inline]
    fn read_u32(&self, at: usize) -> u32 {
        // SAFETY: as above.
        unsafe { self.ptr.add(at).cast::<u32>().read_unaligned() }
    }

    /// Container keys, read in place (2-byte alignment checked at parse).
    #[inline]
    pub fn keys(&self) -> &[u16] {
        // SAFETY: n keys at 4 + 4n, inside the validated header and 2-byte aligned.
        unsafe { std::slice::from_raw_parts(self.ptr.add(4 + 4 * self.n).cast::<u16>(), self.n) }
    }

    #[inline]
    pub fn key(&self, i: usize) -> u16 {
        self.keys()[i]
    }

    #[inline]
    pub fn card(&self, i: usize) -> u32 {
        debug_assert!(i < self.n);
        u32::from(self.read_u16(4 + 6 * self.n + 2 * i)) + 1
    }

    #[inline]
    pub fn cref(&self, i: usize) -> CRef<'_> {
        assert!(i < self.n);
        let n = self.n;
        let off = self.read_u32(4 + 4 * i) as usize;
        let card = self.card(i);
        // SAFETY: `check` validated kind, bounds and alignment of every container.
        unsafe {
            let at = self.ptr.add(off);
            match *self.ptr.add(4 + 8 * n + i) {
                KIND_ARRAY => CRef::Array(std::slice::from_raw_parts(at.cast::<u16>(), card as usize)),
                KIND_BITSET => CRef::Bitset(&*at.cast::<Words>(), card),
                _ => {
                    let runs = at.cast::<u16>().read() as usize;
                    CRef::Run(std::slice::from_raw_parts(at.add(2).cast::<[u16; 2]>(), runs), card)
                }
            }
        }
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// True when the bytes are a memory-mapped file rather than a heap buffer.
    pub fn is_mapped(&self) -> bool {
        self.bytes.is_mapped()
    }
}

fn pad_to(out: &mut Vec<u8>, align: usize) {
    while out.len() % align != 0 {
        out.push(0);
    }
}

/// Append the values of the frozen bitmap `b` (as `write` lays it out), ascending, to `out`, read
/// where they are: no copy, no validation beyond the bounds of `b` (a merge's postings, of sections
/// validated when opened), no reference counted.
pub fn decode_into(b: &[u8], out: &mut Vec<u32>) -> io::Result<()> {
    let short = || invalid("truncated");
    let n = u32_at(b.get(..4).ok_or_else(short)?, 0) as usize;
    let header = b.get(..4 + 9 * n).ok_or_else(short)?;
    for i in 0..n {
        let off = u32_at(header, 4 + 4 * i) as usize;
        let high = u32::from(u16_at(header, 4 + 4 * n + 2 * i)) << 16;
        let card = usize::from(u16_at(header, 4 + 6 * n + 2 * i)) + 1;
        match header[4 + 8 * n + i] {
            KIND_ARRAY => {
                let v = b.get(off..off + 2 * card).ok_or_else(short)?;
                out.extend(v.as_chunks::<2>().0.iter().map(|&c| high | u32::from(u16::from_le_bytes(c))));
            }
            KIND_BITSET => {
                let w = b.get(off..off + WORDS * 8).ok_or_else(short)?;
                out.reserve(card);
                for (j, &c) in w.as_chunks::<8>().0.iter().enumerate() {
                    let mut word = u64::from_le_bytes(c);
                    while word != 0 {
                        out.push(high | (64 * j as u32 + word.trailing_zeros()));
                        word &= word - 1;
                    }
                }
            }
            KIND_RUN => {
                let runs = usize::from(u16_at(b.get(off..off + 2).ok_or_else(short)?, 0));
                let r = b.get(off + 2..off + 2 + 4 * runs).ok_or_else(short)?;
                out.reserve(card);
                for c in r.as_chunks::<4>().0 {
                    let (start, len) = (u32::from(u16::from_le_bytes([c[0], c[1]])), u32::from(u16::from_le_bytes([c[2], c[3]])));
                    out.extend((start..=start + len).map(|v| high | v));
                }
            }
            _ => return Err(invalid("unknown container kind")),
        }
    }
    Ok(())
}

/// Kind and encoded size of the smallest encoding of a container.
fn choose(c: &CRef<'_>) -> (u8, usize) {
    let card = c.card() as usize;
    let run_bytes = 2 + 4 * c.count_runs();
    let flat = if card <= ARRAY_MAX { (KIND_ARRAY, 2 * card) } else { (KIND_BITSET, WORDS * 8) };
    if run_bytes < flat.1 { (KIND_RUN, run_bytes) } else { flat }
}

/// Append a frozen bitmap. `out.len()` is taken as the absolute position in an 8-aligned buffer.
pub fn write<'a>(containers: impl ExactSizeIterator<Item = (u16, CRef<'a>)> + Clone, out: &mut Vec<u8>) {
    pad_to(out, 4);
    let start = out.len();
    let n = containers.len();
    let header = 4 + 9 * n;
    out.resize(start + header, 0);
    out[start..start + 4].copy_from_slice(&(n as u32).to_le_bytes());
    for (i, (key, c)) in containers.enumerate() {
        let (kind, _) = choose(&c);
        pad_to(out, if kind == KIND_BITSET { 8 } else { 2 });
        let off = (out.len() - start) as u32;
        out[start + 4 + 4 * i..start + 8 + 4 * i].copy_from_slice(&off.to_le_bytes());
        out[start + 4 + 4 * n + 2 * i..start + 6 + 4 * n + 2 * i].copy_from_slice(&key.to_le_bytes());
        let card_m1 = (c.card() - 1) as u16;
        out[start + 4 + 6 * n + 2 * i..start + 6 + 6 * n + 2 * i].copy_from_slice(&card_m1.to_le_bytes());
        out[start + 4 + 8 * n + i] = kind;
        match kind {
            KIND_ARRAY => match c {
                CRef::Array(v) => out.extend_from_slice(bytemuck::cast_slice(v)),
                other => out.extend(other.iter().flat_map(u16::to_le_bytes)),
            },
            KIND_BITSET => match c {
                CRef::Bitset(w, _) => out.extend_from_slice(bytemuck::cast_slice(&w[..])),
                other => out.extend_from_slice(bytemuck::cast_slice(&other.to_words()[..])),
            },
            _ => {
                let runs = c.to_runs();
                out.extend_from_slice(&(runs.len() as u16).to_le_bytes());
                for r in runs {
                    out.extend_from_slice(&r[0].to_le_bytes());
                    out.extend_from_slice(&r[1].to_le_bytes());
                }
            }
        }
    }
}

/// Size `write` produces when starting at an 8-aligned position.
pub fn size<'a>(containers: impl ExactSizeIterator<Item = (u16, CRef<'a>)>) -> usize {
    let n = containers.len();
    let mut pos = 4 + 9 * n;
    for (_, c) in containers {
        let (kind, len) = choose(&c);
        let align = if kind == KIND_BITSET { 8 } else { 2 };
        pos = pos.next_multiple_of(align) + len;
    }
    pos
}
