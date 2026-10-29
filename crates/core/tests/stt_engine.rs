//! The transcription engine through the Engine: backend selection (local/cloud), the cloud
//! adapter against a local mock server (never the internet), network and secret gating, the
//! live/final switches, reload vetoes, and the sessions it writes.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use oats_core::action::Action;
use oats_core::config::ConfigError;
use oats_core::control::engine::{Engine, EngineOpts};
use oats_core::stt::wav;
use serde_json::{json, Value};

// ---- mock cloud server ----------------------------------------------------------------------

struct Mock {
    port: u16,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
}

type Handler = Arc<dyn Fn() -> (u16, String, Duration) + Send + Sync>;

impl Mock {
    fn start(h: Handler) -> Mock {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let port = l.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (stop2, seen2) = (Arc::clone(&stop), Arc::clone(&seen));
        thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                match l.accept() {
                    Ok((mut c, _)) => {
                        let (h, seen) = (Arc::clone(&h), Arc::clone(&seen2));
                        thread::spawn(move || {
                            let _ = c.set_nonblocking(false);
                            let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
                            let mut buf = Vec::new();
                            let mut tmp = [0u8; 8192];
                            let (head_end, want) = loop {
                                match c.read(&mut tmp) {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                                }
                                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let head =
                                        String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                                    let len = head
                                        .lines()
                                        .find_map(|l| l.strip_prefix("content-length:"))
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                        .unwrap_or(0);
                                    break (i + 4, len);
                                }
                            };
                            while buf.len() < head_end + want {
                                match c.read(&mut tmp) {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                                }
                            }
                            seen.lock().unwrap().push(buf);
                            let (status, body, delay) = h();
                            thread::sleep(delay);
                            let head = format!(
                                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = c.write_all(head.as_bytes());
                            let _ = c.write_all(body.as_bytes());
                        });
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Mock { port, stop, seen }
    }

    fn ok() -> Mock {
        Mock::start(Arc::new(|| {
            (200, r#"{"text":"hello from the mock","segments":[{"start":0.0,"end":1.5,"text":" hello from the mock "}]}"#.to_string(), Duration::ZERO)
        }))
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
    fn hits(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    fn request(&self, i: usize) -> Vec<u8> {
        self.seen.lock().unwrap()[i].clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

// ---- rig ------------------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/stt")
        .join(name)
}

/// you.wav on the left, them.wav on the right starting one second after "you" ends.
fn call_wav(dir: &Path) -> PathBuf {
    let you = wav::decode(&std::fs::read(fixture("you.wav")).unwrap())
        .unwrap()
        .you;
    let them = wav::decode(&std::fs::read(fixture("them.wav")).unwrap())
        .unwrap()
        .you;
    let gap = 16_000;
    let total = you.len() + gap + them.len();
    let path = dir.join("call.wav");
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..total {
        let l = you.get(i).copied().unwrap_or(0.0);
        let r = if i >= you.len() + gap {
            them[i - you.len() - gap]
        } else {
            0.0
        };
        w.write_sample((l * 32767.0) as i16).unwrap();
        w.write_sample((r * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();
    path
}

struct Rig {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
}

impl Rig {
    fn with_config(toml: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("cfg").join("config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, toml).unwrap();
        let engine = Engine::new(EngineOpts {
            user_config: cfg,
            pack: None,
            os: "linux".into(),
            arch: "x86_64".into(),
            machine: "default".into(),
            sessions_dir: dir.path().join("data").join("sessions"),
            secrets: None,
        })
        .unwrap();
        Rig { dir, engine }
    }

    /// Cloud backend pointed at `mock`, key from the environment variable `OATS_SECRET_STT_<tag>`.
    fn cloud(mock: &Mock, tag: &str, extra: &str) -> Rig {
        std::env::set_var(format!("OATS_SECRET_STT_{tag}"), "tok-123");
        Rig::with_config(&format!(
            "[network]\nallow = true\n[secrets]\nbackend = \"env\"\n[stt]\nbackend = \"cloud\"\n[stt.cloud]\nadapter = \"openai-compatible\"\nurl = \"{}\"\nkey = \"secret:stt/{}\"\ntimeout = \"2s\"\n{extra}",
            mock.url(),
            tag.to_ascii_lowercase()
        ))
    }

    fn transcribe(
        &self,
        path: &Path,
        session: Option<&str>,
        speaker: Option<&str>,
    ) -> Result<Value, ConfigError> {
        self.engine.dispatch(&Action::SttTranscribe {
            path: path.display().to_string(),
            session: session.map(str::to_string),
            speaker: speaker.map(str::to_string),
        })
    }

    fn session_dir(&self, id: &str) -> PathBuf {
        self.dir.path().join("data/sessions").join(id)
    }
}

fn code<T: std::fmt::Debug>(r: Result<T, ConfigError>) -> &'static str {
    r.unwrap_err().code
}

// ---- tests ----------------------------------------------------------------------------------

#[test]
fn cloud_backend_end_to_end_writes_live_and_final_with_you_and_them() {
    let mock = Mock::ok();
    let rig = Rig::cloud(&mock, "E2E", "");
    let rx = rig
        .engine
        .bus
        .subscribe(vec!["segment".into(), "session-saved".into()]);
    let call = call_wav(rig.dir.path());
    let r = rig.transcribe(&call, Some("session_call"), None).unwrap();
    assert_eq!(r["backend"], "openai-compatible");
    assert!(
        r["live_utterances"].as_u64().unwrap() >= 2 && r["final_utterances"].as_u64().unwrap() == 2,
        "{r}"
    );

    let t = rig
        .engine
        .dispatch(&Action::TranscriptGet {
            session: "session_call".into(),
        })
        .unwrap();
    assert_eq!(t["source"], "final");
    let us = t["utterances"].as_array().unwrap();
    assert_eq!(us.len(), 2);
    let speakers: Vec<&str> = us.iter().map(|u| u["speaker"].as_str().unwrap()).collect();
    assert_eq!(speakers, ["you", "remote_1"]); // you speaks first, them after the gap
    assert!(us
        .iter()
        .all(|u| u["text"] == "hello from the mock"
            && u["timestamp"].as_str().unwrap().ends_with('Z')));
    for f in [
        "transcript.live.jsonl",
        "transcript.final.jsonl",
        "session.json",
    ] {
        assert!(rig.session_dir("session_call").join(f).is_file(), "{f}");
    }
    let meta: Value = serde_json::from_str(
        &std::fs::read_to_string(rig.session_dir("session_call").join("session.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["engine"], "openai-compatible:whisper-1");
    assert_eq!(meta["utteranceCount"], 2);
    assert!(meta["endedAt"].is_string() && meta["startedAt"].is_string());
    // The session store reads it back through the normal actions.
    let got = rig
        .engine
        .dispatch(&Action::SessionGet {
            session: "session_call".into(),
        })
        .unwrap();
    assert_eq!(got["utterance_count"], 2);

    // Request shape: bearer from the secret, multipart WAV, model and language fields.
    let req = mock.request(0);
    let head =
        String::from_utf8_lossy(&req[..req.windows(4).position(|w| w == b"\r\n\r\n").unwrap()])
            .to_ascii_lowercase();
    assert!(head.starts_with("post /v1/audio/transcriptions "), "{head}");
    assert!(head.contains("authorization: bearer tok-123"), "{head}");
    assert!(
        head.contains("content-type: multipart/form-data; boundary="),
        "{head}"
    );
    let body = String::from_utf8_lossy(&req).to_string();
    assert!(
        body.contains("name=\"model\"\r\n\r\nwhisper-1"),
        "model field"
    );
    assert!(
        body.contains("name=\"language\"\r\n\r\nen")
            && body.contains("name=\"response_format\"\r\n\r\nverbose_json")
    );
    assert!(req.windows(4).any(|w| w == b"RIFF"));

    let mut kinds = Vec::new();
    while let Ok(l) = rx.try_recv() {
        let v: Value = serde_json::from_str(&l).unwrap();
        kinds.push(v["event"].as_str().unwrap().to_string());
    }
    assert!(
        kinds.iter().filter(|k| *k == "segment").count() >= 4,
        "live + final segments: {kinds:?}"
    );
    assert_eq!(kinds.last().unwrap(), "session-saved");
}

#[test]
fn live_and_final_switches_each_way() {
    for (live, fin) in [(true, true), (true, false), (false, true)] {
        let mock = Mock::ok();
        let rig = Rig::cloud(
            &mock,
            "SW",
            &format!("[stt.live]\nenabled = {live}\n[stt.final]\nenabled = {fin}\n"),
        );
        let call = call_wav(rig.dir.path());
        rig.transcribe(&call, Some("s"), None).unwrap();
        let d = rig.session_dir("s");
        let non_empty = |f: &str| {
            std::fs::read_to_string(d.join(f))
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false)
        };
        assert_eq!(non_empty("transcript.live.jsonl"), live, "{live}/{fin}");
        assert_eq!(non_empty("transcript.final.jsonl"), fin, "{live}/{fin}");
    }
    let mock = Mock::ok();
    let rig = Rig::cloud(
        &mock,
        "SW2",
        "[stt.live]\nenabled = false\n[stt.final]\nenabled = false\n",
    );
    let call = call_wav(rig.dir.path());
    assert_eq!(
        code(rig.transcribe(&call, Some("s"), None)),
        "nothing_enabled"
    );
    assert_eq!(mock.hits(), 0);
}

#[test]
fn mono_input_takes_the_speaker_flag() {
    let mock = Mock::ok();
    let rig = Rig::cloud(&mock, "MONO", "[stt.live]\nenabled = false\n");
    for (flag, want) in [
        (None, "you"),
        (Some("you"), "you"),
        (Some("them"), "remote_1"),
    ] {
        let id = format!("m_{}", flag.unwrap_or("default"));
        rig.transcribe(&fixture("them.wav"), Some(&id), flag)
            .unwrap();
        let t = rig
            .engine
            .dispatch(&Action::TranscriptGet { session: id })
            .unwrap();
        assert_eq!(t["utterances"][0]["speaker"], want);
    }
    assert_eq!(
        code(rig.transcribe(&fixture("them.wav"), Some("bad"), Some("nobody"))),
        "invalid_argument"
    );
    assert_eq!(
        code(rig.transcribe(&fixture("them.wav"), Some("m_default"), None)),
        "exists"
    );
    assert_eq!(
        code(rig.transcribe(Path::new("/nonexistent/x.wav"), None, None)),
        "io_error"
    );
}

#[test]
fn speaker_names_are_config_keys() {
    let mock = Mock::ok();
    let rig = Rig::cloud(&mock, "SPK", "");
    rig.engine
        .set("stt.speaker_you", &json!("me"), "test")
        .unwrap();
    rig.engine
        .set("stt.speaker_them", &json!("caller"), "test")
        .unwrap();
    rig.engine
        .set("stt.live.enabled", &json!(false), "test")
        .unwrap();
    let call = call_wav(rig.dir.path());
    rig.transcribe(&call, Some("s"), None).unwrap();
    let t = rig
        .engine
        .dispatch(&Action::TranscriptGet {
            session: "s".into(),
        })
        .unwrap();
    let speakers: Vec<&str> = t["utterances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["speaker"].as_str().unwrap())
        .collect();
    assert_eq!(speakers, ["me", "caller"]);
}

#[test]
fn cloud_is_denied_without_network_allow_and_before_any_request() {
    let mock = Mock::ok();
    std::env::set_var("OATS_SECRET_STT_DENY", "tok");
    let rig = Rig::with_config(&format!(
        "[secrets]\nbackend = \"env\"\n[stt]\nbackend = \"cloud\"\n[stt.cloud]\nadapter = \"openai-compatible\"\nurl = \"{}\"\nkey = \"secret:stt/deny\"\n",
        mock.url()
    ));
    let call = call_wav(rig.dir.path());
    assert_eq!(
        code(rig.transcribe(&call, Some("s"), None)),
        "network_denied"
    );
    assert_eq!(mock.hits(), 0);
    assert!(
        !rig.session_dir("s").exists(),
        "nothing is created when the backend is refused"
    );
    rig.engine
        .set("network.allow", &json!(true), "test")
        .unwrap();
    rig.transcribe(&call, Some("s"), None).unwrap();
    assert!(mock.hits() > 0);
}

#[test]
fn a_missing_key_fails_before_any_request_and_plaintext_is_vetoed() {
    let mock = Mock::ok();
    let rig = Rig::with_config(&format!(
        "[network]\nallow = true\n[secrets]\nbackend = \"env\"\n[stt]\nbackend = \"cloud\"\n[stt.cloud]\nadapter = \"openai-compatible\"\nurl = \"{}\"\nkey = \"secret:stt/never-set-0001\"\n",
        mock.url()
    ));
    let call = call_wav(rig.dir.path());
    assert_eq!(
        code(rig.transcribe(&call, Some("s"), None)),
        "secret_not_found"
    );
    assert_eq!(mock.hits(), 0);
    let e = rig
        .engine
        .set("stt.cloud.key", &json!("sk-live-plaintext"), "test")
        .unwrap_err();
    assert_eq!(e.code, "secret_plaintext");
    assert!(!e.message.contains("sk-live-plaintext"));
    // An empty reference means no Authorization header (a local OpenAI-compatible server).
    rig.engine.set("stt.cloud.key", &json!(""), "test").unwrap();
    rig.transcribe(&call, Some("s"), None).unwrap();
    let req = String::from_utf8_lossy(&mock.request(0)).to_ascii_lowercase();
    assert!(!req.contains("authorization:"), "{req}");
}

#[test]
fn cloud_failures_have_stable_codes() {
    for (status, body, delay, want) in [
        (
            401,
            r#"{"error":"bad key"}"#,
            Duration::ZERO,
            "stt_http_status",
        ),
        (500, "oops", Duration::ZERO, "stt_http_status"),
        (200, "not json", Duration::ZERO, "stt_bad_response"),
        (200, "{}", Duration::ZERO, "stt_bad_response"),
        (
            200,
            r#"{"text":"late"}"#,
            Duration::from_secs(4),
            "stt_timeout",
        ),
    ] {
        let b = body.to_string();
        let mock = Mock::start(Arc::new(move || (status, b.clone(), delay)));
        let rig = Rig::cloud(&mock, "FAIL", "");
        rig.set_timeout_one();
        let call = call_wav(rig.dir.path());
        assert_eq!(
            code(rig.transcribe(&call, Some("s"), None)),
            want,
            "{status} {body}"
        );
    }
}

impl Rig {
    fn set_timeout_one(&self) {
        self.engine
            .set("stt.cloud.timeout", &json!("1s"), "test")
            .unwrap();
    }
}

#[test]
fn local_backend_reports_what_this_build_can_do() {
    let rig = Rig::with_config("");
    let call = call_wav(rig.dir.path());
    let s = rig.engine.dispatch(&Action::SttStatus {}).unwrap();
    assert_eq!(s["backend"], "local");
    assert_eq!(s["local"]["compiled"], cfg!(feature = "whisper"));
    assert_eq!(s["local"]["present"], false);
    let c = code(rig.transcribe(&call, Some("s"), None));
    if cfg!(feature = "whisper") {
        assert_eq!(c, "model_not_found");
    } else {
        assert_eq!(c, "backend_unavailable");
    }
    assert!(!rig.session_dir("s").exists());
}

#[test]
fn status_never_leaks_the_key() {
    let mock = Mock::ok();
    let rig = Rig::cloud(&mock, "STAT", "");
    let s = rig.engine.dispatch(&Action::SttStatus {}).unwrap();
    assert_eq!(s["backend"], "cloud");
    assert_eq!(s["cloud"]["key"], "set");
    assert_eq!(s["cloud"]["network_allow"], true);
    assert!(!s.to_string().contains("tok-123"));
    rig.engine
        .set("stt.cloud.key", &json!("secret:stt/nope-0002"), "test")
        .unwrap();
    assert_eq!(
        rig.engine.dispatch(&Action::SttStatus {}).unwrap()["cloud"]["key"],
        "missing"
    );
    rig.engine.set("stt.cloud.key", &json!(""), "test").unwrap();
    assert_eq!(
        rig.engine.dispatch(&Action::SttStatus {}).unwrap()["cloud"]["key"],
        "none"
    );
}

#[test]
fn the_subsystem_vetoes_bad_stt_values_and_keeps_the_running_config() {
    let rig = Rig::with_config("");
    for (key, bad) in [("stt.language", ""), ("stt.speaker_you", "remote_1")] {
        let e = rig.engine.set(key, &json!(bad), "test").unwrap_err();
        assert!(e.message.starts_with("stt:"), "{key}={bad}: {e}");
        assert_eq!(e.code, "invalid_value", "{key}={bad}");
        rig.engine.dispatch(&Action::ConfigCheck {}).unwrap();
    }
    // stt.local.model is an enum: anything outside the pinned set is refused by the key table.
    for bad in ["../escape", "large-v3", "base.EN", ""] {
        let e = rig
            .engine
            .set("stt.local.model", &json!(bad), "test")
            .unwrap_err();
        assert_eq!(e.code, "invalid_value", "model={bad}: {e}");
    }
    // Cloud-only checks apply once the cloud backend is selected.
    rig.engine
        .set("stt.cloud.adapter", &json!("openai-compatible"), "test")
        .unwrap();
    rig.engine
        .set("stt.cloud.url", &json!("ftp://nope"), "test")
        .unwrap(); // local backend: not checked
    let e = rig
        .engine
        .set("stt.backend", &json!("cloud"), "test")
        .unwrap_err();
    assert_eq!(e.code, "invalid_value");
    assert_eq!(
        rig.engine.dispatch(&Action::SttStatus {}).unwrap()["backend"],
        "local"
    );
    rig.engine
        .set("stt.cloud.url", &json!("http://127.0.0.1:1/v1"), "test")
        .unwrap();
    rig.engine
        .set("stt.backend", &json!("cloud"), "test")
        .unwrap();
    assert_eq!(
        rig.engine.dispatch(&Action::SttStatus {}).unwrap()["backend"],
        "cloud"
    );
}

#[test]
fn engine_label_and_values_follow_config() {
    let mock = Mock::ok();
    let rig = Rig::cloud(&mock, "LBL", "");
    rig.engine
        .set("stt.cloud.model", &json!("whisper-large"), "test")
        .unwrap();
    rig.engine
        .set("stt.language", &json!("de"), "test")
        .unwrap();
    rig.engine
        .set("stt.live.enabled", &json!(false), "test")
        .unwrap();
    let call = call_wav(rig.dir.path());
    rig.transcribe(&call, Some("s"), None).unwrap();
    let body = String::from_utf8_lossy(&mock.request(0)).to_string();
    assert!(
        body.contains("name=\"model\"\r\n\r\nwhisper-large")
            && body.contains("name=\"language\"\r\n\r\nde")
    );
    let meta: Value = serde_json::from_str(
        &std::fs::read_to_string(rig.session_dir("s").join("session.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["engine"], "openai-compatible:whisper-large");
}
