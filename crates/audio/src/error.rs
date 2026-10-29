use std::fmt;

/// Structured error: `code` is the stable machine-readable part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioError {
    pub code: &'static str,
    pub message: String,
}

impl AudioError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for AudioError {}

pub type Result<T> = std::result::Result<T, AudioError>;

pub fn io_err(what: &str, e: impl fmt::Display) -> AudioError {
    AudioError::new("io_error", format!("{what}: {e}"))
}
