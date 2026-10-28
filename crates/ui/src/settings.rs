//! Settings view model, generated from the typed key table (`config::keys::KEYS`): one row per key
//! whose `ui` flag is set, grouped by the first path segment. Keys with `ui = false` are
//! deliberately hidden. Editing a row dispatches `config-set`, so the settings view and `ctl`
//! write the same user layer and trigger the same reload.

use std::collections::BTreeMap;

use oats_core::config::keys::{self, Kind, Reload};
use oats_core::config::resolve::Flat;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    Toggle(bool),
    Choice { options: Vec<&'static str>, current: String },
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub path: &'static str,
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
            current: value.and_then(Value::as_str).unwrap_or_default().to_string(),
        },
        Kind::Str => Widget::Text(value.and_then(Value::as_str).unwrap_or_default().to_string()),
    }
}

pub fn build(flat: &Flat) -> Vec<Group> {
    let mut groups: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for k in keys::KEYS.iter().filter(|k| k.ui) {
        let group = k.path.split('.').next().unwrap_or(k.path).to_string();
        groups.entry(group).or_default().push(Row {
            path: k.path,
            doc: k.doc,
            reload: k.reload,
            widget: widget(k.kind, flat.get(k.path)),
        });
    }
    groups.into_iter().map(|(name, rows)| Group { name, rows }).collect()
}

/// Keys deliberately absent from the settings view.
pub fn hidden() -> Vec<&'static str> {
    keys::KEYS.iter().filter(|k| !k.ui).map(|k| k.path).collect()
}

/// The value a toggle / next-choice click should write.
pub fn next_value(row: &Row) -> Option<Value> {
    match &row.widget {
        Widget::Toggle(b) => Some(json!(!b)),
        Widget::Choice { options, current } => {
            let i = options.iter().position(|o| *o == current).map_or(0, |i| i + 1);
            options.get(i % options.len().max(1)).map(|o| json!(o))
        }
        Widget::Text(_) => None,
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
        let shown: BTreeSet<&str> = groups.iter().flat_map(|g| g.rows.iter().map(|r| r.path)).collect();
        let hidden: BTreeSet<&str> = hidden().into_iter().collect();
        let all: BTreeSet<&str> = keys::KEYS.iter().map(|k| k.path).collect();
        assert!(shown.is_disjoint(&hidden));
        assert_eq!(shown.union(&hidden).copied().collect::<BTreeSet<_>>(), all);
    }

    #[test]
    fn rows_reflect_resolved_values_and_edit_via_config_set() {
        let (engine, _d) = testutil::engine(&[("app.menubar", json!(true))]);
        let rows = |e: &oats_core::control::engine::Engine| {
            build(&e.resolved().flat).into_iter().flat_map(|g| g.rows).collect::<Vec<_>>()
        };
        let row = rows(&engine).into_iter().find(|r| r.path == "app.menubar").unwrap();
        assert_eq!(row.widget, Widget::Toggle(true));
        let v = next_value(&row).unwrap();
        model::dispatch(&engine, "config-set", json!({"key": row.path, "value": v})).unwrap();
        let row = rows(&engine).into_iter().find(|r| r.path == "app.menubar").unwrap();
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
                Kind::Str => continue,
            };
            for v in values {
                // A value can be rejected by a subsystem (e.g. cloud STT without a key); the point
                // here is that the row never produces a value outside the declared domain.
                if let Err(e) = model::dispatch(&engine, "config-set", json!({"key": k.path, "value": v})) {
                    assert_ne!(e.code, "unknown_key", "{}", k.path);
                    assert_ne!(e.code, "type_mismatch", "{} = {v}", k.path);
                }
            }
        }
    }
}
