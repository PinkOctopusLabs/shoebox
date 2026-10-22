//! Raw sensor data for Develop (`docs/EDITING-SPEC.md`, RAW decode), beside
//! the embedded-preview path the library uses. `rawler` decodes Lossless and
//! uncompressed NEFs and DNGs (High efficiency NEFs come here as DNGs made by
//! Adobe's converter); this module does the raw-domain prep the pipe starts
//! from: crop to the image area, subtract black, normalise to white.
//!
//! The data stays mosaiced and un-white-balanced: white balance,
//! highlight reconstruction and demosaic belong to the pipe. A linear DNG
//! (LinearRaw, as Shoebox exports) is already demosaiced: its three samples
//! a pixel are kept as they are ([`RawImage::linear`]).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use rawler::decoders::RawDecodeParams;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;

use crate::DecodeError;

/// A photosite at or above this fraction of white is clipped, and highlight
/// reconstruction fills it from its neighbours.
pub const CLIP: f32 = 0.98;

/// CFA colours, as stored in [`RawImage::cfa`].
pub const RED: u8 = 0;
pub const GREEN: u8 = 1;
pub const BLUE: u8 = 2;

/// A camera's XYZ → camera-RGB matrix, calibrated under one illuminant.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorMatrix {
    /// The calibration illuminant's correlated colour temperature, in kelvin.
    pub temperature: f32,
    pub xyz_to_cam: [[f32; 3]; 3],
}

/// Bayer sensor data, cropped to the image area, black-subtracted and
/// normalised so the white level is 1.0. Unoriented: apply the file's
/// orientation after demosaic.
#[derive(Debug, Clone)]
pub struct RawImage {
    pub width: usize,
    pub height: usize,
    /// The colour ([`RED`], [`GREEN`], [`BLUE`]) at `(row % 2, col % 2)`.
    pub cfa: [[u8; 2]; 2],
    /// `width * height` photosites, row by row. Never negative; values above
    /// 1.0 are kept (a white level below the sensor's maximum).
    pub data: Vec<f32>,
    /// As-shot white balance multipliers (R, G, B), green 1.0; None when the
    /// file records none.
    pub as_shot: Option<[f32; 3]>,
    /// The file's colour matrices, by calibration temperature, lowest first
    /// (the Z f has two: A and D65).
    pub matrices: Vec<ColorMatrix>,
    pub make: String,
    pub model: String,
    /// Set for a linear DNG: `data` is already demosaiced, three samples a
    /// pixel (camera RGB, not white-balanced), and `cfa` means nothing.
    pub linear: Option<Linear>,
}

/// What a linear DNG says about its data.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Linear {
    /// NoiseReductionApplied (0–1), when the file records it: Shoebox's
    /// exports record their AI denoise in it.
    pub noise_reduction: Option<f32>,
}

impl RawImage {
    /// The CFA colour of photosite `(x, y)`.
    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        self.cfa[y % 2][x % 2]
    }

    /// Whether photosite `(x, y)` is clipped (see [`CLIP`]); for a linear
    /// DNG, whether any of the pixel's samples is.
    pub fn clipped(&self, x: usize, y: usize) -> bool {
        let i = y * self.width + x;
        if self.linear.is_some() {
            return self
                .data
                .get(i * 3..i * 3 + 3)
                .is_some_and(|p| p.iter().any(|&v| v >= CLIP));
        }
        self.data.get(i).is_some_and(|&v| v >= CLIP)
    }

    /// Mosaiced (a colour filter array), so demosaic and AI denoise apply:
    /// anything but a linear DNG.
    pub fn is_mosaic(&self) -> bool {
        self.linear.is_none()
    }
}

/// Decode a raw file (a Lossless or uncompressed NEF, or a DNG).
pub fn decode_path(path: &Path) -> Result<RawImage, DecodeError> {
    let data =
        std::fs::read(path).map_err(|e| DecodeError::Format(format!("{}: {e}", path.display())))?;
    decode(&data)
}

/// [`decode_path`] from the file's bytes.
pub fn decode(data: &[u8]) -> Result<RawImage, DecodeError> {
    // rawler has `todo!()`s and asserts for inputs it doesn't expect; a bad
    // file must not take the app down.
    let raw = catch_unwind(AssertUnwindSafe(|| {
        rawler::decode(
            &RawSource::new_from_slice(data),
            &RawDecodeParams::default(),
        )
    }))
    .map_err(|_| DecodeError::Format("the raw decoder failed on this file".into()))?
    .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
    if raw.cpp == 3 && raw.photometric == RawPhotometricInterpretation::LinearRaw {
        let linear = Linear {
            noise_reduction: crate::dng::noise_reduction_applied(data),
        };
        return prepare_linear(&raw, linear);
    }
    prepare(&raw)
}

/// The image area: the default crop, else the active area, else all of it.
fn area(raw: &rawler::RawImage) -> (usize, usize, usize, usize) {
    let (full_w, full_h) = (raw.width, raw.height);
    match raw.crop_area.or(raw.active_area) {
        Some(r) if r.p.x + r.d.w <= full_w && r.p.y + r.d.h <= full_h && r.d.w > 1 && r.d.h > 1 => {
            (r.p.x, r.p.y, r.d.w, r.d.h)
        }
        _ => (0, 0, full_w, full_h),
    }
}

/// Crop, black-subtract and normalise rawler's output.
fn prepare(raw: &rawler::RawImage) -> Result<RawImage, DecodeError> {
    if raw.cpp != 1 {
        return Err(DecodeError::Unsupported(
            "raw data that isn't a colour filter array".into(),
        ));
    }
    let (full_w, full_h) = (raw.width, raw.height);
    if full_w == 0 || full_h == 0 {
        return Err(DecodeError::Format("empty raw image".into()));
    }
    let (x0, y0, w, h) = area(raw);
    // Black levels repeat from the active area's corner (DNG's rule).
    let (bx, by) = raw.active_area.map_or((0, 0), |r| (r.p.x, r.p.y));

    let cfa = &raw.camera.cfa;
    if cfa.width != 2 || cfa.height != 2 {
        return Err(DecodeError::Unsupported(format!(
            "{}×{} colour filter pattern (only Bayer is supported)",
            cfa.width, cfa.height
        )));
    }
    let mut pattern = [[0u8; 2]; 2];
    for (r, row) in pattern.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            let colour = cfa.color_at(y0 + r, x0 + c);
            if colour > 2 {
                return Err(DecodeError::Unsupported(
                    "a colour filter that isn't RGB".into(),
                ));
            }
            *v = colour as u8;
        }
    }

    let black = &raw.blacklevel;
    let (bw, bh) = (black.width.max(1), black.height.max(1));
    let levels: Vec<f32> = black.levels.iter().map(|l| l.as_f32()).collect();
    if levels.len() < bw * bh {
        return Err(DecodeError::Format("short black level table".into()));
    }
    let whites = &raw.whitelevel.0;
    let white_of = |colour: u8| -> f32 {
        let w = whites
            .get(usize::from(colour))
            .or(whites.first())
            .copied()
            .unwrap_or(u32::from(u16::MAX));
        w as f32
    };
    let white = [white_of(RED), white_of(GREEN), white_of(BLUE)];

    let src = raw.data.as_f32();
    if src.len() < full_w * full_h {
        return Err(DecodeError::Truncated);
    }
    let mut data = vec![0.0f32; w * h];
    for y in 0..h {
        let fy = y0 + y;
        let brow = (fy.wrapping_sub(by) % bh) * bw;
        let src_row = &src[fy * full_w + x0..fy * full_w + x0 + w];
        let out = &mut data[y * w..(y + 1) * w];
        for (x, (o, &v)) in out.iter_mut().zip(src_row).enumerate() {
            let b = levels[brow + (x0 + x).wrapping_sub(bx) % bw];
            let range = white[usize::from(pattern[y % 2][x % 2])] - b;
            *o = if range > 0.0 {
                ((v - b) / range).max(0.0)
            } else {
                0.0
            };
        }
    }

    let (as_shot, matrices) = colour(raw)?;
    Ok(RawImage {
        width: w,
        height: h,
        cfa: pattern,
        data,
        as_shot,
        matrices,
        make: raw.clean_make.clone(),
        model: raw.clean_model.clone(),
        linear: None,
    })
}

/// A linear DNG's three samples a pixel, cropped, black-subtracted and
/// normalised per channel, as [`prepare`] does photosites.
fn prepare_linear(raw: &rawler::RawImage, linear: Linear) -> Result<RawImage, DecodeError> {
    let (full_w, full_h) = (raw.width, raw.height);
    if full_w == 0 || full_h == 0 {
        return Err(DecodeError::Format("empty raw image".into()));
    }
    let (x0, y0, w, h) = area(raw);
    let src = raw.data.as_f32();
    if src.len() < full_w * full_h * 3 {
        return Err(DecodeError::Truncated);
    }
    // A black and a white level a channel (linear files have no repeating
    // black pattern).
    let levels: Vec<f32> = raw.blacklevel.levels.iter().map(|l| l.as_f32()).collect();
    let black = [0, 1, 2].map(|c| levels.get(c).or(levels.first()).copied().unwrap_or(0.0));
    let whites = &raw.whitelevel.0;
    let white = [0, 1, 2].map(|c| {
        whites
            .get(c)
            .or(whites.first())
            .copied()
            .unwrap_or(u32::from(u16::MAX)) as f32
    });
    let mut data = vec![0.0f32; w * h * 3];
    for (y, out) in data.chunks_exact_mut(w * 3).enumerate() {
        let at = ((y0 + y) * full_w + x0) * 3;
        for (i, (o, &v)) in out.iter_mut().zip(&src[at..at + w * 3]).enumerate() {
            let c = i % 3;
            let range = white[c] - black[c];
            *o = if range > 0.0 {
                ((v - black[c]) / range).max(0.0)
            } else {
                0.0
            };
        }
    }
    let (as_shot, matrices) = colour(raw)?;
    Ok(RawImage {
        width: w,
        height: h,
        cfa: [[RED, GREEN], [GREEN, BLUE]],
        data,
        as_shot,
        matrices,
        make: raw.clean_make.clone(),
        model: raw.clean_model.clone(),
        linear: Some(linear),
    })
}

/// As-shot multipliers and the colour matrices, lowest temperature first.
type Colour = (Option<[f32; 3]>, Vec<ColorMatrix>);

fn colour(raw: &rawler::RawImage) -> Result<Colour, DecodeError> {
    let wb = raw.wb_coeffs;
    let as_shot = (wb[..3].iter().all(|v| v.is_finite() && *v > 0.0))
        .then(|| [wb[0] / wb[1], 1.0, wb[2] / wb[1]]);

    let mut matrices: Vec<ColorMatrix> = raw
        .color_matrix
        .iter()
        .filter_map(|(ill, m)| {
            let temperature = temperature(*ill)?;
            (m.len() == 9).then(|| ColorMatrix {
                temperature,
                xyz_to_cam: [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]],
            })
        })
        .collect();
    matrices.sort_by(|a, b| a.temperature.total_cmp(&b.temperature));
    if matrices.is_empty() {
        return Err(DecodeError::Unsupported(format!(
            "no colour matrix for {} {}",
            raw.make, raw.model
        )));
    }
    Ok((as_shot, matrices))
}

/// A plain sRGB rendering of a raw file by rawler's own developer (PPG
/// demosaic, the D65 matrix, as-shot white, the sRGB curve, no tone curve):
/// for a DNG with no preview big enough when ImageIO can't render it. Long
/// edge at most `long_edge`; not oriented.
pub fn render(data: &[u8], long_edge: Option<u32>) -> Result<crate::Rgb8, DecodeError> {
    use rawler::imgop::develop::{Intermediate, RawDevelop};
    let developed = catch_unwind(AssertUnwindSafe(|| {
        let raw = rawler::decode(
            &RawSource::new_from_slice(data),
            &RawDecodeParams::default(),
        )?;
        if crate::check_dimensions(raw.width as u32, raw.height as u32).is_err() {
            return Err(rawler::RawlerError::DecoderFailed(
                "the image is too large".into(),
            ));
        }
        RawDevelop::default().develop_intermediate(&raw)
    }))
    .map_err(|_| DecodeError::Format("the raw decoder failed on this file".into()))?
    .map_err(|e| DecodeError::Unsupported(e.to_string()))?;
    let Intermediate::ThreeColor(px) = developed else {
        return Err(DecodeError::Unsupported(
            "a raw file that doesn't develop to RGB".into(),
        ));
    };
    let rgb = px
        .data
        .iter()
        .flat_map(|p| p.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8))
        .collect();
    let img = crate::Rgb8::new(px.width as u32, px.height as u32, rgb)?;
    Ok(match long_edge {
        Some(l) => img.fit_long_edge(l),
        None => img,
    })
}

/// Correlated colour temperature of an EXIF/DNG light source, as Adobe's DNG
/// SDK uses them (None for "unknown").
fn temperature(i: Illuminant) -> Option<f32> {
    Some(match i {
        Illuminant::A | Illuminant::Tungsten => 2856.0,
        Illuminant::IsoStudioTungsten => 3200.0,
        Illuminant::WhiteFluorescent => 3525.0,
        Illuminant::Fluorescent | Illuminant::CoolWhiteFluorescent => 4150.0,
        Illuminant::B => 4874.0,
        Illuminant::DaylightWhiteFluorescent => 5000.0,
        Illuminant::D50 => 5003.0,
        Illuminant::Daylight | Illuminant::Flash | Illuminant::FineWeather => 5500.0,
        Illuminant::D55 => 5503.0,
        Illuminant::DaylightFluorescent => 6430.0,
        Illuminant::CloudyWeather => 6500.0,
        Illuminant::D65 => 6504.0,
        Illuminant::C => 6774.0,
        Illuminant::Shade => 7500.0,
        Illuminant::D75 => 7504.0,
        Illuminant::Unknown => return None,
    })
}
