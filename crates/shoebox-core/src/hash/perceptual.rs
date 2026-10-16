//! 64-bit dHash and pHash over a grayscale thumbnail.

use std::sync::OnceLock;

use shoebox_decode::Gray;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PHashes {
    pub dhash: u64,
    pub phash: u64,
}

impl PHashes {
    pub fn of(g: &Gray) -> Self {
        Self {
            dhash: dhash(g),
            phash: phash(g),
        }
    }

    /// Combined distance: the worse of the two hashes.
    pub fn distance(&self, other: &PHashes) -> u32 {
        hamming(self.phash, other.phash).max(hamming(self.dhash, other.dhash))
    }
}

#[inline]
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Horizontal gradient hash: 9×8 thumbnail, bit set where a pixel is brighter
/// than its right neighbour.
pub fn dhash(g: &Gray) -> u64 {
    let t = g.resize(9, 8);
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h <<= 1;
            if t.at(x, y) > t.at(x + 1, y) {
                h |= 1;
            }
        }
    }
    h
}

const N: usize = 32;
const K: usize = 8;

fn cos_table() -> &'static [[f64; N]; K] {
    static T: OnceLock<[[f64; N]; K]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0.0; N]; K];
        for (u, row) in t.iter_mut().enumerate() {
            for (x, c) in row.iter_mut().enumerate() {
                *c = (((2 * x + 1) * u) as f64 * std::f64::consts::PI / (2 * N) as f64).cos();
            }
        }
        t
    })
}

/// DCT hash: 32×32 thumbnail, 2-D DCT-II, low-frequency 8×8 block, bit set
/// where a coefficient exceeds the median of the 63 AC coefficients. The DC
/// bit is always zero.
pub fn phash(g: &Gray) -> u64 {
    let t = g.resize(N as u32, N as u32);
    let c = cos_table();
    let mut rows = [[0.0f64; K]; N];
    for (y, row) in rows.iter_mut().enumerate() {
        for (u, out) in row.iter_mut().enumerate() {
            *out = (0..N).map(|x| f64::from(t.px[y * N + x]) * c[u][x]).sum();
        }
    }
    let mut coef = [0.0f64; K * K];
    for v in 0..K {
        for u in 0..K {
            coef[v * K + u] = (0..N).map(|y| rows[y][u] * c[v][y]).sum();
        }
    }
    let mut ac: Vec<f64> = coef[1..].to_vec();
    ac.sort_by(f64::total_cmp);
    let median = ac[ac.len() / 2];
    let mut h = 0u64;
    for (i, &v) in coef.iter().enumerate() {
        h <<= 1;
        if i != 0 && v > median {
            h |= 1;
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> Gray {
        let px = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| f(x, y))
            .collect();
        Gray::new(w, h, px).unwrap()
    }

    #[test]
    fn identical_images_have_identical_hashes() {
        let g = pattern(256, 170, |x, y| ((x * 3 + y * 5) % 256) as u8);
        assert_eq!(PHashes::of(&g), PHashes::of(&g.clone()));
    }

    #[test]
    fn resize_barely_changes_hashes() {
        let f = |x: u32, y: u32, s: f64| {
            let (u, v) = (f64::from(x) / s, f64::from(y) / s);
            (128.0 + 60.0 * (u * 9.0).sin() + 50.0 * (v * 5.0 + u * 2.0).cos()) as u8
        };
        let big = pattern(1024, 683, |x, y| f(x, y, 1024.0));
        let small = pattern(256, 171, |x, y| f(x, y, 256.0));
        let d = PHashes::of(&big).distance(&PHashes::of(&small));
        assert!(d <= 4, "distance {d}");
    }

    #[test]
    fn different_images_are_far_apart() {
        let a = pattern(256, 256, |x, _| (x % 256) as u8);
        let b = pattern(
            256,
            256,
            |x, y| if (x / 32 + y / 32) % 2 == 0 { 20 } else { 230 },
        );
        assert!(PHashes::of(&a).distance(&PHashes::of(&b)) > 10);
    }
}
