//! A default build transcribes out of the box (dot-4uzgol.26): local whisper is compiled in, a
//! missing model is never silent (state, banner problem, fix action), and the model download is an
//! action with progress. Runs only with default features, which is what ships.
#![cfg(feature = "whisper")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use oats_audio::fake::FakeBackend;
use oats_core::action::Action;
use oats_core::control::engine::{Engine, EngineOpts};
use serde_json::{json, Value};

struct Rig {
    dir: tempfile::TempDir,
    engine: Arc<Engine>,
}

/// An engine with fake capture and NO injected ASR: the real `stt::make_asr` path.
fn rig(auto_download: bool, model_url: &str) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::new(EngineOpts {
        user_config: dir.path().join("config.toml"),
        pack: None,
        os: "linux".into(),
        arch: "x86_64".into(),
        machine: "default".into(),
        sessions_dir: dir.path().join("sessions"),
        secrets: None,
    })
    .unwrap();
    engine
        .set(
            "sessions.root",
            &json!(dir.path().join("rec").to_str().unwrap()),
            "test",
        )
        .unwrap();
    engine
        .set(
            "stt.local.models_dir",
            &json!(dir.path().join("models").to_str().unwrap()),
            "test",
        )
        .unwrap();
    engine
        .set("stt.local.auto_download", &json!(auto_download), "test")
        .unwrap();
    engine
        .set("stt.local.model_url", &json!(model_url), "test")
        .unwrap();
    engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
    Rig { dir, engine }
}

/// Where the default model (`stt.local.model`'s default catalog entry) lives in the rig.
fn default_model(r: &Rig) -> (&'static oats_core::stt::catalog::Entry, std::path::PathBuf) {
    // The model this build runs for the configured one (the CoreML default resolves to its
    // sherpa-onnx fallback off macOS).
    let e = oats_core::stt::catalog::get(&r.engine.snapshot().config.stt.local.model)
        .unwrap()
        .resolved();
    (e, e.location(&r.dir.path().join("models")))
}

fn status(r: &Rig) -> Value {
    r.engine.dispatch(&Action::SttStatus {}).unwrap()
}

/// Serves `body` to every request until dropped.
fn serve(body: &'static [u8]) -> (String, Arc<tiny_http::Server>) {
    let srv = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let url = format!("http://{}", srv.server_addr().to_ip().unwrap());
    let s2 = Arc::clone(&srv);
    std::thread::spawn(move || {
        for req in s2.incoming_requests() {
            let _ = req.respond(tiny_http::Response::from_data(body.to_vec()));
        }
    });
    (url, srv)
}

fn wait(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_default_build_compiles_local_asr_and_names_the_missing_model() {
    let r = rig(false, "http://127.0.0.1:9");
    let s = status(&r);
    assert_eq!(s["local"]["compiled"], true);
    assert_eq!(s["local"]["present"], false);
    assert_eq!(s["problem"]["code"], "model_not_found");
    assert_eq!(s["problem"]["fix"], "stt-model-fetch");
    assert!(s["problem"]["message"]
        .as_str()
        .unwrap()
        .contains("audio only"));
}

#[test]
fn recording_without_a_model_says_so_at_start_and_at_stop_and_keeps_the_audio() {
    let r = rig(false, "http://127.0.0.1:9");
    let start = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("s1".into()),
            title: None,
        })
        .unwrap();
    let t = &start["transcription"];
    assert_eq!(
        (t["state"].as_str(), t["code"].as_str()),
        (Some("unavailable"), Some("model_not_found"))
    );
    assert!(
        t["reason"].as_str().unwrap().contains("stt model fetch"),
        "{t}"
    );
    std::thread::sleep(Duration::from_millis(600));
    let stop = r.engine.dispatch(&Action::RecordStop {}).unwrap();
    assert_eq!(stop["transcription"]["state"], "unavailable");
    assert!(
        std::path::Path::new(stop["path"].as_str().unwrap()).is_file(),
        "the audio is kept"
    );
}

#[test]
fn a_failed_download_is_visible_and_leaves_no_file() {
    let (url, _srv) = serve(b"definitely not a model");
    let r = rig(false, &url);
    let e = r
        .engine
        .dispatch(&Action::SttModelFetch {
            model: None,
            wait: Some(true),
        })
        .unwrap_err();
    assert_eq!(e.code, "model_checksum_mismatch");
    let s = status(&r);
    assert!(!default_model(&r).1.exists());
    assert_eq!(s["local"]["download"]["state"], "failed");
    assert_eq!(s["problem"]["code"], "model_download_failed");
    assert_eq!(s["problem"]["fix"], "stt-model-fetch");
}

#[test]
fn the_first_recording_starts_the_download_and_tells_the_user() {
    let (url, _srv) = serve(b"definitely not a model");
    let r = rig(true, &url);
    let rx = r.engine.bus.subscribe(vec!["model-download".into()]);
    let start = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("s1".into()),
            title: None,
        })
        .unwrap();
    assert_eq!(start["transcription"]["state"], "unavailable");
    assert!(
        start["transcription"]["reason"]
            .as_str()
            .unwrap()
            .contains("Downloading it now"),
        "{start}"
    );
    wait("the download to fail on the bad bytes", || {
        status(&r)["local"]["download"]["state"] == "failed"
    });
    let first: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(first["data"]["state"], "downloading");
    r.engine.dispatch(&Action::RecordStop {}).unwrap();
}

#[test]
fn fetch_without_wait_returns_at_once_and_a_present_model_is_left_alone() {
    let (url, _srv) = serve(b"definitely not a model");
    let r = rig(false, &url);
    let v = r
        .engine
        .dispatch(&Action::SttModelFetch {
            model: None,
            wait: None,
        })
        .unwrap();
    assert_eq!(v["state"], "downloading");
    wait("the download to settle", || {
        status(&r)["local"]["download"]["state"] == "failed"
    });
    let (e, at) = default_model(&r);
    std::fs::create_dir_all(&at).unwrap();
    for f in &e.files {
        // A coreml file sits inside its .mlmodelc bundle (`Encoder.mlmodelc/weights/...`).
        let p = at.join(&f.name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }
    let v = r
        .engine
        .dispatch(&Action::SttModelFetch {
            model: None,
            wait: Some(true),
        })
        .unwrap();
    assert_eq!(v["state"], "present");
    assert_eq!(status(&r)["problem"], Value::Null);
}

/// With the real model (ci/fetch-models.sh exports OATS_WHISPER_MODEL) the default record path
/// builds whisper and turns transcription on.
#[test]
fn with_a_model_the_default_record_path_turns_transcription_on() {
    let Ok(model) = std::env::var("OATS_WHISPER_MODEL") else {
        eprintln!("OATS_WHISPER_MODEL unset: skipped");
        return;
    };
    let r = rig(false, "http://127.0.0.1:9");
    r.engine
        .set("stt.local.model", &json!("tiny.en"), "test")
        .unwrap();
    std::fs::create_dir_all(r.dir.path().join("models")).unwrap();
    std::fs::copy(model, r.dir.path().join("models/ggml-tiny.en.bin")).unwrap();
    assert_eq!(status(&r)["problem"], Value::Null);
    let start = r
        .engine
        .dispatch(&Action::RecordStart {
            session: Some("s1".into()),
            title: None,
        })
        .unwrap();
    assert_eq!(start["transcription"]["state"], "on", "{start}");
    r.engine.dispatch(&Action::RecordStop {}).unwrap();
}
