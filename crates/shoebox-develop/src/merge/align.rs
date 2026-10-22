//! Global alignment of overlapping photos (DECISIONS, "Photo merges:
//! groundwork"): one transform that maps a reference photo's pixels onto
//! another's.
//!
//! Two stages, as Hugin's `align_image_stack` and OpenCV's ECC do them:
//!
//! 1. **Features** ([`features`], [`matches`], [`ransac`]): Harris corners,
//!    spread over a grid, each described by an 8×8 patch sampled from a
//!    blurred copy and normalised for gain and offset (Brown, Szeliski and
//!    Winder's MOPS, without the orientation: frames may turn by a few
//!    degrees, not more). Nearest neighbours pass Lowe's ratio test both
//!    ways; RANSAC fits the model. Panorama matches overlapping frames this
//!    way.
//! 2. **Direct refinement** ([`refine`]): Gauss–Newton on the pixels
//!    themselves, coarse to fine, over the strongest-gradient pixels of the
//!    reference, with a gain and offset for what exposure normalising left
//!    and Huber weights for what moved. It takes the features' estimate
//!    (good to a pixel or so) to hundredths of a pixel.
//!
//! Images are [`Gray`]: one channel, prepared by the caller (an HDR merge
//! normalises exposure and encodes it); NaN marks a pixel to leave out
//! (clipped, lost in noise). Transforms are 3×3 matrices mapping reference
//! pixel coordinates (pixel centres at whole numbers) to the other photo's.

// Matrix loops read clearer indexed than zipped.
#![allow(clippy::needless_range_loop)]

use rayon::prelude::*;

use crate::colour::{self, M3};
use crate::cpu::Rgb;

/// One channel. NaN: no value (left out of features and refinement).
#[derive(Debug, Clone)]
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Gray {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0.0; width * height],
        }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }

    /// Bilinear sample at a fractional position; None outside the image or
    /// next to a missing value.
    #[inline]
    pub fn sample(&self, x: f64, y: f64) -> Option<f32> {
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        if x0 + 1 >= self.width || y0 + 1 >= self.height {
            // The last row and column, exactly.
            if x0 + 1 == self.width && x == x0 as f64 && y0 < self.height {
                return self.sample_column(x0, y);
            }
            return None;
        }
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let i = y0 * self.width + x0;
        let (a, b) = (self.data[i], self.data[i + 1]);
        let (c, d) = (self.data[i + self.width], self.data[i + self.width + 1]);
        let v = (a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy;
        v.is_finite().then_some(v)
    }

    fn sample_column(&self, x: usize, y: f64) -> Option<f32> {
        let y0 = y.floor() as usize;
        let fy = (y - y0 as f64) as f32;
        let a = self.data[y0 * self.width + x];
        let v = if y0 + 1 < self.height {
            a * (1.0 - fy) + self.data[(y0 + 1) * self.width + x] * fy
        } else {
            a
        };
        v.is_finite().then_some(v)
    }

    /// Half the size, each pixel the mean of four (missing if any is).
    pub fn half(&self) -> Gray {
        let (w, h) = (self.width / 2, self.height / 2);
        let mut out = Gray::new(w.max(1), h.max(1));
        out.data
            .par_chunks_mut(w.max(1))
            .enumerate()
            .for_each(|(y, row)| {
                for (x, o) in row.iter_mut().enumerate() {
                    let i = 2 * y * self.width + 2 * x;
                    *o = 0.25
                        * (self.data[i]
                            + self.data[i + 1]
                            + self.data[i + self.width]
                            + self.data[i + self.width + 1]);
                }
            });
        out
    }

    /// A box blur of radius `r` (missing values spread).
    fn blur(&self, r: usize) -> Gray {
        let (w, h) = (self.width, self.height);
        let n = (2 * r + 1) as f32;
        let mut tmp = Gray::new(w, h);
        tmp.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let src = &self.data[y * w..(y + 1) * w];
            for (x, o) in row.iter_mut().enumerate() {
                let mut s = 0.0;
                for dx in 0..=2 * r {
                    s += src[(x + dx).saturating_sub(r).min(w - 1)];
                }
                *o = s / n;
            }
        });
        let mut out = Gray::new(w, h);
        out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let mut s = 0.0;
                for dy in 0..=2 * r {
                    s += tmp.data[(y + dy).saturating_sub(r).min(h - 1) * w + x];
                }
                *o = s / n;
            }
        });
        out
    }
}

/// The image and its halvings, finest first, down to `min_long` pixels on
/// the long edge.
pub fn pyramid(g: Gray, min_long: usize) -> Vec<Gray> {
    let mut levels = vec![g];
    while let Some(last) = levels.last() {
        if last.width.max(last.height) / 2 < min_long.max(16) {
            break;
        }
        let next = last.half();
        levels.push(next);
    }
    levels
}

/// Level `level`'s coordinates → the full image's (each level halves; a
/// pixel's centre is the mean of the four below it).
fn level_to_full(level: usize) -> M3 {
    let s = (1u64 << level) as f64;
    [
        [s, 0.0, (s - 1.0) / 2.0],
        [0.0, s, (s - 1.0) / 2.0],
        [0.0, 0.0, 1.0],
    ]
}

/// A transform between full-size coordinates, as one between level
/// `level`'s.
pub fn at_level(h: &M3, level: usize) -> M3 {
    let t = level_to_full(level);
    let ti = colour::inv(&t).unwrap_or(colour::IDENTITY);
    normalise(&colour::mul(&ti, &colour::mul(h, &t)))
}

/// A transform between level `level`'s coordinates, as one between the
/// full-size images'.
pub fn from_level(h: &M3, level: usize) -> M3 {
    let t = level_to_full(level);
    let ti = colour::inv(&t).unwrap_or(colour::IDENTITY);
    normalise(&colour::mul(&t, &colour::mul(h, &ti)))
}

/// Scaled so the bottom-right entry is 1.
fn normalise(h: &M3) -> M3 {
    let s = h[2][2];
    if s.abs() < 1e-12 {
        return *h;
    }
    h.map(|r| r.map(|v| v / s))
}

/// Where `h` takes `(x, y)`.
#[inline]
pub fn apply(h: &M3, x: f64, y: f64) -> (f64, f64) {
    let w = h[2][0] * x + h[2][1] * y + h[2][2];
    let w = if w.abs() < 1e-12 { 1e-12 } else { w };
    (
        (h[0][0] * x + h[0][1] * y + h[0][2]) / w,
        (h[1][0] * x + h[1][1] * y + h[1][2]) / w,
    )
}

/// How far apart two transforms put the corners and centre of a
/// `width`×`height` image, at most (pixels).
pub fn distance(a: &M3, b: &M3, width: usize, height: usize) -> f64 {
    let (w, h) = (width as f64 - 1.0, height as f64 - 1.0);
    [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h), (w / 2.0, h / 2.0)]
        .iter()
        .map(|&(x, y)| {
            let (p, q) = (apply(a, x, y), apply(b, x, y));
            (p.0 - q.0).hypot(p.1 - q.1)
        })
        .fold(0.0, f64::max)
}

/// What can move between the frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    Translation,
    /// Translation, rotation and scale (focus breathing).
    Similarity,
    Affine,
    /// A plane seen from elsewhere, or a camera turned about its centre
    /// (hand-held brackets, panoramas).
    Homography,
}

impl Model {
    /// Parameters, and points a minimal sample needs.
    fn params(self) -> usize {
        match self {
            Model::Translation => 2,
            Model::Similarity => 4,
            Model::Affine => 6,
            Model::Homography => 8,
        }
    }

    fn sample_size(self) -> usize {
        self.params().div_ceil(2)
    }

    /// d(h00, h01, h02, h10, h11, h12, h20, h21) / d(parameters).
    fn basis(self) -> Vec<[f64; 8]> {
        let unit = |i: usize| {
            let mut v = [0.0; 8];
            v[i] = 1.0;
            v
        };
        match self {
            Model::Translation => vec![unit(2), unit(5)],
            // h00 = h11 = 1 + a, h10 = −h01 = b.
            Model::Similarity => vec![
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
                [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
                unit(2),
                unit(5),
            ],
            Model::Affine => (0..6).map(unit).collect(),
            Model::Homography => (0..8).map(unit).collect(),
        }
    }

    /// The parameters of `h` (which is of this model).
    fn to_params(self, h: &M3) -> Vec<f64> {
        let h = normalise(h);
        match self {
            Model::Translation => vec![h[0][2], h[1][2]],
            Model::Similarity => vec![
                0.5 * (h[0][0] + h[1][1]) - 1.0,
                0.5 * (h[1][0] - h[0][1]),
                h[0][2],
                h[1][2],
            ],
            Model::Affine => vec![
                h[0][0] - 1.0,
                h[0][1],
                h[0][2],
                h[1][0],
                h[1][1] - 1.0,
                h[1][2],
            ],
            Model::Homography => vec![
                h[0][0] - 1.0,
                h[0][1],
                h[0][2],
                h[1][0],
                h[1][1] - 1.0,
                h[1][2],
                h[2][0],
                h[2][1],
            ],
        }
    }

    fn matrix(self, q: &[f64]) -> M3 {
        let mut p = [0.0; 8];
        for (b, &v) in self.basis().iter().zip(q) {
            for i in 0..8 {
                p[i] += b[i] * v;
            }
        }
        [
            [1.0 + p[0], p[1], p[2]],
            [p[3], 1.0 + p[4], p[5]],
            [p[6], p[7], 1.0],
        ]
    }
}

// ---------------------------------------------------------------------------
// Features

/// A corner and its descriptor.
#[derive(Debug, Clone)]
pub struct Feature {
    pub x: f32,
    pub y: f32,
    pub strength: f32,
    /// 64 samples, zero mean and unit length.
    pub descriptor: [f32; 64],
}

/// Descriptor samples: 8×8, this many pixels apart.
const SPACING: f32 = 3.0;

/// Up to about `max` corners, spread over the image: Harris response,
/// local maxima, the strongest in each cell of a grid.
pub fn features(g: &Gray, max: usize) -> Vec<Feature> {
    let (w, h) = (g.width, g.height);
    if w < 40 || h < 40 {
        return Vec::new();
    }
    // Gradients, then the structure tensor summed over a 5×5 window.
    let mut ixx = Gray::new(w, h);
    let mut iyy = Gray::new(w, h);
    let mut ixy = Gray::new(w, h);
    ixx.data
        .par_chunks_mut(w)
        .zip(iyy.data.par_chunks_mut(w))
        .zip(ixy.data.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, ((xx, yy), xy))| {
            if y == 0 || y + 1 >= h {
                return;
            }
            for x in 1..w - 1 {
                let gx = 0.5 * (g.at(x + 1, y) - g.at(x - 1, y));
                let gy = 0.5 * (g.at(x, y + 1) - g.at(x, y - 1));
                if gx.is_finite() && gy.is_finite() {
                    xx[x] = gx * gx;
                    yy[x] = gy * gy;
                    xy[x] = gx * gy;
                } else {
                    xx[x] = f32::NAN;
                }
            }
        });
    // A NaN in the window makes the response NaN: no corner there.
    let (sxx, syy, sxy) = (ixx.blur(2), iyy.blur(2), ixy.blur(2));
    let response: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (a, b, c) = (sxx.data[i], syy.data[i], sxy.data[i]);
            let r = a * b - c * c - 0.04 * (a + b) * (a + b);
            if r.is_finite() { r } else { 0.0 }
        })
        .collect();
    // Descriptors come from a blurred copy, out to this far.
    let reach = (SPACING * 3.5).ceil() as usize + 2;
    let blurred = g.blur(1).blur(1);
    let cells_x = 16usize;
    let cells_y = 12usize;
    let per_cell = (max / (cells_x * cells_y)).max(2);
    // Each cell keeps its own strongest: a bright window's edges mustn't
    // crowd out a dim room's texture. Only what's next to nothing is out.
    let max_r = response.par_iter().copied().reduce(|| 0.0, f32::max);
    let floor = max_r * 1e-8;
    let cells: Vec<Vec<(f32, usize, usize)>> = (0..cells_x * cells_y)
        .into_par_iter()
        .map(|cell| {
            let (cx, cy) = (cell % cells_x, cell / cells_x);
            let x0 = (cx * w / cells_x).max(reach);
            let x1 = ((cx + 1) * w / cells_x).min(w - reach);
            let y0 = (cy * h / cells_y).max(reach);
            let y1 = ((cy + 1) * h / cells_y).min(h - reach);
            let mut found = Vec::new();
            for y in y0..y1.max(y0) {
                for x in x0..x1.max(x0) {
                    let r = response[y * w + x];
                    if r <= floor {
                        continue;
                    }
                    let mut peak = true;
                    'n: for dy in 0..3 {
                        for dx in 0..3 {
                            if (dx, dy) != (1, 1) && response[(y + dy - 1) * w + x + dx - 1] >= r {
                                peak = false;
                                break 'n;
                            }
                        }
                    }
                    if peak {
                        found.push((r, x, y));
                    }
                }
            }
            found.sort_by(|a, b| b.0.total_cmp(&a.0));
            found.truncate(per_cell);
            found
        })
        .collect();
    cells
        .into_par_iter()
        .flatten()
        .filter_map(|(r, x, y)| {
            let (fx, fy) = subpixel(&response, w, x, y);
            let descriptor = describe(&blurred, fx, fy)?;
            Some(Feature {
                x: fx,
                y: fy,
                strength: r,
                descriptor,
            })
        })
        .collect()
}

/// The response peak at `(x, y)` to a fraction of a pixel (a parabola
/// through each axis's neighbours).
fn subpixel(r: &[f32], w: usize, x: usize, y: usize) -> (f32, f32) {
    let c = r[y * w + x];
    let off = |a: f32, b: f32| {
        let d = a - 2.0 * c + b;
        if d.abs() > 1e-20 {
            (0.5 * (a - b) / d).clamp(-0.5, 0.5)
        } else {
            0.0
        }
    };
    (
        x as f32 + off(r[y * w + x - 1], r[y * w + x + 1]),
        y as f32 + off(r[(y - 1) * w + x], r[(y + 1) * w + x]),
    )
}

fn describe(blurred: &Gray, x: f32, y: f32) -> Option<[f32; 64]> {
    let mut d = [0.0f32; 64];
    for j in 0..8 {
        for i in 0..8 {
            let sx = x + (i as f32 - 3.5) * SPACING;
            let sy = y + (j as f32 - 3.5) * SPACING;
            d[j * 8 + i] = blurred.sample(f64::from(sx), f64::from(sy))?;
        }
    }
    let mean = d.iter().sum::<f32>() / 64.0;
    d.iter_mut().for_each(|v| *v -= mean);
    let norm = d.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm < 1e-6 {
        return None;
    }
    d.iter_mut().for_each(|v| *v /= norm);
    Some(d)
}

/// A point in the reference and where it is in the other photo.
pub type Pair = ([f64; 2], [f64; 2]);

/// Features of `a` (the reference) matched to `b`'s: each other's nearest
/// neighbours, clearly nearer than the next best (Lowe's ratio). With
/// `near`, only features within that distance of `guess(a)` are compared.
pub fn matches(a: &[Feature], b: &[Feature], near: Option<(f64, &M3)>) -> Vec<Pair> {
    const RATIO: f32 = 0.8;
    let dist = |p: &[f32; 64], q: &[f32; 64]| -> f32 {
        p.iter().zip(q).map(|(x, y)| (x - y) * (x - y)).sum()
    };
    let close = |fa: &Feature, fb: &Feature| -> bool {
        match near {
            None => true,
            Some((r, h)) => {
                let (gx, gy) = apply(h, f64::from(fa.x), f64::from(fa.y));
                (gx - f64::from(fb.x)).hypot(gy - f64::from(fb.y)) <= r
            }
        }
    };
    // Best and second best for each, both ways.
    let best = |from: &[Feature], to: &[Feature], forward: bool| -> Vec<Option<usize>> {
        from.par_iter()
            .map(|f| {
                let (mut d1, mut d2, mut at) = (f32::MAX, f32::MAX, None);
                for (j, g) in to.iter().enumerate() {
                    let ok = if forward { close(f, g) } else { close(g, f) };
                    if !ok {
                        continue;
                    }
                    let d = dist(&f.descriptor, &g.descriptor);
                    if d < d1 {
                        d2 = d1;
                        d1 = d;
                        at = Some(j);
                    } else if d < d2 {
                        d2 = d;
                    }
                }
                at.filter(|_| d1 < RATIO * RATIO * d2)
            })
            .collect()
    };
    let ab = best(a, b, true);
    let ba = best(b, a, false);
    ab.iter()
        .enumerate()
        .filter_map(|(i, j)| {
            let j = (*j)?;
            (ba[j] == Some(i)).then(|| {
                (
                    [f64::from(a[i].x), f64::from(a[i].y)],
                    [f64::from(b[j].x), f64::from(b[j].y)],
                )
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Fitting

/// Solve `a·x = b` (n×n) by Gaussian elimination with partial pivoting.
pub(crate) fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-14 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            if f == 0.0 {
                continue;
            }
            for k in col..n {
                a[row][k] -= f * a[col][k];
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// Least-squares fit of `model` to the pairs (exact for a minimal
/// sample). Points are centred and scaled first (Hartley's normalisation).
pub fn fit(model: Model, pairs: &[Pair]) -> Option<M3> {
    if pairs.len() < model.sample_size() {
        return None;
    }
    // Each side centred, both scaled alike (so a similarity stays one).
    let centre = |side: fn(&Pair) -> [f64; 2]| -> ([f64; 2], f64) {
        let n = pairs.len() as f64;
        let c = [
            pairs.iter().map(|p| side(p)[0]).sum::<f64>() / n,
            pairs.iter().map(|p| side(p)[1]).sum::<f64>() / n,
        ];
        let d = pairs
            .iter()
            .map(|p| (side(p)[0] - c[0]).hypot(side(p)[1] - c[1]))
            .sum::<f64>()
            / n;
        (c, d)
    };
    let (ca, da) = centre(|p| p.0);
    let (cb, db) = centre(|p| p.1);
    let d = da.max(db);
    let s = if d > 1e-9 {
        std::f64::consts::SQRT_2 / d
    } else {
        1.0
    };
    let ta = [[s, 0.0, -s * ca[0]], [0.0, s, -s * ca[1]], [0.0, 0.0, 1.0]];
    let tb = [[s, 0.0, -s * cb[0]], [0.0, s, -s * cb[1]], [0.0, 0.0, 1.0]];
    let pts: Vec<([f64; 2], [f64; 2])> = pairs
        .iter()
        .map(|(p, q)| {
            let a = apply(&ta, p[0], p[1]);
            let b = apply(&tb, q[0], q[1]);
            ([a.0, a.1], [b.0, b.1])
        })
        .collect();
    let hn = match model {
        Model::Translation => {
            let n = pts.len() as f64;
            let tx = pts.iter().map(|(a, b)| b[0] - a[0]).sum::<f64>() / n;
            let ty = pts.iter().map(|(a, b)| b[1] - a[1]).sum::<f64>() / n;
            [[1.0, 0.0, tx], [0.0, 1.0, ty], [0.0, 0.0, 1.0]]
        }
        _ => {
            // Linear in the parameters except a homography's denominator,
            // which the DLT (h22 = 1) moves across: u·(h20 x + h21 y + 1).
            let k = model.params();
            let basis = model.basis();
            let mut ata = vec![vec![0.0; k]; k];
            let mut atb = vec![0.0; k];
            for (a, b) in &pts {
                let (x, y) = (a[0], a[1]);
                // Rows: u = h00 x + h01 y + h02 − h20 x u − h21 y u;
                //       v = h10 x + h11 y + h12 − h20 x v − h21 y v.
                let ru = [x, y, 1.0, 0.0, 0.0, 0.0, -x * b[0], -y * b[0]];
                let rv = [0.0, 0.0, 0.0, x, y, 1.0, -x * b[1], -y * b[1]];
                // In parameter space (q), with the identity's 1s moved over.
                for (row, target) in [(ru, b[0] - x), (rv, b[1] - y)] {
                    let r: Vec<f64> = basis
                        .iter()
                        .map(|bv| (0..8).map(|i| row[i] * bv[i]).sum())
                        .collect();
                    // A homography's DLT has no identity offset on h20, h21:
                    // `target` already moved x, y over for h00, h11.
                    for i in 0..k {
                        atb[i] += r[i] * target;
                        for j in 0..k {
                            ata[i][j] += r[i] * r[j];
                        }
                    }
                }
            }
            let q = solve(ata, atb)?;
            model.matrix(&q)
        }
    };
    // Back to pixels: tb⁻¹ · hn · ta.
    let h = colour::mul(&colour::inv(&tb)?, &colour::mul(&hn, &ta));
    let h = normalise(&h);
    h.iter().flatten().all(|v| v.is_finite()).then_some(h)
}

/// Reprojection error of a pair (pixels).
#[inline]
pub fn error(h: &M3, p: &Pair) -> f64 {
    let (x, y) = apply(h, p.0[0], p.0[1]);
    (x - p.1[0]).hypot(y - p.1[1])
}

/// A small deterministic generator (xorshift64*), so merges repeat.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// RANSAC: the model most pairs agree with (within `threshold` pixels),
/// refitted to them by least squares. Returns it and which pairs agree.
pub fn ransac(model: Model, pairs: &[Pair], threshold: f64) -> Option<(M3, Vec<bool>)> {
    let s = model.sample_size();
    if pairs.len() < s + 2 {
        return None;
    }
    let mut rng = Rng(seed(pairs.len()));
    let mut best: Option<(usize, M3)> = None;
    let mut needed = 4000usize;
    let mut i = 0;
    while i < needed {
        i += 1;
        let mut pick: Vec<usize> = Vec::with_capacity(s);
        while pick.len() < s {
            let j = rng.below(pairs.len());
            if !pick.contains(&j) {
                pick.push(j);
            }
        }
        let sample: Vec<Pair> = pick.iter().map(|&j| pairs[j]).collect();
        let Some(h) = fit(model, &sample) else {
            continue;
        };
        let n = pairs.iter().filter(|p| error(&h, p) <= threshold).count();
        if best.as_ref().is_none_or(|(b, _)| n > *b) {
            best = Some((n, h));
            // Enough tries to draw an all-inlier sample with 99.9%.
            let w = n as f64 / pairs.len() as f64;
            let p = 1.0 - w.powi(s as i32);
            if p <= 1e-12 {
                needed = i;
            } else {
                let k = ((1.0 - 0.999f64).ln() / p.ln()).ceil();
                needed = needed.min((k as usize).max(50));
            }
        }
    }
    let (_, mut h) = best?;
    // Refit to the inliers, twice (the set grows as the fit improves).
    let mut inliers = vec![false; pairs.len()];
    for _ in 0..2 {
        for (k, p) in pairs.iter().enumerate() {
            inliers[k] = error(&h, p) <= threshold;
        }
        let agree: Vec<Pair> = pairs
            .iter()
            .zip(&inliers)
            .filter(|(_, ok)| **ok)
            .map(|(p, _)| *p)
            .collect();
        h = fit(model, &agree)?;
    }
    for (k, p) in pairs.iter().enumerate() {
        inliers[k] = error(&h, p) <= threshold;
    }
    Some((h, inliers))
}

/// A seed from the problem's size, so the same frames give the same fit.
const fn seed(n: usize) -> u64 {
    0x9E37_79B9_7F4A_7C15 ^ (n as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
}

// ---------------------------------------------------------------------------
// Direct refinement

/// What [`refine`] settled on.
#[derive(Debug, Clone, Copy)]
pub struct Refined {
    /// Reference → moving, full-size coordinates.
    pub h: M3,
    /// The moving image's gain and offset that best matched the reference.
    pub gain: f64,
    pub offset: f64,
    /// Root-mean-square difference left at full size, and over how many
    /// pixels.
    pub rms: f64,
    pub samples: usize,
}

/// At most this many pixels a level (the strongest gradients, spread out).
const MAX_SAMPLES: usize = 160_000;

/// Pixels of `g` worth fitting on: away from the edge and missing values,
/// in the top 40% of gradient, thinned evenly to [`MAX_SAMPLES`].
fn samples(g: &Gray) -> Vec<(u32, u32)> {
    let (w, h) = (g.width, g.height);
    if w < 8 || h < 8 {
        return Vec::new();
    }
    let rows: Vec<Vec<(f32, u32, u32)>> = (2..h - 2)
        .into_par_iter()
        .map(|y| {
            let mut out = Vec::new();
            for x in 2..w - 2 {
                let gx = g.at(x + 1, y) - g.at(x - 1, y);
                let gy = g.at(x, y + 1) - g.at(x, y - 1);
                let m = gx * gx + gy * gy;
                if m.is_finite() && m > 0.0 && g.at(x, y).is_finite() {
                    out.push((m, x as u32, y as u32));
                }
            }
            out
        })
        .collect();
    let mut all: Vec<(f32, u32, u32)> = rows.into_iter().flatten().collect();
    if all.is_empty() {
        return Vec::new();
    }
    let cut = all.len() * 6 / 10;
    all.select_nth_unstable_by(cut, |a, b| a.0.total_cmp(&b.0));
    let strong = &all[cut..];
    let step = strong.len().div_ceil(MAX_SAMPLES).max(1);
    let mut picked: Vec<(u32, u32)> = strong.iter().step_by(step).map(|s| (s.1, s.2)).collect();
    picked.sort_unstable_by_key(|p| (p.1, p.0));
    picked
}

/// Refine `h` (reference → moving, full-size coordinates, of `model`) on
/// the pixels, from pyramid level `start` down to full size: Gauss–Newton
/// with a gain and offset, Huber-weighted. None if there's too little to
/// fit on.
pub fn refine(
    reference: &[Gray],
    moving: &[Gray],
    h: &M3,
    model: Model,
    start: usize,
) -> Option<Refined> {
    let levels = reference.len().min(moving.len());
    if levels == 0 {
        return None;
    }
    let mut h = normalise(h);
    let (mut gain, mut offset) = (1.0f64, 0.0f64);
    let mut last = None;
    for level in (0..=start.min(levels - 1)).rev() {
        let (r, m) = (&reference[level], &moving[level]);
        let pts = samples(r);
        if pts.len() < 50 {
            continue;
        }
        // Normalised coordinates: centred, the long edge from −1 to 1.
        let sc = r.width.max(r.height) as f64 / 2.0;
        let cr = ((r.width as f64 - 1.0) / 2.0, (r.height as f64 - 1.0) / 2.0);
        let cm = ((m.width as f64 - 1.0) / 2.0, (m.height as f64 - 1.0) / 2.0);
        let nr = [
            [1.0 / sc, 0.0, -cr.0 / sc],
            [0.0, 1.0 / sc, -cr.1 / sc],
            [0.0, 0.0, 1.0],
        ];
        let nm = [
            [1.0 / sc, 0.0, -cm.0 / sc],
            [0.0, 1.0 / sc, -cm.1 / sc],
            [0.0, 0.0, 1.0],
        ];
        let (nri, nmi) = (colour::inv(&nr)?, colour::inv(&nm)?);
        let hl = at_level(&h, level);
        let hn = normalise(&colour::mul(&nm, &colour::mul(&hl, &nri)));
        let mut q = model.to_params(&hn);
        let k = q.len();
        let basis = model.basis();
        let mut rms = f64::MAX;
        for _ in 0..30 {
            let cur = model.matrix(&q);
            // Each sample's residual and Jacobian row (k geometric, then
            // gain and offset).
            let rows: Vec<(f64, Vec<f64>)> = pts
                .par_iter()
                .filter_map(|&(x, y)| {
                    let (x, y) = (f64::from(x), f64::from(y));
                    let (xn, yn) = ((x - cr.0) / sc, (y - cr.1) / sc);
                    let d = cur[2][0] * xn + cur[2][1] * yn + 1.0;
                    if d.abs() < 1e-6 {
                        return None;
                    }
                    let (un, vn) = apply(&cur, xn, yn);
                    let (mx, my) = (un * sc + cm.0, vn * sc + cm.1);
                    let v = f64::from(m.sample(mx, my)?);
                    let gx = 0.5 * f64::from(m.sample(mx + 1.0, my)? - m.sample(mx - 1.0, my)?);
                    let gy = 0.5 * f64::from(m.sample(mx, my + 1.0)? - m.sample(mx, my - 1.0)?);
                    // d(value)/d(normalised position), then the warp's
                    // derivative in h00 … h21.
                    let (gu, gv) = (gx * sc * gain, gy * sc * gain);
                    let dp = [
                        gu * xn / d,
                        gu * yn / d,
                        gu / d,
                        gv * xn / d,
                        gv * yn / d,
                        gv / d,
                        -(gu * un + gv * vn) * xn / d,
                        -(gu * un + gv * vn) * yn / d,
                    ];
                    let mut j: Vec<f64> = basis
                        .iter()
                        .map(|b| (0..8).map(|i| dp[i] * b[i]).sum())
                        .collect();
                    j.push(v);
                    j.push(1.0);
                    let e = gain * v + offset - f64::from(r.at(x as usize, y as usize));
                    Some((e, j))
                })
                .collect();
            if rows.len() < 50 {
                break;
            }
            // Huber weights from the residuals' spread (MAD).
            let mut abs: Vec<f64> = rows.iter().map(|(e, _)| e.abs()).collect();
            let mid = abs.len() / 2;
            abs.select_nth_unstable_by(mid, f64::total_cmp);
            let delta = 1.345 * (1.4826 * abs[mid]).max(1e-6);
            let n = k + 2;
            let zero = || (vec![0.0f64; n * n], vec![0.0f64; n], 0.0f64);
            let (ata, atb, sq) = rows
                .par_iter()
                .fold(zero, |(mut ata, mut atb, mut sq), (e, j)| {
                    let w = if e.abs() <= delta {
                        1.0
                    } else {
                        delta / e.abs()
                    };
                    for a in 0..n {
                        atb[a] += w * j[a] * e;
                        for b in a..n {
                            ata[a * n + b] += w * j[a] * j[b];
                        }
                    }
                    sq += e * e;
                    (ata, atb, sq)
                })
                .reduce(zero, |(mut a1, mut b1, s1), (a2, b2, s2)| {
                    a1.iter_mut().zip(&a2).for_each(|(x, y)| *x += y);
                    b1.iter_mut().zip(&b2).for_each(|(x, y)| *x += y);
                    (a1, b1, s1 + s2)
                });
            rms = (sq / rows.len() as f64).sqrt();
            let mut a = vec![vec![0.0; n]; n];
            for i in 0..n {
                for j in i..n {
                    a[i][j] = ata[i * n + j];
                    a[j][i] = ata[i * n + j];
                }
                // A touch of damping (Levenberg) for flat directions.
                a[i][i] = a[i][i] * (1.0 + 1e-6) + 1e-12;
            }
            let Some(step) = solve(a, atb.iter().map(|v| -v).collect()) else {
                break;
            };
            let before = model.matrix(&q);
            for i in 0..k {
                q[i] += step[i];
            }
            gain += step[k];
            offset += step[k + 1];
            let after = model.matrix(&q);
            // Converged when no corner moves by a thousandth of a pixel
            // here (`sc` pixels a normalised unit).
            let moved = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
                .iter()
                .map(|&(x, y)| {
                    let (p, q) = (apply(&before, x, y), apply(&after, x, y));
                    (p.0 - q.0).hypot(p.1 - q.1)
                })
                .fold(0.0, f64::max)
                * sc;
            if moved < 1e-3 {
                break;
            }
        }
        let hl = normalise(&colour::mul(&nmi, &colour::mul(&model.matrix(&q), &nr)));
        h = from_level(&hl, level);
        last = Some((rms, pts.len(), level));
    }
    let (rms, samples, level) = last?;
    (level == 0).then_some(Refined {
        h,
        gain,
        offset,
        rms,
        samples,
    })
}

// ---------------------------------------------------------------------------
// The two together

/// How to align.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub model: Model,
    /// The frames overlap almost wholly and moved at most this much (a
    /// fraction of the long edge): matches are only looked for that near,
    /// and if features fail, refinement starts from no movement. None:
    /// anywhere (a panorama's neighbours).
    pub max_shift: Option<f64>,
    /// Corners to look for in each frame.
    pub features: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            model: Model::Homography,
            max_shift: Some(0.08),
            features: 2000,
        }
    }
}

/// How one frame was aligned to the reference.
#[derive(Debug, Clone, Copy)]
pub struct Alignment {
    /// Reference → this frame, full-size pixel coordinates.
    pub h: M3,
    /// Features matched, and how many of them the fit agreed with.
    pub matched: usize,
    pub inliers: usize,
    /// What refinement left (None: it couldn't run; `h` is the features').
    pub refined: Option<Refined>,
}

/// The features are found at a level no bigger than this.
const FEATURE_EDGE: usize = 1600;

/// Align `moving` to `reference` (pyramids from [`pyramid`]).
pub fn align(reference: &[Gray], moving: &[Gray], o: &Options) -> Result<Alignment, String> {
    let levels = reference.len().min(moving.len());
    if levels == 0 {
        return Err("nothing to align".into());
    }
    let fl = (0..levels)
        .find(|&l| reference[l].width.max(reference[l].height) <= FEATURE_EDGE)
        .unwrap_or(levels - 1);
    let (fr, fm) = (&reference[fl], &moving[fl]);
    let a = features(fr, o.features);
    let b = features(fm, o.features);
    let near = o
        .max_shift
        .map(|f| (f * fr.width.max(fr.height) as f64, colour::IDENTITY));
    let pairs = matches(&a, &b, near.as_ref().map(|(r, h)| (*r, h)));
    let found = ransac(o.model, &pairs, 1.5)
        .map(|(h, inl)| (h, inl.iter().filter(|v| **v).count()))
        .filter(|(_, n)| *n >= 12);
    let (start, start_level, inliers) = match found {
        Some((h, n)) => (from_level(&h, fl), fl, n),
        None if o.max_shift.is_some() => (colour::IDENTITY, levels - 1, 0),
        None => {
            return Err(format!(
                "the photos don't overlap enough to align ({} matching points)",
                pairs.len()
            ));
        }
    };
    // Refinement that wandered far from the features' answer found
    // something else (a repeating pattern): keep the features'.
    let reach = 4.0 * (1u64 << fl) as f64;
    let (w, h) = (reference[0].width, reference[0].height);
    let refined = refine(reference, moving, &start, o.model, start_level)
        .filter(|r| inliers == 0 || distance(&r.h, &start, w, h) < reach);
    if inliers == 0 && refined.is_none() {
        return Err("the photos couldn't be aligned".into());
    }
    Ok(Alignment {
        h: refined.map_or(start, |r| r.h),
        matched: pairs.len(),
        inliers,
        refined,
    })
}

// ---------------------------------------------------------------------------
// Warping

/// Catmull–Rom weights for a fraction `t`.
#[inline]
fn cubic(t: f32) -> [f32; 4] {
    let (t2, t3) = (t * t, t * t * t);
    [
        0.5 * (-t3 + 2.0 * t2 - t),
        0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
        0.5 * (-3.0 * t3 + 4.0 * t2 + t),
        0.5 * (t3 - t2),
    ]
}

/// `img` (the moving frame) resampled into the reference's frame
/// (`width`×`height`): each pixel `p` takes `img` at `h(p)`, Catmull–Rom,
/// held within its four nearest values (no ringing at clipped edges).
/// Returns it and which pixels landed inside `img`.
pub fn warp(img: &Rgb, h: &M3, width: usize, height: usize) -> (Rgb, Vec<bool>) {
    let mut out = Rgb::new(width, height);
    let mut inside = vec![false; width * height];
    let (w, hh) = (img.width as isize, img.height as isize);
    out.data
        .par_chunks_mut(width * 3)
        .zip(inside.par_chunks_mut(width))
        .enumerate()
        .for_each(|(y, (row, ins))| {
            for x in 0..width {
                let (sx, sy) = apply(h, x as f64, y as f64);
                if !(sx >= 0.0 && sy >= 0.0 && sx <= (w - 1) as f64 && sy <= (hh - 1) as f64) {
                    continue;
                }
                ins[x] = true;
                let (x0, y0) = (sx.floor() as isize, sy.floor() as isize);
                let wx = cubic((sx - x0 as f64) as f32);
                let wy = cubic((sy - y0 as f64) as f32);
                let at = |xx: isize, yy: isize| -> usize {
                    (yy.clamp(0, hh - 1) * w + xx.clamp(0, w - 1)) as usize * 3
                };
                for c in 0..3 {
                    let mut s = 0.0f32;
                    for j in 0..4 {
                        let mut r = 0.0f32;
                        for i in 0..4 {
                            r += wx[i] * img.data[at(x0 + i as isize - 1, y0 + j as isize - 1) + c];
                        }
                        s += wy[j] * r;
                    }
                    let n = [
                        img.data[at(x0, y0) + c],
                        img.data[at(x0 + 1, y0) + c],
                        img.data[at(x0, y0 + 1) + c],
                        img.data[at(x0 + 1, y0 + 1) + c],
                    ];
                    let lo = n.iter().copied().fold(f32::MAX, f32::min);
                    let hi = n.iter().copied().fold(f32::MIN, f32::max);
                    row[x * 3 + c] = s.clamp(lo, hi);
                }
            }
        });
    (out, inside)
}

/// A per-pixel mask (`mw`×`mh`) carried into the reference's frame like
/// [`warp`]: a pixel is set if any of the 4×4 it was sampled from is.
pub fn warp_mask(
    mask: &[u8],
    mw: usize,
    mh: usize,
    h: &M3,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    let (w, hh) = (mw as isize, mh as isize);
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            let (sx, sy) = apply(h, x as f64, y as f64);
            if !(sx > -1.0 && sy > -1.0 && sx < w as f64 && sy < hh as f64) {
                continue;
            }
            let (x0, y0) = (sx.floor() as isize, sy.floor() as isize);
            let mut v = 0u8;
            for yy in y0 - 1..=y0 + 2 {
                for xx in x0 - 1..=x0 + 2 {
                    v = v.max(mask[(yy.clamp(0, hh - 1) * w + xx.clamp(0, w - 1)) as usize]);
                }
            }
            *o = v;
        }
    });
    out
}
