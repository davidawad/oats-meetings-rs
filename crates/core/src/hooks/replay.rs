//! Replay (dot-4uzgol.111.8, docs/design/hooks.md L5): a session's raw log (`events.jsonl`, L0)
//! fed back through the trigger layer (L2) to say which hooks the session fired, in order, with
//! each envelope and every verdict's reason, without a live meeting.
//!
//! `simulate` is pure. Each line stands for the hook events its topic does (`events_for`), or for
//! the one it is named after (an event an action fired itself: `notes.generated`,
//! `calendar.ending`, ...). Every hook on the event is judged, disabled ones too, as the bus tap
//! does: `transcript.phrase` through `phrase::judge` (cooldown and the live-to-final dedup in
//! recording time, so a replay judges as the live pass did), `calendar.ending` for the hooks that
//! came due on that tick (a hook's `match.before_end` at least the line's `remaining_ms`, once
//! per replay), everything else through `matches`, then dedup. The replay keeps its own dedup
//! state: it starts as the recording did, and consumes nothing of the engine's.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{json, Value};

use super::{ending, events, matches, phrase, Hook, Verdict};
use crate::config::typed::Dur;

/// The session a replay judges against: the envelope's `session` (`{id, dir, title, audio,
/// transcript}`) and `calendar` (null for none).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Base {
    pub session: Value,
    pub calendar: Value,
}

/// The session named by a payload (`session`, else `id`).
pub fn session_of(data: &Value) -> Option<&str> {
    data["session"].as_str().or(data["id"].as_str())
}

impl Base {
    /// What the log itself says of its session: the id the lines name, the title and calendar
    /// event `recording-started` (else `recording-stopped`) carries. Paths are null.
    pub fn from_log(lines: &[Value]) -> Base {
        let id = lines
            .iter()
            .find_map(|l| session_of(&l["data"]))
            .map(str::to_string);
        let of = |topic: &str| {
            lines
                .iter()
                .find(|l| l["topic"] == topic)
                .map(|l| &l["data"])
        };
        let title = ["recording-started", "recording-stopped"]
            .iter()
            .filter_map(|t| of(t))
            .find_map(|d| d["title"].as_str().filter(|t| !t.is_empty()))
            .unwrap_or_default();
        let calendar = ["recording-started", "recording-stopped"]
            .iter()
            .filter_map(|t| of(t))
            .map(|d| events::calendar_of(&d["calendar"]["event"], &Value::Null))
            .find(|c| !c.is_null())
            .unwrap_or(Value::Null);
        Base {
            session: json!({"id": id, "dir": null, "title": title, "audio": null, "transcript": null}),
            calendar,
        }
    }
}

/// The defaults a hook's own `match` falls back to (`app.hook_cooldown`, `app.hook_before_end`).
#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub cooldown: Dur,
    pub before_end: Dur,
}

/// One hook's verdict on one event of the replay.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    /// The log line's `seq`, `at` and `topic`.
    pub seq: u64,
    pub at: String,
    pub topic: String,
    pub event: String,
    pub hook: String,
    pub fire: bool,
    pub reason: String,
    pub dedup: String,
    /// The envelope the hook gets, for a fire (`run_id` `simulated-<seq>-<hook>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope: Option<Value>,
}

/// The run id a simulated fire's envelope carries.
pub fn run_id(seq: u64, hook: &str) -> String {
    format!("simulated-{seq}-{hook}")
}

/// The hook events a log line stands for: the events its topic does, or the one it is named
/// after (an event an action fired itself, logged under its name), and the topic its dedup key
/// is formed with ("" for the latter, as `fire` forms it).
fn specs_of<'a>(topic: &'a str, data: &Value) -> (Vec<&'static events::Spec>, &'a str) {
    match events::find(topic) {
        Some(s) => (vec![s], ""),
        None => (events::events_for(topic, data), topic),
    }
}

/// Replay `lines` (`{seq, at, topic, data}`, in log order) for `hooks` (label, hook) against
/// `base`: every verdict, in the order the engine makes them (line by line, each line's events in
/// table order, each event's hooks in config order).
pub fn simulate(hooks: &[(String, Hook)], lines: &[Value], base: &Base, opts: Opts) -> Vec<Step> {
    let mut fired: BTreeSet<(String, String)> = BTreeSet::new();
    let mut phrases = phrase::State::default();
    let mut due: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for line in lines {
        let topic = line["topic"].as_str().unwrap_or_default();
        let data = &line["data"];
        let seq = line["seq"].as_u64().unwrap_or_default();
        let at = line["at"].as_str().unwrap_or_default().to_string();
        let (specs, dedup_topic) = specs_of(topic, data);
        let named = session_of(data).map(str::to_string);
        let key = named.clone().unwrap_or_default();
        // A line naming no session (or another one) is judged on its own payload, as the tap does
        // for a session the store does not know.
        let mine = named.is_some() && named.as_deref() == base.session["id"].as_str();
        let mut session = if mine {
            base.session.clone()
        } else {
            json!({"id": named, "dir": null, "title": data["title"].as_str().unwrap_or_default(),
                   "audio": null, "transcript": null})
        };
        let calendar = match &base.calendar {
            c if mine && !c.is_null() => c.clone(),
            _ => events::calendar_of(&data["calendar"]["event"], &Value::Null),
        };
        if session["title"].as_str().is_none_or(str::is_empty) {
            session["title"] = json!(data["title"].as_str().unwrap_or_default());
        }
        for spec in specs {
            let on: Vec<&(String, Hook)> =
                hooks.iter().filter(|(_, h)| h.event == spec.name).collect();
            if on.is_empty() {
                continue;
            }
            let head = |dedup: String, data: Value| events::Head {
                event: spec.name.to_string(),
                seq,
                at: at.clone(),
                dedup,
                session: session.clone(),
                calendar: calendar.clone(),
                data,
            };
            let mut step = |hook: &str, dedup: String, data: Value, v: Verdict| {
                let h = head(dedup.clone(), data);
                out.push(Step {
                    seq,
                    at: at.clone(),
                    topic: topic.to_string(),
                    event: spec.name.to_string(),
                    hook: hook.to_string(),
                    fire: v.fire,
                    reason: v.reason,
                    dedup,
                    envelope: v.fire.then(|| h.envelope(hook, &run_id(seq, hook))),
                });
            };
            if spec.name == phrase::EVENT {
                let owned: Vec<(String, Hook)> = on.into_iter().cloned().collect();
                let base_env = head(String::new(), Value::Null).envelope("", "");
                for d in phrase::judge(&mut phrases, &owned, &base_env, data, opts.cooldown) {
                    step(&d.hook, d.dedup, d.data, d.verdict);
                }
                continue;
            }
            let dedup = spec.dedup_key(&key, dedup_topic, data, seq);
            let env = head(dedup.clone(), data.clone()).envelope("", "");
            for (label, hook) in on {
                if spec.name == ending::ENDING {
                    // The timer fires a hook once, on the tick it comes due; others get no say.
                    let left = data["remaining_ms"].as_u64().unwrap_or_default();
                    if ending::before_end(hook, opts.before_end).0 < left || due.contains(label) {
                        continue;
                    }
                    due.insert(label.clone());
                }
                let mut v = matches(hook, &env);
                if v.fire && !fired.insert((label.clone(), dedup.clone())) {
                    v = Verdict {
                        fire: false,
                        reason: format!("already fired for {dedup} ({})", v.reason),
                    };
                }
                step(label, dedup.clone(), data.clone(), v);
            }
        }
    }
    out
}

/// Parse a raw log's text: its JSON object lines with a `seq`, in file order, and the count of
/// lines that are not (a torn last line).
pub fn parse_log(text: &str) -> (Vec<Value>, usize) {
    let mut skipped = 0;
    let mut lines = Vec::new();
    for l in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Value>(l) {
            Ok(v) if v.is_object() && v["seq"].is_u64() => lines.push(v),
            _ => skipped += 1,
        }
    }
    (lines, skipped)
}
