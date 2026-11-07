//! The streaming builder against the import path (`parse_json_document` + `build_index` +
//! `write_segment`, what `torque-admin build` did): every section but FIELDS and DOCS byte for
//! byte, every stored document equal, the same documents refused with the same messages.

use super::*;
use crate::tqbf::container::{kind, SegmentFile};
use crate::tqbf::segment::RecordStore;
use serde_json::{json, Value};
use std::path::PathBuf;

const SCHEMA: &str = r#"{"name":"t","enable_nested_fields":true,"token_separators":["+","-","@",".",":","/"],"fields":[
    {"name":"name","type":"string","name_matching":true},
    {"name":"names.nameOrIdentifier","type":"string[]","facet":true,"optional":true},
    {"name":"names.kind","type":"string[]","optional":true},
    {"name":"n","type":"int32","facet":true},
    {"name":"big","type":"int64","sort":true,"optional":true},
    {"name":"score","type":"float","sort":true,"optional":true},
    {"name":"tags","type":"string[]","facet":true,"optional":true},
    {"name":"nums","type":"int64[]","optional":true},
    {"name":"ints","type":"int32[]","facet":true,"optional":true},
    {"name":"fl","type":"float[]","optional":true},
    {"name":"ceased","type":"bool","optional":true},
    {"name":"flags","type":"bool[]","optional":true},
    {"name":"loc","type":"geopoint","optional":true},
    {"name":"locs","type":"geopoint[]","optional":true},
    {"name":"addr.city","type":"string","facet":true,"optional":true},
    {"name":"addr.zip","type":"int32","optional":true},
    {"name":"addr.geo","type":"geopoint[]","optional":true},
    {"name":"desc","type":"string","optional":true,"stem":true},
    {"name":"code","type":"string","sort":true,"optional":true},
    {"name":"note","type":"string","index":false,"optional":true},
    {"name":"unused","type":"string[]","facet":true,"optional":true}
]}"#;

const WORDS: &[&str] = &[
    "Aktiebolag", "AB", "Göteborg", "Malmö", "Stockholm", "fastighet", "Fastigheter", "bygg", "Byggnads", "konsult",
    "Åkeri", "ÖSTER", "naïve", "Café", "19.99", "x2", "handel", "med", "och", "i", "Sverige", "ΣΟΦΟΣ", "Straße",
    "İstanbul", "e-post", "info@foo.se", "www.example.com", "123-45", "AB-Ö", "running", "runs", "ﬁx", "Øresund",
    "Karlsson", "Carlsson", "Muhammad", "Mohammed", "Oedegaard", "Шипулин", "YAS\u{a0}AIR", "A\u{274}na",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn words(&mut self, lo: u64, hi: u64) -> String {
        let n = lo + self.below(hi - lo + 1);
        (0..n).map(|_| WORDS[self.below(WORDS.len() as u64) as usize]).collect::<Vec<_>>().join(" ")
    }

    fn geo(&mut self) -> Value {
        json!([55.0 + self.below(1_000_000) as f64 / 1e5, 11.0 + self.below(1_300_000) as f64 / 1e5])
    }
}

fn docs(n: usize, seed: u64) -> Vec<Value> {
    let mut r = Rng(seed | 1);
    (0..n)
        .map(|i| {
            let mut d = serde_json::Map::new();
            d.insert("id".into(), json!(format!("{}", 100_000 + i * 7)));
            if i % 101 != 7 {
                d.insert("name".into(), json!(r.words(1, 5)));
            }
            if r.below(5) > 0 {
                let mut names = Vec::new();
                for k in 0..r.below(4) {
                    let mut o = serde_json::Map::new();
                    o.insert("nameOrIdentifier".into(), json!(r.words(1, 3)));
                    match r.below(4) {
                        0 => {}
                        1 => {
                            o.insert("kind".into(), Value::Null);
                        }
                        _ => {
                            o.insert("kind".into(), json!(WORDS[(k as usize + i) % 5]));
                        }
                    }
                    if r.below(3) == 0 {
                        o.insert("firstSeen".into(), json!(1_600_000_000 + i));
                    }
                    names.push(Value::Object(o));
                }
                if !names.is_empty() && r.below(10) == 0 {
                    names.push(json!(5)); // not an object: null in the pivoted arrays
                }
                d.insert("names".into(), Value::Array(names));
            }
            d.insert("n".into(), if i % 97 == 13 { json!("x") } else { json!(i % 37) });
            if r.below(3) > 0 {
                d.insert("big".into(), json!((r.next() as i64) >> r.below(40)));
            }
            match r.below(6) {
                0 => {}
                1 => {
                    d.insert("score".into(), json!(-0.0));
                }
                2 => {
                    d.insert("score".into(), json!(5.0));
                }
                3 => {
                    d.insert("score".into(), json!(i));
                }
                _ => {
                    d.insert("score".into(), json!(i as f64 * 0.37 - 10.0));
                }
            }
            match r.below(5) {
                0 => {}
                1 => {
                    d.insert("tags".into(), json!([]));
                }
                2 => {
                    d.insert("tags".into(), json!(["b", null, "a", "b"]));
                }
                _ => {
                    d.insert("tags".into(), json!([WORDS[r.below(8) as usize], WORDS[r.below(12) as usize]]));
                }
            }
            if r.below(2) == 0 {
                d.insert("nums".into(), json!([i, -(i as i64), 1i64 << 40, null]));
            }
            if r.below(2) == 0 {
                d.insert("ints".into(), json!([i % 5, null, 4_000_000_000i64]));
            }
            if r.below(3) == 0 {
                d.insert("fl".into(), json!([0.1, 2.5, null, i]));
            }
            match r.below(4) {
                0 => {}
                1 => {
                    d.insert("ceased".into(), Value::Null);
                }
                _ => {
                    d.insert("ceased".into(), json!(r.below(2) == 0));
                }
            }
            match r.below(4) {
                0 => {}
                1 => {
                    d.insert("flags".into(), json!([true, null, false]));
                }
                2 => {
                    d.insert("flags".into(), json!([false]));
                }
                _ => {
                    d.insert("flags".into(), json!([true, true]));
                }
            }
            if r.below(3) > 0 {
                d.insert("loc".into(), r.geo());
            }
            if r.below(3) == 0 {
                d.insert("locs".into(), json!([r.geo(), r.geo(), null]));
            }
            if r.below(4) > 0 {
                let mut a = serde_json::Map::new();
                a.insert("city".into(), json!(["Göteborg", "Malmö", "Stockholm", "Åre"][r.below(4) as usize]));
                if r.below(2) == 0 {
                    a.insert("zip".into(), json!(41_000 + r.below(1000)));
                }
                d.insert("addr".into(), Value::Object(a));
                if r.below(20) == 0 {
                    // A literal dotted key after the object: the later key wins.
                    d.insert("addr.city".into(), json!("Literal"));
                }
            } else if r.below(3) == 0 {
                d.insert("addr".into(), json!([{"city": "Lund", "geo": [55.7, 13.19]}, {"zip": 22_100, "geo": [55.71, 13.2]}]));
            }
            if r.below(2) == 0 {
                d.insert("desc".into(), json!(r.words(3, 12)));
            }
            if r.below(3) == 0 {
                d.insert("code".into(), json!(format!("C{}", r.below(50))));
            }
            if r.below(4) == 0 {
                d.insert("note".into(), json!(r.words(1, 4)));
            }
            if r.below(8) == 0 {
                d.insert("language".into(), json!("sv"));
            }
            if r.below(5) == 0 {
                d.insert("extra".into(), json!({"a": [1, 2, {"b": 3}], "s": "x", "m": [1.5, "two", null, false]}));
            }
            Value::Object(d)
        })
        .collect()
}

fn schema(json: &str) -> Schema {
    let mut s: Schema = serde_json::from_str(json).unwrap();
    crate::schema::validate_schema(&s).map_err(|e| e.message).unwrap();
    s.assign_field_ids();
    s
}

/// The import path; returns the (index, error) of every refused document.
fn import_build(schema_json: &str, docs: &[Value], path: &Path) -> Vec<(usize, String)> {
    let schema = schema(schema_json);
    crate::index::docstore::set_docs_level(19);
    let mut seen = rustc_hash::FxHashSet::default();
    let mut parsed = Vec::new();
    let mut rejected = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        match crate::ingest::parse_json_document(d, &schema, 0) {
            Ok(mut doc) if seen.insert(doc.external_id.clone()) => {
                doc.id = parsed.len() as u32;
                parsed.push(doc);
            }
            Ok(doc) => rejected.push((i, format!("A document with id {} already exists.", doc.external_id))),
            Err(e) => rejected.push((i, e)),
        }
    }
    let index = crate::ingest::build_index(&schema, parsed);
    let schema_out = serde_json::to_vec(&schema).unwrap();
    crate::persistence::index::write_segment(&index, Some(&schema_out), path).unwrap();
    rejected
}

/// The streaming builder, `runs` runs started from several threads (run i gets the i-th
/// stretch of documents); returns the (index, error) of every refused document.
fn stream_build(schema_json: &str, docs: &[Value], runs: usize, opts: BuildOptions, path: &Path) -> (Vec<(usize, String)>, BuildReport) {
    let b = SegmentBuilder::new(schema_json.as_bytes(), opts).unwrap();
    let recs: Vec<Vec<u8>> = docs.iter().map(|d| b.encode_json(d).unwrap()).collect();
    let per = recs.len().div_ceil(runs);
    let chunks: Vec<(usize, &[Vec<u8>])> = recs.chunks(per).enumerate().collect();
    let mut rejected = Vec::new();
    std::thread::scope(|s| {
        // Started in reverse key order: the layout follows the keys, not the timing.
        let handles: Vec<_> = chunks
            .iter()
            .rev()
            .map(|&(k, chunk)| {
                let b = &b;
                s.spawn(move || {
                    let mut run = b.run(k as u64);
                    for r in chunk {
                        run.add(r).unwrap();
                    }
                    run.end().unwrap().into_iter().map(|r| (k * per + r.index as usize, r.error)).collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            rejected.extend(h.join().unwrap());
        }
    });
    rejected.sort();
    let report = b.finish(path).unwrap();
    (rejected, report)
}

fn compare(old: &Path, new: &Path) {
    let a = SegmentFile::open(old).unwrap();
    let b = SegmentFile::open(new).unwrap();
    assert_eq!(a.doc_count, b.doc_count);
    let order = |f: &SegmentFile| f.sections.iter().map(|e| (e.kind, e.field_id)).collect::<Vec<_>>();
    assert_eq!(order(&a), order(&b), "same sections in the same order");
    for e in &a.sections {
        if e.kind == kind::FIELDS || e.kind == kind::DOCS {
            continue;
        }
        let (x, y) = (a.section(e.kind, e.field_id).unwrap(), b.section(e.kind, e.field_id).unwrap());
        assert!(*x == *y, "section kind {} field {} differs ({} vs {} bytes)", e.kind, e.field_id, x.len(), y.len());
    }
    b.verify().unwrap();
    let (ra, rb) = (RecordStore::from_file(a.clone()).unwrap(), RecordStore::from_file(b.clone()).unwrap());
    for d in 0..a.doc_count {
        assert_eq!(ra.value(d).unwrap(), rb.value(d).unwrap(), "document {d}");
    }
}

fn tmp() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (old, new) = (dir.path().join("old.tqbf"), dir.path().join("new.tqbf"));
    (dir, old, new)
}

#[test]
fn one_run_equals_an_import() {
    let docs = docs(3000, 7);
    let (_d, old, new) = tmp();
    let want = import_build(SCHEMA, &docs, &old);
    let (got, report) = stream_build(SCHEMA, &docs, 1, BuildOptions::default(), &new);
    assert!(!want.is_empty(), "the sample has refused documents");
    assert_eq!(got, want);
    assert_eq!(report.docs as usize, docs.len() - want.len());
    compare(&old, &new);
}

#[test]
fn runs_and_pieces_equal_an_import() {
    let docs = docs(5000, 11);
    let (_d, old, new) = tmp();
    let want = import_build(SCHEMA, &docs, &old);
    // A piece every 256 documents, spilled; merged on 3 threads.
    let opts = BuildOptions { piece_bytes: 1, threads: 3, ..Default::default() };
    let (got, report) = stream_build(SCHEMA, &docs, 7, opts, &new);
    assert_eq!(got, want);
    assert!(report.pieces > 7);
    compare(&old, &new);
}

#[test]
fn in_memory_and_late_dictionary_give_the_same_file() {
    // Enough records for a dictionary (> 512 KB); runs keyed from 0 (dictionary while records
    // arrive) and from 1000 (dictionary at finish): the same stored documents, the same sections.
    let docs = docs(4000, 13);
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<PathBuf> = (0..3).map(|i| dir.path().join(format!("{i}.tqbf"))).collect();
    let b = |spill: bool| BuildOptions { piece_bytes: 1, spill_dir: spill.then(std::env::temp_dir), ..Default::default() };
    let (r0, _) = stream_build(SCHEMA, &docs, 5, b(false), &paths[0]);
    let (r1, _) = stream_build(SCHEMA, &docs, 5, b(true), &paths[1]);
    assert_eq!(r0, r1);
    // keys 1000.. : no early dictionary
    let builder = SegmentBuilder::new(SCHEMA.as_bytes(), b(true)).unwrap();
    for (k, chunk) in docs.chunks(800).enumerate() {
        let mut run = builder.run(1000 + k as u64);
        for d in chunk {
            run.add(&builder.encode_json(d).unwrap()).unwrap();
        }
        run.end().unwrap();
    }
    builder.finish(&paths[2]).unwrap();
    let files: Vec<Vec<u8>> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    // The stored documents' sections are byte-identical too (same dictionary, same blocks).
    let docs_of = |f: &[u8]| SegmentFile::from_vec(f.to_vec()).unwrap().section(kind::DOCS, crate::tqbf::container::SEGMENT_WIDE).unwrap().to_vec();
    assert_eq!(docs_of(&files[0]), docs_of(&files[1]));
    compare(&paths[0], &paths[1]);
    compare(&paths[0], &paths[2]);
}

#[test]
fn without_nested_fields() {
    let flat = SCHEMA.replace(r#""enable_nested_fields":true"#, r#""enable_nested_fields":false"#).replace("names.", "names_").replace("addr.", "addr_");
    let docs: Vec<Value> = docs(1500, 3)
        .into_iter()
        .enumerate()
        .map(|(i, mut d)| {
            // geopoint as an object, kept as one without nested fields
            if i % 3 == 0 {
                d["loc"] = json!({"lat": 57.7, "lng": 11.97});
            }
            d
        })
        .collect();
    let (_d, old, new) = tmp();
    let want = import_build(&flat, &docs, &old);
    let (got, _) = stream_build(&flat, &docs, 3, BuildOptions::default(), &new);
    assert_eq!(got, want);
    compare(&old, &new);
}

/// Same check on a real sample: TORQUE_BUILD_SCHEMA (collection schema) and TORQUE_BUILD_JSONL
/// (one document per line). `cargo test --release build::tests::sample -- --ignored --nocapture`
#[test]
#[ignore]
fn sample() {
    let (Ok(schema_path), Ok(jsonl)) = (std::env::var("TORQUE_BUILD_SCHEMA"), std::env::var("TORQUE_BUILD_JSONL")) else {
        panic!("set TORQUE_BUILD_SCHEMA and TORQUE_BUILD_JSONL");
    };
    let mut schema_json: Value = serde_json::from_str(&std::fs::read_to_string(schema_path).unwrap()).unwrap();
    if let Some(o) = schema_json.as_object_mut() {
        for k in ["created_at", "num_documents"] {
            o.remove(k);
        }
    }
    let schema_json = schema_json.to_string();
    let docs: Vec<Value> = std::fs::read_to_string(jsonl).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect();
    let (_d, old, new) = tmp();
    let t = std::time::Instant::now();
    let want = import_build(&schema_json, &docs, &old);
    let old_ms = t.elapsed().as_millis();
    let t = std::time::Instant::now();
    let (got, report) = stream_build(&schema_json, &docs, 4, BuildOptions::default(), &new);
    let new_ms = t.elapsed().as_millis();
    println!(
        "{} docs: import {old_ms} ms, streaming {new_ms} ms (merge {} ms), {} pieces, {} frozen bytes, file {} vs {} bytes",
        docs.len(),
        report.merge_millis,
        report.pieces,
        report.frozen_bytes,
        std::fs::metadata(&old).unwrap().len(),
        report.bytes
    );
    assert_eq!(got, want);
    compare(&old, &new);
}

/// Compare two segment files built from the same documents in the same order:
/// TORQUE_COMPARE_A, TORQUE_COMPARE_B. `cargo test --release build::tests::compare_files -- --ignored --nocapture`
#[test]
#[ignore]
fn compare_files() {
    let (Ok(a), Ok(b)) = (std::env::var("TORQUE_COMPARE_A"), std::env::var("TORQUE_COMPARE_B")) else {
        panic!("set TORQUE_COMPARE_A and TORQUE_COMPARE_B");
    };
    compare(Path::new(&a), Path::new(&b));
    println!("{a} and {b}: every index section identical, every document equal");
}

/// zstd speed and size on a docs-only file's records (TORQUE_DOCS_ONLY), as DOCS blocks:
/// `cargo test --release build::tests::zstd_levels -- --ignored --nocapture`
#[test]
#[ignore]
fn zstd_levels() {
    let path = std::env::var("TORQUE_DOCS_ONLY").expect("set TORQUE_DOCS_ONLY");
    let file = SegmentFile::open(Path::new(&path)).unwrap();
    let docs = crate::ingest::segment_file::open_docs_only(&file).unwrap();
    let mut recs: Vec<Vec<u8>> = Vec::new();
    for c in 0..docs.records.chunk_count() {
        recs.extend(docs.records.chunk(c).unwrap().into_iter().map(|r| r.to_vec()));
    }
    let raw: usize = recs.iter().map(Vec::len).sum();
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let (mut cur, mut size) = (Vec::<&[u8]>::new(), 0usize);
    for r in &recs {
        cur.push(r);
        size += r.len() + 4;
        if size >= 16 * 1024 {
            let lens: Vec<u32> = cur.iter().map(|r| r.len() as u32).collect();
            blocks.push(super::run::raw_block_for_test(&lens, &cur.concat()));
            cur.clear();
            size = 0;
        }
    }
    let mut sample = Vec::new();
    let mut sb = 0;
    for r in &recs {
        if sb >= MAX_SAMPLE_BYTES {
            break;
        }
        sb += r.len();
        sample.push(r.clone());
    }
    let dict = zstd::dict::from_samples(&sample, 16 * 1024).unwrap();
    println!("{} records, {:.1} MB raw, {} blocks, dict {} bytes", recs.len(), raw as f64 / 1e6, blocks.len(), dict.len());
    for level in [3, 6, 9, 12, 15, 17, 19] {
        for prepared in [true, false] {
            let t = std::time::Instant::now();
            let enc = zstd::dict::EncoderDictionary::copy(&dict, level);
            let mut comp = if prepared {
                zstd::bulk::Compressor::with_prepared_dictionary(&enc).unwrap()
            } else {
                zstd::bulk::Compressor::with_dictionary(level, &dict).unwrap()
            };
            let out: usize = blocks.iter().map(|b| comp.compress(b).unwrap().len()).sum();
            let s = t.elapsed().as_secs_f64();
            println!(
                "level {level:2} {}: {:6.1} MB/s, {:.3} KB per record ({:.1}% of raw)",
                if prepared { "prepared" } else { "loaded  " },
                raw as f64 / 1e6 / s,
                out as f64 / recs.len() as f64 / 1024.0,
                100.0 * out as f64 / raw as f64
            );
        }
    }
}

/// Cost and memory of adding records (TORQUE_DOCS_ONLY: a docs-only file), streamed chunk by
/// chunk into one run on one thread; TORQUE_BUILD_LEVEL (default 19).
/// `cargo test --release build::tests::add_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn add_cost() {
    let path = std::env::var("TORQUE_DOCS_ONLY").expect("set TORQUE_DOCS_ONLY");
    let level: i32 = std::env::var("TORQUE_BUILD_LEVEL").ok().and_then(|v| v.parse().ok()).unwrap_or(19);
    let file = SegmentFile::open(Path::new(&path)).unwrap();
    let schema_json = file.section(kind::SCHEMA, crate::tqbf::container::SEGMENT_WIDE).unwrap().to_vec();
    let docs = crate::ingest::segment_file::open_docs_only(&file).unwrap();
    let b = SegmentBuilder::new(&schema_json, BuildOptions { docs_level: level, ..Default::default() }).unwrap();
    let mut run = b.run(0);
    let mut n = 0usize;
    let mut add = std::time::Duration::ZERO;
    for c in 0..docs.records.chunk_count() {
        let recs: Vec<Vec<u8>> = docs
            .records
            .chunk(c)
            .unwrap()
            .into_iter()
            .map(|r| b.encode_json(&crate::tqbf::record::to_value(&r, &docs.registry).unwrap()).unwrap())
            .collect();
        let t = std::time::Instant::now();
        for r in &recs {
            run.add(r).unwrap();
        }
        add += t.elapsed();
        n += recs.len();
    }
    let t = std::time::Instant::now();
    run.end().unwrap();
    add += t.elapsed();
    let dir = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let rep = b.finish(&dir.path().join("s.tqbf")).unwrap();
    println!(
        "level {level}: add {:.1} us per document, finish {} ms, {} pieces, frozen {} B/doc, file {} B/doc",
        add.as_secs_f64() * 1e6 / n as f64,
        t.elapsed().as_millis(),
        rep.pieces,
        rep.frozen_bytes / n,
        rep.bytes as usize / n
    );
}
