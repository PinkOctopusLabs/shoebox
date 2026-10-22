//! Subject and person parts: the registered finder sees the picture
//! upright, its mask lands back on the stored image where it found things,
//! and in the background the live view gets it on a later frame.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::{Local, LocalAdjust, Mask, MaskPart, Op, Part, Settings, Shape};
use shoebox_develop::mask::{self, Find};

/// The sizes the finder was given.
static SEEN: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
/// One test at a time (they share the finder and what it saw).
static ONE: Mutex<()> = Mutex::new(());

fn register() {
    mask::register_finder(Box::new(|rgb, w, h, _what| {
        SEEN.lock().unwrap().push((w, h));
        // "Subject": the red pixels.
        Ok(rgb
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| if p[0] > 150 && p[1] < 80 { 255 } else { 0 })
            .collect())
    }));
}

/// 120×80, grey, with a red square at the stored image's top left.
fn source() -> Demosaiced {
    let mut img = Rgb::new(120, 80);
    for y in 0..80 {
        for x in 0..120 {
            let p = if x < 30 && y < 20 {
                [0.8, 0.02, 0.02]
            } else {
                [0.2; 3]
            };
            img.data[(y * 120 + x) * 3..][..3].copy_from_slice(&p);
        }
    }
    Demosaiced::new(img, [1.0; 3], Referred::Display)
}

fn subject() -> Settings {
    Settings {
        locals: vec![Local {
            id: "l1".into(),
            mask: Mask {
                parts: vec![Part::Known(MaskPart {
                    op: Op::Add,
                    invert: false,
                    shape: Shape::Subject,
                })],
            },
            adjust: LocalAdjust {
                exposure: 1.0,
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    }
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
fn found_upright_and_put_back() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    register();
    let s = subject();
    for orientation in [1u16, 6, 3, 8] {
        let src = source();
        let o = Options {
            orientation,
            ..Default::default()
        };
        SEEN.lock().unwrap().clear();
        let f = cpu::frame(&raw(), &src, &s, &o);
        let seen = SEEN.lock().unwrap().clone();
        let upright = if matches!(orientation, 5..=8) {
            (80, 120)
        } else {
            (120, 80)
        };
        assert_eq!(seen, vec![upright], "orientation {orientation}");
        let m = f.layers.masks.as_ref().unwrap();
        assert_eq!(mask::sample(m, 0, 10.0, 10.0), 1.0, "on the square");
        assert_eq!(mask::sample(m, 0, 80.0, 50.0), 0.0, "off it");
        // Asked again, it isn't searched again.
        let _ = cpu::frame(&raw(), &src, &s, &o);
        assert_eq!(SEEN.lock().unwrap().len(), 1);
    }
}

#[test]
fn in_the_background_it_arrives_later() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    register();
    let s = subject();
    let src = source();
    src.cache.set_background(true);
    let o = Options::default();
    let g = src.cache.found_generation();
    let first = cpu::frame(&raw(), &src, &s, &o);
    assert_eq!(
        mask::sample(first.layers.masks.as_ref().unwrap(), 0, 10.0, 10.0),
        0.0,
        "not yet"
    );
    let t = std::time::Instant::now();
    while src.cache.finding().0 {
        assert!(t.elapsed().as_secs() < 10);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(src.cache.found_generation() > g);
    let later = cpu::frame(&raw(), &src, &s, &o);
    assert_eq!(
        mask::sample(later.layers.masks.as_ref().unwrap(), 0, 10.0, 10.0),
        1.0
    );
    let _ = Find::Person;
}

#[test]
fn objects_are_found_where_clicked() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    // A disc of radius 6 around the first click, in the picture it's
    // given (upright).
    mask::register_segmenter(Box::new(|_rgb, w, h, clicks| {
        let (cx, cy, _) = clicks[0];
        Ok((0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                if (x - cx).hypot(y - cy) < 6.0 { 255 } else { 0 }
            })
            .collect())
    }));
    // A click at stored pixel (90, 60) of the 120×80 source.
    let at = [(90.0 + 0.5) / 120.0, (60.0 + 0.5) / 120.0];
    let s = Settings {
        locals: vec![Local {
            id: "l1".into(),
            mask: Mask {
                parts: vec![Part::Known(MaskPart {
                    op: Op::Add,
                    invert: false,
                    shape: Shape::Object {
                        clicks: vec![shoebox_develop::edit::ObjectClick { at, add: true }],
                    },
                })],
            },
            adjust: LocalAdjust {
                exposure: 1.0,
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    };
    for orientation in [1u16, 6, 8, 3] {
        let src = source();
        let o = Options {
            orientation,
            ..Default::default()
        };
        let f = cpu::frame(&raw(), &src, &s, &o);
        let m = f.layers.masks.as_ref().unwrap();
        assert_eq!(
            mask::sample(m, 0, 90.0, 60.0),
            1.0,
            "orientation {orientation}"
        );
        assert_eq!(
            mask::sample(m, 0, 30.0, 20.0),
            0.0,
            "orientation {orientation}"
        );
    }
}

/// Smart Lighting's Face: boxes found upright land on the stored image,
/// the largest first.
#[test]
fn faces_are_boxes_in_the_stored_image() {
    // A "face": the red pixels' box in the upright picture, after a
    // smaller one.
    mask::register_face_finder(Box::new(|rgb, w, _h| {
        let mut b = [f32::MAX, f32::MAX, 0.0f32, 0.0f32];
        for (i, p) in rgb.as_chunks::<3>().0.iter().enumerate() {
            if p[0] > 150 && p[1] < 80 {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                b = [
                    b[0].min(x),
                    b[1].min(y),
                    b[2].max(x + 1.0),
                    b[3].max(y + 1.0),
                ];
            }
        }
        Ok(vec![[1.0, 1.0, 3.0, 3.0], b])
    }));
    let s = Settings::default();
    for orientation in [1u16, 6, 3, 8] {
        let src = source();
        let o = Options {
            orientation,
            ..Default::default()
        };
        let f = cpu::frame(&raw(), &src, &s, &o);
        let faces = cpu::layers::faces(&src, &f.colour, &f.display, orientation).unwrap();
        assert_eq!(faces.len(), 2);
        let want = [0.0, 0.0, 30.0 / 120.0, 20.0 / 120.0];
        for (a, b) in faces[0].iter().zip(want) {
            assert!(
                (a - b).abs() < 1e-9,
                "orientation {orientation}: {:?}",
                faces[0]
            );
        }
    }
}
