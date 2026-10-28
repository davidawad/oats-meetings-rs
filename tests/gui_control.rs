//! GUI mode hosts the control engine (dot-4uzgol.5 acceptance, owned here by .11): start the real
//! binary WITHOUT --headless on a virtual X display, then `ctl ping`, `ctl list-actions`, window
//! actions and `quit` work against that GUI process.
//!
//! Needs Xvfb and a software Vulkan driver, so it only runs when OATS_GUI_E2E=1 (ci/linux-nix.sh
//! sets it when Xvfb is present). Elsewhere it prints a skip and passes.
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const EXE: &str = env!("CARGO_BIN_EXE_oats-meetings-rs");

struct Procs {
    xvfb: Child,
    gui: Child,
    rt: String,
    cfg: String,
    _dir: tempfile::TempDir,
}

impl Drop for Procs {
    fn drop(&mut self) {
        let _ = self.gui.kill();
        let _ = self.gui.wait();
        let _ = self.xvfb.kill();
        let _ = self.xvfb.wait();
    }
}

fn base(rt: &str, cfg: &str) -> Command {
    let mut c = Command::new(EXE);
    c.env("OATS_DATA_DIR", format!("{rt}/../data"))
        .env("OATS_RUNTIME_DIR", rt)
        .env("OATS_CONFIG", cfg)
        .env("OATS_OS", "linux")
        .env_remove("OATS_PACK");
    c
}

impl Procs {
    fn ctl(&self, args: &[&str]) -> (i32, Value) {
        let o = base(&self.rt, &self.cfg).arg("ctl").arg("--json").args(args).output().unwrap();
        let v = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
        (o.status.code().unwrap_or(-1), v)
    }
}

fn start(menubar: bool) -> Option<Procs> {
    if std::env::var("OATS_GUI_E2E").as_deref() != Ok("1") {
        eprintln!("skipped: set OATS_GUI_E2E=1 (needs Xvfb)");
        return None;
    }
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let display = format!(":{}", 90 + (std::process::id() % 50) * 2 + n);
    let xvfb = Command::new("Xvfb")
        .args([&display, "-screen", "0", "1280x800x24"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("Xvfb");
    std::thread::sleep(Duration::from_millis(800));
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("rt").to_str().unwrap().to_string();
    let cfg = dir.path().join("config.toml").to_str().unwrap().to_string();
    std::fs::write(&cfg, format!("[app]\nmenubar = {menubar}\n")).unwrap();
    let gui = base(&rt, &cfg)
        .env("DISPLAY", &display)
        .env_remove("WAYLAND_DISPLAY")
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let p = Procs { xvfb, gui, rt, cfg, _dir: dir };
    let t0 = Instant::now();
    while p.ctl(&["ping"]).0 != 0 {
        assert!(t0.elapsed() < Duration::from_secs(60), "GUI process did not serve the control socket");
        std::thread::sleep(Duration::from_millis(200));
    }
    Some(p)
}

fn check(menubar: bool) {
    let Some(mut p) = start(menubar) else { return };
    let (code, v) = p.ctl(&["ping"]);
    assert_eq!((code, &v["ok"]), (0, &Value::Bool(true)));
    let (code, v) = p.ctl(&["list-actions"]);
    assert_eq!(code, 0);
    let committed: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/config/actions.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["data"], committed);
    // Window actions answer through the GUI process (a GUI that cannot open a window, e.g. no
    // GPU, flips back to hidden on its own, so only the immediate answers are asserted).
    assert_eq!(p.ctl(&["window-hide"]).1["data"]["visible"], Value::Bool(false));
    assert_eq!(p.ctl(&["window-show"]).1["data"]["visible"], Value::Bool(true));
    std::thread::sleep(Duration::from_millis(500));
    // The process is still the GUI, alive after window churn.
    assert!(p.gui.try_wait().unwrap().is_none());
    assert_eq!(p.ctl(&["quit"]).0, 0);
    let t0 = Instant::now();
    while p.gui.try_wait().unwrap().is_none() {
        assert!(t0.elapsed() < Duration::from_secs(20), "GUI did not exit after quit");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn gui_hosts_the_engine_menubar_off() {
    check(false);
}

#[test]
fn gui_hosts_the_engine_menubar_on() {
    check(true);
}
