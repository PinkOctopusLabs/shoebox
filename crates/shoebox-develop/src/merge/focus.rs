//! Focus stacking (DECISIONS, "Focus stacking"): raw photos of one scene
//! focused at different distances, combined into one image sharp from
//! front to back, in the reference frame's pixels.
//!
//! - **Reference:** the middle frame of those given (capture order): its
//!   colour and EXIF become the result's, and the chains to it are
//!   shortest.
//! - **Alignment:** each frame to its neighbour towards the reference (the
//!   two differ least in focus, so share the most detail; frames far apart
//!   may share almost none), a similarity (focus breathing changes the
//!   scale), the transforms composed along the chain.
//! - **Brightness:** small differences divided out by the gain measured
//!   against the reference (the median ratio of block means, which blur
//!   keeps).
//! - **Combining,** streamed: one frame is loaded, aligned, warped and
//!   added at a time, so memory doesn't grow with their number.
//!   - [`Method::Pyramid`] (the default): each frame's Laplacian pyramid;
//!     at each level each coefficient weighted by the local energy of its
//!     luminance against the noise there, steeply (`(1 + max(E/noise −
//!     2.5, 0))^8`, a soft maximum: where one frame is clearly sharper it
//!     alone counts, as in Zerene's PMax; where none is, they average, so
//!     noise falls rather than rises); the coarsest level averaged. Good with overlapping
//!     edges and fine detail. Each pixel is then held within the range the
//!     frames saw there (halo control).
//!   - [`Method::DepthMap`]: each pixel a soft choice of whole frames by
//!     their fine-detail energy (smoothed) against noise, with a wider look
//!     so flat areas follow the edges near them: where one frame is
//!     clearly sharpest the result is its own pixels (truest colour and
//!     contrast), blended across the boundaries.
//! - **Edges:** the result is the part of the reference every frame covers
//!   (focus breathing leaves some frames short of its edges).

// Channel loops read clearer indexed than zipped.
#![allow(clippy::needless_range_loop)]

use std::sync::atomic::AtomicBool;

use rayon::prelude::*;

use super::align::{self, Gray, Model};
use super::{Input, MergeError, Merged, Result, Source, check, load};
use crate::colour::{self, M3};
use crate::cpu::Rgb;

/// How the frames are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    #[default]
    Pyramid,
    DepthMap,
}

impl Method {
    pub const ALL: [Method; 2] = [Method::Pyramid, Method::DepthMap];

    pub fn label(self) -> &'static str {
        match self {
            Method::Pyramid => "Pyramid",
            Method::DepthMap => "Depth map",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Method::Pyramid => {
                "Detail by detail from the sharpest frame: best with overlapping edges \
                 (hair, bristles, branches); contrast and colour can shift a little"
            }
            Method::DepthMap => {
                "Each part from the frame sharpest there: truest colour and contrast, \
                 best for smooth surfaces; edges between near and far can show halos"
            }
        }
    }
}

/// Focus stacking options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub method: Method,
    /// Align the frames (focus breathing, hand-held); off for a rail or
    /// tripod whose frames don't move or change scale.
    pub align: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            method: Method::Pyramid,
            align: true,
        }
    }
}

/// What happened to one frame.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// Reference → this frame (full-size pixels), composed along the
    /// chain; the identity for the reference, or without alignment.
    pub h: M3,
    /// Its alignment to its neighbour towards the reference.
    pub alignment: Option<align::Alignment>,
    /// Why alignment fell back to its neighbour's, if it did.
    pub align_failed: Option<&'static str>,
    /// Its brightness relative to the reference's (measured; divided out).
    pub gain: f64,
}

/// The stack and what happened to each input (in input order).
pub struct Focus {
    pub merged: Merged,
    pub frames: Vec<Frame>,
    /// The part of the reference frame the result is: x, y, width, height.
    pub crop: [usize; 4],
    /// The most full-size frames held at once (two: one loaded, one
    /// warped), whatever their number.
    pub peak_frames: usize,
}

/// At least this many frames (no most: they're streamed).
pub const MIN_FRAMES: usize = 2;

/// The magnification of a transform: the square root of its linear part's
/// determinant.
pub fn scale(h: &M3) -> f64 {
    (h[0][0] * h[1][1] - h[0][1] * h[1][0]).abs().sqrt()
}

/// How steeply sharper wins: weights go as `(1 + energy / noise −
/// FLOOR)^SHARP`.
const SHARP: f32 = 8.0;
/// Energy up to this many times the noise's counts as none: where no frame
/// has more, they average evenly (noise falls by their number).
const FLOOR: f32 = 2.5;
/// The depth map's wider look (flat areas follow the edges near them).
const NEAR: f32 = 4.0;

#[inline]
fn luma(p: &[f32]) -> f32 {
    0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2]
}

/// Luminance for alignment; NaN where clipped or near black. Linear, not
/// square-rooted as HDR's: the frames share an exposure, and a curve would
/// make blurred texture darker on average than sharp (Jensen), a
/// difference refinement partly explains by moving (tests: a third of a
/// pixel's bias at the far frames, halved).
fn gray(rgb: &Rgb, clipped: &[u8]) -> Gray {
    let mut g = Gray::new(rgb.width, rgb.height);
    g.data
        .par_iter_mut()
        .zip(rgb.data.par_chunks(3))
        .zip(clipped.par_iter())
        .for_each(|((o, p), &c)| {
            let y = luma(p);
            let hi = p[0].max(p[1]).max(p[2]);
            *o = if c != 0 || hi > 0.9 || y < 2e-3 {
                f32::NAN
            } else {
                y
            };
        });
    g
}

// ---------------------------------------------------------------------------
// Pyramids

/// Pixels of `c` channels, row by row.
struct Img {
    w: usize,
    h: usize,
    c: usize,
    data: Vec<f32>,
}

/// Burt and Adelson's 5-tap binomial kernel.
const K5: [f32; 5] = [0.0625, 0.25, 0.375, 0.25, 0.0625];

impl Img {
    /// Blurred by [`K5`] and halved (odd sizes round up).
    fn reduce(&self) -> Img {
        let (w, h, c) = (self.w, self.h, self.c);
        let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
        let mut tmp = vec![0.0f32; w2 * h * c];
        tmp.par_chunks_mut(w2 * c).enumerate().for_each(|(y, row)| {
            let src = &self.data[y * w * c..(y + 1) * w * c];
            for x in 0..w2 {
                for (i, k) in K5.iter().enumerate() {
                    let xx = (2 * x + i).saturating_sub(2).min(w - 1);
                    for ch in 0..c {
                        row[x * c + ch] += k * src[xx * c + ch];
                    }
                }
            }
        });
        let mut out = vec![0.0f32; w2 * h2 * c];
        out.par_chunks_mut(w2 * c).enumerate().for_each(|(y, row)| {
            for (i, k) in K5.iter().enumerate() {
                let yy = (2 * y + i).saturating_sub(2).min(h - 1);
                let src = &tmp[yy * w2 * c..(yy + 1) * w2 * c];
                for (o, s) in row.iter_mut().zip(src) {
                    *o += k * s;
                }
            }
        });
        Img {
            w: w2,
            h: h2,
            c,
            data: out,
        }
    }

    /// Doubled to `w`×`h` (the inverse of [`Img::reduce`]'s sampling: zeros
    /// between, filtered by twice [`K5`]).
    fn expand(&self, w: usize, h: usize) -> Img {
        let (sw, sh, c) = (self.w, self.h, self.c);
        // Each output position's two or three sources and weights.
        let taps = |n: usize, len: usize| -> Vec<[(usize, f32); 3]> {
            (0..n)
                .map(|x| {
                    let m = x / 2;
                    let at = |j: isize| j.clamp(0, len as isize - 1) as usize;
                    if x % 2 == 0 {
                        [
                            (at(m as isize - 1), 0.125),
                            (at(m as isize), 0.75),
                            (at(m as isize + 1), 0.125),
                        ]
                    } else {
                        [(at(m as isize), 0.5), (at(m as isize + 1), 0.5), (0, 0.0)]
                    }
                })
                .collect()
        };
        let (tx, ty) = (taps(w, sw), taps(h, sh));
        let mut tmp = vec![0.0f32; w * sh * c];
        tmp.par_chunks_mut(w * c).enumerate().for_each(|(y, row)| {
            let src = &self.data[y * sw * c..(y + 1) * sw * c];
            for x in 0..w {
                for &(j, k) in &tx[x] {
                    for ch in 0..c {
                        row[x * c + ch] += k * src[j * c + ch];
                    }
                }
            }
        });
        let mut out = vec![0.0f32; w * h * c];
        out.par_chunks_mut(w * c).enumerate().for_each(|(y, row)| {
            for &(j, k) in &ty[y] {
                if k == 0.0 {
                    continue;
                }
                let src = &tmp[j * w * c..(j + 1) * w * c];
                for (o, s) in row.iter_mut().zip(src) {
                    *o += k * s;
                }
            }
        });
        Img { w, h, c, data: out }
    }
}

/// One channel blurred by [`K5`] both ways.
fn blur5(data: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut tmp = vec![0.0f32; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &data[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            for (i, k) in K5.iter().enumerate() {
                *o += k * src[(x + i).saturating_sub(2).min(w - 1)];
            }
        }
    });
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (i, k) in K5.iter().enumerate() {
            let yy = (y + i).saturating_sub(2).min(h - 1);
            for (o, s) in row.iter_mut().zip(&tmp[yy * w..(yy + 1) * w]) {
                *o += k * s;
            }
        }
    });
    out
}

/// One channel box-blurred (radius `r`) three times both ways: about a
/// Gaussian of σ = √(r(r + 1)), whatever `r` costs the same.
fn smooth(data: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let along = |src: &[f32], dst: &mut [f32], n: usize| {
        // A running sum, edges clamped.
        let norm = 1.0 / (2 * r + 1) as f32;
        let at = |i: isize| src[i.clamp(0, n as isize - 1) as usize];
        let mut s: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for (i, d) in dst.iter_mut().enumerate().take(n) {
            *d = s * norm;
            let i = i as isize;
            s += at(i + r as isize + 1) - at(i - r as isize);
        }
    };
    // Three passes along rows, then (box blurs being separable) three
    // along columns, as rows of the transpose.
    let rows = |mut cur: Vec<f32>, w: usize| {
        let mut t = vec![0.0f32; cur.len()];
        for _ in 0..3 {
            t.par_chunks_mut(w)
                .zip(cur.par_chunks(w))
                .for_each(|(d, s)| along(s, d, w));
            std::mem::swap(&mut cur, &mut t);
        }
        cur
    };
    let across = rows(data.to_vec(), w);
    let down = rows(transpose(&across, w, h), h);
    transpose(&down, h, w)
}

/// A `w`×`h` channel transposed (`h`×`w`), in strips of 64 columns.
fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    const S: usize = 64;
    let mut out = vec![0.0f32; w * h];
    out.par_chunks_mut(S * h)
        .enumerate()
        .for_each(|(b, strip)| {
            let x0 = b * S;
            let n = strip.len() / h;
            for y in 0..h {
                let row = &src[y * w + x0..y * w + x0 + n];
                for (i, v) in row.iter().enumerate() {
                    strip[i * h + y] = *v;
                }
            }
        });
    out
}

/// A coverage mask (`w`×`h`) at the next level (`w2`×`h2`): covered where
/// all of the 5×5 it's reduced from is.
fn shrink(m: &[bool], w: usize, h: usize, w2: usize, h2: usize) -> Vec<bool> {
    let mut out = vec![false; w2 * h2];
    out.par_chunks_mut(w2).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            *o = (0..5).all(|j| {
                let yy = (2 * y + j).saturating_sub(2).min(h - 1);
                (0..5).all(|i| m[yy * w + (2 * x + i).saturating_sub(2).min(w - 1)])
            });
        }
    });
    out
}

/// Levels for a `w`×`h` image: halvings until the long edge is at most 64.
fn depth(w: usize, h: usize) -> usize {
    let (mut w, mut h, mut n) = (w, h, 0);
    while w.max(h) > 64 && w.min(h) > 8 {
        w = w.div_ceil(2);
        h = h.div_ceil(2);
        n += 1;
    }
    n.max(1)
}

/// The mean luminance band energy at each level of unit-variance white
/// noise in each channel (`levels` of them; deeper ones extrapolated).
fn white_gains(levels: usize) -> Vec<f32> {
    let n = 512;
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // Uniform on ±√3: variance 1.
        ((state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) as f32 * 3f32.sqrt()
    };
    let mut g = vec![Img {
        w: n,
        h: n,
        c: 3,
        data: (0..n * n * 3).map(|_| next()).collect(),
    }];
    let have = depth(n, n).min(levels);
    for l in 0..have {
        let r = g[l].reduce();
        g.push(r);
    }
    let mut k = Vec::with_capacity(levels);
    for l in 0..have {
        let up = g[l + 1].expand(g[l].w, g[l].h);
        let (w, h) = (g[l].w, g[l].h);
        let (mut s, mut count) = (0.0f64, 0usize);
        for y in 4..h - 4 {
            for x in 4..w - 4 {
                let i = (y * w + x) * 3;
                let d = [
                    g[l].data[i] - up.data[i],
                    g[l].data[i + 1] - up.data[i + 1],
                    g[l].data[i + 2] - up.data[i + 2],
                ];
                s += f64::from(luma(&d)).powi(2);
                count += 1;
            }
        }
        k.push((s / count.max(1) as f64) as f32);
    }
    while k.len() < levels {
        let ratio = match k.len() {
            0 | 1 => 0.25,
            n => k[n - 1] / k[n - 2].max(1e-12),
        };
        let last = k.last().copied().unwrap_or(0.1);
        k.push(last * ratio);
    }
    k
}

/// The median of `e / (a·v + b)` (the energy per unit of noise variance),
/// sampled to about a million pixels.
fn measure(e: &[f32], v: &[f32], (a, b): (f32, f32)) -> f32 {
    let step = (e.len() / 1_000_000).max(1);
    let mut r: Vec<f32> = (0..e.len())
        .into_par_iter()
        .step_by(step)
        .map(|i| e[i] / (a * v[i].max(0.0) + b))
        .filter(|r| r.is_finite())
        .collect();
    if r.is_empty() {
        return f32::MAX;
    }
    let mid = r.len() / 2;
    r.select_nth_unstable_by(mid, f32::total_cmp);
    r[mid]
}

// ---------------------------------------------------------------------------
// Combining

/// A running weighted mean whose weights are `exp(lw)`, kept relative to
/// the largest so far (a streamed soft maximum).
struct Soft {
    c: usize,
    num: Vec<f32>,
    den: Vec<f32>,
    top: Vec<f32>,
}

impl Soft {
    fn new(n: usize, c: usize) -> Self {
        Self {
            c,
            num: vec![0.0; n * c],
            den: vec![0.0; n],
            top: vec![f32::NEG_INFINITY; n],
        }
    }

    /// Add `v` (`c` a pixel) with log-weights `lw` (−∞: not at all).
    fn add(&mut self, v: &[f32], lw: &[f32]) {
        let c = self.c;
        self.num
            .par_chunks_mut(c)
            .zip(self.den.par_iter_mut())
            .zip(self.top.par_iter_mut())
            .zip(v.par_chunks(c))
            .zip(lw.par_iter())
            .for_each(|((((num, den), top), v), &l)| {
                if !l.is_finite() {
                    return;
                }
                if l > *top {
                    let s = (*top - l).exp();
                    num.iter_mut().for_each(|n| *n *= s);
                    *den *= s;
                    *top = l;
                }
                let w = (l - *top).exp();
                for ch in 0..c {
                    num[ch] += w * v[ch];
                }
                *den += w;
            });
    }

    fn mean(&self) -> Vec<f32> {
        let c = self.c;
        let mut out = vec![0.0f32; self.num.len()];
        out.par_chunks_mut(c)
            .zip(self.num.par_chunks(c))
            .zip(self.den.par_iter())
            .for_each(|((o, n), &d)| {
                if d > 0.0 {
                    for ch in 0..c {
                        o[ch] = n[ch] / d;
                    }
                }
            });
        out
    }
}

/// The frames combined so far.
struct Fuser {
    method: Method,
    w: usize,
    h: usize,
    /// The noise model's variance `a·v + b` of a value `v`.
    noise: (f32, f32),
    /// Each level's size (the pyramid's, then its top; the depth map's
    /// one).
    dims: Vec<(usize, usize)>,
    /// Each level's band energy per unit of noise variance: white noise's,
    /// or less if the reference shows less (demosaic smooths noise).
    white: Vec<f32>,
    scales: Option<Vec<f32>>,
    levels: Vec<Soft>,
    /// The pyramid's top: a plain mean.
    top: Vec<f32>,
    top_n: Vec<f32>,
    /// The least and most each pixel's channels were in any frame.
    lo: Vec<f32>,
    hi: Vec<f32>,
}

impl Fuser {
    fn new(method: Method, w: usize, h: usize, noise: (f32, f32)) -> Self {
        let n = match method {
            Method::Pyramid => depth(w, h),
            Method::DepthMap => 1,
        };
        let mut dims = vec![(w, h)];
        for _ in 0..n {
            let (a, b) = dims[dims.len() - 1];
            dims.push((a.div_ceil(2), b.div_ceil(2)));
        }
        let (levels, top, lo) = match method {
            Method::Pyramid => {
                let (tw, th) = dims[n];
                (
                    (0..n)
                        .map(|l| Soft::new(dims[l].0 * dims[l].1, 3))
                        .collect(),
                    (vec![0.0; tw * th * 3], vec![0.0; tw * th]),
                    (
                        vec![f32::INFINITY; w * h * 3],
                        vec![f32::NEG_INFINITY; w * h * 3],
                    ),
                )
            }
            Method::DepthMap => (
                vec![Soft::new(w * h, 3)],
                (Vec::new(), Vec::new()),
                (Vec::new(), Vec::new()),
            ),
        };
        Self {
            method,
            w,
            h,
            noise,
            white: white_gains(n),
            dims,
            scales: None,
            levels,
            top: top.0,
            top_n: top.1,
            lo: lo.0,
            hi: lo.1,
        }
    }

    /// The noise scale of level `l`, measured from the first frame (the
    /// reference) if this is it.
    fn scale(&self, l: usize, e: &[f32], v: &[f32], new: &mut Vec<f32>) -> f32 {
        match &self.scales {
            Some(s) => s[l],
            None => {
                let k = self.white[l];
                let s = measure(e, v, self.noise).min(k).max(0.01 * k);
                new.push(s);
                s
            }
        }
    }

    /// Log-weights from energies against noise (−∞ where not covered).
    fn weights(&self, e: &[(&[f32], f32)], v: &[f32], s: f32, cov: Option<&[bool]>) -> Vec<f32> {
        let (a, b) = self.noise;
        (0..v.len())
            .into_par_iter()
            .map(|i| {
                if cov.is_some_and(|m| !m[i]) {
                    return f32::NEG_INFINITY;
                }
                let n2 = s * (a * v[i].max(0.0) + b);
                e.iter()
                    .map(|(e, p)| p * (e[i] / n2 - FLOOR).max(0.0).ln_1p())
                    .sum()
            })
            .collect()
    }

    /// Add a frame (warped into the reference; `inside`: where it covers
    /// it, None: everywhere).
    fn add(&mut self, rgb: Rgb, inside: Option<Vec<bool>>) {
        let (w, h) = (self.w, self.h);
        let mut fresh = Vec::new();
        match self.method {
            Method::Pyramid => {
                self.lo
                    .par_chunks_mut(3)
                    .zip(self.hi.par_chunks_mut(3))
                    .zip(rgb.data.par_chunks(3))
                    .enumerate()
                    .for_each(|(i, ((lo, hi), p))| {
                        if inside.as_ref().is_some_and(|m| !m[i]) {
                            return;
                        }
                        for c in 0..3 {
                            lo[c] = lo[c].min(p[c]);
                            hi[c] = hi[c].max(p[c]);
                        }
                    });
                let n = self.levels.len();
                let mut g = vec![Img {
                    w,
                    h,
                    c: 3,
                    data: rgb.data,
                }];
                for l in 0..n {
                    let r = g[l].reduce();
                    g.push(r);
                }
                let mut cov = inside;
                for l in 0..n {
                    let (lw, lh) = self.dims[l];
                    let mut band = g[l + 1].expand(lw, lh);
                    let v: Vec<f32> = band.data.par_chunks(3).map(luma).collect();
                    band.data
                        .par_iter_mut()
                        .zip(&g[l].data)
                        .for_each(|(u, g)| *u = g - *u);
                    let y2: Vec<f32> = band.data.par_chunks(3).map(|p| luma(p) * luma(p)).collect();
                    let e = blur5(&y2, lw, lh);
                    drop(y2);
                    let s = self.scale(l, &e, &v, &mut fresh);
                    let lwts = self.weights(&[(&e, SHARP)], &v, s, cov.as_deref());
                    self.levels[l].add(&band.data, &lwts);
                    let (nw, nh) = self.dims[l + 1];
                    cov = cov.map(|m| shrink(&m, lw, lh, nw, nh));
                    // Done with this level's pixels.
                    g[l].data = Vec::new();
                }
                let t = &g[n];
                self.top
                    .par_chunks_mut(3)
                    .zip(self.top_n.par_iter_mut())
                    .zip(t.data.par_chunks(3))
                    .enumerate()
                    .for_each(|(i, ((s, k), p))| {
                        if cov.as_ref().is_some_and(|m| !m[i]) {
                            return;
                        }
                        for c in 0..3 {
                            s[c] += p[c];
                        }
                        *k += 1.0;
                    });
            }
            Method::DepthMap => {
                let g0 = Img {
                    w,
                    h,
                    c: 3,
                    data: rgb.data,
                };
                let up = g0.reduce().expand(w, h);
                let v: Vec<f32> = up.data.par_chunks(3).map(luma).collect();
                let y2: Vec<f32> = g0
                    .data
                    .par_chunks(3)
                    .zip(up.data.par_chunks(3))
                    .map(|(p, q)| {
                        let d = luma(p) - luma(q);
                        d * d
                    })
                    .collect();
                drop(up);
                let r = (w.max(h) / 1000).max(2);
                let fine = smooth(&y2, w, h, r);
                let wide = smooth(&y2, w, h, 4 * r);
                drop(y2);
                let s = self.scale(0, &fine, &v, &mut fresh);
                let lwts = self.weights(&[(&fine, SHARP), (&wide, NEAR)], &v, s, inside.as_deref());
                self.levels[0].add(&g0.data, &lwts);
            }
        }
        if self.scales.is_none() {
            self.scales = Some(fresh);
        }
    }

    /// The result, full size.
    fn finish(mut self) -> Rgb {
        let (w, h) = (self.w, self.h);
        match self.method {
            Method::Pyramid => {
                let n = self.levels.len();
                let (tw, th) = self.dims[n];
                let mut data = vec![0.0f32; tw * th * 3];
                data.par_chunks_mut(3)
                    .zip(self.top.par_chunks(3))
                    .zip(self.top_n.par_iter())
                    .for_each(|((o, s), &k)| {
                        if k > 0.0 {
                            for c in 0..3 {
                                o[c] = s[c] / k;
                            }
                        }
                    });
                let mut g = Img {
                    w: tw,
                    h: th,
                    c: 3,
                    data,
                };
                for l in (0..n).rev() {
                    let (lw, lh) = self.dims[l];
                    let mut up = g.expand(lw, lh);
                    let Some(soft) = self.levels.pop() else {
                        break;
                    };
                    let band = soft.mean();
                    drop(soft);
                    up.data.par_iter_mut().zip(&band).for_each(|(u, b)| *u += b);
                    g = up;
                }
                // Halo control: nothing beyond what some frame saw there.
                g.data
                    .par_iter_mut()
                    .zip(self.lo.par_iter())
                    .zip(self.hi.par_iter())
                    .for_each(|((v, &lo), &hi)| {
                        if lo <= hi {
                            *v = v.clamp(lo, hi);
                        }
                    });
                Rgb {
                    width: w,
                    height: h,
                    data: g.data,
                }
            }
            Method::DepthMap => Rgb {
                width: w,
                height: h,
                data: self.levels[0].mean(),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Frames

/// Pixels outside a warped frame (`inside` false) given the nearest inside
/// value along their row, then rows with none the nearest row's: so the
/// pyramid sees no false edges where the frame ends.
fn fill(rgb: &mut Rgb, inside: &[bool]) {
    let w = rgb.width;
    let has: Vec<bool> = rgb
        .data
        .par_chunks_mut(w * 3)
        .zip(inside.par_chunks(w))
        .map(|(row, m)| {
            let (Some(a), Some(b)) = (m.iter().position(|v| *v), m.iter().rposition(|v| *v)) else {
                return false;
            };
            for x in 0..w {
                if !m[x] {
                    let from = if x < a {
                        a
                    } else if x > b {
                        b
                    } else {
                        continue;
                    };
                    for c in 0..3 {
                        row[x * 3 + c] = row[from * 3 + c];
                    }
                }
            }
            true
        })
        .collect();
    let rows: Vec<usize> = (0..has.len()).filter(|&y| has[y]).collect();
    if rows.is_empty() {
        return;
    }
    for y in 0..has.len() {
        if has[y] {
            continue;
        }
        let from = rows
            .iter()
            .copied()
            .min_by_key(|&r| r.abs_diff(y))
            .unwrap_or(y);
        rgb.data
            .copy_within(from * w * 3..(from + 1) * w * 3, y * w * 3);
    }
}

/// Mean luminance in `b`×`b` blocks (whole ones), NaN where a block isn't
/// all inside, is near black or nears white.
fn blocks(rgb: &Rgb, inside: Option<&[bool]>, b: usize) -> Vec<f32> {
    let (w, h) = (rgb.width, rgb.height);
    let (bw, bh) = (w / b, h / b);
    (0..bw * bh)
        .into_par_iter()
        .map(|i| {
            let (x0, y0) = (i % bw * b, i / bw * b);
            let (mut s, mut hi) = (0.0f32, 0.0f32);
            for y in y0..y0 + b {
                for x in x0..x0 + b {
                    let j = y * w + x;
                    if inside.is_some_and(|m| !m[j]) {
                        return f32::NAN;
                    }
                    let p = &rgb.data[j * 3..j * 3 + 3];
                    s += luma(p);
                    hi = hi.max(p[0].max(p[1]).max(p[2]));
                }
            }
            let mean = s / (b * b) as f32;
            if mean < 0.01 || hi > 0.85 {
                f32::NAN
            } else {
                mean
            }
        })
        .collect()
}

/// The median ratio of a frame's block means to the reference's (None
/// with fewer than 16 blocks to go on).
fn gain(reference: &[f32], frame: &[f32]) -> Option<f64> {
    let mut r: Vec<f32> = reference
        .iter()
        .zip(frame)
        .filter(|(a, b)| a.is_finite() && b.is_finite())
        .map(|(a, b)| b / a)
        .collect();
    if r.len() < 16 {
        return None;
    }
    let mid = r.len() / 2;
    r.select_nth_unstable_by(mid, f32::total_cmp);
    let g = f64::from(r[mid]);
    (g.is_finite() && g > 0.0).then_some(g)
}

/// The part of a `w`×`h` reference every frame covers (`hs`: reference →
/// frame): x, y, width, height. Each frame's corners mapped back; inside
/// its quadrilateral (turned a little at most), the box between its inner
/// edges.
fn coverage(hs: &[M3], w: usize, h: usize) -> [usize; 4] {
    let (wf, hf) = ((w - 1) as f64, (h - 1) as f64);
    let (mut l, mut t, mut r, mut b) = (0.0f64, 0.0f64, wf, hf);
    for m in hs {
        let Some(inv) = colour::inv(m) else {
            continue;
        };
        let c = |x: f64, y: f64| align::apply(&inv, x, y);
        let (tl, tr, bl, br) = (c(0.0, 0.0), c(wf, 0.0), c(0.0, hf), c(wf, hf));
        l = l.max(tl.0.max(bl.0));
        r = r.min(tr.0.min(br.0));
        t = t.max(tl.1.max(tr.1));
        b = b.min(bl.1.min(br.1));
    }
    let (x0, y0) = (l.ceil().max(0.0) as usize, t.ceil().max(0.0) as usize);
    let (x1, y1) = (r.floor().max(0.0) as usize, b.floor().max(0.0) as usize);
    [
        x0,
        y0,
        (x1 + 1).saturating_sub(x0).min(w),
        (y1 + 1).saturating_sub(y0).min(h),
    ]
}

fn cut(rgb: Rgb, [x, y, w, h]: [usize; 4]) -> Rgb {
    if (x, y, w, h) == (0, 0, rgb.width, rgb.height) {
        return rgb;
    }
    let mut out = Rgb::new(w, h);
    out.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(j, row)| {
            let s = ((y + j) * rgb.width + x) * 3;
            row.copy_from_slice(&rgb.data[s..s + w * 3]);
        });
    out
}

/// Full-size frames in memory now and at most (the streaming check).
#[derive(Default)]
struct Held {
    now: usize,
    peak: usize,
}

impl Held {
    fn take(&mut self) {
        self.now += 1;
        self.peak = self.peak.max(self.now);
    }

    fn give(&mut self) {
        self.now = self.now.saturating_sub(1);
    }
}

/// Stack photos focused at different distances (see the module's notes).
/// `progress` hears 0–1 as frames are done; `cancel` stops it between
/// steps.
pub fn merge(
    inputs: &[Input],
    o: &Options,
    progress: &(dyn Fn(f32) + Sync),
    cancel: &AtomicBool,
) -> Result<Focus> {
    if inputs.len() < MIN_FRAMES {
        return Err(MergeError::Refused(format!(
            "a focus stack takes at least {MIN_FRAMES} photos"
        )));
    }
    let n = inputs.len();
    let reference = (n - 1) / 2;
    // The reference, then outwards: each frame after its neighbour.
    let order: Vec<usize> = std::iter::once(reference)
        .chain(reference + 1..n)
        .chain((0..reference).rev())
        .collect();
    let mut held = Held::default();
    progress(0.0);
    let r = load(&inputs[reference], cancel)?;
    held.take();
    let (w, h) = (r.width(), r.height());
    let noise = super::hdr::noise(r.exposure.iso.unwrap_or(100.0));
    let block = (w.max(h) / 96).clamp(8, 64);
    let ref_blocks = blocks(&r.rgb, None, block);
    let ref_pyramid = o
        .align
        .then(|| align::pyramid(gray(&r.rgb, &r.clipped), 64));
    let Source {
        rgb,
        mut meta,
        as_shot,
        metadata,
        ..
    } = r;
    let mut fuse = Fuser::new(o.method, w, h, noise);
    fuse.add(rgb, None);
    held.give();
    let mut frames: Vec<Option<Frame>> = vec![None; n];
    frames[reference] = Some(Frame {
        h: colour::IDENTITY,
        alignment: None,
        align_failed: None,
        gain: 1.0,
    });
    progress(1.0 / n as f32);
    // The last frame done on this side of the reference: its pyramid and
    // transform (None: the reference's).
    let mut prev: Option<(Vec<Gray>, M3)> = None;
    for (done, &k) in order.iter().enumerate().skip(1) {
        check(cancel)?;
        if k + 1 == reference {
            prev = None;
        }
        let s = load(&inputs[k], cancel)?;
        held.take();
        if (s.width(), s.height()) != (w, h) {
            return Err(MergeError::Refused(format!(
                "{} is {}×{}, the others {}×{}: a focus stack's photos are the same size",
                super::file_name(&s.path),
                s.width(),
                s.height(),
                w,
                h
            )));
        }
        if (s.meta.make.as_str(), s.meta.model.as_str())
            != (meta.make.as_str(), meta.model.as_str())
        {
            return Err(MergeError::Refused(format!(
                "{} is from another camera",
                super::file_name(&s.path)
            )));
        }
        let (mut hk, mut alignment, mut align_failed) = (colour::IDENTITY, None, None);
        if let Some(rp) = &ref_pyramid {
            let (np, nh) = match &prev {
                Some((p, m)) => (p, *m),
                None => (rp, colour::IDENTITY),
            };
            let pk = align::pyramid(gray(&s.rgb, &s.clipped), 64);
            let ao = align::Options {
                model: Model::Similarity,
                ..Default::default()
            };
            match align::align(np, &pk, &ao) {
                Ok(a) => {
                    hk = colour::mul(&a.h, &nh);
                    alignment = Some(a);
                }
                Err(e) => {
                    tracing::warn!("focus: {}: {e}", super::file_name(&s.path));
                    align_failed = Some("couldn't be aligned");
                    hk = nh;
                }
            }
            prev = Some((pk, hk));
        }
        check(cancel)?;
        let Source { rgb: src, .. } = s;
        let moved = align::distance(&hk, &colour::IDENTITY, w, h) > 0.01;
        let (mut rgb, inside) = if moved {
            let (warped, inside) = align::warp(&src, &hk, w, h);
            held.take();
            drop(src);
            held.give();
            (warped, Some(inside))
        } else {
            (src, None)
        };
        if let Some(m) = &inside {
            fill(&mut rgb, m);
        }
        let g = gain(&ref_blocks, &blocks(&rgb, inside.as_deref(), block)).unwrap_or(1.0);
        if (g - 1.0).abs() > 1e-4 {
            if g.log2().abs() > 0.1 {
                tracing::info!(
                    "focus: {} is {:+.2} EV from the reference",
                    super::file_name(&inputs[k].path),
                    g.log2()
                );
            }
            let inv = (1.0 / g) as f32;
            rgb.data.par_iter_mut().for_each(|v| *v *= inv);
        }
        fuse.add(rgb, inside);
        held.give();
        frames[k] = Some(Frame {
            h: hk,
            alignment,
            align_failed,
            gain: g,
        });
        progress((done + 1) as f32 / n as f32);
    }
    check(cancel)?;
    let full = fuse.finish();
    let frames = frames
        .into_iter()
        .map(|f| f.ok_or_else(|| MergeError::Failed("a frame was skipped".into())))
        .collect::<Result<Vec<_>>>()?;
    let hs: Vec<M3> = frames.iter().map(|f| f.h).collect();
    let crop = coverage(&hs, w, h);
    if crop[2] < w / 2 || crop[3] < h / 2 {
        return Err(MergeError::Failed(
            "the frames moved too far apart to stack".into(),
        ));
    }
    let rgb = cut(full, crop);
    meta.width = rgb.width;
    meta.height = rgb.height;
    progress(1.0);
    Ok(Focus {
        merged: Merged {
            rgb,
            reference,
            meta,
            as_shot,
            metadata,
        },
        frames,
        crop,
        peak_frames: held.peak,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `w`×`h` grey frame from `f(x, y)`, plus noise of `sigma`.
    fn frame(w: usize, h: usize, seed: u64, sigma: f32, f: impl Fn(usize, usize) -> f32) -> Rgb {
        let mut s = seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) as f32 * 3f32.sqrt()
        };
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = f(x, y);
                for c in 0..3 {
                    rgb.data[(y * w + x) * 3 + c] = v + sigma * next();
                }
            }
        }
        rgb
    }

    fn sd(v: &[f32]) -> f32 {
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt()
    }

    #[test]
    fn one_frame_comes_back_as_it_was() {
        let (w, h) = (203, 141);
        let f = frame(w, h, 3, 0.01, |x, y| {
            0.2 + 0.1 * ((x as f32 * 0.7).sin() * (y as f32 * 0.3).cos())
        });
        for method in Method::ALL {
            let mut fuse = Fuser::new(method, w, h, (8e-5, 1.6e-8));
            fuse.add(f.clone(), None);
            let out = fuse.finish();
            let most = out
                .data
                .iter()
                .zip(&f.data)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(most < 1e-5, "{method:?}: {most}");
        }
    }

    #[test]
    fn where_no_frame_is_sharper_noise_averages_away() {
        // Eight frames of a flat grey with independent noise: no frame is
        // sharper anywhere, so they average (PMax would keep the
        // noisiest).
        let (w, h) = (160, 120);
        let sigma = (8e-5f32 * 0.2).sqrt();
        for method in Method::ALL {
            let mut fuse = Fuser::new(method, w, h, (8e-5, 1.6e-8));
            for k in 0..8 {
                fuse.add(frame(w, h, k + 1, sigma, |_, _| 0.2), None);
            }
            let out = fuse.finish();
            let g: Vec<f32> = out.data.iter().skip(1).step_by(3).copied().collect();
            let ratio = sd(&g) / sigma;
            assert!(ratio < 0.45, "{method:?}: noise {ratio:.2} of a frame's");
        }
    }

    #[test]
    fn smoothing_spreads_evenly_both_ways() {
        let (w, h) = (130, 70);
        let mut d = vec![0.0f32; w * h];
        d[35 * w + 60] = 1.0;
        let t = transpose(&d, w, h);
        assert_eq!(t[60 * h + 35], 1.0);
        assert_eq!(transpose(&t, h, w), d);
        let s = smooth(&d, w, h, 3);
        assert!((s.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        for k in 1..8 {
            let (a, b) = (s[35 * w + 60 + k], s[35 * w + 60 - k]);
            let (c, e) = (s[(35 + k) * w + 60], s[(35 - k) * w + 60]);
            assert!((a - b).abs() < 1e-6 && (a - c).abs() < 1e-6 && (c - e).abs() < 1e-6);
        }
    }

    #[test]
    fn coverage_is_what_every_frame_sees() {
        // A frame magnified 1% about the centre covers all but the edges.
        let (w, h) = (200, 100);
        let (cx, cy) = (99.5, 49.5);
        let s = 1.01;
        let m: M3 = [
            [s, 0.0, cx * (1.0 - s)],
            [0.0, s, cy * (1.0 - s)],
            [0.0, 0.0, 1.0],
        ];
        assert_eq!(coverage(&[colour::IDENTITY], w, h), [0, 0, 200, 100]);
        let [x, y, cw, ch] = coverage(&[colour::IDENTITY, m], w, h);
        assert_eq!((x, y), (1, 1));
        assert_eq!((cw, ch), (198, 98));
    }
}
