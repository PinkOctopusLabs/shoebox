//! Export: sizes, Lanczos resizing, TIFF, PNG and JPEG files, borders and
//! watermarks, names, and never replacing a file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::Rgb;
use shoebox_develop::export::{
    FileFormat, OutputSize, Preset, Space, TiffCompression, encode, file_stem, finish, output_size,
    resize, starters, write_new, write_replacing,
};
use shoebox_develop::overlay::{Anchor, Mark, Watermark};

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
    // A 16-bit PNG: the same values, big-endian, and the profile.
    let p = Preset {
        format: FileFormat::Png,
        ..starters()[2].clone()
    };
    let bytes = encode(&img, &p).unwrap();
    let mut r = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .unwrap();
    let info = r.info();
    assert_eq!((info.width, info.height), (40, 30));
    assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
    assert!(info.icc_profile.as_ref().is_some_and(|v| v.len() > 100));
    let mut buf = vec![0; r.output_buffer_size().unwrap()];
    r.next_frame(&mut buf).unwrap();
    let px: Vec<u16> = buf
        .chunks(2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .collect();
    assert_eq!(px, shoebox_develop::cpu::to_u16(&img));
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

fn preset_at(px: u32) -> Preset {
    Preset {
        size: OutputSize::LongEdge { px },
        ..starters()[0].clone()
    }
}

#[test]
fn a_border_keeps_the_files_size() {
    use shoebox_develop::overlay::Border;
    let img = image(400, 300, |_, _| [0.2, 0.3, 0.4]);
    let p = Preset {
        border: Some(Border {
            width: 5.0,
            colour: [255, 0, 0],
        }),
        ..preset_at(200)
    };
    let out = finish(&img, &p, &[]).unwrap();
    // Still 200 long: 10 px of border (5%) all round, the photo (180×135)
    // inside.
    assert_eq!((out.width, out.height), (200, 155));
    let red = out.px(0, 0);
    assert!(red[0] > 0.99 && red[1] < 0.01 && red[2] < 0.01, "{red:?}");
    assert_eq!(out.px(5, 75), red);
    assert_eq!(out.px(100, 150), red);
    let mid = out.px(100, 75);
    assert!((mid[0] - 0.2).abs() < 1e-3, "{mid:?}");
    assert!(
        (out.px(10, 75)[0] - 0.2).abs() < 1e-3,
        "the photo starts at 10"
    );
    assert_eq!(out.px(9, 75), red);
    // In another space the colour is converted: sRGB red is inside Adobe
    // RGB, so not its full red.
    let adobe = Preset {
        space: Space::AdobeRgb,
        ..p
    };
    let r = finish(&img, &adobe, &[]).unwrap().px(0, 0);
    assert!(r[0] < 0.9 && r[1] < 0.05, "{r:?}");
}

#[test]
fn a_text_watermark_sits_in_its_corner() {
    let img = image(400, 300, |_, _| [0.0; 3]);
    let p = Preset {
        watermark: Some(Watermark {
            size: 8.0,
            opacity: 100.0,
            ..Watermark::text("© Shoebox")
        }),
        ..preset_at(400)
    };
    let out = finish(&img, &p, epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    let lit = |x0: usize, y0: usize, x1: usize, y1: usize| {
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| out.px(x, y)[0] > 0.5)
            .count()
    };
    // White text in the bottom right quarter, nothing elsewhere.
    assert!(lit(200, 150, 400, 300) > 100, "{}", lit(200, 150, 400, 300));
    assert_eq!(lit(0, 0, 400, 150), 0);
    assert_eq!(lit(0, 0, 200, 300), 0);
    // The margin (2% of 400) stays clear.
    assert_eq!(lit(392, 0, 400, 300), 0);
    // Half opacity: no pixel brighter than half.
    let half = Preset {
        watermark: Some(Watermark {
            size: 8.0,
            opacity: 50.0,
            ..Watermark::text("© Shoebox")
        }),
        ..preset_at(400)
    };
    let out = finish(&img, &half, epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    assert!(out.data.iter().all(|&v| v <= 0.5 + 1e-4));
    assert!(out.data.iter().any(|&v| v > 0.4));
}

#[test]
fn an_image_watermark_keeps_its_transparency() {
    let t = tempfile::tempdir().unwrap();
    let logo = t.path().join("logo.png");
    {
        // 20×10: the left half opaque green, the right half clear.
        let f = std::fs::File::create(&logo).unwrap();
        let mut e = png::Encoder::new(f, 20, 10);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut w = e.write_header().unwrap();
        let px: Vec<u8> = (0..10)
            .flat_map(|_| {
                (0..20).flat_map(|x| {
                    if x < 10 {
                        [0, 255, 0, 255]
                    } else {
                        [0, 0, 0, 0]
                    }
                })
            })
            .collect();
        w.write_image_data(&px).unwrap();
    }
    let img = image(400, 300, |_, _| [0.5; 3]);
    let p = Preset {
        watermark: Some(Watermark {
            mark: Mark::Image { path: logo },
            size: 10.0,
            opacity: 100.0,
            anchor: Anchor::TopLeft,
            margin: 0.0,
        }),
        ..preset_at(400)
    };
    let out = finish(&img, &p, &[]).unwrap();
    // 40×20 at the top left: green where the logo is opaque, the photo
    // where it's clear.
    let g = out.px(5, 5);
    assert!(g[1] > 0.99 && g[0] < 0.01, "{g:?}");
    assert_eq!(out.px(30, 5), [0.5; 3]);
    assert_eq!(out.px(5, 25), [0.5; 3]);
    // A missing logo fails the export rather than leaving it out quietly.
    let gone = Preset {
        watermark: Some(Watermark {
            mark: Mark::Image {
                path: t.path().join("missing.png"),
            },
            ..p.watermark.clone().unwrap()
        }),
        ..p
    };
    assert!(finish(&img, &gone, &[]).is_err());
}
