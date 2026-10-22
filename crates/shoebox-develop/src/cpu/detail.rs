//! The detail stage: noise reduction (EDITING-SPEC, Noise reduction, tier
//! 1), sharpening (module 13, with Detail and Masking) and texture (module
//! 8's fine scale), on the source before everything else, as one à-trous
//! wavelet decomposition:
//!
//! 1. A generalised Anscombe transform per channel makes the noise unit
//!    variance; its parameters (Poisson gain `a`, read-noise variance `b`)
//!    are estimated from the source's flattest blocks, so no camera noise
//!    profiles are needed.
//! 2. An orthonormal luma/chroma basis keeps that unit variance.
//! 3. Five à-trous scales (B3 spline, holes `2^s`): each band is
//!    soft-thresholded (luma and chroma strengths), then the luma bands
//!    gain sharpening (the fine scales, by radius) and texture (the middle
//!    scales), masked by edge strength and with halos limited by Detail.
//! 4. Back through the basis and the inverse transform.
//!
//! Radii are in the original's pixels: `scale` (source pixels per original
//! pixel) moves the bands, so the proxy looks like a downscaled full render.
//! The GPU stage (`gpu::detail`) runs the same passes.

use rayon::prelude::*;

use super::Rgb;
use crate::edit::Settings;

/// Scales of the decomposition.
pub const SCALES: usize = 5;

/// The noise of unit-variance white noise in each à-trous band (B3 spline,
/// Starck and Murtagh).
pub const BAND_SIGMA: [f32; SCALES] = [0.8908, 0.2007, 0.0856, 0.0413, 0.0205];

/// A source's noise: per channel, variance = `a·x + b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Noise {
    pub a: [f32; 3],
    pub b: [f32; 3],
}

/// Everything the stage needs, per band.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detail {
    pub noise: Noise,
    /// Soft thresholds per band, luma and chroma (in Anscombe units).
    pub luma_threshold: [f32; SCALES],
    pub chroma_threshold: [f32; SCALES],
    /// Of the luma detail removed, how much is kept (0–1).
    pub keep: f32,
    /// Luma gain per band (sharpening and texture), −1 and up.
    pub gain: [f32; SCALES],
    /// Edge strength (noise units) below which sharpening fades out; 0:
    /// no masking.
    pub mask: f32,
    /// Halo limit: how far a band may be pushed, in noise units.
    pub limit: f32,
}

impl Detail {
    /// The stage for these settings, or None when it would do nothing.
    /// `scale`: source pixels per original pixel.
    pub fn new(s: &Settings, noise: Noise, scale: f32) -> Option<Self> {
        let nr = &s.modules.noise_reduction;
        let sh = &s.modules.sharpening;
        let texture = s.modules.clarity.texture;
        if nr.luminance <= 0.0 && nr.chroma <= 0.0 && sh.amount <= 0.0 && texture == 0.0 {
            return None;
        }
        let lum = (nr.luminance / 100.0) as f32;
        let chroma = (nr.chroma / 100.0) as f32;
        let luma_threshold = BAND_SIGMA.map(|s| 3.0 * s * lum);
        let chroma_threshold = BAND_SIGMA.map(|s| 4.0 * s * chroma);
        // Bands by size at this resolution: band s holds detail ~2^s px.
        let bump = |centre: f32, width: f32| -> [f32; SCALES] {
            std::array::from_fn(|b| (-0.5 * ((b as f32 - centre) / width).powi(2)).exp())
        };
        let mut gain = [0f32; SCALES];
        let amount = (sh.amount / 100.0) as f32;
        if amount > 0.0 {
            let r = (sh.radius as f32 * scale).max(1e-3);
            let w = bump(r.log2().max(0.0), 0.75);
            // Below a pixel, the sharpening fades with it.
            let fade = (r / 0.5).min(1.0);
            for b in 0..SCALES {
                gain[b] += 1.5 * amount * w[b] * fade;
            }
        }
        if texture != 0.0 {
            let w = bump((6.0 * scale).log2().max(0.0), 1.0);
            for b in 0..SCALES {
                gain[b] += 0.8 * (texture / 100.0) as f32 * w[b];
            }
        }
        Some(Self {
            noise,
            luma_threshold,
            chroma_threshold,
            keep: 0.5 * (nr.detail / 100.0) as f32,
            gain: gain.map(|g| g.max(-1.0)),
            mask: (sh.masking / 100.0) as f32 * 30.0,
            limit: 2.0 + 20.0 * (sh.detail / 100.0) as f32,
        })
    }
}

/// Generalised Anscombe transform: unit-variance noise.
pub fn anscombe(x: f32, a: f32, b: f32) -> f32 {
    2.0 / a * (a * x + 0.375 * a * a + b).max(0.0).sqrt()
}

/// Its algebraic inverse.
pub fn inverse_anscombe(t: f32, a: f32, b: f32) -> f32 {
    let h = a * t / 2.0;
    (h * h - 0.375 * a * a - b) / a
}

const S3: f32 = 0.577_350_26; // 1/√3
const S2: f32 = 0.707_106_77; // 1/√2
const S6: f32 = 0.408_248_3; // 1/√6

/// Orthonormal luma/chroma.
pub fn to_opponent(p: [f32; 3]) -> [f32; 3] {
    [
        (p[0] + p[1] + p[2]) * S3,
        (p[0] - p[2]) * S2,
        (p[0] - 2.0 * p[1] + p[2]) * S6,
    ]
}

pub fn from_opponent(q: [f32; 3]) -> [f32; 3] {
    [
        q[0] * S3 + q[1] * S2 + q[2] * S6,
        q[0] * S3 - 2.0 * q[2] * S6,
        q[0] * S3 - q[1] * S2 + q[2] * S6,
    ]
}

/// The B3 spline taps.
pub const TAPS: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// One à-trous blur at hole size `step`, rows or columns (clamped edges).
fn blur(src: &[[f32; 3]], w: usize, h: usize, step: usize, rows: bool) -> Vec<[f32; 3]> {
    let mut out = vec![[0f32; 3]; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
        for (x, o) in line.iter_mut().enumerate() {
            let mut acc = [0f32; 3];
            for (k, t) in TAPS.iter().enumerate() {
                let d = (k as isize - 2) * step as isize;
                let (xx, yy) = if rows {
                    ((x as isize + d).clamp(0, w as isize - 1) as usize, y)
                } else {
                    (x, (y as isize + d).clamp(0, h as isize - 1) as usize)
                };
                let p = src[yy * w + xx];
                for c in 0..3 {
                    acc[c] += t * p[c];
                }
            }
            *o = acc;
        }
    });
    out
}

fn shrink(v: f32, t: f32) -> f32 {
    v.signum() * (v.abs() - t).max(0.0)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One band's coefficients `w` (luma, chroma, chroma) after denoise and
/// enhancement, at band `s`.
pub fn band(d: &Detail, s: usize, w: [f32; 3]) -> [f32; 3] {
    let sigma = BAND_SIGMA[s];
    let mut l = shrink(w[0], d.luma_threshold[s]);
    l += d.keep * (w[0] - l);
    let gain = d.gain[s];
    if gain != 0.0 {
        let edge = w[0].abs() / sigma;
        let mask = if d.mask > 0.0 {
            smoothstep(0.3 * d.mask, d.mask, edge)
        } else {
            1.0
        };
        let push = gain * mask * l;
        let limit = d.limit * sigma;
        l += push / (1.0 + push.abs() / limit);
    }
    [
        l,
        shrink(w[1], d.chroma_threshold[s]),
        shrink(w[2], d.chroma_threshold[s]),
    ]
}

/// Into the stage's working space: Anscombe per channel, then opponent.
pub fn forward(d: &Detail, p: [f32; 3]) -> [f32; 3] {
    to_opponent(std::array::from_fn(|c| {
        anscombe(p[c], d.noise.a[c], d.noise.b[c])
    }))
}

/// Back out of it.
pub fn backward(d: &Detail, q: [f32; 3]) -> [f32; 3] {
    let t = from_opponent(q);
    std::array::from_fn(|c| inverse_anscombe(t[c], d.noise.a[c], d.noise.b[c]))
}

/// Run the stage over a whole image.
pub fn process(img: &Rgb, d: &Detail) -> Rgb {
    let (w, h) = (img.width, img.height);
    let mut c: Vec<[f32; 3]> = img
        .data
        .as_chunks::<3>()
        .0
        .par_iter()
        .map(|p| forward(d, *p))
        .collect();
    let mut acc = vec![[0f32; 3]; w * h];
    for s in 0..SCALES {
        let next = blur(&blur(&c, w, h, 1 << s, true), w, h, 1 << s, false);
        acc.par_iter_mut()
            .zip(c.par_iter().zip(&next))
            .for_each(|(a, (cur, nxt))| {
                let wv = [cur[0] - nxt[0], cur[1] - nxt[1], cur[2] - nxt[2]];
                let b = band(d, s, wv);
                for k in 0..3 {
                    a[k] += b[k];
                }
            });
        c = next;
    }
    let mut out = Rgb::new(w, h);
    out.data
        .as_chunks_mut::<3>()
        .0
        .par_iter_mut()
        .zip(c.par_iter().zip(&acc))
        .for_each(|(o, (base, a))| {
            *o = backward(d, [base[0] + a[0], base[1] + a[1], base[2] + a[2]]);
        });
    out
}

/// Where the stage is run to fit [`Sharpen`]: this much extra amount, and
/// 1 (100).
pub const FIT_AT: f32 = 0.25;

/// Sharpening's change to an image for an extra amount `e` (1 is the
/// global module's 100), per pixel: `alpha·e / (1 + beta·e)`. The halo
/// limit makes the stage's push saturate (`push / (1 + |push| / limit)`),
/// so a straight line through one run is up to half out at small
/// amounts; this curve, through the stage's own results at
/// [`FIT_AT`] and 1, follows it to about 2% (see DECISIONS, "Local AI
/// denoise strength and sharpening"). `beta` is per pixel (the limit acts
/// on luma, shared by the channels), `alpha` per channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Sharpen {
    pub alpha: Rgb,
    pub beta: Vec<f32>,
}

impl Sharpen {
    /// The fit through `quarter` and `full`, the stage's results with
    /// [`FIT_AT`] and 1 more sharpening than `main`'s.
    pub fn fit(main: &Rgb, quarter: &Rgb, full: &Rgb) -> Self {
        let mut alpha = Rgb::new(main.width, main.height);
        let beta: Vec<f32> = alpha
            .data
            .as_chunks_mut::<3>()
            .0
            .par_iter_mut()
            .zip(main.data.as_chunks::<3>().0.par_iter())
            .zip(quarter.data.as_chunks::<3>().0.par_iter())
            .zip(full.data.as_chunks::<3>().0.par_iter())
            .map(|(((a, m), q), f)| {
                let dq: [f32; 3] = std::array::from_fn(|c| q[c] - m[c]);
                let d1: [f32; 3] = std::array::from_fn(|c| f[c] - m[c]);
                let (sq, s1) = (dq[0] + dq[1] + dq[2], d1[0] + d1[1] + d1[2]);
                // r = d(¼)/d(1) = (1 + β)/(4 + β), from ¼ (straight) up.
                let r = if s1.abs() > 1e-9 {
                    (sq / s1).clamp(FIT_AT, 0.95)
                } else {
                    FIT_AT
                };
                let b = (r / FIT_AT - 1.0) / (1.0 - r);
                *a = d1.map(|v| v * (1.0 + b));
                b
            })
            .collect();
        Self { alpha, beta }
    }

    /// The change at pixel `(x, y)` for extra amount `e`.
    #[inline]
    pub fn at(&self, x: usize, y: usize, e: f32) -> [f32; 3] {
        let k = e / (1.0 + self.beta[y * self.alpha.width + x] * e);
        self.alpha.px(x, y).map(|a| a * k)
    }
}

/// How much of each band softening takes away at full strength: all
/// detail up to about two of the original's pixels, fading out by eight
/// (`scale`: source pixels per original pixel, so the proxy matches).
pub fn soften_weights(scale: f32) -> [f32; SCALES] {
    std::array::from_fn(|b| 1.0 - smoothstep(1.0, 3.0, b as f32 - scale.max(1e-3).log2()))
}

/// Softening's change to `img` at full strength: less its finest bands
/// (see [`soften_weights`]), on linear values.
pub fn soften(img: &Rgb, scale: f32) -> Rgb {
    let (w, h) = (img.width, img.height);
    let weights = soften_weights(scale);
    let mut c: Vec<[f32; 3]> = img.data.as_chunks::<3>().0.to_vec();
    let mut out = vec![[0f32; 3]; w * h];
    for (s, k) in weights.iter().enumerate() {
        if *k <= 0.0 {
            break;
        }
        let next = blur(&blur(&c, w, h, 1 << s, true), w, h, 1 << s, false);
        out.par_iter_mut()
            .zip(c.par_iter().zip(&next))
            .for_each(|(o, (cur, nxt))| {
                for i in 0..3 {
                    o[i] -= k * (cur[i] - nxt[i]);
                }
            });
        c = next;
    }
    Rgb {
        width: w,
        height: h,
        data: out.into_flattened(),
    }
}

/// Least squares `y = slope·x + icept` (slope and intercept kept positive).
fn fit(pts: &[(f64, f64)]) -> Option<(f64, f64)> {
    if pts.len() < 2 {
        return None;
    }
    // Weighted by relative error (1/v²), so the dark bins, where read
    // noise shows, count as much as the bright ones.
    let (mut s, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for &(x, y) in pts {
        let w = 1.0 / (y * y).max(1e-30);
        s += w;
        sx += w * x;
        sy += w * y;
        sxx += w * x * x;
        sxy += w * x * y;
    }
    let det = s * sxx - sx * sx;
    let slope = if det > 0.0 {
        ((s * sxy - sx * sy) / det).max(1e-8)
    } else {
        1e-8
    };
    Some((slope, ((sy - slope * sx) / s).max(1e-10)))
}

/// Estimate the noise from the flattest blocks: per channel, local means
/// and the variance of a Laplacian-like residual, the lowest tenth of each
/// brightness bin fitted to `variance = a·mean + b`.
pub fn estimate(img: &Rgb) -> Noise {
    const BLOCK: usize = 8;
    const BINS: usize = 16;
    let (w, h) = (img.width, img.height);
    let mut a = [1e-4f32; 3];
    let mut b = [1e-6f32; 3];
    if w < BLOCK * 4 || h < BLOCK * 4 {
        return Noise { a, b };
    }
    let step = ((w * h / 4000) as f64).sqrt().max(BLOCK as f64) as usize;
    let starts: Vec<(usize, usize)> = (1..h - BLOCK - 1)
        .step_by(step)
        .flat_map(|y| (1..w - BLOCK - 1).step_by(step).map(move |x| (x, y)))
        .collect();
    for c in 0..3 {
        let stats: Vec<(f32, f32)> = starts
            .par_iter()
            .map(|&(x0, y0)| {
                let (mut sum, mut res) = (0f64, 0f64);
                for y in y0..y0 + BLOCK {
                    for x in x0..x0 + BLOCK {
                        let v = img.px(x, y)[c];
                        let n = (img.px(x - 1, y)[c]
                            + img.px(x + 1, y)[c]
                            + img.px(x, y - 1)[c]
                            + img.px(x, y + 1)[c])
                            / 4.0;
                        sum += f64::from(v);
                        res += f64::from(v - n).powi(2);
                    }
                }
                let n = (BLOCK * BLOCK) as f64;
                ((sum / n) as f32, (res / n / 1.25) as f32)
            })
            .collect();
        let hi = stats.iter().map(|s| s.0).fold(0f32, f32::max).max(1e-6);
        let mut bins: Vec<Vec<f32>> = vec![Vec::new(); BINS];
        let mut means = [0f64; BINS];
        for (m, v) in &stats {
            if *m <= 0.0 || *v <= 1e-12 {
                continue; // black or clipped: no noise to see
            }
            // Bins by stops below the brightest block (a night scene's
            // blocks are mostly a hundredth of its lights).
            let stops = (hi / m).log2().clamp(0.0, BINS as f32 - 1.0);
            let i = stops as usize;
            bins[i].push(*v);
            means[i] += f64::from(*m);
        }
        let mut pts = Vec::new();
        for (i, bin) in bins.iter_mut().enumerate() {
            if bin.len() < 4 {
                continue;
            }
            let mean = means[i] / bin.len() as f64;
            bin.sort_by(f32::total_cmp);
            pts.push((mean, f64::from(bin[bin.len() / 10])));
        }
        // Fit, then drop bins far above the line (only edges in them) and
        // fit again.
        for _ in 0..3 {
            let Some((slope, icept)) = fit(&pts) else {
                break;
            };
            a[c] = slope as f32;
            b[c] = icept as f32;
            let before = pts.len();
            pts.retain(|p| p.1 <= 3.0 * (slope * p.0 + icept));
            if pts.len() == before {
                break;
            }
        }
        if pts.len() == 1 {
            b[c] = pts[0].1.max(1e-10) as f32;
        }
    }
    Noise { a, b }
}
