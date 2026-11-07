pub mod middleware;
pub mod scoped;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(rename = "value_prefix", alias = "key_prefix")]
    pub key_prefix: String,
    pub description: String,
    pub actions: Vec<String>,
    pub collections: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub autodelete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiKeySummary {
    pub id: u64,
    #[serde(rename = "value_prefix", alias = "key_prefix")]
    pub key_prefix: String,
    pub description: String,
    pub actions: Vec<String>,
    pub collections: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

impl From<&ApiKey> for ApiKeySummary {
    fn from(key: &ApiKey) -> Self {
        Self {
            id: key.id,
            key_prefix: key.key_prefix.clone(),
            description: key.description.clone(),
            actions: key.actions.clone(),
            collections: key.collections.clone(),
            expires_at: key.expires_at,
        }
    }
}

#[derive(Deserialize)]
pub struct CreateKeyRequest {
    pub description: String,
    pub actions: Vec<String>,
    pub collections: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub autodelete: bool,
}

/// Persisted form of a key (includes full value for reload).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedKey {
    pub id: u64,
    pub value: String,
    #[serde(rename = "value_prefix", alias = "key_prefix")]
    pub key_prefix: String,
    pub description: String,
    pub actions: Vec<String>,
    pub collections: Vec<String>,
    pub expires_at: Option<u64>,
    pub autodelete: bool,
}

pub struct KeyStore {
    keys: RwLock<FxHashMap<String, ApiKey>>,
    by_id: RwLock<FxHashMap<u64, String>>,
    next_id: AtomicU64,
    bootstrap_key: String,
}

impl KeyStore {
    pub fn new(bootstrap_key: String) -> Self {
        Self {
            keys: RwLock::new(FxHashMap::default()),
            by_id: RwLock::new(FxHashMap::default()),
            next_id: AtomicU64::new(1),
            bootstrap_key,
        }
    }

    pub fn bootstrap_key(&self) -> &str {
        &self.bootstrap_key
    }

    pub fn create(&self, request: CreateKeyRequest) -> ApiKey {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let value = generate_key_value();
        let key_prefix = value[..4].to_string();

        let key = ApiKey {
            id,
            value: Some(value.clone()),
            key_prefix,
            description: request.description,
            actions: request.actions,
            collections: request.collections,
            expires_at: request.expires_at,
            autodelete: request.autodelete,
        };

        self.keys.write().unwrap_or_else(|e| e.into_inner()).insert(value.clone(), key.clone());
        self.by_id.write().unwrap_or_else(|e| e.into_inner()).insert(id, value);

        key
    }

    pub fn get(&self, value: &str) -> Option<ApiKey> {
        self.keys.read().unwrap_or_else(|e| e.into_inner()).get(value).cloned()
    }

    pub fn get_by_id(&self, id: u64) -> Option<ApiKey> {
        let by_id = self.by_id.read().unwrap_or_else(|e| e.into_inner());
        let value = by_id.get(&id)?;
        let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
        keys.get(value).cloned()
    }

    pub fn get_by_prefix(&self, prefix: &str) -> Option<ApiKey> {
        let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
        keys.values()
            .find(|k| k.key_prefix == prefix)
            .cloned()
    }

    pub fn list(&self) -> Vec<ApiKeySummary> {
        let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
        keys.values().map(ApiKeySummary::from).collect()
    }

    pub fn delete(&self, id: u64) -> bool {
        let mut by_id = self.by_id.write().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = by_id.remove(&id) {
            self.keys.write().unwrap_or_else(|e| e.into_inner()).remove(&value);
            true
        } else {
            false
        }
    }

    /// Check if a key value is the bootstrap key.
    pub fn is_bootstrap(&self, value: &str) -> bool {
        value == self.bootstrap_key
    }

    /// Check if a key permits a given action.
    pub fn key_permits_action(key: &ApiKey, action: &str) -> bool {
        key.actions.iter().any(|a| a == "*" || a == action)
    }

    /// Check if a key permits access to a given collection.
    pub fn key_permits_collection(key: &ApiKey, collection: &str) -> bool {
        key.collections.iter().any(|c| {
            if c == "*" {
                return true;
            }
            if c == collection {
                return true;
            }
            // Regex-like matching: "org_.*" matches "org_foo"
            if c.contains(".*") {
                if let Ok(re) = regex::Regex::new(&format!("^{c}$")) {
                    return re.is_match(collection);
                }
            }
            false
        })
    }

    /// Check if a key has expired.
    pub fn key_is_expired(key: &ApiKey) -> bool {
        if let Some(expires_at) = key.expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            now >= expires_at
        } else {
            false
        }
    }

    /// Load persisted keys (called on startup).
    pub fn load(&self, keys: Vec<PersistedKey>) {
        let mut key_map = self.keys.write().unwrap_or_else(|e| e.into_inner());
        let mut id_map = self.by_id.write().unwrap_or_else(|e| e.into_inner());
        let mut max_id = 0u64;

        for pk in keys {
            max_id = max_id.max(pk.id);
            let api_key = ApiKey {
                id: pk.id,
                value: Some(pk.value.clone()),
                key_prefix: pk.key_prefix,
                description: pk.description,
                actions: pk.actions,
                collections: pk.collections,
                expires_at: pk.expires_at,
                autodelete: pk.autodelete,
            };
            id_map.insert(pk.id, pk.value.clone());
            key_map.insert(pk.value, api_key);
        }

        self.next_id.store(max_id + 1, Ordering::Relaxed);
    }

    /// Export all keys for persistence.
    pub fn export(&self) -> Vec<PersistedKey> {
        let keys = self.keys.read().unwrap_or_else(|e| e.into_inner());
        keys.values()
            .map(|k| PersistedKey {
                id: k.id,
                value: k.value.clone().unwrap_or_default(),
                key_prefix: k.key_prefix.clone(),
                description: k.description.clone(),
                actions: k.actions.clone(),
                collections: k.collections.clone(),
                expires_at: k.expires_at,
                autodelete: k.autodelete,
            })
            .collect()
    }
}

fn generate_key_value() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Generate a random-enough key using timestamp + thread-local state
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let thread_id = std::thread::current().id();
    let hash1 = rustc_hash::FxHasher::default();
    use std::hash::{Hash, Hasher};
    let mut hasher = hash1;
    nanos.hash(&mut hasher);
    thread_id.hash(&mut hasher);
    let h1 = hasher.finish();

    let mut hasher2 = rustc_hash::FxHasher::default();
    h1.hash(&mut hasher2);
    nanos.wrapping_mul(6364136223846793005).hash(&mut hasher2);
    let h2 = hasher2.finish();

    let mut hasher3 = rustc_hash::FxHasher::default();
    h2.hash(&mut hasher3);
    nanos.wrapping_add(h1 as u128).hash(&mut hasher3);
    let h3 = hasher3.finish();

    let mut hasher4 = rustc_hash::FxHasher::default();
    h3.hash(&mut hasher4);
    h1.wrapping_add(h2).hash(&mut hasher4);
    let h4 = hasher4.finish();

    format!("{h1:016x}{h2:016x}{h3:016x}{h4:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn create_and_lookup() {
        let store = KeyStore::new("bootstrap123".into());
        let key = store.create(CreateKeyRequest {
            description: "test key".into(),
            actions: vec!["documents:search".into()],
            collections: vec!["products".into()],
            expires_at: None,
            autodelete: false,
        });

        assert_eq!(key.id, 1);
        assert!(key.value.is_some());
        let value = key.value.as_ref().unwrap();
        assert_eq!(key.key_prefix, value[..4]);

        // Lookup by value
        let found = store.get(value).unwrap();
        assert_eq!(found.id, 1);

        // Lookup by id
        let found = store.get_by_id(1).unwrap();
        assert_eq!(found.description, "test key");

        // Lookup by prefix
        let found = store.get_by_prefix(&key.key_prefix).unwrap();
        assert_eq!(found.id, 1);
    }

    #[test]
    fn list_and_delete() {
        let store = KeyStore::new("boot".into());
        store.create(CreateKeyRequest {
            description: "key1".into(),
            actions: vec!["*".into()],
            collections: vec!["*".into()],
            expires_at: None,
            autodelete: false,
        });
        store.create(CreateKeyRequest {
            description: "key2".into(),
            actions: vec!["documents:search".into()],
            collections: vec!["products".into()],
            expires_at: None,
            autodelete: false,
        });

        assert_eq!(store.list().len(), 2);
        assert!(store.delete(1));
        assert_eq!(store.list().len(), 1);
        assert!(store.get_by_id(1).is_none());
        assert!(!store.delete(999));
    }

    #[test]
    fn action_matching() {
        let key = ApiKey {
            id: 1,
            value: None,
            key_prefix: "test".into(),
            description: "".into(),
            actions: vec!["documents:search".into()],
            collections: vec!["*".into()],
            expires_at: None,
            autodelete: false,
        };

        assert!(KeyStore::key_permits_action(&key, "documents:search"));
        assert!(!KeyStore::key_permits_action(&key, "collections:create"));

        let admin = ApiKey {
            actions: vec!["*".into()],
            ..key.clone()
        };
        assert!(KeyStore::key_permits_action(&admin, "anything"));
    }

    #[test]
    fn collection_matching() {
        let key = ApiKey {
            id: 1,
            value: None,
            key_prefix: "test".into(),
            description: "".into(),
            actions: vec!["*".into()],
            collections: vec!["products".into()],
            expires_at: None,
            autodelete: false,
        };

        assert!(KeyStore::key_permits_collection(&key, "products"));
        assert!(!KeyStore::key_permits_collection(&key, "orders"));

        let wildcard = ApiKey {
            collections: vec!["*".into()],
            ..key.clone()
        };
        assert!(KeyStore::key_permits_collection(&wildcard, "anything"));

        let regex_key = ApiKey {
            collections: vec!["org_.*".into()],
            ..key.clone()
        };
        assert!(KeyStore::key_permits_collection(&regex_key, "org_foo"));
        assert!(KeyStore::key_permits_collection(&regex_key, "org_bar"));
        assert!(!KeyStore::key_permits_collection(&regex_key, "other"));
    }

    #[test]
    fn expiry_check() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;

        let not_expired = ApiKey {
            id: 1,
            value: None,
            key_prefix: "test".into(),
            description: "".into(),
            actions: vec![],
            collections: vec![],
            expires_at: Some(future),
            autodelete: false,
        };
        assert!(!KeyStore::key_is_expired(&not_expired));

        let expired = ApiKey {
            expires_at: Some(1),
            ..not_expired.clone()
        };
        assert!(KeyStore::key_is_expired(&expired));

        let no_expiry = ApiKey {
            expires_at: None,
            ..not_expired
        };
        assert!(!KeyStore::key_is_expired(&no_expiry));
    }

    #[test]
    fn load_and_export() {
        let store = KeyStore::new("boot".into());
        store.create(CreateKeyRequest {
            description: "key1".into(),
            actions: vec!["*".into()],
            collections: vec!["*".into()],
            expires_at: None,
            autodelete: false,
        });

        let exported = store.export();
        assert_eq!(exported.len(), 1);

        let store2 = KeyStore::new("boot".into());
        store2.load(exported.clone());
        assert_eq!(store2.list().len(), 1);
        assert_eq!(store2.get_by_id(1).unwrap().description, "key1");

        // Next ID should be 2 after loading key with id=1
        let key2 = store2.create(CreateKeyRequest {
            description: "key2".into(),
            actions: vec!["*".into()],
            collections: vec!["*".into()],
            expires_at: None,
            autodelete: false,
        });
        assert_eq!(key2.id, 2);
    }

    #[test]
    fn bootstrap_check() {
        let store = KeyStore::new("my-secret-key".into());
        assert!(store.is_bootstrap("my-secret-key"));
        assert!(!store.is_bootstrap("other-key"));
    }

    #[test]
    fn rwlock_poison_recovery() {
        let store = Arc::new(KeyStore::new("boot".into()));
        let store2 = Arc::clone(&store);

        // Poison the lock by panicking while holding a write guard
        let handle = std::thread::spawn(move || {
            let _guard = store2.keys.write().unwrap();
            panic!("intentional panic to poison lock");
        });
        let _ = handle.join(); // join the panicked thread

        // Subsequent operations should still work despite poisoned lock
        let key = store.create(CreateKeyRequest {
            description: "after poison".into(),
            actions: vec!["*".into()],
            collections: vec!["*".into()],
            expires_at: None,
            autodelete: false,
        });
        assert!(key.value.is_some());
        assert_eq!(store.list().len(), 1);
    }
}
