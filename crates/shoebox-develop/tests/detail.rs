//! The detail stage (noise reduction, sharpening, texture) on the CPU
//! reference.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::detail::{
    Detail, Noise, backward, estimate, forward, from_opponent, process, to_opponent,
};
use shoebox_develop::cpu::{Demosaiced, Referred, Rgb};
use shoebox_develop::edit::Settings;

/// A tiny deterministic PRNG (xorshift) and a normal deviate.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn normal(&mut self) -> f32 {
        let (u, v) = (self.next().max(1e-7), self.next());
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }
}

/// Left half 0.1, right half 0.4 (an edge), with Poisson-Gaussian noise of
/// gain `a` and read variance `b`.
fn noisy(w: usize, h: usize, a: f32, b: f32) -> (Rgb, Rgb) {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut clean = Rgb::new(w, h);
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = if x < w / 2 { 0.1 } else { 0.4 };
            for c in 0..3 {
                let i = (y * w + x) * 3 + c;
                clean.data[i] = v;
                img.data[i] = v + (a * v + b).sqrt() * rng.normal();
            }
        }
    }
    (clean, img)
}

fn std_dev(img: &Rgb, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f32 {
    let vals: Vec<f32> = ys
        .flat_map(|y| xs.clone().map(move |x| (x, y)))
        .map(|(x, y)| img.px(x, y)[1])
        .collect();
    let m = vals.iter().sum::<f32>() / vals.len() as f32;
    (vals.iter().map(|v| (v - m).powi(2)).sum::<f32>() / vals.len() as f32).sqrt()
}

#[test]
fn the_noise_is_estimated() {
    let (_, img) = noisy(400, 300, 2e-4, 4e-6);
    let n = estimate(&img);
    for c in 0..3 {
        let at = |v: f32| n.a[c] * v + n.b[c];
        let want = |v: f32| 2e-4 * v + 4e-6;
        for v in [0.1, 0.4] {
            let r = at(v) / want(v);
            assert!((0.5..2.0).contains(&r), "channel {c} at {v}: {r} ({n:?})");
        }
    }
}

#[test]
fn transforms_round_trip() {
    let n = Noise {
        a: [2e-4, 1e-4, 3e-4],
        b: [4e-6, 1e-6, 2e-6],
    };
    let d = Detail::new(
        &{
            let mut s = Settings::default();
            s.modules.noise_reduction.luminance = 10.0;
            s
        },
        n,
        1.0,
    )
    .unwrap();
    for p in [[0.01, 0.2, 0.9], [0.5, 0.5, 0.5], [0.0, 1.5, 0.3]] {
        let q = backward(&d, forward(&d, p));
        assert!(
            p.iter().zip(q).all(|(a, b)| (a - b).abs() < 1e-4),
            "{p:?} {q:?}"
        );
        let o = from_opponent(to_opponent(p));
        assert!(p.iter().zip(o).all(|(a, b)| (a - b).abs() < 1e-6));
    }
    assert!(Detail::new(&Settings::default(), n, 1.0).is_none());
}

#[test]
fn denoise_smooths_flat_areas_and_keeps_the_edge() {
    let (clean, img) = noisy(256, 192, 2e-4, 4e-6);
    let src = Demosaiced::new(img.clone(), [1.0; 3], Referred::Scene);
    let mut s = Settings::default();
    s.modules.noise_reduction.luminance = 80.0;
    s.modules.noise_reduction.chroma = 80.0;
    s.modules.noise_reduction.detail = 0.0;
    let out = src.detailed(&s).unwrap();
    let before = std_dev(&img, 20..100, 20..170);
    let after = std_dev(&out, 20..100, 20..170);
    assert!(after < before / 2.5, "noise {before} → {after}");
    // The edge stays where it was, as steep: one pixel either side.
    let edge = |im: &Rgb| {
        let rows = 20..170;
        let n = rows.len() as f32;
        let l: f32 = rows.clone().map(|y| im.px(126, y)[1]).sum::<f32>() / n;
        let r: f32 = rows.map(|y| im.px(129, y)[1]).sum::<f32>() / n;
        r - l
    };
    assert!(
        edge(&out) > edge(&clean) * 0.8,
        "{} {}",
        edge(&out),
        edge(&clean)
    );
    // The result is kept for the same settings, made again for others.
    assert!(std::sync::Arc::ptr_eq(&out, &src.detailed(&s).unwrap()));
    s.modules.noise_reduction.luminance = 40.0;
    assert!(!std::sync::Arc::ptr_eq(&out, &src.detailed(&s).unwrap()));
}

#[test]
fn sharpening_steepens_edges_and_masking_spares_flat_noise() {
    let (_, img) = noisy(256, 192, 2e-5, 1e-7);
    let src = Demosaiced::new(img.clone(), [1.0; 3], Referred::Scene);
    let mut s = Settings::default();
    s.modules.sharpening.amount = 100.0;
    let out = src.detailed(&s).unwrap();
    // Overshoot either side of the edge.
    let mean = |im: &Rgb, x: usize| (20..170).map(|y| im.px(x, y)[1]).sum::<f32>() / 150.0;
    assert!(mean(&out, 127) < mean(&img, 127) - 0.005);
    assert!(mean(&out, 128) > mean(&img, 128) + 0.005);
    let grain = std_dev(&out, 20..100, 20..170) / std_dev(&img, 20..100, 20..170);
    assert!(
        grain > 1.2,
        "sharpening without masking raises the noise: {grain}"
    );
    s.modules.sharpening.masking = 100.0;
    let masked = src.detailed(&s).unwrap();
    let grain_masked = std_dev(&masked, 20..100, 20..170) / std_dev(&img, 20..100, 20..170);
    assert!(
        grain_masked < 1.05,
        "masked: flat areas alone: {grain_masked}"
    );
    assert!(
        mean(&masked, 128) > mean(&img, 128) + 0.004,
        "the edge still sharpened"
    );
}

#[test]
fn a_proxy_moves_the_bands() {
    let mut s = Settings::default();
    s.modules.sharpening.amount = 100.0;
    let n = Noise {
        a: [1e-4; 3],
        b: [1e-6; 3],
    };
    let full = Detail::new(&s, n, 1.0).unwrap();
    let proxy = Detail::new(&s, n, 0.25).unwrap();
    // A 1 px radius at a quarter size is sub-pixel: weaker.
    assert!(
        proxy.gain[0] < full.gain[0] * 0.6,
        "{:?} {:?}",
        proxy.gain,
        full.gain
    );
    s.modules.clarity.texture = 100.0;
    let t = Detail::new(&s, n, 1.0).unwrap();
    assert!(
        t.gain[2] > full.gain[2] + 0.3,
        "texture lifts the middle bands: {:?}",
        t.gain
    );
    // And processing a flat image changes nothing.
    let flat = Rgb {
        width: 32,
        height: 32,
        data: vec![0.3; 32 * 32 * 3],
    };
    let out = process(&flat, &t);
    assert!(out.data.iter().all(|v| (v - 0.3).abs() < 1e-4));
}
