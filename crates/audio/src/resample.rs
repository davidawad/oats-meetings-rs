//! Downmix to mono and linear-interpolation resample to a target rate. Streaming: keeps the
//! fractional position and the last sample across calls.

pub struct Resampler {
    step: f64, // source samples per output sample
    pos: f64,  // position of the next output sample, in source samples, relative to `prev`
    prev: Option<f32>,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: f64::from(from_rate) / f64::from(to_rate),
            pos: 0.0,
            prev: None,
        }
    }

    /// Push mono samples; append resampled output to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.step - 1.0).abs() < f64::EPSILON {
            out.extend_from_slice(input);
            return;
        }
        for &cur in input {
            let Some(prev) = self.prev else {
                self.prev = Some(cur);
                out.push(cur);
                self.pos = self.step; // next output is `step` source samples after the first
                continue;
            };
            // `pos` is the next output position relative to `prev`; `cur` sits at 1.0.
            while self.pos < 1.0 {
                let f = self.pos as f32;
                out.push(prev + (cur - prev) * f);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = Some(cur);
        }
    }
}

/// 48 kHz -> 16 kHz by averaging each group of three samples (a box filter: cheap, and enough
/// anti-aliasing for speech). Streaming: a partial group carries to the next call.
#[derive(Default)]
pub struct Decimate3 {
    carry: Vec<f32>,
}

impl Decimate3 {
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.carry.extend_from_slice(input);
        let whole = self.carry.len() / 3 * 3;
        out.extend(
            self.carry[..whole]
                .chunks_exact(3)
                .map(|g| (g[0] + g[1] + g[2]) / 3.0),
        );
        self.carry.drain(..whole);
    }
}

/// Average interleaved frames of `channels` into mono.
pub fn downmix(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    out.extend(
        interleaved
            .chunks_exact(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_at_equal_rates() {
        let mut r = Resampler::new(48000, 48000);
        let mut out = vec![];
        r.push(&[0.1, 0.2, 0.3], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn length_scales_with_rate_ratio_across_chunks() {
        for (from, to) in [
            (44100u32, 48000u32),
            (16000, 48000),
            (96000, 48000),
            (48000, 44100),
        ] {
            let mut r = Resampler::new(from, to);
            let mut out = vec![];
            let src: Vec<f32> = (0..from as usize)
                .map(|i| (i as f32 * 0.01).sin())
                .collect();
            for chunk in src.chunks(441) {
                r.push(chunk, &mut out);
            }
            let want = to as i64;
            assert!(
                (out.len() as i64 - want).abs() <= 4,
                "{from}->{to}: {} vs {want}",
                out.len()
            );
        }
    }

    #[test]
    fn preserves_a_dc_level() {
        let mut r = Resampler::new(44100, 48000);
        let mut out = vec![];
        r.push(&vec![0.5; 4410], &mut out);
        assert!(out.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn decimate3_thirds_across_chunks() {
        let mut d = Decimate3::default();
        let mut out = vec![];
        d.push(&[3.0, 3.0], &mut out);
        assert!(out.is_empty());
        d.push(&[3.0, 6.0, 6.0, 6.0, 9.0], &mut out);
        assert_eq!(out, vec![3.0, 6.0]);
        d.push(&[9.0, 9.0], &mut out);
        assert_eq!(out, vec![3.0, 6.0, 9.0]);
    }

    #[test]
    fn downmix_averages() {
        let mut o = vec![];
        downmix(&[1.0, 0.0, 0.5, 0.5], 2, &mut o);
        assert_eq!(o, vec![0.5, 0.5]);
    }
}
