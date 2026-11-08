//! sherpa-onnx models in a process of their own (dot-4uzgol.108). On macOS the CoreML execution
//! provider keeps what it compiles and loads for a model (the compiled model, CoreML's and the
//! Neural Engine's caches) for the life of the process: sodium kept ~345 MB of phys_footprint
//! after Parakeet v2 unloaded, ~240 MB more per load and unload, even with the model confined to
//! a thread that exits with it (`confine`). Linux, on the CPU provider, returns to ~40 MB. So the
//! app runs each loaded sherpa-onnx model in a child process of itself (`<app> stt-worker`): the
//! model is made, run and dropped there, and unloading it ends the process, which gives back
//! everything it held whatever kept it. onnxruntime aborting on a file it cannot parse ends the
//! worker, not the app.
//!
//! The host binary says how to start a worker with `set_command` (the app: its own executable);
//! without one (a library user) models stay in this process, each on a thread of its own.
//!
//! Wire: frames of a little-endian u32 length and that many bytes, over the worker's stdin and
//! stdout. The worker writes `HELLO`, reads the model `Spec` (JSON), replies `Made` (JSON), then
//! answers each frame of little-endian f32 samples with a `Reply` (JSON) until its stdin closes.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::sherpa::{Model, Reply, Spec};

/// The argument that makes the app binary a model worker (`serve`).
pub const ARG: &str = "stt-worker";

/// What a worker writes first: anything its host printed before (a test harness) is skipped.
const HELLO: &[u8] = b"oats-stt-worker 1\n";

/// How to start a worker: an executable and the arguments before `ARG`.
static COMMAND: Mutex<Option<(PathBuf, Vec<String>)>> = Mutex::new(None);

/// The process ids of the workers alive now.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Run sherpa-onnx models in workers started as `exe args.. stt-worker`.
pub fn set_command(exe: PathBuf, args: Vec<String>) {
    *COMMAND.lock().expect("stt worker command") = Some((exe, args));
}

/// The process ids of the workers alive now: one per loaded sherpa-onnx model, none once they
/// unload (`stt-status` `models.workers` is how many).
pub fn live() -> Vec<u32> {
    LIVE.lock().expect("stt workers").clone()
}

#[derive(Serialize, Deserialize)]
enum Made {
    Ok,
    Failed(String),
}

fn write_frame(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let n = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too long"))?;
    w.write_all(&n.to_le_bytes())?;
    w.write_all(bytes)?;
    w.flush()
}

/// The next frame; `None` at the end of the stream.
fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut n = [0u8; 4];
    match r.read_exact(&mut n) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut buf = vec![0u8; u32::from_le_bytes(n) as usize];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn samples_to_bytes(pcm: &[f32]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn bytes_to_samples(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// The worker's end of the wire. Anything else in the process that writes to stdout (onnxruntime,
/// sherpa-onnx) goes to stderr instead, so it cannot corrupt a frame.
fn wire_out() -> io::Result<Box<dyn Write>> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        io::stdout().flush()?;
        // SAFETY: dup / dup2 on the process's own standard descriptors; the duplicate is owned
        // by the File from here on.
        unsafe {
            let fd = libc::dup(libc::STDOUT_FILENO);
            if fd < 0 || libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Box::new(std::fs::File::from_raw_fd(fd)))
        }
    }
    #[cfg(not(unix))]
    Ok(Box::new(io::stdout()))
}

/// The worker process: make the model the parent asks for, answer its calls, exit when its stdin
/// closes (the model unloads, or the app is gone). Returns the exit code.
pub fn serve() -> i32 {
    match serve_io() {
        Ok(()) => 0,
        Err(e) => {
            crate::elog!("oats-meetings-rs stt-worker: {e}");
            1
        }
    }
}

fn serve_io() -> io::Result<()> {
    let mut out = wire_out()?;
    let mut input = io::stdin().lock();
    out.write_all(HELLO)?;
    out.flush()?;
    let Some(spec) = read_frame(&mut input)? else {
        return Ok(());
    };
    let spec: Spec = serde_json::from_slice(&spec).map_err(invalid)?;
    let Some(model) = super::confine::pooled(|| Model::make(&spec)) else {
        let made = Made::Failed(format!("sherpa-onnx could not make {}", spec.what()));
        return write_frame(&mut out, &serde_json::to_vec(&made).map_err(invalid)?);
    };
    write_frame(&mut out, &serde_json::to_vec(&Made::Ok).map_err(invalid)?)?;
    while let Some(pcm) = read_frame(&mut input)? {
        let reply = super::confine::pooled(|| model.run(&bytes_to_samples(&pcm)));
        write_frame(&mut out, &serde_json::to_vec(&reply).map_err(invalid)?)?;
    }
    Ok(())
}

/// A model in a worker process. Dropping it ends the process, and waits for it.
pub struct Worker {
    child: Child,
    wire: Mutex<(ChildStdin, BufReader<ChildStdout>)>,
}

impl Worker {
    /// Start a worker making `spec`'s model. `None` when there is no worker to start (no
    /// command set, or it would not start: the caller keeps the model in this process);
    /// `Some(Err)` when the worker could not make the model (or died trying).
    pub fn spawn(spec: &Spec) -> Option<Result<Worker, String>> {
        let (exe, args) = COMMAND.lock().expect("stt worker command").clone()?;
        let child = Command::new(&exe)
            .args(&args)
            .arg(ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                crate::elog!(
                    "oats-meetings-rs: cannot start the stt worker {}: {e}; the model runs in-process",
                    exe.display()
                );
                return None;
            }
        };
        LIVE.lock().expect("stt workers").push(child.id());
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut w = Worker {
            child,
            wire: Mutex::new((stdin, stdout)),
        };
        Some(w.handshake(spec).map(|()| w))
    }

    fn handshake(&mut self, spec: &Spec) -> Result<(), String> {
        let wire = self.wire.get_mut().expect("stt worker wire");
        let made = (|| -> io::Result<Made> {
            skip_to_hello(&mut wire.1)?;
            write_frame(&mut wire.0, &serde_json::to_vec(spec).map_err(invalid)?)?;
            let f = read_frame(&mut wire.1)?
                .ok_or_else(|| invalid("exited before the model was made"))?;
            serde_json::from_slice(&f).map_err(invalid)
        })();
        match made {
            Ok(Made::Ok) => Ok(()),
            Ok(Made::Failed(e)) => Err(e),
            Err(e) => Err(format!("stt worker for {}: {e}", spec.what())),
        }
    }

    /// Run the model on `pcm` in the worker; `Err` when the worker is gone.
    pub fn run(&self, pcm: &[f32]) -> Result<Reply, String> {
        let mut wire = self.wire.lock().expect("stt worker wire");
        let (stdin, stdout) = &mut *wire;
        let reply = (|| -> io::Result<Reply> {
            write_frame(stdin, &samples_to_bytes(pcm))?;
            let f = read_frame(stdout)?.ok_or_else(|| invalid("the stt worker exited"))?;
            serde_json::from_slice(&f).map_err(invalid)
        })();
        reply.map_err(|e| format!("stt worker {}: {e}", self.child.id()))
    }
}

/// Read up to and including `HELLO`.
fn skip_to_hello(r: &mut impl BufRead) -> io::Result<()> {
    let mut seen = Vec::new();
    loop {
        let mut line = Vec::new();
        if r.read_until(b'\n', &mut line)? == 0 {
            return Err(invalid("exited before it started"));
        }
        seen.extend_from_slice(&line);
        if seen.ends_with(HELLO) {
            return Ok(());
        }
    }
}

impl Drop for Worker {
    /// The process has ended, and everything it held is back with the OS, before this returns.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let id = self.child.id();
        LIVE.lock().expect("stt workers").retain(|p| *p != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_end_cleanly() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"spec").unwrap();
        write_frame(&mut buf, &samples_to_bytes(&[0.5, -1.0])).unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"spec");
        let pcm = bytes_to_samples(&read_frame(&mut r).unwrap().unwrap());
        assert_eq!(pcm, vec![0.5, -1.0]);
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn what_a_host_printed_before_hello_is_skipped() {
        let mut out = b"\nrunning 1 test\ntest worker ... ".to_vec();
        out.extend_from_slice(HELLO);
        write_frame(&mut out, b"made").unwrap();
        let mut r = BufReader::new(&out[..]);
        skip_to_hello(&mut r).unwrap();
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), b"made");
        assert!(skip_to_hello(&mut BufReader::new(&b"no hello"[..])).is_err());
    }
}
