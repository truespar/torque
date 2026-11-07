use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CurationRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// `exact` or `contains`.
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_mode: Option<String>,
    /// Applies when the search's `filter_by` param equals this exactly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurationInclude {
    pub id: String,
    pub position: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurationExclude {
    pub id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CurationItem {
    #[serde(default)]
    pub id: String,
    pub rule: CurationRule,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub includes: Option<Vec<CurationInclude>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excludes: Option<Vec<CurationExclude>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_matched_tokens: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_curated_hits: Option<bool>,
    /// Stop evaluating further items after this one matches. Typesense
    /// default: true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_processing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_from_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_to_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurationSet {
    /// Optional in request bodies — PUT /curation_sets/{name} takes the
    /// name from the path (Typesense v30.1 body is `{items, description}`).
    #[serde(default)]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub items: Vec<CurationItem>,
}

/// Everything a matched curation item set changes about a search. Applied
/// by the search handler before the query runs.
#[derive(Debug, Default)]
pub struct CurationOutcome {
    /// `(external_id, 1-based position)` pins, first writer per id wins.
    pub pinned: Vec<(String, usize)>,
    pub hidden: Vec<String>,
    /// AND-appended to the search's filter_by.
    pub extra_filter: Option<String>,
    pub sort_by: Option<String>,
    pub replaced_query: Option<String>,
    /// Lowercased rule tokens to strip from the query
    /// (remove_matched_tokens).
    pub remove_tokens: Vec<String>,
    /// Apply the search's filter conditions to pinned docs too.
    pub filter_curated_hits: bool,
    /// Echoed as the top-level `metadata` object in the search response.
    pub metadata: Option<serde_json::Value>,
}

impl CurationOutcome {
    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty()
            && self.hidden.is_empty()
            && self.extra_filter.is_none()
            && self.sort_by.is_none()
            && self.replaced_query.is_none()
            && self.remove_tokens.is_empty()
            && self.metadata.is_none()
    }
}

pub struct CurationStore {
    sets: RwLock<FxHashMap<String, CurationSet>>,
}

impl CurationStore {
    pub fn new() -> Self {
        Self {
            sets: RwLock::new(FxHashMap::default()),
        }
    }

    pub fn upsert_set(&self, set: CurationSet) {
        self.sets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(set.name.clone(), set);
    }

    pub fn get_set(&self, name: &str) -> Option<CurationSet> {
        self.sets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    pub fn list_sets(&self) -> Vec<CurationSet> {
        let mut sets: Vec<CurationSet> = self
            .sets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        sets.sort_by(|a, b| a.name.cmp(&b.name));
        sets
    }

    pub fn delete_set(&self, name: &str) -> bool {
        self.sets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .is_some()
    }

    pub fn upsert_item(&self, set_name: &str, item: CurationItem) -> bool {
        let mut sets = self.sets.write().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = sets.get_mut(set_name) {
            if let Some(existing) = set.items.iter_mut().find(|i| i.id == item.id) {
                *existing = item;
            } else {
                set.items.push(item);
            }
            true
        } else {
            false
        }
    }

    pub fn get_item(&self, set_name: &str, item_id: &str) -> Option<CurationItem> {
        self.sets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(set_name)
            .and_then(|s| s.items.iter().find(|i| i.id == item_id).cloned())
    }

    pub fn list_items(&self, set_name: &str) -> Option<Vec<CurationItem>> {
        self.sets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(set_name)
            .map(|s| s.items.clone())
    }

    pub fn delete_item(&self, set_name: &str, item_id: &str) -> bool {
        let mut sets = self.sets.write().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = sets.get_mut(set_name) {
            let before = set.items.len();
            set.items.retain(|i| i.id != item_id);
            set.items.len() < before
        } else {
            false
        }
    }

    pub fn load(&self, sets: Vec<CurationSet>) {
        let mut store = self.sets.write().unwrap_or_else(|e| e.into_inner());
        for set in sets {
            store.insert(set.name.clone(), set);
        }
    }

    /// Evaluate the given curation sets against a query and produce the
    /// combined outcome. Items are processed in lexical id order across
    /// sets; a matching item with `stop_processing` (default true) ends
    /// evaluation.
    ///
    /// Tag semantics (Typesense v30.1): items whose rule carries tags only
    /// trigger when the search's `curation_tags` intersects them; untagged
    /// items trigger on their query/filter rule alone.
    pub fn apply(
        &self,
        set_names: &[String],
        query: &str,
        filter_by: Option<&str>,
        curation_tags: &[String],
        now_ts: i64,
    ) -> CurationOutcome {
        let mut outcome = CurationOutcome::default();
        if set_names.is_empty() {
            return outcome;
        }
        let sets = self.sets.read().unwrap_or_else(|e| e.into_inner());
        let mut items: Vec<&CurationItem> = set_names
            .iter()
            .filter_map(|n| sets.get(n.as_str()))
            .flat_map(|s| s.items.iter())
            .collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));

        let q_lower = query.to_lowercase();
        let q_tokens: Vec<&str> = q_lower.split_whitespace().collect();

        for item in items {
            if let Some(from) = item.effective_from_ts {
                if now_ts < from {
                    continue;
                }
            }
            if let Some(to) = item.effective_to_ts {
                if now_ts > to {
                    continue;
                }
            }

            let rule = &item.rule;
            if let Some(ref tags) = rule.tags {
                if !tags.is_empty() && !tags.iter().any(|t| curation_tags.contains(t)) {
                    continue;
                }
            }

            let mut matched_query_tokens: Vec<String> = Vec::new();
            if let Some(ref rq) = rule.query {
                let rq_lower = rq.to_lowercase();
                let matches = if rq_lower == "*" {
                    true
                } else if rule.match_mode.as_deref() == Some("contains") {
                    let rule_tokens: Vec<&str> = rq_lower.split_whitespace().collect();
                    !rule_tokens.is_empty()
                        && q_tokens
                            .windows(rule_tokens.len())
                            .any(|w| w == rule_tokens.as_slice())
                } else {
                    q_lower == rq_lower
                };
                if !matches {
                    continue;
                }
                if rq_lower != "*" {
                    matched_query_tokens =
                        rq_lower.split_whitespace().map(String::from).collect();
                }
            }
            if let Some(ref rf) = rule.filter_by {
                if filter_by != Some(rf.as_str()) {
                    continue;
                }
            }
            if rule.query.is_none() && rule.filter_by.is_none() {
                // A rule with neither query nor filter never fires.
                continue;
            }

            // Matched — accumulate actions. First writer wins for the
            // scalar actions; pins/hides merge.
            if let Some(ref includes) = item.includes {
                for inc in includes {
                    if !outcome.pinned.iter().any(|(id, _)| id == &inc.id) {
                        outcome.pinned.push((inc.id.clone(), inc.position));
                    }
                }
            }
            if let Some(ref excludes) = item.excludes {
                for exc in excludes {
                    if !outcome.hidden.contains(&exc.id) {
                        outcome.hidden.push(exc.id.clone());
                    }
                }
            }
            if let Some(ref f) = item.filter_by {
                outcome.extra_filter = Some(match outcome.extra_filter.take() {
                    Some(prev) => format!("({prev}) && ({f})"),
                    None => f.clone(),
                });
            }
            if outcome.sort_by.is_none() {
                outcome.sort_by = item.sort_by.clone();
            }
            if outcome.replaced_query.is_none() {
                outcome.replaced_query = item.replace_query.clone();
            }
            if item.remove_matched_tokens.unwrap_or(false) {
                outcome.remove_tokens.extend(matched_query_tokens);
            }
            if item.filter_curated_hits.unwrap_or(false) {
                outcome.filter_curated_hits = true;
            }
            if outcome.metadata.is_none() {
                outcome.metadata = item.metadata.clone();
            }

            if item.stop_processing.unwrap_or(true) {
                break;
            }
        }

        outcome
    }
}

impl Default for CurationStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, rule: CurationRule) -> CurationItem {
        CurationItem {
            id: id.into(),
            rule,
            ..Default::default()
        }
    }

    fn query_rule(q: &str, mode: &str) -> CurationRule {
        CurationRule {
            query: Some(q.into()),
            match_mode: Some(mode.into()),
            ..Default::default()
        }
    }

    fn store_with(items: Vec<CurationItem>) -> CurationStore {
        let store = CurationStore::new();
        store.upsert_set(CurationSet {
            name: "promo".into(),
            description: None,
            items,
        });
        store
    }

    #[test]
    fn exact_match_pins_and_hides() {
        let mut it = item("a", query_rule("apple", "exact"));
        it.includes = Some(vec![CurationInclude { id: "doc1".into(), position: 1 }]);
        it.excludes = Some(vec![CurationExclude { id: "doc9".into() }]);
        let store = store_with(vec![it]);

        let out = store.apply(&["promo".into()], "Apple", None, &[], 0);
        assert_eq!(out.pinned, vec![("doc1".to_string(), 1)]);
        assert_eq!(out.hidden, vec!["doc9".to_string()]);

        let out = store.apply(&["promo".into()], "apple pie", None, &[], 0);
        assert!(out.is_empty(), "exact rule must not fire on a longer query");
    }

    #[test]
    fn contains_match_and_remove_tokens() {
        let mut it = item("a", query_rule("apple", "contains"));
        it.replace_query = None;
        it.remove_matched_tokens = Some(true);
        it.filter_by = Some("category:=fruit".into());
        let store = store_with(vec![it]);

        let out = store.apply(&["promo".into()], "fresh apple pie", None, &[], 0);
        assert_eq!(out.extra_filter.as_deref(), Some("category:=fruit"));
        assert_eq!(out.remove_tokens, vec!["apple".to_string()]);
    }

    #[test]
    fn filter_rule_matches_exact_filter_param() {
        let it = item(
            "a",
            CurationRule {
                filter_by: Some("brand:=Acme".into()),
                ..Default::default()
            },
        );
        let mut it = it;
        it.sort_by = Some("price:asc".into());
        let store = store_with(vec![it]);

        let out = store.apply(&["promo".into()], "anything", Some("brand:=Acme"), &[], 0);
        assert_eq!(out.sort_by.as_deref(), Some("price:asc"));
        let out = store.apply(&["promo".into()], "anything", Some("brand:=Other"), &[], 0);
        assert!(out.is_empty());
    }

    #[test]
    fn tagged_rules_require_matching_tags() {
        let mut it = item("a", query_rule("*", "exact"));
        it.rule.tags = Some(vec!["summer".into()]);
        it.includes = Some(vec![CurationInclude { id: "doc1".into(), position: 1 }]);
        let store = store_with(vec![it]);

        let out = store.apply(&["promo".into()], "shoes", None, &[], 0);
        assert!(out.is_empty(), "tagged rule must not fire without tags");
        let out = store.apply(&["promo".into()], "shoes", None, &["summer".into()], 0);
        assert_eq!(out.pinned.len(), 1);
    }

    #[test]
    fn effective_window_gates_rule() {
        let mut it = item("a", query_rule("*", "exact"));
        it.effective_from_ts = Some(100);
        it.effective_to_ts = Some(200);
        it.includes = Some(vec![CurationInclude { id: "d".into(), position: 1 }]);
        let store = store_with(vec![it]);

        assert!(store.apply(&["promo".into()], "q", None, &[], 50).is_empty());
        assert!(!store.apply(&["promo".into()], "q", None, &[], 150).is_empty());
        assert!(store.apply(&["promo".into()], "q", None, &[], 250).is_empty());
    }

    #[test]
    fn stop_processing_defaults_to_first_match() {
        let mut a = item("a", query_rule("*", "exact"));
        a.includes = Some(vec![CurationInclude { id: "first".into(), position: 1 }]);
        let mut b = item("b", query_rule("*", "exact"));
        b.includes = Some(vec![CurationInclude { id: "second".into(), position: 2 }]);
        let store = store_with(vec![a.clone(), b.clone()]);

        let out = store.apply(&["promo".into()], "q", None, &[], 0);
        assert_eq!(out.pinned, vec![("first".to_string(), 1)]);

        // With stop_processing=false on the first item, both fire.
        let mut a2 = a;
        a2.stop_processing = Some(false);
        let store = store_with(vec![a2, b]);
        let out = store.apply(&["promo".into()], "q", None, &[], 0);
        assert_eq!(out.pinned.len(), 2);
    }

    #[test]
    fn replace_query_and_metadata() {
        let mut it = item("a", query_rule("cheap phones", "exact"));
        it.replace_query = Some("budget smartphones".into());
        it.metadata = Some(serde_json::json!({"banner": "sale"}));
        let store = store_with(vec![it]);

        let out = store.apply(&["promo".into()], "cheap phones", None, &[], 0);
        assert_eq!(out.replaced_query.as_deref(), Some("budget smartphones"));
        assert_eq!(out.metadata.unwrap()["banner"], "sale");
    }
}
