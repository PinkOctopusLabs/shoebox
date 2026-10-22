//! Subject and person masks from Apple's Vision framework (EDITING-SPEC,
//! Local adjustments: AI subject / person), on the device. A thin FFI
//! wrapper: pixels in, a coverage mask out, nothing else. Other systems
//! have no Vision: [`segment`] says so.
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

use crate::MlError;

/// What to find.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Find {
    /// Every foreground object (people, animals, things in front).
    Subject,
    /// People, their hair and clothes.
    Person,
}

/// A coverage mask, one byte per pixel (0: none, 255: all), `width`×`height`.
#[derive(Debug, Clone, PartialEq)]
pub struct Coverage {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// Find `what` in an upright 8-bit sRGB image (`rgb`, `width`×`height`,
/// three bytes a pixel). The mask is the image's size; all zero when
/// there's nothing to find.
pub fn segment(rgb: &[u8], width: usize, height: usize, what: Find) -> Result<Coverage, MlError> {
    if width == 0 || height == 0 || rgb.len() < width * height * 3 {
        return Err(MlError::Vision("no image".into()));
    }
    imp::segment(rgb, width, height, what)
}

/// `src` (`sw`×`sh`) resampled bilinearly to `w`×`h`.
fn resize(src: &[f32], sw: usize, sh: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let fy = ((y as f32 + 0.5) * sh as f32 / h as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy - fy.floor());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * sw as f32 / w as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx - fx.floor());
            let x1 = (x0 + 1).min(sw - 1);
            let at = |xx: usize, yy: usize| src[yy * sw + xx];
            let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
            let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
            out[y * w + x] = ((top + (bot - top) * ty).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    use objc2::AllocAnyThread;
    use objc2::rc::autoreleasepool;
    use objc2_core_graphics::{
        CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage,
        CGImageAlphaInfo,
    };
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
        CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
        CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        kCVPixelFormatType_OneComponent8, kCVPixelFormatType_OneComponent32Float,
    };
    use objc2_foundation::{NSArray, NSDictionary};
    use objc2_vision::{
        VNGenerateForegroundInstanceMaskRequest, VNGeneratePersonSegmentationRequest,
        VNGeneratePersonSegmentationRequestQualityLevel, VNImageRequestHandler, VNRequest,
    };

    use super::{Coverage, Find, resize};
    use crate::MlError;

    fn err(what: &str, e: impl std::fmt::Display) -> MlError {
        MlError::Vision(format!("{what}: {e}"))
    }

    pub fn segment(rgb: &[u8], w: usize, h: usize, what: Find) -> Result<Coverage, MlError> {
        // RGBX, as CoreGraphics takes it; kept alive until Vision is done.
        let mut rgbx = Vec::with_capacity(w * h * 4);
        for p in rgb[..w * h * 3].as_chunks::<3>().0 {
            rgbx.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
        autoreleasepool(|_| {
            // SAFETY: `rgbx` outlives the provider and the image (both are
            // dropped at the end of this closure) and isn't changed.
            let provider = unsafe {
                CGDataProvider::with_data(
                    std::ptr::null_mut(),
                    rgbx.as_ptr().cast::<c_void>(),
                    rgbx.len(),
                    None,
                )
            }
            .ok_or_else(|| MlError::Vision("no data provider".into()))?;
            let space = CGColorSpace::new_device_rgb()
                .ok_or_else(|| MlError::Vision("no colour space".into()))?;
            // SAFETY: the sizes describe `rgbx` exactly.
            let image = unsafe {
                CGImage::new(
                    w,
                    h,
                    8,
                    32,
                    w * 4,
                    Some(&space),
                    CGBitmapInfo(CGImageAlphaInfo::NoneSkipLast.0),
                    Some(&provider),
                    std::ptr::null(),
                    false,
                    CGColorRenderingIntent::RenderingIntentDefault,
                )
            }
            .ok_or_else(|| MlError::Vision("no image".into()))?;
            let options = NSDictionary::new();
            // SAFETY: an empty options dictionary is of the right type.
            let handler = unsafe {
                VNImageRequestHandler::initWithCGImage_options(
                    VNImageRequestHandler::alloc(),
                    &image,
                    &options,
                )
            };
            match what {
                Find::Subject => {
                    // SAFETY: a plain init.
                    let request = unsafe { VNGenerateForegroundInstanceMaskRequest::new() };
                    let list = NSArray::from_retained_slice(&[objc2::rc::Retained::into_super(
                        objc2::rc::Retained::into_super(request.clone()),
                    )]);
                    let list: objc2::rc::Retained<NSArray<VNRequest>> = list;
                    handler
                        .performRequests_error(&list)
                        .map_err(|e| err("subject", e.localizedDescription()))?;
                    // SAFETY: results after a successful perform.
                    let results = unsafe { request.results() };
                    let Some(obs) = results.and_then(|r| r.firstObject()) else {
                        return Ok(Coverage {
                            width: w,
                            height: h,
                            data: vec![0; w * h],
                        });
                    };
                    // SAFETY: instances from this observation, with its handler.
                    let buffer = unsafe {
                        obs.generateScaledMaskForImageForInstances_fromRequestHandler_error(
                            &obs.allInstances(),
                            &handler,
                        )
                    }
                    .map_err(|e| err("subject mask", e.localizedDescription()))?;
                    read(&buffer, w, h)
                }
                Find::Person => {
                    // SAFETY: a plain init and two setters with valid values.
                    let request = unsafe { VNGeneratePersonSegmentationRequest::new() };
                    unsafe {
                        request.setQualityLevel(
                            VNGeneratePersonSegmentationRequestQualityLevel::Accurate,
                        );
                        request.setOutputPixelFormat(kCVPixelFormatType_OneComponent8);
                    }
                    let list: objc2::rc::Retained<NSArray<VNRequest>> =
                        NSArray::from_retained_slice(&[objc2::rc::Retained::into_super(
                            objc2::rc::Retained::into_super(objc2::rc::Retained::into_super(
                                request.clone(),
                            )),
                        )]);
                    handler
                        .performRequests_error(&list)
                        .map_err(|e| err("person", e.localizedDescription()))?;
                    // SAFETY: results after a successful perform.
                    let results = unsafe { request.results() };
                    let Some(obs) = results.and_then(|r| r.firstObject()) else {
                        return Ok(Coverage {
                            width: w,
                            height: h,
                            data: vec![0; w * h],
                        });
                    };
                    // SAFETY: the observation's own buffer.
                    let buffer = unsafe { obs.pixelBuffer() };
                    read(&buffer, w, h)
                }
            }
        })
    }

    /// A one-component mask buffer (8-bit or float) as `w`×`h` bytes.
    fn read(buffer: &CVPixelBuffer, w: usize, h: usize) -> Result<Coverage, MlError> {
        let (bw, bh) = (
            CVPixelBufferGetWidth(buffer),
            CVPixelBufferGetHeight(buffer),
        );
        let format = CVPixelBufferGetPixelFormatType(buffer);
        // SAFETY: locked for reading while its memory is read, then
        // unlocked; rows are read within the buffer's own sizes.
        let values = unsafe {
            CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
            let base = CVPixelBufferGetBaseAddress(buffer).cast::<u8>();
            let row = CVPixelBufferGetBytesPerRow(buffer);
            let mut v = Vec::with_capacity(bw * bh);
            if !base.is_null() {
                for y in 0..bh {
                    let line = base.add(y * row);
                    for x in 0..bw {
                        v.push(if format == kCVPixelFormatType_OneComponent32Float {
                            line.add(x * 4).cast::<f32>().read_unaligned()
                        } else {
                            f32::from(*line.add(x)) / 255.0
                        });
                    }
                }
            }
            CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
            v
        };
        if values.len() != bw * bh || bw == 0 || bh == 0 {
            return Err(MlError::Vision(format!("unreadable mask ({format:#x})")));
        }
        Ok(Coverage {
            width: w,
            height: h,
            data: resize(&values, bw, bh, w, h),
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Coverage, Find};
    use crate::MlError;

    pub fn segment(_: &[u8], _: usize, _: usize, _: Find) -> Result<Coverage, MlError> {
        Err(MlError::Vision("Vision is macOS only".into()))
    }
}
