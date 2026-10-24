//! Global shortcuts via Carbon (`global-hotkey`). No Accessibility permission needed.
//! ⌥V toggles dictation; Esc is claimed only while recording, to cancel.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};

use global_hotkey::hotkey::{Code, HotKey};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::keys::Shortcut;
use crate::util;

static DICTATE_ID: AtomicU32 = AtomicU32::new(0);
static ESC_ID: AtomicU32 = AtomicU32::new(0);

struct Registered {
    manager: GlobalHotKeyManager,
    dictate: Option<HotKey>,
}

thread_local! {
    static STATE: RefCell<Option<Registered>> = const { RefCell::new(None) };
}

/// Register (or re-register) the dictation shortcut.
pub fn install(shortcut: Shortcut) -> Result<(), String> {
    STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            let manager = GlobalHotKeyManager::new().map_err(|e| format!("Shortcuts unavailable: {e}"))?;
            GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
                if event.state() != HotKeyState::Pressed {
                    return;
                }
                let id = event.id();
                if id == DICTATE_ID.load(Ordering::Relaxed) {
                    util::main_async(crate::app::toggle);
                } else if id == ESC_ID.load(Ordering::Relaxed) {
                    util::main_async(crate::app::cancel);
                }
            }));
            *cell = Some(Registered { manager, dictate: None });
        }
        let reg = cell.as_mut().unwrap();
        if let Some(old) = reg.dictate.take() {
            let _ = reg.manager.unregister(old);
        }
        let hk = shortcut.hotkey().ok_or_else(|| format!("{} isn't a supported shortcut.", shortcut.label()))?;
        reg.manager
            .register(hk)
            .map_err(|e| format!("{} is already used by another app: {e}", shortcut.label()))?;
        DICTATE_ID.store(hk.id(), Ordering::Relaxed);
        reg.dictate = Some(hk);
        Ok(())
    })
}

/// Stop listening while the Settings shortcut recorder captures keys.
pub fn suspend() {
    STATE.with(|cell| {
        if let Some(reg) = cell.borrow_mut().as_mut()
            && let Some(hk) = reg.dictate
        {
            let _ = reg.manager.unregister(hk);
        }
    });
}

pub fn resume() {
    STATE.with(|cell| {
        if let Some(reg) = cell.borrow_mut().as_mut()
            && let Some(hk) = reg.dictate
        {
            let _ = reg.manager.register(hk);
        }
    });
}

/// Claim Esc system-wide while recording so it always cancels, then give it back.
pub fn set_escape(on: bool) {
    let esc = HotKey::new(None, Code::Escape);
    STATE.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(reg) = cell.as_mut() else { return };
        let registered = ESC_ID.load(Ordering::Relaxed) != 0;
        if on && !registered {
            match reg.manager.register(esc) {
                Ok(()) => ESC_ID.store(esc.id(), Ordering::Relaxed),
                Err(e) => util::log(format!("couldn't claim Esc: {e}")),
            }
        } else if !on && registered {
            let _ = reg.manager.unregister(esc);
            ESC_ID.store(0, Ordering::Relaxed);
        }
    });
}
