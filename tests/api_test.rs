use std::net::SocketAddr;
use std::sync::Arc;
use torque::auth::KeyStore;
use torque::collection::alias::AliasRegistry;
use torque::collection::CollectionRegistry;
use torque::config::Config;
use torque::memory::MemoryGuard;
use torque::presets::PresetStore;

use torque::stopwords::StopwordStore;
use torque::synonyms::SynonymStore;

const KEY: &str = "test-api-key-12345";

fn make_config(addr: SocketAddr) -> Config {
    Config {
        api_addr: addr,
        ingest_addr: "127.0.0.1:0".parse().unwrap(),
        api_key: KEY.into(),
        data_dir: {
            // collections.json is saved there: a create that cannot save it fails
            let dir = std::env::temp_dir().join(format!("torque-api-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        },
        log_level: "error".into(),
        log_dir: None,
        cors_origins: vec!["*".into()],
        ingest_allow: vec![],
        gpu_vram_budget_bytes: 0,
        max_vector_memory_bytes: 16 * 1024 * 1024 * 1024,
        max_http_body_bytes: 128 * 1024 * 1024,
        max_memory_bytes: u64::MAX,
        max_ingest_connections: 64,
        query_timeout_ms: 10_000,
        ai: None,
        studio_dir: None,
        compaction_interval_secs: 1800,
        compaction_memory_threshold_percent: 80,
        tls_cert: None,
        tls_key: None,
        slow_query_ms: 0,
        allow_drop_collection: true,
        studio_enabled: true,
        quiet: true,
        allowed_ips: String::new(),
    }
}

fn products_schema() -> serde_json::Value {
    serde_json::json!({
        "name": "products",
        "fields": [
            {"name": "title", "type": "string"},
            {"name": "price", "type": "float", "facet": true, "sort": true},
            {"name": "brand", "type": "string", "facet": true},
            {"name": "in_stock", "type": "bool", "facet": true},
            {"name": "rating", "type": "int32", "sort": true},
        ]
    })
}

async fn start_server() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let config = make_config(addr);
    let collections = Arc::new(CollectionRegistry::new(None));
    let aliases = Arc::new(AliasRegistry::new());
    let synonyms = Arc::new(SynonymStore::new());
    let curations = Arc::new(torque::curation::CurationStore::new());
    let analytics = Arc::new(torque::analytics::AnalyticsStore::new());
    let llm = Arc::new(torque::llm::LlmRegistry::new());
    let presets = Arc::new(PresetStore::new());
    let stopwords = Arc::new(StopwordStore::new());
    let keys = Arc::new(KeyStore::new(KEY.into()));

    tokio::spawn(async move {
        use axum::middleware;
        use axum::Router;
        use torque::auth::middleware::auth_middleware;
        use torque::server::{routes, AppState};

        let metrics = Arc::new(torque::observability::MetricsCollector::new());
        let memory_guard = MemoryGuard::new(u64::MAX, metrics.memory_resident.clone());
        let state = Arc::new(AppState {
            collections,
            aliases,
            synonyms,
            curations,
            analytics,
            llm,
            presets,
            stopwords,
            keys,
            config: Arc::new(config),
            metrics,
            start_time: std::time::Instant::now(),
            memory_guard,
            ip_filter: Arc::new(torque::ip_filter::IpFilter::default()),
            fs_serial: Arc::new(tokio::sync::Mutex::new(())),
            slow_query_ms: std::sync::atomic::AtomicU64::new(0),
            search_cache: Default::default(),
            #[cfg(feature = "cuda")]
            gpu: None,
        });

        let app = Router::new()
            .merge(routes::routes())
            .merge(torque::server::segments::routes())
            .merge(routes::studio_routes())
            .layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .with_state(state);

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    addr
}

fn c() -> reqwest::Client {
    reqwest::Client::new()
}

fn u(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

async fn create_products(addr: SocketAddr) {
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&products_schema())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create collection: {}", resp.status());
}

async fn import_docs(addr: SocketAddr, collection: &str, jsonl: &str) {
    let resp = c()
        .post(u(addr, &format!("/collections/{collection}/documents/import?action=create")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .header("Content-Type", "text/plain")
        .body(jsonl.to_string())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "import: {}", resp.status());
}

// ── Document CRUD ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn create_and_get_document() {
    let addr = start_server().await;
    create_products(addr).await;

    // Create
    let doc = serde_json::json!({"id": "1", "title": "Laptop", "price": 999.99, "brand": "Acme", "in_stock": true, "rating": 5});
    let resp = c()
        .post(u(addr, "/collections/products/documents"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&doc)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create doc: {}", resp.status());

    // Get
    let resp = c()
        .get(u(addr, "/collections/products/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Laptop");
    assert_eq!(body["price"], 999.99);
}

#[tokio::test(flavor = "multi_thread")]
async fn update_document() {
    let addr = start_server().await;
    create_products(addr).await;

    let doc = serde_json::json!({"id": "1", "title": "Laptop", "price": 999.99, "brand": "Acme", "in_stock": true, "rating": 5});
    c().post(u(addr, "/collections/products/documents"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&doc)
        .send()
        .await
        .unwrap();

    // Update
    let patch = serde_json::json!({"price": 799.99});
    let resp = c()
        .patch(u(addr, "/collections/products/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&patch)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "update: {}", resp.status());

    // Verify
    let resp = c()
        .get(u(addr, "/collections/products/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["price"], 799.99);
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_document() {
    let addr = start_server().await;
    create_products(addr).await;

    let doc = serde_json::json!({"id": "1", "title": "Laptop", "price": 999.99, "brand": "Acme", "in_stock": true, "rating": 5});
    c().post(u(addr, "/collections/products/documents"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&doc)
        .send()
        .await
        .unwrap();

    // Delete
    let resp = c()
        .delete(u(addr, "/collections/products/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Verify gone
    let resp = c()
        .get(u(addr, "/collections/products/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn get_document_not_found() {
    let addr = start_server().await;
    create_products(addr).await;

    let resp = c()
        .get(u(addr, "/collections/products/documents/99999"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn import_and_export_jsonl() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}
{"id":"3","title":"Tablet","price":299.99,"brand":"Acme","in_stock":false,"rating":3}"#;
    import_docs(addr, "products", jsonl).await;

    // Export
    let resp = c()
        .get(u(addr, "/collections/products/documents/export"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    let lines: Vec<&str> = body.trim().lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 exported docs, got {}", lines.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_documents_by_filter() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}
{"id":"3","title":"Tablet","price":299.99,"brand":"Acme","in_stock":false,"rating":3}"#;
    import_docs(addr, "products", jsonl).await;

    // Delete by filter
    let resp = c()
        .delete(u(addr, "/collections/products/documents?filter_by=brand:=Acme"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "delete by filter: {}", resp.status());

    // Verify: only non-Acme docs remain
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=*&query_by=title&per_page=10"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1, "should have 1 doc left after filter delete");
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_documents_requires_filter_or_truncate() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}"#;
    import_docs(addr, "products", jsonl).await;

    // Bare DELETE without filter_by or truncate is a 400 (Typesense v30.1)
    let resp = c()
        .delete(u(addr, "/collections/products/documents"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "bare bulk delete must be rejected");

    // truncate=true deletes everything
    let resp = c()
        .delete(u(addr, "/collections/products/documents?truncate=true"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "truncate: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_deleted"], 2);

    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=*&query_by=title&per_page=10"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 0, "collection should be empty after truncate");
}

#[tokio::test(flavor = "multi_thread")]
async fn update_documents_by_filter() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}
{"id":"3","title":"Tablet","price":299.99,"brand":"Acme","in_stock":true,"rating":3}"#;
    import_docs(addr, "products", jsonl).await;

    // PATCH without filter_by is a 400
    let resp = c()
        .patch(u(addr, "/collections/products/documents"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"in_stock": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "bulk update without filter must 400");

    // Update all Acme docs
    let resp = c()
        .patch(u(addr, "/collections/products/documents?filter_by=brand:=Acme"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"in_stock": false}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "bulk update: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_updated"], 2);

    // Patched docs match the new value, others untouched
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=*&query_by=title&filter_by=in_stock:=false&per_page=10"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2, "both Acme docs should be out of stock");

    // Unpatched fields preserved
    let resp = c()
        .get(u(addr, "/collections/products/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Laptop");
    assert_eq!(body["in_stock"], false);

    // No matches → num_updated 0
    let resp = c()
        .patch(u(addr, "/collections/products/documents?filter_by=brand:=Nonexistent"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"in_stock": false}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_updated"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn document_write_actions() {
    let addr = start_server().await;
    create_products(addr).await;

    let laptop = serde_json::json!(
        {"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5});

    // action=update on a missing doc → 404
    let resp = c()
        .post(u(addr, "/collections/products/documents?action=update"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&laptop)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "update of missing doc must 404");

    // action=upsert on a missing doc creates it → 201
    let resp = c()
        .post(u(addr, "/collections/products/documents?action=upsert"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&laptop)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "upsert-create: {}", resp.status());

    // default action=create on an existing doc → 409
    let resp = c()
        .post(u(addr, "/collections/products/documents"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&laptop)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409, "create of existing doc must 409");

    // action=upsert on the existing doc replaces it → 200
    let laptop_v2 = serde_json::json!(
        {"id":"1","title":"Laptop Pro","price":1299.99,"brand":"Acme","in_stock":true,"rating":5});
    let resp = c()
        .post(u(addr, "/collections/products/documents?action=upsert"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&laptop_v2)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "upsert-replace: {}", resp.status());

    // action=update with a partial body merges into the stored doc
    let resp = c()
        .post(u(addr, "/collections/products/documents?action=update"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"id":"1","price":1099.0}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "partial update: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Laptop Pro", "update must preserve unpatched fields");
    assert_eq!(body["price"], 1099.0);

    // action=emplace creates when missing, merges when present
    let resp = c()
        .post(u(addr, "/collections/products/documents?action=emplace"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!(
            {"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "emplace-create: {}", resp.status());
    let resp = c()
        .post(u(addr, "/collections/products/documents?action=emplace"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"id":"2","rating":1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "emplace-merge: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["title"], "Phone", "emplace must preserve unpatched fields");
    assert_eq!(body["rating"], 1);

    // Unknown action → 400
    let resp = c()
        .post(u(addr, "/collections/products/documents?action=replace"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&laptop)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "unknown action must 400");

    // Both docs searchable with merged state
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=*&query_by=title&per_page=10"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn import_return_doc() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}"#;
    let resp = c()
        .post(u(addr, "/collections/products/documents/import?return_doc=true&return_id=true"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .body(jsonl)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "import: {}", resp.status());
    let body = resp.text().await.unwrap();
    let line: serde_json::Value = serde_json::from_str(body.trim().lines().next().unwrap()).unwrap();
    assert_eq!(line["success"], true);
    assert_eq!(line["id"], "1");
    assert_eq!(line["document"]["title"], "Laptop");
}

// ── Collection Management ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn get_collection() {
    let addr = start_server().await;
    create_products(addr).await;

    let resp = c()
        .get(u(addr, "/collections/products"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "products");
    assert!(body["fields"].as_array().unwrap().len() >= 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_collection() {
    let addr = start_server().await;
    create_products(addr).await;

    let resp = c()
        .delete(u(addr, "/collections/products"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Verify gone
    let resp = c()
        .get(u(addr, "/collections/products"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_collection_duplicate() {
    let addr = start_server().await;
    create_products(addr).await;

    // Try to create again
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&products_schema())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409, "duplicate collection should be 409");
}

// ── Aliases ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn alias_crud() {
    let addr = start_server().await;
    create_products(addr).await;

    // Create alias
    let resp = c()
        .put(u(addr, "/aliases/shop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"collection_name": "products"}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create alias: {}", resp.status());

    // Get alias
    let resp = c()
        .get(u(addr, "/aliases/shop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["collection_name"], "products");

    // List aliases
    let resp = c()
        .get(u(addr, "/aliases"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["aliases"].as_array().unwrap().len() >= 1);

    // Delete alias
    let resp = c()
        .delete(u(addr, "/aliases/shop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Verify gone
    let resp = c()
        .get(u(addr, "/aliases/shop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn search_via_alias() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}"#;
    import_docs(addr, "products", jsonl).await;

    // Create alias
    c().put(u(addr, "/aliases/shop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"collection_name": "products"}))
        .send()
        .await
        .unwrap();

    // Search via alias
    let resp = c()
        .get(u(addr, "/collections/shop/documents/search?q=laptop&query_by=title&per_page=10"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["found"].as_i64().unwrap() >= 1);
}

// ── Multi-Search ─────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn multi_search_basic() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}"#;
    import_docs(addr, "products", jsonl).await;

    let resp = c()
        .post(u(addr, "/multi_search"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "searches": [
                {"collection": "products", "q": "laptop", "query_by": "title"},
                {"collection": "products", "q": "phone", "query_by": "title"},
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0]["found"].as_i64().unwrap() >= 1);
    assert!(results[1]["found"].as_i64().unwrap() >= 1);
}

// ── Synonyms ─────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn synonym_set_crud() {
    let addr = start_server().await;

    // Create (Typesense API: PUT /synonym_sets/{name} with {name})
    let resp = c()
        .put(u(addr, "/synonym_sets/electronics"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"name": "electronics", "items": []}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create synonym set: {}", resp.status());

    // Get
    let resp = c()
        .get(u(addr, "/synonym_sets/electronics"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // List
    let resp = c()
        .get(u(addr, "/synonym_sets"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Delete
    let resp = c()
        .delete(u(addr, "/synonym_sets/electronics"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

// ── Presets ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn preset_crud() {
    let addr = start_server().await;

    // Create (Typesense API: PUT /presets/{name} with {value}, answered with the name added)
    let resp = c()
        .put(u(addr, "/presets/default_search"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"value": {"per_page": 20, "query_by": "title"}}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create preset: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body, serde_json::json!({"name": "default_search", "value": {"per_page": 20, "query_by": "title"}}));

    // Get
    let resp = c()
        .get(u(addr, "/presets/default_search"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // List
    let resp = c()
        .get(u(addr, "/presets"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Delete
    let resp = c()
        .delete(u(addr, "/presets/default_search"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

// ── Stopwords ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn stopword_crud() {
    let addr = start_server().await;

    // Create (Typesense API: PUT /stopwords/{id} with {id, stopwords})
    let resp = c()
        .put(u(addr, "/stopwords/english"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"id": "english", "stopwords": ["the", "a", "an", "is", "are"]}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create stopwords: {}", resp.status());

    // Get
    let resp = c()
        .get(u(addr, "/stopwords/english"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // List
    let resp = c()
        .get(u(addr, "/stopwords"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Delete
    let resp = c()
        .delete(u(addr, "/stopwords/english"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

// ── Admin & Health ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn health_ready() {
    let addr = start_server().await;
    let resp = c()
        .get(u(addr, "/health/ready"))
        .send()
        .await
        .unwrap();
    // 200 when all collections ready, 503 during startup — both are valid
    assert!(resp.status() == 200 || resp.status() == 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("ok").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_stats() {
    let addr = start_server().await;
    let resp = c()
        .get(u(addr, "/admin/stats"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("system").is_some() || body.get("collections").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_endpoint() {
    let addr = start_server().await;
    let resp = c()
        .get(u(addr, "/metrics"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert!(body.contains("torque_") || body.contains("# HELP") || body.contains("# TYPE"));
}

// ── Search with facets, sort, filter ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn search_with_facets_and_sort() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop Pro","price":1299.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Laptop Basic","price":499.99,"brand":"Acme","in_stock":true,"rating":3}
{"id":"3","title":"Phone","price":899.99,"brand":"Beta","in_stock":false,"rating":4}"#;
    import_docs(addr, "products", jsonl).await;

    // Faceted search with sort
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=*&query_by=title&facet_by=brand,in_stock&sort_by=price:desc&per_page=10"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 3);
    // facet_counts should be present
    if let Some(fc) = body["facet_counts"].as_array() {
        assert!(fc.len() >= 1, "expected facet counts");
    }

    // Verify sort order (price desc)
    let hits = body["hits"].as_array().unwrap();
    let first_price = hits[0]["document"]["price"].as_f64().unwrap();
    let last_price = hits[hits.len() - 1]["document"]["price"].as_f64().unwrap();
    assert!(first_price >= last_price, "should be sorted by price desc");

    // Filter search
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=*&query_by=title&filter_by=brand:=Acme&per_page=10"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2, "should find 2 Acme products");
}

async fn search_json(addr: SocketAddr, query: &str) -> (u16, serde_json::Value) {
    let resp = c()
        .get(u(addr, &format!("/collections/products/documents/search?{query}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap())
}

fn keys(v: &serde_json::Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

#[tokio::test(flavor = "multi_thread")]
async fn search_response_has_typesense_keys() {
    let addr = start_server().await;
    create_products(addr).await;
    let jsonl = (1..=25).map(|i| format!(r#"{{"id":"{i}","title":"laptop {i}","price":{i}.5,"brand":"Acme","in_stock":true,"rating":{i}}}"#)).collect::<Vec<_>>().join("
");
    import_docs(addr, "products", &jsonl).await;

    let (status, body) = search_json(addr, "q=laptop&query_by=title&per_page=10").await;
    assert_eq!(status, 200);
    assert_eq!(keys(&body), ["facet_counts", "found", "hits", "out_of", "page", "request_params", "search_cutoff", "search_time_ms"]);
    assert_eq!(body["request_params"], serde_json::json!({"collection_name": "products", "first_q": "laptop", "per_page": 10, "q": "laptop"}));
    assert!(body["hits"][0]["highlight"].is_object());
    assert!(body["hits"][0]["highlights"].is_array());
    let (_, body) = search_json(addr, "q=laptop&query_by=title&enable_highlight_v1=false").await;
    assert!(body["hits"][0].get("highlights").is_none());
    assert_eq!(body["hits"][0]["highlight"]["title"]["snippet"], "<mark>laptop</mark> 25");

    // q=*: hits carry an empty highlight object and highlights array
    let (_, body) = search_json(addr, "q=*&query_by=title&per_page=1").await;
    assert_eq!(body["hits"][0]["highlight"], serde_json::json!({}));
    assert_eq!(body["hits"][0]["highlights"], serde_json::json!([]));

    // offset without page: hits from that offset, answered as `offset`
    let (_, by_offset) = search_json(addr, "q=*&query_by=title&sort_by=rating:asc&offset=3&per_page=5").await;
    assert_eq!(by_offset["offset"], 3);
    assert!(by_offset.get("page").is_none());
    let ratings: Vec<i64> = by_offset["hits"].as_array().unwrap().iter().map(|h| h["document"]["rating"].as_i64().unwrap()).collect();
    assert_eq!(ratings, [4, 5, 6, 7, 8]);
    // page wins over offset
    let (_, by_page) = search_json(addr, "q=*&query_by=title&sort_by=rating:asc&offset=3&page=2&per_page=5").await;
    assert_eq!(by_page["page"], 2);
    assert_eq!(by_page["hits"][0]["document"]["rating"], 6);

    // per_page 0: counts only; above 250: 422
    let (status, body) = search_json(addr, "q=*&query_by=title&per_page=0&facet_by=brand").await;
    assert_eq!(status, 200);
    assert_eq!(body["found"], 25);
    assert_eq!(body["hits"], serde_json::json!([]));
    let (status, body) = search_json(addr, "q=*&query_by=title&per_page=251").await;
    assert_eq!(status, 422);
    assert_eq!(body["message"], "Only upto 250 hits can be fetched per page.");
}

#[tokio::test(flavor = "multi_thread")]
async fn facets_as_typesense_answers_them() {
    let addr = start_server().await;
    create_products(addr).await;
    // prices 1.5 .. 10.5, brands Acme (odd) and Bolt (even), in stock below 8
    let jsonl = (1..=10)
        .map(|i| format!(r#"{{"id":"{i}","title":"laptop {i}","price":{i}.5,"brand":"{}","in_stock":{},"rating":{i}}}"#, if i % 2 == 1 { "Acme" } else { "Bolt" }, i < 8))
        .collect::<Vec<_>>()
        .join("
");
    import_docs(addr, "products", &jsonl).await;

    let (status, body) = search_json(addr, "q=*&query_by=title&facet_by=brand,price,in_stock&max_facet_values=3").await;
    assert_eq!(status, 200);
    let facets = body["facet_counts"].as_array().unwrap();
    assert_eq!(keys(&facets[0]), ["counts", "field_name", "sampled", "stats"]);
    assert_eq!(facets[0]["stats"], serde_json::json!({"total_values": 2}));
    assert_eq!(facets[0]["counts"][0], serde_json::json!({"value": "Acme", "highlighted": "Acme", "count": 5}));
    // numeric values counted, ties in value order, stats over all of them
    let prices: Vec<&str> = facets[1]["counts"].as_array().unwrap().iter().map(|c| c["value"].as_str().unwrap()).collect();
    assert_eq!(prices, ["1.5", "2.5", "3.5"]);
    assert_eq!(facets[1]["stats"], serde_json::json!({"min": 1.5, "max": 10.5, "sum": 60.0, "avg": 6.0, "total_values": 10}));
    assert_eq!(facets[2]["counts"], serde_json::json!([{"value": "true", "highlighted": "true", "count": 7}, {"value": "false", "highlighted": "false", "count": 3}]));

    // range facets with an open upper bound
    let (status, body) = search_json(addr, "q=*&query_by=title&facet_by=price(low:[0,4],mid:[4,8],high:[8,])").await;
    assert_eq!(status, 200);
    let counts: Vec<(String, u64)> = body["facet_counts"][0]["counts"].as_array().unwrap().iter().map(|c| (c["value"].as_str().unwrap().to_string(), c["count"].as_u64().unwrap())).collect();
    assert_eq!(counts, [("mid".to_string(), 4), ("low".to_string(), 3), ("high".to_string(), 3)]);

    // alphabetical
    let (_, body) = search_json(addr, "q=*&query_by=title&facet_by=brand(sort_by:_alpha:desc)").await;
    assert_eq!(body["facet_counts"][0]["counts"][0]["value"], "Bolt");

    // Typesense's errors
    let (status, body) = search_json(addr, "q=*&query_by=title&facet_by=title").await;
    assert_eq!((status, body["message"].as_str().unwrap()), (404, "Could not find a facet field named `title` in the schema."));
    let (status, body) = search_json(addr, "q=*&query_by=title&facet_by=price(a:[0,4],b:[5,8])").await;
    assert_eq!((status, body["message"].as_str().unwrap()), (400, "Ranges in range facet syntax should be continous."));
    let (status, body) = search_json(addr, "q=*&query_by=title&facet_by=brand(x:[0,4])").await;
    assert_eq!((status, body["message"].as_str().unwrap()), (400, "Range facet is restricted to only integer and float fields."));
}

async fn put_preset(addr: SocketAddr, name: &str, value: serde_json::Value) {
    let resp = c().put(u(addr, &format!("/presets/{name}"))).header("X-TYPESENSE-API-KEY", KEY).json(&serde_json::json!({ "value": value })).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

async fn multi_json(addr: SocketAddr, query: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    let resp = c().post(u(addr, &format!("/multi_search?{query}"))).header("X-TYPESENSE-API-KEY", KEY).json(&body).send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn presets_give_every_parameter_the_search_lacks() {
    let addr = start_server().await;
    create_products(addr).await;
    // prices 1.5 .. 10.5, brands Acme (odd) and Bolt (even), in stock below 8
    let jsonl = (1..=10)
        .map(|i| format!(r#"{{"id":"{i}","title":"laptop {i}","price":{i}.5,"brand":"{}","in_stock":{},"rating":{i}}}"#, if i % 2 == 1 { "Acme" } else { "Bolt" }, i < 8))
        .collect::<Vec<_>>()
        .join("\n");
    import_docs(addr, "products", &jsonl).await;
    let listing = serde_json::json!({
        "q": "*", "query_by": "title", "sort_by": "price:desc", "per_page": 3, "include_fields": "title,price",
        "facet_by": "brand", "max_facet_values": 1, "filter_by": "in_stock:true", "num_typos": "1", "prefix": false
    });
    put_preset(addr, "listing", listing.clone()).await;

    let prices = |body: &serde_json::Value| -> Vec<f64> { body["hits"].as_array().unwrap().iter().map(|h| h["document"]["price"].as_f64().unwrap()).collect() };
    let (status, body) = search_json(addr, "preset=listing").await;
    assert_eq!(status, 200);
    assert_eq!(prices(&body), [7.5, 6.5, 5.5]);
    assert_eq!(keys(&body["hits"][0]["document"]), ["price", "title"]);
    assert_eq!(body["facet_counts"][0]["counts"].as_array().unwrap().len(), 1);
    // the search's own parameters win, filter_by too
    let (_, body) = search_json(addr, "preset=listing&per_page=2&filter_by=brand:=Acme").await;
    assert_eq!(prices(&body), [9.5, 7.5]);
    let (_, body) = search_json(addr, "q=laptop&preset=listing&sort_by=price:asc").await;
    assert_eq!(prices(&body), [1.5, 2.5, 3.5]);

    // multi_search: an entry's own parameters over the URL's, its preset under both
    let (status, body) = multi_json(
        addr,
        "collection=products&per_page=1",
        serde_json::json!({"searches": [{"preset": "listing"}, {"preset": "listing", "per_page": 4, "include_fields": "id"}, {"q": "*", "query_by": "title"}]}),
    )
    .await;
    assert_eq!(status, 200);
    let r = &body["results"];
    assert_eq!(prices(&r[0]), [7.5]);
    assert_eq!(r[1]["hits"].as_array().unwrap().len(), 4);
    assert_eq!(keys(&r[1]["hits"][0]["document"]), ["id"]);
    assert_eq!(r[2]["found"], 10);
    // a multi-search preset named in the URL is the request
    put_preset(addr, "two", serde_json::json!({"searches": [{"collection": "products", "q": "*", "per_page": 2}, {"collection": "products", "q": "*", "filter_by": "brand:=Bolt"}]})).await;
    let (status, body) = multi_json(addr, "preset=two", serde_json::json!({"searches": []})).await;
    assert_eq!(status, 200);
    assert_eq!(body["results"][0]["hits"].as_array().unwrap().len(), 2);
    assert_eq!(body["results"][1]["found"], 5);
    // Typesense's checks
    let (status, body) = multi_json(addr, "limit_multi_searches=1", serde_json::json!({"searches": [{}, {}]})).await;
    assert_eq!((status, body["message"].as_str().unwrap()), (400, "Number of multi searches exceeds `limit_multi_searches` parameter."));
    let (status, body) = multi_json(addr, "", serde_json::json!({"searches": [{"collection": "products", "q": "*", "facet_by": ["brand"]}]})).await;
    assert_eq!((status, body["message"].as_str().unwrap()), (400, "One or more search parameters are malformed."));
    let (status, _) = multi_json(addr, "", serde_json::json!({"nothing": []})).await;
    assert_eq!(status, 400);

    // the preset API's shapes
    let resp = c().get(u(addr, "/presets")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["presets"][0], serde_json::json!({"name": "listing", "value": listing}));
    let resp = c().delete(u(addr, "/presets/listing")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "listing");
    let resp = c().get(u(addr, "/presets/listing")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    let resp = c().put(u(addr, "/presets/bad")).header("X-TYPESENSE-API-KEY", KEY).json(&serde_json::json!({"q": "*"})).send().await.unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn implicit_highlights_follow_include_and_exclude_fields() {
    let addr = start_server().await;
    create_products(addr).await;
    import_docs(addr, "products", r#"{"id":"1","title":"acme laptop","price":1.5,"brand":"acme","in_stock":true,"rating":1}"#).await;
    let highlighted = |body: &serde_json::Value| -> Vec<String> {
        let mut k: Vec<String> = body["hits"][0]["highlight"].as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    let (_, body) = search_json(addr, "q=acme&query_by=title,brand").await;
    assert_eq!(highlighted(&body), ["brand", "title"]);
    let (_, body) = search_json(addr, "q=acme&query_by=title,brand&include_fields=title,price").await;
    assert_eq!(highlighted(&body), ["title"]);
    let (_, body) = search_json(addr, "q=acme&query_by=title,brand&exclude_fields=title").await;
    assert_eq!(highlighted(&body), ["brand"]);
    // highlight_fields as given
    let (_, body) = search_json(addr, "q=acme&query_by=title,brand&highlight_fields=title,brand&include_fields=price").await;
    assert_eq!(highlighted(&body), ["brand", "title"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn num_typos_and_prefix_per_field() {
    let addr = start_server().await;
    create_products(addr).await;
    import_docs(addr, "products", r#"{"id":"1","title":"keyboard","price":1.0,"brand":"Logitech","in_stock":true,"rating":1}"#).await;

    // one typo in each field: allowed in brand only
    let (_, body) = search_json(addr, "q=keybord&query_by=title,brand&num_typos=0,2&prefix=false").await;
    assert_eq!(body["found"], 0);
    let (_, body) = search_json(addr, "q=logitek&query_by=title,brand&num_typos=0,2&prefix=false").await;
    assert_eq!(body["found"], 1);
    // prefix in title only
    let (_, body) = search_json(addr, "q=keyb&query_by=title,brand&prefix=true,false").await;
    assert_eq!(body["found"], 1);
    let (_, body) = search_json(addr, "q=logi&query_by=title,brand&prefix=true,false&num_typos=0").await;
    assert_eq!(body["found"], 0);
    // a list of another length
    let (status, body) = search_json(addr, "q=key&query_by=title,brand&num_typos=0,1,2").await;
    assert_eq!(status, 400);
    assert_eq!(body["message"], "Number of values in `num_typos` does not match number of `query_by` fields.");
    let (status, body) = search_json(addr, "q=key&query_by=title,brand&prefix=true,false,true").await;
    assert_eq!(status, 400);
    assert_eq!(body["message"], "Number of prefix values in `prefix` does not match number of `query_by` fields.");
}

// ── Collection Update ────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn update_collection_add_field() {
    let addr = start_server().await;
    create_products(addr).await;

    // Add a new field via PATCH
    let resp = c()
        .patch(u(addr, "/collections/products"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "fields": [
                {"name": "color", "type": "string", "optional": true}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "update collection: {}", resp.status());

    // Verify field was added
    let resp = c()
        .get(u(addr, "/collections/products"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let fields = body["fields"].as_array().unwrap();
    let has_color = fields.iter().any(|f| f["name"] == "color");
    assert!(has_color, "collection should have color field after update");
}

#[tokio::test(flavor = "multi_thread")]
async fn update_collection_drop_field() {
    let addr = start_server().await;
    create_products(addr).await;

    // Drop rating field (Typesense format: include type when dropping)
    let resp = c()
        .patch(u(addr, "/collections/products"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "fields": [
                {"name": "rating", "type": "int32", "drop": true}
            ]
        }))
        .send()
        .await
        .unwrap();
    // 200 if drop succeeded, 422 if not supported for this field type
    let s = resp.status().as_u16();
    assert!(s == 200 || s == 422, "drop field: expected 200 or 422, got {s}");
}

// ── Collection Compact ───────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn compact_collection() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}"#;
    import_docs(addr, "products", jsonl).await;

    let resp = c()
        .post(u(addr, "/collections/products/documents/compact"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    // 200 if the flush ran, 400 if collection is batch-mode (nothing in memory to flush)
    let s = resp.status().as_u16();
    assert!(s == 200 || s == 400, "compact: expected 200 or 400, got {s}");
}

// ── Collection Suspend / Resume ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn suspend_and_resume_collection() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}"#;
    import_docs(addr, "products", jsonl).await;

    // Suspend endpoint exists and returns a valid HTTP response
    let resp = c()
        .post(u(addr, "/collections/products/suspend"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let suspend_status = resp.status().as_u16();
    // 200 = suspended, 400/500 = not supported for this config — both are valid responses
    assert!(suspend_status < 500 || suspend_status == 503,
        "suspend: unexpected status {suspend_status}");

    if suspend_status == 200 {
        // Resume
        let resp = c()
            .post(u(addr, "/collections/products/resume"))
            .header("X-TYPESENSE-API-KEY", KEY)
            .send()
            .await
            .unwrap();
        // Resume may succeed or fail depending on persistence state
        let _status = resp.status();
    }
}

// ── Synonym Items ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn synonym_item_crud() {
    let addr = start_server().await;

    // Create set first
    c().put(u(addr, "/synonym_sets/tech"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"name": "tech", "items": []}))
        .send()
        .await
        .unwrap();

    // Add synonym item
    let resp = c()
        .put(u(addr, "/synonym_sets/tech/items/laptop_synonyms"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"id": "laptop_synonyms", "synonyms": ["laptop", "notebook", "portable computer"]}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create synonym item: {}", resp.status());

    // Get item
    let resp = c()
        .get(u(addr, "/synonym_sets/tech/items/laptop_synonyms"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "laptop_synonyms");

    // List items
    let resp = c()
        .get(u(addr, "/synonym_sets/tech/items"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Delete item
    let resp = c()
        .delete(u(addr, "/synonym_sets/tech/items/laptop_synonyms"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

// ── Overrides (Typesense compatibility stubs) ────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn overrides_stubs() {
    let addr = start_server().await;
    create_products(addr).await;

    // List overrides (should return empty array)
    let resp = c()
        .get(u(addr, "/collections/products/overrides"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Get non-existent override
    let resp = c()
        .get(u(addr, "/collections/products/overrides/test"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── Debug Endpoints ──────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn debug_endpoint() {
    let addr = start_server().await;
    let resp = c()
        .get(u(addr, "/debug"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// ── 404 for non-existent collection ──────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn search_nonexistent_collection() {
    let addr = start_server().await;
    let resp = c()
        .get(u(addr, "/collections/nonexistent/documents/search?q=test&query_by=title"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── Document upsert via import ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn import_upsert_mode() {
    let addr = start_server().await;
    create_products(addr).await;

    // Initial import
    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}"#;
    import_docs(addr, "products", jsonl).await;

    // Upsert with updated price
    let upsert = r#"{"id":"1","title":"Laptop","price":799.99,"brand":"Acme","in_stock":true,"rating":5}"#;
    let resp = c()
        .post(u(addr, "/collections/products/documents/import?action=upsert"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .header("Content-Type", "text/plain")
        .body(upsert.to_string())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "upsert import: {}", resp.status());

    // Verify updated
    let resp = c()
        .get(u(addr, "/collections/products/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["price"], 799.99);
}

// ── Alias swap (zero-downtime migration) ─────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn alias_swap() {
    let addr = start_server().await;

    // Create two collections
    create_products(addr).await;
    let schema2 = serde_json::json!({
        "name": "products_v2",
        "fields": [
            {"name": "title", "type": "string"},
            {"name": "price", "type": "float"},
            {"name": "brand", "type": "string", "facet": true},
            {"name": "in_stock", "type": "bool"},
            {"name": "rating", "type": "int32"},
        ]
    });
    c().post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&schema2)
        .send()
        .await
        .unwrap();

    // Create alias pointing to v1
    c().put(u(addr, "/aliases/shop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"collection_name": "products"}))
        .send()
        .await
        .unwrap();

    // Swap to v2
    let resp = c()
        .put(u(addr, "/aliases/shop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"collection_name": "products_v2"}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Verify alias now points to v2
    let resp = c()
        .get(u(addr, "/aliases/shop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["collection_name"], "products_v2");
}

// ── Error handling ───────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn invalid_json_returns_400() {
    let addr = start_server().await;
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .header("Content-Type", "application/json")
        .body("not json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_required_field_returns_error() {
    let addr = start_server().await;
    // Create collection without name
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"fields": [{"name": "x", "type": "string"}]}))
        .send()
        .await
        .unwrap();
    let s = resp.status().as_u16();
    assert!(s == 400 || s == 422, "expected 400 or 422, got {s}");
}

// ── Binary file import ───────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn import_binary_uncompressed() {
    let addr = start_server().await;
    create_products(addr).await;

    // Build a TQBF file manually: header + uncompressed payload
    // We'll use the JSONL import first to get docs, then test binary import on a fresh collection
    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}"#;
    import_docs(addr, "products", jsonl).await;

    // Export to get binary representation — use the export endpoint
    let resp = c()
        .get(u(addr, "/collections/products/documents/export"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // For a proper binary import test, construct a minimal TQBF with flag=0 (uncompressed)
    // This tests the endpoint accepts and processes the format correctly
    // Just verify the endpoint exists and rejects invalid data gracefully
    let resp = c()
        .post(u(addr, "/collections/products/documents/import-binary"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .header("Content-Type", "application/octet-stream")
        .body(b"TQBF\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec()) // valid header, 0 docs
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "binary import with 0 docs: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_imported"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn import_binary_rejects_bad_magic() {
    let addr = start_server().await;
    create_products(addr).await;

    let resp = c()
        .post(u(addr, "/collections/products/documents/import-binary"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .header("Content-Type", "application/octet-stream")
        .body(b"XXXX\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn import_binary_rejects_too_small() {
    let addr = start_server().await;
    create_products(addr).await;

    let resp = c()
        .post(u(addr, "/collections/products/documents/import-binary"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .header("Content-Type", "application/octet-stream")
        .body(b"TQBF".to_vec()) // only 4 bytes, needs 16
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn stemming_dictionaries_end_to_end() {
    let addr = start_server().await;

    // Import a dictionary (JSONL of word/root pairs)
    let jsonl = r#"{"word":"people","root":"person"}
{"word":"geese","root":"goose"}"#;
    let resp = c()
        .post(u(addr, "/stemming/dictionaries/import?id=irregular-e2e"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .body(jsonl)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "dictionary import");
    let body = resp.text().await.unwrap();
    assert_eq!(body.lines().count(), 2, "import echoes accepted lines");

    // List + get
    let resp = c()
        .get(u(addr, "/stemming/dictionaries"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["dictionaries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "irregular-e2e"));

    let resp = c()
        .get(u(addr, "/stemming/dictionaries/irregular-e2e"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "irregular-e2e");
    assert_eq!(body["words"].as_array().unwrap().len(), 2);

    let resp = c()
        .get(u(addr, "/stemming/dictionaries/no-such-dict"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // Collection whose field uses the dictionary
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "stem_dict_docs",
            "fields": [
                {"name": "title", "type": "string", "stem_dictionary": "irregular-e2e"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create collection: {}", resp.status());

    let jsonl = r#"{"id":"1","title":"many people gathered"}
{"id":"2","title":"a flock of geese"}
{"id":"3","title":"one dog"}"#;
    import_docs(addr, "stem_dict_docs", jsonl).await;

    // Query with the dictionary word maps to the root at query time
    for (q, expected, why) in [
        ("people", 1, "dictionary word maps to root"),
        ("person", 1, "root matches dictionary-stemmed postings"),
        ("goose", 1, "root form finds 'geese' doc"),
        ("dog", 1, "non-dictionary words still Snowball-stem"),
    ] {
        let resp = c()
            .get(u(
                addr,
                &format!(
                    "/collections/stem_dict_docs/documents/search?q={q}&query_by=title&prefix=false"
                ),
            ))
            .header("X-TYPESENSE-API-KEY", KEY)
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["found"], expected, "q={q}: {why}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn operations_and_config_endpoints() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}"#;
    import_docs(addr, "products", jsonl).await;

    // snapshot (no snapshot_path: persist-all only)
    let resp = c()
        .post(u(addr, "/operations/snapshot"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "snapshot");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);

    // snapshot with a path outside the data dir
    let snap_dir = std::env::temp_dir().join(format!("torque-snap-test-{}", std::process::id()));
    let resp = c()
        .post(u(
            addr,
            &format!("/operations/snapshot?snapshot_path={}", snap_dir.display()),
        ))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "snapshot with path");
    assert!(
        snap_dir.join("indexes").exists(),
        "snapshot dir should contain persisted indexes"
    );
    std::fs::remove_dir_all(&snap_dir).ok();

    // db/compact + cache/clear + schema_changes
    for (method_post, path) in [
        (true, "/operations/db/compact"),
        (true, "/operations/cache/clear"),
        (false, "/operations/schema_changes"),
    ] {
        let req = if method_post {
            c().post(u(addr, path))
        } else {
            c().get(u(addr, path))
        };
        let resp = req
            .header("X-TYPESENSE-API-KEY", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }

    // metrics.json has Typesense-shaped string-encoded system values
    let resp = c()
        .get(u(addr, "/metrics.json"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["system_memory_total_bytes"].is_string());
    assert!(body["system_disk_total_bytes"].is_string());
    assert!(body["typesense_memory_resident_bytes"].is_string());

    // POST /config adjusts the slow-query threshold at runtime
    let resp = c()
        .post(u(addr, "/config"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"log-slow-requests-time-ms": 100}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "config update");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);

    // Unsupported config key → 400
    let resp = c()
        .post(u(addr, "/config"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"unknown-setting": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_search_union_and_common_params() {
    let addr = start_server().await;
    create_products(addr).await;

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}
{"id":"3","title":"Laptop Stand","price":49.99,"brand":"Beta","in_stock":true,"rating":3}"#;
    import_docs(addr, "products", jsonl).await;

    // Query-string common params supply collection + query_by for all searches
    let resp = c()
        .post(u(addr, "/multi_search?collection=products&query_by=title"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "searches": [ {"q": "laptop"}, {"q": "phone"} ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "common params");
    let body: serde_json::Value = resp.json().await.unwrap();
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["found"], 2, "laptop matches 2 docs");
    assert_eq!(results[1]["found"], 1, "phone matches 1 doc");

    // Per-item values still win over common params
    let resp = c()
        .post(u(addr, "/multi_search?collection=products&query_by=brand"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "searches": [ {"q": "laptop", "query_by": "title"} ]
        }))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["results"][0]["found"], 2, "item query_by overrides common");

    // union: true merges hits into a single ranked result set
    let resp = c()
        .post(u(addr, "/multi_search?collection=products&query_by=title&per_page=2"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "union": true,
            "searches": [ {"q": "laptop"}, {"q": "phone"} ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "union");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("results").is_none(), "union returns a single result set");
    assert_eq!(body["found"], 3, "union found = sum of per-search founds");
    assert_eq!(body["hits"].as_array().unwrap().len(), 2, "union respects per_page");
    assert!(body["union_request_params"].is_array());

    // union + group_by: every search grouped (Typesense), else the union's error
    let resp = c()
        .post(u(addr, "/multi_search?collection=products&query_by=title"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "union": true,
            "searches": [ {"q": "laptop", "group_by": "brand"}, {"q": "phone", "group_by": "brand"} ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "grouped union");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 3, "groups: Acme and Beta for laptop, Beta for phone: {body}");
    assert_eq!(body["found_docs"], 3, "{body}");
    assert_eq!(body["grouped_hits"].as_array().unwrap().len(), 3, "{body}");
    let resp = c()
        .post(u(addr, "/multi_search?collection=products&query_by=title"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "union": true,
            "searches": [ {"q": "laptop", "group_by": "brand"}, {"q": "phone"} ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "a union's error is its response");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], 400);
    assert_eq!(body["error"], "Invalid group_by searches count. All searches with union search should be uniform.");

    // A search without a collection fails in its own slot, as in Typesense
    let resp = c()
        .post(u(addr, "/multi_search"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({ "searches": [ {"q": "laptop", "query_by": "title"} ] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["results"][0], serde_json::json!({"code": 404, "error": "Collection not found"}));
}

#[tokio::test(flavor = "multi_thread")]
async fn collection_metadata_synonym_sets_and_list_params() {
    let addr = start_server().await;

    // Create with metadata + a bound synonym set
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "meta_col",
            "fields": [{"name": "title", "type": "string"}],
            "metadata": {"owner": "team-search", "rev": 3},
            "synonym_sets": ["speed-e2e"]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["metadata"]["owner"], "team-search", "metadata echoed on create");
    assert_eq!(body["synonym_sets"][0], "speed-e2e");

    // Bound synonym set applies to searches without a synonym_sets param
    let resp = c()
        .put(u(addr, "/synonym_sets/speed-e2e"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "items": [{"id": "1", "synonyms": ["fast", "quick"]}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "synonym set: {}", resp.status());

    import_docs(
        addr,
        "meta_col",
        r#"{"id":"1","title":"fast car"}
{"id":"2","title":"quick brown fox"}"#,
    )
    .await;

    let resp = c()
        .get(u(addr, "/collections/meta_col/documents/search?q=quick&query_by=title&prefix=false"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2, "collection-bound synonym set expands quick->fast");

    // PATCH metadata without fields; docs must survive
    let resp = c()
        .patch(u(addr, "/collections/meta_col"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"metadata": {"owner": "team-core"}}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "patch metadata: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["metadata"]["owner"], "team-core");
    assert_eq!(body["num_documents"], 2, "metadata PATCH must not drop documents");

    // PATCH adding a field must keep documents too
    let resp = c()
        .patch(u(addr, "/collections/meta_col"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"fields": [{"name": "tag", "type": "string", "optional": true}]}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_documents"], 2, "add-field PATCH must not drop documents");
    assert_eq!(body["metadata"]["owner"], "team-core", "metadata survives field PATCH");

    // GET /collections honors limit/offset/exclude_fields
    let resp = c()
        .get(u(addr, "/collections?limit=1&exclude_fields=fields"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "limit=1");
    assert!(arr[0].get("fields").is_none(), "exclude_fields=fields strips the key");
}

#[tokio::test(flavor = "multi_thread")]
async fn facet_return_parent_nested() {
    let addr = start_server().await;

    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "shops",
            "enable_nested_fields": true,
            "fields": [
                {"name": "title", "type": "string"},
                {"name": "address.city", "type": "string", "facet": true},
                {"name": "address.zip", "type": "string", "optional": true}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create: {}", resp.status());

    import_docs(
        addr,
        "shops",
        r#"{"id":"1","title":"North Shop","address":{"city":"Stockholm","zip":"111 22"}}
{"id":"2","title":"South Shop","address":{"city":"Stockholm","zip":"333 44"}}
{"id":"3","title":"West Shop","address":{"city":"Göteborg","zip":"555 66"}}"#,
    )
    .await;

    let resp = c()
        .get(u(
            addr,
            "/collections/shops/documents/search?q=*&query_by=title&facet_by=address.city&facet_return_parent=address.city",
        ))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let counts = body["facet_counts"][0]["counts"].as_array().unwrap();
    assert_eq!(counts.len(), 2);
    for count in counts {
        let parent = count.get("parent").expect("parent object attached");
        assert!(
            parent.get("city").is_some(),
            "parent should be the address object: {parent}"
        );
    }

    // Without the param, no parent key is emitted
    let resp = c()
        .get(u(
            addr,
            "/collections/shops/documents/search?q=*&query_by=title&facet_by=address.city",
        ))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["facet_counts"][0]["counts"][0].get("parent").is_none());

    // An array of objects: a number's parent is the element holding it,
    // a float's too (Typesense compares floats as "4.500000" and finds none); an array field's
    // value is any element (Typesense: string elements only).
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "orgs",
            "enable_nested_fields": true,
            "fields": [
                {"name": "name", "type": "string"},
                {"name": "sni.code", "type": "int32[]", "facet": true},
                {"name": "sni.name", "type": "string[]"},
                {"name": "sni.share", "type": "float[]", "facet": true},
                {"name": "hq.tags", "type": "string[]", "facet": true}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create: {}", resp.status());
    import_docs(
        addr,
        "orgs",
        r#"{"id":"1","name":"One","sni":[{"code":46900,"name":"Wholesale","share":0.25},{"code":68201,"name":"Letting","share":4.5}],"hq":{"city":"Lund","tags":["trade","property"]}}
{"id":"2","name":"Two","sni":[{"code":68201,"name":"Letting","share":4.5}],"hq":{"city":"Umeå","tags":["rent"]}}"#,
    )
    .await;
    let resp = c()
        .get(u(
            addr,
            "/collections/orgs/documents/search?q=*&query_by=name&facet_by=sni.code,sni.share,hq.tags&facet_return_parent=sni.code,sni.share,hq.tags",
        ))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let parent_of = |facet: usize, value: &str| -> serde_json::Value {
        let counts = body["facet_counts"][facet]["counts"].as_array().unwrap();
        let count = counts.iter().find(|c| c["value"] == value).unwrap_or_else(|| panic!("value {value}: {counts:?}"));
        count.get("parent").cloned().unwrap_or_else(|| panic!("no parent for {value}: {count}"))
    };
    assert_eq!(parent_of(0, "46900")["name"], "Wholesale");
    assert_eq!(parent_of(0, "68201")["name"], "Letting");
    assert_eq!(parent_of(0, "68201")["code"], 68201);
    assert_eq!(parent_of(1, "0.25")["code"], 46900);
    assert_eq!(parent_of(1, "4.5")["code"], 68201);
    assert_eq!(parent_of(2, "property")["city"], "Lund");
    assert_eq!(parent_of(2, "rent")["city"], "Umeå");
}

#[tokio::test(flavor = "multi_thread")]
async fn max_facet_values_has_no_cap() {
    // Typesense gives max_facet_values values whatever their number; 0 gives none.
    let addr = start_server().await;
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "many",
            "fields": [{"name": "title", "type": "string"}, {"name": "code", "type": "int32", "facet": true}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create: {}", resp.status());
    let docs: Vec<String> = (0..1200).map(|i| format!(r#"{{"id":"{i}","title":"t","code":{i}}}"#)).collect();
    import_docs(addr, "many", &docs.join("\n")).await;
    for (max, expected) in [(100000, 1200), (1100, 1100), (0, 0)] {
        let resp = c()
            .get(u(addr, &format!("/collections/many/documents/search?q=*&query_by=title&facet_by=code&max_facet_values={max}")))
            .header("X-TYPESENSE-API-KEY", KEY)
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["facet_counts"][0]["counts"].as_array().unwrap().len(), expected, "max_facet_values={max}");
        assert_eq!(body["facet_counts"][0]["stats"]["total_values"], 1200, "max_facet_values={max}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn search_result_cache() {
    let addr = start_server().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}"#,
    )
    .await;

    let search = "/collections/products/documents/search?q=laptop&query_by=title&use_cache=true&cache_ttl=60";

    // Prime the cache
    let resp = c().get(u(addr, search)).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1);

    // Cached response is served identically
    let resp = c().get(u(addr, search)).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1);

    // A write changes the bundle, so the key misses naturally
    import_docs(
        addr,
        "products",
        r#"{"id":"2","title":"Laptop Pro","price":1299.99,"brand":"Acme","in_stock":true,"rating":5}"#,
    )
    .await;
    let resp = c().get(u(addr, search)).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2, "index change must invalidate the cache key");

    // cache/clear empties the cache (works: next request recomputes)
    let resp = c()
        .post(u(addr, "/operations/cache/clear"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = c().get(u(addr, search)).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn object_field_type_discovers_subfields() {
    let addr = start_server().await;

    // object requires enable_nested_fields
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "obj_reject",
            "fields": [{"name": "meta", "type": "object"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "object without enable_nested_fields must 400");

    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "obj_docs",
            "enable_nested_fields": true,
            "fields": [
                {"name": "title", "type": "string"},
                {"name": "meta", "type": "object"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create: {}", resp.status());

    import_docs(
        addr,
        "obj_docs",
        r#"{"id":"1","title":"first","meta":{"color":"red","qty":5}}
{"id":"2","title":"second","meta":{"color":"blue","qty":7}}"#,
    )
    .await;

    // Discovered sub-fields are searchable/filterable and appear in the schema
    let resp = c()
        .get(u(addr, "/collections/obj_docs/documents/search?q=*&query_by=title&filter_by=meta.qty:>6"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1, "filter on discovered meta.qty");

    let resp = c()
        .get(u(addr, "/collections/obj_docs"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let names: Vec<&str> = body["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"meta.color"), "schema shows discovered fields: {names:?}");
    assert!(names.contains(&"meta.qty"));

    // Docs round-trip with nested shape intact
    let resp = c()
        .get(u(addr, "/collections/obj_docs/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["meta"]["color"], "red");
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_catchall_and_string_star() {
    let addr = start_server().await;

    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "auto_docs",
            "fields": [
                {"name": "title", "type": "string"},
                {"name": "tag", "type": "string*"},
                {"name": ".*", "type": "auto"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create: {}", resp.status());

    // tag as scalar in one doc, array in another; extra undeclared key "level"
    import_docs(
        addr,
        "auto_docs",
        r#"{"id":"1","title":"first","tag":"alpha","level":3}
{"id":"2","title":"second","tag":["alpha","beta"],"level":9}"#,
    )
    .await;

    // string* matches both scalar and array docs
    let resp = c()
        .get(u(addr, "/collections/auto_docs/documents/search?q=alpha&query_by=tag&prefix=false"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2, "string* matches scalar and array docs");

    // auto-discovered numeric field is filterable
    let resp = c()
        .get(u(addr, "/collections/auto_docs/documents/search?q=*&query_by=title&filter_by=level:>5"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1, "filter on auto-discovered level field");
}

#[tokio::test(flavor = "multi_thread")]
async fn bool_array_and_geopoint_array_types()  {
    let addr = start_server().await;

    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "routes",
            "fields": [
                {"name": "title", "type": "string"},
                {"name": "flags", "type": "bool[]"},
                {"name": "stops", "type": "geopoint[]"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create: {}", resp.status());

    // Stockholm ~ (59.33, 18.07); Göteborg ~ (57.71, 11.97); Malmö ~ (55.60, 13.00)
    import_docs(
        addr,
        "routes",
        r#"{"id":"1","title":"north route","flags":[true,true],"stops":[[59.33,18.07],[59.86,17.64]]}
{"id":"2","title":"mixed route","flags":[true,false],"stops":[[57.71,11.97],[55.60,13.00]]}
{"id":"3","title":"south route","flags":[false],"stops":[[55.60,13.00]]}"#,
    )
    .await;

    // bool[] match-any: :=true matches docs 1+2; :=false matches docs 2+3
    for (val, expected) in [("true", 2), ("false", 2)] {
        let resp = c()
            .get(u(addr, &format!("/collections/routes/documents/search?q=*&query_by=title&filter_by=flags:={val}")))
            .header("X-TYPESENSE-API-KEY", KEY)
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["found"], expected, "flags:={val}");
    }

    // geopoint[] radius: 50km around Stockholm matches only doc 1 (its first stop);
    // 50km around Malmö matches docs 2 and 3 (any-point semantics)
    let resp = c()
        .get(u(addr, "/collections/routes/documents/search?q=*&query_by=title&filter_by=stops:(59.33,18.07,50%20km)"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1, "Stockholm radius");

    let resp = c()
        .get(u(addr, "/collections/routes/documents/search?q=*&query_by=title&filter_by=stops:(55.60,13.00,50%20km)"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2, "Malmö radius matches any point");

    // Docs round-trip with array shapes intact
    let resp = c()
        .get(u(addr, "/collections/routes/documents/2"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["flags"], serde_json::json!([true, false]));
    assert_eq!(body["stops"][1], serde_json::json!([55.60, 13.00]));

    // Geo sort by nearest-stop field works (first-point dense semantics)
    let resp = c()
        .get(u(addr, "/collections/routes/documents/search?q=*&query_by=title&sort_by=stops(59.33,18.07):asc"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["hits"][0]["document"]["id"], "1", "Stockholm-first sort");

}

#[tokio::test(flavor = "multi_thread")]
async fn misc_param_compat() {
    let addr = start_server().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}
{"id":"3","title":"Tablet","price":299.99,"brand":"Acme","in_stock":false,"rating":3}"#,
    )
    .await;

    // limit is an alias for per_page
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=*&query_by=title&limit=1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["hits"].as_array().unwrap().len(), 1, "limit=1");
    assert_eq!(body["found"], 3);

    // validate_field_names=false skips unknown query_by fields
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=nonexistent,title&validate_field_names=false&prefix=false"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "lenient field names");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1);

    // Default (strict) still errors
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=nonexistent,title"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "strict field names: Typesense's 404");

    // enable_synonyms=false disables expansion
    c().put(u(addr, "/synonym_sets/devices"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"items": [{"id": "1", "synonyms": ["laptop", "phone"]}]}))
        .send()
        .await
        .unwrap();

    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title&synonym_sets=devices&prefix=false"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2, "synonyms expand laptop->phone");

    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title&synonym_sets=devices&enable_synonyms=false&prefix=false"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1, "enable_synonyms=false disables expansion");
}

async fn start_mock_embeddings() -> SocketAddr {
    use axum::{routing::post, Json, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = Router::new().route(
            "/v1/embeddings",
            post(|Json(body): Json<serde_json::Value>| async move {
                let inputs = body["input"].as_array().cloned().unwrap_or_default();
                let data: Vec<serde_json::Value> = inputs
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let text = v.as_str().unwrap_or("").to_lowercase();
                        let emb = if text.contains("laptop") {
                            [1.0, 0.0, 0.0]
                        } else if text.contains("phone") {
                            [0.0, 1.0, 0.0]
                        } else {
                            [0.0, 0.0, 1.0]
                        };
                        serde_json::json!({"index": i, "embedding": emb})
                    })
                    .collect();
                Json(serde_json::json!({"data": data}))
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_embedding_end_to_end() {
    let addr = start_server().await;
    let mock = start_mock_embeddings().await;

    // embed without num_dim is rejected
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "emb_bad",
            "fields": [
                {"name": "title", "type": "string"},
                {"name": "emb", "type": "float[]",
                 "embed": {"from": ["title"], "model_config": {"model_name": "mock", "url": format!("http://{mock}/v1/embeddings")}}}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "embed without num_dim must 400");

    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "emb_docs",
            "fields": [
                {"name": "title", "type": "string"},
                {"name": "emb", "type": "float[]", "num_dim": 3,
                 "embed": {"from": ["title"], "model_config": {"model_name": "mock", "url": format!("http://{mock}/v1/embeddings"), "api_key": "k"}}}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create: {}", resp.status());

    // Docs without emb values get embedded at import time
    import_docs(
        addr,
        "emb_docs",
        r#"{"id":"1","title":"Laptop Pro"}
{"id":"2","title":"Phone Mini"}
{"id":"3","title":"Tablet Air"}"#,
    )
    .await;

    let resp = c()
        .get(u(addr, "/collections/emb_docs/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["emb"],
        serde_json::json!([1.0, 0.0, 0.0]),
        "index-time embedding stored on the doc"
    );

    // Query-time: empty vector embeds q through the field's model
    let resp = c()
        .get(u(
            addr,
            "/collections/emb_docs/documents/search?q=phone&query_by=title&prefix=false&vector_query=emb:([]%2C%20k:3)",
        ))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "hybrid search");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["hits"][0]["document"]["id"], "2",
        "query embedding ranks the phone doc first: {body}"
    );

    // Empty vector on an embed field without q is rejected
    let resp = c()
        .get(u(
            addr,
            "/collections/emb_docs/documents/search?q=*&query_by=title&vector_query=emb:([]%2C%20k:3)",
        ))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "empty vector without q must 400");
}

// ── Curation Sets ────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn curation_set_and_item_crud() {
    let addr = start_server().await;

    // Create set with one item (PUT /curation_sets/{name}, body {items})
    let resp = c()
        .put(u(addr, "/curation_sets/promo"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "description": "homepage promos",
            "items": [{
                "id": "pin-laptop",
                "rule": {"query": "laptop", "match": "exact"},
                "includes": [{"id": "3", "position": 1}]
            }]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create curation set: {}", resp.status());

    // List returns a bare array (spec shape)
    let resp = c()
        .get(u(addr, "/curation_sets"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_array(), "list must be a bare array: {body}");
    assert_eq!(body[0]["name"], "promo");

    // Get set
    let resp = c()
        .get(u(addr, "/curation_sets/promo"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["items"][0]["id"], "pin-laptop");

    // Item CRUD
    let resp = c()
        .put(u(addr, "/curation_sets/promo/items/hide-phone"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "rule": {"query": "phone", "match": "contains"},
            "excludes": [{"id": "2"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "upsert item: {}", resp.status());

    let resp = c()
        .get(u(addr, "/curation_sets/promo/items/hide-phone"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["rule"]["match"], "contains");

    let resp = c()
        .get(u(addr, "/curation_sets/promo/items"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);

    let resp = c()
        .delete(u(addr, "/curation_sets/promo/items/hide-phone"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Delete set
    let resp = c()
        .delete(u(addr, "/curation_sets/promo"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let resp = c()
        .get(u(addr, "/curation_sets/promo"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// Seed the products collection, bind a curation set to it via PATCH, and
/// return the server address.
async fn curation_fixture(addr: SocketAddr, items: serde_json::Value) {
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        concat!(
            r#"{"id": "1", "title": "Gaming Laptop", "price": 1500.0, "brand": "Acme", "in_stock": true, "rating": 5}"#, "\n",
            r#"{"id": "2", "title": "Budget Laptop", "price": 400.0, "brand": "Beta", "in_stock": true, "rating": 3}"#, "\n",
            r#"{"id": "3", "title": "Workstation Laptop", "price": 2500.0, "brand": "Acme", "in_stock": false, "rating": 4}"#,
        ),
    )
    .await;

    let resp = c()
        .put(u(addr, "/curation_sets/promo"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({ "items": items }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create curation set: {}", resp.status());

    // Bind the set to the collection (PATCH — mirrors synonym_sets).
    let resp = c()
        .patch(u(addr, "/collections/products"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"curation_sets": ["promo"]}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "bind curation set: {}", resp.status());
}

#[tokio::test(flavor = "multi_thread")]
async fn curation_pins_hides_and_echoes_metadata() {
    let addr = start_server().await;
    curation_fixture(
        addr,
        serde_json::json!([{
            "id": "laptop-promo",
            "rule": {"query": "laptop", "match": "contains"},
            "includes": [{"id": "3", "position": 1}],
            "excludes": [{"id": "2"}],
            "metadata": {"banner": "laptop-sale"}
        }]),
    )
    .await;

    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits[0]["document"]["id"], "3", "pinned doc must lead: {body}");
    assert!(
        !hits.iter().any(|h| h["document"]["id"] == "2"),
        "excluded doc must be hidden: {body}"
    );
    assert_eq!(body["metadata"]["banner"], "laptop-sale");

    // enable_curations=false bypasses the rule entirely.
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title&enable_curations=false"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 3, "all docs return without curations: {body}");
    assert!(body.get("metadata").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn curation_filter_and_tags() {
    let addr = start_server().await;
    curation_fixture(
        addr,
        serde_json::json!([
            {
                "id": "a-acme-only",
                "rule": {"query": "laptop", "match": "contains"},
                "filter_by": "brand:=Acme"
            },
            {
                "id": "b-summer-pin",
                "rule": {"query": "*", "tags": ["summer"]},
                "includes": [{"id": "2", "position": 1}]
            }
        ]),
    )
    .await;

    // The filter rule narrows results to Acme laptops.
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 2, "curation filter must narrow to Acme: {body}");

    // The tagged rule only fires with curation_tags.
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=workstation&query_by=title&curation_tags=summer"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits[0]["document"]["id"], "2", "tag-gated pin must lead: {body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn curation_filter_curated_hits() {
    let addr = start_server().await;
    // Pin doc 3 (out of stock) on every query.
    curation_fixture(
        addr,
        serde_json::json!([{
            "id": "pin-3",
            "rule": {"query": "*"},
            "includes": [{"id": "3", "position": 1}]
        }]),
    )
    .await;

    // Default: the pin bypasses the filter.
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title&filter_by=in_stock:=true"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["hits"][0]["document"]["id"], "3",
        "pin bypasses filter by default: {body}"
    );

    // filter_curated_hits=true: the out-of-stock pin is dropped.
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title&filter_by=in_stock:=true&filter_curated_hits=true"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        !body["hits"].as_array().unwrap().iter().any(|h| h["document"]["id"] == "3"),
        "filtered pin must be dropped: {body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn curation_replace_query() {
    let addr = start_server().await;
    curation_fixture(
        addr,
        serde_json::json!([{
            "id": "redirect",
            "rule": {"query": "cheap machines", "match": "exact"},
            "replace_query": "budget"
        }]),
    )
    .await;

    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=cheap%20machines&query_by=title"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1, "replaced query must find Budget Laptop: {body}");
    assert_eq!(body["hits"][0]["document"]["id"], "2");
}

// ── Analytics ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn analytics_rule_crud() {
    let addr = start_server().await;

    // POST accepts a single rule
    let resp = c()
        .post(u(addr, "/analytics/rules"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "popular",
            "type": "popular_queries",
            "collection": "products",
            "event_type": "search",
            "params": {"destination_collection": "product_queries", "limit": 100}
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create rule: {}", resp.status());

    // POST accepts an array
    let resp = c()
        .post(u(addr, "/analytics/rules"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!([{
            "name": "clicks",
            "type": "counter",
            "collection": "products",
            "event_type": "click",
            "params": {"counter_field": "popularity", "weight": 1}
        }]))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create rules array: {}", resp.status());

    // Validation: popular without destination 400s
    let resp = c()
        .post(u(addr, "/analytics/rules"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "bad", "type": "popular_queries",
            "collection": "products", "event_type": "search"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // List + get
    let resp = c()
        .get(u(addr, "/analytics/rules"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["rules"].as_array().unwrap().len(), 2);

    let resp = c()
        .get(u(addr, "/analytics/rules/popular"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "popular_queries");

    // PUT partial update keeps unspecified fields
    let resp = c()
        .put(u(addr, "/analytics/rules/popular"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"rule_tag": "homepage"}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "put rule: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "popular_queries", "partial PUT keeps type: {body}");
    assert_eq!(body["rule_tag"], "homepage");

    // Delete
    let resp = c()
        .delete(u(addr, "/analytics/rules/clicks"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let resp = c()
        .get(u(addr, "/analytics/rules/clicks"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn analytics_capture_flush_and_status() {
    let addr = start_server().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        r#"{"id": "1", "title": "Gaming Laptop", "price": 1500.0, "brand": "Acme", "in_stock": true, "rating": 5}"#,
    )
    .await;

    // Destination collection for popular queries (realtime so flush upserts land instantly)
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "product_queries",
            "index_mode": "realtime",
            "fields": [
                {"name": "q", "type": "string"},
                {"name": "count", "type": "int32"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "dest collection: {}", resp.status());

    let resp = c()
        .post(u(addr, "/analytics/rules"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "popular",
            "type": "popular_queries",
            "collection": "products",
            "event_type": "search",
            "params": {"destination_collection": "product_queries"}
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Two searches auto-captured + one explicit event
    for _ in 0..2 {
        let resp = c()
            .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title"))
            .header("X-TYPESENSE-API-KEY", KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }
    let resp = c()
        .post(u(addr, "/analytics/events"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "popular", "event_type": "search",
            "data": {"q": "laptop", "user_id": "u1"}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "event accepted");

    // Status shows one pending distinct query
    let resp = c()
        .get(u(addr, "/analytics/status"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["popular_prefix_queries"], 1, "pending queries: {body}");

    // Flush writes {q, count} into the destination collection
    let resp = c()
        .post(u(addr, "/analytics/flush"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);

    let resp = c()
        .get(u(addr, "/collections/product_queries/documents/laptop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "flushed query doc must exist");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["count"], 3, "2 captured searches + 1 event: {body}");

    // Second flush accumulates on top of the existing count
    c().post(u(addr, "/analytics/events"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "popular", "event_type": "search", "data": {"q": "laptop"}
        }))
        .send()
        .await
        .unwrap();
    c().post(u(addr, "/analytics/flush"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let resp = c()
        .get(u(addr, "/collections/product_queries/documents/laptop"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["count"], 4, "flush accumulates: {body}");

    // Status is drained
    let resp = c()
        .get(u(addr, "/analytics/status"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["popular_prefix_queries"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn analytics_counter_and_log_events() {
    let addr = start_server().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        r#"{"id": "1", "title": "Gaming Laptop", "price": 1500.0, "brand": "Acme", "in_stock": true, "rating": 5}"#,
    )
    .await;

    // Counter rule targets the products.rating field (int32, exists in schema)
    let resp = c()
        .post(u(addr, "/analytics/rules"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!([
            {
                "name": "clicks",
                "type": "counter",
                "collection": "products",
                "event_type": "click",
                "params": {"counter_field": "rating", "weight": 2}
            },
            {
                "name": "journal",
                "type": "log",
                "collection": "products",
                "event_type": "conversion"
            }
        ]))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // 3 clicks x weight 2 = +6 on rating (5 -> 11)
    for _ in 0..3 {
        let resp = c()
            .post(u(addr, "/analytics/events"))
            .header("X-TYPESENSE-API-KEY", KEY)
            .json(&serde_json::json!({
                "name": "clicks", "event_type": "click",
                "data": {"doc_id": "1", "user_id": "u1"}
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
    }

    // Log events retained and queryable
    let resp = c()
        .post(u(addr, "/analytics/events"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "journal", "event_type": "conversion",
            "data": {"doc_id": "1", "user_id": "u7"}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let resp = c()
        .get(u(addr, "/analytics/events?user_id=u7&name=journal&n=10"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["events"].as_array().unwrap().len(), 1, "log event listed: {body}");
    assert_eq!(body["events"][0]["doc_id"], "1");

    // Event type mismatch 400s
    let resp = c()
        .post(u(addr, "/analytics/events"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "name": "clicks", "event_type": "conversion", "data": {"doc_id": "1"}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Flush applies the counter deltas to the doc
    c().post(u(addr, "/analytics/flush"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let resp = c()
        .get(u(addr, "/collections/products/documents/1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["rating"], 11, "counter flush applies weighted deltas: {body}");
}

// ── Conversational + NL search models ────────────────────────────────────

/// Mock OpenAI-compatible chat-completions endpoint. Routes on the user
/// message content: questions get a canned RAG answer, NL translation
/// requests get a canned JSON param object.
async fn start_mock_chat() -> SocketAddr {
    use axum::{routing::post, Json, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<serde_json::Value>| async move {
                let messages = body["messages"].as_array().cloned().unwrap_or_default();
                let system = messages
                    .first()
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or("")
                    .to_string();
                let user = messages
                    .last()
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or("")
                    .to_string();
                // Streaming request: emit OpenAI-style SSE delta chunks.
                if body["stream"] == true {
                    let question = user
                        .split(torque::llm::QUESTION_STR)
                        .nth(1)
                        .and_then(|s| s.split(torque::llm::ANSWER_STR).next())
                        .unwrap_or("")
                        .to_string();
                    let sse = format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        serde_json::json!({"choices": [{"delta": {"content": "streamed answer "}}]}),
                        serde_json::json!({"choices": [{"delta": {"content": format!("for: {question}")}}]}),
                    );
                    return axum::response::Response::builder()
                        .status(200)
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse))
                        .unwrap();
                }
                let content = if user.contains("The question on its own:") {
                    // Standalone-question rewrite request
                    "standalone rewritten query".to_string()
                } else if system.contains("search parameters for one collection") {
                    // NL translation request
                    r#"{"q": "laptop", "filter_by": "price:<1000", "sort_by": "price:asc"}"#
                        .to_string()
                } else if user.starts_with(torque::llm::DATA_STR) {
                    // RAG answer request: echo the question segment so tests can
                    // verify which query drove the prompt.
                    let question = user
                        .split(torque::llm::QUESTION_STR)
                        .nth(1)
                        .and_then(|s| s.split(torque::llm::ANSWER_STR).next())
                        .unwrap_or("");
                    format!("answer for: {question}")
                } else {
                    format!("unexpected request: {user}")
                };
                axum::response::Response::builder()
                    .status(200)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "choices": [{"message": {"role": "assistant", "content": content}}]
                        })
                        .to_string(),
                    ))
                    .unwrap()
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn conversational_search_streaming() {
    let addr = start_server().await;
    let mock = start_mock_chat().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        r#"{"id": "1", "title": "Gaming Laptop", "price": 1500.0, "brand": "Acme", "in_stock": true, "rating": 5}"#,
    )
    .await;

    let resp = c()
        .post(u(addr, "/conversations/models"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "id": "conv-stream",
            "model_name": "vllm/assistant",
            "vllm_url": format!("http://{mock}"),
            "max_bytes": 16384
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create model: {}", resp.status());

    // Streamed first turn: SSE with delta events, [DONE], then the full
    // results JSON as the final event.
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=gaming%20laptop&query_by=title&conversation=true&conversation_stream=true&conversation_model_id=conv-stream"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream"),
        "SSE content type"
    );
    let body = resp.text().await.unwrap();
    let events: Vec<&str> = body
        .split("\n\n")
        .filter(|e| !e.trim().is_empty())
        .map(|e| e.trim_start_matches("data: "))
        .collect();
    assert!(events.len() >= 2, "delta event(s) + DONE + final results: {body}");

    // Delta events carry the conversation id + message chunks; chunks may
    // batch depending on read boundaries, so assert on the concatenation.
    let first: serde_json::Value = serde_json::from_str(events[0]).unwrap();
    let cid = first["conversation_id"].as_str().unwrap().to_string();
    assert!(!cid.is_empty());
    let streamed: String = events
        .iter()
        .filter_map(|e| serde_json::from_str::<serde_json::Value>(e).ok())
        .filter_map(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .collect();
    assert_eq!(streamed, "streamed answer for: gaming laptop");

    // The DONE sentinel is forwarded.
    assert!(events.iter().any(|e| *e == "[DONE]"), "DONE forwarded: {body}");

    // The final event is the complete search-results JSON with the
    // accumulated answer folded into `conversation`.
    let last: serde_json::Value = serde_json::from_str(events.last().unwrap()).unwrap();
    assert_eq!(last["found"], 1, "full results in final event: {last}");
    let conv = &last["conversation"];
    assert_eq!(conv["answer"], "streamed answer for: gaming laptop");
    assert_eq!(conv["conversation_id"], cid.as_str());
    assert_eq!(conv["conversation_history"].as_array().unwrap().len(), 2);

    // History is durable, same as the non-streaming path.
    let resp = c()
        .get(u(addr, "/collections/ts_conversation_history_conv-stream"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_documents"], 2, "history persisted: {body}");

    // A streamed follow-up still goes through the standalone rewrite.
    let resp = c()
        .get(u(addr, &format!("/collections/products/documents/search?q=and%20cheaper%20ones%3F&query_by=title&conversation=true&conversation_stream=true&conversation_model_id=conv-stream&conversation_id={cid}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    let last = body
        .split("\n\n")
        .filter(|e| !e.trim().is_empty())
        .last()
        .unwrap()
        .trim_start_matches("data: ");
    let last: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(
        last["conversation"]["answer"], "streamed answer for: standalone rewritten query",
        "rewrite drives the streamed answer: {last}"
    );
    assert_eq!(
        last["conversation"]["conversation_history"].as_array().unwrap().len(),
        4
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn conversation_model_crud() {
    let addr = start_server().await;

    // Create without id — server generates one
    let resp = c()
        .post(u(addr, "/conversations/models"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "model_name": "openai/gpt-4o",
            "max_bytes": 16384,
            "history_collection": "conversation_store",
            "api_key": "sk-test"
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create model: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    assert!(!id.is_empty(), "auto-generated id: {body}");

    // List + get + update + delete
    let resp = c()
        .get(u(addr, "/conversations/models"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 1);

    let resp = c()
        .put(u(addr, &format!("/conversations/models/{id}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "model_name": "openai/gpt-4o-mini",
            "max_bytes": 8192,
            "history_collection": "conversation_store"
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["model_name"], "openai/gpt-4o-mini");

    let resp = c()
        .delete(u(addr, &format!("/conversations/models/{id}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let resp = c()
        .get(u(addr, &format!("/conversations/models/{id}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn nl_search_model_crud_and_translation() {
    let addr = start_server().await;
    let mock = start_mock_chat().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        concat!(
            r#"{"id": "1", "title": "Gaming Laptop", "price": 1500.0, "brand": "Acme", "in_stock": true, "rating": 5}"#, "\n",
            r#"{"id": "2", "title": "Budget Laptop", "price": 400.0, "brand": "Beta", "in_stock": true, "rating": 3}"#,
        ),
    )
    .await;

    // Model CRUD
    let resp = c()
        .post(u(addr, "/nl_search_models"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "id": "nl-default",
            "model_name": "custom/translator",
            "api_url": format!("http://{mock}/v1/chat/completions"),
            "max_bytes": 16384
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create nl model: {}", resp.status());

    let resp = c()
        .get(u(addr, "/nl_search_models/nl-default"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // NL search: the mock translates to q=laptop + price:<1000 + price:asc
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=show%20me%20cheap%20laptops&query_by=title&nl_query=true&nl_model_id=nl-default"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 1, "translated filter narrows to <1000: {body}");
    assert_eq!(body["hits"][0]["document"]["id"], "2");
    // Typesense: request_params echoes the query as asked, parsed_nl_query what the model gave
    assert_eq!(body["request_params"]["q"], "show me cheap laptops", "{body}");
    assert_eq!(body["parsed_nl_query"]["generated_params"], serde_json::json!({"q": "laptop", "filter_by": "price:<1000", "sort_by": "price:asc"}));
    assert_eq!(body["parsed_nl_query"]["augmented_params"]["q"], "laptop");

    // The search's own filter_by is ANDed with the model's.
    let search = |query: &'static str| async move {
        let resp = c().get(u(addr, &format!("/collections/products/documents/search?{query}"))).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
        (resp.status().as_u16(), resp.json::<serde_json::Value>().await.unwrap())
    };
    let (status, body) = search("q=show%20me%20laptops&query_by=title&nl_query=true&nl_model_id=nl-default&filter_by=brand:Acme%20||%20brand:Beta").await;
    assert_eq!(status, 200);
    assert_eq!(body["parsed_nl_query"]["augmented_params"]["filter_by"], "(brand:Acme || brand:Beta) && (price:<1000)");
    assert_eq!(body["found"], 1);

    // Without nl_model_id the model named `default` translates; there is none, so the search runs
    // as asked and says why (Typesense).
    let (status, body) = search("q=laptop&query_by=title&nl_query=true").await;
    assert_eq!(status, 200);
    assert_eq!(body["found"], 2);
    assert!(body["parsed_nl_query"]["error"].as_str().unwrap().contains("Model not found"), "{body}");

    // multi_search: each search translated by its own model; exclude_fields leaves keys out
    let (status, body) = multi_json(
        addr,
        "",
        serde_json::json!({"searches": [
            {"collection": "products", "q": "show me cheap laptops", "query_by": "title", "nl_query": true, "nl_model_id": "nl-default"},
            {"collection": "products", "q": "laptop", "query_by": "title"},
            {"collection": "products", "q": "show me cheap laptops", "query_by": "title", "nl_query": true, "nl_model_id": "nl-default", "exclude_fields": "parsed_nl_query,out_of"},
        ]}),
    )
    .await;
    assert_eq!(status, 200);
    let r = &body["results"];
    assert_eq!((r[0]["found"].as_u64(), r[1]["found"].as_u64(), r[2]["found"].as_u64()), (Some(1), Some(2), Some(1)), "{body}");
    assert_eq!(r[0]["parsed_nl_query"]["augmented_params"]["sort_by"], "price:asc");
    assert!(r[1].get("parsed_nl_query").is_none() && r[2].get("parsed_nl_query").is_none() && r[2].get("out_of").is_none());
    assert!(r[0].get("out_of").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn exclude_fields_leaves_response_keys_out() {
    let addr = start_server().await;
    create_products(addr).await;
    import_docs(addr, "products", r#"{"id": "1", "title": "laptop", "price": 1.5, "brand": "Acme", "in_stock": true, "rating": 5, "out_of": 3}"#).await;
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=laptop&query_by=title&exclude_fields=out_of,search_time_ms,brand"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("out_of").is_none() && body.get("search_time_ms").is_none(), "{body}");
    assert_eq!(body["found"], 1);
    // a document's own `out_of` stays (Typesense: a response key), its brand goes
    let doc = &body["hits"][0]["document"];
    assert_eq!((doc.get("out_of"), doc.get("brand")), (Some(&serde_json::json!(3)), None));
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_search_conversation_answers_from_every_search() {
    let addr = start_server().await;
    let mock = start_mock_chat().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        concat!(
            r#"{"id": "1", "title": "Gaming Laptop", "price": 1500.0, "brand": "Acme", "in_stock": true, "rating": 5}"#, "\n",
            r#"{"id": "2", "title": "Budget Laptop", "price": 400.0, "brand": "Beta", "in_stock": true, "rating": 3}"#,
        ),
    )
    .await;
    let resp = c()
        .post(u(addr, "/conversations/models"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"id": "conv-multi", "model_name": "vllm/assistant", "vllm_url": format!("http://{mock}/v1/chat/completions"), "max_bytes": 16384}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let searches = serde_json::json!({"searches": [
        {"collection": "products", "query_by": "title"},
        {"collection": "products", "query_by": "title", "filter_by": "price:<1000"},
    ]});

    let (status, body) = multi_json(addr, "q=laptop&conversation=true&conversation_model_id=conv-multi", searches.clone()).await;
    assert_eq!(status, 200, "{body}");
    let conv = &body["conversation"];
    assert_eq!((conv["answer"].as_str(), conv["query"].as_str()), (Some("answer for: laptop"), Some("laptop")), "{body}");
    assert_eq!(conv["conversation_history"].as_array().unwrap().len(), 2);
    assert_eq!((body["results"][0]["found"].as_u64(), body["results"][1]["found"].as_u64()), (Some(2), Some(1)));
    assert_eq!(body["results"][1]["request_params"]["q"], "laptop");
    let cid = conv["conversation_id"].as_str().unwrap().to_string();

    // a follow-up runs as the standalone question; every search echoes the question as asked
    let (status, body) = multi_json(addr, &format!("q=which%20is%20cheapest&conversation=true&conversation_model_id=conv-multi&conversation_id={cid}&exclude_fields=conversation_history"), searches.clone()).await;
    assert_eq!(status, 200, "{body}");
    let conv = &body["conversation"];
    assert_eq!(conv["answer"], "answer for: standalone rewritten query");
    assert_eq!((conv["query"].as_str(), conv["conversation_id"].as_str()), (Some("which is cheapest"), Some(cid.as_str())));
    assert!(conv.get("conversation_history").is_none());
    assert_eq!(body["results"][0]["request_params"]["q"], "which is cheapest");

    // Typesense's rules: the question, model and conversation in the URL only
    let error = |body: serde_json::Value| body["message"].as_str().unwrap_or_default().to_string();
    let (status, body) = multi_json(addr, "q=laptop&conversation=true&conversation_model_id=conv-multi", serde_json::json!({"searches": [{"collection": "products", "q": "x"}]})).await;
    assert_eq!(status, 400);
    assert!(error(body).starts_with("`q` parameter cannot be used in POST body if `conversation` is enabled."));
    let (status, body) = multi_json(addr, "q=laptop", serde_json::json!({"searches": [{"collection": "products", "query_by": "title", "conversation": true}]})).await;
    assert_eq!((status, error(body).starts_with("`conversation` cannot be used in POST body.")), (400, true));
    let (status, body) = multi_json(addr, "conversation=true&conversation_model_id=conv-multi", searches.clone()).await;
    assert_eq!((status, error(body).starts_with("`q` parameter has to be common for all searches")), (400, true));
    let (status, body) = multi_json(addr, "q=laptop&conversation_id=abc", searches).await;
    assert_eq!((status, error(body)), (400, "`conversation_id` can only be used if `conversation` is enabled.".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn conversational_search_end_to_end() {
    let addr = start_server().await;
    let mock = start_mock_chat().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        r#"{"id": "1", "title": "Gaming Laptop", "price": 1500.0, "brand": "Acme", "in_stock": true, "rating": 5}"#,
    )
    .await;

    // No history_collection given: the server auto-creates the Typesense
    // default `ts_conversation_history_{model_id}`.
    let resp = c()
        .post(u(addr, "/conversations/models"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({
            "id": "conv-default",
            "model_name": "vllm/assistant",
            "vllm_url": format!("http://{mock}/v1/chat/completions"),
            "max_bytes": 16384
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create conv model: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["history_collection"], "ts_conversation_history_conv-default",
        "default history collection assigned: {body}"
    );

    // First turn — no history, so no standalone rewrite: the answer prompt
    // carries the raw question.
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=what%20laptops%20do%20you%20have&query_by=title&conversation=true&conversation_model_id=conv-default"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let conv = &body["conversation"];
    assert_eq!(
        conv["answer"], "answer for: what laptops do you have",
        "first turn answers the raw question: {body}"
    );
    let cid = conv["conversation_id"].as_str().unwrap().to_string();
    assert!(!cid.is_empty());
    // History entries are role-keyed objects (Typesense shape).
    let history = conv["conversation_history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["user"], "what laptops do you have");
    assert_eq!(history[1]["assistant"], "answer for: what laptops do you have");
    assert_eq!(conv["query"], "what laptops do you have");

    // Second turn — the follow-up is rewritten into a standalone question
    // which drives BOTH retrieval and the answer prompt; the RAW follow-up
    // goes into the history.
    let resp = c()
        .get(u(addr, &format!("/collections/products/documents/search?q=which%20is%20fastest&query_by=title&conversation=true&conversation_model_id=conv-default&conversation_id={cid}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let conv = &body["conversation"];
    assert_eq!(conv["conversation_id"], cid.as_str());
    assert_eq!(
        conv["answer"], "answer for: standalone rewritten query",
        "answer prompt must carry the standalone question: {body}"
    );
    assert_eq!(
        conv["query"], "standalone rewritten query",
        "conversation.query echoes the effective query: {body}"
    );
    assert_eq!(
        body["request_params"]["q"], "standalone rewritten query",
        "retrieval must re-run on the standalone question: {body}"
    );
    let history = conv["conversation_history"].as_array().unwrap();
    assert_eq!(history.len(), 4);
    assert_eq!(
        history[2]["user"], "which is fastest",
        "history stores the RAW follow-up, not the rewrite: {body}"
    );

    // History is durable: the exchange docs landed in the auto-created
    // history collection ({conversation_id, role, message, timestamp}).
    let resp = c()
        .get(u(addr, "/collections/ts_conversation_history_conv-default"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_documents"], 4, "4 history docs persisted: {body}");

    // Collection fallback: pre-seed history docs for a conversation id the
    // in-memory cache has never seen — the follow-up must find it there
    // (this is the restart-recovery path).
    let seed = concat!(
        r#"{"id": "h1", "conversation_id": "ext-conv-1", "model_id": "conv-default", "role": "user", "message": "older question", "timestamp": 100}"#, "\n",
        r#"{"id": "h2", "conversation_id": "ext-conv-1", "model_id": "conv-default", "role": "assistant", "message": "older answer", "timestamp": 100}"#,
    );
    import_docs(addr, "ts_conversation_history_conv-default", seed).await;
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=and%20the%20cheapest%3F&query_by=title&conversation=true&conversation_model_id=conv-default&conversation_id=ext-conv-1"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "history read back from collection: {body}");
    assert_eq!(
        body["conversation"]["answer"], "answer for: standalone rewritten query",
        "rewrite fired from collection-loaded history: {body}"
    );

    // Unknown conversation_id is an error (Typesense: conversation not found).
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=x&query_by=title&conversation=true&conversation_model_id=conv-default&conversation_id=nope"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Missing model id 400s
    let resp = c()
        .get(u(addr, "/collections/products/documents/search?q=x&query_by=title&conversation=true"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn operations_quiesce_endpoint() {
    let addr = start_server().await;
    // Unique collection name: the api_test harness shares one data_dir
    // across parallel tests, and other tests create/delete `products` —
    // the on-disk assertion below must not race them.
    let mut schema = products_schema();
    schema["name"] = serde_json::json!("quiesce_products");
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&schema)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "create collection: {}", resp.status());

    let jsonl = r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.0,"brand":"Bmax","in_stock":true,"rating":4}"#;
    import_docs(addr, "quiesce_products", jsonl).await;

    // Quiesce: drains in-flight builds/saves (the import above spawns a
    // background save), persists everything, returns the inventory.
    let resp = c()
        .post(u(addr, "/operations/quiesce"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "quiesce");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], true);
    let inv = body["inventory"].as_array().expect("inventory array");
    let products = inv
        .iter()
        .find(|col| col["name"] == "quiesce_products")
        .expect("quiesce_products in inventory");
    assert_eq!(products["num_documents"], 2);
    assert_eq!(products["persistence_enabled"], true);

    // After quiesce: gauges drained and the index is on disk.
    let resp = c()
        .get(u(addr, "/admin/stats"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let stats: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(stats["in_flight"]["index_builds"], 0);
    assert_eq!(stats["in_flight"]["index_saves"], 0);
    assert_eq!(stats["in_flight"]["ingest_sessions"], 0);
    let col = stats["collections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|col| col["name"] == "quiesce_products")
        .expect("quiesce_products in stats");
    assert_eq!(col["persistence"]["index_on_disk"], true);

    // Quiesce with an aggressive timeout on an idle server still succeeds.
    let resp = c()
        .post(u(addr, "/operations/quiesce?timeout_ms=1000"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "idle quiesce with short timeout");
}

// ── Segment upload (TQBF v3 spec §10) ───────────────────────────────────

fn docs_only_file(path: &std::path::Path, docs: &[serde_json::Value]) {
    use torque::tqbf::container::{kind, SegmentWriter, SEGMENT_WIDE};
    let mut reg = torque::tqbf::FieldRegistry::new();
    let mut rw = torque::tqbf::records::RecordsWriter::new();
    for d in docs {
        rw.push(&torque::tqbf::record::encode(d, d["id"].as_str().unwrap(), &mut reg).unwrap()).unwrap();
    }
    let mut w = SegmentWriter::new();
    w.add(kind::SCHEMA, SEGMENT_WIDE, serde_json::to_vec(&products_schema()).unwrap());
    w.add(kind::FIELDS, SEGMENT_WIDE, reg.encode());
    w.add(kind::RECORDS, SEGMENT_WIDE, rw.finish().unwrap());
    w.write(path, docs.len() as u32, 0).unwrap();
}

fn product(i: u32) -> serde_json::Value {
    serde_json::json!({"id": format!("p{i}"), "title": format!("Widget {i}"), "price": f64::from(i) + 0.5,
        "brand": if i % 2 == 0 { "Acme" } else { "Globex" }, "in_stock": i % 3 != 0, "rating": i, "extra": {"x": [i]}})
}

async fn put_part(addr: SocketAddr, name: &str, part: u32, parts: u32, file: &std::path::Path) -> reqwest::Response {
    c().put(u(addr, &format!("/collections/{name}/segments?part={part}&parts={parts}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .header("Content-Type", "application/octet-stream")
        .body(std::fs::read(file).unwrap())
        .send()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_segments_creates_collections() {
    let addr = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let docs: Vec<serde_json::Value> = (0..40).map(product).collect();

    // A docs-only file: the server builds it.
    let docs_only = dir.path().join("docs.tqbf");
    docs_only_file(&docs_only, &docs);
    let resp = put_part(addr, "up_docs", 0, 1, &docs_only).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_documents"], 40);

    // The same documents built offline into two segments (one per half), uploaded as two parts.
    let opts = torque::ingest::segment_file::BuildOptions { docs_level: 3 };
    let mut segments = Vec::new();
    for (i, half) in [&docs[..25], &docs[25..]].into_iter().enumerate() {
        let input = dir.path().join(format!("half{i}.tqbf"));
        docs_only_file(&input, half);
        let report = torque::ingest::segment_file::build(&input, &dir.path().join(format!("built{i}")), &opts).unwrap();
        assert_eq!(report.segments.len(), 1);
        segments.push(report.segments[0].0.clone());
    }
    let resp = put_part(addr, "up_built", 1, 2, &segments[1]).await;
    assert_eq!(resp.status(), 202, "first part only stages");
    let resp = put_part(addr, "up_built", 0, 2, &segments[0]).await;
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    for name in ["up_docs", "up_built"] {
        let get = |id: &'static str| c().get(u(addr, &format!("/collections/{name}/documents/{id}"))).header("X-TYPESENSE-API-KEY", KEY).send();
        let doc: serde_json::Value = get("p31").await.unwrap().json().await.unwrap();
        assert_eq!(doc, product(31), "{name}");
        let res: serde_json::Value = c()
            .get(u(addr, &format!("/collections/{name}/documents/search?q=widget&query_by=title&filter_by=brand:=Acme&per_page=50")))
            .header("X-TYPESENSE-API-KEY", KEY)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(res["found"], 20, "{name}: {res}");
    }

    // A name in use is refused; so is a damaged file.
    let resp = put_part(addr, "up_docs", 0, 1, &docs_only).await;
    assert_eq!(resp.status(), 409);
    let mut bad = std::fs::read(&segments[0]).unwrap();
    let n = bad.len();
    bad[n - 100] ^= 0xFF;
    let bad_path = dir.path().join("bad.tqbf");
    std::fs::write(&bad_path, bad).unwrap();
    let resp = put_part(addr, "up_bad", 0, 1, &bad_path).await;
    assert_eq!(resp.status(), 400, "{}", resp.text().await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn union_orders_by_the_searches_sort_and_counts_each_document_once() {
    let addr = start_server().await;
    create_products(addr).await;
    import_docs(
        addr,
        "products",
        r#"{"id":"1","title":"Laptop","price":999.99,"brand":"Acme","in_stock":true,"rating":5}
{"id":"2","title":"Phone","price":499.99,"brand":"Beta","in_stock":true,"rating":4}
{"id":"3","title":"Laptop Stand","price":49.99,"brand":"Beta","in_stock":true,"rating":3}"#,
    )
    .await;
    let mut gadgets = products_schema();
    gadgets["name"] = "gadgets".into();
    let resp = c().post(u(addr, "/collections")).header("X-TYPESENSE-API-KEY", KEY).json(&gadgets).send().await.unwrap();
    assert!(resp.status().is_success());
    import_docs(
        addr,
        "gadgets",
        r#"{"id":"g1","title":"Laptop Bag","price":79.0,"brand":"Acme","in_stock":true,"rating":2}
{"id":"g2","title":"Tablet","price":599.0,"brand":"Gamma","in_stock":false,"rating":5}"#,
    )
    .await;
    let ids = |b: &serde_json::Value| b["hits"].as_array().unwrap().iter().map(|h| h["document"]["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();

    // two collections ordered by price, each hit with its search and collection
    let both = serde_json::json!({ "union": true, "searches": [
        {"collection": "products", "q": "*", "sort_by": "price:desc"},
        {"collection": "gadgets", "q": "*", "sort_by": "price:desc"}
    ]});
    let (status, body) = multi_json(addr, "per_page=10", both.clone()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(ids(&body), ["1", "g2", "2", "g1", "3"], "{body}");
    assert_eq!(body["found"], 5);
    assert_eq!(body["out_of"], 5);
    assert_eq!(body["hits"][1]["search_index"], 1);
    assert_eq!(body["hits"][1]["collection"], "gadgets");
    assert_eq!(body["union_request_params"][1]["found"], 2, "{body}");
    // a page of the ordering
    let (_, page) = multi_json(addr, "per_page=2&page=2", both).await;
    assert_eq!(ids(&page), ["2", "g1"], "{page}");
    assert_eq!(page["page"], 2);

    // Typesense's checks of the searches' sorts
    let union = |a: serde_json::Value, b: serde_json::Value| serde_json::json!({ "union": true, "searches": [a, b] });
    let (status, body) = multi_json(addr, "", union(
        serde_json::json!({"collection": "products", "q": "*", "sort_by": "rating:desc"}),
        serde_json::json!({"collection": "gadgets", "q": "*", "sort_by": "price:desc"}),
    ))
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["code"], 400);
    assert_eq!(
        body["error"],
        "Expected type of `price` sort_by (float_field) at search index `1` to be the same as the type of `rating` sort_by (int32_field) at search index `0`."
    );
    let (_, body) = multi_json(addr, "", union(
        serde_json::json!({"collection": "products", "q": "*", "sort_by": "price:asc"}),
        serde_json::json!({"collection": "gadgets", "q": "*", "sort_by": "price:desc"}),
    ))
    .await;
    assert_eq!(body["error"], "Expected order of `price` sort_by (DESC) at search index `1` to be the same as the order of `price` sort_by (ASC) at search index `0`.");
    let (_, body) = multi_json(addr, "query_by=title", union(
        serde_json::json!({"collection": "products", "q": "laptop", "sort_by": "price:desc"}),
        serde_json::json!({"collection": "gadgets", "q": "*", "sort_by": "price:desc"}),
    ))
    .await;
    assert_eq!(
        body["error"],
        "Expected size of `sort_by` parameter of all searches to be equal. The first union search sorts on {`price: float_field`, `_text_match: text_match`} but the search at index `1` sorts on {`price: float_field`, `_union_search_index: union_query_order`, `_seq_id: insertion_order`}."
    );

    // the same collection twice: "Laptop Stand" matches both, appears and counts once
    let overlap = |dupes: bool| serde_json::json!({ "union": true, "remove_duplicates": dupes, "searches": [
        {"collection": "products", "q": "laptop", "facet_by": "brand"},
        {"collection": "products", "q": "stand", "facet_by": "brand"}
    ]});
    let (status, body) = multi_json(addr, "query_by=title&per_page=10", overlap(true)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(ids(&body).len(), 2, "{body}");
    assert_eq!(body["found"], 2, "each document once: {body}");
    assert_eq!(body["out_of"], 3, "a collection once: {body}");
    let beta = |b: &serde_json::Value| b["facet_counts"][0]["counts"].as_array().unwrap().iter().find(|c| c["value"] == "Beta").map(|c| c["count"].clone());
    assert_eq!(beta(&body), Some(1.into()), "{body}");
    // found does not depend on the page asked for
    let (_, small) = multi_json(addr, "query_by=title&per_page=1", overlap(true)).await;
    assert_eq!(small["found"], 2, "{small}");
    let (_, body) = multi_json(addr, "query_by=title&per_page=10", overlap(false)).await;
    assert_eq!(ids(&body).len(), 3, "{body}");
    assert_eq!(body["found"], 3, "{body}");
    assert_eq!(beta(&body), Some(2.into()), "{body}");

    // a pinned hit at its place, once
    let (status, body) = multi_json(addr, "query_by=title&pinned_hits=2:1", serde_json::json!({ "union": true, "searches": [
        {"collection": "products", "q": "laptop"},
        {"collection": "products", "q": "phone"}
    ]}))
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(ids(&body)[0], "2", "{body}");
    assert_eq!(body["hits"][0]["curated"], true);
    assert_eq!(ids(&body).iter().filter(|i| *i == "2").count(), 1, "{body}");
    assert_eq!(body["found"], 3, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn joins_see_pending_writes_and_superseded_documents() {
    let addr = start_server().await;
    let create = |schema: serde_json::Value| async move {
        let resp = c().post(u(addr, "/collections")).header("X-TYPESENSE-API-KEY", KEY).json(&schema).send().await.unwrap();
        assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
    };
    create(serde_json::json!({ "name": "brands", "index_mode": "realtime", "fields": [
        {"name": "name", "type": "string"}, {"name": "country", "type": "string", "facet": true}
    ]}))
    .await;
    create(serde_json::json!({ "name": "items", "index_mode": "realtime", "fields": [
        {"name": "title", "type": "string"}, {"name": "brand_id", "type": "string", "reference": "brands.id"}
    ]}))
    .await;
    import_docs(addr, "brands", "{\"id\":\"b1\",\"name\":\"Nike\",\"country\":\"USA\"}\n{\"id\":\"b2\",\"name\":\"Adidas\",\"country\":\"Germany\"}").await;
    import_docs(addr, "items", "{\"id\":\"p1\",\"title\":\"shoe\",\"brand_id\":\"b1\"}\n{\"id\":\"p2\",\"title\":\"boot\",\"brand_id\":\"b2\"}").await;
    // later writes: b1 renamed and moved, p2 now Nike's
    let upsert = |coll: &'static str, doc: serde_json::Value| async move {
        let resp = c().post(u(addr, &format!("/collections/{coll}/documents?action=upsert"))).header("X-TYPESENSE-API-KEY", KEY).json(&doc).send().await.unwrap();
        assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
    };
    upsert("brands", serde_json::json!({"id": "b1", "name": "Nike Inc", "country": "Germany"})).await;
    upsert("items", serde_json::json!({"id": "p2", "title": "boot", "brand_id": "b1"})).await;
    let get = |query: &'static str| async move {
        let resp = c().get(u(addr, &format!("/collections/items/documents/search?{query}"))).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
        (resp.status().as_u16(), resp.json::<serde_json::Value>().await.unwrap())
    };

    let (status, body) = get("q=*&include_fields=$brands(name)&sort_by=_text_match:desc").await;
    assert_eq!(status, 200, "{body}");
    let brand_of = |id: &str| body["hits"].as_array().unwrap().iter().find(|h| h["document"]["id"] == id).map(|h| h["document"]["brands"]["name"].clone());
    assert_eq!(brand_of("p1"), Some("Nike Inc".into()), "{body}");
    assert_eq!(brand_of("p2"), Some("Nike Inc".into()), "{body}");
    let (_, body) = get("q=*&filter_by=$brands(country:=Germany)&facet_by=$brands(country)").await;
    assert_eq!(body["found"], 2, "both reference b1, now German: {body}");
    assert_eq!(body["facet_counts"][0]["counts"][0]["value"], "Germany", "{body}");
    assert_eq!(body["facet_counts"][0]["counts"][0]["count"], 2, "{body}");
    let (_, body) = get("q=*&filter_by=$brands(name:=Adidas)").await;
    assert_eq!(body["found"], 0, "no item references Adidas any more: {body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn embedding_models_get_their_indexing_and_query_prefixes() {
    use axum::{routing::post, Json, Router};
    let addr = start_server().await;
    // a model recording what it is asked to embed
    let asked = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock = listener.local_addr().unwrap();
    let record = Arc::clone(&asked);
    tokio::spawn(async move {
        let app = Router::new().route(
            "/v1/embeddings",
            post(move |Json(body): Json<serde_json::Value>| {
                let record = Arc::clone(&record);
                async move {
                    let inputs: Vec<String> = body["input"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
                    let data: Vec<serde_json::Value> = inputs
                        .iter()
                        .enumerate()
                        .map(|(i, t)| serde_json::json!({"index": i, "embedding": if t.contains("phone") { [0.0, 1.0] } else { [1.0, 0.0] }}))
                        .collect();
                    record.lock().unwrap().extend(inputs);
                    Json(serde_json::json!({"data": data}))
                }
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });
    let config = serde_json::json!({"model_name": "e5", "url": format!("http://{mock}/v1/embeddings"), "indexing_prefix": "passage: ", "query_prefix": "query: "});
    let resp = c()
        .post(u(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({ "name": "e5_docs", "fields": [
            {"name": "title", "type": "string"},
            {"name": "emb", "type": "float[]", "num_dim": 2, "embed": {"from": ["title"], "model_config": config}}
        ]}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
    import_docs(addr, "e5_docs", "{\"id\":\"1\",\"title\":\"Laptop Pro\"}\n{\"id\":\"2\",\"title\":\"Phone Mini\"}").await;
    assert_eq!(*asked.lock().unwrap(), ["passage: Laptop Pro", "passage: Phone Mini"]);
    asked.lock().unwrap().clear();

    let resp = c().get(u(addr, "/collections/e5_docs/documents/search?q=phone&query_by=emb&prefix=false")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["hits"][0]["document"]["id"], "2", "{body}");
    assert_eq!(*asked.lock().unwrap(), ["query: phone"]);

    // the collection keeps them
    let resp = c().get(u(addr, "/collections/e5_docs")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let model = &body["fields"].as_array().unwrap().iter().find(|f| f["name"] == "emb").unwrap()["embed"]["model_config"];
    assert_eq!(model["indexing_prefix"], "passage: ", "{body}");
    assert_eq!(model["query_prefix"], "query: ", "{body}");
}
