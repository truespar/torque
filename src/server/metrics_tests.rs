//! What /metrics holds stays bounded: a request counts under the route it matched, not its path (a
//! path holds document ids and collection names), and a deleted collection takes its series along.

use super::model_tests::{call, serve};
use crate::collection::CollectionRegistry;
use axum::http::Method;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread")]
async fn metrics_hold_routes_and_the_collections_there_are() {
    let dir = tempfile::tempdir().unwrap();
    let (state, app) = serve(dir.path(), Arc::new(CollectionRegistry::new(None)));
    let app = app.layer(axum::middleware::from_fn_with_state(Arc::clone(&state), super::request_id_middleware));
    let metrics = || {
        let mut text = String::new();
        prometheus_client::encoding::text::encode(&mut text, &state.metrics.registry).unwrap();
        text
    };

    let schema = json!({"name": "nightly_1", "index_mode": "realtime", "fields": [{"name": "title", "type": "string"}]});
    let (s, b) = call(&app, Method::POST, "/collections", Some(schema.to_string())).await;
    assert!((200..300).contains(&s), "create: {s} {b}");
    let docs = "{\"id\":\"doc-a\",\"title\":\"alpha\"}\n{\"id\":\"doc-b\",\"title\":\"beta\"}";
    let (s, b) = call(&app, Method::POST, "/collections/nightly_1/documents/import?action=create", Some(docs.into())).await;
    assert_eq!(s, 200, "import: {b}");
    for id in ["doc-a", "doc-b"] {
        let (s, b) = call(&app, Method::GET, &format!("/collections/nightly_1/documents/{id}"), None).await;
        assert_eq!(s, 200, "get {id}: {b}");
    }
    for q in ["alpha", "*"] {
        let (s, b) = call(&app, Method::GET, &format!("/collections/nightly_1/documents/search?q={q}&query_by=title"), None).await;
        assert_eq!(s, 200, "search {q}: {b}");
    }
    let (s, _) = call(&app, Method::GET, "/no/such/route", None).await;
    assert_eq!(s, 404);
    crate::observability::stats::refresh_metrics(&state.metrics, &state.collections);

    let text = metrics();
    assert!(text.contains(r#"torque_http_requests_total{method="GET",path="/collections/{name}/documents/{id}"} 2"#), "{text}");
    assert!(text.contains(r#"torque_http_requests_total{method="GET",path="unmatched"} 1"#), "{text}");
    assert!(!text.contains("doc-a") && !text.contains("/no/such/route"), "{text}");
    assert!(text.contains(r#"collection="nightly_1""#), "{text}");

    // its series go in the delete's cleanup, in the background, after any save under way
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.metrics.index_saves_in_flight.get() > 0 || state.metrics.index_builds_in_flight.get() > 0 {
        assert!(Instant::now() < deadline, "background work did not end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (s, b) = call(&app, Method::DELETE, "/collections/nightly_1", None).await;
    assert_eq!(s, 200, "delete: {b}");
    while metrics().contains(r#"collection="nightly_1""#) {
        assert!(Instant::now() < deadline, "{}", metrics());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
