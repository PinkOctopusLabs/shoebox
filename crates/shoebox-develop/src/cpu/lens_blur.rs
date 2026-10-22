//! Lens Blur (EDITING-SPEC, Phase 4 additions): a synthetic depth of
//! field from a depth map (see [`crate::depth`]), in the source stage, so
//! in scene-referred linear light: highlights bloom as they would through
//! a lens, and everything after (the colour matrix, tone, the display
//! transform) sees the blurred scene.
//!
//! - **Circle of confusion.** Depth maps are relative inverse depth, and a
//!   lens's blur circle grows with the difference in inverse depth from
//!   the focus: `c = amount · max(|d − focus| − range/2, 0)` (signed:
//!   positive nearer than the focus), sized as a fraction of the long edge
//!   so the proxy, 100% and an export agree.
//! - **A smaller working copy.** The blur is gathered on a copy whose long
//!   edge puts the largest circle at most [`REACH`] of its pixels (between
//!   [`WORK_MIN`] and [`WORK_MAX`] long), area-averaged from the source
//!   with highlights boosted, each pixel with its circle. A large blur
//!   hides detail anyway, and it keeps the cost fixed: about 850 taps a
//!   pixel at most, whatever the photo's size.
//! - **Gathered as if scattered**, two layers (after Jimenez's and
//!   Sousa's game renderers): each neighbour within its own circle covers
//!   the pixel, weighted by one over its circle's area (its light spread
//!   over it). Neighbours clearly nearer and blurrier go to a front layer
//!   whose coverage is its alpha; the rest form the back, where one
//!   farther than the pixel reaches it no further than the pixel's own
//!   circle, so a sharp subject never takes the background's blur, and the
//!   background never takes the subject's colour (no halos).
//! - **Back to the source's pixels** as each source pixel is read: the
//!   sharp source mixed with the blurred back layer by how blurred the
//!   pixel is (its own circle, in working pixels; read bilaterally, the
//!   working pixels weighted by how like the pixel's circle theirs is, so
//!   across a depth edge the other side doesn't bleed in), then the front
//!   layer over it. A pixel in focus is the source exactly.
//!
//! The GPU runs the same two passes (`gpu::lens_blur`) and the same mix in
//! its `texel`.

use std::sync::{Arc, OnceLock};

use rayon::prelude::*;

use super::{Demosaiced, Display, Rgb, layers::Layers};
use crate::edit::{Bokeh, Settings};

/// The blur circle's radius, as a fraction of the long edge, at Amount 100
/// and a whole depth map from the focus.
pub const MAX_BLUR: f32 = 0.04;
/// The largest circle in working pixels.
pub const REACH: f32 = 16.0;
/// The working copy's long edge, at most and at least (unless the source
/// is smaller).
pub const WORK_MAX: usize = 1024;
pub const WORK_MIN: usize = 256;
/// Blades of the bladed aperture.
pub const BLADES: usize = 6;
/// A clipped highlight's gain at full boost (it fades in from 40% of
/// clipping).
pub const BOOST: f32 = 4.0;
/// Area table steps per working pixel of radius.
pub const AREA_STEPS: f32 = 8.0;

/// A depth map refined by local adjustments, on the masks' grid in the
/// stored image's orientation, 0–1 (1 nearest).
#[derive(Debug, Clone, PartialEq)]
pub struct Depth {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    /// The depth focused on when no point is chosen: the nearest tenth's.
    pub nearest: f32,
}

impl Depth {
    pub fn new(width: usize, height: usize, data: Vec<f32>) -> Self {
        let nearest = percentile(&data, 0.9);
        Self {
            width,
            height,
            data,
            nearest,
        }
    }

    /// Bilinear at grid position `(x, y)`, clamped to the edges (as
    /// `mask::sample`).
    pub fn at(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.width - 1) as f32);
        let y = y.clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let v = |xx: usize, yy: usize| self.data[yy * self.width + xx];
        let top = v(x0, y0) + (v(x1, y0) - v(x0, y0)) * fx;
        let bot = v(x0, y1) + (v(x1, y1) - v(x0, y1)) * fx;
        top + (bot - top) * fy
    }
}

/// The value below which a share `q` of `v` lies (by a 1024-bin
/// histogram of 0–1).
fn percentile(v: &[f32], q: f32) -> f32 {
    const BINS: usize = 1024;
    let mut h = [0usize; BINS];
    for x in v {
        h[((x.clamp(0.0, 1.0) * (BINS - 1) as f32).round()) as usize] += 1;
    }
    let want = (v.len() as f32 * q) as usize;
    let mut seen = 0;
    for (i, n) in h.iter().enumerate() {
        seen += n;
        if seen > want {
            return i as f32 / (BINS - 1) as f32;
        }
    }
    1.0
}

/// The neighbours a working pixel gathers from, nearest first: offsets and
/// their distance in the aperture's shape (a circle's radius; for blades,
/// scaled so the hexagon's corners are at its radius), and the area a
/// circle of each radius covers (in [`AREA_STEPS`]).
#[derive(Debug)]
pub struct Taps {
    pub list: Vec<[f32; 3]>,
    pub area: Vec<f32>,
}

impl Taps {
    fn new(blades: usize) -> Self {
        let r = REACH as i32 + 1;
        let mut list = Vec::new();
        for dy in -r..=r {
            for dx in -r..=r {
                let d = (dx as f32).hypot(dy as f32);
                let e = if blades > 0 && d > 0.0 {
                    // A regular polygon with a corner at the top: its edge
                    // is `cos(π/n) / cos(θ′)` of the radius, θ′ the angle
                    // from the nearest edge's middle.
                    let n = blades as f32;
                    let seg = std::f32::consts::TAU / n;
                    let t = (dy as f32).atan2(dx as f32) + std::f32::consts::FRAC_PI_2;
                    let a = t.rem_euclid(seg) - seg / 2.0;
                    d * a.cos() / (std::f32::consts::PI / n).cos()
                } else {
                    d
                };
                if e <= REACH + 0.5 {
                    list.push([dx as f32, dy as f32, e]);
                }
            }
        }
        list.sort_by(|a, b| {
            a[2].total_cmp(&b[2])
                .then(a[1].total_cmp(&b[1]))
                .then(a[0].total_cmp(&b[0]))
        });
        let steps = ((REACH + 2.0) * AREA_STEPS) as usize;
        let area = (0..=steps)
            .map(|i| {
                let r = (i as f32 / AREA_STEPS).max(0.5);
                list.iter().map(|t| cover(r, t[2])).sum::<f32>()
            })
            .collect();
        Self { list, area }
    }

    /// The taps for a shape, made once.
    pub fn of(bokeh: Bokeh) -> Arc<Taps> {
        static CIRCLE: OnceLock<Arc<Taps>> = OnceLock::new();
        static BLADED: OnceLock<Arc<Taps>> = OnceLock::new();
        match bokeh {
            Bokeh::Circle => CIRCLE.get_or_init(|| Arc::new(Taps::new(0))).clone(),
            Bokeh::Blades => BLADED.get_or_init(|| Arc::new(Taps::new(BLADES))).clone(),
        }
    }

    /// How many taps reach `r` (they're nearest first).
    pub fn within(&self, r: f32) -> usize {
        self.list.partition_point(|t| t[2] <= r + 0.5)
    }

    /// The area a circle of radius `r` covers (interpolated).
    pub fn area(&self, r: f32) -> f32 {
        let x = r.max(0.5) * AREA_STEPS;
        let last = (self.area.len() - 2) as f32;
        let i = x.floor().min(last);
        let t = x - i;
        let i = i as usize;
        self.area[i] + (self.area[i + 1] - self.area[i]) * t
    }
}

/// How much a neighbour whose circle has radius `r` covers a pixel `d`
/// away (its edge antialiased over a pixel).
pub fn cover(r: f32, d: f32) -> f32 {
    (r.max(0.5) + 0.5 - d).clamp(0.0, 1.0)
}

/// The highlight boost's gain for a source pixel.
pub fn boosted(p: [f32; 3], boost: f32) -> [f32; 3] {
    if boost <= 0.0 {
        return p;
    }
    let m = p[0].max(p[1]).max(p[2]);
    let t = ((m - 0.4) / 0.6).clamp(0.0, 1.0);
    let k = 1.0 + BOOST * boost * t * t * (3.0 - 2.0 * t);
    p.map(|v| v * k)
}

/// Everything the blur needs for one render, worked out from the
/// settings, the depth map and the layers.
#[derive(Debug, Clone)]
pub struct Stage {
    pub depth: Arc<Depth>,
    /// Source pixels → depth grid pixels.
    pub dscale: [f32; 2],
    /// The depth in focus, and half the focus range.
    pub focus: f32,
    pub half: f32,
    /// Working pixels of circle radius per depth unit past the range.
    pub coc: f32,
    /// 0–1.
    pub boost: f32,
    pub taps: Arc<Taps>,
    /// How many taps the largest circle reaches.
    pub count: usize,
    /// The source's size and the working copy's.
    pub src: (usize, usize),
    pub work: (usize, usize),
    /// Source pixels per working pixel.
    pub per: [f32; 2],
}

/// The span of `n` source pixels working pixel `o` of `m` averages.
pub fn span(o: usize, n: usize, m: usize) -> std::ops::Range<usize> {
    let a = o * n / m;
    a..((o + 1) * n).div_ceil(m).max(a + 1)
}

impl Stage {
    /// The signed circle of confusion at depth `d`, in working pixels
    /// (positive: nearer than the focus).
    pub fn coc(&self, d: f32) -> f32 {
        let e = d - self.focus;
        let m = (e.abs() - self.half).max(0.0) * self.coc;
        if e >= 0.0 { m } else { -m }
    }

    /// The depth at source position `(x, y)` (pixel indices).
    pub fn depth_at(&self, x: f32, y: f32) -> f32 {
        self.depth.at(
            (x + 0.5) * self.dscale[0] - 0.5,
            (y + 0.5) * self.dscale[1] - 0.5,
        )
    }

    /// How many working pixels the largest circle reaches.
    pub fn reach(&self) -> f32 {
        self.coc
    }
}

/// The Lens Blur stage for these settings, or None: off, no blur, or the
/// depth map isn't there yet. `layers` (with their masks) refine the
/// depth map.
pub fn stage(
    src: &Demosaiced,
    colour: &[[f32; 3]; 3],
    display: &Display,
    s: &Settings,
    orientation: u16,
    layers: &Layers,
) -> Option<Stage> {
    let lb = &s.modules.lens_blur;
    if !lb.enabled {
        return None;
    }
    let base = crate::depth::map(src, colour, display, orientation)?;
    let (sw, sh) = (src.rgb.width, src.rgb.height);
    let (gw, gh) = crate::mask::grid(sw, sh);
    if base.len() != gw * gh {
        return None;
    }
    let depth = refine(src, base, (gw, gh), layers);
    let long = sw.max(sh).max(1) as f32;
    let focus = match lb.focus {
        Some(at) => {
            let glong = gw.max(gh) as f32;
            depth.at(at[0] as f32 * glong - 0.5, at[1] as f32 * glong - 0.5)
        }
        None => depth.nearest,
    };
    let blur = (lb.amount / 100.0).clamp(0.0, 1.0) as f32 * MAX_BLUR;
    let work_long = if blur > 0.0 {
        let w = ((REACH / blur / 16.0).round() * 16.0) as usize;
        w.clamp(WORK_MIN, WORK_MAX).min(sw.max(sh))
    } else {
        WORK_MIN.min(sw.max(sh))
    };
    let k = work_long as f32 / long;
    let (ww, wh) = (
        ((sw as f32 * k).round() as usize).clamp(1, sw.max(1)),
        ((sh as f32 * k).round() as usize).clamp(1, sh.max(1)),
    );
    let coc = blur * work_long as f32;
    let taps = Taps::of(lb.bokeh);
    let count = taps.within(coc);
    Some(Stage {
        dscale: [gw as f32 / sw as f32, gh as f32 / sh as f32],
        focus,
        half: (lb.range / 200.0).clamp(0.0, 0.5) as f32,
        coc,
        boost: (lb.boost / 100.0).clamp(0.0, 1.0) as f32,
        count,
        taps,
        src: (sw, sh),
        work: (ww, wh),
        per: [sw as f32 / ww as f32, sh as f32 / wh as f32],
        depth,
    })
}

/// The depth map with each layer's Depth pushed in under its mask, in
/// order, kept to 0–1. Kept on the source while nothing it's made from
/// changes.
fn refine(
    src: &Demosaiced,
    base: Arc<Vec<f32>>,
    (gw, gh): (usize, usize),
    layers: &Layers,
) -> Arc<Depth> {
    use std::hash::{Hash, Hasher};
    let pushes: Vec<(usize, f32)> = layers
        .list
        .iter()
        .enumerate()
        .filter(|(_, l)| l.depth != 0.0)
        .map(|(i, l)| (i, l.depth))
        .collect();
    let masks = layers.masks.as_ref().filter(|_| !pushes.is_empty());
    let key = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        Arc::as_ptr(&base).hash(&mut h);
        masks.map(Arc::as_ptr).hash(&mut h);
        for (i, d) in &pushes {
            (i, d.to_bits()).hash(&mut h);
        }
        (gw, gh).hash(&mut h);
        h.finish()
    };
    let from = (base.clone(), masks.cloned());
    src.cache.refined(key, from, || {
        let Some(masks) = masks else {
            return Depth::new(gw, gh, (*base).clone());
        };
        let (sw, sh) = (src.rgb.width as f32, src.rgb.height as f32);
        let mut data = (*base).clone();
        data.par_chunks_mut(gw).enumerate().for_each(|(y, line)| {
            for (x, d) in line.iter_mut().enumerate() {
                // This grid pixel's source position, then the masks'.
                let sx = (x as f32 + 0.5) * sw / gw as f32 - 0.5;
                let sy = (y as f32 + 0.5) * sh / gh as f32 - 0.5;
                let (mx, my) = (
                    (sx + 0.5) * layers.scale[0] - 0.5,
                    (sy + 0.5) * layers.scale[1] - 0.5,
                );
                for &(i, push) in &pushes {
                    let m = crate::mask::sample(masks, i, mx, my);
                    if m > 0.0 {
                        *d = (*d + m * push).clamp(0.0, 1.0);
                    }
                }
            }
        });
        Depth::new(gw, gh, data)
    })
}

/// The blur's two layers on the working grid, RGBA each: the back
/// (normalised colour, and the pixel's own signed circle in alpha) and the
/// front (colour times its alpha, and the alpha).
#[derive(Debug, Clone)]
pub struct Blurred {
    pub width: usize,
    pub height: usize,
    pub back: Vec<[f32; 4]>,
    pub front: Vec<[f32; 4]>,
}

/// How much a pixel whose circle differs by `d` (working pixels) from
/// another's counts beside it: in averaging the working copy and reading
/// it back, so the two sides of a depth edge keep apart.
pub fn alike(d: f32) -> f32 {
    1.0 / (1.0 + d * d)
}

/// The working copy: each working pixel's circle, and the source averaged
/// over it with highlights boosted. Its circle is its nearest source
/// pixel's (the largest signed circle), and each source pixel counts by
/// how like that its own is, so a working pixel on a depth edge is the
/// near side's, colour and circle, not a mix of the two at a depth neither
/// has (which would spread the subject's colour onto the background).
/// (Not the circle at its centre, nor the most common: those jump between
/// the sides as the depth moves a little; this moves with it.)
pub fn prepare(img: &Rgb, st: &Stage) -> Vec<[f32; 4]> {
    let (ww, wh) = st.work;
    let (sw, sh) = (img.width, img.height);
    let mut out = vec![[0f32; 4]; ww * wh];
    out.par_chunks_mut(ww).enumerate().for_each(|(j, line)| {
        let ys = span(j, sh, wh);
        for (i, o) in line.iter_mut().enumerate() {
            let xs = span(i, sw, ww);
            let at = |x: usize, y: usize| st.coc(st.depth_at(x as f32, y as f32));
            let mut c = f32::MIN;
            for y in ys.clone() {
                for x in xs.clone() {
                    c = c.max(at(x, y));
                }
            }
            let mut sum = [0f32; 3];
            let mut n = 0f32;
            for y in ys.clone() {
                for x in xs.clone() {
                    let p = boosted(img.px(x, y), st.boost);
                    let w = alike(at(x, y) - c);
                    for k in 0..3 {
                        sum[k] += w * p[k];
                    }
                    n += w;
                }
            }
            *o = [sum[0] / n, sum[1] / n, sum[2] / n, c];
        }
    });
    out
}

/// How much nearer and blurrier (working pixels of circle) a neighbour
/// must be to go in the front layer, for a pixel whose circle is `rp`.
pub fn front_margin(rp: f32) -> f32 {
    (0.5 * rp).max(1.0)
}

/// The share of a neighbour `ahead` working pixels of circle nearer than
/// the pixel that goes in the front layer: none up to the margin, all a
/// pixel past it (a ramp, so the layers don't jump as depths move).
pub fn front_share(ahead: f32, margin: f32) -> f32 {
    (ahead - margin).clamp(0.0, 1.0)
}

/// The gather (see the module's notes).
pub fn gather(work: &[[f32; 4]], st: &Stage) -> Blurred {
    let (ww, wh) = st.work;
    let taps = &st.taps.list[..st.count.min(st.taps.list.len())];
    let mut back = vec![[0f32; 4]; ww * wh];
    let mut front = vec![[0f32; 4]; ww * wh];
    back.par_chunks_mut(ww)
        .zip(front.par_chunks_mut(ww))
        .enumerate()
        .for_each(|(j, (bl, fl))| {
            for i in 0..ww {
                let p = work[j * ww + i];
                let cp = p[3];
                let rp = cp.abs();
                let margin = front_margin(rp);
                let (mut bs, mut bw, mut fs, mut fw) = ([0f32; 3], 0f32, [0f32; 3], 0f32);
                for t in taps {
                    let (qx, qy) = (i as f32 + t[0], j as f32 + t[1]);
                    if qx < 0.0 || qy < 0.0 || qx >= ww as f32 || qy >= wh as f32 {
                        continue;
                    }
                    let q = work[qy as usize * ww + qx as usize];
                    let cq = q[3];
                    let rq = cq.abs();
                    let area = st.taps.area(rq);
                    let s = front_share(cq - cp, margin);
                    if s > 0.0 {
                        let w = s * cover(rq, t[2]) / area;
                        for c in 0..3 {
                            fs[c] += w * q[c];
                        }
                        fw += w;
                    }
                    if s < 1.0 {
                        let r = if cq < cp { rq.min(rp) } else { rq };
                        let w = (1.0 - s) * cover(r, t[2]) / area;
                        for c in 0..3 {
                            bs[c] += w * q[c];
                        }
                        bw += w;
                    }
                }
                let a = fw.min(1.0);
                let b = if bw > 0.0 {
                    [bs[0] / bw, bs[1] / bw, bs[2] / bw]
                } else {
                    [p[0], p[1], p[2]]
                };
                bl[i] = [b[0], b[1], b[2], cp];
                fl[i] = if fw > 0.0 {
                    let k = a / fw;
                    [fs[0] * k, fs[1] * k, fs[2] * k, a]
                } else {
                    [0.0; 4]
                };
            }
        });
    Blurred {
        width: ww,
        height: wh,
        back,
        front,
    }
}

/// The blur of a source image (the source the pipe reads).
pub fn blur(img: &Rgb, st: &Stage) -> Blurred {
    gather(&prepare(img, st), st)
}

/// How much of the blurred back a pixel whose circle is `t` working
/// pixels takes (a circle under a working pixel blurs less than the
/// working copy is soft).
pub fn blurred_share(t: f32) -> f32 {
    ((t - 0.25) / 0.75).clamp(0.0, 1.0)
}

/// The source pixel `v` at `(x, y)` (pixel indices) with the blur mixed in.
pub fn texel(st: &Stage, b: &Blurred, v: [f32; 3], x: f32, y: f32) -> [f32; 3] {
    let c = st.coc(st.depth_at(x, y));
    let w = blurred_share(c.abs());
    let wx = (x + 0.5) / st.per[0] - 0.5;
    let wy = (y + 0.5) / st.per[1] - 0.5;
    let base = if w > 0.0 {
        let k = back_at(b, wx, wy, c);
        [0, 1, 2].map(|i| v[i] + (k[i] - v[i]) * w)
    } else {
        v
    };
    let f = front_at(b, wx, wy);
    [0, 1, 2].map(|i| base[i] * (1.0 - f[3]) + f[i])
}

/// The four working pixels around `(x, y)` (clamped) and their bilinear
/// weights.
fn corners(w: usize, h: usize, x: f32, y: f32) -> ([usize; 4], [f32; 4]) {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    (
        [y0 * w + x0, y0 * w + x1, y1 * w + x0, y1 * w + x1],
        [
            (1.0 - fx) * (1.0 - fy),
            fx * (1.0 - fy),
            (1.0 - fx) * fy,
            fx * fy,
        ],
    )
}

/// The back layer at working position `(x, y)`, each working pixel
/// weighted by how like `c` (the pixel's circle) its own is.
pub fn back_at(b: &Blurred, x: f32, y: f32, c: f32) -> [f32; 3] {
    let (at, wt) = corners(b.width, b.height, x, y);
    let mut s = [0f32; 3];
    let mut n = 0f32;
    for k in 0..4 {
        let q = b.back[at[k]];
        let w = wt[k] * alike(q[3] - c);
        for i in 0..3 {
            s[i] += w * q[i];
        }
        n += w;
    }
    let n = n.max(1e-12);
    s.map(|v| v / n)
}

/// The front layer (premultiplied colour, alpha) at working position
/// `(x, y)`, bilinear.
pub fn front_at(b: &Blurred, x: f32, y: f32) -> [f32; 4] {
    let (at, wt) = corners(b.width, b.height, x, y);
    let mut s = [0f32; 4];
    for k in 0..4 {
        let q = b.front[at[k]];
        for i in 0..4 {
            s[i] += wt[k] * q[i];
        }
    }
    s
}

/// The depth map shown (Show depth): near light, far dark, the focus
/// range tinted amber.
pub fn shown(st: &Stage, d: f32) -> [f32; 3] {
    let g = d.clamp(0.0, 1.0);
    let sharp = (d - st.focus).abs() <= st.half;
    if sharp {
        [0, 1, 2].map(|i| g + (DEPTH_TINT[i] - g) * 0.5)
    } else {
        [g; 3]
    }
}

/// The focus range's tint when the depth map is shown.
pub const DEPTH_TINT: [f32; 3] = [1.0, 0.7, 0.15];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_are_nearest_first_and_areas_grow() {
        for b in [Bokeh::Circle, Bokeh::Blades] {
            let t = Taps::of(b);
            assert!(t.list.windows(2).all(|w| w[0][2] <= w[1][2]));
            assert_eq!(t.list[0], [0.0, 0.0, 0.0]);
            assert!(t.area.windows(2).all(|w| w[0] <= w[1]));
            // A circle of radius 10 covers about π·10².
            let a = t.area(10.0);
            let want = match b {
                Bokeh::Circle => std::f32::consts::PI * 100.0,
                // A hexagon with its corners on the circle.
                Bokeh::Blades => 1.5 * 3f32.sqrt() * 100.0,
            };
            assert!((a / want - 1.0).abs() < 0.03, "{b:?}: {a} against {want}");
        }
    }

    #[test]
    fn circles_grow_past_the_range() {
        let st = Stage {
            depth: Arc::new(Depth::new(1, 1, vec![0.5])),
            dscale: [1.0, 1.0],
            focus: 0.5,
            half: 0.1,
            coc: 10.0,
            boost: 0.0,
            taps: Taps::of(Bokeh::Circle),
            count: 1,
            src: (1, 1),
            work: (1, 1),
            per: [1.0, 1.0],
        };
        assert_eq!(st.coc(0.55), 0.0);
        assert!((st.coc(0.9) - 3.0).abs() < 1e-5);
        assert!((st.coc(0.1) + 3.0).abs() < 1e-5);
    }

    #[test]
    fn percentiles_by_histogram() {
        let v: Vec<f32> = (0..1000).map(|i| i as f32 / 999.0).collect();
        assert!((percentile(&v, 0.9) - 0.9).abs() < 0.01);
    }
}
