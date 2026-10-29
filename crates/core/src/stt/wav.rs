//! WAV in, 16 kHz f32 channels out; and mono 16-bit WAV bytes for the cloud adapter.

use std::io::Cursor;
use std::path::Path;

use crate::config::{ConfigError, Result};

pub const SAMPLE_RATE: u32 = 16_000;

/// Left channel = the microphone ("you"), right = system audio ("them"). A mono file is `you`
/// only. Always at 16 kHz.
#[derive(Clone, Debug, Default)]
pub struct Channels {
    pub you: Vec<f32>,
    pub them: Vec<f32>,
}

impl Channels {
    pub fn seconds(&self) -> f64 {
        self.you.len().max(self.them.len()) as f64 / f64::from(SAMPLE_RATE)
    }
}

fn bad(msg: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("bad_audio", msg.to_string())
}

pub fn read(path: &Path) -> Result<Channels> {
    let bytes = std::fs::read(path)
        .map_err(|e| ConfigError::new("io_error", format!("{}: {e}", path.display())))?;
    decode(&bytes)
        .map_err(|e| ConfigError::new(e.code, format!("{}: {}", path.display(), e.message)))
}

pub fn decode(bytes: &[u8]) -> Result<Channels> {
    let mut r = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|e| bad(format!("not a WAV file: {e}")))?;
    let spec = r.spec();
    let n = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| bad(e))?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(8, 32) - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| bad(e))?
        }
    };
    if spec.sample_rate == 0 {
        return Err(bad("sample rate is 0"));
    }
    let mut chans: Vec<Vec<f32>> = vec![Vec::with_capacity(interleaved.len() / n); n.min(2)];
    for frame in interleaved.chunks(n) {
        for (c, out) in chans.iter_mut().enumerate() {
            out.push(frame.get(c).copied().unwrap_or(0.0));
        }
    }
    let mut it = chans.into_iter().map(|c| resample(&c, spec.sample_rate));
    Ok(Channels {
        you: it.next().unwrap_or_default(),
        them: it.next().unwrap_or_default(),
    })
}

/// Linear resampling to 16 kHz; identity at 16 kHz.
pub fn resample(x: &[f32], from: u32) -> Vec<f32> {
    if from == SAMPLE_RATE || x.is_empty() {
        return x.to_vec();
    }
    let ratio = f64::from(from) / f64::from(SAMPLE_RATE);
    let n = (x.len() as f64 / ratio).floor() as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let i0 = pos.floor() as usize;
            let frac = (pos - i0 as f64) as f32;
            let a = x[i0.min(x.len() - 1)];
            let b = x[(i0 + 1).min(x.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// 16 kHz mono 16-bit PCM WAV bytes.
pub fn encode_mono(pcm: &[f32]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).expect("in-memory wav header");
        for s in pcm {
            let v = (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
            w.write_sample(v).expect("in-memory wav sample");
        }
        w.finalize().expect("in-memory wav finalize");
    }
    buf.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_mono() {
        let x: Vec<f32> = (0..1600).map(|i| (i as f32 / 100.0).sin() * 0.5).collect();
        let c = decode(&encode_mono(&x)).unwrap();
        assert_eq!(c.you.len(), 1600);
        assert!(c.them.is_empty());
        assert!(c.you.iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-3));
    }

    #[test]
    fn stereo_splits_and_resamples() {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 32_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
            for _ in 0..3200 {
                w.write_sample(16_384i16).unwrap(); // left
                w.write_sample(-16_384i16).unwrap(); // right
            }
            w.finalize().unwrap();
        }
        let c = decode(&buf.into_inner()).unwrap();
        assert_eq!(c.you.len(), 1600);
        assert_eq!(c.them.len(), 1600);
        assert!((c.you[10] - 0.5).abs() < 1e-3 && (c.them[10] + 0.5).abs() < 1e-3);
        assert!((c.seconds() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn garbage_is_bad_audio() {
        assert_eq!(decode(b"not a wav").unwrap_err().code, "bad_audio");
    }
}
