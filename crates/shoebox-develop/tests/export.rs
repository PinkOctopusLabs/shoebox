//! Export: sizes, Lanczos resizing, TIFF and JPEG files, names, and never
//! replacing a file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::Rgb;
use shoebox_develop::export::{
    FileFormat, OutputSize, Preset, TiffCompression, encode, file_stem, output_size, resize,
    starters, write_new, write_replacing,
};

#[test]
fn sizes() {
    assert_eq!(output_size(6048, 4032, OutputSize::Full), (6048, 4032));
    assert_eq!(
        output_size(6048, 4032, OutputSize::LongEdge { px: 2048 }),
        (2048, 1365)
    );
    assert_eq!(
        output_size(4032, 6048, OutputSize::LongEdge { px: 2048 }),
        (1365, 2048)
    );
    let (w, h) = output_size(6048, 4032, OutputSize::Megapixels { mp: 6.0 });
    assert!(
        (w * h) as f64 <= 6.0e6 * 1.001 && (w * h) as f64 > 5.9e6,
        "{w}×{h}"
    );
    // Never larger than the render.
    assert_eq!(
        output_size(800, 600, OutputSize::LongEdge { px: 2048 }),
        (800, 600)
    );
}

fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [f32; 3]) -> Rgb {
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&f(x, y));
        }
    }
    img
}

#[test]
fn lanczos_keeps_flat_flat_and_ramps_in_order() {
    let flat = resize(&image(300, 200, |_, _| [0.25, 0.5, 0.75]), 97, 64);
    assert_eq!((flat.width, flat.height), (97, 64));
    assert!(
        flat.data
            .chunks(3)
            .all(|p| (p[0] - 0.25).abs() < 1e-4 && (p[2] - 0.75).abs() < 1e-4)
    );
    let ramp = resize(&image(400, 10, |x, _| [x as f32 / 399.0; 3]), 100, 10);
    let row: Vec<f32> = (0..100).map(|x| ramp.px(x, 5)[0]).collect();
    assert!(row.windows(2).all(|w| w[1] >= w[0] - 1e-4), "{row:?}");
    assert!(row[0] < 0.02 && row[99] > 0.98);
}

#[test]
fn a_16_bit_tiff_reads_back_with_its_profile() {
    let img = image(40, 30, |x, y| [x as f32 / 39.0, y as f32 / 29.0, 0.5]);
    for compression in [TiffCompression::Lzw, TiffCompression::Zip] {
        let p = Preset {
            format: FileFormat::Tiff { compression },
            ..starters()[2].clone()
        };
        let bytes = encode(&img, &p).unwrap();
        let mut d = tiff::decoder::Decoder::new(std::io::Cursor::new(&bytes)).unwrap();
        assert_eq!(d.dimensions().unwrap(), (40, 30));
        assert!(
            d.get_tag_u8_vec(tiff::tags::Tag::IccProfile)
                .is_ok_and(|v| v.len() > 100)
        );
        let tiff::decoder::DecodingResult::U16(px) = d.read_image().unwrap() else {
            panic!("not 16-bit");
        };
        let want = shoebox_develop::cpu::to_u16(&img);
        assert_eq!(px, want);
    }
    // And a JPEG is a JPEG.
    let jpg = encode(&img, &starters()[0]).unwrap();
    assert_eq!(&jpg[..2], &[0xFF, 0xD8]);
}

#[test]
fn names() {
    assert_eq!(
        file_stem("{name}_{preset}", "DSC_1234", "Web 2048", None, 1),
        "DSC_1234_Web 2048"
    );
    assert_eq!(
        file_stem("{date}_{seq}", "x", "p", Some("2026-10-20"), 7),
        "2026-10-20_7"
    );
    // Slashes can't make folders; an empty result falls back to the name.
    assert_eq!(file_stem("{preset}", "x", "a/b", None, 1), "a_b");
    assert_eq!(file_stem("...", "x", "p", None, 1), "x");
}

#[test]
fn never_replaces_a_file() {
    let t = tempfile::tempdir().unwrap();
    let a = write_new(t.path(), "photo", "jpg", b"one").unwrap();
    let b = write_new(t.path(), "photo", "jpg", b"two").unwrap();
    assert_eq!(a, t.path().join("photo.jpg"));
    assert_eq!(b, t.path().join("photo-1.jpg"));
    assert_eq!(std::fs::read(&a).unwrap(), b"one");
}

#[test]
fn presets_are_json() {
    for p in starters() {
        let text = serde_json::to_string_pretty(&p).unwrap();
        let back: Preset = serde_json::from_str(&text).unwrap();
        assert_eq!(back, p);
    }
    let v = serde_json::to_value(&starters()[1]).unwrap();
    assert_eq!(v["format"]["kind"], "jpeg");
    assert_eq!(
        v["size"],
        serde_json::json!({ "kind": "long_edge", "px": 2048 })
    );
}

#[test]
fn replacing_when_chosen() {
    let t = tempfile::tempdir().unwrap();
    let a = write_new(t.path(), "photo", "jpg", b"one").unwrap();
    let b = write_replacing(t.path(), "photo", "jpg", b"two").unwrap();
    assert_eq!(a, b);
    assert_eq!(std::fs::read(&a).unwrap(), b"two");
    // Nothing else left behind.
    assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1);
    // And it creates when there's nothing to replace.
    let c = write_replacing(t.path(), "other", "jpg", b"three").unwrap();
    assert_eq!(std::fs::read(c).unwrap(), b"three");
}

/// A link planted in the export folder is never written through.
#[cfg(unix)]
#[test]
fn replacing_never_writes_through_a_link() {
    let t = tempfile::tempdir().unwrap();
    let victim = t.path().join("victim.txt");
    std::fs::write(&victim, b"keep me").unwrap();
    let dir = t.path().join("exports");
    std::fs::create_dir_all(&dir).unwrap();
    // The old fixed temporary name, and the destination itself, as links.
    std::os::unix::fs::symlink(&victim, dir.join(".photo.jpg.shoebox-export")).unwrap();
    std::os::unix::fs::symlink(&victim, dir.join("photo.jpg")).unwrap();
    let out = write_replacing(&dir, "photo", "jpg", b"new").unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert!(
        !std::fs::symlink_metadata(&out)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"new");
}
