//! Auto (EDITING-SPEC, Auto mode): image statistics measured once when a
//! raw photo is prepared ([`measure`]), turned into ordinary settings
//! ([`settings`]) that show on the sliders and are stored as plain numbers,
//! so an improved Auto never changes an old edit.
//!
//! - Exposure from the 99.5th percentile (the brightest highlights land
//!   near white), nudged halfway towards the camera JPEG's brightness.
//! - Noise reduction by ISO; light capture sharpening (raw files are soft
//!   until sharpened, as every raw editor's defaults reflect).
//! - Dehaze only when the photo measures hazy.
//! - A little Shadows when much of the frame is deep shadow (a stand-in for
//!   the Smart Lighting module of Phase 3).
//! - Lens corrections on when there's a profile.

use serde::{Deserialize, Serialize};

use crate::cpu::{self, Demosaiced, Options};
use crate::edit::Settings;
use shoebox_decode::raw::RawImage;

/// What Auto measured of a photo (at its as-shot white balance, exposure 0).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// Exposure that puts the 99.5th percentile of luminance near white.
    pub highlights_ev: f32,
    /// Exposure at which the render's median matches the camera JPEG's.
    pub camera_ev: Option<f32>,
    /// Share of the frame more than four stops below mid grey.
    pub deep_shadows: f32,
    /// How hazy (0–1): the median of the darkest channel against the
    /// 99.5th percentile of luminance.
    pub haze: f32,
}

fn luma(p: [f32; 3], w: [f32; 3]) -> f32 {
    p[0] * w[0] + p[1] * w[1] + p[2] * w[2]
}

fn mul3(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
    ]
}

/// The exposure (stops, in `lo..hi`) at which `f(ev)` reaches `target`,
/// `f` increasing.
fn solve(f: impl Fn(f32) -> f32, target: f32, lo: f32, hi: f32) -> f32 {
    let (mut lo, mut hi) = (lo, hi);
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if f(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

/// Measure a raw photo. `camera_jpeg`: its embedded preview (sRGB), when
/// it has one.
pub fn measure(
    raw: &RawImage,
    src: &Demosaiced,
    camera_jpeg: Option<&shoebox_decode::Rgb8>,
) -> Stats {
    let f = cpu::frame(raw, src, &Settings::default(), &Options::default());
    let w = f.look.luma;
    let small = cpu::maps::small(&src.rgb);
    let lin: Vec<[f32; 3]> = small
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| mul3(&f.colour, *p))
        .collect();
    let mut ys: Vec<f32> = lin.iter().map(|p| luma(*p, w).max(0.0)).collect();
    ys.sort_by(f32::total_cmp);
    let at = |q: f32| ys[((ys.len() - 1) as f32 * q) as usize];
    let (p50, p995) = (at(0.5).max(1e-6), at(0.995).max(1e-6));
    // The display value of a grey of scene value v (encoded 0–1).
    let shown = |v: f32| cpu::display_pixel(&f.display, [v; 3])[1];
    // Near white: the scene value the default curve shows at 0.95.
    let white = solve(|ev| shown(2f32.powf(ev)), 0.95, -8.0, 8.0);
    let highlights_ev = (white - p995.log2()).clamp(-4.0, 4.0);
    let camera_ev = camera_jpeg.map(|j| {
        let mut l: Vec<f32> =
            j.px.as_chunks::<3>()
                .0
                .iter()
                .map(|p| {
                    let c = p.map(|v| cpu::srgb_eotf(f32::from(v) / 255.0));
                    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
                })
                .collect();
        l.sort_by(f32::total_cmp);
        let target = cpu::srgb_oetf(l[l.len() / 2]);
        solve(|ev| shown(p50 * 2f32.powf(ev)), target, -4.0, 4.0)
    });
    let deep = cpu::MID_GREY / 16.0;
    let deep_shadows = ys.iter().filter(|&&y| y < deep).count() as f32 / ys.len() as f32;
    // Haze lifts the darkest channel everywhere: its median against the
    // highlights. (Clear scenes have dark shadows somewhere in every colour;
    // a night scene's darkest channel is near black beside its lights.)
    let mut dark: Vec<f32> = lin
        .iter()
        .map(|p| p[0].min(p[1]).min(p[2]).max(0.0))
        .collect();
    dark.sort_by(f32::total_cmp);
    let haze = (dark[dark.len() / 2] / p995).clamp(0.0, 1.0);
    Stats {
        highlights_ev,
        camera_ev,
        deep_shadows,
        haze,
    }
}

/// `base` with Auto's settings: exposure, noise reduction, sharpening,
/// dehaze, shadows and (with `lens`) lens corrections.
pub fn settings(base: &Settings, s: &Stats, iso: Option<u32>, lens: bool) -> Settings {
    let mut out = base.clone();
    let mut ev = s.highlights_ev.clamp(-2.0, 2.0);
    if let Some(c) = s.camera_ev {
        ev += 0.5 * (c - ev);
    }
    out.modules.exposure.ev = round(f64::from(ev.clamp(-3.0, 3.0)), 0.05);
    let iso = f64::from(iso.unwrap_or(100).max(50));
    let stops = (iso / 400.0).log2();
    out.modules.noise_reduction.luminance = round((10.0 * stops).clamp(0.0, 60.0), 1.0);
    out.modules.noise_reduction.chroma =
        round((20.0 + 6.0 * (iso / 100.0).log2()).clamp(20.0, 50.0), 1.0);
    out.modules.sharpening.amount = 30.0;
    out.modules.sharpening.masking = 20.0;
    out.modules.dehaze.amount = round(f64::from(((s.haze - 0.25) * 100.0).clamp(0.0, 30.0)), 1.0);
    if s.deep_shadows > 0.3 {
        out.modules.highlights_shadows.shadows = 15.0;
    }
    out.modules.lens.enabled = lens;
    out
}

/// Auto's input colour: the camera match profile when there's one for the
/// camera, else the matrix.
pub fn profile_for(model: &str) -> crate::edit::Profile {
    if crate::profile::has(model) {
        crate::edit::Profile::CameraMatch
    } else {
        crate::edit::Profile::Matrix
    }
}

fn round(v: f64, step: f64) -> f64 {
    (v / step).round() * step
}
