//! Provider keys. Stored in Windows Credential Manager, never in files or logs.

use std::collections::HashMap;
use std::fmt;

use parking_lot::Mutex;

pub const SERVICE: &str = "Mika";

/// A secret value. `Debug` prints `Secret(***)`; there is no `Display` or `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret store unavailable: {0}")]
    Backend(String),
}

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Secret>, SecretError>;
    /// An empty value deletes the entry.
    fn set(&self, key: &str, value: &Secret) -> Result<(), SecretError>;
    /// A missing entry is not an error.
    fn delete(&self, key: &str) -> Result<(), SecretError>;
}

#[derive(Default)]
pub struct MemorySecretStore {
    map: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self.map.lock().get(key).cloned().map(Secret))
    }

    fn set(&self, key: &str, value: &Secret) -> Result<(), SecretError> {
        if value.is_empty() {
            return self.delete(key);
        }
        self.map.lock().insert(key.into(), value.0.clone());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.map.lock().remove(key);
        Ok(())
    }
}

/// Windows Credential Manager, service `Mika`.
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(key: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, key).map_err(backend)
    }
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

// keyring errors describe the operation, not the stored value.
fn backend(e: keyring::Error) -> SecretError {
    SecretError::Backend(e.to_string())
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        match Self::entry(key)?.get_password() {
            Ok(v) => Ok(Some(Secret(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    fn set(&self, key: &str, value: &Secret) -> Result<(), SecretError> {
        if value.is_empty() {
            return self.delete(key);
        }
        Self::entry(key)?.set_password(&value.0).map_err(backend)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(backend(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("sk-12345");
        assert_eq!(format!("{:?}", s), "Secret(***)");
        assert_eq!(format!("{:?}", Some(s.clone())), "Some(Secret(***))");
        assert_eq!(s.expose(), "sk-12345");
    }

    fn check_store(store: &dyn SecretStore, key: &str) {
        assert_eq!(store.get(key).unwrap(), None);
        store.delete(key).unwrap();
        store.set(key, &Secret::new("one")).unwrap();
        assert_eq!(store.get(key).unwrap(), Some(Secret::new("one")));
        store.set(key, &Secret::new("two")).unwrap();
        assert_eq!(store.get(key).unwrap(), Some(Secret::new("two")));
        store.set(key, &Secret::new("")).unwrap();
        assert_eq!(store.get(key).unwrap(), None);
        store.set(key, &Secret::new("three")).unwrap();
        store.delete(key).unwrap();
        assert_eq!(store.get(key).unwrap(), None);
    }

    #[test]
    fn memory_store_contract() {
        check_store(&MemorySecretStore::default(), "llm.api_key");
    }

    #[test]
    #[ignore = "writes to Windows Credential Manager"]
    fn keyring_store_contract() {
        let key = format!("test.contract.{}", std::process::id());
        check_store(&KeyringSecretStore::new(), &key);
    }
}
