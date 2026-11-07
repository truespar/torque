use std::sync::Arc;
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

fn make_product(
    id: DocId,
    title: &str,
    price: f64,
    color: &str,
    brand: &str,
    rating: i32,
) -> Document {
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

fn setup_products() -> (Arc<CollectionRegistry>, Schema) {
    let mut schema = product_schema();
    schema.assign_field_ids();

    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();

    let collection = registry.get("products").unwrap();

    let docs = vec![
        make_product(0, "Red Running Shoes", 89.99, "red", "Nike", 4),
        make_product(1, "Blue Running Shoes", 79.99, "blue", "Adidas", 5),
        make_product(2, "Green Running Hat", 29.99, "green", "Nike", 3),
        make_product(3, "Red Sport Jacket", 149.99, "red", "Puma", 4),
        make_product(4, "Blue Casual Shoes", 59.99, "blue", "Adidas", 2),
        make_product(5, "Black Running Shorts", 39.99, "black", "Nike", 5),
        make_product(6, "Red Training Gloves", 24.99, "red", "Puma", 3),
        make_product(7, "White Running Socks", 14.99, "white", "Adidas", 4),
        make_product(8, "Blue Sport Bag", 44.99, "blue", "Nike", 3),
        make_product(9, "Green Casual Hat", 19.99, "green", "Puma", 2),
    ];

    let index = build_index(&collection.schema, docs);
    collection.swap_index(index);

    (registry, (*collection.schema).clone())
}

fn make_params(q: &str, sort_by: Option<&str>, facet_by: Option<&str>) -> search::SearchParams {
    search::SearchParams {
        q: q.into(),
        query_by: vec!["title".into()],
        filter_by: None,
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

// -- Sort tests --

#[test]
fn sort_by_price_ascending() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", Some("price:asc"), None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 10);
    assert_eq!(result.hits.len(), 10);

    let prices: Vec<f64> = result
        .hits
        .iter()
        .map(|h| h.document["price"].as_f64().unwrap())
        .collect();
    for w in prices.windows(2) {
        assert!(w[0] <= w[1], "Expected ascending: {} <= {}", w[0], w[1]);
    }
    assert_eq!(prices[0], 14.99);
    assert_eq!(prices[9], 149.99);
}

#[test]
fn sort_by_price_descending() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*", Some("price:desc"), None);
    params.per_page = 5;
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 10);
    assert_eq!(result.hits.len(), 5);

    let prices: Vec<f64> = result
        .hits
        .iter()
        .map(|h| h.document["price"].as_f64().unwrap())
        .collect();
    for w in prices.windows(2) {
        assert!(w[0] >= w[1], "Expected descending: {} >= {}", w[0], w[1]);
    }
    assert_eq!(prices[0], 149.99);
}

#[test]
fn sort_by_text_match_then_price() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("running shoes", Some("_text_match:desc,price:asc"), None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found >= 2);
}

#[test]
fn sort_by_rating_descending() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", Some("rating:desc"), None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    let ratings: Vec<i64> = result
        .hits
        .iter()
        .map(|h| h.document["rating"].as_i64().unwrap())
        .collect();
    for w in ratings.windows(2) {
        assert!(w[0] >= w[1], "Expected descending: {} >= {}", w[0], w[1]);
    }
}

#[test]
fn sort_by_invalid_field_errors() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", Some("nonexistent:asc"), None);
    match search::execute(&index, &schema, &params) {
        Err(e) => assert!(e.contains("nonexistent"), "Error should mention field: {e}"),
        Ok(_) => panic!("Expected error for invalid sort field"),
    }
}

#[test]
fn sort_by_non_sortable_field_errors() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", Some("title:asc"), None);
    match search::execute(&index, &schema, &params) {
        Err(e) => assert!(e.starts_with("Could not find a field named") && e.ends_with("in the schema for sorting."), "{e}"),
        Ok(_) => panic!("Expected error for non-sortable field"),
    }
}

// -- Facet tests --

#[test]
fn facet_by_color() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", None, Some("color"));
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.facet_counts.len(), 1);

    let color_facet = &result.facet_counts[0];
    assert_eq!(color_facet.field_name, "color");
    assert_eq!(color_facet.counts.len(), 5);

    let top_two: Vec<&str> = color_facet.counts[..2]
        .iter()
        .map(|c| c.value.as_str())
        .collect();
    assert!(top_two.contains(&"red"));
    assert!(top_two.contains(&"blue"));
    assert_eq!(color_facet.counts[0].count, 3);
    assert_eq!(color_facet.counts[1].count, 3);
}

#[test]
fn facet_by_multiple_fields() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", None, Some("color,brand"));
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.facet_counts.len(), 2);

    let names: Vec<&str> = result
        .facet_counts
        .iter()
        .map(|f| f.field_name.as_str())
        .collect();
    assert!(names.contains(&"color"));
    assert!(names.contains(&"brand"));
}

#[test]
fn facet_with_filter() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*", None, Some("brand"));
    params.filter_by = Some("color:=red".into());
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 3);

    let brand_facet = &result.facet_counts[0];
    let total_faceted: usize = brand_facet.counts.iter().map(|c| c.count).sum();
    assert_eq!(total_faceted, 3);
}

#[test]
fn facet_numeric_stats() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", None, Some("rating"));
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.facet_counts.len(), 1);

    let rating_facet = &result.facet_counts[0];
    let stats = &rating_facet.stats;
    assert_eq!(stats.min, Some(2.0));
    assert_eq!(stats.max, Some(5.0));
    // and the values counted
    assert_eq!(rating_facet.counts.iter().map(|c| c.count).sum::<usize>() as u64, stats.count);
    assert_eq!(stats.total_values, rating_facet.counts.len());
}

#[test]
fn facet_non_faceted_field_errors() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", None, Some("title"));
    match search::execute(&index, &schema, &params) {
        Err(e) => assert_eq!(e, "Could not find a facet field named `title` in the schema."),
        Ok(_) => panic!("Expected error for non-faceted field"),
    }
}

// -- Highlight tests --

#[test]
fn highlights_in_search_results() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("running", None, None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found > 0);

    for hit in &result.hits {
        assert!(
            !hit.highlights.as_ref().unwrap().is_empty(),
            "Expected highlights for query 'running'"
        );
        let hl = &hit.highlights.as_ref().unwrap()[0];
        assert_eq!(hl.field, "title");
        // snippet is always set; short fields carry the full marked text
        let snippet = hl.snippet.as_ref().expect("snippet should be set for text fields");
        assert!(
            snippet.contains("<mark>"),
            "Snippet should contain <mark> tags: {}",
            snippet
        );
        // value requires highlight_full_fields (Typesense v30.1 semantics)
        assert!(hl.value.is_none());
    }
}

#[test]
fn no_highlights_for_filter_only() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", None, None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    for hit in &result.hits {
        assert!(hit.highlights.as_ref().unwrap().is_empty());
    }
}

// -- Pagination tests --

#[test]
fn pagination_with_sort() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params1 = make_params("*", Some("price:asc"), None);
    params1.per_page = 3;
    let result1 = unwrap_ungrouped(search::execute(&index, &schema, &params1).unwrap());
    assert_eq!(result1.found, 10);
    assert_eq!(result1.hits.len(), 3);

    let mut params2 = make_params("*", Some("price:asc"), None);
    params2.per_page = 3;
    params2.page = 2;
    let result2 = unwrap_ungrouped(search::execute(&index, &schema, &params2).unwrap());
    assert_eq!(result2.hits.len(), 3);

    let ids1: Vec<String> = result1
        .hits
        .iter()
        .map(|h| h.document["id"].as_str().unwrap().to_string())
        .collect();
    let ids2: Vec<String> = result2
        .hits
        .iter()
        .map(|h| h.document["id"].as_str().unwrap().to_string())
        .collect();
    for id in &ids1 {
        assert!(!ids2.contains(id), "Page overlap: doc {id} in both pages");
    }

    let p1_max = result1.hits.last().unwrap().document["price"]
        .as_f64()
        .unwrap();
    let p2_min = result2.hits[0].document["price"].as_f64().unwrap();
    assert!(
        p1_max <= p2_min,
        "Page 1 max {} should <= page 2 min {}",
        p1_max,
        p2_min
    );
}

#[test]
fn pagination_last_page_partial() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*", Some("price:asc"), None);
    params.per_page = 4;
    params.page = 3;
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 10);
    assert_eq!(result.hits.len(), 2);
}

#[test]
fn pagination_beyond_results() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*", None, None);
    params.page = 100;
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(result.found, 10);
    assert_eq!(result.hits.len(), 0);
}

// -- Combined tests --

#[test]
fn sort_facet_and_highlight_combined() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("running", Some("price:asc"), Some("color,brand"));
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found > 0);
    assert_eq!(result.facet_counts.len(), 2);

    let prices: Vec<f64> = result
        .hits
        .iter()
        .map(|h| h.document["price"].as_f64().unwrap())
        .collect();
    for w in prices.windows(2) {
        assert!(w[0] <= w[1], "Expected ascending: {} <= {}", w[0], w[1]);
    }

    for hit in &result.hits {
        assert!(!hit.highlights.as_ref().unwrap().is_empty());
    }
}

// -- Typesense v30.1 compatibility tests --

#[test]
fn highlight_object_present_for_text_queries() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("running", None, None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found > 0);

    for hit in &result.hits {
        // highlight (singular) object should be set when there are highlights
        let hl_obj = &hit.highlight;
        assert!(hl_obj.is_object(), "highlight should be a JSON object");
        let obj = hl_obj.as_object().unwrap();
        assert!(obj.contains_key("title"), "highlight object should contain 'title' field");

        let title_hl = &obj["title"];
        assert!(title_hl["matched_tokens"].is_array());
        assert!(title_hl["snippet"].is_string());
    }
}

#[test]
fn text_match_info_present_for_text_queries() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("running shoes", None, None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found > 0);

    for hit in &result.hits {
        let tmi = hit.text_match_info.as_ref().expect("text_match_info should be set for text queries");
        assert!(tmi.fields_matched > 0, "Should have at least one field matched");
        assert!(tmi.tokens_matched > 0, "Should have at least one token matched");
        // score should be a parseable u64 string
        tmi.score.parse::<u64>().expect("score should be a u64 string");
    }
}

#[test]
fn text_match_info_absent_for_wildcard() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("*", None, None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    for hit in &result.hits {
        assert!(hit.text_match_info.is_none(), "text_match_info should be None for wildcard queries");
        assert_eq!(hit.highlight, serde_json::json!({}), "a wildcard hit's highlight is empty, as Typesense's");
    }
}

#[test]
fn search_cutoff_is_false_for_small_result_sets() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let params = make_params("running", None, None);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(!result.search_cutoff, "search_cutoff should be false for small result sets");
}

#[test]
fn highlight_full_fields_emits_value() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("running", None, None);
    params.highlight_full_fields = Some("title".into());
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found > 0);
    for hit in &result.hits {
        let hl = &hit.highlights.as_ref().unwrap()[0];
        let value = hl.value.as_ref().expect("value emitted for full field");
        assert!(value.contains("<mark>"));
    }
}

#[test]
fn snippet_threshold_windows_long_fields() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Force windowing on short titles by lowering the threshold to 1 token
    let mut params = make_params("running", None, None);
    params.snippet_threshold = Some(1);
    params.highlight_affix_num_tokens = Some(1);
    let result = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert!(result.found > 0);
    let snippet = result.hits[0].highlights.as_ref().unwrap()[0]
        .snippet
        .as_ref()
        .expect("windowed snippet");
    assert!(snippet.contains("<mark>"));
}

/// facet_sample_percent asks Typesense to estimate counts from a sample; counting is exact here at
/// any size, so the counts are exact and not marked sampled.
#[test]
fn facet_counts_are_exact_when_sampling_is_asked() {
    let (registry, schema) = setup_products();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let counts = |r: &search::SearchResult| r.facet_counts[0].counts.iter().map(|c| (c.value.clone(), c.count)).collect::<Vec<_>>();
    let full = unwrap_ungrouped(search::execute(&index, &schema, &make_params("*", None, Some("color"))).unwrap());
    let mut params = make_params("*", None, Some("color"));
    params.facet_sample_percent = Some(50);
    params.facet_sample_threshold = Some(1);
    let sampled = unwrap_ungrouped(search::execute(&index, &schema, &params).unwrap());
    assert_eq!(counts(&sampled), counts(&full));
    assert!(!sampled.facet_counts[0].sampled);
}

// -- Typesense's sort order: ties, missing values, query_by objects --

fn ids(result: &search::SearchResult) -> Vec<String> {
    result.hits.iter().map(|h| h.document["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn sort_ties_keep_the_latest_inserted_first_both_ways() {
    let (registry, schema) = setup_products();
    let index = registry.get("products").unwrap().snapshot();
    let desc = unwrap_ungrouped(search::execute(&index, &schema, &make_params("*", Some("rating:desc"), None)).unwrap());
    assert_eq!(ids(&desc), ["5", "1", "7", "3", "0", "8", "6", "2", "9", "4"]);
    let asc = unwrap_ungrouped(search::execute(&index, &schema, &make_params("*", Some("rating:asc"), None)).unwrap());
    assert_eq!(ids(&asc), ["9", "4", "8", "6", "2", "7", "3", "0", "5", "1"]);
}

fn setup_optional() -> (Arc<CollectionRegistry>, Schema) {
    let mut schema = product_schema();
    schema.name = "optional".into();
    schema.fields = vec![
        make_field("title", FieldType::String, true, false),
        make_field("n", FieldType::Int32, true, false),
    ];
    for f in &mut schema.fields {
        f.optional = true;
    }
    schema.assign_field_ids();
    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();
    let collection = registry.get("optional").unwrap();
    let rows: [(Option<&str>, Option<i32>); 6] =
        [(Some("b"), Some(30)), (None, Some(10)), (Some("a"), None), (Some("b"), Some(20)), (None, Some(10)), (Some("c"), None)];
    let docs = rows
        .iter()
        .enumerate()
        .map(|(i, (t, n))| Document {
            id: i as DocId,
            external_id: i.to_string(),
            language: Language::English,
            fields: vec![
                t.map_or(FieldValue::Null, |t| FieldValue::Text(t.into())),
                n.map_or(FieldValue::Null, FieldValue::Int32),
            ]
            .into(),
            record: None,
        })
        .collect();
    collection.swap_index(build_index(&collection.schema, docs));
    (registry, (*collection.schema).clone())
}

#[test]
fn numbers_without_a_value_sort_last_both_ways_strings_first_descending() {
    let (registry, schema) = setup_optional();
    let index = registry.get("optional").unwrap().snapshot();
    let run = |sort: &str| ids(&unwrap_ungrouped(search::execute(&index, &schema, &make_params("*", Some(sort), None)).unwrap()));
    assert_eq!(run("n:desc"), ["0", "3", "4", "1", "5", "2"]);
    assert_eq!(run("n:asc"), ["4", "1", "3", "0", "5", "2"]);
    assert_eq!(run("n(missing_values: first):desc"), ["5", "2", "0", "3", "4", "1"]);
    assert_eq!(run("title:asc"), ["2", "3", "0", "5", "4", "1"]);
    assert_eq!(run("title:desc"), ["4", "1", "5", "3", "0", "2"]);
    assert_eq!(run("title(missing_values: last):desc"), ["5", "3", "0", "2", "4", "1"]);
    // Two sort fields: ties on the first go to the second, then to the latest inserted
    assert_eq!(run("n:desc,title:asc"), ["0", "3", "4", "1", "2", "5"]);
    let bad = search::execute(&index, &schema, &make_params("*", Some("n(missing_values: middle):desc"), None));
    assert!(bad.is_err());
}

#[test]
fn query_by_an_object_searches_its_nested_string_fields() {
    let mut schema = product_schema();
    schema.name = "nested".into();
    schema.enable_nested_fields = true;
    schema.fields = vec![
        make_field("name", FieldType::String, false, false),
        make_field("addr.city", FieldType::String, false, false),
        make_field("addr.street", FieldType::String, false, false),
        make_field("addr.zip", FieldType::Int32, false, false),
        make_field("address", FieldType::String, false, false),
    ];
    schema.assign_field_ids();
    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();
    let collection = registry.get("nested").unwrap();
    let docs = [("Acme", "Lund", "Storgatan", "Malmo"), ("Malmo Bygg", "Lund", "Kungsgatan", "Lund"), ("Beta", "Malmo", "Torget", "Lund")]
        .iter()
        .enumerate()
        .map(|(i, (name, city, street, address))| Document {
            id: i as DocId,
            external_id: i.to_string(),
            language: Language::English,
            fields: vec![
                FieldValue::Text((*name).into()),
                FieldValue::Text((*city).into()),
                FieldValue::Text((*street).into()),
                FieldValue::Int32(22100),
                FieldValue::Text((*address).into()),
            ]
            .into(),
            record: None,
        })
        .collect();
    collection.swap_index(build_index(&collection.schema, docs));
    let schema = (*collection.schema).clone();
    let index = collection.snapshot();
    let query = |q: &str, query_by: &str| {
        let mut p = make_params(q, None, None);
        p.query_by = query_by.split(',').map(String::from).collect();
        search::execute(&index, &schema, &p).map(|r| {
            let mut v = ids(&unwrap_ungrouped(r));
            v.sort();
            v
        })
    };
    // `addr` is addr.city and addr.street (addr.zip is not a string; `address` is no child)
    assert_eq!(query("malmo", "addr").unwrap(), ["2"]);
    assert_eq!(query("torget", "addr").unwrap(), ["2"]);
    assert_eq!(query("malmo", "name,addr").unwrap(), ["1", "2"]);
    assert_eq!(query("malmo", "addr*").unwrap(), ["0", "2"]);
    assert_eq!(query("x", "nope").err().unwrap(), "Could not find a field named `nope` in the schema.");
    assert_eq!(query("x", "addr.zip").err().unwrap(), "Field `addr.zip` should be a string or a string array.");
}
