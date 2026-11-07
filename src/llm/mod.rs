//! Remote LLM integration for conversational search (`/conversations/models`)
//! and natural-language search (`/nl_search_models`). v1 is remote-only,
//! mirroring the auto-embedding decision: any OpenAI-compatible
//! chat-completions endpoint works (`api_url` / `vllm_url` override, and
//! `openai/*` model names default to the OpenAI endpoint).

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, RwLock};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Default conversation TTL (Typesense default: 24h).
pub const DEFAULT_CONVERSATION_TTL_SECS: i64 = 86_400;
/// Cap on retained messages per conversation (context is also byte-capped
/// by the model's `max_bytes` at prompt-build time).
const MAX_HISTORY_MESSAGES: usize = 40;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// The answer request, one user message: `DATA_STR{documents}QUESTION_STR{question}ANSWER_STR`.
pub const DATA_STR: &str = "Answer the question from the search results below. If they do not hold the answer, say so instead of guessing.\n\nSearch results:\n";
pub const QUESTION_STR: &str = "\n\nQuestion:\n";
pub const ANSWER_STR: &str = "\n\nAnswer:";

/// The request that makes a follow-up searchable on its own: retrieval sees only this one question,
/// so whatever the earlier turns settled (names, places, dates) has to be written out in it.
pub const STANDALONE_QUESTION_PROMPT: &str = "Reply with the last question below written so that it makes sense without the conversation before it. Spell out everything it refers back to. Reply with the question only, not an answer to it.";
const HISTORY_STR: &str = "\n\nConversation so far:\n";
const FOLLOW_UP_STR: &str = "\n\nLast question:\n";
const REWRITTEN_STR: &str = "\n\nThe question on its own:\n";

/// OpenAI reasoning models (o-series, gpt-5) reject `temperature` and use
/// `max_completion_tokens` instead of `max_tokens`, told apart by name.
pub fn is_reasoning_model(bare_model_name: &str) -> bool {
    let bytes = bare_model_name.as_bytes();
    let is_o_model = bytes.len() >= 2 && bytes[0] == b'o' && bytes[1].is_ascii_digit();
    is_o_model || bare_model_name.contains("gpt-5")
}

/// Extract a JSON object from LLM output: direct parse first, then the
/// greedy first-`{`-to-last-`}` slice, so prose- or fence-wrapped JSON still
/// parses.
pub fn extract_json_object(content: &str) -> Result<serde_json::Value, String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(content.trim()) {
        if v.is_object() {
            return Ok(v);
        }
    }
    let (Some(start), Some(end)) = (content.find('{'), content.rfind('}')) else {
        return Err(format!("no JSON object in LLM output: {content}"));
    };
    if end < start {
        return Err(format!("no JSON object in LLM output: {content}"));
    }
    serde_json::from_str(&content[start..=end])
        .map_err(|e| format!("could not parse JSON from LLM output: {e}: {content}"))
}

/// Build the standalone-question rewrite request: the conversation history
/// as one-line role-keyed JSON objects, oldest messages dropped until the
/// prompt fits `max_bytes`.
pub fn build_standalone_prompt(
    history: &[ChatMessage],
    question: &str,
    max_bytes: usize,
) -> String {
    let fixed = STANDALONE_QUESTION_PROMPT.len() + HISTORY_STR.len() + FOLLOW_UP_STR.len() + question.len() + REWRITTEN_STR.len();
    let budget = max_bytes.saturating_sub(fixed);

    let lines: Vec<String> = history
        .iter()
        .map(|m| serde_json::json!({ m.role.clone(): m.content }).to_string())
        .collect();
    let mut start = 0;
    let mut total: usize = lines.iter().map(|l| l.len() + 1).sum();
    while start < lines.len() && total > budget {
        total -= lines[start].len() + 1;
        start += 1;
    }

    let mut prompt = String::with_capacity(fixed + total);
    prompt.push_str(STANDALONE_QUESTION_PROMPT);
    prompt.push_str(HISTORY_STR);
    for line in &lines[start..] {
        prompt.push_str(line);
        prompt.push('\n');
    }
    prompt.push_str(FOLLOW_UP_STR);
    prompt.push_str(question);
    prompt.push_str(REWRITTEN_STR);
    prompt
}

/// A conversation model, as `/conversations/models` takes it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationModel {
    #[serde(default)]
    pub id: String,
    pub model_name: String,
    pub max_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_collection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vllm_url: Option<String>,
}

/// An NL search model, as `/nl_search_models` takes it. Only the
/// OpenAI-compatible subset of the spec's provider fields is used in v1;
/// the GCP/Cloudflare-specific fields are accepted and stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NlSearchModel {
    #[serde(default)]
    pub id: String,
    pub model_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A conversation model's `vllm_url` is a BASE URL in Typesense — the
/// chat-completions path is always appended.
/// Full paths are accepted too for convenience.
pub fn vllm_chat_url(vllm_url: &Option<String>) -> Option<String> {
    vllm_url.as_ref().map(|u| {
        if u.ends_with("/chat/completions") {
            u.clone()
        } else if u.ends_with('/') {
            format!("{u}v1/chat/completions")
        } else {
            format!("{u}/v1/chat/completions")
        }
    })
}

/// Resolve an OpenAI-compatible chat-completions endpoint: explicit URL
/// wins; `openai/<name>` models default to api.openai.com.
pub fn resolve_chat_endpoint(
    model_name: &str,
    explicit_url: Option<&str>,
) -> Result<(String, String), String> {
    let bare_name = model_name
        .split_once('/')
        .map(|(_, m)| m)
        .unwrap_or(model_name)
        .to_string();
    let url = match (explicit_url, model_name.starts_with("openai/")) {
        (Some(u), _) => u.to_string(),
        (None, true) => "https://api.openai.com/v1/chat/completions".into(),
        (None, false) => {
            return Err(format!(
                "Model `{model_name}` needs an endpoint URL (only openai/* models have a default)."
            ))
        }
    };
    Ok((url, bare_name))
}

/// Incremental parser for OpenAI-style streaming chat chunks (SSE). Feed
/// raw bytes as they arrive; returns the extracted
/// `choices[0].delta.content` texts plus whether the `[DONE]` sentinel was
/// seen. `pending` carries any partial event across calls — the same
/// remainder-buffer approach.
pub fn drain_sse_deltas(pending: &mut String, chunk: &str) -> (Vec<String>, bool) {
    pending.push_str(chunk);
    let mut deltas = Vec::new();
    let mut done = false;
    while let Some(pos) = pending.find("\n\n") {
        let event: String = pending[..pos].to_string();
        pending.drain(..pos + 2);
        for line in event.lines() {
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                done = true;
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
                if let Some(content) = v["choices"][0]["delta"]["content"].as_str() {
                    if !content.is_empty() {
                        deltas.push(content.to_string());
                    }
                }
            }
        }
    }
    (deltas, done)
}

/// Call an OpenAI-compatible chat-completions endpoint. Blocking (ureq) —
/// call from a blocking context.
pub fn chat_complete(
    model_name: &str,
    explicit_url: Option<&str>,
    api_key: Option<&str>,
    temperature: Option<f64>,
    max_tokens: Option<usize>,
    messages: &[ChatMessage],
) -> Result<String, String> {
    let (url, bare_name) = resolve_chat_endpoint(model_name, explicit_url)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(DEFAULT_TIMEOUT_MS)))
        .build()
        .into();
    let mut body = serde_json::json!({ "model": bare_name, "messages": messages });
    // Reasoning models (o-series/gpt-5) reject `temperature` and rename the
    // token cap — same handling as Typesense.
    let reasoning = is_reasoning_model(&bare_name);
    if let Some(cap) = max_tokens {
        body[if reasoning { "max_completion_tokens" } else { "max_tokens" }] =
            serde_json::json!(cap);
    }
    if let Some(t) = temperature {
        if !reasoning {
            body["temperature"] = serde_json::json!(t);
        }
    }
    let mut req = agent.post(&url);
    if let Some(key) = api_key {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }
    let mut resp = req
        .send_json(&body)
        .map_err(|e| format!("LLM request failed: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Failed to read LLM response: {e}"))?;
    if status != 200 {
        return Err(format!("LLM endpoint returned HTTP {status}: {text}"));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid LLM response: {e}"))?;
    parsed["choices"][0]["message"]["content"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| "LLM response missing choices[0].message.content".to_string())
}

struct Conversation {
    messages: Vec<ChatMessage>,
    last_activity: i64,
    ttl: i64,
}

/// Registry for both LLM model kinds plus in-memory conversation history.
pub struct LlmRegistry {
    conversation_models: RwLock<FxHashMap<String, ConversationModel>>,
    nl_models: RwLock<FxHashMap<String, NlSearchModel>>,
    history: Mutex<FxHashMap<String, Conversation>>,
}

impl LlmRegistry {
    pub fn new() -> Self {
        Self {
            conversation_models: RwLock::new(FxHashMap::default()),
            nl_models: RwLock::new(FxHashMap::default()),
            history: Mutex::new(FxHashMap::default()),
        }
    }

    // ── Conversation models ────────────────────────────────────────────

    pub fn upsert_conversation_model(&self, model: ConversationModel) {
        self.conversation_models
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(model.id.clone(), model);
    }

    pub fn get_conversation_model(&self, id: &str) -> Option<ConversationModel> {
        self.conversation_models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn list_conversation_models(&self) -> Vec<ConversationModel> {
        let mut models: Vec<ConversationModel> = self
            .conversation_models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }

    pub fn delete_conversation_model(&self, id: &str) -> bool {
        self.conversation_models
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .is_some()
    }

    pub fn load_conversation_models(&self, models: Vec<ConversationModel>) {
        let mut store = self
            .conversation_models
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for model in models {
            store.insert(model.id.clone(), model);
        }
    }

    // ── NL search models ───────────────────────────────────────────────

    pub fn upsert_nl_model(&self, model: NlSearchModel) {
        self.nl_models
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(model.id.clone(), model);
    }

    pub fn get_nl_model(&self, id: &str) -> Option<NlSearchModel> {
        self.nl_models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn list_nl_models(&self) -> Vec<NlSearchModel> {
        let mut models: Vec<NlSearchModel> = self
            .nl_models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models
    }

    pub fn delete_nl_model(&self, id: &str) -> bool {
        self.nl_models
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .is_some()
    }

    pub fn load_nl_models(&self, models: Vec<NlSearchModel>) {
        let mut store = self.nl_models.write().unwrap_or_else(|e| e.into_inner());
        for model in models {
            store.insert(model.id.clone(), model);
        }
    }

    // ── Conversation history ───────────────────────────────────────────

    /// Prior messages for a conversation, pruning expired entries on the way.
    pub fn conversation_history(&self, conversation_id: &str, now_ts: i64) -> Vec<ChatMessage> {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        history.retain(|_, c| now_ts - c.last_activity <= c.ttl);
        history
            .get(conversation_id)
            .map(|c| c.messages.clone())
            .unwrap_or_default()
    }

    /// Replace a conversation's cached messages wholesale — used to re-seed
    /// the cache from the durable history collection after a restart.
    pub fn seed_conversation(
        &self,
        conversation_id: &str,
        messages: Vec<ChatMessage>,
        ttl: i64,
        now_ts: i64,
    ) {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        history.insert(
            conversation_id.to_string(),
            Conversation {
                messages,
                last_activity: now_ts,
                ttl,
            },
        );
    }

    /// Append a user/assistant exchange to a conversation, creating it on
    /// first use.
    pub fn append_exchange(
        &self,
        conversation_id: &str,
        question: &str,
        answer: &str,
        ttl: i64,
        now_ts: i64,
    ) {
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let conv = history
            .entry(conversation_id.to_string())
            .or_insert_with(|| Conversation {
                messages: Vec::new(),
                last_activity: now_ts,
                ttl,
            });
        conv.messages.push(ChatMessage {
            role: "user".into(),
            content: question.to_string(),
        });
        conv.messages.push(ChatMessage {
            role: "assistant".into(),
            content: answer.to_string(),
        });
        if conv.messages.len() > MAX_HISTORY_MESSAGES {
            let excess = conv.messages.len() - MAX_HISTORY_MESSAGES;
            conv.messages.drain(..excess);
        }
        conv.last_activity = now_ts;
        conv.ttl = ttl;
    }
}

impl Default for LlmRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Random hex id for models/conversations created without an explicit id.
pub fn generate_id() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("failed to generate random id");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_resolution() {
        let (url, name) = resolve_chat_endpoint("openai/gpt-4o", None).unwrap();
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(name, "gpt-4o");

        let (url, name) =
            resolve_chat_endpoint("vllm/llama-3", Some("http://localhost:8000/v1/chat/completions"))
                .unwrap();
        assert_eq!(url, "http://localhost:8000/v1/chat/completions");
        assert_eq!(name, "llama-3");

        assert!(resolve_chat_endpoint("custom-model", None).is_err());
    }

    #[test]
    fn history_ttl_and_cap() {
        let reg = LlmRegistry::new();
        reg.append_exchange("c1", "q1", "a1", 100, 1000);
        assert_eq!(reg.conversation_history("c1", 1050).len(), 2);
        // Expired after ttl seconds of inactivity.
        assert!(reg.conversation_history("c1", 1201).is_empty());

        // Cap: only the newest MAX_HISTORY_MESSAGES survive.
        for i in 0..40 {
            reg.append_exchange("c2", &format!("q{i}"), &format!("a{i}"), 10_000, 2000);
        }
        let history = reg.conversation_history("c2", 2001);
        assert_eq!(history.len(), MAX_HISTORY_MESSAGES);
        assert_eq!(history.last().unwrap().content, "a39");
    }

    #[test]
    fn reasoning_model_detection() {
        assert!(is_reasoning_model("o1-mini"));
        assert!(is_reasoning_model("o3"));
        assert!(is_reasoning_model("gpt-5-turbo"));
        assert!(!is_reasoning_model("gpt-4o"));
        assert!(!is_reasoning_model("omni-model"));
        assert!(!is_reasoning_model("llama-3"));
    }

    #[test]
    fn json_extraction_direct_and_wrapped() {
        let direct = extract_json_object(r#"{"q": "laptop"}"#).unwrap();
        assert_eq!(direct["q"], "laptop");

        let fenced = extract_json_object("```json\n{\"q\": \"laptop\"}\n```").unwrap();
        assert_eq!(fenced["q"], "laptop");

        let prose =
            extract_json_object("Here are the params: {\"filter_by\": \"price:<10\"} hope that helps!")
                .unwrap();
        assert_eq!(prose["filter_by"], "price:<10");

        assert!(extract_json_object("no json here").is_err());
    }

    #[test]
    fn standalone_prompt_shape_and_truncation() {
        let history = vec![
            ChatMessage { role: "user".into(), content: "first question".into() },
            ChatMessage { role: "assistant".into(), content: "first answer".into() },
        ];
        let prompt = build_standalone_prompt(&history, "and then?", 16384);
        assert!(prompt.contains(HISTORY_STR));
        assert!(prompt.contains(r#"{"user":"first question"}"#));
        assert!(prompt.contains("Last question:\nand then?"));
        assert!(prompt.ends_with(REWRITTEN_STR));

        // Tight budget: oldest messages are dropped first.
        let long_history: Vec<ChatMessage> = (0..50)
            .map(|i| ChatMessage {
                role: "user".into(),
                content: format!("message number {i} with some padding text"),
            })
            .collect();
        let small = build_standalone_prompt(&long_history, "q", 600);
        assert!(!small.contains("message number 0"), "oldest dropped");
        assert!(small.contains("message number 49"), "newest kept");
    }

    #[test]
    fn vllm_url_base_appending() {
        assert_eq!(
            vllm_chat_url(&Some("http://host:8000".into())).unwrap(),
            "http://host:8000/v1/chat/completions"
        );
        assert_eq!(
            vllm_chat_url(&Some("http://host:8000/".into())).unwrap(),
            "http://host:8000/v1/chat/completions"
        );
        assert_eq!(
            vllm_chat_url(&Some("http://host:8000/v1/chat/completions".into())).unwrap(),
            "http://host:8000/v1/chat/completions"
        );
        assert_eq!(vllm_chat_url(&None), None);
    }

    #[test]
    fn sse_delta_parsing_across_chunk_boundaries() {
        let mut pending = String::new();
        // A delta split across two reads, a reasoning-only delta (ignored),
        // and the DONE sentinel.
        let (d, done) = drain_sse_deltas(
            &mut pending,
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"cont",
        );
        assert_eq!(d, vec!["Hel"]);
        assert!(!done);
        let (d, done) = drain_sse_deltas(
            &mut pending,
            "ent\":\"lo\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\ndata: [DONE]\n\n",
        );
        assert_eq!(d, vec!["lo"]);
        assert!(done);
        assert!(pending.is_empty());
    }

    #[test]
    fn model_crud_roundtrip() {
        let reg = LlmRegistry::new();
        reg.upsert_conversation_model(ConversationModel {
            id: "m1".into(),
            model_name: "openai/gpt-4o".into(),
            max_bytes: 16384,
            history_collection: Some("conversation_store".into()),
            api_key: None,
            system_prompt: None,
            ttl: None,
            account_id: None,
            vllm_url: None,
        });
        assert_eq!(reg.list_conversation_models().len(), 1);
        assert!(reg.get_conversation_model("m1").is_some());
        assert!(reg.delete_conversation_model("m1"));
        assert!(!reg.delete_conversation_model("m1"));
    }
}
