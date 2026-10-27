//! The single defaults object. Every default value appears here and nowhere else; the
//! schema, `defaults.toml` and `defaults.json` are generated from this table.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Str,
    Enum(&'static [&'static str]),
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
}

pub const STT_BACKENDS: &[&str] = &["local", "cloud"];
pub const STT_ADAPTERS: &[&str] = &["none", "openai-compatible"];
pub const LLM_BACKENDS: &[&str] = &["ollama", "openrouter", "openai-compatible"];
pub const AUDIO_BACKENDS: &[&str] = &["auto", "pulse", "pipewire", "cpal"];
pub const AUDIO_LAYOUTS: &[&str] = &["split", "mix"];
pub const HIDE_METHODS: &[&str] = &["none", "sharing_type", "display_affinity"];

/// Product choices are keys; defaults get chosen at the end (David, dot-4uzgol ruling).
pub const KEYS: &[Key] = &[
    Key {
        path: "app.menubar",
        kind: Kind::Bool,
        default: Def::B(false),
        doc: "Show a menubar / tray item.",
        ui: true,
    },
    Key {
        path: "app.single_instance",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Refuse to start a second engine on the same control socket.",
        ui: true,
    },
    Key {
        path: "audio.backend",
        kind: Kind::Enum(AUDIO_BACKENDS),
        default: Def::S("auto"),
        doc: "Capture backend: auto picks per OS (Linux: pulse, else pipewire, else cpal; Windows: cpal/WASAPI).",
        ui: true,
    },
    Key {
        path: "audio.layout",
        kind: Kind::Enum(AUDIO_LAYOUTS),
        default: Def::S("split"),
        doc: "WAV layout: split is two channels (left system audio, right mic); mix is one summed channel.",
        ui: true,
    },
    Key {
        path: "audio.mic.device",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Microphone device id (see `ctl devices list`); empty means the default mic.",
        ui: true,
    },
    Key {
        path: "audio.mic.enabled",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Record the microphone.",
        ui: true,
    },
    Key {
        path: "audio.record_dir",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Directory recordings are written under (one folder per session); empty means the per-user sessions dir.",
        ui: true,
    },
    Key {
        path: "audio.system.device",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "System-audio loopback device id (see `ctl devices list`); empty means the default loopback.",
        ui: true,
    },
    Key {
        path: "audio.system.enabled",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Record system audio (what the machine plays).",
        ui: true,
    },
    Key {
        path: "config.hot_reload",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Watch the user config file and reload on change.",
        ui: true,
    },
    Key {
        path: "control.socket",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Control socket path; empty means the per-user runtime dir.",
        ui: false,
    },
    Key {
        path: "license.holder",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Licence holder metadata (placeholder; no licence chosen).",
        ui: false,
    },
    Key {
        path: "license.spdx",
        kind: Kind::Str,
        default: Def::S("LicenseRef-Undecided"),
        doc: "SPDX expression for packaging metadata (placeholder).",
        ui: false,
    },
    Key {
        path: "llm.backend",
        kind: Kind::Enum(LLM_BACKENDS),
        default: Def::S("ollama"),
        doc: "Notes-generation LLM backend.",
        ui: true,
    },
    Key {
        path: "network.allow",
        kind: Kind::Bool,
        default: Def::B(false),
        doc: "Allow outward network calls (cloud STT / LLM). Off means such backends are denied.",
        ui: true,
    },
    Key {
        path: "privacy.hide_from_capture",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Hide the app window from screen capture.",
        ui: true,
    },
    Key {
        path: "privacy.hide_method",
        kind: Kind::Enum(HIDE_METHODS),
        default: Def::S("none"),
        doc: "How hiding is done; set per OS by the os axis.",
        ui: false,
    },
    Key {
        path: "stt.backend",
        kind: Kind::Enum(STT_BACKENDS),
        default: Def::S("local"),
        doc: "Speech-to-text backend: on-device or a cloud adapter.",
        ui: true,
    },
    Key {
        path: "stt.cloud.adapter",
        kind: Kind::Enum(STT_ADAPTERS),
        default: Def::S("none"),
        doc: "Cloud STT adapter; must not be none when stt.backend is cloud.",
        ui: true,
    },
    Key {
        path: "stt.local.model",
        kind: Kind::Str,
        default: Def::S("base.en"),
        doc: "Local ASR model name.",
        ui: true,
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
