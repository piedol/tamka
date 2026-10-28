//! Tamka's side of the updater (the shared part is in update/): the menu item,
//! the daily check and its setting, the notification, and the update window.

use std::cell::RefCell;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{sel, AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSAlert, NSBorderType, NSButton, NSFont, NSFontAttributeName, NSMutableParagraphStyle,
    NSParagraphStyleAttributeName, NSScrollView, NSTextAlignment, NSTextTab, NSTextView, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSError, NSPoint, NSRange, NSRect, NSSet, NSSize, NSURL};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions, UNNotificationRequest,
    UNNotificationSettings, UNUserNotificationCenter,
};

use crate::update::notes::{Line, Style};
use crate::update::{self, Check, Release, Step};
use crate::util::{self, ns};
use crate::{app, hud, models, settings, ui};

pub static CONFIG: update::Config = update::Config {
    repo: "piedol/tamka",
    asset: "Tamka-macOS-arm64.zip",
    name: "Tamka",
    bundle_id: "top.piedol.tamka",
    team_id: "D9JNQPYFBM",
    env_prefix: "TAMKA",
    fallback_version: env!("CARGO_PKG_VERSION"),
    log: |m| util::log(m),
};

/// What the menu item offers.
enum State {
    Idle,
    /// A check chosen from the menu is running.
    Checking,
    Available(Release),
    Installing(Release, Step),
}

// The update window's buttons (their tags).
const INSTALL: isize = 1;
const LATER: isize = 2;
const OK: isize = 3;

/// The notification's category, action and request id.
const NOTIFICATION: &str = "update";

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::Idle) };
    /// The update window or a message, while one is open.
    static ALERT: RefCell<Option<Retained<NSAlert>>> = const { RefCell::new(None) };
    /// The version last announced in a notification, so each is announced once per run.
    static NOTIFIED: RefCell<Option<update::Version>> = const { RefCell::new(None) };
    /// TAMKA_UPDATE_TEST=install: unattended, so results go to the log only.
    static TESTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("Update UI must run on the main thread")
}

fn current() -> update::Version {
    update::current_version(&CONFIG)
}

fn set_state(s: State) {
    STATE.with(|st| *st.borrow_mut() = s);
}

fn available() -> Option<Release> {
    STATE.with(|st| match &*st.borrow() {
        State::Available(r) | State::Installing(r, _) => Some(r.clone()),
        _ => None,
    })
}

/// Called from `app::launched`.
pub fn launched() {
    if let Some(v) = update::launched(&CONFIG) {
        hud::toast(&format!("Updated to Tamka {v}"), None);
    }
    // For testing the whole install: check now and install anything newer without asking.
    if std::env::var("TAMKA_UPDATE_TEST").as_deref() == Ok("install") {
        TESTING.with(|t| t.set(true));
        util::log("update test: checking now, installing without asking");
        update::check(&CONFIG, |result| match result {
            Ok(Check::Available(r)) => {
                set_state(State::Available(r.clone()));
                install(r);
            }
            Ok(Check::UpToDate) => util::log("update test: nothing newer"),
            Err(e) => util::log(format!("update test: {e}")),
        });
        return;
    }
    setup_notifications();
    start_schedule();
}

fn start_schedule() {
    update::schedule::start(|| settings::get().check_updates, last_check, auto_check);
}

/// The Settings checkbox changed.
pub fn setting_changed() {
    start_schedule();
}

fn last_check() -> Option<SystemTime> {
    let secs = settings::get().last_update_check;
    (secs > 0).then(|| UNIX_EPOCH + Duration::from_secs(secs))
}

fn auto_check() {
    if STATE.with(|s| matches!(&*s.borrow(), State::Checking | State::Installing(..))) {
        return;
    }
    update::check(&CONFIG, |result| checked(result, false));
}

/// Check for Updates… in the menu.
fn check_now() {
    set_state(State::Checking);
    update::check(&CONFIG, |result| checked(result, true));
}

fn checked(result: Result<Check, update::Error>, manual: bool) {
    if result.is_ok() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        settings::update(|s| s.last_update_check = now);
    }
    if update::installing() {
        return;
    }
    match result {
        Ok(Check::Available(r)) => {
            set_state(State::Available(r.clone()));
            if manual {
                show_update(&r);
            } else {
                notify(&r);
            }
        }
        Ok(Check::UpToDate) => {
            set_state(State::Idle);
            if manual {
                message("Tamka is up to date", &format!("Tamka {} is the newest version.", current()));
            }
        }
        Err(e) => {
            if STATE.with(|s| matches!(&*s.borrow(), State::Checking)) {
                set_state(State::Idle);
            }
            if manual {
                message("Couldn't check for updates", &e.to_string());
            }
        }
    }
}

// ---------------------------------------------------------------- menu

/// The menu item: (title, subtitle, enabled).
pub fn menu_item() -> (String, Option<String>, bool) {
    STATE.with(|st| match &*st.borrow() {
        State::Idle => ("Check for Updates…".into(), None, true),
        State::Checking => ("Checking for Updates…".into(), None, false),
        State::Available(r) => (format!("Update to Tamka {}…", r.version), Some(format!("You have {}", current())), true),
        State::Installing(r, step) => (format!("Installing Tamka {}…", r.version), Some(step_text(*step, r.size)), false),
    })
}

fn step_text(step: Step, size: u64) -> String {
    match step {
        Step::Downloading { received, total } => {
            let total = if total > 0 { total } else { size };
            match (received * 100).checked_div(total) {
                Some(percent) => format!("Downloading, {}%", percent.min(100)),
                None => "Downloading".into(),
            }
        }
        Step::Verifying => "Checking the download".into(),
        Step::Waiting if models::downloading().is_some() => "Installs after the model download".into(),
        Step::Waiting => "Installs when you finish dictating".into(),
        Step::Replacing => "Replacing the app".into(),
    }
}

pub fn menu_chosen() {
    match available() {
        Some(r) if !update::installing() => show_update(&r),
        Some(_) => {}
        None if STATE.with(|s| matches!(&*s.borrow(), State::Idle)) => check_now(),
        None => {}
    }
}

// ---------------------------------------------------------------- installing

fn install(r: Release) {
    set_state(State::Installing(r.clone(), Step::Downloading { received: 0, total: 0 }));
    // A toast takes the indicator's place, so none while dictating.
    if app::is_idle() {
        hud::toast(&format!("Downloading Tamka {}…", r.version), None);
    }
    update::install(&CONFIG, r, update::Hooks { is_idle: app::is_idle, progress, done: installed });
}

fn progress(step: Step) {
    STATE.with(|st| {
        if let State::Installing(_, s) = &mut *st.borrow_mut() {
            *s = step;
        }
    });
}

fn installed(result: Result<(), update::Error>) {
    let Some(r) = available() else { return };
    match result {
        Ok(()) => ui::app(mtm()).terminate(None),
        Err(_) if TESTING.with(|t| t.get()) => set_state(State::Available(r)),
        Err(update::Error::NotWritable(why)) => {
            set_state(State::Available(r.clone()));
            if let Some(url) = NSURL::URLWithString(&ns(&r.page)) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
            message(&format!("Download Tamka {} from GitHub", r.version), &format!("{why} The release page is open in your browser."));
        }
        Err(update::Error::Relaunch(why)) => {
            set_state(State::Idle);
            message(&format!("Tamka {} is installed", r.version), &why);
        }
        Err(e) => {
            set_state(State::Available(r.clone()));
            message(&format!("Tamka {} wasn't installed", r.version), &format!("{e} Tamka {} is still installed.", current()));
        }
    }
}

// ---------------------------------------------------------------- windows

/// The version, the installed version and the release notes, with Install and Relaunch and Later.
fn show_update(r: &Release) {
    let mtm = mtm();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(&format!("Tamka {} is available", r.version)));
    alert.setInformativeText(&ns(&format!(
        "You have Tamka {}. Installing quits Tamka and opens the new version. Your settings and models carry over.",
        current()
    )));
    alert.setAccessoryView(Some(&notes_view(mtm, &r.notes)));
    let install = alert.addButtonWithTitle(&ns("Install and Relaunch"));
    let later = alert.addButtonWithTitle(&ns("Later"));
    later.setKeyEquivalent(&ns("\u{1b}"));
    present(alert, &[(install, INSTALL), (later, LATER)]);
}

/// A message with an OK button.
fn message(title: &str, text: &str) {
    let alert = NSAlert::new(mtm());
    alert.setMessageText(&ns(title));
    alert.setInformativeText(&ns(text));
    let ok = alert.addButtonWithTitle(&ns("OK"));
    present(alert, &[(ok, OK)]);
}

/// Show an alert without making it modal, so the menu and the shortcut keep
/// working while it's open. Its buttons call `button`.
fn present(alert: Retained<NSAlert>, buttons: &[(Retained<NSButton>, isize)]) {
    close_alert(false);
    let target = ui::controller(mtm());
    for (b, tag) in buttons {
        b.setTag(*tag);
        unsafe {
            b.setTarget(Some(&target));
            b.setAction(Some(sel!(updateButton:)));
        }
    }
    alert.layout();
    let window = alert.window();
    window.center();
    // The answer to a check arrives after the menu has closed, without fresh user input, and
    // macOS 14's cooperative activation then refuses `activate`, leaving the window behind
    // other apps' windows. The older call still brings it forward.
    let app = ui::app(mtm());
    app.activate();
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    window.makeKeyAndOrderFront(None);
    window.orderFrontRegardless();
    ALERT.with(|a| *a.borrow_mut() = Some(alert));
}

fn close_alert(deactivate: bool) {
    let Some(alert) = ALERT.with(|a| a.borrow_mut().take()) else { return };
    let window = alert.window();
    window.orderOut(None);
    // Hand focus back to the app you were in, unless Settings is still open.
    let app = ui::app(mtm());
    let others = app.windows().iter().any(|w| {
        Retained::as_ptr(&w) != Retained::as_ptr(&window) && w.isVisible() && w.styleMask().contains(NSWindowStyleMask::Titled)
    });
    if deactivate && !others {
        app.deactivate();
    }
}

/// A button in the update window or a message.
pub fn button(tag: isize) {
    close_alert(true);
    if tag == INSTALL
        && let Some(r) = available()
        && !update::installing()
    {
        install(r);
    }
}

/// The release notes in a scrolling text view.
fn notes_view(mtm: MainThreadMarker, markdown: &str) -> Retained<NSScrollView> {
    let scroll = NSTextView::scrollableTextView(mtm);
    scroll.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(440.0, 220.0)));
    scroll.setBorderType(NSBorderType::BezelBorder);
    let Some(view) = scroll.documentView().and_then(|v| v.downcast::<NSTextView>().ok()) else { return scroll };
    view.setEditable(false);
    view.setTextContainerInset(NSSize::new(6.0, 8.0));
    let lines = update::notes::parse(markdown);
    let (text, styles) = if lines.is_empty() { ("No release notes.".to_string(), Vec::new()) } else { layout(&lines) };
    view.setString(&ns(&text));
    view.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    let Some(storage) = (unsafe { view.textStorage() }) else { return scroll };
    let bullet = NSMutableParagraphStyle::new();
    let tab = unsafe {
        NSTextTab::initWithTextAlignment_location_options(NSTextTab::alloc(), NSTextAlignment::Left, 14.0, &NSDictionary::new())
    };
    bullet.setTabStops(Some(&NSArray::from_retained_slice(&[tab])));
    bullet.setHeadIndent(14.0);
    bullet.setParagraphSpacing(3.0);
    let fonts = [
        NSFont::boldSystemFontOfSize(13.0),
        NSFont::boldSystemFontOfSize(12.0),
        NSFont::monospacedSystemFontOfSize_weight(11.0, 0.0),
        NSFont::systemFontOfSize(5.0),
    ];
    for (range, look) in styles {
        let (key, value): (_, &AnyObject) = match look {
            Look::Heading => (unsafe { NSFontAttributeName }, &fonts[0]),
            Look::Bold => (unsafe { NSFontAttributeName }, &fonts[1]),
            Look::Code => (unsafe { NSFontAttributeName }, &fonts[2]),
            Look::Gap => (unsafe { NSFontAttributeName }, &fonts[3]),
            Look::Bullet => (unsafe { NSParagraphStyleAttributeName }, &bullet),
        };
        // SAFETY: fonts and a paragraph style are what these keys take; ranges lie within the text.
        unsafe { storage.addAttribute_value_range(key, value, range) };
    }
    scroll
}

#[derive(Clone, Copy)]
enum Look {
    Heading,
    Bold,
    Code,
    Gap,
    Bullet,
}

/// The notes as one string, with the ranges (in UTF-16 units) to style.
fn layout(lines: &[Line]) -> (String, Vec<(NSRange, Look)>) {
    let mut text = String::new();
    let mut styles = Vec::new();
    let len = |s: &str| s.encode_utf16().count();
    let mut after_gap = false;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            // A blank line is the newline that ends an empty paragraph: make it short.
            if after_gap {
                styles.push((NSRange::new(len(&text), 1), Look::Gap));
            }
            text.push('\n');
        }
        after_gap = matches!(line, Line::Gap);
        let start = len(&text);
        let spans = match line {
            Line::Gap => continue,
            Line::Bullet(s) => {
                text.push_str("•\t");
                s
            }
            Line::Heading(s) | Line::Text(s) => s,
        };
        for span in spans {
            let at = len(&text);
            text.push_str(&span.text);
            let look = match span.style {
                Style::Plain => continue,
                Style::Bold => Look::Bold,
                Style::Code => Look::Code,
            };
            styles.push((NSRange::new(at, len(&span.text)), look));
        }
        let whole = NSRange::new(start, len(&text) - start);
        match line {
            Line::Heading(_) => styles.push((whole, Look::Heading)),
            Line::Bullet(_) => styles.push((whole, Look::Bullet)),
            _ => {}
        }
    }
    (text, styles)
}

// ---------------------------------------------------------------- notification

/// UserNotifications only works inside the app bundle, not in a development build.
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    let main = NSBundle::mainBundle();
    let bundled = main.bundleIdentifier().is_some() && main.bundlePath().to_string().ends_with(".app");
    bundled.then(UNUserNotificationCenter::currentNotificationCenter)
}

/// The delegate and the notification's Update… button. No permission prompt.
fn setup_notifications() {
    let Some(c) = center() else { return };
    c.setDelegate(Some(ProtocolObject::from_ref(&*ui::controller(mtm()))));
    let action = UNNotificationAction::actionWithIdentifier_title_options(
        &ns(NOTIFICATION),
        &ns("Update…"),
        UNNotificationActionOptions::Foreground,
    );
    let category = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &ns(NOTIFICATION),
        &NSArray::from_retained_slice(&[action]),
        &NSArray::new(),
        UNNotificationCategoryOptions::empty(),
    );
    c.setNotificationCategories(&NSSet::from_retained_slice(&[category]));
}

/// Announce an update found by the daily check. macOS asks for permission the
/// first time this happens, not at launch.
fn notify(r: &Release) {
    let seen = NOTIFIED.with(|n| n.borrow_mut().replace(r.version.clone()));
    if seen.as_ref() == Some(&r.version) {
        return;
    }
    let Some(c) = center() else { return };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&ns(&format!("Tamka {} is available", r.version)));
    content.setBody(&ns(&format!("You have {}. Click to see what's new and install it.", current())));
    content.setCategoryIdentifier(&ns(NOTIFICATION));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&ns(NOTIFICATION), &content, None);
    let center = c.clone();
    let block = RcBlock::new(move |s: std::ptr::NonNull<UNNotificationSettings>| {
        // SAFETY: the system passes a valid settings object.
        let status = unsafe { s.as_ref() }.authorizationStatus();
        let post = {
            let (center, request) = (center.clone(), request.clone());
            move || center.addNotificationRequest_withCompletionHandler(&request, None)
        };
        if status == UNAuthorizationStatus::NotDetermined {
            let asked = RcBlock::new(move |granted: Bool, _e: *mut NSError| {
                util::log(format!("notifications {}", if granted.as_bool() { "allowed" } else { "declined" }));
                if granted.as_bool() {
                    post();
                }
            });
            center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &asked);
        } else if status == UNAuthorizationStatus::Authorized || status == UNAuthorizationStatus::Provisional {
            post();
        }
    });
    c.getNotificationSettingsWithCompletionHandler(&block);
}

/// A click on the notification or its Update… button (not a dismissal).
pub fn notification_response(action: &str) {
    if action == "com.apple.UNNotificationDismissActionIdentifier" {
        return;
    }
    util::main_async(|| {
        if let Some(r) = available()
            && !update::installing()
        {
            show_update(&r);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_layout_styles_the_right_text() {
        let lines = update::notes::parse("## New\n\n- **Mic choice.** Pick one.\n- `a`é");
        let (text, styles) = layout(&lines);
        assert_eq!(text, "New\n\n•\tMic choice. Pick one.\n•\taé");
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let slice = |r: NSRange| String::from_utf16(&utf16[r.location..r.location + r.length]).unwrap();
        let bold = styles.iter().find(|(_, l)| matches!(l, Look::Bold)).unwrap().0;
        assert_eq!(slice(bold), "Mic choice.");
        let code = styles.iter().find(|(_, l)| matches!(l, Look::Code)).unwrap().0;
        assert_eq!(slice(code), "a");
        assert_eq!(styles.iter().filter(|(_, l)| matches!(l, Look::Bullet)).count(), 2);
        assert!(styles.iter().all(|(r, _)| r.location + r.length <= utf16.len()));
        let gap = styles.iter().find(|(_, l)| matches!(l, Look::Gap)).unwrap().0;
        assert_eq!((gap.location, slice(gap)), (4, "\n".to_string()));
    }

    #[test]
    fn progress_reads_as_text() {
        assert_eq!(step_text(Step::Downloading { received: 5, total: 10 }, 0), "Downloading, 50%");
        assert_eq!(step_text(Step::Downloading { received: 5, total: 0 }, 20), "Downloading, 25%");
        assert_eq!(step_text(Step::Downloading { received: 0, total: 0 }, 0), "Downloading");
    }
}
