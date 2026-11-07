use std::sync::Arc;
use std::thread;
use torque::collection::CollectionRegistry;
use torque::ingest::build_index;
use torque::schema::*;
use torque::search;

// -- Test helpers --

fn make_field(name: &str, field_type: FieldType, sort: bool, facet: bool) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type,
        facet,
        index: true,
        optional: false,
        sort,
        stem: true,
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
    }
}

fn product_schema() -> Schema {
    Schema {
        index_mode: Default::default(),
        name: "products".into(),
        fields: vec![
            make_field("title", FieldType::String, false, false),
            make_field("price", FieldType::Float64, true, false),
            make_field("color", FieldType::String, false, true),
            make_field("brand", FieldType::String, false, true),
            make_field("rating", FieldType::Int32, true, true),
        ],
        default_sorting_field: None,
        enable_nested_fields: false,
        symbols_to_index: vec![],
        token_separators: vec![],
        default_language: Language::default(),
        persistence_enabled: true,
        name_to_id: Default::default(),
        geopoint_field_names: vec![],
        nested_layout: None,
        registry: Default::default(),
        metadata: None,
        synonym_sets: None,
        curation_sets: None,
    }
}

fn make_product(id: DocId, title: &str, price: f64, color: &str, brand: &str, rating: i32) -> Document {
    Document {
        id,
        external_id: format!("{id}"),
        language: Language::English,
        fields: vec![
            FieldValue::Text(title.into()),
            FieldValue::Float64(price),
            FieldValue::Text(color.into()),
            FieldValue::Text(brand.into()),
            FieldValue::Int32(rating),
        ].into(),
        record: None,
    }
}

fn make_params(q: &str, sort_by: Option<&str>, facet_by: Option<&str>, filter_by: Option<&str>) -> search::SearchParams {
    search::SearchParams {
        q: q.into(),
        query_by: vec!["title".into()],
        filter_by: filter_by.map(|s| s.into()),
        sort_by: sort_by.map(|s| s.into()),
        facet_by: facet_by.map(|s| s.into()),
        per_page: 10,
        page: 1,
        ..Default::default()
    }
}

fn unwrap_ungrouped(resp: search::SearchResponse) -> search::SearchResult {
    match resp {
        search::SearchResponse::Ungrouped(r) => r,
        _ => panic!("Expected ungrouped result"),
    }
}

fn setup_large_collection() -> (Arc<CollectionRegistry>, Schema) {
    let mut schema = product_schema();
    schema.assign_field_ids();

    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();
    let collection = registry.get("products").unwrap();

    let colors = ["red", "blue", "green", "black", "white"];
    let brands = ["Nike", "Adidas", "Puma", "Reebok"];
    let adjectives = ["Running", "Casual", "Sport", "Training", "Premium"];
    let nouns = ["Shoes", "Hat", "Jacket", "Shorts", "Socks", "Bag", "Gloves", "Pants"];

    let mut docs = Vec::with_capacity(1000);
    for i in 0..1000u32 {
        let adj = adjectives[(i as usize) % adjectives.len()];
        let noun = nouns[(i as usize) % nouns.len()];
        let color = colors[(i as usize) % colors.len()];
        let brand = brands[(i as usize) % brands.len()];
        let title = format!("{color} {adj} {noun} {i}");
        let price = 10.0 + (i as f64) * 0.5;
        let rating = (i % 5 + 1) as i32;
        docs.push(make_product(i, &title, price, color, brand, rating));
    }

    let index = build_index(&collection.schema, docs);
    collection.swap_index(index);

    (registry, schema)
}

// -- Concurrency tests --

/// 20 threads execute the same text search query concurrently.
/// Validates all threads get identical results.
#[test]
fn concurrent_20_text_search_same_query() {
    let (registry, schema) = setup_large_collection();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut handles = Vec::new();
    for _ in 0..20 {
        let idx = Arc::clone(&index);
        let s = schema.clone();
        handles.push(thread::spawn(move || {
            let params = make_params("Running Shoes", None, None, None);
            let result = unwrap_ungrouped(search::execute(&idx, &s, &params).unwrap());
            (result.found, result.hits.len())
        }));
    }

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let (expected_found, expected_hits) = results[0];
    assert!(expected_found > 0, "Search should find results");
    for (i, &(found, hits)) in results.iter().enumerate() {
        assert_eq!(found, expected_found, "Thread {i} found mismatch");
        assert_eq!(hits, expected_hits, "Thread {i} hits mismatch");
    }
}

/// 20 threads execute different query types concurrently:
/// text search, filter, sort, facet, group_by.
#[test]
fn concurrent_20_mixed_query_types() {
    let (registry, schema) = setup_large_collection();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut handles = Vec::new();
    for i in 0..20 {
        let idx = Arc::clone(&index);
        let s = schema.clone();
        handles.push(thread::spawn(move || {
            let params = match i % 5 {
                0 => make_params("Running", None, None, None),
                1 => make_params("*", Some("price:asc"), None, Some("color:=red")),
                2 => make_params("*", None, Some("color,brand"), None),
                3 => make_params("Shoes", Some("rating:desc"), Some("brand"), None),
                _ => {
                    let mut p = make_params("*", None, Some("color"), Some("brand:=Nike"));
                    p.group_by = Some("color".into());
                    p.group_limit = Some(3);
                    p
                }
            };
            let result = search::execute(&idx, &s, &params).unwrap();
            match &result {
                search::SearchResponse::Ungrouped(r) => {
                    assert!(r.found > 0, "Thread {i} (pattern {}) should find results", i % 5);
                }
                search::SearchResponse::Grouped(r) => {
                    assert!(!r.grouped_hits.is_empty(), "Thread {i} group_by should have groups");
                }
            }
            true
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        assert!(h.join().unwrap(), "Thread {i} should succeed");
    }
}

/// 20 threads search while index is being rebuilt (ArcSwap swap).
/// Validates no panics or data corruption during concurrent read + swap.
#[test]
fn concurrent_search_during_index_rebuild() {
    let (registry, schema) = setup_large_collection();
    let collection = registry.get("products").unwrap();

    let barrier = Arc::new(std::sync::Barrier::new(21)); // 20 searchers + 1 rebuilder

    let mut handles = Vec::new();

    // 20 searcher threads
    for t in 0..20 {
        let col = Arc::clone(&collection);
        let s = schema.clone();
        let b = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b.wait(); // synchronize start
            let mut successes = 0;
            for _ in 0..50 {
                let idx = col.snapshot();
                let params = make_params("Running", Some("price:asc"), Some("color"), None);
                let result = search::execute(&idx, &s, &params);
                assert!(result.is_ok(), "Thread {t} search should not error");
                let ungrouped = unwrap_ungrouped(result.unwrap());
                assert!(ungrouped.found > 0, "Thread {t} should find results");
                successes += 1;
            }
            successes
        }));
    }

    // 1 rebuilder thread — swaps index 5 times during concurrent searches
    {
        let col = Arc::clone(&collection);
        let s = schema.clone();
        let b = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b.wait(); // synchronize start
            let mut successes = 0;
            for round in 0..5 {
                // Build a slightly different index each round
                let colors = ["red", "blue", "green", "black", "white"];
                let brands = ["Nike", "Adidas", "Puma", "Reebok"];
                let mut docs = Vec::with_capacity(1000);
                for i in 0..1000u32 {
                    let title = format!("Running Shoes variant {round} item {i}");
                    let color = colors[(i as usize) % colors.len()];
                    let brand = brands[(i as usize) % brands.len()];
                    docs.push(make_product(i, &title, 10.0 + i as f64, color, brand, (i % 5 + 1) as i32));
                }
                let index = build_index(&s, docs);
                col.swap_index(index);
                successes += 1;
                thread::sleep(std::time::Duration::from_millis(1));
            }
            successes
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        let count = h.join().unwrap();
        if i < 20 {
            assert_eq!(count, 50, "Searcher thread {i} should complete all 50 queries");
        } else {
            assert_eq!(count, 5, "Rebuilder should complete all 5 swaps");
        }
    }
}

/// 20 threads search across 4 different collections concurrently.
/// Validates collection isolation under concurrent access.
#[test]
fn concurrent_search_multiple_collections() {
    let registry = Arc::new(CollectionRegistry::new(None));
    let mut schemas = Vec::new();

    for col_idx in 0..4 {
        let mut schema = Schema {
        index_mode: Default::default(),
            name: format!("collection_{col_idx}"),
            fields: vec![
                make_field("title", FieldType::String, false, false),
                make_field("price", FieldType::Float64, true, false),
                make_field("color", FieldType::String, false, true),
                make_field("brand", FieldType::String, false, true),
                make_field("rating", FieldType::Int32, true, true),
            ].into(),
            default_sorting_field: None,
            enable_nested_fields: false,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();
        registry.create(schema.clone()).unwrap();

        let collection = registry.get(&format!("collection_{col_idx}")).unwrap();
        let doc_count = 200 + col_idx * 100; // 200, 300, 400, 500 docs
        let mut docs = Vec::with_capacity(doc_count);
        for i in 0..doc_count as u32 {
            docs.push(make_product(
                i,
                &format!("Product {i} from collection {col_idx}"),
                10.0 + i as f64,
                ["red", "blue"][(i as usize) % 2],
                ["Nike", "Adidas"][(i as usize) % 2],
                (i % 5 + 1) as i32,
            ));
        }
        let index = build_index(&collection.schema, docs);
        collection.swap_index(index);
        schemas.push(schema);
    }

    let mut handles = Vec::new();
    for t in 0..20 {
        let reg = Arc::clone(&registry);
        let col_idx = t % 4;
        let s = schemas[col_idx].clone();
        handles.push(thread::spawn(move || {
            let collection = reg.get(&format!("collection_{col_idx}")).unwrap();
            let idx = collection.snapshot();
            let params = make_params("Product", Some("price:asc"), Some("color"), None);
            let result = unwrap_ungrouped(search::execute(&idx, &s, &params).unwrap());
            (col_idx, result.found)
        }));
    }

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for &(col_idx, found) in &results {
        let expected = 200 + col_idx * 100;
        assert_eq!(found, expected, "Collection {col_idx} should have {expected} docs, got {found}");
    }
}

/// High-contention stress: 20 threads each run 100 queries with filters and facets.
/// Validates correctness and no panics under sustained load.
#[test]
fn stress_20_threads_100_queries_each() {
    let (registry, schema) = setup_large_collection();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let barrier = Arc::new(std::sync::Barrier::new(20));

    let mut handles = Vec::new();
    for t in 0..20 {
        let idx = Arc::clone(&index);
        let s = schema.clone();
        let b = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b.wait(); // all threads start simultaneously
            for q in 0..100 {
                let params = match (t + q) % 4 {
                    0 => make_params("Running", None, None, None),
                    1 => make_params("*", Some("price:desc"), None, Some("color:=blue")),
                    2 => make_params("Shoes", None, Some("color,brand"), None),
                    _ => make_params("*", Some("rating:desc"), Some("brand"), Some("rating:>3")),
                };
                let result = search::execute(&idx, &s, &params).unwrap();
                let ungrouped = unwrap_ungrouped(result);
                assert!(ungrouped.found > 0, "Thread {t} query {q} should find results");
            }
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        h.join().unwrap_or_else(|_| panic!("Thread {i} panicked"));
    }
    // 20 threads × 100 queries = 2000 concurrent queries completed
}
