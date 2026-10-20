//! How a camera JPEG's in-camera distortion correction maps onto the raw:
//! block-match the raw (rendered uncorrected) against the camera's JPEG,
//! then compare candidate models for the NEF's distortion coefficients.
//!
//! `cargo run --release -p shoebox-develop --example lens_probe -- <nef> <jpg> <k1> <k2> <k3>`
//! (a High efficiency NEF needs Adobe DNG Converter; cached in the temp dir).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::{self, Options};
use shoebox_develop::dng::{Converter, DngCache, needs_dng};
use shoebox_develop::edit::Settings;

const HALF: usize = 2; // work at half size

fn luma_half(rgb: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let (hw, hh) = (w / HALF, h / HALF);
    let mut out = vec![0f32; hw * hh];
    for y in 0..hh {
        for x in 0..hw {
            let mut s = 0.0;
            for dy in 0..HALF {
                for dx in 0..HALF {
                    let i = ((y * HALF + dy) * w + x * HALF + dx) * 3;
                    s += 0.3 * rgb[i] + 0.6 * rgb[i + 1] + 0.1 * rgb[i + 2];
                }
            }
            out[y * hw + x] = s / (HALF * HALF) as f32;
        }
    }
    (out, hw, hh)
}

/// The best whole-pixel shift of `b` matching the block of `a` at `(x, y)`,
/// by normalised cross-correlation, and its score.
fn best_shift(
    a: &[f32],
    b: &[f32],
    w: usize,
    x: usize,
    y: usize,
    n: usize,
    r: isize,
) -> (isize, isize, f32) {
    let block = |img: &[f32], x0: usize, y0: usize| -> Vec<f32> {
        let mut v = Vec::with_capacity(n * n);
        for yy in y0..y0 + n {
            v.extend_from_slice(&img[yy * w + x0..yy * w + x0 + n]);
        }
        let m = v.iter().sum::<f32>() / v.len() as f32;
        let sd = (v.iter().map(|p| (p - m).powi(2)).sum::<f32>())
            .sqrt()
            .max(1e-9);
        v.iter().map(|p| (p - m) / sd).collect()
    };
    let ref_block = block(a, x, y);
    let mut best = (0, 0, -1.0f32);
    for dy in -r..=r {
        for dx in -r..=r {
            let (bx, by) = (x as isize + dx, y as isize + dy);
            let b2 = block(b, bx as usize, by as usize);
            let s: f32 = ref_block.iter().zip(&b2).map(|(p, q)| p * q).sum();
            if s > best.2 {
                best = (dx, dy, s);
            }
        }
    }
    best
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (nef, jpg) = (&args[0], &args[1]);
    let k: Vec<f64> = args[2..5].iter().map(|v| v.parse().unwrap()).collect();
    let bytes = std::fs::read(nef).unwrap();
    let cache = DngCache::new(&std::env::temp_dir().join("shoebox-lens-probe"), 1 << 34);
    let source = if needs_dng(&bytes) {
        cache
            .get_or_convert(
                &Converter::find(None).unwrap(),
                std::path::Path::new(nef),
                &bytes,
            )
            .unwrap()
    } else {
        nef.into()
    };
    let raw = shoebox_decode::raw::decode_path(&source).unwrap();
    let img = cpu::render(&raw, &Settings::default(), &Options::default());
    let cam = shoebox_decode::render_path(
        shoebox_decode::Format::Jpeg,
        std::path::Path::new(jpg),
        None,
        1,
        shoebox_decode::Target::Srgb,
    )
    .unwrap();
    assert_eq!(
        (cam.width as usize, cam.height as usize),
        (img.width, img.height)
    );
    let (w, h) = (img.width, img.height);
    let (a, hw, hh) = luma_half(
        &cpu::to_u8(&img)
            .iter()
            .map(|v| *v as f32)
            .collect::<Vec<_>>(),
        w,
        h,
    );
    let (b, _, _) = luma_half(&cam.px.iter().map(|v| *v as f32).collect::<Vec<_>>(), w, h);
    // Blocks on a grid: where each raw block sits in the JPEG.
    let (n, r) = (48usize, 24isize);
    let mut pts = Vec::new();
    for gy in 1..12 {
        for gx in 1..16 {
            let x = gx * (hw - n) / 16;
            let y = gy * (hh - n) / 12;
            if x < r as usize
                || y < r as usize
                || x + n + r as usize >= hw
                || y + n + r as usize >= hh
            {
                continue;
            }
            // Skip flat (black) blocks: nothing to match.
            let mut v = Vec::new();
            for yy in y..y + n {
                v.extend_from_slice(&a[yy * hw + x..yy * hw + x + n]);
            }
            let m = v.iter().sum::<f32>() / v.len() as f32;
            let sd = (v.iter().map(|p| (p - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt();
            if sd < 6.0 {
                continue;
            }
            let (dx, dy, s) = best_shift(&a, &b, hw, x, y, n, r);
            eprintln!("block {x},{y} sd {sd:.1}: shift {dx},{dy} score {s:.2}");
            if s > 0.6 {
                // Block centres in full-size pixels, raw → JPEG.
                let (cx, cy) = ((x + n / 2) * HALF, (y + n / 2) * HALF);
                pts.push((
                    cx as f64,
                    cy as f64,
                    (cx as f64) + (dx * HALF as isize) as f64,
                    (cy as f64) + (dy * HALF as isize) as f64,
                ));
            }
        }
    }
    println!("{} matched blocks", pts.len());
    let (ox, oy) = (w as f64 / 2.0, h as f64 / 2.0);
    for (name, norm) in [
        ("half diagonal", (ox.hypot(oy))),
        ("half width", ox),
        ("half height", oy),
    ] {
        for inverse in [false, true] {
            // Model: raw radius = s · jpg radius · (1 + k1 r² + k2 r⁴ + k3 r⁶) (or the
            // jpg radius from the raw one). Fit the scale s by least squares.
            let poly = |r: f64| 1.0 + k[0] * r * r + k[1] * r.powi(4) + k[2] * r.powi(6);
            let mut num = 0.0;
            let mut den = 0.0;
            let mut pred = Vec::new();
            for &(rx, ry, jx, jy) in &pts {
                let rr = (rx - ox).hypot(ry - oy);
                let rj = (jx - ox).hypot(jy - oy);
                let (from, to) = if inverse { (rr, rj) } else { (rj, rr) };
                let model = from * poly(from / norm);
                num += model * to;
                den += model * model;
                pred.push((model, to));
            }
            let s = num / den;
            let rms = (pred.iter().map(|(m, t)| (s * m - t).powi(2)).sum::<f64>()
                / pred.len() as f64)
                .sqrt();
            println!(
                "{name:>14} {}: scale {s:.5}, rms {rms:.3} px",
                if inverse {
                    "jpg = f(raw)"
                } else {
                    "raw = f(jpg)"
                }
            );
        }
    }
    // And no distortion at all (only a scale).
    let mut num = 0.0;
    let mut den = 0.0;
    let mut rs = Vec::new();
    for &(rx, ry, jx, jy) in &pts {
        let rr = (rx - ox).hypot(ry - oy);
        let rj = (jx - ox).hypot(jy - oy);
        num += rj * rr;
        den += rj * rj;
        rs.push((rj, rr));
    }
    let s = num / den;
    let rms = (rs.iter().map(|(j, r)| (s * j - r).powi(2)).sum::<f64>() / rs.len() as f64).sqrt();
    println!("   scale only: scale {s:.5}, rms {rms:.3} px");
}
