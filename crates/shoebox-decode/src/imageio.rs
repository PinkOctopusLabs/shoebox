//! Thin FFI wrapper over macOS ImageIO + CoreGraphics: decode a downscaled
//! thumbnail of any format ImageIO understands (JPEG, TIFF, PNG, HEIC, NEF)
//! straight to 8-bit grayscale.
#![allow(unsafe_code)]

use std::ffi::c_void;

use crate::{DecodeError, Gray};

type CFTypeRef = *const c_void;
type CFIndex = isize;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[repr(C)]
struct CFDictionaryKeyCallBacks {
    _private: [u8; 0],
}

#[repr(C)]
struct CFDictionaryValueCallBacks {
    _private: [u8; 0],
}

const K_CF_NUMBER_SINT32: CFIndex = 3;
const K_CG_IMAGE_ALPHA_NONE: u32 = 0;
const K_CG_IMAGE_ALPHA_NONE_SKIP_LAST: u32 = 5;
const K_CG_INTERPOLATION_HIGH: i32 = 3;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFAllocatorNull: CFTypeRef;
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    fn CFDataCreateWithBytesNoCopy(
        alloc: CFTypeRef,
        bytes: *const u8,
        len: CFIndex,
        dealloc: CFTypeRef,
    ) -> CFTypeRef;
    fn CFNumberCreate(alloc: CFTypeRef, kind: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFDictionaryCreate(
        alloc: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: CFIndex,
        key_cb: *const CFDictionaryKeyCallBacks,
        value_cb: *const CFDictionaryValueCallBacks,
    ) -> CFTypeRef;
    fn CFStringCreateWithBytes(
        alloc: CFTypeRef,
        bytes: *const u8,
        len: CFIndex,
        encoding: u32,
        external: u8,
    ) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
    static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
    static kCGImageSourceCreateThumbnailWithTransform: CFTypeRef;
    static kCGImageSourceShouldCacheImmediately: CFTypeRef;
    static kCGImageSourceTypeIdentifierHint: CFTypeRef;
    fn CGImageSourceCreateWithData(data: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
    fn CGImageSourceCreateThumbnailAtIndex(
        src: CFTypeRef,
        index: usize,
        options: CFTypeRef,
    ) -> CFTypeRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGImageGetWidth(img: CFTypeRef) -> usize;
    fn CGImageGetHeight(img: CFTypeRef) -> usize;
    static kCGColorSpaceSRGB: CFTypeRef;
    static kCGColorSpaceDisplayP3: CFTypeRef;
    fn CGColorSpaceCreateDeviceGray() -> CFTypeRef;
    fn CGColorSpaceCreateWithName(name: CFTypeRef) -> CFTypeRef;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CFTypeRef,
        bitmap_info: u32,
    ) -> CFTypeRef;
    fn CGContextSetInterpolationQuality(ctx: CFTypeRef, quality: i32);
    fn CGContextDrawImage(ctx: CFTypeRef, rect: CGRect, img: CFTypeRef);
}

/// Owned CoreFoundation reference, released on drop.
struct Cf(CFTypeRef);

impl Cf {
    fn new(r: CFTypeRef, what: &str) -> Result<Self, DecodeError> {
        if r.is_null() {
            Err(DecodeError::Format(format!("ImageIO: {what} failed")))
        } else {
            Ok(Self(r))
        }
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: self.0 is a non-null CF object we own (Create rule).
        unsafe { CFRelease(self.0) }
    }
}

/// Uniform Type Identifier ImageIO needs to tell a NEF from a plain TIFF.
pub fn type_hint(format: crate::Format) -> &'static str {
    match format {
        crate::Format::Nef => "com.nikon.raw-image",
        crate::Format::Jpeg => "public.jpeg",
        crate::Format::Tiff => "public.tiff",
        crate::Format::Png => "public.png",
        crate::Format::Heic => "public.heic",
    }
}

#[derive(Clone, Copy)]
enum Out {
    Gray,
    /// Colour-managed by ColorSync into the target space.
    Rgb(crate::Target),
}

/// Decode a thumbnail whose long edge is at most `max_px`, as 8-bit gray.
/// ImageIO uses DCT scaling for JPEG and embedded previews for RAW where it
/// can. `apply_orientation` rotates by the file's EXIF Orientation. `uti`
/// (see [`type_hint`]) is needed for RAW files, which look like TIFFs.
pub fn thumbnail_luma(
    data: &[u8],
    max_px: u32,
    apply_orientation: bool,
    uti: Option<&str>,
) -> Result<Gray, DecodeError> {
    let (w, h, px) = render(data, max_px, apply_orientation, uti, Out::Gray)?;
    Gray::new(w, h, px)
}

/// Decode to sRGB (ImageIO/ColorSync does the colour management), with the
/// long edge at most `max_px`.
pub fn thumbnail_srgb(
    data: &[u8],
    max_px: u32,
    apply_orientation: bool,
    uti: Option<&str>,
) -> Result<crate::Rgb8, DecodeError> {
    thumbnail_rgb(data, max_px, apply_orientation, uti, crate::Target::Srgb)
}

/// Decode to RGB in `target`'s colour space via ColorSync. Used for HEIC,
/// which has no pure-Rust decoder, and as the reference in colour tests.
pub fn thumbnail_rgb(
    data: &[u8],
    max_px: u32,
    apply_orientation: bool,
    uti: Option<&str>,
    target: crate::Target,
) -> Result<crate::Rgb8, DecodeError> {
    let (w, h, rgbx) = render(data, max_px, apply_orientation, uti, Out::Rgb(target))?;
    let rgb = rgbx
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    crate::Rgb8::new(w, h, rgb)
}

fn render(
    data: &[u8],
    max_px: u32,
    apply_orientation: bool,
    uti: Option<&str>,
    out: Out,
) -> Result<(u32, u32, Vec<u8>), DecodeError> {
    let len = CFIndex::try_from(data.len()).map_err(|_| DecodeError::Truncated)?;
    let max =
        i32::try_from(max_px).map_err(|_| DecodeError::Unsupported("thumbnail size".into()))?;
    // SAFETY: every pointer passed below is either valid for the duration of
    // the call (data, max, keys/values arrays) or a CF object kept alive by a
    // `Cf` guard; `data` outlives `cfdata` because both live in this frame and
    // the no-copy CFData uses kCFAllocatorNull (it never frees the bytes).
    // The bitmap context writes only inside `px`, sized for w × h × bpp.
    unsafe {
        let cfdata = Cf::new(
            CFDataCreateWithBytesNoCopy(std::ptr::null(), data.as_ptr(), len, kCFAllocatorNull),
            "CFData",
        )?;
        let hint = match uti {
            Some(u) => Some(Cf::new(
                CFStringCreateWithBytes(
                    std::ptr::null(),
                    u.as_ptr(),
                    u.len() as CFIndex,
                    K_CF_STRING_ENCODING_UTF8,
                    0,
                ),
                "CFString",
            )?),
            None => None,
        };
        let src_opts = match &hint {
            Some(h) => Some(Cf::new(
                CFDictionaryCreate(
                    std::ptr::null(),
                    [kCGImageSourceTypeIdentifierHint].as_ptr(),
                    [h.0].as_ptr(),
                    1,
                    &raw const kCFTypeDictionaryKeyCallBacks,
                    &raw const kCFTypeDictionaryValueCallBacks,
                ),
                "source options",
            )?),
            None => None,
        };
        let src = Cf::new(
            CGImageSourceCreateWithData(
                cfdata.0,
                src_opts.as_ref().map_or(std::ptr::null(), |o| o.0),
            ),
            "image source",
        )?;
        let size = Cf::new(
            CFNumberCreate(
                std::ptr::null(),
                K_CF_NUMBER_SINT32,
                (&raw const max).cast(),
            ),
            "CFNumber",
        )?;
        let keys = [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceThumbnailMaxPixelSize,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceShouldCacheImmediately,
        ];
        let values = [
            kCFBooleanTrue,
            size.0,
            if apply_orientation {
                kCFBooleanTrue
            } else {
                kCFBooleanFalse
            },
            kCFBooleanTrue,
        ];
        let opts = Cf::new(
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                keys.len() as CFIndex,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            ),
            "options",
        )?;
        let img = Cf::new(
            CGImageSourceCreateThumbnailAtIndex(src.0, 0, opts.0),
            "thumbnail decode",
        )?;
        let (w, h) = (CGImageGetWidth(img.0), CGImageGetHeight(img.0));
        let (w32, h32) = (
            u32::try_from(w).map_err(|_| DecodeError::Truncated)?,
            u32::try_from(h).map_err(|_| DecodeError::Truncated)?,
        );
        crate::check_dimensions(w32, h32)?;
        let (bpp, space, info) = match out {
            Out::Gray => (1, CGColorSpaceCreateDeviceGray(), K_CG_IMAGE_ALPHA_NONE),
            Out::Rgb(t) => (
                4,
                CGColorSpaceCreateWithName(match t {
                    crate::Target::Srgb => kCGColorSpaceSRGB,
                    crate::Target::DisplayP3 => kCGColorSpaceDisplayP3,
                }),
                K_CG_IMAGE_ALPHA_NONE_SKIP_LAST,
            ),
        };
        let space = Cf::new(space, "colour space")?;
        let mut px = vec![0u8; w * h * bpp];
        let ctx = Cf::new(
            CGBitmapContextCreate(px.as_mut_ptr().cast(), w, h, 8, w * bpp, space.0, info),
            "bitmap context",
        )?;
        CGContextSetInterpolationQuality(ctx.0, K_CG_INTERPOLATION_HIGH);
        let rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: w as f64,
                height: h as f64,
            },
        };
        CGContextDrawImage(ctx.0, rect, img.0);
        drop(ctx); // finish drawing before `px` is read
        Ok((w32, h32, px))
    }
}
