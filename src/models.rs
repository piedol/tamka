//! Speech models: the catalog with plain-language grades, what's downloaded, and
//! downloading or removing them through the engine helper. Nothing is bundled with
//! the app; models arrive only when the user asks for them.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::{engine, util};

/// A model the user can download, with grades a non-expert can read at a glance.
/// Grades come from the benchmark in docs/benchmark-public.md (117 public clips,
/// M5 Max): Size = download and memory, Speed = time from the end of speech to
/// text, Accuracy = word errors on everyday speech.
pub struct Model {
    /// Engine arguments, also the settings value ("parakeet ultra").
    pub id: &'static str,
    pub name: &'static str,
    /// One line on what it's best at.
    pub tagline: &'static str,
    pub size: &'static str,
    pub speed: &'static str,
    pub accuracy: &'static str,
    /// Approximate download, for the button and the tagline.
    pub download_mb: u32,
    pub recommended: bool,
    /// Installed and removed by macOS itself (Apple's speech model).
    pub system: bool,
    /// Tooltips explaining each grade in numbers.
    pub size_detail: &'static str,
    pub speed_detail: &'static str,
    pub accuracy_detail: &'static str,
}

pub const CATALOG: &[Model] = &[
    Model {
        id: "parakeet ultra",
        name: "Parakeet Ultra",
        tagline: "Most accurate, best punctuation",
        size: "B",
        speed: "A",
        accuracy: "A",
        download_mb: 630,
        recommended: true,
        system: false,
        size_detail: "630 MB download; about 0.5 GB of memory while ready",
        speed_detail: "About 0.17 s from the end of a 50-second dictation to text",
        accuracy_detail: "6.2% of words wrong on everyday speech (the best tested)",
    },
    Model {
        id: "parakeet phonon2",
        name: "Parakeet Phonon-2",
        tagline: "Best with casual speech",
        size: "A",
        speed: "A",
        accuracy: "A−",
        download_mb: 360,
        recommended: false,
        system: false,
        size_detail: "360 MB download; about 0.5 GB of memory while ready",
        speed_detail: "About 0.17 s from the end of a 50-second dictation to text",
        accuracy_detail: "6.6% of words wrong on everyday speech; best on spontaneous talk, weaker punctuation",
    },
    Model {
        id: "parakeet v3",
        name: "Parakeet v3",
        tagline: "Speaks 25 European languages",
        size: "A",
        speed: "A",
        accuracy: "A−",
        download_mb: 470,
        recommended: false,
        system: false,
        size_detail: "470 MB download; about 0.5 GB of memory while ready",
        speed_detail: "About 0.17 s from the end of a 50-second dictation to text",
        accuracy_detail: "6.4% of words wrong on everyday English speech",
    },
    Model {
        id: "parakeet v2",
        name: "Parakeet v2",
        tagline: "The fastest",
        size: "A",
        speed: "A+",
        accuracy: "B+",
        download_mb: 445,
        recommended: false,
        system: false,
        size_detail: "445 MB download; about 0.35 GB of memory while ready",
        speed_detail: "About 0.13 s from the end of a 50-second dictation to text (the fastest)",
        accuracy_detail: "7.1% of words wrong on everyday speech",
    },
    Model {
        id: "apple",
        name: "Apple Speech",
        tagline: "Built into macOS",
        size: "A+",
        speed: "B",
        accuracy: "B",
        download_mb: 0,
        recommended: false,
        system: true,
        size_detail: "Managed by macOS; almost no memory in this app",
        speed_detail: "About 0.7 s from the end of a 50-second dictation to text",
        accuracy_detail: "7.3% of words wrong on everyday speech",
    },
];

pub fn find(id: &str) -> Option<&'static Model> {
    CATALOG.iter().find(|m| m.id == id)
}

pub fn recommended() -> &'static Model {
    CATALOG.iter().find(|m| m.recommended).unwrap_or(&CATALOG[0])
}

/// Display name without any parenthetical.
pub fn name(id: &str) -> &str {
    find(id).map_or(id, |m| m.name)
}

#[derive(Clone, Debug, Default)]
pub struct Installed {
    pub downloaded: bool,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub id: String,
    pub progress: f64,
    /// "preparing", "downloading" or "optimizing".
    pub phase: String,
}

static INSTALLED: Mutex<Option<HashMap<String, Installed>>> = Mutex::new(None);
static ACTIVE: Mutex<Option<(Download, Child)>> = Mutex::new(None);

/// What's downloaded, as last checked (None until the first check finishes).
pub fn installed() -> Option<HashMap<String, Installed>> {
    INSTALLED.lock().unwrap().clone()
}

pub fn is_downloaded(id: &str) -> Option<bool> {
    INSTALLED.lock().unwrap().as_ref().map(|m| m.get(id).is_some_and(|i| i.downloaded))
}

pub fn downloading() -> Option<Download> {
    ACTIVE.lock().unwrap().as_ref().map(|(d, _)| d.clone())
}

/// Ask the engine helper what's on disk. Blocks briefly; call off the main thread.
pub fn refresh() -> HashMap<String, Installed> {
    let mut found = HashMap::new();
    if let Some(path) = engine::helper_path()
        && let Ok(out) = Command::new(path).arg("status").stderr(Stdio::null()).output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Some(start) = line.find("{\"") else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line[start..]) else { continue };
            let Some(id) = v.get("id").and_then(|i| i.as_str()) else { continue };
            found.insert(
                id.to_string(),
                Installed {
                    downloaded: v.get("downloaded").and_then(|d| d.as_bool()).unwrap_or(false),
                    bytes: v.get("bytes").and_then(|b| b.as_u64()).unwrap_or(0),
                },
            );
        }
    }
    *INSTALLED.lock().unwrap() = Some(found.clone());
    found
}

/// Refresh in the background, then run `then` on the main thread.
pub fn refresh_async(then: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        refresh();
        util::main_async(then);
    });
}

/// Download a model. `progress` runs on the main thread as it goes; `done` runs once
/// at the end with the error, if any (None also when cancelled).
pub fn download(
    id: &str,
    progress: impl Fn(Download) + Send + Sync + 'static,
    done: impl FnOnce(Result<(), String>) + Send + 'static,
) -> Result<(), String> {
    if ACTIVE.lock().unwrap().is_some() {
        return Err("Another model is downloading. Wait for it to finish first.".into());
    }
    let path = engine::helper_path().ok_or("The speech engine is missing from the app.")?;
    let mut child = Command::new(path)
        .arg("download")
        .args(id.split_whitespace())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start the download: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let start = Download { id: id.to_string(), progress: 0.0, phase: "preparing".into() };
    *ACTIVE.lock().unwrap() = Some((start, child));
    util::log(format!("download started: {id}"));
    let id = id.to_string();
    let progress = std::sync::Arc::new(progress);
    std::thread::spawn(move || {
        let mut result: Result<(), String> = Err("The download stopped unexpectedly.".into());
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Some(start) = line.find("{\"") else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line[start..]) else { continue };
            if v.get("done").is_some() {
                result = Ok(());
            } else if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                result = Err(e.to_string());
            } else if let Some(p) = v.get("progress").and_then(|p| p.as_f64()) {
                let d = Download {
                    id: id.clone(),
                    progress: p,
                    phase: v.get("phase").and_then(|p| p.as_str()).unwrap_or("downloading").to_string(),
                };
                if let Some((current, _)) = ACTIVE.lock().unwrap().as_mut() {
                    *current = d.clone();
                }
                let progress = progress.clone();
                util::main_async(move || progress(d));
            }
        }
        let cancelled = ACTIVE.lock().unwrap().take().map(|(_, mut child)| {
            let _ = child.wait();
        });
        if cancelled.is_none() {
            // `cancel` already took it.
            result = Err(String::new());
        }
        util::log(format!("download {id}: {}", if result.is_ok() { "done" } else { "stopped" }));
        refresh();
        util::main_async(move || done(result));
    });
    Ok(())
}

/// Stop the download in progress, if any, and clear away its partial files so a
/// half-written model is never mistaken for a complete one.
pub fn cancel() {
    let stopped = ACTIVE.lock().unwrap().take();
    if let Some((download, mut child)) = stopped {
        let _ = child.kill();
        let _ = child.wait();
        std::thread::spawn(move || {
            let _ = remove(&download.id);
        });
    }
}

/// Remove a downloaded model (its files go to the Trash). Blocks; call off the main thread.
pub fn remove(id: &str) -> Result<(), String> {
    let path = engine::helper_path().ok_or("The speech engine is missing from the app.")?;
    let out = Command::new(path)
        .arg("remove")
        .args(id.split_whitespace())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let ok = text.contains("\"done\"");
    refresh();
    if ok {
        util::log(format!("removed model {id}"));
        Ok(())
    } else {
        Err("The model couldn't be removed.".into())
    }
}

/// "632 MB" / "1.2 GB".
pub fn format_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 { format!("{:.1} GB", mb / 1000.0) } else { format!("{mb:.0} MB") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_one_recommendation_and_valid_grades() {
        assert_eq!(CATALOG.iter().filter(|m| m.recommended).count(), 1);
        for m in CATALOG {
            for g in [m.size, m.speed, m.accuracy] {
                assert!(["A+", "A", "A−", "B+", "B", "B−", "C"].contains(&g), "{} has grade {g}", m.id);
            }
        }
        assert_eq!(recommended().id, "parakeet ultra");
    }

    #[test]
    fn bytes_read_naturally() {
        assert_eq!(format_bytes(632_360_960), "632 MB");
        assert_eq!(format_bytes(1_234_000_000), "1.2 GB");
    }
}
