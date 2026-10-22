//! Effects: the vignette darkens (or lightens) the edges of the whole
//! frame, a 100% region gets its own part of it, and grain sits in the
//! midtones, averages out, and evens out on a proxy.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::effects::{Effects, apply};
use shoebox_develop::edit::{self, Settings};

fn fx(f: impl FnOnce(&mut edit::Effects), scale: f32) -> Effects {
    let mut e = edit::Effects::default();
    f(&mut e);
    Effects::new(&e, [300.0, 200.0], scale)
}

#[test]
fn vignette_and_its_regions() {
    let e = fx(|e| e.vignette.amount = -80.0, 1.0);
    let grey = [0.5; 3];
    assert_eq!(apply(&e, grey, 149.5, 99.5), grey, "the middle untouched");
    let corner = apply(&e, grey, 0.0, 0.0);
    assert!(corner[0] < 0.2, "corners dark: {corner:?}");
    // A region starting at (100, 50) sees the frame's own vignette.
    let mut r = e;
    r.offset = [100.0, 50.0];
    assert_eq!(apply(&r, grey, 10.0, 10.0), apply(&e, grey, 110.0, 60.0));
    // Lighter edges, and highlights kept when darkening.
    let light = fx(|e| e.vignette.amount = 60.0, 1.0);
    assert!(apply(&light, grey, 0.0, 0.0)[0] > 0.7);
    let keep = fx(
        |e| {
            e.vignette.amount = -80.0;
            e.vignette.highlights = 100.0;
        },
        1.0,
    );
    assert!(apply(&keep, [0.98; 3], 0.0, 0.0)[0] > 0.9);
}

#[test]
fn grain_in_the_midtones_averaging_out() {
    let e = fx(|e| e.grain.amount = 100.0, 1.0);
    let mut sum = 0.0;
    let mut spread = 0.0f32;
    for y in 0..200 {
        for x in 0..300 {
            let v = apply(&e, [0.5; 3], x as f32, y as f32)[0] - 0.5;
            sum += v;
            spread = spread.max(v.abs());
        }
    }
    let mean = sum / 60_000.0;
    assert!(mean.abs() < 0.005, "averages out: {mean}");
    assert!(spread > 0.05, "visible: {spread}");
    assert_eq!(
        apply(&e, [0.0; 3], 10.0, 10.0),
        [0.0; 3],
        "black stays black"
    );
    assert_eq!(
        apply(&e, [1.0; 3], 10.0, 10.0),
        [1.0; 3],
        "white stays white"
    );
    // On a quarter-size proxy each pixel spans several grains: weaker.
    let small = fx(|e| e.grain.amount = 100.0, 0.25);
    let peak = |e: &Effects| {
        (0..200)
            .flat_map(|y| (0..300).map(move |x| (x, y)))
            .map(|(x, y)| (apply(e, [0.5; 3], x as f32, y as f32)[0] - 0.5).abs())
            .fold(0.0f32, f32::max)
    };
    assert!(peak(&small) < peak(&e) * 0.6);
    // Defaults do nothing and stay out of the document.
    assert!(!fx(|_| {}, 1.0).active());
    assert_eq!(Settings::default().to_value(), serde_json::json!({}));
}
