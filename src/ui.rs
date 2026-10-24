//! The single Objective-C controller that AppKit calls back into (app delegate,
//! menu delegate, menu items, timers), plus small AppKit helpers.

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSFontWeightMedium, NSImage, NSImageSymbolConfiguration, NSMenu,
    NSMenuDelegate,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSNotification, NSObject, NSObjectProtocol, NSURL};

use crate::util::ns;

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "TamkaController"]
    pub struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSApplicationDelegate for Controller {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            crate::app::launched(self.mtm());
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _n: &NSNotification) {
            crate::engine::shutdown();
        }

        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            for url in urls.iter() {
                if let Some(host) = url.host() {
                    crate::app::open_url(&host.to_string());
                }
            }
        }
    }

    unsafe impl NSMenuDelegate for Controller {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            crate::app::menu_will_open();
        }
    }

    impl Controller {
        #[unsafe(method(command:))]
        fn command(&self, sender: Option<&AnyObject>) {
            let tag: isize = match sender {
                Some(s) => unsafe { msg_send![s, tag] },
                None => 0,
            };
            crate::app::handle_tag(tag);
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _timer: Option<&AnyObject>) {
            crate::app::tick();
        }
    }
);

thread_local! {
    static CONTROLLER: OnceCell<Retained<Controller>> = const { OnceCell::new() };
}

/// The single controller instance (target of every menu item and timer).
pub fn controller(mtm: MainThreadMarker) -> Retained<Controller> {
    CONTROLLER.with(|c| {
        c.get_or_init(|| {
            let this = Controller::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        })
        .clone()
    })
}

pub fn app(mtm: MainThreadMarker) -> Retained<NSApplication> {
    NSApplication::sharedApplication(mtm)
}

pub fn set_app_delegate(mtm: MainThreadMarker) {
    let c = controller(mtm);
    app(mtm).setDelegate(Some(ProtocolObject::from_ref(&*c)));
}

pub fn menu_delegate(mtm: MainThreadMarker) -> Retained<ProtocolObject<dyn NSMenuDelegate>> {
    ProtocolObject::from_retained(controller(mtm))
}

/// A coloured state symbol (Photon's vocabulary: green check = fine, orange
/// triangle = needs attention). Filled symbols get a white glyph on the colour,
/// so the check or "!" stays visible instead of becoming a solid dot.
pub fn state_symbol(name: &str, points: f64, color: Retained<objc2_app_kit::NSColor>) -> Option<Retained<NSImage>> {
    let img = symbol(name, points)?;
    let colors = if name.ends_with(".fill") {
        objc2_foundation::NSArray::from_retained_slice(&[objc2_app_kit::NSColor::whiteColor(), color])
    } else {
        objc2_foundation::NSArray::from_retained_slice(&[color])
    };
    let cfg = NSImageSymbolConfiguration::configurationWithPaletteColors(&colors);
    Some(img.imageWithSymbolConfiguration(&cfg).unwrap_or(img))
}

/// An SF Symbol, optionally sized.
pub fn symbol(name: &str, points: f64) -> Option<Retained<NSImage>> {
    let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(name), None)?;
    if points > 0.0 {
        let cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight(points, unsafe { NSFontWeightMedium });
        return img.imageWithSymbolConfiguration(&cfg).or(Some(img));
    }
    Some(img)
}
