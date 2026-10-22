//! Spot removal: heal blends into its surroundings, clone copies, outside
//! the circle nothing changes, and the automatic source finds a place that
//! looks like the spot's surroundings.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::{Demosaiced, Referred, Rgb};
use shoebox_develop::edit::{Settings, Spot, SpotKind};
use shoebox_develop::heal::{find_source, heal};

/// A brightness gradient left to right (0.1 to 0.6), with a dark blot at
/// (60, 40).
fn blotted() -> Rgb {
    let (w, h) = (200, 100);
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = 0.1 + 0.5 * x as f32 / w as f32;
            let blot = ((x as f32 - 60.0).hypot(y as f32 - 40.0)) < 5.0;
            let v = if blot { 0.01 } else { v };
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&[v, v * 0.9, v * 0.8]);
        }
    }
    img
}

/// Units of the 200 px long edge.
fn u(px: f64) -> f64 {
    (px + 0.5) / 200.0
}

fn spot(kind: SpotKind) -> Spot {
    Spot {
        kind,
        dest: [u(60.0), u(40.0)],
        // From 50 px to the right: 0.125 brighter there.
        source: [u(110.0), u(40.0)],
        radius: 8.0 / 200.0,
        feather: 0.0,
        opacity: 100.0,
    }
}

#[test]
fn heal_matches_the_surroundings_and_clone_copies() {
    let img = blotted();
    let want = |x: usize| 0.1 + 0.5 * x as f32 / 200.0;
    let healed = heal(&img, &[spot(SpotKind::Heal)]);
    for x in 54..=66 {
        let v = healed.px(x, 40)[0];
        assert!(
            (v - want(x)).abs() < 0.01,
            "healed at {x}: {v} vs {}",
            want(x)
        );
    }
    let cloned = heal(&img, &[spot(SpotKind::Clone)]);
    // The source's own values, a gradient 50 px further right.
    let v = cloned.px(60, 40)[0];
    assert!((v - want(110)).abs() < 1e-4, "cloned: {v}");
    // Outside the circle, nothing changes.
    for p in [(40, 40), (60, 10), (80, 60)] {
        assert_eq!(healed.px(p.0, p.1), img.px(p.0, p.1));
        assert_eq!(cloned.px(p.0, p.1), img.px(p.0, p.1));
    }
    // At no opacity, nothing at all.
    let none = heal(
        &img,
        &[Spot {
            opacity: 0.0,
            ..spot(SpotKind::Heal)
        }],
    );
    assert_eq!(none.data, img.data);
}

#[test]
fn the_source_found_looks_like_the_surroundings() {
    // Stripes on the left half, flat on the right; a spot in the stripes
    // copies from stripes, and in the same phase.
    let (w, h) = (240, 160);
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = if x < 120 {
                if (x / 6) % 2 == 0 { 0.6 } else { 0.1 }
            } else {
                0.35
            };
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&[v; 3]);
        }
    }
    let long = 240.0;
    let dest = [(60.5) / long, (80.5) / long];
    let s = find_source(&img, dest, 8.0 / long);
    let (sx, sy) = (s[0] * long - 0.5, s[1] * long - 0.5);
    assert!(sx < 120.0 - 12.0, "in the stripes: {sx}");
    assert!(
        (sy - 80.0).abs() > 10.0 || (sx - 60.0).abs() > 10.0,
        "elsewhere"
    );
    let phase = |x: f64| ((x.round() as i64).rem_euclid(12)) / 6;
    assert_eq!(phase(sx), phase(60.0), "the stripes line up: {sx}");
}

#[test]
fn the_prepared_source_is_kept_until_the_spots_change() {
    let src = Demosaiced::new(blotted(), [1.0; 3], Referred::Display);
    let mut s = Settings::default();
    assert!(src.prepared(&s).is_none());
    s.modules.heal.spots = vec![spot(SpotKind::Heal)];
    let a = src.prepared(&s).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &src.prepared(&s).unwrap()));
    s.modules.heal.spots[0].kind = SpotKind::Clone;
    assert!(!std::sync::Arc::ptr_eq(&a, &src.prepared(&s).unwrap()));
}
