//! The single command grammar. Every user action is a variant of `Action`; menus, keybindings,
//! the tray and the control API all dispatch it. The manifest (`ctl list-actions`,
//! `config/actions.json`) is generated from this same declaration, so they cannot drift.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const fn is_option(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() >= 6
        && b[0] == b'O'
        && b[1] == b'p'
        && b[2] == b't'
        && b[3] == b'i'
        && b[4] == b'o'
        && b[5] == b'n'
}

macro_rules! actions {
    ($(
        $name:ident, $wire:literal, $doc:literal, $status:literal, $effect:literal,
        { $( $f:ident : $t:ty ),* $(,)? };
    )*) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(tag = "action")]
        pub enum Action {
            $( #[serde(rename = $wire)] $name { $( $f: $t ),* }, )*
        }

        /// (wire name, doc, status, effect, [(param, type, required)])
        #[allow(clippy::type_complexity)]
        pub const SPECS: &[(&str, &str, &str, &str, &[(&str, &str, bool)])] = &[
            $( ($wire, $doc, $status, $effect, &[ $( (stringify!($f), stringify!($t), !is_option(stringify!($t))) ),* ]) ),*
        ];
    };
}

// NB: `id` is the request correlation id on the wire, so no action may have a param named `id`.
// status: "implemented" has an engine handler now; "planned" returns `not_implemented` until the
// bead that owns it lands. effect: read (no state change), write (changes state or config),
// control (affects the running engine).
actions! {
    Ping, "ping", "Liveness check.", "implemented", "read", {};
    Version, "version", "App, protocol and schema versions.", "implemented", "read", {};
    ListActions, "list-actions", "The action manifest.", "implemented", "read", {};
    Subscribe, "subscribe", "Stream events on this connection; optional topic filter.", "implemented", "read", { topics: Option<Vec<String>> };
    Quit, "quit", "Stop the engine.", "implemented", "control", {};
    ConfigGet, "config-get", "Resolved value of one key.", "implemented", "read", { key: String };
    ConfigSet, "config-set", "Persist a key in the user config file.", "implemented", "write", { key: String, value: Value };
    ConfigReload, "config-reload", "Re-read the user config file.", "implemented", "write", {};
    ConfigConvert, "config-convert", "Rewrite the user config file in another format (to: toml, json or yaml) beside it, the same keys, and remove the old file; comments are not carried over.", "implemented", "write", { to: String };
    ConfigCheck, "config-check", "Validate the resolved config.", "implemented", "read", {};
    ConfigResolved, "config-resolved", "Resolved config with provenance.", "implemented", "read", {};
    WindowShow, "window-show", "Show the main window.", "implemented", "control", {};
    WindowHide, "window-hide", "Hide the main window.", "implemented", "control", {};
    WindowToggle, "window-toggle", "Toggle the main window.", "implemented", "control", {};
    PrivacyToggle, "privacy-toggle", "Toggle hiding the window from screen capture.", "implemented", "write", {};
    DevicesList, "devices-list", "List audio capture devices (loopback and mic) for the selected backend.", "implemented", "read", {};
    RecordStart, "record-start", "Start recording a session (system loopback + mic to one WAV).", "implemented", "control", { session: Option<String>, title: Option<String> };
    RecordStop, "record-stop", "Stop recording and save the session.", "implemented", "control", {};
    RecordPause, "record-pause", "Pause or resume recording.", "implemented", "control", {};
    SessionImport, "session-import", "Import sessions from an OpenOats sessions root or one session dir.", "implemented", "write", { from: String, overwrite: Option<bool> };
    SessionExport, "session-export", "Write a session's OpenOats markdown export (openoats/v1: frontmatter, notes, [hh:mm:ss] **Speaker:** lines) into dir (default sessions.export_dir); re-exporting replaces that session's file.", "implemented", "write", { session: String, dir: Option<String> };
    SessionExportOpenoats, "session-export-openoats", "Copy a session into an OpenOats sessions folder (default: sessions/ in sessions.openoats_dir) in a form OpenOats lists and opens; overwrite=true replaces a different copy there.", "implemented", "write", { session: String, to: Option<String>, overwrite: Option<bool> };
    IcsAdd, "ics-add", "Subscribe to an ICS feed (http, https, webcal or file URL); header is an optional 'Name: value' request header.", "implemented", "write", { name: String, url: String, enabled: Option<bool>, refresh_secs: Option<u64>, header: Option<String> };
    IcsRemove, "ics-remove", "Unsubscribe from a feed and drop its cache.", "implemented", "write", { name: String };
    IcsList, "ics-list", "Subscribed feeds with cache and refresh state.", "implemented", "read", {};
    IcsRefresh, "ics-refresh", "Fetch one feed (or every enabled feed) now; gated by network.allow.", "implemented", "write", { name: Option<String> };
    IcsEvents, "ics-events", "Events from the cached feeds in a time window (RFC 3339 or YYYY-MM-DD), recurrences expanded.", "implemented", "read", { from: Option<String>, to: Option<String>, feed: Option<String> };
    CalendarSourcesList, "calendar-sources-list", "Calendar sources (calendar.sources) with what each can read: an ics source's feeds, an eventkit source's macOS Calendar accounts and calendars, each marked selected or not, and the Calendars access state. Never asks for access.", "implemented", "read", {};
    CalendarSourceAdd, "calendar-source-add", "Add a calendar source: kind ics (feeds: feed names, default every enabled feed) or eventkit (macOS Calendar; accounts: account titles or emails, calendars: calendar titles or ids, default every calendar). Each list takes one string or an array. Enabling an eventkit source asks macOS for Calendars access the first time.", "implemented", "write", { name: String, kind: String, enabled: Option<bool>, feeds: Option<Value>, accounts: Option<Value>, calendars: Option<Value> };
    CalendarSourceRemove, "calendar-source-remove", "Remove a calendar source.", "implemented", "write", { name: String };
    CalendarSourceEnable, "calendar-source-enable", "Turn a calendar source on (default) or off; turning an eventkit source on asks macOS for Calendars access the first time.", "implemented", "write", { name: String, enabled: Option<bool> };
    CalendarSelect, "calendar-select", "Tick (selected=true, default) or untick one calendar (title or id) or a whole account (title, email or id) of an eventkit source, as the Settings checkboxes do.", "implemented", "write", { source: String, account: Option<String>, calendar: Option<String>, selected: Option<bool> };
    CalendarAccess, "calendar-access", "The macOS Calendars access state; request=true asks for it (the one system prompt, only while undetermined).", "implemented", "control", { request: Option<bool> };
    CalendarEvents, "calendar-events", "Events from every enabled calendar source (or one) in a time window (RFC 3339 or YYYY-MM-DD), with attendees and video links.", "implemented", "read", { from: Option<String>, to: Option<String>, source: Option<String> };
    CalendarMatch, "calendar-match", "Which event would title a recording starting at 'at' (default now; to: its end) and the title it gets, with every overlapping candidate and why it lost.", "implemented", "read", { at: Option<String>, to: Option<String> };
    SecretSet, "secret-set", "Store a secret under key 'namespace/name' in the writable secrets backend (OS keychain). The value is never shown again.", "implemented", "write", { key: String, value: String };
    SecretDelete, "secret-delete", "Delete a stored secret.", "implemented", "write", { key: String };
    SecretHas, "secret-has", "Whether a secret resolves in the configured backend (never returns the value).", "implemented", "read", { key: String };
    SttTranscribe, "stt-transcribe", "Transcribe a WAV file into a new session using the stt.* keys: stereo is you (left) / them (right), mono is one speaker (speaker=you|them).", "implemented", "write", { path: String, session: Option<String>, speaker: Option<String> };
    SttStatus, "stt-status", "Which STT backend is selected and whether it can run (model present, network, key); never shows the key.", "implemented", "read", {};
    SttModelFetch, "stt-model-fetch", "Download a catalog model, whisper.cpp or sherpa-onnx (default: the selected stt.local.model; every file SHA-256 pinned by the catalog) into the models directory. Returns at once with the download state (progress: stt-status, stt-model-list and model-download events); wait=true blocks until it is done.", "implemented", "write", { model: Option<String>, wait: Option<bool> };
    SttModelList, "stt-model-list", "Every model in the STT catalog, for every runtime (whisper.cpp, sherpa-onnx): its size, languages, published WER / RTFx and source, whether this build can run it, whether it is downloaded, which is selected, and any download in progress.", "implemented", "read", {};
    SttModelDelete, "stt-model-delete", "Delete a downloaded catalog model (its file or directory) from the models directory (not while it downloads).", "implemented", "write", { model: String };
    SessionList, "session-list", "List sessions, optionally in a time window (RFC 3339).", "implemented", "read", { from: Option<String>, to: Option<String> };
    SessionGet, "session-get", "One session with its metadata.", "implemented", "read", { session: String };
    SessionRename, "session-rename", "Rename (retitle) a session.", "implemented", "write", { session: String, name: String };
    SessionSpeakerName, "session-speaker-name", "Name a speaker of a session by its transcript id (you, remote_1, remote_2, room_1 ...); the name wins over the [speakers] label in the app, notes and exports. An empty name clears it.", "implemented", "write", { session: String, speaker: String, name: String };
    SessionTag, "session-tag", "Add a tag to a session, or remove it with remove=true.", "implemented", "write", { session: String, tag: String, remove: Option<bool> };
    SessionDelete, "session-delete", "Delete a session; needs confirm=true while sessions.confirm_delete is on.", "implemented", "write", { session: String, confirm: Option<bool> };
    TrashList, "trash-list", "Sessions in the OS trash (moved there by session-delete with sessions.delete_mode=trash), newest first. Linux and Windows; macOS says unsupported.", "implemented", "read", {};
    TrashRestore, "trash-restore", "Restore a trashed session to the session store.", "implemented", "write", { session: String };
    TrashEmpty, "trash-empty", "Delete trashed sessions for good (one session, or all of them); needs confirm=true while sessions.confirm_delete is on.", "implemented", "write", { session: Option<String>, confirm: Option<bool> };
    SessionTranscribe, "session-transcribe", "Transcribe a saved session's own audio into it with the stt.* keys (a recording saved as audio only, e.g. while the model was missing; those are also transcribed automatically once it downloads). overwrite=true replaces an existing transcript.", "implemented", "write", { session: String, overwrite: Option<bool> };
    TranscriptGet, "transcript-get", "A session transcript (final, else live).", "implemented", "read", { session: String };
    TranscriptSearch, "transcript-search", "Search session transcripts: utterances holding every word of the query (case-insensitive), with session id, utterance index and text, offset (seconds into the recording) and a snippet; at most limit hits (default 50).", "implemented", "read", { query: String, limit: Option<u64> };
    NotesGenerate, "notes-generate", "Generate notes for a session from a template with the configured LLM backend; dry_run shows the request without sending.", "implemented", "write", { session: String, template: String, dry_run: Option<bool> };
    NotesGet, "notes-get", "A session's notes (notes.md) and how they were made; empty text when it has none.", "implemented", "read", { session: String };
    NotesSet, "notes-set", "Replace a session's notes with edited text (marked edited; template and backend kept).", "implemented", "write", { session: String, notes: String };
    TemplateList, "template-list", "List note templates.", "implemented", "read", {};
    TemplateGet, "template-get", "One note template.", "implemented", "read", { name: String };
    TemplateSave, "template-save", "Create a note template, or edit (and rename) the one named previous; icon \"\" clears it; overwrite replaces a same-named template on create.", "implemented", "write", { name: String, system_prompt: String, icon: Option<String>, previous: Option<String>, overwrite: Option<bool> };
    TemplateDelete, "template-delete", "Delete a note template file.", "implemented", "write", { name: String };
    TemplateImport, "template-import", "Import an OpenOats templates.json into the templates folder.", "implemented", "write", { from: String, overwrite: Option<bool> };
    TemplateExport, "template-export", "Merge the note templates into an OpenOats templates.json (default: templates.json in sessions.openoats_dir); same id or name is replaced, other OpenOats templates are kept.", "implemented", "write", { to: Option<String> };
    HookList, "hook-list", "The [[hooks]] entries (name, event, command, timeout, enabled, the last run) and the recent hook runs with exit code, timing and output, newest first.", "implemented", "read", {};
    HookTest, "hook-test", "Run one hook now (by name or 1-based position, enabled or not) as if its event (or event) happened for session (default: the newest), wait for it and return the run: exit code, timed_out, stdout and stderr.", "implemented", "control", { hook: String, session: Option<String>, event: Option<String> };
    HookAdd, "hook-add", "Add a [[hooks]] entry: name, event (record.start, record.stop, record.pause, session.saved, transcript.final, notes.generated, calendar.matched), command (argv array, or a string split on spaces; with shell=true the script), timeout (e.g. \"30s\"; default app.hook_timeout), enabled, cwd.", "implemented", "write", { name: String, event: String, command: Value, shell: Option<bool>, timeout: Option<String>, enabled: Option<bool>, cwd: Option<String> };
    HookEdit, "hook-edit", "Change fields of a hook (by name or 1-based position); timeout \"\" goes back to app.hook_timeout.", "implemented", "write", { hook: String, name: Option<String>, event: Option<String>, command: Option<Value>, shell: Option<bool>, timeout: Option<String>, enabled: Option<bool>, cwd: Option<String> };
    HookRemove, "hook-remove", "Remove a hook (by name or 1-based position).", "implemented", "write", { hook: String };
    HookEnable, "hook-enable", "Turn a hook on (default) or off.", "implemented", "write", { hook: String, enabled: Option<bool> };
    KbSearch, "kb-search", "Search the knowledge base (deterministic ranking).", "implemented", "read", { query: String, limit: Option<u64> };
    KbReindex, "kb-reindex", "Rebuild the knowledge base index.", "implemented", "write", {};
    UiSelect, "ui-select", "Switch the open GUI window to a tab (Live, Sessions, Notes, Templates, Calendar, Trash, Models, Settings) as a tab-bar click does; returns the ui-state dump. GUI mode only (headless: no_window).", "implemented", "control", { tab: String };
    UiState, "ui-state", "Dump what the open GUI window shows: active tab, status line, banner and the active view's rows and labels. GUI mode only (headless: no_window).", "implemented", "read", {};
    UiInvoke, "ui-invoke", "Press a view's button in the open GUI window, as a click does (one call is one click: two-step deletes need two); op is view.verb (e.g. templates.save, calendar.add, sessions.search), args its fields; switches to the view and returns the ui-state dump. ui-invoke op=help lists the ops. GUI mode only (headless: no_window).", "implemented", "control", { op: String, args: Option<Value> };
}

pub const EVENTS: &[&str] = &[
    "setting-changed",
    "window-changed",
    "config-reloaded",
    "config-reload-failed",
    "recording-started",
    "recording-paused",
    "recording-stopped",
    "recording-config-deferred",
    "segment",
    "model-download",
    "session-saved",
    "session-deleted",
    "ics-updated",
    "hook-finished",
    "quit",
];

pub fn is_implemented(wire: &str) -> bool {
    SPECS.iter().any(|s| s.0 == wire && s.2 == "implemented")
}

pub fn exists(wire: &str) -> bool {
    SPECS.iter().any(|s| s.0 == wire)
}

pub const MARKER: &str =
    "GENERATED by `oats-meetings-rs config generate` from crates/core/src/action.rs; do not edit";

pub fn manifest_json() -> Value {
    let actions: Vec<Value> = SPECS
        .iter()
        .map(|(name, doc, status, effect, params)| {
            let ps: Vec<Value> = params
                .iter()
                .map(|(n, t, req)| json!({"name": n, "type": t.replace(' ', ""), "required": req}))
                .collect();
            json!({"name": name, "doc": doc, "status": status, "effect": effect, "params": ps})
        })
        .collect();
    json!({
        "$comment": MARKER,
        "schema_version": 1,
        "actions": actions,
        "events": EVENTS,
    })
}

/// Parameter names in declaration order, for positional CLI arguments.
pub fn param_names(wire: &str) -> Vec<&'static str> {
    SPECS
        .iter()
        .find(|s| s.0 == wire)
        .map(|s| s.4.iter().map(|p| p.0).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_matches_enum_names() {
        // Every spec deserializes as an Action with the right tag (params filled with samples).
        for (name, _, _, _, params) in SPECS {
            let mut obj = serde_json::Map::new();
            obj.insert("action".into(), json!(name));
            for (p, t, req) in *params {
                if *req {
                    let v = if t.contains("Value") {
                        json!("x")
                    } else {
                        json!("x")
                    };
                    obj.insert((*p).to_string(), v);
                }
            }
            let a: Action = serde_json::from_value(Value::Object(obj))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let back = serde_json::to_value(&a).unwrap();
            assert_eq!(back["action"], json!(name));
        }
    }

    #[test]
    fn no_param_is_named_id() {
        for s in SPECS {
            assert!(s.4.iter().all(|p| p.0 != "id" && p.0 != "v"), "{}", s.0);
        }
    }

    #[test]
    fn names_are_unique_and_kebab() {
        let mut seen = std::collections::BTreeSet::new();
        for s in SPECS {
            assert!(seen.insert(s.0), "dup {}", s.0);
            assert!(s.0.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
        }
    }

    #[test]
    fn param_types_are_reported() {
        let m = manifest_json();
        let set = m["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == "config-set")
            .unwrap();
        assert_eq!(set["params"][1]["type"], "Value");
        let sub = m["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == "subscribe")
            .unwrap();
        assert_eq!(sub["params"][0]["required"], false);
    }
}
