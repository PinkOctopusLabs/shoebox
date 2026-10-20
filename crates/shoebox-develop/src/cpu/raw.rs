//! Raw-domain stages on the Bayer mosaic: highlight reconstruction and
//! demosaicing. CPU reference versions (the GPU shaders are checked
//! against these).
//!
//! Ported from darktable at commit 2747ed2a0d162515bd9c91d02201653de895dddb
//! (GPL-3.0-or-later, as Shoebox is):
//! - `src/iop/hlreconstruct/opposed.c` (`_process_opposed`, Bayer path) and
//!   `_calc_refavg` from `src/iop/hlreconstruct/segbased.c`: "inpaint
//!   opposed", by Hanno Schwalm with @garagecoder and @Iain (G'MIC).
//! - `src/iop/demosaicing/rcd.c` (`rcd_demosaic`, `demosaic_box3`): Ratio
//!   Corrected Demosaicing 2.3 by Luis Sanz Rodríguez, tuned by Ingo Weyrich
//!   and Hanno Schwalm.
//!
//! Differences from darktable: whole-image buffers instead of tiles (the
//! P/Q high-pass is kept at every photosite rather than in half-width
//! rows), and the 9-pixel border RCD can't fill comes from the bilinear
//! (box3) demosaic rather than PPG.

// Channel and matrix loops read clearer indexed (c, r) than zipped.
#![allow(clippy::needless_range_loop)]

use rayon::prelude::*;

/// darktable's highlight clip factor for "inpaint opposed".
pub const OPPOSED_CLIP: f32 = 0.987;

/// A Bayer mosaic of white-balanced photosites.
pub struct Mosaic<'a> {
    pub width: usize,
    pub height: usize,
    /// Colour (0 R, 1 G, 2 B) at `(row % 2, col % 2)`.
    pub cfa: [[u8; 2]; 2],
    pub data: &'a [f32],
}

impl Mosaic<'_> {
    #[inline]
    fn fc(&self, row: usize, col: usize) -> usize {
        usize::from(self.cfa[row % 2][col % 2])
    }
}

/// Mean of each colour in the 3×3 around `(row, col)`, cube roots
/// averaged, and the "opposed" estimate for that photosite's colour: the
/// mean of the other two colours (darktable's `_calc_refavg`, linear).
fn refavg(m: &Mosaic<'_>, row: usize, col: usize) -> f32 {
    let mut mean = [0.0f32; 3];
    let mut cnt = [0.0f32; 3];
    let ymin = row.saturating_sub(1);
    let xmin = col.saturating_sub(1);
    let ymax = (m.height - 1).min(row + 2);
    let xmax = (m.width - 1).min(col + 2);
    for y in ymin..ymax {
        for x in xmin..xmax {
            let v = m.data[y * m.width + x].max(0.0);
            let c = m.fc(y, x);
            mean[c] += v;
            cnt[c] += 1.0;
        }
    }
    for c in 0..3 {
        mean[c] = if cnt[c] > 0.0 {
            (mean[c] / cnt[c]).cbrt()
        } else {
            0.0
        };
    }
    let opp = [
        0.5 * (mean[1] + mean[2]),
        0.5 * (mean[0] + mean[2]),
        0.5 * (mean[0] + mean[1]),
    ];
    let v = opp[m.fc(row, col)];
    v * v * v
}

/// A clip mask dilated by darktable's radius-3 pattern (`_mask_dilated`).
fn dilated(mask: &[u8], mx: usize, w1: usize) -> u8 {
    let at = |d: isize| mask[(mx as isize + d) as usize];
    let w1 = w1 as isize;
    if at(0) != 0 {
        return 1;
    }
    let near = [-w1 - 1, -w1, -w1 + 1, -1, 1, w1 - 1, w1, w1 + 1];
    if near.iter().any(|&d| at(d) != 0) {
        return 1;
    }
    let (w2, w3) = (2 * w1, 3 * w1);
    let far = [
        -w3 - 2,
        -w3 - 1,
        -w3,
        -w3 + 1,
        -w3 + 2, //
        -w2 - 3,
        -w2 - 2,
        -w2 - 1,
        -w2,
        -w2 + 1,
        -w2 + 2,
        -w2 + 3, //
        -w1 - 3,
        -w1 - 2,
        -w1 + 2,
        -w1 + 3, //
        -3,
        -2,
        2,
        3, //
        w1 - 3,
        w1 - 2,
        w1 + 2,
        w1 + 3, //
        w2 - 3,
        w2 - 2,
        w2 - 1,
        w2,
        w2 + 1,
        w2 + 2,
        w2 + 3, //
        w3 - 2,
        w3 - 1,
        w3,
        w3 + 1,
        w3 + 2,
    ];
    u8::from(far.iter().any(|&d| at(d) != 0))
}

/// "Inpaint opposed" highlight reconstruction. `clips` are each colour's
/// clip level in the (white-balanced) data. Clipped photosites are raised
/// to the mean of the opposing colours around them, corrected by the
/// average chrominance found just outside the clipped areas. Returns the
/// reconstructed mosaic.
pub fn inpaint_opposed(m: &Mosaic<'_>, clips: [f32; 3]) -> Vec<f32> {
    let (w, h) = (m.width, m.height);
    let (mw, mh) = (w / 3, h / 3);
    let msize = mw * mh;
    let mut chroma = [0.0f32; 3];
    if msize > 0 {
        let mut mask = vec![0u8; 3 * msize];
        let mut any = false;
        for mrow in 0..mh {
            for mcol in 0..mw {
                for y in 3 * mrow..3 * mrow + 3 {
                    for x in 3 * mcol..3 * mcol + 3 {
                        let c = m.fc(y, x);
                        if m.data[y * w + x] >= clips[c] {
                            mask[c * msize + mrow * mw + mcol] = 1;
                            any = true;
                        }
                    }
                }
            }
        }
        if any {
            let mut grown = vec![0u8; 3 * msize];
            for row in 0..mh {
                for col in 0..mw {
                    let mx = row * mw + col;
                    let safe = col >= 3 && row >= 3 && col + 3 < mw && row + 3 < mh;
                    for c in 0..3 {
                        grown[c * msize + mx] = if safe {
                            dilated(&mask[c * msize..(c + 1) * msize], mx, mw)
                        } else {
                            mask[c * msize + mx]
                        };
                    }
                }
            }
            let (sums, cnts) = (0..h)
                .into_par_iter()
                .map(|row| {
                    let mut s = [0.0f64; 3];
                    let mut n = [0.0f64; 3];
                    for col in 0..w {
                        let c = m.fc(row, col);
                        let v = m.data[row * w + col];
                        let (r3, c3) = (row / 3, col / 3);
                        if v < clips[c]
                            && v > 0.2 * clips[c]
                            && r3 < mh
                            && c3 < mw
                            && grown[c * msize + r3 * mw + c3] != 0
                        {
                            s[c] += f64::from(v - refavg(m, row, col));
                            n[c] += 1.0;
                        }
                    }
                    (s, n)
                })
                .reduce(
                    || ([0.0; 3], [0.0; 3]),
                    |a, b| {
                        (
                            [a.0[0] + b.0[0], a.0[1] + b.0[1], a.0[2] + b.0[2]],
                            [a.1[0] + b.1[0], a.1[1] + b.1[1], a.1[2] + b.1[2]],
                        )
                    },
                );
            for c in 0..3 {
                chroma[c] = if cnts[c] > 100.0 {
                    (sums[c] / cnts[c]) as f32
                } else {
                    0.0
                };
            }
        }
    }
    let mut out = m.data.to_vec();
    out.par_chunks_mut(w).enumerate().for_each(|(row, line)| {
        for (col, o) in line.iter_mut().enumerate() {
            let c = m.fc(row, col);
            if *o >= clips[c] {
                *o = o.max(refavg(m, row, col) + chroma[c]);
            }
        }
    });
    out
}

/// Clip every photosite to the lowest colour's clip level: blown areas go
/// neutral white (cheap; the proxy's fallback).
pub fn clip(m: &Mosaic<'_>, clips: [f32; 3]) -> Vec<f32> {
    let level = clips[0].min(clips[1]).min(clips[2]);
    m.data.iter().map(|v| v.clamp(0.0, level)).collect()
}

/// RGB, interleaved, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct Rgb {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

impl Rgb {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0.0; width * height * 3],
        }
    }

    #[inline]
    pub fn px(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.width + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }
}

/// Bilinear demosaic: each colour is the mean of its photosites in the 3×3
/// (darktable's `demosaic_box3`).
pub fn bilinear(m: &Mosaic<'_>) -> Rgb {
    let (w, h) = (m.width, m.height);
    let mut out = Rgb::new(w, h);
    out.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(row, line)| {
            for col in 0..w {
                let mut sum = [0.0f32; 3];
                let mut cnt = [0.0f32; 3];
                for y in row.saturating_sub(1)..(row + 2).min(h) {
                    for x in col.saturating_sub(1)..(col + 2).min(w) {
                        let c = m.fc(y, x);
                        sum[c] += m.data[y * w + x].max(0.0);
                        cnt[c] += 1.0;
                    }
                }
                for c in 0..3 {
                    line[col * 3 + c] = sum[c] / cnt[c].max(1.0);
                }
            }
        });
    out
}

const EPS: f32 = 1e-5;
const EPSSQ: f32 = 1e-10;
/// RCD leaves this many pixels at each edge to the bilinear demosaic.
const RCD_MARGIN: usize = 9;

#[inline]
fn sq(v: f32) -> f32 {
    v * v
}

#[inline]
fn interp(a: f32, b: f32, c: f32) -> f32 {
    a * (b - c) + c
}

/// Run `f(row, line)` for the rows `rows` of `buf` (rows of `w`), in
/// parallel. Each RCD step writes one buffer and reads others, so rows are
/// independent.
fn par_rows(
    buf: &mut [f32],
    w: usize,
    rows: std::ops::Range<usize>,
    f: impl Fn(usize, &mut [f32]) + Sync + Send,
) {
    buf.par_chunks_mut(w)
        .enumerate()
        .filter(|(r, _)| rows.contains(r))
        .for_each(|(r, line)| f(r, line));
}

/// The refined discrimination: the centre's, or its diagonal neighbours'
/// mean when that is more decided.
#[inline]
fn refined(dir: &[f32], i: usize, w1: usize) -> f32 {
    let central = dir[i];
    let neighbourhood =
        0.25 * (dir[i - w1 - 1] + dir[i - w1 + 1] + dir[i + w1 - 1] + dir[i + w1 + 1]);
    let disc = if (0.5 - central).abs() < (0.5 - neighbourhood).abs() {
        neighbourhood
    } else {
        central
    };
    disc.clamp(0.0, 1.0)
}

/// Ratio Corrected Demosaicing (see the module notes).
pub fn rcd(m: &Mosaic<'_>) -> Rgb {
    let mut out = bilinear(m);
    let (w, h) = (m.width, m.height);
    if w < 2 * RCD_MARGIN + 2 || h < 2 * RCD_MARGIN + 2 {
        return out;
    }
    let scaler = m.data.iter().copied().fold(0.0f32, f32::max).max(1e-6);
    let rev = 1.0 / scaler;
    let n = w * h;
    let cfa: Vec<f32> = m.data.par_iter().map(|v| v.max(0.0) * rev).collect();
    let cfa = &cfa[..];
    let fc = |r: usize, c: usize| m.fc(r, c);
    let is_rb = |r: usize, c: usize| fc(r, c) != 1;
    let (w1, w2, w3, w4) = (w, 2 * w, 3 * w, 4 * w);

    // Step 0: each channel starts as the photosite values of the colours
    // in its row (the rest is filled below).
    let mut rgb = [vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]];
    for (c, chan) in rgb.iter_mut().enumerate() {
        par_rows(chan, w, 0..h, |row, line| {
            if fc(row, 0) == c || fc(row, 1) == c {
                line.copy_from_slice(&cfa[row * w..(row + 1) * w]);
            }
        });
    }

    // Step 1: vertical / horizontal discrimination.
    let hpf = |i: usize, s: usize| {
        sq((cfa[i - 3 * s] - cfa[i - s] - cfa[i + s] + cfa[i + 3 * s])
            - 3.0 * (cfa[i - 2 * s] + cfa[i + 2 * s])
            + 6.0 * cfa[i])
    };
    let mut vhpf = vec![0.0f32; n];
    par_rows(&mut vhpf, w, 3..h - 3, |row, line| {
        for col in 4..w - 4 {
            line[col] = hpf(row * w + col, w1);
        }
    });
    let mut hhpf = vec![0.0f32; n];
    par_rows(&mut hhpf, w, 4..h - 4, |row, line| {
        for col in 3..w - 3 {
            line[col] = hpf(row * w + col, 1);
        }
    });
    let mut vh_dir = vec![0.0f32; n];
    par_rows(&mut vh_dir, w, 4..h - 4, |row, line| {
        for col in 4..w - 4 {
            let i = row * w + col;
            let v = EPSSQ.max(vhpf[i - w1] + vhpf[i] + vhpf[i + w1]);
            let hs = EPSSQ.max(hhpf[i - 1] + hhpf[i] + hhpf[i + 1]);
            line[col] = v / (v + hs);
        }
    });

    // Step 2: low-pass filter at red and blue photosites.
    let mut lpf = vec![0.0f32; n];
    par_rows(&mut lpf, w, 2..h - 2, |row, line| {
        for col in 2..w - 2 {
            if !is_rb(row, col) {
                continue;
            }
            let i = row * w + col;
            line[col] = cfa[i]
                + 0.5 * (cfa[i - w1] + cfa[i + w1] + cfa[i - 1] + cfa[i + 1])
                + 0.25 * (cfa[i - w1 - 1] + cfa[i - w1 + 1] + cfa[i + w1 - 1] + cfa[i + w1 + 1]);
        }
    });

    // Step 3: green at red and blue photosites.
    par_rows(&mut rgb[1], w, 4..h - 4, |row, line| {
        for col in 4..w - 4 {
            if !is_rb(row, col) {
                continue;
            }
            let i = row * w + col;
            let cfai = cfa[i];
            let n_grad = EPS
                + (cfa[i - w1] - cfa[i + w1]).abs()
                + (cfai - cfa[i - w2]).abs()
                + (cfa[i - w1] - cfa[i - w3]).abs()
                + (cfa[i - w2] - cfa[i - w4]).abs();
            let s_grad = EPS
                + (cfa[i + w1] - cfa[i - w1]).abs()
                + (cfai - cfa[i + w2]).abs()
                + (cfa[i + w1] - cfa[i + w3]).abs()
                + (cfa[i + w2] - cfa[i + w4]).abs();
            let w_grad = EPS
                + (cfa[i - 1] - cfa[i + 1]).abs()
                + (cfai - cfa[i - 2]).abs()
                + (cfa[i - 1] - cfa[i - 3]).abs()
                + (cfa[i - 2] - cfa[i - 4]).abs();
            let e_grad = EPS
                + (cfa[i + 1] - cfa[i - 1]).abs()
                + (cfai - cfa[i + 2]).abs()
                + (cfa[i + 1] - cfa[i + 3]).abs()
                + (cfa[i + 2] - cfa[i + 4]).abs();
            let lpfi = lpf[i];
            let n_est = cfa[i - w1] * (lpfi + lpfi) / (EPS + lpfi + lpf[i - w2]);
            let s_est = cfa[i + w1] * (lpfi + lpfi) / (EPS + lpfi + lpf[i + w2]);
            let w_est = cfa[i - 1] * (lpfi + lpfi) / (EPS + lpfi + lpf[i - 2]);
            let e_est = cfa[i + 1] * (lpfi + lpfi) / (EPS + lpfi + lpf[i + 2]);
            let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
            let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);
            line[col] = interp(refined(&vh_dir, i, w1), h_est, v_est);
        }
    });

    // Step 4.0: diagonal high-pass, P (NW–SE) and Q (NE–SW).
    let mut p_hpf = vec![0.0f32; n];
    par_rows(&mut p_hpf, w, 3..h - 3, |row, line| {
        for col in 3..w - 3 {
            let i = row * w + col;
            line[col] = sq(
                (cfa[i - w3 - 3] - cfa[i - w1 - 1] - cfa[i + w1 + 1] + cfa[i + w3 + 3])
                    - 3.0 * (cfa[i - w2 - 2] + cfa[i + w2 + 2])
                    + 6.0 * cfa[i],
            );
        }
    });
    let mut q_hpf = vec![0.0f32; n];
    par_rows(&mut q_hpf, w, 3..h - 3, |row, line| {
        for col in 3..w - 3 {
            let i = row * w + col;
            line[col] = sq(
                (cfa[i - w3 + 3] - cfa[i - w1 + 1] - cfa[i + w1 - 1] + cfa[i + w3 - 3])
                    - 3.0 * (cfa[i - w2 + 2] + cfa[i + w2 - 2])
                    + 6.0 * cfa[i],
            );
        }
    });
    // Step 4.1: diagonal discrimination at red and blue photosites.
    let mut pq_dir = vec![0.0f32; n];
    par_rows(&mut pq_dir, w, 4..h - 4, |row, line| {
        for col in 4..w - 4 {
            if !is_rb(row, col) {
                continue;
            }
            let i = row * w + col;
            let p = EPSSQ.max(p_hpf[i - w1 - 1] + p_hpf[i] + p_hpf[i + w1 + 1]);
            let q = EPSSQ.max(q_hpf[i - w1 + 1] + q_hpf[i] + q_hpf[i + w1 - 1]);
            line[col] = p / (p + q);
        }
    });

    // Step 4.2: red at blue photosites and blue at red ones. Each reads its
    // own channel only at the other kind of photosite, so a channel can be
    // written from a copy of itself.
    for c in [0usize, 2] {
        let rc = rgb[c].clone();
        let g = &rgb[1];
        let rc = &rc[..];
        let mut next = rgb[c].clone();
        par_rows(&mut next, w, 4..h - 4, |row, line| {
            for col in 4..w - 4 {
                if fc(row, col) != 2 - c {
                    continue;
                }
                let i = row * w + col;
                let nw_grad = EPS
                    + (rc[i - w1 - 1] - rc[i + w1 + 1]).abs()
                    + (rc[i - w1 - 1] - rc[i - w3 - 3]).abs()
                    + (g[i] - g[i - w2 - 2]).abs();
                let ne_grad = EPS
                    + (rc[i - w1 + 1] - rc[i + w1 - 1]).abs()
                    + (rc[i - w1 + 1] - rc[i - w3 + 3]).abs()
                    + (g[i] - g[i - w2 + 2]).abs();
                let sw_grad = EPS
                    + (rc[i - w1 + 1] - rc[i + w1 - 1]).abs()
                    + (rc[i + w1 - 1] - rc[i + w3 - 3]).abs()
                    + (g[i] - g[i + w2 - 2]).abs();
                let se_grad = EPS
                    + (rc[i - w1 - 1] - rc[i + w1 + 1]).abs()
                    + (rc[i + w1 + 1] - rc[i + w3 + 3]).abs()
                    + (g[i] - g[i + w2 + 2]).abs();
                let nw_est = rc[i - w1 - 1] - g[i - w1 - 1];
                let ne_est = rc[i - w1 + 1] - g[i - w1 + 1];
                let sw_est = rc[i + w1 - 1] - g[i + w1 - 1];
                let se_est = rc[i + w1 + 1] - g[i + w1 + 1];
                let p_est = (nw_grad * se_est + se_grad * nw_est) / (nw_grad + se_grad);
                let q_est = (ne_grad * sw_est + sw_grad * ne_est) / (ne_grad + sw_grad);
                line[col] = g[i] + interp(refined(&pq_dir, i, w1), q_est, p_est);
            }
        });
        rgb[c] = next;
    }
    // Step 4.3: red and blue at green photosites (reading them only at red
    // and blue ones).
    for c in [0usize, 2] {
        let rc = rgb[c].clone();
        let g = &rgb[1];
        let rc = &rc[..];
        let mut next = rgb[c].clone();
        par_rows(&mut next, w, 4..h - 4, |row, line| {
            for col in 4..w - 4 {
                if is_rb(row, col) {
                    continue;
                }
                let i = row * w + col;
                let rgb1 = g[i];
                let n1 = EPS + (rgb1 - g[i - w2]).abs();
                let s1 = EPS + (rgb1 - g[i + w2]).abs();
                let w1g = EPS + (rgb1 - g[i - 2]).abs();
                let e1 = EPS + (rgb1 - g[i + 2]).abs();
                let sn = (rc[i - w1] - rc[i + w1]).abs();
                let ew = (rc[i - 1] - rc[i + 1]).abs();
                let n_grad = n1 + sn + (rc[i - w1] - rc[i - w3]).abs();
                let s_grad = s1 + sn + (rc[i + w1] - rc[i + w3]).abs();
                let w_grad = w1g + ew + (rc[i - 1] - rc[i - 3]).abs();
                let e_grad = e1 + ew + (rc[i + 1] - rc[i + 3]).abs();
                let n_est = rc[i - w1] - g[i - w1];
                let s_est = rc[i + w1] - g[i + w1];
                let w_est = rc[i - 1] - g[i - 1];
                let e_est = rc[i + 1] - g[i + 1];
                let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
                let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
                line[col] = rgb1 + interp(refined(&vh_dir, i, w1), h_est, v_est);
            }
        });
        rgb[c] = next;
    }

    out.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .filter(|(row, _)| (RCD_MARGIN..h - RCD_MARGIN).contains(row))
        .for_each(|(row, line)| {
            for col in RCD_MARGIN..w - RCD_MARGIN {
                let i = row * w + col;
                for c in 0..3 {
                    line[col * 3 + c] = scaler * rgb[c][i].max(0.0);
                }
            }
        });
    out
}
