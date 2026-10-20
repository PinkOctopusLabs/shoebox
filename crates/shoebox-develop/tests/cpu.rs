//! The CPU reference pipe: demosaic accuracy against known images,
//! highlight reconstruction, geometry, the display transform, and whole
//! renders of synthetic DNGs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::raw::{Mosaic, bilinear, clip, inpaint_opposed, rcd};
use shoebox_develop::cpu::{self, Curve, Options, Rgb, Target, max_crop, orient, rotate_crop};
use shoebox_develop::edit::{DisplayTransform, Geometry, Highlights, Settings};
use shoebox_fixtures::Dng;

const RGGB: [[u8; 2]; 2] = [[0, 1], [1, 2]];

/// A synthetic scene as linear RGB (from the fixtures' test scene).
fn truth(w: u32, h: u32) -> Rgb {
    let img = shoebox_fixtures::scene(7, w, h);
    let data = img
        .as_raw()
        .iter()
        .map(|&v| (f32::from(v) / 255.0).powf(2.2))
        .collect();
    Rgb {
        width: w as usize,
        height: h as usize,
        data,
    }
}

fn mosaic_of(img: &Rgb, cfa: [[u8; 2]; 2]) -> Vec<f32> {
    let mut out = vec![0.0; img.width * img.height];
    for y in 0..img.height {
        for x in 0..img.width {
            out[y * img.width + x] = img.px(x, y)[usize::from(cfa[y % 2][x % 2])];
        }
    }
    out
}

/// PSNR in dB over the interior (the edges are left to bilinear by both).
fn psnr(a: &Rgb, b: &Rgb) -> f64 {
    let m = 12;
    let mut se = 0.0f64;
    let mut n = 0.0f64;
    for y in m..a.height - m {
        for x in m..a.width - m {
            let (p, q) = (a.px(x, y), b.px(x, y));
            for c in 0..3 {
                se += f64::from(p[c] - q[c]).powi(2);
                n += 1.0;
            }
        }
    }
    10.0 * (1.0 / (se / n)).log10()
}

#[test]
fn rcd_beats_bilinear_on_every_bayer_layout() {
    let t = truth(256, 192);
    for cfa in [RGGB, [[1, 0], [2, 1]], [[1, 2], [0, 1]], [[2, 1], [1, 0]]] {
        let data = mosaic_of(&t, cfa);
        let m = Mosaic {
            width: t.width,
            height: t.height,
            cfa,
            data: &data,
        };
        let (r, b) = (psnr(&rcd(&m), &t), psnr(&bilinear(&m), &t));
        assert!(r > b + 1.0, "{cfa:?}: RCD {r:.1} dB vs bilinear {b:.1} dB");
        assert!(r > 30.0, "{cfa:?}: RCD {r:.1} dB");
    }
}

#[test]
fn a_flat_colour_stays_flat() {
    let (w, h) = (64, 48);
    let colour = [0.2f32, 0.5, 0.35];
    let data: Vec<f32> = (0..w * h)
        .map(|i| colour[usize::from(RGGB[(i / w) % 2][(i % w) % 2])])
        .collect();
    let m = Mosaic {
        width: w,
        height: h,
        cfa: RGGB,
        data: &data,
    };
    for img in [rcd(&m), bilinear(&m)] {
        for p in img.data.chunks(3) {
            for c in 0..3 {
                assert!((p[c] - colour[c]).abs() < 1e-4, "{p:?}");
            }
        }
    }
}

#[test]
fn clipped_red_is_rebuilt_from_its_neighbours() {
    // A warm gradient whose red clips in the middle; green and blue don't.
    let (w, h) = (90, 90);
    let level = |x: usize, y: usize| {
        let d = ((x as f32 - 45.0).powi(2) + (y as f32 - 45.0).powi(2)).sqrt();
        (1.6 - d / 40.0).max(0.1)
    };
    let ratio = [1.0f32, 0.6, 0.4];
    let clips = [0.987f32; 3];
    let data: Vec<f32> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let c = usize::from(RGGB[y % 2][x % 2]);
            (level(x, y) * ratio[c]).min(1.0)
        })
        .collect();
    let m = Mosaic {
        width: w,
        height: h,
        cfa: RGGB,
        data: &data,
    };
    let out = inpaint_opposed(&m, clips);
    // Unclipped photosites are untouched; clipped red is raised above clip.
    let mut raised = 0;
    for i in 0..w * h {
        if data[i] < clips[0] {
            assert_eq!(out[i], data[i]);
        } else {
            assert!(out[i] >= data[i]);
            if out[i] > data[i] + 0.05 {
                raised += 1;
            }
        }
    }
    assert!(raised > 50, "{raised}");
    // Clip mode flattens to white instead.
    let flat = clip(&m, clips);
    assert!(flat.iter().all(|&v| v <= 0.987));
}

#[test]
fn orientation_puts_the_corner_where_exif_says() {
    // A 3×2 image with a marked top-left pixel.
    let mut img = Rgb::new(3, 2);
    img.data[0] = 1.0;
    let marked = |o: &Rgb| -> (usize, usize) {
        let i = o.data.chunks(3).position(|p| p[0] == 1.0).unwrap();
        (i % o.width, i / o.width)
    };
    // Where the stored top-left ends up, upright.
    let want = [
        (1, (0, 0)),
        (2, (2, 0)),
        (3, (2, 1)),
        (4, (0, 1)),
        (5, (0, 0)),
        (6, (1, 0)),
        (7, (1, 2)),
        (8, (0, 2)),
    ];
    for (o, at) in want {
        let r = orient(&img, o);
        assert_eq!(marked(&r), at, "orientation {o}");
        let dims = if o >= 5 { (2, 3) } else { (3, 2) };
        assert_eq!((r.width, r.height), dims);
    }
}

#[test]
fn rotation_and_crop() {
    let t = truth(120, 80);
    // No rotation, full crop: unchanged.
    assert_eq!(rotate_crop(&t, &Geometry::default()), t);
    // A crop keeps exactly that part.
    let g = Geometry {
        crop: [0.25, 0.5, 0.75, 1.0],
        ..Default::default()
    };
    let c = rotate_crop(&t, &g);
    assert_eq!((c.width, c.height), (60, 40));
    assert_eq!(c.px(0, 0), t.px(30, 40));
    // The largest crop for a rotation never reaches outside the image
    // (an image with no black of its own: black would be outside).
    let lifted = Rgb {
        data: t.data.iter().map(|v| v + 0.1).collect(),
        ..t.clone()
    };
    assert!(t.data.contains(&0.0), "the scene has black of its own");
    let t = lifted;
    let crop = max_crop(120.0, 80.0, 7.0);
    let g = Geometry {
        rotate: 7.0,
        crop: crop.map(|v| v),
        ..Default::default()
    };
    let r = rotate_crop(&t, &g);
    let black = r
        .data
        .chunks(3)
        .filter(|p| p.iter().all(|&v| v == 0.0))
        .count();
    assert_eq!(black, 0);
    assert!(crop[0] > 0.0 && crop[2] < 1.0);
}

#[test]
fn the_display_curve() {
    let c = Curve::new(&DisplayTransform::default());
    // Mid grey to display mid grey; black to black; far highlights to white.
    let grey = c.apply(0.18).powf(2.2);
    assert!((grey - 0.18).abs() < 1e-3, "{grey}");
    assert!(c.apply(0.0) < 1e-3);
    assert!(c.apply(100.0) > 0.999);
    let mut last = 0.0;
    for i in 0..400 {
        let v = c.apply(2f32.powf(i as f32 / 20.0 - 14.0));
        assert!(v >= last, "not monotone at {i}");
        last = v;
    }
    // More contrast: darker shadows, brighter highlights, same grey.
    let hard = Curve::new(&DisplayTransform {
        contrast: 50.0,
        ..Default::default()
    });
    assert!(hard.apply(0.02) < c.apply(0.02) && hard.apply(1.0) > c.apply(1.0));
    assert!((hard.apply(0.18).powf(2.2) - 0.18).abs() < 1e-3);
}

/// A DNG whose every photosite reads `level` of white, scaled by its
/// colour's as-shot neutral: a neutral grey card under the shot's light.
fn grey_card(level: f32) -> shoebox_decode::raw::RawImage {
    let dng = Dng::new(64, 48);
    let n = dng.neutral;
    let range = f32::from(dng.white - dng.black);
    let bytes = dng.write(|x, y| {
        let c = usize::from(RGGB[(y % 2) as usize][(x % 2) as usize]);
        dng.black + (n[c] * level * range) as u16
    });
    shoebox_decode::raw::decode(&bytes).unwrap()
}

#[test]
fn a_grey_card_renders_neutral_and_exposure_moves_it() {
    let raw = grey_card(0.2);
    let s = Settings::default();
    let o = Options::default();
    let img = cpu::render(&raw, &s, &o);
    let p = img.px(32, 24);
    assert!(
        (p[0] - p[1]).abs() < 1.0 / 255.0 && (p[2] - p[1]).abs() < 1.0 / 255.0,
        "{p:?}"
    );
    let mut brighter = s.clone();
    brighter.modules.exposure.ev = 1.0;
    let q = cpu::render(&raw, &brighter, &o).px(32, 24);
    assert!(q[1] > p[1] + 0.05, "{p:?} → {q:?}");
    // Display P3 renders a neutral as the same neutral.
    let p3 = cpu::render(
        &raw,
        &s,
        &Options {
            target: Target::DisplayP3,
            ..o
        },
    )
    .px(32, 24);
    assert!((p3[1] - p[1]).abs() < 1.0 / 255.0);
}

#[test]
fn the_proxy_is_a_downscale_of_the_same_render() {
    let raw = grey_card(0.3);
    let full = cpu::render(&raw, &Settings::default(), &Options::default());
    let proxy = cpu::render(
        &raw,
        &Settings::default(),
        &Options {
            long_edge: Some(32),
            ..Default::default()
        },
    );
    assert_eq!((proxy.width, proxy.height), (32, 24));
    let (a, b) = (full.px(32, 24), proxy.px(16, 12));
    assert!((a[1] - b[1]).abs() < 2.0 / 255.0, "{a:?} {b:?}");
    let mut s = Settings::default();
    s.modules.raw.highlights = Highlights::Clip;
    assert_eq!(cpu::render(&raw, &s, &Options::default()).width, 64);
}

/// A finished image at default settings comes out exactly as it went in.
#[test]
fn a_finished_image_is_unchanged_by_default() {
    let (w, h) = (64, 48);
    let px: Vec<u8> = (0..w * h * 3).map(|i| ((i * 53) % 256) as u8).collect();
    for space in [Target::Srgb, Target::DisplayP3] {
        let src = cpu::from_display(w, h, &px, space);
        let raw = grey_card(0.1); // metadata unused for finished images
        let o = Options {
            target: space,
            ..Default::default()
        };
        let f = cpu::frame(&raw, &src, &Settings::default(), &o);
        let out = cpu::to_u8(&cpu::apply(&src.rgb, &f, true));
        let worst = out
            .iter()
            .zip(&px)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 1, "{space:?}: off by {worst}");
        // Exposure brightens; contrast and blacks move away from identity.
        let mut s = Settings::default();
        s.modules.exposure.ev = 1.0;
        let brighter = cpu::to_u8(&cpu::apply(&src.rgb, &cpu::frame(&raw, &src, &s, &o), true));
        assert!(brighter.iter().zip(&px).filter(|(a, b)| a > b).count() > px.len() / 2);
    }
}

#[test]
fn relative_white_balance() {
    use shoebox_develop::edit::{WbMode, WhiteBalance};
    let (t, n, m) = shoebox_develop::wb::relative(&WhiteBalance::default());
    assert!((t - 6504.0).abs() < 40.0 && n.abs() < 15.0, "{t} {n}");
    assert_eq!(m, shoebox_develop::colour::IDENTITY);
    // Saying the light was tungsten makes a neutral bluer.
    let (_, _, m) = shoebox_develop::wb::relative(&WhiteBalance {
        mode: WbMode::Tungsten,
        ..Default::default()
    });
    let grey = shoebox_develop::colour::mulv(&m, [0.5, 0.5, 0.5]);
    assert!(grey[2] > grey[0], "{grey:?}");
}

#[test]
fn a_grey_card_is_neutral_in_every_export_space() {
    let raw = grey_card(0.2);
    for target in [
        Target::Srgb,
        Target::DisplayP3,
        Target::AdobeRgb,
        Target::ProPhoto,
    ] {
        let p = cpu::render(
            &raw,
            &Settings::default(),
            &Options {
                target,
                ..Default::default()
            },
        )
        .px(32, 24);
        assert!(
            (p[0] - p[1]).abs() < 1.5 / 255.0 && (p[2] - p[1]).abs() < 1.5 / 255.0,
            "{target:?}: {p:?}"
        );
    }
}

#[test]
fn quarter_turns_and_mirror_compose_with_the_files_orientation() {
    use shoebox_develop::cpu::compose_orientation;
    // A quarter turn of an upright file is EXIF 6; two are 3; a mirror 2.
    assert_eq!(compose_orientation(1, 1, false), 6);
    assert_eq!(compose_orientation(1, 2, false), 3);
    assert_eq!(compose_orientation(1, 3, false), 8);
    assert_eq!(compose_orientation(1, 0, true), 2);
    // A portrait file (6) turned back anticlockwise is upright.
    assert_eq!(compose_orientation(6, 3, false), 1);
    for o in 1..=8 {
        assert_eq!(compose_orientation(o, 4, false), o);
        assert_eq!(
            compose_orientation(compose_orientation(o, 0, true), 0, true),
            o
        );
    }
}

#[test]
fn a_turned_crop_shows_the_same_pixels_turned() {
    let t = truth(60, 40);
    let mut g = Geometry {
        crop: [0.1, 0.2, 0.6, 0.9],
        ..Default::default()
    };
    let before = rotate_crop(&t, &g);
    for clockwise in [true, false] {
        let mut turned = g.clone();
        turned.turn(clockwise);
        let after = rotate_crop(&t, &turned);
        let want = orient(&before, if clockwise { 6 } else { 8 });
        assert_eq!((after.width, after.height), (want.width, want.height));
        assert!(
            after
                .data
                .iter()
                .zip(&want.data)
                .all(|(a, b)| (a - b).abs() < 1e-5)
        );
    }
    // Mirrored, and four turns (with or without a mirror) come back.
    let mut m = g.clone();
    m.mirror();
    assert!(
        rotate_crop(&t, &m)
            .data
            .iter()
            .zip(&orient(&before, 2).data)
            .all(|(a, b)| (a - b).abs() < 1e-5)
    );
    m.turn(true);
    let want = orient(&orient(&before, 2), 6);
    let got = rotate_crop(&t, &m);
    assert!(
        got.data
            .iter()
            .zip(&want.data)
            .all(|(a, b)| (a - b).abs() < 1e-5),
        "turn after mirror"
    );
    for _ in 0..4 {
        g.turn(true);
    }
    assert_eq!(rotate_crop(&t, &g), before);
}

/// The eyedropper: a patch that should be grey comes out grey, for a raw
/// and for a finished image.
#[test]
fn the_eyedropper_makes_its_patch_neutral() {
    use shoebox_develop::edit::{WbMode, WhiteBalance};
    let raw = grey_card(0.2);
    let tinted = [0.30f64, 0.25, 0.16]; // a warm patch
    for src in [
        cpu::Demosaiced::new(Rgb::new(4, 4), [2.0, 1.0, 1.0 / 0.7], cpu::Referred::Scene),
        cpu::from_display(4, 4, &[128; 48], Target::Srgb),
    ] {
        let (temp, tint) = shoebox_develop::wb::neutral_at(&raw, &src, tinted);
        let mut s = Settings::default();
        s.modules.white_balance = WhiteBalance {
            mode: WbMode::Custom,
            temp: Some(temp),
            tint: Some(tint),
        };
        let f = cpu::frame(&raw, &src, &s, &Options::default());
        let m = f.colour;
        let p: Vec<f32> = (0..3)
            .map(|r| (0..3).map(|k| m[r][k] * tinted[k] as f32).sum())
            .collect();
        assert!(
            (p[0] / p[1] - 1.0).abs() < 0.01 && (p[2] / p[1] - 1.0).abs() < 0.01,
            "{:?}: {p:?}",
            src.referred
        );
    }
}

/// A region rendered on its own is exactly that part of the whole render
/// (zoomed views render only what's visible).
#[test]
fn a_region_is_that_part_of_the_render() {
    let raw = grey_card(0.3);
    let src = cpu::Demosaiced::new(truth(80, 60), [1.0; 3], cpu::Referred::Scene);
    let mut s = Settings::default();
    s.geometry.rotate = 3.0;
    s.geometry.crop = [0.1, 0.05, 0.9, 0.95];
    let f = cpu::frame(
        &raw,
        &src,
        &s,
        &Options {
            orientation: 6,
            ..Default::default()
        },
    );
    let whole = cpu::apply(&src.rgb, &f, true);
    let part = cpu::apply(&src.rgb, &f.region([7, 11, 20, 15]), true);
    assert_eq!((part.width, part.height), (20, 15));
    for y in 0..15 {
        for x in 0..20 {
            let (a, b) = (part.px(x, y), whole.px(x + 7, y + 11));
            assert!(
                a.iter().zip(b).all(|(p, q)| (p - q).abs() < 1e-5),
                "({x},{y})"
            );
        }
    }
    // Clamped to the frame.
    let edge = f.region([f.width - 5, 0, 50, 50]);
    assert_eq!((edge.width, edge.height), (5, 50.min(f.height)));
}

/// 16-bit samples land where their 8-bit equivalents do, and keep the
/// steps between.
#[test]
fn sixteen_bit_sources_match_and_refine_eight_bit() {
    for space in [Target::Srgb, Target::DisplayP3] {
        let px8: Vec<u8> = (0..=255u8).flat_map(|v| [v, 255 - v, v / 2]).collect();
        let px16: Vec<u16> = px8.iter().map(|&v| u16::from(v) * 257).collect();
        let a = cpu::from_display(256, 1, &px8, space);
        let b = cpu::from_display16(256, 1, &px16, space);
        for (x, y) in a.rgb.data.iter().zip(&b.rgb.data) {
            assert!((x - y).abs() < 1e-5, "{x} {y}");
        }
        let fine = cpu::from_display16(2, 1, &[32768, 32768, 32768, 32769, 32769, 32769], space);
        assert!(fine.rgb.data[3] > fine.rgb.data[0]);
    }
}
