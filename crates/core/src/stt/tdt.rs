//! Greedy Token-and-Duration Transducer decoding (dot-4uzgol.69), the loop FluidAudio's
//! `TdtDecoderV3` runs for one window of Parakeet TDT (`AsrManager`, single chunk, last chunk). The
//! networks sit behind `Nets` so the loop is plain Rust: the CoreML driver (`coreml`) runs them on
//! the Neural Engine, the tests script them.
//!
//! - The prediction network is primed with blank (start of sequence) and run again only after a
//!   non-blank token; blanks reuse its output (silence does not change the language context).
//! - Each joint step gives a token and a duration bin; the bin maps to frames to advance. A blank
//!   with duration 0 advances 1; so does a second non-blank at the same frame with duration 0.
//! - Blanks enter an inner loop that only runs the joint until a non-blank (or the end).
//! - A token found as the frames run out is dropped, as in FluidAudio; the tail pass below
//!   (FluidAudio's last-chunk finalisation) then looks at the last frames for pending tokens.
//! - `max_symbols` tokens at one frame force an advance; past `max_tokens` the window stops.

use crate::config::Result;

/// One joint network step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    pub token: usize,
    pub prob: f32,
    pub duration_bin: usize,
}

/// The prediction and joint networks of one model over one window of encoder frames.
pub trait Nets {
    /// Feed `token` to the prediction network from its current state; its output and new state
    /// replace the current ones.
    fn predict(&mut self, token: usize) -> Result<()>;
    /// The joint network on encoder frame `frame` and the current prediction output.
    fn joint(&mut self, frame: usize) -> Result<Decision>;
}

/// The decoder's limits and the model's token facts.
#[derive(Clone, Debug)]
pub struct Limits<'a> {
    pub blank: usize,
    /// Frames each duration bin advances (`[0, 1, 2, 3, 4]` for Parakeet TDT v2).
    pub durations: &'a [usize],
    /// Tokens at one frame before the decoder is forced on (`stt.local.tdt_max_symbols`).
    pub max_symbols: usize,
    /// Tokens per window before decoding stops (`stt.local.tdt_max_tokens`).
    pub max_tokens: usize,
    /// Consecutive blanks that end the tail pass (`stt.local.tdt_tail_blanks`).
    pub tail_blanks: usize,
}

/// The decoded tokens with the encoder frame each was emitted at, its duration in frames and its
/// probability.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hypothesis {
    pub tokens: Vec<usize>,
    pub frames: Vec<usize>,
    pub durations: Vec<usize>,
    pub probs: Vec<f32>,
}

impl Hypothesis {
    fn push(&mut self, token: usize, frame: usize, duration: usize, prob: f32) {
        self.tokens.push(token);
        self.frames.push(frame);
        self.durations.push(duration);
        self.probs.push(prob);
    }
}

fn bin(l: &Limits, d: &Decision) -> Result<usize> {
    l.durations.get(d.duration_bin).copied().ok_or_else(|| {
        crate::config::ConfigError::new(
            "asr_failed",
            format!("duration bin {} out of range", d.duration_bin),
        )
    })
}

fn prob(p: f32) -> f32 {
    if p.is_finite() {
        p.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Decode one window. `encoder_frames` is the encoder's valid length, `audio_frames` the frames
/// the audio itself covers (`ceil(samples / samples per frame)`); decoding stops at the smaller.
pub fn decode(
    nets: &mut dyn Nets,
    encoder_frames: usize,
    audio_frames: usize,
    l: &Limits,
) -> Result<Hypothesis> {
    let mut hyp = Hypothesis::default();
    let len = encoder_frames.min(audio_frames);
    // Under 160 ms there is nothing to decode (FluidAudio: encoder length <= 1).
    if encoder_frames <= 1 || len == 0 {
        return Ok(hyp);
    }
    let last = len - 1;
    nets.predict(l.blank)?;
    let mut t = 0usize;
    let mut safe_t = 0usize;
    let mut active = t < len;
    let (mut last_emit, mut at_emit) = (None::<usize>, 0usize);
    let mut emitted = 0usize;
    while active {
        let d = nets.joint(safe_t)?;
        let (mut label, mut p) = (d.token, prob(d.prob));
        let mut dur = bin(l, &d)?;
        let mut blank = label == l.blank;
        if !blank && dur == 0 && last_emit == Some(t) && at_emit >= 1 {
            dur = 1;
        }
        if blank && dur == 0 {
            dur = 1;
        }
        let mut t_label = t;
        t += dur;
        safe_t = t.min(last);
        active = t < len;
        let mut advance = active && blank;
        while advance {
            t_label = t;
            let d = nets.joint(safe_t)?;
            label = d.token;
            p = prob(d.prob);
            dur = bin(l, &d)?;
            blank = label == l.blank;
            if blank && dur == 0 {
                dur = 1;
            }
            t += dur;
            safe_t = t.min(last);
            active = t < len;
            advance = active && blank;
        }
        if active && label != l.blank {
            emitted += 1;
            if emitted > l.max_tokens {
                break;
            }
            hyp.push(label, t_label, dur, p);
            nets.predict(label)?;
            if last_emit == Some(t_label) {
                at_emit += 1;
            } else {
                last_emit = Some(t_label);
                at_emit = 1;
            }
            if at_emit >= l.max_symbols {
                t = (t + 1).min(last);
                safe_t = t.min(last);
                at_emit = 0;
                last_emit = None;
            }
        }
        active = t < len;
    }
    // Tail pass (last chunk): look again at the frames around the end for tokens the main loop
    // stepped past.
    let (mut steps, mut blanks) = (0usize, 0usize);
    let mut tail_t = t;
    while steps < l.max_symbols && blanks < l.tail_blanks {
        let choices = [
            tail_t.min(encoder_frames - 1),
            last.min(encoder_frames - 1),
            len.saturating_sub(2).min(encoder_frames - 1),
        ];
        let d = nets.joint(choices[steps % choices.len()])?;
        let dur = bin(l, &d)?;
        if d.token == l.blank {
            blanks += 1;
        } else {
            blanks = 0;
            hyp.push(d.token, tail_t.min(last), dur, prob(d.prob));
            nets.predict(d.token)?;
        }
        tail_t = (tail_t + dur.max(1)).min(len);
        steps += 1;
    }
    Ok(hyp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const BLANK: usize = 9;

    fn limits() -> Limits<'static> {
        Limits {
            blank: BLANK,
            durations: &[0, 1, 2, 3, 4],
            max_symbols: 10,
            max_tokens: 150,
            tail_blanks: 5,
        }
    }

    /// Joint decisions keyed by (frame, last token fed to the predictor); anything unlisted is
    /// a blank advancing one frame. Records every call.
    struct Script {
        table: HashMap<(usize, usize), (usize, usize)>,
        last: Option<usize>,
        predicted: Vec<usize>,
        joints: Vec<usize>,
    }

    impl Script {
        fn new(rows: &[(usize, usize, usize, usize)]) -> Script {
            Script {
                table: rows
                    .iter()
                    .map(|&(f, prev, tok, bin)| ((f, prev), (tok, bin)))
                    .collect(),
                last: None,
                predicted: vec![],
                joints: vec![],
            }
        }
    }

    impl Nets for Script {
        fn predict(&mut self, token: usize) -> Result<()> {
            self.predicted.push(token);
            self.last = Some(token);
            Ok(())
        }
        fn joint(&mut self, frame: usize) -> Result<Decision> {
            self.joints.push(frame);
            let prev = self.last.expect("primed before the first joint");
            let (token, duration_bin) = self
                .table
                .get(&(frame, prev))
                .copied()
                .unwrap_or((BLANK, 1));
            Ok(Decision {
                token,
                prob: 0.9,
                duration_bin,
            })
        }
    }

    /// Fixture: tokens 1, 2, 3 at frames 0, 2, 2 of a 6-frame window; blanks in between skip.
    #[test]
    fn emits_tokens_at_their_frames_and_runs_the_predictor_only_after_tokens() {
        let mut s = Script::new(&[
            // frame 0: token 1, stay (duration 0)
            (0, BLANK, 1, 0),
            // frame 0 after 1: blank jumping 2 frames
            (0, 1, BLANK, 2),
            // frame 2 after 1: token 2, stay
            (2, 1, 2, 0),
            // frame 2 after 2: token 3 jumping 3 frames
            (2, 2, 3, 3),
        ]);
        let h = decode(&mut s, 6, 6, &limits()).unwrap();
        assert_eq!(h.tokens, [1, 2, 3]);
        assert_eq!(h.frames, [0, 2, 2]);
        assert_eq!(h.durations, [0, 0, 3]);
        // Primed with blank, then once per token (blanks never run it).
        assert_eq!(s.predicted, [BLANK, 1, 2, 3]);
        // Main loop: 0 (token 1), 0 (blank +2), 2 (token 2), 2 (token 3, +3), 5 (blank) -> 6 ends.
        // Tail: the end frame clamped to 5, the last frame 5, the one before 4, ... five blanks.
        assert_eq!(s.joints, [0, 0, 2, 2, 5, 5, 5, 4, 5, 5]);
    }

    #[test]
    fn a_blank_with_duration_zero_still_advances() {
        let mut s = Script::new(&[(0, BLANK, BLANK, 0), (1, BLANK, 7, 1)]);
        let h = decode(&mut s, 4, 4, &limits()).unwrap();
        assert_eq!((h.tokens, h.frames), (vec![7], vec![1]));
    }

    #[test]
    fn a_repeated_token_at_one_frame_with_duration_zero_is_pushed_on() {
        // Token 4 at frame 1 keeps predicting duration 0; the second one is moved to duration 1.
        let mut s = Script::new(&[(1, BLANK, 4, 0), (1, 4, 4, 0), (2, 4, 5, 1)]);
        let h = decode(&mut s, 5, 5, &limits()).unwrap();
        assert_eq!(h.tokens, [4, 4, 5]);
        assert_eq!(h.frames, [1, 1, 2]);
        assert_eq!(h.durations, [0, 1, 1]);
    }

    #[test]
    fn too_many_symbols_at_one_frame_force_the_decoder_on() {
        // Frame 0 always says token 3 with duration 0: the second emission there is pushed to
        // duration 1, and with max_symbols 2 the decoder is then forced one more frame on, so
        // frame 1 is skipped.
        let mut s = Script::new(&[(0, BLANK, 3, 0), (0, 3, 3, 0)]);
        let mut l = limits();
        l.max_symbols = 2;
        let h = decode(&mut s, 3, 3, &l).unwrap();
        assert_eq!((h.tokens, h.frames), (vec![3, 3], vec![0, 0]));
        assert_eq!(&s.joints[..3], &[0, 0, 2]);
    }

    #[test]
    fn the_token_limit_stops_the_window() {
        let rows: Vec<(usize, usize, usize, usize)> = (0..20)
            .map(|f| (f, if f == 0 { BLANK } else { 1 }, 1, 1))
            .collect();
        let mut s = Script::new(&rows);
        let mut l = limits();
        l.max_tokens = 4;
        let h = decode(&mut s, 20, 20, &l).unwrap();
        // Four from the main loop; the tail pass then adds what the last frames hold.
        assert_eq!(&h.frames[..4], &[0, 1, 2, 3]);
        assert!(h.frames[4..].iter().all(|&f| f >= 4));
    }

    #[test]
    fn padding_frames_are_not_decoded_and_tiny_windows_are_empty() {
        let mut s = Script::new(&[(7, BLANK, 2, 1)]);
        // The encoder has 10 frames but the audio covers 5: the main loop stays below 5 and the
        // tail pass looks at most one frame past (FluidAudio's end-frame candidate); 7 is never
        // looked at.
        let h = decode(&mut s, 10, 5, &limits()).unwrap();
        assert!(h.tokens.is_empty());
        assert_eq!(&s.joints[..5], &[0, 1, 2, 3, 4]);
        assert!(s.joints.iter().all(|&f| f <= 5), "{:?}", s.joints);
        let mut s = Script::new(&[]);
        assert_eq!(
            decode(&mut s, 1, 1, &limits()).unwrap(),
            Hypothesis::default()
        );
        assert!(s.joints.is_empty() && s.predicted.is_empty());
    }

    #[test]
    fn the_tail_pass_recovers_a_token_at_the_last_frame() {
        // A token predicted at the last frame with a duration that steps past the end is dropped
        // by the main loop; the tail pass looks at that frame again and keeps it.
        let mut s = Script::new(&[(3, BLANK, 6, 2)]);
        let h = decode(&mut s, 4, 4, &limits()).unwrap();
        assert_eq!(h.tokens, [6]);
        assert_eq!(h.frames, [3]);
    }

    #[test]
    fn a_bad_duration_bin_is_an_error() {
        let mut s = Script::new(&[(0, BLANK, 1, 9)]);
        assert_eq!(
            decode(&mut s, 3, 3, &limits()).unwrap_err().code,
            "asr_failed"
        );
    }
}
