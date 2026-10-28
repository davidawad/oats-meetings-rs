//! The cloud adapter: an OpenAI-compatible `POST {url}/audio/transcriptions` (multipart, WAV file
//! plus model/language/response_format=verbose_json). Works against any server that speaks that
//! API, including a local one. Policy (network.allow, the secret) is the caller's.


use serde_json::Value;

use super::{wav, Asr, Segment};
use crate::config::{ConfigError, Result};

pub struct OpenAiAsr {
    pub url: String,
    pub model: String,
    /// Bearer token; `None` sends no Authorization header.
    pub key: Option<String>,
    pub timeout: std::time::Duration,
    /// Characters of an error reply kept (`network.error_snippet`).
    pub snippet: usize,
    /// Largest reply accepted, bytes (`stt.cloud.max_response`).
    pub max_response: u64,
}

fn multipart(boundary: &str, fields: &[(&str, &str)], file: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    for (k, v) in fields {
        b.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n").as_bytes());
    }
    b.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
    );
    b.extend_from_slice(file);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    b
}

fn map_err(e: ureq::Error) -> ConfigError {
    match e {
        ureq::Error::Timeout(_) => ConfigError::new("stt_timeout", e.to_string()),
        other => ConfigError::new("stt_failed", other.to_string()),
    }
}

/// Segments from a `verbose_json` (or plain `{text}`) response body.
pub fn parse_response(body: &str, duration: f64) -> Result<Vec<Segment>> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| ConfigError::new("stt_bad_response", format!("not JSON: {e}")))?;
    if let Some(segs) = v["segments"].as_array() {
        let out: Vec<Segment> = segs
            .iter()
            .filter_map(|s| {
                Some(Segment {
                    start: s["start"].as_f64()?,
                    end: s["end"].as_f64()?,
                    text: s["text"].as_str()?.trim().to_string(),
                })
            })
            .collect();
        if !out.is_empty() {
            return Ok(out);
        }
    }
    match v["text"].as_str() {
        Some(t) => Ok(vec![Segment { start: 0.0, end: duration, text: t.trim().to_string() }]),
        None => Err(ConfigError::new("stt_bad_response", "response has neither segments nor text")),
    }
}

impl Asr for OpenAiAsr {
    fn name(&self) -> &'static str {
        "openai-compatible"
    }

    fn transcribe(&self, pcm: &[f32], language: &str) -> Result<Vec<Segment>> {
        let boundary = "----oatsmeetingsrsboundary7f3a9c";
        let mut fields = vec![("model", self.model.as_str()), ("response_format", "verbose_json")];
        if !language.is_empty() {
            fields.push(("language", language));
        }
        let body = multipart(boundary, &fields, &wav::encode_mono(pcm));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .build()
            .into();
        let url = format!("{}/audio/transcriptions", self.url.trim_end_matches('/'));
        let mut req = agent
            .post(&url)
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .header("Accept", "application/json");
        if let Some(k) = &self.key {
            req = req.header("Authorization", format!("Bearer {k}"));
        }
        let mut resp = req.send(&body[..]).map_err(map_err)?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .with_config()
            .limit(self.max_response)
            .read_to_string()
            .map_err(map_err)?;
        if !(200..300).contains(&status) {
            let hint: String = text.chars().take(self.snippet).collect();
            return Err(ConfigError::new("stt_http_status", format!("{url}: HTTP {status} {hint}")));
        }
        parse_response(&text, pcm.len() as f64 / f64::from(wav::SAMPLE_RATE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_verbose_and_plain() {
        let v = parse_response(r#"{"text":"a b","segments":[{"start":0.0,"end":1.5,"text":" a "},{"start":1.5,"end":2.0,"text":"b"}]}"#, 2.0).unwrap();
        assert_eq!(v, [Segment { start: 0.0, end: 1.5, text: "a".into() }, Segment { start: 1.5, end: 2.0, text: "b".into() }]);
        let p = parse_response(r#"{"text":" only text "}"#, 3.0).unwrap();
        assert_eq!(p, [Segment { start: 0.0, end: 3.0, text: "only text".into() }]);
        assert_eq!(parse_response("nope", 1.0).unwrap_err().code, "stt_bad_response");
        assert_eq!(parse_response("{}", 1.0).unwrap_err().code, "stt_bad_response");
    }

    #[test]
    fn multipart_shape() {
        let b = multipart("B", &[("model", "m")], b"RIFFxx");
        let s = String::from_utf8_lossy(&b).to_string();
        assert!(s.starts_with("--B\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nm\r\n"));
        assert!(s.contains("filename=\"audio.wav\"") && s.contains("RIFFxx") && s.ends_with("--B--\r\n"));
    }
}
