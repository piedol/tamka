//! Small helpers shared across modules.

use std::io::Write;
use std::path::PathBuf;

use dispatch2::{DispatchQueue, DispatchTime};
use objc2::rc::Retained;
use objc2_foundation::NSString;

/// Run `f` on the main queue soon.
pub fn main_async(f: impl FnOnce() + Send + 'static) {
    DispatchQueue::main().exec_async(f);
}

/// Run `f` on the main queue after `secs` seconds.
pub fn main_after(secs: f64, f: impl FnOnce() + Send + 'static) {
    let when = DispatchTime::NOW.time((secs * 1_000_000_000.0) as i64);
    let _ = DispatchQueue::main().after(when, f);
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

/// Where the last recording and other scratch files go.
pub fn cache_dir() -> PathBuf {
    let dir = home().join("Library/Caches/Tamka");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn log_path() -> PathBuf {
    home().join("Library/Logs/Tamka.log")
}

/// Append a line to ~/Library/Logs/Tamka.log (and stderr in debug builds).
/// Transcripts are never logged, only events and timings.
pub fn log(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    if cfg!(debug_assertions) || std::env::var_os("TAMKA_LOG").is_some() {
        eprintln!("[tamka] {msg}");
    }
    let path = log_path();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{secs} {msg}");
    }
}

unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// Hand freed audio buffers back to the system shortly after a dictation, so the
/// idle footprint returns to baseline.
pub fn release_memory_soon() {
    main_after(1.0, || {
        // SAFETY: a null zone means "all zones"; goal 0 means "as much as possible".
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    });
}
