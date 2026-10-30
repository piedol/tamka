//! The speech engine runs as a helper process (Contents/Helpers/tamka-engine) that
//! keeps the model loaded on the Neural Engine. Tamka sends it a WAV path and gets
//! text back over stdin/stdout (protocol in engine/Sources/tamka-engine/main.swift).
//!
//! Keeping the model in its own process means a crash there never takes Tamka
//! down (it is restarted on the next dictation), and the engine can be swapped
//! without touching the app.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::{settings, util};

struct Helper {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    args: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Stopped,
    Loading,
    Ready,
    /// The selected model hasn't been downloaded yet.
    NotDownloaded,
    Failed(String),
}

pub const NOT_DOWNLOADED: &str = "Download a speech model in Settings first.";

static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
static STATUS: Mutex<Status> = Mutex::new(Status::Stopped);
/// Dictations waiting for the engine. Files wait while this is above zero, so a
/// dictation goes next instead of queueing behind the rest of a batch.
static DICTATIONS_WAITING: AtomicUsize = AtomicUsize::new(0);
/// The helper's pid while it transcribes a file, so Stop can end it.
static FILE_PID: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// First launch can download the model (~600 MB), so allow plenty of time.
const READY_TIMEOUT: Duration = Duration::from_secs(600);

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

fn set_status(s: Status) {
    *STATUS.lock().unwrap() = s;
}

/// The helper binary: inside the app bundle, or the dev build next to the sources.
pub fn helper_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TAMKA_ENGINE") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let bundled = exe.parent()?.parent()?.join("Helpers/tamka-engine");
    if bundled.exists() {
        return Some(bundled);
    }
    // A dev build runs from target/…/release/: look for the engine next to the sources,
    // found from the executable rather than a path compiled into the binary.
    exe.ancestors()
        .skip(1)
        .take(5)
        .map(|dir| dir.join("engine/.build/release/tamka-engine"))
        .find(|p| p.exists())
}

/// Load the model in the background so the first dictation is instant.
pub fn start_async() {
    std::thread::spawn(|| {
        if let Err(e) = ensure_running() {
            util::log(format!("engine failed to start: {e}"));
        }
    });
}

/// Restart with the current settings (after the engine setting changed).
pub fn restart() {
    shutdown();
    start_async();
}

fn spawn(args: &str) -> Result<Helper, String> {
    let path = helper_path().ok_or("The speech engine (tamka-engine) is missing from the app.")?;
    let mut child = Command::new(&path)
        .args(args.split_whitespace())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start the speech engine: {e}"))?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Ok(Helper { child, stdin, lines, args: args.to_string() })
}

/// Make sure a helper with the current engine setting is running and ready.
fn ensure_running() -> Result<(), String> {
    let s = settings::get();
    if crate::models::is_downloaded(&s.engine) == Some(false) {
        set_status(Status::NotDownloaded);
        return Err(NOT_DOWNLOADED.into());
    }
    let args = settings::engine_args(&s);
    let mut guard = HELPER.lock().unwrap();
    if let Some(h) = guard.as_mut() {
        let alive = matches!(h.child.try_wait(), Ok(None));
        if alive && h.args == args {
            return Ok(());
        }
        let _ = h.child.kill();
        let _ = h.child.wait();
        *guard = None;
    }
    set_status(Status::Loading);
    let t0 = Instant::now();
    let helper = spawn(&args).inspect_err(|e| set_status(Status::Failed(e.clone())))?;
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match helper.lines.recv_timeout(left) {
            Ok(line) if line.contains("READY") => break,
            Ok(_) => continue,
            Err(_) => {
                // The helper exits early when the model's files are missing.
                let installed = crate::models::refresh();
                if !installed.get(&s.engine).is_some_and(|i| i.downloaded) {
                    set_status(Status::NotDownloaded);
                    return Err(NOT_DOWNLOADED.into());
                }
                let msg = "The speech engine didn't finish loading.".to_string();
                set_status(Status::Failed(msg.clone()));
                return Err(msg);
            }
        }
    }
    util::log(format!("engine '{args}' ready in {} ms", t0.elapsed().as_millis()));
    set_status(Status::Ready);
    *guard = Some(helper);
    Ok(())
}

/// Transcribe a dictation's WAV. Blocks; call from a background thread. It goes ahead
/// of files waiting in Transcribe Files.
pub fn transcribe(wav: &Path) -> Result<String, String> {
    DICTATIONS_WAITING.fetch_add(1, Ordering::SeqCst);
    let result = ensure_running().map(|()| HELPER.lock().unwrap());
    DICTATIONS_WAITING.fetch_sub(1, Ordering::SeqCst);
    run(result?, wav)
}

/// Transcribe a file for Transcribe Files: waits while a dictation is waiting, and
/// `interrupt_file` can end it.
pub fn transcribe_file(wav: &Path) -> Result<String, String> {
    loop {
        while DICTATIONS_WAITING.load(Ordering::SeqCst) > 0 {
            std::thread::sleep(Duration::from_millis(20));
        }
        ensure_running()?;
        let guard = HELPER.lock().unwrap();
        if DICTATIONS_WAITING.load(Ordering::SeqCst) > 0 {
            continue;
        }
        let pid = guard.as_ref().map_or(0, |h| h.child.id());
        FILE_PID.store(pid, Ordering::SeqCst);
        let result = run(guard, wav);
        FILE_PID.store(0, Ordering::SeqCst);
        return result;
    }
}

/// Stop the file being transcribed by ending the helper; it's started again for the
/// next dictation. Does nothing while no file is being transcribed.
pub fn interrupt_file() {
    let pid = FILE_PID.swap(0, Ordering::SeqCst);
    if pid != 0 {
        // SAFETY: plain kill(2) on our own helper's pid.
        unsafe { kill(pid as i32, 9) };
    }
}

/// Read the sound from an audio or video file into a 16 kHz mono WAV at `out`
/// (`tamka-engine decode`, a separate short-lived process, so it never waits for the
/// model). Returns the length in seconds. `started` gets the process id, so Stop can end it.
pub fn decode(input: &Path, out: &Path, started: impl FnOnce(u32)) -> Result<f64, String> {
    let path = helper_path().ok_or("The speech engine (tamka-engine) is missing from the app.")?;
    let child = Command::new(&path)
        .arg("decode")
        .arg(input)
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start the speech engine: {e}"))?;
    started(child.id());
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if msg.is_empty() { "Tamka can't read this file.".into() } else { msg });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let seconds = stdout
        .lines()
        .filter_map(|l| l.find("{\"").map(|i| &l[i..]))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v.get("seconds").and_then(|s| s.as_f64()))
        .unwrap_or(0.0);
    Ok(seconds)
}

/// Send one request to the running helper and wait for its answer.
fn run(mut guard: std::sync::MutexGuard<'_, Option<Helper>>, wav: &Path) -> Result<String, String> {
    let helper = guard.as_mut().ok_or("The speech engine isn't running.")?;
    let request = serde_json::json!({ "wav": wav.to_string_lossy() }).to_string();
    let sent = writeln!(helper.stdin, "{request}").and_then(|_| helper.stdin.flush());
    if sent.is_err() {
        *guard = None;
        set_status(Status::Stopped);
        return Err("The speech engine stopped. Try again.".into());
    }
    // Generous: about 1 s of headroom per 10 s of audio on top of a fixed minute.
    let deadline = Instant::now() + Duration::from_secs(60 + audio_seconds(wav) / 10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match helper.lines.recv_timeout(left) {
            Ok(line) => {
                let Some(start) = line.find("{\"") else { continue };
                let Ok(reply) = serde_json::from_str::<serde_json::Value>(&line[start..]) else { continue };
                let Some(text) = reply.get("text").and_then(|t| t.as_str()) else { continue };
                if let Some(err) = reply.get("error").and_then(|e| e.as_str()) {
                    return Err(err.to_string());
                }
                let ms = reply.get("ms").and_then(|m| m.as_f64()).unwrap_or(0.0);
                util::log(format!("transcribed {} s of audio in {ms:.0} ms", audio_seconds(wav)));
                return Ok(text.to_string());
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = helper.child.kill();
                *guard = None;
                set_status(Status::Stopped);
                return Err("The speech engine took too long and was restarted.".into());
            }
            Err(RecvTimeoutError::Disconnected) => {
                *guard = None;
                set_status(Status::Stopped);
                return Err("The speech engine stopped unexpectedly. Try again.".into());
            }
        }
    }
}

fn audio_seconds(wav: &Path) -> u64 {
    // 16-bit mono: bytes / (2 × rate). The header's byte rate is at offset 28.
    let Ok(bytes) = std::fs::read(wav) else { return 0 };
    if bytes.len() < 44 {
        return 0;
    }
    let byte_rate = u32::from_le_bytes(bytes[28..32].try_into().unwrap()).max(1) as u64;
    (bytes.len() as u64 - 44) / byte_rate
}

pub fn shutdown() {
    if let Some(mut h) = HELPER.lock().unwrap().take() {
        let _ = writeln!(h.stdin, "QUIT");
        let _ = h.stdin.flush();
        std::thread::sleep(Duration::from_millis(50));
        let _ = h.child.kill();
        let _ = h.child.wait();
    }
    set_status(Status::Stopped);
}
