//! Recordings that stop and start by themselves (dot-4uzgol.110): the level detectors.
//!
//! Both work on 10 ms frames of the 48 kHz audio and a frame's peak, the gate `stt.speech_peak`
//! uses, in per-mille of full scale. They are pure state machines fed samples per source; the
//! engine reads their state on its own clock (`record.auto_check`) and acts on it.
//!
//! `Silence` (auto-stop) is fed the recording's own audio as it is written, so its clock is the
//! WAV's: paused audio is not written and not counted. Silence is the time since the last sound on
//! *either* channel, i.e. both channels below `record.auto_stop.silence_level`. Hysteresis: sound
//! shorter than `record.auto_stop.blip` does not end the silence. A resume or `record-keep` starts
//! it over. Its `last_sound` is where the trailing silence is cut.
//!
//! `Activity` (auto-start) is fed the input monitor's audio while idle. A source is active once a
//! run of frames at or above `record.auto_start.level` has lasted `record.auto_start.after`;
//! hysteresis: a quiet dip shorter than `record.auto_start.gap` does not end the run.

use oats_audio::{Source, SAMPLE_RATE};

use crate::config::typed::{AutoStartSources, Config};

/// Samples in one detector frame: 10 ms.
fn frame_len() -> usize {
    SAMPLE_RATE as usize / 100
}

/// Milliseconds of `frames` detector frames.
pub fn frames_ms(frames: u64) -> u64 {
    frames * 10
}

/// Detector frames in `ms` (rounded up: a rule of 1 ms needs one frame).
fn ms_frames(ms: u64) -> u64 {
    ms.div_ceil(10)
}

fn index(s: Source) -> usize {
    usize::from(s == Source::System)
}

/// Cuts a source's samples into frames and yields each whole frame's peak.
#[derive(Debug, Default, Clone)]
struct Framer {
    n: usize,
    peak: f32,
}

impl Framer {
    fn push(&mut self, samples: &[f32], mut frame: impl FnMut(f32)) {
        let len = frame_len();
        for s in samples {
            self.peak = self.peak.max(s.abs());
            self.n += 1;
            if self.n == len {
                frame(self.peak);
                self.n = 0;
                self.peak = 0.0;
            }
        }
    }
}

fn permille(level: u32) -> f32 {
    level as f32 / 1000.0
}

/// The auto-stop rule (`record.auto_stop.*`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StopRule {
    pub enabled: bool,
    /// Per-mille of full scale.
    pub level: u32,
    pub after_ms: u64,
    pub warn_ms: u64,
    pub blip_ms: u64,
    pub tail_ms: u64,
}

impl StopRule {
    pub fn of(c: &Config) -> StopRule {
        let a = &c.record.auto_stop;
        StopRule {
            enabled: a.enabled,
            level: a.silence_level,
            after_ms: a.after.0,
            warn_ms: a.warn_before.0,
            blip_ms: a.blip.0,
            tail_ms: a.tail.0,
        }
    }
}

/// One source of a recording, as auto-stop sees it.
#[derive(Debug, Default, Clone)]
struct StopChan {
    framer: Framer,
    /// Whole frames seen.
    frames: u64,
    /// Where the current run of loud frames began, if one is running.
    run: Option<u64>,
}

/// What `Silence::check` says about the recording now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCheck {
    /// Sound, or not silent long enough to warn (or auto-stop is off, or paused).
    Quiet,
    /// Silent for `silence_ms`; the recording stops in `stop_in_ms` unless sound or `keep` comes.
    /// `first` is true the first time this is reported in a stretch of silence.
    Warn {
        silence_ms: u64,
        stop_in_ms: u64,
        first: bool,
    },
    /// A warning was given and the silence ended (sound came back).
    Resumed,
    /// Stop now; cut the audio at `cut_ms` (the last sound plus the tail, never past the end).
    Stop { silence_ms: u64, cut_ms: u64 },
}

/// The auto-stop detector of one recording.
#[derive(Debug, Clone)]
pub struct Silence {
    rule: StopRule,
    /// Per source (mic, system); None for a source the recording does not have.
    chans: [Option<StopChan>; 2],
    /// The frame the silence counts from: the end of the last sound that counted, or the
    /// start, a resume or a `keep`.
    since: u64,
    /// Where the last sound that counted ended (None before any).
    last_sound: Option<u64>,
    paused: bool,
    warned: bool,
}

impl Silence {
    /// A detector for a recording with the mic and/or system source.
    pub fn new(rule: StopRule, mic: bool, system: bool) -> Silence {
        Silence {
            rule,
            chans: [mic.then(StopChan::default), system.then(StopChan::default)],
            since: 0,
            last_sound: None,
            paused: false,
            warned: false,
        }
    }

    pub fn set_rule(&mut self, rule: StopRule) {
        self.rule = rule;
    }

    /// Audio of `source`, in order, as the recording writes it.
    pub fn push(&mut self, source: Source, samples: &[f32]) {
        let rule = self.rule;
        let (level, blip) = (permille(rule.level), ms_frames(rule.blip_ms).max(1));
        let Some(c) = self.chans[index(source)].as_mut() else {
            return;
        };
        let mut sound = None;
        let StopChan {
            framer,
            frames,
            run,
        } = c;
        framer.push(samples, |peak| {
            let at = *frames;
            *frames += 1;
            if peak >= level {
                let begun = *run.get_or_insert(at);
                // A run that has lasted `blip` is sound up to this frame's end.
                if *frames - begun >= blip {
                    sound = Some(*frames);
                }
            } else {
                *run = None;
            }
        });
        if let Some(end) = sound {
            self.last_sound = Some(self.last_sound.map_or(end, |l| l.max(end)));
            self.since = self.since.max(end);
        }
    }

    /// Audio seen so far, in frames: the source furthest behind (the writer keeps them in step).
    pub fn now(&self) -> u64 {
        self.chans
            .iter()
            .flatten()
            .map(|c| c.frames)
            .min()
            .unwrap_or(0)
    }

    pub fn silence_ms(&self) -> u64 {
        if self.paused {
            return 0;
        }
        frames_ms(self.now().saturating_sub(self.since))
    }

    /// Where the last sound ended, ms into the recording (None: no sound yet).
    pub fn last_sound_ms(&self) -> Option<u64> {
        self.last_sound.map(frames_ms)
    }

    /// While paused nothing counts; a resume starts the silence over (a resume is someone there).
    pub fn set_paused(&mut self, paused: bool) {
        if self.paused && !paused {
            self.since = self.now();
        }
        self.paused = paused;
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    /// `record-keep`: the silence starts over now. True when a warning was pending.
    pub fn keep(&mut self) -> bool {
        self.since = self.now();
        std::mem::replace(&mut self.warned, false)
    }

    /// Whether a warning is out (an auto-stop is pending).
    pub fn pending(&self) -> bool {
        self.warned
    }

    pub fn check(&mut self) -> StopCheck {
        let r = self.rule;
        if !r.enabled || self.paused {
            return if std::mem::replace(&mut self.warned, false) {
                StopCheck::Resumed
            } else {
                StopCheck::Quiet
            };
        }
        let silence_ms = self.silence_ms();
        if silence_ms >= r.after_ms {
            // Cut after the last sound (or the start, resume or keep it counts from), plus the
            // tail; never past what was recorded.
            let now = frames_ms(self.now());
            let cut_ms = (frames_ms(self.since) + r.tail_ms).min(now);
            return StopCheck::Stop { silence_ms, cut_ms };
        }
        if r.warn_ms > 0 && silence_ms >= r.after_ms.saturating_sub(r.warn_ms) {
            let first = !std::mem::replace(&mut self.warned, true);
            return StopCheck::Warn {
                silence_ms,
                stop_in_ms: r.after_ms - silence_ms,
                first,
            };
        }
        if std::mem::replace(&mut self.warned, false) {
            StopCheck::Resumed
        } else {
            StopCheck::Quiet
        }
    }
}

/// The auto-start rule (`record.auto_start.*`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StartRule {
    pub enabled: bool,
    /// Per-mille of full scale.
    pub level: u32,
    pub after_ms: u64,
    pub gap_ms: u64,
    pub sources: AutoStartSources,
}

impl StartRule {
    pub fn of(c: &Config) -> StartRule {
        let a = &c.record.auto_start;
        StartRule {
            enabled: a.enabled,
            level: a.level,
            after_ms: a.after.0,
            gap_ms: a.gap.0,
            sources: a.sources,
        }
    }

    /// Hears nothing and is never due: until the engine hands over the configured rule.
    pub fn off() -> StartRule {
        StartRule {
            enabled: false,
            level: 1_000,
            after_ms: u64::MAX,
            gap_ms: 0,
            sources: AutoStartSources::Speaker,
        }
    }

    fn counts(&self, source: Source) -> bool {
        match self.sources {
            AutoStartSources::Speaker => source == Source::System,
            AutoStartSources::Mic => source == Source::Mic,
            AutoStartSources::Either => true,
        }
    }
}

/// One source of the idle monitor, as auto-start sees it.
#[derive(Debug, Default, Clone)]
struct StartChan {
    framer: Framer,
    frames: u64,
    /// The run of activity: where it began and where its last loud frame ended.
    run: Option<(u64, u64)>,
}

impl StartChan {
    fn sustained_ms(&self) -> u64 {
        self.run.map_or(0, |(from, to)| frames_ms(to - from))
    }
}

/// The auto-start detector over the idle monitor.
#[derive(Debug, Clone)]
pub struct Activity {
    rule: StartRule,
    chans: [StartChan; 2],
}

impl Activity {
    pub fn new(rule: StartRule) -> Activity {
        Activity {
            rule,
            chans: Default::default(),
        }
    }

    pub fn set_rule(&mut self, rule: StartRule) {
        self.rule = rule;
    }

    /// Forget every run (the monitor reopened, a recording started or stopped).
    pub fn reset(&mut self) {
        self.chans = Default::default();
    }

    /// Audio of `source`, in order, as the monitor hears it.
    pub fn push(&mut self, source: Source, samples: &[f32]) {
        let (level, gap) = (permille(self.rule.level), ms_frames(self.rule.gap_ms));
        let StartChan {
            framer,
            frames,
            run,
        } = &mut self.chans[index(source)];
        framer.push(samples, |peak| {
            let at = *frames;
            *frames += 1;
            if peak >= level {
                *run = Some(match *run {
                    // Within the gap the dip belongs to the run.
                    Some((from, to)) if at - to <= gap => (from, *frames),
                    _ => (at, *frames),
                });
            } else if run.is_some_and(|(_, to)| *frames - to > gap) {
                *run = None;
            }
        });
    }

    /// How long `source` has been active without a break longer than the gap, ms.
    pub fn sustained_ms(&self, source: Source) -> u64 {
        self.chans[index(source)].sustained_ms()
    }

    /// The source that has been active for `record.auto_start.after`, if any counted one has.
    pub fn due(&self) -> Option<Source> {
        if !self.rule.enabled {
            return None;
        }
        [Source::System, Source::Mic]
            .into_iter()
            .filter(|s| self.rule.counts(*s))
            .find(|s| self.sustained_ms(*s) >= self.rule.after_ms)
    }
}

/// A source's name in events and `record-auto-status`.
pub fn source_name(s: Source) -> &'static str {
    match s {
        Source::System => "speaker",
        Source::Mic => "mic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOUD: f32 = 0.5;

    /// `ms` of audio at `amp` (a square wave, so every frame's peak is `amp`).
    fn tone(ms: u64, amp: f32) -> Vec<f32> {
        let n = ms as usize * SAMPLE_RATE as usize / 1000;
        (0..n)
            .map(|i| if i % 2 == 0 { amp } else { -amp })
            .collect()
    }

    fn rule() -> StopRule {
        StopRule {
            enabled: true,
            level: 10,
            after_ms: 2_000,
            warn_ms: 500,
            blip_ms: 100,
            tail_ms: 200,
        }
    }

    /// Feed both channels the same length: `mic` and `sys` amplitudes for `ms`.
    fn both(s: &mut Silence, ms: u64, mic: f32, sys: f32) {
        s.push(Source::Mic, &tone(ms, mic));
        s.push(Source::System, &tone(ms, sys));
    }

    #[test]
    fn silence_on_both_channels_stops_after_the_rule_and_cuts_after_the_last_sound() {
        let mut s = Silence::new(rule(), true, true);
        both(&mut s, 1_000, LOUD, 0.0);
        assert_eq!(s.check(), StopCheck::Quiet);
        assert_eq!(s.last_sound_ms(), Some(1_000));
        both(&mut s, 1_400, 0.0, 0.0);
        assert_eq!(
            s.check(),
            StopCheck::Quiet,
            "1.4 s of 2 s, warning at 1.5 s"
        );
        both(&mut s, 200, 0.0, 0.0);
        assert_eq!(
            s.check(),
            StopCheck::Warn {
                silence_ms: 1_600,
                stop_in_ms: 400,
                first: true
            }
        );
        assert!(matches!(s.check(), StopCheck::Warn { first: false, .. }));
        both(&mut s, 400, 0.0, 0.0);
        assert_eq!(
            s.check(),
            StopCheck::Stop {
                silence_ms: 2_000,
                cut_ms: 1_200
            }
        );
    }

    #[test]
    fn the_threshold_is_the_peak_per_mille() {
        let mut s = Silence::new(rule(), true, true);
        // 0.009 is below 10 per-mille: silence; 0.01 is not.
        both(&mut s, 2_000, 0.009, 0.009);
        assert!(matches!(s.check(), StopCheck::Stop { .. }));
        let mut s = Silence::new(rule(), true, true);
        both(&mut s, 1_000, 0.0, 0.0);
        both(&mut s, 1_000, 0.0, 0.0101);
        assert_eq!(s.silence_ms(), 0);
        assert_eq!(s.check(), StopCheck::Quiet);
    }

    #[test]
    fn sound_on_either_channel_keeps_the_recording_and_both_must_be_silent() {
        for (mic, sys) in [(LOUD, 0.0), (0.0, LOUD), (LOUD, LOUD)] {
            let mut s = Silence::new(rule(), true, true);
            for _ in 0..10 {
                both(&mut s, 1_000, mic, sys);
                assert_eq!(s.check(), StopCheck::Quiet, "mic {mic} sys {sys}");
            }
        }
        // A recording of one source watches that one only.
        let mut s = Silence::new(rule(), false, true);
        s.push(Source::Mic, &tone(3_000, LOUD));
        s.push(Source::System, &tone(3_000, 0.0));
        assert!(matches!(s.check(), StopCheck::Stop { .. }));
    }

    #[test]
    fn a_blip_shorter_than_the_hysteresis_does_not_end_the_silence() {
        let mut s = Silence::new(rule(), true, true);
        both(&mut s, 1_000, 0.0, 0.0);
        // 50 ms ping: shorter than the 100 ms blip.
        both(&mut s, 50, 0.0, LOUD);
        both(&mut s, 950, 0.0, 0.0);
        assert_eq!(s.last_sound_ms(), None);
        assert!(matches!(s.check(), StopCheck::Stop { cut_ms: 200, .. }));
        // 150 ms of sound counts, and the silence runs from its end.
        let mut s = Silence::new(rule(), true, true);
        both(&mut s, 1_000, 0.0, 0.0);
        both(&mut s, 150, LOUD, 0.0);
        both(&mut s, 850, 0.0, 0.0);
        assert_eq!(s.last_sound_ms(), Some(1_150));
        assert_eq!(s.silence_ms(), 850);
    }

    #[test]
    fn pause_freezes_the_silence_and_resume_starts_it_over() {
        let mut s = Silence::new(rule(), true, true);
        both(&mut s, 1_800, 0.0, 0.0);
        assert!(matches!(s.check(), StopCheck::Warn { .. }));
        s.set_paused(true);
        assert_eq!(s.silence_ms(), 0);
        assert_eq!(s.check(), StopCheck::Resumed, "the warning is withdrawn");
        assert_eq!(s.check(), StopCheck::Quiet);
        s.set_paused(false);
        both(&mut s, 1_000, 0.0, 0.0);
        assert_eq!(s.silence_ms(), 1_000);
        assert_eq!(s.check(), StopCheck::Quiet);
        both(&mut s, 1_000, 0.0, 0.0);
        // Nothing was said: the cut is after the resume (plus the tail).
        assert_eq!(
            s.check(),
            StopCheck::Stop {
                silence_ms: 2_000,
                cut_ms: 2_000
            }
        );
    }

    #[test]
    fn keep_and_returning_sound_withdraw_a_pending_stop() {
        let mut s = Silence::new(rule(), true, true);
        both(&mut s, 1_600, 0.0, 0.0);
        assert!(matches!(s.check(), StopCheck::Warn { first: true, .. }));
        assert!(s.pending());
        assert!(s.keep());
        assert!(!s.pending());
        assert_eq!(s.check(), StopCheck::Quiet);
        both(&mut s, 1_600, 0.0, 0.0);
        assert!(matches!(s.check(), StopCheck::Warn { first: true, .. }));
        both(&mut s, 200, LOUD, 0.0);
        assert_eq!(s.check(), StopCheck::Resumed);
        assert_eq!(s.check(), StopCheck::Quiet);
    }

    #[test]
    fn off_or_without_a_warning_window() {
        let mut s = Silence::new(
            StopRule {
                enabled: false,
                ..rule()
            },
            true,
            true,
        );
        both(&mut s, 5_000, 0.0, 0.0);
        assert_eq!(s.check(), StopCheck::Quiet);
        // Turned on later (a config reload): the silence so far counts.
        s.set_rule(StopRule {
            warn_ms: 0,
            ..rule()
        });
        assert!(matches!(s.check(), StopCheck::Stop { .. }));
        let mut s = Silence::new(
            StopRule {
                warn_ms: 0,
                ..rule()
            },
            true,
            true,
        );
        both(&mut s, 1_900, 0.0, 0.0);
        assert_eq!(s.check(), StopCheck::Quiet, "no warning");
    }

    #[test]
    fn channels_out_of_step_count_from_the_one_behind() {
        let mut s = Silence::new(rule(), true, true);
        s.push(Source::Mic, &tone(3_000, 0.0));
        s.push(Source::System, &tone(1_000, 0.0));
        assert_eq!(s.silence_ms(), 1_000);
        // Odd block sizes are framed across calls.
        let mut s = Silence::new(rule(), true, false);
        for _ in 0..100 {
            s.push(Source::Mic, &tone(7, 0.0));
            s.push(Source::Mic, &[0.0; 13]);
        }
        assert_eq!(s.now(), (100 * (7 * 48 + 13)) as u64 / 480);
    }

    fn start_rule() -> StartRule {
        StartRule {
            enabled: true,
            level: 20,
            after_ms: 3_000,
            gap_ms: 500,
            sources: AutoStartSources::Speaker,
        }
    }

    #[test]
    fn sustained_activity_is_due_and_blips_are_not() {
        let mut a = Activity::new(start_rule());
        // Notification sounds: 300 ms on, 1 s off, many times.
        for _ in 0..10 {
            a.push(Source::System, &tone(300, LOUD));
            a.push(Source::System, &tone(1_000, 0.0));
        }
        assert_eq!(a.due(), None);
        a.push(Source::System, &tone(2_900, LOUD));
        assert_eq!(a.due(), None);
        a.push(Source::System, &tone(100, LOUD));
        assert_eq!(a.due(), Some(Source::System));
        a.reset();
        assert_eq!(a.due(), None);
    }

    #[test]
    fn dips_within_the_gap_keep_the_run_and_longer_ones_break_it() {
        let mut a = Activity::new(start_rule());
        for _ in 0..3 {
            a.push(Source::System, &tone(800, LOUD));
            a.push(Source::System, &tone(400, 0.0));
        }
        a.push(Source::System, &tone(200, LOUD));
        assert_eq!(a.sustained_ms(Source::System), 3_800);
        assert_eq!(a.due(), Some(Source::System));
        let mut a = Activity::new(start_rule());
        for _ in 0..3 {
            a.push(Source::System, &tone(800, LOUD));
            a.push(Source::System, &tone(600, 0.0));
        }
        assert_eq!(a.sustained_ms(Source::System), 0);
        a.push(Source::System, &tone(800, LOUD));
        assert_eq!(a.sustained_ms(Source::System), 800);
        assert_eq!(a.due(), None);
    }

    #[test]
    fn below_the_level_is_quiet() {
        let mut a = Activity::new(start_rule());
        a.push(Source::System, &tone(5_000, 0.019));
        assert_eq!(a.due(), None);
        a.push(Source::System, &tone(3_000, 0.02));
        assert_eq!(a.due(), Some(Source::System));
    }

    #[test]
    fn sources_pick_the_channel_that_counts() {
        let feed = |sources, src| {
            let mut a = Activity::new(StartRule {
                sources,
                ..start_rule()
            });
            a.push(src, &tone(3_000, LOUD));
            a.due()
        };
        use AutoStartSources::*;
        assert_eq!(feed(Speaker, Source::System), Some(Source::System));
        assert_eq!(feed(Speaker, Source::Mic), None);
        assert_eq!(feed(Mic, Source::Mic), Some(Source::Mic));
        assert_eq!(feed(Mic, Source::System), None);
        assert_eq!(feed(Either, Source::Mic), Some(Source::Mic));
        assert_eq!(feed(Either, Source::System), Some(Source::System));
        let mut a = Activity::new(StartRule {
            enabled: false,
            ..start_rule()
        });
        a.push(Source::System, &tone(5_000, LOUD));
        assert_eq!(a.due(), None, "off");
    }
}
