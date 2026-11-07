use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::RwLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StopwordSet {
    pub id: String,
    pub stopwords: HashSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(skip)]
    cached_lowercase: HashSet<String>,
}

impl StopwordSet {
    pub fn new(id: String, stopwords: HashSet<String>, locale: Option<String>) -> Self {
        let cached_lowercase = stopwords.iter().map(|w| w.to_lowercase()).collect();
        Self { id, stopwords, locale, cached_lowercase }
    }

    fn compute_cache(&mut self) {
        self.cached_lowercase = self.stopwords.iter().map(|w| w.to_lowercase()).collect();
    }
}

pub struct StopwordStore {
    sets: RwLock<FxHashMap<String, StopwordSet>>,
}

impl StopwordStore {
    pub fn new() -> Self {
        Self {
            sets: RwLock::new(FxHashMap::default()),
        }
    }

    pub fn upsert(&self, mut set: StopwordSet) {
        set.compute_cache();
        self.sets.write().unwrap_or_else(|e| e.into_inner()).insert(set.id.clone(), set);
    }

    pub fn get(&self, id: &str) -> Option<StopwordSet> {
        self.sets.read().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    pub fn list(&self) -> Vec<StopwordSet> {
        self.sets.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    pub fn delete(&self, id: &str) -> bool {
        self.sets.write().unwrap_or_else(|e| e.into_inner()).remove(id).is_some()
    }

    pub fn get_stopwords(&self, set_id: &str) -> Option<HashSet<String>> {
        self.sets.read().unwrap_or_else(|e| e.into_inner()).get(set_id).map(|s| {
            s.cached_lowercase.clone()
        })
    }

    pub fn load(&self, sets: Vec<StopwordSet>) {
        let mut store = self.sets.write().unwrap_or_else(|e| e.into_inner());
        for mut set in sets {
            set.compute_cache();
            store.insert(set.id.clone(), set);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud_operations() {
        let store = StopwordStore::new();

        store.upsert(StopwordSet::new(
            "english".into(),
            HashSet::from(["the".into(), "a".into(), "an".into()]),
            Some("en".into()),
        ));

        assert!(store.get("english").is_some());
        assert_eq!(store.list().len(), 1);

        let sw = store.get_stopwords("english").unwrap();
        assert!(sw.contains("the"));
        assert!(sw.contains("a"));
        assert!(sw.contains("an"));
        assert_eq!(sw.len(), 3);

        assert!(store.delete("english"));
        assert!(store.get("english").is_none());
    }

    #[test]
    fn get_stopwords_nonexistent() {
        let store = StopwordStore::new();
        assert!(store.get_stopwords("nope").is_none());
    }

    #[test]
    fn stopwords_lowercased() {
        let store = StopwordStore::new();
        store.upsert(StopwordSet::new(
            "test".into(),
            HashSet::from(["The".into(), "AN".into()]),
            None,
        ));

        let sw = store.get_stopwords("test").unwrap();
        assert!(sw.contains("the"));
        assert!(sw.contains("an"));
    }

    #[test]
    fn upsert_replaces_existing() {
        let store = StopwordStore::new();
        store.upsert(StopwordSet::new(
            "test".into(),
            HashSet::from(["old".into()]),
            None,
        ));
        store.upsert(StopwordSet::new(
            "test".into(),
            HashSet::from(["new".into()]),
            None,
        ));

        let sw = store.get_stopwords("test").unwrap();
        assert!(sw.contains("new"));
        assert!(!sw.contains("old"));
    }
}
