use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

use super::error::AppError;
use super::response::{AliasResponse, CollectionListItem, CollectionResponse};
use super::AppState;
use crate::auth::scoped::{ResolvedKey, ScopedKeyParams};
use crate::auth::{ApiKeySummary, CreateKeyRequest};
use crate::collection::CollectionError;
use crate::ingest;
use crate::observability::{self, QueryLabels};
use crate::persistence;
use crate::presets;
use crate::index::document_to_json;
use crate::tqbf::json::TsJson;
use crate::schema::{FieldDef, Schema};
use crate::search;
use crate::stopwords::StopwordSet;
use crate::synonyms::{SynonymItem, SynonymSet};

const MAX_QUERY_LEN: usize = 4096;
const MAX_FILTER_LEN: usize = 4096;
const MAX_SORT_LEN: usize = 2048;
const MAX_FACET_LEN: usize = 2048;

fn validate_search_param_lengths(params: &search::SearchParams) -> Result<(), AppError> {
    if params.q.len() > MAX_QUERY_LEN {
        return Err(AppError::BadRequest(format!(
            "`q` is {} characters, exceeding the maximum of {MAX_QUERY_LEN}.",
            params.q.len()
        )));
    }
    if let Some(ref f) = params.filter_by {
        if f.len() > MAX_FILTER_LEN {
            return Err(AppError::BadRequest(format!(
                "`filter_by` is {} characters, exceeding the maximum of {MAX_FILTER_LEN}.",
                f.len()
            )));
        }
    }
    if let Some(ref s) = params.sort_by {
        if s.len() > MAX_SORT_LEN {
            return Err(AppError::BadRequest(format!(
                "`sort_by` is {} characters, exceeding the maximum of {MAX_SORT_LEN}.",
                s.len()
            )));
        }
    }
    if let Some(ref f) = params.facet_by {
        if f.len() > MAX_FACET_LEN {
            return Err(AppError::BadRequest(format!(
                "`facet_by` is {} characters, exceeding the maximum of {MAX_FACET_LEN}.",
                f.len()
            )));
        }
    }
    Ok(())
}

pub fn routes() -> Router<Arc<AppState>> {
    #[allow(unused_mut)]
    let mut router = Router::new()
        .route("/health", get(health))
        .route("/health/ready", get(health_ready))
        .route("/metrics", get(metrics))
        .route("/admin/stats", get(admin_stats))
        .route("/stats.json", get(admin_stats))
        .route("/api/ai/chat", post(super::ai::chat))
        .route("/debug", get(debug_info))
        .route("/debug/malloc", get(debug_malloc_stats))
        // Collection routes
        .route("/collections", post(create_collection))
        .route("/collections", get(list_collections))
        .route("/collections/{name}", get(get_collection))
        .route("/collections/{name}", patch(update_collection))
        .route("/collections/{name}", delete(delete_collection))
        // Document routes
        .route(
            "/collections/{name}/documents/search",
            get(search_documents),
        )
        .route(
            "/collections/{name}/documents/import",
            post(import_documents),
        )
        .route(
            "/collections/{name}/documents/export",
            get(export_documents),
        )
        .route(
            "/collections/{name}/documents/import-binary",
            post(import_binary).put(import_binary),
        )
        .route(
            "/collections/{name}/documents/compact",
            post(compact_collection),
        )
        .route(
            "/collections/{name}/suspend",
            post(suspend_collection),
        )
        .route(
            "/collections/{name}/resume",
            post(resume_collection),
        )
        .route(
            "/collections/{name}/documents",
            post(create_document)
                .patch(update_documents_by_filter)
                .delete(delete_documents_by_filter),
        )
        .route(
            "/collections/{name}/documents/{id}",
            get(get_document)
                .patch(update_document)
                .delete(delete_document),
        )
        // Multi-search
        .route("/multi_search", post(multi_search))
        // Alias routes
        .route("/aliases", get(list_aliases))
        .route("/aliases/{name}", put(upsert_alias))
        .route("/aliases/{name}", get(get_alias))
        .route("/aliases/{name}", delete(delete_alias))
        // Synonym routes
        .route("/synonym_sets", get(list_synonym_sets))
        .route("/synonym_sets/{name}", put(upsert_synonym_set))
        .route("/synonym_sets/{name}", get(get_synonym_set))
        .route("/synonym_sets/{name}", delete(delete_synonym_set))
        .route(
            "/synonym_sets/{name}/items",
            get(list_synonym_items),
        )
        .route(
            "/synonym_sets/{name}/items/{id}",
            put(upsert_synonym_item),
        )
        .route(
            "/synonym_sets/{name}/items/{id}",
            get(get_synonym_item),
        )
        .route(
            "/synonym_sets/{name}/items/{id}",
            delete(delete_synonym_item),
        )
        // Conversation model routes
        .route("/conversations/models", post(create_conversation_model))
        .route("/conversations/models", get(list_conversation_models))
        .route("/conversations/models/{id}", get(get_conversation_model))
        .route("/conversations/models/{id}", put(update_conversation_model))
        .route("/conversations/models/{id}", delete(delete_conversation_model))
        // NL search model routes
        .route("/nl_search_models", post(create_nl_model))
        .route("/nl_search_models", get(list_nl_models))
        .route("/nl_search_models/{id}", get(get_nl_model))
        .route("/nl_search_models/{id}", put(update_nl_model))
        .route("/nl_search_models/{id}", delete(delete_nl_model))
        // Analytics routes
        .route("/analytics/rules", post(create_analytics_rules))
        .route("/analytics/rules", get(list_analytics_rules))
        .route("/analytics/rules/{name}", put(upsert_analytics_rule))
        .route("/analytics/rules/{name}", get(get_analytics_rule))
        .route("/analytics/rules/{name}", delete(delete_analytics_rule))
        .route("/analytics/events", post(create_analytics_event))
        .route("/analytics/events", get(list_analytics_events))
        .route("/analytics/flush", post(flush_analytics))
        .route("/analytics/status", get(analytics_status))
        // Curation set routes
        .route("/curation_sets", get(list_curation_sets))
        .route("/curation_sets/{name}", put(upsert_curation_set))
        .route("/curation_sets/{name}", get(get_curation_set))
        .route("/curation_sets/{name}", delete(delete_curation_set))
        .route("/curation_sets/{name}/items", get(list_curation_items))
        .route("/curation_sets/{name}/items/{id}", put(upsert_curation_item))
        .route("/curation_sets/{name}/items/{id}", get(get_curation_item))
        .route(
            "/curation_sets/{name}/items/{id}",
            delete(delete_curation_item),
        )
        // Preset routes
        .route("/presets", get(list_presets))
        .route("/presets/{id}", put(upsert_preset))
        .route("/presets/{id}", get(get_preset))
        .route("/presets/{id}", delete(delete_preset))
        // Stopword routes
        .route("/stopwords", get(list_stopwords))
        .route("/stopwords/{id}", put(upsert_stopword_set))
        .route("/stopwords/{id}", get(get_stopword_set))
        .route("/stopwords/{id}", delete(delete_stopword_set))
        // Stemming dictionaries
        .route("/stemming/dictionaries", get(list_stemming_dictionaries))
        .route("/stemming/dictionaries/import", post(import_stemming_dictionary))
        .route("/stemming/dictionaries/{id}", get(get_stemming_dictionary))
        // Operations + runtime config (Typesense v30.1)
        .route("/operations/snapshot", post(operations_snapshot))
        .route("/operations/quiesce", post(operations_quiesce))
        .route("/operations/db/compact", post(operations_db_compact))
        .route("/operations/cache/clear", post(operations_cache_clear))
        .route("/operations/schema_changes", get(operations_schema_changes))
        .route("/metrics.json", get(metrics_json))
        .route("/config", post(update_runtime_config))
        // Key routes
        .route("/keys", post(create_key))
        .route("/keys", get(list_keys))
        .route("/keys/{id}", get(get_key))
        .route("/keys/{id}", delete(delete_key))
        // Override stubs (not implemented — Typesense v30.1 compat)
        .route("/collections/{name}/overrides", get(list_overrides))
        .route(
            "/collections/{name}/overrides/{id}",
            get(get_override).put(upsert_override).delete(delete_override),
        );

    #[cfg(feature = "mcp")]
    {
        router = router.route(
            "/api/mcp",
            post(super::mcp::mcp_post_handler)
                .get(super::mcp::mcp_get_handler)
                .delete(super::mcp::mcp_delete_handler),
        );
    }

    router
}

// -- Health --

async fn health(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "allow_drop_collection": state.config.allow_drop_collection,
        "studio_enabled": state.config.studio_enabled,
        "features": { "ai": state.config.ai.is_some() },
    }))
}

async fn health_ready(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let collections = state.collections.list();
    let total = collections.len();
    let ready = collections
        .iter()
        .filter(|c| c.snapshot_bundle().total_docs() > 0)
        .count();

    if ready > 0 {
        (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "collections_ready": ready,
                "collections_total": total,
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "ok": false,
                "collections_ready": 0,
                "collections_total": total,
                "message": "no collections have data yet",
            })),
        )
    }
}

async fn metrics(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    observability::prometheus::metrics_handler(&state.metrics)
}

async fn admin_stats(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    let ingest_port = state.config.ingest_addr.port();
    let stats = observability::stats::build_admin_stats(
        &state.metrics,
        &state.collections,
        state.start_time,
        ingest_port,
        &state.config.data_dir,
    );
    Json(serde_json::to_value(stats).unwrap_or_else(|_| json!({"error": "stats serialization failed"})))
}

async fn debug_info() -> Json<Value> {
    Json(json!({
        "state": 1,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn debug_malloc_stats() -> String {
    #[cfg(feature = "jemalloc")]
    {
        use tikv_jemalloc_ctl::{epoch, stats};
        crate::memory::purge_allocator();
        epoch::advance().ok();
        let allocated = stats::allocated::read().unwrap_or(0);
        let active = stats::active::read().unwrap_or(0);
        let mapped = stats::mapped::read().unwrap_or(0);
        let retained = stats::retained::read().unwrap_or(0);
        let metadata = stats::metadata::read().unwrap_or(0);
        let resident = stats::resident::read().unwrap_or(0);
        format!(
            "jemalloc stats (after purge):\n\
             allocated: {} MB\n\
             active:    {} MB  (internal frag: {} MB)\n\
             resident:  {} MB\n\
             mapped:    {} MB\n\
             retained:  {} MB\n\
             metadata:  {} MB\n",
            allocated / (1024*1024),
            active / (1024*1024), (active - allocated) / (1024*1024),
            resident / (1024*1024),
            mapped / (1024*1024),
            retained / (1024*1024),
            metadata / (1024*1024),
        )
    }
    #[cfg(not(feature = "jemalloc"))]
    {
        "malloc stats not available".to_string()
    }
}

// -- Collection handlers --

async fn create_collection(
    State(state): State<Arc<AppState>>,
    Json(schema): Json<Schema>,
) -> Result<(StatusCode, Json<CollectionResponse>), AppError> {
    let _fs_guard = state.fs_serial.lock().await;

    // Collection name must not shadow an existing alias
    if state.aliases.get(&schema.name).is_some() {
        return Err(AppError::Conflict(format!(
            "An alias with name `{}` already exists — collection names cannot shadow alias names.",
            schema.name
        )));
    }

    let collection = persistence::create_collection(&state.config.data_dir, &state.collections, schema).map_err(collection_err)?;

    tracing::info!(collection = %collection.name, "Collection created");
    Ok((StatusCode::CREATED, Json(CollectionResponse::from_collection(&collection))))
}

#[derive(Deserialize)]
struct ListCollectionsQuery {
    limit: Option<usize>,
    offset: Option<usize>,
    exclude_fields: Option<String>,
}

async fn list_collections(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListCollectionsQuery>,
) -> Json<Value> {
    // Newest-first with a name tiebreak, so limit/offset paginate stably
    // (Typesense v30.1 returns collections by creation time descending).
    let mut collections = state.collections.list();
    collections.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.name.cmp(&b.name))
    });

    let offset = params.offset.unwrap_or(0);
    let limit = params.limit.unwrap_or(usize::MAX);
    let exclude: Vec<&str> = params
        .exclude_fields
        .as_deref()
        .map(|s| s.split(',').map(str::trim).collect())
        .unwrap_or_default();

    let items: Vec<Value> = collections
        .iter()
        .skip(offset)
        .take(limit)
        .map(|c| {
            let mut v = serde_json::to_value(CollectionListItem::from_collection(c))
                .unwrap_or_else(|_| json!({}));
            if let Some(obj) = v.as_object_mut() {
                for f in &exclude {
                    obj.remove(*f);
                }
            }
            v
        })
        .collect();
    Json(Value::Array(items))
}

async fn get_collection(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<CollectionResponse>, AppError> {
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;
    Ok(Json(CollectionResponse::from_collection(&collection)))
}

#[derive(Deserialize)]
struct SchemaUpdateRequest {
    #[serde(default)]
    fields: Vec<FieldDef>,
    metadata: Option<Value>,
    synonym_sets: Option<Vec<String>>,
    curation_sets: Option<Vec<String>>,
}

async fn update_collection(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<SchemaUpdateRequest>,
) -> Result<Json<CollectionResponse>, AppError> {
    if body.fields.is_empty()
        && body.metadata.is_none()
        && body.synonym_sets.is_none()
        && body.curation_sets.is_none()
    {
        return Err(AppError::BadRequest(
            "PATCH body must contain `fields`, `metadata`, `synonym_sets`, or `curation_sets`.".into(),
        ));
    }
    let resolved = state.aliases.resolve(&name);
    let updated = change_schema(&state, &resolved, body.fields, body.metadata, body.synonym_sets, body.curation_sets).await?;
    Ok(Json(CollectionResponse::from_collection(&updated)))
}

/// A collection's schema change (`CollectionRegistry::change_schema`, which persists it), off the
/// async workers: a field change rebuilds the collection from its live documents.
async fn change_schema(
    state: &Arc<AppState>,
    name: &str,
    fields: Vec<crate::schema::FieldDef>,
    metadata: Option<Value>,
    synonym_sets: Option<Vec<String>>,
    curation_sets: Option<Vec<String>>,
) -> Result<Arc<crate::collection::Collection>, AppError> {
    let s = Arc::clone(state);
    let name = name.to_string();
    let changed = tokio::task::spawn_blocking(move || {
        s.collections.change_schema(&name, fields, metadata, synonym_sets, curation_sets, |schema, docs| {
            rebuild_index(
                &s.config.data_dir,
                &name,
                schema,
                docs,
                #[cfg(feature = "cuda")]
                s.gpu.as_ref(),
            )
        })
    })
    .await
    .map_err(|e| AppError::Internal(format!("Schema change failed: {e}")))?;
    crate::memory::purge_allocator();
    changed.map_err(collection_err)
}

async fn delete_collection(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<CollectionResponse>, AppError> {
    // Owned: the cleanup holds it past this answer, and CREATE and imports wait until the files are
    // gone.
    let fs_owned_guard = state.fs_serial.clone().lock_owned().await;
    if !state.config.allow_drop_collection {
        return Err(AppError::Forbidden(
            "Collection deletion is disabled on this server. Enable it via `allow_drop_collection = true` in torque.toml, TORQUE_ALLOW_DROP_COLLECTION=true, or the --allow-drop-collection flag.".into()
        ));
    }

    let resolved = state.aliases.resolve(&name);

    // Reject deletion if any aliases point to this collection
    let aliases = state.aliases.aliases_for_collection(&resolved);
    if !aliases.is_empty() {
        return Err(AppError::BadRequest(format!(
            "Cannot delete collection `{}`: it has active aliases: {}. Delete the aliases first.",
            resolved,
            aliases.join(", ")
        )));
    }

    // Set aside at the start, its files unreadable (another format version's): no collection to let
    // go of, only its entry and files to remove. Until then its name is taken.
    if state.collections.get(&resolved).is_none() {
        let (s, target) = (Arc::clone(&state), resolved.clone());
        let entry = tokio::task::spawn_blocking(move || persistence::delete_set_aside(&s.config.data_dir, &s.collections, &target))
            .await
            .map_err(|e| AppError::Internal(format!("Delete failed: {e}")))?
            .map_err(collection_err)?;
        tracing::info!(collection = %resolved, "Collection set aside at the start deleted");
        return Ok(Json(CollectionResponse::from_persisted(&entry)));
    }

    // Out of the registry and collections.json (the deletion's commit) before answering.
    let deleted = persistence::delete_collection(&state.config.data_dir, &state.collections, &resolved).map_err(collection_err)?;

    // Build the response from the still-alive reference before handing
    // `deleted` over to the blocking task. We need to return this Json
    // regardless of how long the cleanup takes.
    let response = CollectionResponse::from_collection(&deleted);

    // The rest is synchronous and long on a large collection: removing the files, closing the log,
    // and the last Arc's drop, which unmaps every segment. It runs off this handler: 200 means the
    // collection has left the registry (the next GET or search is a 404), and a CREATE of the same
    // name waits on fs_serial until the files are gone.
    let data_dir = state.config.data_dir.clone();
    let metrics = Arc::clone(&state.metrics);
    let resolved_for_task = resolved.clone();
    let cleanup = move || {
        let _fs_owned = fs_owned_guard;

        // its segment set (its mapped files released first: Windows refuses to delete a mapped file,
        // and searches still holding a snapshot finish within moments) and its log
        persistence::remove_collection_files(&data_dir, &deleted);

        drop(deleted);

        // its series in /metrics, late so that a search or a stats pass under way when it went has
        // most likely ended (one ending after this makes a few again)
        metrics.remove_collection(&resolved_for_task);

        crate::memory::purge_allocator();

        tracing::info!(collection = %resolved_for_task, "Collection delete cleanup done");
    };

    tokio::task::spawn_blocking(cleanup);

    tracing::info!(collection = %resolved, "Collection deleted");
    Ok(Json(response))
}

// -- Alias handlers --

#[derive(Deserialize)]
struct AliasUpsertRequest {
    collection_name: String,
}

async fn upsert_alias(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<AliasUpsertRequest>,
) -> Result<Json<AliasResponse>, AppError> {
    // Alias name must not shadow an existing collection
    if state.collections.get(&name).is_some() {
        return Err(AppError::Conflict(format!(
            "A collection with name `{name}` already exists — alias names cannot shadow collection names."
        )));
    }
    // Target collection must exist
    if state.collections.get(&body.collection_name).is_none() {
        return Err(AppError::NotFound(format!(
            "Collection `{}` not found.",
            body.collection_name
        )));
    }
    state
        .aliases
        .upsert(name.clone(), body.collection_name.clone());

    if let Err(e) = persistence::save_aliases(&state.config.data_dir, &state.aliases.list()) {
        tracing::error!("Failed to persist aliases: {e}");
    }

    tracing::info!(alias = %name, collection = %body.collection_name, "Alias upserted");
    Ok(Json(AliasResponse {
        name,
        collection_name: body.collection_name,
    }))
}

async fn get_alias(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<AliasResponse>, AppError> {
    let info = state
        .aliases
        .get(&name)
        .ok_or_else(|| AppError::NotFound(format!("Alias `{name}` not found.")))?;
    Ok(Json(AliasResponse::from(info)))
}

async fn list_aliases(State(state): State<Arc<AppState>>) -> Json<Value> {
    let items: Vec<_> = state.aliases.list().into_iter().map(AliasResponse::from).collect();
    Json(json!({ "aliases": items }))
}

async fn delete_alias(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<AliasResponse>, AppError> {
    let collection_name = state
        .aliases
        .delete(&name)
        .ok_or_else(|| AppError::NotFound(format!("Alias `{name}` not found.")))?;

    if let Err(e) = persistence::save_aliases(&state.config.data_dir, &state.aliases.list()) {
        tracing::error!("Failed to persist aliases: {e}");
    }

    tracing::info!(alias = %name, "Alias deleted");
    Ok(Json(AliasResponse {
        name,
        collection_name,
    }))
}

// -- Conversation model + NL search model handlers --

fn persist_conversation_models(state: &AppState) {
    if let Err(e) = persistence::save_conversation_models(
        &state.config.data_dir,
        &state.llm.list_conversation_models(),
    ) {
        tracing::error!("Failed to persist conversation models: {e}");
    }
}

fn persist_nl_models(state: &AppState) {
    if let Err(e) =
        persistence::save_nl_search_models(&state.config.data_dir, &state.llm.list_nl_models())
    {
        tracing::error!("Failed to persist NL search models: {e}");
    }
}

async fn create_conversation_model(
    State(state): State<Arc<AppState>>,
    Json(mut model): Json<crate::llm::ConversationModel>,
) -> Result<Json<crate::llm::ConversationModel>, AppError> {
    if model.id.is_empty() {
        model.id = crate::llm::generate_id();
    }
    // Typesense auto-creates a default history collection when none is
    // given (`ts_conversation_history_{id}`, conversation_model_manager).
    if model.history_collection.is_none() {
        let name = format!("ts_conversation_history_{}", model.id);
        ensure_history_collection(&state, &name)?;
        model.history_collection = Some(name);
    }
    state.llm.upsert_conversation_model(model.clone());
    persist_conversation_models(&state);
    Ok(Json(model))
}

/// Create the default conversation-history collection (Typesense schema:
/// conversation_id indexed, role/message/model_id stored, timestamp
/// sortable) if it does not already exist. Realtime so history upserts
/// apply without a rebuild.
fn ensure_history_collection(state: &Arc<AppState>, name: &str) -> Result<(), AppError> {
    if state.collections.get(name).is_some() {
        return Ok(());
    }
    let schema_json = json!({
        "name": name,
        "index_mode": "realtime",
        "fields": [
            { "name": "conversation_id", "type": "string" },
            { "name": "model_id", "type": "string", "index": false, "optional": true },
            { "name": "role", "type": "string", "index": false },
            { "name": "message", "type": "string", "index": false },
            { "name": "timestamp", "type": "int64", "sort": true }
        ]
    });
    let mut schema: crate::schema::Schema = serde_json::from_value(schema_json)
        .map_err(|e| AppError::Internal(format!("history collection schema: {e}")))?;
    schema.assign_field_ids();
    state
        .collections
        .create(schema)
        .map_err(|e| AppError::Internal(format!("history collection create failed: {e:?}")))?;
    if let Err(e) = persistence::save_collections(&state.config.data_dir, &state.collections) {
        tracing::error!("Failed to persist collections after history collection create: {e}");
    }
    Ok(())
}

async fn list_conversation_models(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!(state.llm.list_conversation_models()))
}

async fn get_conversation_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<crate::llm::ConversationModel>, AppError> {
    state
        .llm
        .get_conversation_model(&id)
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("Conversation model `{id}` not found.")))
}

async fn update_conversation_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut model): Json<crate::llm::ConversationModel>,
) -> Result<Json<crate::llm::ConversationModel>, AppError> {
    if state.llm.get_conversation_model(&id).is_none() {
        return Err(AppError::NotFound(format!(
            "Conversation model `{id}` not found."
        )));
    }
    model.id = id;
    state.llm.upsert_conversation_model(model.clone());
    persist_conversation_models(&state);
    Ok(Json(model))
}

async fn delete_conversation_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    if !state.llm.delete_conversation_model(&id) {
        return Err(AppError::NotFound(format!(
            "Conversation model `{id}` not found."
        )));
    }
    persist_conversation_models(&state);
    Ok(Json(json!({ "id": id })))
}

async fn create_nl_model(
    State(state): State<Arc<AppState>>,
    Json(mut model): Json<crate::llm::NlSearchModel>,
) -> Result<Json<crate::llm::NlSearchModel>, AppError> {
    if model.max_bytes.map_or(true, |b| b == 0) {
        return Err(AppError::BadRequest(
            "Property `max_bytes` is not provided or not a positive integer.".into(),
        ));
    }
    if model.id.is_empty() {
        model.id = crate::llm::generate_id();
    }
    state.llm.upsert_nl_model(model.clone());
    persist_nl_models(&state);
    Ok(Json(model))
}

async fn list_nl_models(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!(state.llm.list_nl_models()))
}

async fn get_nl_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<crate::llm::NlSearchModel>, AppError> {
    state
        .llm
        .get_nl_model(&id)
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("NL search model `{id}` not found.")))
}

async fn update_nl_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut model): Json<crate::llm::NlSearchModel>,
) -> Result<Json<crate::llm::NlSearchModel>, AppError> {
    if state.llm.get_nl_model(&id).is_none() {
        return Err(AppError::NotFound(format!(
            "NL search model `{id}` not found."
        )));
    }
    if model.max_bytes.map_or(true, |b| b == 0) {
        return Err(AppError::BadRequest(
            "Property `max_bytes` is not provided or not a positive integer.".into(),
        ));
    }
    model.id = id;
    state.llm.upsert_nl_model(model.clone());
    persist_nl_models(&state);
    Ok(Json(model))
}

async fn delete_nl_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    if !state.llm.delete_nl_model(&id) {
        return Err(AppError::NotFound(format!(
            "NL search model `{id}` not found."
        )));
    }
    persist_nl_models(&state);
    Ok(Json(json!({ "id": id })))
}

/// TTL'd cache of generated NL schema prompts, keyed by collection name
/// (Typesense: schema_prompts LRU, DEFAULT_SCHEMA_PROMPT_TTL_SEC = 86400).
/// Facet-value sampling walks the string filter indexes, so the prompt is
/// worth caching between queries.
static NL_PROMPT_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>,
> = std::sync::OnceLock::new();
const NL_PROMPT_TTL: std::time::Duration = std::time::Duration::from_secs(86_400);

fn nl_schema_prompt_cached(
    collection_name: &str,
    schema: &crate::schema::Schema,
    segment: &crate::index::Segment,
    ttl: std::time::Duration,
) -> String {
    let cache = NL_PROMPT_CACHE.get_or_init(|| std::sync::Mutex::new(Default::default()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((prompt, built_at)) = cache.get(collection_name) {
        if built_at.elapsed() < ttl {
            return prompt.clone();
        }
    }
    let prompt = nl_schema_prompt(schema, segment);
    cache.insert(collection_name.to_string(), (prompt.clone(), std::time::Instant::now()));
    prompt
}

/// The system prompt of an NL translation: the collection's fields with their common values, so
/// the model uses real ones, what the filter and sort syntax allow, and the JSON to answer with.
fn nl_schema_prompt(schema: &crate::schema::Schema, segment: &crate::index::Segment) -> String {
    let mut p = String::with_capacity(2048);
    p.push_str("You turn a person's search request into search parameters for one collection of documents.\n\n");
    p.push_str("The collection's fields, each with its type, whether it can be filtered on, and for some its most common values:\n");
    for field in &schema.fields {
        if field.field_type.is_dynamic_marker() {
            continue;
        }
        let type_name = serde_json::to_value(&field.field_type)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", field.field_type));
        p.push_str(&format!("- {} ({type_name}{})", field.name, if field.index { ", filterable" } else { "" }));
        if field.facet && type_name.starts_with("string") {
            if let Some(idx) = segment.values.get(&field.id).filter(|i| !i.is_empty()) {
                let mut counts: Vec<(String, u64)> = Vec::new();
                idx.for_each_str(|value, ord| {
                    counts.push((value.to_string(), idx.df(ord)));
                    true
                });
                counts.sort_by(|a, b| b.1.cmp(&a.1));
                let more = counts.len() > 10;
                let vals: Vec<String> = counts.into_iter().take(10).map(|(v, _)| v).collect();
                p.push_str(&format!(": {}{}", vals.join(", "), if more { ", and others" } else { "" }));
            }
        }
        p.push('\n');
    }
    p.push_str("\nRead what the request means, not only what it says, and pick the listed value it comes closest to when it words one differently.\n");
    p.push_str("\nfilter_by is a list of conditions:\n");
    p.push_str("- a value: field:value, or any of several: field:[a,b]\n");
    p.push_str("- numbers: field:>10, field:<=10, field:=10, or a range: field:[10..20]\n");
    p.push_str("- not a value: field:!=value, or none of several: field:!=[a,b]\n");
    p.push_str("- all of two conditions: a && b; either: a || b\n");
    p.push_str("- a value holding brackets or commas goes in backticks: field:`a (b)`\n");
    p.push_str("\nsort_by is field:asc or field:desc, up to three of them separated by commas.\n");
    p.push_str("\nAnswer with one JSON object and nothing else, leaving out what the request does not call for:\n");
    p.push_str("{\n");
    p.push_str("  \"q\": \"the words to search the text for, without anything already expressed in filter_by or sort_by\",\n");
    p.push_str("  \"filter_by\": \"the conditions\",\n");
    p.push_str("  \"sort_by\": \"the sort order\"\n");
    p.push_str("}\n");
    p
}

/// What an `nl_query=true` search's translation did, for its `parsed_nl_query`.
struct NlOutcome {
    time_ms: u64,
    /// The query as asked (the response's `request_params.q`).
    original_q: String,
    /// The parameters the model gave.
    generated: serde_json::Map<String, Value>,
    /// q, filter_by and sort_by as searched.
    augmented: serde_json::Map<String, Value>,
    error: Option<String>,
    /// nl_query_debug: the model's answer.
    llm_response: Option<Value>,
}

impl NlOutcome {
    /// Typesense's `parsed_nl_query`.
    fn report(&self) -> Value {
        let mut r = serde_json::Map::new();
        if self.time_ms > 0 {
            r.insert("parse_time_ms".into(), json!(self.time_ms));
        }
        r.insert("generated_params".into(), Value::Object(self.generated.clone()));
        if let Some(e) = &self.error {
            r.insert("error".into(), json!(e));
        }
        r.insert("augmented_params".into(), Value::Object(self.augmented.clone()));
        if let Some(l) = &self.llm_response {
            r.insert("llm_response".into(), l.clone());
        }
        Value::Object(r)
    }
}

/// nl_query=true (Typesense's natural-language search): the query translated by the NL model
/// (`nl_model_id`, by default the one named `default`), prompted with the collection's schema, into
/// search parameters merged into the search's: its q, its filter_by ANDed with the search's, its
/// sort_by and any other it gives. When translation fails the search runs as asked and the error is
/// reported. None without nl_query or a query.
async fn nl_augment(state: &Arc<AppState>, collection: &str, raw: &mut RawParams) -> Option<NlOutcome> {
    if raw.get("nl_query").map(String::as_str) != Some("true") {
        return None;
    }
    let original_q = raw.get("q").filter(|q| !q.is_empty())?.clone();
    let start = Instant::now();
    let debug = raw.get("nl_query_debug").is_some_and(|v| v == "true" || v == "1");
    let translated = nl_translate(state, collection, raw, &original_q).await;
    let mut outcome = NlOutcome {
        time_ms: start.elapsed().as_millis() as u64,
        original_q,
        generated: serde_json::Map::new(),
        augmented: serde_json::Map::new(),
        error: None,
        llm_response: None,
    };
    match translated {
        Ok((generated, answer)) => {
            for (k, v) in generated {
                let Some(text) = presets::param_text(&v) else { continue };
                match k.as_str() {
                    "llm_response" => continue,
                    "filter_by" => {
                        let combined = match (raw.get("filter_by").map(|f| f.trim()).filter(|f| !f.is_empty()), text.trim()) {
                            (Some(own), "") => own.to_string(),
                            (Some(own), generated) => format!("({own}) && ({generated})"),
                            (None, generated) => generated.to_string(),
                        };
                        raw.insert(k.clone(), combined);
                    }
                    _ => {
                        raw.insert(k.clone(), text);
                    }
                }
                outcome.generated.insert(k, v);
            }
            if debug {
                outcome.llm_response = Some(Value::String(answer));
            }
        }
        Err(e) => outcome.error = Some(e),
    }
    for k in ["q", "filter_by", "sort_by"] {
        if let Some(v) = raw.get(k) {
            outcome.augmented.insert(k.into(), Value::String(v.clone()));
        }
    }
    Some(outcome)
}

/// The NL model's parameters for `q`, and its answer.
async fn nl_translate(state: &Arc<AppState>, collection: &str, raw: &RawParams, q: &str) -> Result<(serde_json::Map<String, Value>, String), String> {
    let model_id = raw.get("nl_model_id").map(String::as_str).filter(|m| !m.is_empty()).unwrap_or("default");
    let model = state.llm.get_nl_model(model_id).ok_or("Error getting natural language search model: Model not found.")?;
    let resolved = state.aliases.resolve(collection);
    let coll = state.collections.get(&resolved).ok_or_else(|| format!("Error generating schema prompt: Collection `{collection}` not found."))?;
    let ttl = raw.get("nl_query_prompt_cache_ttl").and_then(|v| v.parse::<u64>().ok()).map_or(NL_PROMPT_TTL, std::time::Duration::from_secs);
    let system = match model.system_prompt.clone() {
        Some(p) => p,
        None => nl_schema_prompt_cached(&resolved, &coll.schema, &coll.snapshot_bundle().primary_segment_arc(), ttl),
    };
    let messages = vec![
        crate::llm::ChatMessage { role: "system".into(), content: system },
        crate::llm::ChatMessage { role: "user".into(), content: q.to_string() },
    ];
    // Typesense sends max_tokens = max_bytes and temperature (defaulting to
    // 0), with reasoning-model renames handled inside chat_complete.
    let answer = tokio::task::spawn_blocking(move || {
        crate::llm::chat_complete(&model.model_name, model.api_url.as_deref(), model.api_key.as_deref(), Some(model.temperature.unwrap_or(0.0)), model.max_bytes, &messages)
    })
    .await
    .map_err(|e| format!("NL query task failed: {e}"))?
    .map_err(|e| format!("Error generating search parameters: {e}"))?;
    let parsed = crate::llm::extract_json_object(&answer).map_err(|e| format!("Error generating search parameters: {e}"))?;
    match parsed {
        Value::Object(o) => Ok((o, answer)),
        _ => Err("Error generating search parameters: the model's answer is not an object.".into()),
    }
}

/// A search's conversation (Typesense's conversational search): its model, the question as asked,
/// the conversation it continues and whether the answer streams.
struct Conversation {
    model: crate::llm::ConversationModel,
    question: String,
    id: Option<String>,
    stream: bool,
}

/// `conversation=true`: the model, and for a follow-up (`conversation_id`) the question and the
/// history rewritten by a separate model call into a standalone question, which the search runs
/// in its place. A multi_search's (`multi`) comes from its URL, with Typesense's messages for it.
async fn conversation_of(state: &Arc<AppState>, raw: &mut RawParams, multi: bool) -> Result<Option<Conversation>, AppError> {
    let bad = |m: &str| AppError::BadRequest(m.to_string());
    let id = raw.get("conversation_id").cloned();
    if raw.get("conversation").map(String::as_str) != Some("true") {
        return match id {
            Some(_) if multi => Err(bad("`conversation_id` can only be used if `conversation` is enabled.")),
            Some(_) => Err(bad("Conversation ID provided but conversation is not enabled for this collection.")),
            None => Ok(None),
        };
    }
    let question = match raw.get("q") {
        Some(q) => q.clone(),
        None if multi => {
            return Err(bad("`q` parameter has to be common for all searches if conversation is enabled. Please set `q` as a query parameter in the request, instead of inside the POST body"))
        }
        None => String::new(),
    };
    let model_id = raw.get("conversation_model_id").ok_or_else(|| {
        bad(if multi { "`conversation_model_id` is needed if conversation is enabled." } else { "Conversation is enabled but no conversation model ID is provided." })
    })?;
    let model = state.llm.get_conversation_model(model_id).ok_or_else(|| {
        if multi {
            bad("`conversation_model_id` is invalid.")
        } else {
            AppError::BadRequest(format!("Conversation model `{model_id}` not found."))
        }
    })?;
    if let Some(cid) = &id {
        let now_ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let history = conversation_history_merged(state, &model, cid, now_ts);
        if history.is_empty() {
            return Err(bad(if multi { "`conversation_id` is invalid." } else { "Conversation not found." }));
        }
        let prompt = crate::llm::build_standalone_prompt(&history, &question, model.max_bytes);
        let call_model = model.clone();
        let messages = vec![crate::llm::ChatMessage { role: "user".into(), content: prompt }];
        let standalone = tokio::task::spawn_blocking(move || {
            crate::llm::chat_complete(&call_model.model_name, crate::llm::vllm_chat_url(&call_model.vllm_url).as_deref(), call_model.api_key.as_deref(), None, None, &messages)
        })
        .await
        .map_err(|e| AppError::Internal(format!("Standalone question task failed: {e}")))?
        .map_err(AppError::BadRequest)?;
        let standalone = standalone.trim();
        if !standalone.is_empty() {
            raw.insert("q".into(), standalone.to_string());
        }
    }
    let stream = raw.get("conversation_stream").map(String::as_str) == Some("true");
    Ok(Some(Conversation { model, question, id, stream }))
}

/// Role-keyed history objects (`[{"user": ...}, {"assistant": ...}]`) —
/// the shape Typesense returns in `conversation_history` and stores in the
/// history collection.
fn role_keyed_history(history: &[crate::llm::ChatMessage]) -> Vec<Value> {
    history
        .iter()
        .map(|m| json!({ m.role.clone(): m.content }))
        .collect()
}

/// Prior messages for a conversation: the in-memory cache first, falling
/// back to the model's history collection (docs `{conversation_id, role,
/// message, timestamp}` sorted by timestamp — the durable store Typesense
/// reads). A collection hit re-seeds the cache.
fn conversation_history_merged(
    state: &Arc<AppState>,
    model: &crate::llm::ConversationModel,
    conversation_id: &str,
    now_ts: i64,
) -> Vec<crate::llm::ChatMessage> {
    let cached = state.llm.conversation_history(conversation_id, now_ts);
    if !cached.is_empty() {
        return cached;
    }
    let Some(ref history_collection) = model.history_collection else {
        return Vec::new();
    };
    let resolved = state.aliases.resolve(history_collection);
    let Some(collection) = state.collections.get(&resolved) else {
        return Vec::new();
    };
    let mut rows: Vec<(i64, String, String)> = Vec::new();
    let mut collect = |doc: &crate::schema::Document| {
        let j = document_to_json(doc, &collection.schema);
        if j.get("conversation_id").and_then(|v| v.as_str()) != Some(conversation_id) {
            return;
        }
        let (Some(role), Some(message)) = (
            j.get("role").and_then(|v| v.as_str()),
            j.get("message").and_then(|v| v.as_str()),
        ) else {
            return;
        };
        let ts = j.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
        rows.push((ts, role.to_string(), message.to_string()));
    };
    // the conversation's live documents in every segment (found by the
    // filter where the segment indexes the field, its tokens; `collect` compares the id itself)
    let view = collection.view();
    let field = collection.schema.name_to_id.get("conversation_id").copied();
    let filter = crate::filter::parser::parse_filter(&format!("conversation_id:=`{conversation_id}`"), &collection.schema).ok().flatten();
    for (seg, deleted) in view.segments() {
        let docs = match (&filter, field) {
            (Some(op), Some(f)) if seg.inverted.contains_key(&f) || seg.values.contains_key(&f) => {
                crate::filter::ops::execute_filter(op, seg, &collection.schema)
            }
            _ => seg.all_docs.clone(),
        };
        for d in docs.iter().filter(|&d| !deleted.is_some_and(|m| m.contains(d))) {
            if let Some(doc) = seg.get_doc(d, &collection.schema) {
                collect(&doc);
            }
        }
    }
    rows.sort_by_key(|(ts, _, _)| *ts);
    let messages: Vec<crate::llm::ChatMessage> = rows
        .into_iter()
        .map(|(_, role, content)| crate::llm::ChatMessage { role, content })
        .collect();
    if !messages.is_empty() {
        let ttl = model.ttl.unwrap_or(crate::llm::DEFAULT_CONVERSATION_TTL_SECS);
        state
            .llm
            .seed_conversation(conversation_id, messages.clone(), ttl, now_ts);
    }
    messages
}

/// Write a question/answer exchange into the model's history collection as
/// `{conversation_id, model_id, role, message, timestamp}` docs (the
/// durable store Typesense uses). Best-effort: a missing collection is
/// logged, not fatal.
fn persist_history_exchange(
    state: &Arc<AppState>,
    model: &crate::llm::ConversationModel,
    conversation_id: &str,
    question: &str,
    answer: &str,
    now_ts: i64,
) {
    let Some(ref history_collection) = model.history_collection else {
        return;
    };
    let resolved = state.aliases.resolve(history_collection);
    let Some(collection) = state.collections.get(&resolved) else {
        tracing::warn!(
            model = model.id.as_str(),
            history_collection = history_collection.as_str(),
            "conversation history collection missing; history kept in memory only"
        );
        return;
    };
    let mut docs = Vec::with_capacity(2);
    for (role, message) in [("user", question), ("assistant", answer)] {
        let doc_json = json!({
            "id": crate::llm::generate_id(),
            "conversation_id": conversation_id,
            "model_id": model.id,
            "role": role,
            "message": message,
            "timestamp": now_ts,
        });
        match ingest::parse_json_document(&doc_json, &collection.schema, 0) {
            Ok(doc) => docs.push(doc),
            Err(e) => tracing::warn!(
                model = model.id.as_str(),
                error = %e,
                "history collection schema rejected conversation message"
            ),
        }
    }
    bulk_upsert_flush_docs(state, &collection, docs);
}

/// A conversation's context from search results: their documents, vector fields left out.
fn context_docs<'a>(schema: &crate::schema::Schema, docs: impl Iterator<Item = &'a Value>) -> Vec<Value> {
    let vector_fields: Vec<&str> = schema
        .fields
        .iter()
        .filter(|f| f.num_dim.is_some())
        .map(|f| f.name.as_str())
        .collect();
    docs.map(|doc| match doc {
        Value::Object(obj) if !vector_fields.is_empty() => {
            let mut trimmed = obj.clone();
            for vf in &vector_fields {
                trimmed.remove(*vf);
            }
            Value::Object(trimmed)
        }
        other => other.clone(),
    })
    .collect()
}

/// A result's documents in order.
fn result_documents(result: &search::SearchResponse) -> Vec<&Value> {
    match result {
        search::SearchResponse::Ungrouped(r) => r.hits.iter().map(|h| &h.document).collect(),
        search::SearchResponse::Grouped(r) => r.grouped_hits.iter().flat_map(|g| g.hits.iter().map(|h| &h.document)).collect(),
    }
}

/// Bytes of a conversation model's answer request (`max_bytes`) its context may take.
fn context_budget(model: &crate::llm::ConversationModel, q: &str) -> usize {
    model.max_bytes.saturating_sub(
        crate::llm::DATA_STR.len()
            + crate::llm::QUESTION_STR.len()
            + crate::llm::ANSWER_STR.len()
            + q.len()
            + model.system_prompt.as_deref().map_or(0, str::len)
            + 2, // array brackets
    )
}

/// Build the Typesense-shape answer request: an optional system message
/// plus one user message of the documents as JSON and the question (`DATA_STR`), the
/// context capped by the model's `max_bytes`.
fn answer_messages(context: &[Value], model: &crate::llm::ConversationModel, effective_q: &str) -> Vec<crate::llm::ChatMessage> {
    let budget = context_budget(model, effective_q);
    let mut context_docs: Vec<String> = Vec::new();
    let mut used = 0usize;
    for doc in context {
        let line = doc.to_string();
        if used + line.len() + 1 > budget {
            break;
        }
        used += line.len() + 1;
        context_docs.push(line);
    }
    let context = format!("[{}]", context_docs.join(","));

    let mut messages = Vec::with_capacity(2);
    if let Some(ref sp) = model.system_prompt {
        messages.push(crate::llm::ChatMessage {
            role: "system".into(),
            content: sp.clone(),
        });
    }
    messages.push(crate::llm::ChatMessage {
        role: "user".into(),
        content: format!(
            "{}{}{}{}{}",
            crate::llm::DATA_STR,
            context,
            crate::llm::QUESTION_STR,
            effective_q,
            crate::llm::ANSWER_STR
        ),
    });
    messages
}

/// The answer request for one search's results.
fn build_answer_messages(
    schema: &crate::schema::Schema,
    result: &search::SearchResponse,
    model: &crate::llm::ConversationModel,
    effective_q: &str,
) -> Vec<crate::llm::ChatMessage> {
    answer_messages(&context_docs(schema, result_documents(result).into_iter()), model, effective_q)
}

/// A multi_search's context: each search's documents, while they are too long the lowest ranked
/// of the search with the most dropped (Typesense's trimming), then in search order.
fn balanced_context(mut per_search: Vec<Vec<Value>>, budget: usize) -> Vec<Value> {
    let mut sizes: Vec<Vec<usize>> = per_search.iter().map(|docs| docs.iter().map(|d| d.to_string().len() + 1).collect()).collect();
    let mut total: usize = sizes.iter().flatten().sum();
    while total > budget {
        let Some(k) = (0..per_search.len()).filter(|&k| !per_search[k].is_empty()).max_by_key(|&k| (per_search[k].len(), std::cmp::Reverse(k))) else { break };
        per_search[k].pop();
        total -= sizes[k].pop().unwrap_or(0);
    }
    per_search.into_iter().flatten().collect()
}

/// Stream a conversational answer as SSE (Typesense `conversation_stream`):
/// the upstream LLM is called with `stream: true`; its content deltas are
/// re-emitted as `data: {"conversation_id", "message"}` events, `[DONE]` is
/// forwarded, and the complete search-results JSON (with the accumulated
/// answer folded into `conversation`) is sent as the final event. History
/// is appended after the stream completes, exactly like the non-streaming
/// path (`conversation_history` left out unless `with_history`).
#[allow(clippy::too_many_arguments)]
async fn conversation_stream_response(
    state: Arc<AppState>,
    model: crate::llm::ConversationModel,
    raw_question: String,
    effective_q: String,
    conversation_id: String,
    messages: Vec<crate::llm::ChatMessage>,
    mut results_value: Value,
    with_history: bool,
) -> Result<axum::response::Response, AppError> {
    let (url, bare_name) = crate::llm::resolve_chat_endpoint(
        &model.model_name,
        crate::llm::vllm_chat_url(&model.vllm_url).as_deref(),
    )
    .map_err(AppError::BadRequest)?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let body = serde_json::json!({
            "model": bare_name,
            "messages": messages,
            "stream": true,
        });
        let mut req = agent.post(&url);
        if let Some(ref key) = model.api_key {
            if !key.is_empty() {
                req = req.header("Authorization", &format!("Bearer {key}"));
            }
        }
        let resp = match req.send_json(&body) {
            Ok(resp) => resp,
            Err(e) => {
                let _ = ready_tx.send(Err(format!("LLM request failed: {e}")));
                return;
            }
        };
        if resp.status().as_u16() != 200 {
            let status = resp.status().as_u16();
            let mut resp = resp;
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            let _ = ready_tx.send(Err(format!("LLM endpoint returned HTTP {status}: {text}")));
            return;
        }
        let _ = ready_tx.send(Ok(()));

        let mut reader = resp.into_body().into_reader();
        let mut pending = String::new();
        let mut answer = String::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
            let (deltas, done) = crate::llm::drain_sse_deltas(&mut pending, &chunk);
            if !deltas.is_empty() {
                let batch = deltas.concat();
                answer.push_str(&batch);
                let event = format!(
                    "data: {}\n\n",
                    serde_json::json!({
                        "conversation_id": conversation_id,
                        "message": batch,
                    })
                );
                if tx.blocking_send(Ok(event.into_bytes())).is_err() {
                    return; // client disconnected
                }
            }
            if done {
                let _ = tx.blocking_send(Ok(b"data: [DONE]\n\n".to_vec()));
            }
        }

        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let ttl = model
            .ttl
            .unwrap_or(crate::llm::DEFAULT_CONVERSATION_TTL_SECS);
        state
            .llm
            .append_exchange(&conversation_id, &raw_question, &answer, ttl, now_ts);
        persist_history_exchange(&state, &model, &conversation_id, &raw_question, &answer, now_ts);
        let full_history = state.llm.conversation_history(&conversation_id, now_ts);

        results_value["conversation"] = serde_json::json!({
            "answer": answer,
            "conversation_history": role_keyed_history(&full_history),
            "conversation_id": conversation_id,
            "query": effective_q,
        });
        if !with_history {
            if let Some(c) = results_value["conversation"].as_object_mut() {
                c.remove("conversation_history");
            }
        }
        let final_event = format!("data: {results_value}\n\n");
        let _ = tx.blocking_send(Ok(final_event.into_bytes()));
    });

    ready_rx
        .await
        .map_err(|_| AppError::Internal("Conversation stream task failed".into()))?
        .map_err(AppError::BadRequest)?;

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    axum::response::Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(axum::body::Body::from_stream(stream))
        .map_err(|e| AppError::Internal(e.to_string()))
}

/// The conversation's answer (Typesense's RAG step): the conversation model answers `messages`
/// (one user message of `<Data>{docs json}<Question>{q}<Answer>`, plus the model's system prompt
/// when set); the question as asked goes into the history, `conversation.query` echoes `query`,
/// and `conversation_history` is left out unless `with_history`.
async fn conversation_answer(
    state: &Arc<AppState>,
    messages: Vec<crate::llm::ChatMessage>,
    model: crate::llm::ConversationModel,
    raw_question: String,
    query: &str,
    conversation_id: Option<String>,
    with_history: bool,
) -> Result<Value, AppError> {
    let call_model = model.clone();
    let answer = tokio::task::spawn_blocking(move || {
        crate::llm::chat_complete(
            &call_model.model_name,
            crate::llm::vllm_chat_url(&call_model.vllm_url).as_deref(),
            call_model.api_key.as_deref(),
            None,
            None,
            &messages,
        )
    })
    .await
    .map_err(|e| AppError::Internal(format!("Conversation task failed: {e}")))?
    .map_err(AppError::BadRequest)?;

    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let conversation_id = conversation_id.unwrap_or_else(crate::llm::generate_id);
    let ttl = model.ttl.unwrap_or(crate::llm::DEFAULT_CONVERSATION_TTL_SECS);
    state
        .llm
        .append_exchange(&conversation_id, &raw_question, &answer, ttl, now_ts);
    persist_history_exchange(state, &model, &conversation_id, &raw_question, &answer, now_ts);
    let full_history = state.llm.conversation_history(&conversation_id, now_ts);

    let mut v = json!({
        "answer": answer,
        "conversation_history": role_keyed_history(&full_history),
        "conversation_id": conversation_id,
        "query": query,
    });
    if !with_history {
        v.as_object_mut().expect("object").remove("conversation_history");
    }
    Ok(v)
}

// -- Analytics handlers --

fn persist_analytics_rules(state: &AppState) {
    if let Err(e) =
        persistence::save_analytics_rules(&state.config.data_dir, &state.analytics.list_rules())
    {
        tracing::error!("Failed to persist analytics rules: {e}");
    }
}

/// POST /analytics/rules accepts a single rule or an array of rules
/// (Typesense v30.1 `oneOf`); the response mirrors the request shape.
async fn create_analytics_rules(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let rules: Vec<crate::analytics::AnalyticsRule> = if body.is_array() {
        serde_json::from_value(body.clone()).map_err(|e| AppError::BadRequest(e.to_string()))?
    } else {
        vec![serde_json::from_value(body.clone()).map_err(|e| AppError::BadRequest(e.to_string()))?]
    };
    for rule in &rules {
        rule.validate().map_err(AppError::BadRequest)?;
    }
    for rule in rules {
        state
            .analytics
            .upsert_rule(rule)
            .map_err(AppError::BadRequest)?;
    }
    persist_analytics_rules(&state);
    Ok(Json(body))
}

async fn list_analytics_rules(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "rules": state.analytics.list_rules() }))
}

async fn upsert_analytics_rule(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> Result<Json<crate::analytics::AnalyticsRule>, AppError> {
    if let Some(obj) = body.as_object_mut() {
        obj.insert("name".into(), json!(name));
        // PUT is an upsert with partial-update semantics: missing fields
        // fall back to the existing rule's values.
        if let Some(existing) = state.analytics.get_rule(&name) {
            let existing = serde_json::to_value(&existing).unwrap_or_default();
            if let Some(existing) = existing.as_object() {
                for (k, v) in existing {
                    obj.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
    }
    let rule: crate::analytics::AnalyticsRule =
        serde_json::from_value(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
    state
        .analytics
        .upsert_rule(rule.clone())
        .map_err(AppError::BadRequest)?;
    persist_analytics_rules(&state);
    Ok(Json(rule))
}

async fn get_analytics_rule(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<crate::analytics::AnalyticsRule>, AppError> {
    state
        .analytics
        .get_rule(&name)
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("Analytics rule `{name}` not found.")))
}

async fn delete_analytics_rule(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    if !state.analytics.delete_rule(&name) {
        return Err(AppError::NotFound(format!(
            "Analytics rule `{name}` not found."
        )));
    }
    persist_analytics_rules(&state);
    Ok(Json(json!({ "name": name })))
}

async fn create_analytics_event(
    State(state): State<Arc<AppState>>,
    Json(event): Json<crate::analytics::AnalyticsEvent>,
) -> Result<Json<Value>, AppError> {
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    state
        .analytics
        .record_event(event, now_ts)
        .map_err(AppError::BadRequest)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AnalyticsEventsQuery {
    user_id: String,
    name: String,
    n: usize,
}

async fn list_analytics_events(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AnalyticsEventsQuery>,
) -> Json<Value> {
    Json(json!({ "events": state.analytics.events_for(&q.user_id, &q.name, q.n) }))
}

async fn analytics_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!(state.analytics.status()))
}

/// POST /analytics/flush — write pending aggregations to their destination
/// collections: popular/nohits query counts as `{id, q, count}` docs
/// (accumulating with any existing count), counter deltas onto the target
/// docs' counter_field.
async fn flush_analytics(State(state): State<Arc<AppState>>) -> Result<Json<Value>, AppError> {
    let pending = state.analytics.drain_pending();
    let state_clone = Arc::clone(&state);
    tokio::task::spawn_blocking(move || apply_analytics_flush(&state_clone, pending))
        .await
        .map_err(|e| AppError::Internal(format!("Analytics flush task failed: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

fn apply_analytics_flush(state: &Arc<AppState>, pending: crate::analytics::PendingFlush) {
    for (rule, counts) in pending.query_counts {
        let Some(dest) = rule
            .params
            .as_ref()
            .and_then(|p| p.destination_collection.clone())
        else {
            continue;
        };
        let resolved = state.aliases.resolve(&dest);
        let Some(collection) = state.collections.get(&resolved) else {
            tracing::warn!(
                rule = rule.name.as_str(),
                destination = dest.as_str(),
                "analytics flush: destination collection missing; dropping counts"
            );
            continue;
        };
        let mut docs = Vec::with_capacity(counts.len());
        let limit = rule.params.as_ref().and_then(|p| p.limit).unwrap_or(1000);
        for (q, count) in counts.into_iter().take(limit) {
            let existing_count = collection
                .document(&q)
                .map(|d| document_to_json(&d, &collection.schema))
                .and_then(|j| j.get("count").and_then(|c| c.as_u64()))
                .unwrap_or(0);
            let doc_json = json!({ "id": q, "q": q, "count": existing_count + count });
            match ingest::parse_json_document(&doc_json, &collection.schema, 0) {
                Ok(doc) => docs.push(doc),
                Err(e) => tracing::warn!(
                    rule = rule.name.as_str(),
                    error = %e,
                    "analytics flush: destination schema rejected query doc (needs `q` string + `count` int32)"
                ),
            }
        }
        bulk_upsert_flush_docs(state, &collection, docs);
    }

    for (rule, deltas) in pending.doc_counts {
        let Some(field) = rule
            .params
            .as_ref()
            .and_then(|p| p.counter_field.clone())
        else {
            continue;
        };
        let resolved = state.aliases.resolve(&rule.collection);
        let Some(collection) = state.collections.get(&resolved) else {
            tracing::warn!(
                rule = rule.name.as_str(),
                collection = rule.collection.as_str(),
                "analytics flush: counter collection missing; dropping deltas"
            );
            continue;
        };
        let mut docs = Vec::with_capacity(deltas.len());
        for (doc_id, delta) in deltas {
            let Some(existing) = collection.document(&doc_id) else {
                tracing::debug!(
                    rule = rule.name.as_str(),
                    doc = doc_id.as_str(),
                    "analytics flush: counter target doc missing; skipping"
                );
                continue;
            };
            let mut doc_json = document_to_json(&existing, &collection.schema);
            let prior = doc_json.get(&field).and_then(|v| v.as_i64()).unwrap_or(0);
            if let Some(obj) = doc_json.as_object_mut() {
                obj.insert(field.clone(), json!(prior + delta));
            }
            match ingest::parse_json_document(&doc_json, &collection.schema, 0) {
                Ok(doc) => docs.push(doc),
                Err(e) => tracing::warn!(
                    rule = rule.name.as_str(),
                    doc = doc_id.as_str(),
                    error = %e,
                    "analytics flush: counter update rejected by schema"
                ),
            }
        }
        bulk_upsert_flush_docs(state, &collection, docs);
    }
}

/// Upsert a batch of flush documents in one write (`write_documents`).
fn bulk_upsert_flush_docs(
    state: &Arc<AppState>,
    collection: &Arc<crate::collection::Collection>,
    docs: Vec<crate::schema::Document>,
) {
    // (durable with the log's next sync; nothing waits for it)
    if let Err(e) = apply_documents(state, collection, docs, Vec::new()) {
        tracing::warn!(collection = collection.name.as_str(), error = %e, "analytics flush: write refused; dropping");
    }
}

// -- Curation set handlers --

async fn upsert_curation_set(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(mut body): Json<crate::curation::CurationSet>,
) -> Result<Json<crate::curation::CurationSet>, AppError> {
    body.name = name;
    for item in &mut body.items {
        if item.id.is_empty() {
            return Err(AppError::BadRequest(
                "Every curation item must have an `id`.".into(),
            ));
        }
    }
    state.curations.upsert_set(body.clone());
    if let Err(e) = persistence::save_curations(&state.config.data_dir, &state.curations.list_sets())
    {
        tracing::error!("Failed to persist curation sets: {e}");
    }
    Ok(Json(body))
}

async fn get_curation_set(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<crate::curation::CurationSet>, AppError> {
    state
        .curations
        .get_set(&name)
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("Curation set `{name}` not found.")))
}

async fn list_curation_sets(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!(state.curations.list_sets()))
}

async fn delete_curation_set(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    if !state.curations.delete_set(&name) {
        return Err(AppError::NotFound(format!(
            "Curation set `{name}` not found."
        )));
    }
    if let Err(e) = persistence::save_curations(&state.config.data_dir, &state.curations.list_sets())
    {
        tracing::error!("Failed to persist curation sets: {e}");
    }
    Ok(Json(json!({ "name": name })))
}

async fn upsert_curation_item(
    State(state): State<Arc<AppState>>,
    Path((name, id)): Path<(String, String)>,
    Json(mut body): Json<crate::curation::CurationItem>,
) -> Result<Json<crate::curation::CurationItem>, AppError> {
    body.id = id;
    if !state.curations.upsert_item(&name, body.clone()) {
        return Err(AppError::NotFound(format!(
            "Curation set `{name}` not found."
        )));
    }
    if let Err(e) = persistence::save_curations(&state.config.data_dir, &state.curations.list_sets())
    {
        tracing::error!("Failed to persist curation sets: {e}");
    }
    Ok(Json(body))
}

async fn get_curation_item(
    State(state): State<Arc<AppState>>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<crate::curation::CurationItem>, AppError> {
    state
        .curations
        .get_item(&name, &id)
        .map(Json)
        .ok_or_else(|| {
            AppError::NotFound(format!("Curation item `{id}` not found in set `{name}`."))
        })
}

async fn list_curation_items(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    state
        .curations
        .list_items(&name)
        .map(|items| Json(json!(items)))
        .ok_or_else(|| AppError::NotFound(format!("Curation set `{name}` not found.")))
}

async fn delete_curation_item(
    State(state): State<Arc<AppState>>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    if !state.curations.delete_item(&name, &id) {
        return Err(AppError::NotFound(format!(
            "Curation item `{id}` not found in set `{name}`."
        )));
    }
    if let Err(e) = persistence::save_curations(&state.config.data_dir, &state.curations.list_sets())
    {
        tracing::error!("Failed to persist curation sets: {e}");
    }
    Ok(Json(json!({ "id": id })))
}

// -- Synonym handlers --

async fn upsert_synonym_set(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(mut body): Json<SynonymSet>,
) -> Json<SynonymSet> {
    body.name = name;
    state.synonyms.upsert_set(body.clone());

    if let Err(e) = persistence::save_synonyms(&state.config.data_dir, &state.synonyms.list_sets())
    {
        tracing::error!("Failed to persist synonyms: {e}");
    }

    Json(body)
}

async fn get_synonym_set(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<SynonymSet>, AppError> {
    state
        .synonyms
        .get_set(&name)
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("Synonym set `{name}` not found.")))
}

async fn list_synonym_sets(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "synonym_sets": state.synonyms.list_sets() }))
}

async fn delete_synonym_set(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    if !state.synonyms.delete_set(&name) {
        return Err(AppError::NotFound(format!(
            "Synonym set `{name}` not found."
        )));
    }

    if let Err(e) = persistence::save_synonyms(&state.config.data_dir, &state.synonyms.list_sets())
    {
        tracing::error!("Failed to persist synonyms: {e}");
    }

    Ok(Json(json!({ "name": name })))
}

async fn upsert_synonym_item(
    State(state): State<Arc<AppState>>,
    Path((name, id)): Path<(String, String)>,
    Json(mut body): Json<SynonymItem>,
) -> Result<Json<SynonymItem>, AppError> {
    body.id = id;
    if !state.synonyms.upsert_item(&name, body.clone()) {
        return Err(AppError::NotFound(format!(
            "Synonym set `{name}` not found."
        )));
    }

    if let Err(e) = persistence::save_synonyms(&state.config.data_dir, &state.synonyms.list_sets())
    {
        tracing::error!("Failed to persist synonyms: {e}");
    }

    Ok(Json(body))
}

async fn get_synonym_item(
    State(state): State<Arc<AppState>>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<SynonymItem>, AppError> {
    state
        .synonyms
        .get_item(&name, &id)
        .map(Json)
        .ok_or_else(|| {
            AppError::NotFound(format!("Synonym item `{id}` not found in set `{name}`."))
        })
}

async fn list_synonym_items(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Vec<SynonymItem>>, AppError> {
    state
        .synonyms
        .list_items(&name)
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("Synonym set `{name}` not found.")))
}

async fn delete_synonym_item(
    State(state): State<Arc<AppState>>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    if !state.synonyms.delete_item(&name, &id) {
        return Err(AppError::NotFound(format!(
            "Synonym item `{id}` not found in set `{name}`."
        )));
    }

    if let Err(e) = persistence::save_synonyms(&state.config.data_dir, &state.synonyms.list_sets())
    {
        tracing::error!("Failed to persist synonyms: {e}");
    }

    Ok(Json(json!({ "id": id })))
}

// -- Preset handlers --

async fn upsert_preset(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let Some(value) = body.get("value").cloned() else {
        return Err(AppError::BadRequest("Parameter `value` is required.".into()));
    };
    state.presets.upsert(&name, value);
    if let Err(e) = persistence::save_presets(&state.config.data_dir, &state.presets.list()) {
        tracing::error!("Failed to persist presets: {e}");
    }
    if let Some(obj) = body.as_object_mut() {
        obj.insert("name".into(), Value::String(name));
    }
    Ok(Json(body))
}

async fn get_preset(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    let value = state.presets.get(&name).ok_or_else(|| AppError::NotFound("Not found.".into()))?;
    Ok(Json(json!({ "name": name, "value": value })))
}

async fn list_presets(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "presets": state.presets.list() }))
}

async fn delete_preset(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    let value = state.presets.delete(&name).ok_or_else(|| AppError::NotFound("Not found.".into()))?;
    if let Err(e) = persistence::save_presets(&state.config.data_dir, &state.presets.list()) {
        tracing::error!("Failed to persist presets: {e}");
    }
    Ok(Json(json!({ "name": name, "value": value })))
}

// -- Stopword handlers --

// -- Operations + runtime config (Typesense v30.1 /operations/*, /config) --

#[derive(Deserialize)]
struct SnapshotParams {
    snapshot_path: Option<String>,
}

/// Force-persist every collection, then optionally copy the data directory
/// to `snapshot_path` for a point-in-time backup.
async fn operations_snapshot(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SnapshotParams>,
) -> Result<Json<Value>, AppError> {
    let data_dir = state.config.data_dir.clone();
    if let Some(ref p) = params.snapshot_path {
        let snap = std::path::Path::new(p);
        if snap.starts_with(&data_dir) {
            return Err(AppError::BadRequest(
                "snapshot_path must be outside the data directory.".into(),
            ));
        }
    }

    let collections = state.collections.list();
    let snapshot_path = params.snapshot_path.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        for col in &collections {
            if !col.schema.persistence_enabled {
                continue;
            }
            col.save(&data_dir).map_err(|e| format!("persist `{}`: {e}", col.name))?;
        }
        if let Some(p) = snapshot_path {
            copy_dir_recursive(&data_dir, std::path::Path::new(&p))
                .map_err(|e| format!("copy to snapshot_path: {e}"))?;
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("Snapshot task failed: {e}")))?;

    result.map_err(AppError::Internal)?;
    Ok(Json(json!({ "success": true })))
}

#[derive(Deserialize)]
struct QuiesceParams {
    timeout_ms: Option<u64>,
}

/// POST /operations/quiesce — barrier for host-driven stops (VM upgrades,
/// planned kills). Waits for all in-flight index builds, background saves,
/// and TCP ingest sessions to drain, flushes analytics, drains every
/// realtime write-ahead log, persists every collection synchronously, and
/// returns a doc-count inventory for post-restart verification.
///
/// Contract: after a 200, SIGKILL loses no acknowledged data. The caller
/// is responsible for stopping new writes first — quiesce does not fence
/// them, so a write racing the barrier is simply not covered by it.
async fn operations_quiesce(
    State(state): State<Arc<AppState>>,
    Query(params): Query<QuiesceParams>,
) -> Result<Json<Value>, AppError> {
    let timeout = std::time::Duration::from_millis(params.timeout_ms.unwrap_or(120_000));
    let deadline = std::time::Instant::now() + timeout;

    // Drain twice around the analytics flush: the flush upserts into
    // destination collections, which spawns background saves of its own.
    drain_in_flight(&state, deadline).await?;
    let pending = state.analytics.drain_pending();
    let state_clone = Arc::clone(&state);
    tokio::task::spawn_blocking(move || apply_analytics_flush(&state_clone, pending))
        .await
        .map_err(|e| AppError::Internal(format!("Analytics flush task failed: {e}")))?;
    drain_in_flight(&state, deadline).await?;

    // Write-ahead logs: every write appended so far on disk
    for col in state.collections.list() {
        col.wal_barrier().await;
    }

    // Persist-all, synchronous (same path as /operations/snapshot).
    let collections = state.collections.list();
    let data_dir = state.config.data_dir.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        for col in &collections {
            if !col.schema.persistence_enabled {
                continue;
            }
            col.save(&data_dir).map_err(|e| format!("persist `{}`: {e}", col.name))?;
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("Quiesce persist task failed: {e}")))?;
    result.map_err(AppError::Internal)?;

    let inventory: Vec<Value> = state
        .collections
        .list()
        .iter()
        .map(|c| {
            let num_documents = c.snapshot_bundle().total_docs() as u64;
            json!({
                "name": c.name,
                "num_documents": num_documents,
                "persistence_enabled": c.schema.persistence_enabled,
            })
        })
        .collect();

    Ok(Json(json!({ "success": true, "inventory": inventory })))
}

async fn drain_in_flight(
    state: &Arc<AppState>,
    deadline: std::time::Instant,
) -> Result<(), AppError> {
    loop {
        let builds = state.metrics.index_builds_in_flight.get();
        let saves = state.metrics.index_saves_in_flight.get();
        let sessions = state.metrics.ingest_active_sessions.get();
        if builds == 0 && saves == 0 && sessions == 0 {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(AppError::ServiceUnavailable(format!(
                "quiesce timed out: index_builds={builds} index_saves={saves} ingest_sessions={sessions}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "torque.lock" {
            continue;
        }
        let to = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Flush the segments in memory of every realtime collection to the disk.
async fn operations_db_compact(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let mut compacted = 0usize;
    for col in state.collections.list() {
        if col.schema.index_mode != crate::schema::IndexMode::Realtime {
            continue;
        }
        let s = Arc::clone(&state);
        tokio::task::spawn_blocking(move || crate::collection::compaction::flush(&col, Some(&s.config.data_dir), &s.metrics))
            .await
            .map_err(|e| AppError::Internal(format!("Compaction task failed: {e}")))?;
        compacted += 1;
    }
    Ok(Json(json!({ "success": true, "compacted_collections": compacted })))
}

async fn operations_cache_clear(State(state): State<Arc<AppState>>) -> Json<Value> {
    state.search_cache.clear();
    Json(json!({ "success": true }))
}

/// Schema changes are applied synchronously (PATCH rebuilds inline), so
/// there are never in-flight changes to report.
async fn operations_schema_changes() -> Json<Value> {
    Json(json!([]))
}

/// Typesense-shaped system metrics. System values are string-encoded like
/// Typesense's metrics.json.
async fn metrics_json(State(state): State<Arc<AppState>>) -> Json<Value> {
    let (disk_free, disk_total) = crate::memory::disk_space();
    let mem_total = crate::memory::total_memory_bytes();
    let mem_available = crate::memory::available_memory_bytes();
    let rss = crate::memory::process_rss().unwrap_or(0);

    let mut m = serde_json::Map::new();
    m.insert("system_disk_total_bytes".into(), json!(disk_total.to_string()));
    m.insert(
        "system_disk_used_bytes".into(),
        json!(disk_total.saturating_sub(disk_free).to_string()),
    );
    m.insert("system_memory_total_bytes".into(), json!(mem_total.to_string()));
    m.insert(
        "system_memory_used_bytes".into(),
        json!(mem_total.saturating_sub(mem_available).to_string()),
    );
    m.insert("typesense_memory_resident_bytes".into(), json!(rss.to_string()));

    #[cfg(feature = "jemalloc")]
    {
        use tikv_jemalloc_ctl::{epoch, stats};
        epoch::advance().ok();
        m.insert(
            "typesense_memory_active_bytes".into(),
            json!(stats::active::read().unwrap_or(0).to_string()),
        );
        m.insert(
            "typesense_memory_allocated_bytes".into(),
            json!(stats::allocated::read().unwrap_or(0).to_string()),
        );
        m.insert(
            "typesense_memory_mapped_bytes".into(),
            json!(stats::mapped::read().unwrap_or(0).to_string()),
        );
        m.insert(
            "typesense_memory_retained_bytes".into(),
            json!(stats::retained::read().unwrap_or(0).to_string()),
        );
        m.insert(
            "typesense_memory_metadata_bytes".into(),
            json!(stats::metadata::read().unwrap_or(0).to_string()),
        );
    }

    m.insert(
        "uptime_seconds".into(),
        json!(state.start_time.elapsed().as_secs().to_string()),
    );
    Json(Value::Object(m))
}

#[derive(Deserialize)]
struct RuntimeConfigBody {
    #[serde(rename = "log-slow-requests-time-ms")]
    log_slow_requests_time_ms: Option<i64>,
}

async fn update_runtime_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RuntimeConfigBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let Some(ms) = body.log_slow_requests_time_ms else {
        return Err(AppError::BadRequest(
            "Unsupported config. Supported: log-slow-requests-time-ms.".into(),
        ));
    };
    // Typesense uses -1 to disable; Torque's internal convention is 0.
    let v = if ms < 0 { 0 } else { ms as u64 };
    state
        .slow_query_ms
        .store(v, std::sync::atomic::Ordering::Relaxed);
    Ok((StatusCode::CREATED, Json(json!({ "success": true }))))
}

// -- Stemming dictionaries (Typesense v30.1 /stemming/dictionaries) --

async fn list_stemming_dictionaries() -> Json<Value> {
    Json(json!({ "dictionaries": crate::stemming::global().list_ids() }))
}

async fn get_stemming_dictionary(
    Path(id): Path<String>,
) -> Result<Json<crate::stemming::StemmingDictionary>, AppError> {
    crate::stemming::global()
        .get(&id)
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("Stemming dictionary `{id}` not found.")))
}

#[derive(Deserialize)]
struct StemmingImportParams {
    id: String,
}

/// JSONL body of `{"word": ..., "root": ...}` lines. Echoes the accepted
/// lines back as JSONL, mirroring Typesense's import response.
async fn import_stemming_dictionary(
    State(state): State<Arc<AppState>>,
    Query(params): Query<StemmingImportParams>,
    body: String,
) -> Result<(StatusCode, String), AppError> {
    let mut words = Vec::new();
    let mut echoed = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let word: crate::stemming::DictionaryWord = serde_json::from_str(line)
            .map_err(|e| AppError::BadRequest(format!("Invalid dictionary line `{line}`: {e}")))?;
        echoed.push(serde_json::to_string(&word).unwrap());
        words.push(word);
    }
    if words.is_empty() {
        return Err(AppError::BadRequest(
            "Dictionary import body must contain at least one {\"word\", \"root\"} line.".into(),
        ));
    }
    crate::stemming::global().import(&params.id, words);

    if let Err(e) = persistence::save_stemming_dictionaries(
        &state.config.data_dir,
        &crate::stemming::global().dump(),
    ) {
        tracing::error!("Failed to persist stemming dictionaries: {e}");
    }

    Ok((StatusCode::OK, echoed.join("\n")))
}

async fn upsert_stopword_set(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(mut body): Json<StopwordSet>,
) -> Json<StopwordSet> {
    body.id = id;
    state.stopwords.upsert(body.clone());

    if let Err(e) = persistence::save_stopwords(&state.config.data_dir, &state.stopwords.list()) {
        tracing::error!("Failed to persist stopwords: {e}");
    }

    Json(body)
}

async fn get_stopword_set(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<StopwordSet>, AppError> {
    state
        .stopwords
        .get(&id)
        .map(Json)
        .ok_or_else(|| AppError::NotFound(format!("Stopword set `{id}` not found.")))
}

async fn list_stopwords(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "stopwords": state.stopwords.list() }))
}

async fn delete_stopword_set(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    if !state.stopwords.delete(&id) {
        return Err(AppError::NotFound(format!(
            "Stopword set `{id}` not found."
        )));
    }

    if let Err(e) = persistence::save_stopwords(&state.config.data_dir, &state.stopwords.list()) {
        tracing::error!("Failed to persist stopwords: {e}");
    }

    Ok(Json(json!({ "id": id })))
}

// -- Search handler --

#[derive(Deserialize)]
#[allow(dead_code)]
struct SearchQueryParams {
    /// A multi_search entry's collection.
    collection: Option<String>,
    #[serde(default)]
    q: String,
    #[serde(default)]
    query_by: String,
    filter_by: Option<String>,
    sort_by: Option<String>,
    facet_by: Option<String>,
    per_page: Option<usize>,
    page: Option<usize>,
    highlight_fields: Option<String>,
    group_by: Option<String>,
    group_limit: Option<usize>,
    group_missing_values: Option<bool>,
    text_match_type: Option<String>,
    stopwords: Option<String>,
    synonym_sets: Option<String>,
    preset: Option<String>,
    #[serde(default, deserialize_with = "de_list")]
    num_typos: Option<String>,
    typo_tokens_threshold: Option<usize>,
    #[serde(default, deserialize_with = "de_list")]
    prefix: Option<String>,
    vector_query: Option<String>,
    max_facet_values: Option<usize>,
    facet_query: Option<String>,
    facet_query_num_typos: Option<usize>,
    highlight_start_tag: Option<String>,
    highlight_end_tag: Option<String>,
    highlight_affix_num_tokens: Option<usize>,
    include_fields: Option<String>,
    exclude_fields: Option<String>,
    query_by_weights: Option<String>,
    pinned_hits: Option<String>,
    hidden_hits: Option<String>,
    offset: Option<usize>,
    limit_hits: Option<usize>,
    drop_tokens_threshold: Option<usize>,
    drop_tokens_mode: Option<String>,
    min_len_1typo: Option<usize>,
    min_len_2typo: Option<usize>,
    split_join_tokens: Option<String>,
    enable_typos_for_numerical_tokens: Option<bool>,
    enable_typos_for_alpha_numerical_tokens: Option<bool>,
    synonym_num_typos: Option<usize>,
    highlight_full_fields: Option<String>,
    snippet_threshold: Option<usize>,
    prioritize_exact_match: Option<bool>,
    exhaustive_search: Option<bool>,
    search_cutoff_ms: Option<usize>,
    max_candidates: Option<usize>,
    // facet counts are exact at any size: the sample Typesense estimates from is not taken
    facet_sample_percent: Option<usize>,
    facet_sample_threshold: Option<usize>,
    facet_return_parent: Option<String>,
    use_cache: Option<bool>,
    cache_ttl: Option<usize>,
    validate_field_names: Option<bool>,
    enable_synonyms: Option<bool>,
    infix: Option<String>,
    max_extra_prefix: Option<usize>,
    max_extra_suffix: Option<usize>,
    /// Alias for per_page (Typesense v30.1).
    limit: Option<usize>,
    // Typesense v30.1 compat: accepted but not used.
    // pre_segmented_query has no effect (the tokenizer is already
    // whitespace-driven, there is no CJK segmenter to bypass);
    // enable_analytics is a no-op until the analytics API lands;
    // dirty_values coercion is not implemented (documents are validated
    // against the schema and rejected on mismatch).
    synonym_prefix: Option<bool>,
    /// Torque extension: `text_match_info.matches` per hit.
    explain_matches: Option<bool>,
    /// Torque extension: off, fallback or always per query_by field.
    name_matching: Option<String>,
    /// Torque extension: the first candidates scored and ordered by name.
    name_score: Option<bool>,
    name_score_candidates: Option<usize>,
    name_score_kind: Option<String>,
    pre_segmented_query: Option<bool>,
    enable_analytics: Option<bool>,
    // facet_strategy: Torque has a single (exhaustive, bitmap-intersection)
    // facet algorithm, so exhaustive/top_values/automatic all behave alike.
    facet_strategy: Option<String>,
    // max_filter_by_candidates has no analog in bitmap-evaluated filters.
    prioritize_token_position: Option<bool>,
    prioritize_num_matching_fields: Option<bool>,
    max_filter_by_candidates: Option<usize>,
    enable_lazy_filter: Option<bool>,
    // Curations: enable_overrides is the pre-v30 alias for enable_curations;
    // either set to false disables curation evaluation for the query.
    enable_overrides: Option<bool>,
    enable_curations: Option<bool>,
    curation_tags: Option<String>,
    // Conversational + NL search (remote LLM, v1).
    conversation: Option<bool>,
    conversation_model_id: Option<String>,
    conversation_id: Option<String>,
    conversation_stream: Option<bool>,
    nl_query: Option<bool>,
    nl_model_id: Option<String>,
    nl_query_debug: Option<bool>,
    nl_query_prompt_cache_ttl: Option<u64>,
    filter_curated_hits: Option<bool>,
    enable_highlight_v1: Option<bool>,
    remote_embedding_timeout_ms: Option<usize>,
    remote_embedding_num_tries: Option<usize>,
}

/// A list parameter ("2,1"): a string, or in a JSON body a bare number or boolean.
fn de_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Scalar {
        S(String),
        B(bool),
        N(u64),
    }
    Ok(Option::<Scalar>::deserialize(d)?.map(|v| match v {
        Scalar::S(s) => s,
        Scalar::B(b) => b.to_string(),
        Scalar::N(n) => n.to_string(),
    }))
}

/// A search's parameters by name, as text (Typesense's `req_params`).
type RawParams = std::collections::BTreeMap<String, String>;

/// The query string's parameters.
fn query_params(raw: Option<&str>) -> Result<RawParams, AppError> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw.unwrap_or("")).map_err(|e| AppError::BadRequest(format!("Bad query string: {e}")))?;
    Ok(pairs.into_iter().collect())
}

/// A multi_search entry's parameters over `params` (the URL's): its own win.
fn json_params(params: &mut RawParams, obj: &serde_json::Map<String, Value>) -> Result<(), AppError> {
    for (k, v) in obj {
        let text = presets::param_text(v).ok_or_else(|| AppError::BadRequest("One or more search parameters are malformed.".into()))?;
        params.insert(k.clone(), text);
    }
    Ok(())
}

/// What lies over a search's own parameters, as Typesense layers them, every parameter alike: the
/// preset it names fills what they lack (explicit parameters override a preset's), then a scoped
/// API key's embedded parameters override both, its filter_by ANDed with the search's (the key's
/// own preset named first). The key's include and exclude lists restrict the search's in
/// `projection_of`, its limit_hits caps it in `apply_scoped_params`.
fn layer_params(params: &mut RawParams, store: &presets::PresetStore, scoped: Option<&ScopedKeyParams>) -> Result<(), AppError> {
    if let Some(name) = scoped.and_then(|s| s.other.get("preset")).and_then(presets::param_text) {
        params.insert("preset".into(), name);
    }
    if let Some(preset) = params.get("preset").and_then(|n| store.get(n)) {
        presets::fill(params, &preset).map_err(AppError::BadRequest)?;
    }
    if let Some(s) = scoped {
        for (k, v) in s.other.iter().filter(|(k, _)| *k != "preset") {
            let text = presets::param_text(v).ok_or_else(|| AppError::BadRequest("Error applying search parameters inside Scoped Search API key".into()))?;
            params.insert(k.clone(), text);
        }
        if let Some(f) = s.filter_by.as_deref().filter(|f| !f.trim().is_empty()) {
            let combined = match params.get("filter_by").filter(|e| !e.trim().is_empty()) {
                Some(own) => format!("({own}) && ({f})"),
                None => f.to_string(),
            };
            params.insert("filter_by".into(), combined);
        }
    }
    Ok(())
}

/// The parameters parsed as one search's, the text of each to its type as a query string's is.
fn parse_params(params: &RawParams) -> Result<SearchQueryParams, AppError> {
    let encoded = serde_urlencoded::to_string(params).map_err(|e| AppError::BadRequest(e.to_string()))?;
    serde_urlencoded::from_str(&encoded).map_err(|e| AppError::BadRequest(format!("Failed to parse search parameters: {e}")))
}

/// What hits return: the search's include and exclude lists, restricted by a scoped key's.
fn projection_of(params: &SearchQueryParams, scoped: Option<&ScopedKeyParams>) -> search::projection::Projection {
    let mut p = search::projection::Projection::new(params.include_fields.as_deref(), params.exclude_fields.as_deref());
    if let Some(s) = scoped {
        p.restrict(s.include_fields.as_deref(), s.exclude_fields.as_deref());
    }
    p
}

/// Whether the request's key may search `collection` (a multi_search names its collections in
/// the body, which the middleware does not read).
fn key_permits(key: Option<&ResolvedKey>, collection: &str) -> bool {
    match key {
        None | Some(ResolvedKey::Bootstrap) => true,
        Some(ResolvedKey::Direct(k)) | Some(ResolvedKey::Scoped { parent: k, .. }) => crate::auth::KeyStore::key_permits_collection(k, collection),
    }
}

/// Hits a page may hold (Typesense's `max_per_page`).
const MAX_PER_PAGE: usize = 250;

/// `per_page` (or its alias `limit`), `page` and `offset` as Typesense resolves them: per_page
/// defaults to 10 (0 with a facet query), may be 0 and at most 250 (else 422); `offset` counts
/// only without `page`.
fn resolve_paging(
    per_page: Option<usize>,
    limit: Option<usize>,
    page: Option<usize>,
    offset: Option<usize>,
    facet_query: bool,
) -> Result<(usize, usize, Option<usize>), AppError> {
    let per_page = limit.or(per_page).unwrap_or(if facet_query { 0 } else { 10 });
    if per_page > MAX_PER_PAGE {
        return Err(AppError::Unprocessable(format!("Only upto {MAX_PER_PAGE} hits can be fetched per page.")));
    }
    let page_given = page.is_some_and(|p| p > 0);
    let offset = offset.filter(|&o| o > 0 && !page_given);
    Ok((per_page, page.filter(|&p| p > 0).unwrap_or(1), offset))
}

/// The `request_params` of a search response, as Typesense echoes them.
fn build_request_params(collection_name: &str, per_page: usize, q: &str) -> Value {
    json!({
        "collection_name": collection_name,
        "per_page": per_page,
        "q": q,
        "first_q": q,
    })
}

/// Split a comma-separated query-string value into trimmed, non-empty parts.
fn parse_csv_list(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Evaluate the collection's bound curation sets against the query and fold
/// the matched actions into `search_params`. Returns the matched item's
/// `metadata` for echoing in the search response.
fn apply_curations(
    state: &AppState,
    schema: &crate::schema::Schema,
    search_params: &mut search::SearchParams,
    enabled: bool,
    curation_tags: &[String],
) -> Option<Value> {
    if !enabled {
        return None;
    }
    let set_names = schema.curation_sets.as_ref()?;
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let outcome = state.curations.apply(
        set_names,
        &search_params.q,
        search_params.filter_by.as_deref(),
        curation_tags,
        now_ts,
    );

    if let Some(q) = outcome.replaced_query {
        search_params.q = q;
    } else if !outcome.remove_tokens.is_empty() {
        let kept: Vec<&str> = search_params
            .q
            .split_whitespace()
            .filter(|t| !outcome.remove_tokens.iter().any(|r| r.eq_ignore_ascii_case(t)))
            .collect();
        search_params.q = if kept.is_empty() { "*".into() } else { kept.join(" ") };
    }
    if let Some(f) = outcome.extra_filter {
        search_params.filter_by = Some(match search_params.filter_by.take() {
            Some(existing) => format!("({existing}) && ({f})"),
            None => f,
        });
    }
    if outcome.sort_by.is_some() {
        search_params.sort_by = outcome.sort_by;
    }
    if !outcome.pinned.is_empty() {
        let mut pins: Vec<String> = outcome
            .pinned
            .iter()
            .map(|(id, pos)| format!("{id}:{pos}"))
            .collect();
        if let Some(existing) = search_params.pinned_hits.take() {
            pins.push(existing);
        }
        search_params.pinned_hits = Some(pins.join(","));
    }
    if !outcome.hidden.is_empty() {
        let mut hidden = outcome.hidden.join(",");
        if let Some(existing) = search_params.hidden_hits.take() {
            hidden.push(',');
            hidden.push_str(&existing);
        }
        search_params.hidden_hits = Some(hidden);
    }
    if outcome.filter_curated_hits {
        search_params.filter_curated_hits = true;
    }
    outcome.metadata
}

fn build_search_params(params: SearchQueryParams) -> Result<search::SearchParams, AppError> {
    let (per_page, page, offset) = resolve_paging(params.per_page, params.limit, params.page, params.offset, params.facet_query.is_some())?;
    Ok(search::SearchParams {
        q: params.q,
        query_by: params
            .query_by
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        filter_by: params.filter_by,
        sort_by: params.sort_by,
        facet_by: params.facet_by,
        per_page,
        page,
        offset,
        highlight_fields: params.highlight_fields,
        group_by: params.group_by,
        group_limit: params.group_limit,
        group_missing_values: params.group_missing_values,
        text_match_type: params.text_match_type,
        stopwords: params.stopwords,
        synonym_sets: params.synonym_sets,
        preset: params.preset,
        num_typos: params.num_typos,
        typo_tokens_threshold: params.typo_tokens_threshold,
        prefix: params.prefix,
        vector_query: params.vector_query,
        max_facet_values: params.max_facet_values,
        facet_query: params.facet_query,
        facet_query_num_typos: params.facet_query_num_typos,
        highlight_start_tag: params.highlight_start_tag,
        highlight_end_tag: params.highlight_end_tag,
        highlight_affix_num_tokens: params.highlight_affix_num_tokens,
        query_by_weights: params.query_by_weights,
        pinned_hits: params.pinned_hits,
        hidden_hits: params.hidden_hits,
        filter_curated_hits: params.filter_curated_hits.unwrap_or(false),
        limit_hits: params.limit_hits,
        min_len_1typo: params.min_len_1typo,
        min_len_2typo: params.min_len_2typo,
        drop_tokens_threshold: params.drop_tokens_threshold,
        drop_tokens_mode: params.drop_tokens_mode,
        split_join_tokens: params.split_join_tokens,
        enable_typos_for_numerical_tokens: params.enable_typos_for_numerical_tokens,
        enable_typos_for_alpha_numerical_tokens: params.enable_typos_for_alpha_numerical_tokens,
        synonym_num_typos: params.synonym_num_typos,
        synonym_prefix: params.synonym_prefix,
        explain_matches: params.explain_matches,
        name_matching: params.name_matching,
        name_score: params.name_score,
        name_score_candidates: params.name_score_candidates,
        name_score_kind: params.name_score_kind,
        highlight_full_fields: params.highlight_full_fields,
        enable_highlight_v1: params.enable_highlight_v1,
        snippet_threshold: params.snippet_threshold,
        exhaustive_search: params.exhaustive_search,
        search_cutoff_ms: params.search_cutoff_ms,
        max_candidates: params.max_candidates,
        prioritize_exact_match: params.prioritize_exact_match,
        prioritize_token_position: params.prioritize_token_position,
        prioritize_num_matching_fields: params.prioritize_num_matching_fields,
        facet_sample_percent: params.facet_sample_percent,
        facet_sample_threshold: params.facet_sample_threshold,
        facet_return_parent: params.facet_return_parent,
        validate_field_names: params.validate_field_names,
        enable_synonyms: params.enable_synonyms,
        infix: params.infix,
        max_extra_prefix: params.max_extra_prefix,
        max_extra_suffix: params.max_extra_suffix,
        projection: Default::default(),
        union: None,
    })
}

async fn search_documents(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
    resolved_key: Option<axum::extract::Extension<ResolvedKey>>,
) -> Result<axum::response::Response, AppError> {
    let search_start = Instant::now();

    let resolved_key = resolved_key.map(|ext| ext.0);
    let scoped = resolved_key.as_ref().and_then(ResolvedKey::scoped_params).cloned();
    let mut raw = query_params(raw_query.as_deref())?;
    layer_params(&mut raw, &state.presets, scoped.as_ref())?;
    // the parameters as asked, checked before any model call
    let asked = parse_params(&raw)?;

    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    // the collection as searched, a realtime collection's writes in it as soon as made
    let view = collection.view();
    let bundle = Arc::clone(&view.bundle);

    // use_cache: TTL-bounded result cache, keyed by the parameters as asked (before a model
    // rewrites them). Scoped keys bypass it so per-key filters and projections can't leak between
    // keys; a conversation is never cached.
    let cache_enabled = asked.use_cache.unwrap_or(false) && scoped.is_none() && !asked.conversation.unwrap_or(false);
    let cache_ttl = std::time::Duration::from_secs(asked.cache_ttl.unwrap_or(60) as u64);
    let cache_key = cache_enabled.then(|| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        resolved.hash(&mut h);
        raw.hash(&mut h);
        (Arc::as_ptr(&bundle) as usize).hash(&mut h);
        h.finish()
    });
    if let Some(key) = cache_key {
        if let Some(hit) = state.search_cache.get(key) {
            state.metrics.queries_total.inc();
            return Ok(Json(&*hit).into_response());
        }
    }

    // A conversation's follow-up runs as the standalone question a model makes of it and the
    // history; nl_query has a model translate the query into search parameters.
    let conversation = conversation_of(&state, &mut raw, false).await?;
    let nl = nl_augment(&state, &resolved, &mut raw).await;
    let params = parse_params(&raw)?;
    let projection = projection_of(&params, scoped.as_ref());
    let meta_excluded = excluded_meta(params.exclude_fields.as_deref(), scoped.as_ref());
    let include_fields = params.include_fields.clone();
    // enable_overrides is the pre-v30 alias for enable_curations.
    let curation_enabled = params.enable_curations.or(params.enable_overrides).unwrap_or(true);
    let curation_tags = parse_csv_list(params.curation_tags.as_deref());
    let (embedding_timeout, embedding_tries) = (params.remote_embedding_timeout_ms, params.remote_embedding_num_tries);
    let mut search_params = build_search_params(params)?;
    search_params.projection = projection.clone();
    validate_search_param_lengths(&search_params)?;
    // the query as searched for (a semantic search runs it as `*`, through its vector)
    let query = search_params.q.clone();

    // Auto-embedding: a text query on an embedding field searched through the field's model's
    // vector of it.
    if let Some(embedding) = query_embedding(&collection.schema, &search_params, embedding_tries)? {
        let (embedding, vector) = tokio::task::spawn_blocking(move || {
            let vector = embedding.embed(embedding_timeout, embedding_tries);
            (embedding, vector)
        })
        .await
        .map_err(|e| AppError::Internal(format!("Embedding task failed: {e}")))?;
        embedding.apply(&mut search_params, vector?);
    }

    tracing::debug!(
        collection = resolved.as_str(),
        q = search_params.q.as_str(),
        query_by = search_params.query_by.join(",").as_str(),
        filter_by = search_params.filter_by.as_deref().unwrap_or(""),
        sort_by = search_params.sort_by.as_deref().unwrap_or(""),
        facet_by = search_params.facet_by.as_deref().unwrap_or(""),
        group_by = search_params.group_by.as_deref().unwrap_or(""),
        per_page = search_params.per_page,
        page = search_params.page,
        "search request"
    );

    // Curations: evaluate the collection's bound curation sets against the
    // (post-preset) query and fold the matched actions into the params.
    let curation_metadata = apply_curations(
        &state,
        &collection.schema,
        &mut search_params,
        curation_enabled,
        &curation_tags,
    );

    // a scoped key's limit_hits
    if let Some(s) = &scoped {
        apply_scoped_params(&mut search_params, s);
    }

    let timeout_ms = state.config.query_timeout_ms;
    let search_view = &view;
    let search_schema = collection.schema.clone();
    let search_synonyms = Arc::clone(&state.synonyms);
    let search_stopwords = Arc::clone(&state.stopwords);
    #[cfg(feature = "cuda")]
    let search_gpu = state.gpu.clone();

    // Build JoinContext if any parameter uses $Collection(...) JOIN syntax
    let has_join = search_params.filter_by.as_ref().is_some_and(|f| f.contains('$'))
        || search_params.sort_by.as_ref().is_some_and(|s| s.contains('$'))
        || search_params.facet_by.as_ref().is_some_and(|f| f.contains('$'));
    let collections_ref = &state.collections;
    let aliases_ref = &state.aliases;
    let local_coll_name = resolved.clone();

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let run_search = move || {
        let join_ctx = has_join.then(|| crate::filter::ops::JoinContext::new(collections_ref, aliases_ref, &local_coll_name));
        let r = search::execute_with_stores_gpu_join(
            search_view.target(),
            &search_schema,
            &search_params,
            Some(&search_synonyms),
            Some(&search_stopwords),
            #[cfg(feature = "cuda")]
            search_gpu.as_ref(),
            join_ctx.as_ref(),
        );
        (r, search_params)
    };
    let (search_result, search_params) = tokio::task::block_in_place(run_search);
    if std::time::Instant::now() > deadline {
        return Err(AppError::RequestTimeout(format!(
            "Query timed out after {timeout_ms}ms."
        )));
    }
    let (mut result, timings) = search_result.map_err(search_error)?;

    // Analytics: auto-capture the query for popular/nohits rules bound to
    // this collection (gated on a relaxed atomic so the common no-rules
    // case costs one load).
    if state.analytics.captures_searches() {
        let found = match &result {
            search::SearchResponse::Ungrouped(r) => r.found,
            search::SearchResponse::Grouped(r) => r.found_docs,
        };
        state.analytics.capture_search(&resolved, &query, found);
    }

    // Conversational search: the top hits answer the question (`conversation`). Streamed, the
    // answer follows in the SSE tail below; its conversation id is made in advance (as Typesense
    // does) so the delta events can carry it.
    let with_history = !meta_excluded.contains(&"conversation_history");
    let mut conversation_stream_ctx: Option<(Conversation, String)> = None;
    let conversation_val = match conversation {
        Some(c) if c.stream => {
            let cid = c.id.clone().unwrap_or_else(crate::llm::generate_id);
            conversation_stream_ctx = Some((c, cid));
            None
        }
        Some(c) => {
            let messages = build_answer_messages(&collection.schema, &result, &c.model, &query);
            Some(conversation_answer(&state, messages, c.model, c.question, &query, c.id, with_history).await?)
        }
        None => None,
    };

    // Inject request_params for Typesense v30.1 compatibility, plus the
    // matched curation item's metadata when a rule carried one.
    let request_params_val = Some(build_request_params(&resolved, search_params.per_page, &query));
    match &mut result {
        search::SearchResponse::Ungrouped(r) => {
            r.request_params = request_params_val;
            r.metadata = curation_metadata;
            r.conversation = conversation_val;
        }
        search::SearchResponse::Grouped(r) => {
            r.request_params = request_params_val;
            r.metadata = curation_metadata;
            r.conversation = conversation_val;
        }
    }

    // Record query metrics
    let elapsed = search_start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let elapsed_ms = elapsed_secs * 1000.0;
    state.metrics.queries_total.inc();
    state.metrics.query_duration.observe(elapsed_secs);
    state.metrics.latency_tracker.record(elapsed_ms);

    // Per-collection query metrics
    let query_type = classify_query_type(&search_params);
    let query_labels = QueryLabels {
        collection: resolved.clone(),
        query_type,
    };
    state
        .metrics
        .query_collection_total
        .get_or_create(&query_labels)
        .inc();
    state
        .metrics
        .query_collection_duration
        .get_or_create(&query_labels)
        .observe(elapsed_secs);

    // Slow query logging is disabled when slow_query_ms == 0 (documented default).
    // Read the runtime value (adjustable via POST /config), not the boot config.
    let slow_threshold = state
        .slow_query_ms
        .load(std::sync::atomic::Ordering::Relaxed);
    if slow_threshold > 0 && elapsed_ms > slow_threshold as f64 {
        tracing::warn!(
            collection = resolved.as_str(),
            q = search_params.q.as_str(),
            query_by = search_params.query_by.join(",").as_str(),
            filter_by = search_params.filter_by.as_deref().unwrap_or(""),
            sort_by = search_params.sort_by.as_deref().unwrap_or(""),
            elapsed_ms = format!("{elapsed_ms:.2}").as_str(),
            filter_ms = format!("{:.2}", timings.filter_ms).as_str(),
            text_ms = format!("{:.2}", timings.text_ms).as_str(),
            vector_ms = format!("{:.2}", timings.vector_ms).as_str(),
            facet_ms = format!("{:.2}", timings.facet_ms).as_str(),
            sort_ms = format!("{:.2}", timings.sort_ms).as_str(),
            deferred_filter_ms = format!("{:.2}", timings.deferred_filter_ms).as_str(),
            "slow query"
        );
    }

    if let Some(s) = &scoped {
        apply_scoped_result_limits(&mut result, s);
    }
    shape_hits(&mut result, include_fields.as_deref(), &projection, &view, &resolved, &state.collections, &state.aliases);

    // Conversational streaming (Typesense `conversation_stream=true`):
    // an SSE response of answer deltas, a forwarded [DONE] sentinel, then
    // the complete search-results JSON as the final event.
    if let Some((c, cid)) = conversation_stream_ctx {
        let results_value = finish_value(serde_json::to_value(&result).map_err(|e| AppError::Internal(e.to_string()))?, nl.as_ref(), &meta_excluded);
        let messages = build_answer_messages(&collection.schema, &result, &c.model, &query);
        return conversation_stream_response(Arc::clone(&state), c.model, c.question, query, cid, messages, results_value, with_history).await;
    }

    // Only a response with what the struct does not carry (an NL translation's report, response
    // keys left out) or a cached one pays for Value conversion; the default path serializes the
    // response exactly once.
    if nl.is_none() && meta_excluded.is_empty() && cache_key.is_none() {
        return Ok(crate::tqbf::json::TsJsonInOrder(result).into_response());
    }
    let value = finish_value(serde_json::to_value(&result).map_err(|e| AppError::Internal(e.to_string()))?, nl.as_ref(), &meta_excluded);
    if let Some(key) = cache_key {
        let value = Arc::new(value);
        state
            .search_cache
            .insert(key, cache_ttl, Arc::clone(&value), Arc::clone(&bundle));
        return Ok(TsJson(&*value).into_response());
    }
    Ok(TsJson(value).into_response())
}

/// The response keys exclude_fields names (Typesense: `out_of`, `search_time_ms`,
/// `parsed_nl_query`, and a conversation's `conversation_history`), from the search's list and a
/// scoped key's.
fn excluded_meta(exclude: Option<&str>, scoped: Option<&ScopedKeyParams>) -> Vec<&'static str> {
    const META: [&str; 4] = ["out_of", "search_time_ms", "parsed_nl_query", "conversation_history"];
    let named: Vec<&str> = exclude.into_iter().chain(scoped.and_then(|s| s.exclude_fields.as_deref())).flat_map(|l| l.split(',')).map(str::trim).collect();
    META.into_iter().filter(|m| named.contains(m)).collect()
}

/// A search's response JSON with what its struct does not carry: an NL translation's report
/// (`parsed_nl_query`, the query as asked in `request_params`, its time in `search_time_ms`) and
/// without the response keys exclude_fields names.
fn finish_value(mut v: Value, nl: Option<&NlOutcome>, meta: &[&str]) -> Value {
    let Some(o) = v.as_object_mut() else { return v };
    if let Some(nl) = nl {
        if let Some(rp) = o.get_mut("request_params").and_then(Value::as_object_mut) {
            rp.insert("q".into(), Value::String(nl.original_q.clone()));
        }
        if let Some(t) = o.get("search_time_ms").and_then(Value::as_u64) {
            o.insert("search_time_ms".into(), json!(t + nl.time_ms));
        }
        o.insert("parsed_nl_query".into(), nl.report());
    }
    for m in meta {
        if *m == "conversation_history" {
            if let Some(c) = o.get_mut("conversation").and_then(Value::as_object_mut) {
                c.remove("conversation_history");
            }
        } else {
            o.remove(*m);
        }
    }
    v
}

/// A query to embed before it is searched (Typesense's auto-embedding): the text query on an
/// embedding field's model, from `query_by` naming the field or a `vector_query` with an empty
/// vector.
struct QueryEmbedding {
    field: String,
    model: crate::schema::EmbedModelConfig,
    text: String,
    /// query_by names only the embedding field: a semantic search, without a text match.
    semantic_only: bool,
}

/// The embedding a search needs, with Typesense's checks: one embedding field, an auto-embedding
/// one, no prefix search through a remote model (each keystroke would call it), at least one try.
fn query_embedding(schema: &Schema, p: &search::SearchParams, tries: Option<usize>) -> Result<Option<QueryEmbedding>, AppError> {
    let bad = |m: String| AppError::BadRequest(m);
    let vector_field = |name: &str| {
        schema.name_to_id.get(name.trim()).map(|&f| &schema.fields[f as usize]).filter(|f| matches!(f.field_type, crate::schema::FieldType::FloatArray { num_dim } if num_dim > 0))
    };
    let vq_field = p.vector_query.as_deref().map(|vq| vq.split(':').next().unwrap_or("").trim());
    let vq_empty = p.vector_query.as_deref().is_some_and(|vq| vq.contains("[]"));
    let in_query_by: Vec<(usize, &FieldDef)> = p.query_by.iter().enumerate().filter_map(|(i, e)| vector_field(e.split(':').next().unwrap_or("")).map(|f| (i, f))).collect();
    if in_query_by.len() > 1 || in_query_by.first().is_some_and(|(_, f)| vq_field.is_some_and(|v| v != f.name)) {
        return Err(bad("Only one embedding field is allowed in the query.".into()));
    }
    let text_query = !p.q.is_empty() && p.q != "*";
    let field = match in_query_by.first() {
        // a wildcard query leaves the embedding field out
        Some(_) if !text_query => return Ok(None),
        Some(&(i, f)) => {
            if f.embed.is_none() {
                return Err(bad(format!("Vector field `{}` is not an auto-embedding field, do not use `query_by` with it, use `vector_query` instead.", f.name)));
            }
            let prefixes: Vec<bool> = p.prefix.as_deref().map_or_else(|| vec![true], |v| v.split(',').map(|x| x.trim() == "true").collect());
            if prefixes.get(i).or(prefixes.first()).copied().unwrap_or(true) {
                return Err(bad("Prefix search is not supported for remote embedders. Please set `prefix=false` as an additional search parameter to disable prefix searching.".into()));
            }
            if tries == Some(0) {
                return Err(bad("`remote_embedding_num_tries` must be greater than 0.".into()));
            }
            // a vector given is searched as it is
            if p.vector_query.is_some() && !vq_empty {
                return Ok(None);
            }
            f
        }
        None if vq_empty => {
            let Some(f) = vq_field.and_then(vector_field).filter(|f| f.embed.is_some()) else { return Ok(None) };
            if !text_query {
                return Err(bad("vector_query with an empty vector needs a text query `q` to embed.".into()));
            }
            f
        }
        None => return Ok(None),
    };
    let Some(embed) = &field.embed else { return Ok(None) };
    Ok(Some(QueryEmbedding {
        field: field.name.clone(),
        model: embed.model_config.clone(),
        // the model's query prefix before the query (Typesense)
        text: format!("{}{}", embed.model_config.query_prefix.as_deref().unwrap_or(""), p.q),
        semantic_only: in_query_by.len() == p.query_by.len(),
    }))
}

impl QueryEmbedding {
    /// The query's vector, from the field's model (a blocking call).
    fn embed(&self, timeout_ms: Option<usize>, tries: Option<usize>) -> Result<Vec<f32>, AppError> {
        crate::embedding::embed_texts(&self.model, std::slice::from_ref(&self.text), timeout_ms.map(|t| t as u64), tries)
            .map_err(|e| AppError::BadRequest(format!("Query embedding failed: {e}")))
            .map(|v| v.into_iter().next().unwrap_or_default())
    }

    /// The vector query of the search: the given one with the vector filled in, else the field's
    /// nearest documents, at least as many as the page reaches.
    fn apply(self, p: &mut search::SearchParams, vector: Vec<f32>) {
        let floats = vector.iter().map(f32::to_string).collect::<Vec<_>>().join(",");
        let k = (p.start() + p.per_page).max(100);
        p.vector_query = Some(match p.vector_query.take() {
            Some(vq) if vq.contains("[]") => vq.replacen("[]", &format!("[{floats}]"), 1),
            _ => format!("{}:([{floats}], k:{k})", self.field),
        });
        if self.semantic_only {
            p.q = "*".into();
        }
    }
}

/// Parse and resolve `$CollectionName(field1, field2)` in include_fields.
/// For each hit, the live documents its own copy (in `local`, the collection as searched)
/// references, in any of the referenced collection's segments, embedded as a nested object.
fn resolve_join_includes(
    result: &mut search::SearchResponse,
    include_fields: &str,
    local: &crate::collection::CollectionView,
    local_name: &str,
    collections: &crate::collection::CollectionRegistry,
    aliases: &crate::collection::alias::AliasRegistry,
) {
    // Parse $Collection(fields) entries
    let mut join_projections: Vec<(String, Vec<String>)> = Vec::new();
    let mut i = 0;
    let chars: Vec<char> = include_fields.chars().collect();
    let len = chars.len();

    while i < len {
        if chars[i] == '$' {
            i += 1;
            let start = i;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-') {
                i += 1;
            }
            let coll_name: String = chars[start..i].iter().collect();
            if i < len && chars[i] == '(' {
                i += 1;
                let pstart = i;
                let mut depth = 1;
                while i < len && depth > 0 {
                    if chars[i] == '(' { depth += 1; }
                    if chars[i] == ')' { depth -= 1; }
                    if depth > 0 { i += 1; }
                }
                let fields_str: String = chars[pstart..i].iter().collect();
                let fields: Vec<String> = fields_str
                    .split(',')
                    .map(|f| f.trim().to_string())
                    .filter(|f| !f.is_empty())
                    .collect();
                join_projections.push((coll_name, fields));
                if i < len { i += 1; } // skip ')'
            }
        } else {
            i += 1;
        }
    }

    if join_projections.is_empty() {
        return;
    }

    // Resolve each join projection
    let ctx = crate::filter::ops::JoinContext::new(collections, aliases, local_name);
    let resolve_hits = |hits: &mut [search::Hit]| {
        for (target_coll_name, fields) in &join_projections {
            let Some(target) = ctx.target(target_coll_name) else { continue };
            let target_schema = &target.schema;
            let target_registry = target_schema.registry.load();
            let mut referenced = Vec::new();

            for hit in hits.iter_mut() {
                // the hit's own copy, and the live documents it references
                let Some((seg, local_doc_id)) = local.locate(hit.seq) else { continue };
                let Some(ref_idx) = crate::filter::ops::JoinContext::references(local.segment(seg).0, &target, target_coll_name) else { continue };
                referenced.clear();
                crate::filter::ops::Referenced::new(ref_idx, &target.view).each(local_doc_id, &mut referenced);

                // Resolve target docs
                let mut joined_docs: Vec<serde_json::Value> = Vec::new();
                for &(target_seg, target_doc_id) in &referenced {
                    if let Some(mut doc_json) = target.view.segment(target_seg).0.doc_json(target_doc_id, target_schema, &target_registry) {
                        // Apply field projection if not wildcard
                        if !(fields.len() == 1 && fields[0] == "*") && !fields.is_empty() {
                            if let Some(obj) = doc_json.as_object_mut() {
                                obj.retain(|k, _| fields.iter().any(|f| f == k) || k == "id");
                            }
                        }
                        joined_docs.push(doc_json);
                    }
                }

                // Embed in hit document
                if let Some(obj) = hit.document.as_object_mut() {
                    if joined_docs.len() == 1 {
                        obj.insert(
                            target_coll_name.clone(),
                            joined_docs.into_iter().next().unwrap(),
                        );
                    } else if !joined_docs.is_empty() {
                        obj.insert(
                            target_coll_name.clone(),
                            serde_json::Value::Array(joined_docs),
                        );
                    }
                }
            }
        }
    };

    match result {
        search::SearchResponse::Ungrouped(r) => resolve_hits(&mut r.hits),
        search::SearchResponse::Grouped(r) => {
            for group in &mut r.grouped_hits {
                resolve_hits(&mut group.hits);
            }
        }
    }
}

/// Hits as the request shapes them: the documents of referenced collections embedded
/// (`$brands(name)` in include_fields), then each document pruned to what it returns.
#[allow(clippy::too_many_arguments)]
fn shape_hits(
    result: &mut search::SearchResponse,
    include_fields: Option<&str>,
    projection: &search::projection::Projection,
    local: &crate::collection::CollectionView,
    local_name: &str,
    collections: &crate::collection::CollectionRegistry,
    aliases: &crate::collection::alias::AliasRegistry,
) {
    if let Some(inc) = include_fields.filter(|i| i.contains('$')) {
        resolve_join_includes(result, inc, local, local_name, collections, aliases);
    }
    if projection.is_empty() {
        return;
    }
    match result {
        search::SearchResponse::Ungrouped(r) => r.hits.iter_mut().for_each(|h| projection.prune(&mut h.document)),
        search::SearchResponse::Grouped(r) => r.grouped_hits.iter_mut().flat_map(|g| g.hits.iter_mut()).for_each(|h| projection.prune(&mut h.document)),
    }
}

// -- Multi-search handler --

/// Searches one multi_search may hold (Typesense's default `limit_multi_searches`).
const MAX_MULTI_SEARCHES: usize = 50;

/// A search error as Typesense answers it: 408 when the search cutoff passed before anything was
/// found, 404 for a field it could not find ("Could not find a ...") and a sort it cannot do, 422
/// for too many sort fields, else 400.
fn search_error(e: String) -> AppError {
    if e == search::REQUEST_TIMEOUT {
        AppError::RequestTimeout(e)
    } else if e.starts_with("Could not find a ")
        || e.starts_with("sort_by vector_distance is only supported")
        || e.starts_with("group_by parameters should not be empty")
    {
        AppError::NotFound(e)
    } else if e.starts_with("Only upto ") {
        AppError::Unprocessable(e)
    } else {
        AppError::BadRequest(e)
    }
}

async fn multi_search(
    State(state): State<Arc<AppState>>,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
    resolved_key: Option<axum::extract::Extension<ResolvedKey>>,
    refusal: Option<axum::extract::Extension<crate::auth::middleware::Refusal>>,
    Json(body): Json<Value>,
) -> Result<axum::response::Response, AppError> {
    let multi_start = Instant::now();
    // the request's key (none without authentication), or why it may not search
    let request_key: Result<Option<ResolvedKey>, crate::auth::middleware::Refusal> = match refusal {
        Some(r) => Err(r.0),
        None => Ok(resolved_key.map(|ext| ext.0)),
    };
    let refused = |r: crate::auth::middleware::Refusal| match r.status {
        StatusCode::FORBIDDEN => AppError::Forbidden(r.message.into()),
        _ => AppError::Unauthorized(r.message.into()),
    };
    // The URL's parameters are every search's; a multi-search preset named there is the request
    // (Typesense then reads no body).
    let mut common = query_params(raw_query.as_deref())?;
    common.remove("x-typesense-api-key");
    let body = match common.get("preset").and_then(|n| state.presets.get(n)) {
        Some(p) if p.get("searches").is_some() => p,
        _ => body,
    };
    let searches = body.get("searches").and_then(Value::as_array).ok_or_else(|| AppError::BadRequest("Missing `searches` array.".into()))?;
    // Each search's key: its own `x-typesense-api-key`, else the request's (Typesense); the
    // request fails when one of them may not search.
    let keys: Vec<Option<ResolvedKey>> = searches
        .iter()
        .map(|s| match s.get("x-typesense-api-key").and_then(Value::as_str) {
            Some(own) => crate::auth::middleware::resolve_key(&state.keys, own, "documents:search", None).map(Some),
            None => request_key.clone(),
        })
        .collect::<Result<_, _>>()
        .map_err(refused)?;
    // the strictest `limit_multi_searches` of the keys searching, else the URL's
    let limit = keys
        .iter()
        .flatten()
        .filter_map(|k| k.scoped_params()?.other.get("limit_multi_searches")?.as_u64())
        .min()
        .or_else(|| common.get("limit_multi_searches").and_then(|v| v.parse().ok()))
        .unwrap_or(MAX_MULTI_SEARCHES as u64);
    if searches.len() as u64 > limit {
        return Err(AppError::BadRequest("Number of multi searches exceeds `limit_multi_searches` parameter.".into()));
    }
    let search_count = searches.len();
    let union = body.get("union").and_then(Value::as_bool).unwrap_or(false);
    let remove_duplicates = body.get("remove_duplicates").and_then(Value::as_bool).unwrap_or(true);

    // A conversation is one question over every search's results; its question, model and
    // conversation are the URL's (Typesense).
    let conversation = conversation_of(&state, &mut common, true).await?;

    // A union pages its merged hits by the URL's paging; its searches leave theirs out.
    let paging = match union.then(|| UnionPaging::of(&common)).transpose() {
        Ok(p) => p,
        Err(e) => return Ok(union_failure(e)),
    };
    if union {
        for k in UNION_PAGING {
            common.remove(k);
        }
    }

    // Each search's parameters: the URL's, its own over them, then its preset and its key's.
    let mut raws: Vec<RawParams> = Vec::with_capacity(search_count);
    for (search, key) in searches.iter().zip(&keys) {
        let obj = search.as_object().ok_or_else(|| AppError::BadRequest("The value of `searches` must be an array of objects.".into()))?;
        for k in obj.keys() {
            let in_url = |p: &str| AppError::BadRequest(format!("`{p}` cannot be used in POST body. Please set `{p}` as a query parameter in the request, instead of inside the POST body"));
            match k.as_str() {
                "conversation" => return Err(in_url("conversation")),
                "q" if conversation.is_some() => {
                    return Err(AppError::BadRequest("`q` parameter cannot be used in POST body if `conversation` is enabled. Please set `q` as a query parameter in the request, instead of inside the POST body".into()))
                }
                "conversation_model_id" | "conversation_id" if conversation.is_some() => return Err(in_url(k)),
                _ => {}
            }
        }
        let mut raw = common.clone();
        json_params(&mut raw, obj)?;
        raw.remove("x-typesense-api-key");
        if union {
            for k in UNION_PAGING {
                raw.remove(k);
            }
        }
        layer_params(&mut raw, &state.presets, key.as_ref().and_then(ResolvedKey::scoped_params))?;
        // the conversation is the multi_search's
        for k in ["conversation", "conversation_id", "conversation_model_id", "conversation_stream"] {
            raw.remove(k);
        }
        raws.push(raw);
    }

    // nl_query searches translated by their models, at once
    let mut nl: Vec<Option<NlOutcome>> = raws.iter().map(|_| None).collect();
    if raws.iter().any(|r| r.get("nl_query").is_some_and(|v| v == "true")) {
        let tasks: Vec<_> = std::mem::take(&mut raws)
            .into_iter()
            .map(|mut raw| {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let name = raw.get("collection").cloned().unwrap_or_default();
                    let outcome = nl_augment(&state, &name, &mut raw).await;
                    (raw, outcome)
                })
            })
            .collect();
        for (k, task) in tasks.into_iter().enumerate() {
            let (raw, outcome) = task.await.map_err(|e| AppError::Internal(format!("NL query task failed: {e}")))?;
            raws.push(raw);
            nl[k] = outcome;
        }
    }

    // every search of a collection sees it as one snapshot
    let mut views: rustc_hash::FxHashMap<String, Arc<crate::collection::CollectionView>> = rustc_hash::FxHashMap::default();
    let mut items: Vec<Result<Prepared, AppError>> = Vec::with_capacity(search_count);
    for ((raw, nl), key) in raws.iter().zip(nl).zip(&keys) {
        let item = parse_params(raw);
        if let Some(c) = item.as_ref().ok().and_then(|p| p.collection.as_deref()) {
            if !key_permits(key.as_ref(), c) {
                return Err(AppError::Forbidden("API key is not allowed to access this collection.".into()));
            }
        }
        let scoped = key.as_ref().and_then(ResolvedKey::scoped_params).cloned();
        items.push(item.and_then(|p| prepare_search(&state, p, nl, scoped, &mut views)));
    }

    // a union's searches: checked alike (Typesense), each asked for its first hits up to the
    // union's page end
    if let Some(paging) = &paging {
        if let Some(e) = items.iter().find_map(|i| i.as_ref().err()) {
            return Ok(union_failure(UnionError::of(e)));
        }
        let prepared: Vec<&Prepared> = items.iter().filter_map(|i| i.as_ref().ok()).collect();
        if let Err(e) = check_union(&state, &prepared, &raws) {
            return Ok(union_failure(e));
        }
        let grouped = prepared.first().is_some_and(|p| p.params.group_by.is_some());
        let part = if remove_duplicates && !grouped { search::UnionPart::Distinct } else { search::UnionPart::Each };
        let top_k = prepared.iter().any(|p| p.params.facet_by.as_deref().is_some_and(|f| facet_top_k(f, &p.collection.schema)));
        let fetch = paging.fetch(top_k);
        for p in items.iter_mut().flatten() {
            p.params.page = 1;
            p.params.offset = None;
            p.params.per_page = fetch;
            p.params.union = Some(part);
        }
    }

    // query embeddings made, at once
    let mut pending = Vec::new();
    for (k, item) in items.iter_mut().enumerate() {
        let Ok(p) = item else { continue };
        match query_embedding(&p.collection.schema, &p.params, p.embedding_tries) {
            Ok(Some(embedding)) => {
                let (timeout, tries) = (p.embedding_timeout, p.embedding_tries);
                pending.push((k, tokio::task::spawn_blocking(move || {
                    let vector = embedding.embed(timeout, tries);
                    (embedding, vector)
                })));
            }
            Ok(None) => {}
            Err(e) => *item = Err(e),
        }
    }
    for (k, task) in pending {
        let (embedding, vector) = task.await.map_err(|e| AppError::Internal(format!("Embedding task failed: {e}")))?;
        match (&mut items[k], vector) {
            (Ok(p), Ok(vector)) => embedding.apply(&mut p.params, vector),
            (item, Err(e)) => *item = Err(e),
            _ => {}
        }
    }
    // each search's collection, for the conversation's context
    let collections: Vec<Option<Arc<crate::collection::Collection>>> = items.iter().map(|i| i.as_ref().ok().map(|p| Arc::clone(&p.collection))).collect();

    let timeout_ms = state.config.query_timeout_ms * 2; // 2x single timeout for multi
    let state_clone = Arc::clone(&state);
    let multi_result = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        tokio::task::spawn_blocking(move || {
            use rayon::prelude::*;
            items.into_par_iter().map(|item| multi_search_item(&state_clone, item?)).collect::<Vec<Result<Searched, AppError>>>()
        }),
    )
    .await;

    let results = match multi_result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return Err(AppError::Internal(format!("Multi-search task panicked: {e}")));
        }
        Err(_) => {
            tracing::warn!(searches = search_count, timeout_ms, "Multi-search timed out");
            return Err(AppError::RequestTimeout(format!(
                "Multi-search timed out after {timeout_ms}ms."
            )));
        }
    };

    // Record multi_search metrics
    let elapsed = multi_start.elapsed();
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    state.metrics.queries_total.inc_by(search_count as u64);

    if elapsed_ms > 100.0 {
        tracing::info!(
            searches = search_count,
            elapsed_ms = format!("{elapsed_ms:.2}").as_str(),
            "slow multi_search"
        );
    }

    // As in Typesense: a search cut by its deadline fails the whole request (the client retries);
    // other failed searches answer in their own slot, a union with its error.
    if let Some(Err(e)) = results.iter().find(|r| matches!(r, Err(AppError::RequestTimeout(_)))) {
        return Err(AppError::RequestTimeout(e.status().1.to_string()));
    }
    let mut response = match &paging {
        Some(paging) => {
            let parts = match results.into_iter().collect::<Result<Vec<Searched>, AppError>>() {
                Ok(parts) => parts,
                Err(e) => return Ok(union_failure(UnionError::of(&e))),
            };
            match union_response(&state, parts, paging, remove_duplicates, elapsed.as_millis() as u64) {
                Ok(v) => v,
                Err(e) => return Ok(union_failure(e)),
            }
        }
        None => {
            let results: Vec<Value> = results
                .into_iter()
                .map(|r| {
                    r.and_then(Searched::into_value).unwrap_or_else(|e| {
                        let (status, message) = e.status();
                        json!({ "code": status.as_u16(), "error": message })
                    })
                })
                .collect();
            json!({ "results": results })
        }
    };
    // with a conversation every search echoes its question
    if let Some(c) = &conversation {
        let echo = |rp: &mut Value| {
            if let Some(rp) = rp.as_object_mut() {
                rp.insert("q".into(), Value::String(c.question.clone()));
            }
        };
        for r in response.get_mut("results").and_then(Value::as_array_mut).into_iter().flatten() {
            if let Some(rp) = r.get_mut("request_params") {
                echo(rp);
            }
        }
        for rp in response.get_mut("union_request_params").and_then(Value::as_array_mut).into_iter().flatten() {
            echo(rp);
        }
    }

    // The conversation: the model answers the (standalone) question from every search's documents.
    if let Some(c) = conversation {
        // the context: each search's documents (a union's, its hits)
        let docs_of = |r: &Value, k: usize| -> Vec<Value> {
            let hits = r.get("hits").and_then(Value::as_array).into_iter().flatten();
            let grouped = r.get("grouped_hits").and_then(Value::as_array).into_iter().flatten().flat_map(|g| g.get("hits").and_then(Value::as_array).into_iter().flatten());
            let docs = hits.chain(grouped).filter_map(|h| h.get("document"));
            match collections.get(k).and_then(Option::as_ref) {
                Some(coll) => context_docs(&coll.schema, docs),
                None => docs.cloned().collect(),
            }
        };
        let per_search: Vec<Vec<Value>> = match response.get("results").and_then(Value::as_array) {
            Some(results) => results.iter().enumerate().filter(|(_, r)| r.get("code").is_none()).map(|(k, r)| docs_of(r, k)).collect(),
            None => vec![docs_of(&response, usize::MAX)],
        };
        let effective_q = common.get("q").cloned().unwrap_or_default();
        let request_scoped = request_key.as_ref().ok().and_then(Option::as_ref).and_then(ResolvedKey::scoped_params);
        let with_history = !excluded_meta(common.get("exclude_fields").map(String::as_str), request_scoped).contains(&"conversation_history");
        let context = balanced_context(per_search, context_budget(&c.model, &effective_q));
        let messages = answer_messages(&context, &c.model, &effective_q);
        if c.stream {
            let cid = c.id.clone().unwrap_or_else(crate::llm::generate_id);
            return conversation_stream_response(Arc::clone(&state), c.model, c.question.clone(), c.question, cid, messages, response, with_history).await;
        }
        response["conversation"] = conversation_answer(&state, messages, c.model, c.question.clone(), &c.question, c.id, with_history).await?;
    }
    Ok(TsJson(response).into_response())
}

/// A search of a multi_search, ready to run.
struct Prepared {
    resolved: String,
    collection: Arc<crate::collection::Collection>,
    /// The collection as every search of it in the request sees it.
    view: Arc<crate::collection::CollectionView>,
    params: search::SearchParams,
    /// Its key's embedded parameters.
    scoped: Option<ScopedKeyParams>,
    include_fields: Option<String>,
    projection: search::projection::Projection,
    curation_enabled: bool,
    curation_tags: Vec<String>,
    meta_excluded: Vec<&'static str>,
    nl: Option<NlOutcome>,
    /// The query as searched for, for `request_params`.
    query: String,
    embedding_timeout: Option<usize>,
    embedding_tries: Option<usize>,
}

/// A multi_search search's collection and parameters.
fn prepare_search(
    state: &AppState,
    params: SearchQueryParams,
    nl: Option<NlOutcome>,
    scoped: Option<ScopedKeyParams>,
    views: &mut rustc_hash::FxHashMap<String, Arc<crate::collection::CollectionView>>,
) -> Result<Prepared, AppError> {
    let collection_name = params.collection.clone().ok_or_else(|| AppError::NotFound("Collection not found".into()))?;
    let resolved = state.aliases.resolve(&collection_name);
    let collection = state.collections.get(&resolved).ok_or_else(|| AppError::NotFound("Collection not found".into()))?;
    let view = Arc::clone(views.entry(resolved.clone()).or_insert_with(|| Arc::new(collection.view())));
    let include_fields = params.include_fields.clone();
    let projection = projection_of(&params, scoped.as_ref());
    let meta_excluded = excluded_meta(params.exclude_fields.as_deref(), scoped.as_ref());
    let curation_enabled = params.enable_curations.or(params.enable_overrides).unwrap_or(true);
    let curation_tags = parse_csv_list(params.curation_tags.as_deref());
    let (embedding_timeout, embedding_tries) = (params.remote_embedding_timeout_ms, params.remote_embedding_num_tries);
    let mut search_params = build_search_params(params)?;
    search_params.projection = projection.clone();
    validate_search_param_lengths(&search_params)?;
    let query = search_params.q.clone();
    Ok(Prepared {
        resolved,
        collection,
        view,
        params: search_params,
        scoped,
        include_fields,
        projection,
        curation_enabled,
        curation_tags,
        meta_excluded,
        nl,
        query,
        embedding_timeout,
        embedding_tries,
    })
}

/// A search of a multi_search, done: its response with its hits shaped, and what its slot of
/// `results` or the union needs of it.
struct Searched {
    result: search::SearchResponse,
    resolved: String,
    collection: Arc<crate::collection::Collection>,
    view: Arc<crate::collection::CollectionView>,
    params: search::SearchParams,
    query: String,
    nl: Option<NlOutcome>,
    meta_excluded: Vec<&'static str>,
}

impl Searched {
    /// Its slot of `results`.
    fn into_value(self) -> Result<Value, AppError> {
        let value = serde_json::to_value(self.result).map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(finish_value(value, self.nl.as_ref(), &self.meta_excluded))
    }
}

/// One search of a multi_search.
fn multi_search_item(state: &AppState, p: Prepared) -> Result<Searched, AppError> {
    let Prepared { resolved, collection, view, params: mut search_params, scoped, include_fields, projection, curation_enabled, curation_tags, meta_excluded, nl, query, .. } = p;
    let curation_metadata = apply_curations(state, &collection.schema, &mut search_params, curation_enabled, &curation_tags);
    if let Some(s) = &scoped {
        apply_scoped_params(&mut search_params, s);
    }

    // Build JoinContext if any parameter uses $Collection(...) JOIN syntax
    let has_join = search_params.filter_by.as_ref().is_some_and(|f| f.contains('$'))
        || search_params.sort_by.as_ref().is_some_and(|s| s.contains('$'))
        || search_params.facet_by.as_ref().is_some_and(|f| f.contains('$'));
    let join_ctx = has_join.then(|| crate::filter::ops::JoinContext::new(&state.collections, &state.aliases, &resolved));
    let (mut result, _timings) = search::execute_with_stores_gpu_join(
        view.target(),
        &collection.schema,
        &search_params,
        Some(&state.synonyms),
        Some(&state.stopwords),
        #[cfg(feature = "cuda")]
        state.gpu.as_ref(),
        join_ctx.as_ref(),
    )
    .map_err(search_error)?;

    let rp = Some(build_request_params(&resolved, search_params.per_page, &query));
    match &mut result {
        search::SearchResponse::Ungrouped(r) => {
            r.request_params = rp;
            r.metadata = curation_metadata;
        }
        search::SearchResponse::Grouped(r) => {
            r.request_params = rp;
            r.metadata = curation_metadata;
        }
    }
    if let Some(s) = &scoped {
        apply_scoped_result_limits(&mut result, s);
    }
    shape_hits(&mut result, include_fields.as_deref(), &projection, &view, &resolved, &state.collections, &state.aliases);
    Ok(Searched { result, resolved, collection, view, params: search_params, query, nl, meta_excluded })
}

/// The paging parameters a union takes from the URL, which its searches leave out.
const UNION_PAGING: [&str; 5] = ["page", "per_page", "offset", "limit", "limit_hits"];

/// A union's paging (Typesense's global union parameters): the URL's page, per_page (or
/// limit), offset (without a page) and limit_hits.
struct UnionPaging {
    page: usize,
    per_page: usize,
    start: usize,
    limit_hits: usize,
}

impl UnionPaging {
    fn of(common: &RawParams) -> Result<Self, UnionError> {
        let number = |k: &str| -> Result<Option<usize>, UnionError> {
            common
                .get(k)
                .map(|v| {
                    v.trim().parse::<usize>().map_err(|_| UnionError(400, format!("Error while initializing global parameters of union: Parameter `{k}` must be an unsigned integer.")))
                })
                .transpose()
        };
        let (per_page, page, offset) = resolve_paging(number("per_page")?, number("limit")?, number("page")?, number("offset")?, false).map_err(|e| UnionError::of(&e))?;
        Ok(Self { page, per_page, start: offset.unwrap_or((page - 1) * per_page), limit_hits: number("limit_hits")?.unwrap_or(usize::MAX) })
    }

    /// Where the page ends in the union's hits.
    fn end(&self) -> usize {
        (self.start + self.per_page).min(self.limit_hits)
    }

    /// The hits each search gives: up to the page's end, with top_k facets at least their hits.
    fn fetch(&self, top_k: bool) -> usize {
        if top_k { self.end().max(250) } else { self.end() }
    }
}

/// A union failing: its status and message, answered as the union's response (Typesense).
struct UnionError(u16, String);

impl UnionError {
    fn of(e: &AppError) -> Self {
        let (status, message) = e.status();
        Self(status.as_u16(), message.to_string())
    }
}

/// A failed union's response: its code and error, with status 200 as Typesense answers it (a
/// timed-out one with its status).
fn union_failure(e: UnionError) -> axum::response::Response {
    if e.0 == 408 {
        return AppError::RequestTimeout(e.1).into_response();
    }
    TsJson(json!({ "code": e.0, "error": e.1 })).into_response()
}

/// Whether a facet_by asks for a top_k facet.
fn facet_top_k(facet_by: &str, schema: &Schema) -> bool {
    crate::index::facet::parse_facet_by(facet_by, schema).is_ok_and(|f| f.local.iter().any(|s| s.top_k))
}

/// A facet_by's entries, at its top-level commas.
fn facet_entries(facet_by: &str) -> Vec<&str> {
    let (mut out, mut depth, mut from) = (Vec::new(), 0i32, 0usize);
    for (i, ch) in facet_by.char_indices() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(facet_by[from..i].trim());
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(facet_by[from..].trim());
    out.retain(|e| !e.is_empty());
    out
}

/// What a union asks of its searches (Typesense): all grouped or none; each sorting as the first
/// does (as many sort fields, of the same types, in the same orders); and faceting alike (a field
/// faceted the same way everywhere, one facet_query, one facet_strategy, one facet_return_parent
/// per field).
fn check_union(state: &AppState, searches: &[&Prepared], raws: &[RawParams]) -> Result<(), UnionError> {
    let bad = |m: String| UnionError(400, m);
    let grouped = searches.iter().filter(|p| p.params.group_by.is_some()).count();
    if grouped > 0 && grouped != searches.len() {
        return Err(bad("Invalid group_by searches count. All searches with union search should be uniform.".into()));
    }

    let sort_of = |p: &Prepared| {
        let ctx = crate::filter::ops::JoinContext::new(&state.collections, &state.aliases, &p.resolved);
        search::union_sort(&p.params, &p.collection.schema, Some(&ctx)).map_err(|e| UnionError::of(&search_error(e)))
    };
    let Some(first) = searches.first() else { return Ok(()) };
    let first_sort = sort_of(first)?;
    let first_default = search::sorts_by_default(&first.params, &first.collection.schema);
    let list = |fields: &[search::UnionSortField]| fields.iter().map(|f| format!("`{}: {}`", f.name, f.kind)).collect::<Vec<_>>().join(", ");
    let order = |desc: bool| if desc { "DESC" } else { "ASC" };
    for (i, p) in searches.iter().enumerate().skip(1) {
        let sort = sort_of(p)?;
        if sort.len() != first_sort.len() {
            return Err(bad(format!(
                "Expected size of `sort_by` parameter of all searches to be equal. The first union search sorts on {{{}}} but the search at index `{i}` sorts on {{{}}}.",
                list(&first_sort),
                list(&sort)
            )));
        }
        let default = search::sorts_by_default(&p.params, &p.collection.schema);
        for (this, that) in sort.iter().zip(&first_sort) {
            if this.kind != that.kind {
                let hint = match (default, first_default) {
                    (true, true) => format!(
                        " Both `{}` and `{}` collections have declared a default sorting field of different type. Since union expects the searches to sort_by on the same type of fields, default sorting fields of the collections should be removed.",
                        p.resolved, first.resolved
                    ),
                    (true, false) => format!(
                        " `{}` collection has declared a default sorting field of different type. Since union expects the searches to sort_by on the same type of fields, default sorting field of the collection should be removed.",
                        p.resolved
                    ),
                    (false, true) => format!(
                        " `{}` collection has declared a default sorting field of different type. Since union expects the searches to sort_by on the same type of fields, default sorting field of the collection should be removed.",
                        first.resolved
                    ),
                    (false, false) => String::new(),
                };
                return Err(bad(format!(
                    "Expected type of `{}` sort_by ({}) at search index `{i}` to be the same as the type of `{}` sort_by ({}) at search index `0`.{hint}",
                    this.name, this.kind, that.name, that.kind
                )));
            }
            if this.desc != that.desc {
                return Err(bad(format!(
                    "Expected order of `{}` sort_by ({}) at search index `{i}` to be the same as the order of `{}` sort_by ({}) at search index `0`.",
                    this.name,
                    order(this.desc),
                    that.name,
                    order(that.desc)
                )));
            }
        }
    }

    // faceting alike
    let generic = " should be uniform across searches for faceting with union search.";
    let faceting: Vec<(&Prepared, &RawParams)> = searches.iter().zip(raws).filter(|(p, _)| p.params.facet_by.as_deref().is_some_and(|f| !f.trim().is_empty())).map(|(p, r)| (*p, r)).collect();
    let Some(&(first, first_raw)) = faceting.first() else { return Ok(()) };
    let mut fields: rustc_hash::FxHashMap<String, (String, bool)> = rustc_hash::FxHashMap::default();
    for (p, raw) in &faceting {
        if raw.get("facet_strategy") != first_raw.get("facet_strategy") {
            return Err(bad(format!("`facet_strategy`{generic}")));
        }
        if p.params.facet_query.as_deref().unwrap_or("") != first.params.facet_query.as_deref().unwrap_or("") {
            return Err(bad(format!("`facet_query`{generic}")));
        }
        let parents: Vec<&str> = p.params.facet_return_parent.as_deref().unwrap_or("").split(',').map(str::trim).collect();
        for entry in facet_entries(p.params.facet_by.as_deref().unwrap_or("")) {
            let name = entry.split('(').next().unwrap_or(entry).trim();
            let parent = parents.iter().any(|v| *v == "*" || *v == name);
            match fields.get(name) {
                Some((e, _)) if e != entry => return Err(bad(format!("facet fields{generic}"))),
                Some((_, r)) if *r != parent => return Err(bad(format!("`facet_return_parent`{generic}"))),
                Some(_) => {}
                None => {
                    fields.insert(name.to_string(), (entry.to_string(), parent));
                }
            }
        }
    }
    Ok(())
}

/// A union of searches (Typesense's `union: true`): their hits in one ordering, by the sort keys
/// they share (`check_union`), ties to the earlier search and within a search its own order; the
/// pinned ones where they are pinned (the first search's at a place taken, the next after).
/// Removing duplicates (the default, without group_by), a document found by several searches of
/// its collection appears once, where it ranks best, and `found` and the facets count each
/// document once (Typesense subtracts duplicates only among the hits it fetched, so its `found`
/// changes with the page asked for); else they add up. `out_of` counts each collection once.
fn union_response(state: &AppState, mut parts: Vec<Searched>, paging: &UnionPaging, remove_duplicates: bool, search_time_ms: u64) -> Result<Value, UnionError> {
    use rustc_hash::{FxHashMap, FxHashSet};
    use search::SearchResponse::{Grouped, Ungrouped};
    if parts.is_empty() {
        return Ok(json!({ "found": 0, "out_of": 0, "search_time_ms": search_time_ms, "page": paging.page, "hits": [], "facet_counts": [], "union_request_params": [], "search_cutoff": false }));
    }
    let grouped = matches!(parts[0].result, Grouped(_));
    let distinct = remove_duplicates && !grouped;
    // without removing duplicates, each search's facets, to add up
    let own_facets: Vec<Vec<crate::index::facet::FacetResult>> = if distinct {
        Vec::new()
    } else {
        parts
            .iter_mut()
            .map(|p| match &mut p.result {
                Ungrouped(r) => std::mem::take(&mut r.facet_counts),
                Grouped(g) => std::mem::take(&mut g.facet_counts),
            })
            .collect()
    };
    let first = &parts[0];
    let order = search::HitOrder::new(&first.params, &first.collection.schema).map_err(|e| UnionError::of(&search_error(e)))?;
    let schema = &first.collection.schema;

    // the collections searched, each once
    let mut names: Vec<&str> = Vec::new();
    let cid: Vec<usize> = parts
        .iter()
        .map(|p| match names.iter().position(|n| *n == p.resolved) {
            Some(c) => c,
            None => {
                names.push(&p.resolved);
                names.len() - 1
            }
        })
        .collect();

    // per search its hits (or groups by their first hit) in its order, with their places
    let leads: Vec<Vec<(usize, &search::Hit)>> = parts
        .iter()
        .map(|p| match &p.result {
            Ungrouped(r) => r.hits.iter().enumerate().collect(),
            Grouped(g) => g.grouped_hits.iter().enumerate().filter_map(|(i, h)| Some((i, h.hits.first()?))).collect(),
        })
        .collect();
    let identity = |s: usize, h: &search::Hit| (cid[s], h.seq);

    // the pinned ones at their places (a hit's place in its search's first hits), by place then
    // search, each place after the one before
    let mut seen: FxHashSet<(usize, u64)> = FxHashSet::default();
    let mut pins: Vec<(usize, usize, usize)> = leads
        .iter()
        .enumerate()
        .flat_map(|(s, l)| l.iter().enumerate().filter(|(_, (_, h))| h.curated == Some(true)).map(move |(place, &(i, _))| (place, s, i)))
        .collect();
    pins.sort_unstable();
    let mut placed_pins: Vec<(usize, usize, usize)> = Vec::with_capacity(pins.len());
    for (place, s, i) in pins {
        let lead = leads[s].iter().find(|(j, _)| *j == i).map(|(_, h)| *h).expect("a pinned hit");
        if distinct && !seen.insert(identity(s, lead)) {
            continue;
        }
        let slot = placed_pins.last().map_or(place, |&(prev, _, _)| place.max(prev + 1));
        placed_pins.push((slot, s, i));
    }

    // the ordering, up to what the page and top_k facets need
    let top_k = distinct && parts.iter().any(|p| p.params.facet_by.as_deref().is_some_and(|f| facet_top_k(f, &p.collection.schema)));
    let want = paging.fetch(top_k);
    let mut heads = vec![0usize; parts.len()];
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(want);
    let mut pins = placed_pins.into_iter().peekable();
    while merged.len() < want {
        if pins.peek().is_some_and(|&(slot, _, _)| slot <= merged.len()) {
            let (_, s, i) = pins.next().expect("peeked");
            merged.push((s, i));
            continue;
        }
        let mut best: Option<usize> = None;
        for s in 0..parts.len() {
            while let Some(&(_, h)) = leads[s].get(heads[s]) {
                if h.curated == Some(true) || (distinct && seen.contains(&identity(s, h))) {
                    heads[s] += 1;
                } else {
                    break;
                }
            }
            let Some(&(_, h)) = leads[s].get(heads[s]) else { continue };
            best = match best {
                Some(b) if order.cmp(leads[b][heads[b]].1, h, schema) != std::cmp::Ordering::Greater => Some(b),
                _ => Some(s),
            };
        }
        let Some(s) = best else {
            // only pinned ones left: in their order
            match pins.next() {
                Some((_, s, i)) => {
                    merged.push((s, i));
                    continue;
                }
                None => break,
            }
        };
        let (i, h) = leads[s][heads[s]];
        heads[s] += 1;
        if distinct {
            seen.insert(identity(s, h));
        }
        merged.push((s, i));
    }

    // found: each document once (per collection the union of its searches' results), else added up
    let docs_of = |c: usize, only: Option<&dyn Fn(&Searched) -> bool>| -> Vec<crate::bitmap::Bitmap> {
        let mut out: Vec<crate::bitmap::Bitmap> = Vec::new();
        for p in (0..parts.len()).filter(|&s| cid[s] == c).map(|s| &parts[s]).filter(|p| only.is_none_or(|f| f(p))) {
            if let Ungrouped(r) = &p.result {
                if out.len() < r.segment_docs.len() {
                    out.resize(r.segment_docs.len(), crate::bitmap::Bitmap::new());
                }
                for (k, d) in r.segment_docs.iter().enumerate() {
                    out[k] |= d;
                }
            }
        }
        out
    };
    let (found, found_docs) = match () {
        _ if grouped => parts.iter().fold((0usize, 0usize), |(f, d), p| match &p.result {
            Grouped(g) => (f + g.found, d + g.found_docs),
            Ungrouped(r) => (f + r.found, d + r.found),
        }),
        _ if distinct => ((0..names.len()).map(|c| docs_of(c, None).iter().map(|d| d.cardinality() as usize).sum::<usize>()).sum(), 0),
        _ => (parts.iter().map(|p| if let Ungrouped(r) = &p.result { r.found } else { 0 }).sum(), 0),
    };
    let out_of: u64 = (0..names.len()).map(|c| u64::from(parts[cid.iter().position(|&x| x == c).expect("searched")].view.total_docs())).sum();

    // facets: each document once over every search of its collection faceting alike, else each
    // search's added up
    let max_facet_values = parts.iter().find(|p| p.params.facet_by.is_some()).and_then(|p| p.params.max_facet_values);
    let mut facet_parts: Vec<Vec<crate::index::facet::FacetResult>> = own_facets;
    if distinct {
        for c in 0..names.len() {
            let mut kinds: Vec<&str> = Vec::new();
            for p in (0..parts.len()).filter(|&s| cid[s] == c).map(|s| &parts[s]) {
                if let Some(f) = p.params.facet_by.as_deref().filter(|f| !f.trim().is_empty() && !kinds.contains(f)) {
                    kinds.push(f);
                }
            }
            // per field the facets of every search of the collection faceting on it
            let mut by_field: FxHashMap<String, Vec<&str>> = FxHashMap::default();
            for &kind in &kinds {
                for entry in facet_entries(kind) {
                    by_field.entry(entry.split('(').next().unwrap_or(entry).trim().to_string()).or_default().push(kind);
                }
            }
            for &kind in &kinds {
                let p = (0..parts.len()).filter(|&s| cid[s] == c).map(|s| &parts[s]).find(|p| p.params.facet_by.as_deref() == Some(kind)).expect("a search faceting");
                // the documents of the searches faceting this way (or, for a field others facet too, all of theirs)
                let mut fields: Vec<(Vec<&str>, Vec<String>)> = Vec::new();
                for entry in facet_entries(kind) {
                    let name = entry.split('(').next().unwrap_or(entry).trim().to_string();
                    let with = by_field.get(&name).cloned().unwrap_or_default();
                    // a field faceted by several kinds: counted once, by the first of them
                    if with.first() != Some(&kind) {
                        continue;
                    }
                    match fields.iter_mut().find(|(w, _)| *w == with) {
                        Some((_, names)) => names.push(name),
                        None => fields.push((with, vec![name])),
                    }
                }
                for (with, wanted) in fields {
                    let docs = docs_of(c, Some(&|q: &Searched| q.params.facet_by.as_deref().is_some_and(|f| with.contains(&f))));
                    // the union's first hits of the collection, for top_k facets
                    let tops: Option<Vec<crate::bitmap::Bitmap>> = facet_top_k(kind, &p.collection.schema).then(|| {
                        let mut tops = vec![crate::bitmap::Bitmap::new(); p.view.len()];
                        for &(s, i) in merged.iter().take(paging.end().max(250)) {
                            if cid[s] != c {
                                continue;
                            }
                            if let Ungrouped(r) = &parts[s].result {
                                if let Some((k, d)) = r.hits.get(i).and_then(|h| p.view.locate(h.seq)) {
                                    tops[k].add(d);
                                }
                            }
                        }
                        tops
                    });
                    let ctx = crate::filter::ops::JoinContext::new(&state.collections, &state.aliases, &p.resolved);
                    let facets = search::facets_over(p.view.target(), &p.collection.schema, &p.params, &docs, tops.as_deref(), Some(&ctx)).map_err(|e| UnionError::of(&search_error(e)))?;
                    let keeps = |name: &str| wanted.iter().any(|w| w.contains('*') || name == w || name.starts_with(&format!("{w}(")));
                    facet_parts.push(facets.into_iter().filter(|f| keeps(&f.field_name)).collect());
                }
            }
        }
    }
    let facet_counts = search::merge_union_facets(facet_parts, max_facet_values);

    // the page
    let mut hits: Vec<Value> = Vec::new();
    for &(s, i) in merged.iter().skip(paging.start).take(paging.end().saturating_sub(paging.start)) {
        let tag = |v: &mut Value| {
            if let Some(o) = v.as_object_mut() {
                o.insert("search_index".into(), json!(s));
                o.insert("collection".into(), json!(parts[s].collection.name));
            }
        };
        let mut v = match &parts[s].result {
            Ungrouped(r) => serde_json::to_value(&r.hits[i]),
            Grouped(g) => serde_json::to_value(&g.grouped_hits[i]),
        }
        .map_err(|e| UnionError(500, e.to_string()))?;
        if grouped {
            for h in v.get_mut("hits").and_then(Value::as_array_mut).into_iter().flatten() {
                tag(h);
            }
        } else {
            tag(&mut v);
        }
        hits.push(v);
    }

    let union_request_params: Vec<Value> = parts
        .iter()
        .map(|p| {
            let found = match &p.result {
                Ungrouped(r) => r.found,
                Grouped(g) => g.found,
            };
            json!({ "collection_name": p.collection.name, "per_page": paging.per_page, "q": p.query, "found": found, "first_q": p.query })
        })
        .collect();
    let search_cutoff = parts.iter().any(|p| match &p.result {
        Ungrouped(r) => r.search_cutoff,
        Grouped(g) => g.search_cutoff,
    });
    let facet_counts = serde_json::to_value(&facet_counts).map_err(|e| UnionError(500, e.to_string()))?;
    let mut response = json!({
        "found": found,
        "out_of": out_of,
        "search_time_ms": search_time_ms,
        "page": paging.page,
    });
    let o = response.as_object_mut().expect("object");
    if grouped {
        o.insert("found_docs".into(), json!(found_docs));
        o.insert("grouped_hits".into(), Value::Array(hits));
    } else {
        o.insert("hits".into(), Value::Array(hits));
    }
    o.insert("facet_counts".into(), facet_counts);
    o.insert("union_request_params".into(), Value::Array(union_request_params));
    o.insert("search_cutoff".into(), json!(search_cutoff));
    Ok(response)
}

// -- Import handler --

#[derive(Deserialize)]
#[allow(dead_code)]
struct ImportParams {
    #[serde(default = "default_import_action")]
    action: String,
    #[serde(default)]
    return_id: bool,
    #[serde(default)]
    return_doc: bool,
    remote_embedding_batch_size: Option<usize>,
    // Typesense v30.1 compat: accepted but not used
    dirty_values: Option<String>,
    batch_size: Option<usize>,
}

/// Typesense's default: an import creates its documents.
fn default_import_action() -> String {
    "create".into()
}

/// Typesense's answer to an action it does not know.
fn check_action(action: &str) -> Result<(), AppError> {
    if matches!(action, "create" | "upsert" | "update" | "emplace") {
        return Ok(());
    }
    Err(AppError::BadRequest("Parameter `action` must be a create|update|upsert.".into()))
}

async fn import_documents(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(import_params): Query<ImportParams>,
    body: String,
) -> Result<impl IntoResponse, AppError> {

    // Preflight: estimate if we have enough memory for this import body
    crate::memory::check_memory_preflight(body.len() as u64)
        .map_err(AppError::ServiceUnavailable)?;

    // Admission control against currently-available physical memory. For JSONL we use the raw body size as the payload estimate
    // (no compression) and the default 6× build multiplier.
    if let Err(e) = crate::memory::check_import_admission(body.len() as u64) {
        return Err(AppError::InsufficientStorage(e));
    }

    // Preflight: reject if disk space is critically low (prevents silent persistence failures)
    crate::memory::check_disk_preflight()
        .map_err(AppError::ServiceUnavailable)?;

    check_action(&import_params.action)?;
    let import_start = Instant::now();
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    // Dynamic schema (object/object[]/auto): discover concrete sub-fields
    // from the incoming docs, extend the schema, and re-seed the index so
    // the normal import flow below runs against the extended collection.
    // Gated on marker fields being present — plain schemas skip this
    // entirely (no extra JSON parse).
    let collection = if collection
        .schema
        .fields
        .iter()
        .any(|f| f.field_type.is_dynamic_marker())
    {
        let parsed: Vec<Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let refs: Vec<&Value> = parsed.iter().collect();
        let discovered = ingest::discover_dynamic_fields(&collection.schema, &refs);
        if discovered.is_empty() {
            collection
        } else {
            let discovered_count = discovered.len();
            let updated = change_schema(&state, &resolved, discovered, None, None, None).await?;
            tracing::info!(
                collection = resolved.as_str(),
                discovered = discovered_count,
                "dynamic fields discovered from import"
            );
            updated
        }
    } else {
        collection
    };

    let mut documents: Vec<crate::schema::Document> = Vec::new();
    let mut results = Vec::new();
    let action = import_params.action.as_str();
    // The documents this import writes, by id (their place in `documents`): a later line of the
    // import sees what an earlier one wrote.
    let mut written: rustc_hash::FxHashMap<String, usize> = rustc_hash::FxHashMap::default();

    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let col_labels = observability::CollectionLabels {
            collection: resolved.clone(),
        };
        // A line that fails, as Typesense answers it (the line itself with return_doc).
        let failure = |code: u16, error: String| {
            let mut res = json!({"code": code, "error": error, "success": false});
            if import_params.return_doc {
                res["document"] = json!(line);
            }
            res.to_string()
        };
        let json_val = match serde_json::from_str::<Value>(line) {
            Ok(v) => v,
            Err(e) => {
                state.metrics.ingest_parse_errors_total.get_or_create(&col_labels).inc();
                results.push(failure(400, format!("Invalid JSON: {e}")));
                continue;
            }
        };
        let ext_id = json_val.get("id").and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        let pending = ext_id.as_ref().and_then(|id| written.get(id).copied());
        let exists = pending.is_some() || ext_id.as_deref().is_some_and(|id| collection.contains(id));
        match (action, ext_id.as_deref()) {
            ("create", Some(id)) if exists => {
                results.push(failure(409, format!("A document with id {id} already exists.")));
                continue;
            }
            ("update", None) => {
                results.push(failure(400, "For update, the `id` key must be provided.".into()));
                continue;
            }
            ("update", Some(id)) if !exists => {
                results.push(failure(404, format!("Could not find a document with id: {id}")));
                continue;
            }
            _ => {}
        }
        // update, and emplace of an existing document, change only the fields given
        let doc_json = match ext_id.as_deref() {
            Some(id) if exists && matches!(action, "update" | "emplace") => {
                let base = match pending {
                    Some(k) => Some(documents[k].clone()),
                    None => collection.document(id),
                };
                let mut merged = base.map_or_else(|| json_val.clone(), |d| document_to_json(&d, &collection.schema));
                if let (Some(m), Some(updates)) = (merged.as_object_mut(), json_val.as_object()) {
                    for (k, v) in updates {
                        m.insert(k.clone(), v.clone());
                    }
                }
                merged
            }
            _ => json_val.clone(),
        };
        match ingest::parse_json_document(&doc_json, &collection.schema, 0) {
            Ok(doc) => {
                let id = doc.external_id.clone();
                let result_line = if import_params.return_doc {
                    let mut obj = serde_json::Map::new();
                    obj.insert("success".into(), json!(true));
                    if import_params.return_id {
                        obj.insert("id".into(), json!(id));
                    }
                    obj.insert("document".into(), doc_json);
                    Value::Object(obj).to_string()
                } else if import_params.return_id {
                    json!({"success": true, "id": id}).to_string()
                } else {
                    r#"{"success": true}"#.to_string()
                };
                results.push(result_line);
                match pending {
                    Some(k) => documents[k] = doc,
                    None => {
                        written.insert(id, documents.len());
                        documents.push(doc);
                    }
                }
            }
            Err(e) => {
                state.metrics.ingest_schema_mismatches_total.get_or_create(&col_labels).inc();
                results.push(failure(400, e));
            }
        }
    }

    // Auto-embedding: fill missing embed-field vectors via the remote
    // model before indexing. A failure fails the whole import.
    if collection.schema.fields.iter().any(|f| f.embed.is_some()) && !documents.is_empty() {
        let schema = Arc::clone(&collection.schema);
        let batch = import_params.remote_embedding_batch_size;
        let docs_moved = std::mem::take(&mut documents);
        let embedded = tokio::task::spawn_blocking(move || {
            let mut docs = docs_moved;
            crate::embedding::embed_documents(&schema, &mut docs, batch, None, None)
                .map(|n| (docs, n))
        })
        .await
        .map_err(|e| AppError::Internal(format!("Embedding task failed: {e}")))?;
        let (docs, n) =
            embedded.map_err(|e| AppError::BadRequest(format!("Auto-embedding failed: {e}")))?;
        documents = docs;
        if n > 0 {
            tracing::info!(
                embedded = n,
                collection = resolved.as_str(),
                "auto-embedded documents"
            );
        }
    }

    let docs_imported = documents.len();

    if !documents.is_empty() {
        // Check memory limit before building index
        if let Err(msg) = state.memory_guard.check() {
            return Err(AppError::ServiceUnavailable(msg));
        }

        // One write: a segment of its own (realtime), or a rebuild keeping each document's place (batch)
        write_documents(&state, &collection, documents, Vec::new()).await?;

        let docs_total = collection.snapshot_bundle().total_docs();

        let build_ms = import_start.elapsed().as_millis();
        tracing::info!(
            collection = resolved.as_str(),
            docs_imported,
            docs_total,
            build_ms,
            action,
            "HTTP import completed"
        );

        // Record ingest metrics
        state
            .metrics
            .ingest_sessions_total
            .get_or_create(&observability::IngestLabels {
                collection: resolved.clone(),
                status: "success".into(),
            })
            .inc();
        state
            .metrics
            .ingest_duration
            .get_or_create(&observability::CollectionLabels {
                collection: resolved.clone(),
            })
            .observe(import_start.elapsed().as_secs_f64());
        state
            .metrics
            .ingest_docs_total
            .get_or_create(&observability::CollectionLabels {
                collection: resolved,
            })
            .inc_by(docs_imported as u64);
    }

    let response_body = results.join("\n");
    Ok((StatusCode::OK, response_body))
}

// -- Binary file import --
//
// Accepts a binary file with format:
//
//   v1 (16-byte header):
//     [4 bytes] magic: "TQBF"
//     [4 bytes] version: 1
//     [4 bytes] doc_count: u32 LE
//     [4 bytes] flags: bit 0 = LZ4, bit 1 = ZLib
//     [N bytes] payload: concatenated serialized documents (compressed per flags)
//
//   v2 (24-byte header):
//     [4 bytes] magic: "TQBF"
//     [4 bytes] version: 2
//     [4 bytes] doc_count: u32 LE
//     [4 bytes] flags: bit 0 = LZ4, bit 1 = ZLib
//     [8 bytes] uncompressed_size: u64 LE  ← NEW in v2
//     [N bytes] payload: concatenated serialized documents (compressed per flags)
//
// Why v2: v1 didn't include the uncompressed payload size, so the ZLib decoder
// path used Vec::new() + read_to_end, which doubles capacity as it grows. With
// v2 the destination Vec is sized exactly once.

const TQBF_MAGIC: &[u8; 4] = b"TQBF";
const TQBF_VERSION_V1: u32 = 1;
const TQBF_VERSION_V2: u32 = 2;
const TQBF_FLAG_LZ4: u32 = 1;
const TQBF_FLAG_ZLIB: u32 = 2;

async fn import_binary(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, AppError> {
    let _fs_guard = state.fs_serial.lock().await;
    crate::memory::check_memory_preflight(body.len() as u64)
        .map_err(AppError::ServiceUnavailable)?;
    crate::memory::check_disk_preflight()
        .map_err(AppError::ServiceUnavailable)?;

    let import_start = Instant::now();
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    // Parse header. v1 = 16 bytes; v2 = 24 bytes (adds u64 uncompressed_size).
    if body.len() < 16 {
        return Err(AppError::BadRequest("Binary file too small (need 16-byte header)".into()));
    }
    let magic = &body[0..4];
    if magic != TQBF_MAGIC {
        return Err(AppError::BadRequest("Invalid binary file magic (expected TQBF)".into()));
    }
    let version = u32::from_le_bytes(body[4..8].try_into().unwrap());
    let doc_count = u32::from_le_bytes(body[8..12].try_into().unwrap());
    let flags = u32::from_le_bytes(body[12..16].try_into().unwrap());

    let (uncompressed_size_hint, header_len): (Option<usize>, usize) = match version {
        TQBF_VERSION_V1 => (None, 16),
        TQBF_VERSION_V2 => {
            if body.len() < 24 {
                return Err(AppError::BadRequest(
                    "TQBF v2 file too small (need 24-byte header)".into(),
                ));
            }
            let size = u64::from_le_bytes(body[16..24].try_into().unwrap()) as usize;
            (Some(size), 24)
        }
        _ => {
            return Err(AppError::BadRequest(format!(
                "Unsupported TQBF version: {version}"
            )));
        }
    };

    // Admission control. Estimate peak memory from the uncompressed size (v2 header) or a
    // conservative multiple of the compressed body (v1), and answer 507 Insufficient Storage when
    // the process doesn't have room.
    //
    // Segment-aware: the streaming builder only holds ~one segment's build
    // state at a time, so the 6x multiplier applies to a segment, not the
    // whole payload (payload + serving data account for the constant 2x).
    let admission_bytes: u64 = uncompressed_size_hint
        .map(|u| u as u64)
        .unwrap_or_else(|| (body.len() as u64).saturating_mul(5));
    let seg_size = crate::ingest::builder::STREAMING_SEGMENT_DOCS as u64;
    let num_segments = (doc_count as u64).max(1).div_ceil(seg_size);
    let factor = if num_segments > 1 {
        2 + 6u64.div_ceil(num_segments)
    } else {
        crate::memory::DEFAULT_BUILD_MULTIPLIER
    };
    if let Err(e) = crate::memory::check_import_admission_with_factor(admission_bytes, factor) {
        return Err(AppError::InsufficientStorage(e));
    }

    let raw_payload = &body[header_len..];

    let payload: Vec<u8> = if (flags & TQBF_FLAG_LZ4) != 0 {
        lz4_flex::decompress_size_prepended(raw_payload)
            .map_err(|e| AppError::BadRequest(format!("LZ4 decompression failed: {e}")))?
    } else if (flags & TQBF_FLAG_ZLIB) != 0 {
        use std::io::Read;
        let mut decoder = flate2::read::ZlibDecoder::new(raw_payload);
        let mut decompressed = match uncompressed_size_hint {
            Some(size) => Vec::with_capacity(size),
            None => Vec::new(),
        };
        decoder.read_to_end(&mut decompressed)
            .map_err(|e| AppError::BadRequest(format!("Zlib decompression failed: {e}")))?;
        decompressed
    } else {
        raw_payload.to_vec()
    };

    // v2 sanity check: actual decompressed size must match the header hint.
    if let Some(hint) = uncompressed_size_hint {
        if payload.len() != hint {
            return Err(AppError::BadRequest(format!(
                "TQBF v2 uncompressed size mismatch: header={hint} actual={}",
                payload.len()
            )));
        }
    }

    let actual_count = doc_count as usize;

    // Build index — streaming for batch mode, chunked decode for realtime
    if collection.schema.index_mode == crate::schema::IndexMode::Realtime {
        // Realtime mode: decode and upsert individual documents
        let all_docs = ingest::protocol::decode_doc_batch_chunked(&payload, doc_count, &collection.schema, 5000)
            .map_err(|e| AppError::BadRequest(format!("Failed to decode binary documents: {e}")))?;
        write_documents(&state, &collection, all_docs, Vec::new()).await?;
        return Ok(Json(json!({
            "success": true,
            "num_imported": actual_count,
            "compressed": flags != 0,
            "elapsed_ms": (import_start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
        })));
    }

    // Batch mode: split the payload into per-segment byte chunks, then spawn the build task.
    let chunks = match ingest::builder::split_payload_into_segment_chunks(
        payload,
        doc_count,
        &collection.schema,
    ) {
        Ok(c) => c,
        Err(e) => {
            return Err(AppError::BadRequest(format!(
                "Failed to split payload into segments: {e}"
            )));
        }
    };

    let col = collection.clone();
    let state2 = state.clone();
    let data_dir = state.config.data_dir.clone();
    let schema = collection.schema.clone();
    #[cfg(feature = "cuda")]
    let gpu = state.gpu.clone();
    let build_guard =
        crate::observability::track_in_flight(&state.metrics.index_builds_in_flight);
    let build_fn = move || {
        // Holds the in-flight gauge until the save below is counted;
        // save_index_background bumps the saves gauge before returning,
        // so quiesce never sees a gap between build and save.
        let _build_guard = build_guard;
        let bundle = ingest::builder::build_index_streaming_from_chunks(
            chunks,
            doc_count,
            &schema,
            Some(&data_dir),
            Some(&col.name),
            #[cfg(feature = "cuda")]
            gpu.as_ref(),
        );
        crate::memory::purge_allocator();
        col.swap_bundle(bundle);
        crate::memory::purge_allocator();
        save_index_background(&state2, &col);
    };
    tokio::task::spawn_blocking(build_fn);

    let elapsed = import_start.elapsed();
    tracing::info!(
        collection = resolved.as_str(),
        docs = actual_count,
        compressed = flags != 0,
        payload_bytes = raw_payload.len(),
        elapsed_ms = format!("{:.1}", elapsed.as_secs_f64() * 1000.0).as_str(),
        "Binary file import complete (index building async)"
    );

    Ok(Json(json!({
        "success": true,
        "num_imported": actual_count,
        "compressed": flags != 0,
        "elapsed_ms": (elapsed.as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
    })))
}

// -- Single document CRUD --

async fn get_document(
    State(state): State<Arc<AppState>>,
    Path((name, doc_id)): Path<(String, String)>,
) -> Result<TsJson<Value>, AppError> {
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    match document_json(&collection, &doc_id)? {
        Some(doc) => Ok(TsJson(doc)),
        None => Err(AppError::NotFound(format!("Could not find a document with id: {doc_id}"))),
    }
}

/// The live document `doc_id` as Typesense returns it (GET; DELETE answers with it): the whole
/// document as written.
fn document_json(collection: &crate::collection::Collection, doc_id: &str) -> Result<Option<Value>, AppError> {
    // The live copy: older copies are in their segments' deletion masks.
    let bundle = collection.snapshot_bundle();
    let Some((seg_idx, local_id)) = bundle.live_lookup(doc_id) else { return Ok(None) };
    let seg = &bundle.segments[seg_idx];
    if let Some(records) = seg.doc_store.records() {
        if let Some(v) = records.value(local_id).map_err(|e| AppError::Internal(e.to_string()))? {
            return Ok(Some(v));
        }
    }
    Ok(seg.doc_store.get_doc(local_id, &collection.schema).map(|doc| document_to_json(&doc, &collection.schema)))
}

#[derive(Deserialize)]
struct DocWriteParams {
    action: Option<String>,
    // Typesense v30.1 compat: accepted but not used (dirty_values coercion
    // rules are not implemented; documents are validated against the schema).
    #[allow(dead_code)]
    dirty_values: Option<String>,
}

async fn create_document(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<DocWriteParams>,
    Json(json_val): Json<Value>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let action = params.action.as_deref().unwrap_or("create");
    check_action(action)?;

    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    // Dynamic schema: discover sub-fields from this document (see import).
    let collection = if collection
        .schema
        .fields
        .iter()
        .any(|f| f.field_type.is_dynamic_marker())
    {
        let discovered =
            ingest::discover_dynamic_fields(&collection.schema, &[&json_val]);
        if discovered.is_empty() {
            collection
        } else {
            change_schema(&state, &resolved, discovered, None, None, None).await?
        }
    } else {
        collection
    };

    let ext_id = json_val
        .get("id")
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .ok_or_else(|| AppError::BadRequest("Missing or invalid 'id' field".into()))?;

    let existing = collection.document(&ext_id);
    let existing = existing.as_ref();

    match action {
        "create" if existing.is_some() => {
            return Err(AppError::Conflict(format!(
                "A document with id {ext_id} already exists."
            )));
        }
        "update" if existing.is_none() => {
            return Err(AppError::NotFound(format!("Could not find a document with id: {ext_id}")));
        }
        _ => {}
    }

    // update / emplace-on-existing merge the payload into the stored doc;
    // create / upsert / emplace-on-missing index the payload as a full document.
    let doc_json = match (action, existing) {
        ("update", Some(existing)) | ("emplace", Some(existing)) => {
            let mut doc_json = document_to_json(existing, &collection.schema);
            if let (Some(base), Some(updates)) = (doc_json.as_object_mut(), json_val.as_object()) {
                for (k, v) in updates {
                    base.insert(k.clone(), v.clone());
                }
            }
            doc_json
        }
        _ => json_val.clone(),
    };

    let doc = ingest::parse_json_document(&doc_json, &collection.schema, 0)
        .map_err(AppError::BadRequest)?;

    // Auto-embedding for the single document (same path as bulk import).
    let doc = if collection.schema.fields.iter().any(|f| f.embed.is_some()) {
        let schema = Arc::clone(&collection.schema);
        tokio::task::spawn_blocking(move || {
            let mut docs = vec![doc];
            crate::embedding::embed_documents(&schema, &mut docs, None, None, None)
                .map(|_| docs.pop().unwrap())
        })
        .await
        .map_err(|e| AppError::Internal(format!("Embedding task failed: {e}")))?
        .map_err(|e| AppError::BadRequest(format!("Auto-embedding failed: {e}")))?
    } else {
        doc
    };

    write_documents(&state, &collection, vec![doc], Vec::new()).await?;

    Ok((StatusCode::CREATED, Json(doc_json)))
}

async fn update_document(
    State(state): State<Arc<AppState>>,
    Path((name, doc_id)): Path<(String, String)>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    let existing = collection
        .document(&doc_id)
        .ok_or_else(|| AppError::NotFound(format!("Could not find a document with id: {doc_id}")))?;

    // Merge: start with existing doc JSON, overlay patch fields
    let mut doc_json = document_to_json(&existing, &collection.schema);
    if let (Some(base), Some(updates)) = (doc_json.as_object_mut(), patch.as_object()) {
        for (k, v) in updates {
            base.insert(k.clone(), v.clone());
        }
    }

    let new_doc = ingest::parse_json_document(&doc_json, &collection.schema, 0)
        .map_err(AppError::BadRequest)?;

    write_documents(&state, &collection, vec![new_doc], Vec::new()).await?;

    Ok(Json(doc_json))
}

#[derive(Deserialize)]
struct DeleteDocumentParams {
    /// Typesense's: a document that is not there answers `{"id": ...}` instead of a 404.
    #[serde(default)]
    ignore_not_found: bool,
}

/// DELETE of one document: Typesense answers with the document deleted.
async fn delete_document(
    State(state): State<Arc<AppState>>,
    Path((name, doc_id)): Path<(String, String)>,
    Query(params): Query<DeleteDocumentParams>,
) -> Result<TsJson<Value>, AppError> {
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    let Some(doc) = document_json(&collection, &doc_id)? else {
        if params.ignore_not_found {
            return Ok(TsJson(json!({ "id": doc_id })));
        }
        return Err(AppError::NotFound(format!("Could not find a document with id: {doc_id}")));
    };
    write_documents(&state, &collection, Vec::new(), vec![doc_id]).await?;
    Ok(TsJson(doc))
}

// -- Export handler --

#[derive(Debug, Deserialize)]
struct ExportQuery {
    filter_by: Option<String>,
    include_fields: Option<String>,
    exclude_fields: Option<String>,
}

async fn export_documents(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<ExportQuery>,
) -> Result<impl IntoResponse, AppError> {
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    let view = collection.view();
    let schema = Arc::clone(&collection.schema);

    // Parse filter_by if provided
    let filter_op = if let Some(ref filter_str) = params.filter_by {
        let trimmed = filter_str.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(crate::filter::parser::parse_filter(trimmed, &schema)
                .map_err(|e| AppError::BadRequest(e.message))?
                .ok_or_else(|| AppError::BadRequest("Invalid filter expression.".into()))?)
        }
    } else {
        None
    };

    // Parse include/exclude fields
    let include_fields: Option<Vec<String>> = params.include_fields
        .map(|s| s.split(',').map(|f| f.trim().to_string()).collect());
    let exclude_fields: Option<Vec<String>> = params.exclude_fields
        .map(|s| s.split(',').map(|f| f.trim().to_string()).collect());

    // Stream JSONL: the live documents (every segment's, the segments in memory
    // too) in `_seq_id` order as Typesense exports them, each as it was written, serialized in
    // chunks. Only their places are gathered first.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(64);
    tokio::task::spawn_blocking(move || {
        const CHUNK_SIZE: usize = 500;
        let segments = view.segments();
        let mut places: Vec<(u64, u32, u32)> = Vec::new();
        for (i, (segment, deleted)) in segments.iter().enumerate() {
            let mut live = match filter_op {
                Some(ref op) => crate::filter::ops::execute_filter(op, segment, &schema),
                None => segment.all_docs.clone(),
            };
            if let Some(d) = deleted {
                live.andnot_inplace(d);
            }
            places.extend(live.iter().map(|doc| (segment.seq_id(doc), i as u32, doc)));
        }
        if !places.windows(2).all(|w| w[0].0 < w[1].0) {
            places.sort_unstable();
        }
        let mut buf = String::with_capacity(CHUNK_SIZE * 512);
        let mut emitted = 0usize;
        for (_, i, doc_id) in places {
            let segment = segments[i as usize].0;
            let stored = segment.doc_store.records().and_then(|r| r.value(doc_id).ok().flatten());
            let Some(mut json_val) = stored.or_else(|| segment.doc_store.get_doc(doc_id, &schema).map(|d| document_to_json(&d, &schema))) else {
                continue;
            };
            if let Some(obj) = json_val.as_object_mut() {
                if let Some(ref include) = include_fields {
                    obj.retain(|k, _| include.iter().any(|f| f == k));
                }
                if let Some(ref exclude) = exclude_fields {
                    obj.retain(|k, _| !exclude.iter().any(|f| f == k));
                }
            }
            if emitted > 0 {
                buf.push('\n');
            }
            if let Ok(line) = serde_json::to_string(&json_val) {
                buf.push_str(&line);
            }
            emitted += 1;
            if emitted % CHUNK_SIZE == 0 {
                if tx.blocking_send(Ok(std::mem::take(&mut buf))).is_err() {
                    return; // client disconnected
                }
                buf = String::with_capacity(CHUNK_SIZE * 512);
            }
        }
        if !buf.is_empty() {
            let _ = tx.blocking_send(Ok(buf));
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = axum::body::Body::from_stream(stream);
    Ok(body.into_response())
}

// -- Bulk delete by filter --

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeleteByFilterQuery {
    filter_by: Option<String>,
    truncate: Option<bool>,
    // Typesense v30.1 compat: accepted but not used. Deletion is applied as
    // one atomic index swap (no batching), and bulk delete never 404s per-doc.
    batch_size: Option<usize>,
    ignore_not_found: Option<bool>,
}

async fn delete_documents_by_filter(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<DeleteByFilterQuery>,
) -> Result<Json<Value>, AppError> {
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    let schema = Arc::clone(&collection.schema);

    // Typesense v30.1: filter_by is required unless truncate=true (delete all).
    let filter_str = params.filter_by.as_deref().unwrap_or("").trim();
    let filter_op = if params.truncate.unwrap_or(false) {
        None // Delete all
    } else if filter_str.is_empty() {
        return Err(AppError::BadRequest(
            "Parameter `filter_by` is required (pass `truncate=true` to delete all documents).".into(),
        ));
    } else {
        Some(crate::filter::parser::parse_filter(filter_str, &schema)
            .map_err(|e| AppError::BadRequest(e.message))?
            .ok_or_else(|| AppError::BadRequest("Invalid filter expression.".into()))?)
    };

    if filter_op.is_none() {
        // truncate: an empty segment set, without a deletion per document
        let num_deleted = collection.view().total_docs() as usize;
        let commit = collection.truncate().map_err(write_error)?;
        save_index_background(&state, &collection);
        durable(commit).await?;
        return Ok(Json(json!({ "num_deleted": num_deleted })));
    }

    // The live documents matching, in every segment, each
    // deleted once: an older copy is in its segment's deletion mask.
    let ids = matching_ids(&collection, filter_op.as_ref());
    let num_deleted = ids.len();
    write_documents(&state, &collection, Vec::new(), ids).await?;

    Ok(Json(json!({ "num_deleted": num_deleted })))
}

/// The ids of a collection's live documents matching a filter (every one without), in every
/// segment.
fn matching_ids(collection: &crate::collection::Collection, filter_op: Option<&crate::filter::FilterOp>) -> Vec<String> {
    let view = collection.view();
    let mut ids = Vec::new();
    for (seg, deleted) in view.segments() {
        let mut matching = match filter_op {
            Some(op) => crate::filter::ops::execute_filter(op, seg, &collection.schema),
            None => seg.all_docs.clone(),
        };
        if let Some(d) = deleted {
            matching.andnot_inplace(d);
        }
        ids.extend(matching.iter().filter_map(|d| seg.doc_store.external_id(d)));
    }
    ids
}

// -- Bulk update by filter (Typesense v30.1: PATCH /collections/{name}/documents) --

#[derive(Deserialize)]
struct UpdateByFilterQuery {
    filter_by: Option<String>,
}

async fn update_documents_by_filter(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<UpdateByFilterQuery>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    let schema = Arc::clone(&collection.schema);

    let filter_str = params.filter_by.as_deref().unwrap_or("").trim();
    if filter_str.is_empty() {
        return Err(AppError::BadRequest("Parameter `filter_by` is required.".into()));
    }
    let filter_op = crate::filter::parser::parse_filter(filter_str, &schema)
        .map_err(|e| AppError::BadRequest(e.message))?
        .ok_or_else(|| AppError::BadRequest("Invalid filter expression.".into()))?;

    let patch_obj = patch
        .as_object()
        .ok_or_else(|| AppError::BadRequest("Expected JSON object".into()))?;

    // Merge the patch into every live matching doc up front so a patch
    // that violates the schema rejects the whole request instead of applying to half the
    // matches. `id` is immutable and ignored if present in the patch.
    let mut updated_docs: Vec<crate::schema::Document> = Vec::new();
    for id in matching_ids(&collection, Some(&filter_op)) {
        let Some(doc) = collection.document(&id) else { continue };
        let mut doc_json = document_to_json(&doc, &schema);
        if let Some(base) = doc_json.as_object_mut() {
            for (k, v) in patch_obj {
                if k != "id" {
                    base.insert(k.clone(), v.clone());
                }
            }
        }
        updated_docs.push(ingest::parse_json_document(&doc_json, &schema, 0).map_err(AppError::BadRequest)?);
    }

    let num_updated = updated_docs.len();
    write_documents(&state, &collection, updated_docs, Vec::new()).await?;

    Ok(Json(json!({ "num_updated": num_updated })))
}

// -- Scoped key enforcement --

/// A scoped key's limit_hits (its filter_by and other parameters are layered in `layer_params`).
fn apply_scoped_params(
    search_params: &mut search::SearchParams,
    scoped: &crate::auth::scoped::ScopedKeyParams,
) {
    // Apply limit_hits pre-search: cap per_page so pagination works correctly
    if let Some(limit) = scoped.limit_hits {
        let limit = limit as usize;
        let offset = search_params.start();
        if offset >= limit {
            search_params.per_page = 0;
        } else {
            search_params.per_page = search_params.per_page.min(limit - offset);
        }
    }
}

fn apply_scoped_result_limits(
    result: &mut search::SearchResponse,
    scoped: &crate::auth::scoped::ScopedKeyParams,
) {
    // limit_hits: cap the reported `found` count (hits already capped pre-search via per_page)
    if let Some(limit) = scoped.limit_hits {
        let limit = limit as usize;
        match result {
            search::SearchResponse::Ungrouped(r) => {
                if r.found > limit {
                    r.found = limit;
                }
            }
            search::SearchResponse::Grouped(r) => {
                if r.found > limit {
                    r.found = limit;
                }
                if r.found_docs > limit {
                    r.found_docs = limit;
                }
            }
        }
    }
}

// -- Key handlers --

async fn create_key(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let key = state.keys.create(body);

    if let Err(e) = persistence::save_keys(&state.config.data_dir, &state.keys.export()) {
        tracing::error!("Failed to persist keys: {e}");
    }

    let value = serde_json::to_value(key)
        .map_err(|e| AppError::Internal(format!("Key serialization failed: {e}")))?;
    Ok((StatusCode::CREATED, Json(value)))
}

async fn get_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u64>,
) -> Result<Json<ApiKeySummary>, AppError> {
    state
        .keys
        .get_by_id(id)
        .map(|k| Json(ApiKeySummary::from(&k)))
        .ok_or_else(|| AppError::NotFound(format!("Key `{id}` not found.")))
}

async fn list_keys(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({ "keys": state.keys.list() }))
}

async fn delete_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u64>,
) -> Result<Json<Value>, AppError> {
    if !state.keys.delete(id) {
        return Err(AppError::NotFound(format!("Key `{id}` not found.")));
    }

    if let Err(e) = persistence::save_keys(&state.config.data_dir, &state.keys.export()) {
        tracing::error!("Failed to persist keys: {e}");
    }

    Ok(Json(json!({ "id": id })))
}

// -- Compact handler (realtime mode) --

async fn compact_collection(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    let resolved = state.aliases.resolve(&name);
    let collection = state
        .collections
        .get(&resolved)
        .ok_or_else(|| AppError::NotFound(format!("Collection `{name}` not found.")))?;

    if collection.schema.index_mode != crate::schema::IndexMode::Realtime {
        return Err(AppError::BadRequest(format!(
            "Collection `{name}` is not in realtime mode."
        )));
    }

    let (col, s) = (collection.clone(), Arc::clone(&state));
    let result = tokio::task::spawn_blocking(move || crate::collection::compaction::flush(&col, Some(&s.config.data_dir), &s.metrics))
        .await
        .map_err(|e| AppError::Internal(format!("Compaction task failed: {e}")))?;
    crate::memory::purge_allocator();

    Ok(Json(json!({
        "ok": true,
        "docs_compacted": result.docs_compacted,
        "total_docs": result.total_docs,
        "compact_ms": result.compact_ms,
    })))
}

// -- Suspend / Resume --

async fn suspend_collection(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    let resolved = state.aliases.resolve(&name);

    // Reject if aliases point to this collection
    let aliases = state.aliases.aliases_for_collection(&resolved);
    if !aliases.is_empty() {
        return Err(AppError::BadRequest(format!(
            "Cannot suspend collection `{}`: it has active aliases: {}. Delete the aliases first.",
            resolved,
            aliases.join(", ")
        )));
    }

    // saved, its log closed, kept in collections.json as suspended and out of the registry
    let doc_count = persistence::suspend_collection(&state.config.data_dir, &state.collections, &resolved).map_err(|e| match e {
        CollectionError::NotFound(_) => AppError::NotFound(format!("Collection `{name}` not found.")),
        other => collection_err(other),
    })?;
    crate::memory::purge_allocator();

    tracing::info!(collection = %resolved, docs = doc_count, "Collection suspended");
    Ok(Json(json!({
        "ok": true,
        "collection": resolved,
        "docs": doc_count,
        "message": "Collection suspended. Index persisted to disk. Use POST /collections/{name}/resume to reload."
    })))
}

async fn resume_collection(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    // Check if already loaded
    if state.collections.get(&name).is_some() {
        return Err(AppError::Conflict(format!(
            "Collection `{name}` is already loaded."
        )));
    }

    // Check memory preflight — estimate from index files on disk
    let index_dir = state.config.data_dir.join("indexes").join(&name);
    if index_dir.exists() {
        let mut total_bytes: u64 = 0;
        if let Ok(entries) = std::fs::read_dir(&index_dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    total_bytes += meta.len();
                }
            }
        }
        // Estimate 4x file size for decompression + in-memory structures
        crate::memory::check_memory_preflight(total_bytes * 2)
            .map_err(AppError::ServiceUnavailable)?;
    }

    let collection = persistence::resume_collection(&state.config.data_dir, &state.collections, &name, None).map_err(|e| match e {
        CollectionError::AlreadyExists(_) => AppError::Conflict(format!("Collection `{name}` is already loaded.")),
        CollectionError::NotFound(_) => AppError::NotFound(format!("Collection `{name}` not found in persisted data.")),
        other => collection_err(other),
    })?;
    let doc_count = collection.snapshot_bundle().total_docs();

    tracing::info!(collection = name.as_str(), docs = doc_count, "Collection resumed");
    Ok(Json(json!({
        "ok": true,
        "collection": name,
        "docs": doc_count,
    })))
}

// -- Override stubs (Typesense v30.1 compat) --

async fn list_overrides(Path(_name): Path<String>) -> Json<Value> {
    Json(json!({ "overrides": [] }))
}

async fn get_override(
    Path((_name, id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    Err(AppError::NotFound(format!("Override `{id}` not found.")))
}

async fn upsert_override(
    Path((_name, _id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    Err(AppError::BadRequest("Overrides are not supported.".into()))
}

async fn delete_override(
    Path((_name, id)): Path<(String, String)>,
) -> Result<Json<Value>, AppError> {
    Err(AppError::NotFound(format!("Override `{id}` not found.")))
}

// -- Error mapping --

fn classify_query_type(params: &search::SearchParams) -> String {
    let has_text = !params.q.is_empty() && params.q != "*";
    let has_vector = params.vector_query.is_some();
    match (has_text, has_vector) {
        (true, true) => "hybrid".into(),
        (false, true) => "vector".into(),
        (true, false) => "text".into(),
        (false, false) => "filter_only".into(),
    }
}

fn collection_err(e: CollectionError) -> AppError {
    match e {
        CollectionError::AlreadyExists(name) => {
            AppError::Conflict(format!("Collection `{name}` already exists."))
        }
        CollectionError::NotFound(name) => {
            AppError::NotFound(format!("Collection `{name}` not found."))
        }
        CollectionError::InvalidSchema(msg) => AppError::BadRequest(msg),
        CollectionError::LimitExceeded(msg) => AppError::BadRequest(msg),
        CollectionError::Storage(msg) => AppError::Internal(msg),
    }
}

/// Build an index from documents, dispatching to GPU or CPU as appropriate.
fn rebuild_index(
    data_dir: &std::path::Path,
    collection_name: &str,
    schema: &Schema,
    docs: Vec<crate::schema::Document>,
    #[cfg(feature = "cuda")] gpu: Option<&Arc<crate::gpu::GpuContext>>,
) -> crate::index::Index {
    // Backing dir threaded through so disk-backed columns (TextMapped and
    // stored-only offload) apply on every build path, not just TCP/TQBF.
    #[cfg(feature = "cuda")]
    {
        ingest::build_index_with_gpu_and_backing(schema, docs, gpu, data_dir, collection_name)
    }
    #[cfg(not(feature = "cuda"))]
    {
        ingest::build_index_with_backing(schema, docs, data_dir, collection_name)
    }
}

/// Write documents and delete ids, every document write's last step, answered once the write is
/// durable: a realtime collection's once its write-ahead log has it on disk (`apply_documents`).
pub(super) async fn write_documents(
    state: &Arc<AppState>,
    collection: &Arc<crate::collection::Collection>,
    upserts: Vec<crate::schema::Document>,
    deletes: Vec<String>,
) -> Result<(), AppError> {
    durable(apply_documents(state, collection, upserts, deletes)?).await
}

/// Resolve a write's acknowledgement from the write-ahead log (None: durable already).
pub(super) async fn durable(commit: Option<crate::persistence::wal::Commit>) -> Result<(), AppError> {
    match commit {
        Some(commit) => commit.wait().await.map_err(AppError::ServiceUnavailable),
        None => Ok(()),
    }
}

fn write_error(e: crate::collection::WriteError) -> AppError {
    match e {
        crate::collection::WriteError::Invalid(e) => AppError::BadRequest(e),
        crate::collection::WriteError::Log(e) => AppError::ServiceUnavailable(e),
    }
}

/// Write documents and delete ids, without waiting for durability. A realtime collection takes
/// them as one write, a segment of its own (refused while the segments in memory are full; merged
/// and flushed in the background), durable once the returned acknowledgement resolves; a batch
/// collection is rebuilt from its live documents, a written document in the place of the copy it
/// replaces (Typesense keeps its sequence id) and new ones last, and saved in the background.
pub(super) fn apply_documents(
    state: &Arc<AppState>,
    collection: &Arc<crate::collection::Collection>,
    upserts: Vec<crate::schema::Document>,
    deletes: Vec<String>,
) -> Result<Option<crate::persistence::wal::Commit>, AppError> {
    if upserts.is_empty() && deletes.is_empty() {
        return Ok(None);
    }
    if collection.schema.index_mode == crate::schema::IndexMode::Realtime {
        refuse_when_full(state, collection)?;
        let commit = collection.apply_writes(upserts, &deletes).map_err(write_error)?;
        flush_when_due(state, collection.successor.get().unwrap_or(collection));
        return Ok(commit);
    }
    {
        // One rebuild at a time, published alone: a segment merge in progress finishes first.
        let (publishing, writing) = collection.exclusive();
        if let Some(next) = collection.successor.get() {
            // a schema change replaced the collection: the write is the new one's
            let upserts = upserts
                .iter()
                .map(|d| ingest::retype(d, &collection.schema, &next.schema))
                .collect::<Result<Vec<_>, _>>()
                .map_err(AppError::BadRequest)?;
            drop((publishing, writing));
            return apply_documents(state, next, upserts, deletes);
        }
        let docs = crate::collection::merge_writes(collection.snapshot_bundle().live_documents(), upserts, &deletes);
        let index = rebuild_index(
            &state.config.data_dir,
            &collection.name,
            &collection.schema,
            docs,
            #[cfg(feature = "cuda")]
            state.gpu.as_ref(),
        );
        collection.swap_index(index);
    }
    save_index_background(state, collection);
    crate::memory::purge_allocator();
    Ok(None)
}

/// 429 while a realtime collection's segments in memory hold all the documents they may; the
/// flush begun here (if none is under way) makes room.
fn refuse_when_full(state: &Arc<AppState>, collection: &Arc<crate::collection::Collection>) -> Result<(), AppError> {
    if !collection.memory_full() {
        return Ok(());
    }
    state.metrics.backpressure_events_total.inc();
    flush_when_due(state, collection);
    Err(AppError::TooManyRequests("Writes are ahead of flushing. Retry shortly.".into()))
}

/// Merge and flush a realtime collection's segments in memory in the background as they become
/// due (`compaction::maintain`).
fn flush_when_due(state: &Arc<AppState>, collection: &Arc<crate::collection::Collection>) {
    crate::collection::compaction::maintain(collection, Some(state.config.data_dir.clone()), Arc::clone(&state.metrics));
}

/// Persist a collection's index to disk in a background task.
/// Skips persistence if `persistence_enabled` is false on the schema.
fn save_index_background(state: &Arc<AppState>, collection: &Arc<crate::collection::Collection>) {
    if !collection.schema.persistence_enabled {
        return;
    }
    let data_dir = state.config.data_dir.clone();
    let metrics = Arc::clone(&state.metrics);
    let collection = Arc::clone(collection);
    let save_guard = crate::observability::track_in_flight(&metrics.index_saves_in_flight);
    let save_fn = move || {
        let _save_guard = save_guard;
        let name = collection.name.clone();
        match collection.save(&data_dir) {
            // a collection a schema change replaced saves nothing, and its log went on
            Ok(None) => {}
            Ok(Some(elapsed)) => {
                metrics
                    .persistence_save_duration
                    .get_or_create(&crate::observability::CollectionLabels {
                        collection: name,
                    })
                    .observe(elapsed.as_secs_f64());
            }
            Err(e) => {
                tracing::error!(
                    collection = name.as_str(),
                    error = %e,
                    "Failed to persist index to disk"
                );
            }
        }
    };
    tokio::task::spawn_blocking(save_fn);
}

// -- Studio routes (no API key auth required) --

pub fn studio_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/studio/queries", get(list_saved_queries))
        .route("/api/studio/queries", post(save_query))
        .route("/api/studio/queries/{name}", delete(delete_saved_query))
}

#[derive(Serialize, Deserialize)]
struct SavedQuery {
    name: String,
    query: String,
}

async fn list_saved_queries(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<SavedQuery>> {
    let studio_dir = state.config.data_dir.join("studio");
    let path = studio_dir.join("queries.json");
    let queries: Vec<SavedQuery> = if path.exists() {
        let data = std::fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        vec![]
    };
    Json(queries)
}

async fn save_query(
    State(state): State<Arc<AppState>>,
    Json(query): Json<SavedQuery>,
) -> Result<Json<Value>, AppError> {
    let studio_dir = state.config.data_dir.join("studio");
    std::fs::create_dir_all(&studio_dir)
        .map_err(|e| AppError::Internal(format!("Failed to create studio dir: {e}")))?;
    let path = studio_dir.join("queries.json");

    let mut queries: Vec<SavedQuery> = if path.exists() {
        let data = std::fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        vec![]
    };

    // Replace if exists, otherwise append
    if let Some(existing) = queries.iter_mut().find(|q| q.name == query.name) {
        existing.query = query.query;
    } else {
        queries.push(query);
    }

    let json = serde_json::to_string_pretty(&queries)
        .map_err(|e| AppError::Internal(format!("JSON serialization: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| AppError::Internal(format!("Failed to write queries: {e}")))?;

    Ok(Json(json!({ "ok": true })))
}

async fn delete_saved_query(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    let studio_dir = state.config.data_dir.join("studio");
    let path = studio_dir.join("queries.json");

    if !path.exists() {
        return Err(AppError::NotFound(format!("Query `{name}` not found.")));
    }

    let data = std::fs::read_to_string(&path).unwrap_or_default();
    let mut queries: Vec<SavedQuery> = serde_json::from_str(&data).unwrap_or_default();
    let before = queries.len();
    queries.retain(|q| q.name != name);
    if queries.len() == before {
        return Err(AppError::NotFound(format!("Query `{name}` not found.")));
    }

    let json = serde_json::to_string_pretty(&queries)
        .map_err(|e| AppError::Internal(format!("JSON serialization: {e}")))?;
    std::fs::write(&path, json)
        .map_err(|e| AppError::Internal(format!("Failed to write queries: {e}")))?;

    Ok(Json(json!({ "ok": true })))
}
