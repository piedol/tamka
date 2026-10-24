//! Microphone and Accessibility permission checks.
//!
//! Microphone: needed to record. Accessibility: needed to press ⌘V for you in the
//! app you're typing in. Without it Tamka still copies the text to the clipboard.

use std::sync::Mutex;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_app_kit::NSWorkspace;
use objc2_avf_audio::{AVAudioApplication, AVAudioApplicationRecordPermission};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use crate::util::{self, ns};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mic {
    Granted,
    Denied,
    NotAsked,
}

pub fn mic() -> Mic {
    let p = unsafe { AVAudioApplication::sharedInstance().recordPermission() };
    if p == AVAudioApplicationRecordPermission::Granted {
        Mic::Granted
    } else if p == AVAudioApplicationRecordPermission::Denied {
        Mic::Denied
    } else {
        Mic::NotAsked
    }
}

/// Show the system microphone prompt; `then(granted)` runs on the main thread.
pub fn request_mic(then: impl FnOnce(bool) + Send + 'static) {
    let then = Mutex::new(Some(then));
    let block = RcBlock::new(move |granted: Bool| {
        if let Some(f) = then.lock().unwrap().take() {
            let granted = granted.as_bool();
            util::main_async(move || f(granted));
        }
    });
    unsafe { AVAudioApplication::requestRecordPermissionWithCompletionHandler(&block) };
}

pub fn accessibility_ok() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Show the system "Tamka would like to control this computer" prompt, which
/// links straight to the Accessibility list in System Settings.
pub fn ask_accessibility() {
    let key = NSString::from_str("AXTrustedCheckOptionPrompt");
    let yes = NSNumber::new_bool(true);
    let options = NSDictionary::<NSString, NSNumber>::from_slices(&[&*key], &[&*yes]);
    unsafe { AXIsProcessTrustedWithOptions(objc2::rc::Retained::as_ptr(&options).cast()) };
}

pub fn open_settings(anchor: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{anchor}");
    if let Some(u) = NSURL::URLWithString(&ns(&url)) {
        NSWorkspace::sharedWorkspace().openURL(&u);
    }
}
