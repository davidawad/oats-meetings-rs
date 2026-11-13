//! The raw per-session event log (docs/design/hooks.md L0): `<session>/events.jsonl`, one
//! `{seq, at, topic, data}` line per bus message that names the session (`session` or `id`), and
//! per hook event an action fires itself (`notes.generated`, logged under its event name).
//!
//! `seq` is the hook envelope's `seq`: this log is the counter. It is kept per session, behind
//! that session's own lock (the map of sessions is held only to find it), so a session's lines
//! are written in `seq` order by one writer and no lock is shared across sessions. A line is one
//! `write` of the whole JSON object and its newline, opened in append mode; a crash mid-write can
//! leave only a torn last line, which readers skip and the next run of the app cuts off before it
//! appends. The first message of a session in a run of the app continues from the last `seq` in
//! its log, so `seq` stays monotonic across restarts.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

/// The log's file name in the session directory.
pub const FILE: &str = "events.jsonl";

#[derive(Default)]
struct Counter {
    /// The last `seq` handed out (0: none yet).
    seq: u64,
    /// The log on disk has been read for its last `seq` (and a torn tail cut off).
    seeded: bool,
}

/// Every session's `seq` counter and log writer.
#[derive(Default)]
pub struct Log {
    sessions: Mutex<HashMap<String, Arc<Mutex<Counter>>>>,
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

impl Log {
    fn counter(&self, session: &str) -> Arc<Mutex<Counter>> {
        let mut map = self.sessions.lock().expect("event log sessions");
        Arc::clone(map.entry(session.to_string()).or_default())
    }

    /// Count one message of `session` and return its `seq` (1 for the first). With `dir` (the
    /// session's directory, when it exists) the message is appended to its `events.jsonl` as
    /// `{seq, at, topic, data}`. A write that fails is logged; the `seq` is still used.
    pub fn record(&self, session: &str, dir: Option<&Path>, topic: &str, data: &Value) -> u64 {
        let counter = self.counter(session);
        let mut c = counter.lock().expect("event log session");
        let Some(dir) = dir.filter(|d| d.is_dir()) else {
            c.seq += 1;
            return c.seq;
        };
        let path = dir.join(FILE);
        if !c.seeded {
            c.seeded = true;
            c.seq = c.seq.max(seed(&path));
        }
        c.seq += 1;
        let line = json!({"seq": c.seq, "at": now_stamp(), "topic": topic, "data": data});
        if let Err(e) = append(&path, &line) {
            crate::elog!("oats-meetings-rs: {}: {e}", path.display());
        }
        c.seq
    }

    /// The last `seq` handed out for `session` (0 before its first message in this run).
    pub fn current(&self, session: &str) -> u64 {
        self.counter(session).lock().expect("event log session").seq
    }
}

fn append(path: &Path, line: &Value) -> std::io::Result<()> {
    let mut text = line.to_string();
    text.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// The last `seq` in the log at `path` (0 for none). A torn last line (a crash mid-write) is cut
/// off so the next line starts on a line of its own.
fn seed(path: &Path) -> u64 {
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    let whole = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if whole < bytes.len() {
        let cut = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|f| f.set_len(whole as u64));
        if let Err(e) = cut {
            crate::elog!(
                "oats-meetings-rs: {}: cannot cut a torn line: {e}",
                path.display()
            );
        }
    }
    String::from_utf8_lossy(&bytes[..whole])
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["seq"].as_u64())
        .max()
        .unwrap_or(0)
}

/// What `session-events` filters on.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    /// Only lines on this topic.
    pub topic: Option<String>,
    /// Only lines with `from <= seq <= to`.
    pub from: Option<u64>,
    pub to: Option<u64>,
}

/// The log of the session in `dir`: `{path, count, events, skipped}`, the lines in file order
/// filtered by `f`. A line that is not a JSON object (a torn last line) is skipped and counted.
/// A session without a log has none: `count` 0.
pub fn read(dir: &Path, f: &Filter) -> std::io::Result<Value> {
    let path: PathBuf = dir.join(FILE);
    let text = match std::fs::read(&path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut skipped = 0;
    let mut events = vec![];
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Some(v) = serde_json::from_str::<Value>(line)
            .ok()
            .filter(|v| v.is_object() && v["seq"].is_u64())
        else {
            skipped += 1;
            continue;
        };
        let seq = v["seq"].as_u64().unwrap_or_default();
        if f.topic.as_deref().is_some_and(|t| v["topic"] != t)
            || f.from.is_some_and(|from| seq < from)
            || f.to.is_some_and(|to| seq > to)
        {
            continue;
        }
        events.push(v);
    }
    Ok(json!({"path": path, "count": events.len(), "events": events, "skipped": skipped}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seqs(v: &Value) -> Vec<u64> {
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect()
    }

    #[test]
    fn a_recording_shaped_sequence_is_an_ordered_gap_free_log() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let log = Log::default();
        let msgs = [
            (
                "recording-started",
                json!({"session": "a", "started_at": "t"}),
            ),
            (
                "segment",
                json!({"session": "a", "kind": "live", "text": "hello"}),
            ),
            ("recording-paused", json!({"session": "a", "paused": true})),
            (
                "segment",
                json!({"session": "a", "kind": "live", "text": "pricing"}),
            ),
            ("recording-stopped", json!({"session": "a"})),
            ("session-saved", json!({"id": "a", "change": "record"})),
        ];
        for (i, (topic, data)) in msgs.iter().enumerate() {
            assert_eq!(log.record("a", Some(&a), topic, data), i as u64 + 1);
            // Another session's messages interleave without touching a's counter.
            assert_eq!(log.record("b", Some(&b), "segment", data), i as u64 + 1);
        }
        assert_eq!(log.current("a"), msgs.len() as u64);
        let v = read(&a, &Filter::default()).unwrap();
        assert_eq!(seqs(&v), (1..=msgs.len() as u64).collect::<Vec<_>>());
        assert_eq!(v["skipped"], 0);
        for (e, (topic, data)) in v["events"].as_array().unwrap().iter().zip(&msgs) {
            assert_eq!(e["topic"], *topic);
            assert_eq!(e["data"], *data);
            assert!(chrono::DateTime::parse_from_rfc3339(e["at"].as_str().unwrap()).is_ok());
        }
        // Filters.
        let only = |topic: Option<&str>, from, to| {
            seqs(
                &read(
                    &a,
                    &Filter {
                        topic: topic.map(str::to_string),
                        from,
                        to,
                    },
                )
                .unwrap(),
            )
        };
        assert_eq!(only(Some("segment"), None, None), vec![2, 4]);
        assert_eq!(only(None, Some(3), Some(5)), vec![3, 4, 5]);
        assert_eq!(only(Some("segment"), Some(3), None), vec![4]);
    }

    #[test]
    fn concurrent_publishers_keep_one_session_ordered_and_gap_free() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let log = Arc::new(Log::default());
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let (log, dir) = (Arc::clone(&log), dir.clone());
                std::thread::spawn(move || {
                    for i in 0..50 {
                        log.record("s", Some(&dir), "segment", &json!({"t": t, "i": i}));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let v = read(&dir, &Filter::default()).unwrap();
        assert_eq!(seqs(&v), (1..=200).collect::<Vec<_>>());
    }

    #[test]
    fn no_directory_counts_but_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("deleted");
        let log = Log::default();
        assert_eq!(log.record("x", None, "quit", &json!({})), 1);
        assert_eq!(
            log.record("x", Some(&gone), "session-deleted", &json!({})),
            2
        );
        assert!(
            !gone.exists(),
            "a missing session directory is never created"
        );
    }

    #[test]
    fn a_crash_mid_write_leaves_valid_jsonl_up_to_the_last_full_line() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let log = Log::default();
        for i in 0..3 {
            log.record("s", Some(dir), "segment", &json!({"i": i}));
        }
        // A crash mid-write: half a line, no newline.
        let path = dir.join(FILE);
        let full = std::fs::read_to_string(&path).unwrap();
        let torn = r#"{"seq":4,"at":"2026-11-13T15:04:05.120Z","topic":"segm"#;
        std::fs::write(&path, format!("{full}{torn}")).unwrap();
        // Every full line still parses; the torn one is skipped and counted.
        let v = read(dir, &Filter::default()).unwrap();
        assert_eq!(seqs(&v), vec![1, 2, 3]);
        assert_eq!(v["skipped"], 1);
        for line in full.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
        // The next run cuts the torn line off and continues the counter after the last full one.
        let next = Log::default();
        assert_eq!(
            next.record("s", Some(dir), "recording-stopped", &json!({})),
            4
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with('\n'));
        for line in text.lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
        let v = read(dir, &Filter::default()).unwrap();
        assert_eq!(seqs(&v), vec![1, 2, 3, 4]);
        assert_eq!(v["skipped"], 0);
    }

    #[test]
    fn a_session_without_a_log_reads_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let v = read(tmp.path(), &Filter::default()).unwrap();
        assert_eq!(v["count"], 0);
        assert_eq!(v["events"], json!([]));
    }
}
