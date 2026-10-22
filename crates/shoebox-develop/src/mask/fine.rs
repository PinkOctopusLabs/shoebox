//! Masks finer than the grid (EDITING-SPEC, Rendering masks: "for export,
//! upsample with a guided filter on full-resolution luminance so edges
//! follow hair and branches rather than going blocky").
//!
//! AI masks are found on the [`super::LONG`] grid. When the source is
//! larger (exports and 100% regions read the full-size source) and a
//! layer has an AI part, every layer is rasterised on a finer grid (the
//! source's own pixels, within [`BUDGET`]):
//!
//! - **AI parts:** He and Sun's fast guided filter. The coefficients of a
//!   guided filter of the found mask by the source are worked out on the
//!   grid, then each fine pixel is `a·I + b`, with `a` and `b` read
//!   bilinearly and `I` the source at that pixel, so edges land where the
//!   full-size picture has them. The guide is the source's colour, each
//!   channel compressed as luminance would be (He, Sun and Tang's colour
//!   guided filter): a luminance guide alone can't see an edge between
//!   two colours equally bright (a red flower on green leaves), and there
//!   it would blur the mask rather than sharpen it.
//! - **Gradients, ellipses and brushes** are rasterised at the fine size:
//!   they're formulas, as cheap per pixel as on the grid, and a hard brush
//!   edge stays sharp.
//! - **Luminance and colour ranges** stay on the grid and are read
//!   bilinearly: they need the picture's tones per pixel (three floats
//!   each, too much at 45 MP), and they're soft by construction.
//!
//! Work goes in bands of rows, so beyond the result only a few rows of
//! floats are held per thread; each AI part's coefficients on the grid
//! are four planes of floats (70 MB at 2560 × 1707), and working them out
//! briefly takes about three times that.
//! Both pipes read the result like any other masks.

use std::sync::Arc;

use rayon::prelude::*;

use super::{Find, Masks, Tones};
use crate::cpu::Rgb;
use crate::cpu::maps::box_mean;
use crate::edit::{Local, Part};

/// The most the fine masks may take, all layers together (so a 45 MP
/// export with two layers is full size, with more a little less). It's
/// also within a GPU storage binding (wgpu's default limit).
pub const BUDGET: usize = 128 << 20;
/// The guided filter's radius, in grid pixels, and its regularisation
/// (the guide runs 0–1): edges in the guide this much (as a variance)
/// above the noise are followed.
pub const RADIUS: usize = 4;
pub const EPS: f32 = 1e-3;
/// Rows per band.
const BAND: usize = 32;

/// The fine grid for a `sw`×`sh` source with `layers` masks; None when the
/// usual grid is already as fine (the source isn't larger than it).
pub fn grid(sw: usize, sh: usize, layers: usize) -> Option<(usize, usize)> {
    let (gw, gh) = super::grid(sw, sh);
    if layers == 0 || sw <= gw || sh == 0 {
        return None;
    }
    let n = sw * sh * layers;
    let k = if n > BUDGET {
        (BUDGET as f64 / n as f64).sqrt()
    } else {
        1.0
    };
    let fw = ((sw as f64 * k).floor() as usize).max(1);
    let fh = ((sh as f64 * k).floor() as usize).max(1);
    (fw > gw && fh >= gh).then_some((fw, fh))
}

/// What AI parts follow: the source's colour per fine pixel, relative to
/// its mean luminance, each channel compressed to 0–1 (`v/(1+v)`) and
/// gamma encoded, so an edge counts about as much in shadows as in
/// highlights. It depends only on the source, not the edit.
pub struct Guide<'a> {
    src: &'a Rgb,
    norm: f32,
    /// On the grid (from the source's copy on it).
    lo: Vec<[f32; 3]>,
}

impl<'a> Guide<'a> {
    /// From the source and its area-averaged copy on the grid.
    pub fn new(src: &'a Rgb, grid: &Rgb) -> Self {
        let px = grid.data.as_chunks::<3>().0;
        let mean = px
            .par_iter()
            .map(|p| f64::from(0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2]))
            .sum::<f64>()
            / px.len().max(1) as f64;
        let norm = 1.0 / (mean as f32).max(1e-6);
        let lo = px.par_iter().map(|p| value(*p, norm)).collect();
        Self { src, norm, lo }
    }

    /// The guide on the grid.
    pub fn grid(&self) -> &[[f32; 3]] {
        &self.lo
    }

    /// Fine rows `rows` of an `fw`×`fh` grid over the source.
    fn rows(&self, rows: std::ops::Range<usize>, fw: usize, fh: usize) -> Vec<[f32; 3]> {
        let (sw, sh) = (self.src.width, self.src.height);
        let mut out = vec![[0f32; 3]; rows.len() * fw];
        out.par_chunks_mut(fw.max(1))
            .enumerate()
            .for_each(|(row, line)| {
                let y = rows.start + row;
                if (fw, fh) == (sw, sh) {
                    for (x, o) in line.iter_mut().enumerate() {
                        *o = value(self.src.px(x, y), self.norm);
                    }
                } else {
                    let sy = (y as f32 + 0.5) * sh as f32 / fh as f32 - 0.5;
                    for (x, o) in line.iter_mut().enumerate() {
                        let sx = (x as f32 + 0.5) * sw as f32 / fw as f32 - 0.5;
                        let p = crate::cpu::sample(
                            self.src,
                            sx.clamp(0.0, (sw - 1) as f32),
                            sy.clamp(0.0, (sh - 1) as f32),
                        );
                        *o = value(p, self.norm);
                    }
                }
            });
        out
    }
}

fn value(p: [f32; 3], norm: f32) -> [f32; 3] {
    p.map(|v| {
        let v = (v * norm).max(0.0);
        (v / (1.0 + v)).powf(1.0 / 2.2)
    })
}

/// A found mask's guided-filter coefficients on the grid: the mask at a
/// pixel with guide `I` is `a·I + b`.
pub struct Coefficients {
    a: [Vec<f32>; 3],
    b: Vec<f32>,
    w: usize,
    h: usize,
}

impl Coefficients {
    /// For `mask` (bytes, `w`×`h`) guided by `guide` (the same size).
    pub fn new(mask: &[u8], guide: &[[f32; 3]], w: usize, h: usize) -> Self {
        let p: Vec<f32> = mask.par_iter().map(|&v| f32::from(v) / 255.0).collect();
        Self::of_values(p, guide, w, h)
    }

    /// For values 0–1 (`w`×`h`) guided by `guide` (the same size).
    pub fn of_values(p: Vec<f32>, guide: &[[f32; 3]], w: usize, h: usize) -> Self {
        let r = RADIUS;
        let mean = |v: &[f32]| box_mean(v, w, h, r);
        let chan = |c: usize| guide.par_iter().map(|g| g[c]).collect::<Vec<f32>>();
        let ch = [chan(0), chan(1), chan(2)];
        let mean_p = mean(&p);
        let mean_i = [mean(&ch[0]), mean(&ch[1]), mean(&ch[2])];
        let product = |a: &[f32], b: &[f32]| {
            let v: Vec<f32> = a.par_iter().zip(b).map(|(a, b)| a * b).collect();
            mean(&v)
        };
        let ip = [
            product(&ch[0], &p),
            product(&ch[1], &p),
            product(&ch[2], &p),
        ];
        drop(p);
        // The guide's covariances: rr, rg, rb, gg, gb, bb.
        let pairs = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];
        let ii: Vec<Vec<f32>> = pairs
            .iter()
            .map(|&(c, d)| product(&ch[c], &ch[d]))
            .collect();
        drop(ch);
        let n = w * h;
        let mut a = [vec![0f32; n], vec![0f32; n], vec![0f32; n]];
        let mut b = vec![0f32; n];
        {
            let [a0, a1, a2] = &mut a;
            a0.par_iter_mut()
                .zip(a1.par_iter_mut())
                .zip(a2.par_iter_mut())
                .zip(b.par_iter_mut())
                .enumerate()
                .for_each(|(k, (((a0, a1), a2), b))| {
                    let mi = [0, 1, 2].map(|c| f64::from(mean_i[c][k]));
                    let mp = f64::from(mean_p[k]);
                    let cov = |q: usize, c: usize, d: usize| f64::from(ii[q][k]) - mi[c] * mi[d];
                    let e = f64::from(EPS);
                    let s = [
                        [cov(0, 0, 0) + e, cov(1, 0, 1), cov(2, 0, 2)],
                        [cov(1, 0, 1), cov(3, 1, 1) + e, cov(4, 1, 2)],
                        [cov(2, 0, 2), cov(4, 1, 2), cov(5, 2, 2) + e],
                    ];
                    let cp = [0, 1, 2].map(|c| f64::from(ip[c][k]) - mi[c] * mp);
                    let x = solve(s, cp);
                    *a0 = x[0] as f32;
                    *a1 = x[1] as f32;
                    *a2 = x[2] as f32;
                    *b = (mp - x[0] * mi[0] - x[1] * mi[1] - x[2] * mi[2]) as f32;
                });
        }
        let [a0, a1, a2] = a;
        Self {
            a: [mean(&a0), mean(&a1), mean(&a2)],
            b: mean(&b),
            w,
            h,
        }
    }

    /// Fine rows `rows` of an `fw`×`fh` grid, given the guide there.
    fn rows(
        &self,
        rows: std::ops::Range<usize>,
        fw: usize,
        fh: usize,
        guide: &[[f32; 3]],
    ) -> Vec<f32> {
        let xs = Taps::new(fw, self.w);
        let mut out = vec![0f32; rows.len() * fw];
        out.par_chunks_mut(fw.max(1))
            .zip(guide.par_chunks(fw.max(1)))
            .enumerate()
            .for_each(|(row, (line, g))| {
                let t = Taps::one(rows.start + row, fh, self.h);
                for (x, (o, i)) in line.iter_mut().zip(g).enumerate() {
                    let at = |v: &[f32]| bilinear(v, self.w, &xs, x, t);
                    let q = at(&self.a[0]) * i[0]
                        + at(&self.a[1]) * i[1]
                        + at(&self.a[2]) * i[2]
                        + at(&self.b);
                    *o = q.clamp(0.0, 1.0);
                }
            });
        out
    }
}

/// `s⁻¹·v` for a symmetric positive definite 3×3 `s` (Cramer's rule).
fn solve(s: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(s);
    if d.abs() < 1e-30 || !d.is_finite() {
        return [0.0; 3];
    }
    [0, 1, 2].map(|c| {
        let mut m = s;
        for (r, row) in m.iter_mut().enumerate() {
            row[c] = v[r];
        }
        det(m) / d
    })
}

/// Where each fine pixel falls on the grid: its two neighbours and the
/// weight of the second (pixel centres line up, edges clamp), as
/// `layers_pixel` maps source positions to the grid.
struct Taps(Vec<(usize, usize, f32)>);

impl Taps {
    fn new(fine: usize, coarse: usize) -> Self {
        Self((0..fine).map(|i| Self::one(i, fine, coarse)).collect())
    }

    fn one(i: usize, fine: usize, coarse: usize) -> (usize, usize, f32) {
        let g = ((i as f32 + 0.5) * coarse as f32 / fine as f32 - 0.5)
            .clamp(0.0, (coarse.max(1) - 1) as f32);
        let i0 = g.floor() as usize;
        (i0, (i0 + 1).min(coarse - 1), g - i0 as f32)
    }
}

fn bilinear(v: &[f32], w: usize, xs: &Taps, x: usize, (y0, y1, fy): (usize, usize, f32)) -> f32 {
    let (x0, x1, fx) = xs.0[x];
    let top = v[y0 * w + x0] + (v[y0 * w + x1] - v[y0 * w + x0]) * fx;
    let bot = v[y1 * w + x0] + (v[y1 * w + x1] - v[y1 * w + x0]) * fx;
    top + (bot - top) * fy
}

/// A found mask (bytes on the `gw`×`gh` grid, guide `guide_lo` there)
/// upsampled to `fw`×`fh` along the fine guide (`guide(y, row)` fills fine
/// row `y`).
pub fn upsample(
    mask: &[u8],
    grid: (usize, usize),
    guide_lo: &[[f32; 3]],
    fine: (usize, usize),
    guide: &(dyn Fn(usize, &mut [[f32; 3]]) + Sync),
) -> Vec<u8> {
    let p: Vec<f32> = mask.par_iter().map(|&v| f32::from(v) / 255.0).collect();
    upsample_values(p, grid, guide_lo, fine, guide)
        .par_iter()
        .map(|v| (v * 255.0).round() as u8)
        .collect()
}

/// [`upsample`] for values 0–1 (a depth map, say), kept as floats.
pub fn upsample_values(
    values: Vec<f32>,
    (gw, gh): (usize, usize),
    guide_lo: &[[f32; 3]],
    (fw, fh): (usize, usize),
    guide: &(dyn Fn(usize, &mut [[f32; 3]]) + Sync),
) -> Vec<f32> {
    let c = Coefficients::of_values(values, guide_lo, gw, gh);
    let mut out = vec![0f32; fw * fh];
    out.par_chunks_mut(BAND * fw.max(1))
        .enumerate()
        .for_each(|(b, band)| {
            let rows = b * BAND..b * BAND + band.len() / fw.max(1);
            let mut g = vec![[0f32; 3]; band.len()];
            for (row, line) in g.chunks_mut(fw.max(1)).enumerate() {
                guide(rows.start + row, line);
            }
            band.copy_from_slice(&c.rows(rows, fw, fh, &g));
        });
    out
}

/// How a part is made on the fine grid.
enum Prep {
    /// Rasterised there.
    Shape,
    /// Read bilinearly from its coverage on the grid.
    Coarse(Vec<f32>),
    /// Upsampled along the guide.
    Guided(Arc<Coefficients>),
    /// Covers nothing (an AI part not found yet).
    Nothing,
}

/// Rasterise `locals` on the `fine` grid: AI parts' coverage from `found`
/// on the `grid` (None: not found yet) upsampled along `guide`; `tones`
/// (on the grid) when [`super::needs_tones`].
pub fn rasterise(
    locals: &[&Local],
    grid: (usize, usize),
    fine: (usize, usize),
    units: [f64; 2],
    tones: Option<&Tones>,
    found: &dyn Fn(Find) -> Option<Arc<Vec<u8>>>,
    guide: &Guide<'_>,
) -> Masks {
    let ((gw, gh), (fw, fh)) = (grid, fine);
    let gn = gw * gh;
    // What each part needs from the grid, worked out once.
    let mut guided: Vec<(Find, Option<Arc<Coefficients>>)> = Vec::new();
    let preps: Vec<Vec<Prep>> = locals
        .iter()
        .map(|l| {
            l.mask
                .parts
                .iter()
                .map(|part| {
                    let Part::Known(p) = part else {
                        return Prep::Nothing;
                    };
                    if let Some(f) = super::find_of(&p.shape) {
                        let c = match guided.iter().find(|g| g.0 == f) {
                            Some(g) => g.1.clone(),
                            None => {
                                let c = found(f)
                                    .filter(|m| m.len() == gn && guide.lo.len() == gn)
                                    .map(|m| Arc::new(Coefficients::new(&m, &guide.lo, gw, gh)));
                                guided.push((f, c.clone()));
                                c
                            }
                        };
                        return c.map_or(Prep::Nothing, Prep::Guided);
                    }
                    if matches!(
                        p.shape,
                        crate::edit::Shape::Luminance { .. } | crate::edit::Shape::Colour { .. }
                    ) {
                        return Prep::Coarse(super::shape(&p.shape, gw, gh, units, tones));
                    }
                    Prep::Shape
                })
                .collect()
        })
        .collect();
    let needs_guide = preps.iter().flatten().any(|p| matches!(p, Prep::Guided(_)));
    let n = fw * fh;
    let count = locals.len();
    let mut data = vec![0u8; n * count];
    // Each band's part of every plane.
    let bands = n.div_ceil((BAND * fw).max(1));
    let mut per_band: Vec<Vec<&mut [u8]>> = (0..bands).map(|_| Vec::new()).collect();
    for plane in data.chunks_mut(n.max(1)) {
        for (b, chunk) in plane.chunks_mut((BAND * fw).max(1)).enumerate() {
            per_band[b].push(chunk);
        }
    }
    let xs = Taps::new(fw, gw);
    per_band.into_par_iter().enumerate().for_each(|(b, outs)| {
        let y0 = b * BAND;
        let rows = y0..(y0 + BAND).min(fh);
        let len = rows.len() * fw;
        let g = needs_guide.then(|| guide.rows(rows.clone(), fw, fh));
        for ((out, l), prep) in outs.into_iter().zip(locals).zip(&preps) {
            let mut m = vec![0f32; len];
            for (part, prep) in l.mask.parts.iter().zip(prep) {
                let Part::Known(p) = part else { continue };
                let v = match prep {
                    Prep::Shape => super::shape_rows(&p.shape, fw, fh, rows.clone(), units, None),
                    Prep::Coarse(c) => {
                        let mut v = vec![0f32; len];
                        for (row, line) in v.chunks_mut(fw.max(1)).enumerate() {
                            let t = Taps::one(y0 + row, fh, gh);
                            for (x, o) in line.iter_mut().enumerate() {
                                *o = bilinear(c, gw, &xs, x, t);
                            }
                        }
                        v
                    }
                    Prep::Guided(c) => match &g {
                        Some(g) => c.rows(rows.clone(), fw, fh, g),
                        None => vec![0.0; len],
                    },
                    Prep::Nothing => vec![0.0; len],
                };
                super::combine(&mut m, &v, p);
            }
            super::quantise(out, &m, l.amount);
        }
    });
    Masks {
        width: fw,
        height: fh,
        count,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_three_by_three() {
        let s = [[4.0, 1.0, 0.5], [1.0, 3.0, 0.2], [0.5, 0.2, 2.0]];
        let x = [0.3, -1.2, 2.5];
        let v = [0, 1, 2].map(|r| (0..3).map(|c| s[r][c] * x[c]).sum::<f64>());
        let got = solve(s, v);
        for c in 0..3 {
            assert!((got[c] - x[c]).abs() < 1e-12, "{got:?}");
        }
    }
}
