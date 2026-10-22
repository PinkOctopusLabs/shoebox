//! Colour calibration: CAT16 from the scene's illuminant after the
//! camera's daylight rendering (`wb`, `Adaptation::Cat16`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_decode::raw::{ColorMatrix, RawImage};
use shoebox_develop::colour::{self, D50, D65};
use shoebox_develop::cpu::{self, Options, Referred, Rgb, Target};
use shoebox_develop::edit::{
    Adaptation, Local, MaskPart, Part, Settings, Shape, WbMode, WhiteBalance,
};
use shoebox_develop::wb;

fn zf() -> RawImage {
    let m = |v: [f32; 9]| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
    RawImage {
        width: 2,
        height: 2,
        cfa: [[0, 1], [1, 2]],
        data: vec![0.0; 4],
        as_shot: Some([1.1425778, 1.0, 1.7753916]),
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

fn neutral(p: [f64; 3]) -> bool {
    (p[0] / p[1] - 1.0).abs() < 1e-3 && (p[2] / p[1] - 1.0).abs() < 1e-3
}

fn cat(wb: WhiteBalance) -> WhiteBalance {
    WhiteBalance {
        adaptation: Adaptation::Cat16,
        ..wb
    }
}

#[test]
fn cat16_takes_white_to_white() {
    let a = colour::cat16(D65, D65);
    for (r, row) in a.iter().enumerate() {
        for (c, v) in row.iter().enumerate() {
            assert!((v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-12);
        }
    }
    let warm = colour::temp_tint_to_xy(2850.0, 0.0);
    let out = colour::mulv(&colour::cat16(warm, D50), colour::xy_to_xyz(warm));
    let want = colour::xy_to_xyz(D50);
    assert!(out.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-9));
    // It isn't Bradford: a saturated colour lands elsewhere.
    let red = [0.4, 0.2, 0.05];
    let (c, b) = (
        colour::mulv(&colour::cat16(warm, D65), red),
        colour::mulv(&colour::bradford(warm, D65), red),
    );
    assert!(
        c.iter().zip(b).any(|(x, y)| (x - y).abs() > 1e-3),
        "{c:?} {b:?}"
    );
}

#[test]
fn as_shot_the_white_comes_out_neutral() {
    let raw = zf();
    let r = wb::resolve(&raw, &cat(WhiteBalance::default()));
    // The as-shot white, balanced at the reference multipliers, through
    // the matrix.
    let n = [1.0 / 1.1425778, 1.0, 1.0 / 1.7753916];
    let b = [0, 1, 2].map(|c| n[c] * f64::from(r.multipliers[c]));
    let p = colour::mulv(&r.cam_to_rec2020, b);
    assert!(neutral(p), "{p:?}");
    // A warm light, as the camera matrices see it: near the same
    // temperature as without calibration.
    let plain = wb::resolve(&raw, &WhiteBalance::default());
    assert!(
        (r.temp - plain.temp).abs() < 300.0,
        "{} vs {}",
        r.temp,
        plain.temp
    );
    // The camera's calibration stays at daylight.
    assert!(
        (r.calibrated_at - 6504.0).abs() < 20.0,
        "{}",
        r.calibrated_at
    );
    // White still comes out bright as before.
    let w = colour::mulv(&r.cam_to_rec2020, [1.0; 3]);
    assert!(w[1] > 0.5 && w[1] < 2.0, "{w:?}");
}

#[test]
fn a_custom_illuminant_goes_grey() {
    let raw = zf();
    for (t, tint) in [(3200.0, 5.0), (4300.0, -10.0), (7500.0, 0.0)] {
        let r = wb::resolve(
            &raw,
            &cat(WhiteBalance {
                mode: WbMode::Custom,
                temp: Some(t),
                tint: Some(tint),
                ..Default::default()
            }),
        );
        assert!((r.temp - t).abs() < 1.0 && (r.tint - tint).abs() < 0.1);
        // Something lit by that light, in the daylight rendering (D65's
        // temperature and tint: no adaptation), is grey.
        let (t65, n65) = colour::xy_to_temp_tint(D65);
        let reference = wb::resolve(
            &raw,
            &cat(WhiteBalance {
                mode: WbMode::Custom,
                temp: Some(t65),
                tint: Some(n65),
                ..Default::default()
            }),
        );
        let lit = colour::mulv(
            &colour::xyz_to_rgb(&colour::REC2020, D65),
            colour::xy_to_xyz(colour::temp_tint_to_xy(t, tint)),
        );
        let back = colour::inv(&reference.cam_to_rec2020).unwrap();
        let p = colour::mulv(&r.cam_to_rec2020, colour::mulv(&back, lit));
        assert!(neutral(p), "{t} K: {p:?}");
    }
}

#[test]
fn the_eyedropper_finds_the_illuminant() {
    let raw = zf();
    let tinted = [0.30f64, 0.25, 0.16];
    for src in [
        cpu::Demosaiced::new(Rgb::new(4, 4), [1.1425778, 1.0, 1.7753916], Referred::Scene),
        cpu::from_display(4, 4, &[128; 48], Target::Srgb),
    ] {
        let cam = wb::Camera::new(&raw.matrices);
        let (temp, tint) = wb::neutral_with(&cam, &src, tinted, Adaptation::Cat16);
        let mut s = Settings::default();
        s.modules.white_balance = cat(WhiteBalance {
            mode: WbMode::Custom,
            temp: Some(temp),
            tint: Some(tint),
            ..Default::default()
        });
        let f = cpu::frame(&raw, &src, &s, &Options::default());
        let p: Vec<f64> = (0..3)
            .map(|r| (0..3).map(|k| f64::from(f.colour[r][k]) * tinted[k]).sum())
            .collect();
        assert!(
            (p[0] / p[1] - 1.0).abs() < 0.01 && (p[2] / p[1] - 1.0).abs() < 0.01,
            "{:?}: {p:?}",
            src.referred
        );
    }
}

#[test]
fn finished_images_adapt_by_cat16_and_old_edits_are_untouched() {
    let (_, _, m) = wb::relative(&cat(WhiteBalance::default()));
    assert_eq!(m, colour::IDENTITY);
    let tungsten = WhiteBalance {
        mode: WbMode::Tungsten,
        ..Default::default()
    };
    let (_, _, bradford) = wb::relative(&tungsten);
    let (_, _, c16) = wb::relative(&cat(tungsten.clone()));
    assert_ne!(bradford, c16);
    let warm = colour::mulv(
        &colour::xyz_to_rgb(&colour::REC2020, D65),
        colour::xy_to_xyz(colour::temp_tint_to_xy(2850.0, 0.0)),
    );
    assert!(neutral(colour::mulv(&c16, warm)));
    // The default (camera) adaptation isn't written, so old edits read
    // and render as before; CAT16 is.
    let mut s = Settings::default();
    assert_eq!(s.to_value(), serde_json::json!({}));
    s.modules.white_balance.adaptation = Adaptation::Cat16;
    assert_eq!(
        s.to_value(),
        serde_json::json!({"modules": {"white_balance": {"adaptation": "cat16"}}})
    );
}

#[test]
fn layers_tint_by_cat16_under_calibration() {
    let raw = zf();
    let src = cpu::Demosaiced::new(Rgb::new(8, 8), [1.1425778, 1.0, 1.7753916], Referred::Scene);
    let mut s = Settings::default();
    let mut l = Local::default();
    l.mask.parts = vec![Part::Known(MaskPart {
        op: Default::default(),
        invert: false,
        shape: Shape::Linear {
            from: [0.0, 0.0],
            to: [1.0, 1.0],
        },
    })];
    l.adjust.temperature = 60.0;
    s.locals = vec![l];
    let matrix = |s: &Settings| {
        let f = cpu::frame(&raw, &src, s, &Options::default());
        f.layers.list.first().map(|l| l.matrix)
    };
    let before = matrix(&s);
    s.modules.white_balance.adaptation = Adaptation::Cat16;
    let after = matrix(&s);
    assert!(before.is_some() && after.is_some());
    assert_ne!(before, after);
}
