use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;

use super::scoped::{self, ResolvedKey};
use super::KeyStore;
use crate::server::AppState;

/// Paths that do not require authentication.
const UNAUTHENTICATED_PATHS: &[&str] = &["/health", "/health/ready", "/metrics"];

/// Why a key may not do what a request asks: the status and message answered, and the reason
/// logged.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub status: StatusCode,
    pub message: &'static str,
    reason: &'static str,
}

impl Refusal {
    fn unauthorized(message: &'static str, reason: &'static str) -> Self {
        Self { status: StatusCode::UNAUTHORIZED, message, reason }
    }

    fn forbidden(message: &'static str, reason: &'static str) -> Self {
        Self { status: StatusCode::FORBIDDEN, message, reason }
    }

    /// No key sent.
    pub fn missing() -> Self {
        Self::unauthorized("No API key provided.", "no API key")
    }

    pub fn response(&self) -> Response {
        let body = json!({ "message": self.message });
        (self.status, axum::response::Json(body)).into_response()
    }
}

/// What the key `value` may do, if it may do `action` (on `collection`): the bootstrap key; a
/// scoped key, with its parent's actions and collections and both their expiries; or a stored key.
pub fn resolve_key(keys: &KeyStore, value: &str, action: &str, collection: Option<&str>) -> Result<ResolvedKey, Refusal> {
    if keys.is_bootstrap(value) {
        return Ok(ResolvedKey::Bootstrap);
    }
    let permits = |key: &super::ApiKey| -> Result<(), Refusal> {
        if !KeyStore::key_permits_action(key, action) {
            Err(Refusal::forbidden("API key is not allowed to access this resource.", "action not permitted"))
        } else if collection.is_some_and(|c| !KeyStore::key_permits_collection(key, c)) {
            Err(Refusal::forbidden("API key is not allowed to access this collection.", "collection not permitted"))
        } else {
            Ok(())
        }
    };
    if let Some(decoded) = scoped::try_decode_scoped_key(value, keys) {
        if KeyStore::key_is_expired(&decoded.parent) {
            return Err(Refusal::unauthorized("Parent API key has expired.", "parent key expired"));
        }
        if let Some(expires_at) = decoded.params.expires_at {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
            if now >= expires_at {
                return Err(Refusal::unauthorized("Scoped API key has expired.", "scoped key expired"));
            }
        }
        permits(&decoded.parent)?;
        return Ok(ResolvedKey::Scoped { parent: decoded.parent, params: decoded.params });
    }
    let key = keys.get(value).ok_or(Refusal::unauthorized("API key not found.", "key not found"))?;
    if KeyStore::key_is_expired(&key) {
        return Err(Refusal::unauthorized("API key has expired.", "key expired"));
    }
    permits(&key)?;
    Ok(ResolvedKey::Direct(key))
}

pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Result<Response, Response> {
    let path = request.uri().path().to_string();

    // Skip auth for health/metrics endpoints
    if UNAUTHENTICATED_PATHS.iter().any(|p| path == *p) {
        return Ok(next.run(request).await);
    }

    // Extract API key from header or query param
    let api_key_value = extract_api_key(&request);
    let key_prefix: String = api_key_value.as_deref().unwrap_or("").chars().take(8).collect();
    let (action, collection) = map_request_to_action(request.method(), &path);
    let outcome = match &api_key_value {
        Some(value) => resolve_key(&state.keys, value, &action, collection.as_deref()),
        None => Err(Refusal::missing()),
    };
    match outcome {
        Ok(key) => {
            request.extensions_mut().insert(key);
            Ok(next.run(request).await)
        }
        // A multi_search's searches may each carry their own key (Typesense's
        // `x-typesense-api-key` in a search): the handler applies the refusal to those without.
        Err(refusal) if path == "/multi_search" => {
            request.extensions_mut().insert(refusal);
            Ok(next.run(request).await)
        }
        Err(refusal) => {
            tracing::warn!(
                peer = %peer, key_prefix, action = action.as_str(),
                collection = collection.as_deref().unwrap_or(""),
                path = path.as_str(),
                "auth denied: {}", refusal.reason
            );
            Err(refusal.response())
        }
    }
}

fn extract_api_key(request: &Request) -> Option<String> {
    // Check header first (case-insensitive)
    if let Some(val) = request.headers().get("x-typesense-api-key") {
        return val.to_str().ok().map(|s| s.to_string());
    }

    // Check query parameter
    if let Some(query) = request.uri().query() {
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("x-typesense-api-key=") {
                return Some(val.to_string());
            }
        }
    }

    None
}

/// Map an HTTP request (method + path) to a required action and optional collection name.
fn map_request_to_action(method: &Method, path: &str) -> (String, Option<String>) {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    match segments.as_slice() {
        // Key routes
        ["keys"] if *method == Method::POST => ("keys:create".into(), None),
        ["keys"] if *method == Method::GET => ("keys:list".into(), None),
        ["keys", _id] if *method == Method::GET => ("keys:get".into(), None),
        ["keys", _id] if *method == Method::DELETE => ("keys:delete".into(), None),

        // Collection routes
        ["collections"] if *method == Method::POST => ("collections:create".into(), None),
        ["collections"] if *method == Method::GET => ("collections:list".into(), None),
        ["collections", name] if *method == Method::GET => {
            ("collections:get".into(), Some(name.to_string()))
        }
        ["collections", name] if *method == Method::PATCH => {
            ("collections:update".into(), Some(name.to_string()))
        }
        ["collections", name] if *method == Method::DELETE => {
            ("collections:delete".into(), Some(name.to_string()))
        }
        // Segment upload creates the collection.
        ["collections", name, "segments"] if *method == Method::PUT => {
            ("collections:create".into(), Some(name.to_string()))
        }

        // Document routes
        ["collections", name, "documents"] if *method == Method::PATCH => {
            ("documents:update".into(), Some(name.to_string()))
        }
        ["collections", name, "documents", "search"] => {
            ("documents:search".into(), Some(name.to_string()))
        }
        ["collections", name, "documents", "import"] => {
            ("documents:import".into(), Some(name.to_string()))
        }

        // Multi-search
        ["multi_search"] => ("documents:search".into(), None),

        // Alias routes
        ["aliases"] if *method == Method::GET => ("aliases:list".into(), None),
        ["aliases", _name] if *method == Method::PUT => ("aliases:create".into(), None),
        ["aliases", _name] if *method == Method::GET => ("aliases:get".into(), None),
        ["aliases", _name] if *method == Method::DELETE => ("aliases:delete".into(), None),

        // Synonym routes
        ["synonym_sets"] if *method == Method::GET => ("synonym_sets:list".into(), None),
        ["synonym_sets", _name] if *method == Method::PUT => ("synonym_sets:create".into(), None),
        ["synonym_sets", _name] if *method == Method::GET => ("synonym_sets:get".into(), None),
        ["synonym_sets", _name] if *method == Method::DELETE => {
            ("synonym_sets:delete".into(), None)
        }
        ["synonym_sets", _name, "items"] => ("synonym_sets:get".into(), None),
        ["synonym_sets", _name, "items", _id] if *method == Method::PUT => {
            ("synonym_sets:create".into(), None)
        }
        ["synonym_sets", _name, "items", _id] if *method == Method::GET => {
            ("synonym_sets:get".into(), None)
        }
        ["synonym_sets", _name, "items", _id] if *method == Method::DELETE => {
            ("synonym_sets:delete".into(), None)
        }

        // Preset routes
        ["presets"] if *method == Method::GET => ("presets:list".into(), None),
        ["presets", _id] if *method == Method::PUT => ("presets:create".into(), None),
        ["presets", _id] if *method == Method::GET => ("presets:get".into(), None),
        ["presets", _id] if *method == Method::DELETE => ("presets:delete".into(), None),

        // Stemming dictionary routes
        ["stemming", "dictionaries"] if *method == Method::GET => {
            ("stemming:list".into(), None)
        }
        ["stemming", "dictionaries", "import"] if *method == Method::POST => {
            ("stemming:create".into(), None)
        }
        ["stemming", "dictionaries", _id] if *method == Method::GET => {
            ("stemming:get".into(), None)
        }

        // Stopword routes
        ["stopwords"] if *method == Method::GET => ("stopwords:list".into(), None),
        ["stopwords", _id] if *method == Method::PUT => ("stopwords:create".into(), None),
        ["stopwords", _id] if *method == Method::GET => ("stopwords:get".into(), None),
        ["stopwords", _id] if *method == Method::DELETE => ("stopwords:delete".into(), None),

        // Default: require wildcard
        _ => ("*".into(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_mapping() {
        let (action, col) = map_request_to_action(&Method::POST, "/collections");
        assert_eq!(action, "collections:create");
        assert!(col.is_none());

        let (action, col) = map_request_to_action(&Method::GET, "/collections/products");
        assert_eq!(action, "collections:get");
        assert_eq!(col.as_deref(), Some("products"));

        let (action, col) =
            map_request_to_action(&Method::GET, "/collections/products/documents/search");
        assert_eq!(action, "documents:search");
        assert_eq!(col.as_deref(), Some("products"));

        let (action, col) =
            map_request_to_action(&Method::PATCH, "/collections/products/documents");
        assert_eq!(action, "documents:update");
        assert_eq!(col.as_deref(), Some("products"));

        let (action, _) = map_request_to_action(&Method::POST, "/keys");
        assert_eq!(action, "keys:create");

        let (action, _) = map_request_to_action(&Method::POST, "/multi_search");
        assert_eq!(action, "documents:search");
    }

    #[test]
    fn unauthenticated_paths() {
        assert!(UNAUTHENTICATED_PATHS.contains(&"/health"));
        assert!(UNAUTHENTICATED_PATHS.contains(&"/health/ready"));
        assert!(UNAUTHENTICATED_PATHS.contains(&"/metrics"));
    }
}
