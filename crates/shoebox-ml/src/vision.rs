//! Subject and person masks from Apple's Vision framework (EDITING-SPEC,
//! Local adjustments: AI subject / person), on the device. A thin FFI
//! wrapper: pixels in, a coverage mask out, nothing else. Other systems
//! have no Vision: [`segment`] says so.
#![cfg_attr(target_os = "macos", allow(unsafe_code))]

use crate::MlError;

/// Bumped when the requests made, or how their results become masks
/// (`draw_parts`), change.
pub const REVISION: u32 = 1;

/// What finds the masks, for keeping them: this code's [`REVISION`] and
/// the system's build (Vision's models come with the OS), e.g.
/// `vision/1/25A354`. None where the build can't be read (masks found
/// then aren't kept).
pub fn identity() -> Option<String> {
    let plist = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist").ok()?;
    let build = build_version(&plist)?;
    Some(format!("vision/{REVISION}/{build}"))
}

/// `ProductBuildVersion` from SystemVersion.plist's XML.
fn build_version(plist: &str) -> Option<&str> {
    let after = plist.split("<key>ProductBuildVersion</key>").nth(1)?;
    let v = after.trim_start().strip_prefix("<string>")?;
    let v = v.split("</string>").next()?.trim();
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric())).then_some(v)
}

/// What to find.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Find {
    /// Every foreground object (people, animals, things in front).
    Subject,
    /// People, their hair and clothes.
    Person,
}

/// Parts of faces, from Vision's face landmarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FacePart {
    /// The face, less its eyes, eyebrows and lips.
    Skin,
    Eyes,
    Eyebrows,
    Lips,
    /// Inside the lips.
    Teeth,
}

/// One face's landmarks, in pixels of the image (y down): the face's box
/// (x, y, w, h) and its regions as point lists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Face {
    pub bounds: [f32; 4],
    pub eyes: Vec<Vec<[f32; 2]>>,
    pub brows: Vec<Vec<[f32; 2]>>,
    pub outer_lips: Vec<[f32; 2]>,
    pub inner_lips: Vec<[f32; 2]>,
}

/// The faces in an upright 8-bit sRGB image.
pub fn faces(rgb: &[u8], width: usize, height: usize) -> Result<Vec<Face>, MlError> {
    if width == 0 || height == 0 || rgb.len() < width * height * 3 {
        return Err(MlError::Vision("no image".into()));
    }
    imp::faces(rgb, width, height)
}

/// A part of every face in the image, as a soft-edged mask.
pub fn face_parts(
    rgb: &[u8],
    width: usize,
    height: usize,
    part: FacePart,
) -> Result<Coverage, MlError> {
    let faces = faces(rgb, width, height)?;
    Ok(Coverage {
        width,
        height,
        data: draw_parts(&faces, width, height, part),
    })
}

/// Fill `part` of these faces into a `w`×`h` mask, softened.
pub fn draw_parts(faces: &[Face], w: usize, h: usize, part: FacePart) -> Vec<u8> {
    let mut m = vec![0f32; w * h];
    let mut soft = 1.0f32;
    for f in faces {
        let size = f.bounds[2].max(f.bounds[3]).max(1.0);
        soft = soft.max(size * 0.015);
        let brow = |m: &mut Vec<f32>, v: f32| {
            for b in &f.brows {
                stroke(m, w, h, b, size * 0.035, v);
            }
        };
        match part {
            FacePart::Skin => {
                // An oval over the face's box, reaching up to the forehead.
                let [x, y, bw, bh] = f.bounds;
                let (cx, cy) = (x + bw / 2.0, y + bh * 0.45);
                let (rx, ry) = (bw * 0.5, bh * 0.62);
                fill_with(
                    &mut m,
                    w,
                    h,
                    [cx - rx, cy - ry, cx + rx, cy + ry],
                    1.0,
                    |px, py| ((px - cx) / rx).powi(2) + ((py - cy) / ry).powi(2) <= 1.0,
                );
                for e in &f.eyes {
                    polygon(&mut m, w, h, &grow(e, size * 0.02), 0.0);
                }
                brow(&mut m, 0.0);
                polygon(&mut m, w, h, &grow(&f.outer_lips, size * 0.01), 0.0);
            }
            FacePart::Eyes => {
                for e in &f.eyes {
                    polygon(&mut m, w, h, &grow(e, size * 0.005), 1.0);
                }
            }
            FacePart::Eyebrows => brow(&mut m, 1.0),
            FacePart::Lips => {
                polygon(&mut m, w, h, &f.outer_lips, 1.0);
                polygon(&mut m, w, h, &f.inner_lips, 0.0);
            }
            FacePart::Teeth => polygon(&mut m, w, h, &f.inner_lips, 1.0),
        }
    }
    blur(&mut m, w, h, soft.round() as usize);
    m.iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect()
}

/// Points moved out from their centre by `d` pixels.
fn grow(p: &[[f32; 2]], d: f32) -> Vec<[f32; 2]> {
    if p.is_empty() {
        return Vec::new();
    }
    let n = p.len() as f32;
    let c = [
        p.iter().map(|q| q[0]).sum::<f32>() / n,
        p.iter().map(|q| q[1]).sum::<f32>() / n,
    ];
    p.iter()
        .map(|q| {
            let (dx, dy) = (q[0] - c[0], q[1] - c[1]);
            let l = dx.hypot(dy).max(1e-3);
            [q[0] + dx / l * d, q[1] + dy / l * d]
        })
        .collect()
}

/// Set pixels (centres) inside `bounds` for which `inside` holds to `v`.
fn fill_with(
    m: &mut [f32],
    w: usize,
    h: usize,
    bounds: [f32; 4],
    v: f32,
    inside: impl Fn(f32, f32) -> bool,
) {
    let x0 = (bounds[0].floor().max(0.0) as usize).min(w);
    let y0 = (bounds[1].floor().max(0.0) as usize).min(h);
    let x1 = (bounds[2].ceil().max(0.0) as usize + 1).min(w);
    let y1 = (bounds[3].ceil().max(0.0) as usize + 1).min(h);
    for y in y0..y1 {
        for x in x0..x1 {
            if inside(x as f32 + 0.5, y as f32 + 0.5) {
                m[y * w + x] = v;
            }
        }
    }
}

/// A closed polygon (even-odd) set to `v`.
fn polygon(m: &mut [f32], w: usize, h: usize, p: &[[f32; 2]], v: f32) {
    if p.len() < 3 {
        return;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in p {
        x0 = x0.min(q[0]);
        y0 = y0.min(q[1]);
        x1 = x1.max(q[0]);
        y1 = y1.max(q[1]);
    }
    fill_with(m, w, h, [x0, y0, x1, y1], v, |px, py| {
        let mut inside = false;
        let mut j = p.len() - 1;
        for i in 0..p.len() {
            let (a, b) = (p[i], p[j]);
            if (a[1] > py) != (b[1] > py) && px < (b[0] - a[0]) * (py - a[1]) / (b[1] - a[1]) + a[0]
            {
                inside = !inside;
            }
            j = i;
        }
        inside
    });
}

/// A polyline `r` pixels either side, set to `v`.
fn stroke(m: &mut [f32], w: usize, h: usize, p: &[[f32; 2]], r: f32, v: f32) {
    for s in p.windows(2) {
        let (a, b) = (s[0], s[1]);
        let bounds = [
            a[0].min(b[0]) - r,
            a[1].min(b[1]) - r,
            a[0].max(b[0]) + r,
            a[1].max(b[1]) + r,
        ];
        fill_with(m, w, h, bounds, v, |px, py| {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 {
                (((px - a[0]) * dx + (py - a[1]) * dy) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (px - a[0] - t * dx).hypot(py - a[1] - t * dy) <= r
        });
    }
}

/// A box blur of radius `r`, twice (soft edges).
fn blur(m: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    for _ in 0..2 {
        let mut t = vec![0f32; w * h];
        for y in 0..h {
            let mut acc = 0.0;
            for x in 0..w + r {
                if x < w {
                    acc += m[y * w + x];
                }
                if x > 2 * r {
                    acc -= m[y * w + x - 2 * r - 1];
                }
                if x >= r && x - r < w {
                    t[y * w + x - r] = acc / (2 * r + 1) as f32;
                }
            }
        }
        for x in 0..w {
            let mut acc = 0.0;
            for y in 0..h + r {
                if y < h {
                    acc += t[y * w + x];
                }
                if y > 2 * r {
                    acc -= t[(y - 2 * r - 1) * w + x];
                }
                if y >= r && y - r < h {
                    m[(y - r) * w + x] = acc / (2 * r + 1) as f32;
                }
            }
        }
    }
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

    /// Vision's face landmarks, in the image's pixels (y down).
    pub fn faces(rgb: &[u8], w: usize, h: usize) -> Result<Vec<super::Face>, MlError> {
        let mut rgbx = Vec::with_capacity(w * h * 4);
        for p in rgb[..w * h * 3].as_chunks::<3>().0 {
            rgbx.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
        autoreleasepool(|_| {
            // SAFETY: as in `segment`: `rgbx` outlives the image.
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
            // SAFETY: a plain init.
            let request = unsafe { objc2_vision::VNDetectFaceLandmarksRequest::new() };
            let list: objc2::rc::Retained<NSArray<VNRequest>> =
                NSArray::from_retained_slice(&[objc2::rc::Retained::into_super(
                    objc2::rc::Retained::into_super(request.clone()),
                )]);
            handler
                .performRequests_error(&list)
                .map_err(|e| err("faces", e.localizedDescription()))?;
            // SAFETY: results after a successful perform.
            let Some(results) = (unsafe { request.results() }) else {
                return Ok(Vec::new());
            };
            let (fw, fh) = (w as f64, h as f64);
            let mut out = Vec::new();
            for obs in results.iter() {
                // SAFETY: the observation's own box and landmarks.
                let bb = unsafe { obs.boundingBox() };
                // Vision's coordinates are 0–1 with y up.
                let bounds = [
                    (bb.origin.x * fw) as f32,
                    ((1.0 - bb.origin.y - bb.size.height) * fh) as f32,
                    (bb.size.width * fw) as f32,
                    (bb.size.height * fh) as f32,
                ];
                let mut face = super::Face {
                    bounds,
                    ..Default::default()
                };
                let Some(lm) = (unsafe { obs.landmarks() }) else {
                    out.push(face);
                    continue;
                };
                let points =
                    |r: Option<objc2::rc::Retained<objc2_vision::VNFaceLandmarkRegion2D>>| {
                        let Some(r) = r else { return Vec::new() };
                        // SAFETY: `pointCount` points from `normalizedPoints`.
                        unsafe {
                            let n = r.pointCount();
                            let p = r.normalizedPoints();
                            if p.is_null() {
                                return Vec::new();
                            }
                            (0..n)
                                .map(|i| {
                                    let q = *p.add(i);
                                    [
                                        ((bb.origin.x + q.x * bb.size.width) * fw) as f32,
                                        ((1.0 - bb.origin.y - q.y * bb.size.height) * fh) as f32,
                                    ]
                                })
                                .collect()
                        }
                    };
                // SAFETY: the landmarks' own regions.
                unsafe {
                    face.eyes = vec![points(lm.leftEye()), points(lm.rightEye())];
                    face.brows = vec![points(lm.leftEyebrow()), points(lm.rightEyebrow())];
                    face.outer_lips = points(lm.outerLips());
                    face.inner_lips = points(lm.innerLips());
                }
                out.push(face);
            }
            Ok(out)
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

    pub fn faces(_: &[u8], _: usize, _: usize) -> Result<Vec<super::Face>, MlError> {
        Err(MlError::Vision("Vision is macOS only".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_is_read_from_the_plist() {
        let plist = "<dict>\n\t<key>ProductBuildVersion</key>\n\t<string>25A354</string>\n\
                     \t<key>ProductName</key>\n\t<string>macOS</string>\n</dict>";
        assert_eq!(build_version(plist), Some("25A354"));
        assert_eq!(build_version("<dict></dict>"), None);
        assert_eq!(
            build_version("<key>ProductBuildVersion</key><string>../x</string>"),
            None
        );
        #[cfg(target_os = "macos")]
        assert!(identity().is_some_and(|i| i.starts_with("vision/")));
    }

    /// A square face at (100, 100), 100 wide: eyes, brows, a mouth.
    fn face() -> Face {
        let square = |x: f32, y: f32, r: f32| {
            vec![
                [x - r, y - r],
                [x + r, y - r],
                [x + r, y + r],
                [x - r, y + r],
            ]
        };
        Face {
            bounds: [100.0, 100.0, 100.0, 100.0],
            eyes: vec![square(130.0, 135.0, 6.0), square(170.0, 135.0, 6.0)],
            brows: vec![
                vec![[120.0, 120.0], [140.0, 118.0]],
                vec![[160.0, 118.0], [180.0, 120.0]],
            ],
            outer_lips: square(150.0, 175.0, 12.0),
            inner_lips: square(150.0, 175.0, 5.0),
        }
    }

    #[test]
    fn face_parts_fall_where_their_landmarks_are() {
        let (w, h) = (300, 300);
        let at = |m: &[u8], x: usize, y: usize| m[y * w + x];
        let faces = [face()];
        let eyes = draw_parts(&faces, w, h, FacePart::Eyes);
        assert!(at(&eyes, 130, 135) > 200 && at(&eyes, 150, 150) < 20);
        let lips = draw_parts(&faces, w, h, FacePart::Lips);
        assert!(at(&lips, 150, 184) > 150, "the lips");
        assert!(at(&lips, 150, 175) < 100, "not inside the mouth");
        let teeth = draw_parts(&faces, w, h, FacePart::Teeth);
        assert!(at(&teeth, 150, 175) > 150);
        let skin = draw_parts(&faces, w, h, FacePart::Skin);
        assert!(at(&skin, 150, 150) > 200, "cheeks and nose");
        assert!(at(&skin, 130, 135) < 60, "not the eyes");
        assert!(at(&skin, 20, 20) == 0, "not off the face");
        let brows = draw_parts(&faces, w, h, FacePart::Eyebrows);
        assert!(at(&brows, 130, 119) > 150 && at(&brows, 130, 140) < 20);
    }
}
