//! The look stages between exposure and the display transform (modules 9
//! and 11, plus calibration and black and white from Tools beyond
//! PhotoLab), on linear Rec.2020:
//!
//! - **Calibration** nudges the working primaries: a 3×3 matrix folded into
//!   the frame's colour matrix (white stays white). Its shadows tint is
//!   per pixel, with the colour stage.
//! - **Colour**: per pixel in Oklab (from linear Rec.2020 through LMS):
//!   shadows tint, then HSL per hue band, vibrance and saturation.
//! - **Black and white**: luminance, brightened or darkened by hue band in
//!   proportion to how coloured the pixel was.
//! - **Tone curve**: a lookup table per channel, applied by the display
//!   transform to its display values (see [`super::display_pixel`]).
//!
//! The shader (`develop.wgsl`) follows these operation for operation.

use std::sync::Arc;

use crate::colour::{self, M3};
use crate::edit::{BlackAndWhite, COLOUR_POINTS, Calibration, Colour, ToneCurve};

/// Oklab's LMS from linear sRGB (Björn Ottosson, 2020).
const SRGB_TO_LMS: M3 = [
    [0.412_221_470_8, 0.536_332_536_3, 0.051_445_992_9],
    [0.211_903_498_2, 0.680_699_545_1, 0.107_396_956_6],
    [0.088_302_461_9, 0.281_718_837_6, 0.629_978_700_5],
];

/// Cube-rooted LMS → Lab, and back.
pub const LMS_TO_LAB: [[f32; 3]; 3] = [
    [0.210_454_26, 0.793_617_8, -0.004_072_047],
    [1.977_998_5, -2.428_592_2, 0.450_593_7],
    [0.025_904_037, 0.782_771_77, -0.808_675_77],
];
pub const LAB_TO_LMS: [[f32; 3]; 3] = [
    [1.0, 0.396_337_78, 0.215_803_76],
    [1.0, -0.105_561_346, -0.063_854_17],
    [1.0, -0.089_484_18, -1.291_485_5],
];

/// The hue bands' centres in Oklab hue (degrees): red, orange, yellow,
/// green, aqua, blue, purple, magenta.
pub const BAND_CENTRES: [f32; 8] = [29.0, 58.0, 110.0, 142.0, 195.0, 264.0, 295.0, 328.0];

/// Entries per channel in the tone curve's table.
pub const CURVE_SIZE: usize = 1024;

/// The per-pixel colour stage's numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Anything to do (else the stage is skipped).
    pub active: bool,
    pub to_lms: [[f32; 3]; 3],
    pub from_lms: [[f32; 3]; 3],
    /// −1 to 1.
    pub shadows_tint: f32,
    pub vibrance: f32,
    pub saturation: f32,
    /// Per band: hue shift in degrees, saturation and luminance −1 to 1.
    pub hue: [f32; 8],
    pub sat: [f32; 8],
    pub lum: [f32; 8],
    pub mono: bool,
    /// Per band, −1 to 1.
    pub mix: [f32; 8],
    /// Rec.2020 luminance weights.
    pub luma: [f32; 3],
    /// Defringe: purple and green amounts (0–1); `fringe` when either is on.
    pub fringe: bool,
    pub purple: f32,
    pub green: f32,
    /// Point Color: the first `point_count` of `points`.
    pub points: [Point; COLOUR_POINTS],
    pub point_count: usize,
    /// Colour grading (see [`Grading`]).
    pub grading: Grading,
}

/// Colour grading's numbers: each wheel's Oklab offset (a, b, per unit
/// of lightness) and lightness gain, for shadows, midtones, highlights and
/// global; the ranges' split.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Grading {
    pub on: bool,
    pub wheels: [[f32; 3]; 4],
    /// Lightness is raised to this so the balance point lands at 0.5.
    pub pivot: f32,
    /// The ranges' falloff power: lower overlaps more.
    pub power: f32,
}

impl Grading {
    pub fn new(g: &crate::edit::ColourGrading) -> Self {
        let wheel = |w: &crate::edit::Wheel| {
            let c = (w.saturation.clamp(0.0, 100.0) / 100.0 * 0.12) as f32;
            let h = (w.hue as f32).to_radians();
            [
                c * h.cos(),
                c * h.sin(),
                (w.luminance.clamp(-100.0, 100.0) / 100.0 * 0.5) as f32,
            ]
        };
        let wheels = [
            wheel(&g.shadows),
            wheel(&g.midtones),
            wheel(&g.highlights),
            wheel(&g.global),
        ];
        let on = wheels.iter().flatten().any(|&v| v != 0.0);
        // Balance moves where shadows end and highlights begin (Oklab
        // lightness 0.3–0.7).
        let at = (0.5 + 0.2 * (g.balance.clamp(-100.0, 100.0) / 100.0)) as f32;
        let pivot = 0.5f32.ln() / at.ln();
        let blending = (g.blending.clamp(0.0, 100.0) / 100.0) as f32;
        Self {
            on,
            wheels,
            pivot,
            power: 1.0 + 2.0 * (1.0 - blending),
        }
    }
}

/// Colour grading on Oklab lightness and a, b.
pub fn grade(g: &Grading, lab: [f32; 3]) -> [f32; 3] {
    let [l, mut a, mut b] = lab;
    let x = l.clamp(0.0, 1.0).powf(g.pivot);
    let ws = (1.0 - x).powf(g.power);
    let wh = x.powf(g.power);
    let wm = (1.0 - ws - wh).max(0.0);
    let mut gain = 0.0;
    for (w, wheel) in [ws, wm, wh, 1.0].into_iter().zip(&g.wheels) {
        a += w * wheel[0] * l.max(0.0);
        b += w * wheel[1] * l.max(0.0);
        gain += w * wheel[2];
    }
    [l * (1.0 + gain).max(0.0), a, b]
}

/// One Point Color point's numbers, in Oklab.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Point {
    pub lightness: f32,
    pub chroma: f32,
    pub hue: f32,
    /// Degrees; −1 to 1; stops.
    pub hue_shift: f32,
    pub saturation: f32,
    pub luminance: f32,
    /// Full effect within these of the picked colour, fading to none at
    /// twice as far: degrees of hue, chroma, lightness.
    pub hue_width: f32,
    pub chroma_width: f32,
    pub lightness_width: f32,
}

impl Point {
    pub fn new(p: &crate::edit::ColourPoint) -> Self {
        let r = |v: f64| (v.clamp(0.0, 100.0) / 100.0) as f32;
        Self {
            lightness: p.lightness as f32,
            chroma: p.chroma as f32,
            hue: p.hue as f32,
            hue_shift: (p.hue_shift / 100.0 * 30.0) as f32,
            saturation: (p.saturation / 100.0) as f32,
            luminance: (p.luminance / 100.0) as f32,
            hue_width: 4.0 + 36.0 * r(p.hue_range),
            chroma_width: 0.01 + 0.15 * r(p.saturation_range),
            lightness_width: 0.03 + 0.4 * r(p.luminance_range),
        }
    }

    /// How much of it applies to a colour (Oklab lightness, chroma, hue).
    /// Hue counts less where either colour is nearly grey (its hue means
    /// little there).
    pub fn weight(&self, lum: f32, c: f32, h: f32) -> f32 {
        let d = (h - self.hue).rem_euclid(360.0);
        let dh = d.min(360.0 - d);
        let wh = 1.0 - smoothstep(self.hue_width, 2.0 * self.hue_width, dh);
        let hued = smoothstep(0.0, 0.04, c.min(self.chroma));
        let wh = 1.0 + (wh - 1.0) * hued;
        let wc = 1.0
            - smoothstep(
                self.chroma_width,
                2.0 * self.chroma_width,
                (c - self.chroma).abs(),
            );
        let wl = 1.0
            - smoothstep(
                self.lightness_width,
                2.0 * self.lightness_width,
                (lum - self.lightness).abs(),
            );
        wh * wc * wl
    }
}

/// Oklab lightness, chroma and hue (degrees) as linear Rec.2020.
pub fn from_oklch(l: &Look, lch: [f32; 3]) -> [f32; 3] {
    let hr = lch[2].to_radians();
    let lab = [lch[0], lch[1] * hr.cos(), lch[1] * hr.sin()];
    let lms = mul3(&LAB_TO_LMS, lab).map(|v| v * v * v);
    mul3(&l.from_lms, lms)
}

/// A linear Rec.2020 colour in Oklab: lightness, chroma, hue (degrees).
pub fn oklch(l: &Look, p: [f32; 3]) -> [f32; 3] {
    let lms = mul3(&l.to_lms, p).map(cbrt);
    let [lum, a, b] = mul3(&LMS_TO_LAB, lms);
    [lum, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0)]
}

/// Linear Rec.2020 ↔ Oklab's LMS.
pub(crate) fn lms_matrices() -> ([[f32; 3]; 3], [[f32; 3]; 3]) {
    let to_srgb = colour::mul(
        &colour::xyz_to_rgb(&colour::SRGB, colour::D65),
        &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
    );
    let to = colour::mul(&SRGB_TO_LMS, &to_srgb);
    let from = colour::inv(&to).unwrap_or(colour::IDENTITY);
    (super::f32m(&to), super::f32m(&from))
}

impl Look {
    pub fn new(c: &Colour, cal: &Calibration, bw: &BlackAndWhite) -> Self {
        let (to_lms, from_lms) = lms_matrices();
        let y = colour::rgb_to_xyz(&colour::REC2020, colour::D65)[1];
        let unit = |v: f64| (v / 100.0) as f32;
        let mono = bw.enabled;
        let active = mono
            || cal.shadows_tint != 0.0
            || c.vibrance != 0.0
            || c.saturation != 0.0
            || c.hue
                .iter()
                .chain(&c.hsl_saturation)
                .chain(&c.luminance)
                .any(|&v| v != 0.0);
        Self {
            active,
            to_lms,
            from_lms,
            shadows_tint: unit(cal.shadows_tint),
            vibrance: unit(c.vibrance),
            saturation: unit(c.saturation),
            hue: c.hue.map(|v| (v * 0.3) as f32),
            sat: c.hsl_saturation.map(unit),
            lum: c.luminance.map(unit),
            mono,
            mix: bw.mix.map(unit),
            luma: y.map(|v| v as f32),
            fringe: false,
            purple: 0.0,
            green: 0.0,
            points: [Point::default(); COLOUR_POINTS],
            point_count: 0,
            grading: Grading::default(),
        }
    }

    pub fn with_grading(mut self, g: &crate::edit::ColourGrading) -> Self {
        self.grading = Grading::new(g);
        self.active |= self.grading.on;
        self
    }

    pub fn with_points(mut self, pc: &crate::edit::PointColour) -> Self {
        let n = pc.points.len().min(COLOUR_POINTS);
        for (o, p) in self.points.iter_mut().zip(&pc.points[..n]) {
            *o = Point::new(p);
        }
        self.point_count = n;
        self.active |= n > 0;
        self
    }

    pub fn with_defringe(mut self, d: &crate::edit::Defringe) -> Self {
        self.purple = (d.purple / 100.0) as f32;
        self.green = (d.green / 100.0) as f32;
        self.fringe = self.purple > 0.0 || self.green > 0.0;
        self
    }
}

/// How strongly a hue (degrees) falls in a window `[lo, hi]`, with soft
/// 15° edges.
fn window(h: f32, lo: f32, hi: f32) -> f32 {
    smoothstep(lo - 15.0, lo, h) * (1.0 - smoothstep(hi, hi + 15.0, h))
}

/// The purple and green hue windows (Oklab hue, degrees).
pub const PURPLE: (f32, f32) = (285.0, 345.0);
pub const GREEN: (f32, f32) = (105.0, 165.0);

/// An edge's strength from the luminance either side of a pixel, left,
/// right, up and down: the relative gradient, 0 (flat) to 1 (a hard edge).
pub fn edge(l: f32, r: f32, u: f32, d: f32) -> f32 {
    let g = ((r - l).abs() + (d - u).abs()) / (0.25 * (l + r + u + d)).max(1e-3);
    smoothstep(0.1, 0.6, g)
}

/// Defringe one linear Rec.2020 pixel at an edge of strength `edge`:
/// purple and green lose their colour there.
pub fn defringe_pixel(l: &Look, p: [f32; 3], edge: f32) -> [f32; 3] {
    if !l.fringe || edge <= 0.0 {
        return p;
    }
    let lms = mul3(&l.to_lms, p).map(cbrt);
    let [lum, a, b] = mul3(&LMS_TO_LAB, lms);
    let h = b.atan2(a).to_degrees().rem_euclid(360.0);
    let w = l.purple * window(h, PURPLE.0, PURPLE.1) + l.green * window(h, GREEN.0, GREEN.1);
    let k = (1.0 - w.min(1.0) * edge).max(0.0);
    if k >= 1.0 {
        return p;
    }
    let lms = mul3(&LAB_TO_LMS, [lum, a * k, b * k]).map(|v| v * v * v);
    mul3(&l.from_lms, lms)
}

fn mul3(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
    ]
}

/// Cube root keeping the sign (the shader's `pow` on the magnitude).
fn cbrt(v: f32) -> f32 {
    v.abs().powf(1.0 / 3.0).copysign(v)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The band either side of hue `h` (degrees, 0–360) and how far towards
/// the second it is (smoothed): a band's value is full at its centre and
/// fades to the next band's.
fn bands(h: f32) -> (usize, usize, f32) {
    let mut i = 7;
    for (k, c) in BAND_CENTRES.iter().enumerate() {
        if h >= *c {
            i = k;
        }
    }
    let j = (i + 1) % 8;
    let (a, mut b) = (BAND_CENTRES[i], BAND_CENTRES[j]);
    let mut x = h;
    if b <= a {
        b += 360.0;
    }
    if x < a {
        x += 360.0;
    }
    (i, j, smoothstep(0.0, 1.0, (x - a) / (b - a)))
}

fn band(values: &[f32; 8], (i, j, t): (usize, usize, f32)) -> f32 {
    values[i] + (values[j] - values[i]) * t
}

/// The colour stage for one linear Rec.2020 pixel.
pub fn look_pixel(l: &Look, p: [f32; 3]) -> [f32; 3] {
    look_point(l, p, None).0
}

/// [`look_pixel`], and how much Point Color's point `shown` applied there
/// (0 when none is asked for).
pub fn look_point(l: &Look, p: [f32; 3], shown: Option<usize>) -> ([f32; 3], f32) {
    if !l.active {
        return (p, 0.0);
    }
    let lms = mul3(&l.to_lms, p).map(cbrt);
    let [lum, mut a, b] = mul3(&LMS_TO_LAB, lms);
    // Shadows tint: green (−) to magenta (+), fading out by mid grey.
    let shade = (1.0 - lum / 0.6).clamp(0.0, 1.0);
    a += l.shadows_tint * 0.04 * shade * shade * lum.max(0.0);
    let mut c = a.hypot(b);
    let mut h = b.atan2(a).to_degrees().rem_euclid(360.0);
    let (c0, h0) = (c, h);
    let at = bands(h);
    // How coloured: chroma relative to lightness (greys are left alone).
    let rel = c / lum.max(1e-4);
    let coloured = smoothstep(0.0, 0.08, rel);
    h += band(&l.hue, at) * coloured;
    c *= (1.0 + band(&l.sat, at) * coloured).max(0.0);
    let gain = (band(&l.lum, at) * coloured).exp2();
    let k = cbrt(gain);
    let lum0 = lum;
    let lum = lum * k;
    c *= k;
    // Point Color: each point weighed on the colour as it came in (before
    // HSL), the shifts added up.
    let mut seen = 0.0;
    let (mut lum, mut c) = (lum, c);
    if l.point_count > 0 {
        let (mut dh, mut ks, mut stops) = (0.0, 1.0, 0.0);
        for (i, pt) in l.points[..l.point_count].iter().enumerate() {
            let w = pt.weight(lum0, c0, h0);
            if shown == Some(i) {
                seen = w;
            }
            dh += w * pt.hue_shift;
            ks *= (1.0 + w * pt.saturation).max(0.0);
            stops += w * pt.luminance;
        }
        h += dh;
        c *= ks;
        let k = cbrt(stops.exp2());
        lum *= k;
        c *= k;
    }
    // Vibrance: muted colours more than vivid ones, skin (orange) less.
    let skin = match at {
        (1, _, t) => 1.0 - t,
        (_, 1, t) => t,
        _ => 0.0,
    };
    let muted = 1.0 - smoothstep(0.0, 0.3, rel);
    c *= (1.0 + l.vibrance * muted * (1.0 - 0.6 * skin)).max(0.0);
    c *= (1.0 + l.saturation).max(0.0);
    let hr = h.to_radians();
    let lab = [lum, c * hr.cos(), c * hr.sin()];
    let lms = mul3(&LAB_TO_LMS, lab).map(|v| v * v * v);
    let mut out = mul3(&l.from_lms, lms);
    if l.mono {
        let y = l.luma[0] * out[0] + l.luma[1] * out[1] + l.luma[2] * out[2];
        let rel = c / lum.max(1e-4);
        let mix = band(&l.mix, bands(h.rem_euclid(360.0)));
        out = [y * (mix * 1.5 * smoothstep(0.0, 0.15, rel)).exp2(); 3];
    }
    if l.grading.on {
        // After black and white, so grading can tone a monochrome photo.
        let lms = mul3(&l.to_lms, out).map(cbrt);
        let lab = grade(&l.grading, mul3(&LMS_TO_LAB, lms));
        let lms = mul3(&LAB_TO_LMS, lab).map(|v| v * v * v);
        out = mul3(&l.from_lms, lms);
    }
    (out, seen)
}

/// Calibration's matrix on linear Rec.2020: each primary's hue turned (up
/// to 20°) and saturation scaled (up to ±50%), white kept white.
pub fn calibration_matrix(cal: &Calibration) -> M3 {
    let edits = [
        (cal.red_hue, cal.red_saturation),
        (cal.green_hue, cal.green_saturation),
        (cal.blue_hue, cal.blue_saturation),
    ];
    if edits.iter().all(|&(h, s)| h == 0.0 && s == 0.0) {
        return colour::IDENTITY;
    }
    let to_srgb = colour::mul(
        &colour::xyz_to_rgb(&colour::SRGB, colour::D65),
        &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
    );
    let to_lms = colour::mul(&SRGB_TO_LMS, &to_srgb);
    let Some(from_lms) = colour::inv(&to_lms) else {
        return colour::IDENTITY;
    };
    let f64m = |m: [[f32; 3]; 3]| m.map(|r| r.map(f64::from));
    let (to_lab, to_lms_) = (f64m(LMS_TO_LAB), f64m(LAB_TO_LMS));
    let mut m = [[0.0; 3]; 3];
    for (c, (hue, sat)) in edits.into_iter().enumerate() {
        let mut e = [0.0; 3];
        e[c] = 1.0;
        let lab = colour::mulv(&to_lab, colour::mulv(&to_lms, e).map(f64::cbrt));
        let ch = lab[1].hypot(lab[2]) * (1.0 + sat / 200.0).max(0.0);
        let h = lab[2].atan2(lab[1]) + (hue * 0.2).to_radians();
        let back = colour::mulv(&to_lms_, [lab[0], ch * h.cos(), ch * h.sin()]).map(|v| v * v * v);
        let v = colour::mulv(&from_lms, back);
        for r in 0..3 {
            m[r][c] = v[r];
        }
    }
    // Scale the columns so white maps to white.
    let Some(inv) = colour::inv(&m) else {
        return colour::IDENTITY;
    };
    let k = colour::mulv(&inv, [1.0; 3]);
    colour::mul(&m, &colour::diag(k))
}

/// The tone curve as three tables (red, green, blue) of [`CURVE_SIZE`]
/// entries over display values 0–1; None when it's the identity.
pub fn curve_table(t: &ToneCurve) -> Option<Arc<[f32]>> {
    if t.is_identity() {
        return None;
    }
    let rgb = Spline::new(&t.rgb);
    let chans = [
        Spline::new(&t.red),
        Spline::new(&t.green),
        Spline::new(&t.blue),
    ];
    let mut out = Vec::with_capacity(CURVE_SIZE * 3);
    for spline in &chans {
        let mut last = 0.0f32;
        for i in 0..CURVE_SIZE {
            let x = i as f64 / (CURVE_SIZE - 1) as f64;
            let y = spline.at(rgb.at(parametric(t, x))).clamp(0.0, 1.0) as f32;
            // Never decreasing, whatever the points.
            last = last.max(y);
            out.push(last);
        }
    }
    Some(out.into())
}

/// The table's value for display value `v` (linear interpolation).
pub fn curve_at(table: &[f32], channel: usize, v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0) * (CURVE_SIZE - 1) as f32;
    let i = (x.floor() as usize).min(CURVE_SIZE - 2);
    let t = x - i as f32;
    let row = &table[channel * CURVE_SIZE..];
    row[i] + (row[i + 1] - row[i]) * t
}

/// The parametric curve: each region's slider raises or lowers a smooth
/// hump spanning it (and halfway into its neighbours); the ends stay put.
pub fn parametric(t: &ToneCurve, x: f64) -> f64 {
    let s = t.splits;
    let edges = [0.0, s[0], s[1], s[2], 1.0];
    let centres: Vec<f64> = (0..4).map(|r| (edges[r] + edges[r + 1]) / 2.0).collect();
    let mut y = x;
    for r in 0..4 {
        let amount = t.regions[r] / 100.0;
        if amount == 0.0 {
            continue;
        }
        let lo = if r == 0 { 0.0 } else { centres[r - 1] };
        let hi = if r == 3 { 1.0 } else { centres[r + 1] };
        if x > lo && x < hi {
            let u = (x - lo) / (hi - lo);
            let hump = (std::f64::consts::PI * u).sin().powi(2);
            y += amount * 0.15 * hump;
        }
    }
    y.clamp(0.0, 1.0)
}

/// A monotone cubic (Fritsch–Carlson) through (0, 0), the points and
/// (1, 1).
struct Spline {
    x: Vec<f64>,
    y: Vec<f64>,
    m: Vec<f64>,
}

impl Spline {
    fn new(points: &[[f64; 2]]) -> Self {
        let mut p: Vec<[f64; 2]> = points
            .iter()
            .filter(|p| p[0] > 0.0 && p[0] < 1.0)
            .map(|p| [p[0], p[1].clamp(0.0, 1.0)])
            .collect();
        p.sort_by(|a, b| a[0].total_cmp(&b[0]));
        p.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
        let mut x = vec![0.0];
        let mut y = vec![0.0];
        for q in p {
            x.push(q[0]);
            y.push(q[1]);
        }
        x.push(1.0);
        y.push(1.0);
        let n = x.len();
        let d: Vec<f64> = (0..n - 1)
            .map(|i| (y[i + 1] - y[i]) / (x[i + 1] - x[i]))
            .collect();
        let mut m = vec![0.0; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            m[i] = if d[i - 1] * d[i] <= 0.0 {
                0.0
            } else {
                (d[i - 1] + d[i]) / 2.0
            };
        }
        for i in 0..n - 1 {
            if d[i] == 0.0 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
                continue;
            }
            let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
            let r = a.hypot(b);
            if r > 3.0 {
                m[i] = 3.0 * a / r * d[i];
                m[i + 1] = 3.0 * b / r * d[i];
            }
        }
        Self { x, y, m }
    }

    fn at(&self, v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        let n = self.x.len();
        let i = (1..n).find(|&i| v <= self.x[i]).unwrap_or(n - 1) - 1;
        let h = self.x[i + 1] - self.x[i];
        let t = (v - self.x[i]) / h;
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * self.y[i]
            + (t3 - 2.0 * t2 + t) * h * self.m[i]
            + (-2.0 * t3 + 3.0 * t2) * self.y[i + 1]
            + (t3 - t2) * h * self.m[i + 1]
    }
}
