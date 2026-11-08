//! The local model manager (dot-4uzgol.26, generalised to the catalog in dot-4uzgol.66).
//! `stt-model-fetch` (and, when `stt.local.auto_download` is on, app launch or a recording that
//! finds the model missing) downloads every file of the selected catalog entry (`catalog`) from its
//! catalog URL, or from the `stt.local.model_url` mirror, checks each against the catalog SHA-256,
//! and only then moves it into place: a whisper.cpp model is one `ggml-*.bin` file in the models
//! directory, a sherpa-onnx model a directory of files, a coreml model a directory of `.mlmodelc`
//! bundles (files in sub-directories) and its vocabulary. Progress is a `model-download` bus event and
//! the `download` object of `stt-status`. The download runs on its own thread; one per model. When
//! it lands, the engine transcribes the recordings saved as audio only meanwhile (dot-4uzgol.44).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::catalog::{self, Entry};
use super::Settings;
use crate::config::{ConfigError, Result};
use crate::control::bus::Bus;

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

/// One file to download: from where, under which name, its checksum and catalog size.
#[derive(Clone, Debug)]
struct Fetch {
    url: String,
    name: String,
    sha: String,
    bytes: u64,
}

/// The files of the model `s` selects, with the mirror applied.
fn plan(s: &Settings) -> Vec<Fetch> {
    let base = s.model_url.trim_end_matches('/');
    s.entry
        .files
        .iter()
        .map(|f| Fetch {
            url: match (base.is_empty(), s.entry.is_dir()) {
                (true, _) => f.url.clone(),
                (false, false) => format!("{base}/{}", f.name),
                (false, true) => format!("{base}/{}/{}", s.entry.dir, f.name),
            },
            name: f.name.clone(),
            sha: f.sha256.clone(),
            bytes: f.bytes,
        })
        .collect()
}

/// Whether a download can serve these settings, else why not.
pub fn check(s: &Settings) -> Result<()> {
    if s.explicit_model_path {
        return Err(ConfigError::new(
            "unsupported_engine",
            "stt.local.model_path is set: put the model there yourself; nothing is downloaded for an explicit path",
        ));
    }
    Ok(())
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
    check(s)?;
    fetch(s, bus, &plan(s))
}

/// Where a file being downloaded is written until it is verified: `<file>.part`, or, for a
/// directory model, the file inside `<dir>.part/`.
fn staging(s: &Settings) -> PathBuf {
    let mut name = s.model_file.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    s.model_file.with_file_name(name)
}

fn io_err(p: &Path) -> impl Fn(std::io::Error) -> ConfigError + '_ {
    move |e| ConfigError::new("io_error", format!("{}: {e}", p.display()))
}

fn fetch(s: &Settings, bus: Option<&Bus>, files: &[Fetch]) -> Result<Value> {
    let target = s.model_file.clone();
    let dir_model = s.entry.is_dir();
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    let publish = |p: &Progress| {
        if let Some(b) = bus {
            b.publish("model-download", p.to_json());
        }
    };
    let start = Progress::new(&s.model, "downloading", 0, Some(total));
    record(&target, start.clone());
    publish(&start);
    let stage = staging(s);
    let r = (|| -> Result<u64> {
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(io_err(dir))?;
        }
        if dir_model {
            std::fs::create_dir_all(&stage).map_err(io_err(&stage))?;
        }
        let (mut base, mut last_pct) = (0u64, u64::MAX);
        for f in files {
            let part = if dir_model {
                stage.join(&f.name)
            } else {
                stage.clone()
            };
            // A coreml file sits inside its .mlmodelc bundle (`Encoder.mlmodelc/weights/...`).
            if let Some(dir) = part.parent() {
                std::fs::create_dir_all(dir).map_err(io_err(dir))?;
            }
            let mut got = 0;
            stream(&f.url, &part, &f.sha, |done, _| {
                got = done;
                let all = base + done;
                let p = Progress::new(&s.model, "downloading", all, Some(total.max(all)));
                let pct = all * 100 / total.max(all).max(1);
                record(&target, p.clone());
                if pct != last_pct {
                    last_pct = pct;
                    publish(&p);
                }
            })?;
            base += got;
        }
        if dir_model {
            std::fs::create_dir_all(&target).map_err(io_err(&target))?;
            for f in files {
                let to = target.join(&f.name);
                if let Some(dir) = to.parent() {
                    std::fs::create_dir_all(dir).map_err(io_err(dir))?;
                }
                std::fs::rename(stage.join(&f.name), &to).map_err(io_err(&to))?;
            }
            std::fs::remove_dir_all(&stage).map_err(io_err(&stage))?;
        } else {
            std::fs::rename(&stage, &target).map_err(io_err(&target))?;
        }
        Ok(base)
    })();
    match r {
        Ok(size) => {
            let done = Progress::new(&s.model, "done", size, Some(size));
            record(&target, done.clone());
            publish(&done);
            Ok(
                json!({"model": s.model, "path": target, "bytes": size, "files": files.len(), "state": "done"}),
            )
        }
        Err(e) => {
            let _ = if dir_model {
                std::fs::remove_dir_all(&stage)
            } else {
                std::fs::remove_file(&stage)
            };
            let mut p = Progress::new(&s.model, "failed", 0, None);
            p.error = Some((e.code.to_string(), e.message.clone()));
            record(&target, p.clone());
            publish(&p);
            Err(e)
        }
    }
}

/// Start `run` on its own thread unless the model is present or already being fetched; `then`
/// runs on that thread once the model is in place. Returns the status to show.
pub fn spawn(
    s: &Settings,
    bus: Option<std::sync::Arc<Bus>>,
    then: Box<dyn FnOnce() + Send>,
) -> Result<Value> {
    check(s)?;
    if s.entry.present(&s.model_file) {
        return Ok(json!({"model": s.model, "path": s.model_file, "state": "present"}));
    }
    if is_downloading(&s.model_file) {
        return Ok(progress(&s.model_file)
            .map(|p| p.to_json())
            .unwrap_or(Value::Null));
    }
    let start = Progress::new(&s.model, "downloading", 0, Some(s.entry.bytes()));
    record(&s.model_file, start.clone());
    let s2 = s.clone();
    std::thread::Builder::new()
        .name("oats-model-download".into())
        .spawn(move || {
            if run(&s2, bus.as_deref()).is_ok() {
                then();
            }
        })
        .map_err(|e| {
            ConfigError::new("io_error", format!("cannot start the download thread: {e}"))
        })?;
    Ok(start.to_json())
}

/// A speech model of the catalog, or a speaker-embedding model of the diarizer's.
fn known(model: &str) -> Result<&'static Entry> {
    catalog::get(model)
        .or_else(|| catalog::speaker(model))
        .ok_or_else(|| {
            let ids: Vec<&str> = catalog::all().iter().map(|e| e.id.as_str()).collect();
            let msg = crate::config::keys::retired_stt_model(model).unwrap_or_else(|| {
                format!("'{model}' is not a catalog model ({})", ids.join(", "))
            });
            ConfigError::new("invalid_value", msg)
        })
}

/// `s` retargeted at another catalog model in the same models directory (`stt-model-fetch model=`).
pub fn for_model(s: &Settings, model: &str) -> Result<Settings> {
    let entry = known(model)?;
    let mut t = s.clone();
    t.entry = entry;
    t.model = model.to_string();
    // stt.local.model_path names a speech model; a speaker model always lives in models_dir.
    if catalog::get(model).is_none() {
        t.explicit_model_path = false;
    }
    if !t.explicit_model_path {
        t.model_file = entry.location(&s.models_dir);
    }
    Ok(t)
}

/// Bytes of `entry`'s files present at `path`, or `None` when it is not all there.
fn on_disk(entry: &Entry, path: &Path) -> Option<u64> {
    if !entry.present(path) {
        return None;
    }
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    Some(if entry.is_dir() {
        entry.files.iter().map(|f| size(&path.join(&f.name))).sum()
    } else {
        size(path)
    })
}

/// Every catalog model, whatever its runtime, with its catalog facts (size, WER, RTFx, languages),
/// whether this build can run it, whether it is downloaded and whether it is selected, for
/// `stt-model-list` and the Models tab.
pub fn list(s: &Settings) -> Value {
    let models: Vec<Value> = catalog::all()
        .iter()
        .map(|e| {
            let path = e.location(&s.models_dir);
            let bytes = on_disk(e, &path);
            let mut v = e.to_json();
            v["path"] = json!(path);
            v["present"] = json!(bytes.is_some());
            v["bytes"] = json!(bytes);
            v["selected"] = json!(e.id == s.model);
            v["download"] = json!(progress(&path).map(|p| p.to_json()));
            v
        })
        .collect();
    json!({
        "selected": s.model, "runtime": s.entry.runtime.label(), "models_dir": s.models_dir,
        "explicit_path": s.explicit_model_path.then_some(&s.model_file),
        "auto_download": s.auto_download, "models": models,
    })
}

/// Remove a downloaded model (its file, or its directory); refused while it is downloading.
/// Publishes `model-download` with state `deleted`.
pub fn delete(s: &Settings, model: &str, bus: Option<&Bus>) -> Result<Value> {
    let entry = known(model)?;
    let path = entry.location(&s.models_dir);
    if is_downloading(&path) {
        return Err(ConfigError::new(
            "model_downloading",
            format!("the {model} model is downloading; wait for it to finish"),
        ));
    }
    let gone = || {
        ConfigError::new(
            "model_not_found",
            format!("{} is not downloaded", path.display()),
        )
    };
    if entry.is_dir() {
        if !path.is_dir() {
            return Err(gone());
        }
        std::fs::remove_dir_all(&path).map_err(io_err(&path))?;
    } else {
        if !path.is_file() {
            return Err(gone());
        }
        std::fs::remove_file(&path).map_err(io_err(&path))?;
    }
    table().0.lock().expect("model table").remove(&path);
    let v = json!({"model": model, "path": path, "state": "deleted"});
    if let Some(b) = bus {
        b.publish("model-download", v.clone());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Serves `files` (url path -> (status, body)) until `n` requests are answered.
    fn serve(
        files: Vec<(&str, u16, Vec<u8>)>,
        n: usize,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let srv = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", srv.server_addr().to_ip().unwrap());
        let files: Vec<(String, u16, Vec<u8>)> = files
            .into_iter()
            .map(|(p, st, b)| (p.to_string(), st, b))
            .collect();
        let h = std::thread::spawn(move || {
            let mut seen = vec![];
            for _ in 0..n {
                let Ok(req) = srv.recv() else { break };
                seen.push(req.url().to_string());
                let (st, body) = files
                    .iter()
                    .find(|f| f.0 == req.url())
                    .map_or((404, b"missing".to_vec()), |f| (f.1, f.2.clone()));
                let _ = req.respond(tiny_http::Response::from_data(body).with_status_code(st));
            }
            seen
        });
        (url, h)
    }

    fn settings(dir: &Path, url: &str, over: &[(&str, Value)]) -> Settings {
        let mut c = crate::config::resolve::defaults_flat();
        c.insert("stt.local.models_dir".into(), json!(dir.to_str().unwrap()));
        c.insert("stt.local.model_url".into(), json!(url));
        c.insert("stt.local.model".into(), json!("tiny.en"));
        for (k, v) in over {
            c.insert((*k).into(), v.clone());
        }
        let cfg: crate::config::typed::Config =
            serde_json::from_value(crate::config::resolve::nest(&c)).unwrap();
        Settings::from_config(&cfg, &dir.join("sessions")).unwrap()
    }

    fn body(n: u32, salt: u32) -> Vec<u8> {
        (0..n).map(|i| ((i + salt) % 251) as u8).collect()
    }

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    #[test]
    fn download_verifies_the_checksum_publishes_progress_and_moves_into_place() {
        let b = body(300_000, 0);
        let dir = tempfile::tempdir().unwrap();
        let (url, h) = serve(vec![("/ggml-tiny.en.bin", 200, b.clone())], 1);
        let s = settings(dir.path(), &url, &[]);
        let bus = Bus::default();
        let rx = bus.subscribe(vec!["model-download".into()]);
        let mut files = plan(&s);
        assert_eq!(
            files[0].url,
            format!("{url}/ggml-tiny.en.bin"),
            "mirror URL"
        );
        files[0].sha = sha(&b);
        let r = fetch(&s, Some(&bus), &files).unwrap();
        h.join().unwrap();
        assert_eq!(r["state"], "done");
        assert_eq!(std::fs::read(&s.model_file).unwrap(), b);
        assert!(!staging(&s).exists());
        let events: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|l| serde_json::from_str::<Value>(&l).unwrap()["data"].clone())
            .collect();
        assert_eq!(events.first().unwrap()["state"], "downloading");
        assert_eq!(events.last().unwrap()["state"], "done");
        assert_eq!(progress(&s.model_file).unwrap().state, "done");
    }

    #[test]
    fn a_sherpa_model_downloads_every_file_into_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let e = catalog::get("parakeet-tdt-0.6b-v2").unwrap();
        let bodies: Vec<Vec<u8>> = (0..e.files.len() as u32)
            .map(|i| body(1000 + i, i))
            .collect();
        let served: Vec<String> = e
            .files
            .iter()
            .map(|f| format!("/{}/{}", e.dir, f.name))
            .collect();
        let (url, h) = serve(
            served
                .iter()
                .zip(&bodies)
                .map(|(p, b)| (p.as_str(), 200, b.clone()))
                .collect(),
            e.files.len(),
        );
        let s = settings(
            dir.path(),
            &url,
            &[("stt.local.model", json!("parakeet-tdt-0.6b-v2"))],
        );
        assert!(!e.present(&s.model_file));
        let mut files = plan(&s);
        for (f, b) in files.iter_mut().zip(&bodies) {
            f.sha = sha(b);
        }
        let r = fetch(&s, None, &files).unwrap();
        assert_eq!(h.join().unwrap(), served);
        assert_eq!(
            (r["state"].as_str(), r["files"].as_u64()),
            (Some("done"), Some(4))
        );
        assert!(e.present(&s.model_file));
        assert_eq!(
            std::fs::read(s.model_file.join("tokens.txt")).unwrap(),
            bodies[3]
        );
        assert!(!staging(&s).exists());
        let v = list(&s);
        let row = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "parakeet-tdt-0.6b-v2")
            .unwrap()
            .clone();
        assert_eq!(
            (
                row["present"].as_bool(),
                row["selected"].as_bool(),
                row["runtime"].as_str()
            ),
            (Some(true), Some(true), Some("sherpa-onnx"))
        );
        assert_eq!(
            delete(&s, "parakeet-tdt-0.6b-v2", None).unwrap()["state"],
            "deleted"
        );
        assert!(!s.model_file.exists());
    }

    /// dot-4uzgol.69: a coreml model's files sit in .mlmodelc bundle sub-directories.
    #[test]
    fn a_coreml_model_downloads_its_bundles_into_sub_directories() {
        let dir = tempfile::tempdir().unwrap();
        let e = catalog::get("parakeet-tdt-0.6b-v2-coreml").unwrap();
        let base = settings(dir.path(), "http://127.0.0.1:1", &[]);
        let base = Settings {
            model_url: String::new(),
            ..base
        };
        let s = for_model(&base, &e.id).unwrap();
        assert_eq!(s.model_file, dir.path().join(&e.dir));
        // Without a mirror the files come from the pinned FluidInference revision.
        assert!(plan(&s).iter().all(|f| f.url.starts_with(
            "https://huggingface.co/FluidInference/parakeet-tdt-0.6b-v2-coreml/resolve/"
        )));
        let picks = [
            "Encoder.mlmodelc/coremldata.bin",
            "Encoder.mlmodelc/weights/weight.bin",
            "parakeet_vocab.json",
        ];
        let bodies: Vec<Vec<u8>> = (0..3).map(|i| body(500 + i, i)).collect();
        let served: Vec<String> = picks.iter().map(|n| format!("/{}/{n}", e.dir)).collect();
        let (url, h) = serve(
            served
                .iter()
                .zip(&bodies)
                .map(|(p, b)| (p.as_str(), 200, b.clone()))
                .collect(),
            3,
        );
        let s = Settings {
            model_url: url,
            ..s
        };
        let mut files: Vec<Fetch> = plan(&s)
            .into_iter()
            .filter(|f| picks.contains(&f.name.as_str()))
            .collect();
        for (f, b) in files.iter_mut().zip(&bodies) {
            f.sha = sha(b);
        }
        fetch(&s, None, &files).unwrap();
        assert_eq!(h.join().unwrap(), served);
        assert_eq!(
            std::fs::read(s.model_file.join("Encoder.mlmodelc/weights/weight.bin")).unwrap(),
            bodies[1]
        );
        assert!(!staging(&s).exists());
        delete(&s, &e.id, None).unwrap();
        assert!(!s.model_file.exists());
    }

    #[test]
    fn a_bad_file_in_a_directory_model_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let e = catalog::get("moonshine-base-en").unwrap();
        let (url, h) = serve(
            vec![(
                format!("/{}/{}", e.dir, e.files[0].name).leak() as &str,
                200,
                b"not the model".to_vec(),
            )],
            1,
        );
        let s = settings(
            dir.path(),
            &url,
            &[("stt.local.model", json!("moonshine-base-en"))],
        );
        let err = run(&s, None).unwrap_err();
        h.join().unwrap();
        assert_eq!(err.code, "model_checksum_mismatch");
        assert!(!s.model_file.exists() && !staging(&s).exists());
        assert_eq!(progress(&s.model_file).unwrap().state, "failed");
    }

    #[test]
    fn wrong_bytes_are_discarded_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (url, h) = serve(vec![("/ggml-tiny.en.bin", 200, b"not a model".to_vec())], 1);
        let s = settings(dir.path(), &url, &[]);
        let e = run(&s, None).unwrap_err();
        h.join().unwrap();
        assert_eq!(e.code, "model_checksum_mismatch");
        assert!(!s.model_file.exists() && !staging(&s).exists());
        let p = progress(&s.model_file).unwrap();
        assert_eq!(
            (p.state, p.error.unwrap().0.as_str()),
            ("failed", "model_checksum_mismatch")
        );
    }

    #[test]
    fn http_errors_and_unsupported_settings_have_codes() {
        let dir = tempfile::tempdir().unwrap();
        let (url, h) = serve(vec![], 1);
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
    }

    #[test]
    fn without_a_mirror_files_come_from_the_catalog_urls() {
        let dir = tempfile::tempdir().unwrap();
        for e in catalog::all() {
            // Retarget rather than configure: a configured coreml model resolves to its fallback
            // in a build without CoreML.
            let s = for_model(&settings(dir.path(), "", &[]), &e.id).unwrap();
            let urls: Vec<String> = plan(&s).into_iter().map(|f| f.url).collect();
            let want: Vec<String> = e.files.iter().map(|f| f.url.clone()).collect();
            assert_eq!(urls, want, "{}", e.id);
        }
    }

    /// The words of the script's `<var>=` assignment (a `${X:-default}` value yields the default).
    fn script_ids<'a>(script: &'a str, var: &str) -> Vec<&'a str> {
        let v = script
            .lines()
            .find_map(|l| l.strip_prefix(var)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("ci/fetch-models.sh sets {var}"));
        let v = v
            .split_once(":-")
            .map_or(v, |(_, d)| d.trim_end_matches('}'));
        v.split_whitespace().collect()
    }

    /// dot-4uzgol.104: CI fetches models only through the catalogs, and every catalog URL names an
    /// immutable revision (an upstream re-export of a mutable release asset turned every gate red).
    #[test]
    fn the_ci_script_fetches_only_revision_pinned_catalog_files() {
        let script = include_str!("../../../../ci/fetch-models.sh");
        let code: Vec<&str> = script
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        for l in &code {
            assert!(
                !l.contains("://"),
                "ci/fetch-models.sh has its own URL: {l}"
            );
            assert!(
                !l.split(|c: char| !c.is_ascii_hexdigit())
                    .any(|w| w.len() >= 40),
                "ci/fetch-models.sh has its own hash: {l}"
            );
        }
        let stt = [
            script_ids(script, "_stt_ids"),
            script_ids(script, "_stt_ids_parakeet"),
        ]
        .concat();
        assert!(stt.contains(&"tiny.en") && stt.contains(&"parakeet-tdt-0.6b-v2"));
        for id in stt {
            assert!(catalog::get(id).is_some(), "{id} is not in stt-models.json");
        }
        for id in script_ids(script, "_speaker_ids") {
            assert!(
                catalog::speaker(id).is_some(),
                "{id} is not in speaker-models.json"
            );
        }
        for e in catalog::all().iter().chain(catalog::speakers()) {
            for f in &e.files {
                let rest = f
                    .url
                    .strip_prefix("https://huggingface.co/")
                    .unwrap_or_else(|| panic!("{}: {} is not a Hugging Face URL", e.id, f.url));
                let rev = rest.split('/').nth(3).unwrap_or_default();
                assert_eq!(rest.split('/').nth(2), Some("resolve"), "{}", f.url);
                assert!(
                    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit()),
                    "{}: {} is not pinned to a commit",
                    e.id,
                    f.url
                );
                assert!(!f.url.contains("/resolve/main/") && !f.url.contains("releases/download/"));
                assert!(f.url.ends_with(&format!("/{}", f.name)), "{}", f.url);
            }
        }
    }

    #[test]
    fn list_covers_every_runtime_and_delete_removes_only_a_known_idle_model() {
        let dir = tempfile::tempdir().unwrap();
        let s = settings(dir.path(), "http://127.0.0.1:9", &[]);
        let v = list(&s);
        assert_eq!(v["selected"], "tiny.en");
        let models = v["models"].as_array().unwrap();
        assert_eq!(models.len(), catalog::all().len());
        assert!(models.iter().all(|m| m["present"] == false));
        for rt in ["whisper.cpp", "sherpa-onnx"] {
            assert!(models.iter().any(|m| m["runtime"] == rt), "{rt}");
        }
        assert!(models
            .iter()
            .all(|m| m["size"].as_u64().unwrap() > 0 && m["benchmark_source"].is_string()));
        std::fs::write(dir.path().join("ggml-small.en.bin"), b"abc").unwrap();
        let v = list(&s);
        let small = v["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "small.en")
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
            delete(&s, "base.en", None).unwrap_err().code,
            "model_not_found"
        );
        let e = delete(&s, "parakeet-tdt-0.6b-v3", None).unwrap_err();
        assert_eq!(e.code, "invalid_value");
        assert!(
            e.message.contains("use parakeet-tdt-0.6b-v2"),
            "{}",
            e.message
        );
        let bus = Bus::default();
        let rx = bus.subscribe(vec!["model-download".into()]);
        assert_eq!(
            delete(&s, "small.en", Some(&bus)).unwrap()["state"],
            "deleted"
        );
        assert!(!dir.path().join("ggml-small.en.bin").exists());
        assert!(rx.try_recv().unwrap().contains("deleted"));
        let t = for_model(&s, "base.en").unwrap();
        assert_eq!(
            (t.model.as_str(), t.model_file.clone()),
            ("base.en", dir.path().join("ggml-base.en.bin"))
        );
        let t = for_model(&s, "moonshine-base-en").unwrap();
        assert_eq!(
            (t.entry.runtime, t.model_file.clone()),
            (
                catalog::Runtime::SherpaOnnx,
                dir.path().join("sherpa-onnx-moonshine-base-en-int8")
            )
        );
        assert_eq!(for_model(&s, "huge").unwrap_err().code, "invalid_value");
    }
}
