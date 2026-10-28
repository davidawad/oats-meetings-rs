//! LLM notes, templates, KB and the network gate (dot-4uzgol.10), against local mock HTTP servers
//! only: nothing here touches the internet.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use oats_core::control::client::Client;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::control::server;
use oats_core::kb::hash_embed;
use oats_core::secrets::{MemStore, SecretStore};
use serde_json::{json, Map, Value};

// ---------- mock server ----------

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    auth: Option<String>,
    body: Value,
}

struct Mock {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
}

impl Mock {
    /// `handler(path, body) -> (status, json)`.
    fn start(handler: impl Fn(&str, &Value) -> (u16, String) + Send + 'static) -> Mock {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let seen: Arc<Mutex<Vec<Seen>>> = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, st2, sv2) = (Arc::clone(&seen), Arc::clone(&stop), Arc::clone(&server));
        thread::spawn(move || {
            while !st2.load(Ordering::SeqCst) {
                let Ok(Some(mut req)) = sv2.recv_timeout(Duration::from_millis(50)) else { continue };
                let mut raw = String::new();
                let _ = req.as_reader().read_to_string(&mut raw);
                let body: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                let auth = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.to_string());
                let path = req.url().to_string();
                s2.lock().unwrap().push(Seen { path: path.clone(), auth, body: body.clone() });
                let (code, text) = handler(&path, &body);
                let resp = tiny_http::Response::from_string(text)
                    .with_status_code(code)
                    .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = req.respond(resp);
            }
        });
        Mock { base, seen, stop }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

const NOTES: &str = "## CRM\nContact Name: Dana\nCompany: Acme\n\n## Summary\nRenewal looks fine.\n\n## Next Step\nSend the order form.";

fn chat_reply(backend: &str, text: &str) -> String {
    match backend {
        "ollama" => json!({"message": {"role": "assistant", "content": text}, "done": true}).to_string(),
        _ => json!({"choices": [{"message": {"role": "assistant", "content": text}}]}).to_string(),
    }
}

// ---------- rig ----------

struct Rig {
    _dir: tempfile::TempDir,
    dir: PathBuf,
    rt: PathBuf,
    cfg: PathBuf,
    handle: Option<server::ServerHandle>,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

impl Rig {
    fn new(toml: &str, secrets: &[(&str, &str)]) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, toml).unwrap();
        let mem = MemStore::default();
        for (k, v) in secrets {
            mem.set(k, v).unwrap();
        }
        let store: Arc<dyn SecretStore> = Arc::new(mem);
        let engine = Engine::new(EngineOpts {
            user_config: cfg.clone(),
            pack: None,
            os: "linux".into(),
            arch: "x86_64".into(),
            machine: "default".into(),
            sessions_dir: dir.path().join("sessions"),
            secrets: Some(store),
        })
        .unwrap();
        let rt = dir.path().join("rt");
        let handle = server::serve_with_poll(engine, &rt, Duration::from_millis(40)).unwrap();
        let rig = Rig { dir: dir.path().to_path_buf(), _dir: dir, rt, cfg, handle: Some(handle) };
        let mut c = rig.client();
        let r = c.call("session-import", p(&[("from", json!(fixtures().join("openoats").to_str().unwrap()))])).unwrap();
        assert_eq!(r["ok"], true, "{r}");
        let r = c
            .call("template-import", p(&[("from", json!(fixtures().join("openoats-templates.json").to_str().unwrap()))]))
            .unwrap();
        assert_eq!(r["ok"], true, "{r}");
        rig
    }

    fn client(&self) -> Client {
        Client::connect(&self.rt).unwrap()
    }

    fn generate(&self, template: &str, extra: &[(&str, Value)]) -> Value {
        let mut m = p(&[("session", json!(SESSION)), ("template", json!(template))]);
        for (k, v) in extra {
            m.insert((*k).to_string(), v.clone());
        }
        self.client().call("notes-generate", m).unwrap()
    }
}

const SESSION: &str = "session_2026-10-20_09-00-00";

fn p(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn llm_toml(backend: &str, base: &str, extra: &str) -> String {
    format!("[llm]\nbackend = \"{backend}\"\nbase_url = \"{base}\"\nmodel = \"test-model\"\n{extra}")
}

// ---------- backends ----------

#[test]
fn ollama_backend_generates_and_writes_notes_into_the_session() {
    let mock = Mock::start(|_, _| (200, chat_reply("ollama", NOTES)));
    let rig = Rig::new(&llm_toml("ollama", &mock.base, ""), &[]);
    let mut sub = rig.client();
    sub.call("subscribe", p(&[("topics", json!(["session-saved"]))])).unwrap();
    let r = rig.generate("Sales Prospect Call", &[]);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["data"]["notes"], NOTES);
    assert_eq!(r["data"]["backend"], "ollama");
    // Wire format.
    let req = &mock.requests()[0];
    assert_eq!(req.path, "/api/chat");
    assert_eq!(req.auth, None);
    assert_eq!(req.body["model"], "test-model");
    assert_eq!(req.body["stream"], false);
    assert_eq!(req.body["messages"][0]["role"], "system");
    assert!(req.body["messages"][0]["content"].as_str().unwrap().contains("## CRM"));
    // Transcript: speaker names applied, cleaned text preferred, stale live line absent.
    let user = req.body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("Dana: The renewal is OK."), "{user}");
    assert!(user.contains("you: great"), "{user}");
    assert!(!user.contains("stale live line"), "{user}");
    // Notes landed in the session, in the OpenOats-compatible metadata.
    let dir = rig.dir.join("sessions").join(SESSION);
    assert_eq!(std::fs::read_to_string(dir.join("notes.md")).unwrap(), NOTES);
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("session.json")).unwrap()).unwrap();
    assert_eq!(meta["hasNotes"], true);
    assert_eq!(meta["notesTemplate"], "Sales Prospect Call");
    assert_eq!(meta["engine"], "parakeet", "OpenOats keys survive");
    // The exact headings the template demands are in the notes.
    for h in ["## CRM", "## Summary", "## Next Step"] {
        assert!(NOTES.lines().any(|l| l == h));
        assert!(r["data"]["notes"].as_str().unwrap().lines().any(|l| l == h), "{h}");
    }
    let ev = {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            tx.send(sub.next_event()).ok();
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap()
    };
    assert_eq!((ev["data"]["id"].as_str(), ev["data"]["change"].as_str()), (Some(SESSION), Some("notes")));
}

#[test]
fn openai_compatible_backend_with_and_without_a_key() {
    let mock = Mock::start(|_, _| (200, chat_reply("openai", NOTES)));
    let rig = Rig::new(&llm_toml("openai-compatible", &mock.base, ""), &[("llm/api-key", "sk-test-1")]);
    let r = rig.generate("Generic", &[]);
    // Template "Generic" demands Summary + Action Items; NOTES lacks Action Items, so turn the gate off here.
    assert_eq!(r["error"]["code"], "headings_missing", "{r}");
    let r = rig.generate("Sales Prospect Call", &[]);
    assert_eq!(r["ok"], true, "{r}");
    let req = mock.requests().pop().unwrap();
    assert_eq!(req.path, "/chat/completions");
    assert_eq!(req.auth.as_deref(), Some("Bearer sk-test-1"));
    assert!(req.body.get("stream").is_none());

    // No key configured: openai-compatible endpoints may be keyless (local vLLM etc.).
    let keyless = Rig::new(&llm_toml("openai-compatible", &mock.base, ""), &[]);
    assert_eq!(keyless.generate("Sales Prospect Call", &[])["ok"], true);
    assert_eq!(mock.requests().pop().unwrap().auth, None);

    // No base_url at all is a clear error, not a request to nowhere.
    let nourl = Rig::new("[llm]\nbackend = \"openai-compatible\"\n", &[]);
    assert_eq!(nourl.generate("Sales Prospect Call", &[])["error"]["code"], "base_url_required");
}

#[test]
fn openrouter_backend_needs_network_allow_and_a_key() {
    let mock = Mock::start(|_, _| (200, chat_reply("openrouter", NOTES)));
    let denied = Rig::new(&llm_toml("openrouter", &mock.base, ""), &[("llm/api-key", "k")]);
    let r = denied.generate("Sales Prospect Call", &[]);
    assert_eq!(r["error"]["code"], "network_denied", "{r}");
    assert!(mock.requests().is_empty(), "a denied request must do no I/O");

    let allow = "\n[network]\nallow = true\n";
    let nokey = Rig::new(&format!("{}{allow}", llm_toml("openrouter", &mock.base, "")), &[]);
    assert_eq!(nokey.generate("Sales Prospect Call", &[])["error"]["code"], "secret_missing");
    assert!(mock.requests().is_empty());

    let ok = Rig::new(&format!("{}{allow}", llm_toml("openrouter", &mock.base, "")), &[("llm/api-key", "or-key")]);
    let r = ok.generate("Sales Prospect Call", &[]);
    assert_eq!(r["ok"], true, "{r}");
    let req = &mock.requests()[0];
    assert_eq!(req.path, "/chat/completions");
    assert_eq!(req.auth.as_deref(), Some("Bearer or-key"));
}

#[test]
fn custom_key_name_is_honoured() {
    let mock = Mock::start(|_, _| (200, chat_reply("openai", NOTES)));
    let rig = Rig::new(
        &llm_toml("openai-compatible", &mock.base, "api_key_ref = \"secret:llm/mine\"\n"),
        &[("llm/mine", "named"), ("llm/api-key", "wrong")],
    );
    assert_eq!(rig.generate("Sales Prospect Call", &[])["ok"], true);
    assert_eq!(mock.requests()[0].auth.as_deref(), Some("Bearer named"));
}

// ---------- network gate ----------

#[test]
fn cloud_backend_without_network_allow_is_network_denied_with_default_urls() {
    // No base_url: openrouter's real URL is never contacted.
    let rig = Rig::new("[llm]\nbackend = \"openrouter\"\n", &[("llm/api-key", "k")]);
    let r = rig.generate("Sales Prospect Call", &[]);
    assert_eq!(r["error"]["code"], "network_denied", "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("openrouter.ai"));
}

#[test]
fn non_loopback_ollama_is_outward_and_denied() {
    let rig = Rig::new("[llm]\nbackend = \"ollama\"\nbase_url = \"http://10.255.255.1:11434\"\n", &[]);
    assert_eq!(rig.generate("Sales Prospect Call", &[])["error"]["code"], "network_denied");
    let rig = Rig::new("[llm]\nbackend = \"openai-compatible\"\nbase_url = \"https://api.example.com/v1\"\n", &[]);
    assert_eq!(rig.generate("Sales Prospect Call", &[])["error"]["code"], "network_denied");
}

#[test]
fn dry_run_shows_the_request_without_sending_it_even_when_denied() {
    let mock = Mock::start(|_, _| (200, chat_reply("openai", NOTES)));
    let rig = Rig::new(&llm_toml("openrouter", &mock.base, ""), &[("llm/api-key", "super-secret-key")]);
    let r = rig.generate("Sales Prospect Call", &[("dry_run", json!(true))]);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["data"]["dry_run"], true);
    assert_eq!(r["data"]["would_be_denied"], true);
    assert_eq!(r["data"]["outward"], true);
    assert_eq!(r["data"]["api_key_present"], true);
    assert_eq!(r["data"]["request"]["model"], "test-model");
    assert!(!r.to_string().contains("super-secret-key"), "dry run must never print the key");
    assert!(mock.requests().is_empty());
    assert!(!rig.dir.join("sessions").join(SESSION).join("notes.md").exists());
}

// ---------- failures ----------

#[test]
fn enforce_headings_both_ways() {
    let mock = Mock::start(|_, _| (200, chat_reply("ollama", "Just some prose with no headings.")));
    let on = Rig::new(&llm_toml("ollama", &mock.base, ""), &[]);
    let r = on.generate("Sales Prospect Call", &[]);
    assert_eq!(r["error"]["code"], "headings_missing", "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("## CRM"));
    assert!(!on.dir.join("sessions").join(SESSION).join("notes.md").exists(), "rejected notes must not be written");

    let off = Rig::new(&format!("{}[notes]\nenforce_headings = false\n", llm_toml("ollama", &mock.base, "")), &[]);
    let r = off.generate("Sales Prospect Call", &[]);
    assert_eq!(r["ok"], true, "{r}");
    assert!(off.dir.join("sessions").join(SESSION).join("notes.md").is_file());
}

#[test]
fn backend_failures_have_stable_codes() {
    let m500 = Mock::start(|_, _| (500, "{\"error\":\"boom\"}".into()));
    let rig = Rig::new(&llm_toml("ollama", &m500.base, ""), &[]);
    assert_eq!(rig.generate("Sales Prospect Call", &[])["error"]["code"], "backend_error");

    let junk = Mock::start(|_, _| (200, "not json".into()));
    let rig = Rig::new(&llm_toml("ollama", &junk.base, ""), &[]);
    assert_eq!(rig.generate("Sales Prospect Call", &[])["error"]["code"], "backend_bad_response");

    let empty = Mock::start(|_, _| (200, "{\"choices\":[]}".into()));
    let rig = Rig::new(&llm_toml("openai-compatible", &empty.base, ""), &[]);
    assert_eq!(rig.generate("Sales Prospect Call", &[])["error"]["code"], "backend_bad_response");

    // Nothing listening (a port we just released).
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let rig = Rig::new(&llm_toml("ollama", &format!("http://127.0.0.1:{port}"), ""), &[]);
    assert_eq!(rig.generate("Sales Prospect Call", &[])["error"]["code"], "backend_unreachable");
}

#[test]
fn unknown_template_session_and_empty_transcript() {
    let mock = Mock::start(|_, _| (200, chat_reply("ollama", NOTES)));
    let rig = Rig::new(&llm_toml("ollama", &mock.base, ""), &[]);
    assert_eq!(rig.generate("No Such Template", &[])["error"]["code"], "not_found");
    let r = rig
        .client()
        .call("notes-generate", p(&[("session", json!("nope")), ("template", json!("Generic"))]))
        .unwrap();
    assert_eq!(r["error"]["code"], "not_found");
    assert!(mock.requests().is_empty());
}

#[test]
fn backend_and_url_are_picked_up_by_reload_without_a_restart() {
    let a = Mock::start(|_, _| (200, chat_reply("ollama", NOTES)));
    let b = Mock::start(|_, _| (200, chat_reply("openai", NOTES)));
    let rig = Rig::new(&llm_toml("ollama", &a.base, ""), &[]);
    assert_eq!(rig.generate("Sales Prospect Call", &[])["ok"], true);
    assert_eq!((a.requests().len(), b.requests().len()), (1, 0));
    std::fs::write(&rig.cfg, llm_toml("openai-compatible", &b.base, "")).unwrap();
    let r = rig.client().call("config-reload", Map::new()).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(rig.generate("Sales Prospect Call", &[])["ok"], true);
    assert_eq!((a.requests().len(), b.requests().len()), (1, 1));
    assert_eq!(b.requests()[0].path, "/chat/completions");
}

#[test]
fn bad_urls_and_dirs_are_rejected_by_the_notes_subsystem() {
    let rig = Rig::new("", &[]);
    let mut c = rig.client();
    let r = c.call("config-set", p(&[("key", json!("llm.base_url")), ("value", json!("not a url"))])).unwrap();
    assert_eq!(r["error"]["code"], "invalid_value", "{r}");
    assert!(r["error"]["message"].as_str().unwrap().starts_with("notes: "));
    let r = c.call("config-set", p(&[("key", json!("kb.base_url")), ("value", json!("ftp://x"))])).unwrap();
    assert_eq!(r["error"]["code"], "invalid_value");
    let r = c.call("config-set", p(&[("key", json!("templates.dir")), ("value", json!(rig.cfg.to_str().unwrap()))])).unwrap();
    assert_eq!(r["error"]["code"], "invalid_value");
    let r = c.call("config-set", p(&[("key", json!("llm.api_key_ref")), ("value", json!(" "))])).unwrap();
    assert_eq!(r["error"]["code"], "secret_plaintext");
}

// ---------- secrets ----------

#[test]
fn api_keys_cannot_live_in_the_config_file() {
    let rig = Rig::new("", &[]);
    let mut c = rig.client();
    for key in ["llm.api_key", "llm.key", "secrets.api_key", "secrets.llm"] {
        let r = c.call("config-set", p(&[("key", json!(key)), ("value", json!("sk-leak"))])).unwrap();
        assert_eq!(r["error"]["code"], "unknown_key", "{key}");
    }
    // The one place a key could be typed is the reference field, and it only takes references.
    let r = c.call("config-set", p(&[("key", json!("llm.api_key_ref")), ("value", json!("sk-leak"))])).unwrap();
    assert_eq!(r["error"]["code"], "secret_plaintext", "{r}");
    let r = c.call("config-set", p(&[("key", json!("llm.api_key_ref")), ("value", json!("secret:llm/other"))])).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    std::fs::write(&rig.cfg, "[llm]\napi_key = \"sk-leak\"\n").unwrap();
    let r = c.call("config-reload", Map::new()).unwrap();
    assert_eq!(r["error"]["code"], "unknown_key");
    assert!(!std::fs::read_to_string(&rig.cfg).unwrap().is_empty());
}

#[test]
fn keyring_backend_is_used_when_selected() {
    // No test secret store: the engine builds its own from secrets.source. In a sandbox the
    // keychain is empty or unavailable; both must surface as clear errors, never as a request.
    let mock = Mock::start(|_, _| (200, chat_reply("openai", NOTES)));
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, format!("{}\n[network]\nallow = true\n[secrets]\nbackend = \"keyring\"\nkeyring_service = \"oats-meetings-rs-test-none\"\n", llm_toml("openrouter", &mock.base, ""))).unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: cfg,
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    let h = server::serve_with_poll(engine, &dir.path().join("rt"), Duration::from_millis(40)).unwrap();
    let mut c = Client::connect(&dir.path().join("rt")).unwrap();
    c.call("session-import", p(&[("from", json!(fixtures().join("openoats").to_str().unwrap()))])).unwrap();
    c.call("template-import", p(&[("from", json!(fixtures().join("openoats-templates.json").to_str().unwrap()))])).unwrap();
    let r = c.call("notes-generate", p(&[("session", json!(SESSION)), ("template", json!("Sales Prospect Call"))])).unwrap();
    let code = r["error"]["code"].as_str().unwrap_or("");
    assert!(code == "secret_missing" || code == "secret_backend_unavailable", "{r}");
    assert!(mock.requests().is_empty());
    drop(h);
}

// ---------- templates over the API ----------

#[test]
fn template_list_get_import() {
    let rig = Rig::new("", &[]);
    let mut c = rig.client();
    let r = c.call("template-list", Map::new()).unwrap();
    assert_eq!(r["data"]["count"], 3);
    let names: Vec<&str> = r["data"]["templates"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Generic", "Sales Prospect Call", "Stand-Up"]);
    let sales = &r["data"]["templates"][1];
    assert_eq!(sales["headings"], json!(["## CRM", "## Summary", "## Next Step"]));
    let g = c.call("template-get", p(&[("name", json!("sales prospect call"))])).unwrap();
    assert_eq!(g["data"]["name"], "Sales Prospect Call");
    assert_eq!(g["data"]["schema_version"], 1);
    assert_eq!(c.call("template-get", p(&[("name", json!("zzz"))])).unwrap()["error"]["code"], "not_found");
    // Re-import is a no-op; the files are on disk next to the config.
    let again = c
        .call("template-import", p(&[("from", json!(fixtures().join("openoats-templates.json").to_str().unwrap()))]))
        .unwrap();
    assert_eq!(again["data"]["imported"], json!([]));
    assert!(rig.dir.join("templates/sales-prospect-call.toml").is_file());
}

#[test]
fn templates_dir_key_moves_the_template_store() {
    let other = tempfile::tempdir().unwrap();
    let toml = format!("[templates]\ndir = {:?}\n", other.path().join("tpl").to_str().unwrap());
    let rig = Rig::new(&toml, &[]);
    assert!(other.path().join("tpl/stand-up.toml").is_file());
    assert!(!rig.dir.join("templates").exists());
}

// ---------- KB ----------

fn kb_rig(toml_extra: &str) -> (Rig, PathBuf) {
    let kbdir = tempfile::tempdir().unwrap().keep();
    for e in std::fs::read_dir(fixtures().join("kb")).unwrap().flatten() {
        std::fs::copy(e.path(), kbdir.join(e.file_name())).unwrap();
    }
    let toml = format!("[kb]\ndir = {:?}\n{toml_extra}", kbdir.to_str().unwrap());
    (Rig::new(&toml, &[]), kbdir)
}

fn kb_paths(r: &Value) -> Vec<String> {
    r["data"]["results"].as_array().unwrap().iter().map(|x| x["path"].as_str().unwrap().to_string()).collect()
}

#[test]
fn kb_reindex_and_search_over_the_api_is_deterministic() {
    let (rig, _) = kb_rig("");
    let mut c = rig.client();
    assert_eq!(c.call("kb-search", p(&[("query", json!("pricing"))])).unwrap()["error"]["code"], "index_missing");
    let r = c.call("kb-reindex", Map::new()).unwrap();
    assert_eq!(r["data"]["files"], 6, "{r}");
    let q = p(&[("query", json!("annual renewal discount pricing")), ("limit", json!(3))]);
    let a = c.call("kb-search", q.clone()).unwrap();
    assert_eq!(kb_paths(&a)[0], "pricing.md");
    assert_eq!(a["data"]["count"], 3);
    for _ in 0..3 {
        assert_eq!(c.call("kb-search", q.clone()).unwrap()["data"], a["data"]);
    }
    c.call("kb-reindex", Map::new()).unwrap();
    assert_eq!(c.call("kb-search", q).unwrap()["data"], a["data"]);
}

/// A fake embedding endpoint that answers with the local hash embedding, so ranking is exact.
fn embed_mock() -> Mock {
    Mock::start(|path, body| {
        if path.ends_with("/api/embeddings") {
            let v = hash_embed(body["prompt"].as_str().unwrap_or(""));
            (200, json!({"embedding": v}).to_string())
        } else if path.ends_with("/embeddings") {
            let data: Vec<Value> = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| json!({"embedding": hash_embed(t.as_str().unwrap_or(""))}))
                .collect();
            (200, json!({"data": data}).to_string())
        } else {
            (404, "{}".into())
        }
    })
}

#[test]
fn kb_remote_embedders_ollama_and_openai_compatible() {
    let local = {
        let (rig, _) = kb_rig("");
        let mut c = rig.client();
        c.call("kb-reindex", Map::new()).unwrap();
        c.call("kb-search", p(&[("query", json!("security incident response runbook"))])).unwrap()
    };
    for (embedder, path, key) in [("ollama", "/api/embeddings", None), ("openai-compatible", "/embeddings", Some("emb-key"))] {
        let mock = embed_mock();
        let kbdir = tempfile::tempdir().unwrap().keep();
        for e in std::fs::read_dir(fixtures().join("kb")).unwrap().flatten() {
            std::fs::copy(e.path(), kbdir.join(e.file_name())).unwrap();
        }
        let toml = format!(
            "[kb]\ndir = {:?}\nembedder = \"{embedder}\"\nbase_url = \"{}\"\nmodel = \"emb-m\"\n",
            kbdir.to_str().unwrap(),
            mock.base
        );
        let secrets: Vec<(&str, &str)> = key.map(|k| ("llm/api-key", k)).into_iter().collect();
        let rig = Rig::new(&toml, &secrets);
        let mut c = rig.client();
        let r = c.call("kb-reindex", Map::new()).unwrap();
        assert_eq!(r["ok"], true, "{embedder}: {r}");
        assert_eq!(r["data"]["embedder"], format!("{embedder}:emb-m"));
        let s = c.call("kb-search", p(&[("query", json!("security incident response runbook"))])).unwrap();
        assert_eq!(s["ok"], true, "{embedder}: {s}");
        // Same vectors as local hashing, so the same ranking.
        assert_eq!(kb_paths(&s), kb_paths(&local), "{embedder}");
        assert!(mock.requests().iter().any(|r| r.path.ends_with(path)), "{embedder}");
        if let Some(k) = key {
            assert!(mock.requests().iter().all(|r| r.auth.as_deref() == Some(&format!("Bearer {k}"))));
        }
        // A config that changes the embedder makes the old index stale, loudly.
        std::fs::write(&rig.cfg, format!("[kb]\ndir = {:?}\n", kbdir.to_str().unwrap())).unwrap();
        c.call("config-reload", Map::new()).unwrap();
        assert_eq!(c.call("kb-search", p(&[("query", json!("pricing"))])).unwrap()["error"]["code"], "index_stale");
    }
}

#[test]
fn kb_remote_embedder_is_gated_by_network_allow() {
    let rig = Rig::new("[kb]\nembedder = \"ollama\"\nbase_url = \"http://10.255.255.1:11434\"\n", &[]);
    let mut c = rig.client();
    assert_eq!(c.call("kb-reindex", Map::new()).unwrap()["error"]["code"], "network_denied");
    let rig = Rig::new("[kb]\nembedder = \"openai-compatible\"\n", &[]);
    assert_eq!(rig.client().call("kb-reindex", Map::new()).unwrap()["error"]["code"], "base_url_required");
}
