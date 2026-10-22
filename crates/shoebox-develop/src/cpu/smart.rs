//! Smart Lighting (EDITING-SPEC, module 6: local tone): a local Laplacian
//! filter on log luminance (Paris, Hasinoff and Kautz, 2011, in Aubry et
//! al.'s fast form, 2014) that compresses large-scale tone, lifting
//! shadows and taming highlights, while keeping detail. It runs on the
//! maps' small copy at full strength; the difference it makes (stops) is
//! the gain map the live pipe scales by the Strength slider (see
//! [`super::maps`]).
//!
//! Spot weighted (PhotoLab's other mode): the same filter, then a shift
//! that brings a box's tone (a face, say) towards mid grey, whole for
//! tones on the spot's side and fading out a few stops away, so the rest
//! of the frame keeps its compressed tone around it ([`spotted`]).

use rayon::prelude::*;

/// Differences in log luminance up to this (stops) are detail, kept as
/// they are; larger ones are compressed.
const SIGMA: f32 = 1.0;
/// How much of a large difference is kept at full strength.
const BETA: f32 = 0.35;
/// Reference levels sampled across the image's range.
const LEVELS: usize = 12;
/// Spot weighted: the tones (stops from mid grey) a spot is brought into
/// at full strength (a light face sits about a stop over mid grey), and
/// the most it's moved.
const SPOT_LOW: f32 = -0.5;
const SPOT_HIGH: f32 = 1.0;
const SPOT_MAX: f32 = 2.0;
/// The shift is whole up to this far (stops) past the spot's tone, and
/// gone by [`SPOT_FADE`].
const SPOT_HOLD: f32 = 1.0;
const SPOT_FADE: f32 = 3.0;

/// A plane.
#[derive(Clone)]
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    fn at(&self, x: usize, y: usize) -> f32 {
        self.v[y * self.w + x]
    }
}

/// The 5-tap binomial blur, then every other pixel.
fn down(p: &Plane) -> Plane {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let (w, h) = (p.w, p.h);
    let clamp = |i: i64, n: usize| i.clamp(0, n as i64 - 1) as usize;
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        for (x, o) in out.iter_mut().enumerate() {
            *o = (0..5)
                .map(|k| K[k] * p.at(clamp(x as i64 + k as i64 - 2, w), y))
                .sum();
        }
    });
    let (dw, dh) = (w.div_ceil(2), h.div_ceil(2));
    let mut v = vec![0f32; dw * dh];
    v.par_chunks_mut(dw).enumerate().for_each(|(y, out)| {
        for (x, o) in out.iter_mut().enumerate() {
            *o = (0..5)
                .map(|k| K[k] * rows[clamp(2 * y as i64 + k as i64 - 2, h) * w + 2 * x])
                .sum();
        }
    });
    Plane { w: dw, h: dh, v }
}

/// Bilinear back up to `w`×`h` (pixel centres aligned).
fn up(p: &Plane, w: usize, h: usize) -> Plane {
    let mut v = vec![0f32; w * h];
    v.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let fy = ((y as f32 + 0.5) * p.h as f32 / h as f32 - 0.5).clamp(0.0, (p.h - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy - fy.floor());
        let y1 = (y0 + 1).min(p.h - 1);
        for (x, o) in out.iter_mut().enumerate() {
            let fx = ((x as f32 + 0.5) * p.w as f32 / w as f32 - 0.5).clamp(0.0, (p.w - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx - fx.floor());
            let x1 = (x0 + 1).min(p.w - 1);
            let top = p.at(x0, y0) + (p.at(x1, y0) - p.at(x0, y0)) * tx;
            let bot = p.at(x0, y1) + (p.at(x1, y1) - p.at(x0, y1)) * tx;
            *o = top + (bot - top) * ty;
        }
    });
    Plane { w, h, v }
}

fn gaussian(p: &Plane, n: usize) -> Vec<Plane> {
    let mut g = vec![p.clone()];
    for _ in 1..n {
        let next = down(g.last().unwrap_or(p));
        g.push(next);
    }
    g
}

fn laplacian(g: &[Plane]) -> Vec<Plane> {
    let mut l = Vec::with_capacity(g.len());
    for j in 0..g.len() - 1 {
        let e = up(&g[j + 1], g[j].w, g[j].h);
        let v = g[j].v.par_iter().zip(&e.v).map(|(a, b)| a - b).collect();
        l.push(Plane {
            w: g[j].w,
            h: g[j].h,
            v,
        });
    }
    if let Some(last) = g.last() {
        l.push(last.clone());
    }
    l
}

/// The remapping about reference `g`: detail kept, large differences
/// compressed by `beta`.
fn remap(i: f32, g: f32, beta: f32) -> f32 {
    let d = i - g;
    if d.abs() <= SIGMA {
        i
    } else {
        g + d.signum() * (SIGMA + beta * (d.abs() - SIGMA))
    }
}

/// The gain (stops) Smart Lighting at full strength gives each pixel of a
/// `w`×`h` image of log luminance (stops).
pub fn gain(l: &[f32], w: usize, h: usize) -> Vec<f32> {
    if w < 8 || h < 8 {
        return vec![0.0; w * h];
    }
    let input = Plane {
        w,
        h,
        v: l.to_vec(),
    };
    // Down to a few pixels, so whole regions (a sky, a shadowed
    // foreground) are compressed, not only edges.
    let n = ((w.max(h) as f32).log2().ceil() as usize + 1).clamp(2, 13);
    let g = gaussian(&input, n);
    let (lo, hi) = l
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    if hi - lo < 1e-3 {
        return vec![0.0; w * h];
    }
    let step = (hi - lo) / (LEVELS - 1) as f32;
    // The output pyramid: each level's coefficient from the remapped
    // pyramids of the two references either side of the Gaussian value
    // there, weighted by distance.
    let mut out: Vec<Plane> = g
        .iter()
        .map(|p| Plane {
            w: p.w,
            h: p.h,
            v: vec![0.0; p.v.len()],
        })
        .collect();
    for k in 0..LEVELS {
        let gk = lo + k as f32 * step;
        let remapped = Plane {
            w,
            h,
            v: l.par_iter().map(|&i| remap(i, gk, BETA)).collect(),
        };
        let lap = laplacian(&gaussian(&remapped, n));
        for j in 0..n - 1 {
            let (o, gj, lj) = (&mut out[j], &g[j], &lap[j]);
            o.v.par_iter_mut()
                .zip(gj.v.par_iter().zip(&lj.v))
                .for_each(|(o, (gv, lv))| {
                    let wgt = (1.0 - (gv - gk).abs() / step).max(0.0);
                    *o += wgt * lv;
                });
        }
    }
    // The coarsest level as it was.
    out[n - 1] = g[n - 1].clone();
    // Collapse.
    let mut r = out[n - 1].clone();
    for j in (0..n - 1).rev() {
        let e = up(&r, out[j].w, out[j].h);
        r = Plane {
            w: e.w,
            h: e.h,
            v: e.v.par_iter().zip(&out[j].v).map(|(a, b)| a + b).collect(),
        };
    }
    r.v.par_iter().zip(l).map(|(o, i)| o - i).collect()
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Spot weighted: the uniform gain `g` for a `w`×`h` image of log
/// luminance `l`, shifted so the spot `[x0, y0, x1, y1]` (pixels, clipped
/// to the image) comes out between [`SPOT_LOW`] and [`SPOT_HIGH`] (its
/// mean, after `g`). A dark spot's lift reaches every tone up to
/// [`SPOT_HOLD`] over it and fades by [`SPOT_FADE`], so a brighter
/// background is held; a bright spot's cut reaches down likewise, holding
/// the shadows. An empty spot changes nothing.
pub fn spotted(l: &[f32], g: &[f32], w: usize, h: usize, spot: [f32; 4]) -> Vec<f32> {
    let clip = |v: f32, n: usize| {
        if v.is_finite() {
            v.clamp(0.0, n as f32)
        } else {
            0.0
        }
    };
    let (x0, x1) = (clip(spot[0], w), clip(spot[2], w));
    let (y0, y1) = (clip(spot[1], h), clip(spot[3], h));
    let (x0, x1) = (x0.min(x1).floor() as usize, x0.max(x1).ceil() as usize);
    let (y0, y1) = (y0.min(y1).floor() as usize, y0.max(y1).ceil() as usize);
    let tone = |i: usize| l[i] + g[i];
    let (mut sum, mut n) = (0f64, 0usize);
    for y in y0..y1.min(h) {
        for x in x0..x1.min(w) {
            sum += f64::from(tone(y * w + x));
            n += 1;
        }
    }
    if n == 0 {
        return g.to_vec();
    }
    let at = (sum / n as f64) as f32;
    let shift = if at < SPOT_LOW {
        SPOT_LOW - at
    } else if at > SPOT_HIGH {
        SPOT_HIGH - at
    } else {
        0.0
    }
    .clamp(-SPOT_MAX, SPOT_MAX);
    if shift == 0.0 {
        return g.to_vec();
    }
    (0..w * h)
        .into_par_iter()
        .map(|i| {
            let t = tone(i);
            let reach = if shift > 0.0 {
                1.0 - smoothstep(at + SPOT_HOLD, at + SPOT_FADE, t)
            } else {
                smoothstep(at - SPOT_FADE, at - SPOT_HOLD, t)
            };
            g[i] + shift * reach
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dark_spot_is_lifted_and_the_bright_side_held() {
        // Dark (−4 stops) on the left, bright (+4) on the right.
        let (w, h) = (128, 96);
        let l: Vec<f32> = (0..w * h)
            .map(|i| if i % w < w / 2 { -4.0 } else { 4.0 })
            .collect();
        let g = gain(&l, w, h);
        let mean = |v: &[f32], x0: usize, x1: usize| {
            let mut s = 0.0;
            for y in 0..h {
                for x in x0..x1 {
                    s += v[y * w + x];
                }
            }
            s / ((x1 - x0) * h) as f32
        };
        let spot = spotted(&l, &g, w, h, [8.0, 8.0, 40.0, 88.0]);
        let lift = mean(&spot, 4, 40) - mean(&g, 4, 40);
        assert!(lift > 1.0, "{lift}");
        let held = mean(&spot, 88, 124) - mean(&g, 88, 124);
        assert!(held.abs() < 0.05, "{held}");
        // A bright spot is brought down (here with no uniform gain under
        // it), the dark side held.
        let zero = vec![0.0; w * h];
        let spot = spotted(&l, &zero, w, h, [88.0, 8.0, 120.0, 88.0]);
        assert!((mean(&spot, 88, 124) + SPOT_MAX).abs() < 1e-4);
        assert!(mean(&spot, 4, 40).abs() < 1e-4);
        // An empty spot, or nonsense, changes nothing.
        assert_eq!(spotted(&l, &g, w, h, [200.0, 10.0, 300.0, 80.0]), g);
        assert_eq!(spotted(&l, &g, w, h, [f32::NAN, 0.0, f32::NAN, 1.0]), g);
    }

    #[test]
    fn lifts_dark_halves_and_tames_bright_ones_keeping_detail() {
        // A scene four stops brighter on the right, with fine detail.
        let (w, h) = (128, 96);
        let l: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let base = if x < w / 2 { -3.0 } else { 1.0 };
                base + 0.2 * (((x + y) % 4) as f32 - 1.5)
            })
            .collect();
        let g = gain(&l, w, h);
        let mean = |x0: usize, x1: usize| {
            let mut s = 0.0;
            let mut n = 0.0;
            for y in 0..h {
                for x in x0..x1 {
                    s += g[y * w + x];
                    n += 1.0;
                }
            }
            s / n
        };
        // Away from the edge: the dark side up, the bright side down.
        let (dark, bright) = (mean(4, 40), mean(88, 124));
        assert!(dark > 0.3, "{dark}");
        assert!(bright < -0.3, "{bright}");
        // Detail kept: neighbours' difference much as it was.
        let i = 20 * w + 20;
        let before = l[i + 1] - l[i];
        let after = (l[i + 1] + g[i + 1]) - (l[i] + g[i]);
        assert!((after - before).abs() < 0.08, "{before} → {after}");
    }
}
