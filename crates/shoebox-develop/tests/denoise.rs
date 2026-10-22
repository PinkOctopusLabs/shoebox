//! AI raw denoise, with a stand-in model: the mosaic packed RGGB from
//! wherever red is, the result back in place and balanced like RCD's,
//! matched in brightness, and blended in by Strength.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::AtomicBool;

use shoebox_decode::raw::{ColorMatrix, RawImage};
use shoebox_develop::cpu::{self, Referred};
use shoebox_develop::edit::Settings;

/// A Z f's matrices and as-shot balance, with a flat grey mosaic that has
/// a bright square in it.
fn raw(cfa: [[u8; 2]; 2]) -> RawImage {
    let m = |v: [f32; 9]| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
    let as_shot = [1.1425778, 1.0, 1.7753916];
    let (w, h) = (96, 64);
    let mut data = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let c = usize::from(cfa[y % 2][x % 2]);
            let v = if (40..60).contains(&x) && (20..40).contains(&y) {
                0.5
            } else {
                0.1
            };
            data[y * w + x] = v / as_shot[c];
        }
    }
    RawImage {
        width: w,
        height: h,
        cfa,
        data,
        as_shot: Some(as_shot),
        matrices: vec![
            ColorMatrix {
                temperature: 2856.0,
                xyz_to_cam: m([
                    1.3904, -0.7947, 0.0654, -0.432, 1.2105, 0.2497, -0.0235, 0.083, 0.9243,
                ]),
            },
            ColorMatrix {
                temperature: 6504.0,
                xyz_to_cam: m([
                    1.1607, -0.4491, -0.0977, -0.4522, 1.246, 0.2304, -0.0458, 0.1519, 0.7616,
                ]),
            },
        ],
        make: "Nikon".into(),
        model: "Z f".into(),
        linear: None,
    }
}

/// "Denoises" by repeating each packed RGGB cell over its 2×2 (red, the
/// greens' mean, blue), at three times the brightness.
fn register() {
    shoebox_develop::denoise::register(
        "stand-in",
        Box::new(|p, h, w, progress, _cancel| {
            let n = h * w;
            let mut out = vec![0f32; 3 * 4 * n];
            for y in 0..2 * h {
                for x in 0..2 * w {
                    let i = (y / 2) * w + x / 2;
                    let rgb = [p[i], 0.5 * (p[n + i] + p[2 * n + i]), p[3 * n + i]];
                    for c in 0..3 {
                        out[(c * 2 * h + y) * 2 * w + x] = 3.0 * rgb[c];
                    }
                }
            }
            progress(1.0);
            Ok(out)
        }),
    );
}

#[test]
fn packed_from_red_put_back_and_matched() {
    register();
    let s = Settings::default();
    for cfa in [[[0, 1], [1, 2]], [[1, 0], [2, 1]], [[2, 1], [1, 0]]] {
        let raw = raw(cfa);
        let ai =
            shoebox_develop::denoise::run(&raw, &s.modules.raw, &|_| {}, &AtomicBool::new(false))
                .unwrap();
        let rcd = cpu::demosaic(&raw, &s.modules.raw, None);
        assert_eq!((ai.width, ai.height), (rcd.rgb.width, rcd.rgb.height));
        // Grey and the same brightness as RCD's, inside and outside the
        // square (one pixel in from its edges: the stand-in's blocks are
        // two wide).
        for (x, y) in [(10, 10), (50, 30), (80, 55)] {
            let (a, b) = (ai.px(x, y), rcd.rgb.px(x, y));
            for c in 0..3 {
                assert!(
                    (a[c] - b[c]).abs() < 0.01 * b[c].max(0.05),
                    "{cfa:?} at ({x}, {y}): {a:?} vs {b:?}"
                );
            }
        }
        // The square's edge where it is (not a pixel off).
        assert!(ai.px(41, 30)[1] > 0.4 && ai.px(38, 30)[1] < 0.15, "{cfa:?}");
    }
}

#[test]
fn strength_blends_with_rcd() {
    register();
    let raw = raw([[0, 1], [1, 2]]);
    let mut s = Settings::default();
    let src = cpu::demosaic(&raw, &s.modules.raw, None);
    assert_eq!(src.referred, Referred::Scene);
    // Something the stand-in can't do: everything twice as bright.
    let mut ai = src.rgb.clone();
    for v in &mut ai.data {
        *v *= 2.0;
    }
    src.cache.set_ai(std::sync::Arc::new(ai));
    assert!(src.base(&s).is_none(), "off: RCD's");
    s.modules.denoise_ai.enabled = true;
    assert_eq!(
        src.base(&s).unwrap().px(10, 10)[1],
        2.0 * src.rgb.px(10, 10)[1]
    );
    s.modules.denoise_ai.strength = 50.0;
    let half = src.base(&s).unwrap();
    assert!((half.px(10, 10)[1] - 1.5 * src.rgb.px(10, 10)[1]).abs() < 1e-6);
    // What the pipe reads follows.
    assert!(std::sync::Arc::ptr_eq(&half, &src.prepared(&s).unwrap()));
}
