//! App updates through Sparkle (macOS). `scripts/bundle-macos.sh` embeds
//! `Sparkle.framework` and puts the feed URL and the EdDSA public key in
//! Info.plist; this loads the framework at runtime, starts Sparkle's standard
//! updater (scheduled background checks plus Shoebox > Check for Updates…)
//! and holds a relaunch back while the library worker is handling a command,
//! so an update never interrupts a file operation.
//!
//! Without the framework (`cargo run`, tests) or the public key, nothing
//! happens. On Linux there is no updater.
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

/// Start the updater, if this is a bundled build with Sparkle in it. Call
/// once, on the main thread, after the window exists.
pub fn start(ctx: &eframe::egui::Context) {
    #[cfg(target_os = "macos")]
    mac::start(ctx);
    #[cfg(not(target_os = "macos"))]
    let _ = ctx;
}

/// Run a relaunch Sparkle was holding back, once the library worker is idle.
/// Call every frame, on the main thread.
pub fn tick(ctx: &eframe::egui::Context, working: bool) {
    #[cfg(target_os = "macos")]
    mac::tick(ctx, working);
    #[cfg(not(target_os = "macos"))]
    let _ = (ctx, working);
}

/// The updater's preferences, when this build has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefs {
    /// Check once a day.
    pub auto_check: bool,
    /// Download and install updates (on quit) without asking.
    pub auto_install: bool,
}

/// The current preferences, or None when this build has no updater.
pub fn prefs() -> Option<Prefs> {
    #[cfg(target_os = "macos")]
    return mac::prefs();
    #[cfg(not(target_os = "macos"))]
    None
}

pub fn set_prefs(p: Prefs) {
    #[cfg(target_os = "macos")]
    mac::set_prefs(p);
    #[cfg(not(target_os = "macos"))]
    let _ = p;
}

/// Check now, showing Sparkle's window with the result.
pub fn check_now() {
    #[cfg(target_os = "macos")]
    mac::check_now();
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use std::time::Duration;

    use block2::{Block, RcBlock};
    use eframe::egui;
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject};
    use objc2::{ClassType, define_class, msg_send, sel};
    use objc2_foundation::{NSBundle, NSString};

    struct State {
        ctx: egui::Context,
        /// Sparkle holds only weak references to these.
        controller: Retained<AnyObject>,
        _delegate: Retained<Delegate>,
        /// Sparkle's "go ahead and relaunch", held while the worker is busy.
        relaunch: Option<RcBlock<dyn Fn()>>,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this class
        // has no ivars and no Drop.
        #[unsafe(super(NSObject))]
        #[name = "ShoeboxUpdaterDelegate"]
        struct Delegate;

        /// The parts of `SPUUpdaterDelegate` Shoebox uses (Sparkle asks with
        /// `respondsToSelector:`, so the protocol needn't be declared).
        impl Delegate {
            #[unsafe(method(updater:shouldPostponeRelaunchForUpdate:untilInvokingBlock:))]
            fn postpone_relaunch(
                &self,
                _updater: &AnyObject,
                _item: &AnyObject,
                install: &Block<dyn Fn()>,
            ) -> Bool {
                let postpone = crate::jobs::working()
                    && STATE.with_borrow_mut(|s| match s {
                        Some(s) => {
                            s.relaunch = Some(install.copy());
                            s.ctx.request_repaint();
                            true
                        }
                        None => false,
                    });
                if postpone {
                    tracing::info!("update ready: relaunching when the current operation finishes");
                }
                Bool::new(postpone)
            }

            /// A test feed (`SHOEBOX_UPDATE_FEED`), else Info.plist's.
            #[unsafe(method_id(feedURLStringForUpdater:))]
            fn feed_url(&self, _updater: &AnyObject) -> Option<Retained<NSString>> {
                std::env::var("SHOEBOX_UPDATE_FEED")
                    .ok()
                    .map(|u| NSString::from_str(&u))
            }
        }
    );

    pub fn start(ctx: &egui::Context) {
        if STATE.with_borrow(Option::is_some) {
            return;
        }
        let main = NSBundle::mainBundle();
        let has_key = main
            .objectForInfoDictionaryKey(&NSString::from_str("SUPublicEDKey"))
            .is_some();
        let Some(frameworks) = main.privateFrameworksPath() else {
            return;
        };
        let path = format!("{frameworks}/Sparkle.framework");
        if !has_key || !std::path::Path::new(&path).exists() {
            tracing::debug!("no Sparkle in this build; updates are off");
            return;
        }
        let Some(sparkle) = NSBundle::bundleWithPath(&NSString::from_str(&path)) else {
            return;
        };
        // SAFETY: loads Sparkle's code (our own signed, embedded copy).
        if let Err(e) = unsafe { sparkle.loadAndReturnError() } {
            tracing::warn!("couldn't load Sparkle, so updates are off: {e:?}");
            return;
        }
        let Some(cls) = AnyClass::get(c"SPUStandardUpdaterController") else {
            tracing::warn!("Sparkle has no SPUStandardUpdaterController; updates are off");
            return;
        };
        // SAFETY: `Delegate` is an NSObject subclass; `new` is NSObject's.
        let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::class(), new] };
        // SAFETY: the documented initializer of SPUStandardUpdaterController
        // (BOOL, id<SPUUpdaterDelegate>, id<SPUStandardUserDriverDelegate>),
        // called on the main thread as Sparkle requires.
        let controller: Retained<AnyObject> = unsafe {
            let alloc: Allocated<AnyObject> = msg_send![cls, alloc];
            msg_send![
                alloc,
                initWithStartingUpdater: true,
                updaterDelegate: &*delegate,
                userDriverDelegate: Option::<&AnyObject>::None
            ]
        };
        add_menu_item(&controller);
        if std::env::var_os("SHOEBOX_UPDATE_FEED").is_some() {
            // Testing a feed: check now instead of on Sparkle's schedule.
            // SAFETY: SPUStandardUpdaterController's `updater` property and
            // SPUUpdater's `checkForUpdatesInBackground`, on the main thread.
            unsafe {
                let updater: Retained<AnyObject> = msg_send![&*controller, updater];
                let _: () = msg_send![&*updater, checkForUpdatesInBackground];
            }
        }
        tracing::info!("updates on (Sparkle)");
        STATE.set(Some(State {
            ctx: ctx.clone(),
            controller,
            _delegate: delegate,
            relaunch: None,
        }));
    }

    /// "Check for Updates…" under About in the app menu (winit's default
    /// menu), sending `checkForUpdates:` to the controller.
    fn add_menu_item(controller: &AnyObject) {
        let (Some(app_cls), Some(item_cls)) = (
            AnyClass::get(c"NSApplication"),
            AnyClass::get(c"NSMenuItem"),
        ) else {
            return;
        };
        // SAFETY: standard AppKit menu calls on the main thread; every object
        // is checked for nil before use.
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
                initWithTitle: &*NSString::from_str("Check for Updates…"),
                action: sel!(checkForUpdates:),
                keyEquivalent: &*NSString::from_str("")
            ];
            let _: () = msg_send![&*item, setTarget: controller];
            let count: isize = msg_send![&*app_menu, numberOfItems];
            let _: () = msg_send![&*app_menu, insertItem: &*item, atIndex: count.min(1)];
        }
    }

    /// SPUStandardUpdaterController's `updater`, if the updater runs.
    fn updater() -> Option<Retained<AnyObject>> {
        STATE.with_borrow(|s| {
            let s = s.as_ref()?;
            // SAFETY: the controller's `updater` property, on the main thread.
            let u: Option<Retained<AnyObject>> = unsafe { msg_send![&*s.controller, updater] };
            u
        })
    }

    pub fn prefs() -> Option<super::Prefs> {
        let u = updater()?;
        // SAFETY: SPUUpdater's BOOL properties, on the main thread.
        unsafe {
            Some(super::Prefs {
                auto_check: msg_send![&*u, automaticallyChecksForUpdates],
                auto_install: msg_send![&*u, automaticallyDownloadsUpdates],
            })
        }
    }

    pub fn set_prefs(p: super::Prefs) {
        let Some(u) = updater() else { return };
        // SAFETY: SPUUpdater's BOOL property setters, on the main thread.
        unsafe {
            let _: () = msg_send![&*u, setAutomaticallyChecksForUpdates: p.auto_check];
            let _: () = msg_send![&*u, setAutomaticallyDownloadsUpdates: p.auto_install];
        }
    }

    pub fn check_now() {
        // Out of the RefCell first: Sparkle may call the delegate back.
        let Some(c) = STATE.with_borrow(|s| s.as_ref().map(|s| s.controller.clone())) else {
            return;
        };
        // SAFETY: the controller's documented action, on the main thread.
        unsafe {
            let _: () = msg_send![&*c, checkForUpdates: Option::<&AnyObject>::None];
        }
    }

    pub fn tick(ctx: &egui::Context, working: bool) {
        let ready = STATE.with_borrow_mut(|s| {
            let s = s.as_mut()?;
            s.relaunch.as_ref()?;
            if working {
                ctx.request_repaint_after(Duration::from_millis(500));
                None
            } else {
                s.relaunch.take()
            }
        });
        if let Some(relaunch) = ready {
            tracing::info!("relaunching to finish the update");
            relaunch.call(());
        }
    }
}
