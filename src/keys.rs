//! Key codes: macOS virtual key codes ↔ `global-hotkey` codes and display glyphs.
//! (Same table as Photon.)

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use serde::{Deserialize, Serialize};

pub const SHIFT: u32 = 1 << 17;
pub const CONTROL: u32 = 1 << 18;
pub const OPTION: u32 = 1 << 19;
pub const COMMAND: u32 = 1 << 20;
pub const MOD_MASK: u32 = SHIFT | CONTROL | OPTION | COMMAND;

pub mod vk {
    pub const V: u16 = 9;
    pub const ESCAPE: u16 = 53;
}

/// (mac key code, hotkey code, label)
const TABLE: &[(u16, Code, &str)] = &[
    (0, Code::KeyA, "A"), (11, Code::KeyB, "B"), (8, Code::KeyC, "C"), (2, Code::KeyD, "D"),
    (14, Code::KeyE, "E"), (3, Code::KeyF, "F"), (5, Code::KeyG, "G"), (4, Code::KeyH, "H"),
    (34, Code::KeyI, "I"), (38, Code::KeyJ, "J"), (40, Code::KeyK, "K"), (37, Code::KeyL, "L"),
    (46, Code::KeyM, "M"), (45, Code::KeyN, "N"), (31, Code::KeyO, "O"), (35, Code::KeyP, "P"),
    (12, Code::KeyQ, "Q"), (15, Code::KeyR, "R"), (1, Code::KeyS, "S"), (17, Code::KeyT, "T"),
    (32, Code::KeyU, "U"), (9, Code::KeyV, "V"), (13, Code::KeyW, "W"), (7, Code::KeyX, "X"),
    (16, Code::KeyY, "Y"), (6, Code::KeyZ, "Z"),
    (29, Code::Digit0, "0"), (18, Code::Digit1, "1"), (19, Code::Digit2, "2"), (20, Code::Digit3, "3"),
    (21, Code::Digit4, "4"), (23, Code::Digit5, "5"), (22, Code::Digit6, "6"), (26, Code::Digit7, "7"),
    (28, Code::Digit8, "8"), (25, Code::Digit9, "9"),
    (27, Code::Minus, "-"), (24, Code::Equal, "="), (33, Code::BracketLeft, "["),
    (30, Code::BracketRight, "]"), (41, Code::Semicolon, ";"), (39, Code::Quote, "'"),
    (43, Code::Comma, ","), (47, Code::Period, "."), (44, Code::Slash, "/"),
    (42, Code::Backslash, "\\"), (50, Code::Backquote, "`"),
    (49, Code::Space, "Space"), (36, Code::Enter, "↩"), (48, Code::Tab, "⇥"),
    (122, Code::F1, "F1"), (120, Code::F2, "F2"), (99, Code::F3, "F3"), (118, Code::F4, "F4"),
    (96, Code::F5, "F5"), (97, Code::F6, "F6"), (98, Code::F7, "F7"), (100, Code::F8, "F8"),
    (101, Code::F9, "F9"), (109, Code::F10, "F10"), (103, Code::F11, "F11"), (111, Code::F12, "F12"),
    (105, Code::F13, "F13"), (107, Code::F14, "F14"), (113, Code::F15, "F15"), (106, Code::F16, "F16"),
];

/// A global shortcut: mac key code + modifier bits (NSEvent layout).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    pub key: u16,
    pub mods: u32,
}

impl Shortcut {
    pub fn hotkey(self) -> Option<HotKey> {
        let code = TABLE.iter().find(|(k, _, _)| *k == self.key)?.1;
        let mut m = Modifiers::empty();
        if self.mods & SHIFT != 0 { m |= Modifiers::SHIFT; }
        if self.mods & CONTROL != 0 { m |= Modifiers::CONTROL; }
        if self.mods & OPTION != 0 { m |= Modifiers::ALT; }
        if self.mods & COMMAND != 0 { m |= Modifiers::META; }
        Some(HotKey::new(Some(m), code))
    }

    /// "⌥V" style label in Apple's modifier order.
    pub fn label(self) -> String {
        let mut s = String::new();
        if self.mods & CONTROL != 0 { s.push('⌃'); }
        if self.mods & OPTION != 0 { s.push('⌥'); }
        if self.mods & SHIFT != 0 { s.push('⇧'); }
        if self.mods & COMMAND != 0 { s.push('⌘'); }
        s.push_str(key_label(self.key).unwrap_or("?"));
        s
    }

    /// Global shortcuts need ⌘, ⌃ or ⌥ (shift alone would eat typing).
    pub fn is_valid(self) -> bool {
        self.mods & (COMMAND | CONTROL | OPTION) != 0 && key_label(self.key).is_some()
    }

    /// Menu key equivalent character (lowercase) if the key has one.
    pub fn menu_char(self) -> Option<String> {
        let l = key_label(self.key)?;
        (l.chars().count() == 1 && l.is_ascii()).then(|| l.to_ascii_lowercase())
    }
}

pub fn key_label(key: u16) -> Option<&'static str> {
    TABLE.iter().find(|(k, _, _)| *k == key).map(|t| t.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_v() {
        let s = Shortcut { key: vk::V, mods: OPTION };
        assert_eq!(s.label(), "⌥V");
        assert!(s.hotkey().is_some());
    }
}
