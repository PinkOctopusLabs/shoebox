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

fn real(name: &str) -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/real")
        .join(name);
    std::fs::read(p).ok()
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
    let Some(j) = real("20260501-091502-65.jpg") else {
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
    let Some(n) = real("20260501-091502-65.nef") else {
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
