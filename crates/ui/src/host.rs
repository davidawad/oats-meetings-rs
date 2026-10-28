//! GUI mode hosts the control engine (dot-4uzgol.5 acceptance): the same `Engine` and control
//! socket as `--headless`, plus a GPUI viewer that mirrors engine state. `ctl ping` and
//! `ctl list-actions` work against this process.
//!
//! Engine -> window: a 100 ms tick on the GPUI thread drains the event bus and reconciles window
//! visibility, the tray and capture hiding against the engine's active config. Window -> engine:
//! menus, keys, tray clicks and view buttons all call `model::dispatch`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use gpui::{
    App, AppContext as _, AsyncApp, Bounds, KeyBinding, Menu, MenuItem, QuitMode,
    WindowBounds, WindowHandle, WindowOptions, px, size,
};
use oats_core::config::ConfigError;
use oats_core::control::engine::Engine;
use oats_core::control::{paths, server};
use serde_json::{json, Value};

use crate::capture;
use crate::model::{self, BINDINGS, MENUS};
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


fn menus() -> Vec<Menu> {
    MENUS
        .iter()
        .map(|m| {
            Menu::new(m.title).items(m.items.iter().map(|e| match e.action {
                None => MenuItem::separator(),
                Some(id) => MenuItem::action(e.label, Do { id: id.to_string() }),
            }))
        })
        .collect()
}

fn mod_prefix() -> &'static str {
    if cfg!(target_os = "macos") { "cmd-" } else { "ctrl-" }
}

fn bindings(cx: &App) -> Vec<KeyBinding> {
    BINDINGS
        .iter()
        .filter_map(|b| {
            KeyBinding::load(
                &format!("{}{}", mod_prefix(), b.keys),
                Box::new(Do { id: b.action.to_string() }),
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
    applied_capture: Option<capture::Plan>,
    /// What the engine says the window should be (from `window-changed`).
    want_visible: bool,
}

type Shared = Rc<RefCell<Host>>;

fn capture_plan(engine: &Engine) -> capture::Plan {
    let cfg = engine.snapshot();
    capture::plan(
        cfg.config.privacy.hide_from_capture,
        &engine.resolved().flat["privacy.hide_method"].as_str().unwrap_or("none").to_string(),
    )
}

fn open_window(host: &mut Host, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
    let engine = Arc::clone(&host.engine);
    match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // Wayland app_id / X11 WM_CLASS; matches packaging/linux/*.desktop StartupWMClass.
            app_id: Some(APP_ID.to_string()),
            ..Default::default()
        },
        move |_, cx| cx.new(|_| MainWindow::new(engine)),
    ) {
        Ok(h) => {
            host.window = Some(h);
            host.applied_capture = None; // new window: capture plan must be applied again
        }
        Err(e) => {
            // Keep the engine's idea of the window true: nothing is showing.
            eprintln!("oats-meetings-rs: cannot open window: {e}");
            host.want_visible = false;
            let _ = model::dispatch(&host.engine, "window-hide", json!({}));
        }
    }
}

fn tick(host: &Shared, cx: &mut App) {
    let mut h = host.borrow_mut();
    if h.engine.is_shutdown() {
        cx.quit();
        return;
    }
    let mut changed_sessions = false;
    let lines: Vec<String> = h.events.try_iter().collect();
    for line in &lines {
        let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        match v["event"].as_str() {
            Some("window-changed") => h.want_visible = v["data"]["visible"].as_bool().unwrap_or(false),
            Some("session-saved" | "session-deleted" | "config-reloaded" | "setting-changed") => changed_sessions = true,
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
    match (h.want_visible, h.window) {
        (true, None) => open_window(&mut h, cx),
        (true, Some(w)) => {
            let _ = w.update(cx, |_, window, _| window.activate_window());
        }
        (false, Some(w)) => {
            let _ = w.update(cx, |_, window, _| window.remove_window());
            h.window = None;
        }
        (false, None) => {}
    }
    if let Some(w) = h.window {
        let plan = capture_plan(&h.engine);
        if h.applied_capture != Some(plan) {
            let out = w.update(cx, |_, window, _| capture::apply(window, plan));
            match out {
                Ok(capture::Applied::Failed(e)) => eprintln!("oats-meetings-rs: capture hiding failed: {e}"),
                Ok(capture::Applied::WrongPlatform(why)) => eprintln!("oats-meetings-rs: capture hiding unavailable: {why}"),
                _ => {}
            }
            h.applied_capture = Some(plan);
        }
        for line in lines {
            let _ = w.update(cx, |v, _, cx| v.event(&line, cx));
        }
        if changed_sessions {
            let _ = w.update(cx, |v, _, cx| v.refresh(cx));
        }
    }
    let want = h.tray_state.wanted();
    let h = &mut *h;
    if let tray::Reconciled::Unsupported(why) = tray::reconcile(want, &mut *h.tray, model::TRAY) {
        eprintln!("oats-meetings-rs: app.menubar is on but {why}");
    }
    for id in h.tray.take_clicks() {
        let _ = model::dispatch(&h.engine, &id, json!({}));
    }
}

/// Run the GUI over `engine`, serving the control socket under `runtime_dir`. Returns the exit code.
pub fn run(engine: Arc<Engine>, runtime_dir: &std::path::Path) -> i32 {
    run_with(engine, runtime_dir, tray::native_backend())
}

pub fn run_with(engine: Arc<Engine>, runtime_dir: &std::path::Path, backend: Box<dyn TrayBackend>) -> i32 {
    let tray_state = Arc::new(TrayState::new(engine.snapshot().config.app.menubar));
    engine.register(tray_state.clone());
    let handle = match server::serve(Arc::clone(&engine), runtime_dir) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    eprintln!("oats-meetings-rs: engine listening on {}", handle.socket_display());
    let host: Shared = Rc::new(RefCell::new(Host {
        engine: Arc::clone(&engine),
        events: engine.bus.subscribe(vec![]),
        tray_state,
        tray: backend,
        window: None,
        applied_capture: None,
        want_visible: false,
    }));
    let h2 = Rc::clone(&host);
    gpui_platform::application().run(move |cx: &mut App| {
        cx.set_quit_mode(QuitMode::Explicit);
        cx.bind_keys(bindings(cx));
        cx.set_menus(menus());
        let eng = Arc::clone(&h2.borrow().engine);
        cx.on_action(move |a: &Do, _cx: &mut App| {
            if let Err(e) = model::dispatch(&eng, &a.id, json!({})) {
                if e.code != "not_implemented" {
                    eprintln!("oats-meetings-rs: {}: {e}", a.id);
                }
            }
        });
        // Show the window through the engine so `window-changed` and the control API agree.
        let _ = model::dispatch(&h2.borrow().engine, "window-show", json!({}));
        let h3 = Rc::clone(&h2);
        cx.spawn(async move |cx: &mut AsyncApp| loop {
            // ui.tick is read each round, so a reload changes the cadence at once.
            let every = h3.borrow().engine.snapshot().config.ui.tick.as_duration();
            cx.background_executor().timer(every).await;
            cx.update(|cx| tick(&h3, cx));
        })
        .detach();
        cx.activate(true);
    });
    handle.shutdown();
    0
}

/// `oats-meetings-rs` with no flags: engine from the environment, then the GUI.
pub fn run_env() -> i32 {
    let env = |k: &str| std::env::var(k).ok();
    match Engine::from_env(&env) {
        Ok(engine) => run(engine, &paths::runtime_dir(&env)),
        Err(ConfigError { .. }) => {
            eprintln!("oats-meetings-rs: cannot start the engine");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_id_matches_the_desktop_file() {
        let desktop = include_str!("../../../packaging/linux/oats-meetings-rs.desktop");
        assert!(desktop.lines().any(|l| l == format!("StartupWMClass={}", super::APP_ID)));
        assert!(desktop.lines().any(|l| l == format!("Icon={}", super::APP_ID)));
    }
}
