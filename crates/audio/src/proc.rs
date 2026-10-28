//! A capture that is a child process writing raw little-endian f32 mono PCM at `SAMPLE_RATE`
//! to stdout (`parec`, `pw-record`).

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;

use crate::backend::{Capture, OnSamples};
use crate::error::{io_err, AudioError, Result};

pub struct ProcCapture {
    child: Child,
    reader: Option<JoinHandle<()>>,
}

/// Decode f32le bytes, carrying a partial trailing sample between reads.
pub fn decode_f32le(carry: &mut Vec<u8>, bytes: &[u8], out: &mut Vec<f32>) {
    carry.extend_from_slice(bytes);
    let whole = carry.len() / 4 * 4;
    out.extend(
        carry[..whole]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
    );
    carry.drain(..whole);
}

pub fn spawn(mut cmd: Command, mut on_samples: OnSamples, timing: crate::settings::Timing) -> Result<Box<dyn Capture>> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AudioError::new("tool_missing", format!("{name} not found on PATH"))
        } else {
            io_err(&name, e)
        }
    })?;
    let mut stdout = child.stdout.take().expect("piped");
    let (first_tx, first_rx) = std::sync::mpsc::channel::<()>();
    let reader = std::thread::spawn(move || {
        let (mut buf, mut carry, mut out) = (vec![0u8; 16384], Vec::new(), Vec::new());
        let mut announced = false;
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.clear();
            decode_f32le(&mut carry, &buf[..n], &mut out);
            if !out.is_empty() {
                if !announced {
                    announced = true;
                    let _ = first_tx.send(());
                }
                on_samples(&out);
            }
        }
    });
    // Return only once audio flows: a cold `parec` takes ~2 s to connect on a loaded host, and a
    // `record start` that returned earlier would silently drop that long from the recording. A tool
    // that exits first (bad device name, no server) is an error, not a silent WAV.
    let deadline = std::time::Instant::now() + timing.open_timeout;
    loop {
        if first_rx.recv_timeout(timing.poll).is_ok() {
            break;
        }
        let exited = child.try_wait().ok().flatten();
        let timed_out = std::time::Instant::now() >= deadline;
        if exited.is_some() || timed_out {
            let _ = child.kill();
            let status = child.wait().ok();
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            let _ = reader.join();
            let why = match status {
                Some(st) if !timed_out || exited.is_some() => format!("exited at once ({st})"),
                _ => format!("delivered no audio within {:?}", timing.open_timeout),
            };
            return Err(AudioError::new("capture_failed", format!("{name} {why}: {}", err.trim())));
        }
    }
    Ok(Box::new(ProcCapture { child, reader: Some(reader) }))
}

impl Capture for ProcCapture {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

impl Drop for ProcCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_samples_carry_over() {
        let a = 0.25f32.to_le_bytes();
        let b = (-0.5f32).to_le_bytes();
        let all: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
        let (mut carry, mut out) = (vec![], vec![]);
        decode_f32le(&mut carry, &all[..3], &mut out);
        assert!(out.is_empty());
        decode_f32le(&mut carry, &all[3..6], &mut out);
        assert_eq!(out, vec![0.25]);
        decode_f32le(&mut carry, &all[6..], &mut out);
        assert_eq!(out, vec![0.25, -0.5]);
        assert!(carry.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn missing_tool_and_instant_exit_are_errors() {
        let t = timing(15_000, 25);
        let e = spawn(Command::new("definitely-not-a-real-tool-oats"), Box::new(|_| {}), t).err().unwrap();
        assert_eq!(e.code, "tool_missing");
        let e = spawn(Command::new("false"), Box::new(|_| {}), t).err().unwrap();
        assert_eq!(e.code, "capture_failed");
    }

    fn timing(open_ms: u64, poll_ms: u64) -> crate::settings::Timing {
        crate::settings::Timing {
            open_timeout: std::time::Duration::from_millis(open_ms),
            poll: std::time::Duration::from_millis(poll_ms),
        }
    }

    /// audio.open_timeout is the wait for a silent tool: it gives up at about that long, at both
    /// a short and a longer setting, and says which limit it hit.
    #[cfg(unix)]
    #[test]
    fn open_timeout_is_honoured() {
        for open_ms in [200u64, 700] {
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            let t0 = std::time::Instant::now();
            let e = spawn(cmd, Box::new(|_| {}), timing(open_ms, 10)).err().unwrap();
            let took = t0.elapsed().as_millis() as u64;
            assert_eq!(e.code, "capture_failed");
            assert!(e.message.contains("delivered no audio within"), "{}", e.message);
            assert!(took >= open_ms && took < open_ms + 1500, "open {open_ms}ms took {took}ms");
        }
    }
}
