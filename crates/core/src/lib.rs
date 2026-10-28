//! Engine: no UI. Single command grammar (Action) lands here in later beads.

pub mod action;
pub mod config;
pub mod control;
pub mod record;
pub mod http;
pub mod kb;
pub mod notes;
pub mod secrets;
pub mod templates;
pub mod ics;
pub mod sessions;

pub const APP_NAME: &str = "oats-meetings-rs";

pub fn version_line() -> String {
    format!("{} {}", APP_NAME, env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_line_has_name() {
        assert!(super::version_line().starts_with("oats-meetings-rs "));
    }
}
