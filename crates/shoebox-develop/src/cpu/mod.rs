//! The CPU reference pipeline (`docs/EDITING-SPEC.md`, Architecture): the
//! stages in their fixed order, in plain Rust. It renders exports until the
//! GPU pipe exists, and afterwards every shader is tested against it.
//!
//! Two parts, as on the GPU:
//! - [`demosaic`], once per file (and per raw settings): white balance at
//!   the as-shot multipliers on the mosaic → highlights → demosaic → proxy
//!   downscale. Cached; seconds at most.
//! - [`apply`], the live pipe, per pixel from a [`Frame`] worked out from
//!   the settings: geometry (orientation, rotation and crop as one affine
//!   map, one bilinear sample) → colour (a white balance change relative to
//!   as shot, camera → linear Rec.2020, exposure: one matrix) → display
//!   transform → output space and transfer curve. The GPU runs exactly
//!   this (`crate::gpu`).
//!
//! White balance changes after demosaic, as darktable's "late correction":
//! the mosaic is balanced at as shot for highlights and demosaic, and a
//! different white balance is a per-channel ratio folded into the matrix,
//! so moving it is instant.

// Channel and matrix loops read clearer indexed (c, r) than zipped.
#![allow(clippy::needless_range_loop)]

pub mod raw;

use rayon::prelude::*;
use shoebox_decode::raw::RawImage;

use crate::colour::{self, M3};
use crate::edit::{Demosaic, DisplayTransform, Geometry, Highlights, Raw, Settings};
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

/// Demosaiced camera RGB, white-balanced at as shot (the multipliers it was
/// balanced with are kept).
#[derive(Debug, Clone)]
pub struct Demosaiced {
    pub rgb: Rgb,
    pub as_shot: [f32; 3],
}

/// The raw stages: balance the mosaic at as shot, rebuild clipped
/// highlights, demosaic, and downscale to `long_edge` (the proxy) if given.
pub fn demosaic(raw: &RawImage, r: &Raw, long_edge: Option<u32>) -> Demosaiced {
    let mult = wb::resolve(raw, &Default::default()).multipliers;
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
    let healed = match r.highlights {
        Highlights::InpaintOpposed => raw::inpaint_opposed(&m, clips),
        Highlights::Clip => raw::clip(&m, clips),
    };
    let m = raw::Mosaic { data: &healed, ..m };
    let rgb = match r.demosaic {
        Demosaic::Rcd => raw::rcd(&m),
        Demosaic::Bilinear => raw::bilinear(&m),
    };
    let rgb = match long_edge {
        Some(l) => downscale(&rgb, l as usize),
        None => rgb,
    };
    Demosaiced { rgb, as_shot: mult }
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

/// Everything the live pipe needs for one render, worked out once from the
/// settings: the same numbers drive the CPU reference and the shader.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// Output size.
    pub width: usize,
    pub height: usize,
    /// Output pixel `(x, y)` → source position `(sx, sy)` in source pixel
    /// indices: `sx = map[0]·(x, y, 1)`, `sy = map[1]·(x, y, 1)`.
    pub map: [[f32; 3]; 2],
    /// Source (camera RGB balanced at as shot) → linear Rec.2020, with the
    /// white balance change and exposure folded in.
    pub colour: [[f32; 3]; 3],
    pub display: Display,
}

/// The display transform's numbers.
#[derive(Debug, Clone, Copy)]
pub struct Display {
    pub curve: Curve,
    pub inset: [[f32; 3]; 3],
    pub outset: [[f32; 3]; 3],
    pub to_target: [[f32; 3]; 3],
    pub preserve_hue: bool,
}

impl Display {
    pub fn new(d: &DisplayTransform, target: Target) -> Self {
        Self {
            curve: Curve::new(d),
            inset: f32m(&toward_grey(INSET)),
            outset: f32m(&colour::inv(&toward_grey(OUTSET)).unwrap_or(colour::IDENTITY)),
            to_target: f32m(&colour::mul(
                &colour::xyz_to_rgb(target.primaries(), colour::D65),
                &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
            )),
            preserve_hue: d.preserve_hue,
        }
    }
}

/// The upright size of a `w`×`h` source with this EXIF orientation.
pub fn upright(w: usize, h: usize, orientation: u16) -> (usize, usize) {
    if matches!(orientation, 5..=8) {
        (h, w)
    } else {
        (w, h)
    }
}

/// The affine map from output pixels to source pixels: crop in the rotated
/// upright frame, rotation about its centre (degrees, clockwise), then the
/// EXIF orientation back to the stored image. Pixel centres are at +0.5.
pub fn geometry_map(
    src_w: usize,
    src_h: usize,
    orientation: u16,
    g: &Geometry,
) -> ([[f64; 3]; 2], usize, usize) {
    let (uw, uh) = upright(src_w, src_h, orientation);
    let (uwf, uhf) = (uw as f64, uh as f64);
    let [l, t, r, b] = g.crop.map(|v| v.clamp(0.0, 1.0));
    let ow = ((r - l) * uwf).round().max(1.0) as usize;
    let oh = ((b - t) * uhf).round().max(1.0) as usize;
    // Output index (x, y) → centre in the rotated upright frame, relative
    // to its centre: (x + 0.5 + l·W − W/2, …).
    let (ox, oy) = (l * uwf + 0.5 - uwf / 2.0, t * uhf + 0.5 - uhf / 2.0);
    let a = g.rotate.to_radians();
    let (sin, cos) = (a.sin(), a.cos());
    // Un-rotate: upright centre coords u = cos·px + sin·py + W/2, v = −sin·px + cos·py + H/2.
    let u = [cos, sin, cos * ox + sin * oy + uwf / 2.0];
    let v = [-sin, cos, -sin * ox + cos * oy + uhf / 2.0];
    // Upright (continuous) → stored (continuous), per the EXIF definitions.
    let (w, h) = (src_w as f64, src_h as f64);
    let neg = |p: [f64; 3], k: f64| [-p[0], -p[1], k - p[2]];
    let (sx, sy) = match orientation {
        2 => (neg(u, w), v),
        3 => (neg(u, w), neg(v, h)),
        4 => (u, neg(v, h)),
        5 => (v, u),
        6 => (v, neg(u, h)),
        7 => (neg(v, w), neg(u, h)),
        8 => (neg(v, w), u),
        _ => (u, v),
    };
    // Continuous → sample index (centres at +0.5).
    let to_index = |p: [f64; 3]| [p[0], p[1], p[2] - 0.5];
    ([to_index(sx), to_index(sy)], ow, oh)
}

/// Exposure added to every render before the slider's: cameras place mid
/// grey this much brighter than the raw matrix alone does (Adobe's DNG of
/// the Z f records 0.2; the Z f's own JPEGs sit about 0.75 above, judged by
/// luminance percentiles). Tuned on the definition-of-done set; Auto
/// exposure (Phase 2) refines it per photo.
pub const BASELINE_EV: f64 = 0.75;

/// Work out a frame: `src` is the demosaiced image (proxy or full size).
pub fn frame(raw: &RawImage, src: &Demosaiced, s: &Settings, o: &Options) -> Frame {
    let w = wb::resolve(raw, &s.modules.white_balance);
    // From as-shot-balanced to this white balance, per channel.
    let ratio = [0, 1, 2].map(|c| f64::from(w.multipliers[c]) / f64::from(src.as_shot[c]));
    let e = &s.modules.exposure;
    let ev = BASELINE_EV + if e.enabled { e.ev } else { 0.0 };
    let gain = 2f64.powf(ev);
    let m = colour::mul(&w.cam_to_rec2020, &colour::diag(ratio));
    let colour = f32m(&m.map(|r| r.map(|v| v * gain)));
    let (map, width, height) =
        geometry_map(src.rgb.width, src.rgb.height, o.orientation, &s.geometry);
    Frame {
        width,
        height,
        map: map.map(|r| r.map(|v| v as f32)),
        colour,
        display: Display::new(&s.modules.display_transform, o.target),
    }
}

/// Bilinear sample at a fractional position (black outside the image).
pub fn sample(img: &Rgb, x: f32, y: f32) -> [f32; 3] {
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

fn mul3(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
    ]
}

/// The live pipe on the CPU: display values (0–1, `target`'s transfer
/// curve) of every output pixel; with `display` false, the scene-linear
/// Rec.2020 values before the display transform.
pub fn apply(src: &Rgb, f: &Frame, display: bool) -> Rgb {
    let mut out = Rgb::new(f.width, f.height);
    out.data
        .par_chunks_mut(f.width * 3)
        .enumerate()
        .for_each(|(y, line)| {
            for x in 0..f.width {
                let (xf, yf) = (x as f32, y as f32);
                let sx = f.map[0][0] * xf + f.map[0][1] * yf + f.map[0][2];
                let sy = f.map[1][0] * xf + f.map[1][1] * yf + f.map[1][2];
                let lin = mul3(&f.colour, sample(src, sx, sy));
                let px = if display {
                    display_pixel(&f.display, lin)
                } else {
                    lin
                };
                line[x * 3..x * 3 + 3].copy_from_slice(&px);
            }
        });
    out
}

/// Render for display or export: display-encoded values 0–1 in `target`.
pub fn render(raw: &RawImage, s: &Settings, o: &Options) -> Rgb {
    let src = demosaic(raw, &s.modules.raw, o.long_edge);
    apply(&src.rgb, &frame(raw, &src, s, o), true)
}

/// The scene-referred render: linear Rec.2020, exposed, upright, rotated
/// and cropped. Values above 1.0 are kept.
pub fn scene(raw: &RawImage, s: &Settings, o: &Options) -> Rgb {
    let src = demosaic(raw, &s.modules.raw, o.long_edge);
    apply(&src.rgb, &frame(raw, &src, s, o), false)
}

/// EXIF orientation applied (the geometry map with nothing else).
pub fn orient(img: &Rgb, orientation: u16) -> Rgb {
    let (map, w, h) = geometry_map(img.width, img.height, orientation, &Geometry::default());
    resample(img, map, w, h)
}

/// Rotate and crop an upright image (the geometry map without orientation).
pub fn rotate_crop(img: &Rgb, g: &Geometry) -> Rgb {
    let (map, w, h) = geometry_map(img.width, img.height, 1, g);
    resample(img, map, w, h)
}

fn resample(img: &Rgb, map: [[f64; 3]; 2], w: usize, h: usize) -> Rgb {
    let f = Frame {
        width: w,
        height: h,
        map: map.map(|r| r.map(|v| v as f32)),
        colour: f32m(&colour::IDENTITY),
        display: Display::new(&DisplayTransform::default(), Target::Srgb),
    };
    apply(img, &f, false)
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

/// Scene-linear mid grey, which the display transform keeps at display
/// mid grey.
pub const MID_GREY: f32 = 0.18;
/// Display mid grey, encoded with a 2.2 power (0.18 linear).
pub(crate) const DISPLAY_GREY: f32 = 0.459_565;
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
    /// Highlights: stops per unit of the shoulder, and its hardness.
    pub(crate) up: (f32, f32),
    /// Shadows: `grey · 2^(a·ev)`, less the value at the black point.
    pub(crate) a: f32,
    pub(crate) floor: f32,
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

/// The display transform and output for one scene-linear Rec.2020 pixel:
/// display values (0–1, the target space's transfer curve).
pub fn display_pixel(d: &Display, p: [f32; 3]) -> [f32; 3] {
    let curve = &d.curve;
    let v = mul3(&d.inset, p);
    let shaped = if d.preserve_hue {
        // Tone-map the brightest channel and scale the others with it, so
        // hues hold; blend to the per-channel result as it nears white so
        // highlights still reach white.
        let norm = v[0].max(v[1]).max(v[2]).max(1e-10);
        let n = curve.apply(norm).powf(2.2);
        let scaled = v.map(|c| (c.max(0.0) / norm) * n);
        let per = v.map(|c| curve.apply(c).powf(2.2));
        let t = (n * n).clamp(0.0, 1.0);
        [0, 1, 2].map(|i| scaled[i] + (per[i] - scaled[i]) * t)
    } else {
        v.map(|c| curve.apply(c).powf(2.2))
    };
    let lin = mul3(&d.to_target, mul3(&d.outset, shaped));
    lin.map(|c| srgb_oetf(c.clamp(0.0, 1.0)))
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
