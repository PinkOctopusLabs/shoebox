//! DNG and AVIF in the library: previews, rendering from the raw data,
//! sizes, the image-data hash, and AVIF through ImageIO.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_decode::{Format, Rgb8, Target, dng, render_srgb};
use shoebox_fixtures::{Dng, Shot};

fn mean_abs_diff(a: &Rgb8, b: &Rgb8) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.px.iter()
        .zip(&b.px)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / a.px.len() as f64
}

fn shot(orientation: u16) -> Shot {
    Shot {
        datetime: "2026:05:01 09:15:02",
        subsec: "65",
        orientation,
        serial: "7001234",
    }
}

/// A grey ramp of raw counts.
fn ramp(x: u32, y: u32) -> u16 {
    1100 + ((x * 31 + y * 17) % 900) as u16 * 12
}

/// A camera DNG like Adobe DNG Converter's: raw data and a JPEG preview
/// of the scene, at the raw's size.
fn camera_dng(w: u32, h: u32, orientation: u16) -> (Vec<u8>, Rgb8) {
    let scene = shoebox_fixtures::scene(5, w, h);
    let jpeg = shoebox_fixtures::jpeg(&scene, 92).unwrap();
    let dng = Dng {
        preview: Some(jpeg.clone()),
        shot: Some(shot(orientation)),
        ..Dng::new(w, h)
    };
    let preview = shoebox_decode::jpeg::decode_rgb(&jpeg).unwrap().0;
    (dng.write(ramp), preview)
}

#[test]
fn a_camera_dng_shows_its_preview() {
    let (data, preview) = camera_dng(320, 240, 1);
    let found = dng::previews(&data).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].width, found[0].height), (320, 240));
    assert_eq!(dng::dimensions(&data).unwrap(), (320, 240));
    assert_eq!(
        shoebox_decode::dimensions(Format::Dng, &data).unwrap(),
        (320, 240)
    );
    // The preview, not a render of the raw ramp.
    let r = render_srgb(Format::Dng, &data, Some(160), 1).unwrap();
    assert_eq!((r.width, r.height), (160, 120));
    let d = mean_abs_diff(&r, &preview.fit_long_edge(160));
    assert!(d < 2.0, "mean |Δ| {d:.2} from the preview");
    // Full size too: the preview is the image's own size.
    let full = render_srgb(Format::Dng, &data, None, 1).unwrap();
    assert!(mean_abs_diff(&full, &preview) < 2.0);
    // Turned upright by IFD 0's orientation.
    let (turned, _) = camera_dng(320, 240, 6);
    let r = render_srgb(Format::Dng, &turned, Some(160), 6).unwrap();
    assert_eq!((r.width, r.height), (120, 160));
    let g = shoebox_decode::luma_thumbnail(Format::Dng, &data, 64).unwrap();
    assert_eq!((g.width, g.height), (64, 48));
    // The embedded XMP finder reads it as a TIFF.
    assert_eq!(shoebox_decode::embedded_xmp(Format::Dng, &data), None);
    assert!(!dng::is_linear(&data));
}

/// The image-data hash covers the raw data only: a new preview or new
/// metadata doesn't change it; new raw data does.
#[test]
fn the_image_data_is_the_raw_data() {
    let hash = |data: &[u8]| -> Vec<u8> {
        shoebox_decode::image_data_ranges(Format::Dng, data)
            .unwrap()
            .unwrap()
            .into_iter()
            .flat_map(|r| data[r].to_vec())
            .collect()
    };
    let (a, _) = camera_dng(64, 48, 1);
    let (b, _) = camera_dng(64, 48, 6);
    let plain = Dng::new(64, 48).write(ramp);
    let other = Dng::new(64, 48).write(|x, y| ramp(x, y) + 1);
    assert_ne!(a, b);
    assert_eq!(hash(&a), hash(&b));
    assert_eq!(hash(&a), hash(&plain));
    assert_ne!(hash(&a), hash(&other));
    assert_eq!(hash(&a).len(), 64 * 48 * 2);
}

/// Without a preview big enough, the raw data is rendered (ImageIO on
/// macOS, else rawler): a grey ramp stays grey.
#[test]
fn a_dng_without_a_preview_renders_from_the_raw_data() {
    let dng = Dng::new(96, 64);
    let n = dng.neutral;
    let data = dng.write(|x, y| {
        let c = usize::from(dng.cfa[(y % 2) as usize][(x % 2) as usize]);
        let level = 0.05 + 0.4 * x as f32 / 96.0;
        dng.black + (n[c] * level * f32::from(dng.white - dng.black)) as u16
    });
    assert!(dng::previews(&data).unwrap().is_empty());
    let r = shoebox_decode::render(Format::Dng, &data, Some(48), 1, Target::Srgb).unwrap();
    assert_eq!(r.width.max(r.height), 48);
    let (mut lo, mut hi) = (255u8, 0u8);
    for p in r.px.as_chunks::<3>().0 {
        let spread = p.iter().max().unwrap() - p.iter().min().unwrap();
        assert!(spread <= 24, "a grey ramp renders grey: {p:?}");
        lo = lo.min(p[1]);
        hi = hi.max(p[1]);
    }
    assert!(hi > lo + 20, "the ramp shows ({lo}..{hi})");
    // rawler's own developer, the fallback, makes it too.
    let fallback = shoebox_decode::raw::render(&data, Some(48)).unwrap();
    assert_eq!(fallback.width.max(fallback.height), 48);
    assert!(shoebox_decode::luma_thumbnail(Format::Dng, &data, 32).is_ok());
}

/// AVIF goes through ImageIO, as HEIC does; `sips` makes one from a JPEG.
/// Its size comes from the boxes.
#[cfg(target_os = "macos")]
#[test]
fn avif_renders_through_imageio() {
    let t = tempfile::tempdir().unwrap();
    let (jpg, avif) = (t.path().join("a.jpg"), t.path().join("a.avif"));
    let img = shoebox_fixtures::scene(8, 600, 400);
    std::fs::write(&jpg, shoebox_fixtures::jpeg(&img, 92).unwrap()).unwrap();
    let ok = std::process::Command::new("sips")
        .args(["-s", "format", "avif"])
        .arg(&jpg)
        .arg("--out")
        .arg(&avif)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("sips can't write AVIF here; skipping");
        return;
    }
    let data = std::fs::read(&avif).unwrap();
    assert_eq!(Format::from_path(&avif), Some(Format::Avif));
    assert_eq!(shoebox_decode::heif::dimensions(&data).unwrap(), (600, 400));
    assert_eq!(
        shoebox_decode::dimensions(Format::Avif, &data).unwrap(),
        (600, 400)
    );
    let r = render_srgb(Format::Avif, &data, Some(300), 1).unwrap();
    assert_eq!((r.width, r.height), (300, 200));
    let ours = render_srgb(Format::Jpeg, &std::fs::read(&jpg).unwrap(), Some(300), 1).unwrap();
    let d = mean_abs_diff(&r, &ours);
    assert!(d < 6.0, "AVIF render differs from its source by {d:.2}");
    let g = shoebox_decode::luma_thumbnail(Format::Avif, &data, 64).unwrap();
    assert_eq!(g.width, 64);
    assert_eq!(
        shoebox_decode::image_data_ranges(Format::Avif, &data).unwrap(),
        None
    );
}
