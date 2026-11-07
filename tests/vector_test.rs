use torque::collection::CollectionRegistry;
use torque::ingest;
use torque::schema::{Document, FieldDef, FieldType, FieldValue, Language, Schema, VecDist};
use torque::search;

const DIM: u16 = 8;

fn vector_schema() -> Schema {
    let mut schema = Schema {
        index_mode: Default::default(),
        name: "products".into(),
        fields: vec![
            FieldDef {
                name: "title".into(),
                field_type: FieldType::String,
                facet: false,
                index: true,
                optional: false,
                sort: false,
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
            },
            FieldDef {
                name: "category".into(),
                field_type: FieldType::String,
                facet: true,
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
            },
            FieldDef {
                name: "embedding".into(),
                field_type: FieldType::FloatArray { num_dim: DIM },
                facet: false,
                index: true,
                optional: false,
                sort: false,
                stem: false,
                infix: false,
                locale: None,
                num_dim: Some(DIM),
                vec_dist: Some(VecDist::Cosine),
                id: 0,
                reference: None,
                drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            },
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
    };
    schema.assign_field_ids();
    schema
}

/// Generate a simple deterministic "embedding" for a doc.
/// The vector is based on a seed so similar docs have similar vectors.
fn make_vec(seed: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity(DIM as usize);
    let mut state = seed;
    for _ in 0..DIM {
        state = state.wrapping_mul(1103515245).wrapping_add(12345);
        v.push(((state >> 16) as f32 / 32768.0) - 1.0);
    }
    // Normalize for cosine similarity
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn make_doc(id: u32, title: &str, category: &str, embedding: Vec<f32>) -> Document {
    Document {
        id,
        external_id: format!("{id}"),
        language: Language::English,
        fields: vec![
            FieldValue::Text(title.into()),
            FieldValue::Text(category.into()),
            FieldValue::FloatArray(embedding),
        ].into(),
        record: None,
    }
}

fn setup() -> CollectionRegistry {
    let schema = vector_schema();
    let registry = CollectionRegistry::new(None);
    let collection = registry.create(schema.clone()).unwrap();

    // Use seeds so that electronics (100-104) are in one cluster,
    // clothing (200-204) in another
    let docs = vec![
        make_doc(0, "wireless headphones", "electronics", make_vec(100)),
        make_doc(1, "bluetooth speaker", "electronics", make_vec(101)),
        make_doc(2, "laptop stand", "electronics", make_vec(102)),
        make_doc(3, "USB-C charger", "electronics", make_vec(103)),
        make_doc(4, "gaming mouse", "electronics", make_vec(104)),
        make_doc(5, "cotton t-shirt", "clothing", make_vec(200)),
        make_doc(6, "denim jeans", "clothing", make_vec(201)),
        make_doc(7, "wool sweater", "clothing", make_vec(202)),
        make_doc(8, "silk scarf", "clothing", make_vec(203)),
        make_doc(9, "leather boots", "clothing", make_vec(204)),
    ];

    let index = ingest::build_index(&collection.schema, docs);
    collection.swap_index(index);

    registry
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

fn format_vec(v: &[f32]) -> String {
    v.iter()
        .map(|x| format!("{x:.6}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn unwrap_ungrouped(resp: search::SearchResponse) -> search::SearchResult {
    match resp {
        search::SearchResponse::Ungrouped(r) => r,
        _ => panic!("Expected ungrouped result"),
    }
}

// --- Tests ---

#[test]
fn vector_only_search() {
    let registry = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Search with a vector similar to electronics (seed 100)
    let query_vec = make_vec(100);
    let vq = format!("embedding:([{}], k:5)", format_vec(&query_vec));

    let mut params = make_params("*");
    params.vector_query = Some(vq);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert!(result.found > 0, "Vector search should return results");
    assert!(result.found <= 5, "Should return at most k=5 results");

    // The first result should be doc 0 (seed 100 — identical vector)
    let first_name = result.hits[0].document["title"].as_str().unwrap();
    assert_eq!(first_name, "wireless headphones", "Exact vector match should be first");

    // Should have vector_distance
    assert!(
        result.hits[0].vector_distance.is_some(),
        "vector_distance should be present"
    );
}

#[test]
fn vector_search_returns_k_results() {
    let registry = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    let query_vec = make_vec(100);
    let vq = format!("embedding:([{}], k:3)", format_vec(&query_vec));

    let mut params = make_params("*");
    params.vector_query = Some(vq);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert_eq!(result.found, 3, "Should return exactly k=3 results");
}

#[test]
fn vector_distance_is_correct_for_self() {
    let registry = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Query with exact vector of doc 0
    let query_vec = make_vec(100);
    let vq = format!("embedding:([{}], k:5)", format_vec(&query_vec));

    let mut params = make_params("*");
    params.vector_query = Some(vq);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    // First hit's cosine similarity should be ~1.0
    let dist = result.hits[0].vector_distance.unwrap();
    assert!(
        (dist - 1.0).abs() < 0.01,
        "Self-similarity should be ~1.0, got {dist}"
    );
}

#[test]
fn hybrid_search_with_rrf() {
    let registry = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Text query for "headphones" + vector query near electronics
    let query_vec = make_vec(100);
    let vq = format!("embedding:([{}], k:10)", format_vec(&query_vec));

    let mut params = make_params("headphones");
    params.vector_query = Some(vq);
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    // "wireless headphones" should be top result (appears in both lexical and vector)
    assert!(result.found > 0);
    let first_name = result.hits[0].document["title"].as_str().unwrap();
    assert!(
        first_name.contains("headphones"),
        "Hybrid search should rank headphones first, got: {first_name}"
    );

    // hybrid_search_info should be present with positive RRF score
    for hit in &result.hits {
        let info = hit.hybrid_search_info.as_ref().expect("hybrid_search_info should be present for hybrid results");
        assert!(info.rank_fusion_score > 0.0, "rank_fusion_score should be positive, got: {}", info.rank_fusion_score);
    }
}

#[test]
fn vector_search_with_pre_filter() {
    let registry = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Vector near electronics, but filter to clothing only
    let query_vec = make_vec(100);
    let vq = format!("embedding:([{}], k:10)", format_vec(&query_vec));

    let mut params = make_params("*");
    params.vector_query = Some(vq);
    params.filter_by = Some("category:=clothing".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    // All results should be clothing
    for hit in &result.hits {
        let cat = hit.document["category"].as_str().unwrap();
        assert_eq!(cat, "clothing", "Pre-filtered results should only be clothing");
    }
}

#[test]
fn wrong_dimensions_error() {
    let registry = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Wrong number of dimensions (3 instead of 8)
    let mut params = make_params("*");
    params.vector_query = Some("embedding:([0.1, 0.2, 0.3], k:5)".into());
    let result = search::execute(&index, &collection.schema, &params);
    match result {
        Err(ref err) => assert!(
            err.contains("dimension mismatch"),
            "Should report dimension mismatch, got: {err}"
        ),
        Ok(_) => panic!("Expected dimension mismatch error"),
    }
}

#[test]
fn invalid_vector_field_error() {
    let registry = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Try to vector search on a non-vector field
    let mut params = make_params("*");
    params.vector_query = Some("title:([0.1, 0.2], k:5)".into());
    let result = search::execute(&index, &collection.schema, &params);
    match result {
        Err(ref err) => assert!(
            err.contains("not a vector"),
            "Should report non-vector field, got: {err}"
        ),
        Ok(_) => panic!("Expected non-vector field error"),
    }
}

#[test]
fn vector_search_euclidean() {
    // Test with L2 distance metric
    let mut schema = Schema {
        index_mode: Default::default(),
        name: "l2test".into(),
        fields: vec![
            FieldDef {
                name: "title".into(),
                field_type: FieldType::String,
                facet: false,
                index: true,
                optional: false,
                sort: false,
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
            },
            FieldDef {
                name: "vec".into(),
                field_type: FieldType::FloatArray { num_dim: 4 },
                facet: false,
                index: true,
                optional: false,
                sort: false,
                stem: false,
                infix: false,
                locale: None,
                num_dim: Some(4),
                vec_dist: Some(VecDist::Euclidean),
                id: 0,
                reference: None,
                drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            },
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
    };
    schema.assign_field_ids();

    let registry = CollectionRegistry::new(None);
    let collection = registry.create(schema.clone()).unwrap();

    let docs = vec![
        Document {
            id: 0,
            external_id: "0".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("near".into()),
                FieldValue::FloatArray(vec![1.0, 0.0, 0.0, 0.0]),
            ].into(),
            record: None,
        },
        Document {
            id: 1,
            external_id: "1".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("far".into()),
                FieldValue::FloatArray(vec![10.0, 10.0, 10.0, 10.0]),
            ].into(),
            record: None,
        },
    ];

    let index = ingest::build_index(&collection.schema, docs);
    collection.swap_index(index);
    let index = collection.snapshot();

    let params = search::SearchParams {
        q: "*".into(),
        query_by: vec!["title".into()],
        filter_by: None,
        sort_by: None,
        facet_by: None,
        per_page: 10,
        page: 1,
        highlight_fields: None,
        group_by: None,
        group_limit: None,
        group_missing_values: None,
        text_match_type: None,
        stopwords: None,
        synonym_sets: None,
        preset: None,
        num_typos: None,
        typo_tokens_threshold: None,
        prefix: None,
        vector_query: Some("vec:([1.0, 0.0, 0.0, 0.0], k:2)".into()),
        ..Default::default()
    };

    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert_eq!(result.found, 2);
    // First should be "near" (distance 0)
    let first_name = result.hits[0].document["title"].as_str().unwrap();
    assert_eq!(first_name, "near");

    // Euclidean distance for self should be ~0
    let dist = result.hits[0].vector_distance.unwrap();
    assert!(dist < 0.01, "L2 distance to self should be ~0, got {dist}");
}

#[test]
fn vector_search_dot_product() {
    let mut schema = Schema {
        index_mode: Default::default(),
        name: "iptest".into(),
        fields: vec![
            FieldDef {
                name: "title".into(),
                field_type: FieldType::String,
                facet: false,
                index: true,
                optional: false,
                sort: false,
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
            },
            FieldDef {
                name: "vec".into(),
                field_type: FieldType::FloatArray { num_dim: 4 },
                facet: false,
                index: true,
                optional: false,
                sort: false,
                stem: false,
                infix: false,
                locale: None,
                num_dim: Some(4),
                vec_dist: Some(VecDist::DotProduct),
                id: 0,
                reference: None,
                drop: false,
                stem_dictionary: None,
                embed: None,
                name_matching: false,
            },
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
    };
    schema.assign_field_ids();

    let registry = CollectionRegistry::new(None);
    let collection = registry.create(schema.clone()).unwrap();

    let docs = vec![
        Document {
            id: 0,
            external_id: "0".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("high".into()),
                FieldValue::FloatArray(vec![1.0, 1.0, 1.0, 1.0]),
            ].into(),
            record: None,
        },
        Document {
            id: 1,
            external_id: "1".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("low".into()),
                FieldValue::FloatArray(vec![-1.0, -1.0, -1.0, -1.0]),
            ].into(),
            record: None,
        },
    ];

    let index = ingest::build_index(&collection.schema, docs);
    collection.swap_index(index);
    let index = collection.snapshot();

    let params = search::SearchParams {
        q: "*".into(),
        query_by: vec!["title".into()],
        filter_by: None,
        sort_by: None,
        facet_by: None,
        per_page: 10,
        page: 1,
        highlight_fields: None,
        group_by: None,
        group_limit: None,
        group_missing_values: None,
        text_match_type: None,
        stopwords: None,
        synonym_sets: None,
        preset: None,
        num_typos: None,
        typo_tokens_threshold: None,
        prefix: None,
        vector_query: Some("vec:([1.0, 1.0, 1.0, 1.0], k:2)".into()),
        ..Default::default()
    };

    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    // "high" has higher dot product with [1,1,1,1] → should be first
    let first_name = result.hits[0].document["title"].as_str().unwrap();
    assert_eq!(first_name, "high");

    // Dot product of [1,1,1,1] with itself = 4.0
    let dist = result.hits[0].vector_distance.unwrap();
    assert!(
        (dist - 4.0).abs() < 0.01,
        "Dot product of [1,1,1,1] with [1,1,1,1] should be 4.0, got {dist}"
    );
}

#[test]
fn recall_quality_integration() {
    // Test that HNSW recall is reasonable in an integration setting
    let registry = setup();
    let collection = registry.get("products").unwrap();
    let index = collection.snapshot();

    // Query with every doc's vector and check if self is in results
    let mut self_found = 0;
    for i in 0..10u32 {
        let seed = if i < 5 { 100 + i } else { 200 + i - 5 };
        let query_vec = make_vec(seed);
        let vq = format!("embedding:([{}], k:5)", format_vec(&query_vec));

        let mut params = make_params("*");
        params.vector_query = Some(vq);
        let result =
            unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

        let ext_ids: Vec<&str> = result
            .hits
            .iter()
            .map(|h| h.document["id"].as_str().unwrap())
            .collect();
        if ext_ids.contains(&format!("{i}").as_str()) {
            self_found += 1;
        }
    }

    assert!(
        self_found >= 8,
        "At least 80% of self-queries should find the original doc, got {self_found}/10"
    );
}
