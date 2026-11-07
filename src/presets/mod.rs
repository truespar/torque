//! Search presets (Typesense's `/presets`): named search parameters a search takes with
//! `preset=<name>`. A preset's value is an object of search parameters, or a multi-search preset
//! holding `searches`, which `multi_search?preset=<name>` runs in place of the request's body.
//!
//! A search's own parameters override its preset's (Typesense's documented rule), every parameter
//! alike: the preset fills the parameters the search does not give, before any is parsed, so a
//! preset holds anything a request can. (Typesense's code ANDs a preset's filter_by with the
//! search's, an effect of sharing the merge with scoped API keys; here the search's wins as for
//! every other parameter.)

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::RwLock;

/// A preset as the API shows it: `{"name": ..., "value": ...}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub value: Value,
}

pub struct PresetStore {
    presets: RwLock<FxHashMap<String, Value>>,
}

impl Default for PresetStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetStore {
    pub fn new() -> Self {
        Self { presets: RwLock::new(FxHashMap::default()) }
    }

    pub fn upsert(&self, name: &str, value: Value) {
        self.presets.write().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), value);
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        self.presets.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// Every preset, by name.
    pub fn list(&self) -> Vec<Preset> {
        let mut all: Vec<Preset> =
            self.presets.read().unwrap_or_else(|e| e.into_inner()).iter().map(|(name, value)| Preset { name: name.clone(), value: value.clone() }).collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }

    /// Remove a preset, giving its value.
    pub fn delete(&self, name: &str) -> Option<Value> {
        self.presets.write().unwrap_or_else(|e| e.into_inner()).remove(name)
    }

    pub fn load(&self, presets: Vec<Preset>) {
        let mut store = self.presets.write().unwrap_or_else(|e| e.into_inner());
        for p in presets {
            store.insert(p.name, p.value);
        }
    }
}

/// A request parameter's text as Typesense takes a JSON value: a string itself, a number or a
/// boolean printed; None for anything else.
pub fn param_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Add the parameters of a single preset `params` lacks. A multi-search preset (one holding
/// `searches`) adds nothing to one search.
pub fn fill(params: &mut BTreeMap<String, String>, preset: &Value) -> Result<(), String> {
    let Some(obj) = preset.as_object() else { return Err("Search preset is not an object.".into()) };
    if obj.contains_key("searches") {
        return Ok(());
    }
    for (k, v) in obj {
        if !params.contains_key(k) {
            params.insert(k.clone(), param_text(v).ok_or("One or more search parameters are malformed.")?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fill_adds_what_the_search_lacks() {
        let mut params: BTreeMap<String, String> = [("q", "shoe"), ("sort_by", "price:desc"), ("filter_by", "brand:=Nike")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let preset = json!({"sort_by": "price:asc", "per_page": 25, "prefix": false, "filter_by": "in_stock:true", "num_typos": "2,1", "q": "*"});
        fill(&mut params, &preset).unwrap();
        let get = |k: &str| params.get(k).map(String::as_str);
        assert_eq!((get("q"), get("sort_by"), get("filter_by")), (Some("shoe"), Some("price:desc"), Some("brand:=Nike")));
        assert_eq!((get("per_page"), get("prefix"), get("num_typos")), (Some("25"), Some("false"), Some("2,1")));
    }

    #[test]
    fn fill_skips_multi_presets_and_rejects_bad_values() {
        let mut params = BTreeMap::new();
        fill(&mut params, &json!({"searches": [{"q": "a"}]})).unwrap();
        assert!(params.is_empty());
        assert_eq!(fill(&mut params, &json!({"facet_by": ["a"]})), Err("One or more search parameters are malformed.".into()));
        assert_eq!(fill(&mut params, &json!("x")), Err("Search preset is not an object.".into()));
    }

    #[test]
    fn crud_operations() {
        let store = PresetStore::new();
        store.upsert("p2", json!({}));
        store.upsert("p1", json!({"q": "*"}));
        assert_eq!(store.get("p1"), Some(json!({"q": "*"})));
        assert_eq!(store.list().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["p1", "p2"]);
        assert_eq!(store.delete("p1"), Some(json!({"q": "*"})));
        assert!(store.get("p1").is_none());
        assert!(store.delete("p1").is_none());
    }
}
