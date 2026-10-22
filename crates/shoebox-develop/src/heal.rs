//! Spot removal (EDITING-SPEC, Healing): each spot fills a circle from the
//! same circle elsewhere, on the source image after the detail stage (so
//! after noise reduction, before tone, in linear light), in order, so a
//! later spot can copy from one healed before it.
//!
//! - **Clone** copies the source circle as it is.
//! - **Heal** copies its texture and adds the smooth difference that makes
//!   its edge meet the surroundings: the harmonic (Laplace) interpolation
//!   of the edge's difference over the circle, which is the Poisson
//!   blend's answer (Pérez, Gangnet and Blake, 2003), solved by successive
//!   over-relaxation.
//!
//! A soft edge (feather) and opacity blend the result in. Positions are in
//! the stored image, in units of its long edge, as masks are.
//!
//! Removals (object removal, [`remove`]) are spots too: what their brush
//! strokes paint is filled by an inpainting model.

// Channel loops read clearer indexed.
#![allow(clippy::needless_range_loop)]

pub mod remove;

use std::sync::Arc;

use crate::cpu::Rgb;
use crate::edit::{Spot, SpotKind};

/// At most this many spots are applied (an edit file is JSON anyone can
/// write; this keeps one from taking forever).
pub const MAX_SPOTS: usize = 2000;
/// The largest radius, in long edges.
pub const MAX_RADIUS: f64 = 0.25;
/// The most relaxation sweeps one heal takes.
const MAX_SWEEPS: usize = 4000;

/// The source after these spots (removals filled here and now; see
/// [`heal_with`]).
pub fn heal(img: &Rgb, spots: &[Spot]) -> Rgb {
    heal_with(img, spots, &mut |_, job| match remove::run(job) {
        Ok(f) => Some(Arc::new(f)),
        Err(e) => {
            tracing::warn!("object removal: {e}");
            None
        }
    })
}

/// The source after these spots, each removal's fill (spot `i`, asked of
/// the model as `job`) from `fill`: None leaves it out (still on its way,
/// or it couldn't be made).
pub fn heal_with(
    img: &Rgb,
    spots: &[Spot],
    fill: &mut dyn FnMut(usize, &remove::Job) -> Option<Arc<remove::Fill>>,
) -> Rgb {
    let mut out = img.clone();
    for (i, s) in spots.iter().enumerate().take(MAX_SPOTS) {
        let finite = s.dest.iter().chain(&s.source).all(|v| v.is_finite())
            && s.radius.is_finite()
            && s.feather.is_finite()
            && s.opacity.is_finite()
            && s.darken.is_finite();
        if !finite {
            continue;
        }
        let s = Spot {
            radius: s.radius.clamp(0.0, MAX_RADIUS),
            feather: s.feather.clamp(0.0, 1.0),
            opacity: s.opacity.clamp(0.0, 100.0),
            darken: s.darken.clamp(0.0, 100.0),
            ..s.clone()
        };
        if s.kind == SpotKind::Remove {
            if let Some(job) = remove::job(&out, &s)
                && let Some(f) = fill(i, &job)
            {
                remove::apply(&mut out, &s, &job, &f);
            }
            continue;
        }
        spot(&mut out, &s);
    }
    out
}

/// Bilinear, clamped to the edges.
fn sample(img: &Rgb, x: f64, y: f64) -> [f32; 3] {
    let x = x.clamp(0.0, (img.width - 1) as f64);
    let y = y.clamp(0.0, (img.height - 1) as f64);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(img.width - 1), (y0 + 1).min(img.height - 1));
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let (a, b, c, d) = (
        img.px(x0, y0),
        img.px(x1, y0),
        img.px(x0, y1),
        img.px(x1, y1),
    );
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        top + (bot - top) * fy
    })
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn spot(img: &mut Rgb, s: &Spot) {
    if img.width == 0 || img.height == 0 {
        return;
    }
    if s.kind.is_eye() {
        eye(img, s);
        return;
    }
    let long = img.width.max(img.height) as f64;
    let (cx, cy) = (s.dest[0] * long - 0.5, s.dest[1] * long - 0.5);
    let (ox, oy) = (
        (s.source[0] - s.dest[0]) * long,
        (s.source[1] - s.dest[1]) * long,
    );
    let r = (s.radius * long).max(1.0);
    // The circle and a margin of two pixels (the heal's fixed edge).
    let x0 = (cx - r - 2.0).floor().max(0.0) as usize;
    let y0 = (cy - r - 2.0).floor().max(0.0) as usize;
    let x1 = ((cx + r + 2.0).ceil() as usize + 1).min(img.width);
    let y1 = ((cy + r + 2.0).ceil() as usize + 1).min(img.height);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let at = |x: usize, y: usize| (y - y0) * bw + (x - x0);
    let dist = |x: usize, y: usize| (x as f64 - cx).hypot(y as f64 - cy);
    // What the source circle has, read before anything here changes.
    let mut copied = vec![[0f32; 3]; bw * bh];
    for y in y0..y1 {
        for x in x0..x1 {
            copied[at(x, y)] = sample(img, x as f64 + ox, y as f64 + oy);
        }
    }
    if s.kind == SpotKind::Heal {
        let inside: Vec<bool> = (0..bw * bh)
            .map(|i| {
                let (x, y) = (x0 + i % bw, y0 + i / bw);
                // Every cell solved for has its four neighbours here.
                dist(x, y) < r && x > x0 && y > y0 && x + 1 < x1 && y + 1 < y1
            })
            .collect();
        // The difference on the edge, and its harmonic fill inside.
        let mut f: Vec<[f32; 3]> = (0..bw * bh)
            .map(|i| {
                let p = img.px(x0 + i % bw, y0 + i / bw);
                std::array::from_fn(|k| p[k] - copied[i][k])
            })
            .collect();
        let mut mean = [0f64; 3];
        let mut n = 0.0;
        for (i, v) in f.iter().enumerate() {
            if !inside[i] {
                for k in 0..3 {
                    mean[k] += f64::from(v[k]);
                }
                n += 1.0;
            }
        }
        let mean = mean.map(|m| (m / f64::max(n, 1.0)) as f32);
        for (i, v) in f.iter_mut().enumerate() {
            if inside[i] {
                *v = mean;
            }
        }
        let across = 2.0 * r;
        let omega = (2.0 / (1.0 + (std::f64::consts::PI / across.max(2.0)).sin())) as f32;
        let scale = f.iter().flatten().fold(1e-6f32, |m, v| m.max(v.abs()));
        let cells: Vec<usize> = (0..bw * bh).filter(|&i| inside[i]).collect();
        for _ in 0..((6.0 * across) as usize + 20).min(MAX_SWEEPS) {
            let mut change = 0f32;
            for &i in &cells {
                for k in 0..3 {
                    let avg = 0.25 * (f[i - 1][k] + f[i + 1][k] + f[i - bw][k] + f[i + bw][k]);
                    let d = omega * (avg - f[i][k]);
                    f[i][k] += d;
                    change = change.max(d.abs());
                }
            }
            if change <= 1e-5 * scale {
                break;
            }
        }
        for (i, c) in copied.iter_mut().enumerate() {
            if inside[i] {
                for k in 0..3 {
                    c[k] = (c[k] + f[i][k]).max(0.0);
                }
            }
        }
    }
    let opacity = (s.opacity / 100.0).clamp(0.0, 1.0);
    let inner = r * (1.0 - s.feather.clamp(0.0, 1.0));
    for y in y0..y1 {
        for x in x0..x1 {
            let a = ((1.0 - smoothstep(inner, r, dist(x, y))) * opacity) as f32;
            if a <= 0.0 {
                continue;
            }
            let v = copied[at(x, y)];
            let i = (y * img.width + x) * 3;
            for k in 0..3 {
                let o = &mut img.data[i + k];
                *o += (v[k] - *o) * a;
            }
        }
    }
}

/// Red eye: in the circle, the pupil found by its redness (red well above
/// green and blue, relative to brightness, so skin is left alone) loses
/// its red (red takes green and blue's mean) and darkens. Pet eye: the
/// whole circle turns a dark grey of its own brightness.
fn eye(img: &mut Rgb, s: &Spot) {
    let long = img.width.max(img.height) as f64;
    let (cx, cy) = (s.dest[0] * long - 0.5, s.dest[1] * long - 0.5);
    let r = (s.radius * long).max(1.0);
    let x0 = (cx - r - 1.0).floor().max(0.0) as usize;
    let y0 = (cy - r - 1.0).floor().max(0.0) as usize;
    let x1 = ((cx + r + 1.0).ceil() as usize + 1).min(img.width);
    let y1 = ((cy + r + 1.0).ceil() as usize + 1).min(img.height);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let inner = r * (1.0 - s.feather.clamp(0.0, 1.0));
    let darken = (s.darken / 100.0).clamp(0.0, 1.0) as f32;
    let pet = s.kind == SpotKind::PetEye;
    // How much each pixel is pupil.
    let mut m = vec![0f32; bw * bh];
    for y in y0..y1 {
        for x in x0..x1 {
            let disk = 1.0 - smoothstep(inner, r, (x as f64 - cx).hypot(y as f64 - cy));
            let p = img.px(x, y);
            let red = if pet {
                1.0
            } else {
                let rr = (p[0] - p[1].max(p[2])) / (p[0] + p[1] + p[2]).max(1e-6);
                smoothstep(0.2, 0.4, f64::from(rr))
            };
            m[(y - y0) * bw + (x - x0)] = (disk * red) as f32;
        }
    }
    // A 3×3 blur, so the pupil's edge doesn't speckle.
    let soft: Vec<f32> = (0..bw * bh)
        .map(|i| {
            let (x, y) = ((i % bw) as i64, (i / bw) as i64);
            let (mut sum, mut n) = (0.0, 0.0);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx >= 0 && yy >= 0 && (xx as usize) < bw && (yy as usize) < bh {
                        sum += m[yy as usize * bw + xx as usize];
                        n += 1.0;
                    }
                }
            }
            sum / n
        })
        .collect();
    let opacity = (s.opacity / 100.0).clamp(0.0, 1.0) as f32;
    for y in y0..y1 {
        for x in x0..x1 {
            let a = soft[(y - y0) * bw + (x - x0)] * opacity;
            if a <= 0.0 {
                continue;
            }
            let i = (y * img.width + x) * 3;
            let p = [img.data[i], img.data[i + 1], img.data[i + 2]];
            let fixed = if pet {
                let luma = 0.3 * p[0] + 0.55 * p[1] + 0.15 * p[2];
                [luma * (1.0 - 0.9 * darken); 3]
            } else {
                let gb = 0.5 * (p[1] + p[2]);
                let k = 1.0 - 0.8 * darken;
                [gb * k, p[1] * k, p[2] * k]
            };
            for k in 0..3 {
                img.data[i + k] = p[k] + (fixed[k] - p[k]) * a;
            }
        }
    }
}

/// A good place to copy a spot at `dest` (units) from: of the circles
/// two to five radii away that lie inside the photo, the one whose
/// surroundings (a ring around the circle) look most like the spot's.
pub fn find_source(img: &Rgb, dest: [f64; 2], radius: f64) -> [f64; 2] {
    let long = img.width.max(img.height) as f64;
    if long == 0.0 {
        return dest;
    }
    let (cx, cy) = (dest[0] * long - 0.5, dest[1] * long - 0.5);
    let r = (radius * long).max(1.0);
    let ring = 1.6 * r;
    // Points on the ring between the circle and 1.6 radii.
    let step = (r / 8.0).max(1.0);
    let mut offsets = Vec::new();
    let mut dy = -ring;
    while dy <= ring {
        let mut dx = -ring;
        while dx <= ring {
            let d = dx.hypot(dy);
            if d >= 1.05 * r && d <= ring {
                offsets.push((dx, dy));
            }
            dx += step;
        }
        dy += step;
    }
    let tone = |x: f64, y: f64| {
        let p = sample(img, x, y);
        p.map(|v| v.max(0.0).sqrt())
    };
    let here: Vec<[f32; 3]> = offsets
        .iter()
        .map(|&(dx, dy)| tone(cx + dx, cy + dy))
        .collect();
    let fits = |x: f64, y: f64| {
        x - ring >= 0.0
            && y - ring >= 0.0
            && x + ring <= (img.width - 1) as f64
            && y + ring <= (img.height - 1) as f64
    };
    let mut best: Option<(f64, f64, f64)> = None;
    for k in [2.5, 3.5, 5.0] {
        for a in 0..24 {
            let t = f64::from(a) / 24.0 * std::f64::consts::TAU;
            let (x, y) = (cx + k * r * t.cos(), cy + k * r * t.sin());
            if !fits(x, y) {
                continue;
            }
            let ssd: f64 = offsets
                .iter()
                .zip(&here)
                .map(|(&(dx, dy), h)| {
                    let p = tone(x + dx, y + dy);
                    (0..3).map(|c| f64::from(p[c] - h[c]).powi(2)).sum::<f64>()
                })
                .sum();
            // A little in favour of nearer places.
            let score = ssd * (1.0 + 0.05 * k);
            if best.is_none_or(|b| score < b.0) {
                best = Some((score, x, y));
            }
        }
    }
    let (x, y) = match best {
        Some((_, x, y)) => (x, y),
        None => (
            (cx + 2.5 * r).min((img.width - 1) as f64),
            cy.clamp(0.0, (img.height - 1) as f64),
        ),
    };
    [(x + 0.5) / long, (y + 0.5) / long]
}
