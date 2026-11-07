//! Index persistence: a collection's segment set on disk.
//!
//! Layout: `data_dir/indexes/{collection}/segments/`:
//! - `manifest.bin`, the commit point: the segments in order with their bases, document counts and
//!   deletion files, the schema they were saved under, `next_seq` and the write-ahead log's
//!   checkpoint. [4B magic "TRQX"][4B version LE][4B CRC32 LE][LZ4-compressed JSON], replaced
//!   whole (written to a temporary name, synced, renamed, the directory synced).
//! - `{id:016x}.tqbf`, a segment (`Segment::id`): one TQBF file, mapped, holding the stored
//!   documents, every index section and the `_seq_id`s (docs/tqbf-segment-format.md). Written
//!   once.
//! - `{id:016x}-{n}.del`, the segment's deletion mask as a save found it (portable roaring, TRQX).
//!   Written once; a save that finds the mask changed writes the next `n`.
//!
//! A save (`save_bundle`) writes the segments not on disk yet and the masks changed since the last
//! save, syncs them and the directory, replaces the manifest, then removes the files the previous
//! manifest held and this one does not: it costs what changed, not the collection. A crash before
//! the manifest leaves the previous one and every file it holds; files no manifest holds are
//! removed when the collection next loads (`remove_strays`).

use super::disk;
use crate::bitmap::Bitmap;
use crate::index::geo::GeoIndex;
use crate::index::inverted::InvertedIndex;
use crate::index::reference::ReferenceIndex;
use crate::index::values::{BoolIndex, ValueIndex};
use crate::index::{DocStore, Index, IndexBundle, Segment, SeqIds};
use crate::schema::{FieldId, Schema};
use crate::tqbf::container::{kind, SegmentFile, SegmentWriter, SEGMENT_WIDE};
use crate::vector::VectorIndex;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAGIC: &[u8; 4] = b"TRQX";
const FORMAT_VERSION: u32 = 4; // TRQX container version (the manifest and the deletion files)
const MANIFEST: &str = "manifest.bin";

fn index_dir(data_dir: &Path, collection_name: &str) -> PathBuf {
    data_dir.join("indexes").join(collection_name)
}

/// The directory of a collection's segment set.
fn segments_dir(data_dir: &Path, collection_name: &str) -> PathBuf {
    index_dir(data_dir, collection_name).join("segments")
}

fn segment_file(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id:016x}.tqbf"))
}

fn mask_file(dir: &Path, id: u64, n: u64) -> PathBuf {
    dir.join(format!("{id:016x}-{n}.del"))
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Write bytes with TRQX header + CRC32 + LZ4 compression, synced, under a temporary name renamed
/// into place; with `sync_dir` the rename is made durable too (else the caller syncs the
/// directory once for several files).
fn write_with_header(path: &Path, payload: &[u8], sync_dir: bool) -> io::Result<()> {
    let crc = crc32fast::hash(payload);
    let compressed = lz4_flex::compress_prepend_size(payload);
    let tmp_path = crate::tqbf::container::unique_tmp_path(path);
    let mut combined = Vec::with_capacity(12 + compressed.len());
    combined.extend_from_slice(MAGIC);
    combined.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    combined.extend_from_slice(&crc.to_le_bytes());
    combined.extend_from_slice(&compressed);
    let mut file = disk::create(&tmp_path)?;
    file.write_all(&combined)?;
    disk::sync(&file, &tmp_path)?;
    drop(file);
    disk::rename(&tmp_path, path)?;
    if sync_dir {
        disk::sync_parent(path)?;
    }
    Ok(())
}

/// Read and verify TRQX header, decompress LZ4, verify CRC32.
fn read_with_header(path: &Path) -> io::Result<Vec<u8>> {
    let data = std::fs::read(path)?;
    if data.len() < 12 {
        return Err(invalid("File too small"));
    }
    if &data[0..4] != MAGIC {
        return Err(invalid(format!("Invalid magic: expected TRQX, got {:?}", &data[0..4])));
    }
    let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(invalid(format!(
            "{}: format version {version}, this build reads {FORMAT_VERSION} (saved by another version: re-import the collection)",
            path.display()
        )));
    }
    let expected_crc = u32::from_le_bytes(data[8..12].try_into().unwrap());
    let decompressed = lz4_flex::decompress_size_prepended(&data[12..]).map_err(|e| invalid(format!("LZ4 decompress failed: {e}")))?;
    let actual_crc = crc32fast::hash(&decompressed);
    if actual_crc != expected_crc {
        return Err(invalid(format!("CRC32 mismatch: expected {expected_crc:#x}, got {actual_crc:#x}")));
    }
    Ok(decompressed)
}

// -- Segment files --

/// Write a segment file: the collection schema (when given), the stored documents, the
/// `_seq_id`s the segment keeps, then each field's sections in field order, each streamed from
/// the buffer or mapped file it lives in.
pub fn write_segment(index: &Index, schema_json: Option<&[u8]>, path: &Path) -> io::Result<()> {
    let mut w = SegmentWriter::new();
    w.set_flag(crate::tqbf::container::header_flags::HAS_INDEX);
    if let Some(json) = schema_json {
        w.add(kind::SCHEMA, SEGMENT_WIDE, json.to_vec());
    }
    let records = index.doc_store.records();
    if let Some(records) = records {
        records.add_sections(&mut w);
    }
    if !index.seq.is_empty() {
        w.add(kind::SEQ, SEGMENT_WIDE, index.seq.to_bytes());
    }
    let mut fids: Vec<FieldId> = index
        .inverted
        .keys()
        .chain(index.values.keys())
        .chain(index.bools.keys())
        .chain(index.geo_indexes.keys())
        .chain(index.vector_indexes.keys())
        .copied()
        .collect();
    fids.sort_unstable();
    fids.dedup();
    for fid in fids {
        if let Some(inv) = index.inverted.get(&fid) {
            inv.add_sections(fid, &mut w);
        }
        if let Some(v) = index.values.get(&fid) {
            v.add_sections(fid, &mut w);
        }
        if let Some(b) = index.bools.get(&fid) {
            b.add_sections(fid, &mut w);
        }
        if let Some(g) = index.geo_indexes.get(&fid) {
            g.add_sections(fid, &mut w);
        }
        if let Some(v) = index.vector_indexes.get(&fid) {
            v.add_sections(fid, &mut w);
        }
    }
    let schema_hash = records.map_or(0, |r| r.schema_hash());
    w.write(path, index.total_docs(), schema_hash)
}

/// Read a segment file, mapped: every section is read in place; only the field registry and the
/// section views are built. The segment is `id`, and saved (its file holds it).
pub fn open_segment(path: &Path, schema: &Schema, id: u64) -> io::Result<Segment> {
    let file = SegmentFile::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
    let doc_store = if file.section(kind::DOCS, SEGMENT_WIDE).is_some() {
        let records = crate::tqbf::segment::RecordStore::from_file(file.clone())?;
        if !schema.registry.adopt(&records.registry, schema) {
            return Err(invalid(format!("{}: stored field registry conflicts with the collection's", path.display())));
        }
        DocStore::from_records(records)
    } else {
        DocStore::empty()
    };
    let seq = match file.section(kind::SEQ, SEGMENT_WIDE) {
        Some(b) => SeqIds::from_bytes(&b).ok_or_else(|| invalid(format!("{}: malformed SEQ section", path.display())))?,
        None => SeqIds::default(),
    };
    let mut inverted = FxHashMap::default();
    let mut values = FxHashMap::default();
    let mut bools = FxHashMap::default();
    let mut geo_indexes = FxHashMap::default();
    let mut vector_indexes = FxHashMap::default();
    for e in &file.sections {
        match e.kind {
            kind::TERMS => {
                if let Some(inv) = InvertedIndex::open(&file, e.field_id)? {
                    inverted.insert(e.field_id, inv);
                }
            }
            kind::VALUE_DOCS => {
                if let Some(v) = ValueIndex::open(&file, e.field_id)? {
                    values.insert(e.field_id, v);
                }
            }
            kind::BOOL => {
                if let Some(b) = BoolIndex::open(&file, e.field_id)? {
                    bools.insert(e.field_id, b);
                }
            }
            kind::GEO => {
                if let Some(g) = GeoIndex::open(&file, e.field_id)? {
                    geo_indexes.insert(e.field_id, g);
                }
            }
            kind::VECTORS => {
                if let Some(v) = VectorIndex::open(&file, e.field_id)? {
                    vector_indexes.insert(e.field_id, v);
                }
            }
            _ => {}
        }
    }
    let mut all_docs = Bitmap::new();
    all_docs.add_range(0..doc_store.total_docs());
    let reference_indexes = ReferenceIndex::for_segment(schema, &values);
    Ok(Segment::from_parts(doc_store, inverted, values, bools, geo_indexes, vector_indexes, all_docs, reference_indexes, seq, id, true))
}

/// Write a segment's file into the collection's segment set directory and read it back mapped:
/// the streaming build, which so holds one segment in memory at a time. No manifest holds the
/// file until a save of the build's bundle (which writes no segment again).
pub fn write_and_open(data_dir: &Path, collection_name: &str, segment: &Segment, schema: &Schema) -> io::Result<Segment> {
    let dir = segments_dir(data_dir, collection_name);
    disk::create_dirs(&dir)?;
    let path = segment_file(&dir, segment.id);
    let schema_json = serde_json::to_vec(schema).map_err(io::Error::other)?;
    write_segment(segment, Some(&schema_json), &path)?;
    // the manifest that comes to hold it relies on its name being durable
    disk::sync_parent(&path)?;
    segment.mark_saved();
    let mut mapped = open_segment(&path, schema, segment.id)?;
    mapped.base_doc_id = segment.base_doc_id;
    Ok(mapped)
}

// -- The segment set --

/// Which schema of its collection a bundle is saved under: the collection's (by its creation
/// time) and the schema's version (`Collection::schema_version`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchemaStamp {
    pub created_at: u64,
    pub version: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest<'a> {
    segments: Vec<SavedSegment>,
    /// `IndexBundle::next_seq`.
    next_seq: u64,
    /// `IndexBundle::lsn`: the write-ahead log's checkpoint (`persistence::wal`).
    wal_lsn: u64,
    /// The collection's schema the segments were saved under, and its `SchemaStamp`: a change of
    /// the schema's fields commits here with its rebuilt segments, in the one file replace, so a
    /// crash before collections.json follows cannot part the two (`committed_schema`).
    schema: Cow<'a, Schema>,
    created_at: u64,
    schema_version: u64,
    /// The collection's field registry as the save found it (`tqbf::registry`, hex): every path the
    /// log's records up to `wal_lsn` carried, so those after it read against it.
    fields: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// A segment of a manifest.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct SavedSegment {
    id: u64,
    /// The global doc id of its doc 0.
    base: u32,
    docs: u32,
    /// Its deletion file's number (0: it deletes none) and how many documents the file deletes.
    mask: u64,
    deleted: u32,
}

impl Manifest<'_> {
    /// The names of the files it holds.
    fn files(&self) -> FxHashSet<String> {
        let mut files = FxHashSet::default();
        for s in &self.segments {
            files.insert(format!("{:016x}.tqbf", s.id));
            if s.mask > 0 {
                files.insert(format!("{:016x}-{}.del", s.id, s.mask));
            }
        }
        files
    }
}

fn read_manifest(dir: &Path) -> io::Result<Option<Manifest<'static>>> {
    let path = dir.join(MANIFEST);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = read_with_header(&path)?;
    serde_json::from_slice(&bytes).map(Some).map_err(|e| invalid(format!("{}: {e}", path.display())))
}

/// What a segment set's manifest holds, kept between saves, under the set's lock (one save of a
/// set at a time).
#[derive(Default)]
struct Committed {
    /// Read from the manifest on disk (or there is none), or written by this process.
    known: bool,
    /// The names of the files the manifest holds.
    files: FxHashSet<String>,
    /// Per segment the mask the manifest's deletion file holds, and the file's number. A write
    /// copies a mask it changes, so the same mask is an unchanged one.
    masks: FxHashMap<u64, (Arc<Bitmap>, u64)>,
    /// Files a manifest no longer holds that could not be removed (Windows refuses to remove a
    /// mapped file a search still reads): tried again at each commit.
    doomed: FxHashSet<String>,
}

impl Committed {
    /// Learn what the manifest on disk holds, unless known: a set this process has not loaded nor
    /// saved (one of a collection made again under the name).
    fn know(&mut self, dir: &Path) {
        if !self.known {
            if let Ok(Some(m)) = read_manifest(dir) {
                self.files = m.files();
            }
            self.known = true;
        }
    }
}

static SETS: OnceLock<Mutex<FxHashMap<PathBuf, Arc<Mutex<Committed>>>>> = OnceLock::new();

/// The lock and state of the segment set in `dir`.
fn set_state(dir: &Path) -> Arc<Mutex<Committed>> {
    let mut sets = SETS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(sets.entry(dir.to_path_buf()).or_default())
}

/// Forget the state of a segment set removed with its collection.
fn forget_state(dir: &Path) {
    let state = set_state(dir);
    *state.lock().unwrap_or_else(|e| e.into_inner()) = Committed::default();
}

/// Forget what this process knows of the segment sets under `data_dir`: a restart, in a test.
#[cfg(test)]
pub(crate) fn forget_under(data_dir: &Path) {
    let sets = SETS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    for (dir, state) in sets.iter().filter(|(d, _)| d.starts_with(data_dir)) {
        let _ = dir;
        *state.lock().unwrap_or_else(|e| e.into_inner()) = Committed::default();
    }
}

/// The files the manifest of a collection's segment set holds (None without one).
#[cfg(test)]
pub(crate) fn manifest_files(data_dir: &Path, collection_name: &str) -> Option<FxHashSet<String>> {
    read_manifest(&segments_dir(data_dir, collection_name)).ok().flatten().map(|m| m.files())
}

/// Replace the manifest, then remove the files the previous one held and this one does not (and
/// those earlier commits could not remove). How many it removed.
fn commit(dir: &Path, state: &mut Committed, manifest: &Manifest, masks: FxHashMap<u64, (Arc<Bitmap>, u64)>) -> io::Result<usize> {
    let bytes = serde_json::to_vec(manifest).map_err(io::Error::other)?;
    write_with_header(&dir.join(MANIFEST), &bytes, true)?;
    let files = manifest.files();
    let mut gone: Vec<String> = state.files.difference(&files).cloned().collect();
    gone.extend(state.doomed.drain().filter(|f| !files.contains(f)));
    let mut removed = 0;
    for name in gone {
        match disk::remove_file(&dir.join(&name)) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::debug!(file = %dir.join(&name).display(), error = %e, "A file the manifest no longer holds is not removed yet");
                state.doomed.insert(name);
            }
        }
    }
    state.files = files;
    state.masks = masks;
    Ok(removed)
}

/// What a save did.
#[derive(Debug, Default)]
pub struct Saved {
    pub elapsed: Duration,
    pub segments_written: usize,
    pub masks_written: usize,
    pub bytes_written: u64,
    pub files_removed: usize,
    /// The bundle's segments built in memory (those it wrote, and any an earlier save wrote),
    /// read from their files (mapped), for `Collection::save` to put in their place.
    pub mapped: Vec<Arc<Segment>>,
}

/// Save a bundle: the segments not on disk yet, the deletion masks changed since the last save,
/// then the manifest (`commit`). Segments without documents are left out.
pub fn save_bundle(data_dir: &Path, collection_name: &str, bundle: &IndexBundle, schema: &Schema, stamp: SchemaStamp) -> io::Result<Saved> {
    let start = Instant::now();
    let dir = segments_dir(data_dir, collection_name);
    let state = set_state(&dir);
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    disk::create_dirs(&dir)?;
    state.know(&dir);

    let mut entries = Vec::with_capacity(bundle.segments.len());
    let mut masks: FxHashMap<u64, (Arc<Bitmap>, u64)> = FxHashMap::default();
    let mut new_segments: Vec<&Arc<Segment>> = Vec::new();
    let mut new_masks: Vec<(u64, u64, &Bitmap)> = Vec::new();
    for (i, seg) in bundle.segments.iter().enumerate() {
        if seg.total_docs() == 0 {
            continue;
        }
        if !seg.is_saved() {
            new_segments.push(seg);
        }
        let mask = &bundle.deleted_per_segment[i];
        let (n, deleted) = if mask.is_empty() {
            (0, 0)
        } else {
            let n = match state.masks.get(&seg.id) {
                Some((saved, n)) if Arc::ptr_eq(saved, mask) => *n,
                prev => {
                    let n = prev.map_or(1, |p| p.1 + 1);
                    new_masks.push((seg.id, n, mask));
                    n
                }
            };
            masks.insert(seg.id, (Arc::clone(mask), n));
            (n, mask.cardinality() as u32)
        };
        entries.push(SavedSegment { id: seg.id, base: seg.base_doc_id, docs: seg.total_docs(), mask: n, deleted });
    }

    // the files, written side by side (each synced), then their names made durable at once
    let schema_json = if new_segments.is_empty() { Vec::new() } else { serde_json::to_vec(schema).map_err(io::Error::other)? };
    let segment_bytes = new_segments
        .par_iter()
        .map(|seg| {
            let path = segment_file(&dir, seg.id);
            write_segment(seg, Some(&schema_json), &path)?;
            Ok(std::fs::metadata(&path).map_or(0, |m| m.len()))
        })
        .collect::<io::Result<Vec<u64>>>()?;
    let mask_bytes = new_masks
        .par_iter()
        .map(|&(id, n, mask)| {
            let bytes = mask.serialize_portable();
            write_with_header(&mask_file(&dir, id, n), &bytes, false)?;
            Ok(bytes.len() as u64)
        })
        .collect::<io::Result<Vec<u64>>>()?;
    if !new_segments.is_empty() || !new_masks.is_empty() {
        disk::sync_dir(&dir)?;
    }
    for seg in &new_segments {
        seg.mark_saved();
    }

    let manifest = Manifest {
        segments: entries,
        next_seq: bundle.next_seq(),
        wal_lsn: bundle.lsn,
        schema: Cow::Borrowed(schema),
        created_at: stamp.created_at,
        schema_version: stamp.version,
        fields: hex(&schema.registry.load().encode()),
    };
    let files_removed = commit(&dir, &mut state, &manifest, masks)?;
    drop(state);

    let in_memory: Vec<&Arc<Segment>> = bundle.segments.iter().filter(|s| s.total_docs() > 0 && !s.is_mapped()).collect();
    let mapped = in_memory
        .par_iter()
        .filter_map(|seg| match open_segment(&segment_file(&dir, seg.id), schema, seg.id) {
            Ok(mut m) => {
                m.base_doc_id = seg.base_doc_id;
                Some(Arc::new(m))
            }
            Err(e) => {
                tracing::warn!(collection = collection_name, segment = seg.id, error = %e, "A saved segment could not be read back; it stays in memory");
                None
            }
        })
        .collect();
    let saved = Saved {
        elapsed: start.elapsed(),
        segments_written: new_segments.len(),
        masks_written: new_masks.len(),
        bytes_written: segment_bytes.iter().chain(&mask_bytes).sum(),
        files_removed,
        mapped,
    };
    tracing::debug!(
        collection = collection_name,
        segments = manifest.segments.len(),
        segments_written = saved.segments_written,
        masks_written = saved.masks_written,
        bytes_written = saved.bytes_written,
        files_removed = saved.files_removed,
        save_ms = saved.elapsed.as_millis() as u64,
        "Saved"
    );
    Ok(saved)
}

/// Install finished segment files as a collection's segment set (the upload path, spec §10.3):
/// move each file into place under a new segment id, commit the manifest, and map the set. The
/// files must lie on the data directory's volume (they are renamed, not copied).
pub fn install_segments(data_dir: &Path, collection_name: &str, files: &[PathBuf], schema: &Arc<Schema>, stamp: SchemaStamp) -> io::Result<IndexBundle> {
    let dir = segments_dir(data_dir, collection_name);
    let state = set_state(&dir);
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    disk::create_dirs(&dir)?;
    state.know(&dir);
    let mut segments = Vec::with_capacity(files.len());
    let mut entries = Vec::with_capacity(files.len());
    let mut base = 0u32;
    for f in files {
        let id = crate::index::next_segment_id();
        let path = segment_file(&dir, id);
        disk::rename(f, &path)?;
        let mut seg = open_segment(&path, schema, id)?;
        if seg.total_docs() == 0 {
            drop(seg);
            disk::remove_file(&path).ok();
            continue;
        }
        seg.base_doc_id = base;
        base += seg.total_docs();
        entries.push(SavedSegment { id, base: seg.base_doc_id, docs: seg.total_docs(), mask: 0, deleted: 0 });
        segments.push(Arc::new(seg));
    }
    if !segments.is_empty() {
        disk::sync_dir(&dir)?;
    }
    let manifest = Manifest {
        segments: entries,
        next_seq: 0,
        // installed segments are a new collection's: its log begins after them
        wal_lsn: 0,
        schema: Cow::Borrowed(&**schema),
        created_at: stamp.created_at,
        schema_version: stamp.version,
        fields: hex(&schema.registry.load().encode()),
    };
    commit(&dir, &mut state, &manifest, FxHashMap::default())?;
    if segments.is_empty() {
        return Ok(IndexBundle::empty(Arc::clone(schema)));
    }
    let deleted_per_segment = segments.iter().map(|_| Arc::new(Bitmap::new())).collect();
    Ok(IndexBundle { schema: Arc::clone(schema), total_docs: base, segments, deleted_per_segment, lsn: 0, next_seq: 0, memory: 0 })
}

/// The schema a collection's bundle was last saved under, by its manifest, and its stamp; none
/// without a manifest (or one this build cannot read).
pub fn committed_schema(data_dir: &Path, collection_name: &str) -> Option<(Schema, SchemaStamp)> {
    let manifest = read_manifest(&segments_dir(data_dir, collection_name)).ok()??;
    let stamp = SchemaStamp { created_at: manifest.created_at, version: manifest.schema_version };
    Some((manifest.schema.into_owned(), stamp))
}

/// Load the saved bundle of the collection made at `created_at`: every segment mapped, the deletion
/// masks read. None without a manifest, or with one of an earlier collection of the name (deleted,
/// its files not removed yet: the first save removes them); an error when the manifest, a segment
/// or a mask cannot be read (or a format of another version).
pub fn load_bundle(data_dir: &Path, collection_name: &str, schema: &Arc<Schema>, created_at: u64) -> io::Result<Option<IndexBundle>> {
    let dir = segments_dir(data_dir, collection_name);
    let state = set_state(&dir);
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(manifest) = read_manifest(&dir)? else {
        return Ok(None);
    };
    if manifest.created_at != created_at {
        tracing::info!(collection = collection_name, "The saved segments are an earlier collection's of the name: the first save removes them");
        state.known = true;
        state.files = manifest.files();
        return Ok(None);
    }
    let mut segments = Vec::with_capacity(manifest.segments.len());
    let mut deleted_per_segment = Vec::with_capacity(manifest.segments.len());
    let mut masks = FxHashMap::default();
    for e in &manifest.segments {
        crate::index::note_segment_id(e.id);
        let mut seg = open_segment(&segment_file(&dir, e.id), schema, e.id)?;
        if seg.total_docs() != e.docs {
            return Err(invalid(format!("segment {:016x} of `{collection_name}` holds {} documents, its manifest {}", e.id, seg.total_docs(), e.docs)));
        }
        seg.base_doc_id = e.base;
        let mask = if e.mask == 0 {
            Bitmap::new()
        } else {
            let path = mask_file(&dir, e.id, e.mask);
            let mask = Bitmap::deserialize_portable(&read_with_header(&path)?).map_err(|err| invalid(format!("{}: {err}", path.display())))?;
            if mask.cardinality() != u64::from(e.deleted) {
                return Err(invalid(format!("{}: deletes {} documents, its manifest {}", path.display(), mask.cardinality(), e.deleted)));
            }
            mask
        };
        let mask = Arc::new(mask);
        if e.mask > 0 {
            masks.insert(e.id, (Arc::clone(&mask), e.mask));
        }
        segments.push(Arc::new(seg));
        deleted_per_segment.push(mask);
    }
    state.known = true;
    state.files = manifest.files();
    state.masks = masks;
    // the paths the log's records up to the checkpoint carried (a segment's own may hold fewer)
    let fields = unhex(&manifest.fields)
        .and_then(|b| crate::tqbf::registry::FieldRegistry::decode(&b).ok())
        .ok_or_else(|| invalid(format!("{}: unreadable field registry", dir.join(MANIFEST).display())))?;
    if !schema.registry.adopt(&fields, schema) {
        return Err(invalid(format!("{}: field registry conflicts with the collection's", dir.join(MANIFEST).display())));
    }
    if segments.is_empty() {
        let mut empty = IndexBundle::empty(Arc::clone(schema));
        empty.lsn = manifest.wal_lsn;
        empty.next_seq = manifest.next_seq;
        return Ok(Some(empty));
    }
    let total_docs = IndexBundle::compute_live_total_docs(&segments, &deleted_per_segment);
    Ok(Some(IndexBundle {
        schema: Arc::clone(schema),
        segments,
        deleted_per_segment,
        total_docs,
        lsn: manifest.wal_lsn,
        next_seq: manifest.next_seq,
        memory: 0,
    }))
}

/// Remove what the manifest of a collection's segment set does not hold: files of saves a crash
/// cut short or a removal a mapped file refused, temporary files, and anything of an earlier
/// layout. Only with the manifest read (nothing is known to be garbage otherwise), and only while
/// nothing writes the collection's files (as it loads). How many entries it removed.
pub fn remove_strays(data_dir: &Path, collection_name: &str) -> usize {
    let dir = segments_dir(data_dir, collection_name);
    let state = set_state(&dir);
    let _state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(Some(manifest)) = read_manifest(&dir) else { return 0 };
    let keep = manifest.files();
    let mut removed = 0;
    let mut remove = |path: PathBuf| {
        let r = if path.is_dir() { disk::remove_dir_all(&path) } else { disk::remove_file(&path) };
        match r {
            Ok(()) => removed += 1,
            Err(e) => tracing::debug!(path = %path.display(), error = %e, "A file no manifest holds could not be removed"),
        }
    };
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != MANIFEST && !keep.contains(name.as_ref()) {
            remove(entry.path());
        }
    }
    // the collection's directory holds its segment set alone
    for entry in std::fs::read_dir(index_dir(data_dir, collection_name)).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy() != "segments" {
            remove(entry.path());
        }
    }
    if removed > 0 {
        tracing::info!(collection = collection_name, removed, "Removed files no manifest holds");
    }
    removed
}

/// Delete index files for a collection.
/// Remove a collection's segment set for good (its removal durable): a crash after finds it gone.
pub fn delete_index(data_dir: &Path, collection_name: &str) -> io::Result<()> {
    let dir = index_dir(data_dir, collection_name);
    forget_state(&segments_dir(data_dir, collection_name));
    if dir.exists() {
        disk::remove_dir_all(&dir)?;
        disk::sync_parent(&dir)?;
    }
    Ok(())
}

/// Check if a persisted index exists for a collection.
pub fn has_index(data_dir: &Path, collection_name: &str) -> bool {
    segments_dir(data_dir, collection_name).join(MANIFEST).exists()
}

/// Total disk size of all persisted files for a collection (walks directory tree).
pub fn index_disk_size(data_dir: &Path, collection_name: &str) -> u64 {
    let dir = index_dir(data_dir, collection_name);
    if !dir.exists() {
        return 0;
    }
    walkdir_size(&dir)
}

fn walkdir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let ft = entry.file_type().unwrap_or_else(|_| std::fs::symlink_metadata(entry.path()).unwrap().file_type());
            if ft.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if ft.is_dir() {
                total += walkdir_size(&entry.path());
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tqbf::container::unique_tmp_path;
    use crate::schema::{Document, FieldDef, FieldType, FieldValue, IndexMode, Language};

    fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
        let tmp = unique_tmp_path(path);
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(bytes).unwrap();
        f.sync_all().unwrap();
        drop(f);
        std::fs::rename(&tmp, path).unwrap();
    }

    /// The segment writer (segment.tqbf) publishes
    /// via a tmp file + `rename` precisely so a re-persist can't truncate a
    /// file a reader still has `mmap`'d. Pin that invariant: a mapping taken
    /// before a rewrite keeps reading the OLD bytes (its inode survives the
    /// rename), while a fresh open sees the NEW bytes. An in-place
    /// `File::create` (O_TRUNC) over the same path would truncate the mapped
    /// inode out from under the reader -> SIGBUS (the memory_skills crash).
    #[test]
    fn atomic_rewrite_preserves_live_mmap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("field_0_freqs.bin");

        let old = [0xAAu8; 4096];
        write_atomic(&path, &old);
        let file = std::fs::File::open(&path).unwrap();
        let live = unsafe { memmap2::Mmap::map(&file).unwrap() };
        assert_eq!(&live[..], &old[..]);

        let new = [0xBBu8; 8192];
        write_atomic(&path, &new);

        assert_eq!(&live[..], &old[..], "live mmap must survive an atomic rewrite");
        assert_eq!(std::fs::read(&path).unwrap(), &new[..]);
    }

    fn make_schema(name: &str) -> Schema {
        let mut s = Schema {
            name: name.into(),
            fields: vec![FieldDef {
                name: "title".into(),
                field_type: FieldType::String,
                facet: false,
                index: true,
                optional: false,
                sort: false,
                stem: false,
                infix: false,
                locale: None,
                num_dim: None,
                vec_dist: None,
                id: 0,
                reference: None,
                drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            }],
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: IndexMode::Batch,
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        s.assign_field_ids();
        s
    }

    fn segment(schema: &Schema, ids: &[usize], base: u32) -> Arc<Segment> {
        let docs = ids
            .iter()
            .map(|i| Document {
                id: 0,
                external_id: format!("doc{i}"),
                language: Language::default(),
                fields: vec![FieldValue::Text(format!("hello{i}"))].into(),
                record: None,
            })
            .collect();
        let mut seg = crate::ingest::build_index(schema, docs);
        seg.base_doc_id = base;
        Arc::new(seg)
    }

    fn bundle(schema: &Arc<Schema>, segments: Vec<Arc<Segment>>, masks: Vec<Arc<Bitmap>>) -> IndexBundle {
        let total_docs = IndexBundle::compute_live_total_docs(&segments, &masks);
        IndexBundle { schema: Arc::clone(schema), segments, deleted_per_segment: masks, total_docs, lsn: 7, next_seq: 0, memory: 0 }
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    /// A save writes the segments not on disk and the masks changed since the last, and removes
    /// what the manifest held and holds no more; a load gives the bundle back as saved.
    #[test]
    fn a_save_writes_what_changed_and_removes_what_went() {
        let dir = tempfile::tempdir().unwrap();
        let schema = Arc::new(make_schema("inc"));
        let set = segments_dir(dir.path(), "inc");
        let (a, b) = (segment(&schema, &[0, 1, 2], 0), segment(&schema, &[3, 4], 3));
        let none = || Arc::new(Bitmap::new());
        let first = bundle(&schema, vec![Arc::clone(&a), Arc::clone(&b)], vec![none(), none()]);
        let saved = save_bundle(dir.path(), "inc", &first, &schema, Default::default()).unwrap();
        assert_eq!((saved.segments_written, saved.masks_written, saved.mapped.len()), (2, 0, 2));
        assert!(saved.mapped.iter().all(|m| m.is_saved()));
        assert_eq!(files(&set).len(), 3, "two segments and the manifest");

        // nothing changed: the manifest alone
        let again = save_bundle(dir.path(), "inc", &first, &schema, Default::default()).unwrap();
        assert_eq!((again.segments_written, again.masks_written, again.files_removed), (0, 0, 0));

        // a deletion in `a`: its mask alone, and once
        let mut mask = Bitmap::new();
        mask.add(1);
        let mask = Arc::new(mask);
        let second = bundle(&schema, vec![Arc::clone(&a), Arc::clone(&b)], vec![Arc::clone(&mask), none()]);
        let s = save_bundle(dir.path(), "inc", &second, &schema, Default::default()).unwrap();
        assert_eq!((s.segments_written, s.masks_written), (0, 1));
        assert!(set.join(format!("{:016x}-1.del", a.id)).exists());
        let s = save_bundle(dir.path(), "inc", &second, &schema, Default::default()).unwrap();
        assert_eq!(s.masks_written, 0, "the same mask is not written again");

        // `b` merged away into `c`: `c` written, `b` removed
        let c = segment(&schema, &[3, 4, 5], 3);
        let third = bundle(&schema, vec![Arc::clone(&a), Arc::clone(&c)], vec![Arc::clone(&mask), none()]);
        let s = save_bundle(dir.path(), "inc", &third, &schema, Default::default()).unwrap();
        assert_eq!((s.segments_written, s.masks_written, s.files_removed), (1, 0, 1));
        assert!(!set.join(format!("{:016x}.tqbf", b.id)).exists());

        let loaded = load_bundle(dir.path(), "inc", &schema, 0).unwrap().unwrap();
        assert_eq!(loaded.segments.iter().map(|s| (s.id, s.base_doc_id, s.total_docs())).collect::<Vec<_>>(), [(a.id, 0, 3), (c.id, 3, 3)]);
        assert!(loaded.deleted_per_segment[0].contains(1) && loaded.deleted_per_segment[0].cardinality() == 1);
        assert_eq!((loaded.total_docs(), loaded.lsn), (5, 7));
        assert!(loaded.segments.iter().all(|s| s.is_saved()));
        // the loaded masks are the saved ones: a save of the loaded bundle writes nothing
        let s = save_bundle(dir.path(), "inc", &loaded, &schema, Default::default()).unwrap();
        assert_eq!((s.segments_written, s.masks_written, s.files_removed), (0, 0, 0));
    }

    /// The `_seq_id`s a segment keeps go into its file and come back with it.
    #[test]
    fn seq_ids_are_saved_with_their_segment() {
        let dir = tempfile::tempdir().unwrap();
        let schema = Arc::new(make_schema("seq"));
        let mut seg = crate::ingest::build_index(&schema, vec![Document { id: 0, external_id: "x".into(), language: Language::default(), fields: vec![FieldValue::Text("a".into())].into(), record: None }, Document { id: 0, external_id: "y".into(), language: Language::default(), fields: vec![FieldValue::Text("b".into())].into(), record: None }]);
        seg.seq = SeqIds::from_ids([4, 9]);
        let mut b = bundle(&schema, vec![Arc::new(seg)], vec![Arc::new(Bitmap::new())]);
        b.next_seq = 12;
        save_bundle(dir.path(), "seq", &b, &schema, Default::default()).unwrap();
        let loaded = load_bundle(dir.path(), "seq", &schema, 0).unwrap().unwrap();
        assert_eq!((loaded.segments[0].seq_id(0), loaded.segments[0].seq_id(1), loaded.next_seq()), (4, 9, 12));
    }

    /// Files no manifest holds (a save a crash cut short, an earlier layout) are not read, and go
    /// when the collection loads; the manifest's stay.
    #[test]
    fn files_no_manifest_holds_are_ignored_then_removed() {
        let dir = tempfile::tempdir().unwrap();
        let schema = Arc::new(make_schema("stray"));
        let a = segment(&schema, &[0], 0);
        save_bundle(dir.path(), "stray", &bundle(&schema, vec![Arc::clone(&a)], vec![Arc::new(Bitmap::new())]), &schema, Default::default()).unwrap();
        let set = segments_dir(dir.path(), "stray");
        // a crash after a segment and a mask were written, before the manifest
        let orphan = segment(&schema, &[1], 1);
        write_segment(&orphan, None, &segment_file(&set, orphan.id)).unwrap();
        std::fs::write(mask_file(&set, a.id, 5), b"junk").unwrap();
        std::fs::write(set.join("manifest.bin.3.tmp"), b"junk").unwrap();
        std::fs::create_dir_all(set.join("gen_4/seg_00000000")).unwrap();
        std::fs::write(index_dir(dir.path(), "stray").join("segment.tqbf"), b"junk").unwrap();

        let loaded = load_bundle(dir.path(), "stray", &schema, 0).unwrap().unwrap();
        assert_eq!((loaded.segments.len(), loaded.total_docs()), (1, 1));
        assert_eq!(remove_strays(dir.path(), "stray"), 5);
        assert_eq!(files(&set), [format!("{:016x}.tqbf", a.id), "manifest.bin".to_string()]);
        assert_eq!(files(&index_dir(dir.path(), "stray")), ["segments"]);
    }

    /// A save that fails before its manifest leaves the previous one in force.
    #[test]
    fn a_save_cut_short_leaves_the_previous_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let schema = Arc::new(make_schema("cut"));
        let a = segment(&schema, &[0, 1], 0);
        save_bundle(dir.path(), "cut", &bundle(&schema, vec![Arc::clone(&a)], vec![Arc::new(Bitmap::new())]), &schema, Default::default()).unwrap();
        // a later save's segment file written, the manifest not
        let b = segment(&schema, &[2], 2);
        write_segment(&b, None, &segment_file(&segments_dir(dir.path(), "cut"), b.id)).unwrap();
        let loaded = load_bundle(dir.path(), "cut", &schema, 0).unwrap().unwrap();
        assert_eq!(loaded.segments.iter().map(|s| s.id).collect::<Vec<_>>(), [a.id]);
        assert_eq!(loaded.total_docs(), 2);
    }

    /// A manifest of another format version is refused, not read.
    #[test]
    fn a_manifest_of_another_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let schema = Arc::new(make_schema("old"));
        let set = segments_dir(dir.path(), "old");
        std::fs::create_dir_all(&set).unwrap();
        let mut bytes = b"TRQX".to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        std::fs::write(set.join("manifest.bin"), bytes).unwrap();
        let e = load_bundle(dir.path(), "old", &schema, 0).unwrap_err();
        assert!(e.to_string().contains("re-import"), "{e}");
        assert_eq!(remove_strays(dir.path(), "old"), 0, "nothing is known to be garbage");
    }

    /// Uploaded segment files become the set, mapped.
    #[test]
    fn installed_segments_are_the_set() {
        let dir = tempfile::tempdir().unwrap();
        let schema = Arc::new(make_schema("up"));
        let parts: Vec<PathBuf> = (0..2)
            .map(|i| {
                let seg = segment(&schema, &[i * 10, i * 10 + 1], 0);
                let p = dir.path().join(format!("part{i}.tqbf"));
                write_segment(&seg, None, &p).unwrap();
                p
            })
            .collect();
        let installed = install_segments(dir.path(), "up", &parts, &schema, Default::default()).unwrap();
        assert_eq!((installed.segments.len(), installed.total_docs(), installed.segments[1].base_doc_id), (2, 4, 2));
        let loaded = load_bundle(dir.path(), "up", &schema, 0).unwrap().unwrap();
        assert_eq!(loaded.segments.iter().map(|s| s.id).collect::<Vec<_>>(), installed.segments.iter().map(|s| s.id).collect::<Vec<_>>());
        assert!(loaded.live_document("doc11").is_some());
    }
}
