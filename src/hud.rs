//! On-screen feedback: the recording pill and short toasts. The pill starts at the
//! bottom centre of the screen with the pointer; drag it anywhere and it (and the
//! toasts that replace it) stays there for the next dictation. All windows are
//! non-activating, so the app you're typing in keeps focus and receives the paste.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
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

thread_local! {
    /// While dragging the pill: (pointer at mouse-down, panel origin at mouse-down).
    static DRAG: Cell<Option<(NSPoint, NSPoint)>> = const { Cell::new(None) };
}

define_class!(
    // SAFETY: NSView subclass that only handles mouse events; no ivars, no Drop.
    // It sits on top of the pill and turns any press into a window drag, without
    // activating Tamka (the panel is non-activating), so focus stays in your app.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TamkaDragView"]
    struct DragView;

    impl DragView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            if let Some(w) = self.window() {
                DRAG.with(|d| d.set(Some((NSEvent::mouseLocation(), w.frame().origin))));
                objc2_app_kit::NSCursor::closedHandCursor().push();
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            let (Some((start, origin)), Some(w)) = (DRAG.with(Cell::get), self.window()) else { return };
            let now = NSEvent::mouseLocation();
            w.setFrameOrigin(NSPoint::new(origin.x + now.x - start.x, origin.y + now.y - start.y));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            objc2_app_kit::NSCursor::closedHandCursor().pop();
            let Some((start, _)) = DRAG.with(|d| d.take()) else { return };
            let now = NSEvent::mouseLocation();
            if (now.x - start.x).abs() < 1.0 && (now.y - start.y).abs() < 1.0 {
                return; // a click, not a drag
            }
            if let Some(w) = self.window() {
                let f = w.frame();
                let anchor = [f.origin.x + f.size.width / 2.0, f.origin.y];
                crate::settings::update(|s| s.indicator_anchor = Some(anchor));
                util::log("indicator moved");
            }
        }
    }
);

/// A transparent layer over the whole pill that makes it draggable.
fn drag_layer(mtm: MainThreadMarker, size: NSSize) -> Retained<DragView> {
    let this = DragView::alloc(mtm).set_ivars(());
    let v: Retained<DragView> = unsafe { msg_send![super(this), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)] };
    v.setToolTip(Some(&ns("Drag to move. It stays here for your next dictation.")));
    v
}

/// Where a pill or toast of this size goes: the spot it was last dragged to, if
/// that's still on a connected screen, otherwise bottom centre of the pointer's screen.
fn origin_for(mtm: MainThreadMarker, w: f64, h: f64) -> NSPoint {
    if let Some([cx, bottom]) = crate::settings::get().indicator_anchor {
        for s in NSScreen::screens(mtm).iter() {
            let f = s.frame();
            let inside = cx >= f.origin.x && cx < f.origin.x + f.size.width && bottom >= f.origin.y
                && bottom < f.origin.y + f.size.height;
            if inside {
                // Keep it fully on that screen even if this one is wider than the last.
                let v = s.visibleFrame();
                let x = (cx - w / 2.0).clamp(v.origin.x + 4.0, v.origin.x + v.size.width - w - 4.0);
                let y = bottom.clamp(v.origin.y + 4.0, v.origin.y + v.size.height - h - 4.0);
                return NSPoint::new(x, y);
            }
        }
    }
    let screen = pointer_screen(mtm);
    NSPoint::new(screen.origin.x + (screen.size.width - w) / 2.0, screen.origin.y + 72.0)
}

/// Forget the dragged position (Settings › General › Reset Position).
pub fn reset_position() {
    crate::settings::update(|s| s.indicator_anchor = None);
    let mtm = mtm();
    PILL.with(|cell| {
        if let Some(p) = cell.borrow().as_ref()
            && p.panel.isVisible()
        {
            let f = p.panel.frame();
            p.panel.setFrameOrigin(origin_for(mtm, f.size.width, f.size.height));
        }
    });
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
    // Unlike toasts, the pill takes the mouse so it can be dragged out of the way.
    p.setIgnoresMouseEvents(false);
    p.setBecomesKeyOnlyIfNeeded(true);
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
        // Topmost: the whole pill is a drag handle.
        s.addSubview(&drag_layer(mtm, size));
        pill.panel.setContentView(Some(&s));
        let origin = origin_for(mtm, w, PILL_H);
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
    let origin = origin_for(mtm, w, h);
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
