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

/// No tray on this platform.
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

/// Linux / BSD: a StatusNotifierItem (KDE, waybar, GNOME with the AppIndicator extension, ...).
/// The SNI service runs on its own thread; menu activations queue action ids that `take_clicks`
/// drains on the UI thread, so clicks reach the engine through the same `model::dispatch` path.
#[cfg(all(unix, not(target_os = "macos")))]
pub mod sni {
    use super::*;
    use ksni::blocking::{Handle, TrayMethods};
    use std::sync::{Arc, Mutex};

    type Clicks = Arc<Mutex<Vec<String>>>;

    /// The SNI object: the tray entries and where clicks go.
    pub struct Item {
        entries: Vec<Entry>,
        clicks: Clicks,
    }

    impl Item {
        pub fn new(entries: &[Entry]) -> Self {
            Self { entries: entries.to_vec(), clicks: Clicks::default() }
        }

        fn push(&self, id: &str) {
            self.clicks.lock().unwrap().push(id.to_string());
        }
    }

    /// SNI labels treat `_` as a mnemonic marker; double it so labels show verbatim.
    fn label(s: &str) -> String {
        s.replace('_', "__")
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "oats-meetings".into()
        }

        fn title(&self) -> String {
            "Oats".into()
        }

        fn category(&self) -> ksni::Category {
            ksni::Category::ApplicationStatus
        }

        /// Same 16x16 placeholder glyph as tray-icon, in SNI's ARGB byte order (dot-4uzgol.10).
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            let data = (0..16 * 16).flat_map(|_| [0xffu8, 0x89, 0xb4, 0xfa]).collect();
            vec![ksni::Icon { width: 16, height: 16, data }]
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip { title: "Oats".into(), ..Default::default() }
        }

        /// Primary click on the icon (waybar left click) toggles the window; the menu is on the
        /// secondary click, as on every SNI host.
        fn activate(&mut self, _x: i32, _y: i32) {
            self.push("window-toggle");
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            self.entries
                .iter()
                .map(|e| match e.action {
                    None => ksni::MenuItem::Separator,
                    Some(id) => ksni::menu::StandardItem {
                        label: label(e.label),
                        activate: Box::new(move |t: &mut Self| t.push(id)),
                        ..Default::default()
                    }
                    .into(),
                })
                .collect()
        }
    }

    #[derive(Default)]
    pub struct Sni {
        handle: Option<Handle<Item>>,
        clicks: Clicks,
    }

    impl TrayBackend for Sni {
        fn show(&mut self, entries: &[Entry]) -> Result<(), String> {
            let item = Item::new(entries);
            let clicks = item.clicks.clone();
            // Errors when there is no session bus or no StatusNotifierWatcher (no tray host).
            let handle = item.spawn().map_err(|e| format!("no StatusNotifierItem host: {e}"))?;
            self.clicks = clicks;
            self.handle = Some(handle);
            Ok(())
        }

        fn hide(&mut self) {
            if let Some(h) = self.handle.take() {
                h.shutdown().wait();
            }
        }

        fn visible(&self) -> bool {
            self.handle.as_ref().is_some_and(|h| !h.is_closed())
        }

        fn take_clicks(&mut self) -> Vec<String> {
            std::mem::take(&mut *self.clicks.lock().unwrap())
        }
    }

    impl Drop for Sni {
        fn drop(&mut self) {
            self.hide();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::model;
        use ksni::Tray;

        #[test]
        fn menu_mirrors_the_tray_entries() {
            let item = Item::new(model::TRAY);
            let menu = item.menu();
            assert_eq!(menu.len(), model::TRAY.len());
            for (m, e) in menu.iter().zip(model::TRAY) {
                match (m, e.action) {
                    (ksni::MenuItem::Separator, None) => {}
                    (ksni::MenuItem::Standard(s), Some(_)) => assert_eq!(s.label, e.label),
                    (m, _) => panic!("{m:?} does not match {e:?}"),
                }
            }
        }

        #[test]
        fn menu_clicks_queue_their_action_ids() {
            let mut item = Item::new(model::TRAY);
            let mut want = vec![];
            for (i, e) in model::TRAY.iter().enumerate() {
                let Some(id) = e.action else { continue };
                let ksni::MenuItem::Standard(s) = &item.menu()[i] else { unreachable!() };
                (s.activate)(&mut item);
                want.push(id.to_string());
            }
            item.activate(0, 0);
            want.push("window-toggle".into());
            let mut b = Sni { handle: None, clicks: item.clicks.clone() };
            assert_eq!(b.take_clicks(), want);
            assert!(b.take_clicks().is_empty());
        }

        #[test]
        fn icon_is_a_16px_argb_pixmap() {
            let icons = Item::new(model::TRAY).icon_pixmap();
            assert_eq!((icons[0].width, icons[0].height, icons[0].data.len()), (16, 16, 16 * 16 * 4));
        }

        #[test]
        fn underscores_survive_the_mnemonic_rule() {
            assert_eq!(label("a_b"), "a__b");
        }

        /// The pod has no session bus; neither does a bare tty. The gate must get an error, not a panic.
        #[test]
        fn no_bus_or_no_host_is_unsupported_not_a_panic() {
            if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some() {
                return; // a desktop may really show it; that is the neon acceptance, not a unit test
            }
            let mut b = Sni::default();
            let mut g = Gate::default();
            assert!(g.step(true, &mut b, model::TRAY).is_some());
            assert!(!b.visible());
            assert_eq!(g.step(true, &mut b, model::TRAY), None);
        }
    }
}

/// The backend for this platform.
pub fn native_backend() -> Box<dyn TrayBackend> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        Box::new(native::Native::default())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Box::new(sni::Sni::default())
    }
    #[cfg(not(any(unix, target_os = "windows")))]
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
