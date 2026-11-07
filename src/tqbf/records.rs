//! RECORDS section of a docs-only file (spec §10): the documents a client wrote, as records
//! (spec §5.2) in import order, in zlib chunks so a writer can stream them and a builder can
//! decode chunks in parallel.
//!
//! ```text
//! chunks       u32 n_docs, u32 raw_len, zlib stream of n_docs x (varint(len) record)
//! trailer      u64 chunk_offset[n_chunks + 1] (relative to the section), u32 doc_count,
//!              u32 n_chunks
//! ```
//!
//! zlib because every client platform has it built in (.NET `ZLibStream`); the section only
//! lives on the build machine or in transit, never in a served segment.

use super::bytes::Bytes;
use super::record::put_varint;
use std::io::{self, Read, Write};

/// Target uncompressed bytes per chunk.
pub const CHUNK_TARGET: usize = 1 << 20;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("RECORDS section: {}", msg.into()))
}

/// Writes a RECORDS section in memory (tests and Rust-side writers; the .NET writer streams).
#[derive(Default)]
pub struct RecordsWriter {
    out: Vec<u8>,
    chunk: Vec<u8>,
    chunk_docs: u32,
    offsets: Vec<u64>,
    doc_count: u32,
}

impl RecordsWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, record: &[u8]) -> io::Result<()> {
        put_varint(&mut self.chunk, record.len() as u64);
        self.chunk.extend_from_slice(record);
        self.chunk_docs += 1;
        self.doc_count += 1;
        if self.chunk.len() >= CHUNK_TARGET {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.chunk_docs == 0 {
            return Ok(());
        }
        self.offsets.push(self.out.len() as u64);
        self.out.extend_from_slice(&self.chunk_docs.to_le_bytes());
        self.out.extend_from_slice(&(self.chunk.len() as u32).to_le_bytes());
        let mut z = flate2::write::ZlibEncoder::new(&mut self.out, flate2::Compression::default());
        z.write_all(&self.chunk)?;
        z.finish()?;
        self.chunk.clear();
        self.chunk_docs = 0;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<Vec<u8>> {
        self.flush()?;
        let n_chunks = self.offsets.len() as u32;
        self.offsets.push(self.out.len() as u64);
        for o in &self.offsets {
            self.out.extend_from_slice(&o.to_le_bytes());
        }
        self.out.extend_from_slice(&self.doc_count.to_le_bytes());
        self.out.extend_from_slice(&n_chunks.to_le_bytes());
        Ok(self.out)
    }
}

/// A RECORDS section read in place: its chunks decode independently.
pub struct Records {
    section: Bytes,
    offsets: Vec<u64>,
    doc_count: u32,
}

impl Records {
    pub fn open(section: Bytes) -> io::Result<Self> {
        let len = section.len();
        if len < 16 {
            return Err(invalid("truncated"));
        }
        let doc_count = section.u32_at(len - 8);
        let n_chunks = section.u32_at(len - 4) as usize;
        let table = 8 * (n_chunks + 1);
        let table_at = (len - 8).checked_sub(table).ok_or_else(|| invalid("chunk table past start"))?;
        let offsets: Vec<u64> = (0..=n_chunks).map(|i| section.u64_at(table_at + 8 * i)).collect();
        if offsets.windows(2).any(|w| w[0] + 8 > w[1]) || offsets[n_chunks] != table_at as u64 || (n_chunks > 0 && offsets[0] != 0) {
            return Err(invalid("chunk offsets"));
        }
        let records = Self { section, offsets, doc_count };
        let total: u64 = (0..n_chunks).map(|c| u64::from(records.chunk_docs(c))).sum();
        if total != u64::from(doc_count) {
            return Err(invalid(format!("chunks hold {total} documents, header says {doc_count}")));
        }
        Ok(records)
    }

    pub fn doc_count(&self) -> u32 {
        self.doc_count
    }

    pub fn chunk_count(&self) -> usize {
        self.offsets.len() - 1
    }

    /// Documents in chunk `c`.
    pub fn chunk_docs(&self, c: usize) -> u32 {
        self.section.u32_at(self.offsets[c] as usize)
    }

    /// Chunk `c` inflated: its records as `varint(len) record` one after another.
    pub fn chunk_raw(&self, c: usize) -> io::Result<Vec<u8>> {
        let (a, b) = (self.offsets[c] as usize, self.offsets[c + 1] as usize);
        let raw_len = self.section.u32_at(a + 4) as usize;
        let mut raw = Vec::with_capacity(raw_len);
        flate2::read::ZlibDecoder::new(&self.section[a + 8..b]).read_to_end(&mut raw)?;
        if raw.len() != raw_len {
            return Err(invalid(format!("chunk {c} inflates to {} bytes, expected {raw_len}", raw.len())));
        }
        Ok(raw)
    }

    /// The records of chunk `c`, in order.
    pub fn chunk(&self, c: usize) -> io::Result<Vec<Vec<u8>>> {
        let n = self.chunk_docs(c) as usize;
        let raw = self.chunk_raw(c)?;
        let mut c_ = super::record::Cursor::new(&raw);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let len = c_.varint().map_err(|e| invalid(e.to_string()))? as usize;
            out.push(c_.bytes(len).map_err(|e| invalid(e.to_string()))?.to_vec());
        }
        if c_.remaining() != 0 {
            return Err(invalid(format!("chunk {c} has bytes after its records")));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_across_chunks() {
        for n in [0usize, 1, 5000] {
            let recs: Vec<Vec<u8>> = (0..n).map(|i| format!("record {i} {}", "y".repeat(i % 900)).into_bytes()).collect();
            let mut w = RecordsWriter::new();
            for r in &recs {
                w.push(r).unwrap();
            }
            let records = Records::open(Bytes::from_vec(w.finish().unwrap())).unwrap();
            assert_eq!(records.doc_count() as usize, n);
            let back: Vec<Vec<u8>> = (0..records.chunk_count()).flat_map(|c| records.chunk(c).unwrap()).collect();
            assert_eq!(back, recs);
            if n == 5000 {
                assert!(records.chunk_count() > 1);
            }
        }
    }
}
