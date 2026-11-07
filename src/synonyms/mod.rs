use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SynonymItem {
    pub id: String,
    pub synonyms: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbols_to_index: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SynonymSet {
    /// Optional in request bodies — PUT /synonym_sets/{name} takes the name
    /// from the path (Typesense v30.1 body is `{items}` only).
    #[serde(default)]
    pub name: String,
    pub items: Vec<SynonymItem>,
}

pub struct SynonymStore {
    sets: RwLock<FxHashMap<String, SynonymSet>>,
    /// Each set compiled for searching.
    indexes: RwLock<FxHashMap<String, Arc<SynonymIndex>>>,
}

/// An item compiled: its phrases as normalized words.
struct Compiled {
    /// One-way: what a query must hold for `phrases` to apply. Multi-way (none): any of them.
    root: Option<Vec<String>>,
    phrases: Vec<Vec<String>>,
}

/// A synonym set compiled for searching.
pub struct SynonymIndex {
    items: Vec<Compiled>,
}

impl SynonymItem {
    fn compile(&self) -> Compiled {
        let symbols: Vec<char> = self.symbols_to_index.iter().flatten().filter_map(|s| s.chars().next()).collect();
        let words = |text: &str| -> Vec<String> {
            crate::text::tokenize(text, &symbols, &[]).into_iter().map(|t| {
                let (mut lowered, mut folded) = (String::new(), String::new());
                crate::text::normalize_into(t, &symbols, &mut lowered, &mut folded);
                folded
            }).filter(|t| !t.is_empty()).collect()
        };
        Compiled {
            root: self.root.as_deref().filter(|r| !r.is_empty()).map(words),
            phrases: self.synonyms.iter().map(|s| words(s)).filter(|p| !p.is_empty()).collect(),
        }
    }
}

impl SynonymIndex {
    /// Whether query word `word` is `syn`: within `num_typos` typos, or with `prefix` and `last`
    /// the start of it.
    fn same(word: &str, syn: &str, last: bool, prefix: bool, num_typos: u32) -> bool {
        if word == syn || (prefix && last && syn.starts_with(word)) {
            return true;
        }
        if num_typos == 0 {
            return false;
        }
        use levenshtein_automata::Distance;
        matches!(crate::index::inverted::typo_dfa(word, num_typos as u8).eval(syn), Distance::Exact(d) if u32::from(d) <= num_typos)
    }

    /// Where `phrase` occurs in `words`.
    fn find(words: &[String], phrase: &[String], prefix: bool, num_typos: u32) -> Vec<usize> {
        if phrase.is_empty() || phrase.len() > words.len() {
            return Vec::new();
        }
        (0..=words.len() - phrase.len())
            .filter(|&at| (0..phrase.len()).all(|k| Self::same(&words[at + k], &phrase[k], at + k + 1 == words.len(), prefix, num_typos)))
            .collect()
    }

    /// The queries `words` (normalized) rewrites to, into `out`: each occurrence of a phrase
    /// replaced by what it stands for, once per item and occurrence, without the query itself. A
    /// phrase found through typos or a prefix also stands for itself, spelled right.
    pub fn reduce(&self, words: &[String], prefix: bool, num_typos: u32, out: &mut Vec<Vec<String>>) {
        for item in &self.items {
            let sources: Vec<&Vec<String>> = match &item.root {
                Some(root) => vec![root],
                None => item.phrases.iter().collect(),
            };
            for source in sources {
                for at in Self::find(words, source, prefix, num_typos) {
                    for target in item.phrases.iter().filter(|t| *t != source).chain([source]) {
                        let mut rewrite = words[..at].to_vec();
                        rewrite.extend_from_slice(target);
                        rewrite.extend_from_slice(&words[at + source.len()..]);
                        if rewrite != words {
                            out.push(rewrite);
                        }
                    }
                }
            }
        }
        out.sort();
        out.dedup();
    }
}

impl SynonymStore {
    pub fn new() -> Self {
        Self {
            sets: RwLock::new(FxHashMap::default()),
            indexes: RwLock::new(FxHashMap::default()),
        }
    }

    /// A set's items replace the set's (an id given twice keeps its last item, placed last).
    pub fn upsert_set(&self, mut set: SynonymSet) {
        let mut items: Vec<SynonymItem> = Vec::with_capacity(set.items.len());
        for item in set.items.drain(..) {
            items.retain(|i| i.id != item.id);
            items.push(item);
        }
        set.items = items;
        let name = set.name.clone();
        let mut sets = self.sets.write().unwrap_or_else(|e| e.into_inner());
        sets.insert(name.clone(), set);
        drop(sets);
        self.compile(&name);
    }

    /// The compiled set to search with.
    pub fn index(&self, name: &str) -> Option<Arc<SynonymIndex>> {
        self.indexes.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    pub fn get_set(&self, name: &str) -> Option<SynonymSet> {
        self.sets.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    pub fn list_sets(&self) -> Vec<SynonymSet> {
        self.sets.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    pub fn delete_set(&self, name: &str) -> bool {
        let removed = self.sets.write().unwrap_or_else(|e| e.into_inner()).remove(name).is_some();
        if removed {
            self.indexes.write().unwrap_or_else(|e| e.into_inner()).remove(name);
        }
        removed
    }

    /// Upserting an item removes the item of its id and adds it last.
    pub fn upsert_item(&self, set_name: &str, item: SynonymItem) -> bool {
        let mut sets = self.sets.write().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = sets.get_mut(set_name) {
            set.items.retain(|i| i.id != item.id);
            set.items.push(item);
            drop(sets);
            self.compile(set_name);
            true
        } else {
            false
        }
    }

    pub fn get_item(&self, set_name: &str, item_id: &str) -> Option<SynonymItem> {
        self.sets
            .read()
            .unwrap()
            .get(set_name)
            .and_then(|s| s.items.iter().find(|i| i.id == item_id).cloned())
    }

    pub fn list_items(&self, set_name: &str) -> Option<Vec<SynonymItem>> {
        self.sets
            .read()
            .unwrap()
            .get(set_name)
            .map(|s| s.items.clone())
    }

    pub fn delete_item(&self, set_name: &str, item_id: &str) -> bool {
        let mut sets = self.sets.write().unwrap_or_else(|e| e.into_inner());
        if let Some(set) = sets.get_mut(set_name) {
            let before = set.items.len();
            set.items.retain(|i| i.id != item_id);
            let removed = set.items.len() < before;
            if removed {
                drop(sets);
                self.compile(set_name);
            }
            removed
        } else {
            false
        }
    }

    /// Load all sets from persisted data, compiling each.
    pub fn load(&self, sets: Vec<SynonymSet>) {
        for set in sets {
            self.upsert_set(set);
        }
    }

    /// Compile one set (or drop its index when the set is gone).
    fn compile(&self, name: &str) {
        let index = self
            .sets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(|set| Arc::new(SynonymIndex { items: set.items.iter().map(SynonymItem::compile).collect() }));
        let mut indexes = self.indexes.write().unwrap_or_else(|e| e.into_inner());
        match index {
            Some(index) => {
                indexes.insert(name.to_string(), index);
            }
            None => {
                indexes.remove(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, root: Option<&str>, synonyms: &[&str]) -> SynonymItem {
        SynonymItem { id: id.into(), synonyms: synonyms.iter().map(|s| s.to_string()).collect(), root: root.map(Into::into), locale: None, symbols_to_index: None }
    }

    fn set(name: &str, items: Vec<SynonymItem>) -> SynonymSet {
        SynonymSet { name: name.into(), items }
    }

    /// The rewrites of `q` by set `name` (none when the set is missing).
    fn rewrites(store: &SynonymStore, name: &str, q: &str) -> Vec<String> {
        let Some(index) = store.index(name) else { return Vec::new() };
        let tokens: Vec<String> = crate::text::tokenize(q, &[], &[]).into_iter().map(crate::text::normalize).collect();
        let mut out = Vec::new();
        index.reduce(&tokens, false, 0, &mut out);
        out.iter().map(|e| e.join(" ")).collect()
    }

    #[test]
    fn multi_way_synonyms() {
        let store = SynonymStore::new();
        store.upsert_set(set("shoes", vec![item("1", None, &["sneakers", "trainers", "tennis shoes"])]));
        assert_eq!(rewrites(&store, "shoes", "sneakers"), ["tennis shoes", "trainers"]);
        assert_eq!(rewrites(&store, "shoes", "trainers"), ["sneakers", "tennis shoes"]);
    }

    #[test]
    fn one_way_synonyms() {
        let store = SynonymStore::new();
        store.upsert_set(set("footwear", vec![item("1", Some("shoes"), &["sneakers", "trainers"])]));
        assert_eq!(rewrites(&store, "footwear", "shoes"), ["sneakers", "trainers"]);
        assert!(rewrites(&store, "footwear", "sneakers").is_empty());
    }

    #[test]
    fn each_set_has_its_own_index() {
        let store = SynonymStore::new();
        store.upsert_set(set("set_a", vec![item("1", None, &["big", "large"])]));
        store.upsert_set(set("set_b", vec![item("1", None, &["small", "tiny"])]));
        assert_eq!(rewrites(&store, "set_a", "big"), ["large"]);
        assert!(rewrites(&store, "set_a", "small").is_empty());
        assert!(store.delete_set("set_a"));
        assert!(store.index("set_a").is_none());
    }

    #[test]
    fn delete_item_recompiles() {
        let store = SynonymStore::new();
        store.upsert_set(set("test", vec![item("1", None, &["fast", "quick"])]));
        assert_eq!(rewrites(&store, "test", "fast"), ["quick"]);
        store.delete_item("test", "1");
        assert!(rewrites(&store, "test", "fast").is_empty());
    }

    #[test]
    fn upsert_item_replaces_and_moves_last() {
        let store = SynonymStore::new();
        store.upsert_set(set("test", vec![item("1", None, &["happy", "glad"]), item("2", None, &["sad", "blue"])]));
        assert!(store.upsert_item("test", item("1", None, &["happy", "joyful"])));
        let ids: Vec<String> = store.list_items("test").unwrap().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, ["2", "1"]);
        assert_eq!(rewrites(&store, "test", "happy"), ["joyful"]);
        assert!(!store.upsert_item("nonexistent", item("1", None, &["a", "b"])));
    }

    #[test]
    fn phrases_are_normalized() {
        let store = SynonymStore::new();
        store.upsert_set(set("test", vec![item("1", None, &["Dog", "Hound"])]));
        assert_eq!(rewrites(&store, "test", "dog"), ["hound"]);
    }
}
