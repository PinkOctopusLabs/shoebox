//! AVIF and linear DNG exports: presets that name them, and linear DNGs
//! that raw readers (rawler; ImageIO on macOS) read back as written.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use rawler::RawImageData;
use rawler::decoders::RawDecodeParams;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use shoebox_develop::cpu::{self, Rgb, Target};
use shoebox_develop::edit::Settings;
use shoebox_develop::export::{FileFormat, Preset, Space, encode, linear_dng, starters, stored_at};
use shoebox_fixtures::Dng;

#[test]
fn presets_name_the_new_formats_and_old_ones_still_load() {
    // A preset saved before AVIF and DNG existed.
    let old = r#"{"name":"Old","format":{"kind":"jpeg","quality":90},"space":"srgb"}"#;
    let p: Preset = serde_json::from_str(old).unwrap();
    assert_eq!(
        p.format,
        FileFormat::Jpeg {
            quality: 90,
            full_chroma: false,
            progressive: true
        }
    );
    // AVIF options left out take their defaults: 10-bit, the usual speed.
    let p: Preset = serde_json::from_str(
        r#"{"name":"A","format":{"kind":"avif","quality":70},"space":"display_p3"}"#,
    )
    .unwrap();
    assert_eq!(
        p.format,
        FileFormat::Avif {
            quality: 70,
            ten_bit: true,
            speed: shoebox_develop::encode::AVIF_SPEED
        }
    );
    let p: Preset = serde_json::from_str(r#"{"name":"D","format":{"kind":"linear_dng"}}"#).unwrap();
    assert_eq!(p.format, FileFormat::LinearDng);
    assert_eq!(p.format.extension(), "dng");
    assert!(p.format.spaces().is_empty());
    for format in [
        FileFormat::Avif {
            quality: 80,
            ten_bit: false,
            speed: 4,
        },
        FileFormat::LinearDng,
    ] {
        let p = Preset {
            format,
            ..starters()[0].clone()
        };
        let back: Preset = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        // Neither goes into the library until it reads them.
        assert!(!format.into_library());
    }
    assert!(starters()[0].format.into_library());
}

#[test]
fn avif_presets_encode_in_their_space() {
    let mut img = Rgb::new(64, 48);
    img.data.fill(0.5);
    let avif = |space: Space| {
        let p = Preset {
            format: FileFormat::Avif {
                quality: 80,
                ten_bit: true,
                speed: 9,
            },
            space,
            ..starters()[0].clone()
        };
        encode(&img, &p)
    };
    assert_eq!(&avif(Space::DisplayP3).unwrap()[4..12], b"ftypavif");
    assert!(avif(Space::AdobeRgb).is_err());
    let dng = Preset {
        format: FileFormat::LinearDng,
        ..starters()[0].clone()
    };
    assert!(
        encode(&img, &dng).is_err(),
        "a DNG isn't made from a render"
    );
}

/// The DNG's pixel lookup turns photos upright as the pipe does.
#[test]
fn stored_at_matches_the_pipes_orientation() {
    let (w, h) = (5usize, 3usize);
    let mut img = Rgb::new(w, h);
    for (i, v) in img.data.iter_mut().enumerate() {
        *v = (i / 3) as f32;
    }
    for o in 1..=8u16 {
        let turned = cpu::orient(&img, o);
        let (uw, uh) = cpu::upright(w, h, o);
        assert_eq!((turned.width, turned.height), (uw, uh));
        for y in 0..uh {
            for x in 0..uw {
                let (sx, sy) = stored_at(w, h, o, x, y);
                assert_eq!(
                    turned.px(x, y)[0],
                    (sy * w + sx) as f32,
                    "orientation {o} at ({x},{y})"
                );
            }
        }
    }
}

const RGGB: [[usize; 2]; 2] = [[0, 1], [1, 2]];

/// A raw file: grey on the left (`level` of white, scaled by the as-shot
/// neutral), a red-orange on the right, in camera RGB.
fn two_patches(w: u32, h: u32, level: f32) -> shoebox_decode::raw::RawImage {
    let dng = Dng::new(w, h);
    let n = dng.neutral;
    let range = f32::from(dng.white - dng.black);
    let bytes = dng.write(|x, y| {
        let c = RGGB[(y % 2) as usize][(x % 2) as usize];
        let v = if x < w / 2 {
            n[c] * level
        } else {
            [0.5, 0.12, 0.06][c]
        };
        dng.black + (v * range) as u16
    });
    shoebox_decode::raw::decode(&bytes).unwrap()
}

fn read_back(bytes: &[u8]) -> rawler::RawImage {
    rawler::decode(
        &RawSource::new_from_slice(bytes),
        &RawDecodeParams::default(),
    )
    .unwrap()
}

#[test]
fn a_linear_dng_reads_back_with_its_colour() {
    let raw = two_patches(96, 64, 0.3);
    let s = Settings::default();
    let src = cpu::demosaic(&raw, &s.modules.raw, None);
    let bytes = linear_dng(&raw, &src, &s, 1).unwrap();
    let back = read_back(&bytes);
    assert_eq!((back.width, back.height, back.cpp), (96, 64, 3));
    assert!(matches!(
        back.photometric,
        RawPhotometricInterpretation::LinearRaw
    ));
    assert_eq!(back.whitelevel.0, vec![65535; 3]);
    // As shot: the neutral's inverse, green 1.
    let n = Dng::new(1, 1).neutral;
    for (wb, n) in back.wb_coeffs.iter().zip(n) {
        assert!((wb - 1.0 / n).abs() < 1e-3, "{:?}", back.wb_coeffs);
    }
    // The fixture's two matrices, under A and D65.
    let fixture = Dng::new(1, 1);
    for (ill, m) in [
        (Illuminant::A, fixture.matrix1.0),
        (Illuminant::D65, fixture.matrix2.unwrap().0),
    ] {
        let got = &back.color_matrix[&ill];
        for (a, b) in got.iter().zip(m) {
            assert!((a - b).abs() < 1e-4, "{ill:?}: {got:?}");
        }
    }
    // Camera-native values: the grey card is the neutral at 0.3; the
    // red-orange as it was.
    let RawImageData::Integer(px) = &back.data else {
        panic!("not integer data");
    };
    let at = |x: usize, y: usize| -> [f32; 3] {
        let i = (y * 96 + x) * 3;
        [0, 1, 2].map(|c| f32::from(px[i + c]) / 65535.0)
    };
    for c in 0..3 {
        let grey = at(20, 30)[c];
        assert!((grey - 0.3 * n[c]).abs() < 0.003, "grey {c}: {grey}");
        let red = at(75, 30)[c];
        let want = [0.5, 0.12, 0.06][c];
        assert!((red - want).abs() < 0.003, "red {c}: {red}");
    }
    // No noise reduction applied: the tag says none.
    assert_eq!(noise_reduction(&bytes), 0.0);
}

/// The DNG's NoiseReductionApplied.
fn noise_reduction(bytes: &[u8]) -> f32 {
    use rawler::decoders::WellKnownIFD;
    use rawler::formats::tiff::Value;
    let src = RawSource::new_from_slice(bytes);
    let ifd = rawler::get_decoder(&src)
        .unwrap()
        .ifd(WellKnownIFD::VirtualDngRawTags)
        .unwrap()
        .unwrap();
    let entry = ifd
        .get_entry(rawler::tags::DngTag::NoiseReductionApplied)
        .unwrap();
    let Value::Rational(r) = &entry.value else {
        panic!("{:?}", entry.value);
    };
    r[0].n as f32 / r[0].d as f32
}

#[test]
fn a_linear_dng_is_upright_and_carries_the_ai_denoise() {
    let raw = two_patches(96, 64, 0.3);
    let mut s = Settings::default();
    let src = cpu::demosaic(&raw, &s.modules.raw, None);
    // AI denoise on but not run: refused, rather than an archive that
    // isn't what it says.
    s.modules.denoise_ai.enabled = true;
    assert!(linear_dng(&raw, &src, &s, 1).is_err());
    // A stand-in result: everything half as bright.
    let mut ai = src.rgb.clone();
    ai.data.iter_mut().for_each(|v| *v *= 0.5);
    src.cache.set_ai(std::sync::Arc::new(ai));
    s.modules.denoise_ai.strength = 100.0;
    // Turned a quarter clockwise (EXIF 6): the grey card on top.
    let bytes = linear_dng(&raw, &src, &s, 6).unwrap();
    let back = read_back(&bytes);
    assert_eq!((back.width, back.height), (64, 96));
    let RawImageData::Integer(px) = &back.data else {
        panic!("not integer data");
    };
    let n = Dng::new(1, 1).neutral;
    let i = (20 * 64 + 30) * 3;
    let g = f32::from(px[i + 1]) / 65535.0;
    assert!((g - 0.15 * n[1]).abs() < 0.003, "{g}");
    let i = (75 * 64 + 30) * 3;
    assert!((f32::from(px[i]) / 65535.0 - 0.25).abs() < 0.003);
    assert_eq!(noise_reduction(&bytes), 1.0);
    s.modules.denoise_ai.strength = 40.0;
    let bytes = linear_dng(&raw, &src, &s, 1).unwrap();
    assert!((noise_reduction(&bytes) - 0.4).abs() < 1e-6);
    // A finished image (a JPEG's pixels) has no raw data to archive.
    let jpeg = cpu::from_display(4, 4, &[128; 48], Target::Srgb);
    assert!(linear_dng(&raw, &jpeg, &Settings::default(), 1).is_err());
}

/// macOS's raw engine (Preview's and Photos') renders the DNG from its
/// raw data, not just the preview: full size, the grey card grey, the
/// red-orange red.
#[cfg(target_os = "macos")]
#[test]
fn imageio_renders_a_linear_dng() {
    let raw = two_patches(600, 400, 0.18);
    let s = Settings::default();
    let src = cpu::demosaic(&raw, &s.modules.raw, None);
    let bytes = linear_dng(&raw, &src, &s, 1).unwrap();
    let img = shoebox_decode::imageio::thumbnail_rgb(
        &bytes,
        4096,
        false,
        Some("com.adobe.raw-image"),
        shoebox_decode::Target::Srgb,
    )
    .unwrap();
    // The preview in IFD 0 is 200×134: this is the raw.
    assert_eq!((img.width, img.height), (600, 400));
    let at = |x: usize, y: usize| {
        let i = (y * 600 + x) * 3;
        [0, 1, 2].map(|c| i32::from(img.px[i + c]))
    };
    let grey = at(150, 200);
    assert!(
        grey.iter().max().unwrap() - grey.iter().min().unwrap() <= 8 && grey[1] > 40,
        "{grey:?}"
    );
    let red = at(450, 200);
    assert!(red[0] > red[1] + 60 && red[0] > red[2] + 60, "{red:?}");
}
