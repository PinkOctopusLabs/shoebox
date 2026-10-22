//! Effects (EDITING-SPEC, module 15), on display values after the display
//! transform: a post-crop vignette (placed by the whole frame, so a 100%
//! region gets its part) and monochrome film grain, strongest in the
//! midtones, from an integer hash both pipes share. Grain is sized in
//! the full-size photo's pixels, its strength evened out where a proxy's
//! pixel spans several grains. The shader (`develop.wgsl`) follows this.

use crate::edit;

/// The numbers for one render.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Effects {
    /// −1 to 1 (0: none).
    pub vignette: f32,
    pub midpoint: f32,
    /// The frame's aspect raised to this scales x (and divides y): 0 the
    /// frame's shape, ½ a circle.
    pub round: f32,
    pub feather: f32,
    pub highlights: f32,
    /// 0 to 1 (0: none).
    pub grain: f32,
    /// A grain's size in output pixels.
    pub cell: f32,
    pub rough: f32,
    /// The whole frame's size, and where this render starts in it (a 100%
    /// region).
    pub full: [f32; 2],
    pub offset: [f32; 2],
}

impl Effects {
    /// `scale`: output pixels per full-size pixel (a proxy's is below 1).
    pub fn new(e: &edit::Effects, full: [f32; 2], scale: f32) -> Self {
        let v = &e.vignette;
        let g = &e.grain;
        let unit = |x: f64| (x / 100.0) as f32;
        let px = 1.0 + 5.0 * unit(g.size.clamp(0.0, 100.0));
        Self {
            vignette: unit(v.amount.clamp(-100.0, 100.0)),
            midpoint: unit(v.midpoint.clamp(0.0, 100.0)),
            round: 0.5 * unit(v.roundness.clamp(-100.0, 100.0)).max(0.0),
            feather: unit(v.feather.clamp(0.0, 100.0)).max(0.01),
            highlights: unit(v.highlights.clamp(0.0, 100.0)),
            grain: unit(g.amount.clamp(0.0, 100.0)),
            cell: (px * scale).max(1e-3),
            rough: unit(g.roughness.clamp(0.0, 100.0)),
            full,
            offset: [0.0, 0.0],
        }
    }

    pub fn active(&self) -> bool {
        self.vignette != 0.0 || self.grain != 0.0
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A hash of a lattice point to −1..1.
pub fn hash(x: u32, y: u32) -> f32 {
    let mut h = x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h & 0xffff) as f32 / 32767.5 - 1.0
}

/// Value noise: the lattice's hashes, smoothly interpolated.
fn noise(x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (x - x0, y - y0);
    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
    let (i, j) = (x0 as u32, y0 as u32);
    let a = hash(i, j);
    let b = hash(i.wrapping_add(1), j);
    let c = hash(i, j.wrapping_add(1));
    let d = hash(i.wrapping_add(1), j.wrapping_add(1));
    let top = a + (b - a) * sx;
    let bot = c + (d - c) * sx;
    top + (bot - top) * sy
}

/// The effects on display values `d` of output pixel `(x, y)`.
pub fn apply(e: &Effects, d: [f32; 3], x: f32, y: f32) -> [f32; 3] {
    let (fx, fy) = (x + e.offset[0] + 0.5, y + e.offset[1] + 0.5);
    let mut d = d;
    if e.vignette != 0.0 {
        let (w, h) = (e.full[0].max(1.0), e.full[1].max(1.0));
        let k = (w / h).powf(e.round);
        let (dx, dy) = ((fx / w - 0.5) * 2.0 * k, (fy / h - 0.5) * 2.0 / k);
        let r = (dx * dx + dy * dy).sqrt() / std::f32::consts::SQRT_2;
        let m = e.midpoint;
        let t = smoothstep(m - 0.5 * e.feather, m + 0.5 * e.feather, r);
        let luma = 0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2];
        d = if e.vignette < 0.0 {
            let keep = 1.0 - e.highlights * smoothstep(0.6, 1.0, luma);
            let k = 1.0 + e.vignette * t * keep;
            d.map(|v| v * k)
        } else {
            d.map(|v| v + (1.0 - v) * e.vignette * t)
        };
    }
    if e.grain != 0.0 {
        let (gx, gy) = (fx / e.cell, fy / e.cell);
        let mut n = noise(gx, gy);
        n += e.rough * 0.5 * noise(gx * 2.0 + 17.0, gy * 2.0 + 31.0);
        // A pixel spanning several grains shows their average.
        let even = (1.0 / e.cell).max(1.0);
        let luma = 0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2];
        let mid = 4.0 * luma.clamp(0.0, 1.0) * (1.0 - luma.clamp(0.0, 1.0));
        let g = e.grain * 0.12 * n * mid / even;
        d = d.map(|v| (v + g).clamp(0.0, 1.0));
    }
    d
}
