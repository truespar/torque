//! Random operations through the HTTP API, checked against a reference. A seed makes a sequence of
//! operations on one realtime collection: document writes of every action (create, upsert, update,
//! emplace), PATCH, DELETE and GET of one document, imports of several lines, deletes and updates
//! by filter, truncation, flushes, merges, saves, a field added, restarts, and crashes at a change
//! of the data directory (`persistence::disk::fault`). The reference is the collection's documents
//! in `_seq_id` order under Typesense's rules for each action. Every answer (status and body, each
//! line of an import) must be the reference's, the documents exported its documents, and whole
//! search responses those of one segment built from them. A crash leaves the operation under way
//! done or not: the documents recovered are the one or the other. A failing seed is cut to the
//! fewest operations that still fail.

use super::{routes, AppState};
use crate::collection::CollectionRegistry;
use crate::persistence::disk::fault::{Armed, Disk, Model, Plan};
use axum::body::Body;
use axum::http::{Method, Request};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

const NAME: &str = "m";
const IDS: usize = 12;
const WORDS: [&str; 8] = ["alpha", "beta", "gamma", "delta", "alphabet", "betamax", "gammon", "deltas"];
const TAGS: [&str; 3] = ["red", "green", "blue"];

/// Every run of a seed makes the same operations.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Create,
    Upsert,
    Update,
    Emplace,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Upsert => "upsert",
            Action::Update => "update",
            Action::Emplace => "emplace",
        }
    }

    fn merges(self) -> bool {
        matches!(self, Action::Update | Action::Emplace)
    }
}

#[derive(Clone, Debug)]
enum Filter {
    Below(i64),
    Above(i64),
    Between(i64, i64),
    Tag(&'static str),
}

impl Filter {
    fn text(&self) -> String {
        match self {
            Filter::Below(k) => format!("n:<{k}"),
            Filter::Above(k) => format!("n:>{k}"),
            Filter::Between(a, b) => format!("n:[{a}..{b}]"),
            Filter::Tag(t) => format!("tags:={t}"),
        }
    }

    fn matches(&self, doc: &Value) -> bool {
        let n = doc["n"].as_i64();
        match self {
            Filter::Below(k) => n.is_some_and(|n| n < *k),
            Filter::Above(k) => n.is_some_and(|n| n > *k),
            Filter::Between(a, b) => n.is_some_and(|n| n >= *a && n <= *b),
            Filter::Tag(t) => doc["tags"].as_array().is_some_and(|a| a.iter().any(|v| v == t)),
        }
    }
}

#[derive(Clone, Debug)]
enum Op {
    Write(Action, Value),
    Patch(String, Value),
    Delete(String),
    Get(String),
    Import(Action, Vec<Value>),
    DeleteWhere(Filter),
    UpdateWhere(Filter, Value),
    Truncate,
    Flush,
    MergeMemory,
    Reclaim,
    Save,
    AddField,
    Restart,
    /// The disk stops at its `at`th change from here: the operation under way when it does, or
    /// the sixth after this one, is the last before the crash; `pick` chooses the state it left.
    Crash {
        at: u64,
        pick: u64,
    },
    Search,
}

fn title(r: &mut Rng) -> String {
    (0..1 + r.below(3)).map(|_| WORDS[r.below(WORDS.len())]).collect::<Vec<_>>().join(" ")
}

fn tags(r: &mut Rng) -> Value {
    let mut t: Vec<&str> = TAGS.iter().copied().filter(|_| r.chance(50)).collect();
    if t.is_empty() {
        t.push(TAGS[r.below(TAGS.len())]);
    }
    json!(t)
}

/// A whole document (now and then without a required field: refused).
fn whole(r: &mut Rng, id: &str, extra: bool) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(id));
    if !r.chance(5) {
        m.insert("title".into(), json!(title(r)));
    }
    if !r.chance(5) {
        m.insert("n".into(), json!(r.below(20)));
    }
    if r.chance(60) {
        m.insert("tags".into(), tags(r));
    }
    if r.chance(20) {
        // not in the schema: kept with the document
        m.insert("note".into(), json!(format!("note {}", r.below(100))));
    }
    if extra && r.chance(40) {
        m.insert("extra".into(), json!(WORDS[r.below(WORDS.len())]));
    }
    Value::Object(m)
}

/// Some of a document's fields.
fn part(r: &mut Rng, extra: bool) -> Map<String, Value> {
    let mut m = Map::new();
    loop {
        if r.chance(40) {
            m.insert("title".into(), json!(title(r)));
        }
        if r.chance(40) {
            m.insert("n".into(), json!(r.below(20)));
        }
        if r.chance(30) {
            m.insert("tags".into(), tags(r));
        }
        if r.chance(20) {
            m.insert("note".into(), json!(format!("note {}", r.below(100))));
        }
        if extra && r.chance(30) {
            m.insert("extra".into(), json!(WORDS[r.below(WORDS.len())]));
        }
        if !m.is_empty() {
            return m;
        }
    }
}

fn action(r: &mut Rng) -> Action {
    [Action::Create, Action::Upsert, Action::Update, Action::Emplace][r.below(4)]
}

/// A document a write of `action` sends: some fields for one that merges, now and then.
fn written(r: &mut Rng, action: Action, extra: bool) -> Value {
    let id = format!("d{}", r.below(IDS));
    if action.merges() && r.chance(60) {
        let mut m = part(r, extra);
        m.insert("id".into(), json!(id));
        Value::Object(m)
    } else {
        whole(r, &id, extra)
    }
}

fn filter(r: &mut Rng) -> Filter {
    match r.below(4) {
        0 => Filter::Below(r.below(20) as i64),
        1 => Filter::Above(r.below(20) as i64),
        2 => {
            let a = r.below(20) as i64;
            Filter::Between(a, a + r.below(6) as i64)
        }
        _ => Filter::Tag(TAGS[r.below(TAGS.len())]),
    }
}

/// The operations of a seed.
fn ops(seed: u64, n: usize) -> Vec<Op> {
    let mut r = Rng::new(seed);
    let mut extra = false;
    (0..n)
        .map(|_| {
            let id = format!("d{}", r.below(IDS));
            match r.below(100) {
                0..=24 => {
                    let a = action(&mut r);
                    Op::Write(a, written(&mut r, a, extra))
                }
                25..=33 => Op::Patch(id, Value::Object(part(&mut r, extra))),
                34..=41 => Op::Delete(id),
                42..=45 => Op::Get(id),
                46..=57 => {
                    let a = action(&mut r);
                    Op::Import(a, (0..1 + r.below(5)).map(|_| written(&mut r, a, extra)).collect())
                }
                58..=61 => Op::DeleteWhere(filter(&mut r)),
                62..=65 => Op::UpdateWhere(filter(&mut r), json!({"title": title(&mut r)})),
                66..=67 => Op::Truncate,
                68..=74 => Op::Flush,
                75..=77 => Op::MergeMemory,
                78..=79 => Op::Reclaim,
                80..=81 => Op::Save,
                82 if !extra => {
                    extra = true;
                    Op::AddField
                }
                83..=85 => Op::Restart,
                86..=90 => Op::Crash { at: 1 + r.below(12) as u64, pick: r.next() },
                _ => Op::Search,
            }
        })
        .collect()
}

/// What an operation answers, as far as the reference knows it.
#[derive(Debug, PartialEq)]
enum Answer {
    Json(u16, Value),
    /// An import: a result per line.
    Lines(u16, Vec<Value>),
    /// A flush: the documents the collection holds.
    Total(u16, u64),
    Status(u16),
    None,
}

fn message(status: u16, message: String) -> Answer {
    Answer::Json(status, json!({ "message": message }))
}

/// The collection's documents in `_seq_id` order, each as written (merged for an update), and
/// whether its schema has the field added.
#[derive(Clone, Debug, Default, PartialEq)]
struct Reference {
    docs: Vec<Value>,
    extra: bool,
}

impl Reference {
    fn find(&self, id: &str) -> Option<usize> {
        self.docs.iter().position(|d| d["id"] == id)
    }

    /// A document written: in the place of the one it replaces, or after every other.
    fn put(&mut self, doc: Value) {
        match self.find(doc["id"].as_str().unwrap()) {
            Some(i) => self.docs[i] = doc,
            None => self.docs.push(doc),
        }
    }

    /// Typesense's validation of a document the schema (title, n, tags, extra) types.
    fn check(&self, doc: &Value) -> Result<(), String> {
        for field in ["title", "n"] {
            if doc.get(field).is_none_or(Value::is_null) {
                return Err(format!("Field `{field}` has been declared in the schema, but is not found in the document."));
            }
        }
        Ok(())
    }

    fn merged(base: &Value, part: &Value) -> Value {
        let mut doc = base.clone();
        for (k, v) in part.as_object().unwrap() {
            doc[k] = v.clone();
        }
        doc
    }

    /// The reference after `op`, and what `op` answers.
    fn apply(&self, op: &Op) -> (Reference, Answer) {
        let mut next = self.clone();
        let answer = match op {
            Op::Write(action, doc) => {
                let id = doc["id"].as_str().unwrap().to_string();
                let existing = self.find(&id).map(|i| &self.docs[i]);
                match (action, existing) {
                    (Action::Create, Some(_)) => return (next, message(409, format!("A document with id {id} already exists."))),
                    (Action::Update, None) => return (next, message(404, format!("Could not find a document with id: {id}"))),
                    _ => {}
                }
                let doc = match existing {
                    Some(e) if action.merges() => Self::merged(e, doc),
                    _ => doc.clone(),
                };
                if let Err(e) = self.check(&doc) {
                    return (next, message(400, e));
                }
                next.put(doc.clone());
                // 201 whatever the action
                Answer::Json(201, doc)
            }
            Op::Patch(id, part) => match self.find(id) {
                None => message(404, format!("Could not find a document with id: {id}")),
                Some(i) => {
                    let doc = Self::merged(&self.docs[i], part);
                    if let Err(e) = self.check(&doc) {
                        return (next, message(400, e));
                    }
                    next.put(doc.clone());
                    Answer::Json(200, doc)
                }
            },
            Op::Delete(id) => match self.find(id) {
                None => message(404, format!("Could not find a document with id: {id}")),
                Some(i) => Answer::Json(200, next.docs.remove(i)),
            },
            Op::Get(id) => match self.find(id) {
                None => message(404, format!("Could not find a document with id: {id}")),
                Some(i) => Answer::Json(200, self.docs[i].clone()),
            },
            Op::Import(action, lines) => {
                // a line sees what an earlier line of the import wrote
                let mut docs: Vec<Value> = Vec::new();
                let mut results = Vec::new();
                for line in lines {
                    let id = line["id"].as_str().unwrap();
                    let pending = docs.iter().position(|d| d["id"] == id);
                    let stored = self.find(id).map(|i| &self.docs[i]);
                    let exists = pending.is_some() || stored.is_some();
                    match action {
                        Action::Create if exists => {
                            results.push(json!({"code": 409, "error": format!("A document with id {id} already exists."), "success": false}));
                            continue;
                        }
                        Action::Update if !exists => {
                            results.push(json!({"code": 404, "error": format!("Could not find a document with id: {id}"), "success": false}));
                            continue;
                        }
                        _ => {}
                    }
                    let doc = match (pending, stored) {
                        (Some(k), _) if action.merges() => Self::merged(&docs[k], line),
                        (None, Some(s)) if action.merges() => Self::merged(s, line),
                        _ => line.clone(),
                    };
                    match self.check(&doc) {
                        Ok(()) => {
                            results.push(json!({"success": true}));
                            match pending {
                                Some(k) => docs[k] = doc,
                                None => docs.push(doc),
                            }
                        }
                        Err(e) => results.push(json!({"code": 400, "error": e, "success": false})),
                    }
                }
                for doc in docs {
                    next.put(doc);
                }
                Answer::Lines(200, results)
            }
            Op::DeleteWhere(f) => {
                next.docs.retain(|d| !f.matches(d));
                Answer::Json(200, json!({"num_deleted": self.docs.len() - next.docs.len()}))
            }
            Op::UpdateWhere(f, part) => {
                let mut n = 0;
                for d in next.docs.iter_mut().filter(|d| f.matches(d)) {
                    *d = Self::merged(d, part);
                    n += 1;
                }
                Answer::Json(200, json!({"num_updated": n}))
            }
            Op::Truncate => {
                next.docs.clear();
                Answer::Json(200, json!({"num_deleted": self.docs.len()}))
            }
            Op::Flush => Answer::Total(200, self.docs.len() as u64),
            Op::AddField => {
                next.extra = true;
                Answer::Status(200)
            }
            Op::MergeMemory | Op::Reclaim | Op::Save | Op::Restart | Op::Crash { .. } | Op::Search => Answer::None,
        };
        (next, answer)
    }
}

fn config(dir: &Path) -> crate::config::Config {
    crate::config::Config {
        api_addr: "127.0.0.1:0".parse().unwrap(),
        ingest_addr: "127.0.0.1:0".parse().unwrap(),
        api_key: "model".into(),
        data_dir: dir.to_path_buf(),
        log_level: "error".into(),
        log_dir: None,
        cors_origins: vec![],
        ingest_allow: vec![],
        gpu_vram_budget_bytes: 0,
        max_vector_memory_bytes: 1 << 30,
        max_http_body_bytes: 64 << 20,
        max_memory_bytes: u64::MAX,
        max_ingest_connections: 4,
        query_timeout_ms: 30_000,
        ai: None,
        studio_dir: None,
        compaction_interval_secs: 1800,
        compaction_memory_threshold_percent: 80,
        tls_cert: None,
        tls_key: None,
        slow_query_ms: 0,
        allow_drop_collection: true,
        studio_enabled: false,
        quiet: true,
        allowed_ips: String::new(),
    }
}

/// A server of `collections` with its data in `dir`, called in process.
pub(super) fn serve(dir: &Path, collections: Arc<CollectionRegistry>) -> (Arc<AppState>, axum::Router) {
    let metrics = Arc::new(crate::observability::MetricsCollector::new());
    let memory_guard = crate::memory::MemoryGuard::new(u64::MAX, metrics.memory_resident.clone());
    let state = Arc::new(AppState {
        collections,
        aliases: Arc::new(crate::collection::alias::AliasRegistry::new()),
        synonyms: Arc::new(crate::synonyms::SynonymStore::new()),
        curations: Arc::new(crate::curation::CurationStore::new()),
        analytics: Arc::new(crate::analytics::AnalyticsStore::new()),
        llm: Arc::new(crate::llm::LlmRegistry::new()),
        presets: Arc::new(crate::presets::PresetStore::new()),
        stopwords: Arc::new(crate::stopwords::StopwordStore::new()),
        keys: Arc::new(crate::auth::KeyStore::new("model".into())),
        config: Arc::new(config(dir)),
        metrics,
        start_time: std::time::Instant::now(),
        memory_guard,
        ip_filter: Arc::new(crate::ip_filter::IpFilter::default()),
        fs_serial: Arc::new(tokio::sync::Mutex::new(())),
        slow_query_ms: std::sync::atomic::AtomicU64::new(0),
        search_cache: Default::default(),
        #[cfg(feature = "cuda")]
        gpu: None,
    });
    let app = routes::routes().merge(super::segments::routes()).with_state(Arc::clone(&state));
    (state, app)
}

pub(super) async fn call(app: &axum::Router, method: Method, uri: &str, body: Option<String>) -> (u16, String) {
    let mut req = Request::builder().method(method).uri(uri);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let req = req.body(body.map_or_else(Body::empty, Body::from)).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn json_of(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|_| Value::String(body.to_string()))
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn query(params: &[(&str, &str)]) -> String {
    params.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect::<Vec<_>>().join("&")
}

/// The server under test, its data directory watched (no sync made for real), and the reference.
struct Live {
    dir: tempfile::TempDir,
    disk: Option<Armed>,
    state: Option<Arc<AppState>>,
    app: Option<axum::Router>,
    /// Background merges and flushes left on (the server's way); else only the operations make them.
    background: bool,
}

impl Live {
    fn app(&self) -> &axum::Router {
        self.app.as_ref().unwrap()
    }

    fn col(&self) -> Option<Arc<crate::collection::Collection>> {
        self.state.as_ref().unwrap().collections.get(NAME)
    }

    /// Start (again): the collections of the data directory loaded, as a server starting.
    fn start(&mut self) -> Result<(), String> {
        crate::persistence::index::forget_under(self.dir.path());
        let reg = Arc::new(CollectionRegistry::new(Some(self.dir.path().to_path_buf())));
        crate::persistence::load_collections(self.dir.path(), &reg, None).map_err(|e| format!("loading: {e}"))?;
        if let Some(e) = reg.unloaded().into_iter().find(|e| e.schema.name == NAME && !e.suspended) {
            // why: its segment set read again
            let mut schema = e.schema.clone();
            schema.assign_field_ids();
            crate::persistence::index::forget_under(self.dir.path());
            let why = crate::persistence::index::load_bundle(self.dir.path(), NAME, &Arc::new(schema), e.created_at).err().map_or("(its segment set reads)".into(), |e| e.to_string());
            return Err(format!("the collection is set aside, not loadable: {why}"));
        }
        let (state, app) = serve(self.dir.path(), reg);
        self.state = Some(state);
        self.app = Some(app);
        self.hold();
        Ok(())
    }

    /// No background merge or flush: the operations make them, so a seed runs the same each time.
    fn hold(&self) {
        if let (false, Some(col)) = (self.background, self.col()) {
            col.maintaining.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// The server gone: no shutdown (a crash) or its logs closed (`clean`). Once a background
    /// merge or flush under way has finished, and nothing holds the collections (their files).
    fn stop(&mut self, clean: bool) {
        let wait = |done: &dyn Fn() -> bool| {
            let start = std::time::Instant::now();
            while !done() {
                assert!(start.elapsed() < std::time::Duration::from_secs(20), "the server does not let go of its collections");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        let cols = self.state.as_ref().map(|s| s.collections.list()).unwrap_or_default();
        if self.background {
            for c in &cols {
                wait(&|| !c.maintaining.load(std::sync::atomic::Ordering::Acquire));
            }
        }
        if clean {
            for c in &cols {
                c.close_wal();
            }
        }
        let held: Vec<std::sync::Weak<crate::collection::Collection>> = cols.iter().map(Arc::downgrade).collect();
        drop(cols);
        self.app = None;
        self.state = None;
        for w in held {
            wait(&|| w.upgrade().is_none());
        }
    }

    async fn settle(&self) {
        if let Some(col) = self.col() {
            col.wal_barrier().await;
        }
    }

    /// Nothing under way on the disk: a background merge, flush or save finished (a truncation
    /// saves in the background), the log settled. A disk armed then sees every change of the
    /// directory from what it finds there.
    async fn quiesce(&self) {
        let start = std::time::Instant::now();
        let busy = || {
            let saving = self.state.as_ref().is_some_and(|s| s.metrics.index_saves_in_flight.get() > 0);
            let merging = self.background && self.col().is_some_and(|c| c.maintaining.load(std::sync::atomic::Ordering::Acquire));
            saving || merging
        };
        while busy() {
            assert!(start.elapsed() < std::time::Duration::from_secs(20), "a background merge or save does not finish");
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        self.settle().await;
    }

    async fn export(&self) -> Result<Vec<Value>, String> {
        let (status, body) = call(self.app(), Method::GET, &format!("/collections/{NAME}/documents/export"), None).await;
        if status != 200 {
            return Err(format!("export: {status} {body}"));
        }
        Ok(body.lines().filter(|l| !l.trim().is_empty()).map(json_of).collect())
    }

    /// What `op` answers here.
    async fn execute(&mut self, op: &Op) -> Result<Answer, String> {
        let docs = format!("/collections/{NAME}/documents");
        Ok(match op {
            Op::Write(action, doc) => {
                let (s, b) = call(self.app(), Method::POST, &format!("{docs}?action={}", action.name()), Some(doc.to_string())).await;
                Answer::Json(s, json_of(&b))
            }
            Op::Patch(id, part) => {
                let (s, b) = call(self.app(), Method::PATCH, &format!("{docs}/{id}"), Some(part.to_string())).await;
                Answer::Json(s, json_of(&b))
            }
            Op::Delete(id) => {
                let (s, b) = call(self.app(), Method::DELETE, &format!("{docs}/{id}"), None).await;
                Answer::Json(s, json_of(&b))
            }
            Op::Get(id) => {
                let (s, b) = call(self.app(), Method::GET, &format!("{docs}/{id}"), None).await;
                Answer::Json(s, json_of(&b))
            }
            Op::Import(action, lines) => {
                let body = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
                let (s, b) = call(self.app(), Method::POST, &format!("{docs}/import?action={}", action.name()), Some(body)).await;
                if s != 200 {
                    return Ok(Answer::Json(s, json_of(&b)));
                }
                Answer::Lines(s, b.lines().filter(|l| !l.trim().is_empty()).map(json_of).collect())
            }
            Op::DeleteWhere(f) => {
                let (s, b) = call(self.app(), Method::DELETE, &format!("{docs}?{}", query(&[("filter_by", &f.text())])), None).await;
                Answer::Json(s, json_of(&b))
            }
            Op::UpdateWhere(f, part) => {
                let (s, b) = call(self.app(), Method::PATCH, &format!("{docs}?{}", query(&[("filter_by", &f.text())])), Some(part.to_string())).await;
                Answer::Json(s, json_of(&b))
            }
            Op::Truncate => {
                let (s, b) = call(self.app(), Method::DELETE, &format!("{docs}?truncate=true"), None).await;
                Answer::Json(s, json_of(&b))
            }
            Op::Flush => {
                let (s, b) = call(self.app(), Method::POST, &format!("{docs}/compact"), None).await;
                self.settle().await;
                match s {
                    200 => Answer::Total(s, json_of(&b)["total_docs"].as_u64().unwrap_or(u64::MAX)),
                    _ => Answer::Json(s, json_of(&b)),
                }
            }
            Op::AddField => {
                let body = json!({"fields": [{"name": "extra", "type": "string", "optional": true}]});
                let (s, b) = call(self.app(), Method::PATCH, &format!("/collections/{NAME}"), Some(body.to_string())).await;
                self.hold();
                self.settle().await;
                match s {
                    200 => Answer::Status(s),
                    _ => Answer::Json(s, json_of(&b)),
                }
            }
            Op::MergeMemory => {
                if let Some(col) = self.col() {
                    col.merge_memory();
                }
                Answer::None
            }
            Op::Reclaim => {
                if let Some(col) = self.col() {
                    let metrics = Arc::clone(&self.state.as_ref().unwrap().metrics);
                    crate::collection::compaction::reclaim(&col, Some(self.dir.path()), &metrics);
                    self.settle().await;
                }
                Answer::None
            }
            Op::Save => {
                if let Some(col) = self.col() {
                    // (an error only with the disk stopped: the crash follows)
                    let _ = col.save(self.dir.path());
                    self.settle().await;
                }
                Answer::None
            }
            Op::Restart => {
                self.stop(true);
                self.start()?;
                Answer::None
            }
            Op::Crash { .. } | Op::Search => Answer::None,
        })
    }

    /// Whole search responses: the collection's, those of one segment of the reference's documents.
    async fn searches(&self, reference: &Reference) -> Result<(), String> {
        let col = self.col().ok_or("no collection")?;
        let mut schema = (*col.schema).clone();
        schema.index_mode = crate::schema::IndexMode::Batch;
        schema.persistence_enabled = false;
        let reg = Arc::new(CollectionRegistry::new(None));
        let one = reg.create(schema).map_err(|e| e.to_string())?;
        let docs = reference.docs.iter().map(|d| crate::ingest::parse_json_document(d, &one.schema, 0)).collect::<Result<Vec<_>, _>>()?;
        one.swap_index(crate::ingest::build_index(&one.schema, docs));
        let (_, oracle) = serve(&self.dir.path().join("oracle"), reg);
        let searches: [&[(&str, &str)]; 6] = [
            &[("q", "*"), ("query_by", "title"), ("sort_by", "n:desc"), ("facet_by", "n,tags"), ("per_page", "50")],
            &[("q", "alpha"), ("query_by", "title"), ("facet_by", "tags"), ("per_page", "50")],
            &[("q", "gama"), ("query_by", "title"), ("per_page", "50")],
            &[("q", "bet"), ("query_by", "title"), ("filter_by", "n:<10"), ("sort_by", "n:asc"), ("per_page", "50")],
            &[("q", "*"), ("query_by", "title"), ("filter_by", "tags:=red"), ("group_by", "tags"), ("group_limit", "2"), ("per_page", "50")],
            &[("q", "delta alpha"), ("query_by", "title"), ("per_page", "50")],
        ];
        for params in searches {
            let uri = format!("/collections/{NAME}/documents/search?{}", query(params));
            let answer = |(s, b): (u16, String)| {
                let mut v = json_of(&b);
                if let Some(o) = v.as_object_mut() {
                    o.remove("search_time_ms");
                }
                (s, v)
            };
            let got = answer(call(self.app(), Method::GET, &uri, None).await);
            let want = answer(call(&oracle, Method::GET, &uri, None).await);
            if got != want {
                return Err(format!("search {params:?}: {} {}; one segment of the documents: {} {}", got.0, got.1, want.0, want.1));
            }
        }
        Ok(())
    }
}

/// The states a crash now could leave: under POSIX's rules, and on Windows NTFS's too.
fn crash_states(disk: &Disk, seed: u64) -> Vec<crate::persistence::disk::fault::CrashState> {
    let models: &[Model] = if cfg!(windows) { &[Model::Posix, Model::Ntfs] } else { &[Model::Posix] };
    models.iter().flat_map(|&m| disk.crash_states(m, seed)).collect()
}

/// Run `ops` from an empty data directory: Err with what differed from the reference, and where.
async fn run(ops: &[Op], background: bool) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let disk = Some(Disk::arm(dir.path(), Plan::Watch));
    let mut live = Live { dir, disk, state: None, app: None, background };
    live.start()?;
    let schema = json!({
        "name": NAME,
        "index_mode": "realtime",
        "fields": [
            {"name": "title", "type": "string"},
            {"name": "n", "type": "int32", "facet": true, "sort": true},
            {"name": "tags", "type": "string[]", "facet": true, "optional": true}
        ]
    });
    let (s, b) = call(live.app(), Method::POST, "/collections", Some(schema.to_string())).await;
    if !(200..300).contains(&s) {
        return Err(format!("create: {s} {b}"));
    }
    live.hold();
    let mut reference = Reference::default();
    // a crash under way: the operations since it was set, and its pick
    let mut crash: Option<(usize, u64)> = None;
    let timing = std::env::var_os("TORQUE_MODEL_TIMING").is_some();
    let mut spent: std::collections::BTreeMap<String, (u32, u128)> = Default::default();
    let mut tick = |what: &str, since: std::time::Instant| {
        if timing {
            let e = spent.entry(what.to_string()).or_default();
            e.0 += 1;
            e.1 += since.elapsed().as_micros();
        }
    };
    for (i, op) in ops.iter().enumerate() {
        let at = |e: String| format!("operation {i} ({op:?}): {e}");
        let (after, want) = reference.apply(op);
        let t = std::time::Instant::now();
        let got = live.execute(op).await.map_err(at)?;
        tick(&format!("{op:?}").chars().take_while(|c| c.is_alphanumeric()).collect::<String>(), t);
        let stopped = crash.is_some() && live.disk.as_ref().unwrap().stopped().is_some();
        let candidates = if got == want {
            // acknowledged
            vec![after]
        } else if stopped {
            vec![reference.clone(), after]
        } else {
            return Err(at(format!("answered {got:?}; the reference {want:?}")));
        };
        if let Some((since, pick)) = crash.as_mut() {
            *since += 1;
            if stopped || *since > 6 {
                let pick = *pick;
                crash = None;
                let t = std::time::Instant::now();
                reference = crash_and_recover(&mut live, pick, candidates).await.map_err(at)?;
                tick("crash and recover", t);
                continue;
            }
        }
        reference = candidates.into_iter().next().unwrap();
        let t = std::time::Instant::now();
        let docs = live.export().await.map_err(at)?;
        tick("export", t);
        if docs != reference.docs {
            return Err(at(format!("exported {docs:?}; the reference {:?}", reference.docs)));
        }
        let t = std::time::Instant::now();
        match op {
            Op::Search => {
                live.searches(&reference).await.map_err(at)?;
                tick("searches", t);
            }
            Op::Crash { at: n, pick } => {
                live.quiesce().await;
                if let Some((_, armed)) = crash.take().filter(|_| live.disk.as_ref().unwrap().stopped().is_some()) {
                    // a background save stopped the disk armed before: that crash first
                    reference = crash_and_recover(&mut live, armed, vec![reference.clone()]).await.map_err(at)?;
                }
                live.disk = None;
                live.disk = Some(Disk::arm(live.dir.path(), Plan::Crash(*n)));
                crash = Some((0, *pick));
                tick("arm", t);
            }
            _ => {}
        }
    }
    if timing {
        eprintln!("{}", spent.iter().map(|(k, (n, us))| format!("{k} {n}x {} ms", us / 1000)).collect::<Vec<_>>().join(", "));
    }
    live.searches(&reference).await?;
    live.stop(true);
    Ok(())
}

/// Crash: the server gone without a shutdown, one of the states the crash could leave on disk, a
/// start from it. The documents recovered are one of `candidates`' (the operation under way done
/// or not): the reference from here.
async fn crash_and_recover(live: &mut Live, pick: u64, candidates: Vec<Reference>) -> Result<Reference, String> {
    live.stop(false);
    let disk = live.disk.take().unwrap();
    let states = crash_states(&disk, pick);
    let state = &states[(pick % states.len() as u64) as usize];
    drop(disk);
    state.put(live.dir.path());
    live.disk = Some(Disk::arm(live.dir.path(), Plan::Watch));
    live.start().map_err(|e| format!("after a crash ({}): {e}", state.label))?;
    let docs = live.export().await.map_err(|e| format!("after a crash ({}): {e}", state.label))?;
    let extra = live.col().is_some_and(|c| c.schema.fields.iter().any(|f| f.name == "extra"));
    candidates
        .iter()
        .find(|c| c.docs == docs && c.extra == extra)
        .cloned()
        .ok_or_else(|| format!("after a crash ({}): recovered {docs:?} (field added: {extra}); the reference {candidates:?}", state.label))
}

fn run_blocking(ops: &[Op], background: bool) -> Result<(), String> {
    // (one worker: searches run in `block_in_place`, which a current-thread runtime refuses)
    tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap().block_on(run(ops, background))
}

/// The fewest of `ops` that still fail (each run from the start), and how they fail.
fn shrink(mut ops: Vec<Op>, mut error: String) -> (Vec<Op>, String) {
    let mut chunk = ops.len() / 2;
    let mut runs = 0;
    while chunk >= 1 && runs < 300 {
        let mut i = 0;
        let mut cut = false;
        while i + chunk <= ops.len() && runs < 300 {
            let fewer: Vec<Op> = ops[..i].iter().chain(&ops[i + chunk..]).cloned().collect();
            runs += 1;
            match run_blocking(&fewer, false) {
                Err(e) => {
                    ops = fewer;
                    error = e;
                    cut = true;
                }
                Ok(()) => i += chunk,
            }
        }
        if !cut {
            chunk /= 2;
        }
    }
    (ops, error)
}

/// Run each seed's operations; a failing one is cut to the fewest operations that fail. With
/// `TORQUE_MODEL_PROGRESS` set, a line per seed.
fn check_seeds(seeds: impl IntoIterator<Item = u64>, n: usize, background: bool) {
    let progress = std::env::var_os("TORQUE_MODEL_PROGRESS").is_some();
    for seed in seeds {
        let all = ops(seed, n);
        let start = std::time::Instant::now();
        let result = run_blocking(&all, background);
        if progress {
            eprintln!("seed {seed}{}: {} in {} ms", if background { " (background)" } else { "" }, if result.is_ok() { "ok" } else { "FAILS" }, start.elapsed().as_millis());
        }
        if let Err(e) = result {
            if background {
                panic!("seed {seed} (background merges and flushes on): {e}");
            }
            let (fewest, e) = shrink(all, e);
            let listed: Vec<String> = fewest.iter().enumerate().map(|(i, op)| format!("  {i}: {op:?}")).collect();
            panic!("seed {seed} fails; cut to {} operations:\n{}\n{e}", fewest.len(), listed.join("\n"));
        }
    }
}

#[test]
fn random_operations_answer_as_the_reference() {
    check_seeds(1..=48, 150, false);
}

#[test]
fn random_operations_with_background_merges_answer_as_the_reference() {
    check_seeds(101..=112, 150, true);
}

/// Many seeds: `TORQUE_MODEL_SEEDS=n` (default 500) from `TORQUE_MODEL_FROM` (default 1000).
#[test]
#[ignore]
fn many_random_operations_answer_as_the_reference() {
    let n: u64 = std::env::var("TORQUE_MODEL_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    let from: u64 = std::env::var("TORQUE_MODEL_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(1000);
    check_seeds(from..from + n, 200, false);
    check_seeds(from + n..from + n + n / 10, 200, true);
}

/// Writers and merges going on while readers look: a view holds each write whole (the documents
/// a write wrote together all of it or none of it), whatever segments it is in.
#[test]
fn a_view_holds_each_write_whole() {
    use crate::schema::FieldValue;
    use std::sync::atomic::{AtomicBool, Ordering};
    let reg = Arc::new(CollectionRegistry::new(None));
    let schema: crate::schema::Schema = serde_json::from_value(json!({
        "name": "whole",
        "index_mode": "realtime",
        "persistence_enabled": false,
        "fields": [{"name": "n", "type": "int64", "sort": true}]
    }))
    .unwrap();
    let col = reg.create(schema).unwrap();
    let n_field = col.schema.name_to_id["n"] as usize;
    let done = Arc::new(AtomicBool::new(false));
    let writers: Vec<_> = (0..4)
        .map(|t| {
            let col = Arc::clone(&col);
            std::thread::spawn(move || {
                for v in 1..=400i64 {
                    let ids = [format!("a{t}"), format!("b{t}"), format!("c{t}")];
                    if v % 7 == 0 {
                        col.apply_writes(Vec::new(), &ids).unwrap();
                    } else {
                        let docs = ids
                            .iter()
                            .map(|id| crate::ingest::parse_json_document(&json!({"id": id, "n": v * 10 + t}), &col.schema, 0).unwrap())
                            .collect();
                        col.apply_writes(docs, &[]).unwrap();
                    }
                }
            })
        })
        .collect();
    let merger = {
        let (col, done) = (Arc::clone(&col), Arc::clone(&done));
        std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                col.merge_memory();
                if col.snapshot_bundle().memory > 40 {
                    col.compact();
                }
                col.merge_flushed();
            }
        })
    };
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let (col, done) = (Arc::clone(&col), Arc::clone(&done));
            std::thread::spawn(move || {
                let mut looked = 0u64;
                while !done.load(Ordering::Acquire) {
                    let view = col.view();
                    for t in 0..4 {
                        let values: Vec<Option<i64>> = ["a", "b", "c"]
                            .iter()
                            .map(|p| {
                                let (i, doc) = view.lookup(&format!("{p}{t}"))?;
                                let d = view.segment(i).0.get_doc(doc, &col.schema)?;
                                match d.fields.get(n_field) {
                                    Some(FieldValue::Int64(n)) => Some(*n),
                                    other => panic!("n {other:?}"),
                                }
                            })
                            .collect();
                        assert!(values.windows(2).all(|w| w[0] == w[1]), "writer {t}: a view holds part of a write: {values:?}");
                        looked += 1;
                    }
                }
                looked
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    done.store(true, Ordering::Release);
    merger.join().unwrap();
    for r in readers {
        assert!(r.join().unwrap() > 0);
    }
}
