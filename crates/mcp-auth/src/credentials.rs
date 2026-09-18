//! rmcp `CredentialStore` over a [`SecretStore`]: the whole credential set
//! (client id, tokens, scopes, issuer) is one JSON secret under the server's
//! keyring id. Keyring calls can block on a system prompt, so they run on
//! the blocking pool.

use std::sync::Arc;

use async_trait::async_trait;
use rmcp::transport::auth::{AuthError, CredentialStore, StoredCredentials};

use crate::SecretStore;

/// Persists OAuth credentials for one server.
#[derive(Clone)]
pub struct KeyringCredentialStore {
    secrets: Arc<dyn SecretStore>,
    key: String,
}

impl std::fmt::Debug for KeyringCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringCredentialStore")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl KeyringCredentialStore {
    /// Store under `key` (the spec's `keyring_id`).
    pub fn new(secrets: Arc<dyn SecretStore>, key: impl Into<String>) -> Self {
        Self {
            secrets,
            key: key.into(),
        }
    }

    async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&dyn SecretStore, &str) -> std::result::Result<R, String> + Send + 'static,
    ) -> Result<R, AuthError> {
        let secrets = self.secrets.clone();
        let key = self.key.clone();
        tokio::task::spawn_blocking(move || f(secrets.as_ref(), &key))
            .await
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))?
            .map_err(AuthError::CredentialStoreError)
    }
}

#[async_trait]
impl CredentialStore for KeyringCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let raw = self.blocking(|s, k| s.get(k)).await?;
        raw.map(|json| {
            serde_json::from_str(&json).map_err(|e| AuthError::CredentialStoreError(e.to_string()))
        })
        .transpose()
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let json = serde_json::to_string(&credentials)
            .map_err(|e| AuthError::CredentialStoreError(e.to_string()))?;
        self.blocking(move |s, k| s.set(k, &json)).await
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.blocking(|s, k| s.delete(k)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStore;

    #[tokio::test]
    async fn round_trips_through_the_secret_store() {
        let secrets = Arc::new(MemoryStore::new());
        let store = KeyringCredentialStore::new(secrets.clone(), "srv-1");
        assert!(store.load().await.unwrap().is_none());
        store
            .save(StoredCredentials::new(
                "client-1".into(),
                None,
                vec!["mcp".into()],
                None,
            ))
            .await
            .unwrap();
        let loaded = store.load().await.unwrap().unwrap();
        assert_eq!(loaded.client_id, "client-1");
        assert_eq!(loaded.granted_scopes, ["mcp"]);
        assert_eq!(secrets.len(), 1);
        store.clear().await.unwrap();
        assert!(secrets.is_empty());
    }
}
