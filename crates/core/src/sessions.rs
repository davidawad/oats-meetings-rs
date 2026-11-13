//! Session store. The on-disk layout IS the OpenOats layout (yazinsai/OpenOats `SessionRepository`),
//! so `bin/meeting sessions …` reads it unchanged:
//!
//!   <root>/<id>/session.json            camelCase metadata (id, startedAt, endedAt, title,
//!                                       utteranceCount, hasNotes, engine, speakerNames)
//!   <root>/<id>/transcript.final.jsonl  one JSON object per utterance (batch re-transcription)
//!   <root>/<id>/transcript.live.jsonl   live transcript
//!
//!   <root>/<id>/notes.md, notes.meta.json  notes and the OpenOats `NotesMeta` it needs to show them
//!   <root>/<id>/events.jsonl            the raw event log (`events`: every bus message naming
//!                                       the session, `{seq, at, topic, data}`, append-only)
//!
//! Every file of a session lives in its directory, so trash, restore, delete, import and the
//! OpenOats copy carry `events.jsonl` with the rest (OpenOats ignores files it does not know).
//!
//! Keys only oats-rs writes live in one `x_oats` object in `session.json` (and in each utterance),
//! which the OpenOats reader ignores: `schemaVersion` (this store's layout version),
//! `transcribePending`, the notes provenance and the recording's audio facts. Older builds wrote
//! them at the top level; they are read from there too and moved on the next write. Unknown keys
//! written by OpenOats are preserved. `crate::openoats` holds the format checks and the markdown
//! export.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::typed::DeleteMode;
use crate::config::{ConfigError, Result};
use crate::openoats;

pub mod events;

pub const SCHEMA_VERSION: u64 = 1;
const TRANSCRIPTS: [(&str, &str); 2] = [
    ("final", "transcript.final.jsonl"),
    ("live", "transcript.live.jsonl"),
];
/// A recording saved as audio only while the model was missing; cleared once it is transcribed.
const PENDING: &str = "transcribePending";
/// The object holding the keys OpenOats does not know.
pub const X_OATS: &str = "x_oats";
/// Our keys older builds wrote at the top level of `session.json`.
const LEGACY_KEYS: [&str; 10] = [
    "schemaVersion",
    PENDING,
    "notesTemplate",
    "notesBackend",
    "notesModel",
    "notesEdited",
    "durationMs",
    "audioFile",
    "audioChannels",
    "audioRate",
];

/// One of our keys: under `x_oats`, else (written by an older build) at the top level.
pub fn ext<'a>(meta: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    meta.get(X_OATS)
        .and_then(|x| x.get(key))
        .or_else(|| meta.get(key).filter(|_| LEGACY_KEYS.contains(&key)))
}

fn ext_map(meta: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let x = meta.entry(X_OATS).or_insert_with(|| json!({}));
    if !x.is_object() {
        *x = json!({});
    }
    x.as_object_mut().expect("x_oats object")
}

pub fn set_ext(meta: &mut Map<String, Value>, key: &str, v: Value) {
    meta.remove(key);
    ext_map(meta).insert(key.into(), v);
}

pub fn remove_ext(meta: &mut Map<String, Value>, key: &str) -> Option<Value> {
    let top = LEGACY_KEYS
        .contains(&key)
        .then(|| meta.remove(key))
        .flatten();
    let x = meta
        .get_mut(X_OATS)
        .and_then(Value::as_object_mut)
        .and_then(|x| x.remove(key));
    x.or(top)
}

/// Move legacy top-level keys under `x_oats`.
fn normalize(meta: &mut Map<String, Value>) {
    for k in LEGACY_KEYS {
        if let Some(v) = meta.remove(k) {
            ext_map(meta).entry(k).or_insert(v);
        }
    }
}

fn io(what: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{}: {e}", what.display()))
}

/// Ids become directory names: no separators, no dot-prefix, no surprises.
pub fn valid_id(id: &str, max_len: usize) -> bool {
    !id.is_empty()
        && id.len() <= max_len
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn check_id(id: &str, max_len: usize) -> Result<()> {
    if valid_id(id, max_len) {
        Ok(())
    } else {
        Err(ConfigError::new(
            "invalid_argument",
            format!("invalid session id {id:?} (letters, digits, '_', '-', '.' only; at most {max_len} bytes, sessions.max_id_len)"),
        ))
    }
}

/// Default store location when `sessions.root` is empty.
pub fn default_sessions_dir(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(d) = env("OATS_DATA_DIR") {
        return PathBuf::from(d).join("sessions");
    }
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(&home).join("Library/Application Support")
    } else if cfg!(windows) {
        env("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&home))
    } else {
        env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&home).join(".local/share"))
    };
    base.join("oats-meetings-rs").join("sessions")
}

pub struct Store {
    pub root: PathBuf,
    /// Longest session id, bytes (`sessions.max_id_len`).
    pub max_id: usize,
    /// Context either side of a search match, chars (`sessions.search_snippet_chars`).
    pub snippet_chars: usize,
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
        Value::Object(meta) => Some(Loaded {
            dir: dir.to_path_buf(),
            meta,
        }),
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
        "schema_version": ext(&l.meta, "schemaVersion").cloned().unwrap_or(Value::Null),
        "path": l.dir,
    })
}

/// The words an utterance shows: the refined text when there is one, else the raw text.
fn utterance_text(u: &Value) -> String {
    ["refinedText", "text"]
        .iter()
        .filter_map(|k| u.get(*k).and_then(Value::as_str))
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default()
        .to_string()
}

/// Seconds into the recording: `x_oats.startSeconds` (older builds: top-level `startSeconds`, or
/// the live `start`), else the utterance timestamp less the session start; null when neither is known.
fn utterance_offset(u: &Value, started: Option<&str>) -> Value {
    if let Some(s) = u[X_OATS]["startSeconds"].as_f64().or_else(|| {
        ["startSeconds", "start"]
            .iter()
            .find_map(|k| u.get(*k).and_then(Value::as_f64))
    }) {
        return json!(s);
    }
    let parse = |v: &str| chrono::DateTime::parse_from_rfc3339(v).ok();
    match (
        u.get("timestamp").and_then(Value::as_str).and_then(parse),
        started.and_then(parse),
    ) {
        (Some(t), Some(s)) if t >= s => json!((t - s).num_milliseconds() as f64 / 1000.0),
        _ => Value::Null,
    }
}

/// Lower-cased one char per char, so positions in the result are positions in the original.
fn fold(s: &str) -> Vec<char> {
    s.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

fn find(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `ctx` characters either side of the match at char `at`, `…` where it was cut.
fn snippet(text: &str, at: usize, len: usize, ctx: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let from = at.saturating_sub(ctx);
    let to = (at + len).saturating_add(ctx).min(chars.len());
    let mut out: String = chars[from..to].iter().collect();
    out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if from > 0 {
        out.insert(0, '…');
    }
    if to < chars.len() {
        out.push('…');
    }
    out
}

/// Write `session.json` (atomically), our keys moved under `x_oats`.
pub(crate) fn write_meta(dir: &Path, meta: &Map<String, Value>) -> Result<()> {
    let path = dir.join("session.json");
    let tmp = dir.join(format!(".session.json.tmp{}", std::process::id()));
    let mut meta = meta.clone();
    normalize(&mut meta);
    let text = serde_json::to_string_pretty(&Value::Object(meta))
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
            ConfigError::new(
                "invalid_argument",
                format!("{flag} must be an RFC 3339 timestamp, got {v:?}"),
            )
        })?;
    }
    Ok(())
}

impl Store {
    /// A store with the default limits; the engine uses `with_max_id` and `with_snippet_chars` to
    /// apply the config.
    pub fn new(root: impl Into<PathBuf>) -> Store {
        let max_id = crate::config::keys::default_int("sessions.max_id_len") as usize;
        let snippet_chars =
            crate::config::keys::default_int("sessions.search_snippet_chars") as usize;
        Store {
            root: root.into(),
            max_id,
            snippet_chars,
        }
    }

    pub fn with_max_id(mut self, max_id: usize) -> Store {
        self.max_id = max_id;
        self
    }

    pub fn with_snippet_chars(mut self, snippet_chars: usize) -> Store {
        self.snippet_chars = snippet_chars;
        self
    }

    /// Another handle on the same directory with the same limits.
    pub fn sibling(&self) -> Store {
        Store {
            root: self.root.clone(),
            max_id: self.max_id,
            snippet_chars: self.snippet_chars,
        }
    }

    pub fn valid_id(&self, id: &str) -> bool {
        valid_id(id, self.max_id)
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
        check_id(id, self.max_id)?;
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

    /// The directory of session `id` when the id is valid and the directory exists (a session
    /// being recorded, before its `session.json` is complete, included).
    pub fn session_dir(&self, id: &str) -> Option<PathBuf> {
        let dir = self.root.join(id);
        (self.valid_id(id) && dir.is_dir()).then_some(dir)
    }

    /// `session-events`: the raw event log of session `id` (`events::read`), with its `session`.
    pub fn events(&self, id: &str, filter: &events::Filter) -> Result<Value> {
        let l = self.find(id)?;
        let mut v = events::read(&l.dir, filter).map_err(|e| io(&l.dir.join(events::FILE), e))?;
        v["session"] = json!(id_of(&l));
        Ok(v)
    }

    pub fn get(&self, id: &str) -> Result<Value> {
        let l = self.find(id)?;
        let mut v = summary(&l);
        v["metadata"] = Value::Object(l.meta.clone());
        Ok(v)
    }

    /// Session `id`'s `session.json` alone (no transcript read): cheap enough for a timer.
    pub fn meta(&self, id: &str) -> Result<Map<String, Value>> {
        self.find(id).map(|l| l.meta)
    }

    pub fn transcript(&self, id: &str) -> Result<Value> {
        let l = self.find(id)?;
        let (source, utterances) = transcript(&l.dir);
        let names = l.meta.get("speakerNames").cloned().unwrap_or(json!({}));
        Ok(json!({"id": id, "source": source, "utterances": utterances, "speaker_names": names}))
    }

    /// Utterances whose text holds every whitespace-separated word of `query` (case-insensitive),
    /// in session (directory) order then utterance order; at most `limit` hits. Each hit carries
    /// the session id and title, the utterance (`index`, `speaker`, `timestamp`, `text`), its
    /// `offset` in seconds into the recording (null when the utterance has no timing), the
    /// character position of the first word in the text (`match_at`) and a short `snippet`.
    pub fn search(&self, query: &str, limit: usize) -> Result<Value> {
        let words: Vec<Vec<char>> = query.split_whitespace().map(fold).collect();
        if words.is_empty() {
            return Err(ConfigError::new("invalid_argument", "query is empty"));
        }
        let mut hits = vec![];
        let mut truncated = false;
        'sessions: for l in self.discover() {
            let started = l.meta.get("startedAt").and_then(Value::as_str);
            let title = l.meta.get("title").and_then(Value::as_str).unwrap_or("");
            let (source, utterances) = transcript(&l.dir);
            for (index, u) in utterances.iter().enumerate() {
                let text = utterance_text(u);
                let hay = fold(&text);
                let Some((at, len)) = words
                    .iter()
                    .map(|w| find(&hay, w).map(|i| (i, w.len())))
                    .collect::<Option<Vec<_>>>()
                    .and_then(|v| v.into_iter().min())
                else {
                    continue;
                };
                if hits.len() == limit {
                    truncated = true;
                    break 'sessions;
                }
                hits.push(json!({
                    "session": id_of(&l),
                    "title": title,
                    "source": source,
                    "index": index,
                    "offset": utterance_offset(u, started),
                    "speaker": u.get("speaker").cloned().unwrap_or(Value::Null),
                    "timestamp": u.get("timestamp").cloned().unwrap_or(Value::Null),
                    "text": text,
                    "match_at": at,
                    "snippet": snippet(&text, at, len, self.snippet_chars),
                }));
            }
        }
        Ok(json!({"query": query, "count": hits.len(), "truncated": truncated, "hits": hits}))
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<Value> {
        let mut l = self.find(id)?;
        l.meta.insert("title".into(), json!(title));
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }

    /// Name `speaker` (a transcript label: `you`, `remote_1`, `remote_2` ...) of session `id` in
    /// its `speakerNames`, OpenOats' map; an empty `name` removes the entry.
    pub fn name_speaker(&self, id: &str, speaker: &str, name: &str) -> Result<Value> {
        if speaker.trim().is_empty() {
            return Err(ConfigError::new(
                "invalid_argument",
                "speaker must not be empty",
            ));
        }
        let mut l = self.find(id)?;
        let names = l.meta.entry("speakerNames").or_insert_with(|| json!({}));
        if !names.is_object() {
            *names = json!({});
        }
        let m = names.as_object_mut().expect("speakerNames object");
        match name.trim() {
            "" => m.remove(speaker.trim()),
            n => m.insert(speaker.trim().to_string(), json!(n)),
        };
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }

    pub fn tag(&self, id: &str, tag: &str, remove: bool) -> Result<Value> {
        if tag.trim().is_empty() {
            return Err(ConfigError::new(
                "invalid_argument",
                "tag must not be empty",
            ));
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
        stamp_schema(&mut l.meta);
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }

    /// Write `notes.md` into the session, its OpenOats `notes.meta.json` (the template snapshot
    /// and when), and how it was made in `session.json`.
    pub fn set_notes(
        &self,
        id: &str,
        notes: &str,
        template: &crate::templates::Template,
        backend: &str,
        model: &str,
    ) -> Result<PathBuf> {
        let mut l = self.find(id)?;
        let path = l.dir.join("notes.md");
        write_atomic(&path, notes.as_bytes())?;
        let snap = openoats::snapshot(template);
        write_notes_meta(
            &l.dir,
            openoats::notes_meta(snap.clone(), chrono::Utc::now()),
        )?;
        l.meta.insert("hasNotes".into(), json!(true));
        l.meta.insert("templateSnapshot".into(), snap);
        set_ext(&mut l.meta, "notesTemplate", json!(template.name));
        set_ext(&mut l.meta, "notesBackend", json!(backend));
        set_ext(&mut l.meta, "notesModel", json!(model));
        remove_ext(&mut l.meta, "notesEdited");
        stamp_schema(&mut l.meta);
        write_meta(&l.dir, &l.meta)?;
        Ok(path)
    }

    /// The session's `notes.md` (empty when there is none) and the provenance `set_notes` recorded.
    pub fn notes(&self, id: &str) -> Result<Value> {
        let l = self.find(id)?;
        let path = l.dir.join("notes.md");
        let (text, has) = match std::fs::read_to_string(&path) {
            Ok(t) => (t, true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
            Err(e) => return Err(io(&path, e)),
        };
        let m = |k: &str| ext(&l.meta, k).cloned().unwrap_or(Value::Null);
        // OpenOats records the template in notes.meta.json only.
        let template = ext(&l.meta, "notesTemplate").cloned().unwrap_or_else(|| {
            read_json(&l.dir.join("notes.meta.json"))
                .map(|v| v["templateSnapshot"]["name"].clone())
                .unwrap_or(Value::Null)
        });
        Ok(json!({
            "session": id_of(&l),
            "has_notes": has,
            "notes": text,
            "template": template,
            "backend": m("notesBackend"),
            "model": m("notesModel"),
            "edited": ext(&l.meta, "notesEdited").and_then(Value::as_bool).unwrap_or(false),
            "path": path,
        }))
    }

    /// Replace `notes.md` with hand-edited text, keeping how it was first generated.
    pub fn edit_notes(&self, id: &str, notes: &str) -> Result<Value> {
        let mut l = self.find(id)?;
        let path = l.dir.join("notes.md");
        write_atomic(&path, notes.as_bytes())?;
        // OpenOats shows notes.md only beside a notes.meta.json; keep the generated one's.
        let meta_path = l.dir.join("notes.meta.json");
        if !read_json(&meta_path).is_some_and(|v| openoats::notes_meta_ok(&v)) {
            write_notes_meta(
                &l.dir,
                openoats::notes_meta(openoats::generic_snapshot(), chrono::Utc::now()),
            )?;
        }
        l.meta.insert("hasNotes".into(), json!(true));
        set_ext(&mut l.meta, "notesEdited", json!(true));
        stamp_schema(&mut l.meta);
        write_meta(&l.dir, &l.meta)?;
        self.notes(id)
    }

    /// Callers enforce the `--confirm` policy. `Trash` moves the session directory to the OS
    /// trash (recoverable); `Remove` deletes it for good.
    pub fn delete(&self, id: &str, mode: DeleteMode) -> Result<Value> {
        let l = self.find(id)?;
        match mode {
            DeleteMode::Trash => trash_path(&l.dir).map_err(|e| {
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
        let mut plan: Vec<(
            String,
            Loaded,
            Vec<(String, Vec<u8>)>,
            Map<String, Value>,
            bool,
        )> = vec![];
        let mut conflicts = vec![];
        for src in sources {
            let id = id_of(&src);
            check_id(&id, self.max_id)?;
            let mut meta = src.meta.clone();
            meta.insert("id".into(), json!(id));
            normalize(&mut meta);
            stamp_schema(&mut meta);
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
                format!(
                    "sessions already present with different content: {}; use overwrite",
                    conflicts.join(", ")
                ),
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
                    let to = target.join(name);
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
                    }
                    std::fs::write(&to, bytes).map_err(|e| io(&to, e))?;
                }
            }
            write_meta(&target, &meta)?;
            imported.push(id);
        }
        Ok(json!({"imported": imported, "skipped": skipped}))
    }
}

/// The session's files by path relative to `dir` (`/`-separated): every regular file, in
/// subdirectories too (OpenOats keeps `attachments/`, `images/` and `audio/` there, and
/// `transcript.live.jsonl.pre-cleanup.bak` beside the transcript). Dot-files and symlinks are
/// skipped.
fn read_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
        for e in std::fs::read_dir(dir).map_err(|e| io(dir, e))?.flatten() {
            let ft = e.file_type().map_err(|e2| io(dir, e2))?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let rel = format!("{prefix}{name}");
            if ft.is_dir() {
                walk(&e.path(), &format!("{rel}/"), out)?;
            } else if ft.is_file() && rel != "session.json" {
                out.push((rel, std::fs::read(e.path()).map_err(|e2| io(dir, e2))?));
            }
        }
        Ok(())
    }
    let mut out = vec![];
    walk(dir, "", &mut out)?;
    out.sort();
    Ok(out)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| io(path, e))
}

fn write_notes_meta(dir: &Path, v: Value) -> Result<()> {
    let text = serde_json::to_string_pretty(&v)
        .map_err(|e| ConfigError::new("io_error", e.to_string()))?;
    write_atomic(&dir.join("notes.meta.json"), text.as_bytes())
}

fn stamp_schema(meta: &mut Map<String, Value>) {
    if ext(meta, "schemaVersion").is_none() {
        set_ext(meta, "schemaVersion", json!(SCHEMA_VERSION));
    }
}

/// Writing sessions (the transcription engine and, later, the recorder).
impl Store {
    /// Make an empty session directory with its `session.json`; `exists` if the id is taken.
    pub fn create(&self, id: &str, started_at: &str, title: &str, engine: &str) -> Result<PathBuf> {
        check_id(id, self.max_id)?;
        let dir = self.root.join(id);
        if dir.exists() {
            return Err(ConfigError::new(
                "exists",
                format!("session '{id}' already exists"),
            ));
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
        set_ext(&mut meta, "schemaVersion", json!(SCHEMA_VERSION));
        write_meta(&dir, &meta)?;
        Ok(dir)
    }

    fn transcript_path(&self, id: &str, kind: &str) -> Result<PathBuf> {
        let l = self.find(id)?;
        let file = TRANSCRIPTS
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, f)| *f)
            .ok_or_else(|| {
                ConfigError::new(
                    "invalid_argument",
                    "transcript kind must be 'live' or 'final'",
                )
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

    /// Drop the utterances of `live` or `final` that start at or after `cut_secs` into the
    /// recording (`x_oats.startSeconds`; one without timing is kept): the trailing silence an
    /// auto-stop cuts (dot-4uzgol.110). Returns how many it dropped; no file is no change.
    pub fn trim_transcript(&self, id: &str, kind: &str, cut_secs: f64) -> Result<usize> {
        let path = self.transcript_path(id, kind)?;
        if !path.is_file() {
            return Ok(0);
        }
        let all = read_jsonl(&path);
        let kept: Vec<Value> = all
            .iter()
            .filter(|u| {
                u["x_oats"]["startSeconds"]
                    .as_f64()
                    .is_none_or(|s| s < cut_secs)
            })
            .cloned()
            .collect();
        let dropped = all.len() - kept.len();
        if dropped > 0 {
            self.write_transcript(id, kind, &kept)?;
        }
        Ok(dropped)
    }

    /// Append one utterance line to `live` or `final`.
    pub fn append_utterance(&self, id: &str, kind: &str, utterance: &Value) -> Result<()> {
        use std::io::Write;
        let path = self.transcript_path(id, kind)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| io(&path, e))?;
        writeln!(f, "{utterance}").map_err(|e| io(&path, e))
    }

    /// Stamp `endedAt` and the utterance count (of the transcript the app reads first).
    pub fn finish(&self, id: &str, ended_at: &str) -> Result<Value> {
        let mut l = self.find(id)?;
        let (_, utterances) = transcript(&l.dir);
        l.meta.insert("endedAt".into(), json!(ended_at));
        l.meta
            .insert("utteranceCount".into(), json!(utterances.len()));
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }

    /// Merge `fields` into a session's `session.json`; a null value removes its key.
    pub fn merge_meta(&self, id: &str, fields: &Map<String, Value>) -> Result<Value> {
        let mut l = self.find(id)?;
        for (k, v) in fields {
            if v.is_null() {
                l.meta.remove(k);
            } else {
                l.meta.insert(k.clone(), v.clone());
            }
        }
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }

    /// Mark (or unmark) a session as recorded while transcription was unavailable, so it is
    /// transcribed once the model arrives (`transcribePending` in session.json).
    pub fn set_pending(&self, id: &str, pending: bool) -> Result<()> {
        let mut l = self.find(id)?;
        if pending {
            set_ext(&mut l.meta, PENDING, json!(true));
        } else if remove_ext(&mut l.meta, PENDING).is_none() {
            return Ok(());
        }
        write_meta(&l.dir, &l.meta)
    }

    /// Ids of the sessions marked by `set_pending`, in directory-name order.
    pub fn pending(&self) -> Vec<String> {
        self.discover()
            .iter()
            .filter(|l| ext(&l.meta, PENDING) == Some(&json!(true)))
            .map(id_of)
            .collect()
    }

    /// After transcribing an existing session in place: stamp the engine and utterance count and
    /// drop the pending mark; `endedAt` keeps the recording's end.
    pub fn transcribed(&self, id: &str, engine: &str) -> Result<Value> {
        let mut l = self.find(id)?;
        let (_, utterances) = transcript(&l.dir);
        l.meta.insert("engine".into(), json!(engine));
        l.meta
            .insert("utteranceCount".into(), json!(utterances.len()));
        remove_ext(&mut l.meta, PENDING);
        write_meta(&l.dir, &l.meta)?;
        Ok(summary(&l))
    }

    /// Set one of our keys (under `x_oats`) in `session.json`.
    pub fn set_ext_meta(&self, id: &str, key: &str, value: Value) -> Result<()> {
        let mut l = self.find(id)?;
        set_ext(&mut l.meta, key, value);
        write_meta(&l.dir, &l.meta)
    }

    /// Set one OpenOats `session.json` key (e.g. `language`).
    pub fn set_meta(&self, id: &str, key: &str, value: Value) -> Result<()> {
        let mut l = self.find(id)?;
        l.meta.insert(key.into(), value);
        write_meta(&l.dir, &l.meta)
    }
}

/// OpenOats interchange: the markdown export and a copy into an OpenOats store.
impl Store {
    /// Write the session's `openoats/v1` markdown export into `dir` (see `openoats::Export`).
    pub fn export_markdown(
        &self,
        id: &str,
        dir: &Path,
        recorder: &str,
        tz: chrono_tz::Tz,
        speakers: &crate::config::typed::Speakers,
    ) -> Result<Value> {
        let l = self.find(id)?;
        let (source, utterances) = transcript(&l.dir);
        let notes = std::fs::read_to_string(l.dir.join("notes.md")).ok();
        let sid = id_of(&l);
        let path = openoats::Export {
            meta: &l.meta,
            id: &sid,
            records: &utterances,
            notes: notes.as_deref(),
            recorder,
            tz,
            speakers,
        }
        .write(dir)?;
        Ok(json!({"session": sid, "path": path, "source": source, "utterances": utterances.len()}))
    }

    /// Copy session `id` into the OpenOats sessions folder `root`, made to decode there: dates
    /// rewritten as OpenOats writes them, its required keys filled in, utterances it could not
    /// read dropped, and a `notes.meta.json` beside any `notes.md`. A session already there with
    /// different content is a conflict unless `overwrite`.
    pub fn export_openoats(&self, id: &str, root: &Path, overwrite: bool) -> Result<Value> {
        let l = self.find(id)?;
        let sid = id_of(&l);
        // Staged in a dot-directory of `root` (OpenOats skips those), then imported from there.
        let stage = root.join(format!(".oats-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&stage);
        let out = stage.join(&sid);
        let r = self.stage_openoats(&l, &sid, &out).and_then(|_| {
            Store::new(root)
                .with_max_id(self.max_id)
                .import(&out, overwrite)
        });
        let _ = std::fs::remove_dir_all(&stage);
        let mut v = r?;
        v["path"] = json!(root.join(&sid));
        Ok(v)
    }

    fn stage_openoats(&self, l: &Loaded, sid: &str, out: &Path) -> Result<()> {
        std::fs::create_dir_all(out).map_err(|e| io(out, e))?;
        for (name, bytes) in read_files(&l.dir)? {
            let to = out.join(&name);
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p).map_err(|e| io(p, e))?;
            }
            let bytes = if TRANSCRIPTS.iter().any(|(_, f)| *f == name) {
                // A line OpenOats already decodes is kept as it is.
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter_map(|line| {
                        let r = serde_json::from_str::<Value>(line).ok()?;
                        match openoats::decodes_record(&r) {
                            Ok(()) => Some(format!("{line}\n")),
                            Err(_) => openoats::conform_record(&r).map(|r| format!("{r}\n")),
                        }
                    })
                    .collect::<String>()
                    .into_bytes()
            } else {
                bytes
            };
            std::fs::write(&to, bytes).map_err(|e| io(&to, e))?;
        }
        let mut meta = l.meta.clone();
        if meta
            .get("templateSnapshot")
            .is_some_and(|s| !s.is_null() && !openoats::snapshot_ok(s))
        {
            meta.remove("templateSnapshot");
        }
        let has_notes = out.join("notes.md").is_file();
        if has_notes
            && !read_json(&out.join("notes.meta.json")).is_some_and(|v| openoats::notes_meta_ok(&v))
        {
            let snap = meta
                .get("templateSnapshot")
                .filter(|s| openoats::snapshot_ok(s))
                .cloned()
                .unwrap_or_else(openoats::generic_snapshot);
            write_notes_meta(out, openoats::notes_meta(snap, chrono::Utc::now()))?;
        }
        if has_notes {
            meta.insert("hasNotes".into(), json!(true));
        }
        openoats::conform_meta(&mut meta, sid, transcript(out).1.len(), has_notes);
        openoats::decodes_meta(&Value::Object(meta.clone()))
            .map_err(|e| ConfigError::new("invalid_value", format!("session '{sid}': {e}")))?;
        write_meta(out, &meta)
    }
}

/// Sessions in the OS trash: what `delete` with `sessions.delete_mode = "trash"` moved there.
/// Listing, restoring and purging the trash needs the Freedesktop trash (Linux) or the Windows
/// recycle bin; macOS gives no API for it without Full Disk Access, so there it is `unsupported`.
impl Store {
    /// Trashed sessions whose original folder is this store, newest first.
    pub fn trash_list(&self) -> Result<Value> {
        let items: Vec<Value> = trashed(&self.root)?
            .iter()
            .map(|t| {
                json!({
                    "id": t.name.to_string_lossy(),
                    "title": trashed_title(t).unwrap_or_default(),
                    "deleted_at": deleted_at(t.time_deleted),
                    "original_path": t.original_path().display().to_string(),
                })
            })
            .collect();
        Ok(json!({"root": self.root.display().to_string(), "count": items.len(), "trash": items}))
    }

    /// Put a trashed session back (the most recently trashed copy). Fails when the id is live.
    pub fn trash_restore(&self, id: &str) -> Result<Value> {
        check_id(id, self.max_id)?;
        if self.root.join(id).exists() {
            return Err(ConfigError::new(
                "conflict",
                format!("session '{id}' already exists; delete or rename it first"),
            ));
        }
        let item = trashed(&self.root)?
            .into_iter()
            .find(|t| t.name.to_string_lossy() == id)
            .ok_or_else(|| {
                ConfigError::new("not_found", format!("no session '{id}' in the trash"))
            })?;
        os_trash::restore(vec![item])
            .map_err(|e| ConfigError::new("trash_failed", format!("restore {id}: {e}")))?;
        Ok(json!({"id": id, "restored": true}))
    }

    /// Delete trashed sessions for good: one id, or every session of this store. Callers enforce
    /// the `--confirm` policy. Returns the purged ids.
    pub fn trash_empty(&self, id: Option<&str>) -> Result<Value> {
        if let Some(id) = id {
            check_id(id, self.max_id)?;
        }
        let items: Vec<_> = trashed(&self.root)?
            .into_iter()
            .filter(|t| id.is_none_or(|id| t.name.to_string_lossy() == id))
            .collect();
        if let (Some(id), true) = (id, items.is_empty()) {
            return Err(ConfigError::new(
                "not_found",
                format!("no session '{id}' in the trash"),
            ));
        }
        let ids: Vec<String> = items
            .iter()
            .map(|t| t.name.to_string_lossy().into_owned())
            .collect();
        if !items.is_empty() {
            os_trash::purge(items)
                .map_err(|e| ConfigError::new("trash_failed", format!("empty trash: {e}")))?;
        }
        Ok(json!({"purged": ids, "count": ids.len()}))
    }
}

/// The `title` in a trashed session's `session.json`. Freedesktop ids are the `.trashinfo` path
/// (`<trash>/info/<name>.trashinfo`, the folder is `<trash>/files/<name>`); on Windows the id is
/// the item's own path.
fn trashed_title(t: &trash::TrashItem) -> Option<String> {
    let id = Path::new(&t.id);
    let in_files = id
        .file_name()
        .and_then(|n| n.to_str()?.strip_suffix(".trashinfo"))
        .zip(id.parent().and_then(Path::parent))
        .map(|(n, trash)| trash.join("files").join(n));
    let meta = [Some(id.to_path_buf()), in_files]
        .into_iter()
        .flatten()
        .find_map(|d| std::fs::read_to_string(d.join("session.json")).ok())?;
    let v: Value = serde_json::from_str(&meta).ok()?;
    Some(v["title"].as_str()?.to_string())
}

fn deleted_at(secs: i64) -> Value {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| json!(t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
        .unwrap_or(Value::Null)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    a == b || canon(a) == canon(b)
}

/// Whether this OS lets the app list, restore and purge the trash (`trash-list`). Where it is
/// false (macOS) `delete` still moves sessions to the trash, but only the Finder can put them back,
/// so the GUI hides its Trash tab there.
pub const TRASH_LISTING: bool = os_trash::SUPPORTED;

/// OS trash entries that came from `root`, newest first.
fn trashed(root: &Path) -> Result<Vec<trash::TrashItem>> {
    if !os_trash::SUPPORTED {
        return Err(ConfigError::new(
            "unsupported",
            "listing the OS trash is not available on this OS; use the Finder (sessions.delete_mode = \"trash\")",
        ));
    }
    let mut items: Vec<_> = os_trash::list()
        .map_err(|e| ConfigError::new("trash_failed", format!("list trash: {e}")))?
        .into_iter()
        .filter(|t| same_dir(&t.original_parent, root))
        .collect();
    items.sort_by(|a, b| {
        b.time_deleted
            .cmp(&a.time_deleted)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(items)
}

#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
mod os_trash {
    use trash::{os_limited, Error, TrashItem};

    pub const SUPPORTED: bool = true;

    pub fn list() -> std::result::Result<Vec<TrashItem>, Error> {
        os_limited::list()
    }
    pub fn restore(items: Vec<TrashItem>) -> std::result::Result<(), Error> {
        os_limited::restore_all(items)
    }
    pub fn purge(items: Vec<TrashItem>) -> std::result::Result<(), Error> {
        os_limited::purge_all(items)
    }
}

#[cfg(not(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
)))]
mod os_trash {
    use trash::{Error, TrashItem};

    pub const SUPPORTED: bool = false;

    fn unsupported() -> Error {
        Error::Unknown {
            description: "this OS gives no trash listing API (unsupported)".into(),
        }
    }
    pub fn list() -> std::result::Result<Vec<TrashItem>, Error> {
        Err(unsupported())
    }
    pub fn restore(_: Vec<TrashItem>) -> std::result::Result<(), Error> {
        Err(unsupported())
    }
    pub fn purge(_: Vec<TrashItem>) -> std::result::Result<(), Error> {
        Err(unsupported())
    }
}

/// Move `path` to the OS trash. On macOS use NSFileManager: the crate's default (Finder via
/// AppleScript) is slow and raises an Automation permission prompt.
#[cfg(target_os = "macos")]
fn trash_path(path: &std::path::Path) -> std::result::Result<(), trash::Error> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(path)
}

#[cfg(not(target_os = "macos"))]
fn trash_path(path: &std::path::Path) -> std::result::Result<(), trash::Error> {
    trash::delete(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/openoats")
    }

    fn fixture_ids() -> Vec<String> {
        vec![
            "session_2026-10-20_09-00-00".into(),
            "session_2026-10-21_15-00-00".into(),
        ]
    }

    fn ids(v: &Value) -> Vec<String> {
        v["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect()
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
    fn search_reads_the_refined_text_and_times_from_the_session_start() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("store"));
        s.import(&fixture_dir(), false).unwrap();
        let r = s.search("Renewal ok", 50).unwrap();
        assert_eq!(r["count"], 1, "{r}");
        let h = &r["hits"][0];
        assert_eq!(h["session"], "session_2026-10-20_09-00-00");
        assert_eq!(h["text"], "The renewal is OK.");
        assert_eq!(h["offset"], 60.0); // 13:01 in a session started at 13:00
        assert_eq!(
            (h["index"].as_u64(), h["match_at"].as_u64()),
            (Some(0), Some(4))
        );
        assert_eq!(h["snippet"], "The renewal is OK.");
        assert_eq!(s.search("nothing-said", 50).unwrap()["count"], 0);
        assert_eq!(
            snippet(&"a ".repeat(60), 60, 1, 40).chars().next(),
            Some('…')
        );
    }

    /// sessions.search_snippet_chars sets the context either side of the match.
    #[test]
    fn search_snippet_context_follows_the_config() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("store"));
        assert_eq!(s.snippet_chars, 40);
        s.import(&fixture_dir(), false).unwrap();
        let s = s.with_snippet_chars(4);
        let r = s.search("renewal", 50).unwrap();
        assert_eq!(r["hits"][0]["snippet"], "The renewal is…", "{r}");
        let r = s.with_snippet_chars(0).search("renewal", 50).unwrap();
        assert_eq!(r["hits"][0]["snippet"], "…renewal…", "{r}");
        assert_eq!(snippet("abc", 1, 1, usize::MAX), "abc");
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
        let meta: Value =
            serde_json::from_slice(&std::fs::read(dir.join("session.json")).unwrap()).unwrap();
        for k in [
            "id",
            "startedAt",
            "endedAt",
            "title",
            "utteranceCount",
            "hasNotes",
            "engine",
            "speakerNames",
        ] {
            assert!(meta.get(k).is_some(), "session.json lacks {k}");
        }
        assert_eq!(meta["x_oats"]["schemaVersion"], 1);
        assert!(
            meta.get("schemaVersion").is_none(),
            "our keys stay under x_oats"
        );
        assert_eq!(openoats::decodes_meta(&meta), Ok(()));
        assert_eq!(meta["speakerNames"]["remote_1"], "Dana");
        for f in ["transcript.final.jsonl", "transcript.live.jsonl"] {
            for line in std::fs::read_to_string(dir.join(f)).unwrap().lines() {
                let u: Value = serde_json::from_str(line).unwrap();
                assert!(
                    u.get("speaker").is_some()
                        && u.get("text").is_some()
                        && u.get("timestamp").is_some()
                );
            }
        }
        // Transcript read order: final, then live; an in-progress session falls back to live.
        assert_eq!(
            s.get(&fixture_ids()[0]).unwrap()["transcript_source"],
            "final"
        );
        assert_eq!(
            s.get(&fixture_ids()[1]).unwrap()["transcript_source"],
            "live"
        );
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
        assert_eq!(
            s.tag(id, "q4", false).unwrap()["tags"],
            json!(["sales", "q4"])
        );
        assert_eq!(s.tag(id, "sales", true).unwrap()["tags"], json!(["q4"]));
        // OpenOats' own keys survive edits.
        let g = s.get(id).unwrap();
        assert_eq!(g["metadata"]["engine"], "parakeet");
        assert_eq!(g["metadata"]["speakerNames"]["remote_1"], "Dana");
        // Diarized remote speakers are named by label; the existing names stay.
        let v = s.name_speaker(id, "remote_2", " Lee ").unwrap();
        assert_eq!(
            v["speaker_names"],
            json!({"remote_1": "Dana", "remote_2": "Lee"})
        );
        let v = s.name_speaker(id, "remote_1", "").unwrap();
        assert_eq!(v["speaker_names"], json!({"remote_2": "Lee"}));
        assert_eq!(
            s.name_speaker(id, " ", "x").unwrap_err().code,
            "invalid_argument"
        );
        assert_eq!(s.delete(id, DeleteMode::Remove).unwrap()["deleted"], true);
        assert_eq!(s.get(id).unwrap_err().code, "not_found");
        assert_eq!(
            ids(&s.list(None, None).unwrap()),
            vec![fixture_ids()[1].clone()]
        );
    }

    #[test]
    fn window_filter_and_bad_bounds() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        s.import(&fixture_dir(), false).unwrap();
        let r = s
            .list(
                Some("2026-10-21T15:00:00-04:00"),
                Some("2026-10-22T00:00:00Z"),
            )
            .unwrap();
        assert_eq!(ids(&r), vec![fixture_ids()[1].clone()]);
        assert_eq!(
            s.list(Some("yesterday"), None).unwrap_err().code,
            "invalid_argument"
        );
    }

    #[test]
    fn hostile_ids_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path());
        for bad in ["../x", "a/b", "", ".hidden", "a\\b", "x y"] {
            assert_eq!(s.get(bad).unwrap_err().code, "invalid_argument", "{bad}");
            assert_eq!(
                s.delete(bad, DeleteMode::Remove).unwrap_err().code,
                "invalid_argument",
                "{bad}"
            );
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
        assert_eq!(
            s.import(&d.path().join("src"), false).unwrap_err().code,
            "invalid_argument"
        );
        assert!(!d.path().join("escape").exists());
    }

    /// sessions.max_id_len at its boundary: exactly max is a valid id, one more is not.
    #[test]
    fn id_length_cap_boundary() {
        for max in [8usize, 200, 255] {
            assert!(valid_id(&"a".repeat(max), max), "{max}");
            assert!(!valid_id(&"a".repeat(max + 1), max), "{max}+1");
            assert!(!valid_id("", max));
        }
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path()).with_max_id(8);
        assert!(s
            .create("abcdefgh", "2026-01-01T00:00:00Z", "", "t")
            .is_ok());
        assert_eq!(
            s.create("abcdefghi", "2026-01-01T00:00:00Z", "", "t")
                .unwrap_err()
                .code,
            "invalid_argument"
        );
        assert!(s.sibling().valid_id("12345678") && !s.sibling().valid_id("123456789"));
    }
}
