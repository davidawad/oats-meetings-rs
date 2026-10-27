//! Engine: no UI. Single command grammar (Action) lands here in later beads.

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
