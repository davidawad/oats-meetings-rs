//! Echo cancellation against a recorded far-end signal (dot-4uzgol.65): the fixture speech is
//! played through a synthetic speaker-to-mic path into the mic, and the canceller, given the
//! far end as its reference, must remove most of it while leaving local speech alone.

use std::path::Path;
use std::time::Duration;

use oats_audio::aec::{EchoCanceller, EchoSettings};
use oats_audio::resample::Resampler;
use oats_audio::SAMPLE_RATE;

/// The residual echo must be at least this far below the echo that reached the mic.
const MIN_REDUCTION_DB: f64 = 15.0;

fn fixture(name: &str) -> Vec<f32> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/stt")
        .join(name);
    let w = oats_audio::wav::read(&p).unwrap();
    assert_eq!(w.channels, 1);
    let mut out = Vec::new();
    Resampler::new(w.rate, SAMPLE_RATE).push(&w.samples[0], &mut out);
    out
}

fn settings(delay_ms: u64) -> EchoSettings {
    EchoSettings {
        tail: Duration::from_millis(100),
        delay: Duration::from_millis(delay_ms),
        block: 256,
        step_permille: 500,
        divergence_percent: 200,
        warmup: Duration::ZERO,
        warmup_step_permille: 500,
        dominance_percent: 50,
    }
}

fn ms(n: f64) -> usize {
    (n * f64::from(SAMPLE_RATE) / 1000.0) as usize
}

/// A room: direct path after `lag_ms`, then a few decaying reflections (`lag_ms` may be
/// negative: the echo reaches the mic before the reference reaches us).
fn room(far: &[f32], lag_ms: f64) -> Vec<f32> {
    let taps = [
        (0.0, 0.5),
        (3.0, -0.25),
        (11.0, 0.12),
        (37.0, -0.06),
        (70.0, 0.03),
    ];
    let mut out = vec![0.0f32; far.len()];
    for (at, g) in taps {
        let shift = ms((lag_ms + at).abs()) as isize * if lag_ms + at < 0.0 { -1 } else { 1 };
        for (i, o) in out.iter_mut().enumerate() {
            let j = i as isize - shift;
            if j >= 0 && (j as usize) < far.len() {
                *o += g * far[j as usize];
            }
        }
    }
    out
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>()
}

fn cancel(c: &mut EchoCanceller, mic: &[f32], far: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let (mut m, mut r) = (vec![], vec![]);
    // Arbitrary run lengths, as capture callbacks deliver them.
    let mut at = 0;
    for (i, len) in [480usize, 1000, 37, 4096].iter().cycle().enumerate() {
        if at >= mic.len() {
            break;
        }
        let end = (at + len + i % 3).min(mic.len());
        c.process(&mic[at..end], &far[at..end], &mut m, &mut r);
        at = end;
    }
    c.flush(&mut m, &mut r);
    (m, r)
}

fn reduction_db(lag_ms: f64, delay_ms: u64) -> f64 {
    let far = fixture("them.wav");
    let echo = room(&far, lag_ms);
    let mut c = EchoCanceller::new(&settings(delay_ms));
    let (out, refr) = cancel(&mut c, &echo, &far);
    assert_eq!((out.len(), refr.len()), (far.len(), far.len()));
    // Judge the second half: the filter has had the first to converge.
    let half = far.len() / 2;
    let before = energy(&echo[half..]);
    let after = energy(&out[half..]);
    let db = 10.0 * (before / after.max(f64::MIN_POSITIVE)).log10();
    eprintln!("lag {lag_ms} delay {delay_ms}: {db:.1} dB");
    db
}

#[test]
fn far_end_echo_drops_by_at_least_the_floor() {
    let db = reduction_db(12.0, 0);
    assert!(db >= MIN_REDUCTION_DB, "echo reduced by only {db:.1} dB");
}

#[test]
fn a_reference_later_than_its_echo_is_covered_by_the_mic_delay() {
    // The loopback reaches us 20 ms after the mic hears it: only the mic delay makes it causal.
    let db = reduction_db(-20.0, 40);
    assert!(db >= MIN_REDUCTION_DB, "echo reduced by only {db:.1} dB");
}

#[test]
fn local_speech_survives_while_the_far_end_echo_goes() {
    let far = fixture("them.wav");
    let near = fixture("you.wav");
    let echo = room(&far, 12.0);
    // Local speech starts halfway in, over the echo.
    let half = far.len() / 2;
    let mut local = vec![0.0f32; far.len()];
    for (i, v) in near.iter().take(far.len() - half).enumerate() {
        local[half + i] = 0.5 * v;
    }
    let mic: Vec<f32> = echo.iter().zip(&local).map(|(e, l)| e + l).collect();
    let mut c = EchoCanceller::new(&settings(0));
    let (out, _) = cancel(&mut c, &mic, &far);
    let err: Vec<f32> = out[half..]
        .iter()
        .zip(&local[half..])
        .map(|(o, l)| o - l)
        .collect();
    let echo_db = 10.0 * (energy(&echo[half..]) / energy(&err)).log10();
    let local_db = 10.0 * (energy(&local[half..]) / energy(&err)).log10();
    assert!(
        echo_db >= MIN_REDUCTION_DB,
        "echo reduced by only {echo_db:.1} dB under double talk"
    );
    assert!(
        local_db >= MIN_REDUCTION_DB,
        "local speech only {local_db:.1} dB above the error"
    );
}

/// `settings` with the shipped warmup (`audio.echo_warmup` 3s, `audio.echo_warmup_step` 1000,
/// `audio.echo_dominance` 50).
fn warm(delay_ms: u64) -> EchoSettings {
    EchoSettings {
        warmup: Duration::from_secs(3),
        warmup_step_permille: 1_000,
        ..settings(delay_ms)
    }
}

/// Echo reduction over the first second of far-end speech: the first sentence.
fn first_second_db(s: &EchoSettings) -> f64 {
    let far = fixture("them.wav");
    let echo = room(&far, 12.0);
    let (out, _) = cancel(&mut EchoCanceller::new(s), &echo, &far);
    let n = ms(1000.0);
    10.0 * (energy(&echo[..n]) / energy(&out[..n]).max(f64::MIN_POSITIVE)).log10()
}

#[test]
fn a_fresh_filter_leaks_the_first_sentence_and_the_warmup_mutes_it() {
    // dot-4uzgol.75: the leaked sentence was the first one. A cold filter removes only a few
    // dB of it, which the recogniser still transcribes as "you".
    let cold = first_second_db(&settings(40));
    assert!(
        cold < MIN_REDUCTION_DB / 2.0,
        "cold filter already at {cold:.1} dB"
    );
    let warmed = first_second_db(&warm(40));
    eprintln!("first second: cold {cold:.1} dB, warmup {warmed:.1} dB");
    assert!(
        warmed >= MIN_REDUCTION_DB,
        "first sentence reduced by only {warmed:.1} dB with the warmup"
    );
}

#[test]
fn local_speech_over_the_first_sentence_survives_the_warmup() {
    // Double talk from the first sample: the reference cannot predict local speech, so the
    // warmup must not mute it.
    let far = fixture("them.wav");
    let near = fixture("you.wav");
    let echo = room(&far, 12.0);
    let n = far.len().min(near.len());
    let local: Vec<f32> = near[..n].iter().map(|v| 0.5 * v).collect();
    let mic: Vec<f32> = echo[..n].iter().zip(&local).map(|(e, l)| e + l).collect();
    let (out, _) = cancel(&mut EchoCanceller::new(&warm(0)), &mic, &far[..n]);
    // Every 50 ms of the warmup where the user talks still carries the user's voice.
    let w = ms(50.0);
    let span = ms(3000.0).min(n) / w * w;
    let mean = energy(&local[..span]) / (span / w) as f64;
    let mut talking = 0;
    for i in (0..span).step_by(w) {
        let l = energy(&local[i..i + w]);
        if l < mean / 10.0 {
            continue;
        }
        talking += 1;
        let o = energy(&out[i..i + w]);
        assert!(o >= l / 4.0, "local speech at {i} muted: {o:.4} of {l:.4}");
    }
    assert!(talking >= 20, "{talking} talking windows");
}
