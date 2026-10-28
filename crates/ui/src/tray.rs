//! Menubar / tray item, behind `app.menubar` (David's ruling: a config option, tested on and off).
//!
//! `TrayState` is the engine side: a `Subsystem` that records the desired state on every reload,
//! from any thread. `reconcile` is the UI-thread side: it makes a `TrayBackend` match. Native tray
//! handles are not `Send` on macOS, so the backend lives on the GPUI main thread and is only ever
//! touched by `reconcile`.

use std::sync::atomic::{AtomicBool, Ordering};

use oats_core::config::typed::Config;
use oats_core::control::subsystem::Subsystem;

use crate::model::Entry;

pub trait TrayBackend {
    /// Create the tray item with these entries (separators have `action: None`).
    fn show(&mut self, entries: &[Entry]) -> Result<(), String>;
    fn hide(&mut self);
    fn visible(&self) -> bool;
    /// Action ids clicked since the last call.
    fn take_clicks(&mut self) -> Vec<String>;
}

pub struct TrayState {
    want: AtomicBool,
}

impl TrayState {
    pub fn new(initial: bool) -> Self {
        Self { want: AtomicBool::new(initial) }
    }

    pub fn wanted(&self) -> bool {
        self.want.load(Ordering::SeqCst)
    }
}

impl Subsystem for TrayState {
    fn name(&self) -> &'static str {
        "tray"
    }

    fn apply(&self, _old: &Config, new: &Config) {
        self.want.store(new.app.menubar, Ordering::SeqCst);
    }
}

/// Outcome of one reconcile, for logging and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciled {
    Unchanged,
    Shown,
    Hidden,
    /// The platform has no tray; carries the reason. Not retried until `want` flips.
    Unsupported(String),
}

pub fn reconcile(want: bool, backend: &mut dyn TrayBackend, entries: &[Entry]) -> Reconciled {
    match (want, backend.visible()) {
        (true, false) => match backend.show(entries) {
            Ok(()) => Reconciled::Shown,
            Err(e) => Reconciled::Unsupported(e),
        },
        (false, true) => {
            backend.hide();
            Reconciled::Hidden
        }
        _ => Reconciled::Unchanged,
    }
}

/// Reconcile once per tick without repeating a refusal: an unsupported tray is reported once each
/// time `app.menubar` is turned on, and not retried until it goes off and on again (dot-4uzgol.33).
#[derive(Default)]
pub struct Gate {
    unsupported: bool,
}

impl Gate {
    /// Returns the reason to log when the backend refuses the tray now.
    pub fn step(&mut self, want: bool, backend: &mut dyn TrayBackend, entries: &[Entry]) -> Option<String> {
        if !want {
            self.unsupported = false;
        } else if self.unsupported {
            return None;
        }
        match reconcile(want, backend, entries) {
            Reconciled::Unsupported(why) => {
                self.unsupported = true;
                Some(why)
            }
            _ => None,
        }
    }
}

/// No tray on this platform (Linux: GPUI has no tray API and tray-icon needs gtk + appindicator).
pub struct Unsupported;

impl TrayBackend for Unsupported {
    fn show(&mut self, _: &[Entry]) -> Result<(), String> {
        Err("tray is not supported on this platform; use the app menu or `ctl`".into())
    }
    fn hide(&mut self) {}
    fn visible(&self) -> bool {
        false
    }
    fn take_clicks(&mut self) -> Vec<String> {
        vec![]
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod native {
    use super::*;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    /// 16x16 flat placeholder glyph; the real icon is the packaging bead's (dot-4uzgol.10).
    fn glyph() -> Result<Icon, String> {
        let px: Vec<u8> = (0..16 * 16).flat_map(|_| [0x89u8, 0xb4, 0xfa, 0xff]).collect();
        Icon::from_rgba(px, 16, 16).map_err(|e| e.to_string())
    }

    #[derive(Default)]
    pub struct Native {
        icon: Option<TrayIcon>,
    }

    impl TrayBackend for Native {
        fn show(&mut self, entries: &[Entry]) -> Result<(), String> {
            let menu = Menu::new();
            for e in entries {
                match e.action {
                    None => menu.append(&PredefinedMenuItem::separator()),
                    Some(id) => menu.append(&MenuItem::with_id(id, e.label, true, None)),
                }
                .map_err(|e| e.to_string())?;
            }
            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("Oats")
                .with_icon(glyph()?)
                .build()
                .map_err(|e| e.to_string())?;
            self.icon = Some(icon);
            Ok(())
        }

        fn hide(&mut self) {
            self.icon = None; // dropping the TrayIcon removes it
        }

        fn visible(&self) -> bool {
            self.icon.is_some()
        }

        fn take_clicks(&mut self) -> Vec<String> {
            let mut out = vec![];
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                out.push(ev.id.0.clone());
            }
            out
        }
    }
}

/// The backend for this platform.
pub fn native_backend() -> Box<dyn TrayBackend> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Box::new(native::Native::default())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Box::new(Unsupported)
    }
}

#[cfg(test)]
pub mod mock {
    use super::*;

    #[derive(Default)]
    pub struct Mock {
        pub shown: Vec<Vec<Entry>>,
        pub hides: usize,
        pub up: bool,
        pub clicks: Vec<String>,
    }

    impl TrayBackend for Mock {
        fn show(&mut self, entries: &[Entry]) -> Result<(), String> {
            self.shown.push(entries.to_vec());
            self.up = true;
            Ok(())
        }
        fn hide(&mut self) {
            self.hides += 1;
            self.up = false;
        }
        fn visible(&self) -> bool {
            self.up
        }
        fn take_clicks(&mut self) -> Vec<String> {
            std::mem::take(&mut self.clicks)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::Mock;
    use super::*;
    use crate::{model, testutil};
    use serde_json::json;
    use std::sync::Arc;

    fn wired(initial: &[(&str, serde_json::Value)]) -> (Arc<oats_core::control::engine::Engine>, Arc<TrayState>, tempfile::TempDir) {
        let (engine, dir) = testutil::engine(initial);
        let state = Arc::new(TrayState::new(engine.snapshot().config.app.menubar));
        engine.register(state.clone());
        (engine, state, dir)
    }

    #[test]
    fn menubar_off_never_creates_a_tray() {
        let (_e, state, _d) = wired(&[("app.menubar", json!(false))]);
        let mut b = Mock::default();
        assert_eq!(reconcile(state.wanted(), &mut b, model::TRAY), Reconciled::Unchanged);
        assert!(b.shown.is_empty() && !b.visible());
    }

    #[test]
    fn menubar_on_creates_the_tray_with_the_tray_entries() {
        let (_e, state, _d) = wired(&[("app.menubar", json!(true))]);
        let mut b = Mock::default();
        assert_eq!(reconcile(state.wanted(), &mut b, model::TRAY), Reconciled::Shown);
        assert_eq!(b.shown, vec![model::TRAY.to_vec()]);
        // idempotent
        assert_eq!(reconcile(state.wanted(), &mut b, model::TRAY), Reconciled::Unchanged);
        assert_eq!(b.shown.len(), 1);
    }

    #[test]
    fn toggling_the_key_at_runtime_shows_then_hides() {
        let (engine, state, _d) = wired(&[]);
        let mut b = Mock::default();
        assert_eq!(reconcile(state.wanted(), &mut b, model::TRAY), Reconciled::Unchanged);
        model::dispatch(&engine, "config-set", json!({"key": "app.menubar", "value": true})).unwrap();
        assert_eq!(reconcile(state.wanted(), &mut b, model::TRAY), Reconciled::Shown);
        model::dispatch(&engine, "config-set", json!({"key": "app.menubar", "value": false})).unwrap();
        assert_eq!(reconcile(state.wanted(), &mut b, model::TRAY), Reconciled::Hidden);
        assert_eq!((b.shown.len(), b.hides), (1, 1));
    }

    #[test]
    fn an_unsupported_tray_is_reported_once_per_turn_on() {
        let (mut g, mut b) = (Gate::default(), Unsupported);
        assert!(g.step(true, &mut b, model::TRAY).is_some());
        for _ in 0..150 {
            assert_eq!(g.step(true, &mut b, model::TRAY), None);
        }
        assert_eq!(g.step(false, &mut b, model::TRAY), None);
        assert!(g.step(true, &mut b, model::TRAY).is_some());
    }

    #[test]
    fn unsupported_platform_reports_and_does_not_panic() {
        let mut b = Unsupported;
        assert!(matches!(reconcile(true, &mut b, model::TRAY), Reconciled::Unsupported(_)));
        assert_eq!(reconcile(false, &mut b, model::TRAY), Reconciled::Unchanged);
    }

    #[test]
    fn tray_click_dispatches_through_the_one_grammar() {
        let (engine, _s, _d) = wired(&[("app.menubar", json!(true))]);
        let mut b = Mock::default();
        b.clicks.push("window-toggle".into());
        for id in b.take_clicks() {
            model::dispatch(&engine, &id, json!({})).unwrap();
        }
        let v = model::dispatch(&engine, "window-toggle", json!({})).unwrap();
        assert_eq!(v["visible"], json!(false)); // first click showed it, this one hides it
    }
}
