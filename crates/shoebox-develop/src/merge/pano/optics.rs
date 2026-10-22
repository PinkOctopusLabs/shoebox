//! A frame's lens corrections, as panorama uses them: [`crate::lens`]'s
//! grid (the NEF's own data, then Lensfun: distortion, vignetting, lateral
//! chromatic aberration) read in the frame's upright pixels, so one lookup
//! takes an ideal (corrected) pixel to where red, green and blue are in the
//! frame as loaded, and the light its corner lost. Panorama folds it into
//! its one resample (the projection's), so frames are never resampled
//! twice; the previews it matches on are corrected the same way.

use std::sync::Arc;

use rayon::prelude::*;

use super::super::align::Gray;
use super::super::{Input, Source};
use crate::cpu::Rgb;
use crate::edit::LensCorrection;
use crate::lens::{self, Grid, Shot};

/// A frame's corrections in its upright pixels.
#[derive(Debug, Clone)]
pub struct Optics {
    grid: Option<Arc<Grid>>,
    /// Upright size.
    pub width: usize,
    pub height: usize,
    /// Stored size (the grid's).
    sw: f64,
    sh: f64,
    /// Upright → stored, and back (affine: a quarter turn or a mirror).
    to_stored: [[f64; 3]; 2],
    to_upright: [[f64; 3]; 2],
}

/// Where an ideal pixel's colours are in the frame as loaded, and the gain
/// that undoes its vignetting.
#[derive(Debug, Clone, Copy)]
pub struct At {
    pub r: [f64; 2],
    pub g: [f64; 2],
    pub b: [f64; 2],
    pub gain: f32,
}

impl Optics {
    /// `grid` for a frame `width`×`height` upright, turned upright by EXIF
    /// `orientation`.
    pub fn new(grid: Option<Arc<Grid>>, width: usize, height: usize, orientation: u16) -> Self {
        let (sw, sh) = if matches!(orientation, 5..=8) {
            (height, width)
        } else {
            (width, height)
        };
        let o = if (1..=8).contains(&orientation) {
            orientation
        } else {
            1
        };
        let at = |x, y| {
            let (a, b) = crate::export::stored_at(sw.max(2), sh.max(2), o, x, y);
            [a as f64, b as f64]
        };
        let (p0, px, py) = (at(0, 0), at(1, 0), at(0, 1));
        let to_stored = [
            [px[0] - p0[0], py[0] - p0[0], p0[0]],
            [px[1] - p0[1], py[1] - p0[1], p0[1]],
        ];
        // The linear part is a signed permutation: its inverse is its
        // transpose.
        let l = [
            [to_stored[0][0], to_stored[0][1]],
            [to_stored[1][0], to_stored[1][1]],
        ];
        let li = [[l[0][0], l[1][0]], [l[0][1], l[1][1]]];
        let to_upright = [
            [li[0][0], li[0][1], -(li[0][0] * p0[0] + li[0][1] * p0[1])],
            [li[1][0], li[1][1], -(li[1][0] * p0[0] + li[1][1] * p0[1])],
        ];
        Self {
            grid,
            width,
            height,
            sw: sw.max(2) as f64,
            sh: sh.max(2) as f64,
            to_stored,
            to_upright,
        }
    }

    pub fn none(width: usize, height: usize) -> Self {
        Self::new(None, width, height, 1)
    }

    pub fn corrects(&self) -> bool {
        self.grid.is_some()
    }

    /// Where the lens put the ideal upright pixel `(x, y)`, per colour, and
    /// the vignetting gain there.
    #[inline]
    pub fn at(&self, x: f64, y: f64) -> At {
        let Some(g) = &self.grid else {
            return At {
                r: [x, y],
                g: [x, y],
                b: [x, y],
                gain: 1.0,
            };
        };
        let m = &self.to_stored;
        let sx = m[0][0] * x + m[0][1] * y + m[0][2];
        let sy = m[1][0] * x + m[1][1] * y + m[1][2];
        let n = g.at(
            (sx / (self.sw - 1.0) * (g.cols - 1) as f64) as f32,
            (sy / (self.sh - 1.0) * (g.rows - 1) as f64) as f32,
        );
        let back = |k: usize| {
            let (px, py) = (
                sx + f64::from(n[k]) * self.sw,
                sy + f64::from(n[k + 1]) * self.sh,
            );
            let u = &self.to_upright;
            [
                u[0][0] * px + u[0][1] * py + u[0][2],
                u[1][0] * px + u[1][1] * py + u[1][2],
            ]
        };
        At {
            r: back(0),
            g: back(2),
            b: back(4),
            gain: n[6],
        }
    }
}

/// How a source was taken, for its lens: its EXIF as rawler reads it, and
/// a NEF's own lens data.
pub fn shot(input: &Input, src: &Source) -> Shot {
    let ratio =
        |r: &rawler::formats::tiff::Rational| (r.d != 0).then(|| f64::from(r.n) / f64::from(r.d));
    let md = src.metadata.as_ref();
    let pick = |a: Option<&String>, b: &str| {
        a.filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| b.to_owned())
    };
    let camera_data =
        if shoebox_decode::Format::from_path(&input.path) == Some(shoebox_decode::Format::Nef) {
            std::fs::read(&input.path)
                .map(|b| shoebox_decode::nef::lens_data(&b))
                .unwrap_or_default()
        } else {
            Default::default()
        };
    Shot {
        make: pick(md.map(|m| &m.make), &src.meta.make),
        model: pick(md.map(|m| &m.model), &src.meta.model),
        lens: md
            .and_then(|m| m.exif.lens_model.clone())
            .unwrap_or_default(),
        focal: md
            .and_then(|m| m.exif.focal_length.as_ref())
            .and_then(ratio)
            .unwrap_or(0.0) as f32,
        aperture: md
            .and_then(|m| m.exif.fnumber.as_ref())
            .and_then(ratio)
            .unwrap_or(0.0) as f32,
        camera_data,
    }
}

/// A source's corrections (None from the grid when its lens is unknown or
/// `enabled` is off), and what they were made from.
pub fn optics(shot: &Shot, src: &Source, orientation: u16, enabled: bool) -> (Optics, String) {
    let (w, h) = (src.width(), src.height());
    let (sw, sh) = if matches!(orientation, 5..=8) {
        (h, w)
    } else {
        (w, h)
    };
    let lc = LensCorrection {
        enabled,
        ..Default::default()
    };
    let grid = lens::grid(shot, &lc, sw, sh);
    let what = grid.as_ref().map(|g| g.source.clone()).unwrap_or_default();
    (Optics::new(grid, w, h, orientation), what)
}

/// Luminance of camera-native RGB (green-weighted: it's what the sensor
/// sees best).
#[inline]
pub fn luma(p: &[f32]) -> f32 {
    0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2]
}

/// Bilinear luminance of `img` at `(x, y)`; None outside.
#[inline]
fn sample_luma(img: &Rgb, x: f64, y: f64) -> Option<f32> {
    let (w, h) = (img.width, img.height);
    if !(x >= 0.0 && y >= 0.0 && x <= (w - 1) as f64 && y <= (h - 1) as f64) {
        return None;
    }
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let l = |xx: usize, yy: usize| luma(&img.data[(yy * w + xx) * 3..][..3]);
    let top = l(x0, y0) * (1.0 - fx) + l(x1, y0) * fx;
    let bot = l(x0, y1) * (1.0 - fx) + l(x1, y1) * fx;
    Some(top * (1.0 - fy) + bot * fy)
}

/// A frame made small for matching and measuring: lens-corrected
/// luminance (NaN where the lens leaves nothing), and which of its pixels
/// came from clipped photosites.
pub struct Preview {
    pub gray: Gray,
    pub clipped: Vec<u8>,
    /// Full-size pixels per preview pixel (a whole number).
    pub step: usize,
}

impl Preview {
    /// Full-size coordinates of preview pixel `(x, y)`.
    #[inline]
    pub fn to_full(&self, x: f64, y: f64) -> (f64, f64) {
        let o = (self.step as f64 - 1.0) / 2.0;
        (x * self.step as f64 + o, y * self.step as f64 + o)
    }

    /// Preview coordinates of full-size pixel `(x, y)`.
    #[inline]
    pub fn from_full(&self, x: f64, y: f64) -> (f64, f64) {
        let o = (self.step as f64 - 1.0) / 2.0;
        ((x - o) / self.step as f64, (y - o) / self.step as f64)
    }
}

/// `src` at most `long_edge` pixels long (box-averaged by a whole step),
/// lens-corrected through `optics`.
pub fn preview(src: &Source, optics: &Optics, long_edge: usize) -> Preview {
    let small = crate::cpu::downscale(&src.rgb, long_edge);
    let step = (src.width() / small.width.max(1)).max(1);
    let (w, h) = (small.width, small.height);
    // Clipped anywhere in a box: clipped.
    let mut clipped_small = vec![0u8; w * h];
    clipped_small
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let mut v = 0u8;
                for yy in y * step..((y + 1) * step).min(src.height()) {
                    let at = yy * src.width();
                    for xx in x * step..((x + 1) * step).min(src.width()) {
                        v = v.max(src.clipped[at + xx]);
                    }
                }
                *o = v;
            }
        });
    let mut out = Preview {
        gray: Gray::new(w, h),
        clipped: vec![0u8; w * h],
        step,
    };
    let o = (step as f64 - 1.0) / 2.0;
    let s = step as f64;
    out.gray
        .data
        .par_chunks_mut(w)
        .zip(out.clipped.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (row, crow))| {
            for x in 0..w {
                let a = optics.at(x as f64 * s + o, y as f64 * s + o);
                let (px, py) = ((a.g[0] - o) / s, (a.g[1] - o) / s);
                row[x] = match sample_luma(&small, px, py) {
                    Some(v) => v * a.gain,
                    None => f32::NAN,
                };
                if row[x].is_finite() {
                    let (cx, cy) = (px.round() as usize, py.round() as usize);
                    crow[x] = clipped_small[cy.min(h - 1) * w + cx.min(w - 1)];
                }
            }
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upright_and_stored_pixels_agree() {
        for o in 1..=8u16 {
            let opt = Optics::new(None, 7, 5, o);
            let (sw, sh) = if o >= 5 { (5, 7) } else { (7, 5) };
            for (x, y) in [(0usize, 0usize), (6, 4), (3, 1)] {
                let (a, b) = crate::export::stored_at(sw, sh, o, x, y);
                let m = &opt.to_stored;
                let (fx, fy) = (x as f64, y as f64);
                assert_eq!(
                    (
                        m[0][0] * fx + m[0][1] * fy + m[0][2],
                        m[1][0] * fx + m[1][1] * fy + m[1][2]
                    ),
                    (a as f64, b as f64),
                    "orientation {o}"
                );
                let u = &opt.to_upright;
                let (sa, sb) = (a as f64, b as f64);
                assert_eq!(
                    (
                        u[0][0] * sa + u[0][1] * sb + u[0][2],
                        u[1][0] * sa + u[1][1] * sb + u[1][2]
                    ),
                    (fx, fy)
                );
            }
        }
    }
}
