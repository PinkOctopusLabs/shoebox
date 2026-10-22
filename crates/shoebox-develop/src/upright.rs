//! Auto-upright (EDITING-SPEC, Phase 3): straight lines found in the photo
//! decide the rotation and perspective that make near-vertical lines
//! vertical and near-horizontal ones level, as Lightroom's Upright does.
//!
//! - [`lines`]: line segments in a small upright copy of the photo (lens
//!   corrections applied, so straight things are straight): gradient
//!   pixels grown into regions of one direction and fitted with a line,
//!   after LSD (Grompone von Gioi et al., 2010), much simplified.
//! - [`solve`]: the rotation (degrees) and vertical and horizontal
//!   perspective (the Transform sliders) that minimise how far the lines
//!   lean once corrected, each line's error capped so stray lines don't
//!   dominate. A coarse grid, then a pattern search.
//!
//! - [`solve_guided`]: Guided upright. Two to four lines the person drew
//!   are each made exactly vertical or level (whichever they're nearer),
//!   by a least-squares fit of the same three parameters.
//!
//! Modes: Level (rotation only), Vertical (rotation and vertical
//! perspective), Full (both perspectives), Auto (Full, held back from
//! strong corrections).

use crate::colour::{self, M3};
use crate::cpu::{self, Demosaiced, Options};
use crate::edit::{Geometry, Settings, Transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Level,
    Vertical,
    Full,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Auto, Mode::Level, Mode::Vertical, Mode::Full];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Auto => "Auto",
            Mode::Level => "Level",
            Mode::Vertical => "Vertical",
            Mode::Full => "Full",
        }
    }
}

/// A line segment's ends, in pixels from the image's centre (y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub a: [f64; 2],
    pub b: [f64; 2],
}

impl Segment {
    fn length(&self) -> f64 {
        (self.b[0] - self.a[0]).hypot(self.b[1] - self.a[1])
    }
}

/// The segments found in a `width`×`height` image.
#[derive(Debug, Clone, Default)]
pub struct Lines {
    pub segments: Vec<Segment>,
    pub width: f64,
    pub height: f64,
}

/// What upright does: the rotation (degrees, clockwise) and the
/// Transform's vertical and horizontal perspective.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Correction {
    pub rotate: f64,
    pub vertical: f64,
    pub horizontal: f64,
}

/// The analysis copy's long edge.
const LONG: usize = 1024;

/// The lines of the photo as `s` frames it before any rotation or
/// perspective: upright (orientation, quarter turns, mirror) and with its
/// lens corrections.
pub fn analyse(
    raw: &shoebox_decode::raw::RawImage,
    src: &Demosaiced,
    s: &Settings,
    orientation: u16,
) -> Lines {
    let (w, h) = (src.rgb.width, src.rgb.height);
    let long = w.max(h).max(1);
    let small = if long > LONG {
        let k = LONG as f64 / long as f64;
        let (sw, sh) = (
            ((w as f64 * k).round() as usize).max(1),
            ((h as f64 * k).round() as usize).max(1),
        );
        cpu::maps::resize(&src.rgb, sw, sh)
    } else {
        src.rgb.clone()
    };
    let scale = src.scale * small.width as f32 / w.max(1) as f32;
    let small = Demosaiced::new(small, src.as_shot, src.referred)
        .with_lens(src.lens.clone())
        .with_scale(scale);
    let mut plain = Settings {
        geometry: Geometry {
            quarter: s.geometry.quarter,
            flip: s.geometry.flip,
            ..Default::default()
        },
        ..Default::default()
    };
    plain.modules.lens = s.modules.lens.clone();
    plain.modules.raw = s.modules.raw.clone();
    let o = Options {
        orientation,
        ..Default::default()
    };
    let f = cpu::frame(raw, &small, &plain, &o);
    let img = cpu::apply(&small.rgb, &f, false);
    let y = cpu::maps::Local::none().luma;
    let lum: Vec<f32> = img
        .data
        .as_chunks::<3>()
        .0
        .iter()
        // A little light added, so dark noise doesn't make edges.
        .map(|p| ((y[0] * p[0] + y[1] * p[1] + y[2] * p[2]).max(0.0) + 1e-3).log2())
        .collect();
    lines(&lum, img.width, img.height)
}

/// Fold an angle (degrees) into −90..90.
fn fold(a: f64) -> f64 {
    let a = (a + 90.0).rem_euclid(180.0) - 90.0;
    if a <= -90.0 { a + 180.0 } else { a }
}

/// Line segments in a `w`×`h` image of log luminance.
pub fn lines(lum: &[f32], w: usize, h: usize) -> Lines {
    let mut out = Lines {
        segments: Vec::new(),
        width: w as f64,
        height: h as f64,
    };
    if w < 8 || h < 8 {
        return out;
    }
    // A light blur, then Sobel gradients.
    let at = |v: &[f32], x: usize, y: usize| v[y * w + x];
    let mut blur = vec![0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let mut s = 0.0;
            for dy in 0..3 {
                for dx in 0..3 {
                    s += at(lum, x + dx - 1, y + dy - 1);
                }
            }
            blur[y * w + x] = s / 9.0;
        }
    }
    let mut mag = vec![0f32; w * h];
    let mut dir = vec![0f32; w * h];
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            let b =
                |dx: i64, dy: i64| at(&blur, (x as i64 + dx) as usize, (y as i64 + dy) as usize);
            let gx = (b(1, -1) + 2.0 * b(1, 0) + b(1, 1)) - (b(-1, -1) + 2.0 * b(-1, 0) + b(-1, 1));
            let gy = (b(-1, 1) + 2.0 * b(0, 1) + b(1, 1)) - (b(-1, -1) + 2.0 * b(0, -1) + b(1, -1));
            mag[y * w + x] = gx.hypot(gy);
            // The line runs across the gradient.
            dir[y * w + x] = (gy.atan2(gx).to_degrees() + 90.0).rem_euclid(180.0);
        }
    }
    // Strong edges: the top fifth of the gradients, and at least a
    // contrast worth having.
    let mut sorted: Vec<f32> = mag.iter().copied().filter(|&m| m > 0.0).collect();
    if sorted.is_empty() {
        return out;
    }
    let k = sorted.len() * 4 / 5;
    sorted.select_nth_unstable_by(k, f32::total_cmp);
    let threshold = sorted[k].max(0.6);
    let mut order: Vec<usize> = (0..w * h).filter(|&i| mag[i] >= threshold).collect();
    order.sort_unstable_by(|&a, &b| mag[b].total_cmp(&mag[a]));
    let long = w.max(h) as f64;
    let min_len = 0.04 * long;
    let mut used = vec![false; w * h];
    let close = |a: f32, b: f32| {
        let d = (a - b).rem_euclid(180.0);
        d.min(180.0 - d) <= 22.5
    };
    for &seed in &order {
        if used[seed] {
            continue;
        }
        // Grow a region of pixels of about the seed's direction.
        let mut region = vec![seed];
        used[seed] = true;
        let (mut sx, mut sy) = (
            (2.0 * f64::from(dir[seed])).to_radians().cos(),
            (2.0 * f64::from(dir[seed])).to_radians().sin(),
        );
        let mut k = 0;
        while k < region.len() {
            let i = region[k];
            k += 1;
            let mean = (sy.atan2(sx).to_degrees() / 2.0) as f32;
            let (x, y) = ((i % w) as i64, (i / w) as i64);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if used[j] || mag[j] < threshold || !close(dir[j], mean) {
                        continue;
                    }
                    used[j] = true;
                    region.push(j);
                    let a = (2.0 * f64::from(dir[j])).to_radians();
                    sx += a.cos();
                    sy += a.sin();
                }
            }
        }
        if (region.len() as f64) < min_len {
            continue;
        }
        if let Some(s) = fit(&region, w, min_len) {
            out.segments.push(Segment {
                a: [s.a[0] - w as f64 / 2.0, s.a[1] - h as f64 / 2.0],
                b: [s.b[0] - w as f64 / 2.0, s.b[1] - h as f64 / 2.0],
            });
        }
    }
    out
}

/// The line through a region of pixels (its principal axis), if it's long
/// and thin enough to be one.
fn fit(region: &[usize], w: usize, min_len: f64) -> Option<Segment> {
    let n = region.len() as f64;
    let pts: Vec<[f64; 2]> = region
        .iter()
        .map(|&i| [(i % w) as f64 + 0.5, (i / w) as f64 + 0.5])
        .collect();
    let (mx, my) = (
        pts.iter().map(|p| p[0]).sum::<f64>() / n,
        pts.iter().map(|p| p[1]).sum::<f64>() / n,
    );
    let (mut xx, mut xy, mut yy) = (0.0, 0.0, 0.0);
    for p in &pts {
        let (dx, dy) = (p[0] - mx, p[1] - my);
        xx += dx * dx;
        xy += dx * dy;
        yy += dy * dy;
    }
    let angle = 0.5 * (2.0 * xy).atan2(xx - yy);
    let (c, s) = (angle.cos(), angle.sin());
    let (mut lo, mut hi, mut wide) = (f64::MAX, f64::MIN, 0f64);
    for p in &pts {
        let (dx, dy) = (p[0] - mx, p[1] - my);
        let along = dx * c + dy * s;
        lo = lo.min(along);
        hi = hi.max(along);
        wide = wide.max((-dx * s + dy * c).abs());
    }
    let len = hi - lo;
    if len < min_len || wide > (0.08 * len).max(3.0) {
        return None;
    }
    Some(Segment {
        a: [mx + lo * c, my + lo * s],
        b: [mx + hi * c, my + hi * s],
    })
}

/// Upright coordinates (centred) → corrected output coordinates (centred):
/// the Transform's perspective, then the rotation, as the geometry map
/// undoes them.
fn forward(c: &Correction, w: f64, h: f64) -> M3 {
    let t = Transform {
        vertical: c.vertical,
        horizontal: c.horizontal,
        constrain: false,
        ..Default::default()
    };
    let untransform = cpu::transform_inverse(&t, w, h);
    let transform = colour::inv(&untransform).unwrap_or(colour::IDENTITY);
    let (sin, cos) = c.rotate.to_radians().sin_cos();
    let rotate = [[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]];
    colour::mul(&rotate, &transform)
}

fn apply(m: &M3, p: [f64; 2]) -> [f64; 2] {
    let q = colour::mulv(m, [p[0], p[1], 1.0]);
    [q[0] / q[2], q[1] / q[2]]
}

#[derive(Clone, Copy, PartialEq)]
enum Class {
    Vertical,
    Horizontal,
}

/// How far each segment leans from vertical or level once corrected
/// (degrees), by class.
fn leans(lines: &Lines, c: &Correction) -> Vec<(Class, f64, f64)> {
    let m = forward(c, lines.width, lines.height);
    lines
        .segments
        .iter()
        .filter_map(|s| {
            let class = classify(s)?;
            Some((class, lean(&m, class, s), s.length()))
        })
        .collect()
}

/// Lines within 30° of vertical or level, before any correction.
fn classify(s: &Segment) -> Option<Class> {
    let (dx, dy) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
    if fold(dx.atan2(dy).to_degrees()).abs() <= 30.0 {
        Some(Class::Vertical)
    } else if fold(dy.atan2(dx).to_degrees()).abs() <= 30.0 {
        Some(Class::Horizontal)
    } else {
        None
    }
}

/// The largest lean any one line counts for (degrees).
const CAP: f64 = 6.0;

fn cost(lines: &Lines, c: &Correction, mode: Mode) -> f64 {
    let level_weight = if mode == Mode::Vertical { 0.25 } else { 1.0 };
    let mut total = 0.0;
    let mut weight = 0.0;
    for (class, lean, len) in leans(lines, c) {
        let w = match class {
            Class::Vertical => len,
            Class::Horizontal => len * level_weight,
        };
        total += w * lean.abs().min(CAP).powi(2);
        weight += w;
    }
    let mut cost = total / weight.max(1e-9);
    if mode == Mode::Auto {
        // Strong corrections cost something: Auto prefers to leave a
        // photo nearly as it was when that's almost as straight.
        cost += 1e-3 * (c.vertical.powi(2) + c.horizontal.powi(2)) + 0.02 * c.rotate.powi(2);
    }
    cost
}

/// The correction for these lines in this mode (none when there are too
/// few lines to tell).
pub fn solve(lines: &Lines, mode: Mode) -> Correction {
    let counted = lines
        .segments
        .iter()
        .filter(|s| classify(s).is_some())
        .count();
    if counted < 2 {
        return Correction::default();
    }
    let (vary_v, vary_h) = match mode {
        Mode::Level => (false, false),
        Mode::Vertical => (true, false),
        Mode::Full | Mode::Auto => (true, true),
    };
    let limit = if mode == Mode::Auto { 60.0 } else { 100.0 };
    let range = |on: bool, step: f64, max: f64| -> Vec<f64> {
        if !on {
            return vec![0.0];
        }
        let n = (max / step).round() as i64;
        (-n..=n).map(|i| i as f64 * step).collect()
    };
    let mut best = (f64::MAX, Correction::default());
    for &r in &range(true, 1.0, 10.0) {
        for &v in &range(vary_v, 10.0, limit) {
            for &h in &range(vary_h, 10.0, limit) {
                let c = Correction {
                    rotate: r,
                    vertical: v,
                    horizontal: h,
                };
                let k = cost(lines, &c, mode);
                if k < best.0 {
                    best = (k, c);
                }
            }
        }
    }
    // Pattern search from there, halving the steps.
    let (mut sr, mut sv) = (0.5, 5.0);
    let (mut k, mut c) = best;
    for _ in 0..8 {
        loop {
            let mut moved = false;
            let mut tries = vec![];
            for d in [-1.0, 1.0] {
                tries.push(Correction {
                    rotate: c.rotate + d * sr,
                    ..c
                });
                if vary_v {
                    tries.push(Correction {
                        vertical: (c.vertical + d * sv).clamp(-limit, limit),
                        ..c
                    });
                }
                if vary_h {
                    tries.push(Correction {
                        horizontal: (c.horizontal + d * sv).clamp(-limit, limit),
                        ..c
                    });
                }
            }
            for t in tries {
                let kt = cost(lines, &t, mode);
                if kt < k - 1e-12 {
                    (k, c) = (kt, t);
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        sr /= 2.0;
        sv /= 2.0;
    }
    Correction {
        rotate: (c.rotate.clamp(-45.0, 45.0) * 10.0).round() / 10.0,
        vertical: c.vertical.round(),
        horizontal: c.horizontal.round(),
    }
}

/// Guided upright takes at most this many lines.
pub const GUIDES: usize = 4;

/// A guide shorter than this (a fraction of the long edge) doesn't count:
/// one being drawn, before the pointer has moved far.
const GUIDE_MIN: f64 = 0.01;

/// The guides that count, each vertical or level by whichever it's nearer.
fn guides(lines: &Lines) -> Vec<(Class, Segment)> {
    let min = GUIDE_MIN * lines.width.max(lines.height);
    lines
        .segments
        .iter()
        .filter(|s| s.length() >= min)
        .take(GUIDES)
        .map(|s| {
            let (dx, dy) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
            let class = if fold(dx.atan2(dy).to_degrees()).abs() <= 45.0 {
                Class::Vertical
            } else {
                Class::Horizontal
            };
            (class, *s)
        })
        .collect()
}

/// How far a segment leans from vertical or level through `m` (degrees).
fn lean(m: &M3, class: Class, s: &Segment) -> f64 {
    let (a, b) = (apply(m, s.a), apply(m, s.b));
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    match class {
        Class::Vertical => fold(dx.atan2(dy).to_degrees()),
        Class::Horizontal => fold(dy.atan2(dx).to_degrees()),
    }
}

/// How far each guide leans from vertical or level once corrected
/// (degrees; tests, and judging a result).
pub fn guide_leans(lines: &Lines, c: &Correction) -> Vec<f64> {
    let m = forward(c, lines.width, lines.height);
    guides(lines)
        .iter()
        .map(|(class, s)| lean(&m, *class, s))
        .collect()
}

/// Degrees of camera tilt or pan per perspective slider unit (±100 is
/// ±30°, as `cpu::transform_inverse` has it).
const PER_UNIT: f64 = 0.3;

/// How much the correction itself costs, per degree: enough to choose the
/// smallest of the corrections that straighten the guides when they leave
/// a choice (one vertical and one level), too little to leave a lean.
const SMALL: f64 = 1e-3;

/// Guided upright: the rotation and perspective that make each guide
/// exactly vertical or level, whichever it's nearer. None with fewer than
/// two guides (the spec's two to four).
///
/// Verticals alone turn and tilt; levels alone turn and pan; both together
/// may use all three. Four lines over-determine three parameters, so then
/// it's the least-squares fit. Levenberg–Marquardt on the leans (degrees),
/// the parameters in degrees too.
pub fn solve_guided(lines: &Lines) -> Option<Correction> {
    let g = guides(lines);
    if g.len() < 2 {
        return None;
    }
    let verticals = g.iter().filter(|(c, _)| *c == Class::Vertical).count();
    let levels = g.len() - verticals;
    let both = verticals > 0 && levels > 0;
    let free = [true, verticals >= 2 || both, levels >= 2 || both];
    let limit = [45.0, 100.0 * PER_UNIT, 100.0 * PER_UNIT];
    let correction = |x: [f64; 3]| Correction {
        rotate: x[0],
        vertical: x[1] / PER_UNIT,
        horizontal: x[2] / PER_UNIT,
    };
    let residuals = |x: [f64; 3]| -> Vec<f64> {
        let m = forward(&correction(x), lines.width, lines.height);
        g.iter()
            .map(|(class, s)| lean(&m, *class, s))
            .chain(x.iter().map(|v| SMALL * v))
            .collect()
    };
    let sum = |r: &[f64]| r.iter().map(|v| v * v).sum::<f64>();
    let mut x = [0.0; 3];
    let mut r = residuals(x);
    let mut k = sum(&r);
    let mut damping = 1e-3;
    for _ in 0..100 {
        // The Jacobian, by central differences.
        let d = 1e-5;
        let mut jac = vec![[0.0; 3]; r.len()];
        for j in (0..3).filter(|&j| free[j]) {
            let (mut up, mut down) = (x, x);
            up[j] += d;
            down[j] -= d;
            let (ru, rd) = (residuals(up), residuals(down));
            for (i, row) in jac.iter_mut().enumerate() {
                row[j] = (ru[i] - rd[i]) / (2.0 * d);
            }
        }
        let mut a = [[0.0; 3]; 3];
        let mut b = [0.0; 3];
        for (row, ri) in jac.iter().zip(&r) {
            for p in 0..3 {
                b[p] -= row[p] * ri;
                for q in 0..3 {
                    a[p][q] += row[p] * row[q];
                }
            }
        }
        // Damped steps until one helps.
        let mut stepped = None;
        while damping < 1e9 {
            let mut ad = a;
            let mut bd = b;
            for p in 0..3 {
                if free[p] {
                    ad[p][p] += damping * a[p][p].max(1e-9);
                } else {
                    ad[p] = [0.0; 3];
                    ad[p][p] = 1.0;
                    bd[p] = 0.0;
                }
            }
            let Some(inv) = colour::inv(&ad) else {
                damping *= 4.0;
                continue;
            };
            let step = colour::mulv(&inv, bd);
            let mut next = x;
            for p in 0..3 {
                next[p] = (x[p] + step[p]).clamp(-limit[p], limit[p]);
            }
            let rn = residuals(next);
            let kn = sum(&rn);
            if kn < k {
                damping = (damping / 3.0).max(1e-9);
                stepped = Some((next, rn, kn, step));
                break;
            }
            damping *= 4.0;
        }
        let Some((next, rn, kn, step)) = stepped else {
            break;
        };
        (x, r, k) = (next, rn, kn);
        if step.iter().all(|s| s.abs() < 1e-7) {
            break;
        }
    }
    let c = correction(x);
    // Finer than Auto's rounding: these lines are meant to come out
    // exactly straight. (+ 0.0 makes −0 plain 0.)
    let round = |v: f64, k: f64| (v * k).round() / k + 0.0;
    Some(Correction {
        rotate: round(c.rotate, 100.0),
        vertical: round(c.vertical, 10.0),
        horizontal: round(c.horizontal, 10.0),
    })
}

/// The largest lean (degrees) of the lines `solve` straightens, once
/// corrected (tests, and judging a result).
pub fn worst_lean(lines: &Lines, c: &Correction, mode: Mode) -> f64 {
    leans(lines, c)
        .into_iter()
        .filter(|(class, ..)| mode != Mode::Vertical || *class == Class::Vertical)
        .map(|(_, lean, _)| lean.abs())
        .fold(0.0, f64::max)
}
