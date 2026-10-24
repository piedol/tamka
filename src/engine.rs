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
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("engine/.build/release/tamka-engine");
    dev.exists().then_some(dev)
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

/// Transcribe a WAV file. Blocks; call from a background thread.
pub fn transcribe(wav: &Path) -> Result<String, String> {
    ensure_running()?;
    let mut guard = HELPER.lock().unwrap();
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
