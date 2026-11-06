//! Config surface (dot-4uzgol.4), configmaxxed: ONE typed defaults object (`keys::KEYS`),
//! packs and axes as data (`config/packs/*.json`, `config/axes.json`), a deterministic router
//! (`resolve`), and generated projections (`generate`).
//!
//! Layer order, lowest first: defaults < axes (os, arch, machine) < pack < user file.

pub mod cli;
pub mod generate;
pub mod keys;
pub mod resolve;
pub mod shortcut;
pub mod typed;

use std::fmt;

/// Structured error: `code` is the stable machine-readable part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub code: &'static str,
    pub message: String,
}

impl ConfigError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for ConfigError {}

pub type Result<T> = std::result::Result<T, ConfigError>;

pub const SCHEMA_VERSION: u32 = 1;
