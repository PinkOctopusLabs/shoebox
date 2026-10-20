//! Raw decode for Develop: NEF compression, and rawler's output prepared for
//! the pipe (crop, black, white, white balance, matrices) on synthetic DNGs.

use std::path::Path;

use shoebox_decode::nef::{self, NefCompression};
use shoebox_decode::raw::{self, BLUE, GREEN, RED};
use shoebox_fixtures::{Dng, nikon_compression_stub};

const REAL_NEF: &str = "../../tests/fixtures/real/20260501-203002-65.nef";

#[test]
fn compression_from_the_maker_note() {
    let z = nikon_compression_stub(14, true);
    assert_eq!(
        nef::compression(&z).unwrap(),
        Some(NefCompression::HighEfficiencyStar)
    );
    let z = nikon_compression_stub(13, true);
    assert_eq!(
        nef::compression(&z).unwrap(),
        Some(NefCompression::HighEfficiency)
    );
    let old = nikon_compression_stub(3, false);
    let c = nef::compression(&old).unwrap().unwrap();
    assert_eq!(c, NefCompression::Lossless);
    assert!(!c.needs_dng());
    assert!(NefCompression::HighEfficiencyStar.needs_dng());
    assert_eq!(NefCompression::from_tag(c.tag()), c);

    // A TIFF that isn't a NEF has no maker note.
    let img = shoebox_fixtures::scene(1, 64, 48);
    let tiff = shoebox_fixtures::tiff(&img).unwrap();
    assert_eq!(nef::compression(&tiff).unwrap(), None);
}

/// Raw count for a photosite: black plus a fraction of the range that encodes
/// its position, so tests can tell which photosite landed where.
fn count(x: u32, y: u32) -> u16 {
    1008 + ((x + 7 * y) % 100) as u16 * 100
}

#[test]
fn dng_black_white_and_metadata() {
    let dng = Dng::new(40, 30);
    let raw = raw::decode(&dng.write(count)).unwrap();
    assert_eq!((raw.width, raw.height), (40, 30));
    assert_eq!(raw.cfa, [[RED, GREEN], [GREEN, BLUE]]);
    assert_eq!(raw.color_at(1, 1), BLUE);
    let range = 15892.0 - 1008.0;
    for (x, y) in [(0, 0), (5, 3), (39, 29)] {
        let want = f32::from(count(x, y) - 1008) / range;
        let got = raw.data[(y * 40 + x) as usize];
        assert!((got - want).abs() < 1e-6, "({x},{y}): {got} vs {want}");
    }
    let wb = raw.as_shot.unwrap();
    assert!((wb[0] - 2.0).abs() < 1e-3 && wb[1] == 1.0 && (wb[2] - 1.0 / 0.7).abs() < 1e-3);
    let temps: Vec<f32> = raw.matrices.iter().map(|m| m.temperature).collect();
    assert_eq!(temps, [2856.0, 6504.0]);
    assert!((raw.matrices[1].xyz_to_cam[0][0] - 1.1607).abs() < 1e-4);
    assert_eq!(
        (raw.make.as_str(), raw.model.as_str()),
        ("Shoebox", "Test Sensor")
    );
}

#[test]
fn below_black_is_zero_and_white_clips() {
    let dng = Dng::new(8, 8);
    let raw = raw::decode(&dng.write(|x, _| match x {
        0 => 900,
        1 => 15892,
        2 => 16383,
        _ => 5000,
    }))
    .unwrap();
    assert_eq!(raw.data[0], 0.0);
    assert_eq!(raw.data[1], 1.0);
    assert!(raw.clipped(1, 0) && raw.clipped(2, 0) && !raw.clipped(3, 0));
    // Above white is kept, not clipped to 1.0.
    assert!(raw.data[2] > 1.0);
}

#[test]
fn active_area_and_crop_shift_the_pattern() {
    let mut dng = Dng::new(40, 30);
    // CFA pattern counts from the active area's corner.
    dng.active = Some((1, 2, 39, 30));
    let raw = raw::decode(&dng.write(count)).unwrap();
    assert_eq!((raw.width, raw.height), (38, 28));
    assert_eq!(raw.cfa, [[RED, GREEN], [GREEN, BLUE]]);
    let first = f32::from(count(1, 2) - 1008) / (15892.0 - 1008.0);
    assert!((raw.data[0] - first).abs() < 1e-6);

    dng.crop = Some(((1, 0), (36, 28)));
    let raw = raw::decode(&dng.write(count)).unwrap();
    assert_eq!((raw.width, raw.height), (36, 28));
    assert_eq!(raw.cfa, [[GREEN, RED], [BLUE, GREEN]]);
    let first = f32::from(count(2, 2) - 1008) / (15892.0 - 1008.0);
    assert!((raw.data[0] - first).abs() < 1e-6);
}

#[test]
fn junk_is_an_error_not_a_panic() {
    assert!(raw::decode(b"not a raw file").is_err());
    let mut dng = Dng::new(16, 16).write(count);
    dng.truncate(dng.len() / 2);
    assert!(raw::decode(&dng).is_err());
}

/// The real Z f fixture is High efficiency★: the maker note says so, and
/// rawler declines it (Develop converts such files to DNG first).
#[test]
fn real_high_efficiency_nef() {
    let path = Path::new(REAL_NEF);
    if !path.exists() {
        eprintln!("skipping: {REAL_NEF} not present");
        return;
    }
    let data = std::fs::read(path).unwrap();
    let c = nef::compression(&data).unwrap();
    assert_eq!(c, Some(NefCompression::HighEfficiencyStar));
    assert!(matches!(
        raw::decode(&data),
        Err(shoebox_decode::DecodeError::Unsupported(_))
    ));
}

/// The Z f's own lens data (exiftool reads distortion 0.00935, −0.02086,
/// 0.00998 and vignette 2.2917, −1.8133, 0.8411 for this file).
#[test]
fn real_nef_lens_data() {
    let Ok(bytes) = std::fs::read(REAL_NEF) else {
        eprintln!("skipping: no {REAL_NEF}");
        return;
    };
    let d = nef::lens_data(&bytes);
    let close =
        |got: [f64; 3], want: [f64; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-4);
    assert!(
        close(d.distortion.unwrap(), [0.00935, -0.02086, 0.00998]),
        "{d:?}"
    );
    assert!(
        close(d.vignette.unwrap(), [2.29174, -1.81334, 0.84110]),
        "{d:?}"
    );
    assert_eq!(nef::lens_data(b"not a tiff"), nef::LensData::default());
}
