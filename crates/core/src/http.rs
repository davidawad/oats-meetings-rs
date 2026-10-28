//! Minimal blocking JSON-over-HTTP client for the LLM and embedding backends, plus the URL
//! helpers the network gate uses. Errors never include request headers (so never a key).

use std::time::Duration;

use serde_json::Value;

use crate::config::{ConfigError, Result};

pub const TIMEOUT: Duration = Duration::from_secs(120);

/// Host part of an http(s) URL (userinfo and port stripped), lowercased.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?;
    let host = if let Some(v6) = hostport.strip_prefix('[') {
        v6.split(']').next()?.to_string()
    } else {
        hostport.split(':').next()?.to_string()
    };
    (!host.is_empty()).then(|| host.to_lowercase())
}

pub fn valid_url(url: &str) -> bool {
    host_of(url).is_some()
}

/// Loopback hosts never leave the machine.
pub fn is_loopback(url: &str) -> bool {
    match host_of(url) {
        Some(h) => {
            h == "localhost"
                || h.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
        }
        None => false,
    }
}

pub fn join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// POST `body` as JSON and parse the JSON response.
pub fn post_json(url: &str, bearer: Option<&str>, body: &Value) -> Result<Value> {
    let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
    let mut req = agent.post(url).set("Content-Type", "application/json");
    if let Some(b) = bearer {
        req = req.set("Authorization", &format!("Bearer {b}"));
    }
    match req.send_json(body.clone()) {
        Ok(resp) => resp
            .into_json::<Value>()
            .map_err(|e| ConfigError::new("backend_bad_response", format!("{url}: not JSON: {e}"))),
        Err(ureq::Error::Status(code, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            let snippet: String = text.chars().take(300).collect();
            Err(ConfigError::new("backend_error", format!("{url}: HTTP {code}: {snippet}")))
        }
        Err(ureq::Error::Transport(t)) => {
            Err(ConfigError::new("backend_unreachable", format!("{url}: {}", t.kind())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_loopback() {
        assert_eq!(host_of("http://127.0.0.1:11434/api"), Some("127.0.0.1".into()));
        assert_eq!(host_of("https://User:pw@Example.COM:8443/x?y"), Some("example.com".into()));
        assert_eq!(host_of("http://[::1]:80/"), Some("::1".into()));
        assert_eq!(host_of("ftp://x"), None);
        assert_eq!(host_of("sample-x"), None);
        for u in ["http://localhost:1/", "http://127.0.0.1/", "http://127.9.9.9:5/", "http://[::1]/"] {
            assert!(is_loopback(u), "{u}");
        }
        for u in ["https://openrouter.ai/api/v1", "http://10.0.0.5/", "http://127.0.0.1.evil.com/", "nonsense"] {
            assert!(!is_loopback(u), "{u}");
        }
    }

    #[test]
    fn join_handles_slashes() {
        assert_eq!(join("http://a/b/", "/c"), "http://a/b/c");
        assert_eq!(join("http://a", "c"), "http://a/c");
    }
}
