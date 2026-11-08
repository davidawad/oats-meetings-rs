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
        align: Duration::ZERO,
        align_probe: Duration::ZERO,
        align_confidence_percent: 50,
        suppress_percent: 0,
        suppress_floor_percent: 10,
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
/// `audio.echo_dominance` 50) and residual echo suppressor.
fn warm(delay_ms: u64) -> EchoSettings {
    EchoSettings {
        warmup: Duration::from_secs(3),
        warmup_step_permille: 1_000,
        ..suppressed(delay_ms)
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

/// `settings` with the alignment on (`audio.echo_align` searching 400 ms either way; the
/// shipped probe and confidence).
fn aligned(delay_ms: u64) -> EchoSettings {
    EchoSettings {
        align: Duration::from_millis(400),
        align_probe: Duration::from_millis(1_500),
        align_confidence_percent: 70,
        ..settings(delay_ms)
    }
}

/// Reduction over the last half of the far-end fixture said twice, and the lag the canceller
/// measured: the filter adapts from the realignment on, so it gets a few seconds first.
fn late_db(s: &EchoSettings, lag_ms: f64) -> (f64, Option<i64>) {
    let far = fixture("them.wav").repeat(2);
    let echo = room(&far, lag_ms);
    let mut c = EchoCanceller::new(s);
    let (out, refr) = cancel(&mut c, &echo, &far);
    assert_eq!((out.len(), refr.len()), (far.len(), far.len()));
    assert_eq!(
        refr, far,
        "the reference passes through on its own timeline"
    );
    let tail = far.len() / 2;
    let db = 10.0 * (energy(&echo[tail..]) / energy(&out[tail..]).max(f64::MIN_POSITIVE)).log10();
    (db, c.lag_ms())
}

#[test]
fn a_capture_path_latency_beyond_the_tail_is_measured_and_cancelled() {
    // dot-4uzgol.98: the Core Audio tap hands the system audio over with another offset to the
    // mic than ScreenCaptureKit. Echo 300 ms behind its reference, outside the 100 ms tail.
    let (fixed, none) = late_db(&settings(40), 300.0);
    assert_eq!(none, None);
    let (db, lag) = late_db(&aligned(40), 300.0);
    eprintln!("300 ms: fixed delay {fixed:.1} dB, aligned {db:.1} dB, lag {lag:?}");
    assert!(
        fixed < MIN_REDUCTION_DB / 2.0,
        "fixed delay already at {fixed:.1} dB"
    );
    let lag = lag.expect("measured");
    assert!((lag - 300).abs() <= 15, "lag {lag} ms");
    assert!(db >= MIN_REDUCTION_DB, "echo reduced by only {db:.1} dB");
}

#[test]
fn a_reference_far_later_than_its_echo_is_measured_and_cancelled() {
    // The system audio arrives 150 ms after the mic heard it: more than audio.echo_delay covers.
    let (fixed, _) = late_db(&settings(40), -150.0);
    let (db, lag) = late_db(&aligned(40), -150.0);
    eprintln!("-150 ms: fixed delay {fixed:.1} dB, aligned {db:.1} dB, lag {lag:?}");
    assert!(
        fixed < MIN_REDUCTION_DB / 2.0,
        "fixed delay already at {fixed:.1} dB"
    );
    let lag = lag.expect("measured");
    assert!((lag + 150).abs() <= 15, "lag {lag} ms");
    assert!(db >= MIN_REDUCTION_DB, "echo reduced by only {db:.1} dB");
}

#[test]
fn an_echo_inside_the_tail_keeps_the_filter_it_has() {
    // 12 ms is what the fixed delay already models: measuring it must not cost cancellation.
    let (fixed, _) = late_db(&settings(40), 12.0);
    let (db, lag) = late_db(&aligned(40), 12.0);
    assert!(lag.is_some_and(|l| l.abs() <= 30), "lag {lag:?}");
    assert!(
        (db - fixed).abs() < 0.01,
        "aligned {db:.1} dB vs fixed {fixed:.1} dB"
    );
}

#[test]
fn local_speech_alone_is_not_mistaken_for_an_offset() {
    // The mic hears only the user: the envelopes do not agree, so nothing is realigned.
    let far = fixture("them.wav").repeat(2);
    let near = fixture("you.wav").repeat(2);
    let n = far.len().min(near.len());
    let mut c = EchoCanceller::new(&aligned(40));
    let (out, _) = cancel(&mut c, &near[..n], &far[..n]);
    assert_eq!(c.lag_ms(), None);
    assert_eq!(out.len(), n);
}

/// `settings` with the shipped residual echo suppressor (`audio.echo_suppress` 10,
/// `audio.echo_suppress_floor` 10).
fn suppressed(delay_ms: u64) -> EchoSettings {
    EchoSettings {
        suppress_percent: 10,
        suppress_floor_percent: 10,
        ..settings(delay_ms)
    }
}

/// The far end as laptop speakers play it back into the mic (dot-4uzgol.101): `lag_ms` late,
/// `db` down, through a soft-clipping, band-limited, coarsely quantised speaker that a linear
/// filter cannot model.
fn garbled(far: &[f32], lag_ms: f64, db: f64) -> Vec<f32> {
    let g = 10f64.powf(db / 20.0) as f32;
    let peak = far.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let lag = ms(lag_ms);
    let (mut lp, mut out) = (0.0f32, vec![0.0f32; far.len()]);
    for i in lag..far.len() {
        // Overdriven cone, a resonant low-pass box and a 5-bit converter.
        let x = (3.0 * far[i - lag] / peak).tanh();
        lp += 0.35 * (x - lp);
        out[i] = (lp * 16.0).round() / 16.0 * peak * g;
    }
    out
}

#[test]
fn a_garbled_echo_is_suppressed_where_the_linear_filter_leaves_it() {
    let far = fixture("them.wav").repeat(2);
    let echo = garbled(&far, 60.0, -14.0);
    let half = far.len() / 2;
    let db = |s: &EchoSettings| {
        let (out, refr) = cancel(&mut EchoCanceller::new(s), &echo, &far);
        assert_eq!((out.len(), refr.len()), (far.len(), far.len()));
        assert_eq!(refr, far);
        10.0 * (energy(&echo[half..]) / energy(&out[half..]).max(f64::MIN_POSITIVE)).log10()
    };
    let (linear, both) = (db(&settings(40)), db(&suppressed(40)));
    eprintln!("garbled echo: linear {linear:.1} dB, suppressed {both:.1} dB");
    assert!(
        both >= linear + 6.0 && both >= MIN_REDUCTION_DB,
        "suppressor took the garbled echo from {linear:.1} dB to only {both:.1} dB"
    );
}

#[test]
fn local_speech_survives_the_suppressor_under_double_talk() {
    let far = fixture("them.wav").repeat(2);
    let near = fixture("you.wav");
    let echo = garbled(&far, 60.0, -14.0);
    let half = far.len() / 2;
    let mut local = vec![0.0f32; far.len()];
    for (i, v) in near.iter().take(far.len() - half).enumerate() {
        local[half + i] = 0.5 * v;
    }
    let mic: Vec<f32> = echo.iter().zip(&local).map(|(e, l)| e + l).collect();
    let (out, _) = cancel(&mut EchoCanceller::new(&suppressed(40)), &mic, &far);
    let err: Vec<f32> = out[half..]
        .iter()
        .zip(&local[half..])
        .map(|(o, l)| o - l)
        .collect();
    let local_db = 10.0 * (energy(&local[half..]) / energy(&err)).log10();
    let kept = energy(&out[half..]) / energy(&local[half..]);
    eprintln!("double talk: local {local_db:.1} dB above the error, {kept:.2} of its energy out");
    assert!(
        local_db >= 12.0,
        "local speech only {local_db:.1} dB above the error"
    );
    assert!(kept >= 0.75, "only {kept:.2} of local speech energy kept");
}

#[test]
fn the_suppressor_keeps_the_channels_on_the_input_timeline() {
    // Mic alone with a silent reference: nothing to suppress, so the mic comes out unchanged
    // (the overlap-add frame reconstructs it) and in place.
    let mic = fixture("you.wav");
    let (out, refr) = cancel(
        &mut EchoCanceller::new(&suppressed(40)),
        &mic,
        &vec![0.0; mic.len()],
    );
    assert_eq!((out.len(), refr.len()), (mic.len(), mic.len()));
    let err: Vec<f32> = out.iter().zip(&mic).map(|(o, m)| o - m).collect();
    let db = 10.0 * (energy(&mic) / energy(&err).max(f64::MIN_POSITIVE)).log10();
    assert!(
        db >= 60.0,
        "mic changed: only {db:.1} dB above the difference"
    );
}
