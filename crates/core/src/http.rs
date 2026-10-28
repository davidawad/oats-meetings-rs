//! Minimal blocking JSON-over-HTTP client for the LLM and embedding backends, plus the URL
//! helpers the network gate uses. Errors never include request headers (so never a key).

use std::time::Duration;

use serde_json::Value;

use crate::config::{ConfigError, Result};

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
/// `timeout` is the whole-request limit (`llm.timeout` / `kb.timeout`).
pub fn post_json(url: &str, bearer: Option<&str>, body: &Value, timeout: Duration) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent.post(url).header("Content-Type", "application/json");
    if let Some(b) = bearer {
        req = req.header("Authorization", format!("Bearer {b}"));
    }
    let mut resp = req
        .send(body.to_string())
        .map_err(|e| ConfigError::new("backend_unreachable", format!("{url}: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| ConfigError::new("backend_bad_response", format!("{url}: reading reply: {e}")))?;
    if !(200..300).contains(&status) {
        let snippet: String = text.chars().take(300).collect();
        return Err(ConfigError::new("backend_error", format!("{url}: HTTP {status}: {snippet}")));
    }
    serde_json::from_str(&text)
        .map_err(|e| ConfigError::new("backend_bad_response", format!("{url}: not JSON: {e}")))
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

    /// The caller's timeout is the whole-request limit: a server that accepts and never answers
    /// fails in about that long, whether the timeout is llm.timeout or kb.timeout.
    #[test]
    fn timeout_is_the_callers() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x", l.local_addr().unwrap());
        let hold = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            std::thread::sleep(Duration::from_secs(3));
            drop(s);
        });
        let t = std::time::Instant::now();
        let e = post_json(&url, None, &serde_json::json!({}), Duration::from_secs(1)).unwrap_err();
        assert_eq!(e.code, "backend_unreachable");
        assert!(t.elapsed() < Duration::from_millis(2500), "{:?}", t.elapsed());
        hold.join().unwrap();
    }

    #[test]
    fn join_handles_slashes() {
        assert_eq!(join("http://a/b/", "/c"), "http://a/b/c");
        assert_eq!(join("http://a", "c"), "http://a/c");
    }
}
