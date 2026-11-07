use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::ApiKey;

type HmacSha256 = Hmac<Sha256>;

/// A scoped API key's embedded search parameters (Typesense's `embedded_params`): filter_by is
/// ANDed with a search's, include_fields and exclude_fields restrict a search's, limit_hits caps
/// it, and every other parameter (`other`) overrides the search's.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ScopedKeyParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_fields: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_fields: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_hits: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// The other embedded parameters.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug)]
pub struct DecodedScopedKey {
    pub parent: ApiKey,
    pub params: ScopedKeyParams,
}

/// Resolved key after authentication — either a direct key or a scoped key.
#[derive(Clone, Debug)]
pub enum ResolvedKey {
    Direct(ApiKey),
    Bootstrap,
    Scoped {
        parent: ApiKey,
        params: ScopedKeyParams,
    },
}

impl ResolvedKey {
    pub fn actions(&self) -> &[String] {
        match self {
            Self::Direct(key) => &key.actions,
            Self::Bootstrap => &[],
            Self::Scoped { parent, .. } => &parent.actions,
        }
    }

    pub fn collections(&self) -> &[String] {
        match self {
            Self::Direct(key) => &key.collections,
            Self::Bootstrap => &[],
            Self::Scoped { parent, .. } => &parent.collections,
        }
    }

    pub fn is_admin(&self) -> bool {
        match self {
            Self::Bootstrap => true,
            Self::Direct(key) => key.actions.iter().any(|a| a == "*"),
            Self::Scoped { parent, .. } => parent.actions.iter().any(|a| a == "*"),
        }
    }

    pub fn scoped_params(&self) -> Option<&ScopedKeyParams> {
        match self {
            Self::Scoped { params, .. } => Some(params),
            _ => None,
        }
    }
}

/// Generate a scoped search key from a parent key value.
pub fn generate_scoped_key(parent_key_value: &str, params: &ScopedKeyParams) -> String {
    let engine = base64::engine::general_purpose::STANDARD;

    let params_json = serde_json::to_string(params).unwrap();
    let mut mac =
        HmacSha256::new_from_slice(parent_key_value.as_bytes()).expect("HMAC key length");
    mac.update(params_json.as_bytes());
    let digest = mac.finalize().into_bytes();
    let digest_b64 = engine.encode(digest);

    let prefix = &parent_key_value[..4.min(parent_key_value.len())];
    let raw = format!("{digest_b64}{prefix}{params_json}");
    engine.encode(raw.as_bytes())
}

/// Try to decode a scoped key. Returns None if not a valid scoped key.
pub fn try_decode_scoped_key(
    key: &str,
    key_store: &super::KeyStore,
) -> Option<DecodedScopedKey> {
    let engine = base64::engine::general_purpose::STANDARD;

    // Step 1: Base64 decode
    let decoded = engine.decode(key).ok()?;
    let decoded_str = String::from_utf8(decoded).ok()?;

    // HMAC-SHA256 base64 = 44 chars, prefix = 4 chars, rest = JSON
    if decoded_str.len() < 49 {
        return None;
    }

    let digest_b64 = &decoded_str[..44];
    let parent_prefix = &decoded_str[44..48];
    let params_json = &decoded_str[48..];

    // Step 2: Look up parent key by prefix
    let parent = key_store.get_by_prefix(parent_prefix)?;
    let parent_value = parent.value.as_ref()?;

    // Step 3: Recompute HMAC to verify
    let mut mac =
        HmacSha256::new_from_slice(parent_value.as_bytes()).expect("HMAC key length");
    mac.update(params_json.as_bytes());
    let expected_digest = mac.finalize().into_bytes();
    let expected_b64 = engine.encode(expected_digest);

    if digest_b64 != expected_b64 {
        return None;
    }

    // Step 4: Parse params
    let params: ScopedKeyParams = serde_json::from_str(params_json).ok()?;

    Some(DecodedScopedKey { parent, params })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{CreateKeyRequest, KeyStore};

    #[test]
    fn scoped_key_roundtrip() {
        let store = KeyStore::new("bootstrap".into());
        let parent = store.create(CreateKeyRequest {
            description: "parent".into(),
            actions: vec!["documents:search".into()],
            collections: vec!["products".into()],
            expires_at: None,
            autodelete: false,
        });

        let parent_value = parent.value.as_ref().unwrap();
        let params = ScopedKeyParams {
            filter_by: Some("company_id:123".into()),
            exclude_fields: None,
            include_fields: None,
            limit_hits: Some(100),
            expires_at: None,
            other: Default::default(),
        };

        let scoped = generate_scoped_key(parent_value, &params);
        let decoded = try_decode_scoped_key(&scoped, &store).unwrap();

        assert_eq!(decoded.parent.id, parent.id);
        assert_eq!(decoded.params.filter_by.as_deref(), Some("company_id:123"));
        assert_eq!(decoded.params.limit_hits, Some(100));
    }

    #[test]
    fn scoped_key_wrong_parent_rejected() {
        let store = KeyStore::new("bootstrap".into());
        let parent = store.create(CreateKeyRequest {
            description: "parent".into(),
            actions: vec!["*".into()],
            collections: vec!["*".into()],
            expires_at: None,
            autodelete: false,
        });

        let params = ScopedKeyParams {
            filter_by: Some("x:1".into()),
            exclude_fields: None,
            include_fields: None,
            limit_hits: None,
            expires_at: None,
            other: Default::default(),
        };

        // Generate with a different key
        let scoped = generate_scoped_key("totally_different_key!", &params);
        assert!(try_decode_scoped_key(&scoped, &store).is_none());

        // Valid generation still works
        let valid = generate_scoped_key(parent.value.as_ref().unwrap(), &params);
        assert!(try_decode_scoped_key(&valid, &store).is_some());
    }

    #[test]
    fn scoped_key_tampered_rejected() {
        let store = KeyStore::new("bootstrap".into());
        let parent = store.create(CreateKeyRequest {
            description: "parent".into(),
            actions: vec!["*".into()],
            collections: vec!["*".into()],
            expires_at: None,
            autodelete: false,
        });

        let params = ScopedKeyParams {
            filter_by: Some("tenant:abc".into()),
            exclude_fields: None,
            include_fields: None,
            limit_hits: None,
            expires_at: None,
            other: Default::default(),
        };

        let scoped = generate_scoped_key(parent.value.as_ref().unwrap(), &params);

        // Tamper with the scoped key
        let mut tampered = scoped.clone();
        tampered.push('A');
        assert!(try_decode_scoped_key(&tampered, &store).is_none());

        // Non-base64 is also rejected
        assert!(try_decode_scoped_key("not-a-scoped-key", &store).is_none());
    }

    #[test]
    fn resolved_key_helpers() {
        let key = ApiKey {
            id: 1,
            value: None,
            key_prefix: "test".into(),
            description: "".into(),
            actions: vec!["documents:search".into()],
            collections: vec!["products".into()],
            expires_at: None,
            autodelete: false,
        };

        let direct = ResolvedKey::Direct(key.clone());
        assert!(!direct.is_admin());
        assert!(direct.scoped_params().is_none());

        let bootstrap = ResolvedKey::Bootstrap;
        assert!(bootstrap.is_admin());

        let admin_key = ApiKey {
            actions: vec!["*".into()],
            ..key.clone()
        };
        let admin = ResolvedKey::Direct(admin_key);
        assert!(admin.is_admin());

        let scoped = ResolvedKey::Scoped {
            parent: key,
            params: ScopedKeyParams {
                filter_by: Some("x:1".into()),
                exclude_fields: None,
                include_fields: None,
                limit_hits: None,
                expires_at: None,
                other: Default::default(),
            },
        };
        assert!(scoped.scoped_params().is_some());
        assert_eq!(
            scoped.scoped_params().unwrap().filter_by.as_deref(),
            Some("x:1")
        );
    }
}
