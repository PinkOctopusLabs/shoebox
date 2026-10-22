//! Object removal with a stand-in model (each row of the hole takes its
//! row's mean of what's known, so a vertical gradient fills true): what
//! the strokes paint is filled, nothing else changes, the feather blends,
//! fills are kept in memory and on disk, the live view gets them on a
//! thread of its own, the proxy and full size fill alike, and edits keep
//! the strokes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use shoebox_develop::cpu::{Demosaiced, Referred, Rgb};
use shoebox_develop::edit::{EditDoc, Settings, Source, Spot, SpotKind, Stroke};
use shoebox_develop::heal::{self, remove};

const SIZE: usize = 64;
static RUNS: AtomicUsize = AtomicUsize::new(0);
/// One test at a time: the model, its count and the store are shared.
static SERIAL: Mutex<()> = Mutex::new(());
static SETUP: Once = Once::new();

fn setup() -> std::sync::MutexGuard<'static, ()> {
    SETUP.call_once(|| {
        remove::register(
            "row-means/1",
            SIZE,
            Box::new(|rgb, known| {
                RUNS.fetch_add(1, Ordering::SeqCst);
                let mut out = rgb.to_vec();
                for y in 0..SIZE {
                    let (mut sum, mut n) = ([0f32; 3], 0.0);
                    for x in 0..SIZE {
                        if known[y * SIZE + x] {
                            for c in 0..3 {
                                sum[c] += rgb[(y * SIZE + x) * 3 + c];
                            }
                            n += 1.0;
                        }
                    }
                    for x in 0..SIZE {
                        if !known[y * SIZE + x] {
                            for c in 0..3 {
                                out[(y * SIZE + x) * 3 + c] = sum[c] / f32::max(n, 1.0);
                            }
                        }
                    }
                }
                Ok(out)
            }),
        );
        // Kept for the whole run (never removed: a test's temporary
        // folder).
        let dir = std::env::temp_dir().join(format!("shoebox-remove-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        remove::set_store(Some(shoebox_develop::mask::store::Store::new(
            &dir,
            remove::DEFAULT_LIMIT,
        )));
    });
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Brightening downwards (0.1 to 0.7, linear), a dark post standing in
/// the middle (40% of the width across, a tenth wide, half the height).
fn street(w: usize, h: usize) -> Rgb {
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = 0.1 + 0.6 * y as f32 / h as f32;
            let post = (x as f32 / w as f32 - 0.4).abs() < 0.05
                && (y as f32 / h as f32 - 0.5).abs() < 0.25;
            let p = if post {
                [0.02, 0.02, 0.03]
            } else {
                [v, v * 0.9, v * 0.8]
            };
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&p);
        }
    }
    img
}

/// A removal painted down the post (of a `w`×`h` image), in units.
fn removal(w: usize, h: usize) -> Spot {
    let long = w.max(h) as f64;
    let (x, y0, y1) = (
        0.4 * w as f64 / long,
        0.22 * h as f64 / long,
        0.78 * h as f64 / long,
    );
    let strokes = vec![Stroke {
        points: (0..=10)
            .map(|k| [x, y0 + (y1 - y0) * f64::from(k) / 10.0])
            .collect(),
        radius: 0.06 * w as f64 / long,
        feather: 0.5,
        flow: 1.0,
        erase: false,
    }];
    Spot {
        kind: SpotKind::Remove,
        dest: remove::middle(&strokes).unwrap(),
        radius: 0.06 * w as f64 / long,
        feather: 0.3,
        strokes,
        ..Default::default()
    }
}

fn want(img: &Rgb, y: usize) -> f32 {
    0.1 + 0.6 * y as f32 / img.height as f32
}

#[test]
fn the_object_goes_and_nothing_else_changes() {
    let _g = setup();
    let img = street(300, 200);
    let spot = removal(300, 200);
    let out = heal::heal(&img, std::slice::from_ref(&spot));
    // The post is gone: each row as the street around it.
    for y in [60, 100, 140] {
        for x in [114, 120, 126] {
            let v = out.px(x, y)[0];
            assert!((v - want(&img, y)).abs() < 0.03, "({x}, {y}): {v}");
        }
    }
    // Away from the strokes (and the feather) nothing changes.
    for (x, y) in [(10, 10), (290, 190), (170, 100), (60, 100), (120, 20)] {
        assert_eq!(out.px(x, y), img.px(x, y), "({x}, {y})");
    }
    // The feather blends: between the post and the street, partly each.
    let r = spot.strokes[0].radius * 300.0;
    let edge = (120.0 + r + 0.15 * r) as usize;
    let (a, b) = (out.px(edge, 100)[0], img.px(edge, 100)[0]);
    assert_eq!(b, want(&img, 100), "the street there");
    assert!((a - b).abs() < 0.03, "no seam: {a} {b}");
}

#[test]
fn fills_are_kept_in_memory_and_on_disk() {
    let _g = setup();
    let img = street(300, 200);
    let src = Demosaiced::new(img.clone(), [1.0; 3], Referred::Display);
    let mut s = Settings::default();
    s.modules.heal.spots = vec![removal(300, 200)];
    let before = RUNS.load(Ordering::SeqCst);
    let a = src.prepared(&s).unwrap();
    let ran = RUNS.load(Ordering::SeqCst) - before;
    assert!(
        ran <= 1,
        "once at most (none if another test ran it): {ran}"
    );
    assert!(a.px(120, 100)[0] > 0.3, "filled");
    // Other sliders: the same result.
    s.modules.exposure.ev = 1.0;
    assert!(Arc::ptr_eq(&a, &src.prepared(&s).unwrap()));
    // Opacity: blended again, the model not asked.
    let n = RUNS.load(Ordering::SeqCst);
    s.modules.heal.spots[0].opacity = 50.0;
    let half = src.prepared(&s).unwrap();
    assert_eq!(RUNS.load(Ordering::SeqCst), n);
    let v = half.px(120, 100)[0];
    assert!((v - (0.02 + a.px(120, 100)[0]) * 0.5).abs() < 0.01, "{v}");
    // A new source of the same picture (reopened): from disk.
    let again = Demosaiced::new(img, [1.0; 3], Referred::Display);
    let b = again.prepared(&s).unwrap();
    assert_eq!(RUNS.load(Ordering::SeqCst), n, "read from disk");
    let most = b
        .data
        .iter()
        .zip(&half.data)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    assert!(most < 1e-6, "the same fill: {most}");
    // A different removal: asked.
    s.modules.heal.spots[0].strokes[0].radius *= 1.3;
    again.prepared(&s).unwrap();
    assert_eq!(RUNS.load(Ordering::SeqCst), n + 1);
}

#[test]
fn the_live_view_gets_fills_on_a_thread() {
    let _g = setup();
    // A picture of its own (so nothing's kept for it yet).
    let mut img = street(320, 200);
    img.data.iter_mut().for_each(|v| *v *= 0.9);
    let src = Demosaiced::new(img, [1.0; 3], Referred::Display);
    src.cache.set_background(true);
    let mut s = Settings::default();
    s.modules.heal.spots = vec![removal(320, 200)];
    let first = src.prepared(&s).unwrap();
    assert!(first.px(128, 100)[0] < 0.05, "not yet: the post");
    let t = Instant::now();
    while src.cache.removing().0 {
        assert!(t.elapsed() < Duration::from_secs(20), "the fill arrives");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(src.cache.removing(), (false, None));
    let filled = src.prepared(&s).unwrap();
    assert!(!Arc::ptr_eq(&first, &filled));
    assert!(filled.px(128, 100)[0] > 0.3, "filled");
    // The images beside the source reuse it (no second fill).
    assert!(src.cache.fill_used(0).is_some());
}

#[test]
fn the_proxy_and_full_size_fill_alike() {
    let _g = setup();
    let (small, big) = (street(400, 300), street(1000, 750));
    let a = heal::heal(&small, &[removal(400, 300)]);
    let b = heal::heal(&big, &[removal(1000, 750)]);
    // The same place in each.
    for (fx, fy) in [(0.4, 0.4), (0.41, 0.5), (0.39, 0.6)] {
        let pa = a.px((fx * 400.0) as usize, (fy * 300.0) as usize)[0];
        let pb = b.px((fx * 1000.0) as usize, (fy * 750.0) as usize)[0];
        assert!((pa - pb).abs() < 0.02, "({fx}, {fy}): {pa} {pb}");
    }
    // The model saw the same part of the picture.
    let (ja, jb) = (
        remove::job(&small, &removal(400, 300)).unwrap(),
        remove::job(&big, &removal(1000, 750)).unwrap(),
    );
    assert_eq!(ja.known, jb.known);
    let d = ja
        .input
        .iter()
        .zip(&jb.input)
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    assert!(d < 0.02, "{d}");
}

#[test]
fn without_a_model_or_strokes_nothing_happens() {
    let _g = setup();
    let img = street(300, 200);
    let mut spot = removal(300, 200);
    spot.strokes.clear();
    assert!(remove::job(&img, &spot).is_none());
    assert_eq!(heal::heal(&img, &[spot]).data, img.data);
    // Strokes that only erase paint nothing.
    let mut spot = removal(300, 200);
    spot.strokes[0].erase = true;
    assert!(remove::job(&img, &spot).is_none());
}

#[test]
fn edits_keep_the_strokes() {
    let mut d = EditDoc::new(Source {
        path: "a.jpg".into(),
        blake3: String::new(),
        size: None,
        camera: None,
        lens: None,
    });
    let spot = removal(300, 200);
    assert!(d.change("Remove object", |s| s.modules.heal.spots.push(spot.clone())));
    let v: serde_json::Value = serde_json::from_str(&d.to_json()).unwrap();
    let saved = &v["modules"]["heal"]["spots"][0];
    assert_eq!(saved["kind"], "remove");
    assert_eq!(saved["strokes"][0]["points"].as_array().unwrap().len(), 11);
    let back = EditDoc::from_json(&d.to_json()).unwrap();
    assert_eq!(back.settings().modules.heal.spots, vec![spot]);
    // Other spots don't write strokes.
    let heal = serde_json::to_value(Spot::default()).unwrap();
    assert!(heal.get("strokes").is_none());
}

#[test]
fn a_smooth_fill_gets_the_photos_grain() {
    let _g = setup();
    // The street with noise (σ about 0.02), the same everywhere.
    let mut img = street(300, 200);
    let mut seed = 99u64;
    for v in img.data.iter_mut() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *v += ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.07;
    }
    let out = heal::heal(&img, &[removal(300, 200)]);
    // Each pixel less its 3×3 mean, its spread over a box.
    let detail = |x0: usize, y0: usize, x1: usize, y1: usize| {
        let (mut s, mut n) = (0.0, 0.0);
        for y in y0..y1 {
            for x in x0..x1 {
                let mut m = 0.0;
                for k in 0..9 {
                    m += out.px(x + k % 3 - 1, y + k / 3 - 1)[1] / 9.0;
                }
                s += (out.px(x, y)[1] - m).powi(2);
                n += 1.0;
            }
        }
        (s / n).sqrt()
    };
    let inside = detail(114, 70, 126, 130);
    let outside = detail(30, 70, 60, 130);
    eprintln!("detail inside {inside:.4}, outside {outside:.4}");
    assert!(
        inside > 0.6 * outside && inside < 1.4 * outside,
        "{inside} against {outside}"
    );
}
