//! TQBF v6 container (spec §3): 64-byte header, section table, aligned sections, xxh3 per section.

use super::bytes::Bytes;
use std::io;
use std::path::Path;
use std::sync::Arc;
use xxhash_rust::xxh3::xxh3_64;

pub const MAGIC: &[u8; 4] = b"TQBF";
pub const VERSION: u16 = 6;
const HEADER_LEN: usize = 64;
const ENTRY_LEN: usize = 40;
pub const SEGMENT_WIDE: u16 = 0xFFFF;

pub mod kind {
    pub const FIELDS: u16 = 1;
    pub const DOCS: u16 = 2;
    pub const IDS: u16 = 3;
    /// The collection schema, as `POST /collections` takes it (spec §5.1).
    pub const SCHEMA: u16 = 4;
    /// A docs-only file's records, in zlib chunks (spec §10).
    pub const RECORDS: u16 = 5;
    /// The documents' `_seq_id`s of a segment the realtime write path built (`index::SeqIds`).
    pub const SEQ: u16 = 6;
    pub const TERMS: u16 = 10;
    pub const RAWTERMS: u16 = 11;
    pub const POSTINGS: u16 = 12;
    pub const FREQS: u16 = 13;
    pub const NORMS: u16 = 14;
    /// A name-matching field's keys (`crate::names`).
    pub const KEYS: u16 = 16;
    pub const VALUES: u16 = 20;
    pub const VALUE_DOCS: u16 = 21;
    pub const DOC_ORDS: u16 = 22;
    pub const NUM_VALUES: u16 = 30;
    pub const BOOL: u16 = 40;
    pub const GEO: u16 = 50;
    pub const VECTORS: u16 = 60;
}

pub mod header_flags {
    pub const HAS_INDEX: u16 = 1 << 0;
    pub const TRANSPORT_CODECS: u16 = 1 << 1;
}

/// Section codecs (spec §3). Only `NONE` is ever mapped; `ZSTD` exists in transit.
pub mod codec {
    pub const NONE: u8 = 0;
    pub const ZSTD: u8 = 1;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionEntry {
    pub kind: u16,
    pub field_id: u16,
    pub codec: u8,
    pub offset: u64,
    pub length: u64,
    pub raw_length: u64,
    pub xxh3: u64,
}

fn align_for(len: usize) -> usize {
    if len > 64 * 1024 { 4096 } else { 64 }
}

/// One section to write: its stored bytes, and for a transport-coded section the codec and the
/// length and checksum of the bytes it decodes to.
struct Pending {
    kind: u16,
    field_id: u16,
    bytes: Bytes,
    codec: u8,
    raw_length: u64,
    raw_xxh3: u64,
}

/// Builds a segment file from sections, in memory or streamed to disk. Sections are laid out in
/// call order.
#[derive(Default)]
pub struct SegmentWriter {
    sections: Vec<Pending>,
    flags: u16,
}

impl SegmentWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, kind: u16, field_id: u16, bytes: Vec<u8>) {
        self.add_bytes(kind, field_id, Bytes::from_vec(bytes));
    }

    /// Add a section that already lives in a buffer or a mapped file; it is copied only when the
    /// file is written.
    pub fn add_bytes(&mut self, kind: u16, field_id: u16, bytes: Bytes) {
        self.sections.push(Pending { kind, field_id, bytes, codec: codec::NONE, raw_length: 0, raw_xxh3: 0 });
    }

    pub fn set_flag(&mut self, flag: u16) {
        self.flags |= flag;
    }

    /// For transport: store every section that zstd shrinks by at least `min_gain` (0.1 = 10%)
    /// as a zstd frame (codec 1). The receiver restores them with `remove_transport_codecs`.
    pub fn compress_for_transport(&mut self, level: i32, min_gain: f64) -> io::Result<()> {
        use rayon::prelude::*;
        let packed: Vec<Option<Vec<u8>>> = self
            .sections
            .par_iter()
            .map(|p| {
                if p.codec != codec::NONE || p.bytes.len() < 4096 {
                    return Ok(None);
                }
                let z = zstd::bulk::compress(&p.bytes, level)?;
                Ok(((z.len() as f64) < p.bytes.len() as f64 * (1.0 - min_gain)).then_some(z))
            })
            .collect::<io::Result<_>>()?;
        for (p, z) in self.sections.iter_mut().zip(packed) {
            if let Some(z) = z {
                p.raw_length = p.bytes.len() as u64;
                p.raw_xxh3 = xxh3_64(&p.bytes);
                p.codec = codec::ZSTD;
                p.bytes = Bytes::from_vec(z);
                self.flags |= header_flags::TRANSPORT_CODECS;
            }
        }
        Ok(())
    }

    pub fn finish(self, doc_count: u32, schema_hash: u64) -> Vec<u8> {
        let (head, table, total) = self.layout(doc_count, schema_hash);
        let mut out = vec![0u8; total];
        out[..head.len()].copy_from_slice(&head);
        for (p, e) in self.sections.iter().zip(&table) {
            out[e.offset as usize..e.offset as usize + p.bytes.len()].copy_from_slice(&p.bytes);
        }
        out
    }

    /// Write the file atomically (temp file, fsync, rename), streaming each section from where
    /// it lives instead of assembling the file in memory.
    pub fn write(&self, path: &Path, doc_count: u32, schema_hash: u64) -> io::Result<()> {
        use std::io::Write;
        static ZEROS: [u8; 4096] = [0; 4096];
        let (head, table, _) = self.layout(doc_count, schema_hash);
        let tmp = unique_tmp_path(path);
        {
            let mut f = std::io::BufWriter::with_capacity(1 << 20, crate::persistence::disk::create(&tmp)?);
            f.write_all(&head)?;
            let mut pos = head.len();
            for (p, e) in self.sections.iter().zip(&table) {
                f.write_all(&ZEROS[..e.offset as usize - pos])?;
                f.write_all(&p.bytes)?;
                pos = e.offset as usize + p.bytes.len();
            }
            crate::persistence::disk::sync(&f.into_inner().map_err(|e| e.into_error())?, &tmp)?;
        }
        crate::persistence::disk::rename(&tmp, path)
    }

    /// Header and section table bytes, the table entries, and the file length.
    fn layout(&self, doc_count: u32, schema_hash: u64) -> (Vec<u8>, Vec<SectionEntry>, usize) {
        let n = self.sections.len();
        let mut pos = HEADER_LEN + n * ENTRY_LEN;
        let mut table = Vec::with_capacity(n);
        for p in &self.sections {
            let raw = p.codec == codec::NONE;
            let len = p.bytes.len();
            pos = pos.next_multiple_of(align_for(if raw { len } else { p.raw_length as usize }));
            table.push(SectionEntry {
                kind: p.kind,
                field_id: p.field_id,
                codec: p.codec,
                offset: pos as u64,
                length: len as u64,
                raw_length: if raw { len as u64 } else { p.raw_length },
                xxh3: if raw { xxh3_64(&p.bytes) } else { p.raw_xxh3 },
            });
            pos += len;
        }
        let mut out = vec![0u8; HEADER_LEN + n * ENTRY_LEN];
        let mut t = Vec::with_capacity(n * ENTRY_LEN);
        for e in &table {
            t.extend_from_slice(&e.kind.to_le_bytes());
            t.extend_from_slice(&e.field_id.to_le_bytes());
            t.push(e.codec);
            t.extend_from_slice(&[0u8; 3]);
            t.extend_from_slice(&e.offset.to_le_bytes());
            t.extend_from_slice(&e.length.to_le_bytes());
            t.extend_from_slice(&e.raw_length.to_le_bytes());
            t.extend_from_slice(&e.xxh3.to_le_bytes());
        }
        out[HEADER_LEN..HEADER_LEN + t.len()].copy_from_slice(&t);
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut h = Vec::with_capacity(HEADER_LEN);
        h.extend_from_slice(MAGIC);
        h.extend_from_slice(&VERSION.to_le_bytes());
        h.extend_from_slice(&self.flags.to_le_bytes());
        h.extend_from_slice(&doc_count.to_le_bytes());
        h.extend_from_slice(&(n as u32).to_le_bytes());
        h.extend_from_slice(&schema_hash.to_le_bytes());
        h.extend_from_slice(&created.to_le_bytes());
        h.extend_from_slice(&(HEADER_LEN as u64).to_le_bytes());
        h.extend_from_slice(&xxh3_64(&t).to_le_bytes());
        h.resize(HEADER_LEN, 0);
        out[..HEADER_LEN].copy_from_slice(&h);
        (out, table, pos)
    }
}

/// Writes a segment file section by section as the sections are produced: each section is
/// appended at the alignment `SegmentWriter` gives it, the section table follows the last one and
/// the header (which points at the table) is written last. Nothing is held but the table. The
/// file is written under a temp name and renamed by `finish`.
pub struct StreamingSegmentWriter {
    file: std::io::BufWriter<std::fs::File>,
    tmp: std::path::PathBuf,
    path: std::path::PathBuf,
    pos: u64,
    table: Vec<SectionEntry>,
    flags: u16,
}

impl StreamingSegmentWriter {
    pub fn create(path: &Path) -> io::Result<Self> {
        use std::io::Write;
        let tmp = unique_tmp_path(path);
        let mut file = std::io::BufWriter::with_capacity(4 << 20, crate::persistence::disk::create(&tmp)?);
        file.write_all(&[0u8; HEADER_LEN])?;
        Ok(Self { file, tmp, path: path.to_path_buf(), pos: HEADER_LEN as u64, table: Vec::new(), flags: 0 })
    }

    pub fn set_flag(&mut self, flag: u16) {
        self.flags |= flag;
    }

    /// Append a section given in parts (written one after another, hashed as one).
    pub fn add_parts<'a>(&mut self, kind: u16, field_id: u16, parts: impl IntoIterator<Item = &'a [u8]> + Clone) -> io::Result<()> {
        use std::io::Write;
        static ZEROS: [u8; 4096] = [0; 4096];
        let len: u64 = parts.clone().into_iter().map(|p| p.len() as u64).sum();
        let at = self.pos.next_multiple_of(align_for(len as usize) as u64);
        self.file.write_all(&ZEROS[..(at - self.pos) as usize])?;
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        for p in parts {
            h.update(p);
            self.file.write_all(p)?;
        }
        self.pos = at + len;
        self.table.push(SectionEntry { kind, field_id, codec: codec::NONE, offset: at, length: len, raw_length: len, xxh3: h.digest() });
        Ok(())
    }

    pub fn add(&mut self, kind: u16, field_id: u16, bytes: &[u8]) -> io::Result<()> {
        self.add_parts(kind, field_id, [bytes])
    }

    /// Bytes written so far.
    pub fn len(&self) -> u64 {
        self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Write the table and the header, fsync, and rename the file into place.
    pub fn finish(mut self, doc_count: u32, schema_hash: u64) -> io::Result<u64> {
        use std::io::{Seek, Write};
        let table_at = self.pos.next_multiple_of(8);
        self.file.write_all(&[0u8; 8][..(table_at - self.pos) as usize])?;
        let mut t = Vec::with_capacity(self.table.len() * ENTRY_LEN);
        for e in &self.table {
            t.extend_from_slice(&e.kind.to_le_bytes());
            t.extend_from_slice(&e.field_id.to_le_bytes());
            t.push(e.codec);
            t.extend_from_slice(&[0u8; 3]);
            t.extend_from_slice(&e.offset.to_le_bytes());
            t.extend_from_slice(&e.length.to_le_bytes());
            t.extend_from_slice(&e.raw_length.to_le_bytes());
            t.extend_from_slice(&e.xxh3.to_le_bytes());
        }
        self.file.write_all(&t)?;
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut h = Vec::with_capacity(HEADER_LEN);
        h.extend_from_slice(MAGIC);
        h.extend_from_slice(&VERSION.to_le_bytes());
        h.extend_from_slice(&self.flags.to_le_bytes());
        h.extend_from_slice(&doc_count.to_le_bytes());
        h.extend_from_slice(&(self.table.len() as u32).to_le_bytes());
        h.extend_from_slice(&schema_hash.to_le_bytes());
        h.extend_from_slice(&created.to_le_bytes());
        h.extend_from_slice(&table_at.to_le_bytes());
        h.extend_from_slice(&xxh3_64(&t).to_le_bytes());
        h.resize(HEADER_LEN, 0);
        let end = table_at + t.len() as u64;
        let mut f = self.file.into_inner().map_err(|e| e.into_error())?;
        f.seek(std::io::SeekFrom::Start(0))?;
        f.write_all(&h)?;
        crate::persistence::disk::sync(&f, &self.tmp)?;
        drop(f);
        crate::persistence::disk::rename(&self.tmp, &self.path)?;
        Ok(end)
    }
}

/// A temp sibling of `path`, unique within the process, for an atomic publish: write it, fsync,
/// rename onto `path`. A reader that still maps the previous file keeps reading its old inode
/// (rename only swaps the directory entry), so a re-persist never truncates a live mapping; an
/// in-place `File::create` over the final path would (a compaction did, with a SIGBUS).
pub fn unique_tmp_path(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!("{}.{unique}.tmp", path.file_name().unwrap_or_default().to_string_lossy()))
}

/// A parsed segment file, backed by a memory map or a heap buffer.
#[derive(Clone)]
pub struct SegmentFile {
    bytes: Bytes,
    pub flags: u16,
    pub doc_count: u32,
    pub schema_hash: u64,
    pub created_unix: i64,
    pub sections: Vec<SectionEntry>,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

impl SegmentFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::parse(Self::map(path)?)
    }

    fn map(path: &Path) -> io::Result<Bytes> {
        let file = std::fs::File::open(path)?;
        // SAFETY: segment files are immutable once written (written to a temp name, then renamed).
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        #[cfg(unix)]
        let _ = mmap.advise(memmap2::Advice::Random);
        Ok(Bytes::from_map(Arc::new(mmap)))
    }

    /// Open a file that may still carry transport codecs (only to remove them).
    pub fn open_transport(path: &Path) -> io::Result<Self> {
        Self::parse_with(Self::map(path)?, true)
    }

    pub fn from_vec(v: Vec<u8>) -> io::Result<Self> {
        Self::parse(Bytes::from_vec(v))
    }

    pub fn parse(bytes: Bytes) -> io::Result<Self> {
        Self::parse_with(bytes, false)
    }

    fn parse_with(bytes: Bytes, allow_codecs: bool) -> io::Result<Self> {
        if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
            return Err(invalid("not a TQBF file"));
        }
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        if version != VERSION {
            return Err(invalid(format!("TQBF version {version}, expected {VERSION}")));
        }
        let flags = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
        let doc_count = bytes.u32_at(8);
        let count = bytes.u32_at(12) as usize;
        let schema_hash = bytes.u64_at(16);
        let created_unix = bytes.u64_at(24) as i64;
        let table_offset = bytes.u64_at(32) as usize;
        let table_xxh3 = bytes.u64_at(40);
        let table_end = table_offset + count * ENTRY_LEN;
        if table_end > bytes.len() {
            return Err(invalid("section table past end of file"));
        }
        if xxh3_64(&bytes[table_offset..table_end]) != table_xxh3 {
            return Err(invalid("section table checksum mismatch"));
        }
        let mut sections = Vec::with_capacity(count);
        for i in 0..count {
            let o = table_offset + i * ENTRY_LEN;
            let e = SectionEntry {
                kind: u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap()),
                field_id: u16::from_le_bytes(bytes[o + 2..o + 4].try_into().unwrap()),
                codec: bytes[o + 4],
                offset: bytes.u64_at(o + 8),
                length: bytes.u64_at(o + 16),
                raw_length: bytes.u64_at(o + 24),
                xxh3: bytes.u64_at(o + 32),
            };
            if e.offset as usize + e.length as usize > bytes.len() {
                return Err(invalid(format!("section {i} past end of file")));
            }
            if e.codec != codec::NONE && !(allow_codecs && e.codec == codec::ZSTD) {
                return Err(invalid(format!("section {i} still carries transport codec {}", e.codec)));
            }
            sections.push(e);
        }
        Ok(Self { bytes, flags, doc_count, schema_hash, created_unix, sections })
    }

    pub fn section(&self, kind: u16, field_id: u16) -> Option<Bytes> {
        self.sections
            .iter()
            .find(|e| e.kind == kind && e.field_id == field_id)
            .map(|e| self.bytes.slice(e.offset as usize, e.length as usize))
    }

    /// Recompute every section checksum (upload path; normal loads trust the table checksum).
    pub fn verify(&self) -> io::Result<()> {
        for (i, e) in self.sections.iter().enumerate() {
            let s = &self.bytes[e.offset as usize..(e.offset + e.length) as usize];
            if xxh3_64(s) != e.xxh3 {
                return Err(invalid(format!("section {i} (kind {}) checksum mismatch", e.kind)));
            }
        }
        Ok(())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// True when some section is still transport-coded.
    pub fn has_transport_codecs(&self) -> bool {
        self.sections.iter().any(|e| e.codec != codec::NONE)
    }
}

/// Rewrite a segment file in place with every section zstd shrinks by `min_gain` or more stored
/// as a zstd frame (`torque-admin build --transport`). The result is for upload only.
pub fn compress_file_for_transport(path: &Path, level: i32, min_gain: f64) -> io::Result<()> {
    let tmp = unique_tmp_path(path);
    {
        let file = SegmentFile::open(path)?;
        let mut w = SegmentWriter { flags: file.flags, ..Default::default() };
        for e in &file.sections {
            w.add_bytes(e.kind, e.field_id, file.bytes.slice(e.offset as usize, e.length as usize));
        }
        w.compress_for_transport(level, min_gain)?;
        w.write(&tmp, file.doc_count, file.schema_hash)?;
    }
    // The mapping of `path` is gone here, so the rename also works on Windows.
    std::fs::rename(&tmp, path)
}

/// Rewrite `src` to `dst` with every transport-coded section decoded, verifying each section's
/// checksum on the way (the upload path, spec §10.3). The result is a mappable segment file.
pub fn remove_transport_codecs(src: &Path, dst: &Path) -> io::Result<()> {
    let file = SegmentFile::open_transport(src)?;
    let mut w = SegmentWriter { flags: file.flags & !header_flags::TRANSPORT_CODECS, ..Default::default() };
    for (i, e) in file.sections.iter().enumerate() {
        let stored = file.bytes.slice(e.offset as usize, e.length as usize);
        let bytes = if e.codec == codec::ZSTD {
            let raw = zstd::bulk::decompress(&stored, e.raw_length as usize)?;
            if raw.len() as u64 != e.raw_length {
                return Err(invalid(format!("section {i} decodes to {} bytes, expected {}", raw.len(), e.raw_length)));
            }
            Bytes::from_vec(raw)
        } else {
            stored
        };
        if xxh3_64(&bytes) != e.xxh3 {
            return Err(invalid(format!("section {i} (kind {}) checksum mismatch", e.kind)));
        }
        w.add_bytes(e.kind, e.field_id, bytes);
    }
    w.write(dst, file.doc_count, file.schema_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_parse_round_trip_with_alignment() {
        let mut w = SegmentWriter::new();
        w.add(kind::FIELDS, SEGMENT_WIDE, b"fields".to_vec());
        w.add(kind::DOCS, SEGMENT_WIDE, vec![7u8; 70_000]);
        w.add(kind::IDS, SEGMENT_WIDE, vec![1, 2, 3]);
        let file = SegmentFile::from_vec(w.finish(5, 99)).unwrap();
        assert_eq!(file.doc_count, 5);
        assert_eq!(file.schema_hash, 99);
        assert_eq!(&*file.section(kind::FIELDS, SEGMENT_WIDE).unwrap(), b"fields");
        let docs = file.sections.iter().find(|e| e.kind == kind::DOCS).unwrap();
        assert_eq!(docs.offset % 4096, 0);
        assert_eq!(file.section(kind::DOCS, SEGMENT_WIDE).unwrap().len(), 70_000);
        assert!(file.section(kind::GEO, SEGMENT_WIDE).is_none());
        file.verify().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tqbf");
        let mut w = SegmentWriter::new();
        w.add(kind::FIELDS, SEGMENT_WIDE, b"fields".to_vec());
        w.add_bytes(kind::DOCS, SEGMENT_WIDE, file.section(kind::DOCS, SEGMENT_WIDE).unwrap());
        w.add(kind::IDS, SEGMENT_WIDE, vec![1, 2, 3]);
        w.write(&path, 5, 99).unwrap();
        let streamed = std::fs::read(&path).unwrap();
        assert_eq!(&streamed[..24], &file.as_bytes()[..24]);
        assert_eq!(&streamed[32..], &file.as_bytes()[32..], "same bytes except the creation time");

        let mut bad = file.as_bytes().to_vec();
        let ids = file.sections.iter().find(|e| e.kind == kind::IDS).unwrap();
        bad[ids.offset as usize] ^= 1;
        assert!(SegmentFile::from_vec(bad).unwrap().verify().is_err());
    }

    #[test]
    fn another_version_is_refused() {
        let mut w = SegmentWriter::new();
        w.add(kind::FIELDS, SEGMENT_WIDE, b"fields".to_vec());
        let mut built = w.finish(1, 0);
        assert_eq!(u16::from_le_bytes(built[4..6].try_into().unwrap()), VERSION);
        assert!(SegmentFile::from_vec(built.clone()).is_ok());
        for old in [3u16, 4, 5] {
            built[4..6].copy_from_slice(&old.to_le_bytes());
            assert!(SegmentFile::from_vec(built.clone()).is_err(), "version {old}");
        }
    }

    #[test]
    fn transport_codecs_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (sent, got) = (dir.path().join("sent.tqbf"), dir.path().join("got.tqbf"));
        let compressible: Vec<u8> = (0..200_000u32).map(|i| (i % 7) as u8).collect();
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let random: Vec<u8> = (0..50_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 32) as u8
            })
            .collect();
        let mut w = SegmentWriter::new();
        w.add(kind::FIELDS, SEGMENT_WIDE, b"fields".to_vec());
        w.add(kind::DOCS, SEGMENT_WIDE, compressible.clone());
        w.add(kind::IDS, SEGMENT_WIDE, random.clone());
        w.set_flag(header_flags::HAS_INDEX);
        w.compress_for_transport(3, 0.1).unwrap();
        w.write(&sent, 9, 7).unwrap();
        assert!(SegmentFile::open(&sent).is_err(), "a coded file is not mappable");
        let coded = SegmentFile::open_transport(&sent).unwrap();
        let docs = coded.sections.iter().find(|e| e.kind == kind::DOCS).unwrap();
        assert_eq!((docs.codec, docs.raw_length), (codec::ZSTD, compressible.len() as u64));
        assert!(docs.length < docs.raw_length / 10);
        assert_eq!(coded.sections.iter().find(|e| e.kind == kind::IDS).unwrap().codec, codec::NONE);
        drop(coded);
        remove_transport_codecs(&sent, &got).unwrap();
        let file = SegmentFile::open(&got).unwrap();
        file.verify().unwrap();
        assert_eq!((file.doc_count, file.schema_hash, file.flags), (9, 7, header_flags::HAS_INDEX));
        assert_eq!(&*file.section(kind::DOCS, SEGMENT_WIDE).unwrap(), compressible.as_slice());
        assert_eq!(&*file.section(kind::IDS, SEGMENT_WIDE).unwrap(), random.as_slice());
    }
}
