pub mod builder;
pub mod protocol;
pub mod segment_file;
pub mod server;
pub mod session;

pub use builder::build_index;
pub use builder::build_index_with_backing;
#[cfg(feature = "cuda")]
pub use builder::build_index_with_gpu;
#[cfg(feature = "cuda")]
pub use builder::build_index_with_gpu_and_backing;

use crate::schema::{DocId, Document, FieldType, FieldValue, Language, Schema};
use rustc_hash::FxHashSet;

/// Recursively flatten a nested JSON object into a flat `Map` with dotted keys.
/// No depth limit — matches Typesense v30.1 behavior.
///
/// - Nested objects: `{"metadata": {"brand": "X"}}` → `{"metadata.brand": "X"}`
/// - Nested arrays of objects: `{"variants": [{"color": "red", "size": "M"}]}`
///   → `{"variants.color": ["red"], "variants.size": ["M"]}`
/// - Scalar values and already-flat fields pass through unchanged.
fn flatten_json_object(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    flatten_json_inner(obj)
}

fn flatten_json_inner(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut result = serde_json::Map::new();

    for (key, value) in obj {
        match value {
            serde_json::Value::Object(inner) => {
                let flat_inner = flatten_json_inner(inner)?;
                for (inner_key, inner_val) in flat_inner {
                    result.insert(format!("{key}.{inner_key}"), inner_val);
                }
            }
            serde_json::Value::Array(arr) if !arr.is_empty() && arr[0].is_object() => {
                // Array of objects: pivot to object of arrays.
                // First pass: collect all field names across all elements.
                let mut seen = FxHashSet::default();
                let mut all_field_names = Vec::new();
                for item in arr {
                    if let serde_json::Value::Object(item_obj) = item {
                        for field_name in item_obj.keys() {
                            if seen.insert(field_name.clone()) {
                                all_field_names.push(field_name.clone());
                            }
                        }
                    }
                }
                // Second pass: build arrays with null-padding for missing fields.
                for field_name in &all_field_names {
                    let arr_key = format!("{key}.{field_name}");
                    let mut values = Vec::with_capacity(arr.len());
                    for item in arr {
                        if let serde_json::Value::Object(item_obj) = item {
                            values.push(
                                item_obj
                                    .get(field_name)
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null),
                            );
                        } else {
                            values.push(serde_json::Value::Null);
                        }
                    }
                    result.insert(arr_key, serde_json::Value::Array(values));
                }
            }
            _ => {
                // Scalar or flat array: pass through
                result.insert(key.clone(), value.clone());
            }
        }
    }

    Ok(result)
}

/// Discover concrete sub-fields for dynamic schema declarations (Typesense
/// v30.1 `object`/`object[]`/`auto`). Scans the flattened keys of the given
/// documents; keys covered by an object prefix or an auto pattern get a
/// concrete FieldDef with the type inferred from the first non-null value.
/// Returns the fields to append (empty when the schema has no dynamic
/// declarations or nothing new was found).
pub fn discover_dynamic_fields(
    schema: &Schema,
    doc_jsons: &[&serde_json::Value],
) -> Vec<crate::schema::FieldDef> {
    let object_prefixes: Vec<&str> = schema
        .fields
        .iter()
        .filter(|f| matches!(f.field_type, FieldType::Object | FieldType::ObjectArray))
        .map(|f| f.name.as_str())
        .collect();
    let auto_patterns: Vec<&str> = schema
        .fields
        .iter()
        .filter(|f| f.field_type == FieldType::Auto)
        .map(|f| f.name.as_str())
        .collect();
    if object_prefixes.is_empty() && auto_patterns.is_empty() {
        return Vec::new();
    }

    let covered = |key: &str| -> bool {
        object_prefixes
            .iter()
            .any(|p| key.starts_with(p) && key[p.len()..].starts_with('.'))
            || auto_patterns.iter().any(|p| {
                *p == ".*"
                    || p.strip_suffix(".*")
                        .is_some_and(|prefix| key.starts_with(prefix))
                    || *p == key
            })
    };

    let mut known: FxHashSet<std::string::String> =
        schema.fields.iter().map(|f| f.name.clone()).collect();
    let mut discovered = Vec::new();

    for json in doc_jsons {
        let Some(obj) = json.as_object() else { continue };
        let flat_storage;
        let obj = if schema.enable_nested_fields {
            match flatten_json_object(obj) {
                Ok(f) => {
                    flat_storage = f;
                    &flat_storage
                }
                Err(_) => continue,
            }
        } else {
            obj
        };
        for (key, value) in obj {
            if key == "id" || key == "language" || known.contains(key) || !covered(key) {
                continue;
            }
            let Some(field_type) = infer_field_type(value) else {
                continue;
            };
            known.insert(key.clone());
            discovered.push(crate::schema::FieldDef {
                name: key.clone(),
                field_type,
                optional: true,
                ..Default::default()
            });
        }
    }
    discovered
}

/// Infer a concrete FieldType from a JSON value. Returns None for values
/// with no supported mapping (null, nested objects).
fn infer_field_type(value: &serde_json::Value) -> Option<FieldType> {
    use serde_json::Value;
    match value {
        Value::String(_) => Some(FieldType::String),
        Value::Bool(_) => Some(FieldType::Bool),
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(FieldType::Int64),
        Value::Number(_) => Some(FieldType::Float64),
        Value::Array(arr) => {
            let first = arr.iter().find(|e| !e.is_null())?;
            match first {
                Value::String(_) => Some(FieldType::StringArray),
                Value::Bool(_) => Some(FieldType::BoolArray),
                Value::Number(n) if n.is_i64() || n.is_u64() => Some(FieldType::Int64Array),
                Value::Number(_) => Some(FieldType::FloatArray { num_dim: 0 }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parse a JSON object into a Document, validating against the schema.
pub fn parse_json_document(
    json: &serde_json::Value,
    schema: &Schema,
    doc_id: DocId,
) -> Result<Document, String> {
    let (external_id, language, fields) = typed_fields(json, schema, true)?;
    let record = schema
        .registry
        .encode(json, &external_id)
        .map_err(|e| e.to_string())?;

    Ok(Document {
        id: doc_id,
        external_id,
        language,
        fields,
        record: Some(record.into()),
    })
}

/// A document parsed against one schema typed against another (a schema change replaced its
/// collection's), from its record and validated as a new write is.
pub fn retype(doc: &Document, from: &Schema, to: &Schema) -> Result<Document, String> {
    let record = doc
        .record
        .as_ref()
        .ok_or_else(|| format!("Document `{}` has no JSON to type against the collection's new schema.", doc.external_id))?;
    let json = crate::tqbf::record::to_value(record, &from.registry.load()).map_err(|e| e.to_string())?;
    let (external_id, language, fields) = typed_fields(&json, to, true)?;
    Ok(Document { id: doc.id, external_id, language, fields, record: Some(std::sync::Arc::clone(record)) })
}

/// External id, language and typed field values of a JSON document. `strict` validates against
/// the schema (required fields, types); otherwise (a stored document read back, validated when it
/// was written) fields that do not fit are left out.
pub fn typed_fields(
    json: &serde_json::Value,
    schema: &Schema,
    strict: bool,
) -> Result<(String, Language, crate::schema::SparseFields), String> {
    let raw_obj = json.as_object().ok_or("Expected JSON object")?;

    // Flatten nested objects/arrays when enable_nested_fields is true
    let flat;
    let obj = if schema.enable_nested_fields {
        flat = flatten_json_object(raw_obj)?;
        &flat
    } else {
        raw_obj
    };

    let external_id = obj
        .get("id")
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .ok_or("Missing or invalid 'id' field")?;

    let language = obj
        .get("language")
        .or_else(|| obj.get("locale"))
        .and_then(|v| v.as_str())
        .and_then(Language::from_locale)
        .unwrap_or(schema.default_language);

    let mut fields = crate::schema::SparseFields::with_capacity(schema.fields.len());
    // Auto-embedding reads its source fields from the typed values, indexed or not.
    let has_embed = schema.fields.iter().any(|f| f.embed.is_some());

    for field_def in &schema.fields {
        // Dynamic markers (object/object[]/auto) hold no data of their own —
        // flattening replaces e.g. "meta" with "meta.color" keys, so the
        // marker name is never present in the flattened doc.
        if field_def.field_type.is_dynamic_marker() {
            continue;
        }
        // Unindexed fields live only in the record. Typesense validates only its search schema
        // (indexed fields), so they are neither type-checked nor required.
        if !field_def.index
            && !field_def.facet
            && !field_def.sort
            && field_def.reference.is_none()
            && field_def.embed.is_none()
            && !has_embed
        {
            continue;
        }
        match obj.get(&field_def.name) {
            None | Some(serde_json::Value::Null) => {
                // Auto-embedding fields are implicitly optional at parse
                // time: their vectors are filled in after parsing.
                if strict && !field_def.optional && field_def.embed.is_none() {
                    return Err(format!("Field `{}` has been declared in the schema, but is not found in the document.", field_def.name));
                }
                // Null values not stored in SparseFields
            }
            Some(value) => match json_to_field_value(value, &field_def.field_type) {
                Ok(field_value) => fields.push(field_def.id, field_value),
                Err(e) if strict => return Err(format!("Field '{}': {}", field_def.name, e)),
                Err(_) => {}
            },
        }
    }

    Ok((external_id, language, fields))
}

/// Parse and serialize a chunk of raw source items into an encoded `DocBatch`
/// message in parallel, assigning document ids `base_id .. base_id + chunk.len()`
/// by position.
///
/// Ingest clients (`torque-admin import`, and external ones such as the
/// lei.online harvester) stream one batch at a time over the binary TCP
/// protocol. The per-document parse *and encode* on one core would be the
/// ceiling, not the network or the server, whose decode is already
/// rayon-parallel (`decode_doc_batch`). Fanning both across the rayon pool
/// removes it. Per-document byte outputs are concatenated in document
/// order, so the payload is byte-identical to the sequential `encode_doc_batch`
/// and document ids stay contiguous and deterministic. The first parse error in
/// item order is returned.
pub fn encode_batch_parallel<T, F>(
    chunk: &[T],
    schema: &Schema,
    base_id: DocId,
    parse: F,
) -> Result<protocol::ClientMessage, String>
where
    T: Sync,
    F: Fn(&T, DocId) -> Result<Document, String> + Sync + Send,
{
    use rayon::prelude::*;
    let parts: Vec<Vec<u8>> = chunk
        .par_iter()
        .enumerate()
        .map(|(i, item)| {
            let doc = parse(item, base_id + i as DocId)?;
            Ok::<Vec<u8>, String>(protocol::serialize_document(&doc, schema))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let total: usize = parts.iter().map(|p| p.len()).sum();
    let mut payload = Vec::with_capacity(total);
    for p in &parts {
        payload.extend_from_slice(p);
    }
    Ok(protocol::ClientMessage::DocBatch {
        doc_count: chunk.len() as u32,
        payload,
    })
}

fn json_to_field_value(value: &serde_json::Value, field_type: &FieldType) -> Result<FieldValue, String> {
    match field_type {
        FieldType::Text | FieldType::String => value
            .as_str()
            .map(|s| FieldValue::Text(s.to_string()))
            .ok_or_else(|| format!("expected string, got {}", json_type_name(value))),

        FieldType::Int32 => value
            .as_i64()
            .map(|n| FieldValue::Int32(n as i32))
            .ok_or_else(|| format!("expected int32, got {}", json_type_name(value))),

        FieldType::Int64 => value
            .as_i64()
            .map(FieldValue::Int64)
            .ok_or_else(|| format!("expected int64, got {}", json_type_name(value))),

        FieldType::Float64 => value
            .as_f64()
            .map(FieldValue::Float64)
            .ok_or_else(|| format!("expected float, got {}", json_type_name(value))),

        FieldType::Bool => value
            .as_bool()
            .map(FieldValue::Bool)
            .ok_or_else(|| format!("expected bool, got {}", json_type_name(value))),

        // string* accepts a scalar string too, coerced to a 1-element array.
        FieldType::StringStar if value.is_string() => Ok(FieldValue::StringArray(vec![Some(
            value.as_str().unwrap().to_string(),
        )])),

        // Marker types hold no data; their discovered sub-fields carry it.
        FieldType::Object | FieldType::ObjectArray | FieldType::Auto => Ok(FieldValue::Null),

        FieldType::StringArray | FieldType::StringStar => {
            let arr = value
                .as_array()
                .ok_or_else(|| format!("expected array, got {}", json_type_name(value)))?;
            let strings: Result<Vec<Option<String>>, String> = arr
                .iter()
                .map(|v| {
                    if v.is_null() { return Ok(None); }
                    v.as_str()
                        .map(|s| Some(s.to_string()))
                        .ok_or_else(|| "expected string or null in array".to_string())
                })
                .collect();
            Ok(FieldValue::StringArray(strings?))
        }

        FieldType::BoolArray => {
            let arr = value
                .as_array()
                .ok_or_else(|| format!("expected array, got {}", json_type_name(value)))?;
            let bools: Result<Vec<Option<bool>>, String> = arr
                .iter()
                .map(|v| {
                    if v.is_null() { return Ok(None); }
                    v.as_bool()
                        .map(Some)
                        .ok_or_else(|| "expected bool or null in array".to_string())
                })
                .collect();
            Ok(FieldValue::BoolArray(bools?))
        }

        FieldType::GeoPointArray => {
            let arr = value
                .as_array()
                .ok_or_else(|| format!("expected array, got {}", json_type_name(value)))?;
            let points: Result<Vec<(f64, f64)>, String> = arr
                .iter()
                .filter(|v| !v.is_null())
                .map(|v| {
                    let pair = v
                        .as_array()
                        .filter(|p| p.len() == 2)
                        .ok_or_else(|| "expected [lat, lng] pair in geopoint[]".to_string())?;
                    let lat = pair[0].as_f64().ok_or_else(|| "invalid lat".to_string())?;
                    let lng = pair[1].as_f64().ok_or_else(|| "invalid lng".to_string())?;
                    Ok((lat, lng))
                })
                .collect();
            Ok(FieldValue::GeoPointArray(points?))
        }

        FieldType::Int32Array => {
            let arr = value
                .as_array()
                .ok_or_else(|| format!("expected array, got {}", json_type_name(value)))?;
            let nums: Result<Vec<Option<i32>>, String> = arr
                .iter()
                .map(|v| {
                    if v.is_null() { return Ok(None); }
                    v.as_i64()
                        .map(|n| Some(n as i32))
                        .ok_or_else(|| "expected int or null in array".to_string())
                })
                .collect();
            Ok(FieldValue::Int32Array(nums?))
        }

        FieldType::Int64Array => {
            let arr = value
                .as_array()
                .ok_or_else(|| format!("expected array, got {}", json_type_name(value)))?;
            let nums: Result<Vec<Option<i64>>, String> = arr
                .iter()
                .map(|v| {
                    if v.is_null() { return Ok(None); }
                    v.as_i64()
                        .map(Some)
                        .ok_or_else(|| "expected int or null in array".to_string())
                })
                .collect();
            Ok(FieldValue::Int64Array(nums?))
        }

        FieldType::GeoPoint => {
            // Accept [lat, lng] array or {"lat": x, "lng": y} object
            if let Some(arr) = value.as_array() {
                if arr.len() == 2 {
                    let lat = arr[0]
                        .as_f64()
                        .ok_or("expected float for lat")?;
                    let lng = arr[1]
                        .as_f64()
                        .ok_or("expected float for lng")?;
                    return Ok(FieldValue::GeoPoint { lat, lng });
                }
            }
            if let Some(obj) = value.as_object() {
                let lat = obj
                    .get("lat")
                    .and_then(|v| v.as_f64())
                    .ok_or("expected 'lat' float in geopoint")?;
                let lng = obj
                    .get("lng")
                    .and_then(|v| v.as_f64())
                    .ok_or("expected 'lng' float in geopoint")?;
                return Ok(FieldValue::GeoPoint { lat, lng });
            }
            Err(format!("expected [lat, lng] or {{lat, lng}}, got {}", json_type_name(value)))
        }

        FieldType::FloatArray { num_dim } => {
            let arr = value
                .as_array()
                .ok_or_else(|| format!("expected array, got {}", json_type_name(value)))?;
            let nums: Result<Vec<f32>, String> = arr
                .iter()
                .filter(|v| *num_dim > 0 || !v.is_null())
                .map(|v| {
                    v.as_f64()
                        .map(|n| n as f32)
                        .ok_or_else(|| "expected float in array".to_string())
                })
                .collect();
            Ok(FieldValue::FloatArray(nums?))
        }
    }
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::FieldDef;

    fn test_schema() -> Schema {
        let mut schema = Schema {
            name: "test".into(),
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
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "price".into(),
                    field_type: FieldType::Float64,
                    facet: false,
                    index: false,
                    optional: true,
                    sort: true,
                    stem: false,
                    infix: false,
                    locale: None,
                    num_dim: None,
                    vec_dist: None,
                    id: 0,
                    reference: None, drop: false,
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
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();
        schema
    }

    #[test]
    fn parse_valid_document() {
        let schema = test_schema();
        let json: serde_json::Value =
            serde_json::from_str(r#"{"id": "1", "title": "Hello", "price": 9.99}"#).unwrap();
        let doc = parse_json_document(&json, &schema, 0).unwrap();
        assert_eq!(doc.external_id, "1");
        assert!(matches!(&doc.fields.get(0).unwrap(), FieldValue::Text(s) if s == "Hello"));
        assert!(matches!(doc.fields.get(1).unwrap(), FieldValue::Float64(f) if (f - 9.99).abs() < 0.001));
    }

    #[test]
    fn parse_optional_null() {
        let schema = test_schema();
        let json: serde_json::Value =
            serde_json::from_str(r#"{"id": "1", "title": "Hello"}"#).unwrap();
        let doc = parse_json_document(&json, &schema, 0).unwrap();
        assert!(doc.fields.get(1).is_none(), "optional null field should not be stored");
    }

    #[test]
    fn parse_required_missing_fails() {
        let schema = test_schema();
        let json: serde_json::Value =
            serde_json::from_str(r#"{"id": "1", "price": 5.0}"#).unwrap();
        let err = parse_json_document(&json, &schema, 0).unwrap_err();
        assert!(err.contains("title"));
    }

    #[test]
    fn parse_numeric_id() {
        let schema = test_schema();
        let json: serde_json::Value =
            serde_json::from_str(r#"{"id": 42, "title": "Hello"}"#).unwrap();
        let doc = parse_json_document(&json, &schema, 0).unwrap();
        assert_eq!(doc.external_id, "42");
    }

    #[test]
    fn parse_language_field() {
        let schema = test_schema();
        let json: serde_json::Value =
            serde_json::from_str(r#"{"id": "1", "title": "Hej", "language": "sv"}"#).unwrap();
        let doc = parse_json_document(&json, &schema, 0).unwrap();
        assert_eq!(doc.language, Language::Swedish);
    }

    fn nested_schema() -> Schema {
        let mut schema = Schema {
            name: "test".into(),
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
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "metadata.brand".into(),
                    field_type: FieldType::String,
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
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
                FieldDef {
                    name: "variants.color".into(),
                    field_type: FieldType::StringArray,
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
                    reference: None, drop: false,
                    stem_dictionary: None,
                    embed: None,
                    name_matching: false,
                },
            ],
            default_sorting_field: None,
            enable_nested_fields: true,
            symbols_to_index: vec![],
            token_separators: vec![],
            default_language: Language::default(),
            persistence_enabled: true,
            index_mode: Default::default(),
            name_to_id: Default::default(),
            geopoint_field_names: vec![],
            nested_layout: None,
            registry: Default::default(),
            metadata: None,
            synonym_sets: None,
            curation_sets: None,
        };
        schema.assign_field_ids();
        schema
    }

    #[test]
    fn flatten_nested_object() {
        let schema = nested_schema();
        let json: serde_json::Value = serde_json::from_str(
            r#"{"id": "1", "title": "Widget", "metadata": {"brand": "Acme"}, "variants": [{"color": "red"}, {"color": "blue"}]}"#,
        ).unwrap();
        let doc = parse_json_document(&json, &schema, 0).unwrap();
        assert_eq!(doc.external_id, "1");
        assert!(matches!(&doc.fields.get(0).unwrap(), FieldValue::Text(s) if s == "Widget"));
        assert!(matches!(&doc.fields.get(1).unwrap(), FieldValue::Text(s) if s == "Acme"));
        match &doc.fields.get(2).unwrap() {
            FieldValue::StringArray(arr) => {
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0].as_deref(), Some("red"));
                assert_eq!(arr[1].as_deref(), Some("blue"));
            }
            other => panic!("Expected StringArray, got {other:?}"),
        }
    }

    #[test]
    fn flatten_already_flat_fields() {
        let schema = nested_schema();
        // Pass pre-flattened fields directly — should work fine
        let json: serde_json::Value = serde_json::from_str(
            r#"{"id": "2", "title": "Gadget", "metadata.brand": "Beta", "variants.color": ["green"]}"#,
        ).unwrap();
        let doc = parse_json_document(&json, &schema, 0).unwrap();
        assert!(matches!(&doc.fields.get(1).unwrap(), FieldValue::Text(s) if s == "Beta"));
        match &doc.fields.get(2).unwrap() {
            FieldValue::StringArray(arr) => {
                assert_eq!(arr.len(), 1);
                assert_eq!(arr[0].as_deref(), Some("green"));
            }
            other => panic!("Expected StringArray, got {other:?}"),
        }
    }

    #[test]
    fn flatten_handles_deep_nesting() {
        // No depth limit — deeply nested objects should flatten correctly
        let schema = Schema {
            name: "deep".into(),
            fields: vec![
                FieldDef { name: "a.b.c.d.e".into(), field_type: FieldType::String, index: true, ..Default::default() },
            ],
            enable_nested_fields: true,
            ..Default::default()
        };
        let json: serde_json::Value = serde_json::from_str(
            r#"{"id": "3", "a": {"b": {"c": {"d": {"e": "deep_value"}}}}}"#,
        ).unwrap();
        let doc = parse_json_document(&json, &schema, 0).unwrap();
        assert_eq!(doc.external_id, "3");
    }
}
