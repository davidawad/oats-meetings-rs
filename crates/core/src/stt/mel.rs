//! Log-mel front end for Parakeet TDT on CoreML (dot-4uzgol.69), in Rust. It computes what the
//! exported `Preprocessor.mlmodelc` of FluidInference/parakeet-tdt-0.6b-v2-coreml computes (NeMo's
//! `AudioToMelSpectrogramPreprocessor`): pre-emphasis, a reflect-padded STFT with a Hann window,
//! the power spectrum through a Slaney mel filterbank, the log, and per-mel normalisation over the
//! frames that hold audio; frames past the audio are zero. FluidAudio runs that model on the CPU
//! only, so doing it here costs no Neural Engine time and keeps it testable without CoreML: the
//! tests check it against `data/fixtures/parakeet-mel.json`, features the export's own weights give
//! (`ci/parakeet-mel-fixture.py`).

/// The front end's parameters: facts of the model export, not choices.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub n_fft: usize,
    pub win: usize,
    pub hop: usize,
    pub mels: usize,
    pub sample_rate: f32,
    pub preemph: f32,
    /// Added to the mel power before the log.
    pub log_guard: f32,
    /// Added to the standard deviation in the normalisation.
    pub std_guard: f32,
}

/// Parakeet TDT 0.6B v2 (NeMo config `preprocessor`), as exported to CoreML.
pub const PARAKEET: Spec = Spec {
    n_fft: 512,
    win: 400,
    hop: 160,
    mels: 128,
    sample_rate: 16000.0,
    preemph: 0.97,
    log_guard: 1.0 / 16_777_216.0,
    std_guard: 1e-5,
};

pub struct Mel {
    spec: Spec,
    /// `win` Hann samples centred in `n_fft`.
    window: Vec<f32>,
    /// Per mel band: first FFT bin and the weights from it.
    filters: Vec<(usize, Vec<f32>)>,
    fft: Fft,
}

impl Mel {
    pub fn new(spec: Spec) -> Mel {
        let mut window = vec![0.0f32; spec.n_fft];
        let off = (spec.n_fft - spec.win) / 2;
        let d = (spec.win - 1) as f64;
        for i in 0..spec.win {
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / d).cos();
            window[off + i] = w as f32;
        }
        Mel {
            filters: slaney_filters(&spec),
            window,
            fft: Fft::new(spec.n_fft),
            spec,
        }
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// Mel features of `pcm` (16 kHz mono) zero-padded to `frames` frames, row-major
    /// `[mels][frames]` (the encoder's `[1, mels, frames]` input), and how many frames hold audio
    /// (the encoder's `mel_length`). Audio past `(frames - 1) * hop` samples is ignored.
    pub fn features(&self, pcm: &[f32], frames: usize) -> (Vec<f32>, usize) {
        let s = &self.spec;
        let (mut out, valid) = self.log_mel(pcm, frames);
        let frames = frames.max(1);
        for b in 0..s.mels {
            let row = &mut out[b * frames..b * frames + valid];
            let mean = row.iter().map(|&x| f64::from(x)).sum::<f64>() / valid as f64;
            let var = row
                .iter()
                .map(|&x| (f64::from(x) - mean).powi(2))
                .sum::<f64>()
                / (valid.max(2) - 1) as f64;
            let std = var.sqrt() + f64::from(s.std_guard);
            for x in row.iter_mut() {
                *x = ((f64::from(*x) - mean) / std) as f32;
            }
        }
        (out, valid)
    }

    /// `features` before the per-mel normalisation: the log mel power, which keeps the speaker's
    /// spectral shape (the speaker diarizer's input, dot-4uzgol.77).
    pub fn log_mel(&self, pcm: &[f32], frames: usize) -> (Vec<f32>, usize) {
        let s = &self.spec;
        let frames = frames.max(1);
        let n = (frames - 1) * s.hop;
        let len = pcm.len().min(n);
        let mut y = vec![0.0f32; n.max(1)];
        let mut prev = 0.0f32;
        for (i, out) in y.iter_mut().enumerate().take(len) {
            *out = if i == 0 {
                pcm[0]
            } else {
                pcm[i] - s.preemph * prev
            };
            prev = pcm[i];
        }
        if len < n && len > 0 {
            // The first zero after the audio still carries its pre-emphasis.
            y[len] = -s.preemph * pcm[len - 1];
        }
        // Reflect-pad by n_fft / 2 on both sides.
        let pad = s.n_fft / 2;
        let m = y.len();
        let at = |i: isize| -> f32 {
            let j = if i < 0 {
                (-i) as usize
            } else if i as usize >= m {
                // Past one reflection (audio shorter than the pad): zero, not an underflow.
                (2 * (m - 1)).checked_sub(i as usize).unwrap_or(usize::MAX)
            } else {
                i as usize
            };
            y.get(j).copied().unwrap_or(0.0)
        };
        let valid = (len / s.hop + 1).min(frames);
        let bins = s.n_fft / 2 + 1;
        let mut out = vec![0.0f32; s.mels * frames];
        let (mut re, mut im, mut power) = (
            vec![0.0f32; s.n_fft],
            vec![0.0f32; s.n_fft],
            vec![0.0f32; bins],
        );
        for f in 0..valid {
            let start = (f * s.hop) as isize - pad as isize;
            for k in 0..s.n_fft {
                re[k] = at(start + k as isize) * self.window[k];
                im[k] = 0.0;
            }
            self.fft.run(&mut re, &mut im);
            for k in 0..bins {
                power[k] = re[k] * re[k] + im[k] * im[k];
            }
            for (b, (first, w)) in self.filters.iter().enumerate() {
                let e: f32 = w.iter().zip(&power[*first..]).map(|(a, p)| a * p).sum();
                out[b * frames + f] = (e + s.log_guard).ln();
            }
        }
        (out, valid)
    }
}

/// Slaney-scale triangular filters from 0 Hz to Nyquist, area-normalised (librosa `norm="slaney"`,
/// as NeMo builds them).
fn slaney_filters(s: &Spec) -> Vec<(usize, Vec<f32>)> {
    // The Slaney scale: linear below 1 kHz (200/3 Hz per mel), logarithmic above.
    let (f_sp, min_log_hz) = (200.0 / 3.0, 1000.0);
    let min_log_mel = min_log_hz / f_sp;
    let log_step = (6.4f64).ln() / 27.0;
    let to_mel = |f: f64| {
        if f >= min_log_hz {
            min_log_mel + (f / min_log_hz).ln() / log_step
        } else {
            f / f_sp
        }
    };
    let to_hz = |m: f64| {
        if m >= min_log_mel {
            min_log_hz * (log_step * (m - min_log_mel)).exp()
        } else {
            f_sp * m
        }
    };
    let nyquist = f64::from(s.sample_rate) / 2.0;
    let top = to_mel(nyquist);
    let pts: Vec<f64> = (0..s.mels + 2)
        .map(|i| to_hz(top * i as f64 / (s.mels + 1) as f64))
        .collect();
    let bins = s.n_fft / 2 + 1;
    let freq = |k: usize| nyquist * k as f64 / (bins - 1) as f64;
    (0..s.mels)
        .map(|m| {
            let (lo, c, hi) = (pts[m], pts[m + 1], pts[m + 2]);
            let norm = 2.0 / (hi - lo);
            let w: Vec<f32> = (0..bins)
                .map(|k| {
                    let f = freq(k);
                    let v = ((f - lo) / (c - lo)).min((hi - f) / (hi - c)).max(0.0);
                    (v * norm) as f32
                })
                .collect();
            let first = w.iter().position(|&x| x > 0.0).unwrap_or(0);
            let last = w.iter().rposition(|&x| x > 0.0).unwrap_or(first);
            (first, w[first..=last].to_vec())
        })
        .collect()
}

/// In-place radix-2 complex FFT of a fixed power-of-two size.
struct Fft {
    n: usize,
    cos: Vec<f32>,
    sin: Vec<f32>,
    rev: Vec<usize>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two(), "FFT size {n}");
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| {
                if bits == 0 {
                    0
                } else {
                    i.reverse_bits() >> (usize::BITS - bits)
                }
            })
            .collect();
        let ang = |k: usize| -2.0 * std::f64::consts::PI * k as f64 / n as f64;
        Fft {
            n,
            cos: (0..n / 2).map(|k| ang(k).cos() as f32).collect(),
            sin: (0..n / 2).map(|k| ang(k).sin() as f32).collect(),
            rev,
        }
    }

    fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut size = 2;
        while size <= n {
            let half = size / 2;
            let step = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..half {
                    let (c, s) = (self.cos[k * step], self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fft_matches_a_direct_dft() {
        let n = 16;
        let fft = Fft::new(n);
        let x: Vec<f32> = (0..n).map(|i| ((i * 7 % 5) as f32 - 2.0) * 0.3).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; n]);
        fft.run(&mut re, &mut im);
        for k in 0..n {
            let (mut r, mut i) = (0.0f64, 0.0f64);
            for (t, &v) in x.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                r += f64::from(v) * a.cos();
                i += f64::from(v) * a.sin();
            }
            assert!((f64::from(re[k]) - r).abs() < 1e-4, "re {k}");
            assert!((f64::from(im[k]) - i).abs() < 1e-4, "im {k}");
        }
    }

    #[test]
    fn the_window_and_filterbank_have_the_export_shape() {
        let m = Mel::new(PARAKEET);
        // Hann(400) centred in 512: 56 zeros each side, peak 1 at the middle.
        assert_eq!(m.window.iter().position(|&w| w > 0.0), Some(57));
        assert_eq!(m.window[..56].iter().sum::<f32>(), 0.0);
        assert!((m.window[256] - 1.0).abs() < 1e-4);
        assert_eq!(m.filters.len(), 128);
        // Bands rise with frequency; the export's largest weight is about 0.0423.
        let peak = m
            .filters
            .iter()
            .flat_map(|f| f.1.iter())
            .cloned()
            .fold(0.0f32, f32::max);
        assert!((peak - 0.0423).abs() < 1e-3, "{peak}");
        for w in m.filters.windows(2) {
            assert!(w[1].0 >= w[0].0);
        }
    }

    /// The fixture signal: tones and LCG noise under a slow envelope (ci/parakeet-mel-fixture.py).
    fn fixture_signal(n: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        let tau = 2.0 * std::f64::consts::PI;
        (0..n)
            .map(|t| {
                s = (s * 1_103_515_245 + 12345) % (1u64 << 31);
                let noise = s as f64 / (1u64 << 31) as f64 - 0.5;
                let t = t as f64 / 16000.0;
                let x = 0.3 * (tau * 220.0 * t).sin()
                    + 0.2 * (tau * 1300.0 * t).sin()
                    + 0.1 * (tau * 4100.0 * t).sin()
                    + 0.3 * noise;
                (x * (0.5 + 0.5 * (tau * 3.0 * t).sin())) as f32
            })
            .collect()
    }

    #[test]
    fn features_match_the_coreml_preprocessor_fixture() {
        let fx: serde_json::Value =
            serde_json::from_str(include_str!("../../data/fixtures/parakeet-mel.json")).unwrap();
        let n = fx["samples"].as_u64().unwrap() as usize;
        let frames = fx["frames"].as_u64().unwrap() as usize;
        let pcm = fixture_signal(n, fx["pcm_seed"].as_u64().unwrap());
        let (mel, valid) = Mel::new(PARAKEET).features(&pcm, frames);
        assert_eq!(valid, fx["valid"].as_u64().unwrap() as usize);
        assert_eq!(mel.len(), 128 * frames);
        let want = fx["mel"].as_array().unwrap();
        // The export's DFT kernels are int8: below about 1.5 kHz (bands of one to a few FFT bins,
        // little energy after pre-emphasis) its quantisation noise is a visible share of the power,
        // so those bands get a looser bound. Where the signal has energy (the tones' bands) the two
        // agree to about 0.005. Features are normalised to unit variance per band.
        let (mut sum, mut count) = (0.0f64, 0usize);
        for (b, row) in want.iter().enumerate() {
            let diffs: Vec<f32> = row
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(f, v)| (mel[b * frames + f] - v.as_f64().unwrap() as f32).abs())
                .collect();
            let band_mean = diffs.iter().sum::<f32>() / diffs.len() as f32;
            let worst = diffs.iter().cloned().fold(0.0f32, f32::max);
            assert!(band_mean < 0.2, "band {b}: mean abs diff {band_mean}");
            if b >= 40 {
                assert!(worst < 0.2, "band {b}: worst abs diff {worst}");
            }
            sum += diffs.iter().map(|&d| f64::from(d)).sum::<f64>();
            count += diffs.len();
            // Past the audio the features are zero, as the export masks them.
            assert!(mel[b * frames + valid..(b + 1) * frames]
                .iter()
                .all(|&x| x == 0.0));
        }
        let mean = sum / count as f64;
        assert!(mean < 0.02, "mean abs diff {mean}");
    }

    #[test]
    fn short_and_empty_audio() {
        let m = Mel::new(PARAKEET);
        let (mel, valid) = m.features(&[], 11);
        assert_eq!((mel.len(), valid), (128 * 11, 1));
        assert!(mel.iter().all(|x| x.is_finite()));
        let (_, valid) = m.features(&vec![0.1; 1_000_000], 11);
        assert_eq!(valid, 11, "audio past the window is cut");
        // Windows shorter than the n_fft / 2 reflect pad (a recording stopped at once).
        for frames in 1..4 {
            for len in [0, 1, 159, 160, 161, 400] {
                let (mel, _) = m.log_mel(&vec![0.1; len], frames);
                assert!(mel.iter().all(|x| x.is_finite()), "{frames} {len}");
                let (mel, _) = m.features(&vec![0.1; len], frames);
                assert!(mel.iter().all(|x| x.is_finite()), "{frames} {len}");
            }
        }
    }
}
