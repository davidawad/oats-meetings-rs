//! View state, free of GPUI: what the main window shows, refreshed from the engine through the
//! same actions `ctl` uses. Tested without a window.

use std::time::Instant;

use oats_core::control::engine::Engine;
use serde_json::{json, Value};

use crate::model;
use crate::settings::{self, Group};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Live,
    Sessions,
    Notes,
    Templates,
    Calendar,
    Trash,
    Models,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 8] = [
        Tab::Live,
        Tab::Sessions,
        Tab::Notes,
        Tab::Templates,
        Tab::Calendar,
        Tab::Trash,
        Tab::Models,
        Tab::Settings,
    ];

    /// The tabs in the nav on this OS: without a trash listing API (macOS) there is no Trash
    /// tab, since sessions moved to the trash can only be put back from the Finder.
    pub fn shown() -> Vec<Tab> {
        Tab::ALL
            .into_iter()
            .filter(|t| *t != Tab::Trash || oats_core::sessions::TRASH_LISTING)
            .collect()
    }

    /// The left rail: the daily tabs, then (below a divider) the setup tabs. Trash is not in the
    /// rail; it is the "Recently deleted" view under Sessions (still `ui-select`able as a tab).
    pub const RAIL: [&'static [Tab]; 2] = [
        &[Tab::Live, Tab::Sessions, Tab::Notes],
        &[Tab::Templates, Tab::Calendar, Tab::Models, Tab::Settings],
    ];

    /// The rail entry lit for this tab: Trash lives under Sessions.
    pub fn rail(self) -> Tab {
        match self {
            Tab::Trash => Tab::Sessions,
            t => t,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Live => "Live",
            Tab::Sessions => "Sessions",
            Tab::Notes => "Notes",
            Tab::Templates => "Templates",
            Tab::Calendar => "Calendar",
            Tab::Trash => "Trash",
            Tab::Models => "Models",
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
    /// Start in recording seconds when the engine reported one (live segments).
    pub start: Option<f64>,
}

/// The recording buttons; each maps to the engine action `ctl` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Start,
    Pause,
    Stop,
}

impl Control {
    pub fn action(self) -> &'static str {
        match self {
            Control::Start => "record-start",
            Control::Pause => "record-pause",
            Control::Stop => "record-stop",
        }
    }

    /// Badge text while the action runs.
    pub fn busy_label(self) -> &'static str {
        match self {
            Control::Start => "Starting...",
            Control::Pause => "Pausing...",
            Control::Stop => "Stopping and transcribing...",
        }
    }
}

/// What the recorder is doing, as the window knows it. `base_ms` is the audio duration the engine
/// last reported; `resumed` is when the clock last started running, so elapsed time advances
/// between engine events without a poll.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Rec {
    #[default]
    Idle,
    Recording {
        session: String,
        base_ms: u64,
        resumed: Instant,
    },
    Paused {
        session: String,
        base_ms: u64,
    },
}

impl Rec {
    pub fn session(&self) -> Option<&str> {
        match self {
            Rec::Idle => None,
            Rec::Recording { session, .. } | Rec::Paused { session, .. } => Some(session),
        }
    }

    pub fn elapsed_ms(&self, now: Instant) -> u64 {
        match self {
            Rec::Idle => 0,
            Rec::Paused { base_ms, .. } => *base_ms,
            Rec::Recording {
                base_ms, resumed, ..
            } => base_ms + now.saturating_duration_since(*resumed).as_millis() as u64,
        }
    }
}

/// `MM:SS`, or `H:MM:SS` from one hour.
pub fn fmt_elapsed(ms: u64) -> String {
    let t = ms / 1000;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// The record strip's clock: always `HH:MM:SS`.
pub fn fmt_clock(ms: u64) -> String {
    let t = ms / 1000;
    format!("{:02}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
}

/// The dot on the Live tab: red while recording, amber while paused, none when idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dot {
    Rec,
    Paused,
}

impl Rec {
    pub fn dot(&self) -> Option<Dot> {
        match self {
            Rec::Idle => None,
            Rec::Recording { .. } => Some(Dot::Rec),
            Rec::Paused { .. } => Some(Dot::Paused),
        }
    }
}

/// Which recording controls are usable in a state, and the pause button's label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    pub start: bool,
    pub pause: bool,
    pub stop: bool,
    pub pause_label: &'static str,
}

impl Controls {
    /// The buttons while an action is pending: none usable.
    pub const BUSY: Controls = Controls {
        start: false,
        pause: false,
        stop: false,
        pause_label: "Pause",
    };

    pub fn of(rec: &Rec) -> Self {
        Self {
            start: matches!(rec, Rec::Idle),
            pause: !matches!(rec, Rec::Idle),
            stop: !matches!(rec, Rec::Idle),
            pause_label: if matches!(rec, Rec::Paused { .. }) {
                "Resume"
            } else {
                "Pause"
            },
        }
    }
}

/// The recording badge text: `Not recording`, `Recording 00:12` or `Paused 00:12`; the time part is
/// dropped when `show_elapsed` is off.
pub fn badge_text(rec: &Rec, now: Instant, show_elapsed: bool) -> String {
    let (word, timed) = match rec {
        Rec::Idle => return "Not recording".into(),
        Rec::Recording { .. } => ("Recording", true),
        Rec::Paused { .. } => ("Paused", true),
    };
    if timed && show_elapsed {
        format!("{word} {}", fmt_elapsed(rec.elapsed_ms(now)))
    } else {
        word.into()
    }
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
    /// ctrl/cmd+a selected the whole buffer (see `type_key`).
    pub all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKey {
    Handled,
    Commit,
    Cancel,
    Ignored,
}

/// One key on a plain text buffer: the typing every text input shares. ctrl/cmd+a selects the
/// whole buffer (`all`); the next typed text then replaces it and backspace clears it. False (and
/// the selection untouched) for keys it does not handle. `command` is true when ctrl/alt/cmd is
/// held (never typed).
pub fn type_key(
    buf: &mut String,
    all: &mut bool,
    key: &str,
    key_char: Option<&str>,
    command: bool,
) -> bool {
    if key == "a" && command {
        *all = !buf.is_empty();
        return true;
    }
    let typed = key_char.filter(|c| !command && !c.is_empty() && !c.chars().any(char::is_control));
    if key != "backspace" && typed.is_none() {
        return false;
    }
    if std::mem::take(all) {
        buf.clear();
    } else if key == "backspace" {
        buf.pop();
    }
    if key != "backspace" {
        buf.push_str(typed.unwrap_or_default());
    }
    true
}

/// Why transcription cannot run right now (from `stt-status`), shown as a banner.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub message: String,
    /// Action id that repairs it (a banner button), if any.
    pub fix: Option<String>,
}

/// `devices_note` while the backend is still listing its devices.
pub const DEVICES_PENDING: &str = "listing audio devices...";
/// `devices_note` when the backend did not list them within `audio.devices_timeout`, or failed.
pub const DEVICES_UNAVAILABLE: &str = "devices unavailable";

#[derive(Debug, Default)]
pub struct State {
    pub edit: Option<Edit>,
    pub device_opts: Vec<DeviceOpt>,
    pub stt_problem: Option<Problem>,
    pub tab: Tab,
    pub sessions: Vec<SessionRow>,
    pub selected: Option<String>,
    pub transcript: Vec<Line>,
    /// The Live view: live segments of the current recording, replaced by the saved transcript at
    /// stop, cleared at the next start.
    pub live: Vec<Line>,
    /// The session the Live view shows (the active or the last stopped recording).
    pub live_session: Option<String>,
    pub rec: Rec,
    /// A recording button's action is running off the UI thread (a start loads the ASR model).
    pub pending: Option<Control>,
    pub settings: Vec<Group>,
    /// Number of capture devices the audio backend reports (0 when none or unavailable).
    pub devices: usize,
    /// Why there is no device list: still listing, or `devices unavailable` (timed out / failed).
    pub devices_note: Option<String>,
    pub status: String,
    pub sessions_ui: crate::views::sessions::Ui,
    pub settings_ui: crate::views::settings::Ui,
    /// The default speech model's name and runtime (`stt-model-list`), for the Live header and
    /// status line.
    pub model: Option<(String, String)>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn line_of(u: &Value) -> Line {
    Line {
        speaker: ["speaker", "channel"]
            .iter()
            .map(|k| s(u, k))
            .find(|x| !x.is_empty())
            .unwrap_or_default(),
        text: ["text", "content"]
            .iter()
            .map(|k| s(u, k))
            .find(|x| !x.is_empty())
            .unwrap_or_default(),
        start: u.get("start").and_then(Value::as_f64),
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
                        tags: x["tags"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect(),
                    })
                    .collect();
                rows.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(a.id.cmp(&b.id)));
                self.sessions = rows;
            }
            Err(e) => self.status = e.to_string(),
        }
        self.sync_devices(engine);
        self.stt_problem = model::dispatch(engine, "stt-status", json!({}))
            .ok()
            .and_then(|v| {
                let p = &v["problem"];
                Some(Problem {
                    message: p["message"].as_str()?.to_string(),
                    fix: p["fix"].as_str().map(str::to_string),
                })
            });
        self.settings = settings::build(&engine.resolved().flat);
        self.model = model::dispatch(engine, "stt-model-list", json!({}))
            .ok()
            .and_then(|v| {
                let m = v["models"]
                    .as_array()?
                    .iter()
                    .find(|m| m["selected"].as_bool() == Some(true))?
                    .clone();
                let name = m["name"].as_str().or(m["id"].as_str())?.to_string();
                Some((name, m["runtime"].as_str().unwrap_or_default().to_string()))
            });
        if let Some(id) = self.selected.clone() {
            self.select(engine, &id);
        }
    }

    /// The device list from the engine's cache: never the backend, which can block (dot-4uzgol.57).
    /// While the first listing runs the list is empty; past `audio.devices_timeout`, or when the
    /// backend fails, it is empty and `devices_note` says devices are unavailable. Cheap, so the
    /// view calls it every tick until a listing arrives.
    pub fn sync_devices(&mut self, engine: &Engine) {
        let (devs, note) = match engine.devices_cached() {
            Ok(Some(v)) => (Some(v), None),
            Ok(None) => (None, Some(DEVICES_PENDING)),
            Err(_) => (None, Some(DEVICES_UNAVAILABLE)),
        };
        self.devices_note = note.map(str::to_string);
        let arr = devs.as_ref().and_then(|v| v["devices"].as_array());
        self.devices = arr.map_or(0, Vec::len);
        self.device_opts = arr
            .into_iter()
            .flatten()
            .map(|d| DeviceOpt {
                id: s(d, "id"),
                name: s(d, "name"),
                kind: s(d, "kind"),
            })
            .collect();
    }

    pub fn select(&mut self, engine: &Engine, id: &str) {
        self.selected = Some(id.to_string());
        match model::dispatch(engine, "transcript-get", json!({"session": id})) {
            Ok(v) => {
                self.transcript = v["utterances"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(line_of)
                    .collect()
            }
            Err(e) => {
                self.transcript.clear();
                self.status = e.to_string();
            }
        }
    }

    /// Follow the engine's recorder: adopt a recording already in progress (window opened or reopened
    /// mid-recording, or started by a shortcut or `ctl`), or fall back to idle. Lines are loaded
    /// only when the session changes, so calling this every tick is cheap.
    pub fn sync_rec(&mut self, engine: &Engine) {
        let Some(now) = engine.recording() else {
            return;
        };
        match now {
            None => {
                if !matches!(self.rec, Rec::Idle) {
                    self.rec = Rec::Idle;
                }
            }
            Some((session, paused, dur)) => {
                if self.rec.session() != Some(session.as_str()) {
                    self.live_session = Some(session.clone());
                    self.live = self.saved_lines(engine, &session).unwrap_or_default();
                    self.trim_live(engine);
                    self.rec = if paused {
                        Rec::Paused {
                            session,
                            base_ms: dur,
                        }
                    } else {
                        Rec::Recording {
                            session,
                            base_ms: dur,
                            resumed: Instant::now(),
                        }
                    };
                } else if paused != matches!(self.rec, Rec::Paused { .. }) {
                    self.rec = if paused {
                        Rec::Paused {
                            session,
                            base_ms: dur,
                        }
                    } else {
                        Rec::Recording {
                            session,
                            base_ms: dur,
                            resumed: Instant::now(),
                        }
                    };
                }
            }
        }
    }

    /// Whether New session applies: idle, nothing pending, and Live still shows a finished session.
    pub fn can_new_session(&self) -> bool {
        self.pending.is_none() && matches!(self.rec, Rec::Idle) && self.live_session.is_some()
    }

    /// New session (dot-4uzgol.91): Live drops the finished session it shows (it stays in
    /// Sessions) and is empty again; the next Start makes the session and names it. False, and
    /// nothing changes, when it does not apply.
    pub fn new_session(&mut self) -> bool {
        if !self.can_new_session() {
            return false;
        }
        self.live.clear();
        self.live_session = None;
        self.status.clear();
        true
    }

    fn saved_lines(&self, engine: &Engine, session: &str) -> Option<Vec<Line>> {
        let v = model::dispatch(engine, "transcript-get", json!({"session": session})).ok()?;
        Some(
            v["utterances"]
                .as_array()
                .into_iter()
                .flatten()
                .map(line_of)
                .collect(),
        )
    }

    fn trim_live(&mut self, engine: &Engine) {
        let max = engine.snapshot().config.ui.live_max_lines as usize;
        if self.live.len() > max {
            self.live.drain(..self.live.len() - max);
        }
    }

    /// Apply one bus event line. Recording events drive the badge whoever started the recording
    /// (button, shortcut, tray or `ctl`); segments feed the Live view of the current recording only.
    pub fn on_event(&mut self, engine: &Engine, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let d = &v["data"];
        let session = d["session"].as_str().unwrap_or_default().to_string();
        let dur = d["duration_ms"].as_u64().unwrap_or(0);
        match v["event"].as_str() {
            Some("recording-started") => {
                self.live.clear();
                self.live_session = Some(session.clone());
                self.status.clear();
                self.rec = Rec::Recording {
                    session,
                    base_ms: 0,
                    resumed: Instant::now(),
                };
            }
            Some("recording-paused") => {
                self.rec = if d["paused"].as_bool().unwrap_or(true) {
                    Rec::Paused {
                        session,
                        base_ms: dur,
                    }
                } else {
                    Rec::Recording {
                        session,
                        base_ms: dur,
                        resumed: Instant::now(),
                    }
                };
            }
            Some("recording-stopped") => {
                self.rec = Rec::Idle;
                // The saved transcript (final, else live) replaces the live fragments in one step.
                if let Some(lines) = self.saved_lines(engine, &session) {
                    self.live = lines;
                }
                self.live_session = Some(session.clone());
                self.status = format!("Saved session {session}");
            }
            Some("segment") if d["kind"].as_str().unwrap_or("live") == "live" => {
                if self
                    .live_session
                    .as_deref()
                    .is_some_and(|c| d["session"].as_str().is_none_or(|s| s == c))
                {
                    let l = line_of(d);
                    // A live segment re-reporting the same speaker and start refines its line.
                    match self.live.iter().rposition(|x| {
                        x.speaker == l.speaker && x.start.is_some() && x.start == l.start
                    }) {
                        Some(i) => self.live[i] = l,
                        None => self.live.push(l),
                    }
                    self.trim_live(engine);
                }
            }
            _ => {}
        }
    }

    /// Open the text editor on a row, seeded with its current value.
    pub fn begin_edit(&mut self, path: &'static str, initial: &str) {
        self.cancel_edit();
        self.edit = Some(Edit {
            path,
            buf: initial.to_string(),
            error: None,
            all: false,
        });
    }

    /// Close the editor without saving. The Sessions and Settings searches are live filters, so
    /// closing one keeps what was typed (its clear chip, or Esc, empties it).
    pub fn cancel_edit(&mut self) {
        if let Some(e) = self.edit.take() {
            if e.path == crate::views::sessions::SEARCH {
                self.sessions_ui.query = e.buf.trim().to_string();
            } else if e.path == crate::views::settings::SEARCH {
                self.settings_ui.query = e.buf.trim().to_string();
            }
        }
    }

    /// Feed one key press to the open editor. `key` is the gpui key name, `key_char` the text it
    /// would type; `command` is true when ctrl/alt/cmd is held (never typed).
    pub fn edit_key(&mut self, key: &str, key_char: Option<&str>, command: bool) -> EditKey {
        let Some(e) = self.edit.as_mut() else {
            return EditKey::Ignored;
        };
        match key {
            "enter" => EditKey::Commit,
            "escape" => EditKey::Cancel,
            _ if type_key(&mut e.buf, &mut e.all, key, key_char, command) => {
                e.error = None;
                EditKey::Handled
            }
            _ => EditKey::Ignored,
        }
    }

    /// Write the buffer through `config-set` (the path `ctl config-set` uses). On rejection the
    /// editor stays open with the server's message; on success it closes and the rows reload.
    pub fn commit_edit(&mut self, engine: &Engine) {
        let Some(e) = self.edit.clone() else { return };
        if crate::views::sessions::is_ours(e.path) {
            return crate::views::sessions::commit(self, engine);
        }
        if e.path == crate::views::settings::SEARCH {
            return crate::views::settings::commit(self);
        }
        match model::dispatch(
            engine,
            "config-set",
            json!({"key": e.path, "value": e.buf.trim()}),
        ) {
            Ok(_) => {
                self.edit = None;
                self.status = String::new();
                self.refresh(engine);
            }
            Err(err) => {
                self.edit = Some(Edit {
                    error: Some(err.to_string()),
                    ..e
                })
            }
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
        std::fs::write(
            d.join("session.json"),
            r#"{"id":"s1","title":"Standup","startedAt":"2026-10-27T10:00:00Z"}"#,
        )
        .unwrap();
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
        assert_eq!(
            st.transcript[0],
            Line {
                speaker: "you".into(),
                text: "hello".into(),
                start: None
            }
        );
        assert!(!st.settings.is_empty());
    }

    #[test]
    fn a_missing_model_is_a_banner_with_a_fix_and_a_present_one_is_not() {
        let (engine, dir) = testutil::engine(&[]);
        let mut st = State::default();
        st.refresh(&engine);
        let compiled =
            model::dispatch(&engine, "stt-status", json!({})).unwrap()["local"]["compiled"] == true;
        let p = st
            .stt_problem
            .clone()
            .expect("a fresh data dir has no model");
        assert!(p.message.contains("audio only"), "{}", p.message);
        // Local ASR is compiled into default builds; without it the banner has no fix.
        assert_eq!(p.fix.as_deref(), compiled.then_some("stt-model-fetch"));
        // The model this build runs for the configured one (the CoreML default resolves to its
        // sherpa-onnx fallback off macOS).
        let e = oats_core::stt::catalog::get(&engine.snapshot().config.stt.local.model)
            .unwrap()
            .resolved();
        let m = e.location(&dir.path().join("models"));
        for f in &e.files {
            let at = m.join(&f.name);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, b"x").unwrap();
        }
        st.refresh(&engine);
        assert_eq!(st.stt_problem.is_some(), !compiled);
    }

    #[test]
    fn rename_goes_through_the_action_and_shows_up() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path());
        let mut st = State::default();
        st.act(
            &engine,
            "session-rename",
            json!({"session": "s1", "name": "Retro"}),
        );
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

    fn seg(session: &str, kind: &str, speaker: &str, text: &str, start: f64) -> String {
        json!({"event":"segment","data":{"session":session,"kind":kind,"speaker":speaker,"text":text,"start":start,"end":start+1.0}}).to_string()
    }

    fn ev(name: &str, data: Value) -> String {
        json!({"event": name, "data": data}).to_string()
    }

    #[test]
    fn ctrl_a_selects_all_then_typing_replaces_and_backspace_clears() {
        let (mut buf, mut all) = ("old text".to_string(), false);
        let mut k =
            |key: &str, ch: Option<&str>, cmd: bool| type_key(&mut buf, &mut all, key, ch, cmd);
        assert!(k("a", Some("a"), true));
        assert!(k("n", Some("n"), false));
        assert!(k("e", Some("e"), false));
        assert!(k("a", Some("a"), true));
        assert!(k("backspace", None, false));
        assert!(k("z", Some("z"), false));
        assert!(k("a", Some("a"), true));
        assert!(!k("left", None, false), "not ours; the selection stays");
        assert!(k("q", Some("q"), false));
        assert!(k("r", Some("r"), false));
        assert_eq!(buf, "qr");

        // The settings / Sessions editor: same keys through `edit_key`.
        let mut st = State::default();
        st.begin_edit("stt.local.model", "base.en");
        assert_eq!(st.edit_key("a", Some("a"), true), EditKey::Handled);
        assert!(st.edit.as_ref().unwrap().all);
        assert_eq!(st.edit_key("t", Some("t"), false), EditKey::Handled);
        assert_eq!(st.edit.as_ref().unwrap().buf, "t");
        assert!(!st.edit.as_ref().unwrap().all);
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
        assert_eq!(st.edit_key("c", Some("c"), true), EditKey::Ignored);
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
        let int = KEYS
            .iter()
            .find(|k| k.ui && matches!(k.kind, Kind::Int { .. }))
            .unwrap();
        let Kind::Int { min, max } = int.kind else {
            unreachable!()
        };
        st.begin_edit(int.path, "");
        st.edit.as_mut().unwrap().buf = "not a number".into();
        st.commit_edit(&engine);
        assert!(st.edit.as_ref().unwrap().error.is_some());
        st.edit.as_mut().unwrap().buf = (max + 1).to_string();
        st.commit_edit(&engine);
        assert!(
            st.edit.as_ref().unwrap().error.is_some(),
            "out of range rejected"
        );
        st.edit.as_mut().unwrap().buf = min.to_string();
        st.commit_edit(&engine);
        assert!(st.edit.is_none(), "{:?}", st.edit);
        let dur = KEYS
            .iter()
            .find(|k| k.ui && matches!(k.kind, Kind::Duration { .. }))
            .unwrap();
        st.begin_edit(dur.path, "");
        st.edit.as_mut().unwrap().buf = "soon".into();
        st.commit_edit(&engine);
        assert!(st.edit.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn every_tab_is_in_the_nav_once_with_a_distinct_label() {
        let labels: std::collections::BTreeSet<_> = Tab::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(labels.len(), Tab::ALL.len());
        assert_eq!(Tab::ALL[0], Tab::default());
        for t in ["Notes", "Templates", "Calendar", "Trash", "Models"] {
            assert!(labels.contains(t), "{t}");
        }
    }

    #[test]
    fn the_trash_tab_is_shown_only_where_the_trash_can_be_listed() {
        let shown = Tab::shown();
        assert_eq!(
            shown.contains(&Tab::Trash),
            oats_core::sessions::TRASH_LISTING
        );
        assert_eq!(
            shown.len(),
            Tab::ALL.len() - usize::from(!oats_core::sessions::TRASH_LISTING)
        );
        assert_eq!(shown[0], Tab::default());
    }

    #[test]
    fn segment_events_append_live_lines_of_the_current_recording_only() {
        let (engine, _d) = testutil::engine(&[]);
        let mut st = State::default();
        st.on_event(&engine, &ev("recording-started", json!({"session": "a"})));
        st.on_event(&engine, &seg("a", "live", "them", "hi", 0.0));
        st.on_event(&engine, &seg("zzz", "live", "them", "other session", 0.0));
        st.on_event(&engine, r#"{"event":"other","data":{}}"#);
        st.on_event(&engine, "garbage");
        assert_eq!(
            st.live,
            vec![Line {
                speaker: "them".into(),
                text: "hi".into(),
                start: Some(0.0)
            }]
        );
    }

    #[test]
    fn a_refined_live_segment_replaces_its_stale_partial() {
        let (engine, _d) = testutil::engine(&[]);
        let mut st = State::default();
        st.on_event(&engine, &ev("recording-started", json!({"session": "a"})));
        st.on_event(&engine, &seg("a", "live", "you", "Let's review the", 1.0));
        st.on_event(
            &engine,
            &seg("a", "live", "you", "Let's review the roadmap", 1.0),
        );
        st.on_event(&engine, &seg("a", "live", "them", "Sounds good", 4.0));
        assert_eq!(st.live.len(), 2);
        assert_eq!(st.live[0].text, "Let's review the roadmap");
    }

    #[test]
    fn stop_swaps_live_fragments_for_the_saved_transcript_without_duplicates() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path());
        let mut st = State::default();
        st.on_event(&engine, &ev("recording-started", json!({"session": "s1"})));
        for (i, t) in ["hel", "hello", "hello th"].iter().enumerate() {
            st.on_event(&engine, &seg("s1", "live", "you", t, i as f64 * 10.0));
        }
        // final segments arrive before the stop event; they must not stack under the live ones.
        st.on_event(&engine, &seg("s1", "final", "you", "hello", 0.0));
        st.on_event(&engine, &seg("s1", "final", "them", "hi", 1.0));
        assert_eq!(st.live.len(), 3);
        st.on_event(
            &engine,
            &ev(
                "recording-stopped",
                json!({"session": "s1", "duration_ms": 5000}),
            ),
        );
        assert_eq!(st.rec, Rec::Idle);
        assert_eq!(
            st.live,
            vec![
                Line {
                    speaker: "you".into(),
                    text: "hello".into(),
                    start: None
                },
                Line {
                    speaker: "them".into(),
                    text: "hi".into(),
                    start: None
                },
            ]
        );
    }

    #[test]
    fn start_clears_the_previous_recordings_lines() {
        let (engine, _d) = testutil::engine(&[]);
        let mut st = State::default();
        st.on_event(&engine, &ev("recording-started", json!({"session": "a"})));
        st.on_event(&engine, &seg("a", "live", "you", "one", 0.0));
        st.on_event(&engine, &ev("recording-stopped", json!({"session": "a"})));
        st.on_event(&engine, &ev("recording-started", json!({"session": "b"})));
        assert!(st.live.is_empty());
        st.on_event(&engine, &seg("b", "live", "you", "two", 0.0));
        assert_eq!(st.live.len(), 1);
        assert_eq!(st.live_session.as_deref(), Some("b"));
    }

    #[test]
    fn live_lines_are_capped_by_ui_live_max_lines() {
        let (engine, _d) = testutil::engine(&[("ui.live_max_lines", json!(50))]);
        let mut st = State::default();
        st.on_event(&engine, &ev("recording-started", json!({"session": "a"})));
        for i in 0..80 {
            st.on_event(
                &engine,
                &seg("a", "live", "you", &format!("l{i}"), i as f64),
            );
        }
        assert_eq!(st.live.len(), 50);
        assert_eq!(st.live[0].text, "l30");
    }

    #[test]
    fn pause_resume_stop_events_drive_rec_state_and_elapsed() {
        let (engine, _d) = testutil::engine(&[]);
        let mut st = State::default();
        let t0 = Instant::now();
        assert_eq!(
            Controls::of(&st.rec),
            Controls {
                start: true,
                pause: false,
                stop: false,
                pause_label: "Pause"
            }
        );
        st.on_event(&engine, &ev("recording-started", json!({"session": "a"})));
        assert!(matches!(st.rec, Rec::Recording { .. }));
        assert_eq!(
            Controls::of(&st.rec),
            Controls {
                start: false,
                pause: true,
                stop: true,
                pause_label: "Pause"
            }
        );
        st.on_event(
            &engine,
            &ev(
                "recording-paused",
                json!({"session": "a", "paused": true, "duration_ms": 12_000}),
            ),
        );
        assert_eq!(
            st.rec,
            Rec::Paused {
                session: "a".into(),
                base_ms: 12_000
            }
        );
        assert_eq!(Controls::of(&st.rec).pause_label, "Resume");
        // paused time does not advance
        assert_eq!(
            badge_text(&st.rec, t0 + std::time::Duration::from_secs(99), true),
            "Paused 00:12"
        );
        st.on_event(
            &engine,
            &ev(
                "recording-paused",
                json!({"session": "a", "paused": false, "duration_ms": 12_000}),
            ),
        );
        let Rec::Recording { resumed, .. } = st.rec.clone() else {
            panic!("{:?}", st.rec)
        };
        assert_eq!(
            badge_text(&st.rec, resumed + std::time::Duration::from_secs(3), true),
            "Recording 00:15"
        );
        assert_eq!(badge_text(&st.rec, resumed, false), "Recording");
        st.on_event(&engine, &ev("recording-stopped", json!({"session": "a"})));
        assert_eq!(badge_text(&st.rec, t0, true), "Not recording");
    }

    #[test]
    fn the_rail_holds_every_shown_tab_but_trash_which_lights_sessions() {
        let rail: Vec<Tab> = Tab::RAIL.iter().flat_map(|g| g.iter().copied()).collect();
        let mut want = Tab::ALL.to_vec();
        want.retain(|t| *t != Tab::Trash);
        assert_eq!(rail, want);
        assert_eq!(Tab::Trash.rail(), Tab::Sessions);
        assert_eq!(Tab::Models.rail(), Tab::Models);
    }

    #[test]
    fn the_live_dot_follows_the_recorder() {
        let now = Instant::now();
        let s = || "a".to_string();
        assert_eq!(Rec::Idle.dot(), None);
        let rec = Rec::Recording {
            session: s(),
            base_ms: 0,
            resumed: now,
        };
        assert_eq!(rec.dot(), Some(Dot::Rec));
        assert_eq!(
            Rec::Paused {
                session: s(),
                base_ms: 0
            }
            .dot(),
            Some(Dot::Paused)
        );
    }

    #[test]
    fn elapsed_formats() {
        assert_eq!(fmt_clock(0), "00:00:00");
        assert_eq!(fmt_clock(3_723_000), "01:02:03");
        assert_eq!(fmt_elapsed(0), "00:00");
        assert_eq!(fmt_elapsed(61_999), "01:01");
        assert_eq!(fmt_elapsed(3_723_000), "1:02:03");
    }

    /// A backend whose device probe never answers (dot-4uzgol.57): refresh reads only the
    /// engine's cache, so it returns at once; past audio.devices_timeout the view says devices
    /// unavailable, and the listing shows once the backend answers.
    #[test]
    fn refresh_never_waits_on_a_stuck_audio_backend() {
        let (engine, _d) = testutil::engine(&[("audio.devices_timeout", json!("200ms"))]);
        let (stuck, release) = testutil::stuck_audio();
        engine.set_audio_factory(stuck);
        let mut st = State::default();
        let t = std::time::Instant::now();
        st.refresh(&engine);
        assert!(
            t.elapsed() < std::time::Duration::from_millis(150),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(st.devices_note.as_deref(), Some(DEVICES_PENDING));
        assert_eq!(st.devices, 0);
        std::thread::sleep(std::time::Duration::from_millis(250));
        let t = std::time::Instant::now();
        st.refresh(&engine);
        assert!(t.elapsed() < std::time::Duration::from_millis(150));
        assert_eq!(st.devices_note.as_deref(), Some(DEVICES_UNAVAILABLE));
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while st.devices_note.is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
            st.sync_devices(&engine);
        }
        assert_eq!(st.devices_note, None);
        assert_eq!(st.devices, 3);
        assert!(st.device_opts.iter().any(|d| d.id == "fake.mic"));
    }

    #[test]
    fn buttons_drive_a_real_recording_through_the_engine_actions() {
        use oats_audio::fake::FakeBackend;
        let (engine, _d) = testutil::engine(&[]);
        engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
        let rx = engine.bus.subscribe(vec![]);
        let mut st = State::default();
        let drain = |st: &mut State| {
            for l in rx.try_iter().collect::<Vec<_>>() {
                st.on_event(&engine, &l);
            }
        };
        st.act(&engine, "record-start", json!({}));
        assert_eq!(st.status, "");
        drain(&mut st);
        assert!(matches!(st.rec, Rec::Recording { .. }), "{:?}", st.rec);
        st.act(&engine, "record-pause", json!({}));
        drain(&mut st);
        assert!(matches!(st.rec, Rec::Paused { .. }), "{:?}", st.rec);
        st.act(&engine, "record-pause", json!({}));
        drain(&mut st);
        assert!(matches!(st.rec, Rec::Recording { .. }));
        st.act(&engine, "record-stop", json!({}));
        drain(&mut st);
        assert_eq!(st.rec, Rec::Idle);
        // misuse surfaces as a status line, not a panic
        st.act(&engine, "record-stop", json!({}));
        assert!(!st.status.is_empty());
    }

    #[test]
    fn sync_rec_adopts_a_recording_started_elsewhere_and_drops_a_stale_one() {
        use oats_audio::fake::FakeBackend;
        let (engine, _d) = testutil::engine(&[]);
        engine.set_audio_factory(Box::new(|_| Ok(Box::new(FakeBackend::default()))));
        let mut st = State::default();
        model::dispatch(&engine, "record-start", json!({})).unwrap();
        st.sync_rec(&engine);
        assert!(matches!(st.rec, Rec::Recording { .. }));
        model::dispatch(&engine, "record-pause", json!({})).unwrap();
        st.sync_rec(&engine);
        assert!(matches!(st.rec, Rec::Paused { .. }));
        model::dispatch(&engine, "record-stop", json!({})).unwrap();
        st.sync_rec(&engine);
        assert_eq!(st.rec, Rec::Idle);
    }
}
