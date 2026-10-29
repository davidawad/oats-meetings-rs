//! GUI mode hosts the control engine (dot-4uzgol.5 acceptance): the same `Engine` and control
//! socket as `--headless`, plus a GPUI viewer that mirrors engine state. `ctl ping` and
//! `ctl list-actions` work against this process.
//!
//! Engine -> window: a 100 ms tick on the GPUI thread drains the event bus and reconciles window
//! visibility, the tray and capture hiding against the engine's active config. Window -> engine:
//! menus, keys, tray clicks and view buttons all call `model::dispatch`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Weak};

use gpui::{
    px, size, App, AppContext as _, AsyncApp, Bounds, KeyBinding, Menu, MenuItem, QuitMode,
    WindowBounds, WindowHandle, WindowOptions,
};
use oats_core::config::ConfigError;
use oats_core::control::engine::Engine;
use oats_core::control::{paths, server};
use serde_json::{json, Value};

use crate::capture;
use crate::drive;
use crate::model::{self, BINDINGS, MENUS};
use crate::state::State;
use crate::tray::{self, TrayBackend, TrayState};
use crate::view::MainWindow;

/// Window class on Linux (Wayland app_id, X11 WM_CLASS). Must equal `StartupWMClass` in
/// `packaging/linux/oats-meetings-rs.desktop`; the macOS bundle id is
/// `com.davidawad.oats-meetings-rs` (packaging/macos/Info.plist.in).
pub const APP_ID: &str = "oats-meetings-rs";

/// One gpui action carries every UI dispatch: the id is an `oats_core::action` wire name.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = oats, no_json)]
pub struct Do {
    pub id: String,
}

fn menus(capture_supported: bool) -> Vec<Menu> {
    MENUS
        .iter()
        .map(|m| {
            Menu::new(m.title).items(m.items.iter().map(|e| match e.action {
                None => MenuItem::separator(),
                Some(id) => MenuItem::action(
                    model::label(e, capture_supported),
                    Do { id: id.to_string() },
                ),
            }))
        })
        .collect()
}

fn mod_prefix() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd-"
    } else {
        "ctrl-"
    }
}

fn bindings(cx: &App) -> Vec<KeyBinding> {
    BINDINGS
        .iter()
        .filter_map(|b| {
            KeyBinding::load(
                &format!("{}{}", mod_prefix(), b.keys),
                Box::new(Do {
                    id: b.action.to_string(),
                }),
                None,
                false,
                None,
                cx,
            )
            .ok()
        })
        .collect()
}

struct Host {
    engine: Arc<Engine>,
    events: Receiver<String>,
    tray_state: Arc<TrayState>,
    tray: Box<dyn TrayBackend>,
    window: Option<WindowHandle<MainWindow>>,
    /// The closed window's view state; the next window resumes it.
    kept: Rc<RefCell<Option<State>>>,
    applied_capture: Option<capture::Plan>,
    /// What the engine says the window should be (from `window-changed`).
    want_visible: bool,
    tray_gate: tray::Gate,
    /// Tray entries as last shown; labels follow capture-hiding support.
    tray_entries: Vec<model::Entry>,
    /// Capture-hiding support the menus were last built for.
    capture_supported: bool,
    /// `ui-*` requests from the engine (`ctl ui-select`, `ui-state`, `ui-invoke`).
    port: Arc<drive::Port>,
    /// The next `window-changed` is the launch-time show: open the window without activating.
    launch_show: bool,
    /// `cx.quit()` has been called.
    quitting: bool,
    /// How many times the app took focus (`cx.activate`); only explicit show requests do.
    activations: usize,
}

/// Ends the process with a code: `std::process::exit`; tests record the call instead.
pub type ExitFn = fn(i32);

/// The `ctl quit` watchdog (dot-4uzgol.57, .58). A thread of its own, woken by the bus `quit`
/// event, so a main thread that never ticks again (wedged in a platform call) cannot keep it from
/// firing: `ui.quit_timeout` after the quit it removes `engine.lock` (when this engine holds it,
/// `app.single_instance`) and exits 0. A quit that finishes first ends the process before it, or
/// `cancel`s it.
pub struct QuitWatchdog {
    cancelled: Arc<AtomicBool>,
}

impl QuitWatchdog {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

pub fn spawn_quit_watchdog(engine: &Arc<Engine>, lock: PathBuf, exit: ExitFn) -> QuitWatchdog {
    // Subscribed before the thread runs: a quit sent right after this call is not missed.
    let quit = engine.bus.subscribe(vec!["quit".into()]);
    let (cancelled, flag) = {
        let c = Arc::new(AtomicBool::new(false));
        (Arc::clone(&c), c)
    };
    let weak: Weak<Engine> = Arc::downgrade(engine);
    let spawned = std::thread::Builder::new()
        .name("oats-quit-watchdog".into())
        .spawn(move || {
            // Err: the engine (and its bus) is gone without a quit; nothing to watch.
            if quit.recv().is_err() {
                return;
            }
            let Some((after, single)) = weak.upgrade().map(|e| {
                let c = &e.snapshot().config;
                (c.ui.quit_timeout.as_duration(), c.app.single_instance)
            }) else {
                return;
            };
            std::thread::sleep(after);
            if flag.load(Ordering::SeqCst) {
                return;
            }
            oats_core::elog!("oats-meetings-rs: quit did not finish in time; exiting");
            if single {
                let _ = std::fs::remove_file(&lock);
            }
            exit(0);
        });
    if let Err(e) = spawned {
        oats_core::elog!("oats-meetings-rs: cannot start the quit watchdog: {e}");
    }
    QuitWatchdog { cancelled }
}

type Shared = Rc<RefCell<Host>>;

fn capture_plan(engine: &Engine) -> capture::Plan {
    let cfg = engine.snapshot();
    capture::plan(
        cfg.config.privacy.hide_from_capture,
        &engine.resolved().flat["privacy.hide_method"]
            .as_str()
            .unwrap_or("none")
            .to_string(),
    )
}

/// What a tick does to the main window once the engine's events are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reconcile {
    /// Open the window; `focus` makes it key and activates the app.
    Open {
        focus: bool,
    },
    /// The window is open and a show request came: bring it front, activating only if `activate`.
    Raise {
        activate: bool,
    },
    Close,
    Nothing,
}

/// The window decision. `raise` is an explicit show request this tick (tray, menu,
/// `ctl window-show`); the launch-time show is not one. The app takes focus only for an explicit
/// request with `ui.activate_on_show` on: never at launch, never on its own (dot-4uzgol.54).
fn reconcile(want: bool, open: bool, raise: bool, activate_on_show: bool) -> Reconcile {
    let activate = raise && activate_on_show;
    match (want, open) {
        (true, false) => Reconcile::Open { focus: activate },
        (true, true) if raise => Reconcile::Raise { activate },
        (false, true) => Reconcile::Close,
        (true, true) | (false, false) => Reconcile::Nothing,
    }
}

/// The main window's options. Shown either way; key (and so app-activating) only when `focus`.
pub fn window_options(cx: &App, focus: bool) -> WindowOptions {
    let want = size(px(960.), px(640.));
    let mut bounds = Bounds::centered(None, want, cx);
    // A display that reports no visible area yet would clamp the window to 0x0 (dot-4uzgol.56).
    if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
        bounds.size = want;
    }
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Wayland app_id / X11 WM_CLASS; matches packaging/linux/*.desktop StartupWMClass.
        app_id: Some(APP_ID.to_string()),
        show: true,
        focus,
        ..Default::default()
    }
}

fn open_window(host: &mut Host, cx: &mut App, focus: bool) {
    let engine = Arc::clone(&host.engine);
    let kept = Rc::clone(&host.kept);
    match cx.open_window(window_options(cx, focus), move |_, cx| {
        cx.new(|_| MainWindow::resume(engine, kept))
    }) {
        Ok(h) => {
            host.window = Some(h);
            host.applied_capture = None; // new window: capture plan must be applied again
        }
        Err(e) => {
            // Keep the engine's idea of the window true: nothing is showing.
            oats_core::elog!("oats-meetings-rs: cannot open window: {e}");
            host.want_visible = false;
            let _ = model::dispatch(&host.engine, "window-hide", json!({}));
        }
    }
}

fn tick(host: &Shared, cx: &mut App) {
    let mut h = host.borrow_mut();
    if h.engine.is_shutdown() {
        if !h.quitting {
            h.quitting = true;
            // Close the window first: a live window must not keep the run loop's terminate waiting.
            if let Some(w) = h.window.take() {
                let _ = w.update(cx, |_, window, _| window.remove_window());
            }
            // The watchdog (spawn_quit_watchdog) backs this up if the run loop never ends.
            cx.quit();
        }
        return;
    }
    let mut changed_sessions = false;
    // Raise only on a show request: activating every tick keeps making the window key and
    // ordering it front on macOS, which fights the user and any other app (dot-4uzgol.52).
    let mut raise = false;
    // The launch-time show is the app's own, not a user request: it opens without focus.
    let launch = std::mem::take(&mut h.launch_show);
    let lines: Vec<String> = h.events.try_iter().collect();
    for line in &lines {
        let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        match v["event"].as_str() {
            Some("window-changed") => {
                h.want_visible = v["data"]["visible"].as_bool().unwrap_or(false);
                raise = h.want_visible && !launch;
            }
            Some(
                "session-saved" | "session-deleted" | "config-reloaded" | "setting-changed"
                | "model-download",
            ) => changed_sessions = true,
            _ => {}
        }
    }
    // The user closed the window with the OS control: tell the engine.
    if h.window.is_some() && cx.windows().is_empty() {
        h.window = None;
        if h.want_visible {
            h.want_visible = false;
            let _ = model::dispatch(&h.engine, "window-hide", json!({}));
            // consume our own event next tick
        }
    }
    let activate_on_show = h.engine.snapshot().config.ui.activate_on_show;
    match (
        reconcile(h.want_visible, h.window.is_some(), raise, activate_on_show),
        h.window,
    ) {
        (Reconcile::Open { focus }, _) => {
            open_window(&mut h, cx, focus);
            if focus && h.window.is_some() {
                cx.activate(true);
                h.activations += 1;
            }
        }
        (Reconcile::Raise { activate }, Some(w)) => {
            let _ = w.update(cx, |_, window, _| {
                if activate {
                    window.activate_window();
                } else {
                    capture::order_front(window);
                }
            });
            if activate {
                cx.activate(true);
                h.activations += 1;
            }
        }
        (Reconcile::Close, Some(w)) => {
            let _ = w.update(cx, |_, window, _| window.remove_window());
            h.window = None;
        }
        _ => {}
    }
    if let Some(w) = h.window {
        let plan = capture_plan(&h.engine);
        if h.applied_capture != Some(plan) {
            let out = w.update(cx, |_, window, _| capture::apply(window, plan));
            match out {
                Ok(capture::Applied::Failed(e)) => {
                    oats_core::elog!("oats-meetings-rs: capture hiding failed: {e}")
                }
                Ok(capture::Applied::WrongPlatform(why)) => {
                    oats_core::elog!("oats-meetings-rs: capture hiding unavailable: {why}")
                }
                _ => {}
            }
            h.applied_capture = Some(plan);
        }
        for line in lines {
            let _ = w.update(cx, |v, _, cx| v.event(&line, cx));
        }
        let _ = w.update(cx, |v, _, cx| v.tick(cx));
        if changed_sessions {
            let _ = w.update(cx, |v, _, cx| v.refresh(cx));
        }
    }
    // After the window is reconciled, so `window-show` then `ui-select` finds it open.
    for r in h.port.take() {
        let out = match h.window {
            Some(w) => w
                .update(cx, |v, _, cx| v.drive(&r.req, cx))
                .unwrap_or_else(|e| Err(ConfigError::new("no_window", e.to_string()))),
            None => Err(ConfigError::new(
                "no_window",
                "the window is hidden: window-show first",
            )),
        };
        let _ = r.reply.send(out);
    }
    let supported = h.engine.capture_hiding_supported();
    if supported != h.capture_supported {
        // privacy.hide_method changed at runtime: relabel the menus, and re-show the tray.
        h.capture_supported = supported;
        h.tray_entries = model::entries(model::TRAY, supported);
        cx.set_menus(menus(supported));
        if h.tray.visible() {
            h.tray.hide();
        }
    }
    let want = h.tray_state.wanted();
    let h = &mut *h;
    if let Some(why) = h.tray_gate.step(want, &mut *h.tray, &h.tray_entries) {
        oats_core::elog!("oats-meetings-rs: app.menubar is on but {why}");
    }
    for id in h.tray.take_clicks() {
        let _ = model::dispatch(&h.engine, &id, json!({}));
    }
}

/// Run the GUI over `engine`, serving the control socket under `runtime_dir`. Returns the exit code.
pub fn run(engine: Arc<Engine>, runtime_dir: &std::path::Path) -> i32 {
    run_with(engine, runtime_dir, tray::native_backend())
}

pub fn run_with(
    engine: Arc<Engine>,
    runtime_dir: &std::path::Path,
    backend: Box<dyn TrayBackend>,
) -> i32 {
    let tray_state = Arc::new(TrayState::new(engine.snapshot().config.app.menubar));
    let capture_supported = engine.capture_hiding_supported();
    engine.register(tray_state.clone());
    let port = Arc::new(drive::Port::default());
    engine.set_ui(Some(port.clone()));
    let handle = match server::serve(Arc::clone(&engine), runtime_dir) {
        Ok(h) => h,
        Err(e) => {
            oats_core::elog!("{e}");
            return 1;
        }
    };
    oats_core::elog!(
        "oats-meetings-rs: engine listening on {}",
        handle.socket_display()
    );
    // Before anything that can block the main thread: it must be able to fire with no tick.
    let watchdog = spawn_quit_watchdog(&engine, paths::lock_path(runtime_dir), |c| {
        std::process::exit(c)
    });
    engine.launched();
    let host: Shared = Rc::new(RefCell::new(Host {
        engine: Arc::clone(&engine),
        events: engine.bus.subscribe(vec![]),
        tray_state,
        tray: backend,
        window: None,
        kept: Rc::default(),
        applied_capture: None,
        want_visible: false,
        tray_gate: tray::Gate::default(),
        tray_entries: model::entries(model::TRAY, capture_supported),
        capture_supported,
        port,
        launch_show: false,
        quitting: false,
        activations: 0,
    }));
    let h2 = Rc::clone(&host);
    gpui_platform::application().run(move |cx: &mut App| {
        cx.set_quit_mode(QuitMode::Explicit);
        cx.bind_keys(bindings(cx));
        cx.set_menus(menus(capture_supported));
        let eng = Arc::clone(&h2.borrow().engine);
        cx.on_action(move |a: &Do, cx: &mut App| {
            // Off the UI thread: a record-start loads the ASR model and must not freeze the window.
            let (eng, id) = (Arc::clone(&eng), a.id.clone());
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = model::dispatch(&eng, &id, json!({})) {
                        if e.code != "not_implemented" {
                            oats_core::elog!("oats-meetings-rs: {id}: {e}");
                        }
                    }
                })
                .detach();
        });
        // Show the window through the engine so `window-changed` and the control API agree.
        if !h2.borrow().engine.snapshot().config.ui.start_hidden {
            h2.borrow_mut().launch_show = true;
            let _ = model::dispatch(&h2.borrow().engine, "window-show", json!({}));
        }
        let h3 = Rc::clone(&h2);
        cx.spawn(async move |cx: &mut AsyncApp| loop {
            // ui.tick is read each round, so a reload changes the cadence at once.
            let every = h3.borrow().engine.snapshot().config.ui.tick.as_duration();
            cx.background_executor().timer(every).await;
            cx.update(|cx| tick(&h3, cx));
        })
        .detach();
        // No cx.activate here: the app never takes focus on its own (dot-4uzgol.54).
    });
    engine.set_ui(None);
    handle.shutdown();
    watchdog.cancel();
    0
}

/// `oats-meetings-rs` with no flags: engine from the environment, then the GUI.
pub fn run_env() -> i32 {
    let env = |k: &str| std::env::var(k).ok();
    match Engine::from_env(&env) {
        Ok(engine) => run(engine, &paths::runtime_dir(&env)),
        Err(ConfigError { .. }) => {
            oats_core::elog!("oats-meetings-rs: cannot start the engine");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::{testutil, tray::mock::Mock};
    use gpui::TestAppContext;

    #[test]
    fn hide_then_show_keeps_the_tab_and_the_last_recording_line() {
        let (engine, _d) = testutil::engine(&[]);
        let host: Shared = Rc::new(RefCell::new(Host {
            engine: Arc::clone(&engine),
            events: engine.bus.subscribe(vec![]),
            tray_state: Arc::new(TrayState::new(false)),
            tray: Box::new(Mock::default()),
            window: None,
            kept: Rc::default(),
            applied_capture: None,
            want_visible: false,
            tray_gate: tray::Gate::default(),
            tray_entries: model::entries(model::TRAY, engine.capture_hiding_supported()),
            capture_supported: engine.capture_hiding_supported(),
            port: Arc::default(),
            launch_show: false,
            quitting: false,
            activations: 0,
        }));
        let mut cx = TestAppContext::single();
        let step = |action: &str, cx: &mut TestAppContext| {
            model::dispatch(&engine, action, json!({})).unwrap();
            cx.update(|cx| tick(&host, cx));
            cx.run_until_parked();
            host.borrow().window
        };
        let w = step("window-show", &mut cx).expect("window opened");
        w.update(&mut cx, |v, _, cx| {
            v.state.tab = Tab::Calendar;
            v.state.live_session = Some("session_a".into());
            cx.notify();
        })
        .unwrap();
        assert!(step("window-hide", &mut cx).is_none());
        assert!(cx.update(|cx| cx.windows().is_empty()));
        let w = step("window-show", &mut cx).expect("window reopened");
        w.update(&mut cx, |v, _, _| {
            assert_eq!(v.state.tab, Tab::Calendar);
            assert_eq!(v.state.live_session.as_deref(), Some("session_a"));
        })
        .unwrap();
    }

    fn host_of(engine: &Arc<Engine>) -> Shared {
        let port = Arc::new(drive::Port::default());
        engine.set_ui(Some(port.clone()));
        Rc::new(RefCell::new(Host {
            engine: Arc::clone(engine),
            events: engine.bus.subscribe(vec![]),
            tray_state: Arc::new(TrayState::new(false)),
            tray: Box::new(Mock::default()),
            window: None,
            kept: Rc::default(),
            applied_capture: None,
            want_visible: false,
            tray_gate: tray::Gate::default(),
            tray_entries: model::entries(model::TRAY, engine.capture_hiding_supported()),
            capture_supported: engine.capture_hiding_supported(),
            port,
            launch_show: false,
            quitting: false,
            activations: 0,
        }))
    }

    /// One action the way `ctl` sends it: from another thread, while the GPUI thread ticks.
    fn ctl(
        host: &Shared,
        cx: &mut TestAppContext,
        action: &str,
        params: Value,
    ) -> oats_core::config::Result<Value> {
        let engine = Arc::clone(&host.borrow().engine);
        let (id, p) = (action.to_string(), params);
        let t = std::thread::spawn(move || model::dispatch(&engine, &id, p));
        while !t.is_finished() {
            cx.update(|cx| tick(host, cx));
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        cx.update(|cx| tick(host, cx));
        cx.run_until_parked();
        t.join().unwrap()
    }

    #[test]
    fn ctl_view_select_switches_the_window_and_the_state_dump_shows_it() {
        let (engine, _d) = testutil::engine(&[]);
        // Headless (no GUI port): the ui-* actions say there is no window.
        let e = model::dispatch(&engine, "ui-state", json!({})).unwrap_err();
        assert_eq!(e.code, "no_window");
        let host = host_of(&engine);
        let mut cx = TestAppContext::single();
        // GUI mode with the window hidden: still no window to drive.
        let e = ctl(&host, &mut cx, "ui-select", json!({"tab": "Calendar"})).unwrap_err();
        assert_eq!(e.code, "no_window");
        ctl(&host, &mut cx, "window-show", json!({})).unwrap();
        let w = host.borrow().window.expect("window opened");
        for t in Tab::ALL {
            let name = t.label().to_lowercase();
            let d = ctl(&host, &mut cx, "ui-select", json!({ "tab": name })).unwrap();
            assert_eq!(d["tab"], t.label());
            assert!(d["view"].is_object(), "{d}");
            w.update(&mut cx, |v, _, _| assert_eq!(v.state.tab, t))
                .unwrap();
            let s = ctl(&host, &mut cx, "ui-state", json!({})).unwrap();
            assert_eq!(s["tab"], t.label());
            assert_eq!(s["view"], d["view"], "{t:?}");
        }
        let e = ctl(&host, &mut cx, "ui-select", json!({"tab": "Nope"})).unwrap_err();
        assert_eq!(e.code, "bad_request");
        let d = ctl(&host, &mut cx, "ui-select", json!({"tab": "Settings"})).unwrap();
        assert!(d["view"]["groups"]
            .as_array()
            .is_some_and(|g| !g.is_empty()));
        // A window closed and reopened still answers, on the tab it kept.
        ctl(&host, &mut cx, "window-hide", json!({})).unwrap();
        ctl(&host, &mut cx, "window-show", json!({})).unwrap();
        let s = ctl(&host, &mut cx, "ui-state", json!({})).unwrap();
        assert_eq!(s["tab"], "Settings");
    }

    #[test]
    fn ctl_presses_view_buttons_and_the_dump_shows_the_result() {
        let (engine, d) = testutil::engine(&[("ui.tz", json!("UTC"))]);
        let s = d.path().join("sessions/s1");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(
            s.join("session.json"),
            r#"{"id":"s1","title":"Standup","startedAt":"2026-10-27T10:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            s.join("transcript.final.jsonl"),
            "{\"speaker\":\"you\",\"text\":\"the budget is approved\"}\n",
        )
        .unwrap();
        let host = host_of(&engine);
        let mut cx = TestAppContext::single();
        ctl(&host, &mut cx, "window-show", json!({})).unwrap();
        let invoke = |cx: &mut TestAppContext, op: &str, args: Value| {
            ctl(&host, cx, "ui-invoke", json!({"op": op, "args": args})).unwrap()
        };

        let h = invoke(&mut cx, "help", json!({}));
        assert_eq!(h["ops"].as_array().unwrap().len(), drive::OPS.len());

        // Templates: New, fill the form, Save.
        let v = invoke(
            &mut cx,
            "templates.save",
            json!({"name": "Retro", "system_prompt": "Summarise.\n## Wins\n"}),
        );
        assert_eq!(v["tab"], "Templates");
        assert_eq!(v["view"]["error"], Value::Null, "{v}");
        assert!(v["view"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "Retro"));
        assert_eq!(v["view"]["selected"]["name"], "Retro");
        assert_eq!(v["view"]["draft"], Value::Null);

        // Sessions: the search box matches transcript text, with a snippet.
        let v = invoke(&mut cx, "sessions.search", json!({"query": "budget"}));
        assert_eq!(v["tab"], "Sessions");
        let rows = v["view"]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{v}");
        assert_eq!(rows[0]["id"], "s1");
        assert!(rows[0]["snippet"].as_str().unwrap().contains("budget"));
        let v = invoke(&mut cx, "sessions.search", json!({"query": "zebra"}));
        assert!(v["view"]["rows"].as_array().unwrap().is_empty());
        invoke(&mut cx, "sessions.search", json!({"query": ""}));
        let v = invoke(&mut cx, "sessions.select", json!({"session": "s1"}));
        assert_eq!(v["view"]["transcript"][0]["text"], "the budget is approved");
        let v = invoke(&mut cx, "sessions.rename", json!({"name": "Daily"}));
        assert_eq!(v["view"]["rows"][0]["title"], "Daily", "{v}");

        // Calendar: fill the add-feed form, Add.
        let ics = d.path().join("team.ics");
        std::fs::write(&ics, "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n").unwrap();
        let url = format!("file://{}", ics.display());
        let v = invoke(&mut cx, "calendar.add", json!({"name": "team", "url": url}));
        assert_eq!(v["tab"], "Calendar");
        assert!(v["view"]["feeds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "team" && f["url"] == url.as_str()));
        assert!(v["status"].as_str().unwrap().contains("team"), "{v}");

        // Settings: one row changed through the view.
        let v = invoke(
            &mut cx,
            "settings.set",
            json!({"key": "ui.show_elapsed", "value": false}),
        );
        let row = v["view"]["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["rows"].as_array().unwrap())
            .find(|r| r["path"] == "ui.show_elapsed")
            .cloned()
            .unwrap();
        assert_eq!(row["value"], false);

        // Bad ops and missing args are request errors, not silent no-ops.
        for (op, args) in [
            ("nope.op", json!({})),
            ("templates.save", json!({"name": "x"})),
        ] {
            let e = ctl(&host, &mut cx, "ui-invoke", json!({"op": op, "args": args})).unwrap_err();
            assert_eq!(e.code, "bad_request", "{op}");
        }
    }

    #[test]
    fn ticks_raise_the_window_only_on_a_show_request() {
        let (engine, _d) = testutil::engine(&[]);
        let host: Shared = Rc::new(RefCell::new(Host {
            engine: Arc::clone(&engine),
            events: engine.bus.subscribe(vec![]),
            tray_state: Arc::new(TrayState::new(false)),
            tray: Box::new(Mock::default()),
            window: None,
            kept: Rc::default(),
            applied_capture: None,
            want_visible: false,
            tray_gate: tray::Gate::default(),
            tray_entries: model::entries(model::TRAY, engine.capture_hiding_supported()),
            capture_supported: engine.capture_hiding_supported(),
            port: Arc::default(),
            launch_show: false,
            quitting: false,
            activations: 0,
        }));
        let mut cx = TestAppContext::single();
        let tick_n = |n: usize, cx: &mut TestAppContext| {
            for _ in 0..n {
                cx.update(|cx| tick(&host, cx));
                cx.run_until_parked();
            }
        };
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        tick_n(1, &mut cx);
        let w = host.borrow().window.expect("window opened");
        gpui::VisualTestContext::from_window(w.into(), &cx).deactivate_window();
        tick_n(5, &mut cx);
        assert_eq!(
            cx.update(|cx| cx.active_window()),
            None,
            "idle ticks raised the window"
        );
        // A show of the already-visible window raises it.
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        tick_n(1, &mut cx);
        assert_eq!(cx.update(|cx| cx.active_window()), Some(w.into()));
    }

    fn plain_host(engine: &Arc<Engine>) -> Shared {
        Rc::new(RefCell::new(Host {
            engine: Arc::clone(engine),
            events: engine.bus.subscribe(vec![]),
            tray_state: Arc::new(TrayState::new(false)),
            tray: Box::new(Mock::default()),
            window: None,
            kept: Rc::default(),
            applied_capture: None,
            want_visible: false,
            tray_gate: tray::Gate::default(),
            tray_entries: model::entries(model::TRAY, engine.capture_hiding_supported()),
            capture_supported: engine.capture_hiding_supported(),
            port: Arc::default(),
            launch_show: false,
            quitting: false,
            activations: 0,
        }))
    }

    fn tick_once(host: &Shared, cx: &mut TestAppContext) {
        cx.update(|cx| tick(host, cx));
        cx.run_until_parked();
    }

    #[test]
    fn reconcile_takes_focus_only_for_an_explicit_show_with_activate_on_show() {
        use Reconcile::*;
        // (want, open, raise, activate_on_show) -> what the tick does.
        for (want, open, raise, aos, out) in [
            (true, false, false, true, Open { focus: false }), // launch / idle open
            (true, false, true, true, Open { focus: true }),
            (true, false, true, false, Open { focus: false }),
            (true, true, true, true, Raise { activate: true }),
            (true, true, true, false, Raise { activate: false }),
            (true, true, false, true, Nothing),
            (false, true, false, true, Close),
            (false, true, true, true, Close),
            (false, false, false, true, Nothing),
        ] {
            assert_eq!(
                reconcile(want, open, raise, aos),
                out,
                "want={want} open={open} raise={raise} activate_on_show={aos}"
            );
        }
    }

    #[test]
    fn the_launch_show_opens_the_window_without_taking_focus() {
        let (engine, _d) = testutil::engine(&[]);
        let host = plain_host(&engine);
        let mut cx = TestAppContext::single();
        // What run_with does when ui.start_hidden is off.
        host.borrow_mut().launch_show = true;
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        tick_once(&host, &mut cx);
        assert!(host.borrow().window.is_some(), "window shown at launch");
        assert_eq!(host.borrow().activations, 0, "launch activated the app");
        assert!(!host.borrow().launch_show);
        // Idle ticks never activate either.
        for _ in 0..3 {
            tick_once(&host, &mut cx);
        }
        assert_eq!(host.borrow().activations, 0);
        // An explicit show (tray, ctl window-show) does, with ui.activate_on_show on (default).
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        tick_once(&host, &mut cx);
        assert_eq!(host.borrow().activations, 1);
        // Hidden then shown again: reopened with focus.
        model::dispatch(&engine, "window-hide", json!({})).unwrap();
        tick_once(&host, &mut cx);
        assert!(host.borrow().window.is_none());
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        tick_once(&host, &mut cx);
        assert!(host.borrow().window.is_some());
        assert_eq!(host.borrow().activations, 2);
    }

    #[test]
    fn a_show_request_never_activates_with_activate_on_show_off() {
        let (engine, _d) = testutil::engine(&[("ui.activate_on_show", json!(false))]);
        assert!(!engine.snapshot().config.ui.activate_on_show);
        let host = plain_host(&engine);
        let mut cx = TestAppContext::single();
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        tick_once(&host, &mut cx);
        assert!(host.borrow().window.is_some(), "still shown");
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        tick_once(&host, &mut cx);
        assert!(host.borrow().window.is_some());
        assert_eq!(host.borrow().activations, 0);
    }

    #[test]
    fn the_window_keys_have_their_defaults() {
        let (engine, _d) = testutil::engine(&[]);
        let ui = &engine.snapshot().config.ui;
        assert!(!ui.start_hidden);
        assert!(ui.activate_on_show);
        assert!(ui.quit_timeout.as_duration() > std::time::Duration::ZERO);
    }

    #[test]
    fn quit_closes_the_window_and_quits_once() {
        let (engine, _d) = testutil::engine(&[]);
        let host = plain_host(&engine);
        let mut cx = TestAppContext::single();
        model::dispatch(&engine, "window-show", json!({})).unwrap();
        tick_once(&host, &mut cx);
        assert!(host.borrow().window.is_some());
        model::dispatch(&engine, "quit", json!({})).unwrap();
        tick_once(&host, &mut cx);
        assert!(host.borrow().quitting);
        assert!(host.borrow().window.is_none(), "window kept open at quit");
        assert!(cx.update(|cx| cx.windows().is_empty()));
        // Later ticks do not quit again.
        tick_once(&host, &mut cx);
        assert!(host.borrow().quitting);
    }

    /// Wait (this thread blocked, the way a wedged main thread is) until `cond` or 10 s.
    fn blocked_until(cond: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !cond() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cond()
    }

    #[test]
    fn the_quit_watchdog_fires_with_the_main_loop_blocked_and_removes_the_lock() {
        use std::sync::atomic::{AtomicI32, AtomicUsize};
        static EXITS: AtomicUsize = AtomicUsize::new(0);
        static CODE: AtomicI32 = AtomicI32::new(-1);
        let (engine, d) = testutil::engine(&[("ui.quit_timeout", json!("200ms"))]);
        let lock = paths::lock_path(d.path());
        std::fs::write(&lock, "").unwrap();
        let _w = spawn_quit_watchdog(&engine, lock.clone(), |code| {
            CODE.store(code, Ordering::SeqCst);
            EXITS.fetch_add(1, Ordering::SeqCst);
        });
        // Nothing before the quit: the watchdog is not a timer from launch.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(EXITS.load(Ordering::SeqCst), 0);
        assert!(lock.exists());
        let t = std::time::Instant::now();
        model::dispatch(&engine, "quit", json!({})).unwrap();
        // No tick runs from here on: only the watchdog thread can end the process.
        assert!(blocked_until(|| EXITS.load(Ordering::SeqCst) == 1));
        assert!(t.elapsed() >= std::time::Duration::from_millis(200));
        assert_eq!(CODE.load(Ordering::SeqCst), 0);
        assert!(!lock.exists(), "engine.lock left on disk");
    }

    #[test]
    fn a_quit_that_finishes_first_cancels_the_watchdog() {
        use std::sync::atomic::AtomicUsize;
        static EXITS: AtomicUsize = AtomicUsize::new(0);
        let (engine, d) = testutil::engine(&[("ui.quit_timeout", json!("200ms"))]);
        let lock = paths::lock_path(d.path());
        std::fs::write(&lock, "").unwrap();
        let w = spawn_quit_watchdog(&engine, lock.clone(), |_| {
            EXITS.fetch_add(1, Ordering::SeqCst);
        });
        model::dispatch(&engine, "quit", json!({})).unwrap();
        w.cancel();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(EXITS.load(Ordering::SeqCst), 0);
        assert!(lock.exists());
    }

    #[test]
    fn app_id_matches_the_desktop_file() {
        let desktop = include_str!("../../../packaging/linux/oats-meetings-rs.desktop");
        assert!(desktop
            .lines()
            .any(|l| l == format!("StartupWMClass={APP_ID}")));
        assert!(desktop.lines().any(|l| l == format!("Icon={APP_ID}")));
    }
}
