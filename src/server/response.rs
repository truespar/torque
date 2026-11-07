use crate::collection::alias::AliasInfo;
use crate::schema::{FieldDef, IndexMode};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::collection::Collection;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
}

/// A collection's fields as Typesense's answers show them: `locale` and `stem_dictionary` empty
/// when unset, and `store` (Torque keeps every field).
fn api_fields(fields: &[FieldDef]) -> Vec<Value> {
    fields
        .iter()
        .map(|f| {
            let mut v = serde_json::to_value(f).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                for (k, default) in [("locale", json!("")), ("stem_dictionary", json!("")), ("store", json!(true))] {
                    o.entry(k).or_insert(default);
                }
            }
            v
        })
        .collect()
}

/// A collection as Typesense's collection answers show it (`default_sorting_field`,
/// `token_separators` and `symbols_to_index` always there), with Torque's `index_mode` and
/// `realtime` for a realtime collection.
#[derive(Serialize)]
pub struct CollectionResponse {
    pub name: String,
    pub num_documents: u32,
    pub fields: Vec<Value>,
    pub default_sorting_field: String,
    pub enable_nested_fields: bool,
    pub token_separators: Vec<char>,
    pub symbols_to_index: Vec<char>,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonym_sets: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime: Option<crate::collection::RealtimeStats>,
}

impl CollectionResponse {
    pub fn from_collection(col: &Arc<Collection>) -> Self {
        let is_realtime = col.schema.index_mode == IndexMode::Realtime;
        let realtime = is_realtime.then(|| col.realtime_stats());
        let num_documents = col.snapshot_bundle().total_docs();
        Self {
            name: col.name.clone(),
            num_documents,
            fields: api_fields(&col.schema.fields),
            default_sorting_field: col.schema.default_sorting_field.clone().unwrap_or_default(),
            enable_nested_fields: col.schema.enable_nested_fields,
            token_separators: col.schema.token_separators.clone(),
            symbols_to_index: col.schema.symbols_to_index.clone(),
            created_at: col.created_at,
            index_mode: if is_realtime { Some("realtime".into()) } else { None },
            metadata: col.schema.metadata.clone(),
            synonym_sets: col.schema.synonym_sets.clone(),
            realtime,
        }
    }

    /// A collection not loaded: what collections.json holds of it, its documents not counted.
    pub fn from_persisted(entry: &crate::persistence::PersistedCollection) -> Self {
        let schema = &entry.schema;
        Self {
            name: schema.name.clone(),
            num_documents: 0,
            fields: api_fields(&schema.fields),
            default_sorting_field: schema.default_sorting_field.clone().unwrap_or_default(),
            enable_nested_fields: schema.enable_nested_fields,
            token_separators: schema.token_separators.clone(),
            symbols_to_index: schema.symbols_to_index.clone(),
            created_at: entry.created_at,
            index_mode: (schema.index_mode == IndexMode::Realtime).then(|| "realtime".into()),
            metadata: schema.metadata.clone(),
            synonym_sets: schema.synonym_sets.clone(),
            realtime: None,
        }
    }
}

#[derive(Serialize)]
pub struct CollectionListItem {
    pub name: String,
    pub num_documents: u32,
    pub fields: Vec<Value>,
    pub default_sorting_field: String,
    pub enable_nested_fields: bool,
    pub token_separators: Vec<char>,
    pub symbols_to_index: Vec<char>,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synonym_sets: Option<Vec<String>>,
}

impl CollectionListItem {
    pub fn from_collection(col: &Arc<Collection>) -> Self {
        let is_realtime = col.schema.index_mode == IndexMode::Realtime;
        let num_documents = col.snapshot_bundle().total_docs();
        Self {
            name: col.name.clone(),
            num_documents,
            fields: api_fields(&col.schema.fields),
            default_sorting_field: col.schema.default_sorting_field.clone().unwrap_or_default(),
            enable_nested_fields: col.schema.enable_nested_fields,
            token_separators: col.schema.token_separators.clone(),
            symbols_to_index: col.schema.symbols_to_index.clone(),
            created_at: col.created_at,
            index_mode: if is_realtime { Some("realtime".into()) } else { None },
            metadata: col.schema.metadata.clone(),
            synonym_sets: col.schema.synonym_sets.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct AliasResponse {
    pub name: String,
    pub collection_name: String,
}

impl From<AliasInfo> for AliasResponse {
    fn from(info: AliasInfo) -> Self {
        Self {
            name: info.name,
            collection_name: info.collection_name,
        }
    }
}
