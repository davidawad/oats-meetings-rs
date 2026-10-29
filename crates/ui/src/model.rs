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

/// Key suffixes; the host prefixes `cmd-` on macOS and `ctrl-` elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub keys: &'static str,
    pub action: &'static str,
}

pub const BINDINGS: &[Binding] = &[
    Binding {
        keys: "shift-r",
        action: "record-start",
    },
    Binding {
        keys: "shift-s",
        action: "record-stop",
    },
    Binding {
        keys: "shift-p",
        action: "record-pause",
    },
    Binding {
        keys: "shift-h",
        action: "privacy-toggle",
    },
    Binding {
        keys: "w",
        action: "window-hide",
    },
    Binding {
        keys: "shift-w",
        action: "window-toggle",
    },
    Binding {
        keys: "shift-o",
        action: "window-show",
    },
    Binding {
        keys: "r",
        action: "session-list",
    },
    Binding {
        keys: "q",
        action: "quit",
    },
];

pub const TRAY: &[Entry] = &[
    item("Show / Hide Window", "window-toggle"),
    SEP,
    item("Start Recording", "record-start"),
    item("Stop Recording", "record-stop"),
    item("Hide from Screen Capture", "privacy-toggle"),
    SEP,
    item("Quit", "quit"),
];

/// Actions the views invoke with arguments (buttons and edits in the main window).
pub const VIEW_ACTIONS: &[&str] = &[
    "session-list",
    "session-get",
    "transcript-get",
    "session-rename",
    "session-tag",
    "session-delete",
    "config-set",
    "config-resolved",
    "stt-status",
    "stt-model-fetch",
    "stt-model-list",
    "stt-model-delete",
    "ics-list",
    "devices-list",
    "notes-get",
    "notes-set",
    "notes-generate",
    "template-get",
    "template-save",
    "template-delete",
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
    ("ics-add", "feed management is ctl / config"),
    ("ics-remove", "feed management is ctl / config"),
    ("ics-events", "agent query (ctl)"),
    (
        "secret-set",
        "secrets never pass through the window (ctl, stdin)",
    ),
    ("secret-delete", "secrets never pass through the window"),
    ("secret-has", "secrets never pass through the window"),
    ("stt-transcribe", "batch file transcription (ctl)"),
    (
        "transcript-search",
        "planned in the engine; search box lands with it",
    ),
    ("template-import", "one-shot migration verb (ctl)"),
    ("kb-search", "agent query (ctl)"),
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

pub fn binding_ids() -> BTreeSet<String> {
    ids(BINDINGS.iter().map(|b| b.action))
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

pub fn manifest_ids() -> BTreeSet<String> {
    ids(action::SPECS.iter().map(|s| s.0))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn bindings_do_not_collide() {
        let mut seen = BTreeSet::new();
        for b in BINDINGS {
            assert!(seen.insert(b.keys), "duplicate key {}", b.keys);
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
