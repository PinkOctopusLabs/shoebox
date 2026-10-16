//! Display colour space. On macOS the window's Metal layer is tagged as
//! Display P3 and images are rendered to Display P3, so macOS colour-matches
//! them to the actual screen (wide-gamut or not) the way Preview.app does.
//! Elsewhere everything stays sRGB.
//!
//! egui-wgpu has no option for the surface colour space, and wgpu resets the
//! layer's colour space whenever it reconfigures the surface (on resize), so
//! [`DisplayTagger::ensure`] re-applies the tag every frame (cheap: two
//! property reads unless it changed).
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

use shoebox_decode::Target;

pub struct DisplayTagger {
    #[cfg(target_os = "macos")]
    view: std::ptr::NonNull<std::ffi::c_void>,
}

impl DisplayTagger {
    /// The tagger for this window, or None when there's no native window
    /// (tests) or the platform isn't macOS.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let handle = cc.window_handle().ok()?;
            match handle.as_raw() {
                RawWindowHandle::AppKit(h) => Some(Self { view: h.ns_view }),
                _ => None,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = cc;
            None
        }
    }

    /// The colour space images should be rendered in for this display.
    pub fn target(tagger: Option<&Self>) -> Target {
        if tagger.is_some() {
            Target::DisplayP3
        } else {
            Target::Srgb
        }
    }

    /// Make sure the window's Metal layer is tagged Display P3. Call on the
    /// main thread (eframe's `ui` runs there).
    pub fn ensure(&self) {
        #[cfg(target_os = "macos")]
        // SAFETY: `view` is the NSView of eframe's window, which lives for the
        // whole app; we only send standard CALayer/CAMetalLayer messages to
        // objects it returns, on the main thread.
        unsafe {
            use objc2::msg_send;
            use objc2::rc::Retained;
            use objc2::runtime::AnyObject;
            use objc2_core_graphics::{CGColorSpace, kCGColorSpaceDisplayP3};
            use objc2_quartz_core::{CALayer, CAMetalLayer};

            let view: &AnyObject = self.view.cast::<AnyObject>().as_ref();
            let root: Option<Retained<CALayer>> = msg_send![view, layer];
            let Some(root) = root else { return };
            let Some(subs) = root.sublayers() else { return };
            for layer in subs.iter() {
                let Ok(metal) = layer.downcast::<CAMetalLayer>() else {
                    continue;
                };
                let current = metal.colorspace();
                let is_p3 = CGColorSpace::name(current.as_deref())
                    .is_some_and(|n| *n == *kCGColorSpaceDisplayP3);
                if !is_p3 {
                    let p3 = CGColorSpace::with_name(Some(kCGColorSpaceDisplayP3));
                    metal.setColorspace(p3.as_deref());
                    tracing::debug!("tagged the window's Metal layer as Display P3");
                }
            }
        }
    }
}
