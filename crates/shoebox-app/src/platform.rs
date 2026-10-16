//! Small platform queries.
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

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
