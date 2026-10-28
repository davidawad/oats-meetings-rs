//! Event bus: each subscriber has a bounded queue and its own writer thread; a slow or dead
//! subscriber is dropped, never allowed to stall the engine.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;

use serde_json::{json, Value};

struct Sub {
    topics: Vec<String>,
    tx: SyncSender<String>,
}

#[derive(Default)]
pub struct Bus {
    subs: Mutex<Vec<Sub>>,
    seq: AtomicU64,
}

impl Bus {
    /// Empty `topics` means every topic.
    pub fn subscribe(&self, topics: Vec<String>) -> Receiver<String> {
        let cap = crate::config::keys::default_int("control.event_queue") as usize;
        self.subscribe_with(topics, cap)
    }

    /// Like `subscribe`, with the queue length (`control.event_queue`) given.
    pub fn subscribe_with(&self, topics: Vec<String>, cap: usize) -> Receiver<String> {
        let (tx, rx) = sync_channel(cap);
        self.subs.lock().expect("bus lock").push(Sub { topics, tx });
        rx
    }

    pub fn publish(&self, topic: &str, data: Value) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let line = json!({
            "event": topic, "seq": seq, "data": data,
            "schema_version": crate::config::SCHEMA_VERSION,
        })
        .to_string();
        self.subs.lock().expect("bus lock").retain(|s| {
            if !s.topics.is_empty() && !s.topics.iter().any(|t| t == topic) {
                return true;
            }
            !matches!(
                s.tx.try_send(line.clone()),
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_))
            )
        });
    }

    pub fn subscriber_count(&self) -> usize {
        self.subs.lock().expect("bus lock").len()
    }
}
