//! OpenOats on-disk compatibility (yazinsai/OpenOats, pinned at b4353cd7). The session store
//! already is the OpenOats layout (see `sessions`); this module holds what crosses over:
//!
//! - what OpenOats' strict Swift `Codable` decoders accept (`decodes_meta`, `decodes_record`), so
//!   our writes are checked against it, and `conform_*` to bring a session up to it on export;
//! - the `openoats/v1` markdown export (`MarkdownMeetingWriter.swift`, byte for byte):
//!   `<date>-<HHmm>-<kebab title>.md` with the `schema/title/date/duration/participants/recorder/
//!   engine/language/app/x_openoats_session` frontmatter and `[hh:mm:ss] **Speaker:** text` lines;
//! - template snapshots and `templates.json` ids (`MeetingTemplate`, `TemplateSnapshot`).
//!
//! Speakers: OpenOats stores `you`, `them`, `remote_N`. With `speakers.export = "openoats"` an
//! unnamed `remote_N` (our diarized system-audio speakers) is "Them" on export, as a two-sided
//! OpenOats meeting shows it, and our `room_N` is "You"; with `labels` they are the app's labels
//! (`crate::speakers`). A name in `speakerNames` wins either way. Fields only oats-rs knows live under `x_oats` (session.json, utterances)
//! or as `x_oats_*` frontmatter keys, which OpenOats ignores.

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::config::typed::Speakers;
use crate::config::{ConfigError, Result};
use crate::speakers::Labels;

/// The markdown export schema id OpenOats writes and reads back (`x_openoats_session`).
pub const MARKDOWN_SCHEMA: &str = "openoats/v1";
/// OpenOats' built-in "Generic" template (`TemplateStore.genericID`), the snapshot for notes that
/// came from no template (hand-written).
pub const GENERIC_TEMPLATE_ID: &str = "00000000-0000-0000-0000-000000000000";
/// `TemplateStore.builtInTemplates` ids share this prefix (…0000 to …0005).
const BUILTIN_ID_PREFIX: &str = "00000000-0000-0000-0000-00000000000";
/// Longest kebab-case title slug in an export file name and `app:` (the openoats/v1 spec).
pub const SLUG_MAX: usize = 60;
/// The SF Symbol OpenOats' Generic template shows; `MeetingTemplate.icon` is not optional.
pub const DEFAULT_ICON: &str = "doc.text";

/// A zone by IANA name (also `TZ`'s `:Area/City` and a zoneinfo path); empty or unknown falls back
/// to `TZ`, then the OS setting, then UTC.
pub fn zone_named(zone: &str) -> Tz {
    let parse = |z: &str| {
        let z = z.trim().trim_start_matches(':');
        let z = z.rsplit_once("zoneinfo/").map_or(z, |(_, n)| n);
        z.parse::<Tz>().ok()
    };
    parse(zone)
        .or_else(|| std::env::var("TZ").ok().as_deref().and_then(parse))
        .or_else(|| {
            iana_time_zone::get_timezone()
                .ok()
                .as_deref()
                .and_then(parse)
        })
        .unwrap_or(Tz::UTC)
}

// ---- what OpenOats decodes ------------------------------------------------------------------

/// Swift's `JSONDecoder.DateDecodingStrategy.iso8601`: internet date-time with no fractional
/// seconds, `Z` or a `±HH:MM` offset. Anything else fails the whole object.
pub fn swift_date_ok(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| r.into_iter().all(|i| b[i].is_ascii_digit());
    let base = b.len() >= 20
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19);
    let zone = match &b.get(19..) {
        Some([b'Z']) => true,
        Some([s, h1, h2, b':', m1, m2]) => {
            matches!(s, b'+' | b'-') && [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit())
        }
        _ => false,
    };
    base && zone && DateTime::parse_from_rfc3339(s).is_ok()
}

/// A date as Swift's `.iso8601` encoder writes it (UTC, whole seconds).
pub fn swift_date(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_date(v: Option<&Value>) -> Option<DateTime<Utc>> {
    v.and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn opt_ok(m: &Map<String, Value>, k: &str, ok: impl Fn(&Value) -> bool) -> bool {
    m.get(k).is_none_or(|v| v.is_null() || ok(v))
}

/// A `TemplateSnapshot` OpenOats decodes.
pub fn snapshot_ok(v: &Value) -> bool {
    v.get("id").and_then(Value::as_str).is_some_and(is_uuid)
        && ["name", "icon", "systemPrompt"]
            .iter()
            .all(|k| v.get(*k).is_some_and(Value::is_string))
}

/// Why OpenOats' `SessionMetadata` decoder would reject this `session.json`, if it would (then
/// OpenOats does not list the session at all).
pub fn decodes_meta(meta: &Value) -> std::result::Result<(), String> {
    let m = meta.as_object().ok_or("session.json is not an object")?;
    let str_ok = |v: &Value| v.is_string();
    let date_ok = |v: &Value| v.as_str().is_some_and(swift_date_ok);
    let checks: [(&str, bool); 12] = [
        ("id", m.get("id").is_some_and(Value::is_string)),
        ("startedAt", m.get("startedAt").is_some_and(date_ok)),
        ("endedAt", opt_ok(m, "endedAt", date_ok)),
        (
            "utteranceCount",
            m.get("utteranceCount")
                .is_some_and(|v| v.is_i64() || v.is_u64()),
        ),
        ("hasNotes", m.get("hasNotes").is_some_and(Value::is_boolean)),
        (
            "templateSnapshot",
            opt_ok(m, "templateSnapshot", snapshot_ok),
        ),
        ("title", opt_ok(m, "title", str_ok)),
        ("language", opt_ok(m, "language", str_ok)),
        ("engine", opt_ok(m, "engine", str_ok)),
        (
            "tags",
            opt_ok(m, "tags", |v| {
                v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
            }),
        ),
        (
            "speakerNames",
            opt_ok(m, "speakerNames", |v| {
                v.as_object()
                    .is_some_and(|o| o.values().all(Value::is_string))
            }),
        ),
        ("meetingApp", opt_ok(m, "meetingApp", str_ok)),
    ];
    match checks.iter().find(|(_, ok)| !ok) {
        Some((k, _)) => Err(format!(
            "session.json key {k:?} would not decode in OpenOats"
        )),
        None => Ok(()),
    }
}

/// Why OpenOats' `SessionRecord` decoder would skip this transcript line, if it would.
pub fn decodes_record(r: &Value) -> std::result::Result<(), String> {
    let ok = r.get("speaker").is_some_and(Value::is_string)
        && r.get("text").is_some_and(Value::is_string)
        && r.get("timestamp")
            .and_then(Value::as_str)
            .is_some_and(swift_date_ok)
        && r.get("refinedText")
            .is_none_or(|v| v.is_null() || v.is_string());
    if ok {
        Ok(())
    } else {
        Err(format!("utterance would not decode in OpenOats: {r}"))
    }
}

/// Rewrite dates OpenOats would reject (fractional seconds, other offsets) as it writes them, and
/// fill the keys its decoder requires. `utterances` is the transcript the count is taken from.
pub fn conform_meta(meta: &mut Map<String, Value>, id: &str, utterances: usize, has_notes: bool) {
    meta.insert("id".into(), json!(id));
    for k in ["startedAt", "endedAt"] {
        if let Some(t) = parse_date(meta.get(k)) {
            meta.insert(k.into(), json!(swift_date(t)));
        }
    }
    if !meta.get("utteranceCount").is_some_and(Value::is_u64) {
        meta.insert("utteranceCount".into(), json!(utterances));
    }
    if !meta.get("hasNotes").is_some_and(Value::is_boolean) {
        meta.insert("hasNotes".into(), json!(has_notes));
    }
    if meta.get("speakerNames").is_some_and(|v| !v.is_object()) {
        meta.remove("speakerNames");
    }
}

/// The same for one transcript line; `None` when it has no usable timestamp or text.
pub fn conform_record(r: &Value) -> Option<Value> {
    let mut r = r.as_object()?.clone();
    let t = parse_date(r.get("timestamp"))?;
    r.get("text")?.as_str()?;
    r.insert("timestamp".into(), json!(swift_date(t)));
    if !r.get("speaker").is_some_and(Value::is_string) {
        r.insert("speaker".into(), json!("them"));
    }
    Some(Value::Object(r))
}

// ---- templates -------------------------------------------------------------------------------

/// A stable OpenOats template id for a template that has none: an RFC 9562 v8 UUID from the name.
pub fn template_uuid(name: &str) -> String {
    let h = Sha256::digest(name.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let x: String = b.iter().map(|c| format!("{c:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &x[..8],
        &x[8..12],
        &x[12..16],
        &x[16..20],
        &x[20..]
    )
}

pub fn is_builtin_id(id: &str) -> bool {
    id.len() == BUILTIN_ID_PREFIX.len() + 1 && id.starts_with(BUILTIN_ID_PREFIX)
}

/// `TemplateSnapshot` (`{id, name, icon, systemPrompt}`) of a template.
pub fn snapshot(t: &crate::templates::Template) -> Value {
    json!({
        "id": t.openoats_id.clone().filter(|i| is_uuid(i)).unwrap_or_else(|| template_uuid(&t.name)),
        "name": t.name,
        "icon": t.icon.clone().filter(|i| !i.is_empty()).unwrap_or_else(|| DEFAULT_ICON.to_string()),
        "systemPrompt": t.system_prompt,
    })
}

/// The snapshot recorded for notes no template made.
pub fn generic_snapshot() -> Value {
    json!({"id": GENERIC_TEMPLATE_ID, "name": "Generic", "icon": DEFAULT_ICON, "systemPrompt": ""})
}

/// `notes.meta.json`: OpenOats shows `notes.md` only beside one that decodes.
pub fn notes_meta(snapshot: Value, generated_at: DateTime<Utc>) -> Value {
    json!({"templateSnapshot": snapshot, "generatedAt": swift_date(generated_at)})
}

pub fn notes_meta_ok(v: &Value) -> bool {
    v.get("templateSnapshot").is_some_and(snapshot_ok)
        && v.get("generatedAt")
            .and_then(Value::as_str)
            .is_some_and(swift_date_ok)
}

// ---- markdown export -------------------------------------------------------------------------

/// `MarkdownMeetingWriter.yamlQuote`.
pub fn yaml_quote(v: &str) -> String {
    let e = v
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    format!("\"{e}\"")
}

/// `MarkdownMeetingWriter.toKebabCase`: ASCII letters and digits, hyphens between, at most
/// `SLUG_MAX` characters; "meeting" when nothing is left.
pub fn kebab(input: &str) -> String {
    let max = SLUG_MAX;
    let mut r = String::new();
    for c in input.to_lowercase().chars().filter(char::is_ascii) {
        r.push(if c.is_ascii_alphanumeric() { c } else { '-' });
    }
    while r.contains("--") {
        r = r.replace("--", "-");
    }
    let mut r = r.trim_matches('-').to_string();
    if r.len() > max {
        r.truncate(max);
        r = r.trim_matches('-').to_string();
    }
    if r.is_empty() {
        "meeting".to_string()
    } else {
        r
    }
}

/// `MarkdownMeetingWriter.normalizeAppName`.
pub fn normalize_app(name: &str) -> String {
    let l = name.to_lowercase();
    let known = [
        ("zoom", "zoom"),
        ("teams", "teams"),
        ("facetime", "facetime"),
        ("slack", "slack"),
        ("discord", "discord"),
        ("webex", "webex"),
        ("whatsapp", "whatsapp"),
        ("tuple", "tuple"),
        ("around", "around"),
    ];
    if l.contains("meet") && l.contains("google") && !l.contains("zoom") && !l.contains("teams") {
        return "meet".to_string();
    }
    known
        .iter()
        .find(|(k, _)| l.contains(k))
        .map_or_else(|| kebab(&l), |(_, v)| v.to_string())
}

/// One transcript line as the writer sees it.
struct Line {
    at: DateTime<Utc>,
    speaker: String,
    text: String,
}

/// What OpenOats' `SessionRecord` decoder would keep, in file order (it skips lines it cannot
/// decode); text is `refinedText` when present, else `text`.
fn lines(records: &[Value]) -> Vec<Line> {
    records
        .iter()
        .filter_map(|r| {
            let at = parse_date(r.get("timestamp"))?;
            let text = r.get("text")?.as_str()?;
            let speaker = r.get("speaker")?.as_str()?;
            let text = r.get("refinedText").and_then(Value::as_str).unwrap_or(text);
            Some(Line {
                at,
                speaker: speaker.to_string(),
                text: text.to_string(),
            })
        })
        .collect()
}

/// `MarkdownMeetingWriter.Metadata` plus what the frontmatter needs from the caller.
pub struct Export<'a> {
    pub meta: &'a Map<String, Value>,
    /// The session's id (the directory name when `meta` has none).
    pub id: &'a str,
    pub records: &'a [Value],
    pub notes: Option<&'a str>,
    /// `recorder:`; empty leaves it out (as OpenOats does when the account has no full name).
    pub recorder: &'a str,
    /// The zone `date:` and the file name use (OpenOats: the system zone).
    pub tz: Tz,
    /// `[speakers]`: how speaker ids are named (`speakers.export`).
    pub speakers: &'a Speakers,
}

fn rounded_minutes(secs: f64) -> i64 {
    ((secs / 60.0).round() as i64).max(1)
}

fn hms(secs: f64) -> String {
    let t = secs.max(0.0).round() as i64;
    format!("{:02}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
}

fn local_iso(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl Export<'_> {
    fn started(&self, lines: &[Line]) -> DateTime<Utc> {
        parse_date(self.meta.get("startedAt"))
            .or_else(|| lines.first().map(|l| l.at))
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    }

    fn title(&self) -> Option<&str> {
        self.meta.get("title").and_then(Value::as_str)
    }

    fn names(&self) -> Option<&Map<String, Value>> {
        self.meta.get("speakerNames").and_then(Value::as_object)
    }

    /// `yyyy-MM-dd-HHmm-<kebab title>` in the export zone.
    pub fn base_name(&self) -> String {
        let started = self.started(&lines(self.records));
        let prefix = self
            .tz
            .from_utc_datetime(&started.naive_utc())
            .format("%Y-%m-%d-%H%M");
        format!("{prefix}-{}", kebab(self.title().unwrap_or("meeting")))
    }

    /// The whole file, `buildMarkdown`; an error when no line would decode (OpenOats writes nothing).
    pub fn markdown(&self) -> Result<String> {
        let lines = lines(self.records);
        if lines.is_empty() {
            return Err(ConfigError::new(
                "empty_transcript",
                format!("session '{}' has no transcript to export", self.id),
            ));
        }
        let started = self.started(&lines);
        let title = self.title().filter(|t| !t.is_empty()).unwrap_or("Meeting");
        let s = |k: &str| {
            self.meta
                .get(k)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        };
        let duration = match parse_date(self.meta.get("endedAt")) {
            Some(end) => rounded_minutes((end - started).num_milliseconds() as f64 / 1000.0),
            None => {
                let (a, b) = (lines[0].at, lines[lines.len() - 1].at);
                rounded_minutes((b - a).num_milliseconds() as f64 / 1000.0)
            }
        };
        let labels = Labels::new(
            self.speakers,
            self.names(),
            lines.iter().map(|l| l.speaker.as_str()),
        );
        let mut participants: Vec<String> = vec![];
        for l in &lines {
            let p = labels.export(&l.speaker);
            if !participants.contains(&p) {
                participants.push(p);
            }
        }
        let mut fm = vec![
            "---".to_string(),
            format!("schema: {MARKDOWN_SCHEMA}"),
            format!("title: {}", yaml_quote(title)),
            format!("date: {}", local_iso(started, self.tz)),
            format!("duration: {duration}"),
            "participants:".to_string(),
        ];
        fm.extend(participants.iter().map(|p| format!("  - {p}")));
        if !self.recorder.is_empty() {
            fm.push(format!("recorder: {}", yaml_quote(self.recorder)));
        }
        if let Some(e) = s("engine") {
            fm.push(format!("engine: {e}"));
        }
        if let Some(l) = s("language") {
            fm.push(format!("language: {l}"));
        }
        if let Some(a) = s("meetingApp") {
            fm.push(format!("app: {}", normalize_app(a)));
        }
        fm.push(format!("x_openoats_session: {}", yaml_quote(self.id)));
        let tags: Vec<String> = self
            .meta
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(yaml_quote)
            .collect();
        if !tags.is_empty() {
            fm.push(format!("x_oats_tags: [{}]", tags.join(", ")));
        }
        fm.push("---".to_string());

        let mut body = vec![format!("# {title}"), String::new()];
        if let Some(n) = self.notes.map(str::trim).filter(|n| !n.is_empty()) {
            body.extend([
                "## Notes".to_string(),
                String::new(),
                n.to_string(),
                String::new(),
            ]);
        }
        body.extend(["## Transcript".to_string(), String::new()]);
        let mut tl = vec![];
        for l in &lines {
            let off = (l.at - started).num_milliseconds() as f64 / 1000.0;
            tl.push(format!(
                "[{}] **{}:** {}",
                hms(off),
                labels.export(&l.speaker),
                l.text
            ));
            tl.push(String::new());
        }
        body.push(tl.join("\n"));
        Ok(format!("{}\n{}", fm.join("\n"), body.join("\n")))
    }

    /// Write into `dir`: over the export that already carries this session id (as OpenOats
    /// re-mirrors), else a new `<base>.md`, `<base>-2.md` ... Returns the file.
    pub fn write(&self, dir: &Path) -> Result<PathBuf> {
        let text = self.markdown()?;
        std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        let path = match existing(dir, self.id) {
            Some(p) => p,
            None => {
                let base = self.base_name();
                let taken = |n: &str| dir.join(format!("{n}.md")).exists() || dir.join(n).exists();
                let mut name = base.clone();
                let mut n = 2;
                while taken(&name) {
                    name = format!("{base}-{n}");
                    n += 1;
                }
                dir.join(format!("{name}.md"))
            }
        };
        let tmp = dir.join(format!(".export.md.tmp{}", std::process::id()));
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &path))
            .map_err(|e| io(&path, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(path)
    }
}

fn io(p: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{}: {e}", p.display()))
}

/// The `.md` in `dir` (or `<pkg>/notes.md`) whose frontmatter names session `id`.
fn existing(dir: &Path, id: &str) -> Option<PathBuf> {
    let needle = format!("x_openoats_session: {}", yaml_quote(id));
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    entries.sort();
    entries.into_iter().find_map(|p| {
        let md = if p.is_dir() {
            p.join("notes.md")
        } else if p.extension().is_some_and(|e| e == "md") {
            p
        } else {
            return None;
        };
        std::fs::read_to_string(&md)
            .ok()
            .filter(|t| t.contains(&needle))
            .map(|_| md)
    })
}

/// Default markdown export folder when `sessions.export_dir` is empty.
pub fn default_export_dir(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join("Documents/oats-meetings/recordings")
}

/// Default OpenOats data folder (`sessions/`, `templates.json`) when `sessions.openoats_dir` is
/// empty: where the macOS app keeps it.
pub fn default_openoats_dir(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let home = env("HOME")
        .or_else(|| env("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/OpenOats")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swift_dates() {
        assert!(swift_date_ok("2026-11-06T10:00:57Z"));
        assert!(swift_date_ok("2026-11-06T10:00:57-05:00"));
        assert!(!swift_date_ok("2026-11-06T10:00:57.120Z"));
        assert!(!swift_date_ok("2026-11-06 10:00:57Z"));
        assert!(!swift_date_ok("2026-13-06T10:00:57Z"));
        assert!(!swift_date_ok("2026-11-06T10:00:57"));
    }

    #[test]
    fn kebab_and_apps_match_the_swift_writer() {
        assert_eq!(kebab("Acme — Q4 sync!"), "acme-q4-sync");
        assert_eq!(kebab("日本"), "meeting");
        assert_eq!(kebab(&"a-".repeat(40)).len(), 59);
        assert_eq!(normalize_app("Microsoft Teams"), "teams");
        assert_eq!(normalize_app("Google Meet"), "meet");
        assert_eq!(normalize_app("My Call App"), "my-call-app");
        assert_eq!(yaml_quote("a \"b\"\\\n"), "\"a \\\"b\\\"\\\\\\n\"");
    }

    #[test]
    fn template_ids_are_stable_uuids() {
        let a = template_uuid("Sales");
        assert!(is_uuid(&a), "{a}");
        assert_eq!(a, template_uuid("Sales"));
        assert_ne!(a, template_uuid("Sales2"));
        assert_eq!(&a[14..15], "8");
        assert!(is_builtin_id(GENERIC_TEMPLATE_ID));
        assert!(is_builtin_id("00000000-0000-0000-0000-000000000005"));
        assert!(!is_builtin_id(&a));
    }

    #[test]
    fn speakers() {
        let flat = crate::config::resolve::defaults_flat();
        let cfg: crate::config::typed::Config =
            serde_json::from_value(crate::config::resolve::nest(&flat)).unwrap();
        let names = json!({"remote_2": "Dana", "you": "Me"});
        let ids = ["you", "them", "remote_1", "remote_2", "room_1"];
        let plain = Labels::new(&cfg.speakers, None, ids);
        assert_eq!(plain.export("you"), "You");
        assert_eq!(plain.export("them"), "Them");
        assert_eq!(plain.export("room_1"), "You");
        let l = Labels::new(&cfg.speakers, names.as_object(), ids);
        assert_eq!(l.export("remote_1"), "Them");
        assert_eq!(l.export("remote_2"), "Dana");
        assert_eq!(l.export("you"), "Me");
    }

    #[test]
    fn decoders_reject_what_swift_rejects() {
        let ok = json!({"id": "s", "startedAt": "2026-01-01T00:00:00Z", "utteranceCount": 0, "hasNotes": false});
        assert_eq!(decodes_meta(&ok), Ok(()));
        for bad in [
            json!({"id": "s", "startedAt": "2026-01-01T00:00:00.5Z", "utteranceCount": 0, "hasNotes": false}),
            json!({"id": "s", "startedAt": "2026-01-01T00:00:00Z", "hasNotes": false}),
            json!({"id": "s", "startedAt": "2026-01-01T00:00:00Z", "utteranceCount": 0, "hasNotes": false, "templateSnapshot": {"id": "x"}}),
            json!({"id": "s", "startedAt": "2026-01-01T00:00:00Z", "utteranceCount": 0, "hasNotes": false, "speakerNames": {"a": 1}}),
        ] {
            assert!(decodes_meta(&bad).is_err(), "{bad}");
        }
        let mut m = bad_meta();
        conform_meta(&mut m, "s", 3, true);
        assert_eq!(decodes_meta(&Value::Object(m.clone())), Ok(()), "{m:?}");
        assert_eq!(m["startedAt"], "2026-01-01T05:00:00Z");
        assert_eq!(m["utteranceCount"], 3);
        let r =
            json!({"speaker": "remote_1", "text": "x", "timestamp": "2026-01-01T00:00:01.250Z"});
        assert!(decodes_record(&r).is_err());
        assert_eq!(decodes_record(&conform_record(&r).unwrap()), Ok(()));
        assert!(conform_record(&json!({"text": "x"})).is_none());
    }

    fn bad_meta() -> Map<String, Value> {
        json!({"startedAt": "2026-01-01T00:00:00.123-05:00", "speakerNames": "x"})
            .as_object()
            .unwrap()
            .clone()
    }
}
