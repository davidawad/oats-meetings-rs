//! Getting the feed bytes: http(s) through `ureq`, `webcal(s)://` as http(s), `file://` from disk.
//! Policy (network.allow, ics.allow_file) is enforced by the caller before anything here runs.

use std::path::PathBuf;
use std::time::Duration;

use crate::config::{ConfigError, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Http(String),
    File(PathBuf),
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn file_path(rest: &str) -> PathBuf {
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    // file:///C:/x on Windows carries a leading slash before the drive letter.
    let b = decoded.as_bytes();
    let drive = b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':';
    PathBuf::from(if drive { decoded[1..].to_string() } else { decoded })
}

/// Classify a feed URL; the only error is `invalid_argument`.
pub fn classify(url: &str, webcal_https: bool) -> Result<Source> {
    let bad = |why: &str| ConfigError::new("invalid_argument", format!("feed url '{url}': {why}"));
    let (scheme, rest) = url.trim().split_once("://").ok_or_else(|| bad("missing scheme://"))?;
    if rest.is_empty() {
        return Err(bad("empty after the scheme"));
    }
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => Ok(Source::Http(url.trim().to_string())),
        "webcal" => Ok(Source::Http(format!(
            "{}://{rest}",
            if webcal_https { "https" } else { "http" }
        ))),
        "webcals" => Ok(Source::Http(format!("https://{rest}"))),
        "file" => Ok(Source::File(file_path(rest))),
        other => Err(bad(&format!("unsupported scheme '{other}' (http, https, webcal, webcals, file)"))),
    }
}

#[derive(Clone, Debug)]
pub struct FetchOpts {
    pub timeout_secs: u64,
    pub max_bytes: u64,
    pub header: Option<(String, String)>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug)]
pub enum Fetched {
    Body { text: String, etag: Option<String>, last_modified: Option<String> },
    NotModified,
}

fn too_large(max: u64) -> ConfigError {
    ConfigError::new("feed_too_large", format!("feed body is larger than ics.max_bytes ({max})"))
}

pub fn fetch(src: &Source, o: &FetchOpts) -> Result<Fetched> {
    match src {
        Source::File(path) => {
            let meta = std::fs::metadata(path)
                .map_err(|e| ConfigError::new("fetch_failed", format!("{}: {e}", path.display())))?;
            if meta.len() > o.max_bytes {
                return Err(too_large(o.max_bytes));
            }
            let bytes = std::fs::read(path)
                .map_err(|e| ConfigError::new("fetch_failed", format!("{}: {e}", path.display())))?;
            Ok(Fetched::Body {
                text: String::from_utf8_lossy(&bytes).into_owned(),
                etag: None,
                last_modified: None,
            })
        }
        Source::Http(url) => fetch_http(url, o),
    }
}

fn map_ureq(e: ureq::Error) -> ConfigError {
    match e {
        ureq::Error::Timeout(_) => ConfigError::new("fetch_timeout", e.to_string()),
        other => ConfigError::new("fetch_failed", other.to_string()),
    }
}

fn fetch_http(url: &str, o: &FetchOpts) -> Result<Fetched> {
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(o.timeout_secs.max(1))))
        .http_status_as_error(false);
    #[cfg(windows)]
    let builder = builder.tls_config(
        ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::NativeTls).build(),
    );
    let agent: ureq::Agent = builder.build().into();
    let mut req = agent
        .get(url)
        .header("Accept", "text/calendar, text/plain;q=0.5, */*;q=0.1");
    if let Some((k, v)) = &o.header {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(t) = &o.etag {
        req = req.header("If-None-Match", t.as_str());
    }
    if let Some(t) = &o.last_modified {
        req = req.header("If-Modified-Since", t.as_str());
    }
    let mut resp = req.call().map_err(map_ureq)?;
    let status = resp.status().as_u16();
    if status == 304 && (o.etag.is_some() || o.last_modified.is_some()) {
        return Ok(Fetched::NotModified);
    }
    if !(200..300).contains(&status) {
        return Err(ConfigError::new("http_status", format!("{url}: HTTP {status}")));
    }
    let hdr = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let etag = hdr("etag");
    let last_modified = hdr("last-modified");
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(o.max_bytes)
        .read_to_vec()
        .map_err(|e| {
            if matches!(e, ureq::Error::BodyExceedsLimit(_)) || e.to_string().contains("exceed") {
                too_large(o.max_bytes)
            } else {
                map_ureq(e)
            }
        })?;
    if bytes.len() as u64 > o.max_bytes {
        return Err(too_large(o.max_bytes));
    }
    Ok(Fetched::Body { text: String::from_utf8_lossy(&bytes).into_owned(), etag, last_modified })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_classify() {
        assert_eq!(classify("https://a/b.ics", true).unwrap(), Source::Http("https://a/b.ics".into()));
        assert_eq!(classify("webcal://a/b.ics", true).unwrap(), Source::Http("https://a/b.ics".into()));
        assert_eq!(classify("webcal://a/b.ics", false).unwrap(), Source::Http("http://a/b.ics".into()));
        assert_eq!(classify("WEBCALS://a/b.ics", false).unwrap(), Source::Http("https://a/b.ics".into()));
        assert_eq!(classify("file:///tmp/a%20b.ics", true).unwrap(), Source::File("/tmp/a b.ics".into()));
        assert_eq!(classify("file:///C:/x/y.ics", true).unwrap(), Source::File("C:/x/y.ics".into()));
        for bad in ["ftp://x/y", "nope", "http://", "file://"] {
            assert_eq!(classify(bad, true).unwrap_err().code, "invalid_argument", "{bad}");
        }
    }
}
