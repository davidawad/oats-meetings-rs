//! Fetching the local whisper model (dot-4uzgol.26). `stt-model-fetch` (and, when
//! `stt.local.auto_download` is on, the first recording that finds the model missing) downloads
//! `ggml-<stt.local.model>.bin` from `stt.local.model_url` into the models directory, checks it
//! against the SHA-256 pinned below (the same pins as ci/fetch-models.sh; a test keeps them in
//! sync), and only then moves it into place. Progress is a `model-download` bus event and the
//! `download` object of `stt-status`. The download runs on its own thread; one per target file.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::Settings;
use crate::config::{ConfigError, Result};
use crate::control::bus::Bus;

/// (model name, SHA-256 of ggml-<name>.bin). Keep in sync with ci/fetch-models.sh.
pub const PINS: &[(&str, &str)] = &[
    (
        "tiny.en",
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    ),
    (
        "tiny",
        "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    ),
    (
        "base.en",
        "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
    ),
    (
        "base",
        "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    ),
    (
        "small.en",
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
    ),
    (
        "small",
        "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    ),
];

pub fn pin(model: &str) -> Option<&'static str> {
    PINS.iter().find(|p| p.0 == model).map(|p| p.1)
}

/// State of the download into one file, as `stt-status` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub model: String,
    /// `downloading`, `done` or `failed`.
    pub state: &'static str,
    pub done: u64,
    pub total: Option<u64>,
    pub error: Option<(String, String)>,
}

impl Progress {
    fn new(model: &str, state: &'static str, done: u64, total: Option<u64>) -> Progress {
        Progress {
            model: model.to_string(),
            state,
            done,
            total,
            error: None,
        }
    }

    pub fn to_json(&self) -> Value {
        let pct = self
            .total
            .filter(|t| *t > 0)
            .map(|t| (self.done * 100 / t).min(100));
        let mut v = json!({"model": self.model, "state": self.state, "bytes": self.done, "total": self.total, "percent": pct});
        if let Some((code, msg)) = &self.error {
            v["error"] = json!({"code": code, "message": msg});
        }
        v
    }
}

fn table() -> &'static (Mutex<BTreeMap<PathBuf, Progress>>, Condvar) {
    static T: OnceLock<(Mutex<BTreeMap<PathBuf, Progress>>, Condvar)> = OnceLock::new();
    T.get_or_init(Default::default)
}

/// Block until no download into `file` is running.
pub fn wait_idle(file: &Path) {
    let (m, cv) = table();
    let mut g = m.lock().expect("model table");
    while g.get(file).is_some_and(|p| p.state == "downloading") {
        g = cv.wait(g).expect("model table");
    }
}

/// The latest download state for `file`, if one was started in this process.
pub fn progress(file: &Path) -> Option<Progress> {
    table().0.lock().expect("model table").get(file).cloned()
}

pub fn is_downloading(file: &Path) -> bool {
    progress(file).is_some_and(|p| p.state == "downloading")
}

fn record(file: &Path, p: Progress) {
    table()
        .0
        .lock()
        .expect("model table")
        .insert(file.to_path_buf(), p);
    table().1.notify_all();
}

fn url_for(s: &Settings) -> String {
    format!("{}/ggml-{}.bin", s.model_url.trim_end_matches('/'), s.model)
}

/// Whether a download can serve these settings (returns the pinned checksum), else why not.
pub fn check(s: &Settings) -> Result<&'static str> {
    if s.engine != crate::config::typed::SttEngine::Whisper {
        return Err(ConfigError::new(
            "unsupported_engine",
            "only whisper models can be downloaded here; fetch the Parakeet files with ci/fetch-models.sh or set stt.local.model_path",
        ));
    }
    if s.explicit_model_path {
        return Err(ConfigError::new(
            "unsupported_engine",
            "stt.local.model_path is set: put the model file there yourself; nothing is downloaded for an explicit path",
        ));
    }
    pin(&s.model).ok_or_else(|| {
        ConfigError::new(
            "invalid_value",
            format!("stt.local.model '{}' has no pinned checksum", s.model),
        )
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn stream(url: &str, part: &Path, sha: &str, mut tick: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let io = |e: std::io::Error| ConfigError::new("io_error", format!("{}: {e}", part.display()));
    let net = |e: &dyn std::fmt::Display| {
        ConfigError::new("model_download_failed", format!("{url}: {e}"))
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| net(&e))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(net(&format!("HTTP {status}")));
    }
    let total = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
    let mut out = std::fs::File::create(part).map_err(io)?;
    let (mut hash, mut done, mut buf) = (Sha256::new(), 0u64, vec![0u8; 1 << 16]);
    loop {
        let n = body.read(&mut buf).map_err(|e| net(&e))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(io)?;
        done += n as u64;
        tick(done, total);
    }
    out.sync_all().map_err(io)?;
    let got = hex(&hash.finalize());
    if got != sha {
        return Err(ConfigError::new(
            "model_checksum_mismatch",
            format!("{url}: sha256 {got}, expected {sha}; the file was discarded"),
        ));
    }
    Ok(())
}

/// Download and verify the model for `s` (blocking). Publishes `model-download` events.
pub fn run(s: &Settings, bus: Option<&Bus>) -> Result<Value> {
    run_pinned(s, bus, check(s)?)
}

fn run_pinned(s: &Settings, bus: Option<&Bus>, sha: &str) -> Result<Value> {
    let file = s.model_file.clone();
    let url = url_for(s);
    let publish = |p: &Progress| {
        if let Some(b) = bus {
            b.publish("model-download", p.to_json());
        }
    };
    let start = Progress::new(&s.model, "downloading", 0, None);
    record(&file, start.clone());
    publish(&start);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", dir.display())))?;
    }
    let part = file.with_extension("bin.part");
    let mut last_pct = u64::MAX;
    let r = stream(&url, &part, sha, |done, total| {
        let p = Progress::new(&s.model, "downloading", done, total);
        let pct = total
            .filter(|t| *t > 0)
            .map_or(done >> 22, |t| done * 100 / t);
        record(&file, p.clone());
        if pct != last_pct {
            last_pct = pct;
            publish(&p);
        }
    })
    .and_then(|()| {
        std::fs::rename(&part, &file)
            .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", file.display())))
    });
    match r {
        Ok(()) => {
            let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            let done = Progress::new(&s.model, "done", size, Some(size));
            record(&file, done.clone());
            publish(&done);
            Ok(
                json!({"model": s.model, "path": file, "bytes": size, "sha256": sha, "state": "done"}),
            )
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            let mut p = Progress::new(&s.model, "failed", 0, None);
            p.error = Some((e.code.to_string(), e.message.clone()));
            record(&file, p.clone());
            publish(&p);
            Err(e)
        }
    }
}

/// Start `run` on its own thread unless the file is present or already being fetched. Returns the
/// status to show.
pub fn spawn(s: &Settings, bus: Option<std::sync::Arc<Bus>>) -> Result<Value> {
    check(s)?;
    if s.model_file.is_file() {
        return Ok(json!({"model": s.model, "path": s.model_file, "state": "present"}));
    }
    if is_downloading(&s.model_file) {
        return Ok(progress(&s.model_file)
            .map(|p| p.to_json())
            .unwrap_or(Value::Null));
    }
    let start = Progress::new(&s.model, "downloading", 0, None);
    record(&s.model_file, start.clone());
    let s2 = s.clone();
    std::thread::Builder::new()
        .name("oats-model-download".into())
        .spawn(move || {
            let _ = run(&s2, bus.as_deref());
        })
        .map_err(|e| {
            ConfigError::new("io_error", format!("cannot start the download thread: {e}"))
        })?;
    Ok(start.to_json())
}

fn known(model: &str) -> Result<&'static str> {
    pin(model).ok_or_else(|| {
        let names: Vec<&str> = PINS.iter().map(|p| p.0).collect();
        ConfigError::new(
            "invalid_value",
            format!(
                "'{model}' is not a downloadable model ({})",
                names.join(", ")
            ),
        )
    })
}

fn file_in(s: &Settings, model: &str) -> PathBuf {
    s.models_dir.join(format!("ggml-{model}.bin"))
}

/// `s` retargeted at another pinned model in the same models directory (`stt-model-fetch model=`).
pub fn for_model(s: &Settings, model: &str) -> Result<Settings> {
    known(model)?;
    let mut t = s.clone();
    t.model = model.to_string();
    if !t.explicit_model_path {
        t.model_file = file_in(s, model);
    }
    Ok(t)
}

/// Every pinned model with whether its file is in the models directory, for `stt-model-list`.
pub fn list(s: &Settings) -> Value {
    let models: Vec<Value> = PINS
        .iter()
        .map(|(name, _)| {
            let file = file_in(s, name);
            let bytes = std::fs::metadata(&file)
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len());
            json!({
                "name": name, "path": file, "present": bytes.is_some(), "bytes": bytes,
                "selected": *name == s.model,
                "download": progress(&file).map(|p| p.to_json()),
            })
        })
        .collect();
    let engine = match s.engine {
        crate::config::typed::SttEngine::Whisper => "whisper",
        crate::config::typed::SttEngine::Parakeet => "parakeet",
    };
    json!({
        "selected": s.model, "engine": engine, "models_dir": s.models_dir,
        "explicit_path": s.explicit_model_path.then_some(&s.model_file), "models": models,
    })
}

/// Remove a downloaded model file; refused while it is downloading. Publishes `model-download`
/// with state `deleted`.
pub fn delete(s: &Settings, model: &str, bus: Option<&Bus>) -> Result<Value> {
    known(model)?;
    let file = file_in(s, model);
    if is_downloading(&file) {
        return Err(ConfigError::new(
            "model_downloading",
            format!("the {model} model is downloading; wait for it to finish"),
        ));
    }
    if !file.is_file() {
        return Err(ConfigError::new(
            "model_not_found",
            format!("{} is not downloaded", file.display()),
        ));
    }
    std::fs::remove_file(&file)
        .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", file.display())))?;
    table().0.lock().expect("model table").remove(&file);
    let v = json!({"model": model, "path": file, "state": "deleted"});
    if let Some(b) = bus {
        b.publish("model-download", v.clone());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn serve(body: Vec<u8>, status: u16) -> (String, std::thread::JoinHandle<()>) {
        let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", srv.server_addr().to_ip().unwrap());
        let h = std::thread::spawn(move || {
            if let Ok(req) = srv.recv() {
                let _ = req.respond(tiny_http::Response::from_data(body).with_status_code(status));
            }
        });
        (url, h)
    }

    fn settings(dir: &Path, url: &str, over: &[(&str, Value)]) -> Settings {
        let mut c = crate::config::resolve::defaults_flat();
        c.insert("stt.local.models_dir".into(), json!(dir.to_str().unwrap()));
        c.insert("stt.local.model_url".into(), json!(url));
        for (k, v) in over {
            c.insert((*k).into(), v.clone());
        }
        let cfg: crate::config::typed::Config =
            serde_json::from_value(crate::config::resolve::nest(&c)).unwrap();
        Settings::from_config(&cfg, &dir.join("sessions")).unwrap()
    }

    #[test]
    fn download_verifies_the_checksum_publishes_progress_and_moves_into_place() {
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let sha = hex(&Sha256::digest(&body));
        let dir = tempfile::tempdir().unwrap();
        let (url, h) = serve(body.clone(), 200);
        let s = settings(dir.path(), &url, &[]);
        let bus = Bus::default();
        let rx = bus.subscribe(vec!["model-download".into()]);
        let r = run_pinned(&s, Some(&bus), &sha).unwrap();
        h.join().unwrap();
        assert_eq!(r["state"], "done");
        assert_eq!(std::fs::read(&s.model_file).unwrap(), body);
        assert!(!s.model_file.with_extension("bin.part").exists());
        let events: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|l| serde_json::from_str::<Value>(&l).unwrap()["data"].clone())
            .collect();
        assert_eq!(events.first().unwrap()["state"], "downloading");
        assert_eq!(events.last().unwrap()["state"], "done");
        assert_eq!(progress(&s.model_file).unwrap().state, "done");
    }

    #[test]
    fn wrong_bytes_are_discarded_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (url, h) = serve(b"not a model".to_vec(), 200);
        let s = settings(dir.path(), &url, &[]);
        let e = run(&s, None).unwrap_err();
        h.join().unwrap();
        assert_eq!(e.code, "model_checksum_mismatch");
        assert!(!s.model_file.exists() && !s.model_file.with_extension("bin.part").exists());
        let p = progress(&s.model_file).unwrap();
        assert_eq!(
            (p.state, p.error.unwrap().0.as_str()),
            ("failed", "model_checksum_mismatch")
        );
    }

    #[test]
    fn http_errors_and_unsupported_settings_have_codes() {
        let dir = tempfile::tempdir().unwrap();
        let (url, h) = serve(b"nope".to_vec(), 404);
        let e = run(&settings(dir.path(), &url, &[]), None).unwrap_err();
        h.join().unwrap();
        assert_eq!(e.code, "model_download_failed");
        assert!(e.message.contains("404"), "{}", e.message);
        let e = run(
            &settings(
                dir.path(),
                "http://127.0.0.1:9",
                &[("stt.local.model_path", json!("/x/m.bin"))],
            ),
            None,
        )
        .unwrap_err();
        assert_eq!(e.code, "unsupported_engine");
        let e = run(
            &settings(
                dir.path(),
                "http://127.0.0.1:9",
                &[("stt.local.engine", json!("parakeet"))],
            ),
            None,
        )
        .unwrap_err();
        assert_eq!(e.code, "unsupported_engine");
    }

    #[test]
    fn pins_match_the_allowed_models_and_ci_script() {
        let names: Vec<&str> = PINS.iter().map(|p| p.0).collect();
        assert_eq!(names, crate::config::keys::STT_LOCAL_MODELS);
        let script = include_str!("../../../../ci/fetch-models.sh");
        for (n, sha) in PINS {
            assert!(
                script.contains(&format!("[{n}]={sha}")),
                "ci/fetch-models.sh pin for {n}"
            );
        }
    }

    #[test]
    fn list_reports_presence_and_delete_removes_only_a_known_idle_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings(
            dir.path(),
            "http://127.0.0.1:9",
            &[("stt.local.model", json!("tiny.en"))],
        );
        let v = list(&s);
        assert_eq!(v["selected"], "tiny.en");
        assert_eq!(v["models"].as_array().unwrap().len(), PINS.len());
        assert!(v["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["present"] == false));
        std::fs::write(dir.path().join("ggml-small.bin"), b"abc").unwrap();
        let v = list(&s);
        let small = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "small")
            .unwrap()
            .clone();
        assert_eq!(
            (
                small["present"].as_bool(),
                small["bytes"].as_u64(),
                small["selected"].as_bool()
            ),
            (Some(true), Some(3), Some(false))
        );
        assert_eq!(delete(&s, "../x", None).unwrap_err().code, "invalid_value");
        assert_eq!(
            delete(&s, "base", None).unwrap_err().code,
            "model_not_found"
        );
        let bus = Bus::default();
        let rx = bus.subscribe(vec!["model-download".into()]);
        assert_eq!(delete(&s, "small", Some(&bus)).unwrap()["state"], "deleted");
        assert!(!dir.path().join("ggml-small.bin").exists());
        assert!(rx.try_recv().unwrap().contains("deleted"));
        let t = for_model(&s, "base").unwrap();
        assert_eq!(
            (t.model.as_str(), t.model_file.clone()),
            ("base", dir.path().join("ggml-base.bin"))
        );
        assert_eq!(for_model(&s, "huge").unwrap_err().code, "invalid_value");
    }
}
