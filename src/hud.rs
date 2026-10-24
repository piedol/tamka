//! On-screen feedback: the recording pill (bottom centre of the screen with the
//! pointer) and short toasts. All windows are non-activating, so the app you're
//! typing in keeps focus and receives the paste.

use std::cell::RefCell;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSAppearance, NSAppearanceCustomization,
    NSAppearanceNameDarkAqua, NSBackingStoreType, NSColor,
    NSControlSize, NSEvent, NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSImageView, NSPanel,
    NSProgressIndicator, NSProgressIndicatorStyle, NSScreen, NSScreenSaverWindowLevel, NSTextField,
    NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::util::{self, ns};

const PILL_H: f64 = 38.0;
/// Photon's HUD corner radius; every Tamka surface shares it.
const CORNER: f64 = 10.0;

struct Pill {
    panel: Retained<NSPanel>,
    dot: Retained<NSImageView>,
    spinner: Retained<NSProgressIndicator>,
    time: Retained<NSTextField>,
    title: Retained<NSTextField>,
    hint: Retained<NSTextField>,
}

thread_local! {
    static PILL: RefCell<Option<Pill>> = const { RefCell::new(None) };
    static TOAST: RefCell<Option<(u64, Retained<NSPanel>)>> = const { RefCell::new(None) };
    static TOAST_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Bumped whenever the pill is shown or hidden, so a late fade can't hide a new pill.
    static PILL_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("HUD work must run on the main thread")
}

fn panel(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSPanel> {
    let p = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe { p.setReleasedWhenClosed(false) };
    p.setLevel(NSScreenSaverWindowLevel + 1);
    p.setOpaque(false);
    p.setBackgroundColor(Some(&NSColor::clearColor()));
    p.setHasShadow(true);
    p.setHidesOnDeactivate(false);
    p.setIgnoresMouseEvents(true);
    p.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary,
    );
    p
}

fn surface(mtm: MainThreadMarker, size: NSSize, radius: f64) -> Retained<NSVisualEffectView> {
    let v = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), size));
    v.setMaterial(NSVisualEffectMaterial::HUDWindow);
    v.setState(NSVisualEffectState::Active);
    v.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    v.setWantsLayer(true);
    if let Some(layer) = v.layer() {
        layer.setCornerRadius(radius);
        layer.setMasksToBounds(true);
    }
    if let Some(a) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        v.setAppearance(Some(&a));
    }
    v
}

fn label(mtm: MainThreadMarker, text: &str, size: f64, weight: f64, alpha: f64) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&ns(text), mtm);
    l.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(size, weight)));
    l.setTextColor(Some(&NSColor::colorWithWhite_alpha(1.0, alpha)));
    l
}

/// Visible frame of the screen with the pointer.
fn pointer_screen(mtm: MainThreadMarker) -> NSRect {
    let m = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    for s in screens.iter() {
        let f = s.frame();
        if m.x >= f.origin.x && m.x < f.origin.x + f.size.width && m.y >= f.origin.y && m.y < f.origin.y + f.size.height {
            return s.visibleFrame();
        }
    }
    NSScreen::mainScreen(mtm).map(|s| s.visibleFrame()).unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)))
}

fn place(view: &NSView, x: f64, y: f64, w: f64, h: f64) {
    view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
}

fn build_pill(mtm: MainThreadMarker) -> Pill {
    let p = panel(mtm, NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, PILL_H)));
    let dot = NSImageView::new(mtm);
    if let Some(img) = crate::ui::symbol("circle.fill", 10.0) {
        dot.setImage(Some(&img));
    }
    dot.setContentTintColor(Some(&NSColor::systemRedColor()));
    let spinner = NSProgressIndicator::new(mtm);
    spinner.setStyle(NSProgressIndicatorStyle::Spinning);
    spinner.setControlSize(NSControlSize::Small);
    let time = label(mtm, "0:00", 13.0, unsafe { NSFontWeightSemibold }, 0.95);
    let title = label(mtm, "Listening", 13.0, unsafe { NSFontWeightMedium }, 0.95);
    let hint = label(mtm, "", 11.0, 0.0, 0.55);
    Pill { panel: p, dot, spinner, time, title, hint }
}

/// Lay out and show the pill. `recording` picks the dot + timer or the spinner.
fn show_pill(recording: bool, title: &str, hint: &str) {
    let mtm = mtm();
    PILL.with(|cell| {
        let mut cell = cell.borrow_mut();
        let pill = cell.get_or_insert_with(|| build_pill(mtm));
        PILL_GEN.with(|g| g.set(g.get() + 1));
        pill.title.setStringValue(&ns(title));
        pill.hint.setStringValue(&ns(hint));
        let title_w = pill.title.fittingSize().width;
        let hint_w = if hint.is_empty() { 0.0 } else { pill.hint.fittingSize().width };
        // Room for the widest clock ("00:00"; tabular digits) plus an 8pt gap.
        let time_w = if recording {
            let current = pill.time.stringValue();
            pill.time.setStringValue(&ns("00:00"));
            let w = pill.time.fittingSize().width.ceil() + 8.0;
            pill.time.setStringValue(&current);
            w
        } else {
            0.0
        };
        let w = 14.0 + 16.0 + 8.0 + time_w + title_w + if hint_w > 0.0 { 14.0 + hint_w } else { 0.0 } + 16.0;
        let size = NSSize::new(w, PILL_H);
        let s = surface(mtm, size, CORNER);
        let mid = PILL_H / 2.0;
        let mut x = 14.0;
        if recording {
            place(&pill.dot, x, mid - 8.0, 16.0, 16.0);
            pill.dot.setAlphaValue(1.0);
            s.addSubview(&pill.dot);
            unsafe { pill.spinner.stopAnimation(None) };
        } else {
            place(&pill.spinner, x, mid - 8.0, 16.0, 16.0);
            s.addSubview(&pill.spinner);
            unsafe { pill.spinner.startAnimation(None) };
        }
        x += 16.0 + 8.0;
        if recording {
            place(&pill.time, x, mid - 9.0, time_w, 18.0);
            s.addSubview(&pill.time);
            x += time_w;
        }
        place(&pill.title, x, mid - 9.0, title_w + 2.0, 18.0);
        s.addSubview(&pill.title);
        x += title_w + 14.0;
        if hint_w > 0.0 {
            place(&pill.hint, x, mid - 8.0, hint_w + 2.0, 16.0);
            s.addSubview(&pill.hint);
        }
        pill.panel.setContentView(Some(&s));
        let screen = pointer_screen(mtm);
        let origin = NSPoint::new(screen.origin.x + (screen.size.width - w) / 2.0, screen.origin.y + 72.0);
        pill.panel.setFrame_display(NSRect::new(origin, size), true);
        pill.panel.setAlphaValue(1.0);
        pill.panel.orderFrontRegardless();
    });
}

pub fn show_recording(shortcut: &str) {
    show_pill(true, "Listening", &format!("{shortcut} to finish · Esc to cancel"));
    update_recording(Duration::ZERO, 0.0);
}

/// Refresh the timer and pulse the dot with the input level (0–1).
pub fn update_recording(elapsed: Duration, level: f32) {
    PILL.with(|cell| {
        if let Some(p) = cell.borrow().as_ref() {
            let s = elapsed.as_secs();
            p.time.setStringValue(&ns(&format!("{}:{:02}", s / 60, s % 60)));
            p.dot.setAlphaValue(0.35 + 0.65 * level as f64);
        }
    });
}

pub fn show_transcribing() {
    show_pill(false, "Transcribing…", "");
}

/// Fade the pill out over 150 ms (the one motion Tamka has, as in Photon).
pub fn hide() {
    let generation = PILL_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let Some(panel) = PILL.with(|cell| cell.borrow().as_ref().map(|p| p.panel.clone())) else { return };
    if !panel.isVisible() {
        return;
    }
    NSAnimationContext::runAnimationGroup(&block2::RcBlock::new(|ctx: std::ptr::NonNull<NSAnimationContext>| {
        unsafe { ctx.as_ref() }.setDuration(0.15);
        panel.animator().setAlphaValue(0.0);
    }));
    util::main_after(0.18, move || {
        // A new recording may have shown the pill again during the fade.
        if PILL_GEN.with(|g| g.get()) == generation {
            hide_now();
        }
    });
}

/// Take the pill down at once, when a toast replaces it in the same spot.
fn hide_now() {
    PILL_GEN.with(|g| g.set(g.get() + 1));
    PILL.with(|cell| {
        if let Some(p) = cell.borrow().as_ref() {
            unsafe { p.spinner.stopAnimation(None) };
            p.panel.orderOut(None);
        }
    });
}

/// A quiet confirmation in the pill's spot, e.g. "Copied". Green check, like Photon.
pub fn toast(title: &str, detail: Option<&str>) {
    show_toast(title, detail, false);
}

/// Something needs attention: orange warning symbol, stays a little longer.
pub fn notice(title: &str, detail: Option<&str>) {
    show_toast(title, detail, true);
}

fn show_toast(title: &str, detail: Option<&str>, warning: bool) {
    let mtm = mtm();
    hide_now();
    let t = label(mtm, title, 13.0, unsafe { NSFontWeightSemibold }, 0.95);
    let d = detail.map(|d| label(mtm, d, 11.0, 0.0, 0.6));
    let text_x = 14.0 + 18.0 + 8.0;
    let w = text_x + 16.0 + t.fittingSize().width.max(d.as_ref().map_or(0.0, |d| d.fittingSize().width.min(640.0)));
    let h = if d.is_some() { 50.0 } else { PILL_H };
    let size = NSSize::new(w, h);
    let s = surface(mtm, size, CORNER);
    let (name, tint) = if warning {
        ("exclamationmark.triangle.fill", NSColor::systemOrangeColor())
    } else {
        ("checkmark.circle.fill", NSColor::systemGreenColor())
    };
    if let Some(img) = crate::ui::symbol(name, 15.0) {
        let iv = NSImageView::imageViewWithImage(&img, mtm);
        iv.setContentTintColor(Some(&tint));
        place(&iv, 14.0, h / 2.0 - 10.0, 20.0, 20.0);
        s.addSubview(&iv);
    }
    match &d {
        Some(d) => {
            place(&t, text_x, 26.0, w - text_x - 16.0, 18.0);
            place(d, text_x, 8.0, w - text_x - 16.0, 16.0);
            d.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingMiddle);
            s.addSubview(d);
        }
        None => place(&t, text_x, h / 2.0 - 9.0, w - text_x - 16.0, 18.0),
    }
    s.addSubview(&t);
    let screen = pointer_screen(mtm);
    let origin = NSPoint::new(screen.origin.x + (screen.size.width - w) / 2.0, screen.origin.y + 72.0);
    let p = panel(mtm, NSRect::new(origin, size));
    p.setContentView(Some(&s));
    p.orderFrontRegardless();
    let id = TOAST_ID.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    TOAST.with(|cell| {
        if let Some((_, old)) = cell.borrow_mut().replace((id, p)) {
            old.orderOut(None);
        }
    });
    // Photon's timings: 0.9 s for a plain confirmation, 2.2 s with detail, 4 s for notices.
    let secs = if warning { 4.0 } else if detail.is_some() { 2.2 } else { 0.9 };
    util::main_after(secs, move || {
        let current = TOAST.with(|cell| cell.borrow().as_ref().filter(|(i, _)| *i == id).map(|(_, p)| p.clone()));
        let Some(p) = current else { return };
        // The one motion Tamka has, as in Photon: a 150 ms fade on the way out.
        NSAnimationContext::runAnimationGroup(&block2::RcBlock::new(|ctx: std::ptr::NonNull<NSAnimationContext>| {
            unsafe { ctx.as_ref() }.setDuration(0.15);
            p.animator().setAlphaValue(0.0);
        }));
        util::main_after(0.18, move || {
            TOAST.with(|cell| {
                let mut cell = cell.borrow_mut();
                if cell.as_ref().is_some_and(|(i, _)| *i == id)
                    && let Some((_, p)) = cell.take()
                {
                    p.orderOut(None);
                }
            });
        });
    });
}
