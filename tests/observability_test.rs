use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use torque::collection::CollectionRegistry;
use torque::ingest::build_index;
use torque::observability::{CollectionLabels, IngestLabels, MetricsCollector, StatusLabels};
use torque::schema::*;
use torque::search;

// -- Test helpers --

fn make_field(name: &str, field_type: FieldType) -> FieldDef {
    FieldDef {
        name: name.into(),
        field_type,
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
    }
}

fn make_schema() -> Schema {
    Schema {
        index_mode: Default::default(),
        name: "products".into(),
        fields: vec![make_field("title", FieldType::String)],
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

fn setup_empty_registry() -> Arc<CollectionRegistry> {
    let mut schema = make_schema();
    schema.assign_field_ids();
    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema).unwrap();
    registry
}

fn setup_with_data() -> (Arc<CollectionRegistry>, Schema) {
    let mut schema = make_schema();
    schema.assign_field_ids();
    let registry = Arc::new(CollectionRegistry::new(None));
    let col = registry.create(schema.clone()).unwrap();

    let docs = vec![
        Document {
            id: 0,
            external_id: "0".into(),
            language: Language::English,
            fields: vec![FieldValue::Text("wireless headphones".into())].into(),
            record: None,
        },
        Document {
            id: 1,
            external_id: "1".into(),
            language: Language::English,
            fields: vec![FieldValue::Text("bluetooth speaker".into())].into(),
            record: None,
        },
    ];

    let index = build_index(&col.schema, docs);
    col.swap_index(index);
    (registry, schema)
}

// -- Health ready tests --

#[test]
fn health_ready_no_data_returns_not_ready() {
    let registry = setup_empty_registry();
    let collections = registry.list();
    let total = collections.len();
    let ready = collections
        .iter()
        .filter(|c| c.snapshot().total_docs() > 0)
        .count();

    assert_eq!(total, 1);
    assert_eq!(ready, 0);
}

#[test]
fn health_ready_with_data_returns_ready() {
    let (registry, _) = setup_with_data();
    let collections = registry.list();
    let total = collections.len();
    let ready = collections
        .iter()
        .filter(|c| c.snapshot().total_docs() > 0)
        .count();

    assert_eq!(total, 1);
    assert_eq!(ready, 1);
}

// -- Metrics endpoint tests --

#[test]
fn metrics_encode_produces_openmetrics_text() {
    let metrics = MetricsCollector::new();
    metrics.queries_total.inc();
    metrics.queries_total.inc();
    metrics.queries_total.inc();
    metrics.query_duration.observe(0.003);
    metrics
        .collection_docs
        .get_or_create(&CollectionLabels {
            collection: "products".into(),
        })
        .set(5000);

    let mut buffer = String::new();
    prometheus_client::encoding::text::encode(&mut buffer, &metrics.registry).unwrap();

    assert!(
        buffer.contains("torque_queries"),
        "Should contain queries metric"
    );
    assert!(
        buffer.contains("torque_query_duration_seconds"),
        "Should contain query duration metric"
    );
    assert!(
        buffer.contains("torque_collection_documents"),
        "Should contain collection docs metric"
    );
    assert!(
        buffer.contains("products"),
        "Should contain products label"
    );
    assert!(buffer.contains("# EOF"), "Should end with EOF marker");
}

// -- Query metrics tracking --

#[test]
fn query_metrics_incremented_after_search() {
    let metrics = Arc::new(MetricsCollector::new());
    let (registry, _) = setup_with_data();
    let col = registry.get("products").unwrap();
    let index = col.snapshot();

    let params = search::SearchParams {
        q: "headphones".into(),
        query_by: vec!["title".into()],
        filter_by: None,
        sort_by: None,
        facet_by: None,
        per_page: 10,
        page: 1,
        ..Default::default()
    };

    assert_eq!(metrics.queries_total.get(), 0);

    // Simulate what the search handler does
    let start = Instant::now();
    let result = search::execute_with_stores(&index, &col.schema, &params, None, None);
    let elapsed = start.elapsed();

    metrics.queries_total.inc();
    metrics.query_duration.observe(elapsed.as_secs_f64());
    metrics
        .latency_tracker
        .record(elapsed.as_secs_f64() * 1000.0);

    assert!(result.is_ok());
    assert_eq!(metrics.queries_total.get(), 1);
    assert!(metrics.latency_tracker.count() == 1);
}

// -- Admin stats --

#[test]
fn admin_stats_structure() {
    let metrics = Arc::new(MetricsCollector::new());
    let (registry, _) = setup_with_data();
    let start_time = Instant::now();

    metrics.queries_total.inc();
    metrics.queries_total.inc();
    metrics.latency_tracker.record(2.5);
    metrics.latency_tracker.record(3.1);

    let stats =
        torque::observability::stats::build_admin_stats(&metrics, &registry, start_time, 8109, Path::new("."));

    assert_eq!(stats.system.torque_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(stats.system.ingest_server.port, 8109);
    assert!(stats.system.cpu.cores_available > 0);
    assert!(stats.system.cpu.rayon_threads > 0);
    assert_eq!(stats.queries.total, 2);
    assert_eq!(stats.collections.len(), 1);
    assert_eq!(stats.collections[0].name, "products");
    assert_eq!(stats.collections[0].num_documents, 2);
    assert_eq!(stats.collections[0].num_fields, 1);
}

// -- Ingest metrics --

#[test]
fn ingest_metrics_recorded() {
    let metrics = Arc::new(MetricsCollector::new());

    // Simulate ingest session success
    metrics
        .ingest_sessions_total
        .get_or_create(&IngestLabels {
            collection: "products".into(),
            status: "success".into(),
        })
        .inc();
    metrics
        .ingest_duration
        .get_or_create(&CollectionLabels {
            collection: "products".into(),
        })
        .observe(1.5);

    let success_count = metrics
        .ingest_sessions_total
        .get_or_create(&IngestLabels {
            collection: "products".into(),
            status: "success".into(),
        })
        .get();
    assert_eq!(success_count, 1);
}

// -- HTTP error metrics --

#[test]
fn http_error_metrics_recorded() {
    let metrics = MetricsCollector::new();

    metrics
        .http_errors_total
        .get_or_create(&StatusLabels { status_code: 404 })
        .inc();
    metrics
        .http_errors_total
        .get_or_create(&StatusLabels { status_code: 404 })
        .inc();
    metrics
        .http_errors_total
        .get_or_create(&StatusLabels { status_code: 500 })
        .inc();

    let mut buffer = String::new();
    prometheus_client::encoding::text::encode(&mut buffer, &metrics.registry).unwrap();

    assert!(
        buffer.contains("torque_http_errors"),
        "Should contain HTTP errors metric"
    );
}

// -- Active sessions gauge --

#[test]
fn ingest_active_sessions_gauge() {
    let metrics = MetricsCollector::new();

    assert_eq!(metrics.ingest_active_sessions.get(), 0);
    metrics.ingest_active_sessions.inc();
    metrics.ingest_active_sessions.inc();
    assert_eq!(metrics.ingest_active_sessions.get(), 2);
    metrics.ingest_active_sessions.dec();
    assert_eq!(metrics.ingest_active_sessions.get(), 1);
}

// -- Multiple collections in stats --

#[test]
fn admin_stats_multiple_collections() {
    let metrics = Arc::new(MetricsCollector::new());
    let registry = Arc::new(CollectionRegistry::new(None));

    let mut schema_a = Schema {
        index_mode: Default::default(),
        name: "products".into(),
        fields: vec![make_field("title", FieldType::String)],
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
    schema_a.assign_field_ids();
    registry.create(schema_a).unwrap();

    let mut schema_b = Schema {
        index_mode: Default::default(),
        name: "orders".into(),
        fields: vec![
            make_field("description", FieldType::String),
            make_field("status", FieldType::String),
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
    schema_b.assign_field_ids();
    let col_b = registry.create(schema_b.clone()).unwrap();

    // Ingest data into only orders
    let docs = vec![Document {
        id: 0,
        external_id: "0".into(),
        language: Language::English,
        fields: vec![
            FieldValue::Text("order one".into()),
            FieldValue::Text("pending".into()),
        ].into(),
        record: None,
    }];
    let index = build_index(&col_b.schema, docs);
    col_b.swap_index(index);

    let stats =
        torque::observability::stats::build_admin_stats(&metrics, &registry, Instant::now(), 8109, Path::new("."));

    assert_eq!(stats.collections.len(), 2);

    // Find each collection in stats
    let products = stats.collections.iter().find(|c| c.name == "products").unwrap();
    let orders = stats.collections.iter().find(|c| c.name == "orders").unwrap();

    assert_eq!(products.num_documents, 0);
    assert_eq!(products.num_fields, 1);
    assert_eq!(orders.num_documents, 1);
    assert_eq!(orders.num_fields, 2);
}
