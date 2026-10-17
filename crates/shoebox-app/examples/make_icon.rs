//! Draws the app icon (1024×1024 PNG): a cardboard shoebox with a photo
//! print peeking out, on a rounded macOS-style tile.
//!
//! `cargo run -p shoebox-app --example make_icon -- assets/icon-1024.png`
#![allow(clippy::unwrap_used, clippy::expect_used)]

use image::{Rgba, RgbaImage};

const N: u32 = 1024;
/// Supersampling per axis.
const SS: u32 = 4;

/// Signed distance to a rounded rectangle centred at (cx, cy).
fn rrect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (hw - r);
    let qy = (py - cy).abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Rotate (px, py) about (cx, cy) by `a` radians.
fn rot(px: f32, py: f32, cx: f32, cy: f32, a: f32) -> (f32, f32) {
    let (s, c) = a.sin_cos();
    let (dx, dy) = (px - cx, py - cy);
    (cx + dx * c - dy * s, cy + dx * s + dy * c)
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

/// Paint `top` over `under` with coverage `alpha`.
fn over(under: [f32; 4], top: [f32; 3], alpha: f32) -> [f32; 4] {
    let a = alpha.clamp(0.0, 1.0);
    [
        under[0] * (1.0 - a) + top[0] * a,
        under[1] * (1.0 - a) + top[1] * a,
        under[2] * (1.0 - a) + top[2] * a,
        under[3] + (1.0 - under[3]) * a,
    ]
}

fn rgb(hex: u32) -> [f32; 3] {
    [
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ]
}

/// Inside test for one sample (hard edge; supersampling smooths it).
fn inside(d: f32) -> f32 {
    if d <= 0.0 { 1.0 } else { 0.0 }
}

fn sample(x: f32, y: f32) -> [f32; 4] {
    let mut c = [0.0, 0.0, 0.0, 0.0];
    // The tile: macOS icon grid (824 px square in 1024, corner ~185).
    let tile = rrect(x, y, 512.0, 512.0, 412.0, 412.0, 185.0);
    if tile > 0.0 {
        // Soft drop shadow below the tile.
        let sh = rrect(x, y, 512.0, 530.0, 412.0, 412.0, 185.0);
        let a = (1.0 - (sh / 28.0)).clamp(0.0, 1.0) * 0.28;
        return [0.0, 0.0, 0.0, a * a];
    }
    // Background: deep teal to night blue.
    let t = ((y - 100.0) / 824.0).clamp(0.0, 1.0);
    let bg = mix([0.10, 0.36, 0.42, 1.0], [0.06, 0.13, 0.25, 1.0], t);
    c = over(c, [bg[0], bg[1], bg[2]], 1.0);

    // A print standing in the box, tilted, behind the box front.
    let (px, py) = rot(x, y, 560.0, 380.0, -0.16);
    let print = rrect(px, py, 560.0, 380.0, 185.0, 150.0, 10.0);
    c = over(c, rgb(0xfdfaf2), inside(print));
    // The picture on the print: sky, sea, sun, hills.
    let pic = rrect(px, py, 560.0, 368.0, 160.0, 118.0, 4.0);
    if pic <= 0.0 {
        let s = ((py - 250.0) / 236.0).clamp(0.0, 1.0);
        let sky = mix([0.98, 0.62, 0.36, 1.0], [0.98, 0.84, 0.55, 1.0], s);
        c = over(c, [sky[0], sky[1], sky[2]], 1.0);
        let sun = ((px - 610.0).powi(2) + (py - 360.0).powi(2)).sqrt() - 42.0;
        c = over(c, rgb(0xfff4d6), inside(sun));
        let hill = py - (420.0 + 30.0 * ((px - 400.0) / 90.0).sin());
        c = over(c, rgb(0x2f6f73), inside(-hill));
        let sea = py - 455.0;
        c = over(c, rgb(0x1f4f6b), inside(-sea));
    }

    // The box: lid tilted open at the back, then the body.
    let (lx, ly) = rot(x, y, 512.0, 470.0, 0.05);
    let lid = rrect(lx, ly, 470.0, 455.0, 300.0, 34.0, 10.0);
    c = over(c, rgb(0xb98a55), inside(lid));
    let body = rrect(x, y, 512.0, 650.0, 300.0, 175.0, 18.0);
    let shade = ((y - 475.0) / 350.0).clamp(0.0, 1.0);
    let card = mix([0.86, 0.66, 0.42, 1.0], [0.72, 0.52, 0.31, 1.0], shade);
    c = over(c, [card[0], card[1], card[2]], inside(body));
    // Rim of the box opening and a label.
    let rim = rrect(x, y, 512.0, 490.0, 300.0, 16.0, 8.0);
    c = over(c, rgb(0x9c6f40), inside(rim));
    let label = rrect(x, y, 512.0, 660.0, 120.0, 58.0, 10.0);
    c = over(c, rgb(0xf6efe0), inside(label));
    // Three lines on the label.
    for (i, w) in [86.0f32, 66.0, 76.0].into_iter().enumerate() {
        let line = rrect(x, y, 512.0, 630.0 + i as f32 * 28.0, w, 6.0, 6.0);
        c = over(c, rgb(0xb79a74), inside(line));
    }
    c
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/icon-1024.png".into());
    let mut img = RgbaImage::new(N, N);
    for y in 0..N {
        for x in 0..N {
            let mut acc = [0.0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let s = sample(fx, fy);
                    // Premultiplied accumulation.
                    acc[0] += s[0] * s[3];
                    acc[1] += s[1] * s[3];
                    acc[2] += s[2] * s[3];
                    acc[3] += s[3];
                }
            }
            let n = (SS * SS) as f32;
            let a = acc[3] / n;
            let px = if a > 0.0 {
                [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], a]
            } else {
                [0.0; 4]
            };
            img.put_pixel(
                x,
                y,
                Rgba(px.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)),
            );
        }
    }
    if let Some(dir) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    img.save(&out).unwrap();
    eprintln!("wrote {out}");
}
