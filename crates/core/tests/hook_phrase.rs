//! dot-4uzgol.111.6 (docs/design/hooks.md L2): `transcript.phrase` fires a hook when a final
//! utterance says one of its `match.phrases`. The normalisation table, the speaker filter, and
//! property tests over `hooks::phrase::judge` (seeded, many cases each): one phrase in one final
//! utterance fires exactly once; partials never fire; the cooldown holds and is no stricter than
//! it says; one spoken phrase fires once across the live pass, the final pass and a replay. Then
//! the engine: segments on the bus fire the hook with a valid envelope, `hook-decided` says why,
//! `hook-explain` judges a given utterance, and `config-check` warns under deferred transcription.

use oats_core::config::typed::Dur;
use oats_core::hooks::phrase::{self, Decision, State};
use oats_core::hooks::{self, Hook};
use serde_json::{json, Value};

/// A small deterministic generator (xorshift64*), so a failing case names its seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

const SEEDS: u64 = 300;

/// Words none of the test phrases are made of.
const FILLER: &[&str] = &[
    "so", "we", "should", "look", "at", "the", "numbers", "again", "tomorrow", "okay", "right",
    "maybe", "later", "team", "plan", "budget", "call", "thanks",
];

const PHRASES: &[&str] = &["pricing", "next steps", "action item"];

fn hook(name: &str, m: Value) -> (String, Hook) {
    let h: Hook = serde_json::from_value(
        json!({"name": name, "event": "transcript.phrase", "command": ["true"], "match": m}),
    )
    .unwrap();
    (name.to_string(), h)
}

fn base() -> Value {
    json!({"event": "transcript.phrase", "session": {"id": "s", "title": "Weekly"}, "calendar": null})
}

fn seg(kind: &str, speaker: &str, text: &str, start_ms: u64, end_ms: u64) -> Value {
    json!({"session": "s", "kind": kind, "speaker": speaker, "text": text,
           "start": start_ms as f64 / 1000.0, "end": end_ms as f64 / 1000.0})
}

/// `phrase` said in a random case with random punctuation, among filler words.
fn utterance_with(rng: &mut Rng, phrase: &str) -> String {
    let n = rng.below(8) as usize;
    let at = rng.below(n as u64 + 1) as usize;
    let mut words: Vec<String> = (0..n).map(|_| rng.pick(FILLER).to_string()).collect();
    let said: String = phrase
        .split(' ')
        .map(|w| match rng.below(3) {
            0 => w.to_uppercase(),
            1 => {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            }
            _ => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    words.insert(at, said);
    let tail = *rng.pick(&["", ".", "?", "!", ",", "..."]);
    let mut text = words.join(" ");
    text.push_str(tail);
    if rng.below(2) == 0 {
        text = format!("\u{201c}{text}\u{201d}");
    }
    text
}

fn fires(ds: &[Decision]) -> usize {
    ds.iter().filter(|d| d.verdict.fire).count()
}

const NO_COOLDOWN: Dur = Dur(0);

#[test]
fn normalisation_table() {
    let cases = [
        ("Pricing", "pricing"),
        ("PRICING?", "pricing"),
        ("  let's   talk\tpricing,  ok  ", "lets talk pricing ok"),
        ("Don\u{2019}t", "dont"),
        ("follow-up", "follow up"),
        ("next steps...", "next steps"),
        ("\u{201c}Action item!\u{201d}", "action item"),
        ("U.S. pricing", "u s pricing"),
        ("ÉCLAIR", "éclair"),
        ("$100k/month", "100k month"),
        ("!?", ""),
    ];
    for (raw, want) in cases {
        assert_eq!(phrase::normalize(raw), want, "{raw:?}");
    }
    // Phrases are normalized the same way and match whole words only.
    let said = |p: &str, t: &str| phrase::first_match(&[p.to_string()], t).unwrap().is_some();
    for (p, t, want) in [
        ("pricing", "Let's talk PRICING.", true),
        ("Pricing!", "pricing", true),
        ("next steps", "Next   steps?", true),
        ("next steps", "next, steps", true),
        ("price", "pricing", false),
        ("pricing", "repricing", false),
        ("action item", "action items", false),
        ("follow-up", "a follow up", true),
        ("re:\\bdiscounts?\\b", "Any discounts?", true),
        ("re:\\bdiscounts?\\b", "discountsy", false),
        ("re:^so\\b", "So, pricing", true),
    ] {
        assert_eq!(said(p, t), want, "{p:?} in {t:?}");
    }
    // A phrase with no words, or a bad regex, is refused naming the hook.
    for bad in ["?!", "re:("] {
        let e = hooks::parse(&json!([{"name": "x", "event": "transcript.phrase",
                                      "command": ["true"], "match": {"phrases": [bad]}}]))
        .unwrap_err();
        assert!(e.message.contains("hook x: match.phrases"), "{}", e.message);
    }
    // A phrase hook needs phrases; a phrase with glob characters is a literal.
    let e =
        hooks::parse(&json!([{"name": "x", "event": "transcript.phrase", "command": ["true"]}]))
            .unwrap_err();
    assert!(e.message.contains("needs match.phrases"), "{}", e.message);
    assert!(
        hooks::parse(&json!([{"event": "transcript.phrase", "command": ["true"],
                                  "match": {"phrases": ["[draft]"]}}]))
        .is_ok()
    );
}

#[test]
fn the_speaker_filter() {
    for (want, speaker, fire) in [
        ("you", "you", true),
        ("you", "room_2", true),
        ("you", "remote_1", false),
        ("remote", "remote_1", true),
        ("remote", "remote_3", true),
        ("remote", "them", true),
        ("remote", "you", false),
        ("any", "you", true),
        ("any", "remote_1", true),
        ("", "room_1", true),
    ] {
        let hooks = [hook("h", json!({"phrases": ["pricing"], "speaker": want}))];
        let ds = phrase::judge(
            &mut State::default(),
            &hooks,
            &base(),
            &seg("live", speaker, "pricing", 0, 900),
            NO_COOLDOWN,
        );
        assert_eq!(ds.len(), 1, "{want} / {speaker}");
        assert_eq!(
            ds[0].verdict.fire, fire,
            "{want} / {speaker}: {:?}",
            ds[0].verdict
        );
        if !fire {
            assert_eq!(
                ds[0].verdict.reason,
                format!(
                    "match.speaker = {want}: {speaker:?} is {}",
                    phrase::side(speaker)
                )
            );
        }
    }
}

#[test]
fn verdicts_say_why() {
    let hooks = [hook(
        "flag",
        json!({"phrases": ["pricing", "re:\\bdiscounts?\\b"]}),
    )];
    let mut st = State::default();
    let ds = phrase::judge(
        &mut st,
        &hooks,
        &base(),
        &seg(
            "live",
            "remote_1",
            "Any discounts on pricing?",
            61_200,
            63_900,
        ),
        Dur(120_000),
    );
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert!(d.verdict.fire);
    // The first phrase in the hook's list that is said.
    assert_eq!(
        d.verdict.reason,
        r#"match.phrases: "pricing" said in "Any discounts on pricing?""#
    );
    assert_eq!(d.dedup, "s:transcript.phrase:remote_1@61200:pricing");
    assert_eq!(
        d.data,
        json!({"session": "s", "kind": "live", "live": true, "phrase": "pricing",
               "utterance": {"id": "remote_1@61200", "text": "Any discounts on pricing?",
                             "speaker": "remote_1", "start_ms": 61_200, "end_ms": 63_900}})
    );
    // The data holds to the table's schema.
    let spec = hooks::events::find("transcript.phrase").unwrap();
    hooks::events::validate(&spec.data_schema(), &d.data).unwrap();
    // Within the cooldown: skipped, saying when it fired.
    let ds = phrase::judge(
        &mut st,
        &hooks,
        &base(),
        &seg("live", "remote_1", "pricing again", 90_000, 91_000),
        Dur(120_000),
    );
    assert_eq!(
        ds[0].verdict.reason,
        r#"match.cooldown: fired at 1:01 (recording time), within 2m of 1:30 (match.phrases: "pricing" said in "pricing again")"#
    );
    // An utterance that says none of a hook's phrases is no decision for it.
    assert!(phrase::judge(
        &mut st,
        &hooks,
        &base(),
        &seg("live", "you", "nothing here", 300_000, 301_000),
        NO_COOLDOWN
    )
    .is_empty());
    // The other match fields still hold: a title filter that fails is the reason.
    let titled = [hook(
        "t",
        json!({"phrases": ["pricing"], "title": "Standup*"}),
    )];
    let ds = phrase::judge(
        &mut State::default(),
        &titled,
        &base(),
        &seg("final", "you", "pricing", 0, 500),
        NO_COOLDOWN,
    );
    assert_eq!(
        ds[0].verdict.reason,
        r#"match.title: "Standup*" did not match "Weekly""#
    );
    assert_eq!(ds[0].data["live"], false);
}

/// One phrase in one final utterance (live or final pass) fires exactly once, however it is
/// cased and punctuated; seen again, it does not fire.
#[test]
fn property_one_phrase_in_one_final_utterance_fires_exactly_once() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let p = *rng.pick(PHRASES);
        let text = utterance_with(&mut rng, p);
        let kind = *rng.pick(&["live", "final"]);
        let speaker = *rng.pick(&["you", "remote_1", "room_1", "remote_2"]);
        let start = rng.below(3_600_000);
        let hooks = [hook("h", json!({"phrases": PHRASES}))];
        let mut st = State::default();
        let s = seg(kind, speaker, &text, start, start + 1 + rng.below(9_000));
        let cd = Dur(rng.below(600_000));
        let ds = phrase::judge(&mut st, &hooks, &base(), &s, cd);
        assert_eq!(fires(&ds), 1, "seed {seed}: {text:?} {ds:?}");
        assert_eq!(ds[0].data["phrase"], p, "seed {seed}: {text:?}");
        assert_eq!(ds[0].data["live"], kind == "live");
        let again = phrase::judge(&mut st, &hooks, &base(), &s, cd);
        assert_eq!(fires(&again), 0, "seed {seed}: {again:?}");
        assert!(again[0].verdict.reason.starts_with("already fired for "));
    }
}

/// A segment that is no final utterance never fires and is not even judged.
#[test]
fn property_partials_never_fire() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let text = utterance_with(&mut rng, rng_phrase(seed));
        let kind = *rng.pick(&["partial", "interim", "", "volatile"]);
        let hooks = [
            hook("a", json!({"phrases": PHRASES})),
            hook("b", json!({"phrases": PHRASES, "speaker": "any"})),
        ];
        let s = seg(kind, "remote_1", &text, rng.below(100_000), 200_000);
        let mut st = State::default();
        assert!(phrase::judge(&mut st, &hooks, &base(), &s, NO_COOLDOWN).is_empty());
        assert!(!phrase::any_hit(&hooks, &s));
        assert!(hooks::events_for("segment", &s).is_empty(), "{kind}");
        // The same text as a final utterance does fire: the kind alone kept it out.
        let mut fin = s.clone();
        fin["kind"] = json!("final");
        assert_eq!(
            fires(&phrase::judge(&mut st, &hooks, &base(), &fin, NO_COOLDOWN)),
            2
        );
    }
}

fn rng_phrase(seed: u64) -> &'static str {
    PHRASES[(seed % PHRASES.len() as u64) as usize]
}

/// Over a random stream of utterances that say a phrase: no two fires of a hook are within its
/// cooldown, and every utterance skipped for the cooldown is within it of a fire (it is no
/// stricter than it says). Another hook keeps its own cooldown.
#[test]
fn property_cooldown_holds() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let cd = 1 + rng.below(120_000);
        let hooks = [
            hook(
                "cool",
                json!({"phrases": PHRASES, "cooldown": format!("{cd}ms")}),
            ),
            hook("free", json!({"phrases": PHRASES, "cooldown": "0s"})),
        ];
        let mut st = State::default();
        let mut t = 0;
        let mut fired = Vec::new();
        let mut skipped = Vec::new();
        let n = 2 + rng.below(30);
        for _ in 0..n {
            // Apart from the last utterance (no overlap: each is its own speech).
            t += 600 + rng.below(cd * 2);
            let p = rng_phrase(rng.next());
            let text = utterance_with(&mut rng, p);
            let s = seg("live", "remote_1", &text, t, t + 500);
            let ds = phrase::judge(&mut st, &hooks, &base(), &s, Dur(30_000));
            let cool = ds.iter().find(|d| d.hook == "cool").unwrap();
            let free = ds.iter().find(|d| d.hook == "free").unwrap();
            assert!(free.verdict.fire, "seed {seed}: {free:?}");
            if cool.verdict.fire {
                fired.push(t);
            } else {
                assert!(
                    cool.verdict.reason.starts_with("match.cooldown: "),
                    "seed {seed}: {cool:?}"
                );
                skipped.push(t);
            }
        }
        assert!(!fired.is_empty());
        for w in fired.windows(2) {
            assert!(
                w[1] - w[0] >= cd,
                "seed {seed}: fires {fired:?} within {cd}ms"
            );
        }
        for s in skipped {
            assert!(
                fired.iter().any(|f| f.abs_diff(s) < cd),
                "seed {seed}: {s} skipped with no fire within {cd}ms of it"
            );
        }
    }
}

/// The live pass says some phrases; the final pass re-sees the same speech (its own boundaries,
/// maybe another remote speaker id) and finds one more the live pass missed; then the whole
/// session is replayed. Each spoken phrase fires once.
#[test]
fn property_dedup_holds_across_live_final_and_replay() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let hooks = [hook("h", json!({"phrases": PHRASES}))];
        let mut st = State::default();
        let mut live = Vec::new();
        let mut finals = Vec::new();
        let mut t = 0;
        let n = 1 + rng.below(12);
        for _ in 0..n {
            t += 3_000 + rng.below(20_000);
            let len = 1_000 + rng.below(6_000);
            let p = rng_phrase(rng.next());
            let speaker = *rng.pick(&["you", "remote_1", "remote_2"]);
            live.push(seg(
                "live",
                speaker,
                &utterance_with(&mut rng, p),
                t,
                t + len,
            ));
            // The final pass cuts the same speech a little differently.
            let (a, b) = (rng.below(400), rng.below(400));
            let fspeaker = if speaker == "you" {
                "you"
            } else {
                *rng.pick(&["remote_1", "remote_2"])
            };
            finals.push(seg(
                "final",
                fspeaker,
                &utterance_with(&mut rng, p),
                (t + a).saturating_sub(200),
                t + len + b - 200,
            ));
            t += len;
        }
        // Something only the final pass heard, apart from everything else.
        t += 10_000;
        finals.push(seg(
            "final",
            "remote_1",
            "and the action item is mine",
            t,
            t + 2_000,
        ));
        let mut total = 0;
        for s in &live {
            total += fires(&phrase::judge(&mut st, &hooks, &base(), s, NO_COOLDOWN));
        }
        assert_eq!(total, live.len(), "seed {seed}: every live utterance fires");
        let mut from_final = 0;
        for s in &finals {
            let ds = phrase::judge(&mut st, &hooks, &base(), s, NO_COOLDOWN);
            from_final += fires(&ds);
            for d in ds.iter().filter(|d| !d.verdict.fire) {
                assert!(
                    d.verdict.reason.starts_with("already fired for "),
                    "seed {seed}: {d:?}"
                );
                assert_eq!(d.data["live"], false);
            }
        }
        assert_eq!(
            from_final, 1,
            "seed {seed}: only the phrase the live pass missed fires"
        );
        // A replay of every segment fires nothing.
        for s in live.iter().chain(&finals) {
            assert_eq!(
                fires(&phrase::judge(&mut st, &hooks, &base(), s, NO_COOLDOWN)),
                0
            );
        }
    }
}

#[cfg(unix)]
mod engine {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use oats_core::action::Action;
    use oats_core::control::engine::{Engine, EngineOpts};
    use oats_core::hooks::events;
    use serde_json::{json, Value};

    struct Rig {
        _dir: tempfile::TempDir,
        out: PathBuf,
        engine: Arc<Engine>,
    }

    fn rig(toml: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cfg = root.join("config.toml");
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            &cfg,
            format!(
                "[sessions]\nroot = {:?}\n\n[stt.local]\nauto_download = false\n\n{toml}",
                root.join("rec").to_str().unwrap()
            ),
        )
        .unwrap();
        let engine = Engine::new(EngineOpts {
            user_config: cfg,
            pack: None,
            os: "linux".into(),
            arch: "x86_64".into(),
            machine: "default".into(),
            sessions_dir: root.join("sessions"),
            secrets: None,
        })
        .unwrap();
        Rig {
            _dir: dir,
            out,
            engine,
        }
    }

    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn add(r: &Rig, name: &str, m: Value) -> oats_core::config::Result<Value> {
        r.engine.dispatch(&Action::HookAdd {
            name: name.into(),
            event: "transcript.phrase".into(),
            command: json!([
                "sh",
                "-c",
                r#"cat > "$1/$OATS_HOOK.$OATS_RUN_ID.json""#,
                "hook",
                r.out.to_str().unwrap()
            ]),
            shell: None,
            timeout: None,
            enabled: None,
            cwd: None,
            r#match: Some(m),
        })
    }

    fn segment(kind: &str, speaker: &str, text: &str, start: f64) -> Value {
        json!({"session": "s1", "kind": kind, "speaker": speaker, "text": text,
               "start": start, "end": start + 2.0})
    }

    /// Segments on the bus: the live one fires with a valid envelope, the final pass re-seeing it
    /// does not, a partial is never asked, the other speaker is skipped; every decision is
    /// published as hook-decided.
    #[test]
    fn segments_on_the_bus_fire_phrase_hooks_once() {
        let r = rig("");
        add(
            &r,
            "flag",
            json!({"phrases": ["pricing"], "speaker": "remote", "cooldown": "0s"}),
        )
        .unwrap();
        let rx = r.engine.bus.subscribe(vec!["hook-decided".into()]);
        let bus = &r.engine.bus;
        bus.publish("segment", segment("partial", "remote_1", "pricing", 1.0));
        bus.publish("segment", segment("live", "remote_1", "So, pricing?", 10.0));
        bus.publish("segment", segment("live", "you", "pricing is fine", 20.0));
        bus.publish("segment", segment("live", "remote_1", "the weather", 30.0));
        bus.publish("segment", segment("final", "remote_2", "so pricing", 10.2));
        let runs = || {
            r.engine.dispatch(&Action::HookList {}).unwrap()["runs"]
                .as_array()
                .unwrap()
                .len()
        };
        wait_until("a run", || runs() >= 1);
        let mut decided = vec![];
        while let Ok(l) = rx.recv_timeout(Duration::from_millis(500)) {
            let v: Value = serde_json::from_str(&l).unwrap();
            decided.push(v["data"].clone());
        }
        // Judged in bus order; published from threads, so in any order.
        let mut got: Vec<(bool, String)> = decided
            .iter()
            .map(|d| (d["fire"] == true, d["reason"].as_str().unwrap().to_string()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                (false, r#"already fired for s1:transcript.phrase:remote_1@10000:pricing (match.speaker = remote: "remote_2" is remote; match.phrases: "pricing" said in "so pricing")"#.to_string()),
                (false, r#"match.speaker = remote: "you" is you"#.to_string()),
                (true, r#"match.speaker = remote: "remote_1" is remote; match.phrases: "pricing" said in "So, pricing?""#.to_string()),
            ],
            "{decided:?}"
        );
        assert!(decided
            .iter()
            .all(|d| d["event"] == "transcript.phrase" && d["session"] == "s1"));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(runs(), 1);
        let files: Vec<PathBuf> = std::fs::read_dir(&r.out)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        let env: Value = serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
        let spec = events::find("transcript.phrase").unwrap();
        events::validate(&events::envelope_schema(Some(spec)), &env).unwrap();
        assert_eq!(env["hook"], "flag");
        assert_eq!(env["dedup"], "s1:transcript.phrase:remote_1@10000:pricing");
        assert_eq!(env["data"]["live"], true);
        assert_eq!(env["data"]["phrase"], "pricing");
        assert_eq!(
            env["data"]["utterance"],
            json!({"id": "remote_1@10000", "text": "So, pricing?", "speaker": "remote_1",
                   "start_ms": 10_000, "end_ms": 12_000})
        );
    }

    /// hook-explain judges a given utterance and consumes nothing; with no utterance it says so.
    #[test]
    fn hook_explain_judges_an_utterance() {
        let r = rig("");
        add(
            &r,
            "flag",
            json!({"phrases": ["pricing"], "speaker": "you"}),
        )
        .unwrap();
        let explain = |u: Option<Value>| {
            r.engine
                .dispatch(&Action::HookExplain {
                    session: None,
                    event: "transcript.phrase".into(),
                    utterance: u,
                })
                .unwrap()
        };
        let v = explain(Some(json!({"text": "Pricing?", "speaker": "you"})));
        assert_eq!(v["hooks"][0]["fire"], true, "{v}");
        assert_eq!(
            v["hooks"][0]["reason"],
            r#"match.speaker = you: "you" is you; match.phrases: "pricing" said in "Pricing?""#
        );
        let v = explain(Some(json!({"text": "the weather"})));
        assert_eq!(
            v["hooks"][0]["reason"],
            r#"match.speaker = you: "remote_1" is remote; match.phrases: none of "pricing" said in "the weather""#
        );
        let v = explain(None);
        assert_eq!(v["hooks"][0]["fire"], false);
        assert!(v["hooks"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("the event has no utterance"));
        // Explaining the same utterance again still fires: nothing was consumed.
        let v = explain(Some(json!({"text": "Pricing?", "speaker": "you"})));
        assert_eq!(v["hooks"][0]["fire"], true);
        assert!(
            r.engine.dispatch(&Action::HookList {}).unwrap()["decisions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    /// Deferred transcription: config-check and hook-list warn that the phrase hook cannot fire
    /// mid-meeting; a disabled one and the default mode give no warning.
    #[test]
    fn config_check_warns_under_deferred_transcription() {
        let hooks = "[[hooks]]\nname = \"flag\"\nevent = \"transcript.phrase\"\ncommand = [\"true\"]\n[hooks.match]\nphrases = [\"pricing\"]\n\n[[hooks]]\nname = \"off\"\nevent = \"transcript.phrase\"\ncommand = [\"true\"]\nenabled = false\n[hooks.match]\nphrases = [\"pricing\"]\n";
        let r = rig(&format!("[audio]\ntranscribe = \"deferred\"\n\n{hooks}"));
        let v = r.engine.dispatch(&Action::ConfigCheck {}).unwrap();
        assert_eq!(v["valid"], true);
        let w = v["warnings"].as_array().unwrap();
        assert_eq!(w.len(), 1, "{v}");
        let w = w[0].as_str().unwrap();
        assert!(
            w.starts_with("hook flag: transcript.phrase: audio.transcribe = deferred")
                && w.contains("cannot fire mid-meeting"),
            "{w}"
        );
        let list = r.engine.dispatch(&Action::HookList {}).unwrap();
        assert_eq!(list["warnings"], v["warnings"]);
        let r = rig(hooks);
        let v = r.engine.dispatch(&Action::ConfigCheck {}).unwrap();
        assert_eq!(v["warnings"], json!([]));
    }
}
