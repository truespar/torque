use std::net::SocketAddr;
use std::sync::Arc;
use torque::auth::KeyStore;
use torque::collection::alias::AliasRegistry;
use torque::collection::CollectionRegistry;
use torque::config::Config;
use torque::memory::MemoryGuard;
use torque::presets::PresetStore;
use torque::schema::*;
use torque::stopwords::StopwordStore;
use torque::synonyms::SynonymStore;

const BOOTSTRAP_KEY: &str = "test-bootstrap-key-12345";

fn make_config(addr: SocketAddr) -> Config {
    Config {
        api_addr: addr,
        ingest_addr: "127.0.0.1:0".parse().unwrap(),
        api_key: BOOTSTRAP_KEY.into(),
        data_dir: {
            // collections.json is saved there: a create that cannot save it fails
            let dir = std::env::temp_dir().join("torque-auth-test");
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

fn make_schema() -> Schema {
    Schema {
        index_mode: Default::default(),
        name: "products".into(),
        fields: vec![FieldDef {
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
        }],
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

async fn start_server() -> (SocketAddr, Arc<CollectionRegistry>, Arc<KeyStore>) {
    start_server_with_ai(None).await
}

async fn start_server_with_ai(
    ai: Option<torque::ai::AiConfig>,
) -> (SocketAddr, Arc<CollectionRegistry>, Arc<KeyStore>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let mut config = make_config(addr);
    config.ai = ai.map(Arc::new);
    let collections = Arc::new(CollectionRegistry::new(None));
    let aliases = Arc::new(AliasRegistry::new());
    let synonyms = Arc::new(SynonymStore::new());
    let curations = Arc::new(torque::curation::CurationStore::new());
    let analytics = Arc::new(torque::analytics::AnalyticsStore::new());
    let llm = Arc::new(torque::llm::LlmRegistry::new());
    let presets = Arc::new(PresetStore::new());
    let stopwords = Arc::new(StopwordStore::new());
    let keys = Arc::new(KeyStore::new(BOOTSTRAP_KEY.into()));

    let cols = collections.clone();
    let ks = keys.clone();

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

    // Wait for server to be ready
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    (addr, cols, ks)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

/// A chat-completions endpoint that answers one request with `Hello` and hands back the request.
fn stub_provider() -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = socket.read(&mut buf).unwrap();
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        socket
            .write_all(
                concat!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n",
                    "data: [DONE]\n\n",
                )
                .as_bytes(),
            )
            .unwrap();
        tx.send(String::from_utf8_lossy(&request).into_owned()).unwrap();
    });
    (base_url, rx)
}

// -- Tests --

#[tokio::test(flavor = "multi_thread")]
async fn health_no_key_required() {
    let (addr, _, _) = start_server().await;
    let resp = client()
        .get(url(addr, "/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_key_returns_401() {
    let (addr, _, _) = start_server().await;
    let resp = client()
        .get(url(addr, "/collections"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_key_returns_403() {
    let (addr, _, _) = start_server().await;
    let resp = client()
        .get(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", "wrong-key")
        .send()
        .await
        .unwrap();
    // 401 (Unauthorized): credentials present but invalid. Distinguished
    // from 403 (Forbidden) which means "credentials valid but caller lacks
    // permission for this resource". RFC 7235 / 7231 compliant.
    assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn bootstrap_key_full_access() {
    let (addr, _, _) = start_server().await;

    // Can list collections
    let resp = client()
        .get(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Can create collections
    let schema = make_schema();
    let resp = client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&schema)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Can create keys
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "test key",
            "actions": ["documents:search"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn search_only_key_permissions() {
    let (addr, _, _) = start_server().await;

    // Create a collection first
    let schema = make_schema();
    client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&schema)
        .send()
        .await
        .unwrap();

    // Create a search-only key
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "search only",
            "actions": ["documents:search"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let search_key = key_body["value"].as_str().unwrap();

    // Can search
    let resp = client()
        .get(url(addr, "/collections/products/documents/search?q=test&query_by=title"))
        .header("X-TYPESENSE-API-KEY", search_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Cannot create collections
    let resp = client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", search_key)
        .json(&serde_json::json!({
            "name": "other",
            "fields": [{"name": "x", "type": "string"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // Cannot create keys
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", search_key)
        .json(&serde_json::json!({
            "description": "new key",
            "actions": ["*"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn collection_scoped_key() {
    let (addr, _, _) = start_server().await;

    // Create two collections
    let schema1 = make_schema();
    client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&schema1)
        .send()
        .await
        .unwrap();

    let mut schema2 = make_schema();
    schema2.name = "orders".into();
    client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&schema2)
        .send()
        .await
        .unwrap();

    // Create key scoped to "products" only
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "products only",
            "actions": ["documents:search"],
            "collections": ["products"]
        }))
        .send()
        .await
        .unwrap();
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let scoped_key = key_body["value"].as_str().unwrap();

    // Can search products
    let resp = client()
        .get(url(addr, "/collections/products/documents/search?q=test&query_by=title"))
        .header("X-TYPESENSE-API-KEY", scoped_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Cannot search orders
    let resp = client()
        .get(url(addr, "/collections/orders/documents/search?q=test&query_by=title"))
        .header("X-TYPESENSE-API-KEY", scoped_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_key_revokes_access() {
    let (addr, _, _) = start_server().await;

    // Create a key
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "temp key",
            "actions": ["collections:list"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let key_value = key_body["value"].as_str().unwrap().to_string();
    let key_id = key_body["id"].as_u64().unwrap();

    // Key works
    let resp = client()
        .get(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", &key_value)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Delete the key
    let resp = client()
        .delete(url(addr, &format!("/keys/{key_id}")))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Key no longer works
    let resp = client()
        .get(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", &key_value)
        .send()
        .await
        .unwrap();
    // Deleted key → "API key not found" → 401 (auth invalid), not 403.
    assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn key_crud_operations() {
    let (addr, _, _) = start_server().await;

    // Create
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "admin key",
            "actions": ["*"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["value"].is_string());
    let id = body["id"].as_u64().unwrap();

    // Get by ID (returns summary, no full value)
    let resp = client()
        .get(url(addr, &format!("/keys/{id}")))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["description"], "admin key");
    assert!(body.get("value").is_none());

    // List
    let resp = client()
        .get(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["keys"].as_array().unwrap().len() >= 1);

    // Delete
    let resp = client()
        .delete(url(addr, &format!("/keys/{id}")))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Get again — not found
    let resp = client()
        .get(url(addr, &format!("/keys/{id}")))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_via_query_param() {
    let (addr, _, _) = start_server().await;

    // Works via query param
    let resp = client()
        .get(url(
            addr,
            &format!("/collections?x-typesense-api-key={BOOTSTRAP_KEY}"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_key_with_filter() {
    let (addr, _collections, _) = start_server().await;

    // Create collection with an int32 field for filtering
    let schema = Schema {
        index_mode: Default::default(),
        name: "items".into(),
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
                name: "tenant_id".into(),
                field_type: FieldType::Int32,
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

    client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&schema)
        .send()
        .await
        .unwrap();

    // Ingest documents via import
    let docs = vec![
        r#"{"id": "1", "title": "red car", "tenant_id": 1}"#,
        r#"{"id": "2", "title": "blue van", "tenant_id": 1}"#,
        r#"{"id": "3", "title": "green bus", "tenant_id": 2}"#,
        r#"{"id": "4", "title": "fast truck", "tenant_id": 2}"#,
    ];
    client()
        .post(url(addr, "/collections/items/documents/import"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .body(docs.join("\n"))
        .send()
        .await
        .unwrap();

    // Create a parent search key
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "search key",
            "actions": ["documents:search"],
            "collections": ["items"]
        }))
        .send()
        .await
        .unwrap();
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let parent_value = key_body["value"].as_str().unwrap();

    // Generate a scoped key with filter_by
    let scoped_params = torque::auth::scoped::ScopedKeyParams {
        filter_by: Some("tenant_id:=1".into()),
        exclude_fields: None,
        include_fields: None,
        limit_hits: None,
        expires_at: None,
        other: Default::default(),
    };
    let scoped_key = torque::auth::scoped::generate_scoped_key(parent_value, &scoped_params);

    // Search with scoped key — should only find tenant_id=1 docs
    let resp = client()
        .get(url(
            addr,
            "/collections/items/documents/search?q=*&query_by=title",
        ))
        .header("X-TYPESENSE-API-KEY", &scoped_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let found = body["found"].as_u64().unwrap();
    assert_eq!(found, 2, "scoped key should filter to tenant_id=1 only");

    let get = |key: String, query: &'static str| async move {
        let resp = client().get(url(addr, &format!("/collections/items/documents/search?{query}"))).header("X-TYPESENSE-API-KEY", key).send().await.unwrap();
        (resp.status().as_u16(), resp.json::<serde_json::Value>().await.unwrap())
    };
    // the key's filter is ANDed with the search's as a whole (an OR in the search's leaks nothing)
    let (_, body) = get(scoped_key.clone(), "q=*&query_by=title&filter_by=tenant_id:=1%20%7C%7C%20tenant_id:=2").await;
    assert_eq!(body["found"], 2);

    // embedded parameters override the search's; include lists intersect
    let key_with = |other: serde_json::Value, include: Option<&str>| {
        torque::auth::scoped::generate_scoped_key(
            parent_value,
            &torque::auth::scoped::ScopedKeyParams {
                filter_by: Some("tenant_id:=1".into()),
                include_fields: include.map(String::from),
                other: other.as_object().unwrap().clone(),
                ..Default::default()
            },
        )
    };
    let (_, body) = get(key_with(serde_json::json!({"per_page": 1}), None), "q=*&query_by=title&per_page=10").await;
    assert_eq!(body["hits"].as_array().unwrap().len(), 1);
    let (_, body) = get(key_with(serde_json::json!({}), Some("title")), "q=*&query_by=title&include_fields=id,title,tenant_id").await;
    let doc = body["hits"][0]["document"].as_object().unwrap();
    assert_eq!(doc.keys().collect::<Vec<_>>(), ["title"]);

    // multi_search: the key's filter holds there too, and the parent key's collections
    let mut other = schema.clone();
    other.name = "other".into();
    client().post(url(addr, "/collections")).header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY).json(&other).send().await.unwrap();
    let multi = |key: String, body: serde_json::Value| async move {
        let resp = client().post(url(addr, "/multi_search")).header("X-TYPESENSE-API-KEY", key).json(&body).send().await.unwrap();
        (resp.status().as_u16(), resp.json::<serde_json::Value>().await.unwrap())
    };
    let (status, body) = multi(scoped_key.clone(), serde_json::json!({"searches": [{"collection": "items", "q": "*", "query_by": "title"}]})).await;
    assert_eq!(status, 200);
    assert_eq!(body["results"][0]["found"], 2);
    let (status, _) = multi(scoped_key.clone(), serde_json::json!({"searches": [{"collection": "other", "q": "*", "query_by": "title"}]})).await;
    assert_eq!(status, 403);
    let (status, _) = multi(parent_value.to_string(), serde_json::json!({"searches": [{"collection": "other", "q": "*", "query_by": "title"}]})).await;
    assert_eq!(status, 403);

    // a key per search (`x-typesense-api-key` in a search): each search with its own key's
    // embedded parameters, the request's key for the others
    let tenant2 = torque::auth::scoped::generate_scoped_key(parent_value, &torque::auth::scoped::ScopedKeyParams { filter_by: Some("tenant_id:=2".into()), ..Default::default() });
    let search = |key: Option<&str>| {
        let mut s = serde_json::json!({"collection": "items", "q": "*", "query_by": "title"});
        if let Some(k) = key {
            s["x-typesense-api-key"] = k.into();
        }
        s
    };
    let ids = |r: &serde_json::Value| r["hits"].as_array().unwrap().iter().map(|h| h["document"]["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let unkeyed = |body: serde_json::Value| async move {
        let resp = client().post(url(addr, "/multi_search")).json(&body).send().await.unwrap();
        (resp.status().as_u16(), resp.json::<serde_json::Value>().await.unwrap())
    };
    // without a key in the header, every search needs its own
    let (status, body) = unkeyed(serde_json::json!({"searches": [search(Some(&scoped_key)), search(Some(&tenant2))]})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["results"][0]["found"], 2, "{body}");
    assert_eq!(body["results"][1]["found"], 2, "{body}");
    assert!(ids(&body["results"][1]).iter().all(|id| id == "3" || id == "4"), "{body}");
    let (status, body) = unkeyed(serde_json::json!({"searches": [search(Some(&scoped_key)), search(None)]})).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["message"], "No API key provided.");
    // a search's own key over the header's, even the bootstrap key's (the search asked to be restricted)
    let (status, body) = multi(BOOTSTRAP_KEY.into(), serde_json::json!({"searches": [search(Some(&tenant2)), search(None)]})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["results"][0]["found"], 2, "{body}");
    assert_eq!(body["results"][1]["found"], 4, "{body}");
    // a search's key that may not search fails the request
    let (status, body) = multi(BOOTSTRAP_KEY.into(), serde_json::json!({"searches": [search(None), search(Some("no-such-key"))]})).await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["message"], "API key not found.");
    let mut on_other = search(Some(&tenant2));
    on_other["collection"] = "other".into();
    let (status, _) = multi(BOOTSTRAP_KEY.into(), serde_json::json!({"searches": [search(None), on_other]})).await;
    assert_eq!(status, 403);
    // the strictest limit_multi_searches of the keys searching
    let one = key_with(serde_json::json!({"limit_multi_searches": 1}), None);
    let (status, body) = multi(BOOTSTRAP_KEY.into(), serde_json::json!({"searches": [search(Some(&one)), search(None)]})).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["message"], "Number of multi searches exceeds `limit_multi_searches` parameter.");
    let (status, _) = multi(BOOTSTRAP_KEY.into(), serde_json::json!({"searches": [search(Some(&one))]})).await;
    assert_eq!(status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_key_with_limit_hits() {
    let (addr, _, _) = start_server().await;

    // Create collection and docs
    let schema = make_schema();
    client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&schema)
        .send()
        .await
        .unwrap();

    let docs: Vec<String> = (0..10)
        .map(|i| format!(r#"{{"id": "{i}", "title": "item {i}"}}"#))
        .collect();
    client()
        .post(url(addr, "/collections/products/documents/import"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .body(docs.join("\n"))
        .send()
        .await
        .unwrap();

    // Create parent key
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "search key",
            "actions": ["documents:search"],
            "collections": ["products"]
        }))
        .send()
        .await
        .unwrap();
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let parent_value = key_body["value"].as_str().unwrap();

    // Generate scoped key with limit_hits=3
    let scoped_params = torque::auth::scoped::ScopedKeyParams {
        filter_by: None,
        exclude_fields: None,
        include_fields: None,
        limit_hits: Some(3),
        expires_at: None,
        other: Default::default(),
    };
    let scoped_key = torque::auth::scoped::generate_scoped_key(parent_value, &scoped_params);

    let resp = client()
        .get(url(
            addr,
            "/collections/products/documents/search?q=item&query_by=title&per_page=10",
        ))
        .header("X-TYPESENSE-API-KEY", &scoped_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    let found = body["found"].as_u64().unwrap();
    assert!(found <= 3, "limit_hits should cap found to 3");
    let hits = body["hits"].as_array().unwrap();
    assert!(hits.len() <= 3, "limit_hits should cap hits to 3");
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_scoped_key_rejected() {
    let (addr, _, _) = start_server().await;

    // Create parent key
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "search key",
            "actions": ["documents:search"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let parent_value = key_body["value"].as_str().unwrap();

    // Generate scoped key that already expired
    let scoped_params = torque::auth::scoped::ScopedKeyParams {
        filter_by: None,
        exclude_fields: None,
        include_fields: None,
        limit_hits: None,
        expires_at: Some(1), // epoch + 1 second = already expired
        other: Default::default(),
    };
    let scoped_key = torque::auth::scoped::generate_scoped_key(parent_value, &scoped_params);

    // Create collection first
    let schema = make_schema();
    client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&schema)
        .send()
        .await
        .unwrap();

    let resp = client()
        .get(url(
            addr,
            "/collections/products/documents/search?q=test&query_by=title",
        ))
        .header("X-TYPESENSE-API-KEY", &scoped_key)
        .send()
        .await
        .unwrap();
    // Expired scoped key → "Scoped API key has expired" → 401 (auth fail).
    assert_eq!(resp.status(), 401, "expired scoped key should be rejected");
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_key_full_access() {
    let (addr, _, _) = start_server().await;

    // Create admin key
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "admin",
            "actions": ["*"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let admin_key = key_body["value"].as_str().unwrap().to_string();

    // Can list collections
    let resp = client()
        .get(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", &admin_key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Can create collections
    let schema = make_schema();
    let resp = client()
        .post(url(addr, "/collections"))
        .header("X-TYPESENSE-API-KEY", &admin_key)
        .json(&schema)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    // Can create keys
    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", &admin_key)
        .json(&serde_json::json!({
            "description": "sub-key",
            "actions": ["documents:search"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_queries_need_a_key() {
    let (addr, _, _) = start_server().await;
    let resp = client().get(url(addr, "/api/studio/queries")).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    let query = serde_json::json!({ "name": "mine", "query": "{}" });
    let resp = client().post(url(addr, "/api/studio/queries")).json(&query).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client().delete(url(addr, "/api/studio/queries/mine")).send().await.unwrap();
    assert_eq!(resp.status(), 401);
    let resp = client()
        .get(url(addr, "/api/studio/queries"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_search_assistant_is_for_full_access_keys() {
    let (addr, _, _) = start_server().await;
    let chat = serde_json::json!({ "messages": [{ "role": "user", "content": "hi" }] });

    let resp = client().post(url(addr, "/api/ai/chat")).json(&chat).send().await.unwrap();
    assert_eq!(resp.status(), 401);

    let resp = client()
        .post(url(addr, "/keys"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "description": "search only",
            "actions": ["documents:search"],
            "collections": ["*"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let key_body: serde_json::Value = resp.json().await.unwrap();
    let search_key = key_body["value"].as_str().unwrap();
    let resp = client()
        .post(url(addr, "/api/ai/chat"))
        .header("X-TYPESENSE-API-KEY", search_key)
        .json(&chat)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // Through the key check, and no model on this server.
    let resp = client()
        .post(url(addr, "/api/ai/chat"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&chat)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("ai_provider"));

    let health: serde_json::Value = client().get(url(addr, "/health")).send().await.unwrap().json().await.unwrap();
    assert_eq!(health["features"]["ai"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_search_assistant_answers_from_the_operators_model() {
    let (base_url, request) = stub_provider();
    let ai = torque::ai::AiConfig::resolve(torque::ai::AiSettings {
        provider: Some("openai".into()),
        api_key: Some("operator-secret".into()),
        model: Some("local-model".into()),
        base_url: Some(base_url),
        ..Default::default()
    })
    .unwrap();
    let (addr, _, _) = start_server_with_ai(ai).await;

    let health: serde_json::Value = client().get(url(addr, "/health")).send().await.unwrap().json().await.unwrap();
    assert_eq!(health["features"]["ai"], true);

    let resp = client()
        .post(url(addr, "/api/ai/chat"))
        .header("X-TYPESENSE-API-KEY", BOOTSTRAP_KEY)
        .json(&serde_json::json!({
            "messages": [{ "role": "user", "content": "find books" }],
            "metadata": { "collectionSchema": "Collection: books" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let body = resp.text().await.unwrap();
    assert_eq!(body, "data: {\"text\":\"Hello\",\"type\":\"text\"}\n\ndata: [DONE]\n\n");
    assert!(!body.contains("operator-secret"));

    let request = request.recv().unwrap();
    assert!(request.starts_with("POST /chat/completions HTTP/1.1"));
    assert!(request.to_ascii_lowercase().contains("authorization: bearer operator-secret"));
    let sent: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(sent["model"], "local-model");
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["messages"][0]["role"], "system");
    assert!(sent["messages"][0]["content"].as_str().unwrap().contains("Collection: books"));
    assert_eq!(sent["messages"][1]["content"], "find books");
}
