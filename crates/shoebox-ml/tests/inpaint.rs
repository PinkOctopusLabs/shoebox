//! MI-GAN on drawn pictures: a hole in a smooth gradient, and a post in
//! front of stripes, come back close to the picture without them, and the
//! known pixels are drawn back nearly as they were. Needs the checkpoint:
//! `SHOEBOX_INPAINT_MODEL=<folder with migan_512_places2.pt>`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_ml::inpaint::{FILE, Inpainter, SIZE};

/// Mean absolute difference (0–1 values) over pixels where `pick` holds.
fn diff(a: &[f32], b: &[f32], pick: impl Fn(usize) -> bool) -> f32 {
    let (mut s, mut n) = (0.0, 0.0);
    for i in 0..SIZE * SIZE {
        if pick(i) {
            for c in 0..3 {
                s += (a[i * 3 + c] - b[i * 3 + c]).abs();
            }
            n += 3.0;
        }
    }
    s / n
}

#[test]
#[ignore = "needs the MI-GAN checkpoint (SHOEBOX_INPAINT_MODEL)"]
fn fills_holes_from_their_surroundings() {
    let Ok(dir) = std::env::var("SHOEBOX_INPAINT_MODEL") else {
        eprintln!("skipping: SHOEBOX_INPAINT_MODEL not set");
        return;
    };
    let device = shoebox_ml::device();
    let t = std::time::Instant::now();
    let net = Inpainter::load(&std::path::Path::new(&dir).join(FILE), &device).unwrap();
    eprintln!("load {:.2} s", t.elapsed().as_secs_f32());

    // A sky-like gradient, a disc of 60 px radius missing.
    let n = SIZE * SIZE;
    let mut sky = vec![0f32; 3 * n];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let t = y as f32 / SIZE as f32;
            let i = (y * SIZE + x) * 3;
            sky[i] = 0.3 + 0.4 * t;
            sky[i + 1] = 0.5 + 0.3 * t;
            sky[i + 2] = 0.85 + 0.1 * t;
        }
    }
    let hole = |i: usize| {
        let (x, y) = ((i % SIZE) as f32 - 256.0, (i / SIZE) as f32 - 256.0);
        x.hypot(y) < 60.0
    };
    let known: Vec<bool> = (0..n).map(|i| !hole(i)).collect();
    let mut with = sky.clone();
    for i in 0..n {
        if hole(i) {
            // Something dark there, which the network mustn't see.
            with[i * 3..i * 3 + 3].copy_from_slice(&[0.1, 0.1, 0.1]);
        }
    }
    for run in 0..2 {
        let t = std::time::Instant::now();
        let out = net.inpaint(&with, &known).unwrap();
        eprintln!("inpaint ({run}) {:.2} s", t.elapsed().as_secs_f32());
        let inside = diff(&out, &sky, hole);
        let outside = diff(&out, &sky, |i| !hole(i));
        eprintln!("gradient: hole off by {inside:.4}, the rest by {outside:.4}");
        assert!(inside < 0.04, "the hole follows the gradient: {inside}");
        // Only the hole is trained on (the rest is composited back), so the
        // rest is only roughly drawn.
        assert!(outside < 0.1, "the rest drawn back: {outside}");
    }

    // Vertical stripes (32 px), a post 20 px wide across them.
    let stripes: Vec<f32> = (0..n)
        .flat_map(|i| {
            let x = i % SIZE;
            if (x / 16).is_multiple_of(2) {
                [0.75, 0.6, 0.45]
            } else {
                [0.35, 0.25, 0.2]
            }
        })
        .collect();
    let post = |i: usize| {
        let (x, y) = (i % SIZE, i / SIZE);
        (246..266).contains(&x) && (96..416).contains(&y)
    };
    let known: Vec<bool> = (0..n).map(|i| !post(i)).collect();
    let out = net.inpaint(&stripes, &known).unwrap();
    if let Some(d) = std::env::var_os("SHOEBOX_INPAINT_OUT") {
        let bytes: Vec<u8> = out.iter().map(|v| (v * 255.0).round() as u8).collect();
        image::RgbImage::from_raw(SIZE as u32, SIZE as u32, bytes)
            .unwrap()
            .save(std::path::Path::new(&d).join("stripes.png"))
            .unwrap();
    }
    let inside = diff(&out, &stripes, post);
    // Against the post's own place filled with the stripes' mean (what a
    // blur would give), the network should do better.
    let mean = [0.55f32, 0.425, 0.325];
    let flat: Vec<f32> = (0..n).flat_map(|_| mean).collect();
    let blur = diff(&flat, &stripes, post);
    eprintln!("stripes: hole off by {inside:.4} (a flat fill {blur:.4})");
    assert!(inside < 0.6 * blur, "{inside} against {blur}");
}
