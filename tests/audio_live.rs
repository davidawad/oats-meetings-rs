//! Live audio proof through the real binary (dot-4uzgol.7). Needs a running Pulse-protocol server
//! with two null sinks, set up by `ci/audio-linux.sh`; skipped (passes) unless `OATS_AUDIO_LIVE=1`.
//!
//! Acceptance: a tone played into a null-sink is recorded through `ctl record start|stop` for 5 s;
//! the two-channel WAV is non-silent (RMS above threshold), duration >= 4 s, `ctl devices list
//! --json` shows a loopback source, and `ctl session get` reports the duration.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use oats_audio::wav;
use serde_json::Value;

const EXE: &str = env!("CARGO_BIN_EXE_oats-meetings-rs");
const SYSTEM_SINK: &str = "oats_sink";
const MIC_SINK: &str = "oats_mic_sink";
const RMS_MIN: f64 = 0.05;

fn live() -> bool {
    let on = std::env::var("OATS_AUDIO_LIVE").ok().as_deref() == Some("1");
    if !on {
        eprintln!("audio_live: skipped (set OATS_AUDIO_LIVE=1 with ci/audio-linux.sh)");
    }
    on
}

struct Engine {
    child: Child,
    rt: String,
    cfg: String,
    data: String,
    _dir: tempfile::TempDir,
}

impl Engine {
    fn cmd(&self) -> Command {
        let mut c = Command::new(EXE);
        c.env("OATS_RUNTIME_DIR", &self.rt)
            .env("OATS_CONFIG", &self.cfg)
            .env("OATS_DATA_DIR", &self.data)
            .env("OATS_OS", "linux")
            .env_remove("OATS_PACK");
        c
    }

    fn start(config: &str) -> Engine {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_str().unwrap().to_string();
        let (rt, cfg, data) = (p("rt"), p("config.toml"), p("data"));
        std::fs::write(&cfg, config).unwrap();
        let mut e = Engine {
            child: Command::new("true").spawn().unwrap(),
            rt,
            cfg,
            data,
            _dir: dir,
        };
        e.child.wait().unwrap();
        e.child = e.cmd().arg("--headless").stderr(Stdio::null()).spawn().unwrap();
        let t = Instant::now();
        while e.ctl(&["ping"]).0 != 0 {
            assert!(t.elapsed() < Duration::from_secs(30), "engine did not come up");
            std::thread::sleep(Duration::from_millis(50));
        }
        e
    }

    fn ctl(&self, args: &[&str]) -> (i32, String, String) {
        let o = self.cmd().arg("ctl").args(args).output().unwrap();
        (
            o.status.code().unwrap(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let (code, out, err) = self.ctl(&a);
        let v = serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {out} {err}"));
        (code, v)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Feed a sine into a Pulse sink with `pacat` until dropped.
struct Tone {
    child: Child,
    th: Option<std::thread::JoinHandle<()>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Tone {
    fn play(sink: &str, hz: f32) -> Tone {
        let mut child = Command::new("pacat")
            .args(["--playback", &format!("--device={sink}"), "--format=float32le", "--rate=48000", "--channels=1"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("pacat");
        let mut stdin = child.stdin.take().unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let th = std::thread::spawn(move || {
            let mut n: u64 = 0;
            while !flag.load(std::sync::atomic::Ordering::SeqCst) {
                let mut buf = Vec::with_capacity(4800 * 4);
                for i in 0..4800u64 {
                    let t = (n + i) as f32 / 48000.0;
                    buf.extend_from_slice(&((t * hz * std::f32::consts::TAU).sin() * 0.5).to_le_bytes());
                }
                n += 4800;
                if stdin.write_all(&buf).is_err() {
                    break;
                }
            }
        });
        Tone { child, th: Some(th), stop }
    }
}

impl Drop for Tone {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(t) = self.th.take() {
            let _ = t.join();
        }
    }
}

fn backends() -> Vec<String> {
    std::env::var("OATS_AUDIO_BACKENDS")
        .unwrap_or_else(|_| "auto,pulse".into())
        .split(',')
        .map(str::to_string)
        .collect()
}

#[test]
fn devices_list_json_shows_a_loopback_source() {
    if !live() {
        return;
    }
    for be in backends() {
        let e = Engine::start(&format!("[audio]\nbackend = \"{be}\"\n"));
        let (code, v) = e.json(&["devices", "list"]);
        assert_eq!(code, 0, "{be}: {v}");
        assert_eq!(v["data"]["loopback_available"], true, "{be}: {v}");
        let devs = v["data"]["devices"].as_array().unwrap();
        assert!(
            devs.iter().any(|d| d["kind"] == "loopback" && d["id"].as_str().unwrap().contains(SYSTEM_SINK)),
            "{be}: {v}"
        );
    }
}

#[test]
fn five_second_loopback_recording_is_non_silent_stereo() {
    if !live() {
        return;
    }
    for be in backends() {
        let _sys = Tone::play(SYSTEM_SINK, 440.0);
        let _mic = Tone::play(MIC_SINK, 880.0);
        // The mic channel reads the second sink's monitor: there is no physical mic in a pod.
        let monitor = if be == "pipewire" { MIC_SINK } else { "oats_mic_sink.monitor" };
        let e = Engine::start(&format!("[audio]\nbackend = \"{be}\"\n[audio.mic]\ndevice = \"{monitor}\"\n"));
        let (code, v) = e.json(&["record", "start", "--session", "live_a"]);
        assert_eq!(code, 0, "{be}: {v}");
        std::thread::sleep(Duration::from_secs(5));
        let (code, live_get) = e.json(&["session", "get", "live_a"]);
        assert_eq!(code, 0, "{be}: {live_get}");
        assert_eq!(live_get["data"]["state"], "recording");
        let (code, v) = e.json(&["record", "stop"]);
        assert_eq!(code, 0, "{be}: {v}");
        eprintln!("audio_live[{be}]: stop -> {v}");
        let (code, got) = e.json(&["session", "get", "live_a"]);
        assert_eq!(code, 0, "{be}: {got}");
        let dur = got["data"]["duration_ms"].as_u64().unwrap();
        assert!(dur >= 4000, "{be}: duration_ms {dur}");
        let w = wav::read(std::path::Path::new(got["data"]["audio_path"].as_str().unwrap())).unwrap();
        eprintln!(
            "audio_live[{be}]: {} ch, {} Hz, {:.2}s, rms L {:.3} R {:.3}, tone L {:.0} Hz R {:.0} Hz",
            w.channels, w.rate, w.duration_secs(), w.rms(0), w.rms(1), w.tone_hz(0), w.tone_hz(1)
        );
        assert_eq!(w.channels, 2, "{be}");
        assert!(w.duration_secs() >= 4.0, "{be}: wav {:.2}s", w.duration_secs());
        assert!(w.rms(0) > RMS_MIN, "{be}: system rms {}", w.rms(0));
        assert!(w.rms(1) > RMS_MIN, "{be}: mic rms {}", w.rms(1));
        // Right tone on the right channel: the system sink is 440 Hz, the mic sink 880 Hz.
        assert!((w.tone_hz(0) - 440.0).abs() < 15.0, "{be}: left {} Hz", w.tone_hz(0));
        assert!((w.tone_hz(1) - 880.0).abs() < 30.0, "{be}: right {} Hz", w.tone_hz(1));
    }
}

#[test]
fn layout_mix_and_disabled_mic_over_ctl() {
    if !live() {
        return;
    }
    let _sys = Tone::play(SYSTEM_SINK, 440.0);
    let e = Engine::start("");
    // No physical mic in the pod: the default mic does not exist, which is a structured error.
    let (code, v) = e.json(&["record", "start", "--session", "no_mic"]);
    assert_eq!(code, 1, "{v}");
    assert_eq!(v["error"]["code"], "device_not_found", "{v}");
    assert_eq!(e.ctl(&["config", "set", "audio.mic.enabled", "false"]).0, 0);
    let (code, v) = e.json(&["record", "start", "--session", "sys_only"]);
    assert_eq!(code, 0, "{v}");
    std::thread::sleep(Duration::from_secs(2));
    let (_, v) = e.json(&["record", "stop"]);
    let w = wav::read(std::path::Path::new(v["data"]["path"].as_str().unwrap())).unwrap();
    assert_eq!(w.channels, 2);
    assert!(w.rms(0) > RMS_MIN && w.rms(1) == 0.0, "L {} R {}", w.rms(0), w.rms(1));
    assert_eq!(e.ctl(&["config", "set", "audio.layout", "mix"]).0, 0);
    let (code, _) = e.json(&["record", "start", "--session", "mixed"]);
    assert_eq!(code, 0);
    std::thread::sleep(Duration::from_secs(2));
    let (_, v) = e.json(&["record", "stop"]);
    let w = wav::read(std::path::Path::new(v["data"]["path"].as_str().unwrap())).unwrap();
    assert_eq!(w.channels, 1);
    assert!(w.rms(0) > RMS_MIN);
}
