mod helpers;

use helpers::ingest_client::TestIngestClient;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use torque::collection::CollectionRegistry;
use torque::ingest::protocol::ServerMessage;
use torque::observability::MetricsCollector;
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

fn make_schema(name: &str, fields: Vec<FieldDef>) -> Schema {
    Schema {
        index_mode: Default::default(),
        name: name.into(),
        fields,
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

async fn start_ingest_server(registry: Arc<CollectionRegistry>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, peer) = listener.accept().await.unwrap();
            let reg = Arc::clone(&registry);
            tokio::spawn(async move {
                let mut session = torque::ingest::session::IngestSession::new(Arc::new(MetricsCollector::new()));
                let mut stream = stream;
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                loop {
                    let len = match stream.read_u32().await {
                        Ok(l) => l as usize,
                        Err(_) => break,
                    };
                    if len == 0 {
                        break;
                    }
                    let mut buf = vec![0u8; len];
                    if stream.read_exact(&mut buf).await.is_err() {
                        break;
                    }
                    let response = session.handle_message(&buf, &reg).await;
                    let resp_bytes =
                        torque::ingest::protocol::serialize_server_message(&response);
                    if stream.write_u32(resp_bytes.len() as u32).await.is_err() {
                        break;
                    }
                    if stream.write_all(&resp_bytes).await.is_err() {
                        break;
                    }
                    let _ = stream.flush().await;
                }
                tracing::debug!(peer = %peer, "Test connection closed");
            });
        }
    });

    addr
}

fn make_docs(count: u32, schema: &Schema) -> Vec<Document> {
    (0..count)
        .map(|i| Document {
            id: i,
            external_id: format!("doc-{i}"),
            language: Language::English,
            fields: schema
                .fields
                .iter()
                .map(|f| match f.field_type {
                    FieldType::String | FieldType::Text => {
                        FieldValue::Text(format!("content {i}"))
                    }
                    FieldType::Int32 => FieldValue::Int32(i as i32),
                    FieldType::Int64 => FieldValue::Int64(i as i64),
                    FieldType::Float64 => FieldValue::Float64(i as f64),
                    FieldType::Bool => FieldValue::Bool(i % 2 == 0),
                    _ => FieldValue::Null,
                })
                .collect::<Vec<_>>().into(),
            record: None,
        })
        .collect()
}

// -- Tests --

#[tokio::test(flavor = "multi_thread")]
async fn end_to_end_ingest_and_search() {
    let schema = make_schema("products", vec![make_field("title", FieldType::String)]);
    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();

    let addr = start_ingest_server(registry.clone()).await;
    let mut client = TestIngestClient::connect(&addr.to_string()).await;

    // StartIngest
    let resp = client.start_ingest("products").await;
    assert!(matches!(resp, ServerMessage::Ack { .. }));

    // Send batch of 100 docs
    let collection = registry.get("products").unwrap();
    let docs = make_docs(100, &collection.schema);
    let resp = client.send_batch(&docs, &collection.schema).await;
    match &resp {
        ServerMessage::Ack {
            batch_seq,
            docs_received,
        } => {
            assert_eq!(*batch_seq, 1);
            assert_eq!(*docs_received, 100);
        }
        other => panic!("expected Ack, got {other:?}"),
    }

    // Commit — returns Ack immediately (async rebuild)
    let resp = client.commit().await;
    match resp {
        ServerMessage::Ack {
            docs_received, ..
        } => {
            assert_eq!(docs_received, 100);
        }
        other => panic!("expected Ack, got {other:?}"),
    }

    // Wait for the background rebuild to complete
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Search via the index
    let index = collection.snapshot();
    let params = search::SearchParams {
        q: "content".into(),
        query_by: vec!["title".into()],
        filter_by: None,
        sort_by: None,
        facet_by: None,
        per_page: 10,
        page: 1,
        ..Default::default()
    };
    let result = search::execute(&index, &collection.schema, &params).unwrap();
    match result {
        search::SearchResponse::Ungrouped(r) => {
            assert_eq!(r.found, 100);
            assert_eq!(r.hits.len(), 10);
        }
        _ => panic!("expected ungrouped result"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn abort_preserves_old_index() {
    let schema = make_schema("items", vec![make_field("name", FieldType::String)]);
    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();

    let collection = registry.get("items").unwrap();

    // Pre-populate with 5 docs via direct ingest
    let initial_docs: Vec<Document> = (0..5)
        .map(|i| Document {
            id: i,
            external_id: format!("{i}"),
            language: Language::English,
            fields: vec![FieldValue::Text(format!("initial {i}"))].into(),
            record: None,
        })
        .collect();
    collection.ingest_documents(initial_docs);
    assert_eq!(collection.snapshot().total_docs(), 5);

    // TCP ingest then abort
    let addr = start_ingest_server(registry.clone()).await;
    let mut client = TestIngestClient::connect(&addr.to_string()).await;

    client.start_ingest("items").await;
    let new_docs = make_docs(50, &collection.schema);
    client.send_batch(&new_docs, &collection.schema).await;

    let resp = client.abort().await;
    assert!(matches!(resp, ServerMessage::Ack { .. }));

    // Index should still have the original 5 docs
    assert_eq!(collection.snapshot().total_docs(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_collection_error() {
    let registry = Arc::new(CollectionRegistry::new(None));
    let addr = start_ingest_server(registry).await;
    let mut client = TestIngestClient::connect(&addr.to_string()).await;

    let resp = client.start_ingest("nonexistent").await;
    match resp {
        ServerMessage::Error { code, .. } => {
            assert_eq!(code, torque::ingest::protocol::ERR_COLLECTION_NOT_FOUND);
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sequential_rebuilds() {
    let schema = make_schema("seq", vec![make_field("data", FieldType::String)]);
    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();

    let collection = registry.get("seq").unwrap();
    let addr = start_ingest_server(registry.clone()).await;

    // First ingest: 10 docs
    {
        let mut client = TestIngestClient::connect(&addr.to_string()).await;
        client.start_ingest("seq").await;
        let docs = make_docs(10, &collection.schema);
        client.send_batch(&docs, &collection.schema).await;
        client.commit().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(collection.snapshot().total_docs(), 10);

    // Second ingest: 25 docs (full rebuild replaces first)
    {
        let mut client = TestIngestClient::connect(&addr.to_string()).await;
        client.start_ingest("seq").await;
        let docs = make_docs(25, &collection.schema);
        client.send_batch(&docs, &collection.schema).await;
        client.commit().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(collection.snapshot().total_docs(), 25);
}

#[tokio::test(flavor = "multi_thread")]
async fn multiple_batches_accumulate() {
    let schema = make_schema("multi", vec![make_field("text", FieldType::String)]);
    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema.clone()).unwrap();

    let collection = registry.get("multi").unwrap();
    let addr = start_ingest_server(registry.clone()).await;
    let mut client = TestIngestClient::connect(&addr.to_string()).await;

    client.start_ingest("multi").await;

    // Send 3 separate batches
    for batch_num in 0..3 {
        let docs: Vec<Document> = (0..10)
            .map(|i| {
                let idx = batch_num * 10 + i;
                Document {
                    id: idx,
                    external_id: format!("doc-{idx}"),
                    language: Language::English,
                    fields: vec![FieldValue::Text(format!("batch {batch_num} item {i}"))].into(),
                    record: None,
                }
            })
            .collect();
        let resp = client.send_batch(&docs, &collection.schema).await;
        match resp {
            ServerMessage::Ack {
                batch_seq,
                docs_received,
            } => {
                assert_eq!(batch_seq, (batch_num + 1) as u64);
                assert_eq!(docs_received, ((batch_num + 1) * 10) as u64);
            }
            other => panic!("expected Ack, got {other:?}"),
        }
    }

    let resp = client.commit().await;
    match resp {
        ServerMessage::Ack {
            docs_received, ..
        } => {
            assert_eq!(docs_received, 30);
        }
        other => panic!("expected Ack, got {other:?}"),
    }

    // Wait for the background rebuild to complete
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(collection.snapshot().total_docs(), 30);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_ingests_different_collections() {
    let schema_a = make_schema("alpha", vec![make_field("a", FieldType::String)]);
    let schema_b = make_schema("beta", vec![make_field("b", FieldType::String)]);

    let registry = Arc::new(CollectionRegistry::new(None));
    registry.create(schema_a.clone()).unwrap();
    registry.create(schema_b.clone()).unwrap();

    let col_a = registry.get("alpha").unwrap();
    let col_b = registry.get("beta").unwrap();

    let addr = start_ingest_server(registry.clone()).await;

    let addr_str = addr.to_string();
    let schema_a_clone = col_a.schema.clone();
    let schema_b_clone = col_b.schema.clone();

    let handle_a = tokio::spawn(async move {
        let mut client = TestIngestClient::connect(&addr_str).await;
        client.start_ingest("alpha").await;
        let docs = make_docs(20, &schema_a_clone);
        client.send_batch(&docs, &schema_a_clone).await;
        client.commit().await;
    });

    let addr_str = addr.to_string();
    let handle_b = tokio::spawn(async move {
        let mut client = TestIngestClient::connect(&addr_str).await;
        client.start_ingest("beta").await;
        let docs = make_docs(30, &schema_b_clone);
        client.send_batch(&docs, &schema_b_clone).await;
        client.commit().await;
    });

    handle_a.await.unwrap();
    handle_b.await.unwrap();

    // Wait for background rebuilds
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    assert_eq!(col_a.snapshot().total_docs(), 20);
    assert_eq!(col_b.snapshot().total_docs(), 30);
}

/// bool[]/geopoint[] persistence round-trip: the R-tree for geopoint[] must
/// be rebuilt from the column on load (the dense lat/lng vecs hold only one
/// point per doc), and bool[] match-any filter bitmaps must survive.
#[test]
fn array_types_persist_round_trip() {
    let mut schema = Schema {
        name: "routes_rt".into(),
        fields: vec![
            FieldDef {
                name: "title".into(),
                field_type: FieldType::String,
                ..Default::default()
            },
            FieldDef {
                name: "flags".into(),
                field_type: FieldType::BoolArray,
                ..Default::default()
            },
            FieldDef {
                name: "stops".into(),
                field_type: FieldType::GeoPointArray,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    schema.assign_field_ids();

    let docs = vec![
        torque::ingest::parse_json_document(
            &serde_json::json!({"id":"1","title":"north","flags":[true,true],"stops":[[59.33,18.07],[59.86,17.64]]}),
            &schema,
            0,
        )
        .unwrap(),
        torque::ingest::parse_json_document(
            &serde_json::json!({"id":"2","title":"mixed","flags":[true,false],"stops":[[57.71,11.97],[55.60,13.00]]}),
            &schema,
            1,
        )
        .unwrap(),
    ];

    let index = torque::ingest::build_index(&schema, docs);
    let dir = std::env::temp_dir().join(format!("torque-array-rt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("routes_rt.tqbf");
    torque::persistence::index::write_segment(&index, None, &path).unwrap();

    let loaded = torque::persistence::index::open_segment(&path, &schema, 1).unwrap();

    // Typed values decoded from the stored record
    let flags_fid = schema.name_to_id["flags"];
    let stops_fid = schema.name_to_id["stops"];
    let doc = loaded.get_doc(1, &schema).expect("stored document");
    match doc.fields.get(flags_fid as usize) {
        Some(FieldValue::BoolArray(arr)) => assert_eq!(arr, &vec![Some(true), Some(false)]),
        other => panic!("expected BoolArray, got {other:?}"),
    }
    match doc.fields.get(stops_fid as usize) {
        Some(FieldValue::GeoPointArray(arr)) => {
            assert_eq!(arr, &vec![(57.71, 11.97), (55.60, 13.00)])
        }
        other => panic!("expected GeoPointArray, got {other:?}"),
    }

    // bool[] match-any filter bitmaps
    let bf = loaded.bools.get(&flags_fid).expect("bool index");
    assert!(bf.true_docs.contains(0) && bf.true_docs.contains(1));
    assert!(!bf.false_docs.contains(0) && bf.false_docs.contains(1));

    // Multi-point radius: doc 1's SECOND stop (Malmö) must still match
    let geo = loaded.geo_indexes.get(&stops_fid).expect("geo index");
    let hits = geo.radius(&torque::index::geo::Cap::new(55.60, 13.00, 50_000.0));
    assert!(hits.contains(1), "second point must survive persist+restore");
    assert!(!hits.contains(0));

    std::fs::remove_dir_all(&dir).ok();
}
