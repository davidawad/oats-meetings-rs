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

/// Whisper model names `stt.local.model` accepts; ci/fetch-models.sh pins and fetches each one.
pub const STT_LOCAL_MODELS: &[&str] = &["tiny.en", "tiny", "base.en", "base", "small.en", "small"];
pub const STT_BACKENDS: &[&str] = &["local", "cloud"];
pub const STT_ADAPTERS: &[&str] = &["none", "openai-compatible"];
pub const LLM_BACKENDS: &[&str] = &["ollama", "openrouter", "openai-compatible"];
pub const AUDIO_BACKENDS: &[&str] = &["auto", "pulse", "pipewire", "cpal"];
pub const AUDIO_LAYOUTS: &[&str] = &["split", "mix"];
pub const AUDIO_TRANSCRIBE: &[&str] = &["auto", "on", "off"];
pub const KB_EMBEDDERS: &[&str] = &["hash", "ollama", "openai-compatible"];
pub const ICS_WEBCAL_SCHEMES: &[&str] = &["https", "http"];
pub const ICS_CANCELLED: &[&str] = &["hide", "show"];
pub const ICS_ON_BAD_EVENT: &[&str] = &["error", "skip"];
pub const ICS_UNKNOWN_TZ: &[&str] = &["error", "default_tz"];
pub const ICS_ALL_DAY: &[&str] = &["include", "exclude"];
pub const DELETE_MODES: &[&str] = &["trash", "remove"];
pub const SECRETS_BACKENDS: &[&str] = &["env", "keyring", "env-then-keyring"];
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
        path: "audio.backend",
        kind: Kind::Enum(AUDIO_BACKENDS),
        default: Def::S("auto"),
        doc: "Capture backend: auto picks per OS (Linux: pulse, else pipewire; Windows: cpal/WASAPI).",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "audio.layout",
        kind: Kind::Enum(AUDIO_LAYOUTS),
        default: Def::S("split"),
        doc: "WAV layout: split is two channels (left system audio, right mic); mix is one summed channel.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "audio.mic.device",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Microphone device id (see `ctl devices list`); empty means the default mic.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "audio.mic.enabled",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Record the microphone.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "audio.system.device",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "System-audio loopback device id (see `ctl devices list`); empty means the default loopback.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "audio.system.enabled",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Record system audio (what the machine plays).",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "audio.transcribe",
        kind: Kind::Enum(AUDIO_TRANSCRIBE),
        default: Def::S("auto"),
        doc: "Transcribe while recording (stt.* settings): on fails record-start if the ASR is unavailable, auto records audio only then, off never transcribes.",
        ui: true,
        reload: Reload::Live,
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
        path: "llm.api_key_ref",
        kind: Kind::Str,
        default: Def::S("secret:llm/api-key"),
        doc: "Reference (secret:<namespace>/<name>) to the LLM API key in the secrets backend. The key itself is never in config; store it with secret-set.",
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
        path: "secrets.backend",
        kind: Kind::Enum(SECRETS_BACKENDS),
        default: Def::S("env-then-keyring"),
        doc: "Where secret:<key> references are resolved: environment variables, the OS keychain, or env first then keychain.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "secrets.env_prefix",
        kind: Kind::Str,
        default: Def::S("OATS_SECRET_"),
        doc: "Prefix of the environment variable holding a secret: secret:ics/work reads OATS_SECRET_ICS_WORK.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "secrets.keyring_service",
        kind: Kind::Str,
        default: Def::S("oats-meetings-rs"),
        doc: "Service name secrets are filed under in the OS keychain.",
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
        path: "stt.cloud.key",
        kind: Kind::Str,
        default: Def::S("secret:stt/cloud"),
        doc: "Secret reference for the cloud STT API key (never the key itself); see the secrets module.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.cloud.model",
        kind: Kind::Str,
        default: Def::S("whisper-1"),
        doc: "Model name sent to the cloud STT service.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.cloud.timeout_secs",
        kind: Kind::Str,
        default: Def::S("60"),
        doc: "Whole-request timeout for one cloud transcription, in seconds.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.cloud.url",
        kind: Kind::Str,
        default: Def::S("https://api.openai.com/v1"),
        doc: "Base URL of the OpenAI-compatible transcription API.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.final.enabled",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Re-transcribe the whole recording when it stops and write transcript.final.jsonl.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.language",
        kind: Kind::Str,
        default: Def::S("en"),
        doc: "Spoken language code passed to the recogniser.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.live.chunk_secs",
        kind: Kind::Str,
        default: Def::S("4"),
        doc: "Seconds of audio per live transcription window.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.live.enabled",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Transcribe while recording and write transcript.live.jsonl.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.model_path",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Explicit path to the local model file; empty means <models_dir>/ggml-<stt.local.model>.bin.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.models_dir",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Directory holding local model files; empty means models/ beside the sessions dir.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.threads",
        kind: Kind::Str,
        default: Def::S("4"),
        doc: "CPU threads for local transcription.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.speaker_them",
        kind: Kind::Str,
        default: Def::S("remote_1"),
        doc: "Speaker id written for the system-audio (them) channel.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.speaker_you",
        kind: Kind::Str,
        default: Def::S("you"),
        doc: "Speaker id written for the microphone (you) channel.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.model",
        kind: Kind::Enum(STT_LOCAL_MODELS),
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
