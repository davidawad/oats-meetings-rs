//! LLM notes: render a session transcript, send it with a template's system prompt to the configured
//! backend (`llm.backend`: ollama, openrouter, openai-compatible), check the template's `## `
//! headings are present (`notes.enforce_headings`) and write the notes into the session.
//!
//! The network gate: openrouter always counts as cloud; ollama and openai-compatible are cloud
//! unless their URL is a loopback address. Cloud needs `network.allow = true`, otherwise
//! `network_denied` and no I/O. `dry_run` shows the request without sending it, and works with the
//! gate closed.

use std::path::Path;

use serde_json::{json, Value};

use crate::config::typed::{Config, LlmBackend};
use crate::config::{ConfigError, Result};
use crate::control::subsystem::Subsystem;
use crate::http;
use crate::secrets::{self, SecretStore};
use crate::sessions::Store;
use crate::templates;

pub const OLLAMA_DEFAULT: &str = "http://127.0.0.1:11434";
pub const OPENROUTER_DEFAULT: &str = "https://openrouter.ai/api/v1";

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub store: &'a Store,
    pub templates_dir: &'a Path,
    pub secrets: &'a dyn SecretStore,
}

pub fn backend_name(b: LlmBackend) -> &'static str {
    match b {
        LlmBackend::Ollama => "ollama",
        LlmBackend::Openrouter => "openrouter",
        LlmBackend::OpenaiCompatible => "openai-compatible",
    }
}

/// The chat endpoint for the configured backend.
pub fn chat_url(cfg: &Config) -> Result<String> {
    let base = cfg.llm.base_url.as_str();
    match cfg.llm.backend {
        LlmBackend::Ollama => Ok(http::join(
            if base.is_empty() {
                OLLAMA_DEFAULT
            } else {
                base
            },
            "api/chat",
        )),
        LlmBackend::Openrouter => Ok(http::join(
            if base.is_empty() {
                OPENROUTER_DEFAULT
            } else {
                base
            },
            "chat/completions",
        )),
        LlmBackend::OpenaiCompatible => {
            if base.is_empty() {
                Err(ConfigError::new(
                    "base_url_required",
                    "llm.backend = openai-compatible needs llm.base_url",
                ))
            } else {
                Ok(http::join(base, "chat/completions"))
            }
        }
    }
}

/// Would a request to `url` leave the machine under this backend?
pub fn is_outward(backend: LlmBackend, url: &str) -> bool {
    matches!(backend, LlmBackend::Openrouter) || !http::is_loopback(url)
}

/// `network_denied` unless the request stays local or `network.allow` is on.
pub fn gate(cfg: &Config, backend: LlmBackend, url: &str) -> Result<()> {
    if is_outward(backend, url) && !cfg.network.allow {
        return Err(ConfigError::new(
            "network_denied",
            format!(
                "{} request to {} needs network.allow = true",
                backend_name(backend),
                http::host_of(url).unwrap_or_default()
            ),
        ));
    }
    Ok(())
}

/// The API key for the configured backend: required for openrouter, optional for
/// openai-compatible, none for ollama.
pub fn api_key(cfg: &Config, secrets: &dyn SecretStore) -> Result<Option<String>> {
    if cfg.llm.backend == LlmBackend::Ollama {
        return Ok(None);
    }
    match secrets::resolve(
        secrets,
        &cfg.llm.api_key_ref,
        cfg.secrets.max_key_len as usize,
    ) {
        Ok(k) => Ok(Some(k)),
        Err(e) if e.code == "secret_not_found" && cfg.llm.backend != LlmBackend::Openrouter => {
            Ok(None)
        }
        Err(e) if e.code == "secret_not_found" => Err(ConfigError::new(
            "secret_missing",
            format!(
                "openrouter needs an API key: {} (set it with `secret-set`)",
                e.message
            ),
        )),
        Err(e) => Err(e),
    }
}

fn render_transcript(
    store: &Store,
    session: &str,
    speakers: &crate::config::typed::Speakers,
) -> Result<String> {
    let t = store.transcript(session)?;
    let records = t["utterances"].as_array().map_or(&[][..], Vec::as_slice);
    let labels = crate::speakers::Labels::new(
        speakers,
        t["speaker_names"].as_object(),
        crate::speakers::ids(records),
    );
    let mut lines = vec![];
    for u in records {
        let speaker = u["speaker"].as_str().unwrap_or("?");
        let who = labels.label(speaker);
        let text = u["refinedText"]
            .as_str()
            .or_else(|| u["text"].as_str())
            .unwrap_or("");
        lines.push(match u["timestamp"].as_str() {
            Some(ts) => format!("[{ts}] {who}: {text}"),
            None => format!("{who}: {text}"),
        });
    }
    if lines.is_empty() {
        return Err(ConfigError::new(
            "no_transcript",
            format!("session '{session}' has no transcript"),
        ));
    }
    Ok(lines.join("\n"))
}

fn request_body(cfg: &Config, system: &str, user: &str) -> Value {
    let messages =
        json!([{"role": "system", "content": system}, {"role": "user", "content": user}]);
    match cfg.llm.backend {
        LlmBackend::Ollama => {
            json!({"model": cfg.llm.model, "messages": messages, "stream": false})
        }
        _ => json!({"model": cfg.llm.model, "messages": messages}),
    }
}

fn extract(backend: LlmBackend, resp: &Value) -> Result<String> {
    let text = match backend {
        LlmBackend::Ollama => resp["message"]["content"].as_str(),
        _ => resp["choices"][0]["message"]["content"].as_str(),
    };
    text.map(str::to_string)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| {
            ConfigError::new(
                "backend_bad_response",
                "backend reply has no message content",
            )
        })
}

pub fn missing_headings(template_prompt: &str, output: &str) -> Vec<String> {
    templates::required_headings(template_prompt)
        .into_iter()
        .filter(|h| !output.lines().any(|l| l.trim_end() == h))
        .collect()
}

pub fn generate(ctx: &Ctx, session: &str, template: &str, dry_run: bool) -> Result<Value> {
    let tpl = templates::find(ctx.templates_dir, template)?;
    let transcript = render_transcript(ctx.store, session, &ctx.cfg.speakers)?;
    let url = chat_url(ctx.cfg)?;
    let backend = ctx.cfg.llm.backend;
    let body = request_body(ctx.cfg, &tpl.system_prompt, &transcript);
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "backend": backend_name(backend),
            "url": url,
            "request": body,
            "outward": is_outward(backend, &url),
            "would_be_denied": gate(ctx.cfg, backend, &url).is_err(),
            "api_key_present": api_key(ctx.cfg, ctx.secrets).map(|k| k.is_some()).unwrap_or(false),
        }));
    }
    gate(ctx.cfg, backend, &url)?;
    let key = api_key(ctx.cfg, ctx.secrets)?;
    let resp = http::post_json(
        &url,
        key.as_deref(),
        &body,
        ctx.cfg.llm.timeout.as_duration(),
        ctx.cfg.network.error_snippet as usize,
    )?;
    let notes = extract(backend, &resp)?;
    if ctx.cfg.notes.enforce_headings {
        let missing = missing_headings(&tpl.system_prompt, &notes);
        if !missing.is_empty() {
            return Err(ConfigError::new(
                "headings_missing",
                format!(
                    "backend output lacks required headings: {}",
                    missing.join(", ")
                ),
            ));
        }
    }
    let path = ctx.store.set_notes(
        session,
        &notes,
        &tpl,
        backend_name(backend),
        &ctx.cfg.llm.model,
    )?;
    Ok(json!({
        "session": session,
        "template": tpl.name,
        "backend": backend_name(backend),
        "model": ctx.cfg.llm.model,
        "path": path,
        "notes": notes,
    }))
}

/// Config validation for everything the LLM, template and KB features read.
pub struct NotesSubsystem;

impl Subsystem for NotesSubsystem {
    fn name(&self) -> &'static str {
        "notes"
    }

    fn validate(&self, new: &Config) -> Result<()> {
        for (key, url) in [
            ("llm.base_url", &new.llm.base_url),
            ("kb.base_url", &new.kb.base_url),
        ] {
            if !url.is_empty() && !http::valid_url(url) {
                return Err(ConfigError::new(
                    "invalid_value",
                    format!("{key} {url:?} is not an http(s) URL"),
                ));
            }
        }
        for (key, dir) in [
            ("templates.dir", &new.templates.dir),
            ("kb.dir", &new.kb.dir),
        ] {
            if !dir.is_empty() && Path::new(dir).is_file() {
                return Err(ConfigError::new(
                    "invalid_value",
                    format!("{key} {dir:?} is an existing file, not a directory"),
                ));
            }
        }
        // A literal API key here is refused outright: only references are config.
        let key = secrets::require_ref(&new.llm.api_key_ref, "llm.api_key_ref")?;
        secrets::check_len(&key, new.secrets.max_key_len as usize)?;
        Ok(())
    }
}
