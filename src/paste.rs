//! Put the transcript where you're typing: copy it, then press ⌘V for you.
//!
//! Pressing ⌘V needs Accessibility permission. Without it the text is still on the
//! clipboard and Tamka says so.

use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};
use objc2_foundation::{NSArray, NSData, NSString};

use crate::settings::Settings;
use crate::{permissions, util};

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Copied and ⌘V sent.
    Pasted,
    /// Copied only (auto-paste off, or no Accessibility permission).
    Copied,
}

/// Everything on the clipboard, so it can be put back afterwards.
type Snapshot = Vec<Vec<(String, Vec<u8>)>>;

fn snapshot(pb: &NSPasteboard) -> Snapshot {
    let Some(items) = pb.pasteboardItems() else { return Vec::new() };
    items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|ty| item.dataForType(&ty).map(|d| (ty.to_string(), d.to_vec())))
                .collect()
        })
        .collect()
}

fn restore(pb: &NSPasteboard, saved: Snapshot) {
    if saved.is_empty() {
        return;
    }
    pb.clearContents();
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = saved
        .into_iter()
        .map(|entries| {
            let item = NSPasteboardItem::new();
            for (ty, data) in entries {
                item.setData_forType(&NSData::with_bytes(&data), &NSString::from_str(&ty));
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    pb.writeObjects(&NSArray::from_retained_slice(&items));
}

pub fn deliver(text: &str, settings: &Settings) -> Outcome {
    let pb = NSPasteboard::generalPasteboard();
    let saved = (!settings.keep_on_clipboard).then(|| snapshot(&pb));
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    let ours = pb.changeCount();

    if !settings.auto_paste || !permissions::accessibility_ok() {
        return Outcome::Copied;
    }
    std::thread::spawn(move || {
        wait_for_modifiers_up(Duration::from_millis(800));
        press_command_v();
        if let Some(saved) = saved {
            // Give the target app time to read the clipboard before putting it back.
            std::thread::sleep(Duration::from_millis(600));
            util::main_async(move || {
                let pb = NSPasteboard::generalPasteboard();
                if pb.changeCount() == ours {
                    restore(&pb, saved);
                }
            });
        }
    });
    Outcome::Pasted
}

/// ⌥ from ⌥V may still be held; pressing ⌘V while it is would send ⌥⌘V.
fn wait_for_modifiers_up(max: Duration) {
    let held = CGEventFlags::MaskCommand | CGEventFlags::MaskAlternate | CGEventFlags::MaskControl | CGEventFlags::MaskShift;
    let until = Instant::now() + max;
    while Instant::now() < until {
        if !CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState).intersects(held) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn press_command_v() {
    const V: u16 = 9;
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(None, V, down);
        CGEvent::set_flags(event.as_deref(), CGEventFlags::MaskCommand);
        CGEvent::post(CGEventTapLocation::HIDEventTap, event.as_deref());
    }
}
