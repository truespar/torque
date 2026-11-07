//! Document writes as Typesense answers them, over collections of several segments and pending
//! (realtime) writes: the `id` filter, `_seq_id` (a written document keeps its place), the segments in memory
//! flushing into one from every write path, no write lost to another, and batch
//! collections rebuilt from every segment.

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
            let dir = std::env::temp_dir().join(format!("torque-writes-test-{}", std::process::id()));
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

/// A server, and its collections for looking at segments.
async fn start_server() -> (SocketAddr, Arc<CollectionRegistry>) {
    start_server_in(None).await
}

/// A server persisting to `data_dir` (realtime collections write through their write-ahead log),
/// or keeping everything in memory.
async fn start_server_in(data_dir: Option<std::path::PathBuf>) -> (SocketAddr, Arc<CollectionRegistry>) {
    let collections = Arc::new(CollectionRegistry::new(data_dir.clone()));
    (start_server_over(Arc::clone(&collections), data_dir).await, collections)
}

/// A server over the collections of `collections`, as one that has loaded them.
async fn start_server_over(collections: Arc<CollectionRegistry>, data_dir: Option<std::path::PathBuf>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = make_config(addr);
    if let Some(dir) = &data_dir {
        config.data_dir = dir.clone();
    }
    tokio::spawn(async move {
        use axum::middleware;
        use axum::Router;
        use torque::auth::middleware::auth_middleware;
        use torque::server::{routes, AppState};

        let metrics = Arc::new(torque::observability::MetricsCollector::new());
        let memory_guard = MemoryGuard::new(u64::MAX, metrics.memory_resident.clone());
        let state = Arc::new(AppState {
            collections,
            aliases: Arc::new(AliasRegistry::new()),
            synonyms: Arc::new(SynonymStore::new()),
            curations: Arc::new(torque::curation::CurationStore::new()),
            analytics: Arc::new(torque::analytics::AnalyticsStore::new()),
            llm: Arc::new(torque::llm::LlmRegistry::new()),
            presets: Arc::new(PresetStore::new()),
            stopwords: Arc::new(StopwordStore::new()),
            keys: Arc::new(KeyStore::new(KEY.into())),
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
            .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
            .with_state(state);
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
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

/// `path?query`, the query's values percent-encoded.
fn with_query(path: &str, query: &[(&str, &str)]) -> String {
    let enc = |v: &str| v.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    let q: Vec<String> = query.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect();
    format!("{path}?{}", q.join("&"))
}

async fn create(addr: SocketAddr, schema: serde_json::Value) {
    let resp = c().post(u(addr, "/collections")).header("X-TYPESENSE-API-KEY", KEY).json(&schema).send().await.unwrap();
    assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
}

/// Import JSON lines with `action`; the per-line results.
async fn import(addr: SocketAddr, coll: &str, action: &str, lines: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let body = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
    let resp = c()
        .post(u(addr, &format!("/collections/{coll}/documents/import?action={action}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .header("Content-Type", "text/plain")
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "import: {}", resp.status());
    resp.text().await.unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

async fn write(addr: SocketAddr, coll: &str, action: &str, doc: serde_json::Value) -> u16 {
    let resp = c()
        .post(u(addr, &format!("/collections/{coll}/documents?action={action}")))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&doc)
        .send()
        .await
        .unwrap();
    resp.status().as_u16()
}

async fn delete(addr: SocketAddr, coll: &str, query: &[(&str, &str)]) -> (u16, serde_json::Value) {
    let resp = c().delete(u(addr, &with_query(&format!("/collections/{coll}/documents"), query))).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

async fn get(addr: SocketAddr, path: &str) -> (u16, serde_json::Value) {
    let resp = c().get(u(addr, path)).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

async fn search(addr: SocketAddr, coll: &str, params: &[(&str, &str)]) -> (u16, serde_json::Value) {
    let resp = c()
        .get(u(addr, &with_query(&format!("/collections/{coll}/documents/search"), params)))
        .header("X-TYPESENSE-API-KEY", KEY)
        .send()
        .await
        .unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap())
}

fn ids(body: &serde_json::Value) -> Vec<String> {
    body["hits"].as_array().unwrap().iter().map(|h| h["document"]["id"].as_str().unwrap().to_string()).collect()
}

/// Every document in `_seq_id` order.
async fn in_seq_order(addr: SocketAddr, coll: &str) -> Vec<String> {
    let (status, body) = search(addr, coll, &[("q", "*"), ("sort_by", "_seq_id:asc"), ("per_page", "250")]).await;
    assert_eq!(status, 200, "{body}");
    ids(&body)
}

/// Flush a realtime collection's segments in memory into one.
fn flush(registry: &CollectionRegistry, coll: &str) {
    registry.get(coll).unwrap().compact();
}

fn segments(registry: &CollectionRegistry, coll: &str) -> usize {
    registry.get(coll).unwrap().snapshot_bundle().segment_count()
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn id_filter_finds_documents_in_every_segment_and_the_pending_writes() {
    let (addr, registry) = start_server().await;
    create(addr, serde_json::json!({"name": "brands", "index_mode": "realtime", "fields": [{"name": "name", "type": "string"}]})).await;
    create(addr, serde_json::json!({"name": "rt", "index_mode": "realtime", "fields": [
        {"name": "title", "type": "string"}, {"name": "n", "type": "int32", "sort": true},
        {"name": "brand_id", "type": "string", "reference": "brands.id"}
    ]}))
    .await;
    import(addr, "brands", "create", &[serde_json::json!({"id": "b1", "name": "Nike"}), serde_json::json!({"id": "b2", "name": "Adidas"})]).await;
    let doc = |i: u32| serde_json::json!({"id": format!("a{i}"), "title": format!("item {i}"), "n": i, "brand_id": if i % 2 == 0 { "b1" } else { "b2" }});
    import(addr, "rt", "create", &(0..10).map(doc).collect::<Vec<_>>()).await;
    flush(&registry, "rt");
    import(addr, "rt", "create", &(10..15).map(doc).collect::<Vec<_>>()).await;
    flush(&registry, "rt");
    assert_eq!(segments(&registry, "rt"), 2);
    // pending: a3 written again, a4 deleted, a20 new
    assert_eq!(write(addr, "rt", "upsert", serde_json::json!({"id": "a3", "title": "changed", "n": 3, "brand_id": "b2"})).await, 201);
    assert_eq!(c().delete(u(addr, "/collections/rt/documents/a4")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap().status(), 200);
    assert_eq!(write(addr, "rt", "create", serde_json::json!({"id": "a20", "title": "item 20", "n": 20, "brand_id": "b1"})).await, 201);

    let found = |body: &serde_json::Value| body["found"].as_u64().unwrap();
    let (status, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:=a3")]).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(found(&body), 1, "{body}");
    assert_eq!(body["hits"][0]["document"]["title"], "changed", "the pending version: {body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:[a1, a4, a12, a20, zz]"), ("sort_by", "n:asc")]).await;
    assert_eq!(ids(&body), names(&["a1", "a12", "a20"]), "a deleted id and an unknown one match nothing: {body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id: a5")]).await;
    assert_eq!(ids(&body), names(&["a5"]));
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:!=[a0, a1]")]).await;
    assert_eq!(found(&body), 13, "15 live documents less two: {body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:!a0")]).await;
    assert_eq!(found(&body), 14, "{body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:*")]).await;
    assert_eq!(found(&body), 15, "{body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:!=*")]).await;
    assert_eq!(found(&body), 0, "{body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:[a1, a2] && n:>1")]).await;
    assert_eq!(ids(&body), names(&["a2"]), "{body}");
    // a string field without facet: by its tokens, in every segment
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "title:=changed")]).await;
    assert_eq!(ids(&body), names(&["a3"]), "{body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "title:=[item 3, Item 12, ITEM 20]"), ("sort_by", "n:asc")]).await;
    assert_eq!(ids(&body), names(&["a12", "a20"]), "a3's older copy is gone: {body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "title:item")]).await;
    assert_eq!(found(&body), 14, "{body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "title:!=changed && title:=item 1*"), ("sort_by", "n:asc")]).await;
    assert_eq!(ids(&body), names(&["a1", "a10", "a11", "a12", "a13", "a14"]), "{body}");
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "$brands(name:=nike)")]).await;
    assert_eq!(found(&body), 8, "{body}");
    // the referenced collection's ids, in a JOIN: the even items, less deleted a4, with new a20
    let (status, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "$brands(id:=b1)")]).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(found(&body), 8, "{body}");

    // Typesense's errors
    let (status, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:=")]).await;
    assert_eq!((status, body["message"].as_str().unwrap()), (400, "Error with filter field `id`: Filter value cannot be empty."));
    let (status, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:>a1")]).await;
    assert_eq!((status, body["message"].as_str().unwrap()), (400, "Error with filter field `id`: `>` is not supported, only `=` and `!=`."));
    let (status, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "nope:=x")]).await;
    assert_eq!((status, body["message"].as_str().unwrap()), (404, "Could not find a filter field named `nope` in the schema."));
    let (status, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "title:='x'||sleep(5)||''")]).await;
    assert_eq!((status, body["message"].as_str().unwrap()), (400, "Could not parse the filter query."));

    // deleting by ids, one of them pending
    let (status, body) = delete(addr, "rt", &[("filter_by", "id:[a1, a2, a20]")]).await;
    assert_eq!((status, body["num_deleted"].as_u64()), (200, Some(3)), "{body}");
    let (status, body) = get(addr, "/collections/rt/documents/a1").await;
    assert_eq!((status, body["message"].as_str().unwrap()), (404, "Could not find a document with id: a1"));
    let (_, body) = search(addr, "rt", &[("q", "*"), ("filter_by", "id:*")]).await;
    assert_eq!(found(&body), 12, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn seq_id_keeps_a_written_document_in_its_place() {
    let (addr, registry) = start_server().await;
    create(addr, serde_json::json!({"name": "seq", "index_mode": "realtime", "fields": [
        {"name": "title", "type": "string"}, {"name": "n", "type": "int32", "sort": true}
    ]}))
    .await;
    let doc = |id: &str| serde_json::json!({"id": id, "title": format!("title {id}"), "n": 1});
    import(addr, "seq", "create", &["d0", "d1", "d2", "d3", "d4", "d5"].map(doc)).await;
    flush(&registry, "seq");
    import(addr, "seq", "create", &["d6", "d7"].map(doc)).await;
    flush(&registry, "seq");
    let all = names(&["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"]);
    assert_eq!(in_seq_order(addr, "seq").await, all);

    // written again (pending, then flushed into a later segment): in its place
    assert_eq!(write(addr, "seq", "upsert", serde_json::json!({"id": "d1", "title": "d1 again", "n": 1})).await, 201);
    assert_eq!(in_seq_order(addr, "seq").await, all);
    flush(&registry, "seq");
    assert_eq!(segments(&registry, "seq"), 3);
    assert_eq!(in_seq_order(addr, "seq").await, all);
    let (_, body) = search(addr, "seq", &[("q", "*"), ("sort_by", "_seq_id:desc"), ("per_page", "250")]).await;
    let mut desc = all.clone();
    desc.reverse();
    assert_eq!(ids(&body), desc);
    // the default order of a wildcard query, and ties of a sort: the latest first
    let (_, body) = search(addr, "seq", &[("q", "*"), ("per_page", "250")]).await;
    assert_eq!(ids(&body), desc, "Typesense's default order: {body}");
    let (_, body) = search(addr, "seq", &[("q", "*"), ("sort_by", "n:asc"), ("per_page", "250")]).await;
    assert_eq!(ids(&body), desc, "ties by _seq_id, the latest first: {body}");
    // paged
    let (_, body) = search(addr, "seq", &[("q", "*"), ("sort_by", "_seq_id:asc"), ("per_page", "3"), ("page", "2")]).await;
    assert_eq!(ids(&body), names(&["d3", "d4", "d5"]));

    // deleted and created again: a new document, last
    assert_eq!(c().delete(u(addr, "/collections/seq/documents/d2")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap().status(), 200);
    assert_eq!(write(addr, "seq", "create", doc("d2")).await, 201);
    let moved = names(&["d0", "d1", "d3", "d4", "d5", "d6", "d7", "d2"]);
    assert_eq!(in_seq_order(addr, "seq").await, moved);
    flush(&registry, "seq");
    assert_eq!(in_seq_order(addr, "seq").await, moved);

    // a deletion flushed alone, then a tier's worth of flushed segments for a merge: the merge keeps
    // the live copies in their places (the written-again d1, not its first version; no deleted d5)
    assert_eq!(c().delete(u(addr, "/collections/seq/documents/d5")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap().status(), 200);
    flush(&registry, "seq");
    for id in ["e0", "e1", "e2", "e3", "e4", "e5"] {
        assert_eq!(write(addr, "seq", "create", doc(id)).await, 201);
        flush(&registry, "seq");
    }
    let before = segments(&registry, "seq");
    assert!(before >= torque::collection::write::MERGE_FACTOR, "{before} segments");
    assert!(registry.get("seq").unwrap().merge_flushed() > 0, "the merge ran");
    assert!(segments(&registry, "seq") < before);
    assert_eq!(in_seq_order(addr, "seq").await, names(&["d0", "d1", "d3", "d4", "d6", "d7", "d2", "e0", "e1", "e2", "e3", "e4", "e5"]));
    let (_, d1) = get(addr, "/collections/seq/documents/d1").await;
    assert_eq!(d1["title"], "d1 again", "{d1}");
    let (status, _) = get(addr, "/collections/seq/documents/d5").await;
    assert_eq!(status, 404, "a deleted document stays deleted");

    // a batch collection rebuilt by a write keeps the written document in its place too
    create(addr, serde_json::json!({"name": "bseq", "fields": [{"name": "title", "type": "string"}, {"name": "n", "type": "int32", "sort": true}]})).await;
    import(addr, "bseq", "create", &["p0", "p1", "p2", "p3"].map(doc)).await;
    assert_eq!(write(addr, "bseq", "upsert", serde_json::json!({"id": "p1", "title": "p1 again", "n": 1})).await, 201);
    assert_eq!(in_seq_order(addr, "bseq").await, names(&["p0", "p1", "p2", "p3"]));
    assert_eq!(c().delete(u(addr, "/collections/bseq/documents/p2")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap().status(), 200);
    assert_eq!(write(addr, "bseq", "create", doc("p2")).await, 201);
    assert_eq!(in_seq_order(addr, "bseq").await, names(&["p0", "p1", "p3", "p2"]));

    // a union of searches sorting by _seq_id
    let resp = c()
        .post(u(addr, "/multi_search?per_page=4"))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"union": true, "searches": [
            {"collection": "seq", "q": "*", "sort_by": "_seq_id:desc", "filter_by": "id:[e3, e2]"},
            {"collection": "bseq", "q": "*", "sort_by": "_seq_id:desc"}
        ]}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["found"], 6, "{body}");
    assert_eq!(body["hits"].as_array().unwrap().len(), 4, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_flush_once_the_segments_in_memory_hold_enough() {
    let (addr, registry) = start_server().await;
    for name in ["big", "bulk"] {
        create(addr, serde_json::json!({"name": name, "index_mode": "realtime", "fields": [{"name": "title", "type": "string"}, {"name": "n", "type": "int32"}]})).await;
    }
    let doc = |i: usize| serde_json::json!({"id": format!("x{i}"), "title": format!("title {i}"), "n": i});
    let threshold = torque::collection::FLUSH_DOCS as usize;
    // one document fewer than the threshold: in memory, searchable at once
    import(addr, "big", "create", &(0..threshold - 1).map(doc).collect::<Vec<_>>()).await;
    let col = registry.get("big").unwrap();
    assert_eq!(col.snapshot_bundle().memory_docs() as usize, threshold - 1);
    assert_eq!(col.snapshot_bundle().total_docs() as usize, threshold - 1);
    let (_, body) = search(addr, "big", &[("q", "*")]).await;
    assert_eq!(body["found"].as_u64(), Some(threshold as u64 - 1), "{body}");

    // one more single-document write: the segments in memory flush
    assert_eq!(write(addr, "big", "create", doc(threshold)).await, 201);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while col.snapshot_bundle().memory > 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(col.snapshot_bundle().memory, 0, "flushed");
    assert_eq!(col.snapshot_bundle().total_docs() as usize, threshold);
    let (_, body) = search(addr, "big", &[("q", "*")]).await;
    assert_eq!(body["found"].as_u64(), Some(threshold as u64), "{body}");

    // one import above the threshold flushes too
    import(addr, "bulk", "create", &(0..threshold + 500).map(doc).collect::<Vec<_>>()).await;
    let bulk = registry.get("bulk").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while bulk.snapshot_bundle().memory > 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(bulk.snapshot_bundle().memory, 0);
    assert_eq!(bulk.snapshot_bundle().total_docs() as usize, threshold + 500);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_writes_are_all_kept() {
    let (addr, registry) = start_server().await;
    create(addr, serde_json::json!({"name": "conc", "index_mode": "realtime", "fields": [{"name": "title", "type": "string"}]})).await;
    let tasks: Vec<_> = (0..8)
        .map(|t| {
            tokio::spawn(async move {
                for i in 0..40 {
                    let status = write(addr, "conc", "create", serde_json::json!({"id": format!("t{t}_{i}"), "title": "x"})).await;
                    assert_eq!(status, 201);
                }
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(registry.get("conc").unwrap().snapshot_bundle().total_docs(), 320, "no write lost to another");
    let (_, body) = search(addr, "conc", &[("q", "*")]).await;
    assert_eq!(body["found"], 320, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn filter_writes_and_imports_see_every_document() {
    let (addr, registry) = start_server().await;
    create(addr, serde_json::json!({"name": "fw", "index_mode": "realtime", "fields": [{"name": "title", "type": "string"}, {"name": "n", "type": "int32"}]})).await;
    let doc = |i: u32| serde_json::json!({"id": format!("x{i}"), "title": format!("title {i}"), "n": i});
    import(addr, "fw", "create", &(0..10).map(doc).collect::<Vec<_>>()).await;
    flush(&registry, "fw");
    // pending: x1 written again with n 100, x10 new with n 100
    assert_eq!(write(addr, "fw", "upsert", serde_json::json!({"id": "x1", "title": "title 1", "n": 100})).await, 201);
    assert_eq!(write(addr, "fw", "create", serde_json::json!({"id": "x10", "title": "title 10", "n": 100})).await, 201);

    let resp = c()
        .patch(u(addr, &with_query("/collections/fw/documents", &[("filter_by", "n:>=100")])))
        .header("X-TYPESENSE-API-KEY", KEY)
        .json(&serde_json::json!({"title": "patched"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["num_updated"], 2, "the pending versions match: {body}");
    let (_, x1) = get(addr, "/collections/fw/documents/x1").await;
    assert_eq!((x1["title"].as_str(), x1["n"].as_i64()), (Some("patched"), Some(100)), "{x1}");
    let (_, body) = delete(addr, "fw", &[("filter_by", "n:>=100")]).await;
    assert_eq!(body["num_deleted"], 2, "{body}");
    let (_, body) = delete(addr, "fw", &[("truncate", "true")]).await;
    assert_eq!(body["num_deleted"], 9, "{body}");
    let (_, body) = search(addr, "fw", &[("q", "*")]).await;
    assert_eq!(body["found"], 0, "{body}");
    assert_eq!(write(addr, "fw", "create", doc(0)).await, 201);
    let (_, body) = search(addr, "fw", &[("q", "*")]).await;
    assert_eq!(body["found"], 1, "{body}");

    // imports: update and emplace change only the fields given; create refuses an id written
    // earlier in the same import; Typesense's per-line errors
    create(addr, serde_json::json!({"name": "imp", "fields": [{"name": "title", "type": "string"}, {"name": "n", "type": "int32"}]})).await;
    import(addr, "imp", "create", &[serde_json::json!({"id": "i1", "title": "first", "n": 1})]).await;
    let res = import(addr, "imp", "update", &[serde_json::json!({"id": "i1", "n": 5}), serde_json::json!({"id": "zz", "n": 1}), serde_json::json!({"n": 1})]).await;
    assert_eq!(res[0], serde_json::json!({"success": true}));
    assert_eq!(res[1], serde_json::json!({"code": 404, "error": "Could not find a document with id: zz", "success": false}));
    assert_eq!(res[2], serde_json::json!({"code": 400, "error": "For update, the `id` key must be provided.", "success": false}));
    let (_, i1) = get(addr, "/collections/imp/documents/i1").await;
    assert_eq!((i1["title"].as_str(), i1["n"].as_i64()), (Some("first"), Some(5)), "{i1}");
    let res = import(addr, "imp", "create", &[serde_json::json!({"id": "i2", "title": "a", "n": 1}), serde_json::json!({"id": "i2", "title": "b", "n": 2})]).await;
    assert_eq!(res[1], serde_json::json!({"code": 409, "error": "A document with id i2 already exists.", "success": false}));
    let res = import(addr, "imp", "emplace", &[serde_json::json!({"id": "i2", "n": 7}), serde_json::json!({"id": "i3", "title": "c", "n": 3})]).await;
    assert!(res.iter().all(|r| r["success"] == true), "{res:?}");
    let (_, i2) = get(addr, "/collections/imp/documents/i2").await;
    assert_eq!((i2["title"].as_str(), i2["n"].as_i64()), (Some("a"), Some(7)), "{i2}");
    let (status, body) = c().post(u(addr, "/collections/imp/documents")).header("X-TYPESENSE-API-KEY", KEY).json(&serde_json::json!({"id": "i1", "title": "x", "n": 0})).send().await.map(|r| (r.status().as_u16(), r)).unwrap();
    let body: serde_json::Value = body.json().await.unwrap();
    assert_eq!((status, body["message"].as_str().unwrap()), (409, "A document with id i1 already exists."));
}

// -- a batch collection of several segments --------------------------------------------------

/// Realtime writes over HTTP are answered once their write-ahead log has them on disk: a restart
/// from the data directory with nothing saved finds every acknowledged one, deletes included.
#[tokio::test(flavor = "multi_thread")]
async fn acknowledged_writes_are_on_disk_before_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, _registry) = start_server_in(Some(dir.path().to_path_buf())).await;
    create(addr, serde_json::json!({"name": "durable", "index_mode": "realtime", "fields": [{"name": "title", "type": "string"}]})).await;
    let writers: Vec<_> = (0..8)
        .map(|t| {
            tokio::spawn(async move {
                for n in 0..25 {
                    let status = write(addr, "durable", "upsert", serde_json::json!({"id": format!("{t}-{n}"), "title": format!("w{t}")})).await;
                    assert!((200..300).contains(&status), "write {t}-{n}: {status}");
                }
            })
        })
        .collect();
    for w in writers {
        w.await.unwrap();
    }
    let (status, _) = delete(addr, "durable", &[("filter_by", "id:=[0-0,7-24]")]).await;
    assert_eq!(status, 200);

    let restarted = CollectionRegistry::new(Some(dir.path().to_path_buf()));
    torque::persistence::load_collections(dir.path(), &restarted, None).unwrap();
    let back = restarted.get("durable").unwrap();
    assert_eq!(back.view().total_docs(), 198);
    assert!(!back.contains("0-0") && !back.contains("7-24") && back.contains("3-12"));
    back.close_wal();
}

/// After an upgrade that refuses a collection's files: it is set aside and searches answer 404, and
/// a client's drop and make again goes through. A create alone is a 409 (its files are kept), and a
/// drop removes them unread, also when a nightly rebuild's upload and alias have replaced it.
#[tokio::test(flavor = "multi_thread")]
async fn a_collection_set_aside_at_the_start_is_dropped_and_made_again() {
    let dir = tempfile::tempdir().unwrap();
    let fields = serde_json::json!([{"name": "title", "type": "string"}]);
    let (first, first_registry) = start_server_in(Some(dir.path().to_path_buf())).await;
    create(first, serde_json::json!({"name": "live", "index_mode": "realtime", "fields": fields})).await;
    create(first, serde_json::json!({"name": "nightly", "index_mode": "batch", "fields": fields})).await;
    first_registry.get("live").unwrap().close_wal();
    for name in ["live", "nightly"] {
        let segments = dir.path().join("indexes").join(name).join("segments");
        std::fs::create_dir_all(&segments).unwrap();
        std::fs::write(segments.join("manifest.bin"), b"TRQX and no version this build reads").unwrap();
    }

    let restarted = Arc::new(CollectionRegistry::new(Some(dir.path().to_path_buf())));
    torque::persistence::load_collections(dir.path(), &restarted, None).unwrap();
    assert_eq!(restarted.unloaded().len(), 2, "both set aside");
    let addr = start_server_over(Arc::clone(&restarted), Some(dir.path().to_path_buf())).await;

    let send = |method: reqwest::Method, path: String| c().request(method, u(addr, &path)).header("X-TYPESENSE-API-KEY", KEY).send();
    let status = send(reqwest::Method::GET, "/collections/live/documents/search?q=a&query_by=title".into()).await.unwrap().status();
    assert_eq!(status, 404, "a search answers as for no collection");
    let made = c().post(u(addr, "/collections")).header("X-TYPESENSE-API-KEY", KEY);
    let again = serde_json::json!({"name": "live", "index_mode": "realtime", "fields": fields});
    assert_eq!(made.json(&again).send().await.unwrap().status(), 409, "its name is held");
    assert!(dir.path().join("indexes").join("live").exists(), "its files kept");

    for (name, realtime) in [("live", true), ("nightly", false)] {
        let resp = send(reqwest::Method::DELETE, format!("/collections/{name}")).await.unwrap();
        assert_eq!(resp.status(), 200, "{name}");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["name"], name);
        assert_eq!(body["index_mode"].is_string(), realtime);
        assert!(!dir.path().join("indexes").join(name).exists(), "{name}: its segment set");
        assert!(!dir.path().join("wal").join(name).exists(), "{name}: its log");
        assert_eq!(send(reqwest::Method::DELETE, format!("/collections/{name}")).await.unwrap().status(), 404);
    }
    assert!(restarted.unloaded().is_empty());

    create(addr, again).await;
    assert_eq!(write(addr, "live", "upsert", serde_json::json!({"id": "1", "title": "after"})).await, 201);
    restarted.get("live").unwrap().close_wal();

    let again = CollectionRegistry::new(Some(dir.path().to_path_buf()));
    torque::persistence::load_collections(dir.path(), &again, None).unwrap();
    assert!(again.unloaded().is_empty());
    assert!(again.get("live").unwrap().contains("1"));
    assert!(again.get("nightly").is_none());
    again.get("live").unwrap().close_wal();
}

fn products_schema() -> serde_json::Value {
    serde_json::json!({"name": "products", "fields": [
        {"name": "title", "type": "string"}, {"name": "price", "type": "float", "facet": true, "sort": true},
        {"name": "brand", "type": "string", "facet": true}, {"name": "in_stock", "type": "bool", "facet": true},
        {"name": "rating", "type": "int32", "sort": true}
    ]})
}

fn product(i: u32) -> serde_json::Value {
    serde_json::json!({"id": format!("p{i}"), "title": format!("Widget {i}"), "price": f64::from(i) + 0.5,
        "brand": if i % 2 == 0 { "Acme" } else { "Globex" }, "in_stock": i % 3 != 0, "rating": i})
}

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

#[tokio::test(flavor = "multi_thread")]
async fn writes_to_a_batch_collection_of_several_segments_keep_them_all() {
    let (addr, registry) = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let docs: Vec<serde_json::Value> = (0..40).map(product).collect();
    let opts = torque::ingest::segment_file::BuildOptions { docs_level: 3 };
    let mut files = Vec::new();
    for (i, half) in [&docs[..25], &docs[25..]].into_iter().enumerate() {
        let input = dir.path().join(format!("half{i}.tqbf"));
        docs_only_file(&input, half);
        let report = torque::ingest::segment_file::build(&input, &dir.path().join(format!("built{i}")), &opts).unwrap();
        files.push(report.segments[0].0.clone());
    }
    for (part, file) in files.iter().enumerate().rev() {
        let resp = c()
            .put(u(addr, &format!("/collections/multi/segments?part={part}&parts=2")))
            .header("X-TYPESENSE-API-KEY", KEY)
            .header("Content-Type", "application/octet-stream")
            .body(std::fs::read(file).unwrap())
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
    }
    assert_eq!(segments(&registry, "multi"), 2);

    // a document of the second segment exists: create refuses it, update finds it
    let mut p31 = product(31);
    assert_eq!(write(addr, "multi", "create", p31.clone()).await, 409);
    p31["title"] = "Widget 31 again".into();
    assert_eq!(write(addr, "multi", "update", p31).await, 201);
    assert_eq!(write(addr, "multi", "create", product(100)).await, 201);
    let (_, body) = search(addr, "multi", &[("q", "*")]).await;
    assert_eq!(body["found"], 41, "every segment's documents kept: {body}");
    let (_, p31) = get(addr, "/collections/multi/documents/p31").await;
    assert_eq!(p31["title"], "Widget 31 again", "{p31}");
    let order = in_seq_order(addr, "multi").await;
    assert_eq!(order[31], "p31", "written again, in its place");
    assert_eq!(order[40], "p100");
    assert_eq!(c().delete(u(addr, "/collections/multi/documents/p30")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap().status(), 200);
    let (_, body) = search(addr, "multi", &[("q", "*")]).await;
    assert_eq!(body["found"], 40, "{body}");
}

/// Typesense's answers to writes: an import creates by default and names an action it does not
/// know; a DELETE answers with the document deleted.
#[tokio::test(flavor = "multi_thread")]
async fn imports_create_by_default_and_a_delete_answers_with_the_document() {
    let (addr, _registry) = start_server().await;
    create(addr, serde_json::json!({"name": "td", "fields": [{"name": "title", "type": "string"}]})).await;
    let import_raw = |query: &'static str, body: &'static str| async move {
        let resp = c()
            .post(u(addr, &format!("/collections/td/documents/import{query}")))
            .header("X-TYPESENSE-API-KEY", KEY)
            .body(body)
            .send()
            .await
            .unwrap();
        (resp.status().as_u16(), resp.text().await.unwrap())
    };
    let (status, body) = import_raw("", "{\"id\": \"a\", \"title\": \"one\", \"extra\": 1}").await;
    assert_eq!((status, body.as_str()), (200, "{\"success\": true}"));
    // no action is create: an existing id is refused, as Typesense refuses it
    let (_, body) = import_raw("", "{\"id\": \"a\", \"title\": \"two\"}").await;
    let line: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!((line["success"].as_bool(), line["code"].as_u64()), (Some(false), Some(409)), "{body}");
    let (status, body) = import_raw("?action=replace", "{\"id\": \"b\", \"title\": \"x\"}").await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["message"], "Parameter `action` must be a create|update|upsert.");

    // DELETE answers with the whole document, a field outside the schema too
    let resp = c().delete(u(addr, "/collections/td/documents/a")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let doc: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(doc, serde_json::json!({"id": "a", "title": "one", "extra": 1}));
    let resp = c().delete(u(addr, "/collections/td/documents/a")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    let resp = c().delete(u(addr, "/collections/td/documents/a?ignore_not_found=true")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<serde_json::Value>().await.unwrap(), serde_json::json!({"id": "a"}));
}

/// A schema change keeps every live document (each segment's) in its place,
/// typed against the new schema; one the documents refuse changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_schema_change_keeps_every_segment_and_the_pending_writes() {
    let (addr, registry) = start_server().await;
    create(addr, serde_json::json!({"name": "sc", "index_mode": "realtime", "fields": [
        {"name": "title", "type": "string"}, {"name": "tag", "type": "string"}, {"name": "n", "type": "int32", "sort": true}
    ]}))
    .await;
    let doc = |i: u32| serde_json::json!({"id": format!("a{i}"), "title": format!("item {i}"), "tag": "t", "n": i, "color": if i % 2 == 0 { "red" } else { "blue" }});
    import(addr, "sc", "create", &(0..10).map(doc).collect::<Vec<_>>()).await;
    flush(&registry, "sc");
    import(addr, "sc", "create", &(10..15).map(doc).collect::<Vec<_>>()).await;
    flush(&registry, "sc");
    assert_eq!(write(addr, "sc", "upsert", serde_json::json!({"id": "a3", "title": "changed", "tag": "t", "n": 3, "color": "red"})).await, 201);
    assert_eq!(c().delete(u(addr, "/collections/sc/documents/a4")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap().status(), 200);
    assert_eq!(write(addr, "sc", "create", doc(20)).await, 201);
    let before = in_seq_order(addr, "sc").await;
    assert_eq!(before.len(), 15);
    // an export: the live documents as written, in `_seq_id` order
    let resp = c().get(u(addr, "/collections/sc/documents/export")).header("X-TYPESENSE-API-KEY", KEY).send().await.unwrap();
    let exported: Vec<serde_json::Value> = resp.text().await.unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(exported.iter().map(|d| d["id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), before);
    let a3 = exported.iter().find(|d| d["id"] == "a3").unwrap();
    assert_eq!((a3["title"].as_str(), a3["color"].as_str()), (Some("changed"), Some("red")), "{a3}");

    let patch = |body: serde_json::Value| async move {
        let resp = c().patch(u(addr, "/collections/sc")).header("X-TYPESENSE-API-KEY", KEY).json(&body).send().await.unwrap();
        (resp.status().as_u16(), resp.json::<serde_json::Value>().await.unwrap())
    };
    // the field between the others dropped, a stored one indexed as a facet
    let (status, body) = patch(serde_json::json!({"fields": [
        {"name": "tag", "type": "string", "drop": true}, {"name": "color", "type": "string", "facet": true, "optional": true}
    ]}))
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(in_seq_order(addr, "sc").await, before, "every document, in its place");
    let (_, a3) = get(addr, "/collections/sc/documents/a3").await;
    assert_eq!((a3["title"].as_str(), a3["n"].as_i64()), (Some("changed"), Some(3)), "{a3}");
    let (_, body) = search(addr, "sc", &[("q", "*"), ("filter_by", "n:>=12"), ("sort_by", "n:asc")]).await;
    assert_eq!(ids(&body), names(&["a12", "a13", "a14", "a20"]), "the values stay their fields': {body}");
    let (_, body) = search(addr, "sc", &[("q", "*"), ("filter_by", "color:=red"), ("facet_by", "color")]).await;
    // the even ones less deleted a4, with a3 written red and a20
    assert_eq!(body["found"], 9, "{body}");
    assert_eq!(body["facet_counts"][0]["counts"][0]["count"], 9, "{body}");
    let (status, _) = search(addr, "sc", &[("q", "*"), ("filter_by", "tag:=t")]).await;
    assert_eq!(status, 404, "the dropped field is gone");
    // writes go on, logged for the new schema
    assert_eq!(write(addr, "sc", "create", serde_json::json!({"id": "a21", "title": "item 21", "n": 21, "color": "red"})).await, 201);
    let (_, body) = search(addr, "sc", &[("q", "*")]).await;
    assert_eq!(body["found"], 16, "{body}");

    // a required field the documents lack: refused as Typesense refuses it, nothing changed
    let (status, body) = patch(serde_json::json!({"fields": [{"name": "price", "type": "int32"}]})).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body["message"],
        "Field `price` has been declared in the schema, but is not found in the documents already present in the collection. If you still want to add this field, set it as `optional: true`."
    );
    let (_, schema) = get(addr, "/collections/sc").await;
    assert!(!schema["fields"].as_array().unwrap().iter().any(|f| f["name"] == "price"), "{schema}");
    let (_, body) = search(addr, "sc", &[("q", "*")]).await;
    assert_eq!(body["found"], 16, "{body}");
}
