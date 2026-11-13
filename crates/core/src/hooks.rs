//! Configurable hooks (dot-4uzgol.89): `[[hooks]]` entries in the user config run a command when a
//! recording or session event happens (`record.stop`, `notes.generated`, ...).
//!
//! The engine's bus tap (`on_event`) maps the published events onto hook events (the table in
//! `events`, dot-4uzgol.111.2) and starts each enabled matching hook on its own thread, so a hook
//! never blocks a recording, the control socket or the UI. A hook gets the session as `OATS_*`
//! environment variables and the `oats.hook-event/1` envelope as JSON on stdin and in the file
//! `OATS_EVENT_FILE`; it is killed (its whole process group on unix) past its timeout. A hook
//! fires at most once per dedup key, and only when its `[hooks.match]` filter holds (`matches`,
//! dot-4uzgol.111.4): every decision, fire or skip, comes with a reason, is kept for `hook-list`
//! and published as `hook-decided`, and `hook-explain` shows them without running anything.
//! `transcript.phrase` (dot-4uzgol.111.6, `phrase`) fires on what is said: the tap judges each
//! final utterance against every phrase hook, in order, before it returns. Every run (exit
//! code, timing, the first `app.hook_output` bytes of stdout and stderr) is kept for `hook-list` and published as `hook-finished`; a failure
//! is logged and shown, never raised into the session.

pub mod events;
pub mod phrase;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::action::Action;
use crate::config::keys::{self, Kind};
use crate::config::typed::{Config, Dur};
use crate::config::{ConfigError, Result};
use crate::control::engine::Engine;

pub use events::names;

/// A `[[hooks]]` table's fields in the order they are written to the user file.
pub const FIELDS: &[&str] = &[
    "name", "event", "command", "shell", "timeout", "enabled", "cwd", "match",
];

/// The `speaker` values a `match` may name (the phrase triggers read it, dot-4uzgol.111.6).
pub const SPEAKERS: &[&str] = &["you", "remote", "any"];

/// The key whose range bounds a hook's own `timeout` (and whose value is the default).
const TIMEOUT_KEY: &str = "app.hook_timeout";

/// The key whose range bounds a hook's own `match.cooldown` (and whose value is the default).
const COOLDOWN_KEY: &str = "app.hook_cooldown";

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One `[[hooks]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// How `hook-test` / `-edit` / `-remove` name it; empty means `hook-<position>`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub event: String,
    /// argv: the program then its arguments. With `shell`, the first item is a shell script and
    /// the rest its positional parameters ($1 ...).
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub shell: bool,
    /// None: `app.hook_timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<Dur>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// Working directory; empty means the session folder (or the app's own when there is none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    /// `[hooks.match]`: when the event fires the hook (empty: every time).
    #[serde(default, rename = "match", skip_serializing_if = "Match::is_empty")]
    pub filter: Match,
}

/// A hook's `[hooks.match]` table (docs/design/hooks.md L2). Every field is optional, and all
/// present fields must hold. `title`, each of `attendees` and `source` are patterns: a glob
/// (`*`, `?`, `[abc]`, `[!abc]`; case-insensitive, the whole text), or after `re:` a regex
/// (unanchored, as written). `speaker`, `phrases` and `cooldown` are the phrase triggers'
/// (dot-4uzgol.111.6, `phrase`): `matches` judges `speaker` and `phrases` on `transcript.phrase`,
/// the tap `cooldown`. `before_end` (`calendar.ending`, dot-4uzgol.111.7) is parsed and checked
/// here; its event judges it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    /// The session title (or its calendar event's title).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Any one pattern matching any one attendee (email, else name) of the calendar event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<String>,
    /// The calendar source name of the session's calendar event.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// true: only sessions matched to a calendar event; false: only sessions with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<bool>,
    /// you | remote | any (empty: any): the side of the utterance's speaker.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub speaker: String,
    /// Literal phrases (normalized, word-bounded) or `re:<regex>`; the first said fires.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phrases: Vec<String>,
    /// Per hook per session, in recording time; None: `app.hook_cooldown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<Dur>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_end: Option<Dur>,
}

impl Match {
    pub fn is_empty(&self) -> bool {
        *self == Match::default()
    }
}

/// Compile a `match` pattern: `re:<regex>`, else a glob (case-insensitive, anchored at both
/// ends). Err says what is wrong with it.
pub fn pattern(src: &str) -> std::result::Result<regex::Regex, String> {
    if let Some(re) = src.strip_prefix("re:") {
        return regex::Regex::new(re).map_err(|e| format!("bad regex \"{re}\": {e}"));
    }
    let mut body = String::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => body.push_str(".*"),
            '?' => body.push('.'),
            '[' => {
                body.push('[');
                if chars.next_if_eq(&'!').is_some() {
                    body.push('^');
                }
                // A `]` first in the class is one of its members.
                let mut first = true;
                loop {
                    match chars.next() {
                        None => return Err(format!("bad glob \"{src}\": [ is never closed")),
                        Some(']') if !first => break,
                        Some(c @ ('-' | 'a'..='z' | 'A'..='Z' | '0'..='9')) => body.push(c),
                        Some(c) => body.push_str(&regex::escape(&c.to_string())),
                    }
                    first = false;
                }
                body.push(']');
            }
            c => body.push_str(&regex::escape(&c.to_string())),
        }
    }
    regex::Regex::new(&format!("(?i)^(?:{body})$")).map_err(|e| format!("bad glob \"{src}\": {e}"))
}

/// Whether a hook fires on an event, and why (or why not).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub fire: bool,
    pub reason: String,
}

fn quoted(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("{s:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `hook` fires on `envelope` (an `oats.hook-event/1` envelope, or as much of one as
/// `event`, `session.title` and `calendar`), with a reason either way: the event it listens for,
/// whether it is on, then each `match` field (every failing one when it does not fire, every one
/// that held when it does). Pure: no dedup, nothing run.
pub fn matches(hook: &Hook, envelope: &Value) -> Verdict {
    let event = envelope["event"].as_str().unwrap_or_default();
    let no = |reason: String| Verdict {
        fire: false,
        reason,
    };
    if hook.event != event {
        return no(format!("listens for {}, not {event}", hook.event));
    }
    if !hook.enabled {
        return no("the hook is off (enabled = false)".into());
    }
    let m = &hook.filter;
    let cal = &envelope["calendar"];
    let cal_title = cal["title"].as_str().unwrap_or_default();
    let (mut held, mut failed) = (Vec::new(), Vec::new());
    let mut judge = |ok: bool, why: String| if ok { held.push(why) } else { failed.push(why) };
    if let Some(want) = m.calendar {
        let has = cal.is_object();
        let what = if has {
            format!("the session matched calendar event {cal_title:?}")
        } else {
            "the session matched no calendar event".to_string()
        };
        judge(has == want, format!("match.calendar = {want}: {what}"));
    }
    if !m.title.is_empty() {
        let mut titles = vec![envelope["session"]["title"]
            .as_str()
            .unwrap_or_default()
            .to_string()];
        if !cal_title.is_empty() && cal_title != titles[0] {
            titles.push(cal_title.to_string());
        }
        titles.retain(|t| !t.is_empty());
        match pattern(&m.title) {
            Err(e) => judge(false, format!("match.title: {e}")),
            Ok(re) => match titles.iter().find(|t| re.is_match(t)) {
                Some(t) => judge(true, format!("match.title: {:?} matched {t:?}", m.title)),
                None if titles.is_empty() => judge(
                    false,
                    format!("match.title: {:?}: the session has no title", m.title),
                ),
                None => judge(
                    false,
                    format!(
                        "match.title: {:?} did not match {}",
                        m.title,
                        titles
                            .iter()
                            .map(|t| format!("{t:?}"))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    ),
                ),
            },
        }
    }
    if !m.attendees.is_empty() {
        let people: Vec<String> = cal["attendees"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect();
        let mut hit = None;
        let mut errors = Vec::new();
        for p in &m.attendees {
            match pattern(p) {
                Err(e) => errors.push(e),
                Ok(re) => {
                    if let Some(a) = people.iter().find(|a| re.is_match(a)) {
                        hit = hit.or(Some((p, a)));
                    }
                }
            }
        }
        match (hit, errors.is_empty()) {
            (_, false) => judge(false, format!("match.attendees: {}", errors.join("; "))),
            (Some((p, a)), true) => judge(true, format!("match.attendees: {p:?} matched {a:?}")),
            (None, true) if people.is_empty() => judge(
                false,
                format!(
                    "match.attendees: {}: the session has no attendees",
                    quoted(&m.attendees)
                ),
            ),
            (None, true) => judge(
                false,
                format!(
                    "match.attendees: none of {} matched {}",
                    quoted(&m.attendees),
                    quoted(&people)
                ),
            ),
        }
    }
    if !m.source.is_empty() {
        let source = cal["source"].as_str().unwrap_or_default();
        match pattern(&m.source) {
            Err(e) => judge(false, format!("match.source: {e}")),
            Ok(_) if !cal.is_object() => judge(
                false,
                format!(
                    "match.source: {:?}: the session matched no calendar event",
                    m.source
                ),
            ),
            Ok(re) => judge(
                re.is_match(source),
                format!(
                    "match.source: {:?} {} calendar source {source:?}",
                    m.source,
                    if re.is_match(source) {
                        "matched"
                    } else {
                        "did not match"
                    }
                ),
            ),
        }
    }
    if event == phrase::EVENT {
        let u = &envelope["data"]["utterance"];
        let speaker = u["speaker"].as_str().unwrap_or_default();
        if !m.speaker.is_empty() && m.speaker != "any" {
            let side = phrase::side(speaker);
            judge(
                side == m.speaker,
                format!("match.speaker = {}: {speaker:?} is {side}", m.speaker),
            );
        }
        match u["text"].as_str() {
            None => judge(
                false,
                "match.phrases: the event has no utterance".to_string(),
            ),
            Some(text) => match phrase::first_match(&m.phrases, text) {
                Err(e) => judge(false, format!("match.phrases: {e}")),
                Ok(Some(p)) => judge(true, format!("match.phrases: {p:?} said in {text:?}")),
                Ok(None) if m.phrases.is_empty() => {
                    judge(false, "match.phrases: none set".to_string())
                }
                Ok(None) => judge(
                    false,
                    format!(
                        "match.phrases: none of {} said in {text:?}",
                        quoted(&m.phrases)
                    ),
                ),
            },
        }
    }
    if !failed.is_empty() {
        return no(failed.join("; "));
    }
    Verdict {
        fire: true,
        reason: if m.is_empty() {
            format!("no match filter: fires on every {event}")
        } else if held.is_empty() {
            format!("nothing in match filters {event}: fires on every one")
        } else {
            held.join("; ")
        },
    }
}

impl Hook {
    /// `name`, or `hook-<n>` for the n-th (1-based) entry with none.
    pub fn label(&self, index: usize) -> String {
        if self.name.is_empty() {
            format!("hook-{}", index + 1)
        } else {
            self.name.clone()
        }
    }

    pub fn timeout_of(&self, cfg: &Config) -> Dur {
        self.timeout.unwrap_or(cfg.app.hook_timeout)
    }
}

fn bad(msg: impl Into<String>) -> ConfigError {
    ConfigError::new("invalid_value", msg)
}

/// Parse and check a `[[hooks]]` list: known events, a non-empty program, a timeout within the
/// `app.hook_timeout` range and unique names.
pub fn parse(v: &Value) -> Result<Vec<Hook>> {
    let hooks: Vec<Hook> = serde_json::from_value(v.clone())
        .map_err(|e| ConfigError::new("type_mismatch", format!("[[hooks]]: {e}")))?;
    check(&hooks)?;
    Ok(hooks)
}

fn check(hooks: &[Hook]) -> Result<()> {
    let (min_ms, max_ms) = match keys::find(TIMEOUT_KEY).map(|k| k.kind) {
        Some(Kind::Duration { min_ms, max_ms }) => (min_ms, max_ms),
        _ => unreachable!("{TIMEOUT_KEY} is a duration key"),
    };
    let mut seen = std::collections::BTreeSet::new();
    for (i, h) in hooks.iter().enumerate() {
        let label = h.label(i);
        if events::find(&h.event).is_none() {
            return Err(bad(format!(
                "hook {label}: event \"{}\" is not one of {}",
                h.event,
                names().join(", ")
            )));
        }
        if h.command.first().is_none_or(|p| p.trim().is_empty()) {
            return Err(bad(format!(
                "hook {label}: command must name a program (an argv array like [\"notify-send\", \"done\"])"
            )));
        }
        if let Some(t) = h.timeout {
            if !(min_ms..=max_ms).contains(&t.0) {
                return Err(bad(format!(
                    "hook {label}: timeout must be from {} to {}",
                    keys::format_duration_ms(min_ms),
                    keys::format_duration_ms(max_ms)
                )));
            }
        }
        check_match(&label, &h.filter)?;
        if h.event == phrase::EVENT && h.filter.phrases.is_empty() {
            return Err(bad(format!(
                "hook {label}: a {} hook needs match.phrases (literal phrases or re:<regex>)",
                phrase::EVENT
            )));
        }
        if !seen.insert(label.clone()) {
            return Err(bad(format!("hook name {label} is used twice")));
        }
    }
    Ok(())
}

/// A `match` table's patterns and phrases compile and its `speaker` is one of `SPEAKERS`; Err
/// names the hook.
fn check_match(label: &str, m: &Match) -> Result<()> {
    let fields = [("title", &m.title), ("source", &m.source)]
        .into_iter()
        .filter(|(_, p)| !p.is_empty())
        .chain(m.attendees.iter().map(|p| ("attendees", p)));
    for (field, p) in fields {
        pattern(p).map_err(|e| bad(format!("hook {label}: match.{field}: {e}")))?;
    }
    for p in &m.phrases {
        phrase::compile(p).map_err(|e| bad(format!("hook {label}: match.phrases: {e}")))?;
    }
    if let (Some(c), Some(Kind::Duration { min_ms, max_ms })) =
        (m.cooldown, keys::find(COOLDOWN_KEY).map(|k| k.kind))
    {
        if !(min_ms..=max_ms).contains(&c.0) {
            return Err(bad(format!(
                "hook {label}: match.cooldown must be from {} to {}",
                keys::format_duration_ms(min_ms),
                keys::format_duration_ms(max_ms)
            )));
        }
    }
    if !m.speaker.is_empty() && !SPEAKERS.contains(&m.speaker.as_str()) {
        return Err(bad(format!(
            "hook {label}: match.speaker \"{}\" is not one of {}",
            m.speaker,
            SPEAKERS.join(", ")
        )));
    }
    Ok(())
}

/// The JSON schema of a `[hooks.match]` table.
pub fn match_schema() -> Value {
    let pat = json!({"type": "string", "description": "a glob (*, ?, [abc]; case-insensitive) or re:<regex>"});
    let dur = json!({"type": "string", "format": "duration", "pattern": "^[0-9]+(ms|s|m|h|d)$"});
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "title": pat,
            "attendees": {"type": "array", "items": pat},
            "source": pat,
            "calendar": {"type": "boolean"},
            "speaker": {"type": "string", "enum": SPEAKERS},
            "phrases": {"type": "array", "items": {"type": "string", "description": "a literal phrase (matched on normalized text: case-folded, punctuation stripped, word-bounded) or re:<regex> (run on the normalized text)"}},
            "cooldown": dur,
            "before_end": dur,
        },
    })
}

/// The JSON schema of one `[[hooks]]` entry (for `schema.json`).
pub fn item_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["event", "command"],
        "properties": {
            "name": {"type": "string"},
            "event": {"type": "string", "enum": names()},
            "command": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "shell": {"type": "boolean", "default": false},
            "timeout": {"type": "string", "format": "duration", "pattern": "^[0-9]+(ms|s|m|h|d)$"},
            "enabled": {"type": "boolean", "default": true},
            "cwd": {"type": "string"},
            "match": match_schema(),
        },
    })
}

/// One finished hook run.
#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub seq: u64,
    /// The envelope's `run_id` (`OATS_RUN_ID`).
    pub run_id: String,
    pub hook: String,
    pub event: String,
    pub session: Option<String>,
    /// The envelope's `seq` (`OATS_SEQ`) and `dedup`.
    pub event_seq: u64,
    pub dedup: String,
    /// Started by `hook-test` rather than by the event.
    pub test: bool,
    pub started: String,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Why it did not run or did not finish (spawn failure, timeout, signal).
    pub error: Option<String>,
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

/// One decision on whether a hook fires on an event (the `hook-decided` event).
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub at: String,
    pub hook: String,
    pub event: String,
    pub session: Option<String>,
    /// The envelope's `seq` and `dedup`.
    pub event_seq: u64,
    pub dedup: String,
    pub fire: bool,
    pub reason: String,
}

/// The engine's record of hook runs and decisions, newest first, each session's envelope `seq`
/// (the counter of its raw event log) and the (hook, dedup key) pairs that already fired.
#[derive(Default)]
pub struct Runner {
    runs: Mutex<VecDeque<Run>>,
    decisions: Mutex<VecDeque<Decision>>,
    seq: AtomicU64,
    /// Bus messages seen per session ("" for none), each logged to `<session>/events.jsonl`:
    /// the next envelope's `seq`.
    log: crate::sessions::events::Log,
    fired: Mutex<HashMap<String, BTreeSet<(String, String)>>>,
    /// Each session's `transcript.phrase` state (hits, fire times, dedup), judged in bus order.
    phrases: Mutex<HashMap<String, phrase::State>>,
}

impl Runner {
    fn keep(&self, run: &Run, max: usize) {
        let mut runs = self.runs.lock().expect("hook runs");
        runs.push_front(run.clone());
        runs.truncate(max.max(1));
    }

    fn decided(&self, d: &Decision, max: usize) {
        let mut ds = self.decisions.lock().expect("hook decisions");
        ds.push_front(d.clone());
        ds.truncate(max.max(1));
    }

    pub fn decisions(&self) -> Vec<Decision> {
        self.decisions
            .lock()
            .expect("hook decisions")
            .iter()
            .cloned()
            .collect()
    }

    pub fn runs(&self) -> Vec<Run> {
        self.runs
            .lock()
            .expect("hook runs")
            .iter()
            .cloned()
            .collect()
    }

    /// Count a message of `session` ("" for none) and log it to the session's `events.jsonl`
    /// when `dir` (its directory) exists: its `seq` (1 for its first message).
    pub fn next_seq(&self, session: &str, dir: Option<&Path>, topic: &str, data: &Value) -> u64 {
        self.log.record(session, dir, topic, data)
    }

    /// The `seq` of `session`'s last message (0 before its first in this run).
    pub fn last_seq(&self, session: &str) -> u64 {
        self.log.current(session)
    }

    /// True the first time `hook` asks for `dedup` in `session`; false after.
    pub fn first(&self, session: &str, hook: &str, dedup: &str) -> bool {
        self.fired
            .lock()
            .expect("hook dedup")
            .entry(session.to_string())
            .or_default()
            .insert((hook.to_string(), dedup.to_string()))
    }
}

/// What a hook is told about the event.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub event: String,
    pub session: Option<String>,
    pub dir: Option<PathBuf>,
    pub title: String,
    pub audio: Option<PathBuf>,
    pub transcript: Option<PathBuf>,
    /// The envelope's `seq`, `at` and `dedup`.
    pub seq: u64,
    pub at: String,
    pub dedup: String,
    /// The envelope's `calendar` (null when the session matched no event).
    pub calendar: Value,
    /// The event's own payload (the `record-stop` answer, ...).
    pub data: Value,
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl Ctx {
    fn env(&self, hook: &str, run_id: &str, file: Option<&PathBuf>) -> Vec<(&'static str, String)> {
        let p = |p: Option<&PathBuf>| p.map(|p| p.display().to_string()).unwrap_or_default();
        vec![
            ("OATS_EVENT", self.event.clone()),
            ("OATS_HOOK", hook.to_string()),
            ("OATS_SESSION_ID", self.session.clone().unwrap_or_default()),
            ("OATS_SESSION_DIR", p(self.dir.as_ref())),
            ("OATS_TITLE", self.title.clone()),
            ("OATS_AUDIO", p(self.audio.as_ref())),
            ("OATS_TRANSCRIPT", p(self.transcript.as_ref())),
            ("OATS_RUN_ID", run_id.to_string()),
            ("OATS_SEQ", self.seq.to_string()),
            ("OATS_EVENT_FILE", p(file)),
        ]
    }

    /// The `oats.hook-event/1` envelope a run of `hook` gets.
    pub fn envelope(&self, hook: &str, run_id: &str) -> Value {
        let p = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
        events::Head {
            event: self.event.clone(),
            seq: self.seq,
            at: self.at.clone(),
            dedup: self.dedup.clone(),
            session: json!({
                "id": self.session,
                "dir": p(&self.dir),
                "title": self.title,
                "audio": p(&self.audio),
                "transcript": p(&self.transcript),
            }),
            calendar: self.calendar.clone(),
            data: self.data.clone(),
        }
        .envelope(hook, run_id)
    }
}

/// The session's folder, title, audio, transcript (final, else live) and calendar event, as far
/// as they exist. `seq`, `at` and `dedup` are the caller's to fill.
pub fn context(engine: &Engine, event: &str, session: Option<&str>, data: Value) -> Ctx {
    let mut c = Ctx {
        event: event.to_string(),
        // The payload's title until the session's own is read (a session not stored yet).
        title: data["title"].as_str().unwrap_or_default().to_string(),
        session: session.filter(|s| !s.is_empty()).map(str::to_string),
        at: now_stamp(),
        calendar: events::calendar_of(&data["calendar"]["event"], &Value::Null),
        data,
        ..Ctx::default()
    };
    let Some(id) = c.session.clone() else {
        return c;
    };
    let Ok(v) = engine.store().get(&id) else {
        return c;
    };
    let dir = v["path"].as_str().map(PathBuf::from);
    if let Some(t) = v["title"].as_str().filter(|t| !t.is_empty()) {
        c.title = t.to_string();
    }
    let meta = v["metadata"].as_object().cloned().unwrap_or_default();
    if let Some(ev) = meta.get("calendarEvent").filter(|e| e.is_object()) {
        c.calendar = events::calendar_of(ev, meta.get("attendees").unwrap_or(&Value::Null));
    }
    if let Some(d) = &dir {
        let audio = d.join(
            crate::sessions::ext(&meta, "audioFile")
                .and_then(Value::as_str)
                .unwrap_or(crate::record::AUDIO_FILE),
        );
        c.audio = audio.is_file().then_some(audio);
        c.transcript = ["transcript.final.jsonl", "transcript.live.jsonl"]
            .iter()
            .map(|f| d.join(f))
            .find(|p| p.metadata().is_ok_and(|m| m.len() > 0));
    }
    c.dir = dir;
    c
}

/// The hook events a published bus event stands for.
pub fn events_for(topic: &str, data: &Value) -> Vec<&'static str> {
    events::events_for(topic, data)
        .iter()
        .map(|s| s.name)
        .collect()
}

/// The session a payload names (`session`, else `id`).
fn session_of(data: &Value) -> Option<String> {
    data["session"]
        .as_str()
        .or(data["id"].as_str())
        .map(str::to_string)
}

/// The engine's bus tap: count the message in its session's `seq` and start the hooks for
/// whatever `topic` stands for. Returns at once.
pub fn on_event(engine: &Arc<Engine>, topic: &str, data: &Value) {
    let session = session_of(data);
    let seq = count(engine, session.as_deref(), topic, data);
    for spec in events::events_for(topic, data) {
        start(engine, spec, topic, session.clone(), seq, data.clone());
    }
}

/// Start every enabled hook on `event` (an action's own event, not a bus message), each on its
/// own thread. Returns at once.
pub fn fire(engine: &Arc<Engine>, event: &str, data: Value) {
    let Some(spec) = events::find(event) else {
        crate::elog!("oats-meetings-rs: hooks: no event {event}");
        return;
    };
    let session = session_of(&data);
    // No bus topic stands for it, so the raw log keeps it under its event name.
    let seq = count(engine, session.as_deref(), event, &data);
    start(engine, spec, "", session, seq, data);
}

/// The `seq` of one message of `session` (L0: also appended to its `events.jsonl`).
fn count(engine: &Engine, session: Option<&str>, topic: &str, data: &Value) -> u64 {
    let session = session.unwrap_or_default();
    let dir = (!session.is_empty())
        .then(|| engine.store().session_dir(session))
        .flatten();
    engine.hooks.next_seq(session, dir.as_deref(), topic, data)
}

fn start(
    engine: &Arc<Engine>,
    spec: &events::Spec,
    topic: &str,
    session: Option<String>,
    seq: u64,
    data: Value,
) {
    let key = session.clone().unwrap_or_default();
    let dedup = spec.dedup_key(&key, topic, &data, seq);
    let snap = engine.snapshot();
    let hooks: Vec<(String, Hook)> = snap
        .config
        .hooks
        .iter()
        .enumerate()
        .filter(|(_, h)| h.event == spec.name)
        .map(|(i, h)| (h.label(i), h.clone()))
        .collect();
    if hooks.is_empty() {
        return;
    }
    if spec.name == phrase::EVENT {
        return start_phrase(engine, &hooks, session, seq, &data);
    }
    let event = spec.name;
    let engine = Arc::clone(engine);
    let at = now_stamp();
    let spawned = std::thread::Builder::new()
        .name("oats-hooks".into())
        .spawn(move || {
            let mut ctx = context(&engine, event, session.as_deref(), data);
            ctx.seq = seq;
            ctx.at = at;
            ctx.dedup = dedup;
            let envelope = ctx.envelope("", "");
            let threads: Vec<_> = hooks
                .into_iter()
                .filter(|(label, hook)| {
                    let mut v = matches(hook, &envelope);
                    // At most once per dedup key: a message seen again (a replay, a stop that
                    // re-matches the calendar event of its start) does not run the hook again.
                    if v.fire && !engine.hooks.first(&key, label, &ctx.dedup) {
                        v = Verdict {
                            fire: false,
                            reason: format!("already fired for {} ({})", ctx.dedup, v.reason),
                        };
                    }
                    decide(&engine, label, &ctx, &v);
                    v.fire
                })
                .map(|(label, hook)| {
                    let (engine, ctx) = (Arc::clone(&engine), ctx.clone());
                    std::thread::Builder::new()
                        .name(format!("oats-hook-{label}"))
                        .spawn(move || run_and_report(&engine, &label, &hook, &ctx, false))
                })
                .collect();
            for t in threads.into_iter().flatten() {
                let _ = t.join();
            }
        });
    if let Err(e) = spawned {
        crate::elog!("oats-meetings-rs: hooks for {event}: cannot start a thread: {e}");
    }
}

/// Judge a final utterance (`data`, a `segment` payload) for the `transcript.phrase` hooks in
/// bus order, on the publishing thread, so the cooldown and the live-to-final dedup see the
/// utterances as they were said; the session is read only when some hook's phrase is said. The
/// verdicts are then published (`hook-decided`, never from inside the bus tap) and the hooks
/// that fire run, each on its own thread.
fn start_phrase(
    engine: &Arc<Engine>,
    hooks: &[(String, Hook)],
    session: Option<String>,
    seq: u64,
    data: &Value,
) {
    if !phrase::any_hit(hooks, data) {
        return;
    }
    let key = session.clone().unwrap_or_default();
    let mut ctx = context(engine, phrase::EVENT, session.as_deref(), Value::Null);
    ctx.seq = seq;
    let base = ctx.envelope("", "");
    let cooldown = engine.snapshot().config.app.hook_cooldown;
    let decisions = {
        let mut all = engine.hooks.phrases.lock().expect("hook phrases");
        phrase::judge(all.entry(key).or_default(), hooks, &base, data, cooldown)
    };
    if decisions.is_empty() {
        return;
    }
    let hooks = hooks.to_vec();
    let engine = Arc::clone(engine);
    let spawned = std::thread::Builder::new()
        .name("oats-hooks".into())
        .spawn(move || {
            let threads: Vec<_> = decisions
                .into_iter()
                .filter_map(|d| {
                    let mut c = ctx.clone();
                    c.dedup = d.dedup;
                    c.data = d.data;
                    decide(&engine, &d.hook, &c, &d.verdict);
                    let hook = hooks.iter().find(|(l, _)| *l == d.hook)?.1.clone();
                    d.verdict.fire.then_some((d.hook, hook, c))
                })
                .map(|(label, hook, c)| {
                    let engine = Arc::clone(&engine);
                    std::thread::Builder::new()
                        .name(format!("oats-hook-{label}"))
                        .spawn(move || run_and_report(&engine, &label, &hook, &c, false))
                })
                .collect();
            for t in threads.into_iter().flatten() {
                let _ = t.join();
            }
        });
    if let Err(e) = spawned {
        crate::elog!(
            "oats-meetings-rs: hooks for {}: cannot start a thread: {e}",
            phrase::EVENT
        );
    }
}

/// What the config means for `transcript.phrase` hooks that cannot fire mid-meeting: one line
/// per enabled phrase hook when `audio.transcribe` is `deferred` (they fire during the final
/// pass, `live = false`) or `off`, or `stt.live.enabled` is off. `config-check` and `hook-list`
/// return them.
pub fn warnings(cfg: &Config) -> Vec<String> {
    use crate::config::typed::AudioTranscribe;
    let why = match cfg.audio.transcribe {
        AudioTranscribe::Deferred => Some(
            "audio.transcribe = deferred: it cannot fire mid-meeting, only during the final pass after stop (data.live = false)",
        ),
        AudioTranscribe::Off => Some(
            "audio.transcribe = off: it cannot fire mid-meeting, only when session-transcribe runs later (data.live = false)",
        ),
        _ if !cfg.stt.live.enabled => Some(
            "stt.live.enabled = false: it cannot fire mid-meeting, only during the final pass after stop (data.live = false)",
        ),
        _ => None,
    };
    let Some(why) = why else {
        return Vec::new();
    };
    cfg.hooks
        .iter()
        .enumerate()
        .filter(|(_, h)| h.event == phrase::EVENT && h.enabled)
        .map(|(i, h)| format!("hook {}: {}: {why}", h.label(i), phrase::EVENT))
        .collect()
}

/// Keep a decision and publish it as `hook-decided`.
fn decide(engine: &Engine, label: &str, ctx: &Ctx, v: &Verdict) {
    let d = Decision {
        at: now_stamp(),
        hook: label.to_string(),
        event: ctx.event.clone(),
        session: ctx.session.clone(),
        event_seq: ctx.seq,
        dedup: ctx.dedup.clone(),
        fire: v.fire,
        reason: v.reason.clone(),
    };
    let max = engine.snapshot().config.app.hook_history as usize;
    engine.hooks.decided(&d, max);
    engine.bus.publish(
        "hook-decided",
        serde_json::to_value(&d).unwrap_or(Value::Null),
    );
}

/// Run `hook`, keep the run, log a failure and publish `hook-finished`.
fn run_and_report(engine: &Engine, label: &str, hook: &Hook, ctx: &Ctx, test: bool) -> Run {
    let cfg = engine.snapshot().config.clone();
    let mut run = run(label, hook, ctx, &cfg);
    run.test = test;
    run.seq = engine.hooks.seq.fetch_add(1, Ordering::SeqCst) + 1;
    engine.hooks.keep(&run, cfg.app.hook_history as usize);
    if !run.ok {
        crate::elog!(
            "oats-meetings-rs: hook {label} ({}) failed: {}",
            run.event,
            run.error.clone().unwrap_or_else(|| format!(
                "exit code {}",
                run.exit_code.map_or("none".into(), |c| c.to_string())
            ))
        );
    }
    engine.bus.publish(
        "hook-finished",
        serde_json::to_value(&run).unwrap_or(Value::Null),
    );
    run
}

fn command_of(hook: &Hook) -> Command {
    if !hook.shell {
        let mut c = Command::new(&hook.command[0]);
        c.args(&hook.command[1..]);
        return c;
    }
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(hook.command.join(" "));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(&hook.command[0])
            .arg("oats-hook")
            .args(&hook.command[1..]);
        c
    }
}

/// Read all of `r`, keeping the first `max` bytes; true when some were dropped.
pub(crate) fn drain(mut r: impl Read, max: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    let mut dropped = false;
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                dropped |= n > room;
            }
        }
    }
    (kept, dropped)
}

pub(crate) fn kill(child: &mut Child) {
    #[cfg(unix)]
    {
        // The hook leads its own process group: take its children (a shell's sleep) down too.
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: kill(2) on a process group we created; no memory is touched.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

/// The envelope in a file for `OATS_EVENT_FILE` (for a script that cannot read stdin), removed
/// when the run is over.
struct EventFile(PathBuf);

impl EventFile {
    fn write(run_id: &str, envelope: &[u8]) -> Option<Self> {
        let safe: String = run_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let p = std::env::temp_dir().join(format!("oats-hook-{}-{safe}.json", std::process::id()));
        match std::fs::write(&p, envelope) {
            Ok(()) => Some(EventFile(p)),
            Err(e) => {
                crate::elog!(
                    "oats-meetings-rs: hook run {run_id}: cannot write {}: {e}",
                    p.display()
                );
                None
            }
        }
    }
}

impl Drop for EventFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Run one hook to completion (or its timeout) with `ctx`. Never panics, never returns an error:
/// whatever went wrong is in the `Run`.
pub fn run(label: &str, hook: &Hook, ctx: &Ctx, cfg: &Config) -> Run {
    let started = chrono::Utc::now();
    let t0 = Instant::now();
    let run_id = format!(
        "{}-{}-{label}",
        started.format("%Y%m%dT%H%M%S%3fZ"),
        ctx.seq
    );
    let mut out = Run {
        seq: 0,
        run_id: run_id.clone(),
        hook: label.to_string(),
        event: ctx.event.clone(),
        session: ctx.session.clone(),
        event_seq: ctx.seq,
        dedup: ctx.dedup.clone(),
        test: false,
        started: started.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        duration_ms: 0,
        exit_code: None,
        timed_out: false,
        error: None,
        ok: false,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
    };
    let input = serde_json::to_vec(&ctx.envelope(label, &run_id)).unwrap_or_default();
    let file = EventFile::write(&run_id, &input);
    let mut cmd = command_of(hook);
    cmd.envs(ctx.env(label, &run_id, file.as_ref().map(|f| &f.0)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !hook.cwd.is_empty() {
        cmd.current_dir(&hook.cwd);
    } else if let Some(d) = ctx.dir.as_ref().filter(|d| d.is_dir()) {
        cmd.current_dir(d);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            out.error = Some(format!("cannot run {:?}: {e}", hook.command[0]));
            out.duration_ms = t0.elapsed().as_millis() as u64;
            return out;
        }
    };
    let stdin = child.stdin.take().map(|mut w| {
        std::thread::spawn(move || {
            // A hook that never reads its stdin gets a broken pipe here, which is fine.
            let _ = w.write_all(&input);
        })
    });
    let max = cfg.app.hook_output as usize;
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .map(|r| r.map(|r| std::thread::spawn(move || drain(r, max))))
    .collect();
    let deadline = t0 + hook.timeout_of(cfg).as_duration();
    let poll = cfg.app.hook_poll.as_duration();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() >= deadline => {
                kill(&mut child);
                out.timed_out = true;
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(poll),
            Err(e) => {
                out.error = Some(format!("waiting for the hook: {e}"));
                kill(&mut child);
                break child.wait().ok();
            }
        }
    };
    let mut texts = readers.into_iter().map(|r| {
        r.and_then(|h| h.join().ok())
            .unwrap_or_else(|| (Vec::new(), false))
    });
    let (so, so_cut) = texts.next().unwrap_or_default();
    let (se, se_cut) = texts.next().unwrap_or_default();
    if let Some(h) = stdin {
        let _ = h.join();
    }
    out.stdout = String::from_utf8_lossy(&so).into_owned();
    out.stderr = String::from_utf8_lossy(&se).into_owned();
    out.truncated = so_cut || se_cut;
    out.duration_ms = t0.elapsed().as_millis() as u64;
    out.exit_code = status.and_then(|s| s.code());
    if out.timed_out {
        out.error = Some(format!(
            "timed out after {} and was killed",
            keys::format_duration_ms(hook.timeout_of(cfg).0)
        ));
    } else if out.error.is_none() && out.exit_code.is_none() {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = status.and_then(|s| s.signal()) {
                out.error = Some(format!("killed by signal {sig}"));
            }
        }
        if out.error.is_none() {
            out.error = Some("ended without an exit code".into());
        }
    }
    out.ok = !out.timed_out && out.error.is_none() && out.exit_code == Some(0);
    out
}

// ---------- actions ----------

fn hooks_of(engine: &Engine) -> Vec<Hook> {
    engine.snapshot().config.hooks.clone()
}

/// The index of the hook `name` names: its label, or its 1-based position.
fn find(hooks: &[Hook], name: &str) -> Result<usize> {
    hooks
        .iter()
        .enumerate()
        .position(|(i, h)| h.label(i) == name)
        .or_else(|| {
            name.parse::<usize>()
                .ok()
                .filter(|n| (1..=hooks.len()).contains(n))
                .map(|n| n - 1)
        })
        .ok_or_else(|| {
            ConfigError::new(
                "not_found",
                format!(
                    "no hook {name:?}; hooks: {}",
                    hooks
                        .iter()
                        .enumerate()
                        .map(|(i, h)| h.label(i))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })
}

fn save(engine: &Engine, hooks: &[Hook], source: &str) -> Result<()> {
    check(hooks)?;
    let v = serde_json::to_value(hooks).map_err(|e| ConfigError::new("internal", e.to_string()))?;
    engine.set("hooks", &v, source).map(|_| ())
}

/// A command given as an argv array, or as one string (split on whitespace; with `shell` it is
/// the script itself).
fn command_arg(v: &Value, shell: bool) -> Result<Vec<String>> {
    match v {
        Value::Array(items) => items
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| bad("command must be an array of strings"))
            })
            .collect(),
        Value::String(s) if shell => Ok(vec![s.clone()]),
        Value::String(s) => Ok(s.split_whitespace().map(str::to_string).collect()),
        _ => Err(bad("command must be an argv array or a string")),
    }
}

fn timeout_arg(t: &str) -> Result<Option<Dur>> {
    if t.trim().is_empty() {
        return Ok(None);
    }
    keys::parse_duration_ms(t)
        .map(|ms| Some(Dur(ms)))
        .ok_or_else(|| bad(format!("timeout \"{t}\" is not a duration like \"30s\"")))
}

/// A `match` argument: an object of `[hooks.match]` fields; null or `{}` for none.
fn match_arg(v: &Value) -> Result<Match> {
    if v.is_null() {
        return Ok(Match::default());
    }
    serde_json::from_value(v.clone()).map_err(|e| bad(format!("match: {e}")))
}

fn view(engine: &Engine, i: usize, h: &Hook) -> Value {
    let cfg = &engine.snapshot().config;
    let label = h.label(i);
    let last = engine.hooks.runs().into_iter().find(|r| r.hook == label);
    let decided = engine
        .hooks
        .decisions()
        .into_iter()
        .find(|d| d.hook == label);
    json!({
        "index": i + 1,
        "name": label,
        "event": h.event,
        "command": h.command,
        "shell": h.shell,
        "timeout": keys::format_duration_ms(h.timeout_of(cfg).0),
        "enabled": h.enabled,
        "cwd": h.cwd,
        "match": h.filter,
        "last_run": last,
        "last_decision": decided,
    })
}

/// `hook-explain`'s `transcript.phrase` data from its `utterance` argument (`text`, and
/// optionally `speaker`, default `remote_1`, `start_ms`, `end_ms`); `phrase` names none, as each
/// hook's verdict names its own.
fn explain_phrase_data(session: Option<&str>, u: Option<&Value>) -> Result<Value> {
    let Some(u) = u.filter(|u| !u.is_null()) else {
        return Ok(json!({"session": session}));
    };
    let text = u["text"]
        .as_str()
        .ok_or_else(|| bad("utterance needs text"))?;
    let speaker = u["speaker"].as_str().unwrap_or("remote_1");
    let start_ms = u["start_ms"].as_u64().unwrap_or_default();
    let end_ms = u["end_ms"].as_u64().unwrap_or(start_ms).max(start_ms);
    Ok(json!({
        "session": session, "kind": "final", "live": false, "phrase": null,
        "utterance": {"id": format!("{speaker}@{start_ms}"), "text": text, "speaker": speaker,
                      "start_ms": start_ms, "end_ms": end_ms},
    }))
}

/// The newest session, for a `hook-test` (or `session-events`) given none.
pub(crate) fn newest_session(engine: &Engine) -> Option<String> {
    let list = engine.store().list(None, None).ok()?;
    list["sessions"]
        .as_array()?
        .iter()
        .max_by(|a, b| {
            a["started_at"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["started_at"].as_str().unwrap_or_default())
        })
        .and_then(|s| s["id"].as_str().map(str::to_string))
}

pub fn dispatch(engine: &Engine, a: &Action) -> Result<Value> {
    match a {
        Action::HookList {} => {
            let hooks = hooks_of(engine);
            Ok(json!({
                "hooks": hooks.iter().enumerate().map(|(i, h)| view(engine, i, h)).collect::<Vec<_>>(),
                "runs": engine.hooks.runs(),
                "decisions": engine.hooks.decisions(),
                "warnings": warnings(&engine.snapshot().config),
                "events": names(),
                "event_docs": events::TABLE.iter()
                    .map(|s| json!({"name": s.name, "description": s.description}))
                    .collect::<Vec<_>>(),
            }))
        }
        Action::HookEvents { event } => {
            let mut v = events::table_json();
            if let Some(name) = event.as_deref().filter(|e| !e.is_empty()) {
                if events::find(name).is_none() {
                    return Err(ConfigError::new(
                        "not_found",
                        format!("no hook event \"{name}\"; events: {}", names().join(", ")),
                    ));
                }
                if let Some(list) = v["events"].as_array_mut() {
                    list.retain(|e| e["name"] == name);
                }
            }
            Ok(v)
        }
        Action::HookExplain {
            session,
            event,
            utterance,
        } => {
            if events::find(event).is_none() {
                return Err(ConfigError::new(
                    "not_found",
                    format!("no hook event \"{event}\"; events: {}", names().join(", ")),
                ));
            }
            let session = match session {
                Some(s) => {
                    engine.store().get(s)?;
                    Some(s.clone())
                }
                None => newest_session(engine),
            };
            let ctx = context(
                engine,
                event,
                session.as_deref(),
                json!({"session": session}),
            );
            let mut envelope = ctx.envelope("", "");
            if event == phrase::EVENT {
                envelope["data"] = explain_phrase_data(session.as_deref(), utterance.as_ref())?;
            }
            let hooks: Vec<Value> = hooks_of(engine)
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let v = matches(h, &envelope);
                    json!({"name": h.label(i), "event": h.event, "enabled": h.enabled,
                           "match": h.filter, "fire": v.fire, "reason": v.reason})
                })
                .collect();
            Ok(json!({
                "session": session, "event": event, "title": ctx.title,
                "calendar": ctx.calendar, "data": envelope["data"], "hooks": hooks,
            }))
        }
        Action::HookTest {
            hook,
            session,
            event,
        } => {
            let hooks = hooks_of(engine);
            let i = find(&hooks, hook)?;
            let h = &hooks[i];
            let event = event.clone().unwrap_or_else(|| h.event.clone());
            if events::find(&event).is_none() {
                return Err(bad(format!(
                    "event \"{event}\" is not one of {}",
                    names().join(", ")
                )));
            }
            let session = match session {
                Some(s) => {
                    engine.store().get(s)?;
                    Some(s.clone())
                }
                None => newest_session(engine),
            };
            let data = json!({"session": session, "test": true});
            let mut ctx = context(engine, &event, session.as_deref(), data);
            // A test run is outside dedup: its key names the run, and it always runs. It is no
            // bus message, so it takes the session's last `seq` rather than a new one (the raw
            // log stays gap-free).
            ctx.seq = engine
                .hooks
                .last_seq(session.as_deref().unwrap_or_default());
            ctx.dedup = format!(
                "{}:{event}:test:{}",
                session.as_deref().unwrap_or_default(),
                ctx.seq
            );
            let run = run_and_report(engine, &h.label(i), h, &ctx, true);
            serde_json::to_value(run).map_err(|e| ConfigError::new("internal", e.to_string()))
        }
        Action::HookAdd {
            name,
            event,
            command,
            shell,
            timeout,
            enabled,
            cwd,
            r#match,
        } => {
            let mut hooks = hooks_of(engine);
            let shell = shell.unwrap_or(false);
            let h = Hook {
                name: name.clone(),
                event: event.clone(),
                command: command_arg(command, shell)?,
                shell,
                timeout: timeout_arg(timeout.as_deref().unwrap_or_default())?,
                enabled: enabled.unwrap_or(true),
                cwd: cwd.clone().unwrap_or_default(),
                filter: match_arg(r#match.as_ref().unwrap_or(&Value::Null))?,
            };
            if name.trim().is_empty() {
                return Err(bad("hook-add needs a name"));
            }
            hooks.push(h);
            save(engine, &hooks, "hook-add")?;
            Ok(view(engine, hooks.len() - 1, &hooks[hooks.len() - 1]))
        }
        Action::HookEdit {
            hook,
            name,
            event,
            command,
            shell,
            timeout,
            enabled,
            cwd,
            r#match,
        } => {
            let mut hooks = hooks_of(engine);
            let i = find(&hooks, hook)?;
            let h = &mut hooks[i];
            if let Some(n) = name {
                h.name = n.clone();
            }
            if let Some(e) = event {
                h.event = e.clone();
            }
            if let Some(s) = shell {
                h.shell = *s;
            }
            if let Some(c) = command {
                h.command = command_arg(c, h.shell)?;
            }
            if let Some(t) = timeout {
                h.timeout = timeout_arg(t)?;
            }
            if let Some(e) = enabled {
                h.enabled = *e;
            }
            if let Some(c) = cwd {
                h.cwd = c.clone();
            }
            if let Some(m) = r#match {
                h.filter = match_arg(m)?;
            }
            save(engine, &hooks, "hook-edit")?;
            Ok(view(engine, i, &hooks[i]))
        }
        Action::HookRemove { hook } => {
            let mut hooks = hooks_of(engine);
            let i = find(&hooks, hook)?;
            let gone = hooks.remove(i);
            save(engine, &hooks, "hook-remove")?;
            Ok(json!({"removed": gone.label(i), "count": hooks.len()}))
        }
        Action::HookEnable { hook, enabled } => {
            let mut hooks = hooks_of(engine);
            let i = find(&hooks, hook)?;
            hooks[i].enabled = enabled.unwrap_or(true);
            save(engine, &hooks, "hook-enable")?;
            Ok(view(engine, i, &hooks[i]))
        }
        _ => unreachable!("not a hook action"),
    }
}
