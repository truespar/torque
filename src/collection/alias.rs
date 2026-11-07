use dashmap::DashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasInfo {
    pub name: String,
    pub collection_name: String,
}

pub struct AliasRegistry {
    aliases: DashMap<String, String>,
}

impl AliasRegistry {
    pub fn new() -> Self {
        Self {
            aliases: DashMap::new(),
        }
    }

    /// Resolve a name: if it's an alias, return the target collection name.
    /// Otherwise return the name itself.
    pub fn resolve(&self, name: &str) -> String {
        self.aliases
            .get(name)
            .map(|r| r.value().clone())
            .unwrap_or_else(|| name.to_string())
    }

    pub fn upsert(&self, alias: String, collection_name: String) {
        self.aliases.insert(alias, collection_name);
    }

    pub fn get(&self, alias: &str) -> Option<AliasInfo> {
        self.aliases.get(alias).map(|r| AliasInfo {
            name: alias.to_string(),
            collection_name: r.value().clone(),
        })
    }

    pub fn delete(&self, alias: &str) -> Option<String> {
        self.aliases.remove(alias).map(|(_, v)| v)
    }

    pub fn list(&self) -> Vec<AliasInfo> {
        self.aliases
            .iter()
            .map(|r| AliasInfo {
                name: r.key().clone(),
                collection_name: r.value().clone(),
            })
            .collect()
    }

    /// Return all alias names that point to the given collection.
    pub fn aliases_for_collection(&self, collection_name: &str) -> Vec<String> {
        self.aliases
            .iter()
            .filter(|r| r.value() == collection_name)
            .map(|r| r.key().clone())
            .collect()
    }

    /// Load aliases from a list (used during startup persistence load).
    pub fn load(&self, aliases: Vec<AliasInfo>) {
        for alias in aliases {
            self.aliases.insert(alias.name, alias.collection_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_direct_name() {
        let reg = AliasRegistry::new();
        assert_eq!(reg.resolve("products"), "products");
    }

    #[test]
    fn resolve_alias() {
        let reg = AliasRegistry::new();
        reg.upsert("products".into(), "products_v2".into());
        assert_eq!(reg.resolve("products"), "products_v2");
    }

    #[test]
    fn upsert_overwrites() {
        let reg = AliasRegistry::new();
        reg.upsert("products".into(), "products_v1".into());
        reg.upsert("products".into(), "products_v2".into());
        assert_eq!(reg.resolve("products"), "products_v2");
    }

    #[test]
    fn get_existing() {
        let reg = AliasRegistry::new();
        reg.upsert("products".into(), "products_v2".into());
        let info = reg.get("products").unwrap();
        assert_eq!(info.name, "products");
        assert_eq!(info.collection_name, "products_v2");
    }

    #[test]
    fn get_missing() {
        let reg = AliasRegistry::new();
        assert!(reg.get("nope").is_none());
    }

    #[test]
    fn delete_existing() {
        let reg = AliasRegistry::new();
        reg.upsert("products".into(), "products_v2".into());
        let removed = reg.delete("products").unwrap();
        assert_eq!(removed, "products_v2");
        assert!(reg.get("products").is_none());
    }

    #[test]
    fn delete_missing() {
        let reg = AliasRegistry::new();
        assert!(reg.delete("nope").is_none());
    }

    #[test]
    fn aliases_for_collection_found() {
        let reg = AliasRegistry::new();
        reg.upsert("latest".into(), "products_v2".into());
        reg.upsert("stable".into(), "products_v2".into());
        reg.upsert("other".into(), "orders".into());
        let mut result = reg.aliases_for_collection("products_v2");
        result.sort();
        assert_eq!(result, vec!["latest", "stable"]);
    }

    #[test]
    fn aliases_for_collection_none() {
        let reg = AliasRegistry::new();
        reg.upsert("latest".into(), "products_v2".into());
        assert!(reg.aliases_for_collection("orders").is_empty());
    }

    #[test]
    fn list_all() {
        let reg = AliasRegistry::new();
        reg.upsert("a".into(), "col_a".into());
        reg.upsert("b".into(), "col_b".into());
        let list = reg.list();
        assert_eq!(list.len(), 2);
    }
}
