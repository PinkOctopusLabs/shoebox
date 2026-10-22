//! Object removal (EDITING-SPEC, Healing and object removal): what a
//! removal's brush strokes paint over is filled from its surroundings by
//! an inpainting model the app registers (MI-GAN in `shoebox-ml`), as one
//! of the spots, in their order, on the source after the detail stage.
//!
//! - [`job`]: a square crop around the strokes (twice their extent, at
//!   least [`MIN_SIDE`] long edges, in the stored image's units so the
//!   proxy, 100% regions and exports crop the same part), resampled to the
//!   model's size. The model was trained on sRGB photos, so the crop's
//!   linear values are divided by its own bright end (the 99th percentile
//!   of what's kept) and sRGB-encoded; the fill is decoded back the same
//!   way. The hole is everything the strokes reach, feather included.
//! - [`run`]: the model, or its fill kept on disk (16-bit PNGs in
//!   `<cache>/ai-fills`, through [`crate::mask::store`]'s store), keyed by
//!   a BLAKE3 of exactly what the model sees and its identity.
//! - [`apply`]: the fill (the model's picture where it painted, the input
//!   elsewhere) read bilinearly at the source's pixels and blended in by
//!   the strokes' coverage, soft over the feather, times the opacity.
//!
//! Exports and 100% regions make their own job from the full-size source:
//! the model sees nearly the same picture (the same crop, resampled to the
//! same size), so it fills alike; its fill is upsampled as on the proxy.

use std::sync::{Arc, OnceLock, RwLock};

use rayon::prelude::*;

use crate::cpu::{Rgb, srgb_eotf, srgb_oetf};
use crate::edit::{Spot, Stroke};
use crate::mask::store::Store;

/// Fills `rgb` (`size`² pixels, three sRGB-like 0–1 values each) where
/// `known` is false: the whole picture as the model draws it.
pub type Inpainter = dyn Fn(&[f32], &[bool]) -> Result<Vec<f32>, String> + Send + Sync;

struct Registered {
    id: String,
    size: usize,
    run: Box<Inpainter>,
}

static INPAINTER: OnceLock<Registered> = OnceLock::new();
static STORE: RwLock<Option<Arc<Store>>> = RwLock::new(None);

/// The model's size when none is registered.
const SIZE: usize = 512;
/// The crop is at least this many long edges across …
pub const MIN_SIDE: f64 = 0.2;
/// … and this many times the strokes' extent.
const CONTEXT: f64 = 2.0;
/// Bounds on what an edit file can ask for: strokes, points, and the area
/// (long edges²) the strokes' segments reach over together, which is what
/// drawing them costs (a hand-painted removal is well under one).
pub const MAX_STROKES: usize = 500;
pub const MAX_POINTS: usize = 20_000;
pub const MAX_AREA: f64 = 16.0;
/// A point nearer the last one kept than this many radii is dropped, as
/// the brush spaces them.
const SPACING: f64 = 0.25;
/// Fills kept on disk: about 1 MB each.
pub const DEFAULT_LIMIT: u64 = 256 << 20;
/// Bumped whenever the crop, encoding or what's kept changes in a way the
/// key wouldn't see.
const VERSION: &str = "shoebox-fill/1";

/// Give Develop an inpainting model, named `id` (fills are kept by it),
/// taking `size`² pictures. Once.
pub fn register(id: &str, size: usize, run: Box<Inpainter>) {
    let _ = INPAINTER.set(Registered {
        id: id.to_owned(),
        size,
        run,
    });
}

/// The registered model's name.
pub fn model() -> Option<&'static str> {
    INPAINTER.get().map(|r| r.id.as_str())
}

/// Keep fills in `store` (None: nowhere). The app sets it to
/// `<cache>/ai-fills`.
pub fn set_store(store: Option<Store>) {
    let mut s = STORE.write().unwrap_or_else(|e| e.into_inner());
    *s = store.map(Arc::new);
}

fn store() -> Option<Arc<Store>> {
    STORE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// What the model is asked: a crop of the image, resampled.
#[derive(Debug, Clone)]
pub struct Job {
    /// Over everything below and the model's identity.
    pub key: String,
    /// The crop in the image's pixels: x, y, width, height.
    pub crop: [usize; 4],
    pub size: usize,
    /// Linear values are divided by this before encoding.
    pub white: f32,
    /// `size`² × 3, encoded 0–1 (zero where unknown).
    pub input: Vec<f32>,
    pub known: Vec<bool>,
}

/// What the model gave for a [`Job`]: `size`² × 3 encoded values, the
/// input kept where it was known.
#[derive(Debug, Clone, PartialEq)]
pub struct Fill {
    pub size: usize,
    pub white: f32,
    pub data: Vec<f32>,
}

/// The strokes a removal paints with, sane (an edit file is JSON anyone
/// can write): finite, radii within bounds, points spaced as the brush
/// spaces them, at most [`MAX_STROKES`], [`MAX_POINTS`] and [`MAX_AREA`]
/// in all.
pub fn strokes(spot: &Spot) -> Vec<Stroke> {
    let (mut points, mut area) = (0, 0.0);
    let mut out = Vec::new();
    for s in spot.strokes.iter().take(MAX_STROKES) {
        if !(s.radius.is_finite() && s.radius > 0.0) {
            continue;
        }
        let radius = s.radius.min(super::MAX_RADIUS);
        // The feather reaches up to the radius again beyond it.
        let reach = 2.0 * radius;
        let mut pts: Vec<[f64; 2]> = Vec::new();
        let finite = s
            .points
            .iter()
            .filter(|p| p.iter().all(|v| v.is_finite() && v.abs() < 4.0));
        for p in finite {
            if points + pts.len() >= MAX_POINTS {
                break;
            }
            let cost = match pts.last() {
                Some(q) => {
                    let (dx, dy) = ((p[0] - q[0]).abs(), (p[1] - q[1]).abs());
                    if dx.hypot(dy) < SPACING * radius {
                        continue;
                    }
                    (dx + 2.0 * reach) * (dy + 2.0 * reach)
                }
                None => (2.0 * reach).powi(2),
            };
            if area + cost > MAX_AREA {
                break;
            }
            area += cost;
            pts.push(*p);
        }
        points += pts.len();
        if !pts.is_empty() {
            out.push(Stroke {
                points: pts,
                radius,
                ..s.clone()
            });
        }
    }
    out
}

/// The strokes' reach, in pixels of an image `long` pixels on its long
/// edge, plus `grow`: each pixel's coverage on a grid of `w`×`h` samples
/// at `origin + index × step` (pixel positions, centres at integers).
/// Painting strokes take the maximum; erasing ones take away. Each is
/// full within its radius, fading to nothing over `feather` of it beyond.
#[allow(clippy::too_many_arguments)]
fn coverage(
    strokes: &[Stroke],
    feather: f64,
    long: f64,
    grow: f64,
    origin: [f64; 2],
    step: [f64; 2],
    w: usize,
    h: usize,
) -> Vec<f32> {
    let mut cov = vec![0f32; w * h];
    let mut erased = Vec::new();
    for s in strokes {
        let r = s.radius * long + grow;
        let f = (feather.clamp(0.0, 1.0) * s.radius * long).max(1.0);
        let pts: Vec<[f64; 2]> = s
            .points
            .iter()
            .map(|p| [p[0] * long - 0.5, p[1] * long - 0.5])
            .collect();
        if s.erase {
            erased.clear();
            erased.resize(w * h, 0f32);
        }
        let reach = r + f;
        for k in 0..pts.len() {
            let a = pts[k];
            let b = pts[(k + 1).min(pts.len() - 1)];
            let (lo, hi) = (
                [a[0].min(b[0]) - reach, a[1].min(b[1]) - reach],
                [a[0].max(b[0]) + reach, a[1].max(b[1]) + reach],
            );
            let ix = |v: f64, c: usize| (v - origin[c]) / step[c];
            let x0 = ix(lo[0], 0).floor().max(0.0) as usize;
            let y0 = ix(lo[1], 1).floor().max(0.0) as usize;
            let x1 = (ix(hi[0], 0).ceil() + 1.0).clamp(0.0, w as f64) as usize;
            let y1 = (ix(hi[1], 1).ceil() + 1.0).clamp(0.0, h as f64) as usize;
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len2 = dx * dx + dy * dy;
            let target = if s.erase { &mut erased } else { &mut cov };
            for y in y0..y1 {
                let py = origin[1] + y as f64 * step[1];
                for x in x0..x1 {
                    let px = origin[0] + x as f64 * step[0];
                    let t = if len2 > 0.0 {
                        (((px - a[0]) * dx + (py - a[1]) * dy) / len2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let d = (px - a[0] - t * dx).hypot(py - a[1] - t * dy);
                    if d >= reach {
                        continue;
                    }
                    let c = 1.0 - smoothstep(r, reach, d) as f32;
                    let o = &mut target[y * w + x];
                    *o = o.max(c);
                }
            }
        }
        if s.erase {
            for (c, e) in cov.iter_mut().zip(&erased) {
                *c *= 1.0 - e;
            }
        }
    }
    cov
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The crop around `strokes` in a `w`×`h` image: x, y, width, height.
pub fn crop(strokes: &[Stroke], feather: f64, w: usize, h: usize) -> Option<[usize; 4]> {
    if w == 0 || h == 0 {
        return None;
    }
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for s in strokes.iter().filter(|s| !s.erase) {
        let reach = s.radius * (1.0 + feather.clamp(0.0, 1.0));
        for p in &s.points {
            for c in 0..2 {
                lo[c] = lo[c].min(p[c] - reach);
                hi[c] = hi[c].max(p[c] + reach);
            }
        }
    }
    if !(lo[0] < hi[0] && lo[1] < hi[1]) {
        return None;
    }
    let long = w.max(h) as f64;
    let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]);
    let side = (CONTEXT * extent).max(MIN_SIDE) * long;
    let centre = [(lo[0] + hi[0]) * 0.5 * long, (lo[1] + hi[1]) * 0.5 * long];
    let span = |c: usize, n: usize| {
        let n = n as f64;
        let len = side.min(n);
        let start = (centre[c] - len * 0.5).clamp(0.0, n - len);
        let (a, b) = (start.floor() as usize, (start + len).ceil() as usize);
        (a, b.min(n as usize).max(a + 1) - a)
    };
    let (x, cw) = span(0, w);
    let (y, ch) = span(1, h);
    Some([x, y, cw, ch])
}

/// What the model is to be asked for `spot` (a removal) on `img`, the
/// image as healed so far; None when its strokes paint nothing there.
pub fn job(img: &Rgb, spot: &Spot) -> Option<Job> {
    let strokes = strokes(spot);
    let feather = spot.feather.clamp(0.0, 1.0);
    let [x0, y0, cw, ch] = crop(&strokes, feather, img.width, img.height)?;
    let size = INPAINTER.get().map_or(SIZE, |r| r.size).max(1);
    let long = img.width.max(img.height) as f64;
    let step = [cw as f64 / size as f64, ch as f64 / size as f64];
    // Grid pixels the strokes reach at all (half a grid pixel's grace).
    let origin = [
        x0 as f64 + 0.5 * step[0] - 0.5,
        y0 as f64 + 0.5 * step[1] - 0.5,
    ];
    let grow = 0.5 * step[0].max(step[1]);
    let cov = coverage(&strokes, feather, long, grow, origin, step, size, size);
    let known: Vec<bool> = cov.iter().map(|&c| c <= 0.0).collect();
    if known.iter().all(|&k| k) {
        return None;
    }
    // The crop at the model's size: averaged over the pixels each covers,
    // or bilinear when the crop is smaller.
    let mut lin = vec![0f32; size * size * 3];
    lin.par_chunks_mut(size * 3)
        .enumerate()
        .for_each(|(gy, row)| {
            for gx in 0..size {
                let v = if step[0] > 1.0 || step[1] > 1.0 {
                    let a = |i: usize, s: f64, o: usize, n: usize| {
                        let lo = ((o as f64 + i as f64 * s).floor() as usize).min(n - 1);
                        let hi = ((o as f64 + (i + 1) as f64 * s).ceil() as usize).clamp(lo + 1, n);
                        lo..hi
                    };
                    let (xs, ys) = (
                        a(gx, step[0], x0, img.width),
                        a(gy, step[1], y0, img.height),
                    );
                    let n = (xs.len() * ys.len()) as f32;
                    let mut acc = [0f32; 3];
                    for y in ys {
                        for x in xs.clone() {
                            let p = img.px(x, y);
                            for c in 0..3 {
                                acc[c] += p[c];
                            }
                        }
                    }
                    acc.map(|v| v / n)
                } else {
                    bilinear(
                        img,
                        origin[0] + gx as f64 * step[0],
                        origin[1] + gy as f64 * step[1],
                    )
                };
                row[gx * 3..gx * 3 + 3].copy_from_slice(&v);
            }
        });
    // The bright end of what's kept.
    let mut levels: Vec<f32> = lin
        .chunks(3)
        .zip(&known)
        .filter(|(_, k)| **k)
        .map(|(p, _)| p[0].max(p[1]).max(p[2]))
        .collect();
    let white = if levels.is_empty() {
        1.0
    } else {
        let i = ((levels.len() - 1) as f64 * 0.99) as usize;
        *levels.select_nth_unstable_by(i, f32::total_cmp).1
    }
    .max(1e-6);
    let input: Vec<f32> = lin
        .chunks(3)
        .zip(&known)
        .flat_map(|(p, &k)| {
            let e = |v: f32| {
                if k {
                    srgb_oetf((v / white).clamp(0.0, 1.0))
                } else {
                    0.0
                }
            };
            [e(p[0]), e(p[1]), e(p[2])]
        })
        .collect();
    let key = {
        let id = model().unwrap_or("none");
        let mut k = blake3::Hasher::new();
        for s in [VERSION, id] {
            k.update(&(s.len() as u64).to_le_bytes());
            k.update(s.as_bytes());
        }
        k.update(&(size as u64).to_le_bytes());
        for v in &input {
            k.update(&v.to_bits().to_le_bytes());
        }
        let bits: Vec<u8> = known.iter().map(|&b| u8::from(b)).collect();
        k.update(&bits);
        k.finalize().to_hex().to_string()
    };
    Some(Job {
        key,
        crop: [x0, y0, cw, ch],
        size,
        white,
        input,
        known,
    })
}

/// Bilinear, clamped to the edges.
fn bilinear(img: &Rgb, x: f64, y: f64) -> [f32; 3] {
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

/// Fill `job` with the registered model, or read the fill kept for it.
/// Takes a fraction of a second (more the first time the model loads).
pub fn run(job: &Job) -> Result<Fill, String> {
    let Some(reg) = INPAINTER.get() else {
        return Err("no object removal model in this build".into());
    };
    let n = job.size * job.size;
    if reg.size != job.size || job.input.len() != 3 * n || job.known.len() != n {
        return Err("the removal was set up for another model".into());
    }
    let keep = |out: &[f32]| -> Vec<f32> {
        out.chunks(3)
            .zip(job.input.chunks(3))
            .zip(&job.known)
            .flat_map(|((o, i), &k)| {
                let p = if k { i } else { o };
                [p[0], p[1], p[2]].map(|v| v.clamp(0.0, 1.0))
            })
            .collect()
    };
    let store = store();
    if let Some(s) = &store
        && let Some(px) = s.get_rgb16(&job.key, job.size, job.size)
    {
        let out: Vec<f32> = px.iter().map(|&v| f32::from(v) / 65535.0).collect();
        return Ok(Fill {
            size: job.size,
            white: job.white,
            data: keep(&out),
        });
    }
    let out = (reg.run)(&job.input, &job.known)?;
    if out.len() != 3 * n {
        return Err("the fill came back the wrong size".into());
    }
    // At 16 bits, as kept on disk, so a fill read back is the same.
    let px: Vec<u16> = keep(&out)
        .iter()
        .map(|v| (v * 65535.0).round() as u16)
        .collect();
    let data = px.iter().map(|&v| f32::from(v) / 65535.0).collect();
    if let Some(s) = &store
        && let Err(e) = s.put_rgb16(&job.key, &px, job.size, job.size)
    {
        tracing::warn!("keeping an object removal: {e}");
    }
    Ok(Fill {
        size: job.size,
        white: job.white,
        data,
    })
}

/// Blend `fill` (made for `job`) into `img` where `spot`'s strokes paint.
///
/// A fill upsampled from the model's size is smoother than the photo
/// around it (its noise and finest texture are gone), which shows as a
/// soft patch at 100%. So the fine detail the photo has around the hole
/// (each pixel less its 3×3 mean, its power fitted per channel as `a + b ×
/// value`, as sensor noise grows with the signal) is measured, and what
/// the fill lacks of it is added back as noise (an integer hash of the
/// pixel, so it's the same every render).
pub fn apply(img: &mut Rgb, spot: &Spot, job: &Job, fill: &Fill) {
    let [x0, y0, cw, ch] = job.crop;
    let size = fill.size;
    if size == 0
        || cw == 0
        || ch == 0
        || fill.data.len() != size * size * 3
        || x0 + cw > img.width
        || y0 + ch > img.height
    {
        return;
    }
    let strokes = strokes(spot);
    let long = img.width.max(img.height) as f64;
    let feather = spot.feather.clamp(0.0, 1.0);
    let cov = coverage(
        &strokes,
        feather,
        long,
        0.0,
        [x0 as f64, y0 as f64],
        [1.0, 1.0],
        cw,
        ch,
    );
    let opacity = (spot.opacity / 100.0).clamp(0.0, 1.0) as f32;
    let (kx, ky) = (size as f32 / cw as f32, size as f32 / ch as f32);
    let lin: Vec<f32> = fill
        .data
        .par_iter()
        .map(|&v| srgb_eotf(v) * fill.white)
        .collect();
    // The fill at a crop pixel (bilinear).
    let filled = |x: usize, y: usize| -> [f32; 3] {
        let fy = ((y as f32 + 0.5) * ky - 0.5).clamp(0.0, (size - 1) as f32);
        let fx = ((x as f32 + 0.5) * kx - 0.5).clamp(0.0, (size - 1) as f32);
        let (gx0, gy0, tx, ty) = (fx as usize, fy as usize, fx.fract(), fy.fract());
        let (gx1, gy1) = ((gx0 + 1).min(size - 1), (gy0 + 1).min(size - 1));
        let at = |gx: usize, gy: usize, c: usize| lin[(gy * size + gx) * 3 + c];
        std::array::from_fn(|c| {
            let top = at(gx0, gy0, c) + (at(gx1, gy0, c) - at(gx0, gy0, c)) * tx;
            let bot = at(gx0, gy1, c) + (at(gx1, gy1, c) - at(gx0, gy1, c)) * tx;
            top + (bot - top) * ty
        })
    };
    let photo = |x: usize, y: usize| img.px(x0 + x, y0 + y);
    // Each pixel less its 3×3 mean, and the pixel, for crop pixels whose
    // neighbourhood is all `pick` (every other row and column).
    let detail = |px: &(dyn Fn(usize, usize) -> [f32; 3] + Sync),
                  pick: &(dyn Fn(f32) -> bool + Sync)| {
        let rows: Vec<Vec<([f32; 3], [f32; 3])>> = (1..ch.saturating_sub(1))
            .into_par_iter()
            .step_by(2)
            .map(|y| {
                let mut out = Vec::new();
                for x in (1..cw.saturating_sub(1)).step_by(2) {
                    let near = (0..9).all(|k| pick(cov[(y + k / 3 - 1) * cw + x + k % 3 - 1]));
                    if !near {
                        continue;
                    }
                    let mut mean = [0f32; 3];
                    for k in 0..9 {
                        let p = px(x + k % 3 - 1, y + k / 3 - 1);
                        for c in 0..3 {
                            mean[c] += p[c] / 9.0;
                        }
                    }
                    let v = px(x, y);
                    out.push((v, std::array::from_fn(|c| v[c] - mean[c])));
                }
                out
            })
            .collect();
        rows.into_iter().flatten().collect::<Vec<_>>()
    };
    let around = detail(&photo, &|c| c <= 0.0);
    let inside = detail(&filled, &|c| c >= 1.0);
    // The photo's detail power against value, per channel (a 3×3 mean
    // takes away a ninth of it).
    let fit: [(f64, f64); 3] = std::array::from_fn(|c| {
        let n = around.len() as f64;
        if n < 16.0 {
            return (0.0, 0.0);
        }
        let (mut sv, mut sr, mut svv, mut svr) = (0.0, 0.0, 0.0, 0.0);
        for (v, r) in &around {
            let (v, r2) = (f64::from(v[c]), f64::from(r[c]).powi(2) * 9.0 / 8.0);
            sv += v;
            sr += r2;
            svv += v * v;
            svr += v * r2;
        }
        let (mv, mr) = (sv / n, sr / n);
        let var = svv / n - mv * mv;
        let b = if var > 1e-12 {
            ((svr / n - mv * mr) / var).max(0.0)
        } else {
            0.0
        };
        (mr - b * mv, b)
    });
    let want = |c: usize, v: f32| (fit[c].0 + fit[c].1 * f64::from(v)).max(0.0);
    // How much of it the fill lacks, as a fraction.
    let lack: [f32; 3] = std::array::from_fn(|c| {
        let (mut has, mut wants) = (0.0, 0.0);
        for (v, r) in &inside {
            has += f64::from(r[c]).powi(2) * 9.0 / 8.0;
            wants += want(c, v[c]);
        }
        if wants <= 0.0 {
            0.0
        } else {
            (1.0 - has / wants).clamp(0.0, 1.0) as f32
        }
    });
    // How alike the channels' detail is (demosaicing shares it), so the
    // noise added is as alike.
    let shared = {
        let (mut num, mut den) = (0.0, 0.0);
        for (c, d) in [(0, 1), (1, 2), (0, 2)] {
            let (mut xy, mut xx, mut yy) = (0.0, 0.0, 0.0);
            for (_, r) in &around {
                let (a, b) = (f64::from(r[c]), f64::from(r[d]));
                xy += a * b;
                xx += a * a;
                yy += b * b;
            }
            if xx > 0.0 && yy > 0.0 {
                num += xy / (xx * yy).sqrt();
                den += 1.0;
            }
        }
        if den > 0.0 {
            (num / den).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    };
    let (ks, kc) = (shared.sqrt(), (1.0 - shared).sqrt());
    let width = img.width;
    img.data
        .par_chunks_mut(width * 3)
        .skip(y0)
        .take(ch)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..cw {
                let a = cov[y * cw + x] * opacity;
                if a <= 0.0 {
                    continue;
                }
                let v = filled(x, y);
                let common = gauss(x0 + x, y0 + y, 3);
                let o = &mut row[(x0 + x) * 3..(x0 + x) * 3 + 3];
                for c in 0..3 {
                    let n = ks * common + kc * gauss(x0 + x, y0 + y, c);
                    let n = n * (lack[c] * want(c, v[c]) as f32).sqrt();
                    o[c] += (v[c] + n - o[c]) * a;
                }
            }
        });
}

/// A standard normal-ish number for a pixel and channel (the sum of three
/// hashed uniforms, scaled to unit variance).
fn gauss(x: usize, y: usize, c: usize) -> f32 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (c as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    let mut sum = 0.0;
    for _ in 0..3 {
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 33;
        sum += (h >> 40) as f32 / (1u64 << 24) as f32;
    }
    (sum - 1.5) * 2.0
}

/// Where a removal is grabbed: the middle of its painting strokes.
pub fn middle(strokes: &[Stroke]) -> Option<[f64; 2]> {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in strokes.iter().filter(|s| !s.erase).flat_map(|s| &s.points) {
        for c in 0..2 {
            lo[c] = lo[c].min(p[c]);
            hi[c] = hi[c].max(p[c]);
        }
    }
    (lo[0] <= hi[0]).then(|| [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(points: Vec<[f64; 2]>, radius: f64, erase: bool) -> Stroke {
        Stroke {
            points,
            radius,
            feather: 0.0,
            flow: 1.0,
            erase,
        }
    }

    #[test]
    fn coverage_follows_strokes_and_erasing() {
        // A 100 px image; a horizontal stroke 5 px wide at y = 50.
        let s = [stroke(vec![[0.2, 0.5], [0.8, 0.5]], 0.05, false)];
        let cov = coverage(&s, 0.0, 100.0, 0.0, [0.0, 0.0], [1.0, 1.0], 100, 100);
        let at = |x: usize, y: usize| cov[y * 100 + x];
        assert_eq!(at(50, 50), 1.0);
        assert_eq!(at(50, 53), 1.0, "within the radius");
        assert_eq!(at(50, 57), 0.0, "beyond it (feather at least a pixel)");
        assert_eq!(at(10, 50), 0.0, "past the end's cap");
        // An erasing stroke across takes its part away.
        let e = [
            s[0].clone(),
            stroke(vec![[0.5, 0.3], [0.5, 0.7]], 0.03, true),
        ];
        let cov = coverage(&e, 0.0, 100.0, 0.0, [0.0, 0.0], [1.0, 1.0], 100, 100);
        assert_eq!(cov[50 * 100 + 50], 0.0);
        assert_eq!(cov[50 * 100 + 30], 1.0);
        // Feathered: soft beyond the radius.
        let cov = coverage(&s, 1.0, 100.0, 0.0, [0.0, 0.0], [1.0, 1.0], 100, 100);
        let v = cov[57 * 100 + 50];
        assert!(v > 0.0 && v < 1.0, "{v}");
    }

    #[test]
    fn crops_hold_their_strokes_and_stay_inside() {
        let s = [stroke(vec![[0.5, 0.3], [0.52, 0.32]], 0.01, false)];
        // At least MIN_SIDE across, square, around the strokes.
        let [x, y, w, h] = crop(&s, 0.0, 1000, 600).unwrap_or_default();
        assert_eq!((w, h), (200, 200));
        assert!(x < 500 && x + w > 520 && y < 300 && y + h > 320);
        // The same part of the image at another size.
        let [x2, y2, w2, h2] = crop(&s, 0.0, 2000, 1200).unwrap_or_default();
        assert_eq!((w2, h2), (400, 400));
        assert!(x2.abs_diff(2 * x) <= 1 && y2.abs_diff(2 * y) <= 1);
        // Near an edge: shifted in.
        let e = [stroke(vec![[0.01, 0.01]], 0.01, false)];
        assert_eq!(crop(&e, 0.0, 1000, 600), Some([0, 0, 200, 200]));
        // Larger than the image: all of it.
        let big = [stroke(vec![[0.1, 0.1], [0.9, 0.5]], 0.05, false)];
        assert_eq!(crop(&big, 0.0, 1000, 600), Some([0, 0, 1000, 600]));
        // Only erasing: nothing.
        let none = [stroke(vec![[0.5, 0.5]], 0.05, true)];
        assert_eq!(crop(&none, 0.0, 1000, 600), None);
    }

    #[test]
    fn bad_strokes_are_dropped() {
        let spot = Spot {
            strokes: vec![
                stroke(vec![[f64::NAN, 0.5], [0.5, 0.5]], 0.01, false),
                stroke(vec![[0.5, 0.5]], f64::INFINITY, false),
                stroke(vec![[0.5, 0.5]], 9.0, false),
            ],
            ..Default::default()
        };
        let s = strokes(&spot);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].points, vec![[0.5, 0.5]]);
        assert_eq!(s[1].radius, super::super::MAX_RADIUS);
    }

    #[test]
    fn strokes_cost_is_bounded() {
        // Close points are dropped, as the brush spaces them.
        let fine = (0..100).map(|i| [0.5 + i as f64 * 1e-4, 0.5]).collect();
        let s = strokes(&Spot {
            strokes: vec![stroke(fine, 0.01, false)],
            ..Default::default()
        });
        assert_eq!(s[0].points.len(), 4);
        // Big strokes zigzagging across the picture stop at the budget.
        let zigzag = (0..MAX_POINTS)
            .map(|i| [(i % 2) as f64, 0.5])
            .collect::<Vec<_>>();
        let s = strokes(&Spot {
            strokes: vec![stroke(zigzag.clone(), 9.0, false); 3],
            ..Default::default()
        });
        let r = super::super::MAX_RADIUS;
        let segment = (1.0 + 4.0 * r) * (4.0 * r);
        let n: usize = s.iter().map(|s| s.points.len()).sum();
        assert!(n as f64 * segment <= MAX_AREA + segment, "{n} points");
        assert!(n > 1);
    }
}
