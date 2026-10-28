//! Subsystems (control socket, instance lock, sessions, and later audio, hotkeys, feeds) take part
//! in config reload as a unit: every subsystem validates the candidate config first, then the whole
//! config is swapped, then every subsystem is told. If any validation fails nothing changes.

use crate::config::typed::Config;
use crate::config::Result;

pub trait Subsystem: Send + Sync {
    fn name(&self) -> &'static str;

    /// Can this subsystem run with `new`? May reserve resources; they are released by `abort` if
    /// the reload does not go ahead. Must not change what is currently running.
    fn validate(&self, _new: &Config) -> Result<()> {
        Ok(())
    }

    /// The swap happened; re-apply. Infallible: everything fallible belongs in `validate`.
    fn apply(&self, _old: &Config, _new: &Config) {}

    /// The reload was rejected after (or during) validation.
    fn abort(&self) {}
}
