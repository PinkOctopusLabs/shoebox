//! Local adjustment masks (EDITING-SPEC, Local adjustments): each layer's
//! parts rasterised on the CPU into an 8-bit plane on a grid over the
//! stored image, which the live pipe samples (bilinear) at each pixel's
//! source position, as it does the local maps. The CPU reference and the
//! shader read the same bytes, so they agree.
//!
//! The grid's long edge is at most [`LONG`] whatever the source, so a
//! mask is the same on the proxy, in 100% regions and in an export. Parts
//! that depend on the picture (luminance and colour ranges) read
//! [`Tones`] on that grid. For sources larger than the grid with AI parts
//! (exports, 100% regions), [`fine`] rasterises on a finer grid, the AI
//! parts upsampled by a guided filter on the full-size source.
//!
//! What the AI finders return is kept on disk by [`store`].

pub mod fine;
pub mod store;

use rayon::prelude::*;

use crate::edit::{Local, MaskPart, Op, Part, Shape, Stroke};

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

/// What an AI part finds (an object by a hash of its clicks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Find {
    Subject,
    Person,
    Object(u64),
    Face(crate::edit::FacePart),
}

/// Finds the object at clicks in an upright 8-bit sRGB image (`rgb`,
/// width, height; clicks in its pixels, true to add): coverage per pixel.
pub type Segmenter =
    dyn Fn(&[u8], usize, usize, &[(f32, f32, bool)]) -> Result<Vec<u8>, String> + Send + Sync;

/// A registered finder and its identity (the model and its version:
/// results are kept on disk by it; empty keeps nothing).
struct Registered<F: ?Sized> {
    id: String,
    run: Box<F>,
}

static SEGMENTER: std::sync::OnceLock<Registered<Segmenter>> = std::sync::OnceLock::new();

/// Give the masks a way to find objects at clicks (the app registers SAM
/// 2.1), named `id` (the model's identity, e.g. `sam::MODEL_ID`: masks
/// are kept on disk by it). Once.
pub fn register_segmenter(id: &str, f: Box<Segmenter>) {
    let _ = SEGMENTER.set(Registered {
        id: id.to_owned(),
        run: f,
    });
}

/// Find the object at `clicks` with the registered segmenter (or read
/// what it found before from the [`store`]).
pub fn segment(
    rgb: &[u8],
    w: usize,
    h: usize,
    clicks: &[(f32, f32, bool)],
) -> Result<Vec<u8>, String> {
    match SEGMENTER.get() {
        Some(f) => store::through(&f.id, "object", clicks, rgb, w, h, || {
            (f.run)(rgb, w, h, clicks)
        }),
        None => Err("no object finder in this build".into()),
    }
}

/// An object part's key: a hash of its clicks.
pub fn object_key(clicks: &[crate::edit::ObjectClick]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in clicks {
        c.at[0].to_bits().hash(&mut h);
        c.at[1].to_bits().hash(&mut h);
        c.add.hash(&mut h);
    }
    h.finish()
}

/// Finds a [`Find`] in an upright 8-bit sRGB image (`rgb`, width, height):
/// a coverage byte per pixel, or why it couldn't.
pub type Finder = dyn Fn(&[u8], usize, usize, Find) -> Result<Vec<u8>, String> + Send + Sync;

static FINDER: std::sync::OnceLock<Registered<Finder>> = std::sync::OnceLock::new();

/// Give the masks a way to find subjects and people (the app registers
/// Apple's Vision; `shoebox-develop` doesn't depend on it), named `id`
/// (what finds them and its version, e.g. the OS build: masks are kept on
/// disk by it; empty keeps nothing). Once.
pub fn register_finder(id: &str, f: Box<Finder>) {
    let _ = FINDER.set(Registered {
        id: id.to_owned(),
        run: f,
    });
}

/// Find `what` with the registered finder (or read what it found before
/// from the [`store`]).
pub fn find(rgb: &[u8], w: usize, h: usize, what: Find) -> Result<Vec<u8>, String> {
    use crate::edit::FacePart as P;
    let kind = match what {
        Find::Subject => "subject",
        Find::Person => "person",
        Find::Face(P::Skin) => "face-skin",
        Find::Face(P::Eyes) => "face-eyes",
        Find::Face(P::Eyebrows) => "face-eyebrows",
        Find::Face(P::Lips) => "face-lips",
        Find::Face(P::Teeth) => "face-teeth",
        // Objects are the segmenter's (their clicks are its key).
        Find::Object(_) => "",
    };
    match FINDER.get() {
        Some(f) if !kind.is_empty() => {
            store::through(&f.id, kind, &[], rgb, w, h, || (f.run)(rgb, w, h, what))
        }
        Some(f) => (f.run)(rgb, w, h, what),
        None => Err("no subject finder in this build".into()),
    }
}

/// The [`Find`] an AI part reads (None: not an AI part).
pub fn find_of(shape: &Shape) -> Option<Find> {
    Some(match shape {
        Shape::Subject => Find::Subject,
        Shape::Person => Find::Person,
        Shape::Object { clicks } => Find::Object(object_key(clicks)),
        Shape::Face { part } => Find::Face(*part),
        _ => return None,
    })
}

/// Finds the faces in an upright 8-bit sRGB image (`rgb`, width, height):
/// their boxes `[x0, y0, x1, y1]` in its pixels.
pub type FaceFinder = dyn Fn(&[u8], usize, usize) -> Result<Vec<[f32; 4]>, String> + Send + Sync;

static FACES: std::sync::OnceLock<Box<FaceFinder>> = std::sync::OnceLock::new();

/// Give Develop a way to find faces' boxes (Smart Lighting's spot; the app
/// registers Apple's Vision). Once.
pub fn register_face_finder(f: Box<FaceFinder>) {
    let _ = FACES.set(f);
}

/// Find faces' boxes with the registered face finder.
pub fn find_faces(rgb: &[u8], w: usize, h: usize) -> Result<Vec<[f32; 4]>, String> {
    match FACES.get() {
        Some(f) => f(rgb, w, h),
        None => Err("no face finder in this build".into()),
    }
}

/// The AI parts these layers have (with an object's clicks).
pub fn finds(locals: &[&Local]) -> Vec<(Find, Vec<crate::edit::ObjectClick>)> {
    let mut out: Vec<(Find, Vec<crate::edit::ObjectClick>)> = Vec::new();
    for l in locals {
        for p in &l.mask.parts {
            let Part::Known(m) = p else { continue };
            let f = match &m.shape {
                Shape::Subject => (Find::Subject, Vec::new()),
                Shape::Person => (Find::Person, Vec::new()),
                Shape::Object { clicks } if !clicks.is_empty() => {
                    (Find::Object(object_key(clicks)), clicks.clone())
                }
                Shape::Face { part } => (Find::Face(*part), Vec::new()),
                _ => continue,
            };
            if !out.iter().any(|o| o.0 == f.0) {
                out.push(f);
            }
        }
    }
    out
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
    rasterise_with(locals, width, height, units, tones, &|_| None)
}

/// [`rasterise`], with AI parts' coverage on the grid from `found` (None:
/// not found yet, so they cover nothing).
pub fn rasterise_with(
    locals: &[&Local],
    width: usize,
    height: usize,
    units: [f64; 2],
    tones: Option<&Tones>,
    found: &dyn Fn(Find) -> Option<std::sync::Arc<Vec<u8>>>,
) -> Masks {
    let n = width * height;
    let mut data = vec![0u8; n * locals.len()];
    for (plane, l) in data.chunks_mut(n.max(1)).zip(locals) {
        let mut m = vec![0f32; n];
        for part in &l.mask.parts {
            let Part::Known(p) = part else { continue };
            let v = match find_of(&p.shape) {
                Some(f) => match found(f).filter(|c| c.len() == n) {
                    Some(c) => c.iter().map(|&b| f32::from(b) / 255.0).collect(),
                    None => vec![0.0; n],
                },
                None => shape(&p.shape, width, height, units, tones),
            };
            combine(&mut m, &v, p);
        }
        quantise(plane, &m, l.amount);
    }
    Masks {
        width,
        height,
        count: locals.len(),
        data,
    }
}

/// A part on a layer's coverage so far (`m`), by its op and invert.
pub(crate) fn combine(m: &mut [f32], v: &[f32], p: &MaskPart) {
    m.par_iter_mut().zip(v).for_each(|(m, &v)| {
        let v = if p.invert { 1.0 - v } else { v };
        *m = match p.op {
            Op::Add => m.max(v),
            Op::Subtract => *m * (1.0 - v),
            Op::Intersect => *m * v,
        };
    });
}

/// A layer's coverage, times its amount (0–100), to bytes.
pub(crate) fn quantise(plane: &mut [u8], m: &[f32], amount: f64) {
    let amount = (amount / 100.0).clamp(0.0, 1.0) as f32;
    plane
        .par_iter_mut()
        .zip(m)
        .for_each(|(o, v)| *o = ((v * amount).clamp(0.0, 1.0) * 255.0).round() as u8);
}

/// One part's coverage, 0–1 per grid pixel.
fn shape(s: &Shape, w: usize, h: usize, units: [f64; 2], tones: Option<&Tones>) -> Vec<f32> {
    shape_rows(s, w, h, 0..h, units, tones)
}

/// One part's coverage on rows `rows` of a `w`×`h` grid, 0–1 per pixel
/// (`tones`, when given, cover the whole grid).
pub(crate) fn shape_rows(
    s: &Shape,
    w: usize,
    h: usize,
    rows: std::ops::Range<usize>,
    units: [f64; 2],
    tones: Option<&Tones>,
) -> Vec<f32> {
    let (y0, n) = (rows.start, rows.len() * w);
    // A grid pixel's centre in the long edge's units.
    let at = move |x: usize, y: usize| {
        [
            (x as f64 + 0.5) / w as f64 * units[0],
            (y as f64 + 0.5) / h as f64 * units[1],
        ]
    };
    let each = |f: &(dyn Fn([f64; 2], usize) -> f64 + Sync)| -> Vec<f32> {
        let mut v = vec![0f32; n];
        v.par_chunks_mut(w.max(1))
            .enumerate()
            .for_each(|(row, line)| {
                let y = y0 + row;
                for (x, o) in line.iter_mut().enumerate() {
                    *o = f(at(x, y), y * w + x) as f32;
                }
            });
        v
    };
    let tones = tones.filter(|t| t.shown.len() == w * h);
    match s {
        Shape::Linear { from, to } => {
            let d = [to[0] - from[0], to[1] - from[1]];
            let len2 = d[0] * d[0] + d[1] * d[1];
            if len2 <= 0.0 {
                return vec![0.0; n];
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
        Shape::Brush { strokes } => brush(strokes, w, rows, units),
        Shape::Luminance { range, smooth } => {
            let Some(t) = tones else {
                return vec![0.0; n];
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
            let Some(t) = tones.filter(|t| t.hue.len() == w * h && t.chroma.len() == w * h) else {
                return vec![0.0; n];
            };
            let width = width.max(1.0);
            each(&|_, i| {
                let d = (f64::from(t.hue[i]) - hue).rem_euclid(360.0);
                let d = d.min(360.0 - d);
                (1.0 - smoothstep(width, 2.0 * width, d))
                    * smoothstep(0.02, 0.08, f64::from(t.chroma[i]))
            })
        }
        // Read from what was found (see `rasterise_with`).
        Shape::Subject | Shape::Person | Shape::Object { .. } | Shape::Face { .. } => {
            vec![0.0; n]
        }
    }
}

/// Strokes in order: within a stroke the strongest dab counts (so a stroke
/// doesn't build up over itself); strokes build on each other by their
/// flow, and erasing strokes take away.
/// On rows `rows` of a grid `w` wide.
fn brush(strokes: &[Stroke], w: usize, rows: std::ops::Range<usize>, units: [f64; 2]) -> Vec<f32> {
    let mut b = vec![0f32; w * rows.len()];
    // Grid pixels per unit.
    let k = w as f64 / units[0].max(1e-9);
    for s in strokes {
        if s.points.is_empty() {
            continue;
        }
        // At most a long edge (edit files are JSON anyone can write).
        let radius = if s.radius.is_finite() {
            s.radius.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let r = (radius * k).max(0.5);
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
        let clampy = |v: f64| (v.max(0.0) as usize).clamp(rows.start, rows.end);
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
        b[(by0 - rows.start) * w..(by1 - rows.start) * w]
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
