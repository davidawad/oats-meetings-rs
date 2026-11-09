//! Minimal 16-bit PCM WAV writer and reader (no dependency). The header is patched on `finish`.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::{io_err, AudioError, Result};

pub struct WavWriter {
    w: BufWriter<File>,
    channels: u16,
    rate: u32,
    frames: u64,
    path: String,
}

impl WavWriter {
    pub fn create(path: &Path, channels: u16, rate: u32) -> Result<Self> {
        let p = path.display().to_string();
        let f = File::create(path).map_err(|e| io_err(&p, e))?;
        let mut me = Self {
            w: BufWriter::new(f),
            channels,
            rate,
            frames: 0,
            path: p,
        };
        me.header(0).map_err(|e| io_err(&me.path, e))?;
        Ok(me)
    }

    fn header(&mut self, data_bytes: u32) -> std::io::Result<()> {
        let ch = self.channels;
        let block = ch * 2;
        let w = &mut self.w;
        w.write_all(b"RIFF")?;
        w.write_all(&(36 + data_bytes).to_le_bytes())?;
        w.write_all(b"WAVEfmt ")?;
        w.write_all(&16u32.to_le_bytes())?;
        w.write_all(&1u16.to_le_bytes())?;
        w.write_all(&ch.to_le_bytes())?;
        w.write_all(&self.rate.to_le_bytes())?;
        w.write_all(&(self.rate * u32::from(block)).to_le_bytes())?;
        w.write_all(&block.to_le_bytes())?;
        w.write_all(&16u16.to_le_bytes())?;
        w.write_all(b"data")?;
        w.write_all(&data_bytes.to_le_bytes())
    }

    /// `interleaved.len()` must be a multiple of the channel count.
    pub fn write(&mut self, interleaved: &[f32]) -> Result<()> {
        let mut buf = Vec::with_capacity(interleaved.len() * 2);
        for s in interleaved {
            buf.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        self.w.write_all(&buf).map_err(|e| io_err(&self.path, e))?;
        self.frames += (interleaved.len() / usize::from(self.channels)) as u64;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn finish(mut self) -> Result<u64> {
        let bytes = (self.frames * u64::from(self.channels) * 2) as u32;
        let path = self.path.clone();
        let res = (|| -> std::io::Result<()> {
            self.w.flush()?;
            self.w.seek(SeekFrom::Start(0))?;
            self.header(bytes)?;
            self.w.flush()
        })();
        res.map_err(|e| io_err(&path, e))?;
        Ok(self.frames)
    }
}

/// Cut a WAV this module wrote (the 44-byte header) to its first `frames` frames, in place: the
/// trailing silence an auto-stop drops (dot-4uzgol.110). Returns the frames it keeps (fewer when
/// the file is shorter).
pub fn truncate(path: &Path, frames: u64) -> Result<u64> {
    let p = path.display().to_string();
    let bad = |m: &str| AudioError::new("invalid_wav", format!("{p}: {m}"));
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| io_err(&p, e))?;
    let mut h = [0u8; 44];
    f.read_exact(&mut h).map_err(|e| io_err(&p, e))?;
    if &h[0..4] != b"RIFF" || &h[8..12] != b"WAVE" || &h[36..40] != b"data" {
        return Err(bad("not a WAV with its data at byte 44"));
    }
    let channels = u64::from(u16::from_le_bytes([h[22], h[23]]));
    let block = channels * 2;
    if block == 0 {
        return Err(bad("no channels"));
    }
    let len = f.metadata().map_err(|e| io_err(&p, e))?.len();
    let keep = frames.min(len.saturating_sub(44) / block);
    let bytes = keep * block;
    let res = (|| -> std::io::Result<()> {
        f.set_len(44 + bytes)?;
        f.seek(SeekFrom::Start(4))?;
        f.write_all(&((36 + bytes) as u32).to_le_bytes())?;
        f.seek(SeekFrom::Start(40))?;
        f.write_all(&(bytes as u32).to_le_bytes())?;
        f.flush()
    })();
    res.map_err(|e| io_err(&p, e))?;
    Ok(keep)
}

pub struct WavData {
    pub channels: u16,
    pub rate: u32,
    /// One Vec per channel, samples scaled to -1..1.
    pub samples: Vec<Vec<f32>>,
}

impl WavData {
    pub fn frames(&self) -> usize {
        self.samples.first().map_or(0, Vec::len)
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate.max(1))
    }

    pub fn rms(&self, channel: usize) -> f64 {
        let s = &self.samples[channel];
        if s.is_empty() {
            return 0.0;
        }
        (s.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
    }

    /// Rising zero crossings per second: a cheap dominant-frequency estimate for a tone.
    pub fn tone_hz(&self, channel: usize) -> f64 {
        let s = &self.samples[channel];
        // Measured over the audible span: a source that lagged (CPU starvation) is zero-padded
        // at the tail on stop, and that padding is not part of the tone.
        let end = s.iter().rposition(|x| *x != 0.0).map_or(0, |i| i + 1);
        let ups = s[..end]
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        ups as f64 / (end as f64 / f64::from(self.rate.max(1))).max(1e-9)
    }
}

pub fn read(path: &Path) -> Result<WavData> {
    let p = path.display().to_string();
    let mut b = vec![];
    File::open(path)
        .and_then(|mut f| f.read_to_end(&mut b))
        .map_err(|e| io_err(&p, e))?;
    let bad = |m: &str| AudioError::new("invalid_wav", format!("{p}: {m}"));
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad("not a RIFF/WAVE file"));
    }
    let mut pos = 12;
    let (mut channels, mut rate, mut bits) = (0u16, 0u32, 0u16);
    let mut data: &[u8] = &[];
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = u32::from_le_bytes(b[pos + 4..pos + 8].try_into().expect("4")) as usize;
        let body = &b[pos + 8..(pos + 8 + len).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().expect("4"));
            bits = u16::from_le_bytes([body[14], body[15]]);
        } else if id == b"data" {
            data = body;
            break;
        }
        pos += 8 + len + (len & 1);
    }
    if channels == 0 || bits != 16 {
        return Err(bad("expected 16-bit PCM"));
    }
    let mut samples = vec![vec![]; usize::from(channels)];
    for frame in data.chunks_exact(usize::from(channels) * 2) {
        for (c, s) in frame.chunks_exact(2).enumerate() {
            samples[c].push(f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0);
        }
    }
    Ok(WavData {
        channels,
        rate,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_two_channels() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let mut w = WavWriter::create(&p, 2, 48000).unwrap();
        let mut buf = vec![];
        for i in 0..48000 {
            let t = i as f32 / 48000.0;
            buf.push((t * 440.0 * std::f32::consts::TAU).sin() * 0.5);
            buf.push(0.0);
        }
        w.write(&buf).unwrap();
        assert_eq!(w.finish().unwrap(), 48000);
        let r = read(&p).unwrap();
        assert_eq!((r.channels, r.rate, r.frames()), (2, 48000, 48000));
        assert!((r.duration_secs() - 1.0).abs() < 1e-9);
        assert!((r.rms(0) - 0.3535).abs() < 0.01);
        assert!(r.rms(1) < 1e-6);
        assert!((r.tone_hz(0) - 440.0).abs() < 2.0);
    }

    #[test]
    fn truncate_keeps_the_first_frames_and_a_valid_header() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        let mut w = WavWriter::create(&p, 2, 48000).unwrap();
        w.write(&vec![0.25; 2 * 48000]).unwrap();
        w.finish().unwrap();
        assert_eq!(truncate(&p, 12000).unwrap(), 12000);
        let r = read(&p).unwrap();
        assert_eq!((r.channels, r.frames()), (2, 12000));
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 44 + 12000 * 4);
        // Past the end keeps everything there is.
        assert_eq!(truncate(&p, 99999).unwrap(), 12000);
        std::fs::write(&p, b"nope").unwrap();
        assert!(truncate(&p, 1).is_err());
    }
}
