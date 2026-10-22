//! Smart Lighting (EDITING-SPEC, module 6: local tone): a local Laplacian
//! filter on log luminance (Paris, Hasinoff and Kautz, 2011, in Aubry et
//! al.'s fast form, 2014) that compresses large-scale tone, lifting
//! shadows and taming highlights, while keeping detail. It runs on the
//! maps' small copy at full strength; the difference it makes (stops) is
//! the gain map the live pipe scales by the Strength slider (see
//! [`super::maps`]).

use rayon::prelude::*;

/// Differences in log luminance up to this (stops) are detail, kept as
/// they are; larger ones are compressed.
const SIGMA: f32 = 1.0;
/// How much of a large difference is kept at full strength.
const BETA: f32 = 0.35;
/// Reference levels sampled across the image's range.
const LEVELS: usize = 12;

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

#[cfg(test)]
mod tests {
    use super::*;

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
