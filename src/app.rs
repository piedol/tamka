//! App lifecycle, the menu-bar item, and the dictation state machine:
//! idle → (⌥V) recording → (⌥V) transcribing → paste → idle. Esc cancels a recording.

use std::cell::{Cell, RefCell};
use std::time::SystemTime;

use objc2::rc::Retained;
use objc2::{sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSColor, NSEventModifierFlags, NSImageSymbolConfiguration, NSMenu, NSMenuItem,
    NSSound, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSArray, NSString, NSTimer};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::permissions::{self, Mic};
use crate::util::{self, ns};
use crate::{audio, engine, hotkey, hud, models, paste, settings, settings_window, text, ui};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Idle,
    Recording,
    Transcribing,
}

mod tag {
    pub const TOGGLE: isize = 1;
    pub const RETRY: isize = 2;
    pub const SETTINGS: isize = 3;
    pub const ENGINE: isize = 4;
    pub const QUIT: isize = 7;
    pub const ALLOW_PASTE: isize = 8;
    pub const ALLOW_MIC: isize = 9;
    pub const RECENT: isize = 100;
}

/// Transcripts shown in the menu.
const RECENT_SHOWN: usize = 5;
const MIN_SECONDS: f64 = 0.3;

struct Shell {
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
}

thread_local! {
    static PHASE: Cell<Phase> = const { Cell::new(Phase::Idle) };
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
    static HISTORY: RefCell<Vec<(String, SystemTime)>> = const { RefCell::new(Vec::new()) };
    static TIMER: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    /// Time limit for the current recording, read once when it starts.
    static MAX_SECONDS: Cell<u64> = const { Cell::new(600) };
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("Tamka must start on the main thread");
    let app = ui::app(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    ui::set_app_delegate(mtm);
    app.run();
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("UI work must run on the main thread")
}

/// A main menu the user never sees (Tamka has no Dock icon or menu bar of its
/// own), installed so standard key equivalents reach Settings' text fields and
/// window: ⌘Z ⇧⌘Z ⌘X ⌘C ⌘V ⌘A, ⌘F (search commands) and ⌘W.
fn install_main_menu(mtm: MainThreadMarker) {
    let item = |title: &str, action: objc2::runtime::Sel, key: &str, mods: NSEventModifierFlags| {
        let i = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), Some(action), &ns(key)) };
        i.setKeyEquivalentModifierMask(mods);
        i
    };
    let cmd = NSEventModifierFlags::Command;
    let main = NSMenu::new(mtm);
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Edit"));
    edit.addItem(&item("Undo", sel!(undo:), "z", cmd));
    edit.addItem(&item("Redo", sel!(redo:), "z", cmd | NSEventModifierFlags::Shift));
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    edit.addItem(&item("Cut", sel!(cut:), "x", cmd));
    edit.addItem(&item("Copy", sel!(copy:), "c", cmd));
    edit.addItem(&item("Paste", sel!(paste:), "v", cmd));
    edit.addItem(&item("Select All", sel!(selectAll:), "a", cmd));
    edit.addItem(&NSMenuItem::separatorItem(mtm));
    edit.addItem(&item("Find…", sel!(tamkaFind:), "f", cmd));
    let window = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Window"));
    window.addItem(&item("Close", sel!(performClose:), "w", cmd));
    for (title, sub) in [("Tamka", NSMenu::new(mtm)), ("Edit", edit), ("Window", window)] {
        let top = item(title, sel!(submenuAction:), "", NSEventModifierFlags::empty());
        top.setSubmenu(Some(&sub));
        main.addItem(&top);
    }
    ui::app(mtm).setMainMenu(Some(&main));
}

/// Called from `applicationDidFinishLaunching:`.
pub fn launched(mtm: MainThreadMarker) {
    let s = settings::get();
    install_main_menu(mtm);
    build_status_item(mtm);
    if let Err(e) = apply_shortcut() {
        hud::notice("Tamka's shortcut isn't available", Some(&e));
    }
    // Load the speech model if one is downloaded; otherwise open Settings › Models.
    models::refresh_async(|| {
        if models::is_downloaded(&settings::get().engine) == Some(true) {
            engine::start_async();
        } else {
            util::log("no speech model downloaded; opening Models");
            settings_window::show(Some(settings_window::TAB_MODELS));
        }
    });
    if s.auto_paste && !permissions::accessibility_ok() {
        permissions::ask_accessibility();
    }
    util::log(format!("launched, engine '{}', shortcut {}", s.engine, s.shortcut.label()));
}

/// (Re)register the dictation shortcut from settings.
pub fn apply_shortcut() -> Result<(), String> {
    let sc = settings::get().shortcut;
    hotkey::install(sc).inspect_err(|e| util::log(e))
}

// ---------------------------------------------------------------- menu bar

fn menu_item(mtm: MainThreadMarker, title: &str, tag: isize) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), Some(sel!(command:)), &NSString::new())
    };
    let target = ui::controller(mtm);
    unsafe { item.setTarget(Some(&target)) };
    item.setTag(tag);
    item
}

fn set_key_equivalent(item: &NSMenuItem, sc: crate::keys::Shortcut) {
    let Some(c) = sc.menu_char() else { return };
    item.setKeyEquivalent(&ns(&c));
    let mut m = NSEventModifierFlags::empty();
    if sc.mods & crate::keys::SHIFT != 0 { m |= NSEventModifierFlags::Shift; }
    if sc.mods & crate::keys::CONTROL != 0 { m |= NSEventModifierFlags::Control; }
    if sc.mods & crate::keys::OPTION != 0 { m |= NSEventModifierFlags::Option; }
    if sc.mods & crate::keys::COMMAND != 0 { m |= NSEventModifierFlags::Command; }
    item.setKeyEquivalentModifierMask(m);
}

fn build_status_item(mtm: MainThreadMarker) {
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu.setDelegate(Some(&ui::menu_delegate(mtm)));
    item.setMenu(Some(&menu));
    SHELL.with(|sh| *sh.borrow_mut() = Some(Shell { item, menu }));
    refresh_icon();
    menu_will_open();
}

/// The menu-bar icon while recording: the waveform in recording red.
fn red_waveform() -> Option<Retained<objc2_app_kit::NSImage>> {
    let img = ui::symbol("waveform", 0.0)?;
    let colors = NSArray::from_retained_slice(&[NSColor::systemRedColor()]);
    let cfg = NSImageSymbolConfiguration::configurationWithPaletteColors(&colors);
    Some(img.imageWithSymbolConfiguration(&cfg).unwrap_or(img))
}

/// Menu-bar icon for the current phase: waveform, red waveform, or ellipsis.
fn refresh_icon() {
    let mtm = mtm();
    let phase = PHASE.with(Cell::get);
    SHELL.with(|sh| {
        let sh = sh.borrow();
        let Some(sh) = sh.as_ref() else { return };
        let Some(button) = sh.item.button(mtm) else { return };
        // Tamka's own glyph (waveform + caret, from the logo) as a template image:
        // tinted red while listening, dimmed while transcribing. Falls back to the
        // SF Symbol when run outside the app bundle.
        if let Some(glyph) = objc2_app_kit::NSImage::imageNamed(&ns("MenuIcon")) {
            glyph.setTemplate(true);
            button.setImage(Some(&glyph));
            button.setContentTintColor(if phase == Phase::Recording { Some(NSColor::systemRedColor()) } else { None }.as_deref());
            button.setAppearsDisabled(phase == Phase::Transcribing);
        } else {
            let img = match phase {
                Phase::Recording => red_waveform().inspect(|i| i.setTemplate(false)),
                Phase::Transcribing => ui::symbol("ellipsis", 0.0).inspect(|i| i.setTemplate(true)),
                Phase::Idle => ui::symbol("waveform", 0.0).inspect(|i| i.setTemplate(true)),
            };
            button.setImage(img.as_deref());
        }
        let tip = match phase {
            Phase::Idle => format!("Tamka · {} to dictate", settings::get().shortcut.label()),
            Phase::Recording => "Tamka · listening".to_string(),
            Phase::Transcribing => "Tamka · transcribing".to_string(),
        };
        button.setToolTip(Some(&ns(&tip)));
    });
}

/// "Just now", "4 min ago", "2 h ago", "Yesterday".
fn ago(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..60 => "Just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => "Yesterday or earlier".into(),
    }
}

/// One line, at most `max` characters, cut at a word boundary.
fn snippet(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let cut: String = one_line.chars().take(max).collect();
    let at_word = cut.rfind(' ').filter(|&i| i > max / 2).map_or(cut.as_str(), |i| &cut[..i]);
    format!("{}…", at_word.trim_end_matches([',', '.', ';', ':', ' ']))
}

/// Rebuilt every time the menu opens, so it always shows the current state.
pub fn menu_will_open() {
    let mtm = mtm();
    let s = settings::get();
    let phase = PHASE.with(Cell::get);
    let Some(menu) = SHELL.with(|sh| sh.borrow().as_ref().map(|sh| sh.menu.clone())) else { return };
    menu.removeAllItems();

    // The stop item names what will actually happen to the text.
    let will_paste = s.auto_paste && permissions::accessibility_ok();
    let (title, enabled) = match phase {
        Phase::Idle => ("Start Dictation", true),
        Phase::Recording if will_paste => ("Stop and Paste", true),
        Phase::Recording => ("Stop and Copy", true),
        Phase::Transcribing => ("Transcribing…", false),
    };
    let dictate = menu_item(mtm, title, tag::TOGGLE);
    dictate.setEnabled(enabled);
    set_key_equivalent(&dictate, s.shortcut);
    menu.addItem(&dictate);
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    // Recent: one click copies.
    menu.addItem(&NSMenuItem::sectionHeaderWithTitle(&ns("Recent"), mtm));
    let history = HISTORY.with(|h| h.borrow().clone());
    if history.is_empty() {
        let empty = menu_item(mtm, &format!("Press {} to dictate", s.shortcut.label()), 0);
        empty.setSubtitle(Some(&ns("Your last five dictations appear here")));
        empty.setEnabled(false);
        menu.addItem(&empty);
    }
    for (i, (text, when)) in history.iter().take(RECENT_SHOWN).enumerate() {
        let item = menu_item(mtm, &snippet(text, 44), tag::RECENT + i as isize);
        let words = text.split_whitespace().count();
        item.setSubtitle(Some(&ns(&format!("{} · {words} word{}", ago(*when), if words == 1 { "" } else { "s" }))));
        item.setToolTip(Some(&ns(&format!("Click to copy:\n{}", snippet(text, 400)))));
        menu.addItem(&item);
    }
    if util::cache_dir().join("last.wav").exists() {
        let retry = menu_item(mtm, "Transcribe Last Recording Again", tag::RETRY);
        retry.setEnabled(phase == Phase::Idle);
        menu.addItem(&retry);
    }

    // The speech model row. When the engine has failed it joins the problems
    // below (one problem, one row); otherwise it sits above Settings.
    let status = engine::status();
    let name = models::name(&s.engine);
    let ready = if s.engine == "apple" { "Ready" } else { "Ready on the Neural Engine" };
    let status_text = match &status {
        engine::Status::Ready => ready.to_string(),
        engine::Status::Loading => "Loading the model…".to_string(),
        engine::Status::Stopped => "Starts when you dictate".to_string(),
        engine::Status::NotDownloaded => "Not downloaded yet".to_string(),
        engine::Status::Failed(e) => e.clone(),
    };
    let engine_item = menu_item(mtm, name, tag::ENGINE);
    engine_item.setSubtitle(Some(&ns(&status_text)));
    engine_item.setToolTip(Some(&ns("Speech model settings")));
    let engine_failed = matches!(status, engine::Status::Failed(_) | engine::Status::NotDownloaded);
    if matches!(status, engine::Status::NotDownloaded) {
        engine_item.setTitle(&ns("Download a Speech Model…"));
        engine_item.setSubtitle(Some(&ns(&format!("{} is recommended", models::recommended().name))));
    }

    // Problems, only while they exist. Each opens the place that fixes it.
    let mut problems: Vec<Retained<NSMenuItem>> = Vec::new();
    if engine_failed {
        problems.push(engine_item.clone());
    }
    if permissions::mic() == Mic::Denied {
        let item = menu_item(mtm, "Allow Microphone Access…", tag::ALLOW_MIC);
        item.setSubtitle(Some(&ns("Tamka can't hear you")));
        problems.push(item);
    }
    if s.auto_paste && !permissions::accessibility_ok() {
        let item = menu_item(mtm, "Allow Tamka to Paste…", tag::ALLOW_PASTE);
        item.setSubtitle(Some(&ns("Until then, text is only copied")));
        problems.push(item);
    }
    if !problems.is_empty() {
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        for item in problems {
            item.setImage(ui::state_symbol("exclamationmark.triangle.fill", 0.0, NSColor::systemOrangeColor()).as_deref());
            menu.addItem(&item);
        }
    }

    // Model status (no icon, so every row shares one left edge), then Settings and Quit.
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    if !engine_failed {
        menu.addItem(&engine_item);
    }
    let settings_item = menu_item(mtm, "Settings…", tag::SETTINGS);
    settings_item.setKeyEquivalent(&ns(","));
    menu.addItem(&settings_item);
    let quit = menu_item(mtm, "Quit Tamka", tag::QUIT);
    quit.setKeyEquivalent(&ns("q"));
    menu.addItem(&quit);
}

/// `tamka://toggle` (same as the shortcut), `tamka://cancel`, `tamka://retry`,
/// `tamka://settings[-models|-text]`, and `tamka://hud-demo`, which cycles the
/// indicator's states for a few seconds.
pub fn open_url(host: &str) {
    match host {
        "toggle" => toggle(),
        "cancel" => cancel(),
        "retry" => retry_last(),
        "settings" => settings_window::show(Some(settings_window::TAB_GENERAL)),
        "settings-models" => settings_window::show(Some(settings_window::TAB_MODELS)),
        "settings-text" => settings_window::show(Some(settings_window::TAB_TEXT)),
        // Opens the menu for ~4 s with two clearly fake transcripts, for screenshots.
        "menu-demo" => {
            let demo = HISTORY.with(|h| h.borrow().is_empty());
            if demo {
                let now = SystemTime::now();
                HISTORY.with(|h| {
                    *h.borrow_mut() = vec![
                        ("(Demo) Ask Sam to check the Zapier webhook, then open the support ticket.".into(), now),
                        ("(Demo) Dear Sam, thanks for the update on the invoice.".into(), now - std::time::Duration::from_secs(240)),
                    ]
                });
            }
            let button = SHELL.with(|sh| sh.borrow().as_ref().and_then(|sh| sh.item.button(mtm())));
            if let Some(button) = button {
                util::main_after(4.0, || {
                    if let Some(m) = SHELL.with(|sh| sh.borrow().as_ref().map(|sh| sh.menu.clone())) {
                        m.cancelTracking();
                    }
                });
                // Exactly what a click on the menu-bar icon does.
                unsafe { button.performClick(None) };
            }
            if demo {
                HISTORY.with(|h| h.borrow_mut().clear());
            }
        }
        "hud-demo" if PHASE.with(Cell::get) == Phase::Idle => {
            hud::show_recording(&settings::get().shortcut.label());
            hud::update_recording(std::time::Duration::from_secs(607), 0.8);
            util::main_after(2.5, hud::show_transcribing);
            util::main_after(5.0, || {
                hud::notice("Copied · press ⌘V to paste", Some("Allow Tamka in Privacy & Security › Accessibility to paste automatically."))
            });
            util::main_after(9.5, || hud::toast("Copied", None));
        }
        _ => {}
    }
}

pub fn handle_tag(t: isize) {
    match t {
        tag::TOGGLE => toggle(),
        tag::RETRY => retry_last(),
        tag::SETTINGS => settings_window::show(None),
        tag::ENGINE => settings_window::show(Some(settings_window::TAB_MODELS)),
        tag::ALLOW_PASTE => {
            permissions::ask_accessibility();
            permissions::open_settings("Privacy_Accessibility");
        }
        tag::ALLOW_MIC => permissions::open_settings("Privacy_Microphone"),
        tag::QUIT => {
            if PHASE.with(Cell::get) == Phase::Recording {
                audio::cancel();
            }
            ui::app(mtm()).terminate(None);
        }
        t if t >= tag::RECENT => {
            let i = (t - tag::RECENT) as usize;
            if let Some((text, _)) = HISTORY.with(|h| h.borrow().get(i).cloned()) {
                let mut s = settings::get();
                s.auto_paste = false;
                s.keep_on_clipboard = true;
                paste::deliver(&text, &s);
                hud::toast("Copied", None);
            }
        }
        _ => {}
    }
}

pub fn login_item_enabled() -> bool {
    let status = unsafe { SMAppService::mainAppService().status() };
    status == SMAppServiceStatus::Enabled
}

pub fn set_login_item(on: bool) -> Result<(), String> {
    let service = unsafe { SMAppService::mainAppService() };
    let result = if on { unsafe { service.registerAndReturnError() } } else { unsafe { service.unregisterAndReturnError() } };
    result.map_err(|e| e.localizedDescription().to_string())
}

// ---------------------------------------------------------------- dictation

fn set_phase(p: Phase) {
    PHASE.with(|c| c.set(p));
    refresh_icon();
}

fn sound(name: &str) {
    if !settings::get().sounds {
        return;
    }
    if let Some(s) = NSSound::soundNamed(&ns(name)) {
        s.setVolume(0.25);
        s.play();
    }
}

/// The shortcut: start, or finish and paste.
pub fn toggle() {
    match PHASE.with(Cell::get) {
        Phase::Idle => start(),
        Phase::Recording => finish(),
        Phase::Transcribing => hud::toast("Still transcribing the last one…", None),
    }
}

/// Esc while recording: throw the recording away.
pub fn cancel() {
    if PHASE.with(Cell::get) != Phase::Recording {
        return;
    }
    stop_timer();
    hotkey::set_escape(false);
    audio::cancel();
    set_phase(Phase::Idle);
    hud::toast("Discarded", None);
    util::release_memory_soon();
}

fn start() {
    if models::is_downloaded(&settings::get().engine) == Some(false) {
        hud::notice("Download a speech model first", Some("Pick one in Settings › Models. It takes a minute, once."));
        settings_window::show(Some(settings_window::TAB_MODELS));
        return;
    }
    match permissions::mic() {
        Mic::Granted => {}
        Mic::NotAsked => {
            permissions::request_mic(|granted| {
                if granted {
                    start();
                }
            });
            return;
        }
        Mic::Denied => {
            hud::notice(
                "Microphone access is off for Tamka",
                Some("Turn it on in System Settings › Privacy & Security › Microphone."),
            );
            permissions::open_settings("Privacy_Microphone");
            return;
        }
    }
    sound("Tink");
    match audio::start() {
        Ok(()) => {
            let s = settings::get();
            MAX_SECONDS.with(|m| m.set(s.max_seconds.max(5) as u64));
            set_phase(Phase::Recording);
            hotkey::set_escape(true);
            hud::show_recording(&s.shortcut.label());
            start_timer();
            util::log("recording");
        }
        Err(e) => {
            util::log(format!("couldn't start recording: {e}"));
            hud::notice("Couldn't start recording", Some(&e));
        }
    }
}

fn finish() {
    stop_timer();
    hotkey::set_escape(false);
    let Some(rec) = audio::stop() else {
        set_phase(Phase::Idle);
        return;
    };
    sound("Pop");
    if rec.seconds() < MIN_SECONDS {
        set_phase(Phase::Idle);
        hud::notice("Too short to transcribe", None);
        return;
    }
    set_phase(Phase::Transcribing);
    hud::show_transcribing();
    let path = util::cache_dir().join("last.wav");
    std::thread::spawn(move || {
        let result = audio::write_wav(&path, &rec)
            .map_err(|e| format!("Couldn't save the recording: {e}"))
            .and_then(|()| {
                drop(rec);
                engine::transcribe(&path)
            });
        util::main_async(move || finished(result));
    });
}

fn retry_last() {
    if PHASE.with(Cell::get) != Phase::Idle {
        return;
    }
    let path = util::cache_dir().join("last.wav");
    if !path.exists() {
        return;
    }
    set_phase(Phase::Transcribing);
    hud::show_transcribing();
    std::thread::spawn(move || {
        let result = engine::transcribe(&path);
        util::main_async(move || finished(result));
    });
}

fn finished(result: Result<String, String>) {
    set_phase(Phase::Idle);
    hud::hide();
    match result {
        Ok(raw) => {
            let s = settings::get();
            let opts = text::Options { remove_fillers: s.remove_fillers, collapse_repeats: s.collapse_repeats };
            let out = text::process(&raw, &s.find_replace, &opts);
            if out.is_empty() {
                hud::notice("No speech heard", None);
            } else {
                match paste::deliver(&out, &s) {
                    paste::Outcome::Pasted => {}
                    paste::Outcome::Copied if !s.auto_paste => hud::toast("Copied", None),
                    paste::Outcome::Copied => hud::notice(
                        "Copied · press ⌘V to paste",
                        Some("Allow Tamka in Privacy & Security › Accessibility to paste automatically."),
                    ),
                }
                HISTORY.with(|h| {
                    let mut h = h.borrow_mut();
                    h.insert(0, (out, SystemTime::now()));
                    h.truncate(RECENT_SHOWN);
                });
            }
        }
        Err(e) => {
            util::log(format!("transcription failed: {e}"));
            hud::notice("Couldn't transcribe", Some(&e));
        }
    }
    util::release_memory_soon();
}

fn start_timer() {
    let mtm = mtm();
    let target = ui::controller(mtm);
    let timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(0.1, &target, sel!(tick:), None, true) };
    TIMER.with(|t| {
        if let Some(old) = t.borrow_mut().replace(timer) {
            old.invalidate();
        }
    });
}

fn stop_timer() {
    TIMER.with(|t| {
        if let Some(t) = t.borrow_mut().take() {
            t.invalidate();
        }
    });
}

/// Recording clock: refresh the pill and stop at the time limit.
pub fn tick() {
    if PHASE.with(Cell::get) != Phase::Recording {
        return;
    }
    let elapsed = audio::elapsed();
    hud::update_recording(elapsed, audio::level());
    if elapsed.as_secs() >= MAX_SECONDS.with(Cell::get) {
        finish();
    }
}
