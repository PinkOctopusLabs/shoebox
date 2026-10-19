//! "Settings…" (⌘,) in the macOS app menu. winit's default menu has no such
//! item, so it's added here, with a tiny Objective-C target that raises a
//! flag the app reads each frame. Elsewhere the app's own ⌘/Ctrl+, shortcut
//! and the left panel's Settings… entry do the same.
//!
//! The menu's Quit (⌘Q) is pointed at the same target, so it asks the app
//! (which may want to finish an import first) instead of terminating at once.
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

use std::sync::atomic::{AtomicBool, Ordering};

static OPEN_SETTINGS: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);

/// Add the menu item. Call once, on the main thread, after the window exists.
pub fn install(ctx: &eframe::egui::Context) {
    #[cfg(target_os = "macos")]
    mac::install(ctx);
    #[cfg(not(target_os = "macos"))]
    let _ = ctx;
}

/// Whether Settings… was chosen since the last call.
pub fn take_open_settings() -> bool {
    OPEN_SETTINGS.swap(false, Ordering::AcqRel)
}

/// Whether Quit was chosen since the last call.
pub fn take_quit() -> bool {
    QUIT.swap(false, Ordering::AcqRel)
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::OnceLock;

    use eframe::egui;
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, NSObject};
    use objc2::{ClassType, define_class, msg_send, sel};
    use objc2_foundation::NSString;

    static CTX: OnceLock<egui::Context> = OnceLock::new();

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this class
        // has no ivars and no Drop.
        #[unsafe(super(NSObject))]
        #[name = "ShoeboxMenuTarget"]
        struct Target;

        impl Target {
            #[unsafe(method(openSettings:))]
            fn open_settings(&self, _sender: Option<&AnyObject>) {
                super::OPEN_SETTINGS.store(true, std::sync::atomic::Ordering::Release);
                if let Some(ctx) = CTX.get() {
                    ctx.request_repaint();
                }
            }

            #[unsafe(method(requestQuit:))]
            fn request_quit(&self, _sender: Option<&AnyObject>) {
                super::QUIT.store(true, std::sync::atomic::Ordering::Release);
                if let Some(ctx) = CTX.get() {
                    ctx.request_repaint();
                }
            }
        }
    );

    pub fn install(ctx: &egui::Context) {
        if CTX.set(ctx.clone()).is_err() {
            return; // already installed
        }
        let (Some(app_cls), Some(item_cls)) = (
            AnyClass::get(c"NSApplication"),
            AnyClass::get(c"NSMenuItem"),
        ) else {
            return;
        };
        // SAFETY: `Target` is an NSObject subclass; `new` is NSObject's.
        let target: Retained<Target> = unsafe { msg_send![Target::class(), new] };
        // SAFETY: standard AppKit menu calls on the main thread; every object
        // is checked for nil before use. The menu item doesn't retain its
        // target, so the target is leaked (one small object for the app's
        // lifetime).
        unsafe {
            let app: Option<Retained<AnyObject>> = msg_send![app_cls, sharedApplication];
            let Some(app) = app else { return };
            let menu: Option<Retained<AnyObject>> = msg_send![&*app, mainMenu];
            let Some(menu) = menu else { return };
            let first: Option<Retained<AnyObject>> = msg_send![&*menu, itemAtIndex: 0isize];
            let Some(first) = first else { return };
            let app_menu: Option<Retained<AnyObject>> = msg_send![&*first, submenu];
            let Some(app_menu) = app_menu else { return };
            let alloc: Allocated<AnyObject> = msg_send![item_cls, alloc];
            let item: Retained<AnyObject> = msg_send![
                alloc,
                initWithTitle: &*NSString::from_str("Settings…"),
                action: sel!(openSettings:),
                keyEquivalent: &*NSString::from_str(",")
            ];
            let _: () = msg_send![&*item, setTarget: &*target];
            let count: isize = msg_send![&*app_menu, numberOfItems];
            let _: () = msg_send![&*app_menu, insertItem: &*item, atIndex: count.min(1)];
            // Quit asks the app instead of terminating.
            let count: isize = msg_send![&*app_menu, numberOfItems];
            for i in 0..count {
                let it: Option<Retained<AnyObject>> = msg_send![&*app_menu, itemAtIndex: i];
                let Some(it) = it else { continue };
                let action: Option<objc2::runtime::Sel> = msg_send![&*it, action];
                if action == Some(sel!(terminate:)) {
                    let _: () = msg_send![&*it, setAction: sel!(requestQuit:)];
                    let _: () = msg_send![&*it, setTarget: &*target];
                }
            }
        }
        std::mem::forget(target);
    }
}
