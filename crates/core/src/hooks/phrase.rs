//! Phrase triggers (dot-4uzgol.111.6, docs/design/hooks.md L2): `transcript.phrase` fires a hook
//! when a final utterance (a `segment` of kind `live` or `final`, never a partial) that survived
//! echo removal says one of its `match.phrases`.
//!
//! A literal phrase matches the normalized utterance text (`normalize`: case-folded, punctuation
//! stripped, whitespace collapsed) on word boundaries; a `re:` phrase is a regex run on the same
//! normalized text. `data.live` is true for the live pass while recording, false for the final
//! pass and for anything transcribed from a file (`segment.offline`). `match.speaker` is `you`
//! (the mic: `you`, `room_*`), `remote` (everyone else) or `any`. `match.cooldown` (default `app.hook_cooldown`) holds per hook per session, in
//! recording time: a hook does not fire on an utterance that starts within the cooldown of one it
//! fired on, so a replay or the final pass judges the same way the live pass did.
//!
//! The dedup key is `<session>:transcript.phrase:<utterance id>:<phrase>`. An utterance's id is
//! `<speaker>@<start_ms>`; when the final pass re-sees a phrase a live utterance already said (the
//! same hook and phrase, the same side, overlapping in time) it takes that utterance's id, so one
//! spoken phrase fires once across the live and the final pass. `judge` is pure over a session's
//! `State`; the engine's bus tap keeps one `State` per session.

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Value};

use super::{matches, Hook, Verdict};
use crate::config::keys;
use crate::config::typed::Dur;

/// The event name.
pub const EVENT: &str = "transcript.phrase";

/// The segment kinds that are final utterances: the live pass's and the final pass's. Anything
/// else (a partial) never fires.
pub const FINAL_KINDS: &[&str] = &["live", "final"];

/// `s` case-folded with its punctuation stripped: apostrophes go (`don't` is `dont`), any other
/// character that is neither a letter, a digit nor whitespace is a word break (`follow-up` is
/// `follow up`), and runs of whitespace are one space, trimmed.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if matches!(c, '\'' | '\u{2019}' | '\u{02bc}') {
            // An apostrophe joins its word.
        } else {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A compiled phrase.
#[derive(Debug, Clone)]
pub enum Phrase {
    /// The normalized words, matched on word boundaries.
    Words(String),
    Regex(regex::Regex),
}

impl Phrase {
    /// Whether the phrase is in `norm` (already `normalize`d text).
    pub fn is_in(&self, norm: &str) -> bool {
        match self {
            Phrase::Words(w) => format!(" {norm} ").contains(&format!(" {w} ")),
            Phrase::Regex(re) => re.is_match(norm),
        }
    }
}

/// Compile a `match.phrases` entry: `re:<regex>`, else a literal phrase. Err says what is wrong.
pub fn compile(src: &str) -> std::result::Result<Phrase, String> {
    if let Some(re) = src.strip_prefix("re:") {
        return regex::Regex::new(re)
            .map(Phrase::Regex)
            .map_err(|e| format!("bad regex \"{re}\": {e}"));
    }
    let w = normalize(src);
    if w.is_empty() {
        return Err(format!("phrase \"{src}\" has no words"));
    }
    Ok(Phrase::Words(w))
}

/// The first of `phrases` (in order) that `text` says. Err: a phrase that does not compile.
pub fn first_match<'a>(
    phrases: &'a [String],
    text: &str,
) -> std::result::Result<Option<&'a String>, String> {
    let norm = normalize(text);
    for p in phrases {
        if compile(p)?.is_in(&norm) {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

/// The side a speaker id is on: `you` for the mic (`you`, `room_1`, ...), else `remote`.
pub fn side(speaker: &str) -> &'static str {
    if speaker == "you" || speaker.starts_with("room_") {
        "you"
    } else {
        "remote"
    }
}

/// One utterance, as `data.utterance` carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub id: String,
    pub text: String,
    pub speaker: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Utterance {
    /// The utterance a `segment` payload carries (`start` / `end` in seconds); None when it is no
    /// final utterance (a partial, or no text).
    pub fn of_segment(seg: &Value) -> Option<Utterance> {
        let kind = seg["kind"].as_str().unwrap_or_default();
        let text = seg["text"].as_str().unwrap_or_default();
        if !FINAL_KINDS.contains(&kind) || text.trim().is_empty() {
            return None;
        }
        let ms = |k: &str| (seg[k].as_f64().unwrap_or_default().max(0.0) * 1000.0).round() as u64;
        let speaker = seg["speaker"].as_str().unwrap_or_default().to_string();
        let start_ms = ms("start");
        Some(Utterance {
            id: format!("{speaker}@{start_ms}"),
            text: text.to_string(),
            speaker,
            start_ms,
            end_ms: ms("end").max(start_ms),
        })
    }

    fn overlaps(&self, o: &Utterance) -> bool {
        (self.start_ms < o.end_ms && o.start_ms < self.end_ms)
            || (self.start_ms == o.start_ms && self.end_ms == o.end_ms)
    }

    pub fn json(&self) -> Value {
        json!({"id": self.id, "text": self.text, "speaker": self.speaker,
               "start_ms": self.start_ms, "end_ms": self.end_ms})
    }
}

/// One session's phrase state: the hits seen (to give a re-seen phrase its first utterance's id),
/// each hook's fire times (recording ms, for the cooldown) and the (hook, dedup key) pairs fired.
#[derive(Debug, Default)]
pub struct State {
    seen: Vec<(String, String, Utterance)>,
    fires: HashMap<String, Vec<u64>>,
    fired: BTreeSet<(String, String)>,
}

impl State {
    /// The id `u` goes by for `hook` and `phrase`: an earlier hit's when it is the same speech
    /// (same hook, phrase and side, overlapping in time), else its own. Remembers the hit.
    fn id_for(&mut self, hook: &str, phrase: &str, u: &Utterance) -> String {
        let same = self.seen.iter().find(|(h, p, o)| {
            h == hook && p == phrase && side(&o.speaker) == side(&u.speaker) && o.overlaps(u)
        });
        match same {
            Some((_, _, o)) => o.id.clone(),
            None => {
                self.seen
                    .push((hook.to_string(), phrase.to_string(), u.clone()));
                u.id.clone()
            }
        }
    }

    /// A fire of `hook` within `cooldown_ms` of `at_ms` (recording time).
    fn cooling(&self, hook: &str, at_ms: u64, cooldown_ms: u64) -> Option<u64> {
        if cooldown_ms == 0 {
            return None;
        }
        self.fires
            .get(hook)
            .into_iter()
            .flatten()
            .copied()
            .find(|f| f.abs_diff(at_ms) < cooldown_ms)
    }
}

/// One hook's decision on one utterance.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub hook: String,
    pub dedup: String,
    /// The event's `data`: `{session, kind, live, phrase, utterance}`.
    pub data: Value,
    pub verdict: Verdict,
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Judge one `segment` payload for every `transcript.phrase` hook in `hooks` (label, hook).
/// `base` is the envelope of the session (its `session` and `calendar`; `event`, `dedup` and
/// `data` are set here). A hook whose phrases the utterance does not say gets no decision (the
/// event did not happen for it); one that says several fires for the first in its list. On a
/// match the verdict is `matches` (speaker, phrases, the other `match` fields), then dedup, then
/// the cooldown (`match.cooldown`, else `default_cooldown`). A fire is recorded in `state`.
pub fn judge(
    state: &mut State,
    hooks: &[(String, Hook)],
    base: &Value,
    segment: &Value,
    default_cooldown: Dur,
) -> Vec<Decision> {
    let Some(mut u) = Utterance::of_segment(segment) else {
        return Vec::new();
    };
    let Some(spec) = super::events::find(EVENT) else {
        return Vec::new();
    };
    let session = segment["session"]
        .as_str()
        .or(base["session"]["id"].as_str())
        .unwrap_or_default()
        .to_string();
    let kind = segment["kind"].as_str().unwrap_or_default();
    let own_id = u.id.clone();
    let mut out = Vec::new();
    for (label, hook) in hooks.iter().filter(|(_, h)| h.event == EVENT) {
        let Ok(Some(phrase)) = first_match(&hook.filter.phrases, &u.text) else {
            continue;
        };
        u.id = own_id.clone();
        u.id = state.id_for(label, phrase, &u);
        let live = kind == "live" && segment["offline"] != true;
        let data = json!({"session": session, "kind": kind, "live": live,
                          "phrase": phrase, "utterance": u.json()});
        let dedup = spec.dedup_key(&session, "segment", &data, 0);
        let mut env = base.clone();
        env["event"] = json!(EVENT);
        env["dedup"] = json!(dedup);
        env["data"] = data.clone();
        let mut v = matches(hook, &env);
        if v.fire && state.fired.contains(&(label.clone(), dedup.clone())) {
            v = Verdict {
                fire: false,
                reason: format!("already fired for {dedup} ({})", v.reason),
            };
        }
        let cd = hook.filter.cooldown.unwrap_or(default_cooldown).0;
        if v.fire {
            if let Some(at) = state.cooling(label, u.start_ms, cd) {
                v = Verdict {
                    fire: false,
                    reason: format!(
                        "match.cooldown: fired at {} (recording time), within {} of {} ({})",
                        clock(at),
                        keys::format_duration_ms(cd),
                        clock(u.start_ms),
                        v.reason
                    ),
                };
            }
        }
        if v.fire {
            state.fired.insert((label.clone(), dedup.clone()));
            state
                .fires
                .entry(label.clone())
                .or_default()
                .push(u.start_ms);
        }
        out.push(Decision {
            hook: label.clone(),
            dedup,
            data,
            verdict: v,
        });
    }
    out
}

/// Whether any `transcript.phrase` hook in `hooks` could fire on `segment` (a final utterance
/// saying one of its phrases): the bus tap's cheap test before it reads the session.
pub fn any_hit(hooks: &[(String, Hook)], segment: &Value) -> bool {
    let Some(u) = Utterance::of_segment(segment) else {
        return false;
    };
    hooks
        .iter()
        .filter(|(_, h)| h.event == EVENT)
        .any(|(_, h)| matches!(first_match(&h.filter.phrases, &u.text), Ok(Some(_))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation_table() {
        let cases = [
            ("Pricing", "pricing"),
            ("  Let's talk   PRICING, ok?  ", "lets talk pricing ok"),
            ("Don\u{2019}t", "dont"),
            ("follow-up / next steps.", "follow up next steps"),
            ("U.S. pricing!", "u s pricing"),
            ("Éclair ÜBER", "éclair über"),
            ("$100k", "100k"),
            ("...", ""),
        ];
        for (raw, want) in cases {
            assert_eq!(normalize(raw), want, "{raw:?}");
        }
    }

    #[test]
    fn phrases_match_on_word_boundaries() {
        let hit = |p: &str, t: &str| compile(p).unwrap().is_in(&normalize(t));
        assert!(hit("pricing", "So, PRICING?"));
        assert!(hit("Next steps", "what are the next   steps."));
        assert!(!hit("price", "pricing"));
        assert!(!hit("pricing", "repricing"));
        assert!(hit("follow-up", "a follow up call"));
        assert!(hit("re:\\bdiscounts?\\b", "Any discounts"));
        assert!(!hit("re:\\bdiscounts?\\b", "discountsy"));
        assert!(compile("?!").is_err());
        assert!(compile("re:(").is_err());
    }

    #[test]
    fn sides() {
        assert_eq!(side("you"), "you");
        assert_eq!(side("room_2"), "you");
        assert_eq!(side("remote_1"), "remote");
        assert_eq!(side("them"), "remote");
    }

    #[test]
    fn partials_and_empty_text_are_no_utterance() {
        let seg = |kind: &str, text: &str| json!({"session": "s", "kind": kind, "speaker": "you", "text": text, "start": 1.5, "end": 2.0});
        assert!(Utterance::of_segment(&seg("partial", "pricing")).is_none());
        assert!(Utterance::of_segment(&seg("live", "  ")).is_none());
        let u = Utterance::of_segment(&seg("final", "pricing")).unwrap();
        assert_eq!(
            (u.id.as_str(), u.start_ms, u.end_ms),
            ("you@1500", 1500, 2000)
        );
    }
}
