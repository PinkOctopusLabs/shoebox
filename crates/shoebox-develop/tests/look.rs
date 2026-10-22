//! The look stages (colour, calibration, black and white, tone curve) on
//! the CPU reference: what each slider does, and that defaults do nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::colour;
use shoebox_develop::cpu::look::{
    Look, calibration_matrix, curve_at, curve_table, look_pixel, parametric,
};
use shoebox_develop::edit::{BlackAndWhite, Calibration, Colour, Settings, ToneCurve};

const GREY: [f32; 3] = [0.18, 0.18, 0.18];
const RED: [f32; 3] = [0.3, 0.05, 0.04];
const MUTED: [f32; 3] = [0.2, 0.17, 0.15];
const BLUE: [f32; 3] = [0.03, 0.05, 0.3];

fn look(c: Colour) -> Look {
    Look::new(&c, &Calibration::default(), &BlackAndWhite::default())
}

fn chroma(p: [f32; 3]) -> f32 {
    let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
    (mx - mn) / mx.max(1e-6)
}

fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tol)
}

#[test]
fn defaults_do_nothing_and_stay_out_of_the_document() {
    let l = look(Colour::default());
    assert!(!l.active);
    assert_eq!(look_pixel(&l, RED), RED);
    assert!(curve_table(&ToneCurve::default()).is_none());
    assert_eq!(
        calibration_matrix(&Calibration::default()),
        colour::IDENTITY
    );
    assert_eq!(Settings::default().to_value(), serde_json::json!({}));
    let mut s = Settings::default();
    s.modules.colour.hue[2] = 10.0;
    let v = s.to_value();
    assert_eq!(v["modules"]["colour"]["hue"][2], 10.0);
    assert!(v["modules"]["colour"].get("vibrance").is_none());
}

#[test]
fn saturation_vibrance_and_greys() {
    // −100 saturation: grey, at about the same brightness.
    let l = look(Colour {
        saturation: -100.0,
        ..Colour::default()
    });
    let out = look_pixel(&l, RED);
    assert!(chroma(out) < 1e-3, "{out:?}");
    // Greys never move, whatever the colour sliders.
    let l = look(Colour {
        vibrance: 80.0,
        saturation: 50.0,
        hue: [100.0; 8],
        hsl_saturation: [100.0; 8],
        luminance: [-100.0; 8],
    });
    assert!(close(look_pixel(&l, GREY), GREY, 1e-5));
    // Vibrance lifts muted colours more than vivid ones.
    let l = look(Colour {
        vibrance: 100.0,
        ..Colour::default()
    });
    let gain = |p: [f32; 3]| chroma(look_pixel(&l, p)) / chroma(p);
    assert!(
        gain(MUTED) > gain(BLUE) && gain(MUTED) > 1.2,
        "{} {}",
        gain(MUTED),
        gain(BLUE)
    );
}

#[test]
fn hsl_acts_on_its_own_band() {
    // Blue luminance −100 darkens blue, leaves red nearly alone.
    let mut luminance = [0.0; 8];
    luminance[5] = -100.0;
    let l = look(Colour {
        luminance,
        ..Colour::default()
    });
    let y = |p: [f32; 3]| 0.2627 * p[0] + 0.678 * p[1] + 0.0593 * p[2];
    assert!(y(look_pixel(&l, BLUE)) < y(BLUE) * 0.7);
    assert!((y(look_pixel(&l, RED)) - y(RED)).abs() < y(RED) * 0.02);
    // Red hue +100 turns red towards orange (more green).
    let mut hue = [0.0; 8];
    hue[0] = 100.0;
    let out = look_pixel(
        &look(Colour {
            hue,
            ..Colour::default()
        }),
        RED,
    );
    assert!(out[1] / out[0] > RED[1] / RED[0] * 1.25, "{out:?}");
}

#[test]
fn black_and_white_mixes_by_hue() {
    let bw = |mix: [f64; 8]| {
        Look::new(
            &Colour::default(),
            &Calibration::default(),
            &BlackAndWhite { enabled: true, mix },
        )
    };
    let plain = look_pixel(&bw([0.0; 8]), RED);
    assert!(chroma(plain) < 1e-6, "{plain:?}");
    let mut mix = [0.0; 8];
    mix[0] = 100.0;
    let bright = look_pixel(&bw(mix), RED);
    assert!(bright[0] > plain[0] * 1.5);
    // A grey is the same grey whatever the mix.
    assert!(close(look_pixel(&bw([100.0; 8]), GREY), GREY, 1e-4));
}

#[test]
fn calibration_keeps_white_and_turns_primaries() {
    let cal = Calibration {
        red_hue: 100.0,
        green_saturation: -100.0,
        ..Calibration::default()
    };
    let m = calibration_matrix(&cal);
    let w = colour::mulv(&m, [1.0; 3]);
    assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-9), "{w:?}");
    let red = colour::mulv(&m, [1.0, 0.0, 0.0]);
    assert!(red[1] > 0.01, "red turned towards yellow: {red:?}");
    // Shadows tint +: magenta in the shadows, mid grey and up untouched.
    let l = Look::new(
        &Colour::default(),
        &Calibration {
            shadows_tint: 100.0,
            ..Calibration::default()
        },
        &BlackAndWhite::default(),
    );
    let dark = look_pixel(&l, [0.01; 3]);
    assert!(dark[1] < dark[0] && dark[1] < dark[2], "{dark:?}");
    assert!(close(look_pixel(&l, [0.5; 3]), [0.5; 3], 1e-5));
}

#[test]
fn tone_curves() {
    let shadows = ToneCurve {
        regions: [100.0, 0.0, 0.0, 0.0],
        ..ToneCurve::default()
    };
    assert!(parametric(&shadows, 0.12) > 0.12 + 0.1);
    assert_eq!(parametric(&shadows, 0.0), 0.0);
    assert_eq!(parametric(&shadows, 0.9), 0.9);
    let t = curve_table(&shadows).unwrap();
    for c in 0..3 {
        assert_eq!(curve_at(&t, c, 0.0), 0.0);
        assert!((curve_at(&t, c, 1.0) - 1.0).abs() < 1e-6);
    }
    // A point curve passes through its points and never decreases.
    let s = ToneCurve {
        rgb: vec![[0.25, 0.15], [0.75, 0.9]],
        red: vec![[0.5, 0.6]],
        ..ToneCurve::default()
    };
    let t = curve_table(&s).unwrap();
    assert!((curve_at(&t, 1, 0.25) - 0.15).abs() < 2e-3);
    assert!((curve_at(&t, 1, 0.75) - 0.9).abs() < 2e-3);
    assert!(curve_at(&t, 0, 0.5) > curve_at(&t, 1, 0.5));
    let all: Vec<f32> = (0..=100)
        .map(|i| curve_at(&t, 2, i as f32 / 100.0))
        .collect();
    assert!(all.windows(2).all(|w| w[1] >= w[0]));
    // Even wild points stay monotone.
    let wild = ToneCurve {
        rgb: vec![[0.2, 0.9], [0.4, 0.1], [0.6, 0.95]],
        ..ToneCurve::default()
    };
    let t = curve_table(&wild).unwrap();
    let all: Vec<f32> = (0..=200)
        .map(|i| curve_at(&t, 0, i as f32 / 200.0))
        .collect();
    assert!(all.windows(2).all(|w| w[1] >= w[0]));
}

#[test]
fn point_colour_moves_the_picked_colour_only() {
    use shoebox_develop::cpu::look::{look_point, oklch};
    use shoebox_develop::edit::{ColourPoint, PointColour};
    let plain = look(Colour::default());
    let [l, c, h] = oklch(&plain, RED);
    let pick =
        |p: ColourPoint| look(Colour::default()).with_points(&PointColour { points: vec![p] });
    let point = ColourPoint {
        lightness: f64::from(l),
        chroma: f64::from(c),
        hue: f64::from(h),
        hue_shift: 50.0,
        saturation: -50.0,
        ..Default::default()
    };
    let lk = pick(point.clone());
    assert!(lk.active);
    let (red, w) = look_point(&lk, RED, Some(0));
    assert!((w - 1.0).abs() < 1e-6, "full on the picked colour: {w}");
    let [_, c2, h2] = oklch(&plain, red);
    assert!((h2 - h - 15.0).abs() < 0.5, "hue turned 15°: {h} → {h2}");
    assert!((c2 / c - 0.5).abs() < 0.02, "half the chroma: {c} → {c2}");
    // A blue and a grey are left as they were.
    for other in [BLUE, GREY] {
        let (out, w) = look_point(&lk, other, Some(0));
        assert_eq!(w, 0.0);
        assert!(close(out, other, 1e-5), "{other:?} → {out:?}");
    }
    // A narrow hue range leaves a red 25° away alone; a wide one doesn't.
    let near = {
        let hr = (h + 25.0).to_radians();
        let lab = [l, c * hr.cos(), c * hr.sin()];
        let lms = colour::mulv(
            &shoebox_develop::cpu::look::LAB_TO_LMS.map(|r| r.map(f64::from)),
            lab.map(f64::from),
        )
        .map(|v| (v * v * v) as f32);
        let from = plain.from_lms;
        [0, 1, 2].map(|r| from[r][0] * lms[0] + from[r][1] * lms[1] + from[r][2] * lms[2])
    };
    let narrow = pick(ColourPoint {
        hue_range: 0.0,
        ..point.clone()
    });
    assert_eq!(look_point(&narrow, near, Some(0)).1, 0.0);
    let wide = pick(ColourPoint {
        hue_range: 100.0,
        ..point
    });
    assert!(look_point(&wide, near, Some(0)).1 > 0.99);
}

#[test]
fn point_colour_view_greys_what_it_misses() {
    use shoebox_develop::cpu::greyed;
    let d = [0.8, 0.4, 0.2];
    assert!(close(greyed(d, 1.0), d, 1e-6));
    let g = greyed(d, 0.0);
    assert!((g[0] - g[1]).abs() < 1e-6 && (g[1] - g[2]).abs() < 1e-6);
}
