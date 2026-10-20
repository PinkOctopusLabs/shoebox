//! JPEGs tagged with their space decode back to the same colours.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::{Rgb, Target};
use shoebox_develop::encode::{Jpeg, jpeg};

#[test]
fn a_p3_jpeg_round_trips_through_the_library_decoder() {
    let (w, h) = (96usize, 64usize);
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            // Flat patches (JPEG keeps those), including a saturated P3 red.
            let p = match (x / 32, y / 32) {
                (0, 0) => [0.9, 0.1, 0.1],
                (1, 0) => [0.2, 0.6, 0.3],
                (2, 0) => [0.5, 0.5, 0.5],
                (0, 1) => [0.1, 0.2, 0.8],
                (1, 1) => [0.95, 0.85, 0.2],
                _ => [0.05, 0.05, 0.05],
            };
            img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&p);
        }
    }
    let bytes = jpeg(
        &img,
        Target::DisplayP3,
        Jpeg {
            quality: 95,
            full_chroma: true,
            progressive: true,
        },
    )
    .unwrap();
    let back = shoebox_decode::render(
        shoebox_decode::Format::Jpeg,
        &bytes,
        None,
        1,
        shoebox_decode::Target::DisplayP3,
    )
    .unwrap();
    assert_eq!((back.width as usize, back.height as usize), (w, h));
    for (x, y) in [(16, 16), (48, 16), (80, 16), (16, 48), (48, 48)] {
        let i = (y * w + x) * 3;
        for c in 0..3 {
            let want = (img.data[i + c] * 255.0).round();
            let got = f32::from(back.px[i + c]);
            assert!(
                (want - got).abs() <= 3.0,
                "({x},{y}) channel {c}: {want} vs {got}"
            );
        }
    }
}
