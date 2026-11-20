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
    by_time, final_utts_with_progress, record_stages, utterance_json, wav, Asr, Live, MicStages,
    Settings, Utt,
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
    super::idle::touch(s.unload_after);
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

    /// Flush the live pass, stamp the session as ended and detach the saved-audio final pass.
    /// The worker remains in this process so recognisers use the process-wide model caches;
    /// Parakeet's large model and runtime arenas are not loaded once per session.
    /// Transcription problems are reported in the result, never lost audio: the WAV is already saved.
    /// `cut_secs`: the WAV was cut there (an auto-stop's trailing silence); the live utterances
    /// from there on go too.
    pub fn finish(
        mut self,
        wav_path: &Path,
        split: bool,
        ended: DateTime<Utc>,
        cut_secs: Option<f64>,
    ) -> Value {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Stop);
        }
        let mut live = self.worker.take().map(|w| {
            w.join().unwrap_or(LiveOutcome {
                utterances: 0,
                error: Some(ConfigError::new("internal", "live worker panicked")),
                stages: None,
            })
        });
        if let (Some(cut), Some(l)) = (cut_secs, live.as_mut()) {
            match self.store.trim_transcript(&self.id, "live", cut) {
                Ok(n) => l.utterances = l.utterances.saturating_sub(n),
                Err(e) => l.error = l.error.take().or(Some(e)),
            }
        }
        let live_stages = live.as_ref().and_then(|l| l.stages);
        let live_count = live.as_ref().map_or(0, |l| l.utterances);
        let live_state = match &live {
            None => json!("off"),
            Some(l) => match &l.error {
                None => json!("ok"),
                Some(e) => json!({"error": {"code": e.code, "message": e.message}}),
            },
        };
        let _ = self.store.finish(
            &self.id,
            &ended.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        if !self.plan.settings.final_enabled || !split {
            // A mixed recording cannot be attributed to speakers, so this is a cheap terminal state.
            let final_state = if self.plan.settings.final_enabled {
                json!("skipped: audio.layout is mix")
            } else {
                json!("off")
            };
            let stages =
                record_stages(&self.store, &self.id, live_stages, None).unwrap_or(Value::Null);
            return json!({"state": "done", "backend": self.plan.asr.name(),
                "live": live_state, "live_utterances": live_count,
                "final": final_state, "final_utterances": 0, "mic_stages": stages});
        }

        let id = self.id.clone();
        let job_path = wav_path.to_path_buf();
        let bus = Arc::clone(&self.plan.bus);
        let total_seconds = hound::WavReader::open(wav_path)
            .ok()
            .map(|reader| {
                let spec = reader.spec();
                f64::from(reader.duration())
                    / f64::from(spec.sample_rate)
                    / f64::from(spec.channels)
            })
            .unwrap_or(0.0);
        bus.publish(
            "transcribe-progress",
            json!({"session": id, "mode": "final",
            "state": "running", "processed_seconds": 0.0, "total_seconds": total_seconds}),
        );
        let event_cursor = bus
            .events_since(0, 0)
            .last()
            .and_then(|event| event["seq"].as_u64())
            .unwrap_or(0);
        let worker_id = id.clone();
        let backend = self.plan.asr.name();
        let fallback_store = self.store.sibling();
        let spawned = std::thread::Builder::new()
            .name(format!("oats-final-{id}"))
            .spawn(move || {
                // Wait until the recording's session-saved event, after record.stop hooks ran.
                let mut cursor = event_cursor;
                loop {
                    let events = self.plan.bus.events_since(cursor, 100);
                    if events.iter().any(|event| {
                        event["event"] == "session-saved"
                            && event["data"]["change"] == "record"
                            && event["data"]["id"] == worker_id
                    }) {
                        break;
                    }
                    cursor = events
                        .last()
                        .and_then(|event| event["seq"].as_u64())
                        .unwrap_or(cursor);
                }
                let result = self.final_pass(&job_path);
                match result {
                    Ok((count, stages)) => {
                        let stages = record_stages(&self.store, &worker_id, live_stages, Some(stages))
                            .unwrap_or(Value::Null);
                        if let Err(error) = self.store.transcribed(&worker_id, &self.plan.settings.engine_label()) {
                            let _ = self.store.write_transcript(&worker_id, "live", &[]);
                            let _ = self.store.write_transcript(&worker_id, "final", &[]);
                            let _ = self.store.set_pending(&worker_id, true);
                            self.plan.bus.publish("transcribe-progress", json!({"session": worker_id,
                                "mode": "final", "state": "failed", "total_seconds": total_seconds,
                                "error": {"code": error.code, "message": error.message}}));
                            return;
                        }
                        self.plan.bus.publish("session-saved", json!({"id": worker_id, "change": "transcribed"}));
                        self.plan.bus.publish("transcribe-progress", json!({"session": worker_id,
                            "mode": "final", "state": "done", "processed_seconds": total_seconds,
                            "total_seconds": total_seconds, "final_utterances": count, "mic_stages": stages}));
                    }
                    Err(error) => {
                        let _ = self.store.write_transcript(&worker_id, "live", &[]);
                        let _ = self.store.write_transcript(&worker_id, "final", &[]);
                        let _ = self.store.set_pending(&worker_id, true);
                        self.plan.bus.publish("transcribe-progress", json!({"session": worker_id,
                            "mode": "final", "state": "failed", "total_seconds": total_seconds,
                            "error": {"code": error.code, "message": error.message}}));
                    }
                }
            });
        if let Err(error) = spawned {
            let _ = fallback_store.write_transcript(&id, "live", &[]);
            let _ = fallback_store.write_transcript(&id, "final", &[]);
            let _ = fallback_store.set_pending(&id, true);
            bus.publish(
                "transcribe-progress",
                json!({"session": id, "mode": "final",
                "state": "failed", "total_seconds": total_seconds,
                "error": {"code": "worker_start", "message": error.to_string()}}),
            );
            return json!({"state": "done", "backend": backend, "live": live_state,
                "live_utterances": live_count, "final": {"error": {"code": "worker_start",
                "message": "could not start final transcription worker"}}, "final_utterances": 0});
        }
        json!({
            "state": "running",
            "backend": backend,
            "live": live_state,
            "live_utterances": live_count,
            "final": "running",
            "final_utterances": 0,
        })
    }

    fn final_pass(&self, wav_path: &Path) -> Result<(usize, MicStages)> {
        let s = &self.plan.settings;
        let chans = wav::read(wav_path)?;
        let total_seconds = chans.seconds();
        self.plan.bus.publish(
            "transcribe-progress",
            json!({"session": self.id,
            "mode": "final", "state": "running", "processed_seconds": 0.0,
            "total_seconds": total_seconds}),
        );
        let (all, stages) = final_utts_with_progress(
            &*self.plan.asr,
            s,
            &chans,
            &mut |processed| {
                self.plan.bus.publish("transcribe-progress", json!({"session": self.id,
                "mode": "final", "state": "running", "processed_seconds": processed.min(total_seconds),
                "total_seconds": total_seconds}));
            },
        )?;
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
