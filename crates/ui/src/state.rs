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

#[derive(Debug, Default)]
pub struct State {
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
        self.devices = model::dispatch(engine, "devices-list", json!({}))
            .ok()
            .and_then(|v| v["devices"].as_array().map(Vec::len))
            .unwrap_or(0);
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
    fn segment_events_append_live_lines() {
        let mut st = State::default();
        st.on_event(r#"{"event":"segment","seq":1,"data":{"speaker":"them","text":"hi"}}"#);
        st.on_event(r#"{"event":"other","data":{}}"#);
        st.on_event("garbage");
        assert_eq!(st.live, vec![Line { speaker: "them".into(), text: "hi".into() }]);
    }
}
