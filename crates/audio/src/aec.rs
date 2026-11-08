//! Acoustic echo cancellation of the mic channel (dot-4uzgol.65).
//!
//! Remote speech played through the speakers reaches the mic and would be transcribed a second
//! time as the local speaker. The system-audio channel is exactly what the speakers played, so it
//! is the far-end reference: a partitioned-block frequency-domain NLMS filter (overlap-save, one
//! FFT per block, the gradient constrained to the linear part) learns the speaker-to-mic path and
//! subtracts its estimate from the mic. Two filters run side by side: the background one always
//! adapts, the foreground one (the output) copies it only when it does clearly better, and a background
//! that local speech (double talk) has pulled off course is reset, so talking over the far end
//! does not detune the output.
//!
//! Pure Rust and the same on every OS (no webrtc build, no VoiceProcessingIO). The capture
//! streams are not sample-synchronous: the mic is delayed by `delay` inside the canceller so a
//! reference that arrives a little after its echo still precedes it. Both outputs stay on the
//! input timeline (the delay is latency, not a shift), so the channels stay aligned in the WAV.
//!
//! The capture paths differ in latency (dot-4uzgol.98: a Core Audio process tap delivers the
//! system audio with another offset to the mic than ScreenCaptureKit), so a fixed `delay` can
//! leave the echo outside the modelled tail, and whole sentences leak. Until it is confident, a
//! probe cross-correlates the block energy envelopes of mic and reference over `align` either
//! way; when the measured lag puts the echo outside the tail, the filter is rebuilt with the mic
//! or the reference delayed so the echo again lands `delay` after its reference.
//!
//! A linear filter cannot cancel what the speakers distort (dot-4uzgol.101: on a laptop's
//! speakers the echo came back garbled and was transcribed as the local speaker). As in WebRTC's
//! AEC3, a residual echo suppressor follows: per frequency bin it estimates the echo the filter
//! left from the power of the echo it did model, and attenuates bins where that estimate
//! dominates the filter's output. It runs
//! on 50%-overlapped sqrt-Hann frames (one block of extra latency, still on the input timeline);
//! local speech, which the reference cannot predict, keeps its bins.

use std::collections::VecDeque;
use std::time::Duration;

use crate::SAMPLE_RATE;

/// `audio.echo_*`, filled by `oats-core` from the key table (no defaults here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EchoSettings {
    /// Longest speaker-to-mic echo the filter models (`audio.echo_tail`).
    pub tail: Duration,
    /// Mic delay that lets a late reference still precede its echo (`audio.echo_delay`).
    pub delay: Duration,
    /// Samples per block, rounded up to a power of two (`audio.echo_block`).
    pub block: usize,
    /// NLMS step size in per-mille (`audio.echo_step`).
    pub step_permille: u32,
    /// The adapting filter is reset to the output filter when its residual exceeds this
    /// percentage of the output filter's (`audio.echo_divergence`).
    pub divergence_percent: u32,
    /// Far-end activity before the filter counts as converged (`audio.echo_warmup`): until
    /// then it adapts with `warmup_step_permille` and mutes the mic where echo dominates.
    pub warmup: Duration,
    /// NLMS step during the warmup, per-mille (`audio.echo_warmup_step`).
    pub warmup_step_permille: u32,
    /// During the warmup a mic stretch whose residual is below this percentage of its energy is
    /// mostly echo and is muted (`audio.echo_dominance`).
    pub dominance_percent: u32,
    /// Largest reference-to-mic offset, either way, measured by cross-correlation at the start
    /// (`audio.echo_align`); zero keeps `delay` as the only alignment.
    pub align: Duration,
    /// Far-end sound the measurement collects before it decides (`audio.echo_align_probe`).
    pub align_probe: Duration,
    /// Least normalised correlation, percent, at which the measured offset is used
    /// (`audio.echo_align_confidence`); below it the probe starts over.
    pub align_confidence_percent: u32,
    /// Residual echo suppression after the linear filter, percent over-subtraction of the echo
    /// estimate (`audio.echo_suppress`); 0 turns it off.
    pub suppress_percent: u32,
    /// Least gain, percent of amplitude, the suppressor leaves in a band (`audio.echo_suppress_floor`).
    pub suppress_floor_percent: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct C {
    re: f32,
    im: f32,
}

impl C {
    fn mul(self, o: C) -> C {
        C {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn conj_mul(self, o: C) -> C {
        // conj(self) * o
        C {
            re: self.re * o.re + self.im * o.im,
            im: self.re * o.im - self.im * o.re,
        }
    }
    fn norm(self) -> f32 {
        self.re * self.re + self.im * self.im
    }
}

/// In-place iterative radix-2 FFT of one power-of-two size.
struct Fft {
    n: usize,
    rev: Vec<usize>,
    tw: Vec<C>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        let bits = n.trailing_zeros();
        let rev = (0..n)
            .map(|i| i.reverse_bits() >> (usize::BITS - bits))
            .collect();
        let tw = (0..n / 2)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                C {
                    re: a.cos() as f32,
                    im: a.sin() as f32,
                }
            })
            .collect();
        Fft { n, rev, tw }
    }

    /// Forward transform, or the inverse (conjugate twiddles, scaled by 1/n).
    fn run(&self, x: &mut [C], inverse: bool) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i];
            if i < j {
                x.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..len / 2 {
                    let mut w = self.tw[k * step];
                    if inverse {
                        w.im = -w.im;
                    }
                    let a = x[start + k];
                    let b = x[start + k + len / 2].mul(w);
                    x[start + k] = C {
                        re: a.re + b.re,
                        im: a.im + b.im,
                    };
                    x[start + k + len / 2] = C {
                        re: a.re - b.re,
                        im: a.im - b.im,
                    };
                }
            }
            len *= 2;
        }
        if inverse {
            let s = 1.0 / n as f32;
            for v in x.iter_mut() {
                v.re *= s;
                v.im *= s;
            }
        }
    }
}

fn samples(d: Duration) -> usize {
    (d.as_millis() * u128::from(SAMPLE_RATE) / 1000) as usize
}

/// One adaptive filter at a fixed alignment: the mic delayed by `mic_delay` samples and the
/// reference the filter sees by `ref_delay`.
struct Filter {
    b: usize,
    fft: Fft,
    step: f32,
    divergence: f32,
    warmup_step: f32,
    dominance: f32,
    /// Reference blocks with sound still to see before the warmup ends.
    warmup_left: usize,
    /// The last 2b reference samples (overlap-save input).
    x_time: Vec<f32>,
    /// Reference spectra, newest first, one per partition.
    xf: VecDeque<Vec<C>>,
    /// Output (foreground) and adapting (background) filter partitions, frequency domain.
    fg: Vec<Vec<C>>,
    bg: Vec<Vec<C>>,
    /// Tail-averaged residual energy of each path.
    s_fg: f32,
    s_bg: f32,
    /// Tail-averaged mic energy and adapting-path residual, for the warmup mute.
    s_mic: f32,
    s_res: f32,
    ref_in: VecDeque<f32>,
    mic_in: VecDeque<f32>,
    ref_out: VecDeque<f32>,
    pushed: usize,
    emitted: usize,
    /// Output samples still to drop: the silence the delay line (and the suppressor's frame)
    /// was primed with.
    skip: usize,
    suppress: Option<Suppressor>,
}

impl Filter {
    fn new(s: &EchoSettings, delay: usize, ref_delay: usize) -> Filter {
        let b = block_of(s);
        let n = 2 * b;
        let parts = samples(s.tail).div_ceil(b).max(1);
        let suppress = Suppressor::new(s, b, parts);
        // The suppressor's overlap-add frame is one block more latency.
        let latency = delay + suppress.as_ref().map_or(0, |_| b);
        Filter {
            b,
            fft: Fft::new(n),
            step: s.step_permille as f32 / 1000.0,
            divergence: s.divergence_percent as f32 / 100.0,
            warmup_step: s.warmup_step_permille.max(s.step_permille) as f32 / 1000.0,
            dominance: s.dominance_percent as f32 / 100.0,
            warmup_left: samples(s.warmup).div_ceil(b),
            x_time: vec![0.0; n],
            xf: (0..parts).map(|_| vec![C::default(); n]).collect(),
            fg: (0..parts).map(|_| vec![C::default(); n]).collect(),
            bg: (0..parts).map(|_| vec![C::default(); n]).collect(),
            s_fg: 0.0,
            s_bg: 0.0,
            s_mic: 0.0,
            s_res: 0.0,
            ref_in: std::iter::repeat_n(0.0, ref_delay).collect(),
            mic_in: std::iter::repeat_n(0.0, delay).collect(),
            ref_out: std::iter::repeat_n(0.0, latency).collect(),
            pushed: 0,
            emitted: 0,
            skip: latency,
            suppress,
        }
    }

    fn process(
        &mut self,
        mic: &[f32],
        reference: &[f32],
        mic_out: &mut Vec<f32>,
        ref_out: &mut Vec<f32>,
    ) {
        self.mic_in.extend(mic);
        self.ref_in.extend(reference);
        self.ref_out.extend(reference);
        self.pushed += mic.len();
        while self.ref_in.len() >= self.b && self.mic_in.len() >= self.b {
            self.block(mic_out, ref_out);
        }
    }

    fn flush(&mut self, mic_out: &mut Vec<f32>, ref_out: &mut Vec<f32>) {
        let target = self.pushed;
        let zeros = vec![0.0; self.b];
        while self.emitted < target {
            self.process(&zeros, &zeros, mic_out, ref_out);
        }
        let extra = self.emitted - target;
        mic_out.truncate(mic_out.len() - extra);
        ref_out.truncate(ref_out.len() - extra);
        self.pushed = self.emitted;
    }

    /// The echo `w` predicts for the current block: the last b samples of IFFT(sum W_p X_p).
    fn estimate(&self, w: &[Vec<C>], y: &mut [C]) {
        y.iter_mut().for_each(|c| *c = C::default());
        for (w, x) in w.iter().zip(&self.xf) {
            for ((yk, wk), xk) in y.iter_mut().zip(w).zip(x) {
                let p = wk.mul(*xk);
                yk.re += p.re;
                yk.im += p.im;
            }
        }
        self.fft.run(y, true);
    }

    fn block(&mut self, mic_out: &mut Vec<f32>, ref_out: &mut Vec<f32>) {
        let (b, n) = (self.b, 2 * self.b);
        // Slide the reference window and transform it into the newest partition.
        self.x_time.copy_within(b.., 0);
        for i in 0..b {
            self.x_time[b + i] = self.ref_in.pop_front().unwrap_or(0.0);
        }
        let mut x0 = self.xf.pop_back().unwrap_or_else(|| vec![C::default(); n]);
        for (c, &v) in x0.iter_mut().zip(&self.x_time) {
            *c = C { re: v, im: 0.0 };
        }
        self.fft.run(&mut x0, false);
        self.xf.push_front(x0);

        let d: Vec<f32> = self.mic_in.drain(..b).collect();
        let (mut yf, mut yb) = (vec![C::default(); n], vec![C::default(); n]);
        self.estimate(&self.fg, &mut yf);
        self.estimate(&self.bg, &mut yb);
        let e_fg: Vec<f32> = d.iter().zip(&yf[b..]).map(|(d, y)| d - y.re).collect();
        let e_bg: Vec<f32> = d.iter().zip(&yb[b..]).map(|(d, y)| d - y.re).collect();
        let energy = |e: &[f32]| e.iter().map(|v| v * v).sum::<f32>();
        let a = 1.0 / self.fg.len() as f32;

        // Warmup (dot-4uzgol.75): a fresh filter removes only a few dB of the first far-end
        // sentence, enough for it to be transcribed as local speech. Until `warmup` of far-end
        // sound has passed, adapt with the larger step and mute the mic wherever the adapting
        // filter already explains most of it: echo, not local speech, which the reference
        // cannot predict. A silent reference predicts nothing, so it never mutes.
        let warm = self.warmup_left > 0;
        if warm && energy(&self.x_time[b..]) > 0.0 {
            self.warmup_left -= 1;
        }
        self.s_mic += a * (energy(&d) - self.s_mic);
        self.s_res += a * (energy(&e_bg) - self.s_res);
        let mute = warm && self.s_res < self.dominance * self.s_mic;
        let k = self.skip.min(b);
        self.skip -= k;
        let out = match self.suppress.as_mut() {
            Some(sp) => {
                let y: Vec<f32> = d.iter().zip(&e_fg).map(|(d, e)| d - e).collect();
                sp.block(&self.fft, &y, &e_fg, mute)
            }
            None if mute => vec![0.0; b],
            None => e_fg.clone(),
        };
        mic_out.extend_from_slice(&out[k..]);
        ref_out.extend(self.ref_out.drain(..b).skip(k));
        self.emitted += b - k;

        // Two paths: the background filter always adapts; the foreground (the output) takes its
        // coefficients when it does better by the `divergence` factor, and a background that
        // local speech has pulled worse by that factor is reset to the foreground. Residual
        // energies are averaged over the tail.
        self.s_fg += a * (energy(&e_fg) - self.s_fg);
        self.s_bg += a * (energy(&e_bg) - self.s_bg);
        if self.s_bg * self.divergence < self.s_fg {
            self.fg.clone_from(&self.bg);
            self.s_fg = self.s_bg;
        } else if self.s_bg > self.s_fg * self.divergence {
            self.bg.clone_from(&self.fg);
            self.s_bg = self.s_fg;
            return;
        }

        let mut ef = vec![C::default(); n];
        for (c, &v) in ef[b..].iter_mut().zip(&e_bg) {
            c.re = v;
        }
        self.fft.run(&mut ef, false);
        // Per-bin normalisation, regularised by the mean over bins: a bin where the reference
        // has only leakage (a narrowband far end) must not get a huge step and learn to predict
        // unrelated local sound. The epsilon term keeps a silent reference from dividing by zero.
        let mut norm = vec![0.0f32; n];
        for x in &self.xf {
            for (nk, xk) in norm.iter_mut().zip(x) {
                *nk += xk.norm();
            }
        }
        let mean = norm.iter().sum::<f32>() / n as f32 + n as f32 * f32::EPSILON;
        norm.iter_mut().for_each(|v| *v += mean);
        let step = if warm { self.warmup_step } else { self.step };
        let g = &mut yb;
        for (w, x) in self.bg.iter_mut().zip(&self.xf) {
            for k in 0..n {
                let mu = step / norm[k];
                let v = x[k].conj_mul(ef[k]);
                g[k] = C {
                    re: v.re * mu,
                    im: v.im * mu,
                };
            }
            // Constrain the update to a causal b-tap partition (drop the circular half).
            self.fft.run(g, true);
            g[b..].iter_mut().for_each(|c| *c = C::default());
            self.fft.run(g, false);
            for (wk, gk) in w.iter_mut().zip(g.iter()) {
                wk.re += gk.re;
                wk.im += gk.im;
            }
        }
    }
}

/// Residual echo suppression (`EchoSettings::suppress_percent`), one block behind the filter.
struct Suppressor {
    b: usize,
    beta: f32,
    floor: f32,
    /// Per-block decay of the echo power estimate: the echo's own reverberation over the tail.
    decay: f32,
    /// sqrt-Hann over 2b samples: analysis and synthesis windows at 50% overlap sum to one.
    win: Vec<f32>,
    /// The previous block of the filter's echo estimate and output (the first half of the frame).
    prev_y: Vec<f32>,
    prev_e: Vec<f32>,
    /// Overlap-add carry of the last frame's second half.
    carry: Vec<f32>,
    /// Per bin: the echo power estimate, held over the tail.
    echo: Vec<f32>,
    /// Whether the previous block was muted by the warmup (it is emitted now).
    prev_mute: bool,
}

impl Suppressor {
    fn new(s: &EchoSettings, b: usize, parts: usize) -> Option<Suppressor> {
        (s.suppress_percent > 0).then(|| {
            let n = 2 * b;
            Suppressor {
                b,
                beta: s.suppress_percent as f32 / 100.0,
                floor: (s.suppress_floor_percent as f32 / 100.0).min(1.0),
                decay: 1.0 - 1.0 / parts as f32,
                win: (0..n)
                    .map(|i| (std::f32::consts::PI * (i as f32 + 0.5) / n as f32).sin())
                    .collect(),
                prev_y: vec![0.0; b],
                prev_e: vec![0.0; b],
                carry: vec![0.0; b],
                echo: vec![0.0; n],
                prev_mute: false,
            }
        })
    }

    fn spectrum(&self, fft: &Fft, prev: &[f32], cur: &[f32]) -> Vec<C> {
        let mut f: Vec<C> = prev
            .iter()
            .chain(cur)
            .zip(&self.win)
            .map(|(v, w)| C { re: v * w, im: 0.0 })
            .collect();
        fft.run(&mut f, false);
        f
    }

    /// Take the filter's echo estimate `y` and output `e` for one block; return the previous
    /// block's output, echo suppressed (zeros where the warmup muted it).
    ///
    /// What the speakers distort is not in `y`, but it sits in the same bands at the same
    /// moments and scales with it: the residual echo is estimated as `beta` times the linear
    /// estimate's power, spread to the neighbouring bins (distortion smears a harmonic) and held
    /// over the tail as it decays. Local speech, which the reference cannot predict, has no
    /// linear estimate and keeps its bins.
    fn block(&mut self, fft: &Fft, y: &[f32], e: &[f32], mute: bool) -> Vec<f32> {
        let (b, n) = (self.b, 2 * self.b);
        let ym = self.spectrum(fft, &self.prev_y, y);
        let mut em = self.spectrum(fft, &self.prev_e, e);
        let tiny = f32::EPSILON * n as f32;
        for k in 0..n {
            let near = ym[k.saturating_sub(1)]
                .norm()
                .max(ym[k].norm())
                .max(ym[(k + 1).min(n - 1)].norm());
            self.echo[k] = near.max(self.decay * self.echo[k]);
        }
        for (k, c) in em.iter_mut().enumerate() {
            let pe = c.norm();
            let g = if pe > tiny {
                (1.0 - self.beta * self.echo[k] / pe).max(self.floor)
            } else {
                1.0
            };
            c.re *= g;
            c.im *= g;
        }
        fft.run(&mut em, true);
        let mut out: Vec<f32> = (0..b)
            .map(|i| self.carry[i] + em[i].re * self.win[i])
            .collect();
        for i in 0..b {
            self.carry[i] = em[b + i].re * self.win[b + i];
        }
        if self.prev_mute {
            out.iter_mut().for_each(|v| *v = 0.0);
        }
        self.prev_mute = mute;
        self.prev_y.copy_from_slice(y);
        self.prev_e.copy_from_slice(e);
        out
    }
}

fn block_of(s: &EchoSettings) -> usize {
    s.block.max(2).next_power_of_two()
}

/// Measures the reference-to-mic lag from block energy envelopes (`EchoSettings::align`).
struct Probe {
    b: usize,
    /// Lags searched either way, in blocks.
    reach: usize,
    /// Far-end blocks with sound needed before deciding.
    need: usize,
    confidence: f32,
    /// Energy of the block being summed, and its length so far: (mic, reference, samples).
    acc: (f32, f32, usize),
    mic: VecDeque<f32>,
    far: VecDeque<f32>,
}

impl Probe {
    fn new(s: &EchoSettings) -> Option<Probe> {
        let b = block_of(s);
        let reach = samples(s.align).div_ceil(b);
        (reach > 0).then(|| Probe {
            b,
            reach,
            need: samples(s.align_probe).div_ceil(b).max(1),
            confidence: s.align_confidence_percent as f32 / 100.0,
            acc: (0.0, 0.0, 0),
            mic: VecDeque::new(),
            far: VecDeque::new(),
        })
    }

    /// Take in the input; once enough far-end sound has passed, the lag (mic behind the
    /// reference, samples; negative when the reference is the late one) if the envelopes agree.
    fn push(&mut self, mic: &[f32], reference: &[f32]) -> Option<isize> {
        let mut found = None;
        for (m, x) in mic.iter().zip(reference) {
            self.acc.0 += m * m;
            self.acc.1 += x * x;
            self.acc.2 += 1;
            if self.acc.2 < self.b {
                continue;
            }
            self.mic.push_back(self.acc.0);
            self.far.push_back(self.acc.1);
            self.acc = (0.0, 0.0, 0);
            // Long far-end silence must not grow the window without bound.
            let keep = 2 * (self.need + self.reach);
            while self.far.len() > keep {
                self.far.pop_front();
                self.mic.pop_front();
            }
            let full = self.far.len() >= self.need + 2 * self.reach;
            if found.is_none() && full && self.far.iter().filter(|e| **e > 0.0).count() >= self.need
            {
                found = self.lag();
                if found.is_none() {
                    // Not confident (double talk, a quiet echo): measure afresh.
                    self.far.clear();
                    self.mic.clear();
                }
            }
        }
        found
    }

    /// The block lag with the highest normalised correlation of the envelopes, if high enough.
    fn lag(&self) -> Option<isize> {
        let (m, x): (Vec<f32>, Vec<f32>) = (
            self.mic.iter().map(|e| e.sqrt()).collect(),
            self.far.iter().map(|e| e.sqrt()).collect(),
        );
        let n = m.len() as isize;
        let reach = self.reach as isize;
        let mut best: Option<(f32, isize)> = None;
        for k in -reach..=reach {
            // The same mic blocks against far[i - k] at every lag, so no lag wins on fewer pairs.
            let (lo, hi) = (reach, n - reach);
            let pairs = || (lo..hi).map(|i| (m[i as usize], x[(i - k) as usize]));
            let len = (hi - lo) as f32;
            let (mm, mx) = pairs().fold((0.0, 0.0), |(a, b), (p, q)| (a + p, b + q));
            let (mm, mx) = (mm / len, mx / len);
            let (mut sxy, mut sxx, mut syy) = (0.0f32, 0.0f32, 0.0f32);
            for (p, q) in pairs() {
                let (p, q) = (p - mm, q - mx);
                sxy += p * q;
                sxx += p * p;
                syy += q * q;
            }
            let r = sxy / (sxx * syy).sqrt().max(f32::MIN_POSITIVE);
            if best.is_none_or(|(br, _)| r > br) {
                best = Some((r, k));
            }
        }
        best.filter(|(r, _)| *r >= self.confidence)
            .map(|(_, k)| k * self.b as isize)
    }
}

/// Streaming canceller: push equal-length mic and reference runs, get the echo-free mic and the
/// matching reference back, sample for sample. Output lags input by up to one block plus the mic
/// delay; `flush` returns the rest so the totals match.
pub struct EchoCanceller {
    s: EchoSettings,
    filter: Filter,
    probe: Option<Probe>,
    lag: Option<isize>,
}

impl EchoCanceller {
    pub fn new(s: &EchoSettings) -> EchoCanceller {
        EchoCanceller {
            s: *s,
            filter: Filter::new(s, samples(s.delay), 0),
            probe: Probe::new(s),
            lag: None,
        }
    }

    /// The mic-behind-reference lag the probe measured (`EchoSettings::align`), milliseconds:
    /// negative when the reference arrives after its echo. None until it is confident.
    pub fn lag_ms(&self) -> Option<i64> {
        self.lag.map(|l| l as i64 * 1000 / i64::from(SAMPLE_RATE))
    }

    /// Feed `mic` and `reference` (same length, same instants) and append whatever is ready.
    pub fn process(
        &mut self,
        mic: &[f32],
        reference: &[f32],
        mic_out: &mut Vec<f32>,
        ref_out: &mut Vec<f32>,
    ) {
        debug_assert_eq!(mic.len(), reference.len());
        let found = self.probe.as_mut().and_then(|p| p.push(mic, reference));
        self.filter.process(mic, reference, mic_out, ref_out);
        if let Some(lag) = found {
            self.probe = None;
            self.lag = Some(lag);
            self.realign(lag, mic_out, ref_out);
        }
    }

    /// With the echo `lag` behind its reference, the fixed `delay` places it `lag + delay` into
    /// the filter. Outside `0..=tail - delay` the filter cannot model it: emit the old filter's
    /// pending output and go on with one that delays the mic or the reference so the echo lands
    /// `delay` after the reference again.
    fn realign(&mut self, lag: isize, mic_out: &mut Vec<f32>, ref_out: &mut Vec<f32>) {
        let delay = samples(self.s.delay) as isize;
        let room = samples(self.s.tail) as isize - delay;
        if (0..=room).contains(&(lag + delay)) {
            return;
        }
        self.filter.flush(mic_out, ref_out);
        let (mic_delay, ref_delay) = ((delay - lag).max(0), (lag - delay).max(0));
        self.filter = Filter::new(&self.s, mic_delay as usize, ref_delay as usize);
    }

    /// Pad with silence until every pushed sample has come out.
    pub fn flush(&mut self, mic_out: &mut Vec<f32>, ref_out: &mut Vec<f32>) {
        self.filter.flush(mic_out, ref_out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(tail_ms: u64, delay_ms: u64) -> EchoSettings {
        EchoSettings {
            tail: Duration::from_millis(tail_ms),
            delay: Duration::from_millis(delay_ms),
            block: 256,
            step_permille: 500,
            divergence_percent: 200,
            warmup: Duration::ZERO,
            warmup_step_permille: 500,
            dominance_percent: 50,
            align: Duration::ZERO,
            align_probe: Duration::ZERO,
            align_confidence_percent: 50,
            suppress_percent: 0,
            suppress_floor_percent: 10,
        }
    }

    /// Deterministic noise in [-a, a] (xorshift).
    fn noise(len: usize, a: f32, mut s: u32) -> Vec<f32> {
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32 * 2.0 - 1.0) * a
            })
            .collect()
    }

    #[test]
    fn fft_round_trips() {
        let f = Fft::new(64);
        let x: Vec<C> = noise(64, 1.0, 7)
            .into_iter()
            .map(|re| C { re, im: 0.0 })
            .collect();
        let mut y = x.clone();
        f.run(&mut y, false);
        f.run(&mut y, true);
        for (a, b) in x.iter().zip(&y) {
            assert!((a.re - b.re).abs() < 1e-5 && b.im.abs() < 1e-5);
        }
    }

    #[test]
    fn output_length_matches_input_and_channels_stay_aligned() {
        let mut c = EchoCanceller::new(&settings(20, 7));
        let (mut m, mut r) = (vec![], vec![]);
        let refr: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        for chunk in refr.chunks(333) {
            c.process(&vec![0.0; chunk.len()], chunk, &mut m, &mut r);
        }
        c.flush(&mut m, &mut r);
        // Sample for sample: the 7 ms mic delay is latency, not a shift.
        assert_eq!(r, refr);
        assert_eq!(m.len(), 1000);
    }

    fn tone(hz: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (i as f32 / SAMPLE_RATE as f32 * hz * std::f32::consts::TAU).sin() * 0.5)
            .collect()
    }

    #[test]
    fn local_sound_unrelated_to_the_reference_is_kept() {
        // A far-end tone the mic does not hear and a local tone: nothing to cancel, and nothing
        // for the warmup to mute.
        let mut c = EchoCanceller::new(&EchoSettings {
            warmup: Duration::from_secs(1),
            ..settings(200, 40)
        });
        let (mic, far) = (tone(880.0, 96_000), tone(440.0, 96_000));
        let (mut m, mut r) = (vec![], vec![]);
        c.process(&mic, &far, &mut m, &mut r);
        c.flush(&mut m, &mut r);
        let err: f32 = m.iter().zip(&mic).map(|(o, i)| (o - i) * (o - i)).sum();
        let sig: f32 = mic.iter().map(|v| v * v).sum();
        assert!(
            err < sig / 100.0,
            "local tone changed: error {err} vs {sig}"
        );
    }

    #[test]
    fn silent_reference_leaves_the_mic_alone() {
        let mut c = EchoCanceller::new(&EchoSettings {
            warmup: Duration::from_secs(1),
            ..settings(20, 7)
        });
        let mic = noise(48_000, 0.3, 3);
        let (mut m, mut r) = (vec![], vec![]);
        c.process(&mic, &vec![0.0; mic.len()], &mut m, &mut r);
        c.flush(&mut m, &mut r);
        assert_eq!(m, mic);
    }
}
