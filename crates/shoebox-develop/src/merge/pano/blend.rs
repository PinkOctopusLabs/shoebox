//! Multi-band blending (Burt and Adelson, "A Multiresolution Spline With
//! Application to Image Mosaics", 1983), in linear camera space: each
//! frame's Laplacian pyramid is weighted by the Gaussian pyramid of its
//! seam mask and summed, level by level, then each level normalised by its
//! summed weights and the pyramid collapsed. Fine detail changes frame
//! within a pixel or two of the seam, broad tone over the width of the
//! coarsest band, so neither a sharp line nor a ghost shows.
//!
//! Frames come one at a time (each warped into its own box of the result
//! and dropped once added), so memory is the result's accumulators, about
//! 21 bytes a pixel (three channels and a weight, a third more for the
//! coarser levels), and one frame's box.

use rayon::prelude::*;

use crate::cpu::Rgb;

/// Burt and Adelson's 5-tap kernel.
const K5: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// An image of `c` channels, interleaved.
struct Img {
    w: usize,
    h: usize,
    c: usize,
    data: Vec<f32>,
}

/// Blur by [`K5`] and keep every other pixel each way.
fn reduce(src: &Img) -> Img {
    let (w, h, c) = (src.w, src.h, src.c);
    let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
    let mut tmp = vec![0f32; nw * h * c];
    tmp.par_chunks_mut(nw * c).enumerate().for_each(|(y, row)| {
        let s = &src.data[y * w * c..(y + 1) * w * c];
        for x in 0..nw {
            for (t, k) in K5.iter().enumerate() {
                let sx = (2 * x + t).saturating_sub(2).min(w - 1);
                for ch in 0..c {
                    row[x * c + ch] += k * s[sx * c + ch];
                }
            }
        }
    });
    let mut out = vec![0f32; nw * nh * c];
    out.par_chunks_mut(nw * c).enumerate().for_each(|(y, row)| {
        for (t, k) in K5.iter().enumerate() {
            let sy = (2 * y + t).saturating_sub(2).min(h - 1);
            let s = &tmp[sy * nw * c..(sy + 1) * nw * c];
            for (o, v) in row.iter_mut().zip(s) {
                *o += k * v;
            }
        }
    });
    Img {
        w: nw,
        h: nh,
        c,
        data: out,
    }
}

/// The expansion weights of output index `x` from the half-size image:
/// `(index, weight)` pairs (even: ⅛, ¾, ⅛; odd: ½, ½).
#[inline]
fn taps(x: usize, n: usize) -> [(usize, f32); 3] {
    let m = x / 2;
    let clamp = |i: isize| i.clamp(0, n as isize - 1) as usize;
    if x.is_multiple_of(2) {
        [
            (clamp(m as isize - 1), 0.125),
            (clamp(m as isize), 0.75),
            (clamp(m as isize + 1), 0.125),
        ]
    } else {
        [
            (clamp(m as isize), 0.5),
            (clamp(m as isize + 1), 0.5),
            (0, 0.0),
        ]
    }
}

/// `dst += sign · expand(small)` (dst twice small's size, give or take
/// one), a row at a time (no copy the size of either).
fn expand_into(small: &Img, dst: &mut [f32], w: usize, h: usize, sign: f32) {
    let c = small.c;
    let (sw, sh) = (small.w, small.h);
    let across: Vec<[(usize, f32); 3]> = (0..w).map(|x| taps(x, sw)).collect();
    dst.par_chunks_mut(w * c)
        .enumerate()
        .take(h)
        .for_each(|(y, row)| {
            for (i, k) in taps(y, sh) {
                if k == 0.0 {
                    continue;
                }
                let s = &small.data[i * sw * c..(i + 1) * sw * c];
                for (x, t) in across.iter().enumerate() {
                    for &(j, kk) in t {
                        if kk != 0.0 {
                            let f = sign * k * kk;
                            for ch in 0..c {
                                row[x * c + ch] += f * s[j * c + ch];
                            }
                        }
                    }
                }
            }
        });
}

/// Fill the pixels `cover` leaves out with a smooth continuation of the
/// rest (push–pull: averages of what's there, coarser and coarser, then
/// drawn back down), so a frame's pyramid has no step at its edge.
pub fn fill(img: &mut Rgb, cover: &[bool]) {
    let (w, h) = (img.width, img.height);
    if cover.iter().all(|c| *c) || !cover.iter().any(|c| *c) {
        return;
    }
    // Level 0: values times weight, and weight.
    let mut levels: Vec<(usize, usize, Vec<f32>, Vec<f32>)> = Vec::new();
    let mut sum: Vec<f32> = img
        .data
        .chunks(3)
        .zip(cover)
        .flat_map(|(p, &c)| {
            let k = f32::from(u8::from(c));
            [p[0] * k, p[1] * k, p[2] * k]
        })
        .collect();
    let mut wt: Vec<f32> = cover.iter().map(|&c| f32::from(u8::from(c))).collect();
    let (mut lw, mut lh) = (w, h);
    while lw > 1 || lh > 1 {
        let (nw, nh) = (lw.div_ceil(2), lh.div_ceil(2));
        let mut ns = vec![0f32; nw * nh * 3];
        let mut nwt = vec![0f32; nw * nh];
        for y in 0..lh {
            for x in 0..lw {
                let (i, j) = (y * lw + x, (y / 2) * nw + x / 2);
                nwt[j] += wt[i];
                for c in 0..3 {
                    ns[j * 3 + c] += sum[i * 3 + c];
                }
            }
        }
        levels.push((lw, lh, sum, wt));
        sum = ns;
        wt = nwt;
        lw = nw;
        lh = nh;
    }
    // The top: one average.
    let mut above: (usize, usize, Vec<f32>) = (
        lw,
        lh,
        sum.chunks(3)
            .zip(&wt)
            .flat_map(|(s, &k)| {
                if k > 0.0 {
                    [s[0] / k, s[1] / k, s[2] / k]
                } else {
                    [0.0; 3]
                }
            })
            .collect(),
    );
    for (lw, lh, sum, wt) in levels.into_iter().rev() {
        let (aw, ah, ref av) = above;
        let mut v = vec![0f32; lw * lh * 3];
        v.par_chunks_mut(lw * 3).enumerate().for_each(|(y, row)| {
            for x in 0..lw {
                let i = y * lw + x;
                if wt[i] > 0.0 {
                    for c in 0..3 {
                        row[x * 3 + c] = sum[i * 3 + c] / wt[i];
                    }
                    continue;
                }
                // Bilinear from the level above.
                let fx = ((x as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (aw - 1) as f32);
                let fy = ((y as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (ah - 1) as f32);
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(aw - 1), (y0 + 1).min(ah - 1));
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                for c in 0..3 {
                    let a =
                        av[(y0 * aw + x0) * 3 + c] * (1.0 - tx) + av[(y0 * aw + x1) * 3 + c] * tx;
                    let b =
                        av[(y1 * aw + x0) * 3 + c] * (1.0 - tx) + av[(y1 * aw + x1) * 3 + c] * tx;
                    row[x * 3 + c] = a * (1.0 - ty) + b * ty;
                }
            }
        });
        above = (lw, lh, v);
    }
    img.data = above.2;
}

/// The result's pyramids, adding up.
pub struct Blender {
    /// Per level: size, the weighted sums (three channels), the weights.
    levels: Vec<(usize, usize, Vec<f32>, Vec<f32>)>,
}

impl Blender {
    /// For a `width`×`height` result, with `bands` halvings.
    pub fn new(width: usize, height: usize, bands: usize) -> Self {
        let mut levels = Vec::new();
        let (mut w, mut h) = (width.max(1), height.max(1));
        for _ in 0..=bands {
            levels.push((w, h, vec![0f32; w * h * 3], vec![0f32; w * h]));
            w = w.div_ceil(2);
            h = h.div_ceil(2);
        }
        Self { levels }
    }

    /// Frames' boxes start at multiples of this.
    pub fn align(&self) -> usize {
        1 << (self.levels.len() - 1)
    }

    /// How many halvings fit a `width`×`height` result: enough for a blend
    /// about 5% of its size wide (OpenCV's default), at most 7, the
    /// smallest level at least 4 pixels.
    pub fn bands(width: usize, height: usize) -> usize {
        let wide = 0.05 * ((width * height) as f64).sqrt();
        let mut b = (wide.max(2.0).log2().ceil() as usize)
            .saturating_sub(1)
            .clamp(1, 7);
        while b > 1 && width.min(height) >> b < 4 {
            b -= 1;
        }
        b
    }

    /// Add a frame: `img` and its seam mask (0–1), their top-left corner at
    /// `(x0, y0)` of the result (multiples of [`Self::align`]).
    pub fn add(&mut self, img: Rgb, mask: Vec<f32>, x0: usize, y0: usize) {
        let mut g = Img {
            w: img.width,
            h: img.height,
            c: 3,
            data: img.data,
        };
        let mut m = Img {
            w: g.w,
            h: g.h,
            c: 1,
            data: mask,
        };
        let last = self.levels.len() - 1;
        for l in 0..=last {
            let next = (l < last).then(|| (reduce(&g), reduce(&m)));
            if let Some((gn, _)) = &next {
                // The band: this level less the next one up, expanded.
                expand_into(gn, &mut g.data, g.w, g.h, -1.0);
            }
            let (ox, oy) = (x0 >> l, y0 >> l);
            let (lw, lh, acc, wacc) = &mut self.levels[l];
            let (lw, lh) = (*lw, *lh);
            let rows = g.h.min(lh.saturating_sub(oy));
            let cols = g.w.min(lw.saturating_sub(ox));
            acc.par_chunks_mut(lw * 3)
                .zip(wacc.par_chunks_mut(lw))
                .skip(oy)
                .take(rows)
                .enumerate()
                .for_each(|(r, (arow, wrow))| {
                    let gs = &g.data[r * g.w * 3..];
                    let ms = &m.data[r * m.w..];
                    for x in 0..cols {
                        let k = ms[x];
                        if k <= 0.0 {
                            continue;
                        }
                        wrow[ox + x] += k;
                        for c in 0..3 {
                            arow[(ox + x) * 3 + c] += k * gs[x * 3 + c];
                        }
                    }
                });
            if let Some((gn, mn)) = next {
                g = gn;
                m = mn;
            }
        }
    }

    /// The blend: each level normalised by its weights, collapsed. Pixels
    /// no frame's seam covers are 0.
    pub fn finish(mut self) -> Rgb {
        for (_, _, acc, wacc) in &mut self.levels {
            acc.par_chunks_mut(3)
                .zip(wacc.par_iter())
                .for_each(|(p, &k)| {
                    if k > 1e-8 {
                        p.iter_mut().for_each(|v| *v /= k);
                    } else {
                        p.iter_mut().for_each(|v| *v = 0.0);
                    }
                });
        }
        while self.levels.len() > 1 {
            let Some((sw, sh, sacc, _)) = self.levels.pop() else {
                break;
            };
            let small = Img {
                w: sw,
                h: sh,
                c: 3,
                data: sacc,
            };
            if let Some((w, h, acc, _)) = self.levels.last_mut() {
                expand_into(&small, acc, *w, *h, 1.0);
            }
        }
        let Some((w, h, mut acc, wacc)) = self.levels.pop() else {
            return Rgb::new(1, 1);
        };
        acc.par_chunks_mut(3)
            .zip(wacc.par_iter())
            .for_each(|(p, &k)| {
                for v in p.iter_mut() {
                    *v = if k > 1e-8 { v.max(0.0) } else { 0.0 };
                }
            });
        Rgb {
            width: w,
            height: h,
            data: acc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Rgb {
        let mut img = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = f(x, y);
                img.data[(y * w + x) * 3..][..3].copy_from_slice(&[v, v * 0.5, v * 2.0]);
            }
        }
        img
    }

    #[test]
    fn one_frame_comes_back_as_it_was() {
        let (w, h) = (75, 41);
        let img = ramp(w, h, |x, y| ((x * 7 + y * 3) % 11) as f32 / 10.0 + 0.1);
        let mut b = Blender::new(w, h, 3);
        b.add(img.clone(), vec![1.0; w * h], 0, 0);
        let out = b.finish();
        for (a, b) in out.data.iter().zip(&img.data) {
            assert!((a - b).abs() < 1e-4, "{a} {b}");
        }
    }

    #[test]
    fn two_levels_meet_without_a_step() {
        // Left frame 1.0, right frame 1.2, seam in the middle: the result
        // climbs smoothly from one to the other.
        let (w, h) = (128, 32);
        let mut b = Blender::new(w, h, Blender::bands(w, h).max(4));
        for (v, left) in [(1.0f32, true), (1.2, false)] {
            let img = ramp(w, h, |_, _| v);
            let mask = (0..w * h)
                .map(|i| f32::from(u8::from((i % w < w / 2) == left)))
                .collect();
            b.add(img, mask, 0, 0);
        }
        let out = b.finish();
        let row: Vec<f32> = (0..w).map(|x| out.data[(16 * w + x) * 3]).collect();
        assert!((row[0] - 1.0).abs() < 0.02 && (row[w - 1] - 1.2).abs() < 0.02);
        let worst = row
            .windows(2)
            .map(|p| (p[1] - p[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.03, "a step of {worst}");
    }

    #[test]
    fn filling_continues_what_is_there() {
        let (w, h) = (20, 10);
        let mut img = ramp(w, h, |_, _| 0.5);
        let cover: Vec<bool> = (0..w * h).map(|i| i % w < 12).collect();
        for (p, c) in img.data.chunks_mut(3).zip(&cover) {
            if !c {
                p.fill(0.0);
            }
        }
        fill(&mut img, &cover);
        assert!(img.data.chunks(3).all(|p| (p[0] - 0.5).abs() < 1e-5));
    }
}
