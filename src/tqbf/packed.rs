//! Bit-packed integer arrays, read in place (TQBF spec §7.4).
//!
//! Fixed width: values LSB first at any width up to 64, followed by 8 zero bytes, so a read is one
//! unaligned 8-byte load, a shift and a mask (widths above 56 can take a ninth byte).
//!
//! Monotonic (Lucene's DirectMonotonic): a non-decreasing sequence in blocks of 1024 values, each
//! stored as its residual above a line through the block, so offsets and sorted dictionaries cost
//! a few bits per value instead of their full width.
//!
//! ```text
//! u64 count, u64 n_blocks
//! n_blocks x 32 bytes: u64 base, f64 slope, u64 bit_offset, u32 width, u32 0
//! residuals bit-packed, block after block; 8 zero bytes
//! value(i) = base + floor(slope * j) + residual(i), j = i mod 1024 (wrapping u64 arithmetic)
//! ```

use super::bytes::Bytes;
use std::io;

/// Bits needed for `max` (0 for 0).
#[inline]
pub fn bits_for(max: u64) -> u32 {
    64 - max.leading_zeros()
}

/// `width` bits at bit `pos`. The buffer carries 8 bytes of slack after the last value.
#[inline]
pub fn read(bits: &[u8], pos: u64, width: u32) -> u64 {
    if width == 0 {
        return 0;
    }
    let at = (pos >> 3) as usize;
    let shift = (pos & 7) as u32;
    let mut v = u64::from_le_bytes(bits[at..at + 8].try_into().unwrap()) >> shift;
    if shift + width > 64 {
        v |= u64::from(bits[at + 8]) << (64 - shift);
    }
    if width == 64 { v } else { v & ((1u64 << width) - 1) }
}

/// Append the `n` values of `width` (at most 32) bits from bit `pos` to `out`. The buffer carries 8
/// bytes of slack after the last value.
pub fn unpack(bits: &[u8], pos: u64, width: u32, n: usize, out: &mut Vec<u32>) {
    debug_assert!(width <= 32);
    if width == 0 {
        out.resize(out.len() + n, 0);
        return;
    }
    let mask = if width == 32 { u64::from(u32::MAX) } else { (1u64 << width) - 1 };
    out.reserve(n);
    let mut p = pos;
    for _ in 0..n {
        // shift (< 8) + width (<= 32) stays within the 8 bytes read
        let at = (p >> 3) as usize;
        let word = u64::from_le_bytes(bits[at..at + 8].try_into().unwrap());
        out.push(((word >> (p & 7)) & mask) as u32);
        p += u64::from(width);
    }
}

pub struct BitWriter {
    words: Vec<u64>,
    pos: u64,
}

impl Default for BitWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BitWriter {
    pub fn new() -> Self {
        Self { words: Vec::new(), pos: 0 }
    }

    pub fn with_bits(bits: u64) -> Self {
        Self { words: Vec::with_capacity(bits.div_ceil(64) as usize + 2), pos: 0 }
    }

    /// Bits written so far.
    #[inline]
    pub fn pos(&self) -> u64 {
        self.pos
    }

    pub fn push(&mut self, value: u64, width: u32) {
        if width == 0 {
            return;
        }
        debug_assert!(width == 64 || value >> width == 0, "{value} does not fit {width} bits");
        let (w, b) = ((self.pos >> 6) as usize, (self.pos & 63) as u32);
        if w + 1 >= self.words.len() {
            self.words.resize(w + 2, 0);
        }
        self.words[w] |= value << b;
        if b + width > 64 {
            self.words[w + 1] |= value >> (64 - b);
        }
        self.pos += u64::from(width);
    }

    /// Push every value at `width` bits.
    pub fn push_all(&mut self, values: &[u32], width: u32) {
        if width == 0 || values.is_empty() {
            return;
        }
        let end = self.pos + values.len() as u64 * u64::from(width);
        let need = (end >> 6) as usize + 2;
        if self.words.len() < need {
            self.words.resize(need, 0);
        }
        let mut pos = self.pos;
        for &v in values {
            let v = u64::from(v);
            debug_assert!(width >= 32 || v >> width == 0, "{v} does not fit {width} bits");
            let (w, b) = ((pos >> 6) as usize, (pos & 63) as u32);
            self.words[w] |= v << b;
            if b + width > 64 {
                self.words[w + 1] |= v >> (64 - b);
            }
            pos += u64::from(width);
        }
        self.pos = end;
    }

    /// Append `other`'s bits from the next 64-bit boundary; returns the bit they start at.
    pub fn append_aligned(&mut self, other: BitWriter) -> u64 {
        let base = self.pos.next_multiple_of(64);
        // every word past `pos` is zero
        self.words.resize((base >> 6) as usize, 0);
        let used = (other.pos.div_ceil(64) as usize).min(other.words.len());
        self.words.extend_from_slice(&other.words[..used]);
        self.pos = base + other.pos;
        base
    }

    /// Packed bytes followed by 8 zero bytes.
    pub fn into_bytes(mut self) -> Vec<u8> {
        let used = (self.pos as usize).div_ceil(8);
        self.words.resize(used.div_ceil(8) + 2, 0);
        let mut out: Vec<u8> = bytemuck::cast_slice(&self.words).to_vec();
        out.truncate(used + 8);
        out
    }
}

/// Bytes of `n` values at `width` bits, slack included.
#[inline]
pub fn packed_len(n: u64, width: u32) -> usize {
    (n * u64::from(width)).div_ceil(8) as usize + 8
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("packed array: {msg}"))
}

// -- monotonic --------------------------------------------------------------------------------------

const BLOCK_SHIFT: u32 = 10;
const BLOCK: usize = 1 << BLOCK_SHIFT;
const META: usize = 32;

#[inline]
fn line(slope: f64, j: usize) -> u64 {
    // Saturating float-to-int cast: identical on the writer and the reader.
    (slope * j as f64) as u64
}

/// Append a monotonic sequence. `values` must be non-decreasing.
pub fn write_monotonic(values: &[u64], out: &mut Vec<u8>) {
    debug_assert!(values.windows(2).all(|w| w[0] <= w[1]));
    let n_blocks = values.len().div_ceil(BLOCK);
    let meta_at = out.len() + 16;
    out.extend_from_slice(&(values.len() as u64).to_le_bytes());
    out.extend_from_slice(&(n_blocks as u64).to_le_bytes());
    out.resize(meta_at + META * n_blocks, 0);
    let mut bits = BitWriter::with_bits(values.len() as u64 * 8);
    for (b, block) in values.chunks(BLOCK).enumerate() {
        let first = block[0];
        let mut slope = if block.len() > 1 { (block[block.len() - 1] - first) as f64 / (block.len() - 1) as f64 } else { 0.0 };
        // Residuals against the line, shifted so the smallest is 0; a flat line when they would
        // not fit 64 bits.
        let residuals = |slope: f64| -> Option<(i128, u64)> {
            let r = |j: usize, v: u64| i128::from(v) - i128::from(first) - i128::from(line(slope, j));
            let min = block.iter().enumerate().map(|(j, &v)| r(j, v)).min().unwrap();
            let max = block.iter().enumerate().map(|(j, &v)| r(j, v)).max().unwrap();
            u64::try_from(max - min).ok().map(|span| (min, span))
        };
        let (min, span) = residuals(slope).unwrap_or_else(|| {
            slope = 0.0;
            residuals(0.0).unwrap()
        });
        let width = bits_for(span);
        let base = (i128::from(first) + min) as u64;
        let m = meta_at + META * b;
        out[m..m + 8].copy_from_slice(&base.to_le_bytes());
        out[m + 8..m + 16].copy_from_slice(&slope.to_le_bytes());
        out[m + 16..m + 24].copy_from_slice(&bits.pos().to_le_bytes());
        out[m + 24..m + 28].copy_from_slice(&width.to_le_bytes());
        for (j, &v) in block.iter().enumerate() {
            bits.push(v.wrapping_sub(base).wrapping_sub(line(slope, j)), width);
        }
    }
    out.extend_from_slice(&bits.into_bytes());
}

/// Bytes `write_monotonic` would append, without writing (sizes for layout decisions).
pub fn monotonic_len(values: &[u64]) -> usize {
    let mut out = Vec::new();
    write_monotonic(values, &mut out);
    out.len()
}

/// A monotonic sequence read in place.
#[derive(Clone)]
pub struct Monotonic {
    bytes: Bytes,
    count: usize,
    n_blocks: usize,
}

impl Monotonic {
    /// Parse a sequence at the start of `bytes` (which may extend past it). Returns the sequence
    /// and its length in bytes.
    pub fn parse(bytes: Bytes) -> io::Result<(Self, usize)> {
        if bytes.len() < 16 {
            return Err(invalid("monotonic header truncated"));
        }
        let count = bytes.u64_at(0) as usize;
        let n_blocks = bytes.u64_at(8) as usize;
        if n_blocks != count.div_ceil(BLOCK) || 16 + META * n_blocks > bytes.len() {
            return Err(invalid("monotonic block table past end"));
        }
        let data_at = 16 + META * n_blocks;
        let mut end_bits = 0u64;
        for b in 0..n_blocks {
            let m = 16 + META * b;
            let (off, width) = (bytes.u64_at(m + 16), bytes.u32_at(m + 24));
            let n = (count - b * BLOCK).min(BLOCK) as u64;
            if width > 64 || off != end_bits {
                return Err(invalid("monotonic block offsets inconsistent"));
            }
            end_bits = off + n * u64::from(width);
        }
        let len = data_at + end_bits.div_ceil(8) as usize + 8;
        if len > bytes.len() {
            return Err(invalid("monotonic values past end"));
        }
        Ok((Self { bytes: bytes.slice(0, len), count, n_blocks }, len))
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[inline]
    pub fn get(&self, i: usize) -> u64 {
        debug_assert!(i < self.count);
        let (b, j) = (i >> BLOCK_SHIFT, i & (BLOCK - 1));
        let m = 16 + META * b;
        let base = self.bytes.u64_at(m);
        let slope = f64::from_le_bytes(self.bytes[m + 8..m + 16].try_into().unwrap());
        let off = self.bytes.u64_at(m + 16);
        let width = self.bytes.u32_at(m + 24);
        let data = &self.bytes[16 + META * self.n_blocks..];
        base.wrapping_add(line(slope, j)).wrapping_add(read(data, off + j as u64 * u64::from(width), width))
    }

    /// First index whose value is >= `v` (`len()` when none).
    pub fn lower_bound(&self, v: u64) -> usize {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.get(mid) < v { lo = mid + 1 } else { hi = mid }
        }
        lo
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_width_round_trip_every_width() {
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        for width in 0..=64u32 {
            let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
            let vals: Vec<u64> = (0..300)
                .map(|_| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    rng & mask
                })
                .collect();
            let mut w = BitWriter::new();
            for &v in &vals {
                w.push(v, width);
            }
            let bytes = w.into_bytes();
            assert_eq!(bytes.len(), packed_len(vals.len() as u64, width));
            for (i, &v) in vals.iter().enumerate() {
                assert_eq!(read(&bytes, i as u64 * u64::from(width), width), v, "width {width} index {i}");
            }
        }
    }

    #[test]
    fn monotonic_round_trip() {
        let cases: Vec<Vec<u64>> = vec![
            vec![],
            vec![7],
            (0..5000).map(|i| i * 7 + (i % 7)).collect(),
            (0..3000).map(|i| (i as u64).pow(2)).collect(),
            vec![0, 0, 0, u64::MAX, u64::MAX],
            vec![1, u64::MAX / 2, u64::MAX - 1, u64::MAX],
            (0..2048).map(|i| if i < 1024 { 5 } else { 1u64 << 63 }).collect(),
        ];
        for vals in cases {
            let mut out = vec![0u8; 3];
            write_monotonic(&vals, &mut out);
            assert_eq!(out.len() - 3, monotonic_len(&vals));
            let (m, len) = Monotonic::parse(Bytes::from_vec(out[3..].to_vec())).unwrap();
            assert_eq!(len, out.len() - 3);
            assert_eq!(m.len(), vals.len());
            for (i, &v) in vals.iter().enumerate() {
                assert_eq!(m.get(i), v, "index {i}");
            }
            for probe in vals.iter().copied().chain([0, 1, u64::MAX]) {
                assert_eq!(m.lower_bound(probe), vals.partition_point(|&x| x < probe));
            }
        }
    }

    #[test]
    fn monotonic_offsets_are_compact() {
        // Offsets of 100k documents with 0-3 values each: a few bits per entry, not 32.
        let mut v = vec![0u64];
        for i in 0..100_000u64 {
            v.push(v[v.len() - 1] + i % 4);
        }
        assert!(monotonic_len(&v) < 100_000 * 3 / 8 + 4096, "{}", monotonic_len(&v));
    }
}
