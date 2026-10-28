//! The typed view of a resolved config. Unknown fields are an error (closed vocabulary).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub app: App,
    pub config: ConfigSection,
    pub control: Control,
    pub ics: Ics,
    pub license: License,
    pub llm: Llm,
    pub network: Network,
    pub privacy: Privacy,
    pub secrets: Secrets,
    pub sessions: Sessions,
    pub stt: Stt,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub menubar: bool,
    pub single_instance: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSection {
    pub hot_reload: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
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
    pub backend: LlmBackend,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub allow: bool,
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
    pub root: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SttBackend {
    Local,
    Cloud,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SttLocal {
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stt {
    pub backend: SttBackend,
    pub cloud: SttCloud,
    pub local: SttLocal,
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
    pub default_refresh_secs: String,
    pub default_tz: String,
    pub enabled: bool,
    pub feeds: String,
    pub max_bytes: String,
    pub max_occurrences: String,
    pub on_bad_event: IcsOnBadEvent,
    pub stale_on_error: bool,
    pub timeout_secs: String,
    pub unknown_tz: IcsUnknownTz,
    pub webcal_scheme: IcsWebcalScheme,
    pub window_days: String,
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
}
