//! Searches over 100,000 documents: several words, a prefix, a typo, a filter with a sort, facets, a
//! filter alone. Each on one segment, and on a realtime collection holding the same documents as
//! writes leave it before its flushed segments merge: ten flushed segments, a tenth of the
//! documents written again one by one (their old copies deleted; flushed every 2,500, the writes in
//! memory merged by tier), the latest of them in memory.
//! `cargo bench --bench search_bench`.

// The server's allocator (src/main.rs): what a bench measures is what the server runs.
#[cfg(all(feature = "native", not(feature = "jemalloc"), target_env = "msvc"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use torque::collection::CollectionRegistry;
use torque::ingest::build_index;
use torque::schema::{Document, FieldDef, FieldType, FieldValue, IndexMode, Language, Schema};
use torque::search::{self, SearchParams, SearchTarget};

const DOCS: usize = 100_000;
const WORDS: [&str; 10] = ["quick", "brown", "fox", "jumps", "over", "lazy", "dog", "runs", "fast", "slow"];
const CATEGORIES: [&str; 5] = ["Electronics", "Books", "Clothing", "Home", "Sports"];

fn schema(mode: IndexMode) -> Schema {
    let field = |name: &str, field_type: FieldType, facet: bool, sort: bool| FieldDef { name: name.into(), field_type, facet, sort, ..Default::default() };
    let mut s = Schema {
        name: "bench".into(),
        fields: vec![
            field("title", FieldType::String, false, false),
            field("body", FieldType::String, false, false),
            field("price", FieldType::Int32, false, true),
            field("category", FieldType::String, true, true),
        ],
        default_language: Language::English,
        index_mode: mode,
        persistence_enabled: false,
        ..Default::default()
    };
    s.assign_field_ids();
    s
}

/// Document `i` as written the `version`th time.
fn doc(i: usize, version: usize) -> Document {
    let w = |k: usize| WORDS[(i * k + i / 10 + version) % WORDS.len()];
    Document {
        id: 0,
        external_id: format!("doc-{i}"),
        language: Language::English,
        fields: vec![
            FieldValue::Text(format!("{} {} {} product {i}", w(1), w(3), w(7))),
            FieldValue::Text(format!("description of item {i} with {} {}", w(5), w(2))),
            FieldValue::Int32(((i * 7919 + version) % 10_000) as i32),
            FieldValue::Text(CATEGORIES[i % CATEGORIES.len()].into()),
        ]
        .into(),
        record: None,
    }
}

fn params(q: &str, query_by: &str) -> SearchParams {
    SearchParams { q: q.into(), query_by: query_by.split(',').map(Into::into).collect(), per_page: 10, page: 1, ..Default::default() }
}

fn cases() -> Vec<(&'static str, SearchParams)> {
    let mut filter_sort = params("*", "title");
    filter_sort.filter_by = Some("price:<5000".into());
    filter_sort.sort_by = Some("price:desc".into());
    let mut facets = params("fox", "title,body");
    facets.facet_by = Some("category".into());
    let mut filter = params("*", "title");
    filter.filter_by = Some("category:=Books".into());
    vec![
        ("words", params("quick brown", "title,body")),
        ("prefix", params("qui", "title")),
        ("typo", params("quik", "title")),
        ("filter and sort", filter_sort),
        ("facets", facets),
        ("filter", filter),
    ]
}

fn bench_search(c: &mut Criterion) {
    let one_schema = schema(IndexMode::Batch);
    // (a tenth written twice: the second version)
    let one = build_index(&one_schema, (0..DOCS).map(|i| doc(i, usize::from(i % 10 == 0))).collect());

    // a realtime collection of the same documents, as writes leave it
    let reg = CollectionRegistry::new(None);
    let col = reg.create(schema(IndexMode::Realtime)).unwrap();
    for part in 0..10 {
        col.apply_writes((part * DOCS / 10..(part + 1) * DOCS / 10).map(|i| doc(i, 0)).collect(), &[]).unwrap();
        col.compact();
    }
    for (n, i) in (0..DOCS).step_by(10).enumerate() {
        col.apply_writes(vec![doc(i, 1)], &[]).unwrap();
        col.merge_memory();
        if n % 2_500 == 2_499 {
            col.compact();
        }
    }
    let bundle = col.snapshot_bundle();
    for (name, p) in cases() {
        let found = |r: search::SearchResponse| serde_json::to_value(r).unwrap()["found"].as_u64().unwrap();
        let (a, b) = (found(search::execute(&one, &one_schema, &p).unwrap()), found(search::execute(SearchTarget::Bundle(&bundle), &col.schema, &p).unwrap()));
        assert!(a > 0 && a == b, "{name}: one segment finds {a}, the collection {b}");
    }

    let mut group = c.benchmark_group("search");
    group.sample_size(30);
    for (name, p) in cases() {
        group.bench_with_input(BenchmarkId::new("one segment", name), &p, |b, p| {
            b.iter(|| search::execute(&one, &one_schema, p).unwrap());
        });
        group.bench_with_input(BenchmarkId::new(format!("{} segments", bundle.segments.len()), name), &p, |b, p| {
            b.iter(|| search::execute(SearchTarget::Bundle(&bundle), &col.schema, p).unwrap());
        });
    }
    group.finish();
}

criterion_group!(benches, bench_search);
criterion_main!(benches);
