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

pub fn spawn(mut cmd: Command, mut on_samples: OnSamples) -> Result<Box<dyn Capture>> {
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
    let reader = std::thread::spawn(move || {
        let (mut buf, mut carry, mut out) = (vec![0u8; 16384], Vec::new(), Vec::new());
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.clear();
            decode_f32le(&mut carry, &buf[..n], &mut out);
            if !out.is_empty() {
                on_samples(&out);
            }
        }
    });
    // A tool that exits at once (bad device name, no server) is an error now, not a silent WAV.
    std::thread::sleep(std::time::Duration::from_millis(250));
    if let Ok(Some(status)) = child.try_wait() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = reader.join();
        return Err(AudioError::new(
            "capture_failed",
            format!("{name} exited at once ({status}): {}", err.trim()),
        ));
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
        let e = spawn(Command::new("definitely-not-a-real-tool-oats"), Box::new(|_| {})).err().unwrap();
        assert_eq!(e.code, "tool_missing");
        let e = spawn(Command::new("false"), Box::new(|_| {})).err().unwrap();
        assert_eq!(e.code, "capture_failed");
    }
}
