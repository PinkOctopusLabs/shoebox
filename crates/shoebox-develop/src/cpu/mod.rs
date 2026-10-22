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

pub mod detail;
pub mod effects;
pub mod layers;
pub mod look;
pub mod maps;
pub mod raw;
pub mod smart;

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
    /// Export only (the screen and the GPU pipe are sRGB or P3).
    AdobeRgb,
    /// Export only; D50 white.
    ProPhoto,
}

impl Target {
    pub fn primaries(self) -> &'static [colour::Xy; 3] {
        match self {
            Target::Srgb => &colour::SRGB,
            Target::DisplayP3 => &colour::DISPLAY_P3,
            Target::AdobeRgb => &colour::ADOBE_RGB,
            Target::ProPhoto => &colour::PRO_PHOTO,
        }
    }

    pub fn white(self) -> colour::Xy {
        match self {
            Target::ProPhoto => colour::D50,
            _ => colour::D65,
        }
    }

    /// Linear Rec.2020 (D65) → this space's linear RGB (adapted to its
    /// white).
    pub fn from_rec2020(self) -> M3 {
        colour::mul(
            &colour::xyz_to_rgb(self.primaries(), self.white()),
            &colour::mul(
                &colour::bradford(colour::D65, self.white()),
                &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
            ),
        )
    }

    /// The space's transfer curve (linear → encoded).
    pub fn encode(self, v: f32) -> f32 {
        match self {
            Target::Srgb | Target::DisplayP3 => srgb_oetf(v),
            Target::AdobeRgb => v.max(0.0).powf(256.0 / 563.0),
            Target::ProPhoto => {
                if v < 1.0 / 512.0 {
                    16.0 * v
                } else {
                    v.powf(1.0 / 1.8)
                }
            }
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

/// What the source's values mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Referred {
    /// Raw sensor data: scene light, toned by the display transform.
    Scene,
    /// A finished image (JPEG, TIFF, PNG, HEIC): already toned for display,
    /// so the defaults leave it exactly as it is and the sliders adjust
    /// from there.
    Display,
}

/// The image the live pipe starts from: for a raw file, demosaiced camera
/// RGB white-balanced at as shot (`as_shot` keeps the multipliers); for a
/// finished image, its pixels as linear Rec.2020.
#[derive(Debug, Clone)]
pub struct Demosaiced {
    pub rgb: Rgb,
    pub as_shot: [f32; 3],
    pub referred: Referred,
    /// Source pixels per original pixel (a proxy's is below 1): detail
    /// radii are in the original's pixels.
    pub scale: f32,
    /// How the photo was taken, for lens corrections (None: unknown).
    pub lens: Option<std::sync::Arc<crate::lens::Shot>>,
    /// Its small copy, last local-adjustment maps (see [`maps`]), noise
    /// estimate and detail-stage result (see [`detail`]), shared by clones.
    pub cache: maps::Cache,
}

impl Demosaiced {
    pub fn new(rgb: Rgb, as_shot: [f32; 3], referred: Referred) -> Self {
        Self {
            rgb,
            as_shot,
            referred,
            scale: 1.0,
            lens: None,
            cache: Default::default(),
        }
    }

    pub fn with_lens(mut self, lens: Option<std::sync::Arc<crate::lens::Shot>>) -> Self {
        self.lens = lens;
        self
    }

    pub fn with_scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// The detail stage's numbers for these settings (None: it does
    /// nothing). The noise is estimated once per source.
    pub fn detail(&self, s: &Settings) -> Option<detail::Detail> {
        detail::Detail::new(s, self.cache.noise(&self.rgb), self.scale)
    }

    /// The AI-denoised source, when it's arrived and fits (raw photos
    /// only).
    pub fn ai_ready(&self) -> Option<std::sync::Arc<Rgb>> {
        if self.referred != Referred::Scene {
            return None;
        }
        self.cache
            .ai()
            .filter(|ai| (ai.width, ai.height) == (self.rgb.width, self.rgb.height))
    }

    /// The global AI strength the source has (0–1; 0 when the module is
    /// off), or None while there's no AI result to blend.
    pub fn ai_strength(&self, s: &Settings) -> Option<f32> {
        self.ai_ready()?;
        let a = &s.modules.denoise_ai;
        Some(if a.enabled {
            (a.strength / 100.0).clamp(0.0, 1.0) as f32
        } else {
            0.0
        })
    }

    /// The source before the detail stage: `rgb`, or (None) blended with
    /// the AI-denoised source when that's on and has arrived.
    pub fn base(&self, s: &Settings) -> Option<std::sync::Arc<Rgb>> {
        if !s.modules.denoise_ai.enabled {
            return None;
        }
        let ai = self.ai_ready()?;
        let k = self.ai_strength(s)?;
        Some(if k >= 1.0 {
            ai
        } else {
            self.cache.blended(&self.rgb, &ai, k)
        })
    }

    /// The images local adjustments need beside the source (see [`Aux`]):
    /// None when no layer needs any. Kept while what they're made from
    /// stays the same.
    pub fn aux(&self, s: &Settings) -> Option<std::sync::Arc<Aux>> {
        use std::hash::{Hash, Hasher};
        let w = layers::wants(self, s);
        if !w.any() {
            self.cache.forget_aux();
            return None;
        }
        let main = self.prepared(s);
        let ai = self.ai_ready();
        let d = self.detail(s);
        let spots = &s.modules.heal.spots;
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{w:?} {d:?} {:?}", s.modules.sharpening).hash(&mut h);
            serde_json::to_string(spots)
                .unwrap_or_default()
                .hash(&mut h);
            // What the images are made from (kept alive with them, so the
            // addresses can't be reused).
            main.as_ref().map(std::sync::Arc::as_ptr).hash(&mut h);
            ai.as_ref().map(std::sync::Arc::as_ptr).hash(&mut h);
            h.finish()
        };
        let keep: Vec<_> = main.iter().chain(&ai).cloned().collect();
        Some(self.cache.aux(key, keep, || {
            let heal = |img: Rgb| {
                if spots.is_empty() {
                    img
                } else {
                    crate::heal::heal(&img, spots)
                }
            };
            // The source at another AI strength, as the detail stage and
            // spot removal make it.
            let through = |from: &std::sync::Arc<Rgb>| -> std::sync::Arc<Rgb> {
                if d.is_none() && spots.is_empty() {
                    return from.clone();
                }
                std::sync::Arc::new(heal(match &d {
                    Some(d) => detail::process(from, d),
                    None => (**from).clone(),
                }))
            };
            let main = main.as_deref().unwrap_or(&self.rgb);
            // Sharpening's change: the detail stage (and spots) run again
            // with more sharpening, fitted per pixel.
            let sharpen = w.sharpens.then(|| {
                let base = self.base(s);
                let from = base.as_deref().unwrap_or(&self.rgb);
                let more = |extra: f32| {
                    let mut more = s.clone();
                    more.modules.sharpening.amount =
                        s.modules.sharpening.amount.max(0.0) + f64::from(extra) * 100.0;
                    heal(match self.detail(&more) {
                        Some(d) => detail::process(from, &d),
                        None => from.clone(),
                    })
                };
                detail::Sharpen::fit(main, &more(detail::FIT_AT), &more(1.0))
            });
            Aux {
                more_ai: if w.more_ai {
                    ai.as_ref().map(through)
                } else {
                    None
                },
                less_ai: w
                    .less_ai
                    .then(|| through(&std::sync::Arc::new(self.rgb.clone()))),
                sharpen,
                soften: w.softens.then(|| detail::soften(main, self.scale)),
            }
        }))
    }

    /// The source after the detail stage: None when neither it nor the AI
    /// denoise does anything (use `rgb`). The last result is kept, so
    /// other sliders don't redo it.
    pub fn detailed(&self, s: &Settings) -> Option<std::sync::Arc<Rgb>> {
        let base = self.base(s);
        let Some(d) = self.detail(s) else {
            return base;
        };
        let from = base.as_deref().unwrap_or(&self.rgb);
        Some(
            self.cache
                .detailed(&d, base.as_ref(), || detail::process(from, &d)),
        )
    }

    /// The source the live pipe reads: after the detail stage and spot
    /// removal. None when neither does anything (use `rgb`).
    pub fn prepared(&self, s: &Settings) -> Option<std::sync::Arc<Rgb>> {
        let detailed = self.detailed(s);
        let spots = &s.modules.heal.spots;
        if spots.is_empty() {
            return detailed;
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_string(spots)
                .unwrap_or_default()
                .hash(&mut h);
            h.finish()
        };
        let from = detailed.as_deref().unwrap_or(&self.rgb);
        Some(
            self.cache
                .healed(detailed.as_ref(), key, || crate::heal::heal(from, spots)),
        )
    }
}

/// Images beside the source, the source's size, for local adjustments that
/// change the source stage inside their masks (see [`layers::Wants`]);
/// each is made only when a layer needs it, so a photo without such layers
/// keeps one source in memory. The GPU gets them as one texture array.
#[derive(Debug, Default)]
pub struct Aux {
    /// The source at AI strength 1 and 0, through the detail stage and
    /// spot removal as the source is: where a layer raises or lowers the
    /// strength, each pixel mixes towards one.
    pub more_ai: Option<std::sync::Arc<Rgb>>,
    pub less_ai: Option<std::sync::Arc<Rgb>>,
    /// Sharpening's change for an extra amount, from the detail stage
    /// itself (through spot removal too), for layers that sharpen.
    pub sharpen: Option<detail::Sharpen>,
    /// Softening's change at full strength, for layers that soften.
    pub soften: Option<Rgb>,
}

/// What source sampling reads from: one image, or the source mixed with
/// [`Aux`]'s images at a pixel.
trait Texels {
    fn size(&self) -> (usize, usize);
    fn texel(&self, x: usize, y: usize) -> [f32; 3];
}

impl Texels for Rgb {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    #[inline]
    fn texel(&self, x: usize, y: usize) -> [f32; 3] {
        self.px(x, y)
    }
}

/// The source as one output pixel reads it: mixed with the images beside
/// it by that pixel's [`layers::Mix`]. The shader's `texel` does the same.
struct Source<'a> {
    main: &'a Rgb,
    aux: Option<&'a Aux>,
    mix: layers::Mix,
}

impl Texels for Source<'_> {
    fn size(&self) -> (usize, usize) {
        (self.main.width, self.main.height)
    }

    #[inline]
    fn texel(&self, x: usize, y: usize) -> [f32; 3] {
        let mut v = self.main.px(x, y);
        let Some(aux) = self.aux else {
            return v;
        };
        let m = &self.mix;
        for (img, t) in [(&aux.more_ai, m.more), (&aux.less_ai, m.less)] {
            if let Some(img) = img
                && t > 0.0
            {
                let o = img.px(x, y);
                v = [0, 1, 2].map(|c| v[c] + t * (o[c] - v[c]));
            }
        }
        if let Some(sh) = &aux.sharpen
            && m.sharpen > 0.0
        {
            let d = sh.at(x, y, m.sharpen);
            v = [0, 1, 2].map(|c| v[c] + d[c]);
        }
        if let Some(soft) = &aux.soften
            && m.soften > 0.0
        {
            let d = soft.px(x, y);
            v = [0, 1, 2].map(|c| v[c] + m.soften * d[c]);
        }
        v
    }
}

/// A finished image (8-bit, `space`'s primaries and the sRGB transfer
/// curve) as a display-referred source in linear Rec.2020.
pub fn from_display(width: usize, height: usize, px: &[u8], space: Target) -> Demosaiced {
    let lut: Vec<f32> = (0..256).map(|v| srgb_eotf(v as f32 / 255.0)).collect();
    display_source(width, height, px, &lut, space)
}

/// [`from_display`] for 16-bit samples (16-bit TIFFs and PNGs), kept at
/// their precision.
pub fn from_display16(width: usize, height: usize, px: &[u16], space: Target) -> Demosaiced {
    let lut: Vec<f32> = (0..=u16::MAX)
        .map(|v| srgb_eotf(f32::from(v) / 65535.0))
        .collect();
    display_source(width, height, px, &lut, space)
}

fn display_source<T: Copy + Into<usize> + Sync>(
    width: usize,
    height: usize,
    px: &[T],
    lut: &[f32],
    space: Target,
) -> Demosaiced {
    let to_2020 = f32m(&colour::mul(
        &colour::xyz_to_rgb(&colour::REC2020, colour::D65),
        &colour::rgb_to_xyz(space.primaries(), colour::D65),
    ));
    let mut rgb = Rgb::new(width, height);
    rgb.data
        .par_chunks_mut(3)
        .zip(px.par_chunks(3))
        .for_each(|(o, p)| {
            let lin = [lut[p[0].into()], lut[p[1].into()], lut[p[2].into()]];
            o.copy_from_slice(&mul3(&to_2020, lin));
        });
    Demosaiced::new(rgb, [1.0; 3], Referred::Display)
}

/// The inverse of [`srgb_oetf`].
pub fn srgb_eotf(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The raw stages: balance the mosaic at as shot, rebuild clipped
/// highlights, demosaic, and downscale to `long_edge` (the proxy) if given.
pub fn demosaic(raw: &RawImage, r: &Raw, long_edge: Option<u32>) -> Demosaiced {
    let mult = wb::resolve(raw, &Default::default()).multipliers;
    let levelled = levelled(raw, r);
    let balanced: Vec<f32> = levelled
        .par_chunks(raw.width.max(1))
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
    let scale = rgb.width as f32 / raw.width.max(1) as f32;
    Demosaiced::new(rgb, mult, Referred::Scene).with_scale(scale)
}

/// The mosaic with hot and dead photosites fixed and the raw black and
/// white points applied, not yet white-balanced.
pub fn levelled(raw: &RawImage, r: &Raw) -> Vec<f32> {
    // Hot and dead photosites, then the black and white points (the
    // decoder already put them at 0 and 1; these move them).
    let fixed = r.hot_pixels.then(|| {
        raw::hot_pixels(&raw::Mosaic {
            width: raw.width,
            height: raw.height,
            cfa: raw.cfa,
            data: &raw.data,
        })
        .0
    });
    let data = fixed.as_deref().unwrap_or(&raw.data);
    let (black, white) = (r.black as f32, r.white.max(0.1) as f32);
    let levels = |v: f32| {
        if black == 0.0 && white == 1.0 {
            v
        } else {
            ((v - black) / (white - black)).min(1.0)
        }
    };
    data.par_iter().map(|v| levels(*v)).collect()
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

pub(crate) fn f32m(m: &M3) -> [[f32; 3]; 3] {
    m.map(|r| r.map(|v| v as f32))
}

/// Everything the live pipe needs for one render, worked out once from the
/// settings: the same numbers drive the CPU reference and the shader.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Output size.
    pub width: usize,
    pub height: usize,
    /// Output pixel `(x, y)` → source position `(sx, sy)` in source pixel
    /// indices, projectively: `sx = map[0]·(x, y, 1) / map[2]·(x, y, 1)`,
    /// likewise `sy` with `map[1]`.
    pub map: [[f32; 3]; 3],
    /// Source (camera RGB balanced at as shot) → linear Rec.2020, with the
    /// white balance change, exposure and calibration folded in.
    pub colour: [[f32; 3]; 3],
    /// A DNG camera profile's tables, right after the colour matrix (see
    /// [`crate::dcp::Stage`]).
    pub dcp: Option<crate::dcp::Stage>,
    /// Lens corrections: where each channel comes from, and the
    /// vignetting gain (see [`crate::lens`]).
    pub lens: Option<std::sync::Arc<crate::lens::Grid>>,
    /// Dehaze, highlights and shadows, clarity (see [`maps`]).
    pub local: maps::Local,
    /// Local adjustments (see [`layers`]).
    pub layers: layers::Layers,
    /// Colour and black and white, per pixel (see [`look`]).
    pub look: look::Look,
    pub display: Display,
    /// Show clipping: blown highlights red, crushed shadows blue (J).
    pub warnings: bool,
    /// Soft proofing: show colours the proof space can't hold (magenta).
    pub gamut_warning: bool,
    /// A local adjustment's mask shown (O): its index in `layers`, and
    /// whether alone (else as a red tint).
    pub mask_view: Option<(usize, bool)>,
    /// Point Color's point shown: what it reaches in colour, the rest grey.
    pub point_view: Option<usize>,
    /// Vignette and grain (see [`effects`]).
    pub effects: effects::Effects,
}

/// The display transform's numbers.
#[derive(Debug, Clone)]
pub struct Display {
    pub curve: Curve,
    pub inset: [[f32; 3]; 3],
    pub outset: [[f32; 3]; 3],
    pub to_target: [[f32; 3]; 3],
    pub preserve_hue: bool,
    /// The output space (its transfer curve is applied last).
    pub target: Target,
    /// A finished image's tone instead of the scene curve: black and white
    /// points and contrast on 2.2-encoded values (identity at defaults).
    pub referred: Option<Levels>,
    /// The tone curve's tables (red, green, blue; see
    /// [`look::curve_table`]), applied to the display values; None: none.
    pub tone: Option<std::sync::Arc<[f32]>>,
    /// A camera match profile's look (see [`crate::profile`]).
    pub look: Option<std::sync::Arc<crate::profile::Lut3>>,
    /// A DNG camera profile's tone curve in place of the scene curve (see
    /// [`crate::dcp::Tone`]).
    pub dcp_tone: Option<crate::dcp::Tone>,
    /// A look (`.cube`) after the camera match look (see
    /// [`crate::cube::Stage`]).
    pub cube: Option<crate::cube::Stage>,
    /// Soft proofing: colours limited to an export space's before the
    /// output space.
    pub proof: Option<Proof>,
}

/// Soft proofing (EDITING-SPEC, Tools beyond PhotoLab): the image as an
/// export space would hold it. Display-linear Rec.2020 goes into the
/// space, is clipped to it, and comes back, so the screen shows only what
/// the file could.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Proof {
    pub space: Target,
    /// Linear Rec.2020 → the space's linear RGB, and back.
    pub to: [[f32; 3]; 3],
    pub back: [[f32; 3]; 3],
}

/// How far outside 0–1 a proofed value may be before it counts as out of
/// gamut (rounding in the matrices).
pub const GAMUT_SLACK: f32 = 2e-3;

impl Proof {
    pub fn new(space: Target) -> Self {
        let to = space.from_rec2020();
        Self {
            space,
            to: f32m(&to),
            back: f32m(&colour::inv(&to).unwrap_or(colour::IDENTITY)),
        }
    }

    /// `rec` (display-linear Rec.2020) as the space holds it, and whether
    /// it had to change: a channel below zero, or above one where the
    /// screen (`to_target`) wouldn't clip it anyway.
    pub fn apply(&self, rec: [f32; 3], to_target: &[[f32; 3]; 3]) -> ([f32; 3], bool) {
        let q = mul3(&self.to, rec);
        let low = q.iter().any(|&v| v < -GAMUT_SLACK);
        let high = q.iter().any(|&v| v > 1.0 + GAMUT_SLACK)
            && mul3(to_target, rec).iter().all(|&v| v <= 1.0);
        (mul3(&self.back, q.map(|v| v.clamp(0.0, 1.0))), low || high)
    }
}

/// Tone for a display-referred image: `x' = (x − black) / (white − black)`,
/// then contrast bending it towards (or, negative, away from) a smoothstep
/// S-curve; on 2.2-encoded values. All defaults: the identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub black: f32,
    pub white: f32,
    pub contrast: f32,
}

impl Levels {
    pub fn new(d: &DisplayTransform) -> Self {
        // Blacks and whites keep the raw sliders' sense: positive blacks
        // deepen them, positive whites dim the highlights.
        Self {
            black: (d.black_ev * 0.05) as f32,
            white: (1.0 + d.white_ev * 0.05) as f32,
            contrast: (d.contrast / 100.0) as f32,
        }
    }

    /// Linear display value → toned linear value, 0–1.
    pub fn apply(&self, v: f32) -> f32 {
        let x = v.max(0.0).powf(1.0 / 2.2);
        let x1 = ((x - self.black) / (self.white - self.black).max(1e-3)).clamp(0.0, 1.0);
        let s = x1 * x1 * (3.0 - 2.0 * x1);
        let y = (x1 + self.contrast * (s - x1)).clamp(0.0, 1.0);
        y.powf(2.2)
    }
}

impl Display {
    pub fn new(d: &DisplayTransform, target: Target) -> Self {
        Self {
            curve: Curve::new(d),
            inset: f32m(&toward_grey(INSET)),
            outset: f32m(&colour::inv(&toward_grey(OUTSET)).unwrap_or(colour::IDENTITY)),
            to_target: f32m(&target.from_rec2020()),
            preserve_hue: d.preserve_hue,
            referred: None,
            tone: None,
            look: None,
            dcp_tone: None,
            cube: None,
            proof: None,
            target,
        }
    }

    /// The tone for a display-referred image (no scene curve, no AgX
    /// inset).
    pub fn for_display_referred(d: &DisplayTransform, target: Target) -> Self {
        Self {
            referred: Some(Levels::new(d)),
            inset: f32m(&colour::IDENTITY),
            outset: f32m(&colour::IDENTITY),
            ..Self::new(d, target)
        }
    }
}

/// The 2×2 matrix taking a stored image's (centred, y-down) coordinates to
/// the upright image's, for each EXIF orientation 1–8.
fn orientation_matrix(o: u16) -> [[i8; 2]; 2] {
    match o {
        2 => [[-1, 0], [0, 1]],
        3 => [[-1, 0], [0, -1]],
        4 => [[1, 0], [0, -1]],
        5 => [[0, 1], [1, 0]],
        6 => [[0, -1], [1, 0]],
        7 => [[0, -1], [-1, 0]],
        8 => [[0, 1], [-1, 0]],
        _ => [[1, 0], [0, 1]],
    }
}

/// The file's orientation followed by the edit's quarter turns (clockwise)
/// and mirror, as one EXIF orientation code.
pub fn compose_orientation(exif: u16, quarter: u8, flip: bool) -> u16 {
    let mul = |a: [[i8; 2]; 2], b: [[i8; 2]; 2]| {
        let mut m = [[0i8; 2]; 2];
        for i in 0..2 {
            for j in 0..2 {
                m[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j];
            }
        }
        m
    };
    let mut m = orientation_matrix(exif);
    for _ in 0..quarter % 4 {
        m = mul(orientation_matrix(6), m);
    }
    if flip {
        m = mul(orientation_matrix(2), m);
    }
    (1..=8).find(|&o| orientation_matrix(o) == m).unwrap_or(1)
}

/// The upright size of a `w`×`h` source with this EXIF orientation.
pub fn upright(w: usize, h: usize, orientation: u16) -> (usize, usize) {
    if matches!(orientation, 5..=8) {
        (h, w)
    } else {
        (w, h)
    }
}

/// The frame as the edit shows it before rotation and perspective: a
/// `w`×`h` source turned upright by the file's EXIF `orientation`, then by
/// the edit's quarter turns (its mirror keeps the size). Crops, rotation
/// and guides are fractions of this frame.
pub fn frame_size(w: usize, h: usize, orientation: u16, g: &Geometry) -> (usize, usize) {
    upright(w, h, compose_orientation(orientation, g.quarter, g.flip))
}

/// The affine map from output pixels to source pixels: crop in the rotated
/// upright frame, rotation about its centre (degrees, clockwise), then the
/// EXIF orientation back to the stored image. Pixel centres are at +0.5.
pub fn geometry_map(
    src_w: usize,
    src_h: usize,
    orientation: u16,
    g: &Geometry,
) -> ([[f64; 3]; 3], usize, usize) {
    let (uw, uh) = frame_size(src_w, src_h, orientation, g);
    let orientation = compose_orientation(orientation, g.quarter, g.flip);
    let (uwf, uhf) = (uw as f64, uh as f64);
    let [l, t, r, b] = g.crop.map(|v| v.clamp(0.0, 1.0));
    let ow = ((r - l) * uwf).round().max(1.0) as usize;
    let oh = ((b - t) * uhf).round().max(1.0) as usize;
    // Output index (x, y) → centre in the rotated upright frame, relative
    // to its centre: (x + 0.5 + l·W − W/2, …).
    let (ox, oy) = (l * uwf + 0.5 - uwf / 2.0, t * uhf + 0.5 - uhf / 2.0);
    let out = [[1.0, 0.0, ox], [0.0, 1.0, oy], [0.0, 0.0, 1.0]];
    // Un-rotate.
    let a = g.rotate.to_radians();
    let (sin, cos) = (a.sin(), a.cos());
    let unrotate = [[cos, sin, 0.0], [-sin, cos, 0.0], [0.0, 0.0, 1.0]];
    // Un-transform (perspective, scale, offsets), about the centre.
    let untransform = transform_inverse(&g.transform, uwf, uhf);
    let corner = [
        [1.0, 0.0, uwf / 2.0],
        [0.0, 1.0, uhf / 2.0],
        [0.0, 0.0, 1.0],
    ];
    // Upright (continuous) → stored (continuous), per the EXIF definitions.
    let (w, h) = (src_w as f64, src_h as f64);
    let stored = match orientation {
        2 => [[-1.0, 0.0, w], [0.0, 1.0, 0.0]],
        3 => [[-1.0, 0.0, w], [0.0, -1.0, h]],
        4 => [[1.0, 0.0, 0.0], [0.0, -1.0, h]],
        5 => [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        6 => [[0.0, 1.0, 0.0], [-1.0, 0.0, h]],
        7 => [[0.0, -1.0, w], [-1.0, 0.0, h]],
        8 => [[0.0, -1.0, w], [1.0, 0.0, 0.0]],
        _ => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
    };
    let stored = [stored[0], stored[1], [0.0, 0.0, 1.0]];
    // Continuous → sample index (centres at +0.5).
    let index = [[1.0, 0.0, -0.5], [0.0, 1.0, -0.5], [0.0, 0.0, 1.0]];
    let m = [stored, corner, untransform, unrotate, out]
        .iter()
        .fold(index, |acc, next| colour::mul(&acc, next));
    (m, ow, oh)
}

/// The manual transform's inverse on centred upright coordinates: from
/// where a pixel ends up to where it was. Perspective is a camera tilted
/// (vertical) or panned (horizontal) about the centre, focal length the
/// frame's diagonal; then aspect, scale and offsets. With `constrain`, a
/// zoom after the perspective keeps the frame filled.
pub fn transform_inverse(t: &crate::edit::Transform, w: f64, h: f64) -> M3 {
    if t.is_identity() {
        return colour::IDENTITY;
    }
    let f = w.hypot(h);
    let (av, ah) = (
        (t.vertical / 100.0 * 30.0).to_radians(),
        (t.horizontal / 100.0 * 30.0).to_radians(),
    );
    let tilt = [
        [1.0, 0.0, 0.0],
        [0.0, av.cos(), -av.sin()],
        [0.0, av.sin(), av.cos()],
    ];
    let pan = [
        [ah.cos(), 0.0, ah.sin()],
        [0.0, 1.0, 0.0],
        [-ah.sin(), 0.0, ah.cos()],
    ];
    let k = colour::diag([f, f, 1.0]);
    let k_inv = colour::diag([1.0 / f, 1.0 / f, 1.0]);
    // Original → after the perspective, re-centred: tilting the camera
    // also moves the picture, and the original's centre should stay put.
    let persp = colour::mul(&k, &colour::mul(&colour::mul(&pan, &tilt), &k_inv));
    let c = colour::mulv(&persp, [0.0, 0.0, 1.0]);
    let recentre = [
        [1.0, 0.0, -c[0] / c[2]],
        [0.0, 1.0, -c[1] / c[2]],
        [0.0, 0.0, 1.0],
    ];
    let persp = colour::mul(&recentre, &persp);
    let persp_inv = colour::inv(&persp).unwrap_or(colour::IDENTITY);
    let zoom = if t.constrain {
        fill_zoom(&persp_inv, w, h)
    } else {
        1.0
    };
    let s = t.scale / 100.0 * zoom;
    let aspect = 2f64.powf(t.aspect / 200.0);
    let (sx, sy) = (s * aspect, s / aspect);
    let (dx, dy) = (t.offset_x / 100.0 * 0.25 * w, t.offset_y / 100.0 * 0.25 * h);
    // Where a pixel ends up: perspective, then scale and offset.
    let place_inv = [
        [1.0 / sx, 0.0, -dx / sx],
        [0.0, 1.0 / sy, -dy / sy],
        [0.0, 0.0, 1.0],
    ];
    colour::mul(&persp_inv, &place_inv)
}

/// The smallest zoom at which the frame's corners, taken back through
/// `inv`, land inside the original (so no blank edge shows).
fn fill_zoom(inv: &M3, w: f64, h: f64) -> f64 {
    let inside = |z: f64| {
        [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
            .iter()
            .all(|&(cx, cy)| {
                let p = colour::mulv(inv, [cx * w / 2.0 / z, cy * h / 2.0 / z, 1.0]);
                p[2] > 0.0
                    && (p[0] / p[2]).abs() <= w / 2.0 + 1e-6
                    && (p[1] / p[2]).abs() <= h / 2.0 + 1e-6
            })
    };
    if inside(1.0) {
        return 1.0;
    }
    let (mut lo, mut hi) = (1.0, 8.0);
    if !inside(hi) {
        return 1.0;
    }
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if inside(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// Exposure added to every render before the slider's: cameras place mid
/// grey this much brighter than the raw matrix alone does (Adobe's DNG of
/// the Z f records 0.2; the Z f's own JPEGs sit about 0.75 above, judged by
/// luminance percentiles). Tuned on the definition-of-done set; Auto
/// exposure (Phase 2) refines it per photo.
pub const BASELINE_EV: f64 = 0.75;

/// Work out a frame: `src` is the demosaiced image (proxy or full size).
pub fn frame(raw: &RawImage, src: &Demosaiced, s: &Settings, o: &Options) -> Frame {
    let e = &s.modules.exposure;
    let slider = if e.enabled { e.ev } else { 0.0 };
    let ic = &s.modules.input_colour;
    // A DNG camera profile (raw photos; while it's missing, the matrix).
    let dcp = match src.referred {
        Referred::Scene => crate::dcp::chosen(s),
        Referred::Display => None,
    };
    let (m, ev, display, temp) = match src.referred {
        Referred::Scene => {
            let w = match &dcp {
                Some(d) => wb::resolve_with(&d.camera(), raw.as_shot, &s.modules.white_balance),
                None => wb::resolve(raw, &s.modules.white_balance),
            };
            // From as-shot-balanced to this white balance, per channel.
            let ratio = [0, 1, 2].map(|c| f64::from(w.multipliers[c]) / f64::from(src.as_shot[c]));
            (
                colour::mul(&w.cam_to_rec2020, &colour::diag(ratio)),
                BASELINE_EV + slider + dcp.as_ref().map_or(0.0, |d| d.exposure_offset),
                Display::new(&s.modules.display_transform, o.target),
                w.calibrated_at,
            )
        }
        Referred::Display => (
            wb::relative(&s.modules.white_balance).2,
            slider,
            Display::for_display_referred(&s.modules.display_transform, o.target),
            6504.0,
        ),
    };
    let gain = 2f64.powf(ev);
    let m = colour::mul(&look::calibration_matrix(&s.modules.calibration), &m);
    let colour = f32m(&m.map(|r| r.map(|v| v * gain)));
    // The profile's tone curve replaces the scene curve, and the AgX inset
    // with it: the profile's look was made for its own curve.
    let dcp_tone = dcp
        .as_ref()
        .filter(|_| ic.dcp_tone_curve)
        .and_then(|d| d.tone.clone())
        .map(crate::dcp::Tone::new);
    let display = Display {
        tone: look::curve_table(&s.modules.tone_curve),
        look: match ic.profile {
            crate::edit::Profile::CameraMatch => crate::profile::find(&raw.model),
            crate::edit::Profile::Matrix | crate::edit::Profile::Dcp => None,
        },
        inset: if dcp_tone.is_some() {
            f32m(&colour::IDENTITY)
        } else {
            display.inset
        },
        outset: if dcp_tone.is_some() {
            f32m(&colour::IDENTITY)
        } else {
            display.outset
        },
        dcp_tone,
        ..display
    };
    // A look, while it's there.
    let lk = &s.modules.look_lut;
    let display = Display {
        cube: lk
            .lut
            .as_ref()
            .filter(|_| lk.amount != 0.0)
            .and_then(crate::cube::find)
            .map(|c| crate::cube::Stage::new(c, lk.amount, &display.outset)),
        ..display
    };
    let dcp = dcp.and_then(|d| crate::dcp::Stage::new(d, temp, gain));
    let (mut map, width, height) =
        geometry_map(src.rgb.width, src.rgb.height, o.orientation, &s.geometry);
    // Lens corrections and volume deformation, on the frame's own aspect;
    // their zoom keeps the corrected frame filled.
    let volume = crate::volume::Spec::of(
        &s.geometry.volume,
        compose_orientation(o.orientation, s.geometry.quarter, s.geometry.flip),
    );
    let lens = {
        let (w, h) = match src.referred {
            Referred::Scene if raw.width > 0 => (raw.width, raw.height),
            _ => (src.rgb.width, src.rgb.height),
        };
        match &src.lens {
            Some(shot) => crate::lens::grid_with(shot, &s.modules.lens, volume.as_ref(), w, h),
            None => volume.as_ref().and_then(|v| {
                let off = crate::edit::LensCorrection {
                    enabled: false,
                    ..Default::default()
                };
                crate::lens::grid_with(&Default::default(), &off, Some(v), w, h)
            }),
        }
    };
    if let Some(g) = &lens {
        let z = f64::from(g.zoom);
        let c = [
            (src.rgb.width as f64 - 1.0) / 2.0,
            (src.rgb.height as f64 - 1.0) / 2.0,
        ];
        // Scale the source position about its centre: s' = c + (s − c)/z,
        // on the homogeneous rows.
        let last = map[2];
        for (r, row) in map.iter_mut().take(2).enumerate() {
            for k in 0..3 {
                row[k] = c[r] * last[k] + (row[k] - c[r] * last[k]) / z;
            }
        }
    }
    Frame {
        width,
        height,
        map: map.map(|r| r.map(|v| v as f32)),
        lens,
        local: maps::local(src, &colour, s),
        layers: layers::layers(
            src,
            &colour,
            &display,
            s,
            compose_orientation(o.orientation, s.geometry.quarter, s.geometry.flip),
        ),
        colour,
        dcp,
        look: look::Look::new(
            &s.modules.colour,
            &s.modules.calibration,
            &s.modules.black_and_white,
        )
        .with_defringe(&s.modules.defringe)
        .with_points(&s.modules.point_colour)
        .with_grading(&s.modules.colour_grading),
        display,
        warnings: false,
        gamut_warning: false,
        mask_view: None,
        point_view: None,
        effects: effects::Effects::new(
            &s.modules.effects,
            [width as f32, height as f32],
            src.scale,
        ),
    }
}

/// Out of the proof space's gamut, when shown.
pub const GAMUT: [f32; 3] = [0.95, 0.2, 0.95];

/// Clipping shown: any channel at white → red, every channel at black →
/// blue (display values).
pub fn warn(p: [f32; 3]) -> [f32; 3] {
    let hi = p[0].max(p[1]).max(p[2]);
    if hi >= 0.999 {
        [1.0, 0.1, 0.1]
    } else if hi <= 0.001 {
        [0.1, 0.3, 1.0]
    } else {
        p
    }
}

/// Histograms of display values: R, G, B and luma, 256 bins each, counted
/// as 8-bit values (luma as the library's thumbnails count it).
pub fn histogram(img: &Rgb) -> [[u32; 256]; 4] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    let mut h = [[0u32; 256]; 4];
    for p in img.data.as_chunks::<3>().0 {
        let (r, g, b) = (q(p[0]), q(p[1]), q(p[2]));
        h[0][r as usize] += 1;
        h[1][g as usize] += 1;
        h[2][b as usize] += 1;
        h[3][((77 * r + 150 * g + 29 * b) >> 8) as usize] += 1;
    }
    h
}

impl Frame {
    /// The source position of output pixel `(x, y)`.
    pub fn source_at(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.map;
        let w = m[2][0] * x + m[2][1] * y + m[2][2];
        (
            (m[0][0] * x + m[0][1] * y + m[0][2]) / w,
            (m[1][0] * x + m[1][1] * y + m[1][2]) / w,
        )
    }

    /// The source position before lens corrections (and volume
    /// deformation) that shows source pixel `(sx, sy)`: the lens grid
    /// undone (by iteration), so a point found in the source (a face) can
    /// be drawn where it shows.
    pub fn unlensed(&self, w: usize, h: usize, sx: f32, sy: f32) -> (f32, f32) {
        let Some(g) = self.lens.as_deref() else {
            return (sx, sy);
        };
        let (w, h) = (w.max(2) as f32, h.max(2) as f32);
        let (mut px, mut py) = (sx, sy);
        for _ in 0..50 {
            let n = g.at(
                px / (w - 1.0) * (g.cols - 1) as f32,
                py / (h - 1.0) * (g.rows - 1) as f32,
            );
            let (ex, ey) = (sx - (px + n[2] * w), sy - (py + n[3] * h));
            px += ex;
            py += ey;
            if ex.hypot(ey) < 1e-3 {
                break;
            }
        }
        (px, py)
    }

    /// Just the part `[x, y, width, height]` of this frame (clamped to it):
    /// the same map, starting at the part's corner.
    pub fn region(&self, r: [usize; 4]) -> Frame {
        let x = r[0].min(self.width.saturating_sub(1));
        let y = r[1].min(self.height.saturating_sub(1));
        let mut f = self.clone();
        f.width = r[2].min(self.width - x).max(1);
        f.height = r[3].min(self.height - y).max(1);
        f.effects.offset[0] += x as f32;
        f.effects.offset[1] += y as f32;
        for row in 0..3 {
            f.map[row][2] += self.map[row][0] * x as f32 + self.map[row][1] * y as f32;
        }
        f
    }
}

/// Bilinear sample at a fractional position (black outside the image).
pub fn sample(img: &Rgb, x: f32, y: f32) -> [f32; 3] {
    sample_in(img, x, y)
}

fn sample_in<T: Texels>(img: &T, x: f32, y: f32) -> [f32; 3] {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (w, h) = img.size();
    let get = |xi: f32, yi: f32| -> [f32; 3] {
        if xi < 0.0 || yi < 0.0 || xi >= w as f32 || yi >= h as f32 {
            [0.0; 3]
        } else {
            img.texel(xi as usize, yi as usize)
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

pub fn mul3(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
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
    apply_with(src, None, f, display)
}

/// [`apply`] with the images local adjustments need beside the source
/// (see [`Demosaiced::aux`]); without them, layers' AI strength does
/// nothing.
pub fn apply_with(src: &Rgb, aux: Option<&Aux>, f: &Frame, display: bool) -> Rgb {
    let mut out = Rgb::new(f.width, f.height);
    out.data
        .par_chunks_mut(f.width * 3)
        .enumerate()
        .for_each(|(y, line)| {
            for x in 0..f.width {
                let (lin, mx, my, seen) = before_display(src, aux, f, x as f32, y as f32);
                let px = if display {
                    let (d, out) = display_out(&f.display, lin);
                    let d = if f.effects.active() {
                        effects::apply(&f.effects, d, x as f32, y as f32)
                    } else {
                        d
                    };
                    let d = if f.gamut_warning && out { GAMUT } else { d };
                    let d = if f.warnings { warn(d) } else { d };
                    let d = match f.point_view {
                        Some(_) => greyed(d, seen),
                        None => d,
                    };
                    match f.mask_view {
                        Some((k, only)) => layers::mask_shown(&f.layers, k, only, d, mx, my),
                        None => d,
                    }
                } else {
                    lin
                };
                line[x * 3..x * 3 + 3].copy_from_slice(&px);
            }
        });
    out
}

/// Display values greyed where `w` is 0, as they are where it's 1 (Point
/// Color's view of what a point reaches).
pub fn greyed(d: [f32; 3], w: f32) -> [f32; 3] {
    let y = 0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2];
    d.map(|v| y + (v - y) * w)
}

/// Output pixel `(x, y)` up to the display transform: linear Rec.2020,
/// its source position before lens corrections, and how much the Point
/// Color point shown applied.
fn before_display(
    src: &Rgb,
    aux: Option<&Aux>,
    f: &Frame,
    x: f32,
    y: f32,
) -> ([f32; 3], f32, f32, f32) {
    let (lin, mx, my) = look_input_at(src, aux, f, x, y);
    let (lin, seen) = look::look_point(&f.look, lin, f.point_view);
    (lin, mx, my, seen)
}

/// Output pixel `(x, y)` through geometry, lens corrections, the colour
/// matrix and defringe; with its source positions before and after lens
/// corrections.
fn colour_input(src: &Source, f: &Frame, mx: f32, my: f32) -> ([f32; 3], f32, f32) {
    let (rgb, sx, sy) = lens_sample_in(src, f.lens.as_deref(), mx, my);
    let mut lin = mul3(&f.colour, rgb);
    if let Some(d) = &f.dcp {
        lin = d.apply(lin);
    }
    if f.look.fringe {
        let y = |dx: f32, dy: f32| {
            let p = mul3(&f.colour, sample_in(src, sx + dx, sy + dy));
            f.look.luma[0] * p[0] + f.look.luma[1] * p[1] + f.look.luma[2] * p[2]
        };
        let e = look::edge(y(-1.0, 0.0), y(1.0, 0.0), y(0.0, -1.0), y(0.0, 1.0));
        lin = look::defringe_pixel(&f.look, lin, e);
    }
    (lin, sx, sy)
}

/// What the colour stage sees at output pixel `(x, y)`: linear Rec.2020
/// after the local tone maps and local adjustments (Point Color picks it).
pub fn look_input(src: &Rgb, f: &Frame, x: f32, y: f32) -> [f32; 3] {
    look_input_at(src, None, f, x, y).0
}

/// [`look_input`], and the source position before lens corrections.
fn look_input_at(main: &Rgb, aux: Option<&Aux>, f: &Frame, x: f32, y: f32) -> ([f32; 3], f32, f32) {
    let (mx, my) = f.source_at(x, y);
    // How the layers mix the source here (read where masks are).
    let mix = match aux {
        Some(_) => layers::mix_at(&f.layers, mx, my),
        None => Default::default(),
    };
    let src = &Source { main, aux, mix };
    let (lin, sx, sy) = colour_input(src, f, mx, my);
    let lin = maps::local_pixel(&f.local, lin, sx, sy);
    let lin = layers::layers_pixel(&f.layers, &f.local, lin, mx, my, sx, sy);
    (lin, mx, my)
}

/// Sample the source at corrected position `(sx, sy)` through the lens
/// grid: each channel from where the lens put it, times the vignetting
/// gain. Returns the pixel and green's source position (where the local
/// maps are read).
pub fn lens_sample(
    src: &Rgb,
    grid: Option<&crate::lens::Grid>,
    sx: f32,
    sy: f32,
) -> ([f32; 3], f32, f32) {
    lens_sample_in(src, grid, sx, sy)
}

fn lens_sample_in<T: Texels>(
    src: &T,
    grid: Option<&crate::lens::Grid>,
    sx: f32,
    sy: f32,
) -> ([f32; 3], f32, f32) {
    let sample = sample_in::<T>;
    let Some(g) = grid else {
        return (sample(src, sx, sy), sx, sy);
    };
    let (w, h) = src.size();
    let (w, h) = (w as f32, h as f32);
    let n = g.at(
        sx / (w - 1.0).max(1.0) * (g.cols - 1) as f32,
        sy / (h - 1.0).max(1.0) * (g.rows - 1) as f32,
    );
    let (gx, gy) = (sx + n[2] * w, sy + n[3] * h);
    let mut p = if g.chromatic {
        let r = sample(src, sx + n[0] * w, sy + n[1] * h)[0];
        let mid = sample(src, gx, gy)[1];
        let b = sample(src, sx + n[4] * w, sy + n[5] * h)[2];
        [r, mid, b]
    } else {
        sample(src, gx, gy)
    };
    for v in &mut p {
        *v *= n[6];
    }
    (p, gx, gy)
}

/// Render for display or export: display-encoded values 0–1 in `target`.
pub fn render(raw: &RawImage, s: &Settings, o: &Options) -> Rgb {
    let src = demosaic(raw, &s.modules.raw, o.long_edge);
    render_from(raw, &src, s, o, true)
}

/// The scene-referred render: linear Rec.2020, exposed, upright, rotated
/// and cropped. Values above 1.0 are kept.
pub fn scene(raw: &RawImage, s: &Settings, o: &Options) -> Rgb {
    let src = demosaic(raw, &s.modules.raw, o.long_edge);
    render_from(raw, &src, s, o, false)
}

/// The whole pipe from a prepared source: the detail stage and spot
/// removal, then [`apply`].
pub fn render_from(
    raw: &RawImage,
    src: &Demosaiced,
    s: &Settings,
    o: &Options,
    display: bool,
) -> Rgb {
    apply_from(src, s, &frame(raw, src, s, o), display)
}

/// [`apply`] on the source after the detail stage and spot removal for
/// `s` (the settings `f` was made from).
pub fn apply_from(src: &Demosaiced, s: &Settings, f: &Frame, display: bool) -> Rgb {
    let aux = src.aux(s);
    match src.prepared(s) {
        Some(img) => apply_with(&img, aux.as_deref(), f, display),
        None => apply_with(&src.rgb, aux.as_deref(), f, display),
    }
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

fn resample(img: &Rgb, map: [[f64; 3]; 3], w: usize, h: usize) -> Rgb {
    let f = Frame {
        width: w,
        height: h,
        map: map.map(|r| r.map(|v| v as f32)),
        lens: None,
        colour: f32m(&colour::IDENTITY),
        dcp: None,
        local: maps::Local::none(),
        layers: Default::default(),
        look: look::Look::new(
            &Default::default(),
            &Default::default(),
            &Default::default(),
        ),
        display: Display::new(&DisplayTransform::default(), Target::Srgb),
        warnings: false,
        gamut_warning: false,
        mask_view: None,
        point_view: None,
        effects: Default::default(),
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
    display_out(d, p).0
}

/// [`display_pixel`], and whether soft proofing had to change the colour.
pub fn display_out(d: &Display, p: [f32; 3]) -> ([f32; 3], bool) {
    let shaped = shape(d, p);
    // The camera match look (a profile's table), a look, then the tone curve.
    let shaped = match &d.look {
        Some(l) => l.apply(shaped),
        None => shaped,
    };
    // A look (`.cube`), in sRGB as shown.
    let shaped = match &d.cube {
        Some(c) => c.apply(shaped),
        None => shaped,
    };
    // The tone curve, on 2.2-encoded display values.
    let shaped = match &d.tone {
        Some(t) => {
            [0, 1, 2].map(|c| look::curve_at(t, c, shaped[c].max(0.0).powf(1.0 / 2.2)).powf(2.2))
        }
        None => shaped,
    };
    let rec = mul3(&d.outset, shaped);
    let (rec, out) = match &d.proof {
        Some(p) => p.apply(rec, &d.to_target),
        None => (rec, false),
    };
    let lin = mul3(&d.to_target, rec);
    (lin.map(|c| d.target.encode(c.clamp(0.0, 1.0))), out)
}

/// The display transform's tone mapping of one scene-linear Rec.2020
/// pixel: display-linear Rec.2020 (before the look, the tone curve and the
/// output space).
pub fn shape(d: &Display, p: [f32; 3]) -> [f32; 3] {
    let curve = &d.curve;
    let v = mul3(&d.inset, p);
    if let Some(l) = &d.referred {
        if d.preserve_hue {
            let norm = v[0].max(v[1]).max(v[2]).max(1e-10);
            let n = l.apply(norm);
            let scaled = v.map(|c| (c.max(0.0) / norm) * n);
            let per = v.map(|c| l.apply(c));
            let t = (n * n).clamp(0.0, 1.0);
            [0, 1, 2].map(|i| scaled[i] + (per[i] - scaled[i]) * t)
        } else {
            v.map(|c| l.apply(c))
        }
    } else if let Some(t) = &d.dcp_tone {
        t.apply(v)
    } else if d.preserve_hue {
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
    }
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
