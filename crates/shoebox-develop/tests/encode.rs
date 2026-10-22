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

/// Flat patches, including a saturated colour sRGB can't hold.
fn patches(w: usize, h: usize) -> Rgb {
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
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
    img
}

/// The payload of the first box called `name` (a plain search: the names
/// looked for here occur once).
fn find_box<'a>(bytes: &'a [u8], name: &[u8; 4]) -> &'a [u8] {
    let at = bytes
        .windows(4)
        .position(|w| w == name)
        .unwrap_or_else(|| panic!("no {} box", String::from_utf8_lossy(name)));
    let size = u32::from_be_bytes(bytes[at - 4..at].try_into().unwrap()) as usize;
    &bytes[at + 4..at - 4 + size]
}

#[test]
fn avif_boxes_give_size_depth_and_space() {
    use shoebox_develop::encode::{Avif, avif};
    let img = patches(96, 64);
    for (target, primaries) in [(Target::Srgb, 1u16), (Target::DisplayP3, 12)] {
        for ten_bit in [true, false] {
            let o = Avif {
                quality: 90,
                ten_bit,
                speed: 8,
            };
            let bytes = avif(&img, target, o).unwrap();
            assert_eq!(&bytes[4..12], b"ftypavif");
            let ispe = find_box(&bytes, b"ispe");
            assert_eq!(u32::from_be_bytes(ispe[4..8].try_into().unwrap()), 96);
            assert_eq!(u32::from_be_bytes(ispe[8..12].try_into().unwrap()), 64);
            let pixi = find_box(&bytes, b"pixi");
            let bits = if ten_bit { 10 } else { 8 };
            assert_eq!(&pixi[4..8], [3, bits, bits, bits]);
            if target == Target::Srgb {
                // AVIF's defaults (sRGB, BT.601, full range): no box.
                assert!(!bytes.windows(4).any(|w| w == b"colr"));
                continue;
            }
            let colr = find_box(&bytes, b"colr");
            assert_eq!(&colr[..4], b"nclx");
            let cicp =
                |i: usize| u16::from_be_bytes(colr[4 + 2 * i..6 + 2 * i].try_into().unwrap());
            // The primaries, the sRGB curve (13), BT.601 coefficients (6),
            // full range.
            assert_eq!((cicp(0), cicp(1), cicp(2)), (primaries, 13, 6));
            assert_eq!(colr[10] & 0x80, 0x80);
        }
    }
    assert!(avif(&img, Target::AdobeRgb, Avif::default()).is_err());
    assert!(avif(&img, Target::ProPhoto, Avif::default()).is_err());
}

/// macOS's own decoder (Preview's and Photos') reads Shoebox's AVIFs back
/// in the space they were written in.
#[cfg(target_os = "macos")]
#[test]
fn avif_round_trips_through_imageio() {
    use shoebox_develop::encode::{Avif, avif};
    let (w, h) = (96usize, 64usize);
    let img = patches(w, h);
    let read = |bytes: &[u8], space| {
        shoebox_decode::imageio::thumbnail_rgb(bytes, 4096, false, Some("public.avif"), space)
            .unwrap()
    };
    for target in [Target::Srgb, Target::DisplayP3] {
        for ten_bit in [true, false] {
            let o = Avif {
                quality: 95,
                ten_bit,
                speed: 8,
            };
            let bytes = avif(&img, target, o).unwrap();
            let space = match target {
                Target::Srgb => shoebox_decode::Target::Srgb,
                _ => shoebox_decode::Target::DisplayP3,
            };
            let back = read(&bytes, space);
            assert_eq!((back.width as usize, back.height as usize), (w, h));
            for (x, y) in [(16, 16), (48, 16), (80, 16), (16, 48), (48, 48)] {
                let i = (y * w + x) * 3;
                for c in 0..3 {
                    let want = (img.data[i + c] * 255.0).round();
                    let got = f32::from(back.px[i + c]);
                    assert!(
                        (want - got).abs() <= 4.0,
                        "{target:?} ten_bit {ten_bit} ({x},{y}) channel {c}: {want} vs {got}"
                    );
                }
            }
        }
    }
    // P3 really is read as P3: its saturated red, seen in sRGB, has no
    // green left (read as sRGB it would keep about 26).
    let bytes = avif(&img, Target::DisplayP3, Avif::default()).unwrap();
    let srgb = read(&bytes, shoebox_decode::Target::Srgb);
    let i = (16 * w + 16) * 3;
    assert!(
        srgb.px[i + 1] < 10,
        "P3 red read as sRGB: {:?}",
        &srgb.px[i..i + 3]
    );
}
