//! Auto: its rules from measured statistics, and the measurements on
//! synthetic raws.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::auto::{Stats, measure, settings};
use shoebox_develop::cpu::{Demosaiced, Referred, Rgb};
use shoebox_develop::edit::Settings;

fn stats() -> Stats {
    Stats {
        highlights_ev: 1.0,
        camera_ev: None,
        deep_shadows: 0.1,
        haze: 0.05,
    }
}

#[test]
fn the_rules() {
    let s = settings(&Settings::default(), &stats(), Some(100), false);
    assert_eq!(s.modules.exposure.ev, 1.0);
    assert_eq!(s.modules.noise_reduction.luminance, 0.0);
    assert_eq!(s.modules.noise_reduction.chroma, 20.0);
    assert_eq!(s.modules.dehaze.amount, 0.0);
    assert_eq!(s.modules.highlights_shadows.shadows, 0.0);
    assert!(!s.modules.lens.enabled);
    // Halfway to the camera JPEG; more noise reduction at high ISO; haze
    // and deep shadows; a lens profile.
    let st = Stats {
        camera_ev: Some(0.0),
        deep_shadows: 0.5,
        haze: 0.4,
        ..stats()
    };
    let s = settings(&Settings::default(), &st, Some(6400), true);
    assert_eq!(s.modules.exposure.ev, 0.5);
    assert_eq!(s.modules.noise_reduction.luminance, 40.0);
    assert!(s.modules.noise_reduction.chroma > 40.0);
    assert_eq!(s.modules.dehaze.amount, 15.0);
    assert_eq!(s.modules.highlights_shadows.shadows, 15.0);
    assert!(s.modules.lens.enabled);
    // It keeps what it doesn't set.
    let mut base = Settings::default();
    base.modules.colour.saturation = 30.0;
    assert_eq!(
        settings(&base, &st, None, false).modules.colour.saturation,
        30.0
    );
}

fn raw() -> shoebox_decode::raw::RawImage {
    shoebox_decode::raw::RawImage {
        width: 0,
        height: 0,
        cfa: [[0, 1], [1, 2]],
        data: Vec::new(),
        as_shot: None,
        matrices: Vec::new(),
        make: String::new(),
        model: String::new(),
    }
}

#[test]
fn measurements() {
    // A dim scene: Auto brightens it; a hazy one (every channel lifted)
    // measures hazy, a colourful one doesn't.
    let make = |f: &dyn Fn(usize, usize) -> [f32; 3]| {
        let (w, h) = (200, 150);
        let mut img = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 3;
                img.data[i..i + 3].copy_from_slice(&f(x, y));
            }
        }
        Demosaiced::new(img, [1.0; 3], Referred::Display)
    };
    let dim = make(&|x, _| [0.01 + 0.02 * x as f32 / 200.0; 3]);
    let s = measure(&raw(), &dim, None);
    assert!(s.highlights_ev > 1.0, "{s:?}");
    let hazy = make(&|x, y| {
        let v = 0.3 + 0.2 * ((x + y) % 7) as f32 / 7.0;
        [v + 0.05, v, v + 0.02]
    });
    let clear = make(&|x, _| {
        if x < 100 {
            [0.6, 0.05, 0.02]
        } else {
            [0.02, 0.1, 0.5]
        }
    });
    let (h, c) = (
        measure(&raw(), &hazy, None).haze,
        measure(&raw(), &clear, None).haze,
    );
    assert!(h > 0.4 && c < 0.15, "{h} {c}");
}
