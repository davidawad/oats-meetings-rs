//! View state, free of GPUI: what the main window shows, refreshed from the engine through the
//! same actions `ctl` uses. Tested without a window.

use oats_core::control::engine::Engine;
use serde_json::{json, Value};

use crate::model;
use crate::settings::{self, Group};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Live,
    Sessions,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Live, Tab::Sessions, Tab::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Live => "Live",
            Tab::Sessions => "Sessions",
            Tab::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub utterances: u64,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub speaker: String,
    pub text: String,
}

/// A capture device offered by the picker under `audio.*.device` rows.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceOpt {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// An in-progress text edit of one settings row. `error` is the last `config-set` rejection,
/// shown inline beside the input; the edit stays open until it is accepted or cancelled.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub path: &'static str,
    pub buf: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKey {
    Handled,
    Commit,
    Cancel,
    Ignored,
}

#[derive(Debug, Default)]
pub struct State {
    pub edit: Option<Edit>,
    pub device_opts: Vec<DeviceOpt>,
    pub tab: Tab,
    pub sessions: Vec<SessionRow>,
    pub selected: Option<String>,
    pub transcript: Vec<Line>,
    /// Lines from `segment` events of a recording in progress.
    pub live: Vec<Line>,
    pub settings: Vec<Group>,
    /// Number of capture devices the audio backend reports (0 when none or unavailable).
    pub devices: usize,
    pub status: String,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub fn line_of(u: &Value) -> Line {
    Line {
        speaker: ["speaker", "channel"].iter().map(|k| s(u, k)).find(|x| !x.is_empty()).unwrap_or_default(),
        text: ["text", "content"].iter().map(|k| s(u, k)).find(|x| !x.is_empty()).unwrap_or_default(),
    }
}

impl State {
    pub fn refresh(&mut self, engine: &Engine) {
        match model::dispatch(engine, "session-list", json!({})) {
            Ok(v) => {
                let mut rows: Vec<SessionRow> = v["sessions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|x| SessionRow {
                        id: s(x, "id"),
                        title: s(x, "title"),
                        started_at: s(x, "started_at"),
                        utterances: x["utterance_count"].as_u64().unwrap_or(0),
                        tags: x["tags"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
                    })
                    .collect();
                rows.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(a.id.cmp(&b.id)));
                self.sessions = rows;
            }
            Err(e) => self.status = e.to_string(),
        }
        let devs = model::dispatch(engine, "devices-list", json!({})).ok();
        let arr = devs.as_ref().and_then(|v| v["devices"].as_array());
        self.devices = arr.map_or(0, Vec::len);
        self.device_opts = arr
            .into_iter()
            .flatten()
            .map(|d| DeviceOpt { id: s(d, "id"), name: s(d, "name"), kind: s(d, "kind") })
            .collect();
        self.settings = settings::build(&engine.resolved().flat);
        if let Some(id) = self.selected.clone() {
            self.select(engine, &id);
        }
    }

    pub fn select(&mut self, engine: &Engine, id: &str) {
        self.selected = Some(id.to_string());
        match model::dispatch(engine, "transcript-get", json!({"session": id})) {
            Ok(v) => self.transcript = v["utterances"].as_array().into_iter().flatten().map(line_of).collect(),
            Err(e) => {
                self.transcript.clear();
                self.status = e.to_string();
            }
        }
    }

    pub fn on_event(&mut self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        if v["event"] == "segment" {
            self.live.push(line_of(&v["data"]));
        }
    }

    /// Open the text editor on a row, seeded with its current value.
    pub fn begin_edit(&mut self, path: &'static str, initial: &str) {
        self.edit = Some(Edit { path, buf: initial.to_string(), error: None });
    }

    pub fn cancel_edit(&mut self) {
        self.edit = None;
    }

    /// Feed one key press to the open editor. `key` is the gpui key name, `key_char` the text it
    /// would type; `command` is true when ctrl/alt/cmd is held (never typed).
    pub fn edit_key(&mut self, key: &str, key_char: Option<&str>, command: bool) -> EditKey {
        let Some(e) = self.edit.as_mut() else { return EditKey::Ignored };
        match key {
            "enter" => EditKey::Commit,
            "escape" => EditKey::Cancel,
            "backspace" => {
                e.buf.pop();
                e.error = None;
                EditKey::Handled
            }
            _ => match key_char {
                Some(c) if !command && !c.is_empty() && !c.chars().any(char::is_control) => {
                    e.buf.push_str(c);
                    e.error = None;
                    EditKey::Handled
                }
                _ => EditKey::Ignored,
            },
        }
    }

    /// Write the buffer through `config-set` (the path `ctl config-set` uses). On rejection the
    /// editor stays open with the server's message; on success it closes and the rows reload.
    pub fn commit_edit(&mut self, engine: &Engine) {
        let Some(e) = self.edit.clone() else { return };
        match model::dispatch(engine, "config-set", json!({"key": e.path, "value": e.buf.trim()})) {
            Ok(_) => {
                self.edit = None;
                self.status = String::new();
                self.refresh(engine);
            }
            Err(err) => self.edit = Some(Edit { error: Some(err.to_string()), ..e }),
        }
    }

    /// Run one action for a view control and refresh what it may have changed.
    pub fn act(&mut self, engine: &Engine, id: &str, params: Value) {
        self.status = match model::dispatch(engine, id, params) {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        };
        self.refresh(engine);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn seed(dir: &std::path::Path) {
        let d = dir.join("sessions").join("s1");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("session.json"), r#"{"id":"s1","title":"Standup","startedAt":"2026-10-27T10:00:00Z"}"#).unwrap();
        std::fs::write(
            d.join("transcript.final.jsonl"),
            "{\"speaker\":\"you\",\"text\":\"hello\"}\n{\"speaker\":\"them\",\"text\":\"hi\"}\n",
        )
        .unwrap();
    }

    #[test]
    fn refresh_lists_sessions_and_select_loads_the_transcript() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path());
        let mut st = State::default();
        st.refresh(&engine);
        assert_eq!(st.sessions.len(), 1);
        assert_eq!(st.sessions[0].title, "Standup");
        st.select(&engine, "s1");
        assert_eq!(st.transcript.len(), 2);
        assert_eq!(st.transcript[0], Line { speaker: "you".into(), text: "hello".into() });
        assert!(!st.settings.is_empty());
    }

    #[test]
    fn rename_goes_through_the_action_and_shows_up() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path());
        let mut st = State::default();
        st.act(&engine, "session-rename", json!({"session": "s1", "name": "Retro"}));
        assert_eq!(st.sessions[0].title, "Retro");
        assert_eq!(st.status, "");
    }

    #[test]
    fn errors_land_in_the_status_line_not_a_panic() {
        let (engine, _d) = testutil::engine(&[]);
        let mut st = State::default();
        st.act(&engine, "session-delete", json!({"session": "nope"}));
        assert!(!st.status.is_empty());
    }

    #[test]
    fn text_edit_types_validates_inline_and_persists_through_config_set() {
        let (engine, _d) = testutil::engine(&[]);
        let mut st = State::default();
        st.refresh(&engine);
        st.begin_edit("stt.local.model", "");
        for c in ["b", "o", "g", "u", "s"] {
            assert_eq!(st.edit_key(c, Some(c), false), EditKey::Handled);
        }
        assert_eq!(st.edit_key("a", Some("a"), true), EditKey::Ignored);
        assert_eq!(st.edit_key("enter", None, false), EditKey::Commit);
        st.commit_edit(&engine);
        let e = st.edit.clone().expect("rejected edit stays open");
        assert!(e.error.is_some(), "bogus model is rejected inline");
        // Fix it and commit again.
        for _ in 0..5 {
            st.edit_key("backspace", None, false);
        }
        assert!(st.edit.as_ref().unwrap().error.is_none());
        st.edit.as_mut().unwrap().buf = "base.en".into();
        st.commit_edit(&engine);
        assert!(st.edit.is_none(), "{:?}", st.edit);
        let v = model::dispatch(&engine, "config-get", json!({"key": "stt.local.model"})).unwrap();
        assert_eq!(v.to_string().contains("base.en"), true, "{v}");
    }

    #[test]
    fn int_and_duration_edit_validate_and_persist() {
        use oats_core::config::keys::{Kind, KEYS};
        let (engine, _d) = testutil::engine(&[]);
        let mut st = State::default();
        let int = KEYS.iter().find(|k| k.ui && matches!(k.kind, Kind::Int { .. })).unwrap();
        let Kind::Int { min, max } = int.kind else { unreachable!() };
        st.begin_edit(int.path, "");
        st.edit.as_mut().unwrap().buf = "not a number".into();
        st.commit_edit(&engine);
        assert!(st.edit.as_ref().unwrap().error.is_some());
        st.edit.as_mut().unwrap().buf = (max + 1).to_string();
        st.commit_edit(&engine);
        assert!(st.edit.as_ref().unwrap().error.is_some(), "out of range rejected");
        st.edit.as_mut().unwrap().buf = min.to_string();
        st.commit_edit(&engine);
        assert!(st.edit.is_none(), "{:?}", st.edit);
        let dur = KEYS.iter().find(|k| k.ui && matches!(k.kind, Kind::Duration { .. })).unwrap();
        st.begin_edit(dur.path, "");
        st.edit.as_mut().unwrap().buf = "soon".into();
        st.commit_edit(&engine);
        assert!(st.edit.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn segment_events_append_live_lines() {
        let mut st = State::default();
        st.on_event(r#"{"event":"segment","seq":1,"data":{"speaker":"them","text":"hi"}}"#);
        st.on_event(r#"{"event":"other","data":{}}"#);
        st.on_event("garbage");
        assert_eq!(st.live, vec![Line { speaker: "them".into(), text: "hi".into() }]);
    }
}
