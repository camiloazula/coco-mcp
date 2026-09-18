//! Secret storage: the OS keyring in the app, memory in tests.

use std::collections::HashMap;
use std::sync::Mutex;

/// A fresh keyring entry name for one server's credential.
///
/// Minted here so the add-server form, the app's config import and the CLI's
/// cannot disagree about the shape, and so an entry is never shared between
/// two servers.
pub fn new_keyring_id() -> String {
    format!("server:{}", uuid::Uuid::now_v7())
}

/// Where secrets live. Keys are the `keyring_id`s stored in `ServerSpec`.
pub trait SecretStore: Send + Sync {
    /// Read a secret.
    fn get(&self, key: &str) -> std::result::Result<Option<String>, String>;
    /// Write a secret.
    fn set(&self, key: &str, value: &str) -> std::result::Result<(), String>;
    /// Remove a secret. Missing keys are not an error.
    fn delete(&self, key: &str) -> std::result::Result<(), String>;
}

/// The platform keyring (macOS Keychain, Windows Credential Manager,
/// Secret Service on Linux) through the `keyring` crate.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// Secrets are stored under `service` with the key as the account name.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> std::result::Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, key).map_err(|e| e.to_string())
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new("coco-mcp")
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &str) -> std::result::Result<Option<String>, String> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, key: &str, value: &str) -> std::result::Result<(), String> {
        self.entry(key)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, key: &str) -> std::result::Result<(), String> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// In-memory store for tests and demo data.
#[derive(Debug, Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored secrets (tests).
    pub fn len(&self) -> usize {
        self.map.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> std::result::Result<Option<String>, String> {
        Ok(self
            .map
            .lock()
            .map_err(|e| e.to_string())?
            .get(key)
            .cloned())
    }

    fn set(&self, key: &str, value: &str) -> std::result::Result<(), String> {
        self.map
            .lock()
            .map_err(|e| e.to_string())?
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> std::result::Result<(), String> {
        self.map.lock().map_err(|e| e.to_string())?.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::new();
        assert!(store.is_empty());
        assert_eq!(store.get("a").unwrap(), None);
        store.set("a", "1").unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("1"));
        store.delete("a").unwrap();
        store.delete("a").unwrap();
        assert!(store.is_empty());
    }
}
