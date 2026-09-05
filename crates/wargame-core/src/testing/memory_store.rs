//! An in-memory [`CredentialStore`], for tests that need one without
//! touching disk or the Keychain.

use std::collections::BTreeMap;
use std::sync::Mutex;

use secrecy::ExposeSecret;

use crate::error::Result;
use crate::ports::{CredentialStore, Secret};

#[derive(Default)]
pub struct MemoryStore {
    values: Mutex<BTreeMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<Secret<String>>> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(key)
            .map(|v| Secret::new(Box::new(v.clone()))))
    }

    fn set(&self, key: &str, value: &Secret<String>) -> Result<()> {
        self.values
            .lock()
            .unwrap()
            .insert(key.to_string(), value.expose_secret().to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.values.lock().unwrap().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_value() {
        let store = MemoryStore::new();
        assert!(store.get("k").unwrap().is_none());
        store
            .set("k", &Secret::new(Box::new("v".to_string())))
            .unwrap();
        assert_eq!(store.get("k").unwrap().unwrap().expose_secret(), "v");
        store.delete("k").unwrap();
        assert!(store.get("k").unwrap().is_none());
    }
}
