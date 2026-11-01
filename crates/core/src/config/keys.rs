//! The single defaults object. Every default value appears here and nowhere else; the
//! schema, `defaults.toml` and `defaults.json` are generated from this table.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Str,
    Enum(&'static [&'static str]),
    /// Whole number in `min..=max` (inclusive). Out of range vetoes the reload.
    Int {
        min: i64,
        max: i64,
    },
    /// Duration written as `<digits><unit>` with unit ms, s, m, h or d (`"15s"`, `"250ms"`),
    /// stored in milliseconds within `min_ms..=max_ms`. Out of range vetoes the reload.
    Duration {
        min_ms: u64,
        max_ms: u64,
    },
}

pub const MS: u64 = 1;
pub const SEC: u64 = 1000;
pub const MIN: u64 = 60 * SEC;
pub const HOUR: u64 = 60 * MIN;
pub const DAY: u64 = 24 * HOUR;

/// Parse `"<digits><unit>"` (ms, s, m, h, d) into milliseconds. No bare numbers, no fractions.
pub fn parse_duration_ms(s: &str) -> Option<u64> {
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = t.split_at(split);
    if num.is_empty() {
        return None;
    }
    let n: u64 = num.parse().ok()?;
    let mult = match unit {
        "ms" => MS,
        "s" => SEC,
        "m" => MIN,
        "h" => HOUR,
        "d" => DAY,
        _ => return None,
    };
    n.checked_mul(mult)
}

/// Canonical text for a millisecond count: the largest unit that divides it exactly.
pub fn format_duration_ms(ms: u64) -> String {
    for (unit, mult) in [("d", DAY), ("h", HOUR), ("m", MIN), ("s", SEC)] {
        if ms >= mult && ms % mult == 0 {
            return format!("{}{unit}", ms / mult);
        }
    }
    if ms == 0 {
        return "0s".into();
    }
    format!("{ms}ms")
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
    I(i64),
    /// Milliseconds; rendered as a duration string.
    D(u64),
}

impl Def {
    pub fn to_json(self) -> Value {
        match self {
            Def::B(b) => Value::Bool(b),
            Def::S(s) => Value::String(s.to_string()),
            Def::I(n) => Value::from(n),
            Def::D(ms) => Value::String(format_duration_ms(ms)),
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

/// Model ids `stt.local.model` accepts: the entries of crates/core/data/stt-models.json, in its
/// order (a test keeps them equal). The catalog entry says which runtime runs the model.
pub const STT_LOCAL_MODELS: &[&str] = &[
    "parakeet-tdt-0.6b-v2",
    "large-v3-turbo-q5_0",
    "small.en",
    "base.en",
    "tiny.en",
    "moonshine-base-en",
];
/// Model ids the catalog dropped when it went English-only (dot-4uzgol.68), each with its English
/// replacement, so a config still naming one gets told what to use instead.
pub const STT_RETIRED_MODELS: &[(&str, &str)] = &[
    ("parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v2"),
    ("small", "small.en"),
    ("base", "base.en"),
    ("tiny", "tiny.en"),
];

/// Why `id` is no longer a model, when it is one the catalog dropped.
pub fn retired_stt_model(id: &str) -> Option<String> {
    let (_, to) = STT_RETIRED_MODELS.iter().find(|(from, _)| *from == id)?;
    Some(format!(
        "'{id}' was removed: the model catalog is English only; use {to} (English models: {})",
        STT_LOCAL_MODELS.join(", ")
    ))
}
/// onnxruntime execution providers for sherpa-onnx models: auto is coreml on macOS, cpu elsewhere.
pub const STT_PROVIDERS: &[&str] = &["auto", "cpu", "coreml"];
pub const STT_BACKENDS: &[&str] = &["local", "cloud"];
pub const STT_ADAPTERS: &[&str] = &["none", "openai-compatible"];
pub const LLM_BACKENDS: &[&str] = &["ollama", "openrouter", "openai-compatible"];
pub const AUDIO_BACKENDS: &[&str] = &["auto", "pulse", "pipewire", "cpal", "screencapturekit"];
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
        path: "llm.timeout",
        kind: Kind::Duration { min_ms: SEC, max_ms: HOUR },
        default: Def::D(120 * SEC),
        doc: "Whole-request timeout for a notes-generation LLM call.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "kb.timeout",
        kind: Kind::Duration { min_ms: SEC, max_ms: HOUR },
        default: Def::D(120 * SEC),
        doc: "Whole-request timeout for a KB embedding call.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "kb.max_chunk",
        kind: Kind::Int { min: 100, max: 100_000 },
        default: Def::I(1200),
        doc: "Largest KB chunk, in characters; longer paragraphs are cut.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "kb.min_chunk",
        kind: Kind::Int { min: 1, max: 1_000 },
        default: Def::I(20),
        doc: "Smallest KB chunk kept, in characters; shorter fragments are dropped. Must not exceed kb.max_chunk.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ics.poll_interval",
        kind: Kind::Duration { min_ms: 100 * MS, max_ms: HOUR },
        default: Def::D(SEC),
        doc: "How often the feed scheduler checks which feeds are due.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "config.poll_interval",
        kind: Kind::Duration { min_ms: 10 * MS, max_ms: 10 * MIN },
        default: Def::D(250 * MS),
        doc: "How often the user config file is checked for changes while config.hot_reload is on.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "control.max_line",
        kind: Kind::Int { min: 1_024, max: 67_108_864 },
        default: Def::I(1_048_576),
        doc: "Largest control-socket request line, in bytes; longer lines are refused.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "control.event_queue",
        kind: Kind::Int { min: 1, max: 65_536 },
        default: Def::I(256),
        doc: "Events queued per subscriber before a slow one is dropped.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "control.accept_poll",
        kind: Kind::Duration { min_ms: MS, max_ms: SEC },
        default: Def::D(10 * MS),
        doc: "Idle wait of the control-socket accept loop between checks.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "control.accept_backoff",
        kind: Kind::Duration { min_ms: MS, max_ms: 10 * SEC },
        default: Def::D(50 * MS),
        doc: "Wait after an accept error before trying again.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "control.client_timeout",
        kind: Kind::Duration { min_ms: 100 * MS, max_ms: HOUR },
        default: Def::D(10 * SEC),
        doc: "Default wait of oats-ctl for events or for an engine that is still starting (its --timeout-ms flag overrides).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.open_timeout",
        kind: Kind::Duration { min_ms: SEC, max_ms: 5 * MIN },
        default: Def::D(15 * SEC),
        doc: "How long a capture backend waits for audio to start flowing before giving up.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.devices_timeout",
        kind: Kind::Duration { min_ms: 100 * MS, max_ms: MIN },
        default: Def::D(3 * SEC),
        doc: "How long devices-list (and the device check of a reload) waits for the audio backend to list its devices before failing with devices_timeout; the GUI shows devices unavailable.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.devices_ttl",
        kind: Kind::Duration { min_ms: 100 * MS, max_ms: HOUR },
        default: Def::D(30 * SEC),
        doc: "How long a device listing is reused before the audio backend is asked again.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.poll_interval",
        kind: Kind::Duration { min_ms: 5 * MS, max_ms: SEC },
        default: Def::D(25 * MS),
        doc: "Pause of the recorder writer and of capture readiness polling.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.echo_tail",
        kind: Kind::Duration { min_ms: 10 * MS, max_ms: SEC },
        default: Def::D(200 * MS),
        doc: "Longest speaker-to-mic echo the echo canceller models (audio.echo_cancel).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.echo_delay",
        kind: Kind::Duration { min_ms: 0, max_ms: 500 * MS },
        default: Def::D(40 * MS),
        doc: "Mic delay inside the echo canceller, so system audio that is captured a little after its echo reached the mic is still cancelled; both channels are shifted by it.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.echo_block",
        kind: Kind::Int { min: 32, max: 4_096 },
        default: Def::I(256),
        doc: "Samples per echo-canceller block (rounded up to a power of two): smaller adapts faster, larger costs less.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.echo_step",
        kind: Kind::Int { min: 1, max: 1_000 },
        default: Def::I(500),
        doc: "Echo-canceller adaptation step in per-mille (500 = 0.5): larger converges faster, smaller is steadier.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "audio.echo_divergence",
        kind: Kind::Int { min: 101, max: 1_000 },
        default: Def::I(200),
        doc: "Percent: the echo canceller's output filter takes the adapting filter when its residual is this many times smaller, and resets it when it is this many times larger (double talk).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ui.start_hidden",
        kind: Kind::Bool,
        default: Def::B(false),
        doc: "Start with the main window closed (tray / ctl window-show opens it); off opens it at launch.",
        ui: true,
        reload: Reload::Lock,
    },
    Key {
        path: "ui.activate_on_show",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "An explicit show request (tray Show Window, ctl window-show) brings the app to the front and gives it focus. The app never takes focus on its own, with this on or off.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ui.quit_timeout",
        kind: Kind::Duration { min_ms: 100 * MS, max_ms: 5 * MIN },
        default: Def::D(5 * SEC),
        doc: "How long after ctl quit the app waits for the GUI to exit before it exits the process itself.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ui.tick",
        kind: Kind::Duration { min_ms: 10 * MS, max_ms: 10 * SEC },
        default: Def::D(100 * MS),
        doc: "How often the window re-reads the engine state (window visibility, capture hiding, tray).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ui.drive_wait",
        kind: Kind::Duration { min_ms: 100 * MS, max_ms: 5 * MIN },
        default: Def::D(10 * SEC),
        doc: "How long a ui-select, ui-state or ui-invoke call waits for the window to answer before failing with timeout.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ui.record_controls",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Show the Start, Pause / Resume and Stop buttons and the recording badge in the main window.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ui.show_elapsed",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Show the elapsed recording time in the recording badge.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ui.live_follow",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "The Live view scrolls to the newest line while a recording adds lines.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ui.live_max_lines",
        kind: Kind::Int { min: 50, max: 100_000 },
        default: Def::I(2000),
        doc: "Most lines the Live view keeps for one recording; older lines drop first.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ui.speaker_width",
        kind: Kind::Int { min: 40, max: 400 },
        default: Def::I(96),
        doc: "Width in pixels of the speaker column in transcript views.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "ui.tz",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "IANA zone the window shows meeting times in, labelled with its abbreviation; empty is the system zone (TZ, else the OS setting).",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "kb.hash_dims",
        kind: Kind::Int { min: 8, max: 8_192 },
        default: Def::I(256),
        doc: "Vector length of the local hash embedder. An index records it: changing it makes kb-search refuse the old index until kb-reindex rebuilds it.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "sessions.max_id_len",
        kind: Kind::Int { min: 32, max: 255 },
        default: Def::I(200),
        doc: "Longest session id (it becomes a directory name).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "sessions.search_snippet_chars",
        kind: Kind::Int { min: 0, max: 400 },
        default: Def::I(40),
        doc: "Characters of context kept either side of a transcript-search match in its snippet (the Sessions search shows it under the row).",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "secrets.max_key_len",
        kind: Kind::Int { min: 16, max: 512 },
        default: Def::I(128),
        doc: "Longest secret key (namespace/name).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ics.max_name_len",
        kind: Kind::Int { min: 1, max: 255 },
        default: Def::I(64),
        doc: "Longest feed name.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "control.drain_chunk",
        kind: Kind::Int { min: 1_024, max: 1_048_576 },
        default: Def::I(65_536),
        doc: "Bytes read per step while discarding the rest of an oversize request line.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "control.drain_max",
        kind: Kind::Int { min: 1_024, max: 1_073_741_824 },
        default: Def::I(8_388_608),
        doc: "Most bytes discarded from one oversize request line before the connection is closed.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.speech_peak",
        kind: Kind::Int { min: 1, max: 1_000 },
        default: Def::I(10),
        doc: "Per-mille of full scale: a 10 ms frame whose peak is below this is silence (10 = 0.01).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.pad",
        kind: Kind::Duration { min_ms: 0, max_ms: 10 * SEC },
        default: Def::D(250 * MS),
        doc: "Silence kept around the speech that is sent to the recogniser.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.final_piece",
        kind: Kind::Duration { min_ms: 10 * SEC, max_ms: HOUR },
        default: Def::D(10 * MIN),
        doc: "Longest stretch sent in one request in the final pass (cloud uploads are size-limited).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.feed",
        kind: Kind::Duration { min_ms: 50 * MS, max_ms: 10 * SEC },
        default: Def::D(500 * MS),
        doc: "Window used to feed a recorded file to the live pass as if it were arriving.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.echo_window",
        kind: Kind::Duration { min_ms: 0, max_ms: 10 * SEC },
        default: Def::D(SEC),
        doc: "With audio.echo_cancel, a local-speaker utterance within this time of remote speech that says the same words is dropped as speaker echo.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.echo_overlap",
        kind: Kind::Int { min: 1, max: 100 },
        default: Def::I(60),
        doc: "Percent of a local-speaker utterance's words that must appear in near-simultaneous remote speech for it to be dropped as echo (stt.echo_window).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "network.error_snippet",
        kind: Kind::Int { min: 20, max: 10_000 },
        default: Def::I(300),
        doc: "Characters of a failing HTTP reply kept in the error message (LLM, embedding and cloud STT calls).",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.cloud.max_response",
        kind: Kind::Int { min: 1_024, max: 1_073_741_824 },
        default: Def::I(8_388_608),
        doc: "Largest cloud transcription reply accepted, in bytes.",
        ui: false,
        reload: Reload::Live,
    },
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
        doc: "Capture backend: auto picks per OS (Linux: pulse, else pipewire; Windows: cpal/WASAPI; macOS: screencapturekit).",
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
        path: "audio.echo_cancel",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Cancel speaker echo in the mic using system audio as the reference, and drop local-speaker utterances that repeat near-simultaneous remote speech, so remote speech is not transcribed twice.",
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
        path: "notes.default_template",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Template the Notes view picks for regenerating; empty means the first one listed.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "notes.confirm_regenerate",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "Regenerating over existing notes in the Notes view needs a second click.",
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
        path: "ics.default_refresh",
        kind: Kind::Duration { min_ms: 0, max_ms: 30 * DAY },
        default: Def::D(15 * MIN),
        doc: "Time between automatic refreshes of a feed with no refresh_secs of its own; 0s means manual only.",
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
        kind: Kind::Int { min: 1, max: 1 << 30 },
        default: Def::I(5_242_880),
        doc: "Largest feed body accepted, in bytes.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "ics.max_occurrences",
        kind: Kind::Int { min: 1, max: 65_535 },
        default: Def::I(500),
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
        path: "ics.timeout",
        kind: Kind::Duration { min_ms: SEC, max_ms: 10 * MIN },
        default: Def::D(15 * SEC),
        doc: "Whole-request timeout for a feed fetch.",
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
        kind: Kind::Int { min: 1, max: 36_500 },
        default: Def::I(7),
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
        path: "stt.cloud.timeout",
        kind: Kind::Duration { min_ms: SEC, max_ms: HOUR },
        default: Def::D(60 * SEC),
        doc: "Whole-request timeout for one cloud transcription.",
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
        path: "stt.live.chunk",
        kind: Kind::Duration { min_ms: SEC, max_ms: MIN },
        default: Def::D(4 * SEC),
        doc: "Length of audio per live transcription window.",
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
        path: "stt.local.piece",
        kind: Kind::Duration { min_ms: 5 * SEC, max_ms: 10 * MIN },
        default: Def::D(30 * SEC),
        doc: "Longest stretch a sherpa-onnx model (Parakeet, Moonshine) decodes at once; longer audio is cut at the quietest point near this length, which bounds memory on long recordings. Each piece is split into sentences by the model's token times.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.provider",
        kind: Kind::Enum(STT_PROVIDERS),
        default: Def::S("auto"),
        doc: "Where sherpa-onnx models (Parakeet, Moonshine) run: auto uses CoreML (Apple Neural Engine / GPU) on macOS and the CPU elsewhere; coreml falls back to the CPU when it cannot load the model. stt-status reports the provider in use. whisper.cpp models always run on the CPU.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.model_path",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Explicit path to the local model; empty means where the model catalog puts stt.local.model under models_dir (a ggml-*.bin file for whisper.cpp models, a directory of files for sherpa-onnx models). Nothing is downloaded for an explicit path.",
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
        path: "stt.local.model_url",
        kind: Kind::Str,
        default: Def::S(""),
        doc: "Mirror for model downloads: empty fetches each file from the URL in the model catalog; set, a file comes from <url>/<file> (whisper.cpp) or <url>/<model dir>/<file> (sherpa-onnx). Every file is checked against the catalog's SHA-256, so a mirror must serve the same bytes.",
        ui: false,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.auto_download",
        kind: Kind::Bool,
        default: Def::B(true),
        doc: "When the selected local model is missing at app launch (or when a recording starts), download it in the background (about 660 MB for the default Parakeet v2); recordings saved as audio only meanwhile are transcribed when it finishes. Picking a model in the Models tab also downloads it when this and network.allow are both on. Off means transcription stays unavailable until you run `ctl stt model fetch` or press Download. A download you ask for with stt-model-fetch is not gated by this or by network.allow.",
        ui: true,
        reload: Reload::Live,
    },
    Key {
        path: "stt.local.threads",
        kind: Kind::Int { min: 1, max: 256 },
        default: Def::I(4),
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
        default: Def::S("parakeet-tdt-0.6b-v2"),
        doc: "Local ASR model: an id from the model catalog (stt-model-list shows each with its size, WER and speed). The catalog entry decides the runtime: whisper.cpp for the Whisper models, sherpa-onnx for Parakeet and Moonshine.",
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
            Kind::Str | Kind::Int { .. } | Kind::Duration { .. } => None,
        })
        .collect()
}

/// A key's default whole number.
pub fn default_int(path: &str) -> i64 {
    match find(path).map(|k| k.default) {
        Some(Def::I(n)) => n,
        _ => panic!("{path} is not an integer key"),
    }
}

/// A key's default duration in milliseconds (for callers with no resolved config, like oats-ctl).
pub fn default_duration(path: &str) -> std::time::Duration {
    match find(path).map(|k| k.default) {
        Some(Def::D(ms)) => std::time::Duration::from_millis(ms),
        _ => panic!("{path} is not a duration key"),
    }
}
