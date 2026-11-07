use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

/// Upper bound on retained log-rule events. The store is an in-memory ring:
/// oldest events are dropped once the cap is reached (Typesense analytics
/// also aggregates in memory between flushes).
const MAX_LOG_EVENTS: usize = 10_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalyticsRuleParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_collection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counter_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_search_requests: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expand_query: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta_fields: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<i64>,
}

/// Analytics rule (Typesense v30.1 `AnalyticsRule`). `rule_type` is one of
/// `popular_queries`, `nohits_queries`, `counter`, `log`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalyticsRule {
    pub name: String,
    #[serde(rename = "type")]
    pub rule_type: String,
    pub collection: String,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<AnalyticsRuleParams>,
}

impl AnalyticsRule {
    pub fn validate(&self) -> Result<(), String> {
        match self.rule_type.as_str() {
            "popular_queries" | "nohits_queries" => {
                if self
                    .params
                    .as_ref()
                    .and_then(|p| p.destination_collection.as_ref())
                    .is_none()
                {
                    return Err(format!(
                        "Rule `{}`: `{}` rules need `params.destination_collection`.",
                        self.name, self.rule_type
                    ));
                }
            }
            "counter" => {
                if self
                    .params
                    .as_ref()
                    .and_then(|p| p.counter_field.as_ref())
                    .is_none()
                {
                    return Err(format!(
                        "Rule `{}`: `counter` rules need `params.counter_field`.",
                        self.name
                    ));
                }
            }
            "log" => {}
            other => {
                return Err(format!(
                    "Rule `{}`: unknown analytics rule type `{other}`.",
                    self.name
                ));
            }
        }
        Ok(())
    }

    fn captures_searches(&self) -> bool {
        matches!(self.rule_type.as_str(), "popular_queries" | "nohits_queries")
            && self
                .params
                .as_ref()
                .and_then(|p| p.capture_search_requests)
                .unwrap_or(true)
    }

    pub fn weight(&self) -> i64 {
        self.params.as_ref().and_then(|p| p.weight).unwrap_or(1)
    }
}

/// Event payload (`data` object of a POSTed analytics event).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AnalyticsEventData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analytics_tag: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AnalyticsEvent {
    pub name: String,
    pub event_type: String,
    pub data: AnalyticsEventData,
}

/// A retained event from a `log` rule, shaped for `GET /analytics/events`.
#[derive(Clone, Debug, Serialize)]
pub struct LoggedEvent {
    pub name: String,
    pub event_type: String,
    pub collection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub timestamp: i64,
}

/// Pending aggregation drained by a flush: per rule, either query counts or
/// per-document counter deltas, plus the rule itself for routing.
pub struct PendingFlush {
    pub query_counts: Vec<(AnalyticsRule, FxHashMap<String, u64>)>,
    pub doc_counts: Vec<(AnalyticsRule, FxHashMap<String, i64>)>,
}

#[derive(Default, Serialize)]
pub struct AnalyticsStatus {
    pub popular_prefix_queries: usize,
    pub nohits_prefix_queries: usize,
    pub log_prefix_queries: usize,
    pub query_counter_events: usize,
    pub doc_counter_events: usize,
    pub query_log_events: usize,
    pub doc_log_events: usize,
}

pub struct AnalyticsStore {
    rules: RwLock<FxHashMap<String, AnalyticsRule>>,
    /// rule name -> query -> count (popular/nohits rules).
    query_counts: Mutex<FxHashMap<String, FxHashMap<String, u64>>>,
    /// rule name -> doc id -> accumulated weight (counter rules).
    doc_counts: Mutex<FxHashMap<String, FxHashMap<String, i64>>>,
    log_events: Mutex<Vec<LoggedEvent>>,
    /// Fast-path gate for the per-search capture hook.
    has_capture_rules: AtomicBool,
}

impl AnalyticsStore {
    pub fn new() -> Self {
        Self {
            rules: RwLock::new(FxHashMap::default()),
            query_counts: Mutex::new(FxHashMap::default()),
            doc_counts: Mutex::new(FxHashMap::default()),
            log_events: Mutex::new(Vec::new()),
            has_capture_rules: AtomicBool::new(false),
        }
    }

    pub fn upsert_rule(&self, rule: AnalyticsRule) -> Result<(), String> {
        rule.validate()?;
        let mut rules = self.rules.write().unwrap_or_else(|e| e.into_inner());
        rules.insert(rule.name.clone(), rule);
        let capture = rules.values().any(|r| r.captures_searches());
        drop(rules);
        self.has_capture_rules.store(capture, Ordering::Relaxed);
        Ok(())
    }

    pub fn get_rule(&self, name: &str) -> Option<AnalyticsRule> {
        self.rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    pub fn list_rules(&self) -> Vec<AnalyticsRule> {
        let mut rules: Vec<AnalyticsRule> = self
            .rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        rules.sort_by(|a, b| a.name.cmp(&b.name));
        rules
    }

    pub fn delete_rule(&self, name: &str) -> bool {
        let mut rules = self.rules.write().unwrap_or_else(|e| e.into_inner());
        let removed = rules.remove(name).is_some();
        let capture = rules.values().any(|r| r.captures_searches());
        drop(rules);
        self.has_capture_rules.store(capture, Ordering::Relaxed);
        if removed {
            self.query_counts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(name);
            self.doc_counts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(name);
        }
        removed
    }

    pub fn load(&self, rules: Vec<AnalyticsRule>) {
        let mut store = self.rules.write().unwrap_or_else(|e| e.into_inner());
        for rule in rules {
            store.insert(rule.name.clone(), rule);
        }
        let capture = store.values().any(|r| r.captures_searches());
        drop(store);
        self.has_capture_rules.store(capture, Ordering::Relaxed);
    }

    /// Route a POSTed event to its rule's aggregation. Errors when the rule
    /// doesn't exist or the event type doesn't match the rule's.
    pub fn record_event(&self, event: AnalyticsEvent, now_ts: i64) -> Result<(), String> {
        let rule = self
            .get_rule(&event.name)
            .ok_or_else(|| format!("Analytics rule `{}` not found.", event.name))?;
        if rule.event_type != event.event_type {
            return Err(format!(
                "Event type `{}` does not match rule `{}`'s event type `{}`.",
                event.event_type, rule.name, rule.event_type
            ));
        }
        match rule.rule_type.as_str() {
            "popular_queries" | "nohits_queries" => {
                let q = event
                    .data
                    .q
                    .ok_or_else(|| "Query events need `data.q`.".to_string())?;
                self.bump_query(&rule.name, &q, 1);
            }
            "counter" => {
                let weight = rule.weight();
                let mut counts = self.doc_counts.lock().unwrap_or_else(|e| e.into_inner());
                let entry = counts.entry(rule.name.clone()).or_default();
                if let Some(doc_id) = event.data.doc_id {
                    *entry.entry(doc_id).or_insert(0) += weight;
                }
                for doc_id in event.data.doc_ids.unwrap_or_default() {
                    *entry.entry(doc_id).or_insert(0) += weight;
                }
            }
            "log" => {
                let mut events = self.log_events.lock().unwrap_or_else(|e| e.into_inner());
                if events.len() >= MAX_LOG_EVENTS {
                    events.remove(0);
                }
                events.push(LoggedEvent {
                    name: rule.name.clone(),
                    event_type: event.event_type,
                    collection: rule.collection.clone(),
                    query: event.data.q,
                    doc_id: event.data.doc_id,
                    doc_ids: event.data.doc_ids,
                    user_id: event.data.user_id,
                    timestamp: now_ts,
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Cheap gate for the per-search hook: true when any popular/nohits rule
    /// has `capture_search_requests` enabled.
    pub fn captures_searches(&self) -> bool {
        self.has_capture_rules.load(Ordering::Relaxed)
    }

    /// Auto-capture a search request for popular/nohits rules targeting
    /// `collection`. Call only when `captures_searches()` is true.
    pub fn capture_search(&self, collection: &str, q: &str, found: usize) {
        if q.is_empty() || q == "*" {
            return;
        }
        let rules = self.rules.read().unwrap_or_else(|e| e.into_inner());
        for rule in rules.values() {
            if !rule.captures_searches() || rule.collection != collection {
                continue;
            }
            if rule.rule_type == "nohits_queries" && found > 0 {
                continue;
            }
            self.bump_query(&rule.name, q, 1);
        }
    }

    fn bump_query(&self, rule_name: &str, q: &str, by: u64) {
        let mut counts = self.query_counts.lock().unwrap_or_else(|e| e.into_inner());
        *counts
            .entry(rule_name.to_string())
            .or_default()
            .entry(q.to_lowercase())
            .or_insert(0) += by;
    }

    /// Drain all pending aggregations for a flush. The caller writes them to
    /// their destination collections; on write failure the drained counts are
    /// lost (same at-most-once semantics as Typesense's in-memory analytics).
    pub fn drain_pending(&self) -> PendingFlush {
        let rules = self.rules.read().unwrap_or_else(|e| e.into_inner());
        let mut query_counts = Vec::new();
        for (name, counts) in self
            .query_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            if let Some(rule) = rules.get(&name) {
                query_counts.push((rule.clone(), counts));
            }
        }
        let mut doc_counts = Vec::new();
        for (name, counts) in self
            .doc_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            if let Some(rule) = rules.get(&name) {
                doc_counts.push((rule.clone(), counts));
            }
        }
        PendingFlush {
            query_counts,
            doc_counts,
        }
    }

    /// Recent events for `GET /analytics/events` (log rules only).
    pub fn events_for(&self, user_id: &str, rule_name: &str, n: usize) -> Vec<LoggedEvent> {
        let events = self.log_events.lock().unwrap_or_else(|e| e.into_inner());
        events
            .iter()
            .rev()
            .filter(|e| e.name == rule_name && e.user_id.as_deref() == Some(user_id))
            .take(n.min(1000))
            .cloned()
            .collect()
    }

    pub fn status(&self) -> AnalyticsStatus {
        let rules = self.rules.read().unwrap_or_else(|e| e.into_inner());
        let query_counts = self.query_counts.lock().unwrap_or_else(|e| e.into_inner());
        let doc_counts = self.doc_counts.lock().unwrap_or_else(|e| e.into_inner());
        let log_events = self.log_events.lock().unwrap_or_else(|e| e.into_inner());

        let mut status = AnalyticsStatus::default();
        for (name, counts) in query_counts.iter() {
            match rules.get(name).map(|r| r.rule_type.as_str()) {
                Some("popular_queries") => status.popular_prefix_queries += counts.len(),
                Some("nohits_queries") => status.nohits_prefix_queries += counts.len(),
                _ => {}
            }
        }
        status.doc_counter_events = doc_counts.values().map(|c| c.len()).sum();
        status.query_log_events = log_events.iter().filter(|e| e.query.is_some()).count();
        status.doc_log_events = log_events
            .iter()
            .filter(|e| e.doc_id.is_some() || e.doc_ids.is_some())
            .count();
        status
    }
}

impl Default for AnalyticsStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str, rule_type: &str, collection: &str, event_type: &str) -> AnalyticsRule {
        AnalyticsRule {
            name: name.into(),
            rule_type: rule_type.into(),
            collection: collection.into(),
            event_type: event_type.into(),
            rule_tag: None,
            params: Some(AnalyticsRuleParams {
                destination_collection: Some("dest".into()),
                counter_field: Some("clicks".into()),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn rule_validation() {
        let store = AnalyticsStore::new();
        let mut bad = rule("r", "popular_queries", "c", "search");
        bad.params = None;
        assert!(store.upsert_rule(bad).is_err(), "popular needs destination");
        let mut bad = rule("r", "counter", "c", "click");
        bad.params.as_mut().unwrap().counter_field = None;
        assert!(store.upsert_rule(bad).is_err(), "counter needs counter_field");
        let mut bad = rule("r", "bogus", "c", "click");
        bad.params = None;
        assert!(store.upsert_rule(bad).is_err(), "unknown type rejected");
    }

    #[test]
    fn query_events_aggregate_and_drain() {
        let store = AnalyticsStore::new();
        store.upsert_rule(rule("pop", "popular_queries", "products", "search")).unwrap();
        for q in ["Laptop", "laptop", "phone"] {
            store
                .record_event(
                    AnalyticsEvent {
                        name: "pop".into(),
                        event_type: "search".into(),
                        data: AnalyticsEventData { q: Some(q.into()), ..Default::default() },
                    },
                    0,
                )
                .unwrap();
        }
        let status = store.status();
        assert_eq!(status.popular_prefix_queries, 2, "case-folded queries aggregate");

        let pending = store.drain_pending();
        assert_eq!(pending.query_counts.len(), 1);
        let (_, counts) = &pending.query_counts[0];
        assert_eq!(counts["laptop"], 2);
        assert_eq!(counts["phone"], 1);
        assert_eq!(store.status().popular_prefix_queries, 0, "drained");
    }

    #[test]
    fn counter_events_weighted() {
        let store = AnalyticsStore::new();
        let mut r = rule("clicks", "counter", "products", "click");
        r.params.as_mut().unwrap().weight = Some(2);
        store.upsert_rule(r).unwrap();
        for _ in 0..3 {
            store
                .record_event(
                    AnalyticsEvent {
                        name: "clicks".into(),
                        event_type: "click".into(),
                        data: AnalyticsEventData { doc_id: Some("d1".into()), ..Default::default() },
                    },
                    0,
                )
                .unwrap();
        }
        let pending = store.drain_pending();
        assert_eq!(pending.doc_counts[0].1["d1"], 6);
    }

    #[test]
    fn event_type_mismatch_rejected() {
        let store = AnalyticsStore::new();
        store.upsert_rule(rule("pop", "popular_queries", "products", "search")).unwrap();
        let err = store.record_event(
            AnalyticsEvent {
                name: "pop".into(),
                event_type: "click".into(),
                data: AnalyticsEventData { q: Some("x".into()), ..Default::default() },
            },
            0,
        );
        assert!(err.is_err());
    }

    #[test]
    fn search_capture_respects_rule_type_and_collection() {
        let store = AnalyticsStore::new();
        store.upsert_rule(rule("pop", "popular_queries", "products", "search")).unwrap();
        store.upsert_rule(rule("misses", "nohits_queries", "products", "search")).unwrap();
        assert!(store.captures_searches());

        store.capture_search("products", "laptop", 5);
        store.capture_search("products", "unicorn", 0);
        store.capture_search("other", "laptop", 5);
        store.capture_search("products", "*", 0);

        let pending = store.drain_pending();
        let by_name: FxHashMap<&str, &FxHashMap<String, u64>> = pending
            .query_counts
            .iter()
            .map(|(r, c)| (r.name.as_str(), c))
            .collect();
        assert_eq!(by_name["pop"].len(), 2, "popular counts every non-wildcard query");
        assert_eq!(by_name["misses"].len(), 1, "nohits counts only found=0");
        assert!(by_name["misses"].contains_key("unicorn"));
    }

    #[test]
    fn log_events_retained_and_filtered() {
        let store = AnalyticsStore::new();
        let mut r = rule("journal", "log", "products", "click");
        r.params = None;
        store.upsert_rule(r).unwrap();
        for (user, doc) in [("u1", "d1"), ("u2", "d2"), ("u1", "d3")] {
            store
                .record_event(
                    AnalyticsEvent {
                        name: "journal".into(),
                        event_type: "click".into(),
                        data: AnalyticsEventData {
                            doc_id: Some(doc.into()),
                            user_id: Some(user.into()),
                            ..Default::default()
                        },
                    },
                    42,
                )
                .unwrap();
        }
        let events = store.events_for("u1", "journal", 10);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].doc_id.as_deref(), Some("d3"), "newest first");
        assert_eq!(store.status().doc_log_events, 3);
    }
}
