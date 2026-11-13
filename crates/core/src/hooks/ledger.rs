//! The hook ledger (dot-4uzgol.111.8, docs/design/hooks.md L4): `<session>/hook-runs.jsonl`, one
//! line per decision (a verdict, fire or skip, with its reason) and one per run (exit code,
//! timing, the tail of its output), append-only, one file per session.
//!
//! A line is one `write` of the whole JSON object and its newline, in append mode, under that
//! session's own lock (the map of sessions is held only to find it): no lock is shared across
//! sessions, and a crash mid-write leaves at most a torn last line, which `read` skips and counts.
//! Every row has the same fields: `{kind, at, session, run_id, hook, event, seq, dedup, verdict,
//! reason, outcome, exit, timed_out, error, started, ms, stdout_tail, stderr_tail, test,
//! simulated}`; a verdict row's run fields are null. `outcome` is what `hook-runs` filters on:
//! `fire` / `skip` for a verdict, `ok` / `failed` / `timed_out` for a run.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::{Decision, Run};

/// The ledger's file name in the session directory.
pub const FILE: &str = "hook-runs.jsonl";

/// What a row's `outcome` can be, and so what `hook-runs` filters on.
pub const OUTCOMES: &[&str] = &["fire", "skip", "ok", "failed", "timed_out"];

/// Every session's ledger writer.
#[derive(Default)]
pub struct Ledger {
    sessions: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Ledger {
    /// Append `row` to the ledger of `session` in `dir` (when the directory exists). A write that
    /// fails is logged, never raised: the ledger never stops a hook.
    pub fn append(&self, session: &str, dir: &Path, row: &Value) {
        if !dir.is_dir() {
            return;
        }
        let lock = {
            let mut map = self.sessions.lock().expect("hook ledger sessions");
            Arc::clone(map.entry(session.to_string()).or_default())
        };
        let _one_writer = lock.lock().expect("hook ledger session");
        let path = dir.join(FILE);
        let mut text = row.to_string();
        text.push('\n');
        let wrote = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = wrote {
            crate::elog!("oats-meetings-rs: {}: {e}", path.display());
        }
    }
}

/// The last `max` bytes of `s` (cut forward to a character boundary).
pub fn tail(s: &str, max: usize) -> String {
    let mut from = s.len().saturating_sub(max);
    while !s.is_char_boundary(from) {
        from += 1;
    }
    s[from..].to_string()
}

/// The row of one decision.
pub fn verdict_row(d: &Decision, simulated: bool) -> Value {
    let verdict = if d.fire { "fire" } else { "skip" };
    json!({
        "kind": "verdict", "at": d.at, "session": d.session, "run_id": null, "hook": d.hook,
        "event": d.event, "seq": d.event_seq, "dedup": d.dedup, "verdict": verdict,
        "reason": d.reason, "outcome": verdict, "exit": null, "timed_out": false, "error": null,
        "started": null, "ms": null, "stdout_tail": null, "stderr_tail": null, "test": false,
        "simulated": simulated,
    })
}

/// The row of one run; `reason` is the verdict that fired it, `max` the bytes of each output tail.
pub fn run_row(r: &Run, reason: &str, max: usize) -> Value {
    let outcome = if r.ok {
        "ok"
    } else if r.timed_out {
        "timed_out"
    } else {
        "failed"
    };
    json!({
        "kind": "run", "at": r.started, "session": r.session, "run_id": r.run_id, "hook": r.hook,
        "event": r.event, "seq": r.event_seq, "dedup": r.dedup, "verdict": "fire",
        "reason": reason, "outcome": outcome, "exit": r.exit_code, "timed_out": r.timed_out,
        "error": r.error, "started": r.started, "ms": r.duration_ms,
        "stdout_tail": tail(&r.stdout, max), "stderr_tail": tail(&r.stderr, max), "test": r.test,
        "simulated": r.simulated,
    })
}

/// What `hook-runs` filters on; every field present must hold.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub hook: Option<String>,
    pub event: Option<String>,
    /// One of `OUTCOMES`.
    pub outcome: Option<String>,
}

impl Filter {
    pub fn holds(&self, row: &Value) -> bool {
        let is =
            |field: &str, want: &Option<String>| want.as_ref().is_none_or(|w| row[field] == **w);
        is("hook", &self.hook) && is("event", &self.event) && is("outcome", &self.outcome)
    }
}

/// The ledger of the session in `dir`, in file order, filtered by `f`, and the count of lines
/// skipped for not being a JSON object (a torn last line). A session without one has no rows.
pub fn read(dir: &Path, f: &Filter) -> std::io::Result<(Vec<Value>, usize)> {
    let path: PathBuf = dir.join(FILE);
    let text = match std::fs::read(&path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut skipped = 0;
    let mut rows = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(v) if v.is_object() => {
                if f.holds(&v) {
                    rows.push(v);
                }
            }
            _ => skipped += 1,
        }
    }
    Ok((rows, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(hook: &str, fire: bool) -> Decision {
        Decision {
            at: "2026-11-13T15:00:00.000Z".into(),
            hook: hook.into(),
            event: "record.stop".into(),
            session: Some("s".into()),
            event_seq: 4,
            dedup: "s:record.stop".into(),
            fire,
            reason: if fire {
                "no match filter: fires on every record.stop"
            } else {
                "the hook is off (enabled = false)"
            }
            .into(),
        }
    }

    #[test]
    fn rows_append_filter_and_skip_a_torn_line() {
        let tmp = tempfile::tempdir().unwrap();
        let l = Ledger::default();
        l.append("s", tmp.path(), &verdict_row(&decision("a", true), false));
        l.append("s", tmp.path(), &verdict_row(&decision("b", false), true));
        let path = tmp.path().join(FILE);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(r#"{"kind":"run","ho"#);
        std::fs::write(&path, text).unwrap();
        let (all, skipped) = read(tmp.path(), &Filter::default()).unwrap();
        assert_eq!((all.len(), skipped), (2, 1));
        assert_eq!(all[0]["outcome"], "fire");
        assert_eq!(all[1]["simulated"], true);
        let only = |f: Filter| read(tmp.path(), &f).unwrap().0.len();
        assert_eq!(
            only(Filter {
                outcome: Some("skip".into()),
                ..Filter::default()
            }),
            1
        );
        assert_eq!(
            only(Filter {
                hook: Some("a".into()),
                ..Filter::default()
            }),
            1
        );
        assert_eq!(
            only(Filter {
                event: Some("record.start".into()),
                ..Filter::default()
            }),
            0
        );
        // A missing directory is never created.
        let gone = tmp.path().join("gone");
        l.append("x", &gone, &verdict_row(&decision("a", true), false));
        assert!(!gone.exists());
        assert_eq!(read(&gone, &Filter::default()).unwrap(), (vec![], 0));
    }

    #[test]
    fn tails_cut_on_a_character_boundary() {
        assert_eq!(tail("hello", 3), "llo");
        assert_eq!(tail("hi", 10), "hi");
        assert_eq!(tail("aé", 1), "");
        assert_eq!(tail("aé", 2), "é");
        assert_eq!(tail("x", 0), "");
    }
}
