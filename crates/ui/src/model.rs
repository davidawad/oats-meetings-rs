//! The UI's surfaces as data. Every menu item, keybinding, tray item and view button names an
//! action id from the one `Action` grammar (`oats_core::action`); none has private logic. The
//! `actions_parity` test proves the union of those surfaces plus an explicit exempt list is
//! exactly `ctl list-actions`, so a new action cannot be added without deciding its UI surface.

use std::collections::BTreeSet;

use oats_core::action::{self, Action};
use oats_core::config::Result;
use oats_core::control::engine::Engine;
use serde_json::{json, Value};

/// One entry of a menu or tray menu. `action: None` is a separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub label: &'static str,
    pub action: Option<&'static str>,
}

const fn item(label: &'static str, action: &'static str) -> Entry {
    Entry {
        label,
        action: Some(action),
    }
}

pub const SEP: Entry = Entry {
    label: "",
    action: None,
};

pub struct Menu {
    pub title: &'static str,
    pub items: &'static [Entry],
}

pub const MENUS: &[Menu] = &[
    Menu {
        title: "Oats",
        items: &[
            item("Hide from Screen Capture", "privacy-toggle"),
            item("Reload Config", "config-reload"),
            item("Check Config", "config-check"),
            SEP,
            item("Quit", "quit"),
        ],
    },
    Menu {
        title: "Recording",
        items: &[
            item("Start Recording", "record-start"),
            item("Stop Recording", "record-stop"),
            item("Pause / Resume", "record-pause"),
        ],
    },
    Menu {
        title: "Library",
        items: &[
            item("Refresh Sessions", "session-list"),
            item("Reindex Knowledge Base", "kb-reindex"),
            item("Refresh Calendar Feeds", "ics-refresh"),
            item("Reload Templates", "template-list"),
        ],
    },
    Menu {
        title: "Window",
        items: &[
            item("Show", "window-show"),
            item("Hide", "window-hide"),
            item("Toggle", "window-toggle"),
        ],
    },
];

/// One window command a keyboard shortcut runs. Its keys are the config key
/// `ui.keys.<command>` (`crate::shortcuts`), so every binding can be rebound, reset or disabled.
/// `window: false` commands are `oats_core::action` ids the engine runs; `window: true` ones
/// act on the window itself (switch tab, move the selection, the help overlay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub command: &'static str,
    pub label: &'static str,
    /// The heading it is listed under in the help overlay.
    pub group: &'static str,
    pub window: bool,
}

const fn action(command: &'static str, label: &'static str, group: &'static str) -> Binding {
    Binding {
        command,
        label,
        group,
        window: false,
    }
}

const fn window(command: &'static str, label: &'static str, group: &'static str) -> Binding {
    Binding {
        command,
        label,
        group,
        window: true,
    }
}

/// Every shortcut, in the order the help overlay and the Settings section list them.
pub const BINDINGS: &[Binding] = &[
    window(
        "record-toggle",
        "Start recording, or pause / resume",
        "Recording",
    ),
    action("record-start", "Start recording", "Recording"),
    action("record-pause", "Pause / resume", "Recording"),
    action("record-stop", "Stop recording", "Recording"),
    window("session-new", "New session (clear Live)", "Recording"),
    window("tab-live", "Live", "Tabs"),
    window("tab-sessions", "Sessions", "Tabs"),
    window("tab-notes", "Notes", "Tabs"),
    window("tab-templates", "Templates", "Tabs"),
    window("tab-calendar", "Calendar", "Tabs"),
    window("tab-models", "Models", "Tabs"),
    window("tab-settings", "Settings", "Tabs"),
    window("sessions-next", "Next session", "Sessions"),
    window("sessions-prev", "Previous session", "Sessions"),
    window("sessions-open", "Open the session's notes", "Sessions"),
    window(
        "search",
        "Search sessions, or settings on Settings",
        "Sessions",
    ),
    action("session-list", "Reload the session list", "Sessions"),
    window("back", "Back", "Window"),
    window("help", "Show / hide this list", "Window"),
    action("privacy-toggle", "Hide from screen capture", "Window"),
    action("window-hide", "Hide the window", "Window"),
    action("window-toggle", "Show / hide the window", "Window"),
    action("window-show", "Show the window", "Window"),
    action("quit", "Quit", "Window"),
];

/// The binding for a command id.
pub fn binding(command: &str) -> Option<&'static Binding> {
    BINDINGS.iter().find(|b| b.command == command)
}

/// True for a command the window runs itself (never sent to the engine).
pub fn is_window_command(command: &str) -> bool {
    binding(command).is_some_and(|b| b.window)
}

pub const TRAY: &[Entry] = &[
    item("Show / Hide Window", "window-toggle"),
    SEP,
    item("Start Recording", "record-start"),
    item("Stop Recording", "record-stop"),
    item("Hide from Screen Capture", "privacy-toggle"),
    SEP,
    item("Quit", "quit"),
];

/// Label of the privacy toggle where `privacy.hide_method` cannot take effect (dot-4uzgol.49): it
/// says so instead of offering an on/off that changes nothing.
pub const PRIVACY_UNSUPPORTED_LABEL: &str =
    "Hide from Screen Capture: not supported on this platform";

/// The label a surface shows for `e`, given whether capture hiding works on this platform.
pub fn label(e: &Entry, capture_supported: bool) -> &'static str {
    match e.action {
        Some("privacy-toggle") if !capture_supported => PRIVACY_UNSUPPORTED_LABEL,
        _ => e.label,
    }
}

/// `entries` with labels for this platform's capture support.
pub fn entries(entries: &[Entry], capture_supported: bool) -> Vec<Entry> {
    entries
        .iter()
        .map(|e| Entry {
            label: label(e, capture_supported),
            action: e.action,
        })
        .collect()
}

/// Actions the views invoke with arguments (buttons and edits in the main window).
pub const VIEW_ACTIONS: &[&str] = &[
    "session-list",
    "session-get",
    "transcript-get",
    "session-transcribe",
    "transcript-search",
    "session-rename",
    "session-tag",
    "session-delete",
    "trash-list",
    "trash-restore",
    "trash-empty",
    "config-set",
    "config-resolved",
    "stt-status",
    "stt-model-fetch",
    "stt-model-list",
    "stt-model-delete",
    "ics-list",
    "calendar-events",
    "calendar-sources-list",
    "calendar-select",
    "calendar-source-enable",
    "calendar-source-add",
    "calendar-access",
    "calendar-connect",
    "calendar-disconnect",
    "devices-list",
    "notes-get",
    "notes-set",
    "notes-generate",
    "template-get",
    "template-save",
    "template-delete",
    "hook-list",
    "hook-test",
    "hook-add",
    "hook-edit",
    "hook-remove",
    "hook-enable",
];

/// Actions with deliberately no UI surface: protocol plumbing, or batch/automation verbs that need
/// arguments the window has no place to type (use `ctl`). Each has a reason.
pub const EXEMPT: &[(&str, &str)] = &[
    ("ping", "protocol liveness"),
    ("version", "protocol handshake"),
    ("list-actions", "protocol manifest"),
    ("subscribe", "protocol event stream"),
    ("config-get", "the settings view reads config-resolved"),
    ("session-import", "one-shot migration verb (ctl)"),
    ("config-convert", "one-shot migration verb (ctl)"),
    ("ics-add", "feed management is ctl / config"),
    ("ics-remove", "feed management is ctl / config"),
    (
        "ics-events",
        "agent query (ctl); the Calendar view reads calendar-events",
    ),
    (
        "calendar-source-remove",
        "source management is ctl / config (Settings turns a source off)",
    ),
    ("calendar-match", "agent query (ctl)"),
    (
        "secret-set",
        "secrets never pass through the window (ctl, stdin)",
    ),
    ("secret-delete", "secrets never pass through the window"),
    ("secret-has", "secrets never pass through the window"),
    ("stt-transcribe", "batch file transcription (ctl)"),
    (
        "session-speaker-name",
        "naming remote_1..N is ctl for now (notes show the name)",
    ),
    ("template-import", "one-shot migration verb (ctl)"),
    (
        "template-export",
        "OpenOats interchange verb, like template-import (ctl)",
    ),
    (
        "session-export",
        "OpenOats markdown export to a folder (ctl, sessions.export_dir)",
    ),
    (
        "session-export-openoats",
        "OpenOats interchange verb, like session-import (ctl)",
    ),
    ("kb-search", "agent query (ctl)"),
    (
        "perms-status",
        "headless provisioning check (ctl); the window shows capture errors as they happen",
    ),
    (
        "perms-ensure",
        "headless provisioning verb (ctl): starts the audio server, may add a null sink",
    ),
    (
        "ui-select",
        "drives the window itself (ctl, headless acceptance)",
    ),
    (
        "ui-state",
        "reads the window itself (ctl, headless acceptance)",
    ),
    (
        "ui-invoke",
        "presses the window's own buttons (ctl, headless acceptance)",
    ),
];

fn ids<'a>(it: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    it.map(str::to_string).collect()
}

pub fn menu_ids() -> BTreeSet<String> {
    ids(MENUS
        .iter()
        .flat_map(|m| m.items.iter())
        .filter_map(|e| e.action))
}

/// Engine actions a shortcut runs (window commands are not actions).
pub fn binding_ids() -> BTreeSet<String> {
    ids(BINDINGS.iter().filter(|b| !b.window).map(|b| b.command))
}

pub fn tray_ids() -> BTreeSet<String> {
    ids(TRAY.iter().filter_map(|e| e.action))
}

pub fn view_ids() -> BTreeSet<String> {
    ids(VIEW_ACTIONS.iter().copied())
}

pub fn exempt_ids() -> BTreeSet<String> {
    ids(EXEMPT.iter().map(|e| e.0))
}

/// Union of every UI surface.
pub fn ui_ids() -> BTreeSet<String> {
    let mut s = menu_ids();
    s.extend(binding_ids());
    s.extend(tray_ids());
    s.extend(view_ids());
    s
}

/// Build an `Action` from an id plus parameters and run it on the engine. The only path from any
/// UI surface into the engine.
pub fn dispatch(engine: &Engine, id: &str, params: Value) -> Result<Value> {
    let mut obj = match params {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    obj.insert("action".into(), json!(id));
    let a: Action = serde_json::from_value(Value::Object(obj))
        .map_err(|e| oats_core::config::ConfigError::new("bad_request", format!("{id}: {e}")))?;
    engine.dispatch(&a)
}

/// `dispatch` from one of the window's surfaces (`window`, `menu`, `tray`): a `record-stop` or
/// `quit` says it came from there, so the log and the events name who stopped a recording or
/// quit the app (dot-4uzgol.99).
pub fn dispatch_from(engine: &Engine, id: &str, by: &str) -> Result<Value> {
    let params = if matches!(id, "record-stop" | "quit") {
        json!({ "by": by })
    } else {
        json!({})
    };
    dispatch(engine, id, params)
}

pub fn manifest_ids() -> BTreeSet<String> {
    ids(action::SPECS.iter().map(|s| s.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_toggle_says_unsupported_where_hiding_does_nothing() {
        for surface in [TRAY, MENUS[0].items] {
            let off = entries(surface, false);
            let on = entries(surface, true);
            let p = surface
                .iter()
                .position(|e| e.action == Some("privacy-toggle"))
                .unwrap();
            assert_eq!(off[p].label, PRIVACY_UNSUPPORTED_LABEL);
            assert_eq!(on[p].label, "Hide from Screen Capture");
            assert_eq!(off[p].action, Some("privacy-toggle"));
            for (i, e) in surface.iter().enumerate().filter(|(i, _)| *i != p) {
                assert_eq!((off[i], on[i]), (*e, *e));
            }
        }
    }

    #[test]
    fn linux_engine_reports_capture_hiding_unsupported() {
        // config/axes.json resolves hide_method none on Linux; the UI must not claim it works.
        // testutil::engine always resolves the linux axis, whatever the host OS.
        let (engine, _d) = crate::testutil::engine(&[]);
        assert!(!engine.capture_hiding_supported());
        let tray = entries(TRAY, false);
        assert!(tray.iter().any(|e| e.label == PRIVACY_UNSUPPORTED_LABEL));
        let r = dispatch(&engine, "privacy-toggle", json!({})).unwrap();
        assert_eq!(r["supported"], false);
        assert!(r["warning"].is_string());
    }

    #[test]
    fn actions_parity() {
        let manifest = manifest_ids();
        let ui = ui_ids();
        let exempt = exempt_ids();
        assert!(
            ui.is_disjoint(&exempt),
            "both UI and exempt: {:?}",
            ui.intersection(&exempt).collect::<Vec<_>>()
        );
        let unknown: Vec<_> = ui
            .union(&exempt)
            .filter(|i| !manifest.contains(*i))
            .collect();
        assert!(
            unknown.is_empty(),
            "ids not in the action manifest: {unknown:?}"
        );
        let union: BTreeSet<String> = ui.union(&exempt).cloned().collect();
        let missing: Vec<_> = manifest.difference(&union).collect();
        assert!(
            missing.is_empty(),
            "actions with no UI surface and not exempt: {missing:?}"
        );
    }

    #[test]
    fn surfaces_use_parameterless_actions() {
        // Menu, key and tray dispatch carry no arguments, so each id must build from its name alone.
        for id in menu_ids().iter().chain(&binding_ids()).chain(&tray_ids()) {
            let a: std::result::Result<Action, _> = serde_json::from_value(json!({"action": id}));
            assert!(
                a.is_ok(),
                "{id} needs parameters; it cannot be a menu / key / tray item"
            );
        }
    }

    #[test]
    fn every_exempt_has_a_reason_and_no_duplicates() {
        let mut seen = BTreeSet::new();
        for (id, why) in EXEMPT {
            assert!(!why.is_empty() && seen.insert(*id), "{id}");
        }
    }

    #[test]
    fn every_binding_is_a_ui_keys_key_and_window_commands_are_not_actions() {
        use oats_core::config::keys::{Kind, KEYS};
        let keyed: BTreeSet<&str> = KEYS
            .iter()
            .filter(|k| k.kind == Kind::Keys)
            .map(|k| k.path.strip_prefix("ui.keys.").expect("ui.keys.*"))
            .collect();
        let bound: BTreeSet<&str> = BINDINGS.iter().map(|b| b.command).collect();
        assert_eq!(bound.len(), BINDINGS.len(), "a command listed twice");
        assert_eq!(keyed, bound);
        let manifest = manifest_ids();
        for b in BINDINGS {
            assert_eq!(!b.window, manifest.contains(b.command), "{}", b.command);
            assert!(!b.label.is_empty() && !b.group.is_empty());
        }
    }

    #[test]
    fn menu_dispatch_reaches_engine() {
        let (engine, _dir) = crate::testutil::engine(&[]);
        for id in menu_ids() {
            if matches!(id.as_str(), "quit" | "ics-refresh" | "record-start") {
                continue; // quit stops the engine; ics-refresh needs network.allow; record-start opens audio
            }
            // Domain errors (not_recording, not_implemented) are fine; a menu id must never be
            // rejected as a malformed request.
            if let Err(e) = dispatch(&engine, &id, json!({})) {
                assert!(
                    !matches!(e.code, "bad_request" | "unknown_action"),
                    "{id}: {e}"
                );
            }
        }
    }
}
