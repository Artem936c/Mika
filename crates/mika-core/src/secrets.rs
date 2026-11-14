//! Provider keys. Stored in Windows Credential Manager, never in files or logs.

use std::collections::HashMap;
use std::fmt;

use parking_lot::Mutex;

pub const SERVICE: &str = "Mika";

/// A secret value. `Debug` prints `Secret(***)`; there is no `Display` or `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The journal remembers the value, so that it never reaches a record
    /// even when an error text carries it.
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        crate::journal::remember_secret(&value);
        Self(value)
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
        Ok(self.map.lock().get(key).cloned().map(Secret::new))
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

/// How often a failing read of one entry is written to the journal.
const READ_NOTE_EVERY: std::time::Duration = std::time::Duration::from_secs(600);

/// When a failed read of an entry was last written to the journal.
static READ_NOTES: Mutex<Vec<(String, std::time::Instant)>> = Mutex::new(Vec::new());

/// Whether a failed read of `key` is written now: the first one, and then
/// one in ten minutes.
fn note_due(key: &str) -> bool {
    let mut notes = READ_NOTES.lock();
    let now = std::time::Instant::now();
    match notes.iter_mut().find(|(entry, _)| entry == key) {
        Some((_, at)) if now.duration_since(*at) < READ_NOTE_EVERY => false,
        Some((_, at)) => {
            *at = now;
            true
        }
        None => {
            notes.push((key.to_string(), now));
            true
        }
    }
}

// keyring errors describe the operation, not the stored value.
fn backend(e: keyring::Error) -> SecretError {
    SecretError::Backend(e.to_string())
}

// The journal gets the name of an entry and what the store said about the
// operation, never a value. Most callers treat a failed read as "no key", so
// without these records a broken store looks like a wrong key.
impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        let read = Self::entry(key).and_then(|entry| match entry.get_password() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(backend(e)),
        });
        if let Err(e) = &read {
            // Some entries are read every half a minute: a store that keeps
            // failing is told once in a while, not at every read.
            if note_due(key) {
                log::warn!(target: "app", "a key cannot be read from the credential store: entry={key} error={e}");
            }
        }
        read
    }

    fn set(&self, key: &str, value: &Secret) -> Result<(), SecretError> {
        if value.is_empty() {
            return self.delete(key);
        }
        let written =
            Self::entry(key).and_then(|entry| entry.set_password(&value.0).map_err(backend));
        match &written {
            Ok(()) => {
                log::info!(target: "app", "a key was written to the credential store: entry={key}")
            }
            Err(e) => {
                log::error!(target: "app", "a key cannot be written to the credential store: entry={key} error={e}")
            }
        }
        written
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let deleted = Self::entry(key).and_then(|entry| match entry.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(backend(e)),
        });
        match &deleted {
            Ok(true) => {
                log::info!(target: "app", "a key was removed from the credential store: entry={key}")
            }
            Ok(false) => {}
            Err(e) => {
                log::warn!(target: "app", "a key cannot be removed from the credential store: entry={key} error={e}")
            }
        }
        deleted.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_read_is_noted_once_in_a_while() {
        assert!(note_due("test.entry.one"));
        assert!(!note_due("test.entry.one"));
        // Another entry has its own turn.
        assert!(note_due("test.entry.two"));
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("sk-12345");
        assert_eq!(format!("{:?}", s), "Secret(***)");
        assert_eq!(format!("{:?}", Some(s.clone())), "Some(Secret(***))");
        assert_eq!(s.expose(), "sk-12345");
    }

    #[test]
    fn a_new_secret_is_remembered_by_the_journal() {
        let s = Secret::new("zq-secret-of-the-store-01");
        assert_eq!(
            crate::journal::scrub("401 for zq-secret-of-the-store-01"),
            "401 for ***"
        );
        // A value that comes out of a store is remembered too.
        let store = MemorySecretStore::default();
        store
            .map
            .lock()
            .insert("k".into(), "zq-secret-read-back-02".into());
        let read = store.get("k").unwrap().unwrap();
        assert_eq!(read.expose(), "zq-secret-read-back-02");
        assert_eq!(crate::journal::scrub("zq-secret-read-back-02"), "***");
        assert_eq!(s.expose(), "zq-secret-of-the-store-01");
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
