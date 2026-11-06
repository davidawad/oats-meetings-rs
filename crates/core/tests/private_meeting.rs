//! David's first real meeting (2026-11-06) as a local-only test case (dot-4uzgol.84): the
//! oats-rs take (audio.wav, transcripts) and the OpenOats take of the same meeting, copied into
//! the gitignored tests/fixtures/private/ by ci/private-meeting.sh on carbon. PRIVILEGED: the
//! measurements are `#[ignore]`d and skip when the dir is absent, so pods pass without it, and
//! they print numbers only (`PRIVATE_ROW` lines), never transcript text; no assertion message
//! carries text either.
//!
//!   cargo test -p oats-core --test private_meeting -- --ignored --nocapture --test-threads=1
//!
//! The one test that always runs is the guard: nothing under the dir may ever be tracked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use oats_core::config::resolve::{defaults_flat, nest};
use oats_core::config::typed::Config;
use oats_core::sessions::Store;
use oats_core::stt::diarize::{self, Diarization};
use oats_core::stt::wer::{wer, words};
use oats_core::stt::{is_echo, wav, Tuning, Utt};
use serde_json::Value;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `OATS_PRIVATE_MEETING`, else tests/fixtures/private; `None` (with a note) when absent.
fn fixture() -> Option<PathBuf> {
    let p = std::env::var_os("OATS_PRIVATE_MEETING")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo().join("tests/fixtures/private"));
    if p.is_dir() {
        Some(p)
    } else {
        eprintln!(
            "skipped: {} is absent (carbon only: bash ci/private-meeting.sh)",
            p.display()
        );
        None
    }
}

/// The one `session_*` directory under `take`.
fn session(fx: &Path, take: &str) -> PathBuf {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(fx.join(take))
        .unwrap_or_else(|_| panic!("{take}/ missing from the fixture"))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("session.json").is_file())
        .collect();
    dirs.sort();
    assert_eq!(dirs.len(), 1, "{take}/ should hold exactly one session");
    dirs.remove(0)
}

fn row(test: &str, kv: &[(&str, String)]) {
    let kv: Vec<String> = kv.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("PRIVATE_ROW {test} {}", kv.join(" "));
}

fn config() -> Config {
    serde_json::from_value(nest(&defaults_flat())).unwrap()
}

fn jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a transcript line is not JSON"))
        .collect()
}

fn text_of(u: &Value) -> String {
    ["refinedText", "text"]
        .iter()
        .filter_map(|k| u.get(*k).and_then(Value::as_str))
        .find(|t| !t.trim().is_empty())
        .unwrap_or_default()
        .to_string()
}

fn is_you(u: &Value) -> bool {
    u["speaker"] == "you"
}

/// The session's transcript the app shows: final, else live.
fn transcript(dir: &Path) -> Vec<Value> {
    let f = jsonl(&dir.join("transcript.final.jsonl"));
    if f.is_empty() {
        jsonl(&dir.join("transcript.live.jsonl"))
    } else {
        f
    }
}

fn utt(u: &Value) -> Option<Utt> {
    Some(Utt {
        speaker: u["speaker"].as_str()?.to_string(),
        text: text_of(u),
        start: u["startSeconds"].as_f64()?,
        end: u["endSeconds"].as_f64()?,
    })
}

fn joined(us: &[Value], keep: impl Fn(&Value) -> bool) -> String {
    us.iter()
        .filter(|u| keep(u))
        .map(text_of)
        .collect::<Vec<_>>()
        .join(" ")
}

fn f3(x: f64) -> String {
    format!("{x:.3}")
}

#[test]
fn private_fixture_is_gitignored_and_never_tracked() {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo())
            .args(args)
            .output()
    };
    let Ok(ls) = git(&["ls-files", "--", "tests/fixtures/private"]) else {
        eprintln!("skipped: git is not available");
        return;
    };
    if !ls.status.success() {
        eprintln!("skipped: not a git checkout");
        return;
    }
    let tracked = String::from_utf8_lossy(&ls.stdout).lines().count();
    assert_eq!(
        tracked, 0,
        "{tracked} file(s) under tests/fixtures/private/ are tracked: git rm -r --cached tests/fixtures/private"
    );
    let ignored = git(&["check-ignore", "-q", "tests/fixtures/private/audio.wav"]).unwrap();
    assert!(
        ignored.status.success(),
        "tests/fixtures/private/ is not gitignored"
    );
    let hook = std::fs::read_to_string(repo().join("ci/hooks/pre-commit")).unwrap();
    assert!(hook.contains("tests/fixtures/private"));
}

/// Remote-speaker count and swaps, re-diarizing the system channel of audio.wav at the
/// oats-rs take's remote utterance times with the current diarizer and default config. Against
/// turns.tsv (hand-labelled `start\tend\tspeaker`) when present; else turns.seed.tsv is written
/// from the take's own labels for David to correct and rename.
#[test]
#[ignore = "private fixture, carbon only"]
fn diarization_speaker_count_and_swaps() {
    let Some(fx) = fixture() else { return };
    let dir = session(&fx, "oats-rs");
    let cfg = config();
    // Through Settings, so the speaker models are found where the app looks (stt.local.models_dir).
    let d = oats_core::stt::Settings::from_config(&cfg, &fx)
        .expect("stt settings")
        .diarize
        .unwrap_or_else(|| Diarization::spectral(1, 0.0, 0));
    let chans = wav::read(&dir.join("audio.wav")).expect("audio.wav does not decode");
    let take: Vec<Value> = jsonl(&dir.join("transcript.final.jsonl"));
    let mut remote: Vec<Utt> = take.iter().filter(|u| !is_you(u)).filter_map(utt).collect();
    remote.sort_by(|a, b| a.start.total_cmp(&b.start));
    let recorded: Vec<String> = remote.iter().map(|u| u.speaker.clone()).collect();
    let algorithm = diarize::Diarizer::new(d.clone()).algorithm();
    diarize::label_final(&d, &mut remote, &chans.them, &cfg.stt.speaker_them);
    let hyp: Vec<&str> = remote.iter().map(|u| u.speaker.as_str()).collect();
    let count = |l: &[&str]| l.iter().collect::<BTreeSet<_>>().len();
    let changes = |l: &[&str]| l.windows(2).filter(|w| w[0] != w[1]).count();
    let rec: Vec<&str> = recorded.iter().map(String::as_str).collect();
    let agree = rec.iter().zip(&hyp).filter(|(a, b)| a == b).count();
    let mut kv = vec![
        ("algorithm", algorithm.as_str().to_string()),
        ("seconds", format!("{:.0}", chans.seconds())),
        ("remote_utts", remote.len().to_string()),
        ("speakers_now", count(&hyp).to_string()),
        ("speakers_recorded", count(&rec).to_string()),
        ("label_changes_now", changes(&hyp).to_string()),
        ("agree_with_recorded", agree.to_string()),
    ];

    let turns_path = fx.join("turns.tsv");
    let Ok(turns_src) = std::fs::read_to_string(&turns_path) else {
        let seed: String = std::iter::once(
            "# UNREVIEWED: the app's labels; correct the speaker column, drop this line, rename to turns.tsv\n"
                .to_string(),
        )
        .chain(
            remote
                .iter()
                .map(|u| format!("{:.2}\t{:.2}\t{}\n", u.start, u.end, u.speaker)),
        )
        .collect();
        std::fs::write(fx.join("turns.seed.tsv"), seed).unwrap();
        kv.push(("turns", "absent_seeded".into()));
        row("diarization", &kv);
        return;
    };
    let reviewed = !turns_src.contains("UNREVIEWED");
    let turns: Vec<(f64, f64, String)> = turns_src
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert!(
                f.len() >= 3,
                "turns.tsv: a line without 3 tab-separated fields"
            );
            (
                f[0].trim().parse().expect("turns.tsv: bad start"),
                f[1].trim().parse().expect("turns.tsv: bad end"),
                f[2].trim().to_string(),
            )
        })
        .collect();
    // Each remote utterance's true speaker: the turn holding its midpoint.
    let pairs: Vec<(&str, &str)> = remote
        .iter()
        .zip(&hyp)
        .filter_map(|(u, h)| {
            let mid = (u.start + u.end) / 2.0;
            turns
                .iter()
                .find(|t| mid >= t.0 && mid <= t.1)
                .map(|t| (t.2.as_str(), *h))
        })
        .collect();
    let truth: Vec<&str> = pairs.iter().map(|p| p.0).collect();
    // Each hypothesis label stands for the true speaker it most often covers.
    let mut votes: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for (t, h) in &pairs {
        *votes.entry(h).or_default().entry(t).or_default() += 1;
    }
    let map: BTreeMap<&str, &str> = votes
        .iter()
        .map(|(h, v)| (*h, *v.iter().max_by_key(|(_, n)| **n).unwrap().0))
        .collect();
    let wrong = pairs.iter().filter(|(t, h)| map[h] != *t).count();
    // A swap: the label changes where the speaker does not (false), or stays where it does (missed).
    let (mut false_sw, mut missed_sw) = (0, 0);
    for w in pairs.windows(2) {
        match (w[0].0 == w[1].0, w[0].1 == w[1].1) {
            (true, false) => false_sw += 1,
            (false, true) => missed_sw += 1,
            _ => {}
        }
    }
    kv.extend([
        ("turns_reviewed", reviewed.to_string()),
        ("labelled_utts", pairs.len().to_string()),
        ("speakers_true", count(&truth).to_string()),
        ("mislabelled", wrong.to_string()),
        (
            "mislabel_rate",
            f3(wrong as f64 / pairs.len().max(1) as f64),
        ),
        ("false_swaps", false_sw.to_string()),
        ("missed_swaps", missed_sw.to_string()),
    ]);
    row("diarization", &kv);
}

/// Speaker echo the canceller and the dedupe let through on the `you` channel of the oats-rs
/// take: `you` utterances that say what remote speech said at nearly the same time, by the
/// app's own rule (`stt::is_echo`, default `stt.echo_*`). Also the you channel's level during
/// remote-only speech against its level in remote silence (acoustic leak, dB).
#[test]
#[ignore = "private fixture, carbon only"]
fn echo_leak_on_the_you_channel() {
    let Some(fx) = fixture() else { return };
    let dir = session(&fx, "oats-rs");
    let mut kv = vec![];
    let d = Tuning::from_config(&config())
        .echo
        .expect("audio.echo_cancel is on by default");
    for kind in ["final", "live"] {
        let us: Vec<Utt> = jsonl(&dir.join(format!("transcript.{kind}.jsonl")))
            .iter()
            .filter_map(utt)
            .collect();
        let them: Vec<Utt> = us.iter().filter(|u| u.speaker != "you").cloned().collect();
        let you: Vec<&Utt> = us.iter().filter(|u| u.speaker == "you").collect();
        let leaked = you.iter().filter(|u| is_echo(u, &them, &d)).count();
        kv.push((
            if kind == "final" {
                "final_you"
            } else {
                "live_you"
            },
            you.len().to_string(),
        ));
        kv.push((
            if kind == "final" {
                "final_echo"
            } else {
                "live_echo"
            },
            leaked.to_string(),
        ));
        kv.push((
            if kind == "final" {
                "final_echo_rate"
            } else {
                "live_echo_rate"
            },
            f3(leaked as f64 / you.len().max(1) as f64),
        ));
    }
    // Acoustic leak over 100 ms frames: remote-only = system loud, and no `you` utterance near.
    let chans = wav::read(&dir.join("audio.wav")).expect("audio.wav does not decode");
    let you_spans: Vec<(f64, f64)> = transcript(&dir)
        .iter()
        .filter(|u| is_you(u))
        .filter_map(utt)
        .map(|u| (u.start - 1.0, u.end + 1.0))
        .collect();
    let frame = wav::SAMPLE_RATE as usize / 10;
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
    let n = chans.you.len().min(chans.them.len()) / frame;
    let them_rms: Vec<f32> = (0..n)
        .map(|i| rms(&chans.them[i * frame..(i + 1) * frame]))
        .collect();
    let mut sorted = them_rms.clone();
    sorted.sort_by(f32::total_cmp);
    let loud_at = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let (mut during, mut quiet) = (vec![], vec![]);
    for (i, t) in them_rms.iter().enumerate() {
        let at = i as f64 / 10.0;
        if you_spans.iter().any(|s| at >= s.0 && at <= s.1) {
            continue;
        }
        let y = rms(&chans.you[i * frame..(i + 1) * frame]);
        if *t > loud_at && *t > 1e-4 {
            during.push(y);
        } else if *t <= 1e-4 {
            quiet.push(y);
        }
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
    let db = |x: f32| 20.0 * x.max(1e-9).log10();
    kv.extend([
        ("remote_only_frames", during.len().to_string()),
        ("silent_frames", quiet.len().to_string()),
        ("you_db_remote_only", format!("{:.1}", db(mean(&during)))),
        ("you_db_silent", format!("{:.1}", db(mean(&quiet)))),
    ]);
    row("echo", &kv);
}

/// Terms from terms.txt, else words of 7+ letters the OpenOats take says at least twice.
fn terms(fx: &Path, reference: &[String]) -> Vec<String> {
    if let Ok(t) = std::fs::read_to_string(fx.join("terms.txt")) {
        return t
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_lowercase)
            .collect();
    }
    let mut n: BTreeMap<&str, usize> = BTreeMap::new();
    for w in reference.iter().filter(|w| w.chars().count() >= 7) {
        *n.entry(w).or_default() += 1;
    }
    n.into_iter()
        .filter(|(_, c)| *c >= 2)
        .map(|(w, _)| w.to_string())
        .collect()
}

/// Occurrences of `term` (one or more words) in `ws`.
fn occurrences(ws: &[String], term: &str) -> usize {
    let t = words(term);
    if t.is_empty() || t.len() > ws.len() {
        return 0;
    }
    ws.windows(t.len()).filter(|w| *w == t.as_slice()).count()
}

/// The oats-rs take against the OpenOats take as the baseline: WER overall and per side, and
/// term hits (occurrences the oats-rs take shares with the baseline over the baseline's).
#[test]
#[ignore = "private fixture, carbon only"]
fn wer_and_term_hits_against_the_openoats_take() {
    let Some(fx) = fixture() else { return };
    let ours = transcript(&session(&fx, "oats-rs"));
    let base = transcript(&session(&fx, "openoats"));
    assert!(!base.is_empty(), "the OpenOats take has no transcript");
    let all = |_: &Value| true;
    let remote = |u: &Value| !is_you(u);
    let mut kv = vec![
        ("ours_utts", ours.len().to_string()),
        ("base_utts", base.len().to_string()),
        ("ours_words", words(&joined(&ours, all)).len().to_string()),
        ("base_words", words(&joined(&base, all)).len().to_string()),
        ("wer", f3(wer(&joined(&base, all), &joined(&ours, all)))),
        (
            "wer_you",
            f3(wer(&joined(&base, is_you), &joined(&ours, is_you))),
        ),
        (
            "wer_remote",
            f3(wer(&joined(&base, remote), &joined(&ours, remote))),
        ),
    ];
    let (bw, ow) = (words(&joined(&base, all)), words(&joined(&ours, all)));
    let ts = terms(&fx, &bw);
    let (mut want, mut got, mut terms_found) = (0, 0, 0);
    for t in &ts {
        let (b, o) = (occurrences(&bw, t), occurrences(&ow, t));
        want += b;
        got += o.min(b);
        terms_found += usize::from(o > 0);
    }
    let notes = std::fs::read_to_string(fx.join("openoats-notes.md")).unwrap_or_default();
    let nw = words(&notes);
    kv.extend([
        (
            "terms_from",
            if fx.join("terms.txt").is_file() {
                "terms.txt"
            } else {
                "auto"
            }
            .into(),
        ),
        ("terms", ts.len().to_string()),
        ("terms_found", terms_found.to_string()),
        ("term_hits", got.to_string()),
        ("term_base", want.to_string()),
        ("term_recall", f3(got as f64 / want.max(1) as f64)),
        (
            "terms_in_notes",
            ts.iter()
                .filter(|t| occurrences(&nw, t) > 0)
                .count()
                .to_string(),
        ),
    ]);
    row("wer", &kv);
}

/// Both takes import into one store and read back unchanged: the same files byte for byte, the
/// same utterance count as their JSONL, OpenOats fields on every oats-rs utterance, and a
/// re-import is a no-op.
#[test]
#[ignore = "private fixture, carbon only"]
fn openoats_compat_round_trip() {
    let Some(fx) = fixture() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let mut kv = vec![];
    for take in ["oats-rs", "openoats"] {
        let src = session(&fx, take);
        let r = store.import(&src, false).expect("import failed");
        let id = r["imported"][0]
            .as_str()
            .expect("nothing imported")
            .to_string();
        let again = store.import(&src, false).expect("re-import failed");
        assert_eq!(again["skipped"].as_array().map(Vec::len), Some(1), "{take}");
        let want = transcript(&src);
        let got = store.transcript(&id).unwrap();
        let got = got["utterances"].as_array().unwrap();
        assert_eq!(got.len(), want.len(), "{take}: utterance count");
        assert!(got == &want, "{take}: utterances changed on import");
        let mut same_files = 0;
        for e in std::fs::read_dir(&src).unwrap().flatten() {
            let name = e.file_name();
            if !e.file_type().unwrap().is_file() || name == "session.json" {
                continue;
            }
            let a = std::fs::read(e.path()).unwrap();
            let b = std::fs::read(tmp.path().join(&id).join(&name)).unwrap_or_default();
            assert!(a == b, "{take}: a file changed on import");
            same_files += 1;
        }
        let fields_ok = want
            .iter()
            .filter(|u| {
                u["speaker"].is_string()
                    && u["text"].is_string()
                    && u["timestamp"]
                        .as_str()
                        .is_some_and(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok())
            })
            .count();
        assert_eq!(
            fields_ok,
            want.len(),
            "{take}: utterances missing OpenOats fields"
        );
        let summary = store.get(&id).unwrap();
        assert_eq!(summary["utterance_count"].as_u64(), Some(want.len() as u64));
        let key = if take == "oats-rs" {
            "oats_rs"
        } else {
            "openoats"
        };
        kv.push((key, format!("{}utts/{}files", want.len(), same_files)));
    }
    let listed = store.list(None, None).unwrap()["count"].as_u64().unwrap();
    assert_eq!(listed, 2);
    kv.push(("listed", listed.to_string()));
    row("compat", &kv);
}

/// The final pass re-run with this build over the oats-rs take's audio.wav (default config, the
/// configured local model), against the OpenOats take: WER per side, echo left on the `you`
/// channel, and the per-stage mic counts (dot-4uzgol.93); `wer_you` and `kept` again with the echo
/// dedupe off (`_no_dedupe`, dot-4uzgol.94). The recorded transcripts are what an older build
/// wrote; this is what the current one writes. Skips, saying why, when this test
/// binary has no runtime for the model (add `--features oats-core/parakeet` or `whisper`).
#[test]
#[ignore = "private fixture, carbon only"]
fn final_pass_rerun_against_the_openoats_take() {
    use oats_core::stt::{final_utts, make_asr, Settings};
    let Some(fx) = fixture() else { return };
    let dir = session(&fx, "oats-rs");
    let cfg = config();
    let sessions = oats_core::sessions::default_sessions_dir(&|k| std::env::var(k).ok());
    let s = Settings::from_config(&cfg, &sessions).expect("default stt settings");
    let asr = match make_asr(&s, false, &oats_core::secrets::MemStore::default()) {
        Ok(a) => a,
        Err(e) => {
            row(
                "rerun",
                &[("asr", "unavailable".into()), ("code", e.code.into())],
            );
            return;
        }
    };
    let chans = wav::read(&dir.join("audio.wav")).expect("audio.wav does not decode");
    let (ours, stages) = final_utts(&*asr, &s, &chans).expect("final pass failed");
    // The same pass with the echo dedupe off (dot-4uzgol.94): what the dedupe costs the you side.
    let mut raw = s.clone();
    raw.tuning.echo = None;
    let (all, raw_stages) = final_utts(&*asr, &raw, &chans).expect("final pass failed");
    let base = transcript(&session(&fx, "openoats"));
    let text = |us: &[Utt], you: bool| {
        us.iter()
            .filter(|u| (u.speaker == "you") == you)
            .map(|u| u.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let them: Vec<Utt> = ours
        .iter()
        .filter(|u| u.speaker != "you")
        .cloned()
        .collect();
    let echo = s.tuning.echo.expect("audio.echo_cancel is on by default");
    let leaked = ours
        .iter()
        .filter(|u| u.speaker == "you" && is_echo(u, &them, &echo))
        .count();
    let st = stages.to_json();
    let mut kv = vec![
        ("model", s.model.clone()),
        (
            "you_utts",
            ours.iter()
                .filter(|u| u.speaker == "you")
                .count()
                .to_string(),
        ),
        (
            "wer_you",
            f3(wer(&joined(&base, is_you), &text(&ours, true))),
        ),
        (
            "wer_remote",
            f3(wer(&joined(&base, |u| !is_you(u)), &text(&ours, false))),
        ),
        ("final_echo", leaked.to_string()),
    ];
    for k in [
        "vad_regions",
        "vad_seconds",
        "recognised",
        "echo_dropped",
        "kept",
    ] {
        kv.push((k, st[k].to_string()));
    }
    kv.push((
        "wer_you_no_dedupe",
        f3(wer(&joined(&base, is_you), &text(&all, true))),
    ));
    kv.push(("kept_no_dedupe", raw_stages.kept.to_string()));
    row("rerun", &kv);
}
