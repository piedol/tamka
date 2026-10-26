//! Preferences, stored as JSON in ~/Library/Application Support/Tamka/settings.json.
//! The file is created on first launch with the default spoken commands.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::keys::{self, Shortcut};

/// The spoken commands a fresh install starts with ("open paren" → "(", …).
const DEFAULT_COMMANDS: &str = include_str!("../resources/default-commands.json");

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub spoken: String,
    pub replacement: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Start and stop dictation. Mac virtual key code + modifier bits (see keys.rs).
    pub shortcut: Shortcut,
    /// Speech model: a `models::CATALOG` id, e.g. "parakeet ultra".
    pub engine: String,
    /// Write spoken numbers, money and dates as digits (engine `--itn`). Experimental.
    pub numbers_as_digits: bool,
    /// Leave the transcript on the clipboard after pasting.
    /// When false, the previous clipboard contents are put back.
    pub keep_on_clipboard: bool,
    /// Paste into the focused app. When false, the text is only copied.
    pub auto_paste: bool,
    /// System sound played when recording starts, by name ("Purr"). Empty = no sound.
    pub start_sound: String,
    /// System sound played when recording stops. Empty = no sound.
    pub stop_sound: String,
    /// The old on/off switch for both sounds, read once and turned into the two above.
    #[serde(rename = "sounds", skip_serializing)]
    legacy_sounds: Option<bool>,
    /// Drop "um", "uh" and similar.
    pub remove_fillers: bool,
    /// Collapse stutters like "the the" or "at the at the".
    pub collapse_repeats: bool,
    /// Recording stops on its own after this many seconds.
    pub max_seconds: u32,
    /// Where the listening indicator was last dragged to: [centre x, bottom y] in
    /// screen coordinates. None = bottom centre of the screen with the pointer.
    pub indicator_anchor: Option<[f64; 2]>,
    /// Spoken phrase → text, applied after cleanup. Longest phrase wins.
    pub find_replace: Vec<Rule>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            shortcut: Shortcut { key: keys::vk::V, mods: keys::OPTION },
            engine: "parakeet ultra".into(),
            numbers_as_digits: false,
            keep_on_clipboard: true,
            auto_paste: true,
            start_sound: "Purr".into(),
            stop_sound: "Pop".into(),
            legacy_sounds: None,
            remove_fillers: true,
            collapse_repeats: true,
            max_seconds: 600,
            indicator_anchor: None,
            find_replace: builtin_rules(),
        }
    }
}

/// Arguments for the engine helper.
pub fn engine_args(s: &Settings) -> String {
    if s.numbers_as_digits { format!("{} --itn", s.engine) } else { s.engine.clone() }
}

pub fn default_rules() -> Vec<Rule> {
    builtin_rules()
}

fn builtin_rules() -> Vec<Rule> {
    #[derive(Deserialize)]
    struct Defaults {
        commands: Vec<Rule>,
    }
    serde_json::from_str::<Defaults>(DEFAULT_COMMANDS).map(|d| d.commands).unwrap_or_default()
}

static CURRENT: Mutex<Option<Settings>> = Mutex::new(None);

pub fn dir() -> PathBuf {
    crate::util::home().join("Library/Application Support/Tamka")
}

pub fn path() -> PathBuf {
    dir().join("settings.json")
}

/// Current settings (loaded on first use).
pub fn get() -> Settings {
    let mut cur = CURRENT.lock().unwrap();
    if cur.is_none() {
        *cur = Some(load());
    }
    cur.clone().unwrap()
}

/// Change settings and save them at once. Returns the new settings.
pub fn update(f: impl FnOnce(&mut Settings)) -> Settings {
    let mut s = get();
    f(&mut s);
    save(&s);
    *CURRENT.lock().unwrap() = Some(s.clone());
    s
}

/// Settings from before the app was renamed (Phonon → Tamka), moved over once.
fn migrate_old_settings() {
    let old = crate::util::home().join("Library/Application Support/Phonon/settings.json");
    if !path().exists() && old.exists() {
        let _ = std::fs::create_dir_all(dir());
        if std::fs::copy(&old, path()).is_ok() {
            crate::util::log("moved settings over from the previous app name");
        }
    }
}

/// Sounds switched off with the old checkbox stay off.
fn migrate_sounds(s: &mut Settings) {
    if s.legacy_sounds.take() == Some(false) {
        s.start_sound.clear();
        s.stop_sound.clear();
    }
}

fn load() -> Settings {
    migrate_old_settings();
    match std::fs::read_to_string(path()) {
        Ok(text) => match serde_json::from_str::<Settings>(&text) {
            Ok(mut s) => {
                migrate_sounds(&mut s);
                s
            }
            Err(e) => {
                crate::util::log(format!("settings.json didn't parse ({e}); using defaults"));
                Settings::default()
            }
        },
        Err(_) => {
            let s = Settings::default();
            save(&s);
            s
        }
    }
}

pub fn save(s: &Settings) {
    let _ = std::fs::create_dir_all(dir());
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let tmp = path().with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(tmp, path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_commands_load() {
        let rules = builtin_rules();
        assert_eq!(rules.len(), 112);
        assert!(rules.iter().any(|r| r.spoken == "open paren" && r.replacement == "("));
    }

    #[test]
    fn settings_round_trip_and_fill_missing_fields() {
        let s: Settings = serde_json::from_str(r#"{"engine": "parakeet v2"}"#).unwrap();
        assert_eq!(s.engine, "parakeet v2");
        assert!(s.keep_on_clipboard);
        assert_eq!(s.shortcut, Shortcut { key: keys::vk::V, mods: keys::OPTION });
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.find_replace, s.find_replace);
    }

    #[test]
    fn old_sounds_switch_carries_over() {
        let mut off: Settings = serde_json::from_str(r#"{"sounds": false}"#).unwrap();
        migrate_sounds(&mut off);
        assert_eq!((off.start_sound.as_str(), off.stop_sound.as_str()), ("", ""));
        let mut on: Settings = serde_json::from_str(r#"{"sounds": true}"#).unwrap();
        migrate_sounds(&mut on);
        assert_eq!((on.start_sound.as_str(), on.stop_sound.as_str()), ("Purr", "Pop"));
        assert!(!serde_json::to_string(&on).unwrap().contains("\"sounds\""));
    }
}
