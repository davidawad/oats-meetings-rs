//! Session store. The on-disk layout IS the OpenOats layout (yazinsai/OpenOats `SessionRepository`),
//! so `bin/meeting sessions …` reads it unchanged:
//!
//!   <root>/<id>/session.json            camelCase metadata (id, startedAt, endedAt, title,
//!                                       utteranceCount, hasNotes, engine, speakerNames)
//!   <root>/<id>/transcript.final.jsonl  one JSON object per utterance (batch re-transcription)
//!   <root>/<id>/transcript.live.jsonl   live transcript
//!
//! We add two keys to `session.json`, which the OpenOats reader ignores: `schemaVersion`
//! (this store's layout version) and `tags`. Unknown keys written by OpenOats are preserved.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::typed::DeleteMode;
use crate::config::{ConfigError, Result};

pub const SCHEMA_VERSION: u64 = 1;
const TRANSCRIPTS: [(&str, &str); 2] = [
    ("final", "transcript.final.jsonl"),
    ("live", "transcript.live.jsonl"),
];

fn io(what: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{}: {e}", what.display()))
}

/// Ids become directory names: no separators, no dot-prefix, no surprises.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn check_id(id: &str) -> Result<()> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(ConfigError::new(
            "invalid_argument",
            format!("invalid session id {id:?} (letters, digits, '_', '-', '.' only)"),
        ))
    }
}

/// Default store location when `sessions.root` is empty.
pub fn default_sessions_dir(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(d) = env("OATS_DATA_DIR") {
        return PathBuf::from(d).join("sessions");
    }
    let home = env("HOME").or_else(|| env("USERPROFILE")).unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(&home).join("Library/Application Support")
    } else if cfg!(windows) {
        env("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(&home))
    } else {
        env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&home).join(".local/share"))
    };
    base.join("oats-meetings-rs").join("sessions")
}

pub struct Store {
    pub root: PathBuf,
}

struct Loaded {
    dir: PathBuf,
    meta: Map<String, Value>,
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(Value::is_object)
        .collect()
}

/// (source, utterances): the first non-empty of final, then live (the app's read order).
fn transcript(dir: &Path) -> (Option<&'static str>, Vec<Value>) {
    TRANSCRIPTS
        .iter()
        .map(|(s, f)| (*s, read_jsonl(&dir.join(f))))
        .find(|(_, r)| !r.is_empty())
        .map_or((None, vec![]), |(s, r)| (Some(s), r))
}

fn load(dir: &Path) -> Option<Loaded> {
    let bytes = std::fs::read(dir.join("session.json")).ok()?;
    match serde_json::from_slice::<Value>(&bytes).ok()? {
        Value::Object(meta) => Some(Loaded { dir: dir.to_path_buf(), meta }),
        _ => None,
    }
}

fn id_of(l: &Loaded) -> String {
    l.meta
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| l.dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

fn summary(l: &Loaded) -> Value {
    let (source, utterances) = transcript(&l.dir);
    let s = |k: &str| l.meta.get(k).and_then(Value::as_str).map(str::to_string);
    json!({
        "id": id_of(l),
        "title": s("title").unwrap_or_default(),
        "started_at": s("startedAt"),
        "ended_at": s("endedAt"),
        "transcript_source": source,
        "speaker_names": l.meta.get("speakerNames").cloned().unwrap_or(Value::Null),
        "utterance_count": utterances.len(),
        "tags": l.meta.get("tags").cloned().unwrap_or_else(|| json!([])),
        "schema_version": l.meta.get("schemaVersion").cloned().unwrap_or(Value::Null),
        "path": l.dir,
    })
}

fn write_meta(dir: &Path, meta: &Map<String, Value>) -> Result<()> {
    let path = dir.join("session.json");
    let tmp = dir.join(format!(".session.json.tmp{}", std::process::id()));
    let text = serde_json::to_string_pretty(&Value::Object(meta.clone()))
        .map_err(|e| ConfigError::new("io_error", e.to_string()))?;
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| io(&path, e))
}

fn in_window(started: Option<&str>, from: Option<&str>, to: Option<&str>) -> bool {
    use chrono::DateTime;
    let Some(started) = started.and_then(|s| DateTime::parse_from_rfc3339(s).ok()) else {
        return from.is_none() && to.is_none();
    };
    let after = from
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .is_none_or(|b| started >= b);
    let before = to
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .is_none_or(|b| started <= b);
    after && before
}

fn parse_bound(v: Option<&str>, flag: &str) -> Result<()> {
    if let Some(v) = v {
        chrono::DateTime::parse_from_rfc3339(v).map_err(|_| {
            ConfigError::new("invalid_argument", format!("{flag} must be an RFC 3339 timestamp, got {v:?}"))
        })?;
    }
    Ok(())
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    /// Canonical sessions (a dir with a readable session.json) in directory-name order.
    fn discover(&self) -> Vec<Loaded> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return vec![];
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        dirs.iter().filter_map(|d| load(d)).collect()
    }

    fn find(&self, id: &str) -> Result<Loaded> {
        check_id(id)?;
        load(&self.root.join(id))
            .ok_or_else(|| ConfigError::new("not_found", format!("no session '{id}'")))
    }

    pub fn list(&self, from: Option<&str>, to: Option<&str>) -> Result<Value> {
        parse_bound(from, "from")?;
        parse_bound(to, "to")?;
        let sessions: Vec<Value> = self
            .discover()
            .iter()
            .filter(|l| in_window(l.meta.get("startedAt").and_then(Value::as_str), from, to))
            .map(summary)
            .collect();
        Ok(json!({"count": sessions.len(), "sessions": sessions}))
    }

    pub fn get(&self, id: &str) -> Result<Value> {
        let l = self.find(id)?;
        let mut v = summary(&l);
        v["metadata"] = Value::Object(l.meta.clone());
        Ok(v)
    }

    pub fn transcript(&self, id: &str) -> Result<Value> {
        let l = self.find(id)?;
        let (source, utterances) = transcript(&l.dir);
        Ok(json!({"id": id, "source": source, "utterances": utterances}))
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<Value> {
        let mut l = self.find(id)?;
        l.meta.insert("title".into(), json!(title));
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }

    pub fn tag(&self, id: &str, tag: &str, remove: bool) -> Result<Value> {
        if tag.trim().is_empty() {
            return Err(ConfigError::new("invalid_argument", "tag must not be empty"));
        }
        let mut l = self.find(id)?;
        let mut tags: Vec<Value> = l
            .meta
            .get("tags")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let present = tags.iter().any(|t| t == tag);
        if remove {
            tags.retain(|t| t != tag);
        } else if !present {
            tags.push(json!(tag));
        }
        l.meta.insert("tags".into(), Value::Array(tags));
        l.meta.entry("schemaVersion").or_insert(json!(SCHEMA_VERSION));
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }

    /// Callers enforce the `--confirm` policy. `Trash` moves the session directory to the OS
    /// trash (recoverable); `Remove` deletes it for good.
    pub fn delete(&self, id: &str, mode: DeleteMode) -> Result<Value> {
        let l = self.find(id)?;
        match mode {
            DeleteMode::Trash => trash::delete(&l.dir).map_err(|e| {
                ConfigError::new("trash_failed", format!("{}: {e}", l.dir.display()))
            })?,
            DeleteMode::Remove => std::fs::remove_dir_all(&l.dir).map_err(|e| io(&l.dir, e))?,
        }
        let mode = match mode {
            DeleteMode::Trash => "trash",
            DeleteMode::Remove => "remove",
        };
        Ok(json!({"id": id, "deleted": true, "mode": mode}))
    }

    /// Import from an OpenOats sessions root, or a single session directory. All-or-nothing:
    /// any conflicting id (present here with different content) aborts before anything is
    /// written, unless `overwrite`. Re-importing identical sessions is a no-op (`skipped`).
    pub fn import(&self, from: &Path, overwrite: bool) -> Result<Value> {
        let sources: Vec<Loaded> = if from.join("session.json").is_file() {
            load(from).into_iter().collect()
        } else {
            Store::new(from).discover()
        };
        if sources.is_empty() {
            return Err(ConfigError::new(
                "not_found",
                format!("no sessions under {}", from.display()),
            ));
        }
        // Plan.
        let mut plan: Vec<(String, Loaded, Vec<(String, Vec<u8>)>, Map<String, Value>, bool)> = vec![];
        let mut conflicts = vec![];
        for src in sources {
            let id = id_of(&src);
            check_id(&id)?;
            let mut meta = src.meta.clone();
            meta.insert("id".into(), json!(id));
            meta.entry("schemaVersion").or_insert(json!(SCHEMA_VERSION));
            let files = read_files(&src.dir)?;
            let target = self.root.join(&id);
            let existing = load(&target);
            let same = existing.as_ref().is_some_and(|e| {
                e.meta == meta && read_files(&target).map(|f| f == files).unwrap_or(false)
            });
            if existing.is_some() && !same && !overwrite {
                conflicts.push(id.clone());
            }
            plan.push((id, src, files, meta, same));
        }
        if !conflicts.is_empty() {
            return Err(ConfigError::new(
                "conflict",
                format!("sessions already present with different content: {}; use overwrite", conflicts.join(", ")),
            ));
        }
        let (mut imported, mut skipped) = (vec![], vec![]);
        for (id, _src, files, meta, same) in plan {
            if same {
                skipped.push(id);
                continue;
            }
            let target = self.root.join(&id);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| io(&target, e))?;
            }
            std::fs::create_dir_all(&target).map_err(|e| io(&target, e))?;
            for (name, bytes) in &files {
                if name != "session.json" {
                    std::fs::write(target.join(name), bytes).map_err(|e| io(&target, e))?;
                }
            }
            write_meta(&target, &meta)?;
            imported.push(id);
        }
        Ok(json!({"imported": imported, "skipped": skipped}))
    }
}

/// Regular files directly in the session dir (symlinks and subdirectories are not followed).
fn read_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).map_err(|e| io(dir, e))?.flatten() {
        let ft = e.file_type().map_err(|e2| io(dir, e2))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if ft.is_file() && !name.starts_with('.') && name != "session.json" {
            out.push((name, std::fs::read(e.path()).map_err(|e2| io(dir, e2))?));
        }
    }
    out.sort();
    Ok(out)
}

/// Writing sessions (the transcription engine and, later, the recorder).
impl Store {
    /// Make an empty session directory with its `session.json`; `exists` if the id is taken.
    pub fn create(&self, id: &str, started_at: &str, title: &str, engine: &str) -> Result<PathBuf> {
        check_id(id)?;
        let dir = self.root.join(id);
        if dir.exists() {
            return Err(ConfigError::new("exists", format!("session '{id}' already exists")));
        }
        std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
        let mut meta = Map::new();
        meta.insert("id".into(), json!(id));
        meta.insert("startedAt".into(), json!(started_at));
        meta.insert("title".into(), json!(title));
        meta.insert("utteranceCount".into(), json!(0));
        meta.insert("hasNotes".into(), json!(false));
        meta.insert("engine".into(), json!(engine));
        meta.insert("speakerNames".into(), json!({}));
        meta.insert("schemaVersion".into(), json!(SCHEMA_VERSION));
        write_meta(&dir, &meta)?;
        Ok(dir)
    }

    fn transcript_path(&self, id: &str, kind: &str) -> Result<PathBuf> {
        let l = self.find(id)?;
        let file = TRANSCRIPTS.iter().find(|(k, _)| *k == kind).map(|(_, f)| *f).ok_or_else(|| {
            ConfigError::new("invalid_argument", "transcript kind must be 'live' or 'final'")
        })?;
        Ok(l.dir.join(file))
    }

    /// Replace one transcript file (`live` or `final`) with `utterances`, one JSON object per line.
    pub fn write_transcript(&self, id: &str, kind: &str, utterances: &[Value]) -> Result<()> {
        let path = self.transcript_path(id, kind)?;
        let mut text = String::new();
        for u in utterances {
            text.push_str(&u.to_string());
            text.push('\n');
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .map_err(|e| io(&path, e))
    }

    /// Append one utterance line to `live` or `final`.
    pub fn append_utterance(&self, id: &str, kind: &str, utterance: &Value) -> Result<()> {
        use std::io::Write;
        let path = self.transcript_path(id, kind)?;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| io(&path, e))?;
        writeln!(f, "{utterance}").map_err(|e| io(&path, e))
    }

    /// Stamp `endedAt` and the utterance count (of the transcript the app reads first).
    pub fn finish(&self, id: &str, ended_at: &str) -> Result<Value> {
        let mut l = self.find(id)?;
        let (_, utterances) = transcript(&l.dir);
        l.meta.insert("endedAt".into(), json!(ended_at));
        l.meta.insert("utteranceCount".into(), json!(utterances.len()));
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/openoats")
    }

    fn fixture_ids() -> Vec<String> {
        vec!["session_2026-10-20_09-00-00".into(), "session_2026-10-21_15-00-00".into()]
    }

    fn ids(v: &Value) -> Vec<String> {
        v["sessions"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn import_then_list_equals_fixture_ids() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("store"));
        let r = s.import(&fixture_dir(), false).unwrap();
        assert_eq!(r["imported"], json!(fixture_ids()));
        assert_eq!(ids(&s.list(None, None).unwrap()), fixture_ids());
    }

    #[test]
    fn import_is_idempotent_and_conflicts_are_all_or_nothing() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("store"));
        s.import(&fixture_dir(), false).unwrap();
        let again = s.import(&fixture_dir(), false).unwrap();
        assert_eq!(again["imported"], json!([]));
        assert_eq!(again["skipped"], json!(fixture_ids()));
        s.rename(&fixture_ids()[0], "Changed").unwrap();
        let e = s.import(&fixture_dir(), false).unwrap_err();
        assert_eq!(e.code, "conflict");
        // overwrite restores the original title
        s.import(&fixture_dir(), true).unwrap();
        assert_eq!(s.get(&fixture_ids()[0]).unwrap()["title"], "Acme sync");
    }

    #[test]
    fn layout_matches_openoats_so_the_meeting_facade_reads_it() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        s.import(&fixture_dir(), false).unwrap();
        let dir = d.path().join(&fixture_ids()[0]);
        let meta: Value = serde_json::from_slice(&std::fs::read(dir.join("session.json")).unwrap()).unwrap();
        for k in ["id", "startedAt", "endedAt", "title", "utteranceCount", "hasNotes", "engine", "speakerNames"] {
            assert!(meta.get(k).is_some(), "session.json lacks {k}");
        }
        assert_eq!(meta["schemaVersion"], 1);
        assert_eq!(meta["speakerNames"]["remote_1"], "Dana");
        for f in ["transcript.final.jsonl", "transcript.live.jsonl"] {
            for line in std::fs::read_to_string(dir.join(f)).unwrap().lines() {
                let u: Value = serde_json::from_str(line).unwrap();
                assert!(u.get("speaker").is_some() && u.get("text").is_some() && u.get("timestamp").is_some());
            }
        }
        // Transcript read order: final, then live; an in-progress session falls back to live.
        assert_eq!(s.get(&fixture_ids()[0]).unwrap()["transcript_source"], "final");
        assert_eq!(s.get(&fixture_ids()[1]).unwrap()["transcript_source"], "live");
        // Non-canonical dirs are ignored.
        std::fs::create_dir_all(d.path().join("attachments-only")).unwrap();
        assert_eq!(ids(&s.list(None, None).unwrap()), fixture_ids());
    }

    #[test]
    fn rename_tag_untag_delete() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        s.import(&fixture_dir(), false).unwrap();
        let id = &fixture_ids()[0];
        assert_eq!(s.rename(id, "New title").unwrap()["title"], "New title");
        assert_eq!(s.tag(id, "sales", false).unwrap()["tags"], json!(["sales"]));
        assert_eq!(s.tag(id, "sales", false).unwrap()["tags"], json!(["sales"]));
        assert_eq!(s.tag(id, "q4", false).unwrap()["tags"], json!(["sales", "q4"]));
        assert_eq!(s.tag(id, "sales", true).unwrap()["tags"], json!(["q4"]));
        // OpenOats' own keys survive edits.
        let g = s.get(id).unwrap();
        assert_eq!(g["metadata"]["engine"], "parakeet");
        assert_eq!(g["metadata"]["speakerNames"]["remote_1"], "Dana");
        assert_eq!(s.delete(id, DeleteMode::Remove).unwrap()["deleted"], true);
        assert_eq!(s.get(id).unwrap_err().code, "not_found");
        assert_eq!(ids(&s.list(None, None).unwrap()), vec![fixture_ids()[1].clone()]);
    }

    #[test]
    fn window_filter_and_bad_bounds() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        s.import(&fixture_dir(), false).unwrap();
        let r = s.list(Some("2026-10-21T15:00:00-04:00"), Some("2026-10-22T00:00:00Z")).unwrap();
        assert_eq!(ids(&r), vec![fixture_ids()[1].clone()]);
        assert_eq!(s.list(Some("yesterday"), None).unwrap_err().code, "invalid_argument");
    }

    #[test]
    fn hostile_ids_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        for bad in ["../x", "a/b", "", ".hidden", "a\\b", "x y"] {
            assert_eq!(s.get(bad).unwrap_err().code, "invalid_argument", "{bad}");
            assert_eq!(s.delete(bad, DeleteMode::Remove).unwrap_err().code, "invalid_argument", "{bad}");
        }
        assert_eq!(s.get("nope").unwrap_err().code, "not_found");
    }

    #[test]
    fn missing_store_is_an_empty_list() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("absent"));
        assert_eq!(s.list(None, None).unwrap()["count"], 0);
    }

    #[test]
    fn import_of_a_hostile_id_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src/evil");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("session.json"), r#"{"id":"../escape"}"#).unwrap();
        let s = Store::new(d.path().join("store"));
        assert_eq!(s.import(&d.path().join("src"), false).unwrap_err().code, "invalid_argument");
        assert!(!d.path().join("escape").exists());
    }
}
