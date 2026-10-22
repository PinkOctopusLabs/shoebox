//! Dehaze, highlights and shadows, clarity (the fast guided filter): what
//! each does, that edges don't halo, that the maps are reused, and that the
//! proxy and full size agree.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_decode::raw::{ColorMatrix, RawImage};
use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::Settings;

fn zf() -> RawImage {
    let m = |v: [f32; 9]| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
    RawImage {
        width: 2,
        height: 2,
        cfa: [[0, 1], [1, 2]],
        data: vec![0.0; 4],
        as_shot: Some([1.0, 1.0, 1.0]),
        matrices: vec![ColorMatrix {
            temperature: 6504.0,
            xyz_to_cam: m([
                1.1607, -0.4491, -0.0977, -0.4522, 1.246, 0.2304, -0.0458, 0.1519, 0.7616,
            ]),
        }],
        make: "Nikon".into(),
        model: "Z f".into(),
        linear: None,
    }
}

/// Left half dark (0.01), right half bright (0.8), both with a fine
/// ±20% checker texture.
fn halves(w: usize, h: usize) -> Rgb {
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let base = if x < w / 2 { 0.01 } else { 0.8 };
            let tex = if (x / 2 + y / 2) % 2 == 0 { 1.2 } else { 0.8 };
            let v = base * tex;
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&[v, v, v]);
        }
    }
    img
}

fn src(img: Rgb) -> Demosaiced {
    Demosaiced::new(img, [1.0; 3], Referred::Scene)
}

/// Scene-linear render (before the display transform).
fn render(d: &Demosaiced, s: &Settings) -> Rgb {
    let f = cpu::frame(&zf(), d, s, &Options::default());
    cpu::apply(&d.rgb, &f, false)
}

fn green_mean(img: &Rgb, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f32 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in ys {
        for x in xs.clone() {
            sum += img.px(x, y)[1];
            n += 1.0;
        }
    }
    sum / n
}

/// Texture contrast: the ratio of the bright to the dark checker squares.
fn texture(img: &Rgb, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>) -> f32 {
    let (mut hi, mut lo, mut nh, mut nl) = (0.0, 0.0, 0.0, 0.0);
    for y in ys {
        for x in xs.clone() {
            let v = img.px(x, y)[1];
            if (x / 2 + y / 2) % 2 == 0 {
                hi += v;
                nh += 1.0;
            } else {
                lo += v;
                nl += 1.0;
            }
        }
    }
    (hi / nh) / (lo / nl)
}

#[test]
fn shadows_lift_the_darks_and_keep_their_texture() {
    let d = src(halves(400, 200));
    let plain = render(&d, &Settings::default());
    let mut s = Settings::default();
    s.modules.highlights_shadows.shadows = 100.0;
    let lifted = render(&d, &s);
    let (dark, bright) = ((40..160), (240..360));
    let ys = 50..150;
    let gain_dark = green_mean(&lifted, dark.clone(), ys.clone())
        / green_mean(&plain, dark.clone(), ys.clone());
    let gain_bright = green_mean(&lifted, bright.clone(), ys.clone())
        / green_mean(&plain, bright.clone(), ys.clone());
    assert!(gain_dark > 2.0, "darks lifted: {gain_dark}");
    assert!(
        (gain_bright - 1.0).abs() < 0.05,
        "brights alone: {gain_bright}"
    );
    // Detail keeps its contrast (a global curve would flatten it less
    // evenly; the base layer leaves it exactly).
    let t0 = texture(&plain, dark.clone(), ys.clone());
    let t1 = texture(&lifted, dark, ys);
    assert!((t1 / t0 - 1.0).abs() < 0.05, "texture {t0} → {t1}");
}

#[test]
fn no_halo_at_an_edge() {
    let d = src(halves(400, 200));
    let mut s = Settings::default();
    s.modules.highlights_shadows.shadows = 100.0;
    let lifted = render(&d, &s);
    let plain = render(&d, &Settings::default());
    // Gain right beside the bright half against far from it.
    let near = green_mean(&lifted, 192..198, 50..150) / green_mean(&plain, 192..198, 50..150);
    let far = green_mean(&lifted, 40..80, 50..150) / green_mean(&plain, 40..80, 50..150);
    assert!(near > far * 0.85, "near the edge {near}, far {far}");
}

#[test]
fn highlights_darken_the_brights() {
    let d = src(halves(400, 200));
    let plain = render(&d, &Settings::default());
    let mut s = Settings::default();
    s.modules.highlights_shadows.highlights = -100.0;
    let out = render(&d, &s);
    let g = |r: std::ops::Range<usize>| {
        green_mean(&out, r.clone(), 50..150) / green_mean(&plain, r, 50..150)
    };
    assert!(g(240..360) < 0.6, "{}", g(240..360));
    assert!((g(40..160) - 1.0).abs() < 0.05, "{}", g(40..160));
}

#[test]
fn clarity_changes_midtone_contrast() {
    // Mid grey with a coarse texture.
    let (w, h) = (400, 300);
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = if (x / 25 + y / 25) % 2 == 0 {
                0.25
            } else {
                0.12
            };
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&[v, v, v]);
        }
    }
    let d = src(img);
    let contrast = |s: &Settings| {
        let out = render(&d, s);
        let (mut hi, mut lo) = (0.0f32, f32::MAX);
        for y in 100..200 {
            for x in 100..300 {
                hi = hi.max(out.px(x, y)[1]);
                lo = lo.min(out.px(x, y)[1]);
            }
        }
        hi / lo
    };
    let base = contrast(&Settings::default());
    let mut s = Settings::default();
    s.modules.clarity.amount = 100.0;
    let more = contrast(&s);
    s.modules.clarity.amount = -100.0;
    let less = contrast(&s);
    assert!(
        more > base * 1.2 && less < base * 0.85,
        "{less} {base} {more}"
    );
}

#[test]
fn dehaze_restores_a_hazed_image() {
    // A colourful scene seen through uniform haze (t = 0.5, white airlight).
    let (w, h) = (300, 200);
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let j = [
                0.05 + 0.3 * (x as f32 / w as f32),
                0.05 + 0.2 * (y as f32 / h as f32),
                0.1,
            ];
            let i = (y * w + x) * 3;
            for (o, v) in img.data[i..i + 3].iter_mut().zip(j) {
                *o = v * 0.5 + 0.9 * 0.5;
            }
        }
    }
    // One bright, haze-coloured sky row for the airlight.
    for x in 0..w {
        let i = x * 3;
        img.data[i..i + 3].copy_from_slice(&[0.9; 3]);
    }
    let d = src(img);
    let spread = |s: &Settings| {
        let out = render(&d, s);
        let a = out.px(30, 150)[0];
        let b = out.px(270, 150)[0];
        b - a
    };
    let hazy = spread(&Settings::default());
    let mut s = Settings::default();
    s.modules.dehaze.amount = 100.0;
    let clear = spread(&s);
    assert!(clear > hazy * 1.5, "{hazy} → {clear}");
    s.modules.dehaze.amount = -100.0;
    assert!(spread(&s) < hazy, "adding haze flattens it");
}

#[test]
fn maps_are_reused_and_the_proxy_agrees() {
    let d = src(halves(800, 400));
    let mut s = Settings::default();
    s.modules.highlights_shadows.shadows = 60.0;
    let a = cpu::frame(&zf(), &d, &s, &Options::default());
    s.modules.highlights_shadows.shadows = 80.0;
    s.modules.clarity.amount = 0.0;
    let b = cpu::frame(&zf(), &d, &s, &Options::default());
    assert!(std::sync::Arc::ptr_eq(
        a.local.maps.as_ref().unwrap(),
        b.local.maps.as_ref().unwrap()
    ));
    s.modules.exposure.ev = 1.0;
    let c = cpu::frame(&zf(), &d, &s, &Options::default());
    assert!(!std::sync::Arc::ptr_eq(
        a.local.maps.as_ref().unwrap(),
        c.local.maps.as_ref().unwrap()
    ));
    // Half size gives the same lift.
    let half = src(cpu::downscale(&d.rgb, 400));
    let full = render(&d, &s);
    let small = render(&half, &s);
    let g = green_mean(&full, 100..300, 100..300);
    let gs = green_mean(&small, 50..150, 50..150);
    assert!((g / gs - 1.0).abs() < 0.05, "{g} {gs}");
}

#[test]
fn smart_lighting_lifts_shadows_and_tames_highlights() {
    let d = src(halves(256, 192));
    let plain = render(&d, &Settings::default());
    let mut s = Settings::default();
    s.modules.smart_lighting.strength = 100.0;
    let lit = render(&d, &s);
    let (dark, bright) = ((10..100, 20..170), (160..250, 20..170));
    let ratio = |a: &Rgb, b: &Rgb, (xs, ys): &(std::ops::Range<usize>, std::ops::Range<usize>)| {
        green_mean(a, xs.clone(), ys.clone()) / green_mean(b, xs.clone(), ys.clone())
    };
    let (up, down) = (ratio(&lit, &plain, &dark), ratio(&lit, &plain, &bright));
    assert!(up > 1.3, "shadows lifted: ×{up}");
    assert!(down < 0.85, "highlights tamed: ×{down}");
    // Bias towards the shadows lifts them more and tames less.
    s.modules.smart_lighting.bias = 80.0;
    let biased = render(&d, &s);
    assert!(ratio(&biased, &plain, &dark) > up);
    assert!(ratio(&biased, &plain, &bright) > down);
    // The slider only scales: the maps are made once.
    s.modules.smart_lighting.strength = 40.0;
    let a = cpu::frame(&zf(), &d, &s, &Options::default());
    s.modules.smart_lighting.strength = 60.0;
    let b = cpu::frame(&zf(), &d, &s, &Options::default());
    assert!(std::sync::Arc::ptr_eq(
        a.local.maps.as_ref().unwrap(),
        b.local.maps.as_ref().unwrap()
    ));
}

#[test]
fn smart_lighting_spot_weighted_lifts_the_spot_more() {
    let (w, h) = (256, 192);
    let d = src(halves(w, h));
    let plain = render(&d, &Settings::default());
    let mut s = Settings::default();
    s.modules.smart_lighting.strength = 60.0;
    let uniform = render(&d, &s);
    // A box in the dark half (stored image, long edges).
    let long = w as f64;
    s.modules.smart_lighting.spot = Some([20.0 / long, 30.0 / long, 100.0 / long, 160.0 / long]);
    let spot = render(&d, &s);
    let (dark, bright) = ((10..100, 20..170), (160..250, 20..170));
    let ratio = |a: &Rgb, (xs, ys): &(std::ops::Range<usize>, std::ops::Range<usize>)| {
        green_mean(a, xs.clone(), ys.clone()) / green_mean(&plain, xs.clone(), ys.clone())
    };
    let (u, sp) = (ratio(&uniform, &dark), ratio(&spot, &dark));
    assert!(sp > u * 1.3, "the spot's half lifted more: ×{u} → ×{sp}");
    // The bright half is held where uniform mode put it.
    let (u, sp) = (ratio(&uniform, &bright), ratio(&spot, &bright));
    assert!((sp / u - 1.0).abs() < 0.03, "bright half: ×{u} → ×{sp}");
    // The spot is part of the maps' key; strength still only scales.
    let a = cpu::frame(&zf(), &d, &s, &Options::default());
    s.modules.smart_lighting.strength = 80.0;
    let b = cpu::frame(&zf(), &d, &s, &Options::default());
    assert!(std::sync::Arc::ptr_eq(
        a.local.maps.as_ref().unwrap(),
        b.local.maps.as_ref().unwrap()
    ));
    s.modules.smart_lighting.spot = Some([0.5, 0.1, 0.9, 0.6]);
    let c = cpu::frame(&zf(), &d, &s, &Options::default());
    assert_ne!(a.local.maps, c.local.maps);
}
