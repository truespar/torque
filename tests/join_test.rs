use std::sync::Arc;
use torque::collection::alias::AliasRegistry;
use torque::collection::CollectionRegistry;
use torque::filter::ops::JoinContext;
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

fn make_ref_field(name: &str, field_type: FieldType, reference: &str) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type,
        facet: false,
        index: true,
        optional: false,
        sort: false,
        stem: false,
        infix: false,
        locale: None,
        num_dim: None,
        vec_dist: None,
        id: 0,
        reference: Some(reference.to_string()),
        drop: false,
        stem_dictionary: None,
        embed: None,
        name_matching: false,
    }
}

fn make_schema(name: &str, fields: Vec<FieldDef>) -> Schema {
    Schema {
        name: name.into(),
        fields,
        default_sorting_field: None,
        enable_nested_fields: false,
        symbols_to_index: vec![],
        token_separators: vec![],
        default_language: Language::default(),
        persistence_enabled: true,
        index_mode: Default::default(),
        name_to_id: Default::default(),
        geopoint_field_names: vec![],
        nested_layout: None,
        registry: Default::default(),
        metadata: None,
        synonym_sets: None,
        curation_sets: None,
    }
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

/// Set up two collections: `brands` (target) and `products` (has reference to brands).
/// brands: id, name (text), country (text, facet), revenue (int64, sort)
/// products: id, title (text), price (float64, sort), brand_id (string, reference -> brands.id)
fn setup_join_collections() -> (Arc<CollectionRegistry>, Arc<AliasRegistry>, Schema, Schema) {
    let registry = Arc::new(CollectionRegistry::new(None));

    // -- brands collection --
    let mut brand_schema = make_schema("brands", vec![
        make_field("name", FieldType::Text, false, true),
        make_field("country", FieldType::String, false, true),
        make_field("revenue", FieldType::Int64, true, true),
    ]);
    brand_schema.assign_field_ids();
    registry.create(brand_schema.clone()).unwrap();

    let brands_coll = registry.get("brands").unwrap();
    let brand_docs = vec![
        Document {
            id: 0,
            external_id: "brand_nike".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Nike".into()),
                FieldValue::Text("USA".into()),
                FieldValue::Int64(51_200_000_000),
            ].into(),
            record: None,
        },
        Document {
            id: 1,
            external_id: "brand_adidas".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Adidas".into()),
                FieldValue::Text("Germany".into()),
                FieldValue::Int64(23_600_000_000),
            ].into(),
            record: None,
        },
        Document {
            id: 2,
            external_id: "brand_puma".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Puma".into()),
                FieldValue::Text("Germany".into()),
                FieldValue::Int64(8_600_000_000),
            ].into(),
            record: None,
        },
    ];
    let brand_index = build_index(&brands_coll.schema, brand_docs);
    brands_coll.swap_index(brand_index);

    // -- products collection (references brands) --
    let mut product_schema = make_schema("products", vec![
        make_field("title", FieldType::Text, false, false),
        make_field("price", FieldType::Float64, true, false),
        make_ref_field("brand_id", FieldType::String, "brands.id"),
    ]);
    product_schema.assign_field_ids();
    registry.create(product_schema.clone()).unwrap();

    let products_coll = registry.get("products").unwrap();
    let product_docs = vec![
        Document {
            id: 0,
            external_id: "prod_0".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Red Running Shoes".into()),
                FieldValue::Float64(89.99),
                FieldValue::Text("brand_nike".into()),
            ].into(),
            record: None,
        },
        Document {
            id: 1,
            external_id: "prod_1".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Blue Running Shoes".into()),
                FieldValue::Float64(79.99),
                FieldValue::Text("brand_adidas".into()),
            ].into(),
            record: None,
        },
        Document {
            id: 2,
            external_id: "prod_2".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Green Running Hat".into()),
                FieldValue::Float64(29.99),
                FieldValue::Text("brand_nike".into()),
            ].into(),
            record: None,
        },
        Document {
            id: 3,
            external_id: "prod_3".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Red Sport Jacket".into()),
                FieldValue::Float64(149.99),
                FieldValue::Text("brand_puma".into()),
            ].into(),
            record: None,
        },
        Document {
            id: 4,
            external_id: "prod_4".into(),
            language: Language::English,
            fields: vec![
                FieldValue::Text("Blue Casual Shoes".into()),
                FieldValue::Float64(59.99),
                FieldValue::Text("brand_adidas".into()),
            ].into(),
            record: None,
        },
    ];
    let product_index = build_index(&products_coll.schema, product_docs);
    products_coll.swap_index(product_index);

    let aliases = Arc::new(AliasRegistry::new());
    (registry, aliases, product_schema, brand_schema)
}

fn make_join_context<'a>(
    registry: &'a CollectionRegistry,
    aliases: &'a AliasRegistry,
    collection_name: &'a str,
    _index: &'a torque::index::Index,
) -> JoinContext<'a> {
    JoinContext::new(registry, aliases, collection_name)
}

// -- JOIN filter tests --

#[test]
fn join_filter_by_target_field() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    params.filter_by = Some("$brands(country:=USA)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    // Nike is USA -> products 0 and 2 reference Nike
    assert_eq!(result.found, 2, "Expected 2 products from USA brands, got {}", result.found);
    let ext_ids: Vec<&str> = result.hits.iter()
        .map(|h| h.document["id"].as_str().unwrap())
        .collect();
    assert!(ext_ids.contains(&"prod_0"));
    assert!(ext_ids.contains(&"prod_2"));
}

#[test]
fn join_filter_by_target_field_germany() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    params.filter_by = Some("$brands(country:=Germany)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    // Adidas + Puma are Germany -> products 1, 3, 4
    assert_eq!(result.found, 3, "Expected 3 products from German brands, got {}", result.found);
}

#[test]
fn join_filter_combined_with_local_filter() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    // Products from German brands AND price > 50
    params.filter_by = Some("$brands(country:=Germany) && price:>50".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    // German brands: Adidas (prod_1=79.99, prod_4=59.99) + Puma (prod_3=149.99)
    // Price > 50: prod_1, prod_3, prod_4 all qualify -> 3
    assert_eq!(result.found, 3, "Expected 3 products (German brands, price>50), got {}", result.found);
}

#[test]
fn join_filter_no_match_returns_empty() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    params.filter_by = Some("$brands(country:=Japan)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    assert_eq!(result.found, 0);
}

#[test]
fn join_filter_unknown_collection_returns_empty() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    params.filter_by = Some("$nonexistent(name:=Foo)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    assert_eq!(result.found, 0);
}

// -- JOIN sort tests --

#[test]
fn join_sort_by_target_field_desc() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    params.sort_by = Some("$brands(revenue:desc)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    assert_eq!(result.found, 5);

    // Nike (51.2B) > Adidas (23.6B) > Puma (8.6B)
    // Nike products should come first, then Adidas, then Puma
    let ext_ids: Vec<&str> = result.hits.iter()
        .map(|h| h.document["id"].as_str().unwrap())
        .collect();

    // First two should be Nike products (prod_0, prod_2)
    let nike_positions: Vec<usize> = ext_ids.iter().enumerate()
        .filter(|(_, id)| **id == "prod_0" || **id == "prod_2")
        .map(|(i, _)| i)
        .collect();
    let puma_position = ext_ids.iter().position(|id| *id == "prod_3").unwrap();

    for &pos in &nike_positions {
        assert!(pos < puma_position,
            "Nike products should sort before Puma (Nike pos={}, Puma pos={})", pos, puma_position);
    }
}

#[test]
fn join_sort_by_target_field_asc() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    params.sort_by = Some("$brands(revenue:asc)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    assert_eq!(result.found, 5);

    // Puma (8.6B) < Adidas (23.6B) < Nike (51.2B)
    let ext_ids: Vec<&str> = result.hits.iter()
        .map(|h| h.document["id"].as_str().unwrap())
        .collect();

    let puma_position = ext_ids.iter().position(|id| *id == "prod_3").unwrap();
    let nike_positions: Vec<usize> = ext_ids.iter().enumerate()
        .filter(|(_, id)| **id == "prod_0" || **id == "prod_2")
        .map(|(i, _)| i)
        .collect();

    for &pos in &nike_positions {
        assert!(pos > puma_position,
            "Nike products should sort after Puma in asc (Nike pos={}, Puma pos={})", pos, puma_position);
    }
}

// -- JOIN facet tests --

#[test]
fn join_facet_by_target_string_field() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    params.facet_by = Some("$brands(country)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    assert_eq!(result.found, 5);
    assert_eq!(result.facet_counts.len(), 1, "Expected 1 facet, got {}", result.facet_counts.len());

    let country_facet = &result.facet_counts[0];
    assert_eq!(country_facet.field_name, "$brands(country)");
    let germany = country_facet.counts.iter().find(|c| c.value == "Germany").unwrap();
    assert_eq!(germany.facet_filter.as_deref(), Some("$brands(country: `Germany`)"));

    // Facet counts LOCAL products per target field value:
    // Germany: prod_1 (Adidas) + prod_3 (Puma) + prod_4 (Adidas) = 3 products
    // USA: prod_0 (Nike) + prod_2 (Nike) = 2 products
    let germany_count = country_facet.counts.iter()
        .find(|c| c.value == "Germany")
        .map(|c| c.count)
        .unwrap_or(0);
    let usa_count = country_facet.counts.iter()
        .find(|c| c.value == "USA")
        .map(|c| c.count)
        .unwrap_or(0);

    assert_eq!(germany_count, 3, "Germany should have 3 products");
    assert_eq!(usa_count, 2, "USA should have 2 products");
}

#[test]
fn join_facet_by_target_numeric_field() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    params.facet_by = Some("$brands(revenue)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    assert_eq!(result.facet_counts.len(), 1);

    let revenue_facet = &result.facet_counts[0];
    assert_eq!(revenue_facet.field_name, "$brands(revenue)");
    let stats = &revenue_facet.stats;

    // Min: Puma 8.6B, Max: Nike 51.2B
    assert_eq!(stats.min, Some(8_600_000_000.0));
    assert_eq!(stats.max, Some(51_200_000_000.0));
    assert_eq!(stats.total_values, revenue_facet.counts.len());
    assert!(revenue_facet.counts.iter().all(|c| c.facet_filter.as_deref().is_some_and(|f| f.starts_with("$brands(revenue: "))));
}

#[test]
fn join_facet_combined_with_local_facet() {
    let (registry, aliases, product_schema, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let mut params = make_params("*");
    // No local facet fields in products schema have facet=true, so just test JOIN facet works
    params.facet_by = Some("$brands(country)".into());

    let join_ctx = make_join_context(&registry, &aliases, "products", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &product_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    assert!(!result.facet_counts.is_empty());
}

// -- Reference index unit tests --

#[test]
fn reference_index_built_correctly() {
    let (registry, _aliases, _, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    // Should have exactly one reference index (brand_id -> brands.id)
    assert_eq!(index.reference_indexes.len(), 1, "Expected 1 reference index");

    let ref_idx = index.reference_indexes.values().next().unwrap();
    assert_eq!(ref_idx.target_collection, "brands");
    assert_eq!(ref_idx.target_field, "id");

    // Forward lookup: prod_0 -> brand_nike
    let fwd = ref_idx.forward_lookup(0);
    assert_eq!(fwd.len(), 1);
    assert_eq!(&*fwd[0], "brand_nike");

    // Forward lookup: prod_1 -> brand_adidas
    let fwd = ref_idx.forward_lookup(1);
    assert_eq!(&*fwd[0], "brand_adidas");

    // Reverse lookup: brand_nike -> prod_0, prod_2
    let bitmap = ref_idx.reverse_lookup("brand_nike").unwrap();
    assert_eq!(bitmap.len(), 2);
    assert!(bitmap.contains(0));
    assert!(bitmap.contains(2));

    // Reverse lookup: brand_adidas -> prod_1, prod_4
    let bitmap = ref_idx.reverse_lookup("brand_adidas").unwrap();
    assert_eq!(bitmap.len(), 2);
    assert!(bitmap.contains(1));
    assert!(bitmap.contains(4));
}

#[test]
fn reference_index_reverse_lookup_many() {
    let (registry, _aliases, _, _) = setup_join_collections();
    let products = registry.get("products").unwrap();
    let index = products.snapshot();

    let ref_idx = index.reference_indexes.values().next().unwrap();

    // Lookup many: Nike + Adidas -> prod_0, prod_1, prod_2, prod_4
    let bitmap = ref_idx.reverse_lookup_many(["brand_nike", "brand_adidas"].iter().copied());
    assert_eq!(bitmap.len(), 4);
    assert!(bitmap.contains(0));
    assert!(bitmap.contains(1));
    assert!(bitmap.contains(2));
    assert!(bitmap.contains(4));
}

// -- Bidirectional JOIN tests --
// Query brands (no reference field) with $products(...) filter.
// Products have reference -> brands, so this uses the reverse JOIN path.

#[test]
fn bidirectional_join_filter_from_target_side() {
    let (registry, aliases, _, brand_schema) = setup_join_collections();
    let brands = registry.get("brands").unwrap();
    let index = brands.snapshot();

    let mut params = make_params("*");
    params.query_by = vec!["name".into()];
    // Find brands that have products with price > 100
    params.filter_by = Some("$products(price:>100)".into());

    let join_ctx = make_join_context(&registry, &aliases, "brands", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &brand_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    // Only product with price>100 is prod_3 (149.99) which references brand_puma
    assert_eq!(result.found, 1, "Expected 1 brand with products > $100, got {}", result.found);
    let brand_id = result.hits[0].document["id"].as_str().unwrap();
    assert_eq!(brand_id, "brand_puma");
}

#[test]
fn bidirectional_join_filter_multiple_matches() {
    let (registry, aliases, _, brand_schema) = setup_join_collections();
    let brands = registry.get("brands").unwrap();
    let index = brands.snapshot();

    let mut params = make_params("*");
    params.query_by = vec!["name".into()];
    // Find brands that have products with price > 50
    params.filter_by = Some("$products(price:>50)".into());

    let join_ctx = make_join_context(&registry, &aliases, "brands", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &brand_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    // prod_0 (89.99->Nike), prod_1 (79.99->Adidas), prod_3 (149.99->Puma), prod_4 (59.99->Adidas)
    // All 3 brands have at least one product > $50
    assert_eq!(result.found, 3, "Expected 3 brands with products > $50, got {}", result.found);
}

#[test]
fn bidirectional_join_filter_no_match() {
    let (registry, aliases, _, brand_schema) = setup_join_collections();
    let brands = registry.get("brands").unwrap();
    let index = brands.snapshot();

    let mut params = make_params("*");
    params.query_by = vec!["name".into()];
    // Find brands that have products with price > 1000 (none exist)
    params.filter_by = Some("$products(price:>1000)".into());

    let join_ctx = make_join_context(&registry, &aliases, "brands", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &brand_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    assert_eq!(result.found, 0);
}

#[test]
fn bidirectional_join_combined_with_local_filter() {
    let (registry, aliases, _, brand_schema) = setup_join_collections();
    let brands = registry.get("brands").unwrap();
    let index = brands.snapshot();

    let mut params = make_params("*");
    params.query_by = vec!["name".into()];
    // Find German brands that have products with price > 50
    params.filter_by = Some("$products(price:>50) && country:=Germany".into());

    let join_ctx = make_join_context(&registry, &aliases, "brands", &index);
    let (result, _) = search::execute_with_stores_gpu_join(
        &index, &brand_schema, &params, None, None,
        #[cfg(feature = "cuda")]
        None,
        Some(&join_ctx),
    ).unwrap();

    let result = unwrap_ungrouped(result);
    // German brands with products > $50: Adidas (prod_1=79.99, prod_4=59.99), Puma (prod_3=149.99)
    assert_eq!(result.found, 2, "Expected 2 German brands with products > $50, got {}", result.found);
}
