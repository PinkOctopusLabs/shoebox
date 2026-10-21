//! Local adjustment masks (EDITING-SPEC, Local adjustments): each layer's
//! parts rasterised on the CPU into an 8-bit plane on a grid over the
//! stored image, which the live pipe samples (bilinear) at each pixel's
//! source position, as it does the local maps. The CPU reference and the
//! shader read the same bytes, so they agree.
//!
//! The grid's long edge is at most [`LONG`] whatever the source, so a
//! mask is the same on the proxy, in 100% regions and in an export. Parts
//! that depend on the picture (luminance and colour ranges) read
//! [`Tones`] on that grid.

use rayon::prelude::*;

use crate::edit::{Local, Op, Part, Shape, Stroke};

/// The grid's longest edge.
pub const LONG: usize = 2560;

/// Every layer's mask, `width`×`height` bytes each, one after another.
#[derive(Debug, Clone, PartialEq)]
pub struct Masks {
    pub width: usize,
    pub height: usize,
    pub count: usize,
    pub data: Vec<u8>,
}

/// The grid for a `src_w`×`src_h` source.
pub fn grid(src_w: usize, src_h: usize) -> (usize, usize) {
    let long = src_w.max(src_h).max(1);
    if long <= LONG {
        return (src_w.max(1), src_h.max(1));
    }
    let k = LONG as f64 / long as f64;
    (
        ((src_w as f64 * k).round() as usize).max(1),
        ((src_h as f64 * k).round() as usize).max(1),
    )
}

/// What range parts read, per grid pixel: brightness as shown (0–1), Oklab
/// hue (degrees) and chroma relative to lightness.
#[derive(Debug, Clone, Default)]
pub struct Tones {
    pub shown: Vec<f32>,
    pub hue: Vec<f32>,
    pub chroma: Vec<f32>,
}

/// Whether any of these layers has a part that reads the picture.
pub fn needs_tones(locals: &[&Local]) -> bool {
    locals.iter().any(|l| {
        l.mask.parts.iter().any(|p| {
            matches!(
                p,
                Part::Known(m) if matches!(m.shape, Shape::Luminance { .. } | Shape::Colour { .. })
            )
        })
    })
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rasterise `locals` on a `width`×`height` grid over a stored image whose
/// sides are `units` long edges (`[w/long, h/long]`). `tones` is needed
/// when [`needs_tones`].
pub fn rasterise(
    locals: &[&Local],
    width: usize,
    height: usize,
    units: [f64; 2],
    tones: Option<&Tones>,
) -> Masks {
    let n = width * height;
    let mut data = vec![0u8; n * locals.len()];
    for (plane, l) in data.chunks_mut(n.max(1)).zip(locals) {
        let mut m = vec![0f32; n];
        for part in &l.mask.parts {
            let Part::Known(p) = part else { continue };
            let v = shape(&p.shape, width, height, units, tones);
            m.par_iter_mut().zip(&v).for_each(|(m, &v)| {
                let v = if p.invert { 1.0 - v } else { v };
                *m = match p.op {
                    Op::Add => m.max(v),
                    Op::Subtract => *m * (1.0 - v),
                    Op::Intersect => *m * v,
                };
            });
        }
        let amount = (l.amount / 100.0).clamp(0.0, 1.0) as f32;
        plane
            .par_iter_mut()
            .zip(&m)
            .for_each(|(o, v)| *o = ((v * amount).clamp(0.0, 1.0) * 255.0).round() as u8);
    }
    Masks {
        width,
        height,
        count: locals.len(),
        data,
    }
}

/// One part's coverage, 0–1 per grid pixel.
fn shape(s: &Shape, w: usize, h: usize, units: [f64; 2], tones: Option<&Tones>) -> Vec<f32> {
    // A grid pixel's centre in the long edge's units.
    let at = move |x: usize, y: usize| {
        [
            (x as f64 + 0.5) / w as f64 * units[0],
            (y as f64 + 0.5) / h as f64 * units[1],
        ]
    };
    let each = |f: &(dyn Fn([f64; 2], usize) -> f64 + Sync)| -> Vec<f32> {
        let mut v = vec![0f32; w * h];
        v.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
            for (x, o) in line.iter_mut().enumerate() {
                *o = f(at(x, y), y * w + x) as f32;
            }
        });
        v
    };
    match s {
        Shape::Linear { from, to } => {
            let d = [to[0] - from[0], to[1] - from[1]];
            let len2 = d[0] * d[0] + d[1] * d[1];
            if len2 <= 0.0 {
                return vec![0.0; w * h];
            }
            each(&|p, _| {
                let t = ((p[0] - from[0]) * d[0] + (p[1] - from[1]) * d[1]) / len2;
                1.0 - smoothstep(0.0, 1.0, t)
            })
        }
        Shape::Radial {
            centre,
            radii,
            angle,
            feather,
        } => {
            let (sin, cos) = angle.to_radians().sin_cos();
            let (rx, ry) = (radii[0].abs().max(1e-9), radii[1].abs().max(1e-9));
            let inner = 1.0 - feather.clamp(0.0, 1.0);
            each(&|p, _| {
                let (dx, dy) = (p[0] - centre[0], p[1] - centre[1]);
                // Into the ellipse's own axes (turned back by its angle).
                let (qx, qy) = (cos * dx + sin * dy, -sin * dx + cos * dy);
                let r = ((qx / rx).powi(2) + (qy / ry).powi(2)).sqrt();
                1.0 - smoothstep(inner, 1.0, r)
            })
        }
        Shape::Brush { strokes } => brush(strokes, w, h, units),
        Shape::Luminance { range, smooth } => {
            let Some(t) = tones else {
                return vec![0.0; w * h];
            };
            let (lo, hi, s) = (range[0], range[1], smooth.max(0.0));
            each(&|_, i| {
                let l = f64::from(t.shown[i]);
                let below = if lo <= 0.0 {
                    1.0
                } else {
                    smoothstep(lo - s, lo, l)
                };
                let above = if hi >= 1.0 {
                    1.0
                } else {
                    1.0 - smoothstep(hi, hi + s, l)
                };
                below * above
            })
        }
        Shape::Colour { hue, width } => {
            let Some(t) = tones else {
                return vec![0.0; w * h];
            };
            let width = width.max(1.0);
            each(&|_, i| {
                let d = (f64::from(t.hue[i]) - hue).rem_euclid(360.0);
                let d = d.min(360.0 - d);
                (1.0 - smoothstep(width, 2.0 * width, d))
                    * smoothstep(0.02, 0.08, f64::from(t.chroma[i]))
            })
        }
    }
}

/// Strokes in order: within a stroke the strongest dab counts (so a stroke
/// doesn't build up over itself); strokes build on each other by their
/// flow, and erasing strokes take away.
fn brush(strokes: &[Stroke], w: usize, h: usize, units: [f64; 2]) -> Vec<f32> {
    let mut b = vec![0f32; w * h];
    // Grid pixels per unit.
    let k = w as f64 / units[0].max(1e-9);
    for s in strokes {
        if s.points.is_empty() {
            continue;
        }
        let r = (s.radius * k).max(0.5);
        let inner = r * (1.0 - s.feather.clamp(0.0, 1.0));
        let flow = s.flow.clamp(0.0, 1.0) as f32;
        let pts: Vec<[f64; 2]> = s
            .points
            .iter()
            .map(|p| [p[0] * k - 0.5, p[1] * k - 0.5])
            .collect();
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in &pts {
            x0 = x0.min(p[0] - r);
            y0 = y0.min(p[1] - r);
            x1 = x1.max(p[0] + r);
            y1 = y1.max(p[1] + r);
        }
        let clampx = |v: f64| (v.max(0.0) as usize).min(w);
        let clampy = |v: f64| (v.max(0.0) as usize).min(h);
        let (bx0, bx1) = (clampx(x0.floor()), clampx(x1.ceil() + 1.0));
        let (by0, by1) = (clampy(y0.floor()), clampy(y1.ceil() + 1.0));
        if bx0 >= bx1 || by0 >= by1 {
            continue;
        }
        let segs: Vec<([f64; 2], [f64; 2])> = if pts.len() == 1 {
            vec![(pts[0], pts[0])]
        } else {
            pts.windows(2).map(|p| (p[0], p[1])).collect()
        };
        b[by0 * w..by1 * w]
            .par_chunks_mut(w)
            .enumerate()
            .for_each(|(row, line)| {
                let y = (by0 + row) as f64;
                // The stroke's coverage of this row: each nearby segment's
                // strongest dab, over just the pixels it can reach.
                let mut cover = vec![0f32; bx1 - bx0];
                for (a, b) in &segs {
                    if y < a[1].min(b[1]) - r || y > a[1].max(b[1]) + r {
                        continue;
                    }
                    let lo = clampx((a[0].min(b[0]) - r).floor()).max(bx0);
                    let hi = clampx((a[0].max(b[0]) + r).ceil() + 1.0).min(bx1);
                    for x in lo..hi {
                        let d = segment_distance([x as f64, y], *a, *b);
                        if d < r {
                            let v = (1.0 - smoothstep(inner, r, d)) as f32;
                            let c = &mut cover[x - bx0];
                            *c = c.max(v);
                        }
                    }
                }
                for (o, c) in line[bx0..bx1].iter_mut().zip(cover) {
                    if c <= 0.0 {
                        continue;
                    }
                    let v = c * flow;
                    *o = if s.erase {
                        *o * (1.0 - v)
                    } else {
                        1.0 - (1.0 - *o) * (1.0 - v)
                    };
                }
            });
    }
    b
}

fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
}

/// Layer `layer`'s mask at grid position `(x, y)` (bilinear, clamped to
/// the edges), 0–1.
pub fn sample(m: &Masks, layer: usize, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, (m.width - 1) as f32);
    let y = y.clamp(0.0, (m.height - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(m.width - 1), (y0 + 1).min(m.height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let plane = &m.data[layer * m.width * m.height..];
    let at = |xx: usize, yy: usize| f32::from(plane[yy * m.width + xx]) / 255.0;
    let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
    let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
    top + (bot - top) * fy
}
