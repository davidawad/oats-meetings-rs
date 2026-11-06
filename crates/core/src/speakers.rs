//! Speaker labels (dot-4uzgol.86). Transcripts store stable ids: `you` (the microphone), `room_N`
//! (the microphone split by voice, `speakers.mic.diarize`), `remote_N` (system audio, split by
//! voice with `stt.diarization.enabled`), plus `them` in sessions OpenOats wrote. What a person
//! sees is a label, worked out here when a transcript is shown or exported from the `[speakers]`
//! table and the session's `speakerNames`, so editing the config relabels every session without
//! rewriting one:
//!
//! - a name the session gives the id (`session-speaker-name`) wins;
//! - `you` is `speakers.mic.label`; `room_N` is `speakers.mic.template` with `{n}` = N, or the mic
//!   label when the session has one room voice;
//! - `remote_N` is `speakers.system.template`, or `speakers.system.label_single` when the session
//!   has one remote voice (always so with diarization off); `them` is the single label;
//! - any other id shows as itself.
//!
//! The markdown export (`speakers.export`) uses these labels, or OpenOats' own mapping: the mic
//! side "You" and everything else "Them" unless named (dot-4uzgol.85).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::config::typed::{Speakers, SpeakersExport};
use crate::config::{ConfigError, Result};

/// The microphone channel's id.
pub const YOU: &str = "you";
/// The system-audio side in sessions OpenOats recorded (it never splits it by voice).
pub const THEM: &str = "them";
/// The first remote speaker: the system-audio channel's id when it is not split by voice.
pub const REMOTE_1: &str = "remote_1";
/// The first room speaker (the microphone channel split by voice).
pub const ROOM_1: &str = "room_1";
/// What `{n}` in a template stands for.
pub const N: &str = "{n}";

/// What a transcript id says about its speaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Id {
    You,
    Room(u32),
    Them,
    Remote(u32),
    Other,
}

fn numbered(id: &str, prefix: &str) -> Option<u32> {
    let n = id.strip_prefix(prefix)?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok().filter(|&n| n > 0)
}

pub fn parse(id: &str) -> Id {
    match id {
        YOU => Id::You,
        THEM => Id::Them,
        _ => {
            if let Some(n) = numbered(id, "remote_") {
                Id::Remote(n)
            } else if let Some(n) = numbered(id, "room_") {
                Id::Room(n)
            } else {
                Id::Other
            }
        }
    }
}

/// Whether `id` is the microphone side (`you`, `room_N`).
pub fn is_mic(id: &str) -> bool {
    matches!(parse(id), Id::You | Id::Room(_))
}

/// `template` with `{n}` replaced by `n`.
pub fn fill(template: &str, n: u32) -> String {
    template.replace(N, &n.to_string())
}

/// `config-check` (and every reload): labels are not blank, templates hold `{n}`.
pub fn check(s: &Speakers) -> Result<()> {
    let bad = |key: &str, why: &str| {
        Err(ConfigError::new(
            "invalid_value",
            format!("speakers.{key}: {why}"),
        ))
    };
    for (key, v) in [
        ("mic.label", &s.mic.label),
        ("system.label_single", &s.system.label_single),
    ] {
        if v.trim().is_empty() {
            return bad(key, "must not be empty");
        }
    }
    for (key, v) in [
        ("mic.template", &s.mic.template),
        ("system.template", &s.system.template),
    ] {
        if !v.contains(N) {
            return bad(
                key,
                &format!("\"{v}\" must contain {N}, the speaker's number"),
            );
        }
    }
    Ok(())
}

/// The labels of one session's speakers: the `[speakers]` table, the session's `speakerNames`
/// and which ids its transcript holds (one remote voice is shown differently from several).
pub struct Labels<'a> {
    cfg: &'a Speakers,
    names: Option<&'a Map<String, Value>>,
    remotes: usize,
    rooms: usize,
}

impl<'a> Labels<'a> {
    pub fn new<'i>(
        cfg: &'a Speakers,
        names: Option<&'a Map<String, Value>>,
        ids: impl IntoIterator<Item = &'i str>,
    ) -> Labels<'a> {
        let (mut remotes, mut rooms) = (BTreeSet::new(), BTreeSet::new());
        for id in ids {
            match parse(id) {
                Id::Remote(n) => {
                    remotes.insert(n);
                }
                Id::Room(n) => {
                    rooms.insert(n);
                }
                _ => {}
            }
        }
        Labels {
            cfg,
            names,
            remotes: remotes.len(),
            rooms: rooms.len(),
        }
    }

    fn named(&self, id: &str) -> Option<String> {
        self.names
            .and_then(|m| m.get(id))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
    }

    /// How the app shows `id`.
    pub fn label(&self, id: &str) -> String {
        if let Some(n) = self.named(id) {
            return n;
        }
        let c = self.cfg;
        match parse(id) {
            Id::You => c.mic.label.clone(),
            Id::Room(_) if self.rooms <= 1 => c.mic.label.clone(),
            Id::Room(n) => fill(&c.mic.template, n),
            Id::Them => c.system.label_single.clone(),
            Id::Remote(_) if self.remotes <= 1 => c.system.label_single.clone(),
            Id::Remote(n) => fill(&c.system.template, n),
            Id::Other => id.to_string(),
        }
    }

    /// OpenOats' own mapping (`Speaker.displayName`): a session name, else the mic side "You" and
    /// everything else "Them".
    pub fn openoats(&self, id: &str) -> String {
        self.named(id)
            .unwrap_or_else(|| if is_mic(id) { "You" } else { "Them" }.to_string())
    }

    /// How the markdown export names `id` (`speakers.export`).
    pub fn export(&self, id: &str) -> String {
        match self.cfg.export {
            SpeakersExport::Openoats => self.openoats(id),
            SpeakersExport::Labels => self.label(id),
        }
    }

    /// `{id: label}` for each of `ids`.
    pub fn map<'i>(&self, ids: impl IntoIterator<Item = &'i str>) -> BTreeMap<String, String> {
        ids.into_iter()
            .map(|id| (id.to_string(), self.label(id)))
            .collect()
    }
}

/// The `speaker` of each utterance object in `records`.
pub fn ids(records: &[Value]) -> impl Iterator<Item = &str> {
    records
        .iter()
        .filter_map(|r| r.get("speaker").and_then(Value::as_str))
}

/// `{id: label}` for every speaker of a `transcript-get` answer (its `utterances` and
/// `speaker_names`): what the app shows for each.
pub fn labels_of(cfg: &Speakers, transcript: &Value) -> BTreeMap<String, String> {
    let records = transcript["utterances"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let names = transcript["speaker_names"].as_object();
    let labels = Labels::new(cfg, names, ids(records));
    let mut all: BTreeSet<&str> = ids(records).collect();
    all.extend(names.into_iter().flat_map(|m| m.keys().map(String::as_str)));
    labels.map(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::typed::{SpeakersMic, SpeakersSystem};
    use serde_json::json;

    fn cfg() -> Speakers {
        Speakers {
            export: SpeakersExport::Openoats,
            mic: SpeakersMic {
                diarize: false,
                label: "You".into(),
                template: "Room {n}".into(),
            },
            system: SpeakersSystem {
                label_single: "Them".into(),
                template: "Speaker {n}".into(),
            },
        }
    }

    #[test]
    fn ids_parse() {
        assert_eq!(parse("you"), Id::You);
        assert_eq!(parse("remote_12"), Id::Remote(12));
        assert_eq!(parse("room_2"), Id::Room(2));
        assert_eq!(parse("them"), Id::Them);
        for other in ["remote_", "remote_0", "remote_x", "room_-1", "caller", ""] {
            assert_eq!(parse(other), Id::Other, "{other}");
        }
    }

    #[test]
    fn one_remote_voice_is_the_single_label_several_are_numbered() {
        let c = cfg();
        let one = Labels::new(&c, None, ["you", "remote_1", "you"]);
        assert_eq!(one.label("you"), "You");
        assert_eq!(one.label("remote_1"), "Them");
        let two = Labels::new(&c, None, ["you", "remote_1", "remote_2"]);
        assert_eq!(two.label("remote_1"), "Speaker 1");
        assert_eq!(two.label("remote_2"), "Speaker 2");
        let rooms = Labels::new(&c, None, ["room_1", "room_2", "remote_1"]);
        assert_eq!(rooms.label("room_2"), "Room 2");
        assert_eq!(rooms.label("remote_1"), "Them");
        assert_eq!(Labels::new(&c, None, ["room_1"]).label("room_1"), "You");
        assert_eq!(one.label("caller"), "caller");
    }

    #[test]
    fn session_names_win_and_blank_ones_do_not() {
        let c = cfg();
        let names = json!({"remote_2": "Dana", "you": " ", "room_1": "Lee"});
        let m = names.as_object();
        let l = Labels::new(&c, m, ["you", "remote_1", "remote_2", "room_1"]);
        assert_eq!(l.label("remote_2"), "Dana");
        assert_eq!(l.label("remote_1"), "Speaker 1");
        assert_eq!(l.label("you"), "You");
        assert_eq!(l.openoats("remote_2"), "Dana");
        assert_eq!(l.openoats("remote_1"), "Them");
        assert_eq!(l.openoats("room_1"), "Lee");
        assert_eq!(l.openoats("room_2"), "You");
    }

    #[test]
    fn templates_need_n_and_labels_text() {
        check(&cfg()).unwrap();
        let mut c = cfg();
        c.system.template = "Speaker".into();
        let e = check(&c).unwrap_err();
        assert_eq!(e.code, "invalid_value");
        assert!(e.message.contains("speakers.system.template"), "{e}");
        let mut c = cfg();
        c.mic.label = "  ".into();
        assert!(check(&c)
            .unwrap_err()
            .message
            .contains("speakers.mic.label"));
    }
}
