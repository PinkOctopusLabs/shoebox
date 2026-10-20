//! Camera match profiles: the table, the fitter recovering a known look, and
//! renders picking the look up by camera.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::profile::{Fitter, Lut3, Profile, SIZE, find, register};

/// A known look on encoded values: more saturation and a slight lift.
fn look(p: [f32; 3]) -> [f32; 3] {
    let m = (p[0] + p[1] + p[2]) / 3.0;
    p.map(|v| (m + (v - m) * 1.3 + 0.03 * (1.0 - v)).clamp(0.0, 1.0))
}

#[test]
fn identity_changes_nothing() {
    let l = Lut3::identity(SIZE);
    for p in [[0.1, 0.5, 0.9], [0.0, 0.0, 0.0], [1.0, 0.2, 0.33]] {
        let q = l.sample(p);
        assert!(
            p.iter().zip(q).all(|(a, b)| (a - b).abs() < 1e-5),
            "{p:?} {q:?}"
        );
    }
}

#[test]
fn the_fitter_recovers_a_look() {
    let mut f = Fitter::new(SIZE);
    // Deterministic samples over the cube.
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x >> 40) as f32 / (1u64 << 24) as f32
    };
    for _ in 0..200_000 {
        let p = [next(), next(), next()];
        f.add(p, look(p));
    }
    let lut = f.finish(1.0);
    let mut err = 0.0;
    let n = 2000;
    for _ in 0..n {
        let p = [next(), next(), next()];
        let (got, want) = (lut.sample(p), look(p));
        err += got
            .iter()
            .zip(want)
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / 3.0;
    }
    let mean = err / n as f32;
    assert!(mean < 0.01, "mean error {mean}");
}

#[test]
fn renders_find_their_camera_look() {
    let mut l = Lut3::identity(SIZE);
    for c in &mut l.data {
        *c = look(*c);
    }
    register(&Profile {
        camera: "Test Camera X1".into(),
        look: l,
        pairs: 0,
        delta_e: None,
    });
    assert!(find("test camera x1").is_some());
    assert!(find("Other").is_none());
}
