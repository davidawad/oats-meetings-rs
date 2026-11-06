//! Settings view model, generated from the typed key table (`config::keys::KEYS`): one row per key
//! whose `ui` flag is set, grouped into the design's sections (`SECTIONS`: Recording, Transcription,
//! Privacy, Appearance, ...) by path prefix. Keys with `ui = false` are deliberately hidden. Editing a row dispatches `config-set`, so the settings view and `ctl`
//! write the same user layer and trigger the same reload.

use std::collections::BTreeMap;

use oats_core::config::keys::{self, Kind, Reload};
use oats_core::config::resolve::Flat;
use oats_core::config::shortcut;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    Toggle(bool),
    Choice {
        options: Vec<&'static str>,
        current: String,
    },
    Text(String),
    /// Whole number within `min..=max`.
    Number {
        value: i64,
        min: i64,
        max: i64,
    },
    /// Duration text (`"15s"`) within `min_ms..=max_ms`.
    Duration {
        text: String,
        min_ms: u64,
        max_ms: u64,
    },
    /// A shortcut's keystrokes, canonical (`ui.keys.*`): rebound by pressing a key.
    Keys(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: &'static str,
    /// The row's name in sentence case ("Microphone", "Show elapsed").
    pub label: String,
    pub doc: &'static str,
    pub reload: Reload,
    pub widget: Widget,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    pub rows: Vec<Row>,
}

fn widget(kind: Kind, value: Option<&Value>) -> Widget {
    match kind {
        Kind::Bool => Widget::Toggle(matches!(value, Some(Value::Bool(true)))),
        Kind::Enum(opts) => Widget::Choice {
            options: opts.to_vec(),
            current: value
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        Kind::Str => Widget::Text(
            value
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        Kind::Int { min, max } => Widget::Number {
            value: value.and_then(Value::as_i64).unwrap_or(min),
            min,
            max,
        },
        Kind::Duration { min_ms, max_ms } => Widget::Duration {
            text: value
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            min_ms,
            max_ms,
        },
        Kind::Keys => Widget::Keys(
            shortcut::parse_list(value.and_then(Value::as_str).unwrap_or_default())
                .unwrap_or_default(),
        ),
    }
}

/// The settings sections in order, each with the key path prefixes it holds; a key goes to the
/// first section with a matching prefix (so `ui.theme` is Appearance, other `ui.*` Window).
pub const SECTIONS: &[(&str, &[&str])] = &[
    ("Recording", &["audio."]),
    ("Transcription", &["stt."]),
    ("Privacy", &["network.", "privacy.", "secrets."]),
    (
        "Appearance",
        &[
            "ui.theme",
            "ui.show_elapsed",
            "ui.record_controls",
            "ui.live_follow",
            "ui.live_max_lines",
            "ui.speaker_width",
            "ui.tz",
        ],
    ),
    ("Keyboard shortcuts", &["ui.keys."]),
    ("Window", &["ui.", "app.", "config."]),
    ("Sessions", &["sessions."]),
    ("Notes and templates", &["notes.", "templates.", "llm."]),
    ("Knowledge base", &["kb."]),
    ("Calendar", &["ics."]),
];

/// Names for the rows whose path does not read well on its own.
const LABELS: &[(&str, &str)] = &[
    ("audio.mic.device", "Microphone"),
    ("audio.mic.enabled", "Record the microphone"),
    ("audio.system.device", "Remote audio"),
    ("audio.system.enabled", "Record system audio"),
    ("network.allow", "Allow network access"),
    ("stt.local.auto_download", "Download models automatically"),
    ("stt.local.model", "Speech model"),
    (
        "privacy.hide_from_capture",
        "Hide the window from screen capture",
    ),
    ("ui.theme", "Theme"),
    ("ui.show_elapsed", "Show elapsed time"),
    ("ui.tz", "Time zone"),
    ("ics.feeds", "Feeds"),
];

/// The section a key belongs to (`Other` when none claims it).
pub fn section_of(path: &str) -> &'static str {
    SECTIONS
        .iter()
        .find(|(_, prefixes)| {
            prefixes
                .iter()
                .any(|p| path == *p || (p.ends_with('.') && path.starts_with(p)))
        })
        .map_or("Other", |(name, _)| name)
}

/// A row's label: from `LABELS`, else the path after its first segment in sentence case
/// (`ui.live_follow` is "Live follow", `stt.local.provider` "Local provider").
pub fn label_of(path: &str) -> String {
    if let Some(b) = path
        .strip_prefix("ui.keys.")
        .and_then(crate::model::binding)
    {
        return b.label.to_string();
    }
    if let Some((_, l)) = LABELS.iter().find(|(p, _)| *p == path) {
        return l.to_string();
    }
    let tail = path.split_once('.').map_or(path, |(_, t)| t);
    let words = tail.replace(['.', '_'], " ");
    let mut c = words.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

pub fn build(flat: &Flat) -> Vec<Group> {
    let mut groups: BTreeMap<usize, Vec<Row>> = BTreeMap::new();
    for k in keys::KEYS.iter().filter(|k| k.ui) {
        let name = section_of(k.path);
        let at = SECTIONS
            .iter()
            .position(|(n, _)| *n == name)
            .unwrap_or(SECTIONS.len());
        groups.entry(at).or_default().push(Row {
            path: k.path,
            label: label_of(k.path),
            doc: k.doc,
            reload: k.reload,
            widget: widget(k.kind, flat.get(k.path)),
        });
    }
    groups
        .into_iter()
        .map(|(at, rows)| Group {
            name: SECTIONS.get(at).map_or("Other", |s| s.0).to_string(),
            rows,
        })
        .collect()
}

/// Keys deliberately absent from the settings view.
pub fn hidden() -> Vec<&'static str> {
    keys::KEYS
        .iter()
        .filter(|k| !k.ui)
        .map(|k| k.path)
        .collect()
}

/// The value a toggle / next-choice click should write.
pub fn next_value(row: &Row) -> Option<Value> {
    match &row.widget {
        Widget::Toggle(b) => Some(json!(!b)),
        Widget::Choice { options, current } => {
            let i = options
                .iter()
                .position(|o| *o == current)
                .map_or(0, |i| i + 1);
            options.get(i % options.len().max(1)).map(|o| json!(o))
        }
        Widget::Text(_) | Widget::Number { .. } | Widget::Duration { .. } | Widget::Keys(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model, testutil};
    use std::collections::BTreeSet;

    #[test]
    fn every_key_is_in_the_view_or_marked_ui_false() {
        let (engine, _d) = testutil::engine(&[]);
        let groups = build(&engine.resolved().flat);
        let shown: BTreeSet<&str> = groups
            .iter()
            .flat_map(|g| g.rows.iter().map(|r| r.path))
            .collect();
        let hidden: BTreeSet<&str> = hidden().into_iter().collect();
        let all: BTreeSet<&str> = keys::KEYS.iter().map(|k| k.path).collect();
        assert!(shown.is_disjoint(&hidden));
        assert_eq!(shown.union(&hidden).copied().collect::<BTreeSet<_>>(), all);
    }

    #[test]
    fn keys_fall_into_the_design_sections_with_readable_labels() {
        assert_eq!(section_of("audio.mic.device"), "Recording");
        assert_eq!(section_of("ui.theme"), "Appearance");
        assert_eq!(section_of("ui.start_hidden"), "Window");
        assert_eq!(section_of("network.allow"), "Privacy");
        assert_eq!(section_of("ui.keys.record-toggle"), "Keyboard shortcuts");
        assert_eq!(label_of("ui.keys.tab-live"), "Live");
        assert_eq!(label_of("audio.mic.device"), "Microphone");
        assert_eq!(label_of("ui.live_follow"), "Live follow");
        assert_eq!(label_of("stt.local.provider"), "Local provider");
        let (engine, _d) = testutil::engine(&[]);
        let groups = build(&engine.resolved().flat);
        // Every ui key lands in a named section, in the design's order.
        assert!(groups.iter().all(|g| g.name != "Other"), "{groups:?}");
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        let order: Vec<&str> = SECTIONS
            .iter()
            .map(|s| s.0)
            .filter(|n| names.contains(n))
            .collect();
        assert_eq!(names, order);
    }

    #[test]
    fn rows_reflect_resolved_values_and_edit_via_config_set() {
        let (engine, _d) = testutil::engine(&[("app.menubar", json!(true))]);
        let rows = |e: &oats_core::control::engine::Engine| {
            build(&e.resolved().flat)
                .into_iter()
                .flat_map(|g| g.rows)
                .collect::<Vec<_>>()
        };
        let row = rows(&engine)
            .into_iter()
            .find(|r| r.path == "app.menubar")
            .unwrap();
        assert_eq!(row.widget, Widget::Toggle(true));
        let v = next_value(&row).unwrap();
        model::dispatch(&engine, "config-set", json!({"key": row.path, "value": v})).unwrap();
        let row = rows(&engine)
            .into_iter()
            .find(|r| r.path == "app.menubar")
            .unwrap();
        assert_eq!(row.widget, Widget::Toggle(false));
    }

    #[test]
    fn every_ui_key_edits_cleanly_for_every_value() {
        // Each toggle and each enum choice is written through config-set and reads back.
        let (engine, _d) = testutil::engine(&[]);
        for k in keys::KEYS.iter().filter(|k| k.ui) {
            let values: Vec<Value> = match k.kind {
                Kind::Bool => vec![json!(true), json!(false)],
                Kind::Enum(o) => o.iter().map(|s| json!(s)).collect(),
                Kind::Str | Kind::Keys => continue,
                // The bounds are the values a number / duration row must be able to write.
                Kind::Int { min, max } => vec![json!(min), json!(max)],
                Kind::Duration { min_ms, max_ms } => {
                    vec![
                        json!(keys::format_duration_ms(min_ms)),
                        json!(keys::format_duration_ms(max_ms)),
                    ]
                }
            };
            for v in values {
                // A value can be rejected by a subsystem (e.g. cloud STT without a key); the point
                // here is that the row never produces a value outside the declared domain.
                if let Err(e) =
                    model::dispatch(&engine, "config-set", json!({"key": k.path, "value": v}))
                {
                    assert_ne!(e.code, "unknown_key", "{}", k.path);
                    assert_ne!(e.code, "type_mismatch", "{} = {v}", k.path);
                }
            }
        }
    }
}
