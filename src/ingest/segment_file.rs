//! Segment files from a docs-only TQBF file (spec §10 step 2). A client writes SCHEMA, FIELDS and
//! RECORDS; the streaming builder (`crate::build`) indexes the records in place, in parallel, and
//! writes one segment file whose index sections equal a JSONL import of the same documents.

use crate::schema::Schema;
use crate::tqbf::container::{kind, SegmentFile, SEGMENT_WIDE};
use crate::tqbf::records::Records;
use crate::tqbf::registry::FieldRegistry;
use rayon::prelude::*;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// zstd level of the stored documents: 19 for offline builds, 3 on the server.
    pub docs_level: i32,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self { docs_level: 19 }
    }
}

/// A document the builder refused, as a JSONL import would have.
#[derive(Debug, Clone)]
pub struct Rejected {
    /// Position in the docs-only file.
    pub index: u64,
    pub id: String,
    pub error: String,
}

#[derive(Debug)]
pub struct BuildReport {
    pub schema: Schema,
    pub segments: Vec<(PathBuf, u32)>,
    pub rejected: Vec<Rejected>,
    pub millis: u128,
}

/// The parts of a docs-only file.
pub struct DocsOnly {
    pub schema: Schema,
    pub registry: FieldRegistry,
    pub records: Records,
}

/// The collection schema of a segment or docs-only file, as `POST /collections` takes it.
pub fn file_schema(file: &SegmentFile) -> io::Result<Schema> {
    let bytes = file.section(kind::SCHEMA, SEGMENT_WIDE).ok_or_else(|| invalid("file has no SCHEMA section"))?;
    let mut schema: Schema = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("SCHEMA section: {e}")))?;
    crate::schema::validate_schema(&schema).map_err(|e| invalid(format!("SCHEMA section: {}", e.message)))?;
    schema.assign_field_ids();
    Ok(schema)
}

/// True for a docs-only file (records, no index sections yet).
pub fn is_docs_only(file: &SegmentFile) -> bool {
    file.section(kind::RECORDS, SEGMENT_WIDE).is_some()
}

pub fn open_docs_only(file: &SegmentFile) -> io::Result<DocsOnly> {
    let schema = file_schema(file)?;
    if schema.fields.iter().any(|f| f.embed.is_some()) {
        return Err(invalid("auto-embedding fields need a server-side import (documents/import)"));
    }
    let fields = file.section(kind::FIELDS, SEGMENT_WIDE).ok_or_else(|| invalid("file has no FIELDS section"))?;
    let registry = FieldRegistry::decode(&fields).map_err(invalid)?;
    let records = Records::open(file.section(kind::RECORDS, SEGMENT_WIDE).ok_or_else(|| invalid("file has no RECORDS section"))?)?;
    if records.doc_count() != file.doc_count {
        return Err(invalid("RECORDS and header disagree on the document count"));
    }
    Ok(DocsOnly { schema, registry, records })
}

/// Build a docs-only file into `out_dir/seg_00000000.tqbf`: one run per stretch of chunks, all
/// indexed in parallel. Document ids must be unique (a repeated id fails the build).
pub fn build(input: &Path, out_dir: &Path, opts: &BuildOptions) -> io::Result<BuildReport> {
    let start = Instant::now();
    let file = SegmentFile::open(input)?;
    let DocsOnly { schema, registry, records } = open_docs_only(&file)?;
    let schema_json = file.section(kind::SCHEMA, SEGMENT_WIDE).ok_or_else(|| invalid("file has no SCHEMA section"))?;
    std::fs::create_dir_all(out_dir)?;
    let b = crate::build::SegmentBuilder::with_registry(
        &schema_json,
        crate::build::BuildOptions { docs_level: opts.docs_level, ..Default::default() },
        &registry,
    )?;
    let n = records.chunk_count();
    let groups = rayon::current_num_threads().clamp(1, n.max(1));
    let per = n.div_ceil(groups).max(1);
    let mut first = Vec::with_capacity(groups);
    let mut at = 0u64;
    for g in 0..groups {
        first.push(at);
        at += (g * per..((g + 1) * per).min(n)).map(|c| u64::from(records.chunk_docs(c))).sum::<u64>();
    }
    let rejected: Vec<Vec<Rejected>> = (0..groups)
        .into_par_iter()
        .map(|g| -> io::Result<Vec<Rejected>> {
            let mut run = b.run(g as u64);
            for c in g * per..((g + 1) * per).min(n) {
                run.add_batch(&records.chunk_raw(c)?)?;
            }
            Ok(run.end()?.into_iter().map(|r| Rejected { index: first[g] + r.index, id: r.id, error: r.error }).collect())
        })
        .collect::<io::Result<_>>()?;
    let path = out_dir.join("seg_00000000.tqbf");
    let report = b.finish(&path)?;
    Ok(BuildReport { schema, segments: vec![(path, report.docs)], rejected: rejected.into_iter().flatten().collect(), millis: start.elapsed().as_millis() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Document;
    use crate::tqbf::container::SegmentWriter;
    use crate::tqbf::record;
    use crate::tqbf::records::RecordsWriter;

    const SCHEMA: &str = r#"{"name":"t","enable_nested_fields":true,"fields":[
        {"name":"name","type":"string"},{"name":"n","type":"int32","facet":true},
        {"name":"tags","type":"string[]","facet":true,"optional":true},
        {"name":"loc","type":"geopoint","optional":true},{"name":"a.b","type":"float","optional":true}]}"#;

    fn docs() -> Vec<serde_json::Value> {
        let mut v: Vec<serde_json::Value> = (0..250)
            .map(|i| {
                serde_json::json!({"id": format!("d{i}"), "name": format!("Företag {i}"), "n": i % 7,
                    "tags": [format!("t{}", i % 3), null], "loc": [59.0 + f64::from(i) / 1000.0, 18.0],
                    "a": {"b": f64::from(i) * 0.1, "extra": [1, "x"]}, "notInSchema": {"k": i}})
            })
            .collect();
        v.push(serde_json::json!({"id": "bad", "name": "x", "n": "not a number"}));
        v.push(serde_json::json!({"id": "d3", "name": "duplicate", "n": 1}));
        v
    }

    fn docs_only_file(dir: &Path, docs: &[serde_json::Value]) -> PathBuf {
        let mut reg = FieldRegistry::new();
        let mut rw = RecordsWriter::new();
        for d in docs {
            rw.push(&record::encode(d, d["id"].as_str().unwrap(), &mut reg).unwrap()).unwrap();
        }
        let mut w = SegmentWriter::new();
        w.add(kind::SCHEMA, SEGMENT_WIDE, SCHEMA.as_bytes().to_vec());
        w.add(kind::FIELDS, SEGMENT_WIDE, reg.encode());
        w.add(kind::RECORDS, SEGMENT_WIDE, rw.finish().unwrap());
        let path = dir.join("docs.tqbf");
        w.write(&path, docs.len() as u32, 0).unwrap();
        path
    }

    #[test]
    fn built_segment_matches_a_json_import() {
        let dir = tempfile::tempdir().unwrap();
        let input = docs_only_file(dir.path(), &docs()[..251]);
        let opts = BuildOptions { docs_level: 3 };
        let report = build(&input, &dir.path().join("out"), &opts).unwrap();
        assert_eq!(report.segments.iter().map(|s| s.1).collect::<Vec<_>>(), vec![250]);
        assert_eq!(report.rejected.iter().map(|r| (r.index, r.id.as_str())).collect::<Vec<_>>(), vec![(250, "bad")]);

        // The same documents through the JSONL import path.
        let mut schema: Schema = serde_json::from_str(SCHEMA).unwrap();
        schema.assign_field_ids();
        let imported: Vec<Document> = docs()[..250]
            .iter()
            .enumerate()
            .map(|(i, d)| crate::ingest::parse_json_document(d, &schema, i as u32).unwrap())
            .collect();
        let reference = crate::ingest::build_index(&schema, imported);

        let mut i = 0u32;
        for (path, n) in &report.segments {
            let file = SegmentFile::open(path).unwrap();
            assert!(file.flags & crate::tqbf::container::header_flags::HAS_INDEX != 0);
            assert_eq!(file_schema(&file).unwrap().fields.len(), 5);
            let store = crate::tqbf::segment::RecordStore::from_file(file).unwrap();
            for local in 0..*n {
                let want = reference.doc_store.records().unwrap().value(i).unwrap().unwrap();
                assert_eq!(store.value(local).unwrap().unwrap(), want);
                i += 1;
            }
        }
        assert_eq!(i, 250);
    }

    #[test]
    fn a_repeated_id_fails_the_build() {
        let dir = tempfile::tempdir().unwrap();
        let input = docs_only_file(dir.path(), &docs());
        let e = build(&input, &dir.path().join("out"), &BuildOptions::default()).unwrap_err();
        assert!(e.to_string().contains("`d3` is added twice"), "{e}");
    }
}
