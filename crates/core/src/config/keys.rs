//! The single defaults object. Every default value appears here and nowhere else; the
//! schema, `defaults.toml` and `defaults.json` are generated from this table.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Str,
    Enum(&'static [&'static str]),
}

/// How a change to this key reaches the running app. Every key must be hot-reloadable; the
/// class says how, and the reload test suite exercises each class for each key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reload {
    /// Consumers read the active snapshot at the point of use; the swap is the whole effect.
    Live,
    /// The control socket is re-bound at the new path.
    Rebind,
    /// The single-instance lock is taken or released.
    Lock,
}

impl Reload {
    pub fn as_str(self) -> &'static str {
        match self {
            Reload::Live => "live",
            Reload::Rebind => "rebind",
            Reload::Lock => "lock",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Def {
    B(bool),
    S(&'static str),
}

impl Def {
    pub fn to_json(self) -> Value {
        match self {
            Def::B(b) => Value::Bool(b),
            Def::S(s) => Value::String(s.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub path: &'static str,
    pub kind: Kind,
    pub default: Def,
    pub doc: &'static str,
    /// false = deliberately not shown in the settings view (dot-4uzgol.11 checks this).
    pub ui: bool,
    pub reload: Reload,
}

pub const STT_BACKENDS: &[&str] = &["local", "cloud"];
pub const STT_ADAPTERS: &[&str] = &["none", "openai-compatible"];
pub const LLM_BACKENDS: &[&str] = &["ollama", "openrouter", "openai-compatible"];
pub const KB_EMBEDDERS: &[&str] = &["hash", "ollama", "openai-compatible"];
pub const SECRET_SOURCES: &[&str] = &["env", "keychain"];
pub const ICS_WEBCAL_SCHEMES: &[&str] = &["https", "http"];
pub const ICS_CANCELLED: &[&str] = &["hide", "show"];
pub const ICS_ON_BAD_EVENT: &[&str] = &["error", "skip"];
pub const ICS_UNKNOWN_TZ: &[&str] = &["error", "default_tz"];
pub const ICS_ALL_DAY: &[&str] = &["include", "exclude"];
pub const DELETE_MODES: &[&str] = &["trash", "remove"];
pub const HIDE_METHODS: &[&str] = &["none", "sharing_type", "display_affinity"];

/// Product choices are keys; defaults get chosen at the end (David, dot-4uzgol ruling).
pub const KEYS: &[Key] = &[
    Key {
        path: "app.menubar",
        kind: Kind::Bool,
        default: Def::B(false),
        doc: "Show a menubar / tray item.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "app.single_instance",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Refuse to start a second engine on the same control socket.",
        ui: true,
        reload: Reload::Lock,
    },
    Key {
        path: "config.hot_reload",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Watch the user config file and reload on change.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "control.socket",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Control socket path; empty means the per-user runtime dir.",
        ui: false,
        reload: Reload::Rebind,
    },
    Key {
        path: "kb.base_url",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Embedding endpoint base URL for kb.embedder ollama / openai-compatible.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "kb.dir",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Knowledge-base folder of .md/.txt files; empty means <data dir>/kb.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "kb.embedder",
        kind: Kind::Enum(KB_EMBEDDERS),
        default: Def::S("hash"),
        doc: "KB embedder: local deterministic hashing, or a model endpoint (gated by network.allow unless loopback).",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "kb.model",
        kind: Kind::Str,
        default: Def::S("nomic-embed-text"),
        doc: "Embedding model name for the ollama / openai-compatible embedders.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "llm.api_key_name",
        kind: Kind::Str,
        default: Def::S("OATS_LLM_API_KEY"),
        doc: "NAME of the secret holding the LLM API key (env var or keychain account). The key itself is never in config.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "llm.base_url",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "LLM endpoint base URL; empty means the backend default (required for openai-compatible).",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "llm.model",
        kind: Kind::Str,
        default: Def::S("llama3.1"),
        doc: "Model name sent to the LLM backend.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "notes.enforce_headings",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Reject generated notes that lack a '## ' heading the template's prompt demands.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "secrets.keychain_service",
        kind: Kind::Str,
        default: Def::S("oats-meetings-rs"),
        doc: "Keychain service name used when secrets.source = keychain.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "secrets.source",
        kind: Kind::Enum(SECRET_SOURCES),
        default: Def::S("env"),
        doc: "Where API keys are read from: process environment or the OS keychain.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "templates.dir",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Folder of note-template TOML files; empty means <config dir>/templates.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.all_day",
        kind: Kind::Enum(ICS_ALL_DAY),
        default: Def::S("include"),
        doc: "Whether all-day events appear in ics-events.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.allow_file",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Allow file:// feeds (local, not gated by network.allow).",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.cache_dir",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Directory for cached feeds; empty means ics/ beside the sessions dir.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ics.cancelled",
        kind: Kind::Enum(ICS_CANCELLED),
        default: Def::S("hide"),
        doc: "Whether STATUS:CANCELLED events appear in ics-events.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.default_refresh_secs",
        kind: Kind::Str,
        default: Def::S("900"),
        doc: "Seconds between automatic refreshes of a feed with no refresh_secs of its own; 0 means manual only.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.default_tz",
        kind: Kind::Str,
        default: Def::S("UTC"),
        doc: "IANA zone for floating times and for the midnight bounds of all-day events.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.enabled",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Master switch for refreshing feeds (manual and scheduled). Cached events stay readable.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.feeds",
        kind: Kind::Str,
        default: Def::S("[]"),
        doc: "JSON array of feeds: {name, url, enabled, refresh_secs, header}. Edit with ics-add / ics-remove.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.max_bytes",
        kind: Kind::Str,
        default: Def::S("5242880"),
        doc: "Largest feed body accepted, in bytes.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ics.max_occurrences",
        kind: Kind::Str,
        default: Def::S("500"),
        doc: "Most occurrences expanded per recurring event per query.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ics.on_bad_event",
        kind: Kind::Enum(ICS_ON_BAD_EVENT),
        default: Def::S("error"),
        doc: "A malformed event fails the whole feed (error) or is dropped and counted (skip).",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.stale_on_error",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "When a refresh fails, keep serving the last good cached copy.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.timeout_secs",
        kind: Kind::Str,
        default: Def::S("15"),
        doc: "Whole-request timeout for a feed fetch, in seconds.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ics.unknown_tz",
        kind: Kind::Enum(ICS_UNKNOWN_TZ),
        default: Def::S("error"),
        doc: "An unknown TZID fails the event (error) or falls back to ics.default_tz.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.webcal_scheme",
        kind: Kind::Enum(ICS_WEBCAL_SCHEMES),
        default: Def::S("https"),
        doc: "Scheme webcal:// URLs are fetched with.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ics.window_days",
        kind: Kind::Str,
        default: Def::S("7"),
        doc: "Default length of the ics-events window in days when no end is given.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "license.holder",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Licence holder metadata (placeholder; no licence chosen).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "license.spdx",
        kind: Kind::Str,
        default: Def::S("LicenseRef-Undecided"),
        doc: "SPDX expression for packaging metadata (placeholder).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "llm.backend",
        kind: Kind::Enum(LLM_BACKENDS),
        default: Def::S("ollama"),
        doc: "Notes-generation LLM backend.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "network.allow",
        kind: Kind::Bool,
        default: Def::B(false),
        doc: "Allow outward network calls (cloud STT / LLM). Off means such backends are denied.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "privacy.hide_from_capture",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Hide the app window from screen capture.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "privacy.hide_method",
        kind: Kind::Enum(HIDE_METHODS),
        default: Def::S("none"),
        doc: "How hiding is done; set per OS by the os axis.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "sessions.confirm_delete",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Require confirm=true to delete a session.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "sessions.delete_mode",
        kind: Kind::Enum(DELETE_MODES),
        default: Def::S("trash"),
        doc: "Deleting a session moves it to the OS trash (trash) or removes it for good (remove). Provisional default.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "sessions.root",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Session store directory (OpenOats layout); empty means the per-user data dir.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.backend",
        kind: Kind::Enum(STT_BACKENDS),
        default: Def::S("local"),
        doc: "Speech-to-text backend: on-device or a cloud adapter.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.cloud.adapter",
        kind: Kind::Enum(STT_ADAPTERS),
        default: Def::S("none"),
        doc: "Cloud STT adapter; must not be none when stt.backend is cloud.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.model",
        kind: Kind::Str,
        default: Def::S("base.en"),
        doc: "Local ASR model name.",
        ui: true,
        reload: Reload::Live,
    },
];

pub fn find(path: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.path == path)
}

/// Every choice key (bool or enum) with its allowed values, as strings.
pub fn choices() -> Vec<(&'static str, Vec<String>)> {
    KEYS.iter()
        .filter_map(|k| match k.kind {
            Kind::Bool => Some((k.path, vec!["true".to_string(), "false".to_string()])),
            Kind::Enum(vs) => Some((k.path, vs.iter().map(|v| v.to_string()).collect())),
            Kind::Str => None,
        })
        .collect()
}
