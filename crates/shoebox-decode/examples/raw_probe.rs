//! What Develop's raw decode makes of some files: compression, size, CFA,
//! white balance, matrices and time.
//! `cargo run --release -p shoebox-decode --example raw_probe -- <file>...`
//! With `SHOEBOX_RAW_PNG=<dir>`, also writes a rough half-size sRGB preview
//! of each (2×2 binning, as-shot white balance, the warmest-but-one
//! matrix): enough to see that the pattern and levels are right.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

fn main() {
    for p in std::env::args().skip(1) {
        let path = Path::new(&p);
        let data = std::fs::read(path).expect("read");
        let compression = shoebox_decode::nef::compression(&data).ok().flatten();
        let t = std::time::Instant::now();
        match shoebox_decode::raw::decode(&data) {
            Ok(r) => {
                if let Some(dir) = std::env::var_os("SHOEBOX_RAW_PNG") {
                    let name = path.file_stem().unwrap().to_string_lossy().to_string();
                    preview(&r, &Path::new(&dir).join(format!("{name}.png")));
                }
                let max = r.data.iter().copied().fold(0.0f32, f32::max);
                let clipped = r
                    .data
                    .iter()
                    .filter(|&&v| v >= shoebox_decode::raw::CLIP)
                    .count();
                println!(
                    "{p}: {} {} {}×{} cfa {:?} as shot {:?} matrices {:?} compression {:?}; \
                     max {max:.3}, {clipped} clipped; {:?}",
                    r.make,
                    r.model,
                    r.width,
                    r.height,
                    r.cfa,
                    r.as_shot,
                    r.matrices.iter().map(|m| m.temperature).collect::<Vec<_>>(),
                    compression.map(|c| c.label()),
                    t.elapsed()
                );
            }
            Err(e) => println!(
                "{p}: {e} (compression {:?})",
                compression.map(|c| c.label())
            ),
        }
    }
}

/// Half-size sRGB from 2×2 binning: WB, camera → XYZ (D65 matrix) → sRGB.
fn preview(r: &shoebox_decode::raw::RawImage, out: &Path) {
    let wb = r.as_shot.unwrap_or([1.0; 3]);
    let m = &r.matrices.last().unwrap().xyz_to_cam;
    // sRGB → XYZ (D65), so cam ← sRGB = m · srgb_to_xyz, normalised per row.
    let s2x = [
        [0.4124, 0.3576, 0.1805],
        [0.2126, 0.7152, 0.0722],
        [0.0193, 0.1192, 0.9505],
    ];
    let mut c = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            c[i][j] = (0..3).map(|k| m[i][k] * s2x[k][j]).sum();
        }
        let s: f32 = c[i].iter().sum();
        c[i].iter_mut().for_each(|v| *v /= s);
    }
    let inv = invert(c);
    let (w, h) = (r.width / 2, r.height / 2);
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let mut cam = [0.0f32; 3];
            let mut n = [0.0f32; 3];
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (sx, sy) = (2 * x + dx, 2 * y + dy);
                let col = usize::from(r.color_at(sx, sy));
                cam[col] += r.data[sy * r.width + sx];
                n[col] += 1.0;
            }
            let cam: Vec<f32> = (0..3).map(|i| (cam[i] / n[i] * wb[i]).min(1.0)).collect();
            for row in &inv {
                let v: f32 = (0..3).map(|k| row[k] * cam[k]).sum::<f32>().clamp(0.0, 1.0);
                let g = if v <= 0.0031308 {
                    12.92 * v
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                };
                px.push((g * 255.0 + 0.5) as u8);
            }
        }
    }
    let f = std::fs::File::create(out).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&px).unwrap();
    println!("  preview: {}", out.display());
}

fn invert(m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let d = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let c = |a: usize, b: usize, e: usize, f: usize| m[a][b] * m[e][f] - m[a][f] * m[e][b];
    [
        [c(1, 1, 2, 2) / d, -c(0, 1, 2, 2) / d, c(0, 1, 1, 2) / d],
        [-c(1, 0, 2, 2) / d, c(0, 0, 2, 2) / d, -c(0, 0, 1, 2) / d],
        [c(1, 0, 2, 1) / d, -c(0, 0, 2, 1) / d, c(0, 0, 1, 1) / d],
    ]
}
