//! The CPU reference pipeline (`docs/EDITING-SPEC.md`, Architecture): the
//! stages in their fixed order, in plain Rust. It renders exports until the
//! GPU pipe exists, and afterwards every shader is tested against it.
//!
//! Raw (CFA, normalised) → white balance → highlights → demosaic → proxy
//! downscale → camera → linear Rec.2020 → exposure → orientation → rotate
//! and crop → display transform → output space and transfer curve.

// Channel and matrix loops read clearer indexed (c, r) than zipped.
#![allow(clippy::needless_range_loop)]

pub mod raw;

use rayon::prelude::*;
use shoebox_decode::raw::RawImage;

use crate::colour::{self, M3};
use crate::edit::{Demosaic, DisplayTransform, Geometry, Highlights, Settings};
use crate::wb;

pub use raw::Rgb;

/// The display or file colour space of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Srgb,
    DisplayP3,
}

impl Target {
    fn primaries(self) -> &'static [colour::Xy; 3] {
        match self {
            Target::Srgb => &colour::SRGB,
            Target::DisplayP3 => &colour::DISPLAY_P3,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Downscale after demosaic so the long edge is at most this (the
    /// interactive proxy); None renders full size.
    pub long_edge: Option<u32>,
    /// The file's EXIF orientation (1–8).
    pub orientation: u16,
    pub target: Target,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            long_edge: None,
            orientation: 1,
            target: Target::Srgb,
        }
    }
}

/// Linear, white-balanced, demosaiced camera RGB, cached per file and
/// settings of the raw stages (white balance, highlights, demosaic).
pub fn demosaiced(raw: &RawImage, s: &Settings, long_edge: Option<u32>) -> (Rgb, wb::Resolved) {
    let w = wb::resolve(raw, &s.modules.white_balance);
    let mult = w.multipliers;
    let balanced: Vec<f32> = raw
        .data
        .par_chunks(raw.width)
        .enumerate()
        .flat_map_iter(|(row, line)| {
            line.iter()
                .enumerate()
                .map(move |(col, v)| v * mult[usize::from(raw.cfa[row % 2][col % 2])])
        })
        .collect();
    let clips = mult.map(|m| m * raw::OPPOSED_CLIP);
    let m = raw::Mosaic {
        width: raw.width,
        height: raw.height,
        cfa: raw.cfa,
        data: &balanced,
    };
    let healed = match s.modules.raw.highlights {
        Highlights::InpaintOpposed => raw::inpaint_opposed(&m, clips),
        Highlights::Clip => raw::clip(&m, clips),
    };
    let m = raw::Mosaic { data: &healed, ..m };
    let rgb = match s.modules.raw.demosaic {
        Demosaic::Rcd => raw::rcd(&m),
        Demosaic::Bilinear => raw::bilinear(&m),
    };
    let rgb = match long_edge {
        Some(l) => downscale(&rgb, l as usize),
        None => rgb,
    };
    (rgb, w)
}

/// Box-filter downscale by a whole factor so the long edge fits `long_edge`.
pub fn downscale(img: &Rgb, long_edge: usize) -> Rgb {
    let long = img.width.max(img.height);
    let f = long.div_ceil(long_edge.max(1)).max(1);
    if f == 1 {
        return img.clone();
    }
    let (w, h) = (img.width / f, img.height / f);
    let mut out = Rgb::new(w, h);
    let norm = 1.0 / (f * f) as f32;
    out.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, line)| {
            for x in 0..w {
                let mut s = [0.0f32; 3];
                for dy in 0..f {
                    for dx in 0..f {
                        let p = img.px(x * f + dx, y * f + dy);
                        s[0] += p[0];
                        s[1] += p[1];
                        s[2] += p[2];
                    }
                }
                for c in 0..3 {
                    line[x * 3 + c] = s[c] * norm;
                }
            }
        });
    out
}

fn f32m(m: &M3) -> [[f32; 3]; 3] {
    m.map(|r| r.map(|v| v as f32))
}

/// Apply a 3×3 matrix and a gain to every pixel.
pub fn transform(img: &mut Rgb, m: &M3, gain: f32) {
    let m = f32m(m);
    img.data.par_chunks_mut(3).for_each(|p| {
        let (r, g, b) = (p[0], p[1], p[2]);
        for c in 0..3 {
            p[c] = gain * (m[c][0] * r + m[c][1] * g + m[c][2] * b);
        }
    });
}

/// The scene-referred image: linear Rec.2020, exposed, upright, rotated
/// and cropped. Values above 1.0 are kept.
pub fn scene(raw: &RawImage, s: &Settings, o: &Options) -> Rgb {
    let (mut img, w) = demosaiced(raw, s, o.long_edge);
    let e = &s.modules.exposure;
    let ev = BASELINE_EV + if e.enabled { e.ev } else { 0.0 };
    let gain = 2f64.powf(ev) as f32;
    transform(&mut img, &w.cam_to_rec2020, gain);
    let img = orient(&img, o.orientation);
    rotate_crop(&img, &s.geometry)
}

/// Render for display or export: display-encoded values 0–1 in `target`.
pub fn render(raw: &RawImage, s: &Settings, o: &Options) -> Rgb {
    let mut img = scene(raw, s, o);
    display(&mut img, &s.modules.display_transform, o.target);
    img
}

/// EXIF orientation (1–8) applied, so the image is upright.
pub fn orient(img: &Rgb, orientation: u16) -> Rgb {
    let (w, h) = (img.width, img.height);
    let swap = matches!(orientation, 5..=8);
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    if orientation <= 1 || orientation > 8 {
        return img.clone();
    }
    let mut out = Rgb::new(ow, oh);
    out.data
        .par_chunks_mut(ow * 3)
        .enumerate()
        .for_each(|(y, line)| {
            for x in 0..ow {
                // Source pixel for output (x, y), per the EXIF definitions.
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    8 => (w - 1 - y, x),
                    _ => (x, y),
                };
                let p = img.px(sx, sy);
                line[x * 3..x * 3 + 3].copy_from_slice(&p);
            }
        });
    out
}

/// Bilinear sample at a fractional position (black outside the image).
fn sample(img: &Rgb, x: f32, y: f32) -> [f32; 3] {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let get = |xi: f32, yi: f32| -> [f32; 3] {
        if xi < 0.0 || yi < 0.0 || xi >= img.width as f32 || yi >= img.height as f32 {
            [0.0; 3]
        } else {
            img.px(xi as usize, yi as usize)
        }
    };
    let (a, b, c, d) = (
        get(x0, y0),
        get(x0 + 1.0, y0),
        get(x0, y0 + 1.0),
        get(x0 + 1.0, y0 + 1.0),
    );
    let mut out = [0.0; 3];
    for k in 0..3 {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        out[k] = top + (bot - top) * fy;
    }
    out
}

/// Rotate about the centre (degrees, clockwise) and crop to
/// `geometry.crop` (fractions of the frame), resampling once.
pub fn rotate_crop(img: &Rgb, g: &Geometry) -> Rgb {
    let (w, h) = (img.width as f64, img.height as f64);
    let [l, t, r, b] = g.crop.map(|v| v.clamp(0.0, 1.0));
    if g.rotate == 0.0 && [l, t, r, b] == [0.0, 0.0, 1.0, 1.0] {
        return img.clone();
    }
    let (x0, y0) = (l * w, t * h);
    let ow = ((r - l) * w).round().max(1.0) as usize;
    let oh = ((b - t) * h).round().max(1.0) as usize;
    let a = g.rotate.to_radians();
    let (sin, cos) = (a.sin(), a.cos());
    let (cx, cy) = (w / 2.0, h / 2.0);
    let mut out = Rgb::new(ow, oh);
    out.data
        .par_chunks_mut(ow * 3)
        .enumerate()
        .for_each(|(y, line)| {
            for x in 0..ow {
                // The output pixel's centre in the rotated frame, back into
                // the source by the inverse rotation.
                let (px, py) = (x0 + x as f64 + 0.5 - cx, y0 + y as f64 + 0.5 - cy);
                let sx = cos * px + sin * py + cx - 0.5;
                let sy = -sin * px + cos * py + cy - 0.5;
                let p = sample(img, sx as f32, sy as f32);
                line[x * 3..x * 3 + 3].copy_from_slice(&p);
            }
        });
    out
}

/// The largest crop of the frame's own aspect that stays inside the image
/// when it's rotated by `degrees`: `[left, top, right, bottom]`.
pub fn max_crop(width: f64, height: f64, degrees: f64) -> [f64; 4] {
    let a = degrees.to_radians().abs();
    let (sin, cos) = (a.sin(), a.cos());
    // A w×h rectangle (same aspect) fits inside the rotated image when
    // both of its rotated half-extents stay within the image's.
    let k = (width / (width * cos + height * sin)).min(height / (width * sin + height * cos));
    let (mx, my) = ((1.0 - k) / 2.0, (1.0 - k) / 2.0);
    [mx, my, 1.0 - mx, 1.0 - my]
}

/// Exposure added to every render before the slider's: cameras place mid
/// grey this much brighter than the raw matrix alone does (Adobe's DNG of
/// the Z f records 0.2; the Z f's own JPEGs sit about 0.75 above, judged by
/// luminance percentiles). Tuned on the definition-of-done set; Auto
/// exposure (Phase 2) refines it per photo.
pub const BASELINE_EV: f64 = 0.75;

/// Scene-linear mid grey, which the display transform keeps at display
/// mid grey.
pub const MID_GREY: f32 = 0.18;
/// Display mid grey, encoded with a 2.2 power (0.18 linear).
const DISPLAY_GREY: f32 = 0.459_565;
/// Default stops above mid grey that reach white: the sensor clips about
/// 2.5 stops up, plus room for reconstructed highlights.
const WHITE_EV: f32 = 4.0;
/// Default stops below mid grey that reach black.
const BLACK_EV: f32 = 10.0;
/// Default slope at mid grey, in display units (2.2-encoded) per stop:
/// a little more than a plain sRGB encode's 0.145, as cameras render.
const SLOPE: f32 = 0.17;
/// "Reaches white/black": within this of it.
const NEAR: f32 = 0.99;

/// `t / (1 + t^p)^(1/p)`: slope 1 at 0, approaching 1; larger `p` bends
/// later and harder.
fn shoulder(t: f32, p: f32) -> f32 {
    t / (1.0 + t.powf(p)).powf(1.0 / p)
}

/// The hardness that makes the shoulder reach [`NEAR`] at `t`.
fn hardness(t: f32) -> f32 {
    let (mut lo, mut hi) = (0.3f32, 40.0f32);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if shoulder(t, mid) < NEAR {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// The display transform's curve: scene value (linear, relative to mid
/// grey 0.18) → display value (2.2-encoded, 0–1). In stops from mid grey it
/// leaves grey at display grey with the contrast's slope. Above, a shoulder
/// rolls off smoothly to white at the white point. Below, values fall off
/// exponentially as a plain gamma encode does (so shadows keep their
/// separation), pulled to zero exactly at the black point.
#[derive(Debug, Clone, Copy)]
pub struct Curve {
    slope: f32,
    up: (f32, f32),
    /// Shadows: `grey · 2^(a·ev)`, less the value at the black point.
    a: f32,
    floor: f32,
}

impl Curve {
    pub fn new(d: &DisplayTransform) -> Self {
        let slope = SLOPE * 2f32.powf(d.contrast as f32 / 100.0);
        let white = (WHITE_EV + d.white_ev as f32).max(0.5);
        let black = (BLACK_EV - d.black_ev as f32).max(1.0);
        // The highlight side is scaled so its slope at grey is `slope`.
        let up_scale = (1.0 - DISPLAY_GREY) / slope;
        let a = slope / (DISPLAY_GREY * std::f32::consts::LN_2);
        Self {
            slope,
            up: (up_scale, hardness(white / up_scale)),
            a,
            floor: DISPLAY_GREY * 2f32.powf(-a * black),
        }
    }

    pub fn apply(&self, v: f32) -> f32 {
        let ev = (v.max(1e-12) / MID_GREY).log2();
        let y = if ev >= 0.0 {
            DISPLAY_GREY + (1.0 - DISPLAY_GREY) * shoulder(ev / self.up.0, self.up.1)
        } else {
            let y = DISPLAY_GREY * 2f32.powf(self.a * ev);
            DISPLAY_GREY * (y - self.floor) / (DISPLAY_GREY - self.floor)
        };
        y.clamp(0.0, 1.0)
    }

    /// The slope at mid grey (display units per stop).
    pub fn slope(&self) -> f32 {
        self.slope
    }
}

/// How far primaries are pulled towards grey before the curve, so bright
/// saturated colours roll off towards white instead of clipping into a
/// flat hue (AgX's idea); the curve's output gets back most of it.
const INSET: f64 = 0.15;
const OUTSET: f64 = 0.10;

fn toward_grey(amount: f64) -> M3 {
    // Each primary mixed with the luminance-weighted achromatic axis.
    let y = colour::rgb_to_xyz(&colour::REC2020, colour::D65)[1];
    let mut m = [[0.0; 3]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            *v = amount * y[c] + if r == c { 1.0 - amount } else { 0.0 };
        }
    }
    m
}

/// The display transform and output: scene-linear Rec.2020 in, display
/// values (0–1, the target space's transfer curve) out.
pub fn display(img: &mut Rgb, d: &DisplayTransform, target: Target) {
    let curve = Curve::new(d);
    let inset = f32m(&toward_grey(INSET));
    let outset = f32m(&colour::inv(&toward_grey(OUTSET)).unwrap_or(colour::IDENTITY));
    let to_target = f32m(&colour::mul(
        &colour::xyz_to_rgb(target.primaries(), colour::D65),
        &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
    ));
    let preserve = d.preserve_hue;
    let mul = |m: &[[f32; 3]; 3], p: [f32; 3]| -> [f32; 3] {
        [
            m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
            m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
            m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
        ]
    };
    img.data.par_chunks_mut(3).for_each(|p| {
        let v = mul(&inset, [p[0], p[1], p[2]]);
        let shaped = if preserve {
            // Tone-map the brightest channel and scale the others with it,
            // so hues hold; blend to the per-channel result as it nears
            // white so highlights still reach white.
            let norm = v[0].max(v[1]).max(v[2]).max(1e-10);
            let n = curve.apply(norm).powf(2.2);
            let scaled = v.map(|c| (c.max(0.0) / norm) * n);
            let per = v.map(|c| curve.apply(c).powf(2.2));
            let t = (n * n).clamp(0.0, 1.0);
            [0, 1, 2].map(|i| scaled[i] + (per[i] - scaled[i]) * t)
        } else {
            v.map(|c| curve.apply(c).powf(2.2))
        };
        let lin = mul(&to_target, mul(&outset, shaped));
        for c in 0..3 {
            p[c] = srgb_oetf(lin[c].clamp(0.0, 1.0));
        }
    });
}

/// The sRGB transfer curve (Display P3 uses the same one).
pub fn srgb_oetf(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Quantise display values to 8 bits per channel.
pub fn to_u8(img: &Rgb) -> Vec<u8> {
    img.data
        .iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect()
}

/// Quantise display values to 16 bits per channel.
pub fn to_u16(img: &Rgb) -> Vec<u16> {
    img.data
        .iter()
        .map(|v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16)
        .collect()
}
