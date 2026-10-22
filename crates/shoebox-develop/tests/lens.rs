//! Lens corrections: Lensfun lookups, the camera's own data, the grid, and
//! straight lines after correcting a synthetic barrel.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use shoebox_decode::nef::LensData;
use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::{LensCorrection, Settings};
use shoebox_develop::lens::{self, Shot};

fn zf(lens_name: &str, data: LensData) -> Shot {
    Shot {
        make: "NIKON CORPORATION".into(),
        model: "NIKON Z f".into(),
        lens: lens_name.into(),
        focal: 40.0,
        aperture: 4.0,
        camera_data: data,
    }
}

fn on() -> LensCorrection {
    LensCorrection {
        enabled: true,
        ..LensCorrection::default()
    }
}

#[test]
fn lensfun_knows_the_z_f_and_its_lenses() {
    let db = lens::lensfun().unwrap();
    let shot = zf("NIKKOR Z 40mm f/2", LensData::default());
    let l = lens::find(db, &shot, None).unwrap();
    assert_eq!(l.model, "Nikkor Z 40mm f/2");
    let names = lens::lenses_for(&shot);
    assert!(
        names.len() > 20 && names.iter().any(|n| n.contains("Z 24-70mm")),
        "{}",
        names.len()
    );
    // From Lensfun alone: chromatic aberration and vignetting, a source named.
    let g = lens::grid(&shot, &on(), 6048, 4032).unwrap();
    assert!(g.chromatic && g.source.contains("Lensfun"), "{}", g.source);
    // The corner is brightened (f/4: about 2.4×), the centre not.
    let corner = g.at(0.0, 0.0)[6];
    let centre = g.at((g.cols / 2) as f32, (g.rows / 2) as f32)[6];
    assert!(
        corner > 1.8 && (centre - 1.0).abs() < 0.02,
        "{corner} {centre}"
    );
    assert!(lens::grid(&shot, &LensCorrection::default(), 6048, 4032).is_none());
}

#[test]
fn the_cameras_own_data() {
    let data = LensData {
        distortion: Some([-0.04, 0.0, 0.0]),
        vignette: Some([0.5, 0.0, 0.0]),
    };
    let shot = zf("", data);
    let g = lens::grid(&shot, &on(), 3000, 2000).unwrap();
    assert!(!g.chromatic && g.source == "camera", "{}", g.source);
    // Barrel: a corner of the corrected frame comes from further in, and
    // the zoom out fills the frame again (more of the raw's edge kept).
    let n = g.at(0.0, 0.0);
    assert!(n[2] > 0.0 && n[3] > 0.0, "{n:?}");
    assert!(g.zoom < 1.0, "{}", g.zoom);
    // Vignetting: gain 1 + 0.5·r² where the light fell (r = 1 at the raw's
    // corner; the corrected corner comes from a little further in).
    assert!(n[6] > 1.4 && n[6] < 1.5, "{}", n[6]);
    // Half the amount.
    let half = lens::grid(
        &shot,
        &LensCorrection {
            distortion: 50.0,
            vignetting: 50.0,
            ..on()
        },
        3000,
        2000,
    )
    .unwrap();
    let h = half.at(0.0, 0.0);
    assert!((h[2] - n[2] / 2.0).abs() < 1e-4, "{h:?}");
    assert!(h[6] > 1.4f32.sqrt() && h[6] < 1.5f32.sqrt(), "{}", h[6]);
}

/// A grid of straight lines, bent by a barrel distortion the correction
/// knows, comes out straight.
#[test]
fn barrel_lines_come_out_straight() {
    let (w, h) = (600usize, 400usize);
    let k = [-0.06f64, 0.01, 0.0];
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let hd = cx.hypot(cy);
    // The distorted (raw) image: the corrected pixel p comes from radius
    // r·(1 + k1 r² + …); paint lines at x = 100, 300, 500 in the corrected
    // frame by inverting numerically, or simply sample the ideal pattern.
    let ideal = |x: f64, y: f64| -> f32 {
        let line = |v: f64, s: f64| {
            let m = v.rem_euclid(s);
            !(2.0..=s - 2.0).contains(&m)
        };
        if line(x, 100.0) || line(y, 100.0) {
            0.9
        } else {
            0.05
        }
    };
    // For each raw pixel, find the corrected position mapping onto it
    // (fixed-point iteration), and paint the ideal pattern there.
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (rx, ry) = (x as f64 - cx, y as f64 - cy);
            let (mut ux, mut uy) = (rx, ry);
            for _ in 0..20 {
                let r2 = (ux.hypot(uy) / hd).powi(2);
                let f = 1.0 + k[0] * r2 + k[1] * r2 * r2;
                ux = rx / f;
                uy = ry / f;
            }
            let v = ideal(ux + cx, uy + cy);
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&[v, v, v]);
        }
    }
    let shot = Arc::new(zf(
        "",
        LensData {
            distortion: Some(k),
            vignette: None,
        },
    ));
    let src = Demosaiced::new(img, [1.0; 3], Referred::Scene).with_lens(Some(shot));
    let raw = shoebox_decode::raw::RawImage {
        width: 0,
        height: 0,
        cfa: [[0, 1], [1, 2]],
        data: Vec::new(),
        as_shot: None,
        matrices: Vec::new(),
        make: String::new(),
        model: String::new(),
        linear: None,
    };
    let mut s = Settings::default();
    s.modules.lens.enabled = true;
    let f = cpu::frame(&raw, &src, &s, &Options::default());
    let zoom = f.lens.as_ref().unwrap().zoom;
    let out = cpu::apply(&src.rgb, &f, false);
    // The vertical line at corrected x = 100 (seen at the frame's zoom): its
    // brightest column in each row stays put from top to bottom.
    let want = cx + (100.0 - cx) * f64::from(zoom);
    // (Rows where a horizontal line crosses are skipped.)
    let lo = (want - 15.0) as usize;
    let cols: Vec<usize> = (20..h - 20)
        .step_by(20)
        .filter(|&y| out.px(lo, y)[1] < 0.5)
        .map(|y| {
            (lo..lo + 30)
                .max_by(|&a, &b| out.px(a, y)[1].total_cmp(&out.px(b, y)[1]))
                .unwrap()
        })
        .collect();
    let spread = cols.iter().max().unwrap() - cols.iter().min().unwrap();
    assert!(spread <= 1, "the line wanders: {cols:?}");
    // Uncorrected, it bows.
    s.modules.lens.enabled = false;
    let f = cpu::frame(&raw, &src, &s, &Options::default());
    let bent = cpu::apply(&src.rgb, &f, false);
    let cols: Vec<usize> = (20..h - 20)
        .step_by(20)
        .filter(|&y| bent.px(85, y)[1] < 0.5)
        .map(|y| {
            (85..125)
                .max_by(|&a, &b| bent.px(a, y)[1].total_cmp(&bent.px(b, y)[1]))
                .unwrap()
        })
        .collect();
    let spread = cols.iter().max().unwrap() - cols.iter().min().unwrap();
    assert!(spread >= 3, "the raw bows: {cols:?}");
}

/// The camera's own distortion comes first, even where Lensfun knows the
/// lens (it matches the camera's JPEG; DECISIONS, 2026-10-21).
#[test]
fn the_cameras_distortion_comes_first() {
    let data = LensData {
        distortion: Some([-0.2, 0.0, 0.0]),
        vignette: None,
    };
    let with = lens::grid(&zf("NIKKOR Z 40mm f/2", data), &on(), 6048, 4032).unwrap();
    let without = lens::grid(
        &zf("NIKKOR Z 40mm f/2", LensData::default()),
        &on(),
        6048,
        4032,
    )
    .unwrap();
    let (a, b) = (with.at(0.0, 0.0), without.at(0.0, 0.0));
    assert!((a[2] - b[2]).abs() > 0.01, "{a:?} {b:?}");
    assert!(with.source.contains("camera") && with.chromatic);
}
