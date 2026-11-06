//! Transcribing a recording as it happens (dot-4uzgol.7). `record start` taps the captured audio:
//! each source is decimated 48 -> 16 kHz and queued to a worker thread that owns a `Live`
//! (mic = channel 0 = `you`, system = channel 1 = `remote_1`; see `crate::speakers`) and appends each
//! utterance to `transcript.live.jsonl`. `record stop` flushes the worker, then runs the final pass
//! over the saved WAV into `transcript.final.jsonl`. The tap never blocks the capture thread.

use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use chrono::{DateTime, Utc};
use oats_audio::resample::Decimate3;
use oats_audio::Source;
use serde_json::{json, Value};

use super::{
    by_time, final_utts, record_stages, utterance_json, wav, Asr, Live, MicStages, Settings, Utt,
};
use crate::config::{ConfigError, Result};
use crate::control::bus::Bus;
use crate::sessions::Store;

/// Everything a recording needs to transcribe itself.
#[derive(Clone)]
pub struct Plan {
    pub asr: Arc<dyn Asr>,
    pub settings: Settings,
    pub bus: Arc<Bus>,
}

enum Msg {
    Audio(usize, Vec<f32>),
    Stop,
}

struct LiveOutcome {
    utterances: usize,
    error: Option<ConfigError>,
    stages: Option<MicStages>,
}

pub struct Transcriber {
    plan: Plan,
    store: Store,
    id: String,
    started: DateTime<Utc>,
    tx: Option<Sender<Msg>>,
    worker: Option<JoinHandle<LiveOutcome>>,
}

fn emit(bus: &Bus, id: &str, kind: &str, u: &Utt) {
    bus.publish(
        "segment",
        json!({"session": id, "kind": kind, "speaker": u.speaker, "text": u.text, "start": u.start, "end": u.end}),
    );
}

fn live_worker(
    rx: Receiver<Msg>,
    plan: Plan,
    store: Store,
    id: String,
    started: DateTime<Utc>,
) -> LiveOutcome {
    let s = &plan.settings;
    let mut live = Live::for_settings(Arc::clone(&plan.asr), s);
    let mut out = LiveOutcome {
        utterances: 0,
        error: None,
        stages: None,
    };
    let write = |out: &mut LiveOutcome, us: Vec<Utt>| {
        for u in by_time(us) {
            match store.append_utterance(&id, "live", &utterance_json(&u, started)) {
                Ok(()) => {
                    emit(&plan.bus, &id, "live", &u);
                    out.utterances += 1;
                }
                Err(e) => out.error = Some(e),
            }
        }
    };
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Audio(ch, pcm) if out.error.is_none() => match live.push(ch, &pcm) {
                Ok(us) => write(&mut out, us),
                // Keep recording; the final pass may still succeed.
                Err(e) => out.error = Some(e),
            },
            Msg::Audio(..) => {}
            Msg::Stop => break,
        }
    }
    if out.error.is_none() {
        match live.flush() {
            Ok(us) => write(&mut out, us),
            Err(e) => out.error = Some(e),
        }
    }
    out.stages = Some(live.mic_stages());
    out
}

impl Transcriber {
    pub fn start(plan: Plan, store: Store, id: &str, started: DateTime<Utc>) -> Transcriber {
        let (tx, worker) = if plan.settings.live_enabled {
            let (tx, rx) = channel();
            let (p, st, i) = (plan.clone(), store.sibling(), id.to_string());
            (
                Some(tx),
                Some(std::thread::spawn(move || {
                    live_worker(rx, p, st, i, started)
                })),
            )
        } else {
            (None, None)
        };
        Transcriber {
            plan,
            store,
            id: id.to_string(),
            started,
            tx,
            worker,
        }
    }

    /// The per-source tap handed to the recorder.
    pub fn tap(&self, source: Source) -> Box<dyn FnMut(&[f32]) + Send> {
        let Some(tx) = self.tx.clone() else {
            return Box::new(|_| {});
        };
        let ch = usize::from(source == Source::System);
        let mut dec = Decimate3::default();
        let mut buf = Vec::new();
        Box::new(move |samples| {
            buf.clear();
            dec.push(samples, &mut buf);
            if !buf.is_empty() {
                let _ = tx.send(Msg::Audio(ch, buf.clone()));
            }
        })
    }

    /// Flush the live pass, run the final pass over `wav_path`, stamp the session as ended.
    /// Transcription problems are reported in the result, never lost audio: the WAV is already saved.
    pub fn finish(mut self, wav_path: &Path, split: bool, ended: DateTime<Utc>) -> Value {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Stop);
        }
        let live = self.worker.take().map(|w| {
            w.join().unwrap_or(LiveOutcome {
                utterances: 0,
                error: Some(ConfigError::new("internal", "live worker panicked")),
                stages: None,
            })
        });
        let s = &self.plan.settings;
        let (final_state, final_n, final_stages) = if !s.final_enabled {
            (json!("off"), 0, None)
        } else if !split {
            // One mixed channel cannot be attributed to a speaker.
            (json!("skipped: audio.layout is mix"), 0, None)
        } else {
            match self.final_pass(wav_path) {
                Ok((n, st)) => (json!("ok"), n, Some(st)),
                Err(e) => (
                    json!({"error": {"code": e.code, "message": e.message}}),
                    0,
                    None,
                ),
            }
        };
        let stages = record_stages(
            &self.store,
            &self.id,
            live.as_ref().and_then(|l| l.stages),
            final_stages,
        )
        .unwrap_or(Value::Null);
        let _ = self.store.finish(
            &self.id,
            &ended.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        json!({
            "state": "done",
            "backend": self.plan.asr.name(),
            "live": match &live {
                None => json!("off"),
                Some(l) => match &l.error {
                    None => json!("ok"),
                    Some(e) => json!({"error": {"code": e.code, "message": e.message}}),
                },
            },
            "live_utterances": live.map_or(0, |l| l.utterances),
            "final": final_state,
            "final_utterances": final_n,
            "mic_stages": stages,
        })
    }

    fn final_pass(&self, wav_path: &Path) -> Result<(usize, MicStages)> {
        let s = &self.plan.settings;
        let chans = wav::read(wav_path)?;
        let (all, stages) = final_utts(&*self.plan.asr, s, &chans)?;
        let lines: Vec<Value> = all
            .iter()
            .map(|u| utterance_json(u, self.started))
            .collect();
        self.store.write_transcript(&self.id, "final", &lines)?;
        all.iter()
            .for_each(|u| emit(&self.plan.bus, &self.id, "final", u));
        Ok((lines.len(), stages))
    }
}
