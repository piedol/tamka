//! Transcribe Files: a window that floats above other apps. Drop audio or video files
//! on it (or choose them) and each is transcribed on this Mac with the current model.
//! The text shows in a row per file, or all together in one box, with Copy and Export.
//!
//! Files are decoded to a 16 kHz WAV by `tamka-engine decode`, then sent to the same
//! engine as dictation, one at a time. A dictation goes ahead of waiting files.
//! Transcripts stay in memory until Tamka quits or you click Clear.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAttributedStringNSExtendedStringDrawing, NSBackingStoreType, NSBezelStyle, NSBorderType, NSBox, NSBoxType,
    NSButton, NSColor, NSControlSize, NSDragOperation, NSDraggingDestination, NSDraggingInfo,
    NSFloatingWindowLevel, NSFont, NSFontAttributeName, NSFontWeightSemibold, NSForegroundColorAttributeName,
    NSImageView, NSLineBreakMode, NSModalResponse, NSModalResponseOK, NSOpenPanel, NSPanel, NSPasteboard,
    NSPasteboardTypeFileURL, NSPasteboardTypeString, NSProgressIndicator, NSProgressIndicatorStyle, NSSavePanel,
    NSScrollView, NSSegmentStyle, NSSegmentedControl, NSStringDrawingOptions, NSTextAlignment, NSTextField,
    NSTextView, NSView, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSMutableAttributedString, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize,
    NSURL,
};

use crate::util::{self, ns};
use crate::{engine, models, settings, text, ui};

const PAD: f64 = 16.0;
const TOP_BAR: f64 = 46.0;
const BOTTOM_BAR: f64 = 48.0;
/// A row's text box grows with its text up to this height, then scrolls.
const TEXT_MAX: f64 = 168.0;
const TEXT_INSET: NSSize = NSSize::new(6.0, 6.0);

// Control tags. Row buttons are `id * ROW_TAGS + ROW_COPY | ROW_EXPORT | ROW_REMOVE`;
// row ids start at 1, so they never meet the window's own tags.
const CHOOSE: isize = 1;
const MODE: isize = 2;
const CLEAR: isize = 3;
const STOP: isize = 4;
const EXPORT_ALL: isize = 5;
const COPY_ALL: isize = 6;
const EXPORT_COMBINED: isize = 7;
const ROW_TAGS: isize = 8;
const ROW_COPY: isize = 0;
const ROW_EXPORT: isize = 1;
const ROW_REMOVE: isize = 2;

/// File extensions picked up from inside a dropped folder. Files dropped directly are
/// always tried; the engine says when one has no sound.
const MEDIA: &[&str] = &[
    "m4a", "mp3", "wav", "aif", "aiff", "aifc", "caf", "flac", "aac", "ogg", "opus", "m4b", "amr", "mov", "mp4",
    "m4v", "3gp", "webm", "mkv", "avi",
];

#[derive(Clone, PartialEq, Debug)]
enum RowState {
    Waiting,
    Working,
    Done,
    Failed(String),
    Stopped,
}

struct RowViews {
    view: Retained<Flipped>,
    icon: Retained<NSImageView>,
    name: Retained<NSTextField>,
    status: Retained<NSTextField>,
    spinner: Retained<NSProgressIndicator>,
    copy: Retained<NSButton>,
    export: Retained<NSButton>,
    remove: Retained<NSButton>,
    scroll: Retained<NSScrollView>,
    text: Retained<NSTextView>,
    line: Retained<NSBox>,
}

struct Row {
    id: u64,
    path: PathBuf,
    name: String,
    state: RowState,
    text: String,
    seconds: f64,
    views: RowViews,
}

struct State {
    panel: Retained<NSPanel>,
    _controller: Retained<Controller>,
    root: Retained<DropView>,
    mode: Retained<NSSegmentedControl>,
    choose: Retained<NSButton>,
    top_line: Retained<NSBox>,
    bottom_line: Retained<NSBox>,
    empty: Retained<Flipped>,
    empty_icon: Retained<NSImageView>,
    empty_title: Retained<NSTextField>,
    empty_note: Retained<NSTextField>,
    highlight: Retained<Flipped>,
    rows_scroll: Retained<NSScrollView>,
    rows_doc: Retained<Flipped>,
    combined_scroll: Retained<NSScrollView>,
    combined_text: Retained<NSTextView>,
    status: Retained<NSTextField>,
    clear: Retained<NSButton>,
    stop: Retained<NSButton>,
    export_all: Retained<NSButton>,
    copy_all: Retained<NSButton>,
    export_combined: Retained<NSButton>,
    rows: Vec<Row>,
    next_id: u64,
    /// Files added since the queue was last empty, and how many of them have finished.
    batch_total: usize,
    batch_done: usize,
    /// A one-off note in the status line ("Saved 3 transcripts"), until the next change.
    message: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Files waiting for the worker thread.
static QUEUE: Mutex<Queue> = Mutex::new(Queue { items: VecDeque::new(), running: false });
/// The file the worker is on (0 = none), and the one Stop asked it to give up.
static CURRENT: AtomicU64 = AtomicU64::new(0);
static STOP_ID: AtomicU64 = AtomicU64::new(0);
/// The decoder's pid while it runs, so Stop can end it.
static DECODE_PID: AtomicU32 = AtomicU32::new(0);

struct Queue {
    items: VecDeque<(u64, PathBuf)>,
    running: bool,
}

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("Transcribe Files must run on the main thread")
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

define_class!(
    // SAFETY: NSView subclass that only flips its coordinate system.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "TamkaFilesFlipped"]
    struct Flipped;

    impl Flipped {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

define_class!(
    // SAFETY: the window's flipped content view, and where files are dropped. No ivars.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "TamkaFilesDropView"]
    struct DropView;

    impl DropView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }

    unsafe impl NSObjectProtocol for DropView {}

    unsafe impl NSDraggingDestination for DropView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            if dropped_paths(sender).is_empty() {
                return NSDragOperation::None;
            }
            set_highlight(true);
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            set_highlight(false);
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> objc2::runtime::Bool {
            set_highlight(false);
            let paths = dropped_paths(sender);
            if paths.is_empty() {
                return objc2::runtime::Bool::NO;
            }
            // After the drop finishes, so Finder isn't kept waiting.
            util::main_async(move || add(paths));
            objc2::runtime::Bool::YES
        }
    }
);

define_class!(
    // SAFETY: NSObject subclass; target of the window's controls and its delegate. No Drop impl.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "TamkaFilesController"]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSWindowDelegate for Controller {
        #[unsafe(method(windowDidResize:))]
        fn did_resize(&self, _n: &NSNotification) {
            layout();
        }

        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _n: &NSNotification) {
            ui::app(mtm()).deactivate();
        }
    }

    impl Controller {
        #[unsafe(method(control:))]
        fn control(&self, sender: Option<&AnyObject>) {
            let tag: isize = match sender {
                Some(s) => unsafe { msg_send![s, tag] },
                None => 0,
            };
            // After the click returns, since most actions rebuild the views.
            util::main_async(move || clicked(tag));
        }
    }
);

/// File URLs on a drag's pasteboard, as paths.
fn dropped_paths(sender: &ProtocolObject<dyn NSDraggingInfo>) -> Vec<PathBuf> {
    pasteboard_paths(&sender.draggingPasteboard())
}

fn pasteboard_paths(pb: &NSPasteboard) -> Vec<PathBuf> {
    let Some(items) = pb.pasteboardItems() else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| item.stringForType(unsafe { NSPasteboardTypeFileURL }))
        .filter_map(|s| NSURL::URLWithString(&s))
        .filter_map(|u| u.path())
        .map(|p| PathBuf::from(p.to_string()))
        .collect()
}

// ---------------------------------------------------------------- window

/// Open Transcribe Files (or bring it forward).
pub fn show() {
    let mtm = mtm();
    if STATE.with(|s| s.borrow().is_none()) {
        build(mtm);
    }
    refresh();
    if let Some(panel) = STATE.with(|s| s.borrow().as_ref().map(|s| s.panel.clone())) {
        ui::app(mtm).activate();
        panel.makeKeyAndOrderFront(None);
    }
}

fn label(text: &str, size: f64, secondary: bool) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&ns(text), mtm());
    l.setFont(Some(&NSFont::systemFontOfSize(size)));
    if secondary {
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    l
}

fn button(title: &str, tag: isize, controller: &Controller, small: bool) -> Retained<NSButton> {
    let b = unsafe { NSButton::buttonWithTitle_target_action(&ns(title), Some(controller), Some(sel!(control:)), mtm()) };
    b.setBezelStyle(NSBezelStyle::Push);
    if small {
        b.setControlSize(NSControlSize::Small);
        b.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    }
    b.setTag(tag);
    b.sizeToFit();
    b
}

fn separator() -> Retained<NSBox> {
    let b = NSBox::new(mtm());
    b.setBoxType(NSBoxType::Separator);
    b
}

fn flipped(frame: NSRect) -> Retained<Flipped> {
    let this = Flipped::alloc(mtm()).set_ivars(());
    unsafe { msg_send![super(this), initWithFrame: frame] }
}

/// A read-only, selectable text view in a scroll view. It refuses drops, so files
/// dropped on it go to the window.
fn text_box(border: bool) -> (Retained<NSScrollView>, Retained<NSTextView>) {
    let scroll = NSTextView::scrollableTextView(mtm());
    scroll.setBorderType(if border { NSBorderType::LineBorder } else { NSBorderType::NoBorder });
    scroll.setDrawsBackground(border);
    let view = scroll.documentView().and_then(|v| v.downcast::<NSTextView>().ok()).expect("scrollableTextView has a text view");
    view.setEditable(false);
    view.setSelectable(true);
    view.setRichText(false);
    view.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    view.setTextContainerInset(TEXT_INSET);
    view.setDrawsBackground(border);
    view.unregisterDraggedTypes();
    (scroll, view)
}

fn build(mtm: MainThreadMarker) {
    let controller: Retained<Controller> = {
        let this = Controller::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    };
    let (w, h) = (560.0, 600.0);
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect(0.0, 0.0, w, h),
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitle(&ns("Transcribe Files"));
    // Above every app's windows, on every Space and over full-screen apps.
    panel.setFloatingPanel(true);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setHidesOnDeactivate(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel.setContentMinSize(NSSize::new(420.0, 320.0));
    panel.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    panel.center();

    let root: Retained<DropView> = {
        let this = DropView::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, w, h)] }
    };
    root.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));
    panel.setContentView(Some(&root));

    let mode = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &NSArray::from_retained_slice(&[ns("Separate"), ns("Combined")]),
            objc2_app_kit::NSSegmentSwitchTracking::SelectOne,
            Some(&controller),
            Some(sel!(control:)),
            mtm,
        )
    };
    mode.setSegmentStyle(NSSegmentStyle::Automatic);
    mode.setTag(MODE);
    mode.setSelectedSegment(if settings::get().files_combined { 1 } else { 0 });
    mode.setToolTip(Some(&ns("Show a row per file, or every file's text in one box")));
    root.addSubview(&mode);
    let choose = button("Choose Files…", CHOOSE, &controller, false);
    root.addSubview(&choose);
    let top_line = separator();
    root.addSubview(&top_line);

    // Empty state.
    let empty = flipped(rect(0.0, 0.0, w, 200.0));
    let empty_icon = NSImageView::new(mtm);
    let icon = ui::symbol("waveform.badge.plus", 40.0).or_else(|| ui::symbol("waveform", 40.0));
    empty_icon.setImage(icon.as_deref());
    empty_icon.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
    empty_icon.unregisterDraggedTypes();
    empty.addSubview(&empty_icon);
    let empty_title = label("Drop audio or video files here", 15.0, false);
    empty_title.setFont(Some(&NSFont::systemFontOfSize_weight(15.0, unsafe { NSFontWeightSemibold })));
    empty_title.setAlignment(NSTextAlignment::Center);
    empty.addSubview(&empty_title);
    let empty_note = NSTextField::wrappingLabelWithString(&ns(""), mtm);
    empty_note.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    empty_note.setTextColor(Some(&NSColor::secondaryLabelColor()));
    empty_note.setAlignment(NSTextAlignment::Center);
    empty.addSubview(&empty_note);
    root.addSubview(&empty);

    // Separate: a scrolling list of rows.
    let rows_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, w, 200.0));
    rows_scroll.setHasVerticalScroller(true);
    rows_scroll.setDrawsBackground(false);
    rows_scroll.setBorderType(NSBorderType::NoBorder);
    let rows_doc = flipped(rect(0.0, 0.0, w, 200.0));
    rows_scroll.setDocumentView(Some(&rows_doc));
    root.addSubview(&rows_scroll);

    // Combined: one text box.
    let (combined_scroll, combined_text) = text_box(false);
    combined_text.setTextContainerInset(NSSize::new(PAD - 5.0, 12.0));
    root.addSubview(&combined_scroll);

    // Shown while files are dragged over the window.
    let highlight = flipped(rect(0.0, 0.0, w, 200.0));
    highlight.setWantsLayer(true);
    if let Some(layer) = highlight.layer() {
        layer.setCornerRadius(10.0);
        layer.setBorderWidth(2.0);
        let accent = NSColor::controlAccentColor();
        layer.setBorderColor(Some(&accent.CGColor()));
    }
    highlight.setHidden(true);
    root.addSubview(&highlight);

    let bottom_line = separator();
    root.addSubview(&bottom_line);
    let status = label("", 12.0, true);
    root.addSubview(&status);
    let clear = button("Clear", CLEAR, &controller, false);
    clear.setToolTip(Some(&ns("Remove every file and its text from this window")));
    let stop = button("Stop", STOP, &controller, false);
    let export_all = button("Export All…", EXPORT_ALL, &controller, false);
    export_all.setToolTip(Some(&ns("Save each transcript as a text file in a folder you choose")));
    let copy_all = button("Copy", COPY_ALL, &controller, false);
    let export_combined = button("Export…", EXPORT_COMBINED, &controller, false);
    for b in [&clear, &stop, &export_all, &copy_all, &export_combined] {
        root.addSubview(b);
    }

    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            panel,
            _controller: controller,
            root,
            mode,
            choose,
            top_line,
            bottom_line,
            empty,
            empty_icon,
            empty_title,
            empty_note,
            highlight,
            rows_scroll,
            rows_doc,
            combined_scroll,
            combined_text,
            status,
            clear,
            stop,
            export_all,
            copy_all,
            export_combined,
            rows: Vec::new(),
            next_id: 1,
            batch_total: 0,
            batch_done: 0,
            message: None,
        })
    });
}

fn combined() -> bool {
    settings::get().files_combined
}

fn set_highlight(on: bool) {
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            s.highlight.setHidden(!on);
        }
    });
}

/// Show what the rows hold now, then lay everything out.
fn refresh() {
    // Filling a text view scrolls it into view, which would scroll the list too:
    // keep the list where it was.
    let origin = STATE.with(|s| s.borrow().as_ref().map(|st| st.rows_scroll.contentView().bounds().origin));
    fill();
    layout();
    STATE.with(|s| {
        if let (Some(st), Some(origin)) = (s.borrow().as_ref(), origin) {
            let clip = st.rows_scroll.contentView();
            let max_y = (st.rows_doc.frame().size.height - clip.bounds().size.height).max(0.0);
            clip.scrollToPoint(NSPoint::new(0.0, origin.y.min(max_y)));
            st.rows_scroll.reflectScrolledClipView(&clip);
        }
    });
}

fn fill() {
    STATE.with(|s| {
        let s = s.borrow();
        let Some(st) = s.as_ref() else { return };
        let busy = st.rows.iter().any(|r| matches!(r.state, RowState::Waiting | RowState::Working));
        let done: Vec<&Row> = st.rows.iter().filter(|r| r.state == RowState::Done && !r.text.is_empty()).collect();
        let combined = combined();
        let empty = st.rows.is_empty();

        st.empty.setHidden(!empty);
        st.rows_scroll.setHidden(empty || combined);
        st.combined_scroll.setHidden(empty || !combined);
        st.empty_note.setStringValue(&ns(&format!(
            "Or click Choose Files. Folders work too.\nTranscription runs on this Mac with {}.",
            models::name(&settings::get().engine)
        )));

        for row in &st.rows {
            fill_row(row);
        }
        if combined {
            fill_combined(&st.combined_text, &st.rows);
        }

        // The status line: progress, a one-off message, or a summary.
        let text = if busy {
            format!("Transcribing {} of {}…", (st.batch_done + 1).min(st.batch_total), st.batch_total)
        } else if let Some(m) = &st.message {
            m.clone()
        } else if empty {
            String::new()
        } else {
            let words: usize = done.iter().map(|r| r.text.split_whitespace().count()).sum();
            let failed = st.rows.iter().filter(|r| matches!(r.state, RowState::Failed(_))).count();
            let stopped = st.rows.iter().filter(|r| r.state == RowState::Stopped).count();
            let mut t = format!("{} · {}", plural(st.rows.len(), "file"), plural(words, "word"));
            if failed > 0 {
                t.push_str(&format!(" · {failed} couldn't be transcribed"));
            }
            if stopped > 0 {
                t.push_str(&format!(" · {stopped} stopped"));
            }
            t
        };
        st.status.setStringValue(&ns(&text));

        st.stop.setHidden(!busy);
        st.clear.setHidden(busy || empty);
        st.export_all.setHidden(combined || empty);
        st.export_all.setEnabled(!done.is_empty());
        st.copy_all.setHidden(!combined || empty);
        st.copy_all.setEnabled(!done.is_empty());
        st.export_combined.setHidden(!combined || empty);
        st.export_combined.setEnabled(!done.is_empty());
    });
}

fn plural(n: usize, word: &str) -> String {
    let n_text = if n >= 1000 { format!("{},{:03}", n / 1000, n % 1000) } else { n.to_string() };
    if n == 1 { format!("1 {word}") } else { format!("{n_text} {word}s") }
}

/// "0:07", "4:12", "1:02:09".
fn duration(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn status_text(row: &Row) -> String {
    match &row.state {
        RowState::Waiting => "Waiting".into(),
        RowState::Working => "Transcribing…".into(),
        RowState::Done if row.text.is_empty() => format!("{} · No speech heard", duration(row.seconds)),
        RowState::Done => format!("{} · {}", duration(row.seconds), plural(row.text.split_whitespace().count(), "word")),
        RowState::Failed(e) => e.clone(),
        RowState::Stopped => "Stopped".into(),
    }
}

fn fill_row(row: &Row) {
    let v = &row.views;
    v.status.setStringValue(&ns(&status_text(row)));
    let failed = matches!(row.state, RowState::Failed(_));
    let color = if failed { NSColor::systemOrangeColor() } else { NSColor::secondaryLabelColor() };
    v.status.setTextColor(Some(&color));
    let working = row.state == RowState::Working;
    v.spinner.setHidden(!working);
    if working {
        unsafe { v.spinner.startAnimation(None) };
    } else {
        unsafe { v.spinner.stopAnimation(None) };
    }
    let has_text = row.state == RowState::Done && !row.text.is_empty();
    v.copy.setHidden(!has_text);
    v.export.setHidden(!has_text);
    v.remove.setHidden(working);
    v.scroll.setHidden(!has_text);
    if has_text && v.text.string().to_string() != row.text {
        v.text.setString(&ns(&row.text));
    }
}

/// Every file under its name, in the order they were added.
fn fill_combined(view: &NSTextView, rows: &[Row]) {
    let mut text = String::new();
    let mut headings: Vec<NSRange> = Vec::new();
    let mut notes: Vec<NSRange> = Vec::new();
    let len = |s: &str| s.encode_utf16().count();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            text.push_str("\n\n");
        }
        let start = len(&text);
        text.push_str(&row.name);
        headings.push(NSRange::new(start, len(&row.name)));
        text.push('\n');
        if row.state == RowState::Done && !row.text.is_empty() {
            text.push_str(&row.text);
        } else {
            let note = status_text(row);
            notes.push(NSRange::new(len(&text), len(&note)));
            text.push_str(&note);
        }
    }
    view.setString(&ns(&text));
    let Some(storage) = (unsafe { view.textStorage() }) else { return };
    let whole = NSRange::new(0, len(&text));
    let body = NSFont::systemFontOfSize(13.0);
    let bold = NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightSemibold });
    let primary = NSColor::labelColor();
    let secondary = NSColor::secondaryLabelColor();
    // SAFETY: fonts and colours are what these keys take; every range lies within the text.
    unsafe {
        storage.addAttribute_value_range(NSFontAttributeName, &body, whole);
        storage.addAttribute_value_range(NSForegroundColorAttributeName, &primary, whole);
        for r in headings {
            storage.addAttribute_value_range(NSFontAttributeName, &bold, r);
        }
        for r in notes {
            storage.addAttribute_value_range(NSForegroundColorAttributeName, &secondary, r);
        }
    }
}

/// Height the text needs at this width, inside a text box.
fn text_height(text: &str, width: f64) -> f64 {
    let s = NSMutableAttributedString::from_nsstring(&ns(text));
    let font = NSFont::systemFontOfSize(13.0);
    // SAFETY: a font for the font key, over the whole string.
    unsafe { s.addAttribute_value_range(NSFontAttributeName, &font, NSRange::new(0, text.encode_utf16().count())) };
    let inner = width - 2.0 * TEXT_INSET.width - 10.0;
    let r = s.boundingRectWithSize_options_context(
        NSSize::new(inner, 1.0e6),
        NSStringDrawingOptions::UsesLineFragmentOrigin | NSStringDrawingOptions::UsesFontLeading,
        None,
    );
    r.size.height.ceil() + 2.0 * TEXT_INSET.height + 2.0
}

fn layout() {
    STATE.with(|s| {
        let s = s.borrow();
        let Some(st) = s.as_ref() else { return };
        let size = st.root.frame().size;
        let (w, h) = (size.width, size.height);

        st.mode.sizeToFit();
        let mw = st.mode.frame().size.width;
        st.mode.setFrame(rect(PAD, 11.0, mw, 24.0));
        let cw = st.choose.frame().size.width;
        st.choose.setFrame(rect(w - PAD - cw, 9.0, cw, 28.0));
        st.top_line.setFrame(rect(0.0, TOP_BAR - 1.0, w, 1.0));

        let content = rect(0.0, TOP_BAR, w, (h - TOP_BAR - BOTTOM_BAR).max(0.0));
        st.rows_scroll.setFrame(content);
        st.combined_scroll.setFrame(content);
        st.empty.setFrame(content);
        st.highlight.setFrame(rect(6.0, TOP_BAR + 6.0, w - 12.0, content.size.height - 12.0));

        // Empty state, centred.
        let ch = content.size.height;
        let block = 44.0 + 14.0 + 20.0 + 8.0 + 34.0;
        let top = ((ch - block) / 2.0).max(12.0);
        st.empty_icon.setFrame(rect((w - 60.0) / 2.0, top, 60.0, 44.0));
        st.empty_title.setFrame(rect(PAD, top + 58.0, w - 2.0 * PAD, 20.0));
        let nw = (w - 4.0 * PAD).min(380.0);
        st.empty_note.setFrame(rect((w - nw) / 2.0, top + 86.0, nw, 48.0));

        // Rows, top to bottom.
        let inner_w = st.rows_scroll.contentSize().width;
        let mut y = 0.0;
        let count = st.rows.len();
        for (i, row) in st.rows.iter().enumerate() {
            let v = &row.views;
            let top = 12.0;
            v.icon.setFrame(rect(PAD, top, 24.0, 24.0));
            let mut right = inner_w - PAD;
            v.remove.setFrame(rect(right - 18.0, top + 3.0, 18.0, 18.0));
            right -= 18.0 + 8.0;
            if !v.spinner.isHidden() {
                v.spinner.setFrame(rect(right - 16.0, top + 4.0, 16.0, 16.0));
                right -= 16.0 + 8.0;
            }
            for b in [&v.export, &v.copy] {
                if !b.isHidden() {
                    let bw = b.frame().size.width;
                    b.setFrame(rect(right - bw, top + 1.0, bw, 22.0));
                    right -= bw + 6.0;
                }
            }
            let text_x = PAD + 32.0;
            v.name.setFrame(rect(text_x, top - 1.0, (right - text_x - 4.0).max(40.0), 17.0));
            v.status.setFrame(rect(text_x, top + 16.0, (right - text_x - 4.0).max(40.0), 15.0));
            let mut height = top + 36.0;
            if !v.scroll.isHidden() {
                let bw = inner_w - text_x - PAD;
                let th = text_height(&row.text, bw).clamp(34.0, TEXT_MAX);
                v.scroll.setFrame(rect(text_x, height + 2.0, bw, th));
                height += th + 2.0;
            }
            height += 12.0;
            v.line.setHidden(i + 1 == count);
            v.line.setFrame(rect(text_x, height - 1.0, inner_w - text_x, 1.0));
            v.view.setFrame(rect(0.0, y, inner_w, height));
            y += height;
        }
        let doc_h = y.max(st.rows_scroll.contentSize().height);
        st.rows_doc.setFrame(rect(0.0, 0.0, inner_w, doc_h));

        // Bottom bar: buttons from the right, the status line in what's left.
        st.bottom_line.setFrame(rect(0.0, h - BOTTOM_BAR, w, 1.0));
        let by = h - BOTTOM_BAR + 10.0;
        let mut right = w - PAD;
        for b in [&st.copy_all, &st.export_combined, &st.export_all, &st.stop, &st.clear] {
            if !b.isHidden() {
                let bw = b.frame().size.width;
                b.setFrame(rect(right - bw, by, bw, 28.0));
                right -= bw + 8.0;
            }
        }
        st.status.setFrame(rect(PAD, by + 6.0, (right - PAD - 4.0).max(40.0), 16.0));
    });
}

fn make_row(id: u64, path: &Path, controller: &Controller) -> RowViews {
    let mtm = mtm();
    let view = flipped(rect(0.0, 0.0, 400.0, 60.0));
    let icon = NSImageView::new(mtm);
    icon.setImage(Some(&NSWorkspace::sharedWorkspace().iconForFile(&ns(&path.to_string_lossy()))));
    icon.unregisterDraggedTypes();
    view.addSubview(&icon);
    let name = label(&file_name(path), 13.0, false);
    name.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightSemibold })));
    name.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
    name.setToolTip(Some(&ns(&path.to_string_lossy())));
    view.addSubview(&name);
    let status = label("", 11.0, true);
    view.addSubview(&status);
    let spinner = NSProgressIndicator::new(mtm);
    spinner.setStyle(NSProgressIndicatorStyle::Spinning);
    spinner.setControlSize(NSControlSize::Small);
    spinner.setDisplayedWhenStopped(false);
    view.addSubview(&spinner);
    let tag = id as isize * ROW_TAGS;
    let copy = button("Copy", tag + ROW_COPY, controller, true);
    let export = button("Export…", tag + ROW_EXPORT, controller, true);
    let remove = button("", tag + ROW_REMOVE, controller, false);
    remove.setBordered(false);
    remove.setImage(ui::symbol("xmark.circle.fill", 13.0).as_deref());
    remove.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
    remove.setToolTip(Some(&ns("Remove from this list")));
    for b in [&copy, &export, &remove] {
        view.addSubview(b);
    }
    let (scroll, text) = text_box(true);
    view.addSubview(&scroll);
    let line = separator();
    view.addSubview(&line);
    RowViews { view, icon, name, status, spinner, copy, export, remove, scroll, text, line }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
}

/// The files in a dropped folder that look like audio or video, in name order.
fn expand(path: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if !path.is_dir() {
        out.push(path.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return };
    let mut children: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    children.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    for child in children {
        let hidden = child.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if hidden {
            continue;
        }
        if child.is_dir() {
            if depth < 4 {
                expand(&child, out, depth + 1);
            }
        } else if child
            .extension()
            .is_some_and(|e| MEDIA.contains(&e.to_string_lossy().to_lowercase().as_str()))
        {
            out.push(child);
        }
    }
}

/// Add files (or folders of them) and start transcribing.
pub fn add(paths: Vec<PathBuf>) {
    if STATE.with(|s| s.borrow().is_none()) {
        build(mtm());
    }
    let mut files = Vec::new();
    for p in &paths {
        expand(p, &mut files, 0);
    }
    let mut queued: Vec<(u64, PathBuf)> = Vec::new();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else { return };
        st.message = None;
        if files.is_empty() && !paths.is_empty() {
            st.message = Some("No audio or video files in what you dropped".into());
        }
        let idle = !st.rows.iter().any(|r| matches!(r.state, RowState::Waiting | RowState::Working));
        if idle {
            st.batch_total = 0;
            st.batch_done = 0;
        }
        for path in files {
            let id = st.next_id;
            st.next_id += 1;
            let views = make_row(id, &path, &st._controller);
            st.rows_doc.addSubview(&views.view);
            st.rows.push(Row {
                id,
                name: file_name(&path),
                path: path.clone(),
                state: RowState::Waiting,
                text: String::new(),
                seconds: 0.0,
                views,
            });
            st.batch_total += 1;
            queued.push((id, path));
        }
    });
    show();
    // Bring the first new file into view; the list keeps its place otherwise.
    if let Some(&(first, _)) = queued.first() {
        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref()
                && let Some(row) = st.rows.iter().find(|r| r.id == first)
            {
                let f = row.views.view.frame();
                let visible = st.rows_scroll.contentView().bounds().size.height;
                st.rows_doc.scrollRectToVisible(rect(0.0, f.origin.y, f.size.width, f.size.height.min(visible)));
            }
        });
    }
    enqueue(queued);
}

// ---------------------------------------------------------------- the worker

fn enqueue(items: Vec<(u64, PathBuf)>) {
    if items.is_empty() {
        return;
    }
    let mut q = QUEUE.lock().unwrap();
    q.items.extend(items);
    if !q.running {
        q.running = true;
        std::thread::spawn(worker);
    }
}

fn scratch_dir() -> PathBuf {
    util::cache_dir().join("files")
}

/// Takes files off the queue one at a time until it's empty.
fn worker() {
    let _ = std::fs::create_dir_all(scratch_dir());
    loop {
        let next = {
            let mut q = QUEUE.lock().unwrap();
            let next = q.items.pop_front();
            if next.is_none() {
                q.running = false;
            }
            next
        };
        let Some((id, path)) = next else { break };
        CURRENT.store(id, Ordering::SeqCst);
        util::main_async(move || set_state(id, RowState::Working, String::new(), 0.0));
        let result = transcribe(id, &path);
        CURRENT.store(0, Ordering::SeqCst);
        let stopped = STOP_ID.swap(0, Ordering::SeqCst) == id;
        if stopped && engine::status() == engine::Status::Stopped {
            // Stop ended the engine mid-file; have it ready for the next dictation.
            engine::start_async();
        }
        util::main_async(move || match result {
            _ if stopped => set_state(id, RowState::Stopped, String::new(), 0.0),
            Ok((text, seconds)) => set_state(id, RowState::Done, text, seconds),
            Err(e) => set_state(id, RowState::Failed(e), String::new(), 0.0),
        });
    }
    let _ = std::fs::remove_dir_all(scratch_dir());
    util::main_async(util::release_memory_soon);
}

fn transcribe(id: u64, path: &Path) -> Result<(String, f64), String> {
    let wav = scratch_dir().join(format!("{id}.wav"));
    let decoded = engine::decode(path, &wav, |pid| DECODE_PID.store(pid, Ordering::SeqCst));
    DECODE_PID.store(0, Ordering::SeqCst);
    let result = decoded.and_then(|seconds| {
        if STOP_ID.load(Ordering::SeqCst) == id {
            return Err("Stopped".into());
        }
        let t0 = std::time::Instant::now();
        let raw = engine::transcribe_file(&wav)?;
        util::log(format!("file: {seconds:.0} s of audio transcribed in {} ms", t0.elapsed().as_millis()));
        // Cleanup as in Settings › Text, without spoken commands: in a recording,
        // "comma" is usually a word someone said.
        let s = settings::get();
        let opts = text::Options { remove_fillers: s.remove_fillers, collapse_repeats: s.collapse_repeats };
        Ok((text::process(&raw, &[], &opts), seconds))
    });
    let _ = std::fs::remove_file(&wav);
    match &result {
        Err(_) if STOP_ID.load(Ordering::SeqCst) == id => util::log("file stopped"),
        Err(e) => util::log(format!("file couldn't be transcribed: {e}")),
        Ok(_) => {}
    }
    result
}

fn set_state(id: u64, state: RowState, text: String, seconds: f64) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(st) = s.as_mut() else { return };
        let finished = !matches!(state, RowState::Waiting | RowState::Working);
        if let Some(row) = st.rows.iter_mut().find(|r| r.id == id) {
            row.state = state;
            row.text = text;
            row.seconds = seconds;
            if finished {
                st.batch_done += 1;
            }
        }
    });
    refresh();
}

/// Files are waiting or being transcribed. An update waits until they're done.
pub fn busy() -> bool {
    let q = QUEUE.lock().unwrap();
    q.running || !q.items.is_empty()
}

/// The menu item's subtitle while files are being transcribed.
pub fn menu_subtitle() -> Option<String> {
    STATE.with(|s| {
        let s = s.borrow();
        let st = s.as_ref()?;
        let busy = st.rows.iter().any(|r| matches!(r.state, RowState::Waiting | RowState::Working));
        busy.then(|| format!("Transcribing {} of {}", (st.batch_done + 1).min(st.batch_total), st.batch_total))
    })
}

/// Files given on the command line of a debug build, opened once Tamka has launched.
static AT_LAUNCH: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

#[cfg(debug_assertions)]
pub fn open_at_launch(paths: Vec<PathBuf>) {
    *AT_LAUNCH.lock().unwrap() = paths;
    // For testing the window without a mouse: each line on stdin is "click <tag>"
    // (a button's tag), "add <path>", "drop <path>\t<path>…" (a drop's pasteboard handling),
    // "export-all <folder>" (Export All without the dialog),
    // "url <host>" (a tamka:// link) or "settings <command>" (see settings_window::debug).
    std::thread::spawn(|| {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if let Some(tag) = line.strip_prefix("click ").and_then(|t| t.trim().parse::<isize>().ok()) {
                util::main_async(move || {
                    if tag == MODE {
                        STATE.with(|s| {
                            if let Some(st) = s.borrow().as_ref() {
                                st.mode.setSelectedSegment(1 - st.mode.selectedSegment());
                            }
                        });
                    }
                    clicked(tag);
                });
            } else if let Some(rest) = line.strip_prefix("settings ") {
                let rest = rest.to_string();
                util::main_async(move || crate::settings_window::debug(&rest));
            } else if let Some(host) = line.strip_prefix("url ") {
                let host = host.trim().to_string();
                util::main_async(move || crate::app::open_url(&host));
            } else if let Some(dir) = line.strip_prefix("export-all ") {
                let dir = PathBuf::from(dir.trim());
                util::main_async(move || export_to(dir));
            } else if let Some(paths) = line.strip_prefix("drop ") {
                // What a drop does with Finder's pasteboard, on a private pasteboard
                // holding these tab-separated paths.
                let paths: Vec<String> = paths.split('\t').map(|p| p.trim().to_string()).collect();
                util::main_async(move || {
                    let pb = NSPasteboard::pasteboardWithUniqueName();
                    pb.clearContents();
                    let urls: Vec<Retained<NSURL>> = paths.iter().map(|p| NSURL::fileURLWithPath(&ns(p))).collect();
                    let objects: Vec<Retained<ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>>> =
                        urls.into_iter().map(ProtocolObject::from_retained).collect();
                    pb.writeObjects(&NSArray::from_retained_slice(&objects));
                    let found = pasteboard_paths(&pb);
                    util::log(format!("test drop: {} of {} paths read from the pasteboard", found.len(), paths.len()));
                    add(found);
                    let registered = STATE.with(|s| {
                        s.borrow().as_ref().map_or(0, |st| st.root.registeredDraggedTypes().len())
                    });
                    util::log(format!("test drop: the window takes {registered} dragged type(s)"));
                    let _: () = unsafe { msg_send![&pb, releaseGlobally] };
                });
            } else if let Some(path) = line.strip_prefix("add ") {
                let path = PathBuf::from(path.trim());
                util::main_async(move || add(vec![path]));
            }
        }
    });
}

/// Clear out decoded audio left by a Tamka that quit mid-file.
pub fn launched() {
    let _ = std::fs::remove_dir_all(scratch_dir());
    let paths = std::mem::take(&mut *AT_LAUNCH.lock().unwrap());
    if !paths.is_empty() {
        add(paths);
    }
}

// ---------------------------------------------------------------- actions

fn clicked(tag: isize) {
    match tag {
        CHOOSE => choose(),
        MODE => {
            let combined = STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.mode.selectedSegment() == 1));
            settings::update(|s| s.files_combined = combined);
            refresh();
        }
        CLEAR => {
            STATE.with(|s| {
                if let Some(st) = s.borrow_mut().as_mut() {
                    for row in st.rows.drain(..) {
                        row.views.view.removeFromSuperview();
                    }
                    st.message = None;
                }
            });
            refresh();
            util::release_memory_soon();
        }
        STOP => stop(),
        EXPORT_ALL => export_all(),
        COPY_ALL => {
            if let Some(text) = combined_text() {
                copy(&text);
                flash_copied(None);
            }
        }
        EXPORT_COMBINED => {
            if let Some(text) = combined_text() {
                let name = STATE.with(|s| {
                    let s = s.borrow();
                    let done: Vec<&Row> = s.as_ref().map_or(Vec::new(), |st| ready(&st.rows));
                    if done.len() == 1 { stem(&done[0].path) } else { "Transcripts".to_string() }
                });
                let dir = first_dir();
                save_text(&format!("{name}.txt"), dir, text);
            }
        }
        t if t >= ROW_TAGS => {
            let (id, action) = ((t / ROW_TAGS) as u64, t % ROW_TAGS);
            let row = STATE.with(|s| {
                s.borrow().as_ref().and_then(|st| st.rows.iter().find(|r| r.id == id).map(|r| (r.path.clone(), r.text.clone(), r.state.clone())))
            });
            let Some((path, text, state)) = row else { return };
            match action {
                ROW_COPY => {
                    copy(&text);
                    flash_copied(Some(id));
                }
                ROW_EXPORT => save_text(&format!("{}.txt", stem(&path)), path.parent().map(Path::to_path_buf), text),
                ROW_REMOVE => remove(id, state),
                _ => {}
            }
        }
        _ => {}
    }
}

fn remove(id: u64, state: RowState) {
    if state == RowState::Waiting {
        QUEUE.lock().unwrap().items.retain(|(i, _)| *i != id);
    }
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut()
            && let Some(i) = st.rows.iter().position(|r| r.id == id)
        {
            let row = st.rows.remove(i);
            row.views.view.removeFromSuperview();
            if state == RowState::Waiting {
                st.batch_total = st.batch_total.saturating_sub(1);
            }
        }
    });
    refresh();
}

/// Stop: waiting files are marked Stopped and the one in progress is ended.
fn stop() {
    let dropped: Vec<u64> = QUEUE.lock().unwrap().items.drain(..).map(|(id, _)| id).collect();
    let current = CURRENT.load(Ordering::SeqCst);
    if current != 0 {
        STOP_ID.store(current, Ordering::SeqCst);
        let pid = DECODE_PID.swap(0, Ordering::SeqCst);
        if pid != 0 {
            // SAFETY: plain kill(2) on our own decoder's pid.
            unsafe { kill(pid as i32, 9) };
        }
        engine::interrupt_file();
    }
    for id in dropped {
        set_state(id, RowState::Stopped, String::new(), 0.0);
    }
}

fn ready(rows: &[Row]) -> Vec<&Row> {
    rows.iter().filter(|r| r.state == RowState::Done && !r.text.is_empty()).collect()
}

/// Every finished file under its name, for Copy and Export in Combined.
fn combined_text() -> Option<String> {
    STATE.with(|s| {
        let s = s.borrow();
        let done = ready(&s.as_ref()?.rows);
        if done.is_empty() {
            return None;
        }
        Some(done.iter().map(|r| format!("{}\n{}", r.name, r.text)).collect::<Vec<_>>().join("\n\n"))
    })
}

fn stem(path: &Path) -> String {
    path.file_stem().map_or_else(|| "Transcript".into(), |s| s.to_string_lossy().into_owned())
}

fn first_dir() -> Option<PathBuf> {
    STATE.with(|s| s.borrow().as_ref()?.rows.first()?.path.parent().map(Path::to_path_buf))
}

fn copy(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&ns(text), unsafe { NSPasteboardTypeString });
}

/// "Copied" on the button for a moment.
fn flash_copied(row: Option<u64>) {
    let b = STATE.with(|s| {
        let s = s.borrow();
        let st = s.as_ref()?;
        match row {
            Some(id) => st.rows.iter().find(|r| r.id == id).map(|r| r.views.copy.clone()),
            None => Some(st.copy_all.clone()),
        }
    });
    let Some(b) = b else { return };
    b.setTitle(&ns("Copied"));
    layout();
    util::main_after(1.5, move || {
        STATE.with(|s| {
            if let Some(st) = s.borrow().as_ref() {
                st.copy_all.setTitle(&ns("Copy"));
                for r in &st.rows {
                    r.views.copy.setTitle(&ns("Copy"));
                }
            }
        });
        layout();
    });
}

fn panel() -> Option<Retained<NSPanel>> {
    STATE.with(|s| s.borrow().as_ref().map(|s| s.panel.clone()))
}

/// A UTType by identifier, looked up at run time (AppKit loads UniformTypeIdentifiers).
fn content_type(identifier: &str) -> Option<Retained<AnyObject>> {
    let cls = AnyClass::get(c"UTType")?;
    unsafe { msg_send![cls, typeWithIdentifier: &*ns(identifier)] }
}

fn allow_types(panel: &NSSavePanel, identifier: &str) {
    if let Some(t) = content_type(identifier) {
        let types = NSArray::from_retained_slice(&[t]);
        let _: () = unsafe { msg_send![panel, setAllowedContentTypes: &*types] };
    }
}

fn choose() {
    let Some(window) = panel() else { return };
    let open = NSOpenPanel::openPanel(mtm());
    open.setCanChooseFiles(true);
    open.setCanChooseDirectories(true);
    open.setAllowsMultipleSelection(true);
    open.setMessage(Some(&ns("Choose audio or video files, or a folder of them")));
    open.setPrompt(Some(&ns("Transcribe")));
    allow_types(&open, "public.audiovisual-content");
    let picker = open.clone();
    let handler = RcBlock::new(move |response: NSModalResponse| {
        if response != NSModalResponseOK {
            return;
        }
        let paths: Vec<PathBuf> =
            picker.URLs().iter().filter_map(|u| u.path()).map(|p| PathBuf::from(p.to_string())).collect();
        util::main_async(move || add(paths));
    });
    open.beginSheetModalForWindow_completionHandler(&window, &handler);
}

/// A save dialog for one text file, starting in `dir` (the recording's folder).
fn save_text(name: &str, dir: Option<PathBuf>, text: String) {
    let Some(window) = panel() else { return };
    let save = NSSavePanel::savePanel(mtm());
    allow_types(&save, "public.plain-text");
    save.setNameFieldStringValue(&ns(name));
    save.setCanCreateDirectories(true);
    if let Some(dir) = dir {
        save.setDirectoryURL(Some(&NSURL::fileURLWithPath(&ns(&dir.to_string_lossy()))));
    }
    let picker = save.clone();
    let handler = RcBlock::new(move |response: NSModalResponse| {
        if response != NSModalResponseOK {
            return;
        }
        let Some(path) = picker.URL().and_then(|u| u.path()) else { return };
        let path = PathBuf::from(path.to_string());
        let message = match std::fs::write(&path, format!("{text}\n")) {
            Ok(()) => format!("Saved {}", file_name(&path)),
            Err(e) => format!("Couldn't save {}: {e}", file_name(&path)),
        };
        set_message(message);
    });
    save.beginSheetModalForWindow_completionHandler(&window, &handler);
}

/// Export All: one text file per transcript in a folder you choose, then show them in Finder.
fn export_all() {
    let Some(window) = panel() else { return };
    let open = NSOpenPanel::openPanel(mtm());
    open.setCanChooseFiles(false);
    open.setCanChooseDirectories(true);
    open.setCanCreateDirectories(true);
    open.setAllowsMultipleSelection(false);
    open.setMessage(Some(&ns("Choose a folder for the transcripts. Each file's text is saved as a .txt file named after it.")));
    open.setPrompt(Some(&ns("Export")));
    if let Some(dir) = first_dir() {
        open.setDirectoryURL(Some(&NSURL::fileURLWithPath(&ns(&dir.to_string_lossy()))));
    }
    let picker = open.clone();
    let handler = RcBlock::new(move |response: NSModalResponse| {
        if response != NSModalResponseOK {
            return;
        }
        let Some(dir) = picker.URL().and_then(|u| u.path()) else { return };
        export_to(PathBuf::from(dir.to_string()));
    });
    open.beginSheetModalForWindow_completionHandler(&window, &handler);
}

/// Save every transcript into `dir`, report it in the status line and show the files in Finder.
fn export_to(dir: PathBuf) {
    {
        let items: Vec<(String, String)> = STATE.with(|s| {
            let s = s.borrow();
            s.as_ref().map_or(Vec::new(), |st| ready(&st.rows).iter().map(|r| (stem(&r.path), r.text.clone())).collect())
        });
        let mut saved = Vec::new();
        let mut failed = 0;
        for (name, text) in items {
            let path = unique_path(&dir, &name, &saved);
            match std::fs::write(&path, format!("{text}\n")) {
                Ok(()) => saved.push(path),
                Err(e) => {
                    util::log(format!("export failed: {e}"));
                    failed += 1;
                }
            }
        }
        let mut message = format!("Saved {} to {}", plural(saved.len(), "transcript"), file_name(&dir));
        if failed > 0 {
            message.push_str(&format!(" · {failed} couldn't be saved"));
        }
        set_message(message);
        if !saved.is_empty() {
            let urls: Vec<Retained<NSURL>> =
                saved.iter().map(|p| NSURL::fileURLWithPath(&ns(&p.to_string_lossy()))).collect();
            NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&urls));
        }
    }
}

/// `dir/name.txt`, or `name 2.txt`, `name 3.txt`… when that's taken on disk or
/// already used in this export. Never overwrites.
fn unique_path(dir: &Path, name: &str, taken: &[PathBuf]) -> PathBuf {
    (1..)
        .map(|n| if n == 1 { dir.join(format!("{name}.txt")) } else { dir.join(format!("{name} {n}.txt")) })
        .find(|p| !p.exists() && !taken.contains(p))
        .expect("an unused name")
}

fn set_message(message: String) {
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.message = Some(message);
        }
    });
    refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_counts_read_naturally() {
        assert_eq!(duration(7.4), "0:07");
        assert_eq!(duration(252.0), "4:12");
        assert_eq!(duration(3729.0), "1:02:09");
        assert_eq!(plural(1, "word"), "1 word");
        assert_eq!(plural(0, "file"), "0 files");
        assert_eq!(plural(1204, "word"), "1,204 words");
        assert_eq!(plural(12_045, "word"), "12,045 words");
    }

    #[test]
    fn export_names_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("tamka-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Interview.txt"), "x").unwrap();
        let first = unique_path(&dir, "Interview", &[]);
        assert_eq!(first, dir.join("Interview 2.txt"));
        let second = unique_path(&dir, "Interview", std::slice::from_ref(&first));
        assert_eq!(second, dir.join("Interview 3.txt"));
        assert_eq!(unique_path(&dir, "Memo", &[]), dir.join("Memo.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folders_expand_to_their_audio_and_video() {
        let dir = std::env::temp_dir().join(format!("tamka-expand-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for f in ["b.m4a", "a.MP3", "notes.txt", ".hidden.wav", "sub/c.mov"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let mut out = Vec::new();
        expand(&dir, &mut out, 0);
        let names: Vec<String> = out.iter().map(|p| p.strip_prefix(&dir).unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.MP3", "b.m4a", "sub/c.mov"]);
        // A file dropped directly is always tried, whatever its extension.
        let mut direct = Vec::new();
        expand(&dir.join("notes.txt"), &mut direct, 0);
        assert_eq!(direct.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
