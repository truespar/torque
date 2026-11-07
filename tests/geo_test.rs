use torque::collection::CollectionRegistry;
use torque::ingest;
use torque::schema::{Document, FieldDef, FieldType, FieldValue, Language, Schema};
use torque::search;

fn geo_schema() -> Schema {
    let mut schema = Schema {
        index_mode: Default::default(),
        name: "places".into(),
        fields: vec![
            FieldDef {
                name: "name".into(),
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
                name: "location".into(),
                field_type: FieldType::GeoPoint,
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

fn make_place(id: u32, name: &str, category: &str, lat: f64, lng: f64) -> Document {
    Document {
        id,
        external_id: format!("{id}"),
        language: Language::English,
        fields: vec![
            FieldValue::Text(name.into()),
            FieldValue::Text(category.into()),
            FieldValue::GeoPoint { lat, lng },
        ].into(),
        record: None,
    }
}

fn setup() -> CollectionRegistry {
    let schema = geo_schema();
    let registry = CollectionRegistry::new(None);
    let collection = registry.create(schema.clone()).unwrap();

    // European cities with known coordinates
    let docs = vec![
        make_place(0, "Eiffel Tower restaurant", "restaurant", 48.8584, 2.2945),
        make_place(1, "Louvre Museum cafe", "museum", 48.8606, 2.3376),
        make_place(2, "Notre Dame bakery", "bakery", 48.8530, 2.3499),
        make_place(3, "London Eye restaurant", "restaurant", 51.5033, -0.1195),
        make_place(4, "Tower Bridge cafe", "cafe", 51.5055, -0.0754),
        make_place(5, "Big Ben pub", "pub", 51.5007, -0.1246),
        make_place(6, "Berlin Wall museum", "museum", 52.5163, 13.3777),
        make_place(7, "Brandenburg Gate restaurant", "restaurant", 52.5163, 13.3777),
        make_place(8, "Stockholm Old Town cafe", "cafe", 59.3251, 18.0711),
        make_place(9, "Oslo Opera House restaurant", "restaurant", 59.9075, 10.7530),
    ];

    let index = ingest::build_index(&collection.schema, docs);
    collection.swap_index(index);

    registry
}

fn make_params(q: &str) -> search::SearchParams {
    search::SearchParams {
        q: q.into(),
        query_by: vec!["name".into()],
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

// --- Tests ---

#[test]
fn geo_radius_filter_paris() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // 5km radius around Paris center — should find Paris places only
    let mut params = make_params("*");
    params.filter_by = Some("location:(48.856, 2.352, 5 km)".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert_eq!(result.found, 3, "Should find 3 Paris places within 5km");
    for hit in &result.hits {
        let name = hit.document["name"].as_str().unwrap();
        assert!(
            name.contains("Eiffel") || name.contains("Louvre") || name.contains("Notre"),
            "Unexpected place: {name}"
        );
    }
}

#[test]
fn geo_radius_filter_london() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // 10km radius around London — should find London places
    let mut params = make_params("*");
    params.filter_by = Some("location:(51.505, -0.09, 10 km)".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert_eq!(result.found, 3, "Should find 3 London places within 10km");
}

#[test]
fn geo_radius_filter_very_small() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // 0.1km radius — very tight, should find only 1 or fewer
    let mut params = make_params("*");
    params.filter_by = Some("location:(48.8584, 2.2945, 0.1 km)".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert!(result.found <= 1, "Very small radius should find at most 1 place");
}

#[test]
fn geo_radius_filter_miles() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // 3 miles ≈ 4.8km radius around Paris
    let mut params = make_params("*");
    params.filter_by = Some("location:(48.856, 2.352, 3 mi)".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert!(result.found >= 2, "3mi radius around Paris should find at least 2 places");
}

#[test]
fn geo_sort_by_distance() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // Sort all places by distance from Paris center
    let mut params = make_params("*");
    params.sort_by = Some("location(48.856, 2.352):asc".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert_eq!(result.found, 10);

    // First 3 should be Paris places (closest)
    let first_three: Vec<&str> = result.hits[..3]
        .iter()
        .map(|h| h.document["name"].as_str().unwrap())
        .collect();
    for name in &first_three {
        assert!(
            name.contains("Eiffel") || name.contains("Louvre") || name.contains("Notre"),
            "First 3 should be Paris places, got: {name}"
        );
    }
}

#[test]
fn geo_sort_descending() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // Sort by distance descending — farthest first
    let mut params = make_params("*");
    params.sort_by = Some("location(48.856, 2.352):desc".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    // First result should be the farthest from Paris (Stockholm or Oslo)
    let first_name = result.hits[0].document["name"].as_str().unwrap();
    assert!(
        first_name.contains("Stockholm") || first_name.contains("Oslo"),
        "Farthest from Paris should be Nordic, got: {first_name}"
    );
}

#[test]
fn geo_distance_meters_in_response() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    let mut params = make_params("*");
    params.sort_by = Some("location(48.856, 2.352):asc".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    // All hits should have geo_distance_meters
    for hit in &result.hits {
        let geo_dist = hit.geo_distance_meters.as_ref()
            .expect("geo_distance_meters should be present");
        let dist = geo_dist.get("location")
            .expect("should have 'location' key")
            .as_u64()
            .expect("distance should be a number");
        assert!(dist > 0 || hit.document["name"].as_str().unwrap().contains("Eiffel"),
            "All distances should be > 0 (except maybe very close places)");
    }

    // First hit's distance should be small (< 5km = 5000m)
    let first_dist = result.hits[0].geo_distance_meters.as_ref().unwrap()
        .get("location").unwrap().as_u64().unwrap();
    assert!(first_dist < 5000, "First hit should be within 5km, got {first_dist}m");
}

#[test]
fn geo_filter_with_text_query() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // Search for "restaurant" within 5km of Paris
    let mut params = make_params("restaurant");
    params.filter_by = Some("location:(48.856, 2.352, 5 km)".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert_eq!(result.found, 1, "Only 1 restaurant within 5km of Paris center");
    let name = result.hits[0].document["name"].as_str().unwrap();
    assert!(name.contains("Eiffel"), "Should be Eiffel Tower restaurant, got: {name}");
}

#[test]
fn geo_filter_combined_with_category_filter() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // 1000km radius from Paris center AND category=restaurant
    let mut params = make_params("*");
    params.filter_by = Some("location:(48.856, 2.352, 1000 km) && category:=restaurant".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    // Paris + London restaurants within 1000km
    assert!(result.found >= 2, "Should find at least 2 restaurants within 1000km");
    for hit in &result.hits {
        let cat = hit.document["category"].as_str().unwrap();
        assert_eq!(cat, "restaurant");
    }
}

#[test]
fn geo_filter_and_sort_combined() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // Filter within 500km of London, sort by distance
    let mut params = make_params("*");
    params.filter_by = Some("location:(51.505, -0.09, 500 km)".into());
    params.sort_by = Some("location(51.505, -0.09):asc".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    // Should include London + Paris (≈344km away), not Berlin/Stockholm/Oslo
    assert!(result.found >= 3, "London + Paris within 500km");
    assert!(result.found <= 7, "Should exclude far cities");

    // Verify sorted by distance (ascending)
    let distances: Vec<u64> = result.hits.iter()
        .map(|h| h.geo_distance_meters.as_ref().unwrap()
            .get("location").unwrap().as_u64().unwrap())
        .collect();
    for w in distances.windows(2) {
        assert!(w[0] <= w[1], "Expected ascending distance: {} <= {}", w[0], w[1]);
    }
}

#[test]
fn geo_no_results_outside_radius() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();

    // Search near Antarctica — should find nothing
    let mut params = make_params("*");
    params.filter_by = Some("location:(-75.0, 0.0, 100 km)".into());
    let result = unwrap_ungrouped(search::execute(&index, &collection.schema, &params).unwrap());

    assert_eq!(result.found, 0, "No places near Antarctica");
}

#[test]
fn geo_polygon_filters() {
    let registry = setup();
    let collection = registry.get("places").unwrap();
    let index = collection.snapshot();
    let search = |filter: &str| {
        let mut params = make_params("*");
        params.filter_by = Some(filter.into());
        search::execute(&index, &collection.schema, &params)
    };
    let found = |filter: &str| {
        let mut names: Vec<String> =
            unwrap_ungrouped(search(filter).unwrap()).hits.iter().map(|h| h.document["name"].as_str().unwrap().to_string()).collect();
        names.sort();
        names
    };
    let paris = ["Eiffel Tower restaurant", "Louvre Museum cafe", "Notre Dame bakery"];

    // central Paris, either way round, in either form
    assert_eq!(found("location:(48.80, 2.25, 48.80, 2.40, 48.90, 2.40, 48.90, 2.25)"), paris);
    assert_eq!(found("location:([48.90, 2.25, 48.90, 2.40, 48.80, 2.40, 48.80, 2.25])"), paris);
    // a triangle around the Eiffel Tower
    assert_eq!(found("location:(48.85, 2.28, 48.87, 2.30, 48.85, 2.31)"), ["Eiffel Tower restaurant"]);
    // any value of a list, the bracketed radius among them
    let both = found("location:[(48.80, 2.25, 48.80, 2.40, 48.90, 2.40, 48.90, 2.25), ([51.5, -0.1], radius: 5 km)]");
    assert_eq!(both.len(), 6);
    assert!(paris.iter().all(|p| both.iter().any(|n| n == p)));
    // a crossing polygon is refused with S2's message
    let err = search("location:(48.80, 2.25, 48.90, 2.40, 48.80, 2.40, 48.90, 2.25)").err().unwrap();
    assert_eq!(err, "Polygon is invalid: Edge 0 crosses edge 2");
}
