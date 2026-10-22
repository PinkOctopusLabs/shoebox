//! HDR merge (DECISIONS, "HDR merge"): bracketed raw photos of one scene
//! into one image of its light, in the reference frame's units.
//!
//! Raw data is linear, so there's no response curve to recover (Debevec
//! and Malik's merge without its first half): each frame's pixel, divided
//! by its exposure relative to the reference, estimates the same light,
//! and the estimates are averaged by how much each can be trusted.
//!
//! - **Reference:** the middle exposure (by EXIF), unless chosen. Its size,
//!   colour and EXIF become the result's, and the result's brightness is
//!   its: a render at the defaults looks like it, with the highlights the
//!   shorter frames kept.
//! - **Exposure:** measured from the pixels both frames expose well (the
//!   median ratio, after alignment), which EXIF's nominal times only
//!   approximate; EXIF where too few pixels overlap, and to start from.
//! - **Weights:** inverse variance under a shot-and-read noise model (scaled
//!   by ISO), so a longer exposure counts for more; nothing from pixels the
//!   sensor clipped, tapering off just below clipping. Where every frame
//!   clipped, the shortest one's (highlight-reconstructed) value stands.
//! - **Alignment:** [`super::align`] with a homography (hand-held).
//! - **Deghosting:** each frame is compared with the reference in 4×4
//!   blocks both expose well; where they differ beyond noise (and a
//!   relative tolerance for interpolation), the frame is left out there,
//!   the mask grown and feathered, so moving things come from the
//!   reference alone. Where the reference clipped there's nothing to
//!   compare with, so nothing is left out (basic, as decided).

// Channel loops read clearer indexed than zipped.
#![allow(clippy::needless_range_loop)]

use std::sync::atomic::AtomicBool;

use rayon::prelude::*;

use super::align::{self, Gray};
use super::{Input, MergeError, Merged, Result, Source, check, load, probe};
use crate::colour::{self, M3};
use crate::cpu::Rgb;

/// How hard moving things are looked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Deghost {
    Off,
    Low,
    #[default]
    Medium,
    High,
}

impl Deghost {
    pub const ALL: [Deghost; 4] = [Deghost::Off, Deghost::Low, Deghost::Medium, Deghost::High];

    pub fn label(self) -> &'static str {
        match self {
            Deghost::Off => "Off",
            Deghost::Low => "Low",
            Deghost::Medium => "Medium",
            Deghost::High => "High",
        }
    }

    /// Standard deviations of noise, and a fraction of the value, a
    /// difference must pass to be movement.
    fn thresholds(self) -> Option<(f32, f32)> {
        match self {
            Deghost::Off => None,
            Deghost::Low => Some((8.0, 0.25)),
            Deghost::Medium => Some((5.0, 0.12)),
            Deghost::High => Some((3.5, 0.06)),
        }
    }
}

/// HDR options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Align the frames (hand-held); off for a tripod.
    pub align: bool,
    pub deghost: Deghost,
    /// The reference input (None: the middle exposure).
    pub reference: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            align: true,
            deghost: Deghost::Medium,
            reference: None,
        }
    }
}

/// Where a frame's exposure came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExposureFrom {
    Reference,
    Measured,
    Exif,
}

/// What happened to one frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// Its exposure relative to the reference's (2 = one stop more).
    pub exposure: f64,
    pub exposure_from: ExposureFrom,
    /// What EXIF said, for comparison.
    pub exif_exposure: Option<f64>,
    /// Reference → this frame (None: not aligned, or the reference).
    pub alignment: Option<align::Alignment>,
    /// Why alignment fell back to none, if it did.
    pub align_failed: Option<&'static str>,
    /// Share of the frame left out as movement.
    pub ghosted: f32,
}

/// The merge and what happened to each input (in input order).
pub struct Hdr {
    pub merged: Merged,
    pub frames: Vec<Frame>,
}

/// At least and at most this many frames.
pub const MIN_FRAMES: usize = 2;
pub const MAX_FRAMES: usize = 9;

/// The noise model's variance of a camera-native value `v` (white 1) at
/// `iso`: shot noise of a sensor holding about 50 000 electrons at white at
/// ISO 100, and a read noise of a few electrons, both scaled by gain.
fn noise(iso: f64) -> (f32, f32) {
    let g = (iso / 100.0).max(0.1) as f32;
    (2e-5 * g, 4e-9 * g)
}

/// The weight of one frame's pixel: inverse variance of its estimate of
/// the reference's value, tapering to 0 between 0.85 and 0.96 of white
/// (nothing at all where the sensor clipped).
#[inline]
fn weight(p: [f32; 3], clipped: bool, e: f32, (a, b): (f32, f32)) -> f32 {
    if clipped {
        return 0.0;
    }
    let hi = p[0].max(p[1]).max(p[2]);
    let taper = if hi <= 0.85 {
        1.0
    } else if hi >= 0.96 {
        return 0.0;
    } else {
        let t = (0.96 - hi) / 0.11;
        t * t * (3.0 - 2.0 * t)
    };
    let mean = ((p[0] + p[1] + p[2]) / 3.0).max(0.0);
    taper * e * e / (a * mean + b)
}

/// A weight every pixel gets, largest for the shortest exposure: where
/// every frame clipped, that one's value stands.
#[inline]
fn floor(e: f32) -> f32 {
    1e-12 / (e * e * e * e)
}

/// Luminance of camera-native RGB (for alignment and comparison).
#[inline]
fn luma(p: &[f32]) -> f32 {
    0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2]
}

/// The frame's luminance, divided by its exposure and encoded (square
/// root) for alignment; NaN where clipped or near black.
fn gray(s: &Source, e: f64) -> Gray {
    let mut g = Gray::new(s.width(), s.height());
    let e = e as f32;
    g.data
        .par_iter_mut()
        .zip(s.rgb.data.par_chunks(3))
        .zip(s.clipped.par_iter())
        .for_each(|((o, p), &c)| {
            let y = luma(p);
            let hi = p[0].max(p[1]).max(p[2]);
            *o = if c != 0 || hi > 0.9 || y < 2e-3 {
                f32::NAN
            } else {
                (y / e).sqrt()
            };
        });
    g
}

/// The median ratio of `b`'s luminance to `a`'s over pixels both expose
/// well (where `inside`), sampled to about a million; None below 2000.
fn ratio(a: &Source, b_rgb: &Rgb, b_clipped: &[u8], inside: Option<&[bool]>) -> Option<f64> {
    let n = a.width() * a.height();
    let step = (n / 1_000_000).max(1);
    let mut r: Vec<f32> = (0..n)
        .into_par_iter()
        .step_by(step)
        .filter_map(|i| {
            if inside.is_some_and(|m| !m[i]) || a.clipped[i] != 0 || b_clipped[i] != 0 {
                return None;
            }
            let (p, q) = (&a.rgb.data[i * 3..i * 3 + 3], &b_rgb.data[i * 3..i * 3 + 3]);
            let ok = |p: &[f32]| p[0].max(p[1]).max(p[2]) <= 0.85 && luma(p) >= 0.01;
            (ok(p) && ok(q)).then(|| luma(q) / luma(p))
        })
        .collect();
    if r.len() < 2000 {
        return None;
    }
    let mid = r.len() / 2;
    r.select_nth_unstable_by(mid, f32::total_cmp);
    let v = f64::from(r[mid]);
    (v.is_finite() && v > 0.0).then_some(v)
}

/// Where frame `k` (warped into the reference, its exposure `e`) shows
/// something other than the reference: 0–1 a pixel, grown and feathered.
/// Returns the mask and the share of blocks flagged.
#[allow(clippy::too_many_arguments)]
fn ghosts(
    reference: &Source,
    rgb: &Rgb,
    clipped: &[u8],
    inside: &[bool],
    e: f32,
    noise_ref: (f32, f32),
    noise_k: (f32, f32),
    (sigmas, tolerance): (f32, f32),
) -> (Vec<f32>, f32) {
    const B: usize = 4;
    let (w, h) = (reference.width(), reference.height());
    let (bw, bh) = (w.div_ceil(B), h.div_ceil(B));
    // Per block: flagged (1), fine (0), or can't tell (0).
    let flags: Vec<u8> = (0..bw * bh)
        .into_par_iter()
        .map(|bi| {
            let (bx, by) = (bi % bw * B, bi / bw * B);
            let (mut sr, mut sk, mut n) = (0.0f32, 0.0f32, 0u32);
            for y in by..(by + B).min(h) {
                for x in bx..(bx + B).min(w) {
                    let i = y * w + x;
                    if !inside[i] || reference.clipped[i] != 0 || clipped[i] != 0 {
                        return 0;
                    }
                    let (p, q) = (
                        &reference.rgb.data[i * 3..i * 3 + 3],
                        &rgb.data[i * 3..i * 3 + 3],
                    );
                    if p[0].max(p[1]).max(p[2]) > 0.85 || q[0].max(q[1]).max(q[2]) > 0.85 {
                        return 0;
                    }
                    sr += luma(p);
                    sk += luma(q);
                    n += 1;
                }
            }
            if n == 0 {
                return 0;
            }
            let n = n as f32;
            let (yr, yk) = (sr / n, sk / n);
            let xk = yk / e;
            let var_r = (noise_ref.0 * yr.max(0.0) + noise_ref.1) / n;
            let var_k = (noise_k.0 * yk.max(0.0) + noise_k.1) / (n * e * e);
            let limit = sigmas * (var_r + var_k).sqrt() + tolerance * yr.max(xk);
            u8::from((xk - yr).abs() > limit)
        })
        .collect();
    let flagged = flags.iter().filter(|f| **f != 0).count() as f32 / flags.len().max(1) as f32;
    if flagged == 0.0 {
        return (Vec::new(), 0.0);
    }
    // Grown by the feather's reach plus a block, then feathered outward:
    // inside a ghost the mask stays 1.
    let r = (bw.max(bh) / 250).max(2);
    let grown = super::dilate(&flags, bw, bh, r + 1);
    let mut soft = Gray::new(bw, bh);
    soft.data
        .iter_mut()
        .zip(&grown)
        .for_each(|(o, g)| *o = f32::from(*g));
    let soft = box_blur(&soft, r);
    // Up to full size, bilinear (block centres at (B−1)/2).
    let mut mask = vec![0.0f32; w * h];
    mask.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let fy = ((y as f64 - (B as f64 - 1.0) / 2.0) / B as f64).clamp(0.0, (bh - 1) as f64);
        for (x, o) in row.iter_mut().enumerate() {
            let fx = ((x as f64 - (B as f64 - 1.0) / 2.0) / B as f64).clamp(0.0, (bw - 1) as f64);
            *o = soft.sample(fx, fy).unwrap_or(0.0).clamp(0.0, 1.0);
        }
    });
    (mask, flagged)
}

fn box_blur(g: &Gray, r: usize) -> Gray {
    let (w, h) = (g.width, g.height);
    let n = (2 * r + 1) as f32;
    let mut tmp = Gray::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for d in 0..=2 * r {
                s += g.at((x + d).saturating_sub(r).min(w - 1), y);
            }
            tmp.data[y * w + x] = s / n;
        }
    }
    let mut out = Gray::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for d in 0..=2 * r {
                s += tmp.at(x, (y + d).saturating_sub(r).min(h - 1));
            }
            out.data[y * w + x] = s / n;
        }
    }
    out
}

/// Add one frame's estimates to the running sums.
#[allow(clippy::too_many_arguments)]
fn accumulate(
    num: &mut [f32],
    den: &mut [f32],
    rgb: &Rgb,
    clipped: &[u8],
    inside: Option<&[bool]>,
    ghost: &[f32],
    e: f32,
    n: (f32, f32),
) {
    num.par_chunks_mut(3)
        .zip(den.par_iter_mut())
        .enumerate()
        .for_each(|(i, (s, d))| {
            if inside.is_some_and(|m| !m[i]) {
                return;
            }
            let p = [rgb.data[i * 3], rgb.data[i * 3 + 1], rgb.data[i * 3 + 2]];
            let keep = ghost.get(i).map_or(1.0, |g| 1.0 - g);
            let w = weight(p, clipped[i] != 0, e, n) * keep + floor(e);
            for c in 0..3 {
                s[c] += w * p[c].max(0.0) / e;
            }
            *d += w;
        });
}

/// Merge bracketed raw photos (see the module's notes). `progress` hears
/// 0–1 as frames are done; `cancel` stops it between steps.
pub fn merge(
    inputs: &[Input],
    o: &Options,
    progress: &(dyn Fn(f32) + Sync),
    cancel: &AtomicBool,
) -> Result<Hdr> {
    if !(MIN_FRAMES..=MAX_FRAMES).contains(&inputs.len()) {
        return Err(MergeError::Refused(format!(
            "an HDR merge takes {MIN_FRAMES} to {MAX_FRAMES} photos"
        )));
    }
    let probed: Vec<_> = inputs.iter().map(probe).collect::<Result<_>>()?;
    let stops: Vec<Option<f64>> = probed.iter().map(|(e, _)| e.stops()).collect();
    let reference = match o.reference {
        Some(r) if r < inputs.len() => r,
        _ if stops.iter().all(Option::is_some) => {
            let mut order: Vec<usize> = (0..inputs.len()).collect();
            order.sort_by(|&a, &b| stops[a].unwrap_or(0.0).total_cmp(&stops[b].unwrap_or(0.0)));
            order[(order.len() - 1) / 2]
        }
        _ => 0,
    };
    let steps = inputs.len() as f32 + 0.5;
    progress(0.0);
    let r = load(&inputs[reference], cancel)?;
    let (w, h) = (r.width(), r.height());
    let iso = |s: &Source| s.exposure.iso.unwrap_or(100.0);
    let noise_ref = noise(iso(&r));
    let pyramid_ref = align::pyramid(gray(&r, 1.0), 64);
    let mut num = vec![0.0f32; w * h * 3];
    let mut den = vec![0.0f32; w * h];
    accumulate(
        &mut num,
        &mut den,
        &r.rgb,
        &r.clipped,
        None,
        &[],
        1.0,
        noise_ref,
    );
    let mut frames: Vec<Option<Frame>> = vec![None; inputs.len()];
    frames[reference] = Some(Frame {
        exposure: 1.0,
        exposure_from: ExposureFrom::Reference,
        exif_exposure: Some(1.0),
        alignment: None,
        align_failed: None,
        ghosted: 0.0,
    });
    progress(0.5 / steps);
    // Nearest exposures first (no reason beyond a tidy report).
    let mut order: Vec<usize> = (0..inputs.len()).filter(|&i| i != reference).collect();
    order.sort_by(|&a, &b| {
        let d = |i: usize| match (stops[i], stops[reference]) {
            (Some(s), Some(t)) => (s - t).abs(),
            _ => 0.0,
        };
        d(a).total_cmp(&d(b))
    });
    for (done, &k) in order.iter().enumerate() {
        check(cancel)?;
        let s = load(&inputs[k], cancel)?;
        if (s.width(), s.height()) != (w, h) {
            return Err(MergeError::Refused(format!(
                "{} is {}×{}, the others {}×{}: brackets are the same size",
                super::file_name(&s.path),
                s.width(),
                s.height(),
                w,
                h
            )));
        }
        if (s.meta.make.as_str(), s.meta.model.as_str())
            != (r.meta.make.as_str(), r.meta.model.as_str())
        {
            return Err(MergeError::Refused(format!(
                "{} is from another camera",
                super::file_name(&s.path)
            )));
        }
        let exif = match (stops[k], stops[reference]) {
            (Some(a), Some(b)) => Some(2f64.powf(a - b)),
            _ => None,
        };
        // To start from: EXIF, or the pixels as they are.
        let guess = exif
            .or_else(|| ratio(&r, &s.rgb, &s.clipped, None))
            .ok_or_else(|| {
                MergeError::Refused(format!(
                    "can't tell how {} was exposed (no EXIF, and too little in common)",
                    super::file_name(&s.path)
                ))
            })?;
        let (mut alignment, mut align_failed) = (None, None);
        if o.align {
            let pyramid = align::pyramid(gray(&s, guess), 64);
            match align::align(&pyramid_ref, &pyramid, &align::Options::default()) {
                Ok(a) => alignment = Some(a),
                Err(e) => {
                    tracing::warn!("HDR: {}: {e}", super::file_name(&s.path));
                    align_failed = Some("couldn't be aligned");
                }
            }
        }
        check(cancel)?;
        let moved = alignment
            .as_ref()
            .is_some_and(|a| align::distance(&a.h, &colour::IDENTITY, w, h) > 0.01);
        let (rgb, clipped, inside) = match &alignment {
            Some(a) if moved => {
                let (rgb, inside) = align::warp(&s.rgb, &a.h, w, h);
                let clipped = align::warp_mask(&s.clipped, w, h, &a.h, w, h);
                (rgb, clipped, Some(inside))
            }
            _ => (s.rgb, s.clipped, None),
        };
        let measured = ratio(&r, &rgb, &clipped, inside.as_deref());
        let (e, from) = match measured {
            Some(m) => (m, ExposureFrom::Measured),
            None => (guess, ExposureFrom::Exif),
        };
        if let (Some(m), Some(x)) = (measured, exif)
            && (m / x).log2().abs() > 0.34
        {
            tracing::info!(
                "HDR: {} measured {:+.2} EV from the reference, EXIF says {:+.2}",
                super::file_name(&inputs[k].path),
                m.log2(),
                x.log2()
            );
        }
        let all_inside;
        let inside_ref: &[bool] = match &inside {
            Some(v) => v,
            None => {
                all_inside = vec![true; w * h];
                &all_inside
            }
        };
        let noise_k = noise(s.exposure.iso.unwrap_or(100.0));
        let (ghost, ghosted) = match o.deghost.thresholds() {
            Some(t) => ghosts(
                &r, &rgb, &clipped, inside_ref, e as f32, noise_ref, noise_k, t,
            ),
            None => (Vec::new(), 0.0),
        };
        accumulate(
            &mut num,
            &mut den,
            &rgb,
            &clipped,
            inside.as_deref(),
            &ghost,
            e as f32,
            noise_k,
        );
        frames[k] = Some(Frame {
            exposure: e,
            exposure_from: from,
            exif_exposure: exif,
            alignment,
            align_failed,
            ghosted,
        });
        progress((done as f32 + 1.5) / steps);
    }
    check(cancel)?;
    let mut rgb = Rgb::new(w, h);
    rgb.data
        .par_chunks_mut(3)
        .zip(num.par_chunks(3))
        .zip(den.par_iter())
        .for_each(|((o, s), &d)| {
            for c in 0..3 {
                o[c] = if d > 0.0 { s[c] / d } else { 0.0 };
            }
        });
    let frames = frames
        .into_iter()
        .map(|f| f.ok_or_else(|| MergeError::Failed("a frame was skipped".into())))
        .collect::<Result<Vec<_>>>()?;
    progress(1.0);
    Ok(Hdr {
        merged: Merged {
            rgb,
            reference,
            meta: r.meta,
            as_shot: r.as_shot,
            metadata: r.metadata,
        },
        frames,
    })
}

/// The rotation (degrees, clockwise in image coordinates) and translation
/// of a transform about the image's centre: for reports and tests.
pub fn motion(h: &M3, width: usize, height: usize) -> (f64, f64, f64) {
    let (cx, cy) = ((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0);
    let (x0, y0) = align::apply(h, cx, cy);
    let (x1, y1) = align::apply(h, cx + 100.0, cy);
    ((y1 - y0).atan2(x1 - x0).to_degrees(), x0 - cx, y0 - cy)
}
