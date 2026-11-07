//! JOINs over collections in several segments, with documents superseded by later copies and
//! deleted, answer as over each collection in one segment of its live documents: JOIN filters
//! (both directions), sorts on referenced fields and facets on referenced fields.

use std::sync::Arc;
use torque::bitmap::Bitmap;
use torque::collection::alias::AliasRegistry;
use torque::collection::CollectionRegistry;
use torque::filter::ops::JoinContext;
use torque::index::IndexBundle;
use torque::ingest::build_index;
use torque::schema::*;
use torque::search::{self, SearchParams};

fn field(name: &str, field_type: FieldType, facet: bool, sort: bool, reference: Option<&str>) -> FieldDef {
    FieldDef { name: name.into(), field_type, facet, sort, index: true, reference: reference.map(str::to_string), ..Default::default() }
}

fn schema(name: &str, fields: Vec<FieldDef>) -> Schema {
    let mut s = Schema { name: name.into(), fields, ..Default::default() };
    s.assign_field_ids();
    s
}

fn doc(id: String, fields: Vec<FieldValue>) -> Document {
    Document { id: 0, external_id: id, language: Language::English, fields: fields.into(), record: None }
}

const COUNTRIES: [&str; 4] = ["Germany", "USA", "Sweden", "Italy"];
const COLORS: [&str; 5] = ["red", "blue", "green", "black", "white"];

fn brand(i: usize, country: &str, revenue: i64) -> Document {
    doc(format!("b{i}"), vec![FieldValue::Text(format!("Brand {i}")), FieldValue::Text(country.into()), FieldValue::Int64(revenue)])
}

fn product(i: usize, brand: usize) -> Document {
    doc(
        format!("p{i}"),
        vec![FieldValue::Text(format!("{} shoe {i}", COLORS[i % 5])), FieldValue::Float64((i * 7 % 90) as f64 + 0.5), FieldValue::Text(format!("b{brand}"))],
    )
}

fn review(i: usize) -> Document {
    doc(format!("r{i}"), vec![FieldValue::Text(format!("p{}", i * 13 % 120)), FieldValue::Int32((i % 5 + 1) as i32)])
}

/// A collection's bundle of segments `parts`, the documents `deleted` (segment, local id) gone.
fn bundle(schema: &Arc<Schema>, parts: Vec<Vec<Document>>, deleted: &[(usize, u32)]) -> IndexBundle {
    let mut segments = Vec::new();
    let mut at = 0u32;
    for docs in parts {
        let n = docs.len() as u32;
        let mut seg = build_index(schema, docs);
        seg.base_doc_id = at;
        segments.push(Arc::new(seg));
        at += n;
    }
    let mut masks = vec![Bitmap::new(); segments.len()];
    for &(s, d) in deleted {
        masks[s].add(d);
    }
    let masks: Vec<Arc<Bitmap>> = masks.into_iter().map(Arc::new).collect();
    let total_docs = IndexBundle::compute_live_total_docs(&segments, &masks);
    IndexBundle { schema: Arc::clone(schema), segments, deleted_per_segment: masks, total_docs, lsn: 0, next_seq: 0, memory: 0 }
}

/// brands, products (referencing brands) and reviews (referencing products), each either in one
/// segment of its live documents or in several: brands b3 and b14 superseded by later copies and
/// b7 deleted (products referencing it then reference nothing), product p5 superseded by a copy
/// referencing another brand.
fn registry(many: bool) -> CollectionRegistry {
    let registry = CollectionRegistry::new(None);
    let brands = registry
        .create(schema("brands", vec![field("name", FieldType::Text, false, false, None), field("country", FieldType::String, true, true, None), field("revenue", FieldType::Int64, true, true, None)]))
        .unwrap();
    let products = registry
        .create(schema(
            "products",
            vec![field("title", FieldType::Text, false, false, None), field("price", FieldType::Float64, true, true, None), field("brand_id", FieldType::String, false, false, Some("brands.id"))],
        ))
        .unwrap();
    let reviews = registry
        .create(schema("reviews", vec![field("product_id", FieldType::String, false, false, Some("products.id")), field("stars", FieldType::Int32, true, true, None)]))
        .unwrap();

    let first: Vec<Document> = (0..20).map(|i| brand(i, COUNTRIES[i % 4], (i as i64 * 1_337) % 9_000)).collect();
    let (b3, b14) = (brand(3, "Sweden", 12_000), brand(14, "Italy", 500));
    let all: Vec<Document> = (0..120).map(|i| product(i, i * 7 % 20)).collect();
    let p5 = product(5, 3);
    if many {
        brands.swap_bundle(bundle(&brands.schema, vec![first[..10].to_vec(), first[10..].to_vec(), vec![b3, b14]], &[(0, 3), (0, 7), (1, 4)]));
        products.swap_bundle(bundle(&products.schema, vec![all[..60].to_vec(), all[60..].to_vec(), vec![p5]], &[(0, 5)]));
    } else {
        let live: Vec<Document> = first.into_iter().filter(|d| !["b3", "b7", "b14"].contains(&d.external_id.as_str())).chain([b3, b14]).collect();
        brands.swap_bundle(bundle(&brands.schema, vec![live], &[]));
        let live: Vec<Document> = all.into_iter().filter(|d| d.external_id != "p5").chain([p5]).collect();
        products.swap_bundle(bundle(&products.schema, vec![live], &[]));
    }
    reviews.swap_bundle(bundle(&reviews.schema, vec![(0..40).map(review).collect()], &[]));
    registry
}

fn params(q: &str) -> SearchParams {
    SearchParams { q: q.into(), query_by: vec!["title".into()], per_page: 50, page: 1, ..Default::default() }
}

/// A search of products in the registry, without timings.
fn run(registry: &CollectionRegistry, p: &SearchParams) -> serde_json::Value {
    let aliases = AliasRegistry::new();
    let ctx = JoinContext::new(registry, &aliases, "products");
    let products = registry.get("products").unwrap();
    let view = products.view();
    let (r, _) = search::execute_with_stores_gpu_join(
        view.target(),
        &products.schema,
        p,
        None,
        None,
        #[cfg(feature = "cuda")]
        None,
        Some(&ctx),
    )
    .unwrap();
    let mut v = serde_json::to_value(&r).unwrap();
    v.as_object_mut().unwrap().remove("search_time_ms");
    v
}

fn ids(v: &serde_json::Value) -> Vec<String> {
    v["hits"].as_array().unwrap().iter().map(|h| h["document"]["id"].as_str().unwrap().to_string()).collect()
}

fn same(p: &SearchParams) -> serde_json::Value {
    let (one, many) = (run(&registry(false), p), run(&registry(true), p));
    assert_eq!(one, many, "filter={:?} sort={:?} facet={:?} page={}", p.filter_by, p.sort_by, p.facet_by, p.page);
    many
}

#[test]
fn join_filters_read_every_segment() {
    let germany = same(&SearchParams { filter_by: Some("$brands(country:=Germany)".into()), ..params("*") });
    // brands b0, b4, b8, b12, b16 are German (b3 became Swedish, b14 Italian)
    let expected = (0..120).filter(|&i| i != 5 && [0, 4, 8, 12, 16].contains(&(i * 7 % 20))).count();
    assert_eq!(germany["found"], expected);

    // b3's later copy (a later segment) is Swedish: its products are found, and p5's copy, which references b3
    let sweden = same(&SearchParams { filter_by: Some("$brands(country:=Sweden)".into()), ..params("*") });
    assert!(ids(&sweden).contains(&"p5".to_string()), "{sweden}");
    assert!(ids(&sweden).contains(&"p49".to_string()), "p49 references b3 ({}): {sweden}", 49 * 7 % 20);

    // the deleted brand matches nothing
    let none = same(&SearchParams { filter_by: Some("$brands(name:=Brand 7)".into()), ..params("*") });
    assert_eq!(none["found"], 0);

    same(&SearchParams { filter_by: Some("$brands(revenue:>=5000) && price:<40".into()), ..params("*") });
    same(&SearchParams { filter_by: Some("$brands(country:[Italy, USA])".into()), ..params("red shoe") });
    // the other direction: products a matching review references
    let reviewed = same(&SearchParams { filter_by: Some("$reviews(stars:>=4)".into()), ..params("*") });
    assert!(reviewed["found"].as_u64().unwrap() > 0);
}

#[test]
fn join_sorts_read_every_segment() {
    for order in ["desc", "asc"] {
        for page in [1, 2, 3] {
            let r = same(&SearchParams { sort_by: Some(format!("$brands(revenue:{order})")), page, ..params("*") });
            if page == 1 && order == "desc" {
                // b3's later copy has the highest revenue
                let first = &ids(&r)[0];
                let n: usize = first[1..].parse().unwrap();
                assert!(n == 5 || n * 7 % 20 == 3, "first hit {first} references b3: {r}");
            }
        }
    }
}

#[test]
fn join_facets_read_every_segment() {
    let r = same(&SearchParams { facet_by: Some("$brands(country)".into()), ..params("*") });
    let counts = &r["facet_counts"][0]["counts"];
    let count_of = |c: &str| counts.as_array().unwrap().iter().find(|x| x["value"] == c).map(|x| x["count"].as_u64().unwrap());
    // Sweden: products of b2, b6, b10 and b18, of b3's later copy (and so p5's), not of b14 (now Italian)
    let sweden = (0..120).filter(|&i| i == 5 || [2, 3, 6, 10, 18].contains(&(i * 7 % 20))).count() as u64;
    assert_eq!(sweden, 31);
    assert_eq!(count_of("Sweden"), Some(sweden), "{r}");
    same(&SearchParams { facet_by: Some("$brands(revenue)".into()), ..params("*") });
    same(&SearchParams { facet_by: Some("$brands(country, revenue)".into()), max_facet_values: Some(3), ..params("*") });
    same(&SearchParams { facet_by: Some("$brands(revenue(low:[0, 3000], high:[3000, 20000]))".into()), ..params("*") });
    same(&SearchParams { facet_by: Some("$brands(country)".into()), filter_by: Some("price:>30".into()), ..params("shoe") });
}

#[test]
fn collection_view_finds_live_documents() {
    let registry = registry(true);
    let brands = registry.get("brands").unwrap();
    let view = brands.view();
    assert_eq!(view.len(), 3);
    assert_eq!(view.total_docs(), 19);
    // the later copy, not the superseded one
    assert_eq!(view.lookup("b3"), Some((2, 0)));
    assert_eq!(view.lookup("b14"), Some((2, 1)));
    assert_eq!(view.lookup("b13"), Some((1, 3)));
    assert_eq!(view.lookup("b7"), None);
    assert_eq!(view.locate(21), Some((2, 1)));
    assert_eq!(view.locate(22), None);
}
