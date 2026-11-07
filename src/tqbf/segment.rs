//! A segment's stored documents in TQBF form: the FIELDS, DOCS and IDS sections of the
//! segment file, which also holds the segment's index sections (spec §4).

use super::container::{kind, SegmentFile, SegmentWriter, SEGMENT_WIDE};
use super::docstore::{self, DocStore, DocsOptions, RecordRef};
use super::ids::{self, IdMap};
use super::record;
use super::registry::FieldRegistry;
use std::io;
use std::path::Path;
use std::sync::Arc;

pub const FILE_NAME: &str = "segment.tqbf";

pub struct RecordStore {
    file: SegmentFile,
    pub registry: Arc<FieldRegistry>,
    docs: DocStore,
    ids: IdMap,
}

impl RecordStore {
    /// Build from records in doc id order, all encoded against `registry` (or a prefix of it).
    pub fn build(registry: &FieldRegistry, records: &[&[u8]], opts: &DocsOptions) -> io::Result<Self> {
        let external_ids: Vec<&str> = records
            .iter()
            .map(|r| record::record_id(r).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())))
            .collect::<io::Result<_>>()?;
        let fields = registry.encode();
        let schema_hash = xxhash_rust::xxh3::xxh3_64(&fields);
        let mut w = SegmentWriter::new();
        w.add(kind::FIELDS, SEGMENT_WIDE, fields);
        w.add(kind::DOCS, SEGMENT_WIDE, docstore::build_section(records, opts)?);
        w.add(kind::IDS, SEGMENT_WIDE, ids::build_section(&external_ids)?);
        Self::from_file(SegmentFile::from_vec(w.finish(records.len() as u32, schema_hash))?)
    }

    /// A store of a DOCS and an IDS section of `doc_count` documents encoded against `registry`
    /// (or a prefix of it): a merge's (`index::DocStore::merge`).
    pub fn assemble(registry: &FieldRegistry, docs: Vec<u8>, ids: Vec<u8>, doc_count: u32) -> io::Result<Self> {
        let fields = registry.encode();
        let schema_hash = xxhash_rust::xxh3::xxh3_64(&fields);
        let mut w = SegmentWriter::new();
        w.add(kind::FIELDS, SEGMENT_WIDE, fields);
        w.add(kind::DOCS, SEGMENT_WIDE, docs);
        w.add(kind::IDS, SEGMENT_WIDE, ids);
        Self::from_file(SegmentFile::from_vec(w.finish(doc_count, schema_hash))?)
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        Self::from_file(SegmentFile::open(path)?)
    }

    pub fn from_file(file: SegmentFile) -> io::Result<Self> {
        let need = |k: u16, name: &str| {
            file.section(k, SEGMENT_WIDE)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("segment has no {name} section")))
        };
        let registry = FieldRegistry::decode(&need(kind::FIELDS, "FIELDS")?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let docs = DocStore::open(need(kind::DOCS, "DOCS")?)?;
        let ids = IdMap::open(need(kind::IDS, "IDS")?)?;
        if docs.total_docs() != file.doc_count || ids.len() != file.doc_count as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "segment section doc counts disagree"));
        }
        Ok(Self { file, registry: Arc::new(registry), docs, ids })
    }

    /// Add FIELDS, DOCS and IDS to a segment file being written (not copied until written).
    pub fn add_sections(&self, w: &mut SegmentWriter) {
        for k in [kind::FIELDS, kind::DOCS, kind::IDS] {
            if let Some(bytes) = self.file.section(k, SEGMENT_WIDE) {
                w.add_bytes(k, SEGMENT_WIDE, bytes);
            }
        }
    }

    /// xxh3 of the FIELDS section.
    pub fn schema_hash(&self) -> u64 {
        self.file.schema_hash
    }

    /// Write the store alone as a segment file and return a store reading it through a memory map.
    pub fn persist(&self, path: &Path) -> io::Result<Self> {
        let mut w = SegmentWriter::new();
        self.add_sections(&mut w);
        w.write(path, self.total_docs(), self.schema_hash())?;
        Self::open(path)
    }

    #[inline]
    pub fn total_docs(&self) -> u32 {
        self.file.doc_count
    }

    pub fn record(&self, doc_id: u32) -> io::Result<Option<RecordRef>> {
        self.docs.record(doc_id)
    }

    /// The document as Typesense returns it.
    pub fn value(&self, doc_id: u32) -> io::Result<Option<serde_json::Value>> {
        let Some(rec) = self.docs.record(doc_id)? else { return Ok(None) };
        record::to_value(&rec, &self.registry)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    #[inline]
    pub fn doc_id(&self, external_id: &str) -> Option<u32> {
        self.ids.get(external_id)
    }

    /// The IDS section.
    pub fn ids(&self) -> &IdMap {
        &self.ids
    }

    /// The DOCS section.
    pub fn docs(&self) -> &DocStore {
        &self.docs
    }

    /// Bytes of the FIELDS, DOCS and IDS sections (heap before `persist`, mapped after).
    pub fn stored_bytes(&self) -> usize {
        [kind::FIELDS, kind::DOCS, kind::IDS]
            .iter()
            .filter_map(|&k| self.file.section(k, SEGMENT_WIDE))
            .map(|b| b.len())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_persist_reopen() {
        let mut reg = FieldRegistry::new();
        let docs: Vec<serde_json::Value> = (0..500)
            .map(|i| serde_json::json!({"id": format!("{i}"), "name": format!("Företag {i}"), "n": i, "f": 0.5 * i as f64, "tags": []}))
            .collect();
        let recs: Vec<Vec<u8>> = docs.iter().map(|d| record::encode(d, d["id"].as_str().unwrap(), &mut reg).unwrap()).collect();
        let refs: Vec<&[u8]> = recs.iter().map(Vec::as_slice).collect();
        let store = RecordStore::build(&reg, &refs, &DocsOptions::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mapped = store.persist(&dir.path().join(FILE_NAME)).unwrap();
        for s in [&store, &mapped] {
            assert_eq!(s.total_docs(), 500);
            assert_eq!(s.doc_id("123"), Some(123));
            assert_eq!(s.value(123).unwrap().unwrap(), docs[123]);
        }
    }
}
