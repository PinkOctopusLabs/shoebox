//! Small platform queries.
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

/// Keeps the Mac from sleeping while it's held (a long iCloud Photos
/// sync): `NSProcessInfo`'s user-initiated activity. Dropping it ends it.
pub struct Awake {
    #[cfg(target_os = "macos")]
    activity:
        objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
}

pub fn keep_awake(reason: &str) -> Awake {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
        let activity = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
            NSActivityOptions::UserInitiated,
            &NSString::from_str(reason),
        );
        Awake { activity }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = reason;
        Awake {}
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        // SAFETY: ends the activity this value began, once.
        unsafe {
            objc2_foundation::NSProcessInfo::processInfo().endActivity(&self.activity);
        }
    }
}

/// Whether Caps Lock is on (Lightroom's auto-advance switch). macOS only;
/// elsewhere use the toolbar toggle.
pub fn caps_lock() -> bool {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGEventSourceFlagsState(state_id: i32) -> u64;
        }
        const COMBINED_SESSION_STATE: i32 = 0;
        const ALPHA_SHIFT: u64 = 0x0001_0000;
        // SAFETY: a pure query of the current modifier flags.
        unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) & ALPHA_SHIFT != 0 }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// macOS: the top bar is drawn in the title bar's place (full-size content
/// view), but AppKit still treats every click there as on the title bar: a
/// quick double-click on a button zooms the window. Tell it our view never
/// moves the window; the top bar moves and zooms it itself, from its empty
/// space only. Elsewhere there's nothing to do.
pub fn own_titlebar_clicks(cc: &eframe::CreationContext<'_>) {
    #[cfg(target_os = "macos")]
    {
        use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        extern "C-unwind" fn never(_this: *mut AnyObject, _cmd: Sel) -> Bool {
            Bool::NO
        }
        let Ok(handle) = cc.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        // SAFETY: `ns_view` is eframe's live NSView. We add one method to its
        // class (winit's own view class, which doesn't define it), with the
        // signature AppKit expects: `- (BOOL)mouseDownCanMoveWindow`
        // (BOOL is `bool`, "B", on Apple silicon; `signed char`, "c", on Intel).
        unsafe {
            let view: &AnyObject = h.ns_view.cast::<AnyObject>().as_ref();
            let class: *const AnyClass = view.class();
            let imp: Imp = std::mem::transmute::<
                extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool,
                Imp,
            >(never);
            let added = objc2::ffi::class_addMethod(
                class.cast_mut(),
                objc2::sel!(mouseDownCanMoveWindow),
                imp,
                if cfg!(target_arch = "aarch64") {
                    c"B@:"
                } else {
                    c"c@:"
                }
                .as_ptr(),
            );
            tracing::debug!("title bar clicks kept for the app: {}", added.as_bool());
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cc;
    }
}

/// What a double-click on the title bar does (System Settings → Desktop &
/// Dock → "Double-click a window's title bar to").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // always Zoom there
pub enum TitlebarDoubleClick {
    Zoom,
    Minimize,
    Nothing,
}

pub fn titlebar_double_click() -> TitlebarDoubleClick {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSString, NSUserDefaults};
        let defaults = NSUserDefaults::standardUserDefaults();
        let action = defaults.stringForKey(&NSString::from_str("AppleActionOnDoubleClick"));
        match action.map(|a| a.to_string()).as_deref() {
            Some("Minimize") => TitlebarDoubleClick::Minimize,
            Some("None") => TitlebarDoubleClick::Nothing,
            _ => TitlebarDoubleClick::Zoom,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        TitlebarDoubleClick::Zoom
    }
}
