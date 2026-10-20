//! The modules that need a neighbourhood (EDITING-SPEC modules 5, 7 and 8:
//! dehaze, highlights and shadows, clarity), as a *fast guided filter*:
//!
//! - A small copy of the source (long edge [`SMALL`]) is analysed on the CPU:
//!   guided-filter coefficients `(a, b)` per small pixel, box-averaged. They
//!   live in source space, so crop, rotation, the proxy, 100% regions and
//!   export all use the same maps.
//! - The live pipe samples the coefficients (bilinear) at each pixel's
//!   source position and evaluates `a·I + b` with that pixel's own guide
//!   `I`, so edges stay sharp at any resolution (He and Sun, "Fast Guided
//!   Filter", 2015).
//!
//! Highlights and shadows: a base layer of log luminance (stops from mid
//! grey, guided by itself) decides each pixel's gain, so detail keeps its
//! contrast and edges don't halo. Clarity: the same at a coarser scale,
//! boosting the detail above that base in the midtones. Dehaze: the dark
//! channel prior (He, Sun and Tang, 2009) for airlight and transmission,
//! the transmission refined by a guided filter on luminance.
//!
//! The maps depend on the colour matrix and the dehaze amount, not on the
//! slider amounts, so dragging highlights, shadows or clarity reuses them.

use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use super::{Demosaiced, Rgb};
use crate::colour;
use crate::edit::Settings;

/// The analysis copy's long edge.
pub const SMALL: usize = 512;
/// Floats per map pixel: highlights and shadows `(a, b)`, clarity `(a, b)`,
/// dehaze transmission `(a, b)`.
pub const STRIDE: usize = 6;

/// Guided filter radii (fractions of the long edge) and regularisation.
const HS_RADIUS: f32 = 0.025;
const HS_EPS: f32 = 0.25;
const CLARITY_RADIUS: f32 = 0.06;
const CLARITY_EPS: f32 = 1.0;
const HAZE_PATCH: f32 = 0.01;
const HAZE_RADIUS: f32 = 0.04;
const HAZE_EPS: f32 = 1e-3;
const HAZE_OMEGA: f32 = 0.95;
const HAZE_FLOOR: f32 = 0.1;

/// The maps, `width`×`height`, [`STRIDE`] floats each.
#[derive(Debug, Clone, PartialEq)]
pub struct Maps {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    /// Dehaze's airlight, linear Rec.2020.
    pub airlight: [f32; 3],
}

/// A source's small copy and its last maps (keyed by what they depend on).
#[derive(Clone, Default)]
pub struct Cache(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    small: Option<Arc<Rgb>>,
    maps: Option<(u64, Arc<Maps>)>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("maps::Cache")
    }
}

/// What the live pipe needs for these modules.
#[derive(Debug, Clone)]
pub struct Local {
    /// −1 to 1.
    pub shadows: f32,
    pub highlights: f32,
    pub clarity: f32,
    pub dehaze: f32,
    /// None: nothing to do.
    pub maps: Option<Arc<Maps>>,
    /// Source pixels → map pixels.
    pub scale: [f32; 2],
    /// Rec.2020 luminance weights.
    pub luma: [f32; 3],
}

impl Local {
    pub fn none() -> Self {
        Self {
            shadows: 0.0,
            highlights: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            maps: None,
            scale: [1.0; 2],
            luma: luma(),
        }
    }
}

fn luma() -> [f32; 3] {
    colour::rgb_to_xyz(&colour::REC2020, colour::D65)[1].map(|v| v as f32)
}

/// The modules' numbers for `src` under `colour` (the frame's matrix).
pub fn local(src: &Demosaiced, colour: &[[f32; 3]; 3], s: &Settings) -> Local {
    let m = &s.modules;
    let unit = |v: f64| (v / 100.0) as f32;
    let mut l = Local {
        shadows: unit(m.highlights_shadows.shadows),
        highlights: unit(m.highlights_shadows.highlights),
        clarity: unit(m.clarity.amount),
        dehaze: unit(m.dehaze.amount),
        ..Local::none()
    };
    let need = Need {
        tones: l.shadows != 0.0 || l.highlights != 0.0,
        clarity: l.clarity != 0.0,
        dehaze: l.dehaze,
    };
    if !need.tones && !need.clarity && need.dehaze == 0.0 {
        return l;
    }
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for r in colour {
            for v in r {
                v.to_bits().hash(&mut h);
            }
        }
        (need.tones, need.clarity, need.dehaze.to_bits()).hash(&mut h);
        h.finish()
    };
    let mut cache = src.cache.0.lock().unwrap_or_else(|e| e.into_inner());
    let small = cache
        .small
        .get_or_insert_with(|| Arc::new(small(&src.rgb)))
        .clone();
    let maps = match &cache.maps {
        Some((k, maps)) if *k == key => maps.clone(),
        _ => {
            let maps = Arc::new(analyse(&small, colour, &need));
            cache.maps = Some((key, maps.clone()));
            maps
        }
    };
    l.scale = [
        maps.width as f32 / src.rgb.width as f32,
        maps.height as f32 / src.rgb.height as f32,
    ];
    l.maps = Some(maps);
    l
}

struct Need {
    tones: bool,
    clarity: bool,
    dehaze: f32,
}

/// An area-averaged copy with its long edge at most [`SMALL`].
pub fn small(img: &Rgb) -> Rgb {
    let long = img.width.max(img.height);
    if long <= SMALL {
        return img.clone();
    }
    let k = SMALL as f64 / long as f64;
    let w = ((img.width as f64 * k).round() as usize).max(1);
    let h = ((img.height as f64 * k).round() as usize).max(1);
    let xs = weights(img.width, w);
    let ys = weights(img.height, h);
    let mut rows = vec![0f32; w * img.height * 3];
    rows.par_chunks_mut(w * 3).enumerate().for_each(|(y, out)| {
        for (x, ws) in xs.iter().enumerate() {
            for &(i, wt) in ws {
                let p = img.px(i, y);
                for c in 0..3 {
                    out[x * 3 + c] += p[c] * wt;
                }
            }
        }
    });
    let mut out = Rgb::new(w, h);
    out.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, line)| {
            for &(i, wt) in &ys[y] {
                let src = &rows[i * w * 3..(i + 1) * w * 3];
                for (o, v) in line.iter_mut().zip(src) {
                    *o += v * wt;
                }
            }
        });
    out
}

/// Each output sample as the area-weighted mean of the input it covers.
fn weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|o| {
            let (start, end) = (o as f64 * scale, (o + 1) as f64 * scale);
            let mut ws = Vec::new();
            let mut i = start.floor() as usize;
            while (i as f64) < end && i < src {
                let lo = (i as f64).max(start);
                let hi = ((i + 1) as f64).min(end);
                if hi > lo {
                    ws.push((i, ((hi - lo) / scale) as f32));
                }
                i += 1;
            }
            ws
        })
        .collect()
}

fn mul3(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Stops from mid grey.
fn stops(y: f32) -> f32 {
    (y.max(1e-6) / super::MID_GREY).log2()
}

fn analyse(small: &Rgb, colour: &[[f32; 3]; 3], need: &Need) -> Maps {
    let (w, h) = (small.width, small.height);
    let long = w.max(h) as f32;
    let radius = |f: f32| ((f * long).round() as usize).max(1);
    let y = luma();
    let mut lin: Vec<[f32; 3]> = small
        .data
        .as_chunks::<3>()
        .0
        .par_iter()
        .map(|p| mul3(colour, *p))
        .collect();
    let mut data = vec![0f32; w * h * STRIDE];
    let mut airlight = [1.0f32; 3];
    if need.dehaze != 0.0 {
        let dark = min_filter(
            &lin.iter()
                .map(|p| p[0].min(p[1]).min(p[2]))
                .collect::<Vec<_>>(),
            w,
            h,
            radius(HAZE_PATCH),
        );
        // Airlight: the mean colour of the haziest 0.1%.
        let mut order: Vec<usize> = (0..w * h).collect();
        let n = (w * h / 1000).max(1);
        order.select_nth_unstable_by(n - 1, |&a, &b| dark[b].total_cmp(&dark[a]));
        let mut sum = [0f32; 3];
        for &i in &order[..n] {
            for c in 0..3 {
                sum[c] += lin[i][c];
            }
        }
        airlight = sum.map(|v| (v / n as f32).max(1e-4));
        let norm: Vec<f32> = lin
            .iter()
            .map(|p| (0..3).map(|c| p[c] / airlight[c]).fold(f32::MAX, f32::min))
            .collect();
        let dn = min_filter(&norm, w, h, radius(HAZE_PATCH));
        let t: Vec<f32> = dn
            .iter()
            .map(|d| (1.0 - HAZE_OMEGA * d).clamp(0.0, 1.0))
            .collect();
        let ay = dot(y, airlight);
        let guide: Vec<f32> = lin.par_iter().map(|p| haze_guide(dot(y, *p), ay)).collect();
        let (a, b) = guided(&guide, &t, w, h, radius(HAZE_RADIUS), HAZE_EPS);
        data.par_chunks_mut(STRIDE)
            .zip(lin.par_iter_mut())
            .enumerate()
            .for_each(|(i, (d, p))| {
                d[4] = a[i];
                d[5] = b[i];
                *p = dehaze_px(*p, a[i] * guide[i] + b[i], airlight, need.dehaze);
            });
    }
    let l: Vec<f32> = lin.par_iter().map(|p| stops(dot(y, *p))).collect();
    for (on, at, r, eps) in [
        (need.tones, 0, HS_RADIUS, HS_EPS),
        (need.clarity, 2, CLARITY_RADIUS, CLARITY_EPS),
    ] {
        if on {
            let (a, b) = guided(&l, &l, w, h, radius(r), eps);
            data.par_chunks_mut(STRIDE).enumerate().for_each(|(i, d)| {
                d[at] = a[i];
                d[at + 1] = b[i];
            });
        }
    }
    Maps {
        width: w,
        height: h,
        data,
        airlight,
    }
}

fn haze_guide(y: f32, airlight_y: f32) -> f32 {
    (y / airlight_y.max(1e-6)).clamp(0.0, 2.0)
}

/// Remove (amount > 0) or add (< 0) haze, given transmission `t`.
pub fn dehaze_px(p: [f32; 3], t: f32, airlight: [f32; 3], amount: f32) -> [f32; 3] {
    if amount > 0.0 {
        let t = t.clamp(HAZE_FLOOR, 1.0);
        [0, 1, 2].map(|c| {
            let j = (p[c] - airlight[c]) / t + airlight[c];
            (p[c] + amount * (j - p[c])).max(0.0)
        })
    } else {
        [0, 1, 2].map(|c| p[c] + (-amount) * 0.5 * (airlight[c] - p[c]))
    }
}

/// The mean of a `(2r+1)²` box (clipped at the edges), by running sums.
fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let line = &v[y * w..(y + 1) * w];
        let mut prefix = vec![0f64; w + 1];
        for x in 0..w {
            prefix[x + 1] = prefix[x] + f64::from(line[x]);
        }
        for (x, o) in out.iter_mut().enumerate() {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            *o = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
        }
    });
    // Down the columns: running sums of whole rows.
    let mut prefix = vec![0f64; (h + 1) * w];
    for y in 0..h {
        let (done, next) = prefix.split_at_mut((y + 1) * w);
        let above = &done[y * w..];
        for (x, p) in next[..w].iter_mut().enumerate() {
            *p = above[x] + f64::from(rows[y * w + x]);
        }
    }
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
        let (lo, hi) = (y.saturating_sub(r), (y + r + 1).min(h));
        let n = (hi - lo) as f64;
        let (top, bottom) = (&prefix[lo * w..], &prefix[hi * w..]);
        for (x, o) in line.iter_mut().enumerate() {
            *o = ((bottom[x] - top[x]) / n) as f32;
        }
    });
    out
}

/// The minimum over a `(2r+1)²` box (clipped at the edges).
fn min_filter(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let line = &v[y * w..(y + 1) * w];
        for (x, o) in out.iter_mut().enumerate() {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            *o = line[lo..hi].iter().copied().fold(f32::MAX, f32::min);
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
        let (lo, hi) = (y.saturating_sub(r), (y + r + 1).min(h));
        for (x, o) in line.iter_mut().enumerate() {
            *o = (lo..hi).map(|yy| rows[yy * w + x]).fold(f32::MAX, f32::min);
        }
    });
    out
}

/// A guided filter's box-averaged coefficients `(ā, b̄)`: the output at a
/// pixel with guide `I` is `ā·I + b̄`.
fn guided(
    guide: &[f32],
    p: &[f32],
    w: usize,
    h: usize,
    r: usize,
    eps: f32,
) -> (Vec<f32>, Vec<f32>) {
    let mean_i = box_mean(guide, w, h, r);
    let mean_p = box_mean(p, w, h, r);
    let ip: Vec<f32> = guide.par_iter().zip(p).map(|(i, p)| i * p).collect();
    let ii: Vec<f32> = guide.par_iter().map(|i| i * i).collect();
    let mean_ip = box_mean(&ip, w, h, r);
    let mean_ii = box_mean(&ii, w, h, r);
    let (a, b): (Vec<f32>, Vec<f32>) = (0..w * h)
        .into_par_iter()
        .map(|k| {
            let var = (mean_ii[k] - mean_i[k] * mean_i[k]).max(0.0);
            let cov = mean_ip[k] - mean_i[k] * mean_p[k];
            let a = cov / (var + eps);
            (a, mean_p[k] - a * mean_i[k])
        })
        .unzip();
    (box_mean(&a, w, h, r), box_mean(&b, w, h, r))
}

/// The maps at map position `(x, y)` (bilinear, clamped to the edges).
pub fn sample(m: &Maps, x: f32, y: f32) -> [f32; STRIDE] {
    let x = x.clamp(0.0, (m.width - 1) as f32);
    let y = y.clamp(0.0, (m.height - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(m.width - 1), (y0 + 1).min(m.height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |xx: usize, yy: usize| &m.data[(yy * m.width + xx) * STRIDE..][..STRIDE];
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        top + (bot - top) * fy
    })
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The modules for one linear Rec.2020 pixel at source position
/// `(sx, sy)`.
pub fn local_pixel(l: &Local, p: [f32; 3], sx: f32, sy: f32) -> [f32; 3] {
    let Some(maps) = &l.maps else {
        return p;
    };
    let m = sample(
        maps,
        (sx + 0.5) * l.scale[0] - 0.5,
        (sy + 0.5) * l.scale[1] - 0.5,
    );
    let mut p = p;
    if l.dehaze != 0.0 {
        let g = haze_guide(dot(l.luma, p), dot(l.luma, maps.airlight));
        p = dehaze_px(p, m[4] * g + m[5], maps.airlight, l.dehaze);
    }
    let s = stops(dot(l.luma, p));
    let mut gain = 0.0;
    if l.shadows != 0.0 || l.highlights != 0.0 {
        let base = m[0] * s + m[1];
        gain += l.shadows * 1.5 * (1.0 - smoothstep(-3.0, 0.0, base));
        gain += l.highlights * 1.5 * smoothstep(-0.5, 2.5, base);
    }
    if l.clarity != 0.0 {
        let base = m[2] * s + m[3];
        let detail = (s - base).clamp(-3.0, 3.0);
        let mid = 1.0 - smoothstep(2.0, 4.0, (base + 0.5).abs());
        gain += l.clarity * 0.7 * detail * mid;
    }
    if gain == 0.0 {
        return p;
    }
    let k = gain.exp2();
    p.map(|v| v * k)
}
