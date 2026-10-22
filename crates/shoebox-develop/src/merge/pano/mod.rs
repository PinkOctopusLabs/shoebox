//! Panorama (DECISIONS, "Panorama"): overlapping raw photos stitched into
//! one, after Brown and Lowe, "Automatic Panoramic Image Stitching using
//! Invariant Features" (2007), Hugin and OpenCV's stitching pipeline. No
//! code is ported; the methods are the papers'.
//!
//! 1. **Frames** ([`super::load`]), each corrected for its lens
//!    ([`optics`]: distortion, vignetting, chromatic aberration) as it's
//!    read, so seams line up and corners don't band. Two small corrected
//!    previews are kept (features, and measuring); the frame is dropped.
//! 2. **Matching:** Harris features ([`super::align`]) of every pair,
//!    RANSAC homographies, kept when enough agree (Brown and Lowe's test:
//!    more than 8 + 0.3 × the features in the overlap). A frame that
//!    overlaps none is refused by name.
//! 3. **Registration** ([`bundle`]): a camera turning about one point, a
//!    rotation per frame and one focal length (EXIF and the crop factor to
//!    start), bundle adjusted; frames that fit a plane much better (a
//!    scan, a mural shot from several places) are laid flat instead.
//!    Straightened from the frames' axes.
//! 4. **Exposure:** a gain per frame, solved jointly from the median
//!    brightness ratio of every overlap, in the reference's units (the
//!    best-connected frame, whose colour and EXIF the result takes).
//! 5. **Projection** ([`project`]): Spherical, Cylindrical or Perspective
//!    (Auto by the extent); a flat layout is drawn flat.
//! 6. **Seams** ([`seam`]) on a small canvas: centre-weighted, then graph
//!    cuts where the frames agree; the largest inner rectangle for the
//!    crop.
//! 7. **Blending** ([`blend`]): multi-band, each frame read again and
//!    warped once (projection, lens and gain in one resample), so memory
//!    is the result's and one frame's.

// Channel loops read clearer indexed than zipped.
#![allow(clippy::needless_range_loop)]

pub mod blend;
pub mod bundle;
pub mod geom;
pub mod optics;
pub mod project;
pub mod seam;

use std::sync::atomic::AtomicBool;

use rayon::prelude::*;
use shoebox_decode::raw::RawImage;

use super::align::{self, Feature, Gray, Model, Pair};
use super::{Input, MergeError, Merged, Result, check, load, probe};
use crate::colour::M3;
use crate::cpu::Rgb;
use bundle::{Fit, Link, Rotations};
pub use project::Projection;
use project::{Geometry, Layout};

/// At least and at most this many frames.
pub const MIN_FRAMES: usize = 2;
pub const MAX_FRAMES: usize = 60;
/// The largest result (pixels): bigger panoramas are drawn smaller to fit
/// (blending holds about 21 bytes a pixel, and one frame).
pub const MAX_PIXELS: usize = 150_000_000;
/// And its longest side (DNG's limit, less a margin).
pub const MAX_SIDE: usize = 65_000;

/// Features are found on a copy no bigger than this.
const FEATURE_EDGE: usize = 1600;
/// Exposure and seams are measured on one no bigger than this.
const PREVIEW_EDGE: usize = 1024;
/// The seam canvas, about this many pixels.
const CANVAS_PIXELS: f64 = 1.0e6;
/// At most this many matches a pair are adjusted on.
const PAIRS_PER_LINK: usize = 300;

/// Panorama options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub projection: Projection,
    /// Crop to the largest rectangle inside what the frames cover (else
    /// the whole boundary, empty corners black: a DNG has no transparency).
    pub crop: bool,
    /// Correct each frame's lens first (from the NEF's own data and
    /// Lensfun).
    pub lens: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            projection: Projection::Auto,
            crop: true,
            lens: true,
        }
    }
}

/// What happened to one frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// World → camera (straightened), for a turning camera.
    pub rotation: Option<M3>,
    /// Frame → the plane, for a flat layout.
    pub plane: Option<M3>,
    /// What its pixels were multiplied by to match the reference's
    /// exposure.
    pub gain: f64,
    /// Frames it was matched with.
    pub neighbours: usize,
    /// Its lens corrections ("" if none).
    pub lens: String,
}

/// The panorama and how it was made.
pub struct Panorama {
    pub merged: Merged,
    pub frames: Vec<Frame>,
    /// The focal length found (full-size pixels; None when laid flat).
    pub focal: Option<f64>,
    /// What EXIF suggested, to start from.
    pub focal_exif: Option<f64>,
    /// The projection drawn (Perspective when laid flat).
    pub projection: Projection,
    pub flat: bool,
    /// How well the frames fit (transfer errors, full-size pixels).
    pub fit: Fit,
    /// Where the result's pixels look.
    pub layout: Layout,
    /// The whole panorama's size before cropping.
    pub uncropped: (usize, usize),
    /// Worth telling the person.
    pub notes: Vec<String>,
}

/// One frame as kept between the passes.
struct Kept {
    width: usize,
    height: usize,
    optics: optics::Optics,
    lens: String,
    features: Vec<Feature>,
    /// Full-size pixels per feature pixel.
    feature_step: usize,
    feature_size: (usize, usize),
    preview: optics::Preview,
    /// The preview's square root, halved and halved (for refining).
    pyramid: Vec<Gray>,
    meta: RawImage,
    as_shot: [f32; 3],
    metadata: Option<rawler::decoders::RawMetadata>,
    shot: crate::lens::Shot,
}

fn refused(s: impl Into<String>) -> MergeError {
    MergeError::Refused(s.into())
}

/// Square roots (as alignment wants: noise more even, a gain still a
/// gain).
fn sqrt_gray(g: &Gray) -> Gray {
    let mut g = g.clone();
    g.data.iter_mut().for_each(|v| {
        *v = if v.is_finite() {
            v.max(0.0).sqrt()
        } else {
            f32::NAN
        }
    });
    g
}

/// Read a frame and keep what matching needs.
fn keep(input: &Input, o: &Options, cancel: &AtomicBool) -> Result<Kept> {
    let src = load(input, cancel)?;
    check(cancel)?;
    let shot = optics::shot(input, &src);
    let (optics, lens) = optics::optics(&shot, &src, input.orientation, o.lens);
    let fp = optics::preview(&src, &optics, FEATURE_EDGE);
    let g = sqrt_gray(&fp.gray);
    let features = align::features(&g, 2000);
    let preview = optics::preview(&src, &optics, PREVIEW_EDGE);
    let pyramid = align::pyramid(sqrt_gray(&preview.gray), 64);
    let mut meta = src.meta.clone();
    meta.data = Vec::new();
    Ok(Kept {
        width: src.width(),
        height: src.height(),
        optics,
        lens,
        features,
        feature_step: fp.step,
        feature_size: (g.width, g.height),
        preview,
        pyramid,
        meta,
        as_shot: src.as_shot,
        metadata: src.metadata,
        shot,
    })
}

/// Match two frames: their link if enough agree.
fn link(a: usize, b: usize, ka: &Kept, kb: &Kept) -> Option<Link> {
    let pairs = align::matches(&ka.features, &kb.features, None);
    if pairs.len() < 12 {
        return None;
    }
    let (h, inl) = align::ransac(Model::Homography, &pairs, 1.5)?;
    let agree: Vec<Pair> = pairs
        .iter()
        .zip(&inl)
        .filter(|(_, ok)| **ok)
        .map(|(p, _)| *p)
        .collect();
    // Brown and Lowe's test: the agreeing share of the features both see.
    let inside = |f: &[Feature], h: &M3, size: (usize, usize)| {
        f.iter()
            .filter(|p| {
                let (x, y) = align::apply(h, f64::from(p.x), f64::from(p.y));
                x >= 0.0 && y >= 0.0 && x <= size.0 as f64 - 1.0 && y <= size.1 as f64 - 1.0
            })
            .count()
    };
    let hi = crate::colour::inv(&h)?;
    let overlap = (inside(&ka.features, &h, kb.feature_size)
        + inside(&kb.features, &hi, ka.feature_size)) as f64
        / 2.0;
    if agree.len() < 12 || (agree.len() as f64) < 8.0 + 0.3 * overlap {
        return None;
    }
    // To full-size pixels.
    let full = |p: [f64; 2], s: usize| {
        let o = (s as f64 - 1.0) / 2.0;
        [p[0] * s as f64 + o, p[1] * s as f64 + o]
    };
    let all: Vec<Pair> = agree
        .iter()
        .map(|(p, q)| (full(*p, ka.feature_step), full(*q, kb.feature_step)))
        .collect();
    let h = align::fit(Model::Homography, &all)?;
    // Refined on the pixels (to hundredths of a preview pixel, where the
    // features are good to a few tenths), then matches taken from it on a
    // grid over the overlap. Without refinement, the features' own.
    let (sa, sb) = (ka.preview.step, kb.preview.step);
    let to_full = |s: usize| {
        let o = (s as f64 - 1.0) / 2.0;
        [[s as f64, 0.0, o], [0.0, s as f64, o], [0.0, 0.0, 1.0]]
    };
    let (fa, fb) = (to_full(sa), to_full(sb));
    let fbi = crate::colour::inv(&fb)?;
    let start = geom::mul(&fbi, &geom::mul(&h, &fa));
    let refined = align::refine(
        &ka.pyramid,
        &kb.pyramid,
        &start,
        Model::Homography,
        2.min(ka.pyramid.len() - 1),
    )
    .filter(|r| {
        let (w, ht) = (ka.preview.gray.width, ka.preview.gray.height);
        align::distance(&r.h, &start, w, ht) < 3.0
    });
    let mut pairs: Vec<Pair> = Vec::new();
    let mut h_full = h;
    if let Some(r) = refined {
        let (ga, gb) = (&ka.preview.gray, &kb.preview.gray);
        for j in 0..=16 {
            for i in 0..=24 {
                let p = (
                    i as f64 * (ga.width - 1) as f64 / 24.0,
                    j as f64 * (ga.height - 1) as f64 / 16.0,
                );
                let q = align::apply(&r.h, p.0, p.1);
                if ga.sample(p.0, p.1).is_some() && gb.sample(q.0, q.1).is_some() {
                    let pf = align::apply(&fa, p.0, p.1);
                    let qf = align::apply(&fb, q.0, q.1);
                    pairs.push(([pf.0, pf.1], [qf.0, qf.1]));
                }
            }
        }
        if pairs.len() >= 20 {
            h_full = geom::mul(&fb, &geom::mul(&r.h, &crate::colour::inv(&fa)?));
        } else {
            pairs.clear();
        }
    }
    if pairs.is_empty() {
        let step = all.len().div_ceil(PAIRS_PER_LINK).max(1);
        pairs = all.iter().step_by(step).copied().collect();
    }
    Some(Link {
        a,
        b,
        pairs,
        inliers: agree.len(),
        h: h_full,
    })
}

/// Where frame `a`'s full-size pixel is in frame `b`.
fn transfer(g: &Geometry, a: usize, b: usize, p: [f64; 2]) -> Option<[f64; 2]> {
    match g {
        Geometry::Turning { cams, .. } => cams.pixel(b, cams.ray(a, p[0], p[1])),
        Geometry::Flat { planes, inverse } => {
            let (x, y) = align::apply(&planes.g[a], p[0], p[1]);
            let (u, v) = align::apply(&inverse[b], x, y);
            Some([u, v])
        }
    }
}

/// Each frame's gain (the reference's 1): the median brightness ratio of
/// every overlap, solved together (least squares on their logarithms),
/// held lightly to what EXIF says.
fn gains(g: &Geometry, links: &[Link], kept: &[Kept], prior: &[f64], reference: usize) -> Vec<f64> {
    let n = kept.len();
    let measured: Vec<Option<(f64, f64)>> = links
        .par_iter()
        .map(|l| {
            let (pa, pb) = (&kept[l.a].preview, &kept[l.b].preview);
            let (w, h) = (pa.gray.width, pa.gray.height);
            let stride = ((w * h) as f64 / 40_000.0).sqrt().ceil().max(1.0) as usize;
            let mut logs = Vec::new();
            for y in (0..h).step_by(stride) {
                for x in (0..w).step_by(stride) {
                    let va = pa.gray.at(x, y);
                    if !(va > 0.004 && va < 0.85) || pa.clipped[y * w + x] != 0 {
                        continue;
                    }
                    let (fx, fy) = pa.to_full(x as f64, y as f64);
                    let Some(q) = transfer(g, l.a, l.b, [fx, fy]) else {
                        continue;
                    };
                    let (bx, by) = pb.from_full(q[0], q[1]);
                    let Some(vb) = pb.gray.sample(bx, by) else {
                        continue;
                    };
                    let (cx, cy) = (bx.round() as usize, by.round() as usize);
                    if !(vb > 0.004 && vb < 0.85)
                        || pb.clipped
                            [cy.min(pb.gray.height - 1) * pb.gray.width + cx.min(pb.gray.width - 1)]
                            != 0
                    {
                        continue;
                    }
                    logs.push((f64::from(va) / f64::from(vb)).ln());
                }
            }
            if logs.len() < 50 {
                return None;
            }
            let mid = logs.len() / 2;
            logs.select_nth_unstable_by(mid, f64::total_cmp);
            Some((logs[mid], logs.len() as f64))
        })
        .collect();
    // Unknowns: every frame's log gain but the reference's (0).
    let slot = |i: usize| (i != reference).then(|| if i < reference { i } else { i - 1 });
    let m = n - 1;
    if m == 0 {
        return vec![1.0];
    }
    let total: f64 = measured.iter().flatten().map(|(_, w)| w).sum();
    let mu = 1e-4 * total.max(1.0) / n as f64;
    let mut a = vec![vec![0.0; m]; m];
    let mut b = vec![0.0; m];
    for (l, meas) in links.iter().zip(&measured) {
        let Some((d, w)) = meas else { continue };
        // x_a − x_b = −d (g_a·v_a = g_b·v_b).
        let (sa, sb) = (slot(l.a), slot(l.b));
        if let Some(i) = sa {
            a[i][i] += w;
            b[i] -= w * d;
        }
        if let Some(j) = sb {
            a[j][j] += w;
            b[j] += w * d;
        }
        if let (Some(i), Some(j)) = (sa, sb) {
            a[i][j] -= w;
            a[j][i] -= w;
        }
    }
    for i in 0..n {
        if let Some(s) = slot(i) {
            a[s][s] += mu;
            b[s] += mu * prior[i];
        }
    }
    let x = align::solve(a, b).unwrap_or_else(|| vec![0.0; m]);
    (0..n)
        .map(|i| slot(i).map_or(1.0, |s| x[s].exp()))
        .collect()
}

/// Catmull–Rom sample of channel `c` at `(x, y)`, held within its four
/// nearest values.
#[inline]
fn sample(img: &Rgb, x: f64, y: f64, c: usize) -> f32 {
    let (w, h) = (img.width as isize, img.height as isize);
    let (x0, y0) = (x.floor() as isize, y.floor() as isize);
    let cubic = |t: f32| {
        let (t2, t3) = (t * t, t * t * t);
        [
            0.5 * (-t3 + 2.0 * t2 - t),
            0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
            0.5 * (-3.0 * t3 + 4.0 * t2 + t),
            0.5 * (t3 - t2),
        ]
    };
    let wx = cubic((x - x0 as f64) as f32);
    let wy = cubic((y - y0 as f64) as f32);
    let at = |xx: isize, yy: isize| {
        img.data[(yy.clamp(0, h - 1) * w + xx.clamp(0, w - 1)) as usize * 3 + c]
    };
    let mut s = 0.0f32;
    for j in 0..4 {
        let mut r = 0.0f32;
        for i in 0..4 {
            r += wx[i] * at(x0 + i as isize - 1, y0 + j as isize - 1);
        }
        s += wy[j] * r;
    }
    let n = [
        at(x0, y0),
        at(x0 + 1, y0),
        at(x0, y0 + 1),
        at(x0 + 1, y0 + 1),
    ];
    let lo = n.iter().copied().fold(f32::MAX, f32::min);
    let hi = n.iter().copied().fold(f32::MIN, f32::max);
    s.clamp(lo, hi)
}

/// Frame `i` warped into `roi` of the result (projection, lens and gain in
/// one resample), with what it covers and its seam mask.
#[allow(clippy::too_many_arguments)]
fn warp(
    layout: &Layout,
    i: usize,
    src: &Rgb,
    optics: &optics::Optics,
    gain: f32,
    roi: [usize; 4],
    labels: &[u16],
    canvas: (usize, usize, f64, usize, usize),
) -> (Rgb, Vec<bool>, Vec<f32>) {
    let [x0, y0, x1, y1] = roi;
    let (w, h) = (x1 - x0, y1 - y0);
    let (cw, ch, k, ox, oy) = canvas;
    let mut out = Rgb::new(w, h);
    let mut cover = vec![false; w * h];
    let mut mask = vec![0f32; w * h];
    let (fw, fh) = (optics.width as f64 - 1.0, optics.height as f64 - 1.0);
    let me = i as u16;
    out.data
        .par_chunks_mut(w * 3)
        .zip(cover.par_chunks_mut(w))
        .zip(mask.par_chunks_mut(w))
        .enumerate()
        .for_each(|(r, ((row, crow), mrow))| {
            let v = (y0 + r) as f64;
            for c in 0..w {
                let u = (x0 + c) as f64;
                let Some([fx, fy]) = layout.to_frame(i, u, v) else {
                    continue;
                };
                if !(fx >= 0.0 && fy >= 0.0 && fx <= fw && fy <= fh) {
                    continue;
                }
                let at = optics.at(fx, fy);
                if !(at.g[0] >= 0.0 && at.g[1] >= 0.0 && at.g[0] <= fw && at.g[1] <= fh) {
                    continue;
                }
                let gk = gain * at.gain;
                for (ch, p) in [at.r, at.g, at.b].iter().enumerate() {
                    row[c * 3 + ch] = sample(src, p[0], p[1], ch) * gk;
                }
                crow[c] = true;
                // The seam: whose this is on the canvas (bilinear), or no
                // one's (the edge).
                let (ca, cb) = ((u + ox as f64) / k, (v + oy as f64) / k);
                let (a0, b0) = (ca.floor().max(0.0) as usize, cb.floor().max(0.0) as usize);
                let (ta, tb) = (
                    (ca - a0 as f64).clamp(0.0, 1.0),
                    (cb - b0 as f64).clamp(0.0, 1.0),
                );
                let (mut mine, mut none) = (0.0, 0.0);
                for (dx, dy, wt) in [
                    (0, 0, (1.0 - ta) * (1.0 - tb)),
                    (1, 0, ta * (1.0 - tb)),
                    (0, 1, (1.0 - ta) * tb),
                    (1, 1, ta * tb),
                ] {
                    let l = labels[(b0 + dy).min(ch - 1) * cw + (a0 + dx).min(cw - 1)];
                    if l == me {
                        mine += wt;
                    } else if l == seam::NONE {
                        none += wt;
                    }
                }
                mrow[c] = f32::from(u8::from(mine >= 0.5 || none >= 0.5));
            }
        });
    (out, cover, mask)
}

/// Stitch overlapping raw photos into a panorama (see the module's notes).
/// `progress` hears 0–1; `cancel` stops it between steps.
pub fn merge(
    inputs: &[Input],
    o: &Options,
    progress: &(dyn Fn(f32) + Sync),
    cancel: &AtomicBool,
) -> Result<Panorama> {
    let n = inputs.len();
    if !(MIN_FRAMES..=MAX_FRAMES).contains(&n) {
        return Err(refused(format!(
            "a panorama takes {MIN_FRAMES} to {MAX_FRAMES} photos"
        )));
    }
    let probed: Vec<_> = inputs.iter().map(probe).collect::<Result<_>>()?;
    let stops: Vec<Option<f64>> = probed.iter().map(|(e, _)| e.stops()).collect();
    progress(0.0);
    // 1: read each frame, keep its previews and features.
    let mut kept: Vec<Kept> = Vec::with_capacity(n);
    for (k, input) in inputs.iter().enumerate() {
        check(cancel)?;
        let f = keep(input, o, cancel)?;
        if let Some(first) = kept.first() {
            let same = |a: &Kept| {
                let (w, h) = (a.width.min(a.height), a.width.max(a.height));
                (w, h)
            };
            if same(&f) != same(first) {
                return Err(refused(format!(
                    "{} is {}×{}, the first {}×{}: a panorama's photos are one camera's, one size",
                    input.name(),
                    f.width,
                    f.height,
                    first.width,
                    first.height
                )));
            }
            if (f.meta.make.as_str(), f.meta.model.as_str())
                != (first.meta.make.as_str(), first.meta.model.as_str())
            {
                return Err(refused(format!("{} is from another camera", input.name())));
            }
        }
        kept.push(f);
        progress(0.4 * (k + 1) as f32 / n as f32);
    }
    let sizes: Vec<(usize, usize)> = kept.iter().map(|k| (k.width, k.height)).collect();
    let centres: Vec<[f64; 2]> = sizes
        .iter()
        .map(|&(w, h)| [(w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0])
        .collect();
    let mut notes = Vec::new();
    let unknown: Vec<String> = inputs
        .iter()
        .zip(&kept)
        .filter(|(_, k)| o.lens && !k.optics.corrects())
        .map(|(i, _)| i.name())
        .collect();
    if !unknown.is_empty() && unknown.len() == n {
        notes.push("no lens profile found: frames not corrected for their lens".to_owned());
    }

    // 2: match every pair.
    let all: Vec<(usize, usize)> = (0..n)
        .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
        .collect();
    let mut links: Vec<Link> = Vec::new();
    for chunk in all.chunks(8) {
        check(cancel)?;
        let found: Vec<Option<Link>> = chunk
            .par_iter()
            .map(|&(a, b)| link(a, b, &kept[a], &kept[b]))
            .collect();
        links.extend(found.into_iter().flatten());
    }
    progress(0.43);
    let (root, order) = bundle::tree(&links, n);
    if order.len() + 1 < n {
        let mut reached = vec![false; n];
        reached[root] = true;
        for &(j, _) in &order {
            reached[j] = true;
        }
        let lost: Vec<String> = (0..n)
            .filter(|&i| !reached[i])
            .map(|i| inputs[i].name())
            .collect();
        return Err(refused(if links.is_empty() {
            "the photos don't overlap: no two have enough in common to join".to_owned()
        } else {
            format!(
                "{} {} overlap the other photos (too little in common to join)",
                lost.join(", "),
                if lost.len() == 1 { "doesn't" } else { "don't" }
            )
        }));
    }

    // 3: registration.
    let shot = &kept[root].shot;
    let focal_exif =
        (shot.focal > 0.0).then(|| crate::volume::focal_px(shot, sizes[root].0, sizes[root].1).0);
    let focal0 = focal_exif.unwrap_or_else(|| {
        let mut fs: Vec<f64> = links
            .iter()
            .filter_map(|l| {
                let (ca, cb) = (centres[l.a], centres[l.b]);
                let ta = [[1.0, 0.0, ca[0]], [0.0, 1.0, ca[1]], [0.0, 0.0, 1.0]];
                let tb = [[1.0, 0.0, -cb[0]], [0.0, 1.0, -cb[1]], [0.0, 0.0, 1.0]];
                let hc = geom::mul(&tb, &geom::mul(&l.h, &ta));
                bundle::focal_from_homography(&crate::colour::inv(&hc)?)
            })
            .collect();
        if fs.is_empty() {
            crate::volume::focal_px(shot, sizes[root].0, sizes[root].1).0
        } else {
            let mid = fs.len() / 2;
            fs.select_nth_unstable_by(mid, f64::total_cmp);
            fs[mid]
        }
    });
    let mut cams = bundle::initial_rotations(&links, n, root, &order, focal0, &centres);
    let mut fit = bundle::adjust(&mut cams, n, root, &links, cancel)?;
    // Matches the cameras can't explain (a repeating pattern, something
    // that moved): out, and once more.
    let cleaned = |links: &[Link], p: &dyn Fn(&Link, &Pair) -> f64, median: f64| -> Vec<Link> {
        let cut = (4.0 * 1.4826 * median).max(2.0);
        links
            .iter()
            .map(|l| Link {
                pairs: l.pairs.iter().filter(|m| p(l, m) <= cut).copied().collect(),
                ..l.clone()
            })
            .filter(|l| l.pairs.len() >= 8)
            .collect()
    };
    let rot_err = |c: &Rotations| {
        let c = c.clone();
        move |l: &Link, m: &Pair| {
            bundle::Problem::residual(&c, l, m, &[], &[], &[])
                .map_or(f64::INFINITY, |r| r[0].hypot(r[1]).max(r[2].hypot(r[3])))
        }
    };
    let rot_links = cleaned(&links, &rot_err(&cams), fit.median);
    if bundle::tree(&rot_links, n).1.len() + 1 == n {
        fit = bundle::adjust(&mut cams, n, root, &rot_links, cancel)?;
    }
    progress(0.46);
    let feature_step = kept.iter().map(|k| k.feature_step).max().unwrap_or(1) as f64;
    let mut flat = None;
    // A plane: tried when the turning camera leaves more than the
    // features' own error, taken when it fits much better.
    if fit.median > 0.75 * feature_step {
        let mut planes = bundle::initial_planes(&links, n, &order, &sizes);
        let pf = bundle::adjust(&mut planes, n, root, &links, cancel)?;
        tracing::info!(
            "panorama: turning camera fits to {:.2} px (median), a plane to {:.2}",
            fit.median,
            pf.median
        );
        if pf.median < 0.6 * fit.median {
            flat = Some((planes, pf));
        }
    }
    let worst_ok = 12.0 * feature_step + 4.0;
    let used_fit = flat.as_ref().map_or(fit, |f| f.1);
    if used_fit.median > worst_ok {
        return Err(refused(
            "the photos couldn't be put together: the camera moved too much between them, or they show different things",
        ));
    }
    let mut frames: Vec<Frame> = kept
        .iter()
        .map(|k| Frame {
            rotation: None,
            plane: None,
            gain: 1.0,
            neighbours: 0,
            lens: k.lens.clone(),
        })
        .collect();
    for l in &links {
        frames[l.a].neighbours += 1;
        frames[l.b].neighbours += 1;
    }

    // 4–5: the layout.
    let (geometry, projection, plane_box, scale) = match flat {
        Some((planes, _)) => {
            let (b, inverse) = project::flat_bounds(&planes, &sizes).map_err(refused)?;
            notes.push("laid flat: the photos fit a plane better than a turning camera".to_owned());
            (
                Geometry::Flat { planes, inverse },
                Projection::Perspective,
                b,
                1.0,
            )
        }
        None => {
            bundle::straighten(&mut cams.r);
            let extent = project::centre(&mut cams, &sizes);
            let projection = match o.projection {
                Projection::Auto => extent.auto(),
                p => p,
            };
            let b = project::bounds(&cams, &sizes, projection, &extent).map_err(refused)?;
            let s = cams.f;
            (Geometry::Turning { cams, projection }, projection, b, s)
        }
    };
    let mut scale = scale;
    let span = |s: f64| {
        (
            ((plane_box[2] - plane_box[0]) * s).ceil() as usize + 1,
            ((plane_box[3] - plane_box[1]) * s).ceil() as usize + 1,
        )
    };
    let (w0, h0) = span(scale);
    let shrink =
        (((w0 * h0) as f64 / MAX_PIXELS as f64).sqrt()).max(w0.max(h0) as f64 / MAX_SIDE as f64);
    if shrink > 1.0 {
        scale /= shrink;
        let (w, h) = span(scale);
        notes.push(format!(
            "drawn at {:.0}% ({w}×{h}): the full size would be {:.0} MP",
            100.0 / shrink,
            (w0 * h0) as f64 / 1e6
        ));
    }
    let (width, height) = span(scale);
    let full = Layout {
        geometry,
        scale,
        origin: [plane_box[0], plane_box[1]],
        width,
        height,
    };
    match &full.geometry {
        Geometry::Turning { cams, .. } => {
            for (f, r) in frames.iter_mut().zip(&cams.r) {
                f.rotation = Some(*r);
            }
        }
        Geometry::Flat { planes, .. } => {
            for (f, g) in frames.iter_mut().zip(&planes.g) {
                f.plane = Some(*g);
            }
        }
    }
    let prior: Vec<f64> = (0..n)
        .map(|i| match (stops[i], stops[root]) {
            (Some(a), Some(b)) => -(a - b) * std::f64::consts::LN_2,
            _ => 0.0,
        })
        .collect();
    let gain = gains(&full.geometry, &links, &kept, &prior, root);
    for (f, g) in frames.iter_mut().zip(&gain) {
        f.gain = *g;
    }
    check(cancel)?;
    progress(0.48);

    // 6: seams on a small canvas, and the crop.
    let k = ((width * height) as f64 / CANVAS_PIXELS).sqrt().max(1.0);
    let canvas = full.shrunk(k);
    let previews: Vec<&optics::Preview> = kept.iter().map(|k| &k.preview).collect();
    let labels = seam::find(&canvas, &sizes, &previews, &gain, cancel)?;
    let (cw, ch) = (canvas.width, canvas.height);
    let (mut x0, mut y0, mut x1, mut y1) = (0, 0, width, height);
    if o.crop {
        let covered = |x: isize, y: isize| {
            x >= 0
                && y >= 0
                && (x as usize) < cw
                && (y as usize) < ch
                && labels[y as usize * cw + x as usize] != seam::NONE
        };
        let ok: Vec<bool> = (0..cw * ch)
            .map(|i| {
                let (x, y) = ((i % cw) as isize, (i / cw) as isize);
                (-1..=1).all(|dy| (-1..=1).all(|dx| covered(x + dx, y + dy)))
            })
            .collect();
        let Some([a0, b0, a1, b1]) = seam::largest_rectangle(&ok, cw, ch) else {
            return Err(refused("the photos cover too little to crop"));
        };
        x0 = ((a0 as f64 * k).ceil() as usize).min(width - 1);
        y0 = ((b0 as f64 * k).ceil() as usize).min(height - 1);
        x1 = ((((a1 - 1) as f64 * k).floor() as usize) + 1).clamp(x0 + 1, width);
        y1 = ((((b1 - 1) as f64 * k).floor() as usize) + 1).clamp(y0 + 1, height);
    }
    let layout = full.cropped(x0, y0, x1, y1);
    let (ow, oh) = (layout.width, layout.height);
    let mut blender = blend::Blender::new(ow, oh, blend::Blender::bands(ow, oh));
    let step = blender.align();
    progress(0.5);

    // 7: each frame again, warped and blended.
    let mut done = 0usize;
    for (i, input) in inputs.iter().enumerate() {
        check(cancel)?;
        let Some(r) = layout.reach(i, sizes[i]) else {
            done += 1;
            continue;
        };
        let margin = 3 * step + 4;
        let rx0 = r[0].saturating_sub(margin) / step * step;
        let ry0 = r[1].saturating_sub(margin) / step * step;
        let rx1 = (r[2] + margin).min(ow);
        let ry1 = (r[3] + margin).min(oh);
        let src = load(input, cancel)?;
        check(cancel)?;
        let (mut img, cover, mask) = warp(
            &layout,
            i,
            &src.rgb,
            &kept[i].optics,
            gain[i] as f32,
            [rx0, ry0, rx1, ry1],
            &labels,
            (cw, ch, k, x0, y0),
        );
        drop(src);
        if !mask.iter().any(|m| *m > 0.0) {
            done += 1;
            continue;
        }
        blend::fill(&mut img, &cover);
        drop(cover);
        blender.add(img, mask, rx0, ry0);
        done += 1;
        progress(0.5 + 0.47 * done as f32 / n as f32);
    }
    check(cancel)?;
    let rgb = blender.finish();
    let r = &kept[root];
    // A panorama has no one lens or focal length: Develop mustn't correct
    // it again.
    let metadata = r.metadata.clone().map(|mut m| {
        let x = &mut m.exif;
        x.lens_model = None;
        x.lens_make = None;
        x.lens_spec = None;
        x.lens_serial_number = None;
        x.focal_length = None;
        m.lens = None;
        m
    });
    progress(1.0);
    Ok(Panorama {
        merged: Merged {
            rgb,
            reference: root,
            meta: r.meta.clone(),
            as_shot: r.as_shot,
            metadata,
        },
        frames,
        focal: match &layout.geometry {
            Geometry::Turning { cams, .. } => Some(cams.f),
            Geometry::Flat { .. } => None,
        },
        focal_exif,
        projection,
        flat: matches!(layout.geometry, Geometry::Flat { .. }),
        fit: used_fit,
        uncropped: (width, height),
        layout,
        notes,
    })
}
