//! The realtime write path's costs (docs/realtime-write-path.md, Verification):
//! - the segment a write builds, of one document and of a group of 32;
//! - merges of ten segments: in memory (1, 10, 100 documents each) and flushed (1,000 and 10,000
//!   documents of 60 words each, a tenth of them written again; 50,000 with
//!   `TORQUE_BENCH_LARGE=1`);
//! - a flush and its save after 500 updates of a 100,000-document collection;
//! - the write-ahead log's group commit, 1, 16 and 64 writers (each record synced: the drive's
//!   cache flush);
//! - a restart: a saved collection and 10,000 writes in its log after the save, replayed.
//!
//! `cargo bench --bench realtime_bench`; against an earlier run: `-- --save-baseline before`, then
//! `-- --baseline before`. The data directories are made in `TORQUE_BENCH_DIR` (default: the
//! temporary directory): its drive's sync decides the flush, the log and the restart.

// The server's allocator (src/main.rs): what a bench measures is what the server runs.
#[cfg(all(feature = "native", not(feature = "jemalloc"), target_env = "msvc"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::sync::Arc;
use std::time::{Duration, Instant};
use torque::collection::write::{self, Writes};
use torque::collection::{compaction, CollectionRegistry};
use torque::index::IndexBundle;
use torque::persistence::wal::{Entry, Logged, Wal, WriteOp};
use torque::schema::{Document, Schema};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn schema(realtime: bool) -> Schema {
    let mut sc: Schema = serde_json::from_value(serde_json::json!({
        "name": "bench",
        "index_mode": if realtime { "realtime" } else { "batch" },
        "default_sorting_field": "rank",
        "fields": [
            {"name": "title", "type": "string"},
            {"name": "body", "type": "string"},
            {"name": "tags", "type": "string[]", "facet": true},
            {"name": "city", "type": "string", "facet": true},
            {"name": "rank", "type": "int32", "sort": true},
            {"name": "big", "type": "int64", "facet": true},
            {"name": "ok", "type": "bool"}
        ]
    }))
    .unwrap();
    sc.assign_field_ids();
    sc
}

/// Documents of a vocabulary of 20,000 words, the common ones far more often (as text is).
struct Docs {
    rng: Rng,
    vocab: Vec<String>,
    words: usize,
}

impl Docs {
    fn new(seed: u64, words: usize) -> Docs {
        Docs { rng: Rng(seed | 1), vocab: (0..20_000).map(|i| format!("w{i}")).collect(), words }
    }

    fn json(&mut self, id: u64) -> serde_json::Value {
        let Docs { rng, vocab, words } = self;
        let mut word = || vocab[(vocab.len() as f64 * rng.unit().powi(3)) as usize].clone();
        let title: Vec<String> = (0..4).map(|_| word()).collect();
        let body: Vec<String> = (0..*words).map(|_| word()).collect();
        let tags: Vec<String> = (0..3).map(|_| word()).collect();
        serde_json::json!({
            "id": format!("d{id}"), "title": title.join(" "), "body": body.join(" "), "tags": tags,
            "city": format!("c{}", rng.below(300)), "rank": rng.below(1000), "big": rng.below(1 << 40), "ok": rng.below(2) == 1
        })
    }

    fn doc(&mut self, sc: &Schema, id: u64) -> Document {
        torque::ingest::parse_json_document(&self.json(id), sc, 0).unwrap()
    }
}

fn writes(docs: impl IntoIterator<Item = Document>) -> Writes {
    let mut w = Writes::default();
    for d in docs {
        w.upsert(d);
    }
    w
}

/// A bundle whose first segment, flushed, trained the collection's dictionary (3,000 documents).
fn trained(sc: &Arc<Schema>, docs: &mut Docs, next: &mut u64) -> IndexBundle {
    let b = write::apply(&IndexBundle::empty(Arc::clone(sc)), writes((0..3000).map(|_| {
        *next += 1;
        docs.doc(sc, *next)
    })));
    let flushed = write::merge(&b, 0..1).unwrap();
    write::publish(&b, &b, 0..1, flushed, false).unwrap()
}

fn bench_writes(c: &mut Criterion) {
    let sc = Arc::new(schema(true));
    let mut docs = Docs::new(9, 60);
    let mut next = 0;
    let b = trained(&sc, &mut docs, &mut next);
    let mut group = c.benchmark_group("write");
    for n in [1u64, 32] {
        let pool: Vec<Document> = (0..n * 64).map(|i| docs.doc(&sc, 1_000_000 + i)).collect();
        let mut at = 0;
        group.bench_with_input(BenchmarkId::new("segment of", format!("{n} documents")), &n, |bench, &n| {
            bench.iter_batched(
                || {
                    at = (at + n as usize) % pool.len();
                    writes(pool[at..at + n as usize].iter().cloned())
                },
                |w| write::apply(&b, w),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_merges(c: &mut Criterion) {
    let sc = Arc::new(schema(true));
    let mut group = c.benchmark_group("merge ten segments");
    // in memory: ten writes of a few documents each
    for per in [1u64, 10, 100] {
        let mut docs = Docs::new(per, 60);
        let mut next = 0;
        let mut b = trained(&sc, &mut docs, &mut next);
        for _ in 0..10 {
            b = write::apply(&b, writes((0..per).map(|_| {
                next += 1;
                docs.doc(&sc, next)
            })));
        }
        group.bench_function(BenchmarkId::new("in memory", format!("{per} documents each")), |bench| bench.iter(|| write::merge(&b, 1..11).unwrap()));
    }
    // flushed: ten of a size, a tenth of their documents written again since
    let mut sizes = vec![1_000u64, 10_000];
    if std::env::var_os("TORQUE_BENCH_LARGE").is_some() {
        sizes.push(50_000);
    }
    group.sample_size(10);
    for per in sizes {
        let mut docs = Docs::new(per, 60);
        let mut next = 0;
        let mut b = trained(&sc, &mut docs, &mut next);
        for i in 1..11 {
            b = write::apply(&b, writes((0..per).map(|_| {
                next += 1;
                docs.doc(&sc, next)
            })));
            let flushed = write::merge(&b, i..i + 1).unwrap();
            b = write::publish(&b, &b, i..i + 1, flushed, false).unwrap();
        }
        let again: Vec<u64> = (0..per).map(|_| 3000 + 1 + docs.rng.below(per * 10)).collect();
        b = write::apply(&b, writes(again.into_iter().map(|id| docs.doc(&sc, id))));
        group.bench_function(BenchmarkId::new("flushed", format!("{per} documents each")), |bench| bench.iter(|| write::merge(&b, 1..11).unwrap()));
    }
    group.finish();
}

/// A data directory on the drive `TORQUE_BENCH_DIR` names.
fn work_dir() -> tempfile::TempDir {
    match std::env::var_os("TORQUE_BENCH_DIR") {
        Some(base) => tempfile::tempdir_in(base).unwrap(),
        None => tempfile::tempdir().unwrap(),
    }
}

/// A realtime collection with persistence in a temporary directory.
fn collection(dir: &std::path::Path) -> (Arc<CollectionRegistry>, Arc<torque::collection::Collection>) {
    let reg = Arc::new(CollectionRegistry::new(Some(dir.to_path_buf())));
    let col = torque::persistence::create_collection(dir, &reg, schema(true)).unwrap_or_else(|e| panic!("{e}"));
    (reg, col)
}

fn durable(col: &torque::collection::Collection, docs: Vec<Document>) {
    col.apply_writes(docs, &[]).unwrap().unwrap().wait_blocking().unwrap();
}

fn bench_flush(c: &mut Criterion) {
    let dir = work_dir();
    let (_reg, col) = collection(dir.path());
    let metrics = torque::observability::MetricsCollector::new();
    let mut docs = Docs::new(3, 30);
    const N: u64 = 100_000;
    for start in (0..N).step_by(5000) {
        durable(&col, (start..start + 5000).map(|i| docs.doc(&col.schema, i)).collect());
        compaction::flush(&col, Some(dir.path()), &metrics);
    }
    let mut group = c.benchmark_group("flush");
    group.sample_size(10);
    group.bench_function("flush and save after 500 updates of 100,000 documents", |bench| {
        bench.iter_custom(|iters| {
            let mut spent = Duration::ZERO;
            for _ in 0..iters {
                for _ in 0..500 {
                    let id = docs.rng.below(N);
                    durable(&col, vec![docs.doc(&col.schema, id)]);
                }
                let t = Instant::now();
                compaction::flush(&col, Some(dir.path()), &metrics);
                spent += t.elapsed();
            }
            spent
        });
    });
    group.finish();
    col.close_wal();
}

fn bench_log(c: &mut Criterion) {
    let dir = work_dir();
    let wal = Arc::new(Wal::create(dir.path(), "log").unwrap());
    let record: Arc<[u8]> = vec![7u8; 600].into();
    let mut group = c.benchmark_group("log");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(4));
    for writers in [1u64, 16, 64] {
        group.bench_function(BenchmarkId::new("a write acknowledged", format!("{writers} writers")), |bench| {
            bench.iter_custom(|iters| {
                let each = iters.div_ceil(writers);
                let t = Instant::now();
                let threads: Vec<_> = (0..writers)
                    .map(|_| {
                        let (wal, record) = (Arc::clone(&wal), Arc::clone(&record));
                        std::thread::spawn(move || {
                            for _ in 0..each {
                                let entry = Entry::Writes { paths: Vec::new(), ops: vec![WriteOp { upserts: vec![Logged::Record(Arc::clone(&record))], deletes: Vec::new() }] };
                                wal.append(0, &entry).unwrap().1.wait_blocking().unwrap();
                            }
                        })
                    })
                    .collect();
                for th in threads {
                    th.join().unwrap();
                }
                // (time per write, the writes of all writers together)
                t.elapsed().mul_f64(iters as f64 / (each * writers) as f64)
            });
        });
    }
    group.finish();
}

fn bench_restart(c: &mut Criterion) {
    let dir = work_dir();
    {
        let (_reg, col) = collection(dir.path());
        let metrics = torque::observability::MetricsCollector::new();
        let mut docs = Docs::new(5, 30);
        durable(&col, (0..10_000).map(|i| docs.doc(&col.schema, i)).collect());
        compaction::flush(&col, Some(dir.path()), &metrics);
        // 10,000 writes after the save, by 32 writers (records hold the writes a sync took together)
        let pool: Vec<Vec<Document>> = (0..32u64).map(|t| (0..10_000 / 32).map(|i| docs.doc(&col.schema, 10_000 + t * 1000 + i)).collect()).collect();
        std::thread::scope(|s| {
            for mine in pool {
                let col = &col;
                s.spawn(move || {
                    for d in mine {
                        durable(col, vec![d]);
                    }
                });
            }
        });
        col.close_wal();
    }
    let mut group = c.benchmark_group("restart");
    group.sample_size(10);
    group.bench_function("a saved collection and 10,000 writes in its log", |bench| {
        bench.iter(|| {
            let reg = CollectionRegistry::new(Some(dir.path().to_path_buf()));
            torque::persistence::load_collections(dir.path(), &reg, None).unwrap();
            let col = reg.get("bench").unwrap();
            assert!(col.snapshot_bundle().total_docs() >= 19_000);
            col.close_wal();
        });
    });
    group.finish();
}

criterion_group!(benches, bench_writes, bench_merges, bench_flush, bench_log, bench_restart);
criterion_main!(benches);
