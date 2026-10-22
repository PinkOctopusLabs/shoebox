//! Sky and landscape parts: the scene finder's coarse probabilities are
//! upsampled along the picture's edges, land on the stored image as the
//! other AI parts do, are found once per part and kept on disk; edits
//! read and write them, and a part this build doesn't know is kept.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::{
    LandscapePart, Local, LocalAdjust, Mask, MaskPart, Op, Part, Settings, Shape,
};
use shoebox_develop::mask::{self, Coarse, store};

/// Searches run.
static FINDS: AtomicUsize = AtomicUsize::new(0);
/// One test at a time (they share the finder, the store and the count).
static ONE: Mutex<()> = Mutex::new(());

/// The coarse grid's cell, in picture pixels.
const CELL: usize = 8;

fn blue(p: &[u8]) -> bool {
    p[2] > 150 && p[1] < 180
}

fn green(p: &[u8]) -> bool {
    p[1] > 90 && p[2] < 90
}

/// A fake scene model: in each 8×8 cell, the share of blue pixels is sky
/// and of green ones vegetation; nothing else.
fn register() {
    mask::register_scene_finder(
        "fake-scene/1",
        Box::new(|rgb, w, h, part| {
            FINDS.fetch_add(1, Ordering::SeqCst);
            let test: fn(&[u8]) -> bool = match part {
                LandscapePart::Sky => blue,
                LandscapePart::Vegetation => green,
                _ => |_| false,
            };
            let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
            let mut data = vec![0f32; cw * ch];
            for (i, d) in data.iter_mut().enumerate() {
                let (cx, cy) = (i % cw, i / cw);
                let (mut hit, mut n) = (0.0, 0.0);
                for y in cy * CELL..((cy + 1) * CELL).min(h) {
                    for x in cx * CELL..((cx + 1) * CELL).min(w) {
                        n += 1.0;
                        if test(&rgb[(y * w + x) * 3..][..3]) {
                            hit += 1.0;
                        }
                    }
                }
                *d = hit / n;
            }
            Ok(Coarse {
                width: cw,
                height: ch,
                data,
            })
        }),
    );
}

/// 160×96: blue sky over the top 37 rows of the stored image, green
/// below.
fn source() -> Demosaiced {
    let (w, h) = (160, 96);
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = if y < 37 {
                [0.12, 0.3, 0.85]
            } else {
                [0.08, 0.35, 0.04]
            };
            img.data[(y * w + x) * 3..][..3].copy_from_slice(&p);
        }
    }
    Demosaiced::new(img, [1.0; 3], Referred::Display)
}

fn with(part: LandscapePart) -> Settings {
    Settings {
        locals: vec![Local {
            id: "l1".into(),
            mask: Mask {
                parts: vec![Part::Known(MaskPart {
                    op: Op::Add,
                    invert: false,
                    shape: Shape::Landscape { part },
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

fn fresh() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    register();
    let dir = tempfile::tempdir().unwrap();
    store::set(Some(store::Store::new(
        &dir.path().join("ai-masks"),
        store::DEFAULT_LIMIT,
    )));
    (one, dir)
}

/// The edge, sharp where the coarse grid has it blurred over a cell or
/// two: rows either side of it are on and off, and only the rows at it
/// are in between.
#[test]
fn edges_follow_the_picture() {
    let (w, h, edge) = (200, 120, 45);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            // A little texture, as photos have.
            let n = ((x * 7 + y * 13) % 9) as u8;
            rgb.extend(if y < edge {
                [40 + n, 90 + n, 220]
            } else {
                [30 + n, 120 + n, 25]
            });
        }
    }
    let (cw, ch) = (w / CELL, h / CELL);
    let data: Vec<f32> = (0..cw * ch)
        .map(|i| {
            let cy = i / cw;
            let (y0, y1) = (cy * CELL, (cy + 1) * CELL);
            (edge.clamp(y0, y1) - y0) as f32 / CELL as f32
        })
        .collect();
    let c = Coarse {
        width: cw,
        height: ch,
        data,
    };
    let m = mask::landscape(&rgb, w, h, &c).unwrap();
    assert_eq!(m.len(), w * h);
    let row = |y: usize| {
        m[y * w..(y + 1) * w]
            .iter()
            .map(|&v| f32::from(v))
            .sum::<f32>()
            / (w as f32 * 255.0)
    };
    for y in 0..h {
        let v = row(y);
        if y + 2 <= edge {
            assert!(v > 0.9, "row {y}: {v}");
        } else if y >= edge + 2 {
            assert!(v < 0.1, "row {y}: {v}");
        }
    }
    // Bilinear alone would have rows a cell away half on.
    assert!(row(edge - 4) > 0.95 && row(edge + 4) < 0.05);
    // No finer than its grid: read bilinearly.
    let small = mask::landscape(&rgb[..4 * 3 * 3], 4, 3, &c).unwrap();
    assert_eq!(small.len(), 12);
    assert!(
        mask::landscape(
            &rgb,
            w,
            h,
            &Coarse {
                width: 3,
                height: 3,
                data: vec![0.0; 2]
            }
        )
        .is_err()
    );
}

/// How long the upsampling takes on the masks' full grid from the scene
/// model's (`cargo test --release -p shoebox-develop --test landscape --
/// --ignored --nocapture`).
#[test]
#[ignore]
fn upsampling_time() {
    let (w, h) = (2560, 1707);
    let rgb: Vec<u8> = (0..w * h * 3)
        .map(|i| ((i * 2_654_435_761) >> 13) as u8)
        .collect();
    let c = Coarse {
        width: 240,
        height: 160,
        data: (0..240 * 160).map(|i| (i % 240) as f32 / 240.0).collect(),
    };
    let t = std::time::Instant::now();
    let m = mask::landscape(&rgb, w, h, &c).unwrap();
    eprintln!(
        "{w}×{h} from 240×160: {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    assert_eq!(m.len(), w * h);
}

#[test]
fn found_upright_and_put_back() {
    let (_one, _dir) = fresh();
    for orientation in [1u16, 6, 3, 8] {
        let o = Options {
            orientation,
            ..Default::default()
        };
        for (part, top, bottom) in [
            (LandscapePart::Sky, 1.0, 0.0),
            (LandscapePart::Vegetation, 0.0, 1.0),
            (LandscapePart::Water, 0.0, 0.0),
        ] {
            let f = cpu::frame(&raw(), &source(), &with(part), &o);
            let m = f.layers.masks.as_ref().unwrap();
            let (t, b) = (
                mask::sample(m, 0, 80.0, 10.0),
                mask::sample(m, 0, 80.0, 80.0),
            );
            assert!((t - top).abs() < 0.02, "{part:?} at {orientation}: top {t}");
            assert!(
                (b - bottom).abs() < 0.02,
                "{part:?} at {orientation}: bottom {b}"
            );
            // Near the edge (rows 36 and 38), already as the picture has it.
            let (a, c) = (
                mask::sample(m, 0, 80.0, 35.0),
                mask::sample(m, 0, 80.0, 39.0),
            );
            assert!(
                (a - top).abs() < 0.1 && (c - bottom).abs() < 0.1,
                "{part:?}: {a} {c}"
            );
        }
    }
}

#[test]
fn found_once_and_kept_on_disk() {
    let (_one, _dir) = fresh();
    let n = || FINDS.load(Ordering::SeqCst);
    let o = Options::default();
    let start = n();
    let src = source();
    let _ = cpu::frame(&raw(), &src, &with(LandscapePart::Sky), &o);
    assert_eq!(n(), start + 1);
    // Again on the same source: from memory; another part searches.
    let _ = cpu::frame(&raw(), &src, &with(LandscapePart::Sky), &o);
    assert_eq!(n(), start + 1);
    let _ = cpu::frame(&raw(), &src, &with(LandscapePart::Water), &o);
    assert_eq!(n(), start + 2);
    // A fresh source (the photo opened again): from disk.
    let f = cpu::frame(&raw(), &source(), &with(LandscapePart::Sky), &o);
    assert_eq!(n(), start + 2);
    let m = f.layers.masks.as_ref().unwrap();
    assert!(mask::sample(m, 0, 80.0, 10.0) > 0.98);
}

/// Larger than the masks' grid (an export, a 100% region): the part is
/// upsampled to the source's own pixels along its edges, as the other AI
/// parts are (`mask::fine`).
#[test]
fn big_sources_get_fine_landscape_masks() {
    let (_one, _dir) = fresh();
    let (w, h, edge) = (2700usize, 200usize, 77usize);
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = if y < edge {
                [0.12, 0.3, 0.85]
            } else {
                [0.08, 0.35, 0.04]
            };
            img.data[(y * w + x) * 3..][..3].copy_from_slice(&p);
        }
    }
    let src = Demosaiced::new(img, [1.0; 3], Referred::Display);
    let f = cpu::frame(&raw(), &src, &with(LandscapePart::Sky), &Options::default());
    let m = f.layers.masks.as_ref().unwrap();
    assert_eq!((m.width, m.height), (w, h), "the source's own pixels");
    for x in [10, 1350, 2690] {
        for y in 0..h {
            let v = mask::sample(m, 0, x as f32, y as f32);
            // Within a couple of pixels of the edge, and all but settled
            // by six.
            let want = if y < edge { 1.0 } else { 0.0 };
            let d = (y as f32 + 0.5 - edge as f32).abs();
            if d > 2.0 {
                assert!((v - want).abs() < 0.1, "{v} at {x}, {y}");
            }
            if d > 6.0 {
                assert!((v - want).abs() < 0.03, "{v} at {x}, {y}");
            }
        }
    }
}

#[test]
fn edits_read_and_write_landscape_parts() {
    let s = with(LandscapePart::Mountains);
    let json = serde_json::to_string(&s.locals[0].mask).unwrap();
    assert!(
        json.contains(r#""kind":"landscape","part":"mountains"}"#),
        "{json}"
    );
    let back: Mask = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s.locals[0].mask);
    // A part from a newer build is kept as it was, and covers nothing.
    let newer = r#"{"parts":[{"kind":"landscape","part":"snow"},{"kind":"landscape","part":"sky","op":"subtract"}]}"#;
    let m: Mask = serde_json::from_str(newer).unwrap();
    assert!(matches!(m.parts[0], Part::Other(_)));
    assert!(matches!(
        &m.parts[1],
        Part::Known(MaskPart {
            op: Op::Subtract,
            shape: Shape::Landscape {
                part: LandscapePart::Sky
            },
            ..
        })
    ));
    let again: serde_json::Value = serde_json::to_value(&m).unwrap();
    assert_eq!(again["parts"][0]["part"], "snow");
}
