use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use super::AppState;

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(result), error: None }
    }

    fn error(id: Option<Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message: message.into(), data: None }),
        }
    }
}

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const SERVER_NAME: &str = "torque";

// ── Tool definitions ────────────────────────────────────────────────

fn tool_definitions() -> Value {
    json!([
        {
            "name": "search",
            "title": "Search Documents",
            "description": "Search a collection. Returns matching documents with relevance scores, facets, and highlights.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collection": { "type": "string", "description": "Collection name" },
                    "q": { "type": "string", "description": "Search query text. Use * for match-all." },
                    "query_by": { "type": "string", "description": "Comma-separated field names to search in" },
                    "filter_by": { "type": "string", "description": "Filter expression (e.g. \"price:>100 && brand:=Acme\")" },
                    "sort_by": { "type": "string", "description": "Sort expression (e.g. \"price:desc\")" },
                    "facet_by": { "type": "string", "description": "Comma-separated fields to facet on" },
                    "group_by": { "type": "string", "description": "Field to group results by" },
                    "per_page": { "type": "integer", "description": "Results per page (default: 10, max: 250)" },
                    "page": { "type": "integer", "description": "Page number (default: 1)" },
                    "prefix": { "type": "boolean", "description": "Enable prefix matching (default: false)" },
                    "num_typos": { "type": "integer", "description": "Number of typos to tolerate (0-2)" },
                    "max_facet_values": { "type": "integer", "description": "Max facet values to return" }
                },
                "required": ["collection", "q", "query_by"]
            }
        },
        {
            "name": "multi_search",
            "title": "Multi-Search",
            "description": "Execute multiple searches in parallel. Each search targets a collection with its own parameters.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "searches": {
                        "type": "array",
                        "description": "Array of search requests, each with collection, q, query_by, and optional filter_by/sort_by/facet_by/per_page",
                        "items": {
                            "type": "object",
                            "properties": {
                                "collection": { "type": "string" },
                                "q": { "type": "string" },
                                "query_by": { "type": "string" },
                                "filter_by": { "type": "string" },
                                "sort_by": { "type": "string" },
                                "facet_by": { "type": "string" },
                                "per_page": { "type": "integer" }
                            },
                            "required": ["collection", "q", "query_by"]
                        }
                    }
                },
                "required": ["searches"]
            }
        },
        {
            "name": "collections",
            "title": "List Collections",
            "description": "List all collections with their document counts and field schemas.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false
            }
        },
        {
            "name": "schema",
            "title": "Get Collection Schema",
            "description": "Get the full schema for a collection including field names, types, and attributes (facet, sort, index, optional).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collection": { "type": "string", "description": "Collection name" }
                },
                "required": ["collection"]
            }
        },
        {
            "name": "get_document",
            "title": "Get Document",
            "description": "Retrieve a single document by its ID from a collection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collection": { "type": "string", "description": "Collection name" },
                    "id": { "type": "string", "description": "Document ID" }
                },
                "required": ["collection", "id"]
            }
        },
        {
            "name": "create_collection",
            "title": "Create Collection",
            "description": "Create a new collection with the given schema. Fields define the document structure.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Collection name" },
                    "fields": {
                        "type": "array",
                        "description": "Field definitions",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string" },
                                "type": { "type": "string", "description": "Field type: string, string[], int32, int64, float, bool, geopoint, object, object[], auto" },
                                "facet": { "type": "boolean" },
                                "sort": { "type": "boolean" },
                                "index": { "type": "boolean" },
                                "optional": { "type": "boolean" },
                                "stem": { "type": "boolean" }
                            },
                            "required": ["name", "type"]
                        }
                    },
                    "default_sorting_field": { "type": "string", "description": "Default field to sort by" },
                    "enable_nested_fields": { "type": "boolean", "description": "Enable dot-notation nested fields" }
                },
                "required": ["name", "fields"]
            }
        },
        {
            "name": "upsert_document",
            "title": "Upsert Document",
            "description": "Insert or update a document in a collection. If a document with the same ID exists, it is replaced.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collection": { "type": "string", "description": "Collection name" },
                    "document": { "type": "object", "description": "Document JSON object. Must include an 'id' field." }
                },
                "required": ["collection", "document"]
            }
        },
        {
            "name": "delete_document",
            "title": "Delete Document",
            "description": "Delete a single document by its ID from a collection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collection": { "type": "string", "description": "Collection name" },
                    "id": { "type": "string", "description": "Document ID" }
                },
                "required": ["collection", "id"]
            }
        },
        {
            "name": "delete_collection",
            "title": "Delete Collection",
            "description": "Delete an entire collection and all its documents. Enabled by default; can be disabled via allow_drop_collection config.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collection": { "type": "string", "description": "Collection name" }
                },
                "required": ["collection"]
            }
        }
    ])
}

// ── POST /api/mcp handler ───────────────────────────────────────────

pub async fn mcp_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    // MCP 2025-11-25 §Transports: validate Origin header to prevent DNS rebinding.
    if let Some(origin) = headers.get(header::ORIGIN) {
        if let Ok(origin_str) = origin.to_str() {
            let host = origin_str
                .strip_prefix("http://")
                .or_else(|| origin_str.strip_prefix("https://"))
                .unwrap_or(origin_str);
            let host_part = host.split(':').next().unwrap_or(host);
            if host_part != "localhost" && host_part != "127.0.0.1" && host_part != "::1" {
                // Allow CORS origins configured on the server
                let allowed = state.config.cors_origins.iter().any(|o| o == "*" || o == origin_str);
                if !allowed {
                    return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
                }
            }
        }
    }

    // MCP 2025-11-25 §Transports: validate MCP-Protocol-Version on non-initialize requests.
    if let Some(version_hdr) = headers.get("mcp-protocol-version") {
        if let Ok(v) = version_hdr.to_str() {
            if v != MCP_PROTOCOL_VERSION {
                return (StatusCode::BAD_REQUEST, "Unsupported MCP-Protocol-Version").into_response();
            }
        }
    }

    let request: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            let resp = JsonRpcResponse::error(None, PARSE_ERROR, format!("Parse error: {e}"));
            return json_response(&resp);
        }
    };

    if request.jsonrpc != "2.0" {
        let resp = JsonRpcResponse::error(request.id, INVALID_REQUEST, "Expected jsonrpc: \"2.0\"");
        return json_response(&resp);
    }

    if request.method.starts_with("notifications/") {
        return StatusCode::ACCEPTED.into_response();
    }

    let resp = match request.method.as_str() {
        "initialize" => handle_initialize(request.id),
        "tools/list" => handle_tools_list(request.id),
        "tools/call" => handle_tools_call(request.id, &request.params, &state).await,
        "ping" => JsonRpcResponse::success(request.id, json!({})),
        _ => JsonRpcResponse::error(
            request.id,
            METHOD_NOT_FOUND,
            format!("Method not found: {}", request.method),
        ),
    };

    json_response(&resp)
}

pub async fn mcp_get_handler() -> impl IntoResponse {
    StatusCode::METHOD_NOT_ALLOWED
}

pub async fn mcp_delete_handler() -> impl IntoResponse {
    StatusCode::METHOD_NOT_ALLOWED
}

fn json_response(resp: &JsonRpcResponse) -> axum::response::Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(resp).unwrap(),
    )
        .into_response()
}

// ── MCP method handlers ─────────────────────────────────────────────

fn handle_initialize(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse::success(id, json!({ "tools": tool_definitions() }))
}

async fn handle_tools_call(
    id: Option<Value>,
    params: &Value,
    state: &Arc<AppState>,
) -> JsonRpcResponse {
    let tool_name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return JsonRpcResponse::error(id, INVALID_PARAMS, "Missing 'name' in tools/call params"),
    };

    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match tool_name {
        "search" => tool_search(&arguments, state).await,
        "multi_search" => tool_multi_search(&arguments, state).await,
        "collections" => tool_collections(state),
        "schema" => tool_schema(&arguments, state),
        "get_document" => tool_get_document(&arguments, state),
        "create_collection" => tool_create_collection(&arguments, state),
        "upsert_document" => tool_upsert_document(&arguments, state).await,
        "delete_document" => tool_delete_document(&arguments, state).await,
        "delete_collection" => tool_delete_collection(&arguments, state),
        _ => Err(format!("Unknown tool: {tool_name}")),
    };

    match result {
        Ok(content) => JsonRpcResponse::success(
            id,
            json!({ "content": [{ "type": "text", "text": content }] }),
        ),
        Err(msg) => JsonRpcResponse::success(
            id,
            json!({ "content": [{ "type": "text", "text": msg }], "isError": true }),
        ),
    }
}

// ── Tool implementations ────────────────────────────────────────────

fn resolve_collection(
    name: &str,
    state: &AppState,
) -> Result<std::sync::Arc<crate::collection::Collection>, String> {
    let resolved = state.aliases.resolve(name);
    state
        .collections
        .get(&resolved)
        .ok_or_else(|| format!("Collection '{}' not found", name))
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required '{}' argument", key))
}

async fn tool_search(args: &Value, state: &Arc<AppState>) -> Result<String, String> {
    let collection_name = require_str(args, "collection")?;
    let q = require_str(args, "q")?;
    let query_by = require_str(args, "query_by")?;

    let collection = resolve_collection(collection_name, state)?;
    let bundle = collection.snapshot_bundle();

    let per_page = args.get("per_page").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let page = args.get("page").and_then(|v| v.as_u64()).unwrap_or(1) as usize;

    let search_params = crate::search::SearchParams {
        q: q.to_string(),
        query_by: query_by.split(',').map(|s| s.trim().to_string()).collect(),
        filter_by: args.get("filter_by").and_then(|v| v.as_str()).map(String::from),
        sort_by: args.get("sort_by").and_then(|v| v.as_str()).map(String::from),
        facet_by: args.get("facet_by").and_then(|v| v.as_str()).map(String::from),
        per_page: per_page.clamp(1, 250),
        page: page.clamp(1, 1000),
        group_by: args.get("group_by").and_then(|v| v.as_str()).map(String::from),
        prefix: args.get("prefix").and_then(param_text),
        num_typos: args.get("num_typos").and_then(param_text),
        max_facet_values: args.get("max_facet_values").and_then(|v| v.as_u64()).map(|v| v as usize),
        ..Default::default()
    };

    let synonyms = Arc::clone(&state.synonyms);
    let stopwords = Arc::clone(&state.stopwords);
    #[cfg(feature = "cuda")]
    let gpu = state.gpu.clone();

    let search_bundle = Arc::clone(&bundle);
    let schema = collection.schema.clone();
    let run_search = move || {
        crate::search::execute_with_stores_gpu_join(
            &search_bundle,
            &schema,
            &search_params,
            Some(&synonyms),
            Some(&stopwords),
            #[cfg(feature = "cuda")]
            gpu.as_ref(),
            None,
        )
    };

    let search_result = tokio::task::block_in_place(run_search);

    let (response, _timings) = search_result?;
    Ok(serde_json::to_string_pretty(&response).unwrap())
}

async fn tool_multi_search(args: &Value, state: &Arc<AppState>) -> Result<String, String> {
    let searches = args
        .get("searches")
        .and_then(|v| v.as_array())
        .ok_or("Missing required 'searches' array")?;

    if searches.len() > 50 {
        return Err("Too many searches: maximum is 50".into());
    }

    let mut results = Vec::with_capacity(searches.len());
    for item in searches {
        let result = tool_search(item, state).await;
        match result {
            Ok(json_str) => {
                let parsed: Value = serde_json::from_str(&json_str).unwrap_or(json!({"error": "parse failed"}));
                results.push(parsed);
            }
            Err(e) => results.push(json!({"error": e})),
        }
    }

    Ok(serde_json::to_string_pretty(&json!({ "results": results })).unwrap())
}

/// A per-field search parameter as text, the way the search takes it (`prefix=true,false`): a tool
/// call may send it as a JSON bool, number or string.
fn param_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(_) | Value::Number(_) => Some(v.to_string()),
        _ => None,
    }
}

fn tool_collections(state: &AppState) -> Result<String, String> {
    let collections: Vec<Value> = state
        .collections
        .list()
        .iter()
        .map(|col| {
            let num_docs = col.snapshot_bundle().total_docs();
            json!({
                "name": col.name,
                "num_documents": num_docs,
                "num_fields": col.schema.fields.len(),
            })
        })
        .collect();

    Ok(serde_json::to_string_pretty(&json!({ "collections": collections })).unwrap())
}

fn tool_schema(args: &Value, state: &AppState) -> Result<String, String> {
    let name = require_str(args, "collection")?;
    let collection = resolve_collection(name, state)?;

    let fields: Vec<Value> = collection
        .schema
        .fields
        .iter()
        .map(|f| {
            let mut field = json!({
                "name": f.name,
                "type": f.field_type.as_str(),
            });
            let obj = field.as_object_mut().unwrap();
            if f.facet { obj.insert("facet".into(), json!(true)); }
            if f.sort { obj.insert("sort".into(), json!(true)); }
            if f.optional { obj.insert("optional".into(), json!(true)); }
            if !f.index { obj.insert("index".into(), json!(false)); }
            if f.stem { obj.insert("stem".into(), json!(true)); }
            field
        })
        .collect();

    let resp = json!({
        "name": collection.name,
        "fields": fields,
        "default_sorting_field": collection.schema.default_sorting_field,
        "enable_nested_fields": collection.schema.enable_nested_fields,
        "num_documents": collection.snapshot_bundle().total_docs(),
    });
    Ok(serde_json::to_string_pretty(&resp).unwrap())
}

fn tool_get_document(args: &Value, state: &AppState) -> Result<String, String> {
    let name = require_str(args, "collection")?;
    let doc_id = require_str(args, "id")?;
    let collection = resolve_collection(name, state)?;

    // a pending write, else the live copy in the segments
    match collection.document(doc_id) {
        Some(doc) => Ok(serde_json::to_string_pretty(&crate::index::document_to_json(&doc, &collection.schema)).unwrap()),
        None => Err(format!("Document '{}' not found", doc_id)),
    }
}

fn tool_create_collection(args: &Value, state: &AppState) -> Result<String, String> {
    let schema: crate::schema::Schema =
        serde_json::from_value(args.clone()).map_err(|e| format!("Invalid schema: {e}"))?;

    if state.aliases.get(&schema.name).is_some() {
        return Err(format!(
            "An alias with name '{}' already exists",
            schema.name
        ));
    }

    let collection = crate::persistence::create_collection(&state.config.data_dir, &state.collections, schema).map_err(|e| format!("{e}"))?;

    tracing::info!(collection = %collection.name, "Collection created via MCP");
    Ok(serde_json::to_string_pretty(
        &serde_json::to_value(super::response::CollectionResponse::from_collection(&collection)).unwrap(),
    ).unwrap())
}

async fn tool_upsert_document(args: &Value, state: &Arc<AppState>) -> Result<String, String> {
    let name = require_str(args, "collection")?;
    let doc_json = args
        .get("document")
        .ok_or("Missing required 'document' argument")?;

    let collection = resolve_collection(name, state)?;
    let doc = crate::ingest::parse_json_document(doc_json, &collection.schema, 0)
        .map_err(|e| format!("Invalid document: {e}"))?;

    let ext_id = doc.external_id.clone();
    super::routes::write_documents(state, &collection, vec![doc], Vec::new()).await.map_err(|e| e.to_string())?;

    Ok(json!({"id": ext_id, "success": true}).to_string())
}

async fn tool_delete_document(args: &Value, state: &Arc<AppState>) -> Result<String, String> {
    let name = require_str(args, "collection")?;
    let doc_id = require_str(args, "id")?;
    let collection = resolve_collection(name, state)?;

    if !collection.contains(doc_id) {
        return Err(format!("Document '{}' not found", doc_id));
    }
    super::routes::write_documents(state, &collection, Vec::new(), vec![doc_id.to_string()]).await.map_err(|e| e.to_string())?;

    Ok(json!({"id": doc_id, "deleted": true}).to_string())
}

fn tool_delete_collection(args: &Value, state: &AppState) -> Result<String, String> {
    if !state.config.allow_drop_collection {
        return Err("Collection deletion is disabled on this server. Enable via --allow-drop-collection.".into());
    }

    let name = require_str(args, "collection")?;
    let resolved = state.aliases.resolve(name);

    let aliases = state.aliases.aliases_for_collection(&resolved);
    if !aliases.is_empty() {
        return Err(format!(
            "Cannot delete collection '{}': it has active aliases: {}. Delete the aliases first.",
            resolved,
            aliases.join(", ")
        ));
    }

    let deleted = crate::persistence::delete_collection(&state.config.data_dir, &state.collections, &resolved).map_err(|e| format!("{e}"))?;
    crate::persistence::remove_collection_files(&state.config.data_dir, &deleted);
    drop(deleted);
    crate::memory::purge_allocator();

    tracing::info!(collection = %resolved, "Collection deleted via MCP");
    Ok(json!({"name": resolved, "deleted": true}).to_string())
}
