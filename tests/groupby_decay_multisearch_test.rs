use std::sync::Arc;
use torque::collection::CollectionRegistry;
use torque::grouping::GroupedSearchResult;
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
            make_field("category", FieldType::String, false, true),
            make_field("brand", FieldType::String, false, true),
            make_field("timestamp", FieldType::Int64, true, false),
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

fn make_product(
    id: DocId,
    title: &str,
    price: f64,
    category: &str,
    brand: &str,
    timestamp: i64,
) -> Document {
    Document {
        id,
        external_id: format!("{id}"),
        language: Language::English,
        fields: vec![
            FieldValue::Text(title.into()),
            FieldValue::Float64(price),
            FieldValue::Text(category.into()),
            FieldValue::Text(brand.into()),
            FieldValue::Int64(timestamp),
        ].into(),
        record: None,
    }
}

fn setup_products() -> (Arc<CollectionRegistry>, Schema) {
    let mut schema = product_schema();
    schema.assign_field_ids();

    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();

    let collection = registry.get("products").unwrap();

    let docs = vec![
        make_product(0, "Running Shoes Pro", 89.99, "shoes", "Nike", 1000),
        make_product(1, "Running Shoes Lite", 59.99, "shoes", "Adidas", 900),
        make_product(2, "Running Hat", 29.99, "accessories", "Nike", 800),
        make_product(3, "Sport Jacket", 149.99, "clothing", "Puma", 700),
        make_product(4, "Casual Shoes", 49.99, "shoes", "Adidas", 600),
        make_product(5, "Running Shorts", 39.99, "clothing", "Nike", 500),
        make_product(6, "Training Gloves", 24.99, "accessories", "Puma", 400),
        make_product(7, "Running Socks", 14.99, "accessories", "Adidas", 300),
        make_product(8, "Sport Bag", 44.99, "accessories", "Nike", 200),
        make_product(9, "Casual Hat", 19.99, "accessories", "Puma", 100),
    ];

    let index = build_index(&collection.schema, docs);
    collection.swap_index(index);

    (registry, (*collection.schema).clone())
}

fn make_params(q: &str) -> search::SearchParams {
    search::SearchParams {
        q: q.into(),
        query_by: vec!["title".into()],
        filter_by: None,
        sort_by: None,
        facet_by: None,
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

fn unwrap_grouped(resp: search::SearchResponse) -> GroupedSearchResult {
    match resp {
        search::SearchResponse::Grouped(r) => r,
        _ => panic!("Expected grouped result"),
    }
}

// -- Group By tests --

#[test]
fn group_by_category() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    // 3 categories: shoes(3), accessories(5), clothing(2)
    assert_eq!(result.found, 3);
    assert_eq!(result.found_docs, 10);

    // Each group should have the right key
    let categories: Vec<String> = result
        .grouped_hits
        .iter()
        .map(|g| g.group_key[0].as_str().unwrap().to_string())
        .collect();
    assert!(categories.contains(&"shoes".to_string()));
    assert!(categories.contains(&"accessories".to_string()));
    assert!(categories.contains(&"clothing".to_string()));
}

#[test]
fn group_by_with_group_limit() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());
    params.group_limit = Some(1);

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    // Each group should have at most 1 hit
    for group in &result.grouped_hits {
        assert!(
            group.hits.len() <= 1,
            "Group {:?} has {} hits, expected <= 1",
            group.group_key,
            group.hits.len()
        );
    }

    // But found count should reflect total docs in group
    let accessories = result
        .grouped_hits
        .iter()
        .find(|g| g.group_key[0].as_str() == Some("accessories"))
        .unwrap();
    assert_eq!(accessories.found, 5);
    assert_eq!(accessories.hits.len(), 1);
}

#[test]
fn group_by_with_text_query() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("running");
    params.group_by = Some("category".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    // "running" appears in: shoes(2), accessories(2), clothing(1)
    assert!(result.found >= 2);
    assert!(result.found_docs >= 4);
}

#[test]
fn group_by_with_sort() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());
    params.sort_by = Some("price:asc".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    // Within each group, docs should be sorted by price ascending
    for group in &result.grouped_hits {
        if group.hits.len() >= 2 {
            let prices: Vec<f64> = group
                .hits
                .iter()
                .map(|h| h.document["price"].as_f64().unwrap())
                .collect();
            for w in prices.windows(2) {
                assert!(
                    w[0] <= w[1],
                    "Expected ascending in group {:?}: {} <= {}",
                    group.group_key,
                    w[0],
                    w[1]
                );
            }
        }
    }
}

#[test]
fn group_by_group_found_sort() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());
    params.sort_by = Some("_group_found:desc".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    // Groups sorted by doc count descending
    let counts: Vec<usize> = result.grouped_hits.iter().map(|g| g.found).collect();
    for w in counts.windows(2) {
        assert!(w[0] >= w[1], "Expected descending: {} >= {}", w[0], w[1]);
    }
    // accessories has 5, should be first
    assert_eq!(
        result.grouped_hits[0].group_key[0].as_str().unwrap(),
        "accessories"
    );
}

#[test]
fn group_by_with_pinned_hits_moves_group() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());
    params.pinned_hits = Some("3:1".into()); // doc 3 is in "clothing"

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.grouped_hits[0].group_key[0], "clothing");
    // The group moved — not duplicated
    let clothing_groups = result
        .grouped_hits
        .iter()
        .filter(|g| g.group_key[0] == "clothing")
        .count();
    assert_eq!(clothing_groups, 1);
    assert_eq!(result.grouped_hits.len(), 3);
}

#[test]
fn group_by_pinned_hits_surfaces_offpage_group() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());
    params.per_page = 2;
    params.pinned_hits = Some("3:1".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.grouped_hits.len(), 2);
    assert_eq!(result.grouped_hits[0].group_key[0], "clothing");
    assert_eq!(
        result.grouped_hits[0].hits[0].document["id"],
        "3",
        "pinned doc must be in the surfaced group"
    );
}

#[test]
fn group_by_non_faceted_field_errors() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("title".into());

    match search::execute(&index, &schema, &params) {
        Err(e) => assert!(
            e.contains("faceting"),
            "Error should mention faceting: {e}"
        ),
        Ok(_) => panic!("Expected error for non-faceted group_by field"),
    }
}

#[test]
fn group_by_multi_field() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category,brand".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    // Compound keys: each group_key should have 2 elements
    for group in &result.grouped_hits {
        assert_eq!(
            group.group_key.len(),
            2,
            "Expected 2-element key, got {:?}",
            group.group_key
        );
    }
    assert!(result.found >= 5); // many category+brand combos
}

#[test]
fn group_by_pagination() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Page 1 with 2 groups per page
    let mut params = make_params("*");
    params.group_by = Some("category".into());
    params.per_page = 2;
    params.page = 1;
    params.sort_by = Some("_group_found:desc".into());

    let result1 = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result1.grouped_hits.len(), 2);
    assert_eq!(result1.found, 3); // 3 total groups

    // Page 2
    params.page = 2;
    let result2 = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result2.grouped_hits.len(), 1); // remaining 1 group
}

// -- Decay Sort tests --

#[test]
fn decay_sort_gauss() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Origin=1000 (most recent), gauss decay with scale=500
    // Docs closer to ts=1000 should rank higher
    let mut params = make_params("*");
    params.sort_by = Some("timestamp(origin:1000,func:gauss,scale:500):desc".into());

    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 10);

    // First doc should be the one with timestamp closest to 1000
    let first_ts = result.hits[0].document["timestamp"].as_i64().unwrap();
    assert_eq!(first_ts, 1000); // doc 0 has ts=1000
}

#[test]
fn decay_sort_linear() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.sort_by = Some("timestamp(origin:1000,func:linear,scale:1000):desc".into());

    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    // Timestamps should be in descending order (closest to origin first)
    let timestamps: Vec<i64> = result
        .hits
        .iter()
        .map(|h| h.document["timestamp"].as_i64().unwrap())
        .collect();
    for w in timestamps.windows(2) {
        assert!(w[0] >= w[1], "Expected descending: {} >= {}", w[0], w[1]);
    }
}

#[test]
fn decay_sort_combined_with_text() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("running");
    params.sort_by =
        Some("_text_match:desc,timestamp(origin:1000,func:gauss,scale:300):desc".into());

    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found >= 4); // "running" appears in several docs
}

#[test]
fn decay_sort_invalid_scale_zero() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.sort_by = Some("timestamp(origin:0,func:gauss,scale:0):desc".into());

    match search::execute(&index, &schema, &params) {
        Err(e) => assert!(e.contains("non-zero"), "Error should mention non-zero: {e}"),
        Ok(_) => panic!("Expected error for scale=0"),
    }
}

// -- text_match_type tests --

#[test]
fn text_match_type_max_score() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("running shoes");
    params.text_match_type = Some("max_score".into());

    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found >= 2);
}

#[test]
fn text_match_type_sum_score() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("running shoes");
    params.text_match_type = Some("sum_score".into());

    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found >= 2);
}

#[test]
fn text_match_type_invalid_errors() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("running");
    params.text_match_type = Some("invalid_type".into());

    match search::execute(&index, &schema, &params) {
        Err(e) => assert!(
            e.contains("text_match_type"),
            "Error should mention text_match_type: {e}"
        ),
        Ok(_) => panic!("Expected error for invalid text_match_type"),
    }
}

// -- Combined tests --

#[test]
fn group_by_with_facet_and_filter() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());
    params.facet_by = Some("brand".into());
    params.filter_by = Some("brand:=Nike".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    // Nike products: shoes(1), accessories(2), clothing(1) = 4 docs, 3 categories
    assert_eq!(result.found_docs, 4);
    assert!(result.found >= 2);

    // Facets should be computed
    assert_eq!(result.facet_counts.len(), 1);
    assert_eq!(result.facet_counts[0].field_name, "brand");
}

#[test]
fn empty_collection_grouped() {
    let mut schema = product_schema();
    schema.name = "empty_col".into();
    schema.assign_field_ids();

    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();

    let collection = registry.get("empty_col").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 0);
    assert_eq!(result.found_docs, 0);
    assert!(result.grouped_hits.is_empty());
}

#[test]
fn decay_sort_with_grouping() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.group_by = Some("category".into());
    params.sort_by = Some("timestamp(origin:1000,func:linear,scale:1000):desc".into());

    let result = unwrap_grouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 3);

    // Within each group, docs should be sorted by decay (timestamp closest to 1000 first)
    for group in &result.grouped_hits {
        if group.hits.len() >= 2 {
            let timestamps: Vec<i64> = group
                .hits
                .iter()
                .map(|h| h.document["timestamp"].as_i64().unwrap())
                .collect();
            for w in timestamps.windows(2) {
                assert!(
                    w[0] >= w[1],
                    "Expected descending timestamps in group {:?}: {} >= {}",
                    group.group_key,
                    w[0],
                    w[1]
                );
            }
        }
    }
}
