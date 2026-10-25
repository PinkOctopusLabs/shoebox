//! Display rendering: embedded previews, colour management, orientation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use shoebox_decode::{Format, Rgb8, jpeg, render_srgb};

fn mean_abs_diff(a: &Rgb8, b: &Rgb8) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.px.iter()
        .zip(&b.px)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / a.px.len() as f64
}

/// The first real camera file with this extension in the gitignored
/// `tests/fixtures/real/` (any name), if there is one.
fn real(ext: &str) -> Option<Vec<u8>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/real");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
        .collect();
    files.sort();
    std::fs::read(files.first()?).ok()
}

#[test]
fn orientation_is_applied() {
    let img = shoebox_fixtures::scene(3, 300, 200);
    let j = shoebox_fixtures::jpeg(&img, 90).unwrap();
    let r = render_srgb(Format::Jpeg, &j, Some(150), 6).unwrap();
    assert_eq!(
        (r.width, r.height),
        (100, 150),
        "Orientation 6 turns landscape into portrait"
    );
}

/// Adobe RGB TIFFs must look as they do in Preview.app, i.e. as ImageIO and
/// ColorSync convert them to sRGB.
#[cfg(target_os = "macos")]
#[test]
fn adobe_rgb_tiff_matches_imageio() {
    let img = shoebox_fixtures::scene(7, 600, 400);
    let icc = moxcms::ColorProfile::new_adobe_rgb().encode().unwrap();
    let t = shoebox_fixtures::tiff_with_icc(&img, &icc);
    let ours = render_srgb(Format::Tiff, &t, Some(300), 1).unwrap();
    let theirs =
        shoebox_decode::imageio::thumbnail_srgb(&t, 300, false, Some("public.tiff")).unwrap();
    let untouched = Rgb8::new(600, 400, img.into_raw())
        .unwrap()
        .fit_long_edge(300);
    let d_cms = mean_abs_diff(&ours, &theirs);
    let d_raw = mean_abs_diff(&untouched, &theirs);
    eprintln!("mean |Δ| vs ImageIO: managed {d_cms:.2}, unmanaged {d_raw:.2}");
    assert!(
        d_cms < 2.0,
        "colour-managed render differs from ImageIO by {d_cms:.2}"
    );
    assert!(
        d_raw > 2.0 * d_cms,
        "test image should show the Adobe RGB shift"
    );
}

#[test]
fn real_jpg_uses_mpf_previews_for_thumbnails() {
    let Some(j) = real("jpg") else {
        return;
    };
    let previews = jpeg::mpf_images(&j);
    let dims: Vec<_> = previews
        .iter()
        .map(|r| jpeg::dimensions(&j[r.clone()]).unwrap())
        .collect();
    eprintln!("MPF previews: {dims:?}");
    assert!(dims.contains(&(640, 424)), "{dims:?}");

    let t = std::time::Instant::now();
    let fast = render_srgb(Format::Jpeg, &j, Some(256), 1).unwrap();
    let fast_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = std::time::Instant::now();
    // The preview's aspect differs from the full frame's by a pixel.
    let full = render_srgb(Format::Jpeg, &j, None, 1)
        .unwrap()
        .resize(fast.width, fast.height);
    let full_ms = t.elapsed().as_secs_f64() * 1e3;
    let d = mean_abs_diff(&fast, &full);
    eprintln!("256 px: preview {fast_ms:.1} ms, full decode {full_ms:.1} ms, mean |Δ| {d:.2}");
    assert!(
        d < 4.0,
        "preview thumbnail looks different from the full image ({d:.2})"
    );
}

#[test]
fn real_nef_renders_from_embedded_jpeg() {
    let Some(n) = real("nef") else {
        return;
    };
    let r = render_srgb(Format::Nef, &n, Some(256), 1).unwrap();
    assert_eq!(r.width.max(r.height), 256);
}

/// HEIC goes through ImageIO on macOS. `sips` makes a real HEIC from a JPEG.
#[cfg(target_os = "macos")]
#[test]
fn heic_renders_through_imageio() {
    let t = tempfile::tempdir().unwrap();
    let (jpg, heic) = (t.path().join("a.jpg"), t.path().join("a.heic"));
    let img = shoebox_fixtures::scene(8, 1200, 800);
    std::fs::write(&jpg, shoebox_fixtures::jpeg(&img, 92).unwrap()).unwrap();
    let ok = std::process::Command::new("sips")
        .args(["-s", "format", "heic"])
        .arg(&jpg)
        .arg("--out")
        .arg(&heic)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("sips can't write HEIC here; skipping");
        return;
    }
    let data = std::fs::read(&heic).unwrap();
    let r = render_srgb(Format::Heic, &data, Some(300), 1).unwrap();
    assert_eq!((r.width, r.height), (300, 200));
    let ours = render_srgb(Format::Jpeg, &std::fs::read(&jpg).unwrap(), Some(300), 1).unwrap();
    let d = mean_abs_diff(&r, &ours);
    eprintln!("HEIC vs source JPEG: mean |Δ| {d:.2}");
    assert!(d < 6.0, "HEIC render differs from its source by {d:.2}");
    assert!(shoebox_decode::luma_thumbnail(Format::Heic, &data, 64).is_ok());
}

/// Display P3 output matches ColorSync's, and keeps colours sRGB would clip.
#[cfg(target_os = "macos")]
#[test]
fn adobe_rgb_to_display_p3_matches_imageio_and_keeps_gamut() {
    use shoebox_decode::{Target, imageio, render};
    let icc = moxcms::ColorProfile::new_adobe_rgb().encode().unwrap();
    // A scene plus a band of deep Adobe RGB green (outside sRGB, inside P3).
    let mut img = shoebox_fixtures::scene(7, 600, 400);
    for y in 0..60 {
        for x in 0..600 {
            img.put_pixel(x, y, image::Rgb([20, 200, 40]));
        }
    }
    let t = shoebox_fixtures::tiff_with_icc(&img, &icc);
    let ours = render(Format::Tiff, &t, Some(300), 1, Target::DisplayP3).unwrap();
    let theirs =
        imageio::thumbnail_rgb(&t, 300, false, Some("public.tiff"), Target::DisplayP3).unwrap();
    let d = mean_abs_diff(&ours, &theirs);
    eprintln!("Display P3: mean |Δ| vs ImageIO {d:.2}");
    assert!(d < 2.0, "P3 render differs from ImageIO by {d:.2}");

    // In the green band sRGB clips both red and blue to 0; P3 (a wider
    // gamut) still needs red at 0 but keeps the blue component.
    let srgb = render(Format::Tiff, &t, Some(300), 1, Target::Srgb).unwrap();
    let px = |im: &Rgb8| im.px[(10 * im.width as usize + 150) * 3..][..3].to_vec();
    eprintln!("deep green: sRGB {:?}, P3 {:?}", px(&srgb), px(&ours));
    // sRGB clips both red and blue to 0; P3, a wider gamut, still needs red
    // at 0 for this green but keeps its blue component.
    assert_eq!(
        (px(&srgb)[0], px(&srgb)[2]),
        (0, 0),
        "sRGB should clip this green"
    );
    assert!(
        px(&ours)[2] > 20,
        "P3 should keep more of this green's gamut"
    );
}

/// The first real file with this extension's path, if there is one.
fn real_path(ext: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/real");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
        .collect();
    files.sort();
    files.into_iter().next()
}

/// A Sony ARW shows the whole frame: the NEX-3's embedded JPEG is a 16:9
/// crop of its 3:2 photo, so it's rendered from the raw data instead (with
/// no image-data hash). Its raw data decodes for Develop.
#[test]
fn real_arw() {
    let Some(data) = real("arw") else { return };
    let (w, h) = shoebox_decode::dimensions(Format::Arw, &data).unwrap();
    assert!(w.max(h) >= 4000, "{w}×{h}");
    let img = shoebox_decode::render_srgb(Format::Arw, &data, Some(600), 1).unwrap();
    assert_eq!(img.width.max(img.height), 600);
    let ratio = f64::from(img.width) / f64::from(img.height);
    assert!((ratio - 1.5).abs() < 0.02, "the whole 3:2 frame: {ratio}");
    let full = shoebox_decode::render_srgb(Format::Arw, &data, None, 1).unwrap();
    assert!(full.width >= w - 32 && full.height >= h - 32, "full size");
    assert!(
        shoebox_decode::image_data_ranges(Format::Arw, &data)
            .unwrap()
            .is_none()
    );
    let raw = shoebox_decode::raw::decode(&data).unwrap();
    assert!(raw.is_mosaic());
}

/// WebP and PSD through ImageIO, their sizes from their headers.
#[test]
fn real_webp_and_psd() {
    for (ext, format) in [("webp", Format::Webp), ("psd", Format::Psd)] {
        let Some(data) = real(ext) else { continue };
        let (w, h) = shoebox_decode::dimensions(format, &data).unwrap();
        let img = shoebox_decode::render_srgb(format, &data, None, 1).unwrap();
        assert_eq!((img.width, img.height), (w, h), "{ext}");
        let small = shoebox_decode::render_srgb(format, &data, Some(128), 1).unwrap();
        assert_eq!(small.width.max(small.height), 128, "{ext}");
    }
}

/// An MPEG-1 clip: its size and duration from the stream, a poster frame
/// from AVFoundation.
#[test]
fn real_mpg() {
    let Some(path) = real_path("mpg") else { return };
    let i = shoebox_decode::movie_info(Format::Mpg, &path).unwrap();
    assert!(i.width.is_some() && i.height.is_some(), "{i:?}");
    assert!(i.duration_secs.is_some_and(|d| d > 0.0), "{i:?}");
    let poster = shoebox_decode::render_path(
        Format::Mpg,
        &path,
        Some(320),
        1,
        shoebox_decode::Target::Srgb,
    )
    .unwrap();
    assert_eq!(poster.width.max(poster.height), 320);
}
