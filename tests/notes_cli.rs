//! The acceptance commands through the real binary: a headless engine plus `ctl` processes, talking
//! to a local mock LLM server.
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

const EXE: &str = env!("CARGO_BIN_EXE_oats-meetings-rs");
const SESSION: &str = "session_2026-10-20_09-00-00";
const NOTES: &str = "## CRM\nContact Name: Dana\n\n## Summary\nFine.\n\n## Next Step\nSend the form.";

struct Mock {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Mock {
    fn start() -> Mock {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let base = format!("http://{}", server.server_addr().to_ip().unwrap());
        let hits: Arc<Mutex<Vec<String>>> = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (h2, s2) = (Arc::clone(&hits), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(50)) else { continue };
                let mut raw = String::new();
                let _ = req.as_reader().read_to_string(&mut raw);
                h2.lock().unwrap().push(req.url().to_string());
                let body = serde_json::json!({"message": {"role": "assistant", "content": NOTES}}).to_string();
                let _ = req.respond(tiny_http::Response::from_string(body));
            }
        });
        Mock { base, hits, stop }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

struct Engine {
    child: Child,
    rt: String,
    cfg: String,
    _dir: tempfile::TempDir,
}

fn cmd(rt: &str, cfg: &str) -> Command {
    let mut c = Command::new(EXE);
    c.env("OATS_DATA_DIR", format!("{rt}/../data"))
        .env("OATS_RUNTIME_DIR", rt)
        .env("OATS_CONFIG", cfg)
        .env("OATS_OS", "linux")
        .env_remove("OATS_PACK")
        .env_remove("OATS_LLM_API_KEY");
    c
}

impl Engine {
    fn start(base: &str) -> Engine {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join("rt").to_str().unwrap().to_string();
        let cfg = dir.path().join("config.toml").to_str().unwrap().to_string();
        std::fs::write(&cfg, format!("[llm]\nbackend = \"ollama\"\nbase_url = \"{base}\"\nmodel = \"m\"\n")).unwrap();
        let child = cmd(&rt, &cfg).arg("--headless").stderr(Stdio::null()).spawn().unwrap();
        let e = Engine { child, rt, cfg, _dir: dir };
        let start = Instant::now();
        while e.ctl(&["ping"]).0 != 0 {
            assert!(start.elapsed() < Duration::from_secs(30), "engine did not come up");
            std::thread::sleep(Duration::from_millis(50));
        }
        e
    }

    fn ctl(&self, args: &[&str]) -> (i32, String, String) {
        let o = cmd(&self.rt, &self.cfg).arg("ctl").args(args).output().unwrap();
        (
            o.status.code().unwrap(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fx(rel: &str) -> String {
    format!("{}/tests/fixtures/{rel}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn ctl_notes_generate_with_the_sales_template_emits_the_crm_headings() {
    let mock = Mock::start();
    let e = Engine::start(&mock.base);
    assert_eq!(e.ctl(&["session", "import", "--from", &fx("openoats")]).0, 0);
    let (code, out, err) = e.ctl(&["template", "import", "--from", &fx("openoats-templates.json")]);
    assert_eq!(code, 0, "{out} {err}");
    let (code, out, err) = e.ctl(&["--json", "notes", "generate", "--template", "Sales Prospect Call", "--session", SESSION]);
    assert_eq!(code, 0, "{out} {err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    let notes = v["data"]["notes"].as_str().unwrap();
    for h in ["## CRM", "## Summary", "## Next Step"] {
        assert!(notes.lines().any(|l| l == h), "missing {h} in {notes}");
    }
    assert_eq!(mock.hits.lock().unwrap().as_slice(), ["/api/chat"]);
    // The notes are in the session.
    let (_, out, _) = e.ctl(&["--json", "session", "get", SESSION]);
    let g: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(g["data"]["metadata"]["hasNotes"], true);
}

#[test]
fn ctl_cloud_backend_with_network_allow_off_is_network_denied() {
    let mock = Mock::start();
    let e = Engine::start(&mock.base);
    e.ctl(&["session", "import", "--from", &fx("openoats")]);
    e.ctl(&["template", "import", "--from", &fx("openoats-templates.json")]);
    assert_eq!(e.ctl(&["config", "set", "llm.backend", "openrouter"]).0, 0);
    let (code, out, _) = e.ctl(&["--json", "notes", "generate", "--template", "Generic", "--session", SESSION]);
    assert_eq!(code, 1);
    assert!(out.contains("network_denied"), "{out}");
    assert!(mock.hits.lock().unwrap().is_empty());
    // Dry run works with the gate closed and says what would happen.
    let (code, out, _) = e.ctl(&["--json", "notes", "generate", "--template", "Generic", "--session", SESSION, "--dry_run"]);
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["data"]["would_be_denied"], true);
}

#[test]
fn ctl_kb_search_ranking_is_deterministic() {
    let mock = Mock::start();
    let e = Engine::start(&mock.base);
    let kb = format!("{}/../kb", e.rt);
    std::fs::create_dir_all(&kb).unwrap();
    for f in std::fs::read_dir(fx("kb")).unwrap().flatten() {
        std::fs::copy(f.path(), format!("{kb}/{}", f.file_name().to_string_lossy())).unwrap();
    }
    assert_eq!(e.ctl(&["kb", "reindex"]).0, 0);
    let run = || e.ctl(&["--json", "kb", "search", "annual renewal discount pricing", "--limit", "3"]).1;
    let first = run();
    let v: Value = serde_json::from_str(first.trim()).unwrap();
    assert_eq!(v["data"]["results"][0]["path"], "pricing.md");
    assert_eq!(v["data"]["count"], 3);
    assert_eq!(first, run());
    assert_eq!(first, run());
}
