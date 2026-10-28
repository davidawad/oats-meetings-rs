//! Secrets are never written in the config. A config value that needs one holds a REFERENCE,
//! `secret:<namespace>/<name>` (for example `secret:ics/work`), and the secret itself lives in a
//! store chosen by `secrets.backend`:
//!
//! - `env`: the environment variable `<secrets.env_prefix><KEY>` with the key upper-cased and every
//!   non-alphanumeric character turned into `_` (`ics/work` -> `OATS_SECRET_ICS_WORK`). Read only.
//! - `keyring`: the OS keychain (macOS Keychain, Windows Credential Manager, the Linux Secret
//!   Service) through the `keyring` crate, under service `secrets.keyring_service`.
//! - `env-then-keyring`: the environment first, then the keychain; writes go to the keychain.
//!
//! The trait is generic: other subsystems (API keys for notes generation, cloud STT) take a
//! `&dyn SecretStore` and call `resolve`. Values are never logged, echoed or returned by an action.
//!
//! Stable error codes: `secret_plaintext` (a literal where a reference is required),
//! `secret_not_found`, `secret_read_only`, `secret_backend_unavailable`, `invalid_argument`.

use serde_json::{json, Value};

use crate::action::Action;
use crate::config::typed::{Secrets, SecretsBackend};
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

pub const REF_PREFIX: &str = "secret:";

pub trait SecretStore: Send + Sync {
    fn name(&self) -> &'static str;
    /// `Ok(None)` when the store works but has no such secret.
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    /// `Ok(true)` if something was deleted.
    fn delete(&self, key: &str) -> Result<bool>;
}

/// `namespace/name`, letters, digits and `_ - . /`, no leading slash, no `..`.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 128
        && k.contains('/')
        && !k.starts_with('/')
        && !k.ends_with('/')
        && !k.contains("..")
        && k.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
}

fn check_key(k: &str) -> Result<()> {
    if valid_key(k) {
        Ok(())
    } else {
        Err(ConfigError::new(
            "invalid_argument",
            format!("invalid secret key {k:?} (want namespace/name, letters, digits, '_', '-', '.', '/')"),
        ))
    }
}

/// The key inside a `secret:<key>` reference, if `s` is one.
pub fn parse_ref(s: &str) -> Option<&str> {
    let k = s.trim().strip_prefix(REF_PREFIX)?;
    valid_key(k).then_some(k)
}

/// A reference where a literal is not allowed: plaintext is `secret_plaintext`.
pub fn require_ref(s: &str, what: &str) -> Result<String> {
    parse_ref(s).map(str::to_string).ok_or_else(|| {
        ConfigError::new(
            "secret_plaintext",
            format!("{what} must be a secret reference like secret:ics/<name>, not the secret itself; store it with `secret-set`"),
        )
    })
}

/// Look a reference up in `store`.
pub fn resolve(store: &dyn SecretStore, reference: &str) -> Result<String> {
    let key = require_ref(reference, "the value")?;
    store.get(&key)?.ok_or_else(|| {
        ConfigError::new("secret_not_found", format!("no secret '{key}' in the {} store", store.name()))
    })
}

fn unavailable(what: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("secret_backend_unavailable", what.to_string())
}

// ---- backends ------------------------------------------------------------------------------

pub struct EnvStore {
    prefix: String,
}

impl EnvStore {
    pub fn new(prefix: &str) -> EnvStore {
        EnvStore { prefix: prefix.to_string() }
    }

    pub fn var_name(&self, key: &str) -> String {
        let tail: String = key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
            .collect();
        format!("{}{tail}", self.prefix)
    }
}

impl SecretStore for EnvStore {
    fn name(&self) -> &'static str {
        "env"
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        check_key(key)?;
        Ok(std::env::var(self.var_name(key)).ok().filter(|v| !v.is_empty()))
    }
    fn set(&self, key: &str, _value: &str) -> Result<()> {
        check_key(key)?;
        Err(ConfigError::new(
            "secret_read_only",
            format!("the env store is read only; export {} instead", self.var_name(key)),
        ))
    }
    fn delete(&self, key: &str) -> Result<bool> {
        self.set(key, "").map(|_| false)
    }
}

pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new(service: &str) -> KeyringStore {
        KeyringStore { service: service.to_string() }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry> {
        check_key(key)?;
        keyring::Entry::new(&self.service, key).map_err(unavailable)
    }
}

impl SecretStore for KeyringStore {
    fn name(&self) -> &'static str {
        "keyring"
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        match self.entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(unavailable(e)),
        }
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.entry(key)?.set_password(value).map_err(unavailable)
    }
    fn delete(&self, key: &str) -> Result<bool> {
        match self.entry(key)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(unavailable(e)),
        }
    }
}

/// Stores tried in order: the first hit wins for `get`; `set` and `delete` go to the first store
/// that accepts them.
pub struct Chain(pub Vec<Box<dyn SecretStore>>);

impl SecretStore for Chain {
    fn name(&self) -> &'static str {
        "env-then-keyring"
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        let mut last_err = None;
        for s in &self.0 {
            match s.get(key) {
                Ok(Some(v)) => return Ok(Some(v)),
                Ok(None) => {}
                Err(e) => last_err = Some(e),
            }
        }
        match last_err {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        let mut last_err = None;
        for s in &self.0 {
            match s.set(key, value) {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| unavailable("no secret store configured")))
    }
    fn delete(&self, key: &str) -> Result<bool> {
        let mut last_err = None;
        for s in &self.0 {
            match s.delete(key) {
                Ok(b) => return Ok(b),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| unavailable("no secret store configured")))
    }
}

/// In memory; for tests of anything that takes a `SecretStore`.
#[derive(Default)]
pub struct MemStore(std::sync::Mutex<std::collections::BTreeMap<String, String>>);

impl SecretStore for MemStore {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn get(&self, key: &str) -> Result<Option<String>> {
        check_key(key)?;
        Ok(self.0.lock().expect("mem store").get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        check_key(key)?;
        self.0.lock().expect("mem store").insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<bool> {
        check_key(key)?;
        Ok(self.0.lock().expect("mem store").remove(key).is_some())
    }
}

/// The store `secrets.*` selects.
pub fn store_for(c: &Secrets) -> Box<dyn SecretStore> {
    let env = || Box::new(EnvStore::new(&c.env_prefix)) as Box<dyn SecretStore>;
    let ring = || Box::new(KeyringStore::new(&c.keyring_service)) as Box<dyn SecretStore>;
    match c.backend {
        SecretsBackend::Env => env(),
        SecretsBackend::Keyring => ring(),
        SecretsBackend::EnvThenKeyring => Box::new(Chain(vec![env(), ring()])),
    }
}

// ---- actions -------------------------------------------------------------------------------

pub fn dispatch(engine: &Engine, a: &Action) -> Result<Value> {
    let cfg = engine.resolved().typed()?.secrets;
    let store = store_for(&cfg);
    match a {
        Action::SecretSet { key, value } => {
            check_key(key)?;
            if value.is_empty() {
                return Err(ConfigError::new("invalid_argument", "a secret cannot be empty"));
            }
            store.set(key, value)?;
            Ok(json!({"key": key, "stored": true, "backend": store.name()}))
        }
        Action::SecretDelete { key } => {
            let deleted = store.delete(key)?;
            Ok(json!({"key": key, "deleted": deleted, "backend": store.name()}))
        }
        Action::SecretHas { key } => {
            let present = store.get(key)?.is_some();
            Ok(json!({"key": key, "present": present, "backend": store.name()}))
        }
        _ => Err(ConfigError::new("bad_request", "not a secret action")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_refs() {
        assert!(valid_key("ics/work"));
        assert!(valid_key("llm/openrouter-key.v2"));
        for bad in ["", "x", "/a/b", "a/", "a/../b", "a b/c", "a/b$"] {
            assert!(!valid_key(bad), "{bad}");
        }
        assert_eq!(parse_ref("secret:ics/work"), Some("ics/work"));
        assert_eq!(parse_ref(" secret:ics/work "), Some("ics/work"));
        assert_eq!(parse_ref("Bearer abc"), None);
        assert_eq!(parse_ref("secret:nokey"), None);
        assert_eq!(require_ref("hunter2", "x").unwrap_err().code, "secret_plaintext");
    }

    #[test]
    fn env_var_names() {
        let e = EnvStore::new("OATS_SECRET_");
        assert_eq!(e.var_name("ics/work-1.v2"), "OATS_SECRET_ICS_WORK_1_V2");
    }

    #[test]
    fn env_store_reads_and_is_read_only() {
        let e = EnvStore::new("OATS_TEST_SECRETS_UNIT_");
        std::env::set_var("OATS_TEST_SECRETS_UNIT_NS_ONE", "tok");
        assert_eq!(e.get("ns/one").unwrap().as_deref(), Some("tok"));
        assert_eq!(e.get("ns/two").unwrap(), None);
        assert_eq!(e.set("ns/one", "x").unwrap_err().code, "secret_read_only");
        assert_eq!(e.delete("ns/one").unwrap_err().code, "secret_read_only");
        assert_eq!(resolve(&e, "secret:ns/one").unwrap(), "tok");
        assert_eq!(resolve(&e, "secret:ns/two").unwrap_err().code, "secret_not_found");
        assert_eq!(resolve(&e, "tok").unwrap_err().code, "secret_plaintext");
    }

    #[test]
    fn mem_store_round_trip() {
        let m = MemStore::default();
        assert_eq!(m.get("a/b").unwrap(), None);
        m.set("a/b", "v").unwrap();
        assert_eq!(resolve(&m, "secret:a/b").unwrap(), "v");
        assert!(m.delete("a/b").unwrap());
        assert!(!m.delete("a/b").unwrap());
    }

    #[test]
    fn chain_prefers_first_hit_and_writes_to_first_writable() {
        std::env::set_var("OATS_TEST_SECRETS_CHAIN_NS_K", "from-env");
        let mem = MemStore::default();
        mem.set("ns/k", "from-mem").unwrap();
        mem.set("ns/only", "mem-only").unwrap();
        let c = Chain(vec![Box::new(EnvStore::new("OATS_TEST_SECRETS_CHAIN_")), Box::new(mem)]);
        assert_eq!(c.get("ns/k").unwrap().as_deref(), Some("from-env"));
        assert_eq!(c.get("ns/only").unwrap().as_deref(), Some("mem-only"));
        c.set("ns/new", "v").unwrap(); // env refuses, memory accepts
        assert_eq!(c.get("ns/new").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn keyring_failures_map_to_a_stable_code_and_never_panic() {
        // A pod has no keychain; a laptop has one. Either answer is fine, a panic is not.
        let k = KeyringStore::new("oats-meetings-rs-unit-test");
        match k.get("ns/definitely-not-there-0001") {
            Ok(None) => {}
            Ok(Some(_)) => panic!("unexpected secret"),
            Err(e) => assert_eq!(e.code, "secret_backend_unavailable"),
        }
        assert_eq!(k.get("nokey").unwrap_err().code, "invalid_argument");
    }
}
