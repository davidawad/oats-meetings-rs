//! OAuth 2.0 for the google and microsoft providers: the installed-app flow with a loopback
//! redirect and PKCE (RFC 8252, RFC 7636), and refresh-token upkeep.
//!
//! `calendar-connect source=<name>` binds `127.0.0.1:<free port>`, returns the authorization URL
//! (and opens it with `open=true`), and waits up to `calendar.oauth_timeout` in the background for
//! the browser to come back with the code. The code is swapped for tokens with the PKCE verifier,
//! the account is looked up, and the tokens go into the app's own secret store
//! (`calendar.token_file`, a 0600 file; never the OS keychain) under `calendar/<source>`. The bus
//! then carries `calendar-connected` or `calendar-connect-failed`.
//!
//! The tokens are never logged, returned by an action or written to the config.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::provider::{agent, body_text, check_network, enc, err_json, status_error, unreachable};
use super::{Source, SourceKind};
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

/// Where one source signs in.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scope: &'static str,
    /// Extra authorization-request parameters.
    pub extra: Vec<(&'static str, &'static str)>,
}

const GOOGLE_SCOPE: &str = "https://www.googleapis.com/auth/calendar.readonly";
const MICROSOFT_SCOPE: &str = "offline_access User.Read Calendars.Read";

/// The source's endpoints and client, from its own fields or the `calendar.*` defaults.
pub fn endpoints(engine: &Engine, s: &Source) -> Result<Endpoints> {
    let c = engine.snapshot().config.calendar.clone();
    let pick = |own: &str, dflt: &str| {
        if own.trim().is_empty() {
            dflt.trim().to_string()
        } else {
            own.trim().to_string()
        }
    };
    let secret = if s.client_secret.trim().is_empty() {
        None
    } else {
        let cfg = engine.snapshot().config.clone();
        let store = engine.secret_store(&cfg);
        Some(crate::secrets::resolve(
            &*store,
            &s.client_secret,
            cfg.secrets.max_key_len as usize,
        )?)
    };
    let (client_id, e) = match s.kind {
        SourceKind::Google => (
            pick(&s.client_id, &c.google_client_id),
            Endpoints {
                auth_url: c.google_auth_url.trim().to_string(),
                token_url: c.google_token_url.trim().to_string(),
                client_id: String::new(),
                client_secret: secret,
                scope: GOOGLE_SCOPE,
                // A refresh token every time, so a reconnect never leaves the source without one.
                extra: vec![("access_type", "offline"), ("prompt", "consent")],
            },
        ),
        SourceKind::Microsoft => {
            let tenant = pick(&s.tenant, &c.microsoft_tenant);
            let base = format!(
                "{}/{}/oauth2/v2.0",
                c.microsoft_login.trim().trim_end_matches('/'),
                enc(&tenant)
            );
            (
                pick(&s.client_id, &c.microsoft_client_id),
                Endpoints {
                    auth_url: format!("{base}/authorize"),
                    token_url: format!("{base}/token"),
                    client_id: String::new(),
                    client_secret: secret,
                    scope: MICROSOFT_SCOPE,
                    extra: vec![("response_mode", "query"), ("prompt", "select_account")],
                },
            )
        }
        k => {
            return Err(ConfigError::new(
                "invalid_argument",
                format!(
                    "'{}' is a {} source; only google and microsoft sources sign in",
                    s.name,
                    k.as_str()
                ),
            ))
        }
    };
    if client_id.is_empty() {
        let key = if s.kind == SourceKind::Google {
            "calendar.google_client_id"
        } else {
            "calendar.microsoft_client_id"
        };
        return Err(ConfigError::new(
            "invalid_argument",
            format!(
                "'{}' has no OAuth client id: set the source's client_id or {key}",
                s.name
            ),
        ));
    }
    Ok(Endpoints { client_id, ..e })
}

// ---- PKCE ------------------------------------------------------------------------------------

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A random URL-safe string from `n` random bytes.
fn random(n: usize) -> Result<String> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf)
        .map_err(|e| ConfigError::new("io_error", format!("no randomness: {e}")))?;
    Ok(b64(&buf))
}

/// The S256 challenge of a PKCE verifier.
pub fn challenge(verifier: &str) -> String {
    b64(&Sha256::digest(verifier.as_bytes()))
}

/// The authorization URL the browser opens.
pub fn auth_url(e: &Endpoints, redirect: &str, state: &str, verifier: &str) -> String {
    let mut q = vec![
        ("response_type", "code".to_string()),
        ("client_id", e.client_id.clone()),
        ("redirect_uri", redirect.to_string()),
        ("scope", e.scope.to_string()),
        ("state", state.to_string()),
        ("code_challenge", challenge(verifier)),
        ("code_challenge_method", "S256".to_string()),
    ];
    q.extend(e.extra.iter().map(|(k, v)| (*k, v.to_string())));
    let sep = if e.auth_url.contains('?') { '&' } else { '?' };
    let qs: Vec<String> = q.iter().map(|(k, v)| format!("{k}={}", enc(v))).collect();
    format!("{}{sep}{}", e.auth_url, qs.join("&"))
}

// ---- tokens ----------------------------------------------------------------------------------

/// What the token store keeps for one source (JSON under `calendar/<source>`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub scope: String,
    /// The signed-in address, as the provider reports it.
    #[serde(default)]
    pub account: String,
}

pub fn token_key(s: &Source) -> String {
    format!("calendar/{}", s.name)
}

pub fn load(engine: &Engine, s: &Source) -> Result<Option<Tokens>> {
    let store = engine.token_store();
    match store.get(&token_key(s))? {
        None => Ok(None),
        Some(t) => serde_json::from_str(&t).map(Some).map_err(|e| {
            ConfigError::new(
                "calendar_auth",
                format!(
                    "the stored tokens of '{}' do not parse ({e}); run calendar-connect again",
                    s.name
                ),
            )
        }),
    }
}

fn save(engine: &Engine, s: &Source, t: &Tokens) -> Result<()> {
    engine.token_store().set(
        &token_key(s),
        &serde_json::to_string(t).expect("tokens serialize"),
    )
}

pub fn forget(engine: &Engine, s: &Source) -> Result<bool> {
    engine.token_store().delete(&token_key(s))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// POST a form to the token endpoint; the reply's tokens (the refresh token kept from `old` when
/// the reply has none).
fn token_call(
    engine: &Engine,
    e: &Endpoints,
    form: &[(&str, &str)],
    old: &Tokens,
) -> Result<Tokens> {
    let mut form: Vec<(&str, &str)> = form.to_vec();
    form.push(("client_id", &e.client_id));
    if let Some(cs) = &e.client_secret {
        form.push(("client_secret", cs));
    }
    let url = &e.token_url;
    let mut resp = agent(engine)
        .post(url)
        .header("Accept", "application/json")
        .send_form(form)
        .map_err(|err| unreachable(url, err))?;
    let status = resp.status().as_u16();
    let text = body_text(engine, url, &mut resp)?;
    if !(200..300).contains(&status) {
        let mut err = status_error(engine, url, status, &text);
        if text.contains("invalid_grant") {
            err.code = "calendar_auth".into();
        }
        return Err(err);
    }
    let v: Value = serde_json::from_str(&text).map_err(|err| {
        ConfigError::new("backend_bad_response", format!("{url}: not JSON: {err}"))
    })?;
    let access = v["access_token"].as_str().unwrap_or_default();
    if access.is_empty() {
        return Err(ConfigError::new(
            "backend_bad_response",
            format!("{url}: no access_token in the reply"),
        ));
    }
    Ok(Tokens {
        access_token: access.to_string(),
        refresh_token: v["refresh_token"]
            .as_str()
            .filter(|r| !r.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| old.refresh_token.clone()),
        expires_at: now() + v["expires_in"].as_i64().unwrap_or_default(),
        scope: v["scope"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| old.scope.clone()),
        account: old.account.clone(),
    })
}

fn not_connected(s: &Source) -> ConfigError {
    ConfigError::new(
        "calendar_not_connected",
        format!(
            "calendar source '{}' is not signed in: run calendar-connect source={}",
            s.name, s.name
        ),
    )
}

/// A live access token for `s`, refreshed (and stored) when within `calendar.token_skew` of its
/// expiry.
pub fn access_token(engine: &Engine, s: &Source) -> Result<Tokens> {
    let t = load(engine, s)?.ok_or_else(|| not_connected(s))?;
    let skew = engine.snapshot().config.calendar.token_skew.secs() as i64;
    if !t.access_token.is_empty() && t.expires_at - skew > now() {
        return Ok(t);
    }
    if t.refresh_token.is_empty() {
        return Err(not_connected(s));
    }
    let e = endpoints(engine, s)?;
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", t.refresh_token.as_str()),
    ];
    if s.kind == SourceKind::Microsoft {
        form.push(("scope", e.scope));
    }
    let fresh = token_call(engine, &e, &form, &t)?;
    save(engine, s, &fresh)?;
    Ok(fresh)
}

// ---- the loopback flow -----------------------------------------------------------------------

/// The query parameters of a request line `GET /path?a=b HTTP/1.1`.
pub fn query_of(request_line: &str) -> Vec<(String, String)> {
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let Some((_, q)) = target.split_once('?') else {
        return vec![];
    };
    q.split('&')
        .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%')
            .then(|| b.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn reply(mut stream: &TcpStream, status: &str, text: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>oats-meetings-rs</title><p style=\"font:16px system-ui;margin:3em\">{text}</p>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

/// Wait for the browser's redirect on `listener`; the code, or why not. Requests that carry
/// neither a code nor an error (a favicon) are answered 404 and the wait goes on.
fn wait_for_code(
    listener: &TcpListener,
    state: &str,
    deadline: std::time::Instant,
) -> Result<String> {
    loop {
        let (stream, _) = listener
            .accept()
            .map_err(|e| ConfigError::new("io_error", format!("loopback accept: {e}")))?;
        if std::time::Instant::now() >= deadline {
            return Err(ConfigError::new(
                "calendar_connect_timeout",
                "nobody signed in before calendar.oauth_timeout",
            ));
        }
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        let q = query_of(&line);
        let get = |k: &str| q.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        if let Some(err) = get("error") {
            reply(
                &stream,
                "200 OK",
                "Sign-in was cancelled. You can close this tab.",
            );
            let why = get("error_description").unwrap_or_default();
            return Err(ConfigError::new(
                "calendar_auth",
                format!("the provider refused the sign-in: {err} {why}")
                    .trim()
                    .to_string(),
            ));
        }
        let Some(code) = get("code") else {
            reply(&stream, "404 Not Found", "Not here.");
            continue;
        };
        if get("state").as_deref() != Some(state) {
            reply(
                &stream,
                "400 Bad Request",
                "This sign-in did not come from oats-meetings-rs.",
            );
            return Err(ConfigError::new(
                "calendar_auth",
                "the sign-in reply's state does not match (a stale or forged redirect)",
            ));
        }
        reply(
            &stream,
            "200 OK",
            "Signed in: oats-meetings-rs can read your calendar now. You can close this tab.",
        );
        return Ok(code);
    }
}

/// The signed-in address, asked of the provider with a fresh token.
fn whoami(engine: &Engine, s: &Source, token: &str) -> Result<String> {
    match s.kind {
        SourceKind::Google => super::google::account(engine, token),
        SourceKind::Microsoft => super::graph::account(engine, token),
        _ => Ok(String::new()),
    }
}

fn finish(
    engine: &Engine,
    s: &Source,
    e: &Endpoints,
    listener: TcpListener,
    redirect: &str,
    state: &str,
    verifier: &str,
    deadline: std::time::Instant,
) -> Result<Tokens> {
    let code = wait_for_code(&listener, state, deadline)?;
    drop(listener);
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect),
        ("code_verifier", verifier),
    ];
    let mut t = token_call(engine, e, &form, &Tokens::default())?;
    if t.refresh_token.is_empty() {
        return Err(ConfigError::new(
            "calendar_auth",
            "the provider sent no refresh token (offline access was not granted)",
        ));
    }
    t.account = whoami(engine, s, &t.access_token).unwrap_or_default();
    save(engine, s, &t)?;
    Ok(t)
}

/// Open `url` in the default browser.
pub fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// `calendar-connect` for an OAuth source: start the flow and return the URL to open.
pub fn connect(engine: &Engine, s: &Source, open: bool) -> Result<Value> {
    check_network(engine, s)?;
    let e = endpoints(engine, s)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|err| ConfigError::new("io_error", format!("loopback listener: {err}")))?;
    let addr = listener
        .local_addr()
        .map_err(|err| ConfigError::new("io_error", err.to_string()))?;
    let redirect = format!("http://127.0.0.1:{}/", addr.port());
    let state = random(16)?;
    let verifier = random(32)?;
    let url = auth_url(&e, &redirect, &state, &verifier);
    let wait = engine
        .snapshot()
        .config
        .calendar
        .oauth_timeout
        .as_duration();
    let deadline = std::time::Instant::now() + wait;
    let me = engine.arc();
    let (src, red) = (s.clone(), redirect.clone());
    std::thread::Builder::new()
        .name(format!("oauth-{}", s.name))
        .spawn(move || {
            // Wakes the blocking accept at the deadline.
            std::thread::spawn(move || {
                std::thread::sleep(wait);
                let _ = TcpStream::connect(addr);
            });
            let topic_data = match finish(
                &me, &src, &e, listener, &red, &state, &verifier, deadline,
            ) {
                Ok(t) => (
                    "calendar-connected",
                    json!({"source": src.name, "kind": src.kind.as_str(), "account": t.account}),
                ),
                Err(err) => (
                    "calendar-connect-failed",
                    json!({"source": src.name, "kind": src.kind.as_str(), "error": err_json(&err)}),
                ),
            };
            me.bus.publish(topic_data.0, topic_data.1);
        })
        .map_err(|err| ConfigError::new("io_error", err.to_string()))?;
    let opened = open && open_browser(&url).is_ok();
    Ok(json!({
        "source": s.name, "kind": s.kind.as_str(), "url": url, "redirect_uri": redirect,
        "waiting": true, "opened": opened, "timeout_ms": wait.as_millis() as u64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_appendix_b() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn redirect_query_is_decoded() {
        let q = query_of("GET /?state=a%2Bb&code=4%2F0Ab_c+d&scope=x HTTP/1.1\r\n");
        assert_eq!(q[0], ("state".into(), "a+b".into()));
        assert_eq!(q[1], ("code".into(), "4/0Ab_c d".into()));
        assert!(query_of("GET /favicon.ico HTTP/1.1").is_empty());
    }

    #[test]
    fn auth_url_carries_pkce_and_the_loopback_redirect() {
        let e = Endpoints {
            auth_url: "https://accounts.example/auth".into(),
            token_url: String::new(),
            client_id: "cid.apps".into(),
            client_secret: None,
            scope: GOOGLE_SCOPE,
            extra: vec![("access_type", "offline")],
        };
        let u = auth_url(&e, "http://127.0.0.1:5555/", "st", "ver");
        assert!(u.starts_with("https://accounts.example/auth?response_type=code&client_id=cid.apps&redirect_uri=http%3A%2F%2F127.0.0.1%3A5555%2F&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.readonly&state=st&code_challenge="), "{u}");
        assert!(u.contains(&format!(
            "code_challenge={}&code_challenge_method=S256",
            challenge("ver")
        )));
        assert!(u.ends_with("&access_type=offline"));
    }
}
