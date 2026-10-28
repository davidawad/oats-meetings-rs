//! Secrets (API keys) come from the environment or the OS keychain, never from the TOML config:
//! config holds only the NAME of a secret (`llm.api_key_name`), and `secrets.source` says where to
//! look it up. A new key in a config file would be an unknown key, so a key cannot even be typed
//! into the config by mistake.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Mutex;

use crate::config::{ConfigError, Result};

pub trait SecretStore: Send + Sync + Debug {
    /// `Ok(None)` when the secret is not set; errors only for a broken store.
    fn get(&self, name: &str) -> Result<Option<String>>;
}

/// Process environment variables.
#[derive(Debug, Default)]
pub struct EnvSecrets;

impl SecretStore for EnvSecrets {
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(std::env::var(name).ok().filter(|v| !v.is_empty()))
    }
}

/// The OS keychain (macOS Keychain, Windows Credential Manager, Linux kernel keyring).
#[derive(Debug)]
pub struct KeychainSecrets {
    pub service: String,
}

impl SecretStore for KeychainSecrets {
    fn get(&self, name: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(&self.service, name)
            .map_err(|e| ConfigError::new("secret_store_error", e.to_string()))?;
        match entry.get_password() {
            Ok(p) if !p.is_empty() => Ok(Some(p)),
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ConfigError::new("secret_store_error", e.to_string())),
        }
    }
}

/// In-memory store for tests and embedders.
#[derive(Debug, Default)]
pub struct MemSecrets(pub Mutex<BTreeMap<String, String>>);

impl MemSecrets {
    pub fn with(pairs: &[(&str, &str)]) -> MemSecrets {
        MemSecrets(Mutex::new(
            pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
        ))
    }
}

impl SecretStore for MemSecrets {
    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.0.lock().expect("secrets").get(name).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_store_reads_and_ignores_empty() {
        std::env::set_var("OATS_TEST_SECRET_A", "abc");
        std::env::set_var("OATS_TEST_SECRET_EMPTY", "");
        assert_eq!(EnvSecrets.get("OATS_TEST_SECRET_A").unwrap(), Some("abc".into()));
        assert_eq!(EnvSecrets.get("OATS_TEST_SECRET_EMPTY").unwrap(), None);
        assert_eq!(EnvSecrets.get("OATS_TEST_SECRET_UNSET_XYZ").unwrap(), None);
    }

    #[test]
    fn mem_store() {
        let s = MemSecrets::with(&[("k", "v")]);
        assert_eq!(s.get("k").unwrap(), Some("v".into()));
        assert_eq!(s.get("nope").unwrap(), None);
    }

    #[test]
    fn keychain_store_never_panics_and_unset_is_none_or_a_clear_error() {
        // A container may have no usable keyring; either answer is acceptable, a panic is not.
        let s = KeychainSecrets { service: "oats-meetings-rs-test-nonexistent".into() };
        match s.get("definitely-not-set") {
            Ok(v) => assert_eq!(v, None),
            Err(e) => assert_eq!(e.code, "secret_store_error"),
        }
    }
}
