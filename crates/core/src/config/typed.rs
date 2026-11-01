//! The typed view of a resolved config. Unknown fields are an error (closed vocabulary).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::keys::{format_duration_ms, parse_duration_ms};

/// A duration key's typed value: milliseconds, written and read as `"15s"` / `"250ms"` text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dur(pub u64);

impl Dur {
    pub fn as_duration(self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0)
    }
    pub fn secs(self) -> u64 {
        self.0 / 1000
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_duration_ms(&s).map(Dur).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "'{s}' is not a duration like \"15s\" (units ms, s, m, h, d)"
            ))
        })
    }
}

impl Serialize for Dur {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format_duration_ms(self.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub app: App,
    pub audio: Audio,
    pub config: ConfigSection,
    pub control: Control,
    pub kb: Kb,
    pub ics: Ics,
    pub license: License,
    pub llm: Llm,
    pub network: Network,
    pub notes: Notes,
    pub privacy: Privacy,
    pub secrets: Secrets,
    pub sessions: Sessions,
    pub stt: Stt,
    pub templates: Templates,
    pub ui: Ui,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub menubar: bool,
    pub single_instance: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioBackend {
    Auto,
    Pulse,
    Pipewire,
    Cpal,
    Screencapturekit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioLayout {
    Split,
    Mix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioTranscribe {
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioEndpoint {
    pub device: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    pub backend: AudioBackend,
    pub devices_timeout: Dur,
    pub devices_ttl: Dur,
    pub echo_block: u32,
    pub echo_cancel: bool,
    pub echo_delay: Dur,
    pub echo_divergence: u32,
    pub echo_step: u32,
    pub echo_tail: Dur,
    pub layout: AudioLayout,
    pub mic: AudioEndpoint,
    pub open_timeout: Dur,
    pub poll_interval: Dur,
    pub system: AudioEndpoint,
    pub transcribe: AudioTranscribe,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSection {
    pub hot_reload: bool,
    pub poll_interval: Dur,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub accept_backoff: Dur,
    pub accept_poll: Dur,
    pub drain_chunk: u32,
    pub drain_max: u64,
    pub client_timeout: Dur,
    pub event_queue: u32,
    pub max_line: u64,
    pub socket: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct License {
    pub holder: String,
    pub spdx: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmBackend {
    Ollama,
    Openrouter,
    OpenaiCompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    pub api_key_ref: String,
    pub backend: LlmBackend,
    pub base_url: String,
    pub model: String,
    pub timeout: Dur,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KbEmbedder {
    Hash,
    Ollama,
    OpenaiCompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Kb {
    pub base_url: String,
    pub dir: String,
    pub embedder: KbEmbedder,
    pub hash_dims: u32,
    pub max_chunk: u32,
    pub min_chunk: u32,
    pub model: String,
    pub timeout: Dur,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Notes {
    pub enforce_headings: bool,
    pub default_template: String,
    pub confirm_regenerate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    pub activate_on_show: bool,
    pub drive_wait: Dur,
    pub live_follow: bool,
    pub live_max_lines: u32,
    pub quit_timeout: Dur,
    pub record_controls: bool,
    pub show_elapsed: bool,
    pub speaker_width: u32,
    pub start_hidden: bool,
    pub tick: Dur,
    pub tz: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Templates {
    pub dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub allow: bool,
    pub error_snippet: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HideMethod {
    None,
    SharingType,
    DisplayAffinity,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Privacy {
    pub hide_from_capture: bool,
    pub hide_method: HideMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    Trash,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Sessions {
    pub confirm_delete: bool,
    pub delete_mode: DeleteMode,
    pub max_id_len: u32,
    pub root: String,
    pub search_snippet_chars: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SttBackend {
    Local,
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SttProvider {
    Auto,
    Cpu,
    Coreml,
}

impl SttProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            SttProvider::Auto => "auto",
            SttProvider::Cpu => "cpu",
            SttProvider::Coreml => "coreml",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SttAdapter {
    None,
    OpenaiCompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SttCloud {
    pub adapter: SttAdapter,
    pub key: String,
    pub max_response: u64,
    pub model: String,
    pub timeout: Dur,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SttLocal {
    pub model: String,
    pub piece: Dur,
    pub provider: SttProvider,
    pub model_path: String,
    pub models_dir: String,
    pub model_url: String,
    pub auto_download: bool,
    pub threads: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SttLive {
    pub chunk: Dur,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SttFinal {
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stt {
    pub backend: SttBackend,
    pub cloud: SttCloud,
    #[serde(rename = "final")]
    pub final_: SttFinal,
    pub echo_overlap: u32,
    pub echo_window: Dur,
    pub language: String,
    pub live: SttLive,
    pub local: SttLocal,
    pub feed: Dur,
    pub final_piece: Dur,
    pub pad: Dur,
    pub speaker_them: String,
    pub speaker_you: String,
    pub speech_peak: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IcsAllDay {
    Include,
    Exclude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IcsCancelled {
    Hide,
    Show,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IcsOnBadEvent {
    Error,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IcsUnknownTz {
    Error,
    DefaultTz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IcsWebcalScheme {
    Https,
    Http,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ics {
    pub all_day: IcsAllDay,
    pub allow_file: bool,
    pub cache_dir: String,
    pub cancelled: IcsCancelled,
    pub default_refresh: Dur,
    pub default_tz: String,
    pub enabled: bool,
    pub feeds: String,
    pub max_bytes: u64,
    pub max_name_len: u32,
    pub max_occurrences: u32,
    pub on_bad_event: IcsOnBadEvent,
    pub poll_interval: Dur,
    pub stale_on_error: bool,
    pub timeout: Dur,
    pub unknown_tz: IcsUnknownTz,
    pub webcal_scheme: IcsWebcalScheme,
    pub window_days: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretsBackend {
    Env,
    Keyring,
    EnvThenKeyring,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Secrets {
    pub backend: SecretsBackend,
    pub env_prefix: String,
    pub keyring_service: String,
    pub max_key_len: u32,
}
