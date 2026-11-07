use crate::schema::{Document, EmbedModelConfig, FieldValue, Schema};

/// Default per-request batch size for remote embedding calls
/// (`remote_embedding_batch_size`, Typesense v30.1 default 200).
pub const DEFAULT_BATCH_SIZE: usize = 200;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_NUM_TRIES: usize = 2;

/// Resolve the embeddings endpoint for a model config. `url` wins; models
/// named `openai/<name>` default to the OpenAI endpoint.
fn resolve_endpoint(model: &EmbedModelConfig) -> Result<(String, String), String> {
    let model_name = model
        .model_name
        .split_once('/')
        .map(|(_, m)| m)
        .unwrap_or(&model.model_name)
        .to_string();
    let url = match (&model.url, model.model_name.starts_with("openai/")) {
        (Some(u), _) => u.clone(),
        (None, true) => "https://api.openai.com/v1/embeddings".into(),
        (None, false) => {
            return Err(format!(
                "Embedding model `{}` needs a `url` (only openai/* models have a default endpoint).",
                model.model_name
            ))
        }
    };
    Ok((url, model_name))
}

/// Embed a batch of texts against an OpenAI-compatible embeddings endpoint.
/// Blocking (ureq) — call from a blocking context.
pub fn embed_texts(
    model: &EmbedModelConfig,
    texts: &[String],
    timeout_ms: Option<u64>,
    num_tries: Option<usize>,
) -> Result<Vec<Vec<f32>>, String> {
    let (url, model_name) = resolve_endpoint(model)?;
    let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let tries = num_tries.unwrap_or(DEFAULT_NUM_TRIES).max(1);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into();

    let body = serde_json::json!({ "model": model_name, "input": texts });
    let mut last_err = String::new();
    for _ in 0..tries {
        let mut req = agent.post(&url);
        if let Some(ref key) = model.api_key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }
        match req.send_json(&body) {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let text = resp
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| format!("Failed to read embedding response: {e}"))?;
                if status != 200 {
                    last_err = format!("Embedding endpoint returned HTTP {status}: {text}");
                    continue;
                }
                let parsed: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| format!("Invalid embedding response: {e}"))?;
                let data = parsed
                    .get("data")
                    .and_then(|d| d.as_array())
                    .ok_or("Embedding response missing `data` array")?;
                if data.len() != texts.len() {
                    return Err(format!(
                        "Embedding response has {} vectors for {} inputs",
                        data.len(),
                        texts.len()
                    ));
                }
                return data
                    .iter()
                    .map(|item| {
                        item.get("embedding")
                            .and_then(|e| e.as_array())
                            .ok_or_else(|| "Embedding item missing `embedding`".to_string())
                            .map(|arr| {
                                arr.iter()
                                    .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                                    .collect()
                            })
                    })
                    .collect();
            }
            Err(e) => last_err = format!("Embedding request failed: {e}"),
        }
    }
    Err(last_err)
}

/// Fill missing auto-embedding field values on parsed documents by calling
/// the remote model over the concatenated `from` source fields. Documents
/// that already carry a vector for the field are left untouched. Returns the
/// number of embedded documents.
/// Put a generated vector into the stored record too: Typesense returns it in the document.
fn set_record_vector(schema: &Schema, doc: &mut Document, field: &str, vector: &[f32]) {
    let Some(rec) = doc.record.as_ref() else { return };
    let Ok(mut value) = crate::tqbf::record::to_value(rec, &schema.registry.load()) else { return };
    let arr = serde_json::Value::Array(
        vector
            .iter()
            .map(|&x| serde_json::Number::from_f64(f64::from(x)).map_or(serde_json::Value::Null, serde_json::Value::Number))
            .collect(),
    );
    let mut target = &mut value;
    let parts: Vec<&str> = if schema.enable_nested_fields { field.split('.').collect() } else { vec![field] };
    for (i, part) in parts.iter().enumerate() {
        let Some(obj) = target.as_object_mut() else { return };
        if i + 1 == parts.len() {
            obj.insert((*part).to_string(), arr);
            break;
        }
        target = obj.entry((*part).to_string()).or_insert_with(|| serde_json::Value::Object(Default::default()));
    }
    match schema.registry.encode(&value, &doc.external_id) {
        Ok(r) => doc.record = Some(r.into()),
        Err(e) => tracing::error!(id = %doc.external_id, "cannot store generated vector in record: {e}"),
    }
}

pub fn embed_documents(
    schema: &Schema,
    docs: &mut [Document],
    batch_size: Option<usize>,
    timeout_ms: Option<u64>,
    num_tries: Option<usize>,
) -> Result<usize, String> {
    let batch = batch_size.unwrap_or(DEFAULT_BATCH_SIZE).max(1);
    let mut embedded = 0usize;

    for field_def in schema.fields.iter().filter(|f| f.embed.is_some()) {
        let embed_cfg = field_def.embed.as_ref().unwrap();
        let source_ids: Vec<usize> = embed_cfg
            .from
            .iter()
            .filter_map(|name| schema.name_to_id.get(name.as_str()).map(|&id| id as usize))
            .collect();
        if source_ids.is_empty() {
            continue;
        }

        // Collect docs missing this vector along with their source text.
        let mut pending: Vec<(usize, String)> = Vec::new();
        for (idx, doc) in docs.iter().enumerate() {
            if doc.fields.get(field_def.id as usize).is_some() {
                continue;
            }
            let mut text = String::new();
            for &sid in &source_ids {
                match doc.fields.get(sid) {
                    Some(FieldValue::Text(s)) => {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(s);
                    }
                    Some(FieldValue::StringArray(arr)) => {
                        for s in arr.iter().flatten() {
                            if !text.is_empty() {
                                text.push(' ');
                            }
                            text.push_str(s);
                        }
                    }
                    _ => {}
                }
            }
            if !text.is_empty() {
                pending.push((idx, text));
            }
        }

        // the model's indexing prefix before each document's text (Typesense)
        let prefix = embed_cfg.model_config.indexing_prefix.as_deref().unwrap_or("");
        for chunk in pending.chunks(batch) {
            let texts: Vec<String> = chunk.iter().map(|(_, t)| format!("{prefix}{t}")).collect();
            let vectors = embed_texts(&embed_cfg.model_config, &texts, timeout_ms, num_tries)?;
            for ((doc_idx, _), vector) in chunk.iter().zip(vectors) {
                if let Some(expected) = field_def.num_dim {
                    if vector.len() != expected as usize {
                        return Err(format!(
                            "Model returned {}-dim vector for field `{}` (num_dim = {expected}).",
                            vector.len(),
                            field_def.name
                        ));
                    }
                }
                set_record_vector(schema, &mut docs[*doc_idx], &field_def.name, &vector);
                docs[*doc_idx]
                    .fields
                    .insert_sorted(field_def.id, FieldValue::FloatArray(vector));
                embedded += 1;
            }
        }
    }
    Ok(embedded)
}
