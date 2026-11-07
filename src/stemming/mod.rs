use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DictionaryWord {
    pub word: String,
    pub root: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StemmingDictionary {
    pub id: String,
    pub words: Vec<DictionaryWord>,
}

/// Custom stemming dictionaries (Typesense v30.1 `/stemming/dictionaries`).
/// Fields opt in via `stem_dictionary` in the schema; matching words map to
/// their root instead of going through the Snowball stemmer.
///
/// Accessed through a process-wide instance (`stemming::global()`): the index
/// builder runs deep in the ingest pipeline with no AppState access, and both
/// the build and query paths must resolve against the same dictionary.
#[derive(Default)]
pub struct StemmingDictionaryStore {
    dicts: RwLock<FxHashMap<String, FxHashMap<String, String>>>,
}

impl StemmingDictionaryStore {
    /// Merge words into a dictionary, creating it if absent.
    /// Returns the number of words now in the dictionary.
    pub fn import(&self, id: &str, words: Vec<DictionaryWord>) -> usize {
        let mut dicts = self.dicts.write().unwrap();
        let dict = dicts.entry(id.to_string()).or_default();
        for w in words {
            dict.insert(w.word, w.root);
        }
        dict.len()
    }

    pub fn get(&self, id: &str) -> Option<StemmingDictionary> {
        let dicts = self.dicts.read().unwrap();
        let dict = dicts.get(id)?;
        let mut words: Vec<DictionaryWord> = dict
            .iter()
            .map(|(w, r)| DictionaryWord {
                word: w.clone(),
                root: r.clone(),
            })
            .collect();
        words.sort_by(|a, b| a.word.cmp(&b.word));
        Some(StemmingDictionary {
            id: id.to_string(),
            words,
        })
    }

    pub fn list_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.dicts.read().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Root lookup for one word in one dictionary.
    pub fn lookup(&self, id: &str, word: &str) -> Option<String> {
        self.dicts.read().unwrap().get(id)?.get(word).cloned()
    }

    /// Root lookup across several dictionaries, first match wins.
    pub fn lookup_any(&self, ids: &[&str], word: &str) -> Option<String> {
        let dicts = self.dicts.read().unwrap();
        for id in ids {
            if let Some(root) = dicts.get(*id).and_then(|d| d.get(word)) {
                return Some(root.clone());
            }
        }
        None
    }

    pub fn load(&self, list: Vec<StemmingDictionary>) {
        let mut dicts = self.dicts.write().unwrap();
        for d in list {
            let map = d.words.into_iter().map(|w| (w.word, w.root)).collect();
            dicts.insert(d.id, map);
        }
    }

    pub fn dump(&self) -> Vec<StemmingDictionary> {
        let ids = self.list_ids();
        ids.into_iter().filter_map(|id| self.get(&id)).collect()
    }
}

pub fn global() -> &'static StemmingDictionaryStore {
    static STORE: OnceLock<StemmingDictionaryStore> = OnceLock::new();
    STORE.get_or_init(StemmingDictionaryStore::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_get_lookup() {
        let store = StemmingDictionaryStore::default();
        let n = store.import(
            "irregular",
            vec![
                DictionaryWord {
                    word: "people".into(),
                    root: "person".into(),
                },
                DictionaryWord {
                    word: "geese".into(),
                    root: "goose".into(),
                },
            ],
        );
        assert_eq!(n, 2);
        assert_eq!(store.lookup("irregular", "people").as_deref(), Some("person"));
        assert_eq!(store.lookup("irregular", "dog"), None);
        assert_eq!(store.lookup("missing", "people"), None);
        assert_eq!(store.list_ids(), vec!["irregular"]);
        let d = store.get("irregular").unwrap();
        assert_eq!(d.words.len(), 2);

        // Re-import merges (upsert per word)
        store.import(
            "irregular",
            vec![DictionaryWord {
                word: "people".into(),
                root: "human".into(),
            }],
        );
        assert_eq!(store.lookup("irregular", "people").as_deref(), Some("human"));
        assert_eq!(store.get("irregular").unwrap().words.len(), 2);
    }
}
