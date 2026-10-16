//! 8-bit grayscale buffers: area resampling and EXIF orientation.

use crate::DecodeError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gray {
    pub width: u32,
    pub height: u32,
    pub px: Vec<u8>,
}

impl Gray {
    pub fn new(width: u32, height: u32, px: Vec<u8>) -> Result<Self, DecodeError> {
        if width == 0 || height == 0 || px.len() != width as usize * height as usize {
            return Err(DecodeError::Format(format!(
                "pixel buffer {} does not match {width}x{height}",
                px.len()
            )));
        }
        Ok(Self { width, height, px })
    }

    #[inline]
    pub fn at(&self, x: u32, y: u32) -> u8 {
        self.px[(y * self.width + x) as usize]
    }

    /// Area-average resample to exactly `w`×`h`. Exact for any ratio.
    pub fn resize(&self, w: u32, h: u32) -> Gray {
        let (w, h) = (w.max(1), h.max(1));
        let horiz = resample_rows(&self.px, self.width, self.height, w);
        let out = resample_cols(&horiz, w, self.height, h);
        Gray {
            width: w,
            height: h,
            px: out,
        }
    }

    /// Resize so the long edge is `long` (never upscales).
    pub fn fit_long_edge(&self, long: u32) -> Gray {
        let cur = self.width.max(self.height);
        if cur <= long {
            return self.clone();
        }
        let s = f64::from(long) / f64::from(cur);
        let w = (f64::from(self.width) * s).round().max(1.0) as u32;
        let h = (f64::from(self.height) * s).round().max(1.0) as u32;
        // Cheap integer pre-reduction to roughly 2× the target, then an exact
        // area resample. Drops at most k-1 edge pixels, which hashes don't notice.
        let k = cur / (long * 2);
        if k >= 2 {
            self.box_reduce(k).resize(w, h)
        } else {
            self.resize(w, h)
        }
    }

    /// Average non-overlapping k×k blocks (integer arithmetic).
    pub fn box_reduce(&self, k: u32) -> Gray {
        let (nw, nh) = ((self.width / k).max(1), (self.height / k).max(1));
        let (k, w) = (k as usize, self.width as usize);
        let area = (k * k) as u32;
        let mut sums = vec![0u32; nw as usize];
        let mut px = Vec::with_capacity(nw as usize * nh as usize);
        for by in 0..nh as usize {
            sums.fill(0);
            for row in self.px[by * k * w..].chunks_exact(w).take(k) {
                for (s, block) in sums.iter_mut().zip(row.chunks_exact(k)) {
                    *s += block.iter().map(|&v| u32::from(v)).sum::<u32>();
                }
            }
            px.extend(sums.iter().map(|&s| ((s + area / 2) / area) as u8));
        }
        Gray {
            width: nw,
            height: nh,
            px,
        }
    }

    /// Rotate 90° clockwise.
    pub fn rotate90(&self) -> Gray {
        let (w, h) = (self.width, self.height);
        let mut px = vec![0u8; self.px.len()];
        for y in 0..h {
            for x in 0..w {
                // (x, y) -> (h-1-y, x) in a h×w image
                px[(x * h + (h - 1 - y)) as usize] = self.at(x, y);
            }
        }
        Gray {
            width: h,
            height: w,
            px,
        }
    }

    pub fn rotate180(&self) -> Gray {
        let mut px = self.px.clone();
        px.reverse();
        Gray {
            width: self.width,
            height: self.height,
            px,
        }
    }

    pub fn rotate270(&self) -> Gray {
        self.rotate180().rotate90()
    }

    pub fn flip_h(&self) -> Gray {
        let mut px = self.px.clone();
        for row in px.chunks_mut(self.width as usize) {
            row.reverse();
        }
        Gray {
            width: self.width,
            height: self.height,
            px,
        }
    }

    /// Apply an EXIF Orientation value (1–8) so the result is upright.
    pub fn orient(&self, orientation: u16) -> Gray {
        match orientation {
            2 => self.flip_h(),
            3 => self.rotate180(),
            4 => self.rotate180().flip_h(),
            5 => self.rotate90().flip_h(),
            6 => self.rotate90(),
            7 => self.rotate270().flip_h(),
            8 => self.rotate270(),
            _ => self.clone(),
        }
    }
}

/// Weights for mapping `src_len` input samples onto `dst_len` output samples
/// by area: each output sample is the mean of the input interval it covers.
fn area_weights(src_len: u32, dst_len: u32) -> Vec<Vec<(usize, f32)>> {
    let scale = f64::from(src_len) / f64::from(dst_len);
    (0..dst_len)
        .map(|o| {
            let start = f64::from(o) * scale;
            let end = start + scale;
            let mut ws = Vec::new();
            let mut i = start.floor() as u32;
            while f64::from(i) < end && i < src_len {
                let lo = f64::from(i).max(start);
                let hi = f64::from(i + 1).min(end);
                if hi > lo {
                    ws.push((i as usize, ((hi - lo) / scale) as f32));
                }
                i += 1;
            }
            ws
        })
        .collect()
}

fn resample_rows(px: &[u8], w: u32, h: u32, new_w: u32) -> Vec<f32> {
    let weights = area_weights(w, new_w);
    let mut out = Vec::with_capacity(new_w as usize * h as usize);
    for row in px.chunks_exact(w as usize) {
        for ws in &weights {
            out.push(ws.iter().map(|&(i, k)| f32::from(row[i]) * k).sum());
        }
    }
    out
}

fn resample_cols(px: &[f32], w: u32, h: u32, new_h: u32) -> Vec<u8> {
    let weights = area_weights(h, new_h);
    let w = w as usize;
    let mut out = vec![0u8; w * new_h as usize];
    for (oy, ws) in weights.iter().enumerate() {
        for x in 0..w {
            let v: f32 = ws.iter().map(|&(y, k)| px[y * w + x] * k).sum();
            out[oy * w + x] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// BT.601 luma from interleaved 8-bit RGB(A).
pub fn luma_from_rgb8(px: &[u8], channels: usize) -> Vec<u8> {
    px.chunks_exact(channels)
        .map(|p| ((77 * u32::from(p[0]) + 150 * u32::from(p[1]) + 29 * u32::from(p[2])) >> 8) as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: u32, h: u32) -> Gray {
        let px = (0..w * h).map(|i| (i % 251) as u8).collect();
        Gray::new(w, h, px).unwrap()
    }

    #[test]
    fn rotations_compose() {
        let g = ramp(7, 3);
        assert_eq!(g.rotate90().rotate90(), g.rotate180());
        assert_eq!(g.rotate90().rotate270(), g);
        assert_eq!(g.rotate90().width, 3);
    }

    #[test]
    fn rotate90_is_clockwise() {
        // [1 2]      [3 1]
        // [3 4]  ->  [4 2]
        let g = Gray::new(2, 2, vec![1, 2, 3, 4]).unwrap();
        assert_eq!(g.rotate90().px, vec![3, 1, 4, 2]);
    }

    #[test]
    fn resize_preserves_mean_of_flat_image() {
        let g = Gray::new(100, 37, vec![123; 3700]).unwrap();
        let r = g.resize(9, 8);
        assert!(r.px.iter().all(|&v| v == 123));
    }

    #[test]
    fn box_reduce_averages_blocks() {
        let g = Gray::new(4, 2, vec![0, 10, 100, 110, 20, 30, 120, 130]).unwrap();
        assert_eq!(g.box_reduce(2).px, vec![15, 115]);
    }

    #[test]
    fn fit_long_edge_matches_exact_resize_closely() {
        // Smooth, photo-like content (a sawtooth would exaggerate edge effects).
        let (w, h) = (3000u32, 2000u32);
        let px = (0..w * h)
            .map(|i| {
                let (x, y) = (f64::from(i % w) / 300.0, f64::from(i / w) / 200.0);
                (128.0 + 60.0 * x.sin() + 50.0 * (y + x / 3.0).cos()) as u8
            })
            .collect();
        let g = Gray::new(w, h, px).unwrap();
        let (a, b) = (g.fit_long_edge(256), g.resize(256, 171));
        assert_eq!((a.width, a.height), (b.width, b.height));
        let diff: u32 =
            a.px.iter()
                .zip(&b.px)
                .map(|(x, y)| u32::from(x.abs_diff(*y)))
                .max()
                .unwrap();
        assert!(diff <= 3, "max diff {diff}");
    }

    #[test]
    fn resize_halves_by_averaging() {
        let g = Gray::new(4, 1, vec![0, 100, 200, 250]).unwrap();
        assert_eq!(g.resize(2, 1).px, vec![50, 225]);
    }
}
