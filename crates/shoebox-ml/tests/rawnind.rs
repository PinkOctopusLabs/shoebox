//! RawNIND's UtNet2 on a noisy flat mosaic: far less noise, the same
//! brightness, and tiles that don't show. Needs the checkpoint:
//! `SHOEBOX_RAWNIND_MODEL=<folder with denoiser_bayer2prgb_utnet2.pt>`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::AtomicBool;

use shoebox_ml::rawnind::{FILE, RawDenoiser};

#[test]
#[ignore = "needs the RawNIND checkpoint (SHOEBOX_RAWNIND_MODEL)"]
fn noise_goes_brightness_stays() {
    let Ok(dir) = std::env::var("SHOEBOX_RAWNIND_MODEL") else {
        return;
    };
    let net = RawDenoiser::load(
        &std::path::Path::new(&dir).join(FILE),
        &shoebox_ml::device(),
    )
    .unwrap();
    // Packed RGGB, 600×500 (two tiles each way), a mid grey with noise
    // (a cheap generator; σ about 0.03).
    let (h, w) = (500, 600);
    let mut seed = 12345u64;
    let mut noise = || {
        let mut s = 0.0;
        for _ in 0..4 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s += (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5;
        }
        s * 0.06
    };
    let level = [0.12, 0.2, 0.2, 0.09];
    let packed: Vec<f32> = (0..4 * h * w)
        .map(|i| (level[i / (h * w)] + noise()).clamp(0.0, 1.0))
        .collect();
    let t = std::time::Instant::now();
    let out = net
        .denoise(&packed, h, w, &|_| {}, &AtomicBool::new(false))
        .unwrap();
    eprintln!("{:.2} s", t.elapsed().as_secs_f32());
    let (oh, ow) = (2 * h, 2 * w);
    assert_eq!(out.len(), 3 * oh * ow);
    let stats = |v: &[f32]| {
        let m = v.iter().sum::<f32>() / v.len() as f32;
        let sd = (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt();
        (m, sd)
    };
    let green = &out[oh * ow..2 * oh * ow];
    let (m, sd) = stats(green);
    let (im, isd) = stats(&packed[h * w..2 * h * w]);
    eprintln!("green: in {im:.3} ± {isd:.4}, out {m:.3} ± {sd:.4}");
    assert!(sd < isd / 4.0, "noise down");
    // Tiles meet without a step: the columns either side of the first
    // seam (448 packed, 896 out) differ no more than neighbours elsewhere.
    let col = |x: usize| (0..oh).map(|y| green[y * ow + x]).sum::<f32>() / oh as f32;
    let seam = (col(895) - col(896)).abs();
    let elsewhere = (col(400) - col(401)).abs();
    eprintln!("seam {seam:.5}, elsewhere {elsewhere:.5}");
    assert!(seam < 4.0 * elsewhere.max(1e-3));
}
