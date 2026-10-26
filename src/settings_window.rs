//! The Settings window: General, Models and Text tabs, in Photon's form style.
//! Every control saves the moment it changes; there is no Save button.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSBackingStoreType, NSBezelStyle, NSBorderType, NSButton,
    NSColor, NSControl, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate, NSEvent,
    NSBox, NSBoxType, NSControlSize, NSEventMask, NSFont, NSFontWeightMedium, NSFontWeightSemibold, NSImage,
    NSImageView, NSPopUpButton, NSProgressIndicator, NSProgressIndicatorStyle,
    NSScrollView, NSSearchField, NSSegmentSwitchTracking, NSSegmentStyle, NSSegmentedControl, NSTabView,
    NSTabViewItem, NSTableColumn, NSTableColumnResizingOptions, NSTableView, NSTableViewDataSource,
    NSTableViewDelegate, NSTableViewStyle, NSTabViewDelegate, NSTextAlignment, NSTextField, NSTextFieldDelegate, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSTimer, NSURL,
};

use crate::keys::{self, vk, Shortcut};
use crate::permissions::{self, Mic};
use crate::models::{self, CATALOG};
use crate::settings::{self, Rule};
use crate::util::{self, ns};
use crate::{app, engine, hotkey, ui};

// Control ids (the `tag` of each control).
const KEY: isize = 1;
const AUTO_PASTE: isize = 2;
const KEEP_CLIP: isize = 3;
const LOGIN: isize = 5;
const MIC_FIX: isize = 6;
const AX_FIX: isize = 7;
const OPEN_LOG: isize = 8;
const RESET_INDICATOR: isize = 9;
const FILLERS: isize = 21;
const REPEATS: isize = 22;
const DIGITS: isize = 23;
const LIMIT: isize = 24;
const START_SOUND: isize = 25;
const STOP_SOUND: isize = 26;
const ADD_REMOVE: isize = 30;
const RESTORE: isize = 31;
/// Model rows: MODEL_USE + index (radio), MODEL_ACTION + index (Download / Remove / Cancel).
const MODEL_USE: isize = 100;
const MODEL_ACTION: isize = 200;
/// Table cells: CELL + row * 2 + column.
const CELL: isize = 10_000;

pub const TAB_GENERAL: isize = 0;
pub const TAB_MODELS: isize = 1;
pub const TAB_TEXT: isize = 2;

/// Sound choices: "None", then the macOS system sounds. Tink is left out because it is
/// the default alert sound, so it sounds like an error.
const SOUND_NAMES: &[&str] = &[
    "None", "Purr", "Pop", "Bottle", "Morse", "Frog", "Blow", "Glass", "Ping", "Submarine", "Hero", "Funk", "Basso",
    "Sosumi",
];

fn sound_index(name: &str) -> usize {
    if name.is_empty() {
        return 0;
    }
    SOUND_NAMES.iter().position(|n| *n == name).unwrap_or(0)
}

fn sound_name(i: usize) -> String {
    if i == 0 { String::new() } else { SOUND_NAMES.get(i).map_or(String::new(), |n| n.to_string()) }
}

const LIMITS: &[(u32, &str)] = &[
    (60, "1 minute"),
    (120, "2 minutes"),
    (300, "5 minutes"),
    (600, "10 minutes"),
    (1200, "20 minutes"),
    (1800, "30 minutes"),
];

const LABEL_W: f64 = 150.0;
const X: f64 = 24.0;
const WIN_W: f64 = 600.0;
const WIN_H: f64 = 740.0;

define_class!(
    // SAFETY: NSView subclass that only flips its coordinate system.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "TamkaFlippedView"]
    struct Flipped;

    impl Flipped {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

define_class!(
    // SAFETY: NSTableView subclass that only adds Delete-key handling; no ivars, no Drop.
    #[unsafe(super(NSTableView, NSControl, NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TamkaCommandsTable"]
    struct CommandsTable;

    impl CommandsTable {
        /// Delete or forward-delete removes the selected commands (undoable).
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let code = event.keyCode();
            if code == 51 || code == 117 {
                util::main_async(remove_selected);
            } else {
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }
    }
);

define_class!(
    // SAFETY: NSObject subclass; target of the window's controls, its delegate,
    // and the commands table's data source. No Drop impl.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "TamkaSettingsController"]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSWindowDelegate for Controller {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            util::main_async(closed);
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _n: &NSNotification) {
            refresh();
        }
    }

    unsafe impl NSControlTextEditingDelegate for Controller {
        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, n: &NSNotification) {
            if let Some(field) = n.object().and_then(|o| o.downcast::<NSTextField>().ok()) {
                cell_edited(&field);
            }
        }
    }

    unsafe impl NSTextFieldDelegate for Controller {}

    unsafe impl NSTableViewDataSource for Controller {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            STATE.with(|s| s.borrow().as_ref().map_or(0, |s| s.filtered.len() as NSInteger))
        }
    }

    unsafe impl NSTableViewDelegate for Controller {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(&self, _table: &NSTableView, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
            let col = column.map(|c| c.identifier().to_string()).unwrap_or_default();
            cell_view(self, if col == "replacement" { 1 } else { 0 }, row)
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, _n: &NSNotification) {
            update_remove_button();
        }
    }

    unsafe impl NSTabViewDelegate for Controller {
        #[unsafe(method(tabView:didSelectTabViewItem:))]
        fn did_select_tab(&self, tabs: &NSTabView, item: Option<&NSTabViewItem>) {
            if let Some(item) = item {
                LAST_TAB.with(|t| t.set(tabs.indexOfTabViewItem(item)));
            }
        }
    }

    impl Controller {
        /// ⌘F (from the hidden Edit menu): jump to the command search.
        #[unsafe(method(tamkaFind:))]
        fn find(&self, _sender: Option<&AnyObject>) {
            focus_search();
        }

        #[unsafe(method(control:))]
        fn control(&self, sender: Option<&AnyObject>) {
            let tag: isize = match sender {
                Some(s) => unsafe { msg_send![s, tag] },
                None => 0,
            };
            changed(tag);
        }

        #[unsafe(method(search:))]
        fn search(&self, sender: Option<&AnyObject>) {
            if let Some(field) = sender.and_then(|s| s.downcast_ref::<NSSearchField>()) {
                let q = field.stringValue().to_string();
                filter(&q);
            }
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _timer: Option<&AnyObject>) {
            refresh();
        }
    }
);

/// A status line: symbol + text + an optional fix button.
struct Status {
    icon: Retained<NSImageView>,
    text: Retained<NSTextField>,
    button: Option<Retained<NSButton>>,
    /// Text width when no button is showing.
    full_width: f64,
}

/// One model in the Models tab.
struct ModelRow {
    radio: Retained<NSButton>,
    tagline: Retained<NSTextField>,
    action: Retained<NSButton>,
    progress: Retained<NSProgressIndicator>,
}

struct State {
    window: Retained<NSWindow>,
    tabs: Retained<NSTabView>,
    /// Kept alive here: windows and tables only hold their delegates weakly.
    _controller: Retained<Controller>,
    controls: HashMap<isize, Retained<NSView>>,
    key_note: Retained<NSTextField>,
    mic: Status,
    ax: Status,
    engine: Status,
    rows: Vec<ModelRow>,
    storage_note: Retained<NSTextField>,
    table: Retained<NSTableView>,
    search: Retained<NSSearchField>,
    count: Retained<NSTextField>,
    /// Indexes into settings.find_replace for the rows on screen.
    filtered: Vec<usize>,
    query: String,
    key_monitor: Option<Retained<AnyObject>>,
    timer: Retained<NSTimer>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// The tab Settings reopens on (remembered while Tamka runs).
    static LAST_TAB: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("Settings must run on the main thread")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn as_view<T: objc2::Message>(v: &Retained<T>) -> Retained<NSView> {
    // SAFETY: only called with NSView subclasses.
    unsafe { Retained::cast_unchecked::<NSView>(v.clone()) }
}

// ---------------------------------------------------------------- form layout

/// Top-down layout inside a flipped container (Photon's form: 150pt right-aligned
/// labels, 10pt gutter, 8pt between rows, semibold section titles).
struct Form {
    view: Retained<Flipped>,
    y: f64,
    width: f64,
}

impl Form {
    fn new(width: f64, height: f64) -> Form {
        let this = Flipped::alloc(mtm()).set_ivars(());
        let view: Retained<Flipped> = unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, width, height)] };
        Form { view, y: 18.0, width }
    }

    fn cx(&self) -> f64 {
        X + LABEL_W + 10.0
    }

    fn label(&self, text: &str, y: f64, h: f64) {
        let l = NSTextField::labelWithString(&ns(text), mtm());
        l.setAlignment(NSTextAlignment::Right);
        l.setFrame(rect(X, y + (h - 17.0) / 2.0, LABEL_W, 17.0));
        self.view.addSubview(&l);
    }

    fn section(&mut self, title: &str) {
        if self.y > 20.0 {
            self.y += 10.0;
        }
        let l = NSTextField::labelWithString(&ns(title), mtm());
        l.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightSemibold })));
        l.setFrame(rect(X, self.y, self.width - 2.0 * X, 18.0));
        self.view.addSubview(&l);
        self.y += 26.0;
    }

    /// A labelled row. `w` is the control width (None = rest of the row).
    fn row(&mut self, label: &str, control: &NSView, w: Option<f64>, h: f64) {
        self.label(label, self.y, h);
        let cw = w.unwrap_or(self.width - self.cx() - X);
        control.setFrame(rect(self.cx(), self.y, cw, h));
        self.view.addSubview(control);
        self.y += h + 8.0;
    }

    /// A control without a label, in the control column.
    fn plain(&mut self, control: &NSView, h: f64) {
        control.setFrame(rect(self.cx(), self.y, self.width - self.cx() - X, h));
        self.view.addSubview(control);
        self.y += h + 6.0;
    }

    /// A secondary-colour 11pt note under the control above it.
    fn note(&mut self, text: &str) -> Retained<NSTextField> {
        let l = NSTextField::wrappingLabelWithString(&ns(text), mtm());
        l.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let w = self.width - self.cx() - X;
        l.setFrame(rect(self.cx(), self.y - 4.0, w, 30.0));
        self.view.addSubview(&l);
        self.y += 26.0;
        l
    }

    /// Symbol + text + optional fix button, in the control column.
    fn status(&mut self, label: &str, controller: &Controller, fix: Option<(&str, isize)>) -> Status {
        let h = 22.0;
        self.label(label, self.y, h);
        let icon = NSImageView::new(mtm());
        icon.setFrame(rect(self.cx(), self.y + 3.0, 16.0, 16.0));
        self.view.addSubview(&icon);
        let text = NSTextField::labelWithString(&ns(""), mtm());
        text.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
        let button = fix.map(|(title, tag)| {
            let b = push_button(title, tag, controller);
            b.setFrame(rect(self.width - X - 120.0, self.y - 2.0, 120.0, 26.0));
            self.view.addSubview(&b);
            b
        });
        let full_width = self.width - self.cx() - 22.0 - X;
        text.setFrame(rect(self.cx() + 22.0, self.y + 2.0, full_width, 18.0));
        self.view.addSubview(&text);
        self.y += h + 8.0;
        Status { icon, text, button, full_width }
    }
}

fn target(c: &NSControl, tag: isize, controller: &Controller) {
    unsafe {
        c.setTarget(Some(controller));
        c.setAction(Some(sel!(control:)));
    }
    c.setTag(tag);
}

fn checkbox(title: &str, tag: isize, on: bool, controller: &Controller) -> Retained<NSButton> {
    let b = unsafe { NSButton::checkboxWithTitle_target_action(&ns(title), Some(controller), Some(sel!(control:)), mtm()) };
    b.setTag(tag);
    b.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    b
}

fn push_button(title: &str, tag: isize, controller: &Controller) -> Retained<NSButton> {
    let b = unsafe { NSButton::buttonWithTitle_target_action(&ns(title), Some(controller), Some(sel!(control:)), mtm()) };
    b.setBezelStyle(NSBezelStyle::Push);
    b.setTag(tag);
    b
}

fn popup(items: &[&str], selected: usize, tag: isize, controller: &Controller) -> Retained<NSPopUpButton> {
    let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm()), rect(0.0, 0.0, 200.0, 25.0), false);
    for i in items {
        p.addItemWithTitle(&ns(i));
    }
    p.selectItemAtIndex(selected.min(items.len().saturating_sub(1)) as isize);
    target(&p, tag, controller);
    p
}

fn symbol(name: &str, color: Retained<NSColor>) -> Option<Retained<NSImage>> {
    ui::state_symbol(name, 13.0, color)
}

#[derive(Clone, Copy)]
enum Tone {
    Good,
    Attention,
    Busy,
}

fn set_status(st: &Status, tone: Tone, text: &str, show_button: bool) {
    let img = match tone {
        Tone::Good => symbol("checkmark.circle.fill", NSColor::systemGreenColor()),
        Tone::Attention => symbol("exclamationmark.triangle.fill", NSColor::systemOrangeColor()),
        Tone::Busy => symbol("circle.dotted", NSColor::secondaryLabelColor()),
    };
    st.icon.setImage(img.as_deref());
    st.text.setStringValue(&ns(text));
    // The text gets the button's room whenever the button is hidden.
    let mut frame = st.text.frame();
    frame.size.width = st.full_width - if show_button && st.button.is_some() { 128.0 } else { 0.0 };
    st.text.setFrame(frame);
    if let Some(b) = &st.button {
        b.setHidden(!show_button);
    }
}

// ---------------------------------------------------------------- window

/// Open Settings (or bring it forward) on the given tab.
pub fn show(tab: Option<isize>) {
    let mtm = mtm();
    if let Some((w, tabs)) = STATE.with(|s| s.borrow().as_ref().map(|s| (s.window.clone(), s.tabs.clone()))) {
        if let Some(t) = tab {
            tabs.selectTabViewItemAtIndex(t);
        }
        ui::app(mtm).activate();
        w.makeKeyAndOrderFront(None);
        return;
    }
    let s = settings::get();
    let controller: Retained<Controller> = {
        let this = Controller::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    };
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, WIN_W, WIN_H),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&ns("Tamka Settings"));
    let tabs = NSTabView::initWithFrame(NSTabView::alloc(mtm), rect(0.0, 0.0, WIN_W, WIN_H));
    let (inner_w, inner_h) = (WIN_W - 20.0, WIN_H - 40.0);
    let mut controls: HashMap<isize, Retained<NSView>> = HashMap::new();

    // General.
    let mut g = Form::new(inner_w, inner_h);
    g.section("Shortcut");
    let key = push_button(&s.shortcut.label(), KEY, &controller);
    key.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium })));
    key.setToolTip(Some(&ns("Click, then press the new shortcut")));
    g.row("Start and stop:", &key, Some(140.0), 28.0);
    controls.insert(KEY, as_view(&key));
    let key_note = g.note("Click it, then press the new keys. It needs ⌘, ⌃ or ⌥. While you're dictating, Esc discards the recording.");
    g.section("Recording");
    let labels: Vec<&str> = LIMITS.iter().map(|l| l.1).collect();
    let limit_i = LIMITS.iter().position(|l| l.0 >= s.max_seconds).unwrap_or(LIMITS.len() - 1);
    let limit = popup(&labels, limit_i, LIMIT, &controller);
    g.row("Stop after:", &limit, Some(140.0), 25.0);
    controls.insert(LIMIT, as_view(&limit));
    g.note("Listening stops and the text is typed when a dictation reaches this length.");
    for (label, tag, name) in [("Start sound:", START_SOUND, &s.start_sound), ("Stop sound:", STOP_SOUND, &s.stop_sound)] {
        let p = popup(SOUND_NAMES, sound_index(name), tag, &controller);
        g.row(label, &p, Some(140.0), 25.0);
        controls.insert(tag, as_view(&p));
    }
    g.note("Pick a sound to hear it. None keeps that moment silent.");
    let reset = push_button("Reset Position", RESET_INDICATOR, &controller);
    reset.setToolTip(Some(&ns("Put the listening indicator back at the bottom centre of the screen")));
    g.row("Indicator:", &reset, Some(140.0), 26.0);
    controls.insert(RESET_INDICATOR, as_view(&reset));
    g.note("Drag the listening indicator anywhere; it stays there for your next dictation.");
    g.section("After dictating");
    for (title, tag, on) in [
        ("Paste into the app you're typing in", AUTO_PASTE, s.auto_paste),
        ("Leave the text on the clipboard", KEEP_CLIP, s.keep_on_clipboard),
    ] {
        let c = checkbox(title, tag, on, &controller);
        g.plain(&c, 20.0);
        controls.insert(tag, as_view(&c));
    }
    g.section("Startup");
    let login = checkbox("Launch Tamka at login", LOGIN, app::login_item_enabled(), &controller);
    g.plain(&login, 20.0);
    controls.insert(LOGIN, as_view(&login));
    g.section("Permissions");
    let mic = g.status("Microphone:", &controller, Some(("Open Settings", MIC_FIX)));
    let ax = g.status("Accessibility:", &controller, Some(("Allow…", AX_FIX)));
    g.section("Troubleshooting");
    let log = push_button("Open Log", OPEN_LOG, &controller);
    g.row("Activity log:", &log, Some(120.0), 26.0);
    g.note("Events and timings only. Tamka never logs what you said.");

    // Models.
    let md = Form::new(inner_w, inner_h);
    let intro = NSTextField::wrappingLabelWithString(
        &ns("Speech models download once, then run entirely on your Mac. Start with the recommended one; add or remove others any time."),
        mtm,
    );
    intro.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    intro.setTextColor(Some(&NSColor::secondaryLabelColor()));
    intro.setFrame(rect(X, 16.0, inner_w - 2.0 * X, 32.0));
    md.view.addSubview(&intro);
    let (rows, rows_end) = model_rows(&md, &controller, 60.0, &mut controls);
    let mut md = md;
    md.y = rows_end + 14.0;
    let engine = md.status("In use:", &controller, None);
    md.y += 2.0;
    let legend = NSTextField::wrappingLabelWithString(
        &ns("Grades come from tests on 117 public recordings. Size: download and memory. Speed: how fast text appears after you stop. Accuracy: how few words it gets wrong. Point at a grade for its numbers."),
        mtm,
    );
    legend.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    legend.setTextColor(Some(&NSColor::secondaryLabelColor()));
    legend.setFrame(rect(X, md.y, inner_w - 2.0 * X, 44.0));
    md.view.addSubview(&legend);
    md.y += 50.0;
    let storage_note = NSTextField::wrappingLabelWithString(&ns(""), mtm);
    storage_note.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    storage_note.setTextColor(Some(&NSColor::secondaryLabelColor()));
    storage_note.setFrame(rect(X, md.y, inner_w - 2.0 * X, 30.0));
    md.view.addSubview(&storage_note);

    // Text: cleanup, then spoken commands.
    let mut cmd = Form::new(inner_w, inner_h);
    cmd.section("Cleanup");
    for (title, tag, on) in [
        ("Remove filler words (um, uh)", FILLERS, s.remove_fillers),
        ("Collapse stutters (“the the” → “the”)", REPEATS, s.collapse_repeats),
        ("Write numbers as digits", DIGITS, s.numbers_as_digits),
    ] {
        let c = checkbox(title, tag, on, &controller);
        cmd.plain(&c, 20.0);
        controls.insert(tag, as_view(&c));
    }
    cmd.note("Experimental: digits help with money and dates but can turn “one of the” into “1 of the”.");
    cmd.section("Spoken commands");
    let top = cmd.y - 6.0;
    let intro = NSTextField::wrappingLabelWithString(
        &ns("Say a phrase while dictating and Tamka types its replacement. The longest matching phrase wins; type \\n for a line break."),
        mtm,
    );
    intro.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    intro.setTextColor(Some(&NSColor::secondaryLabelColor()));
    intro.setFrame(rect(X, top, inner_w - 2.0 * X, 30.0));
    cmd.view.addSubview(&intro);
    let search = NSSearchField::new(mtm);
    search.setFrame(rect(X, top + 36.0, inner_w - 2.0 * X, 24.0));
    search.setPlaceholderString(Some(&ns("Search commands")));
    unsafe {
        search.setTarget(Some(&controller));
        search.setAction(Some(sel!(search:)));
    }
    cmd.view.addSubview(&search);

    let table_y = top + 68.0;
    let bar_h = 24.0;
    let table_h = inner_h - table_y - bar_h - 26.0;
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(X, table_y, inner_w - 2.0 * X, table_h));
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(NSBorderType::BezelBorder);
    let table: Retained<NSTableView> = {
        let this = CommandsTable::alloc(mtm).set_ivars(());
        let t: Retained<CommandsTable> =
            unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, inner_w - 2.0 * X, table_h)] };
        Retained::into_super(t)
    };
    table.setStyle(NSTableViewStyle::FullWidth);
    table.setUsesAlternatingRowBackgroundColors(true);
    table.setAllowsMultipleSelection(true);
    table.setRowHeight(22.0);
    table.setColumnAutoresizingStyle(objc2_app_kit::NSTableViewColumnAutoresizingStyle::LastColumnOnlyAutoresizingStyle);
    for (id, title, w) in [("spoken", "When you say", 260.0), ("replacement", "Tamka types", 230.0)] {
        let col = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
        col.setTitle(&ns(title));
        col.setWidth(w);
        col.setMinWidth(120.0);
        col.setResizingMask(NSTableColumnResizingOptions::AutoresizingMask | NSTableColumnResizingOptions::UserResizingMask);
        table.addTableColumn(&col);
    }
    unsafe {
        table.setDataSource(Some(ProtocolObject::from_ref(&*controller)));
        table.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    }
    scroll.setDocumentView(Some(&table));
    cmd.view.addSubview(&scroll);
    // Only now does the table know its real width (the scroll view's content area).
    let content_w = scroll.contentSize().width;
    if let Some(first) = table.tableColumns().firstObject() {
        first.setWidth((content_w * 0.55).floor());
    }
    table.sizeLastColumnToFit();

    let bar_y = table_y + table_h + 8.0;
    let images: Vec<Retained<NSImage>> = ["plus", "minus"].iter().filter_map(|n| ui::symbol(n, 0.0)).collect();
    let add_remove = unsafe {
        NSSegmentedControl::segmentedControlWithImages_trackingMode_target_action(
            &NSArray::from_retained_slice(&images),
            NSSegmentSwitchTracking::Momentary,
            Some(&controller),
            Some(sel!(control:)),
            mtm,
        )
    };
    add_remove.setSegmentStyle(NSSegmentStyle::SmallSquare);
    add_remove.setTag(ADD_REMOVE);
    add_remove.setFrame(rect(X, bar_y, 56.0, bar_h));
    add_remove.setToolTip(Some(&ns("Add a command, or remove the selected ones")));
    cmd.view.addSubview(&add_remove);
    controls.insert(ADD_REMOVE, as_view(&add_remove));
    let count = NSTextField::labelWithString(&ns(""), mtm);
    count.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(11.0, 0.0)));
    count.setTextColor(Some(&NSColor::secondaryLabelColor()));
    count.setFrame(rect(X + 66.0, bar_y + 4.0, 200.0, 16.0));
    cmd.view.addSubview(&count);
    let restore = push_button("Restore Defaults…", RESTORE, &controller);
    restore.setFrame(rect(inner_w - X - 150.0, bar_y - 2.0, 150.0, 28.0));
    cmd.view.addSubview(&restore);

    for (title, form) in [("General", &g), ("Models", &md), ("Text", &cmd)] {
        let item = unsafe { NSTabViewItem::initWithIdentifier(NSTabViewItem::alloc(), Some(&ns(title))) };
        item.setLabel(&ns(title));
        item.setView(Some(&form.view));
        tabs.addTabViewItem(&item);
    }
    window.setContentView(Some(&tabs));
    window.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    tabs.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    window.center();
    // Reopen where it was last left (restores the saved frame when there is one),
    // but always at today's size: an older, smaller saved frame must not win.
    window.setFrameAutosaveName(&ns("TamkaSettings"));
    let top = window.frame().origin.y + window.frame().size.height;
    window.setContentSize(NSSize::new(WIN_W, WIN_H));
    let f = window.frame();
    window.setFrameOrigin(NSPoint::new(f.origin.x, top - f.size.height));
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(1.0, &controller, sel!(tick:), None, true)
    };

    let filtered = (0..s.find_replace.len()).collect();
    STATE.with(|st| {
        *st.borrow_mut() = Some(State {
            window: window.clone(),
            tabs: tabs.clone(),
            _controller: controller,
            controls,
            key_note,
            mic,
            ax,
            engine,
            rows,
            storage_note,
            table,
            search,
            count,
            filtered,
            query: String::new(),
            key_monitor: None,
            timer,
        })
    });
    tabs.selectTabViewItemAtIndex(tab.unwrap_or_else(|| LAST_TAB.with(|t| t.get())));
    refresh();
    models::refresh_async(refresh_models);
    reload_table();
    update_remove_button();
    ui::app(mtm).activate();
    window.makeKeyAndOrderFront(None);
}

fn closed() {
    stop_key_recording();
    // Drop half-made commands (added but never given a phrase).
    let s = settings::get();
    if s.find_replace.iter().any(|r| r.spoken.trim().is_empty()) {
        settings::update(|s| s.find_replace.retain(|r| !r.spoken.trim().is_empty()));
    }
    if let Some(st) = STATE.with(|s| s.borrow_mut().take()) {
        st.timer.invalidate();
    }
    ui::app(mtm()).deactivate();
    util::release_memory_soon();
}

fn control<T: objc2::Message>(id: isize) -> Option<Retained<T>> {
    STATE.with(|s| {
        let s = s.borrow();
        let v = s.as_ref()?.controls.get(&id)?.clone();
        // SAFETY: each id is registered with the control type the caller asks for.
        Some(unsafe { Retained::cast_unchecked::<T>(v) })
    })
}

fn is_on(id: isize) -> bool {
    control::<NSButton>(id).is_some_and(|b| b.state() == NSControlStateValueOn)
}

fn selected(id: isize) -> usize {
    control::<NSPopUpButton>(id).map_or(0, |p| p.indexOfSelectedItem().max(0) as usize)
}

/// Bring statuses, notes and enabled states up to date.
fn refresh() {
    let s = settings::get();
    STATE.with(|st| {
        let st = st.borrow();
        let Some(st) = st.as_ref() else { return };
        match permissions::mic() {
            Mic::Granted => set_status(&st.mic, Tone::Good, "Allowed", false),
            Mic::NotAsked => set_status(&st.mic, Tone::Busy, "Not asked yet; macOS asks when you first dictate", false),
            Mic::Denied => set_status(&st.mic, Tone::Attention, "Off. Tamka can't hear you", true),
        }
        if permissions::accessibility_ok() {
            set_status(&st.ax, Tone::Good, "Allowed", false);
        } else if s.auto_paste {
            set_status(&st.ax, Tone::Attention, "Needed to paste for you", true);
        } else {
            set_status(&st.ax, Tone::Busy, "Not needed while pasting is off", true);
        }
        let name = models::name(&s.engine);
        let (tone, text) = match engine::status() {
            engine::Status::Ready => (Tone::Good, format!("{name} · ready")),
            engine::Status::Loading => (Tone::Busy, format!("{name} · loading…")),
            engine::Status::Stopped => (Tone::Busy, format!("{name} · starts when you dictate")),
            engine::Status::NotDownloaded => (
                Tone::Attention,
                format!("Nothing to use yet. Download {} above to start.", models::recommended().name),
            ),
            engine::Status::Failed(e) => (Tone::Attention, e),
        };
        set_status(&st.engine, tone, &text, false);
    });
    refresh_models();
    if let Some(c) = control::<NSButton>(KEEP_CLIP) {
        // Without auto-paste the text is only copied, so it always stays on the clipboard.
        c.setEnabled(s.auto_paste);
        if !s.auto_paste {
            c.setState(NSControlStateValueOn);
        } else {
            c.setState(if s.keep_on_clipboard { NSControlStateValueOn } else { NSControlStateValueOff });
        }
    }
    if let Some(c) = control::<NSButton>(RESET_INDICATOR) {
        c.setEnabled(s.indicator_anchor.is_some());
    }
    if let Some(c) = control::<NSButton>(LOGIN) {
        c.setState(if app::login_item_enabled() { NSControlStateValueOn } else { NSControlStateValueOff });
    }
}

/// A control changed: save it and apply it at once.
fn changed(id: isize) {
    match id {
        KEY => start_key_recording(),
        AUTO_PASTE => {
            let on = is_on(id);
            settings::update(|s| s.auto_paste = on);
            if on && !permissions::accessibility_ok() {
                permissions::ask_accessibility();
            }
        }
        KEEP_CLIP => {
            let on = is_on(id);
            settings::update(|s| s.keep_on_clipboard = on);
        }
        START_SOUND | STOP_SOUND => {
            let name = sound_name(selected(id));
            app::sound(&name);
            settings::update(|s| if id == START_SOUND { s.start_sound = name } else { s.stop_sound = name });
        }
        LOGIN => {
            if let Err(e) = app::set_login_item(is_on(id)) {
                crate::hud::notice("Couldn't change Launch at Login", Some(&e));
            }
        }
        MIC_FIX => permissions::open_settings("Privacy_Microphone"),
        AX_FIX => {
            permissions::ask_accessibility();
            permissions::open_settings("Privacy_Accessibility");
        }
        RESET_INDICATOR => crate::hud::reset_position(),
        OPEN_LOG => {
            let path = util::home().join("Library/Logs/Tamka.log");
            NSWorkspace::sharedWorkspace().openURL(&NSURL::fileURLWithPath(&ns(&path.to_string_lossy())));
        }
        x if (MODEL_USE..MODEL_USE + 50).contains(&x) => use_model((x - MODEL_USE) as usize),
        x if (MODEL_ACTION..MODEL_ACTION + 50).contains(&x) => model_action((x - MODEL_ACTION) as usize),
        FILLERS => {
            let on = is_on(id);
            settings::update(|s| s.remove_fillers = on);
        }
        REPEATS => {
            let on = is_on(id);
            settings::update(|s| s.collapse_repeats = on);
        }
        DIGITS => {
            let on = is_on(id);
            settings::update(|s| s.numbers_as_digits = on);
            engine::restart();
        }
        LIMIT => {
            let i = selected(id).min(LIMITS.len() - 1);
            settings::update(|s| s.max_seconds = LIMITS[i].0);
        }
        ADD_REMOVE => match control::<NSSegmentedControl>(id).map(|c| c.selectedSegment()) {
            Some(0) => add_command(),
            Some(1) => remove_selected(),
            _ => {}
        },
        RESTORE => restore_defaults(),
        _ => {}
    }
    refresh();
}

// ---------------------------------------------------------------- models tab

const ROW_H: f64 = 58.0;
const GRADE_W: f64 = 52.0;
const COL_SIZE: f64 = 292.0;
const COL_SPEED: f64 = 344.0;
const COL_ACCURACY: f64 = 396.0;
const ACTION_W: f64 = 104.0;

fn small_label(text: &str, size: f64, secondary: bool) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&ns(text), mtm());
    l.setFont(Some(&NSFont::systemFontOfSize(size)));
    if secondary {
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    l
}

fn separator(view: &NSView, y: f64, width: f64) {
    let b = NSBox::new(mtm());
    b.setBoxType(NSBoxType::Separator);
    b.setFrame(rect(X, y, width - 2.0 * X, 1.0));
    view.addSubview(&b);
}

/// Column headers and one row per model. Returns the rows and the y below them.
fn model_rows(
    form: &Form,
    controller: &Controller,
    y0: f64,
    controls: &mut HashMap<isize, Retained<NSView>>,
) -> (Vec<ModelRow>, f64) {
    let mtm = mtm();
    let view: &NSView = &form.view;
    let action_x = form.width - X - ACTION_W;
    for (title, x, w, centered) in [
        ("Model", X + 26.0, 200.0, false),
        ("Size", COL_SIZE, GRADE_W, true),
        ("Speed", COL_SPEED, GRADE_W, true),
        ("Accuracy", COL_ACCURACY, GRADE_W + 8.0, true),
    ] {
        let h = small_label(title, 11.0, true);
        if centered {
            h.setAlignment(NSTextAlignment::Center);
        }
        h.setFrame(rect(x, y0, w, 15.0));
        view.addSubview(&h);
    }
    let mut y = y0 + 20.0;
    separator(view, y, form.width);
    let mut rows = Vec::new();
    for (i, m) in CATALOG.iter().enumerate() {
        let radio = unsafe {
            NSButton::radioButtonWithTitle_target_action(&ns(""), Some(controller), Some(sel!(control:)), mtm)
        };
        radio.setTag(MODEL_USE + i as isize);
        radio.setToolTip(Some(&ns("Use this model")));
        radio.setFrame(rect(X, y + 8.0, 20.0, 18.0));
        view.addSubview(&radio);
        controls.insert(MODEL_USE + i as isize, as_view(&radio));

        let name = NSTextField::labelWithString(&ns(m.name), mtm);
        name.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium })));
        let name_w = name.fittingSize().width.ceil();
        name.setFrame(rect(X + 26.0, y + 8.0, name_w + 2.0, 18.0));
        view.addSubview(&name);
        if m.recommended {
            // Photon's token-chip style: a badge-bezel button that does nothing.
            let badge = NSButton::new(mtm);
            badge.setTitle(&ns("Recommended"));
            badge.setBezelStyle(NSBezelStyle::Badge);
            badge.setFont(Some(&NSFont::systemFontOfSize(10.5)));
            badge.setRefusesFirstResponder(true);
            badge.setToolTip(Some(&ns("The best place to start")));
            let bw = badge.fittingSize().width.ceil() + 4.0;
            badge.setFrame(rect(X + 26.0 + name_w + 8.0, y + 7.0, bw, 20.0));
            view.addSubview(&badge);
        }
        let tagline = small_label(m.tagline, 11.0, true);
        tagline.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
        tagline.setFrame(rect(X + 26.0, y + 27.0, COL_SIZE - X - 34.0, 15.0));
        view.addSubview(&tagline);
        let progress = NSProgressIndicator::new(mtm);
        progress.setStyle(NSProgressIndicatorStyle::Bar);
        progress.setControlSize(NSControlSize::Small);
        progress.setMinValue(0.0);
        progress.setMaxValue(1.0);
        progress.setFrame(rect(X + 26.0, y + 44.0, COL_SIZE - X - 40.0, 8.0));
        progress.setHidden(true);
        view.addSubview(&progress);

        for (grade, detail, x, w) in [
            (m.size, m.size_detail, COL_SIZE, GRADE_W),
            (m.speed, m.speed_detail, COL_SPEED, GRADE_W),
            (m.accuracy, m.accuracy_detail, COL_ACCURACY, GRADE_W + 8.0),
        ] {
            let g = NSTextField::labelWithString(&ns(grade), mtm);
            g.setFont(Some(&NSFont::systemFontOfSize_weight(17.0, unsafe { NSFontWeightSemibold })));
            g.setAlignment(NSTextAlignment::Center);
            g.setToolTip(Some(&ns(detail)));
            g.setFrame(rect(x, y + 17.0, w, 22.0));
            view.addSubview(&g);
        }

        let action = push_button("Download", MODEL_ACTION + i as isize, controller);
        action.setFrame(rect(action_x, y + 14.0, ACTION_W, 28.0));
        view.addSubview(&action);
        controls.insert(MODEL_ACTION + i as isize, as_view(&action));

        y += ROW_H;
        separator(view, y, form.width);
        rows.push(ModelRow { radio, tagline, action, progress });
    }
    (rows, y)
}

/// Bring every model row up to date with what's downloaded and downloading.
fn refresh_models() {
    let s = settings::get();
    let installed = models::installed();
    let active = models::downloading();
    STATE.with(|st| {
        let st = st.borrow();
        let Some(st) = st.as_ref() else { return };
        let mut total: u64 = 0;
        for (i, m) in CATALOG.iter().enumerate() {
            let Some(row) = st.rows.get(i) else { continue };
            let known = installed.is_some();
            let info = installed.as_ref().and_then(|all| all.get(m.id)).cloned().unwrap_or_default();
            let downloading = active.as_ref().filter(|d| d.id == m.id);
            let in_use = s.engine == m.id && info.downloaded;
            if info.downloaded && !m.system {
                total += info.bytes;
            }
            row.radio.setState(if in_use { NSControlStateValueOn } else { NSControlStateValueOff });
            row.radio.setEnabled(info.downloaded && downloading.is_none());

            let text = match downloading {
                Some(d) if d.phase == "preparing" => "Preparing the download…".to_string(),
                Some(d) if d.phase == "optimizing" => "Optimizing it for this Mac…".to_string(),
                Some(d) => format!("Downloading… {:.0}% of {} MB", d.progress * 100.0, m.download_mb),
                None if info.downloaded && m.system => format!("{} · no download", m.tagline),
                None if info.downloaded => format!("{} · {}", m.tagline, models::format_bytes(info.bytes)),
                None if m.system => m.tagline.to_string(),
                None => format!("{} · {} MB", m.tagline, m.download_mb),
            };
            row.tagline.setStringValue(&ns(&text));

            match downloading {
                Some(d) => {
                    row.progress.setHidden(false);
                    let indeterminate = d.phase != "downloading";
                    row.progress.setIndeterminate(indeterminate);
                    if indeterminate {
                        unsafe { row.progress.startAnimation(None) };
                    } else {
                        unsafe { row.progress.stopAnimation(None) };
                        row.progress.setDoubleValue(d.progress);
                    }
                }
                None => {
                    unsafe { row.progress.stopAnimation(None) };
                    row.progress.setHidden(true);
                }
            }

            let (title, hidden, enabled, tip) = if downloading.is_some() {
                ("Cancel", false, true, "Stop this download")
            } else if !known {
                ("Download", false, false, "Checking what's downloaded…")
            } else if info.downloaded && (in_use || m.system) {
                ("Remove…", true, false, "")
            } else if info.downloaded {
                ("Remove…", false, true, "Move this model's files to the Trash")
            } else if active.is_some() {
                ("Download", false, false, "Another model is downloading")
            } else {
                ("Download", false, true, "Download this model")
            };
            row.action.setTitle(&ns(title));
            row.action.setHidden(hidden);
            row.action.setEnabled(enabled);
            row.action.setToolTip(Some(&ns(tip)));
        }
        let note = if total > 0 {
            format!(
                "Downloaded models use {} in ~/Library/Application Support/FluidAudio/Models. Removed models go to the Trash.",
                models::format_bytes(total)
            )
        } else {
            "Downloaded models are stored in ~/Library/Application Support/FluidAudio/Models.".to_string()
        };
        st.storage_note.setStringValue(&ns(&note));
    });
}

/// Radio: switch to a downloaded model.
fn use_model(i: usize) {
    let Some(m) = CATALOG.get(i) else { return };
    if models::is_downloaded(m.id) != Some(true) {
        return;
    }
    if settings::get().engine != m.id {
        settings::update(|s| s.engine = m.id.to_string());
        engine::restart();
    }
}

/// The row's button: Download, Cancel, or Remove….
fn model_action(i: usize) {
    let Some(m) = CATALOG.get(i) else { return };
    if models::downloading().is_some_and(|d| d.id == m.id) {
        models::cancel();
        util::main_after(0.3, refresh_models);
        return;
    }
    match models::is_downloaded(m.id) {
        Some(false) => start_download(m),
        Some(true) if !m.system && settings::get().engine != m.id => confirm_remove(m),
        _ => {}
    }
}

fn start_download(m: &'static models::Model) {
    let id = m.id;
    let started = models::download(
        id,
        |_progress| refresh_models(),
        move |result| {
            match result {
                Ok(()) => {
                    // The first model downloaded becomes the one in use.
                    let s = settings::get();
                    if models::is_downloaded(&s.engine) != Some(true) || s.engine == id {
                        settings::update(|s| s.engine = id.to_string());
                        engine::restart();
                    }
                    crate::hud::toast(&format!("{} is ready", m.name), None);
                }
                Err(e) if e.is_empty() => {} // cancelled
                Err(e) => crate::hud::notice(&format!("{} didn't finish downloading", m.name), Some(&e)),
            }
            refresh();
        },
    );
    if let Err(e) = started {
        crate::hud::notice("Couldn't start the download", Some(&e));
    }
    refresh_models();
}

fn confirm_remove(m: &'static models::Model) {
    let size = models::installed()
        .and_then(|all| all.get(m.id).map(|i| models::format_bytes(i.bytes)))
        .unwrap_or_default();
    let alert = NSAlert::new(mtm());
    alert.setMessageText(&ns(&format!("Remove {}?", m.name)));
    alert.setInformativeText(&ns(&format!(
        "Its files ({size}) go to the Trash. You can download it again any time."
    )));
    alert.addButtonWithTitle(&ns("Remove"));
    alert.addButtonWithTitle(&ns("Cancel"));
    if alert.runModal() != NSAlertFirstButtonReturn {
        return;
    }
    let id = m.id;
    std::thread::spawn(move || {
        let result = models::remove(id);
        util::main_async(move || {
            if let Err(e) = result {
                crate::hud::notice("Couldn't remove the model", Some(&e));
            }
            refresh();
        });
    });
}

// ---------------------------------------------------------------- shortcut recorder

fn set_key_title(text: &str) {
    if let Some(b) = control::<NSButton>(KEY) {
        b.setTitle(&ns(text));
    }
}

/// The recorder's armed state: the button takes the accent colour (selection, in
/// Photon's colour rules) while it waits for keys.
fn set_key_armed(armed: bool) {
    if let Some(b) = control::<NSButton>(KEY) {
        b.setBezelColor(armed.then(NSColor::controlAccentColor).as_deref());
    }
}

fn set_key_note(text: &str) {
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            s.key_note.setStringValue(&ns(text));
        }
    });
}

fn start_key_recording() {
    stop_key_recording();
    hotkey::suspend();
    set_key_title("Press keys…");
    set_key_armed(true);
    let block = RcBlock::new(|e: NonNull<NSEvent>| -> *mut NSEvent {
        let e = unsafe { e.as_ref() };
        let code = e.keyCode();
        let mods = (e.modifierFlags().0 as u32) & keys::MOD_MASK;
        if code == vk::ESCAPE {
            util::main_async(|| finish_key_recording(None));
            return std::ptr::null_mut();
        }
        let sc = Shortcut { key: code, mods };
        if sc.is_valid() {
            util::main_async(move || finish_key_recording(Some(sc)));
        } else {
            util::main_async(|| set_key_note("Add ⌘, ⌃ or ⌥ to the key. Esc keeps the current shortcut."));
        }
        std::ptr::null_mut()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block) };
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.key_monitor = monitor;
        }
    });
}

fn stop_key_recording() {
    let m = STATE.with(|s| s.borrow_mut().as_mut().and_then(|s| s.key_monitor.take()));
    if let Some(m) = m {
        unsafe { NSEvent::removeMonitor(&m) };
        hotkey::resume();
    }
    set_key_armed(false);
}

fn finish_key_recording(sc: Option<Shortcut>) {
    stop_key_recording();
    if let Some(sc) = sc {
        let before = settings::get().shortcut;
        settings::update(|s| s.shortcut = sc);
        match app::apply_shortcut() {
            Ok(()) => set_key_note(&format!("Saved. Press {} anywhere to dictate.", sc.label())),
            Err(e) => {
                // Taken by another app: put the old one back.
                settings::update(|s| s.shortcut = before);
                let _ = app::apply_shortcut();
                set_key_note(&e);
            }
        }
    }
    set_key_title(&settings::get().shortcut.label());
}

// ---------------------------------------------------------------- commands table

/// Newlines show as ⏎ in the table; \n or ⏎ typed in a cell become a newline.
fn display(text: &str) -> String {
    text.replace('\n', "⏎")
}

fn parse(text: &str) -> String {
    text.replace("\\n", "\n").replace('⏎', "\n")
}

fn cell_view(controller: &Controller, col: isize, row: NSInteger) -> Option<Retained<NSView>> {
    let s = settings::get();
    let index = STATE.with(|st| st.borrow().as_ref().and_then(|st| st.filtered.get(row as usize).copied()))?;
    let rule = s.find_replace.get(index)?;
    let value = if col == 0 { rule.spoken.clone() } else { display(&rule.replacement) };
    let field = NSTextField::textFieldWithString(&ns(&value), mtm());
    field.setBordered(false);
    field.setDrawsBackground(false);
    field.setEditable(true);
    field.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    field.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    field.setTag(CELL + row * 2 + col);
    if col == 0 {
        field.setPlaceholderString(Some(&ns("phrase to say")));
    }
    unsafe { field.setDelegate(Some(ProtocolObject::from_ref(controller))) };
    Some(as_view(&field))
}

fn cell_edited(field: &NSTextField) {
    let tag = field.tag();
    if tag < CELL {
        return;
    }
    let (row, col) = ((tag - CELL) / 2, (tag - CELL) % 2);
    let Some(index) = STATE.with(|st| st.borrow().as_ref().and_then(|st| st.filtered.get(row as usize).copied())) else {
        return;
    };
    let value = field.stringValue().to_string();
    settings::update(|s| {
        if let Some(rule) = s.find_replace.get_mut(index) {
            if col == 0 {
                rule.spoken = value.trim().to_string();
            } else {
                rule.replacement = parse(&value);
            }
        }
    });
    update_count();
}

fn filter(query: &str) {
    let s = settings::get();
    let q = query.trim().to_lowercase();
    let filtered: Vec<usize> = s
        .find_replace
        .iter()
        .enumerate()
        .filter(|(_, r)| q.is_empty() || r.spoken.to_lowercase().contains(&q) || r.replacement.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect();
    STATE.with(|st| {
        if let Some(st) = st.borrow_mut().as_mut() {
            st.filtered = filtered;
            st.query = query.to_string();
        }
    });
    if let Some(t) = STATE.with(|st| st.borrow().as_ref().map(|st| st.table.clone())) {
        t.reloadData();
    }
    update_count();
}

fn reload_table() {
    let q = STATE.with(|st| st.borrow().as_ref().map(|st| st.query.clone()).unwrap_or_default());
    filter(&q);
}

fn update_count() {
    let total = settings::get().find_replace.iter().filter(|r| !r.spoken.trim().is_empty()).count();
    STATE.with(|st| {
        if let Some(st) = st.borrow().as_ref() {
            let shown = st.filtered.len();
            let text = if st.query.trim().is_empty() {
                format!("{total} commands")
            } else if shown == 0 {
                format!("None of {total} match")
            } else {
                format!("{shown} of {total} match")
            };
            st.count.setStringValue(&ns(&text));
        }
    });
}

fn add_command() {
    settings::update(|s| s.find_replace.insert(0, Rule { spoken: String::new(), replacement: String::new() }));
    STATE.with(|st| {
        if let Some(st) = st.borrow_mut().as_mut() {
            st.query.clear();
        }
    });
    reload_table();
    if let Some(t) = STATE.with(|st| st.borrow().as_ref().map(|st| st.table.clone())) {
        t.scrollRowToVisible(0);
        t.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(0), false);
        t.editColumn_row_withEvent_select(0, 0, None, true);
    }
}

fn remove_selected() {
    let Some((table, filtered)) = STATE.with(|st| st.borrow().as_ref().map(|st| (st.table.clone(), st.filtered.clone()))) else {
        return;
    };
    let rows = table.selectedRowIndexes();
    let doomed: Vec<usize> = (0..filtered.len()).filter(|r| rows.containsIndex(*r)).map(|r| filtered[r]).collect();
    if !doomed.is_empty() {
        remove_rules(doomed);
    }
}

/// Remove these commands, with an undo (⌘Z) that puts them back where they were.
fn remove_rules(mut indexes: Vec<usize>) {
    indexes.sort_unstable();
    indexes.dedup();
    let mut removed: Vec<(usize, Rule)> = Vec::new();
    settings::update(|s| {
        for &i in indexes.iter().rev() {
            if i < s.find_replace.len() {
                removed.push((i, s.find_replace.remove(i)));
            }
        }
    });
    removed.reverse();
    let name = if removed.len() == 1 { "Delete Command" } else { "Delete Commands" };
    register_undo(name, move || restore_rules(removed.clone()));
    reload_table();
}

/// Put removed commands back (the undo of `remove_rules`; registers its own redo).
fn restore_rules(items: Vec<(usize, Rule)>) {
    let indexes: Vec<usize> = items.iter().map(|(i, _)| *i).collect();
    settings::update(|s| {
        for (i, rule) in items {
            let i = i.min(s.find_replace.len());
            s.find_replace.insert(i, rule);
        }
    });
    let name = if indexes.len() == 1 { "Delete Command" } else { "Delete Commands" };
    register_undo(name, move || remove_rules(indexes.clone()));
    reload_table();
}

/// Register `action` with the Settings window's undo manager. It runs inside the
/// undo, so whatever it registers becomes the redo.
fn register_undo(name: &str, action: impl Fn() + 'static) {
    let Some((window, target)) = STATE.with(|s| s.borrow().as_ref().map(|s| (s.window.clone(), s._controller.clone()))) else {
        return;
    };
    let Some(undo) = window.undoManager() else { return };
    let block = RcBlock::new(move |_target: NonNull<AnyObject>| action());
    unsafe { undo.registerUndoWithTarget_handler(&target, &block) };
    undo.setActionName(&ns(name));
}

/// − is only enabled while something is selected.
fn update_remove_button() {
    let selected = STATE.with(|s| s.borrow().as_ref().map_or(0, |s| s.table.selectedRowIndexes().count()));
    if let Some(c) = control::<NSSegmentedControl>(ADD_REMOVE) {
        c.setEnabled_forSegment(selected > 0, 1);
    }
}

/// ⌘F: show the Commands tab and put the cursor in its search field.
fn focus_search() {
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            s.tabs.selectTabViewItemAtIndex(TAB_TEXT);
            s.window.makeFirstResponder(Some(&s.search));
        }
    });
}

fn restore_defaults() {
    let mtm = mtm();
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&ns("Restore the default commands?"));
    alert.setInformativeText(&ns(
        "Your commands are replaced with the 112 that come with the app. Commands you added or changed are lost.",
    ));
    alert.addButtonWithTitle(&ns("Restore Defaults"));
    alert.addButtonWithTitle(&ns("Cancel"));
    if alert.runModal() == NSAlertFirstButtonReturn {
        settings::update(|s| s.find_replace = settings::default_rules());
        // Older undo steps point at positions in the list that no longer exists.
        if let Some(undo) = STATE.with(|s| s.borrow().as_ref().and_then(|s| s.window.undoManager())) {
            undo.removeAllActions();
        }
        reload_table();
    }
}
