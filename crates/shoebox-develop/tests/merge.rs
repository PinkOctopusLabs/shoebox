//! Photo merges: alignment, the HDR merge and its float DNG, on synthetic
//! brackets (`shoebox_fixtures::Bracket`) whose light is known everywhere.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_range_loop)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use shoebox_develop::cpu;
use shoebox_develop::merge::align::{self, Gray, Model};
use shoebox_develop::merge::hdr::{self, Deghost, ExposureFrom};
use shoebox_develop::merge::{self, Input, MergeError, Record};
use shoebox_fixtures::{Bracket, Dng, hdr_light};

const W: u32 = 480;
const H: u32 = 320;

/// A gray image of what a frame sees (square root of the light, as merges
/// encode it), optionally with noise.
fn gray(b: &Bracket, w: usize, h: usize) -> Gray {
    let mut g = Gray::new(w, h);
    for y in 0..h {
        for x in 0..w {
            g.data[y * w + x] = b.light(x as f64, y as f64).sqrt() as f32;
        }
    }
    g
}

/// The largest distance between where `h` and the truth put a grid of
/// reference points (pixels).
fn worst(h: &shoebox_develop::colour::M3, b: &Bracket, w: usize, ht: usize) -> f64 {
    let mut worst = 0.0f64;
    for j in 0..=8 {
        for i in 0..=8 {
            let (x, y) = (
                i as f64 * (w - 1) as f64 / 8.0,
                j as f64 * (ht - 1) as f64 / 8.0,
            );
            let (p, q) = (align::apply(h, x, y), b.to_frame(x, y));
            worst = worst.max((p.0 - q.0).hypot(p.1 - q.1));
        }
    }
    worst
}

#[test]
fn alignment_finds_a_shift_and_a_turn_to_a_tenth_of_a_pixel() {
    let (w, h) = (W as usize, H as usize);
    let reference = Bracket::new(W, H, 0.0);
    let pr = align::pyramid(gray(&reference, w, h), 64);
    for (model, shift, rotate) in [
        (Model::Homography, (3.4, -2.2), 0.3),
        (Model::Similarity, (-5.25, 1.75), -0.6),
        (Model::Translation, (7.6, 4.1), 0.0),
    ] {
        let moved = Bracket {
            shift,
            rotate,
            ..Bracket::new(W, H, 0.0)
        };
        let pm = align::pyramid(gray(&moved, w, h), 64);
        let o = align::Options {
            model,
            ..Default::default()
        };
        let a = align::align(&pr, &pm, &o).unwrap();
        assert!(a.inliers >= 30, "{model:?}: {} inliers", a.inliers);
        assert!(a.refined.is_some(), "{model:?}: not refined");
        let err = worst(&a.h, &moved, w, h);
        assert!(err < 0.1, "{model:?}: {err:.3} px off");
        // Reference → frame turns the other way.
        let (deg, ..) = hdr::motion(&a.h, w, h);
        assert!(
            (deg + rotate).abs() < 0.01,
            "{model:?}: {deg}° for {rotate}°"
        );
    }
}

#[test]
fn alignment_follows_focus_breathing() {
    // A 1.5% scale change about the centre, a little shift: a similarity.
    let (w, h) = (W as usize, H as usize);
    let reference = Bracket::new(W, H, 0.0);
    let s = 1.015;
    let (cx, cy) = ((w - 1) as f64 / 2.0, (h - 1) as f64 / 2.0);
    let mut g = Gray::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (
                (x as f64 - cx) / s + cx + 1.3,
                (y as f64 - cy) / s + cy - 0.8,
            );
            g.data[y * w + x] = hdr_light(sx, sy, W as f64, H as f64).sqrt() as f32;
        }
    }
    let pr = align::pyramid(gray(&reference, w, h), 64);
    let pm = align::pyramid(g, 64);
    let o = align::Options {
        model: Model::Similarity,
        ..Default::default()
    };
    let a = align::align(&pr, &pm, &o).unwrap();
    let mut err = 0.0f64;
    for (x, y) in [
        (0.0, 0.0),
        (w as f64 - 1.0, h as f64 - 1.0),
        (cx, cy),
        (0.0, h as f64 - 1.0),
    ] {
        let p = align::apply(&a.h, x, y);
        let q = ((x - cx - 1.3) * s + cx, (y - cy + 0.8) * s + cy);
        err = err.max((p.0 - q.0).hypot(p.1 - q.1));
    }
    assert!(err < 0.1, "{err:.3} px off");
}

#[test]
fn features_match_across_a_panorama_overlap() {
    // Two views of a wider scene overlapping by 40%, the second turned a
    // little: matched anywhere (no "near"), as panorama will.
    let (cw, ch) = (900.0, 400.0);
    let (w, h) = (560usize, 400usize);
    let mut left = Gray::new(w, h);
    let mut right = Gray::new(w, h);
    let turn = 0.8f64.to_radians();
    for y in 0..h {
        for x in 0..w {
            let (xf, yf) = (x as f64, y as f64);
            left.data[y * w + x] = hdr_light(xf, yf, cw, ch).sqrt() as f32;
            let (dx, dy) = (xf - 280.0, yf - 200.0);
            let (sx, sy) = (
                turn.cos() * dx - turn.sin() * dy + 280.0 + 340.0,
                turn.sin() * dx + turn.cos() * dy + 200.0,
            );
            right.data[y * w + x] = hdr_light(sx, sy, cw, ch).sqrt() as f32;
        }
    }
    let a = align::features(&left, 1500);
    let b = align::features(&right, 1500);
    let pairs = align::matches(&a, &b, None);
    let (hm, inliers) = align::ransac(Model::Homography, &pairs, 1.5).unwrap();
    let n = inliers.iter().filter(|v| **v).count();
    assert!(n >= 40, "{n} of {} matches agree", pairs.len());
    // A point of the overlap in the left view, and where it is in the right.
    let (x, y) = (450.0, 180.0);
    let (dx, dy) = (x - 340.0 - 280.0, y - 200.0);
    let want = (
        turn.cos() * dx + turn.sin() * dy + 280.0,
        -turn.sin() * dx + turn.cos() * dy + 200.0,
    );
    let got = align::apply(&hm, x, y);
    assert!(
        (got.0 - want.0).hypot(got.1 - want.1) < 0.5,
        "{got:?} for {want:?}"
    );
}

/// Write a bracket to `dir`: −2, 0 and +2 EV, the −2 frame moved and turned
/// a little, the +2 one more, and (with `mover`) something passing through
/// the +2 frame only.
fn bracket(dir: &Path, mover: bool) -> (Vec<Input>, Vec<Bracket>) {
    let frames = vec![
        Bracket {
            shift: (-1.6, 1.1),
            rotate: -0.15,
            seed: 11,
            subsec: "10",
            ..Bracket::new(W, H, -2.0)
        },
        Bracket {
            seed: 12,
            subsec: "30",
            ..Bracket::new(W, H, 0.0)
        },
        Bracket {
            shift: (3.4, -2.2),
            rotate: 0.3,
            seed: 13,
            subsec: "50",
            mover: mover.then_some(([60.0, 200.0, 140.0, 260.0], 0.012)),
            ..Bracket::new(W, H, 2.0)
        },
    ];
    let inputs = frames
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let p = dir.join(format!("DSC_00{}.dng", 41 + i));
            std::fs::write(&p, b.write()).unwrap();
            Input::new(p, 1)
        })
        .collect();
    (inputs, frames)
}

/// The mean of the merged green over a block, against the scene's light
/// there (the reference frame doesn't move, so its pixels are the scene's).
fn block_error(rgb: &cpu::Rgb, x0: usize, y0: usize, n: usize) -> (f64, f64) {
    let (mut got, mut want) = (0.0, 0.0);
    for y in y0..y0 + n {
        for x in x0..x0 + n {
            got += f64::from(rgb.px(x, y)[1]);
            want += hdr_light(x as f64, y as f64, W as f64, H as f64);
        }
    }
    let k = (n * n) as f64;
    (got / k, want / k)
}

#[test]
fn an_hdr_merge_recovers_the_scene() {
    let t = tempfile::tempdir().unwrap();
    let (inputs, frames) = bracket(t.path(), true);
    let cancel = AtomicBool::new(false);
    let o = hdr::Options::default();
    let out = hdr::merge(&inputs, &o, &|_| {}, &cancel).unwrap();
    // The 0 EV frame is the reference (the middle exposure).
    assert_eq!(out.merged.reference, 1);
    let rgb = &out.merged.rgb;
    assert_eq!((rgb.width, rgb.height), (W as usize, H as usize));
    // Exposures measured: a quarter and four times the reference's.
    for (k, want) in [(0, 0.25), (2, 4.0)] {
        let f = &out.frames[k];
        assert_eq!(f.exposure_from, ExposureFrom::Measured);
        assert!(
            (f.exposure / want - 1.0).abs() < 0.03,
            "{k}: {}",
            f.exposure
        );
        assert!((f.exif_exposure.unwrap() / want - 1.0).abs() < 1e-3);
        // Aligned to the truth.
        let a = f.alignment.expect("aligned");
        let err = worst(&a.h, &frames[k], W as usize, H as usize);
        assert!(err < 0.1, "frame {k}: {err:.3} px");
        let (deg, ..) = hdr::motion(&a.h, W as usize, H as usize);
        assert!((deg + frames[k].rotate).abs() < 0.01, "frame {k}: {deg}°");
    }
    // The light, block by block, in the room, the dark corner and the
    // window (above the sensor's white: only −2 EV held it).
    let mut beyond_white = 0;
    for (x0, y0, what, tol) in [
        (40, 40, "room", 0.03),
        (200, 230, "room", 0.03),
        (360, 250, "room", 0.03),
        (20, 260, "dark corner", 0.06),
        (290, 60, "window", 0.03),
        (370, 140, "window", 0.03),
        (300, 150, "window", 0.03),
    ] {
        let (got, want) = block_error(rgb, x0, y0, 12);
        assert!(
            (got / want - 1.0).abs() < tol,
            "{what} at ({x0}, {y0}): {got:.4} for {want:.4}"
        );
        if want > 1.0 {
            beyond_white += 1;
        }
    }
    assert!(beyond_white >= 3);
    // The +2 frame's passer-by is gone: the room behind it, from the
    // reference.
    assert!(out.frames[2].ghosted > 0.0);
    let (got, want) = block_error(rgb, 85, 215, 30);
    assert!(
        (got / want - 1.0).abs() < 0.04,
        "ghost: {got:.4} for {want:.4}"
    );

    // Without deghosting the merge would show it (the +2 frame counts most
    // in the room).
    let off = hdr::Options {
        deghost: Deghost::Off,
        ..o
    };
    let out = hdr::merge(&inputs, &off, &|_| {}, &cancel).unwrap();
    let (got, want) = block_error(&out.merged.rgb, 85, 215, 30);
    assert!(
        got / want < 0.6,
        "no ghost without deghosting: {got:.4} for {want:.4}"
    );
}

#[test]
fn a_tripod_bracket_merges_without_alignment() {
    let t = tempfile::tempdir().unwrap();
    let frames = [-2.0, 0.0, 2.0].map(|ev| Bracket {
        seed: (ev as i64 + 10) as u64,
        ..Bracket::new(W, H, ev)
    });
    let inputs: Vec<Input> = frames
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let p = t.path().join(format!("T{i}.dng"));
            std::fs::write(&p, b.write()).unwrap();
            Input::new(p, 1)
        })
        .collect();
    let o = hdr::Options {
        align: false,
        deghost: Deghost::Off,
        reference: Some(1),
    };
    let out = hdr::merge(&inputs, &o, &|_| {}, &AtomicBool::new(false)).unwrap();
    assert!(out.frames.iter().all(|f| f.alignment.is_none()));
    let (got, want) = block_error(&out.merged.rgb, 300, 150, 16);
    assert!((got / want - 1.0).abs() < 0.03, "{got} for {want}");
    // Stopped part-way: nothing.
    let stop = AtomicBool::new(true);
    assert!(matches!(
        hdr::merge(&inputs, &o, &|_| {}, &stop),
        Err(MergeError::Cancelled)
    ));
}

#[test]
fn merges_refuse_what_they_cannot_merge() {
    let t = tempfile::tempdir().unwrap();
    let raw = t.path().join("a.dng");
    std::fs::write(&raw, Bracket::new(64, 48, 0.0).write()).unwrap();
    let small = t.path().join("b.dng");
    std::fs::write(&small, Bracket::new(48, 32, 2.0).write()).unwrap();
    let jpg = t.path().join("c.jpg");
    let img = shoebox_fixtures::scene(1, 64, 48);
    std::fs::write(&jpg, shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
    let cancel = AtomicBool::new(false);
    let run = |paths: &[&PathBuf]| {
        let inputs: Vec<Input> = paths.iter().map(|p| Input::new(*p, 1)).collect();
        hdr::merge(&inputs, &hdr::Options::default(), &|_| {}, &cancel)
    };
    assert!(matches!(run(&[&raw]), Err(MergeError::Refused(_))));
    assert!(matches!(run(&[&raw, &jpg]), Err(MergeError::Refused(_))));
    assert!(matches!(run(&[&raw, &small]), Err(MergeError::Refused(_))));
}

/// A merged result without running a merge: a fixture's colour and EXIF,
/// and an image with a band of light four times the sensor's white.
fn merged(dir: &Path) -> merge::Merged {
    let p = dir.join("ref.dng");
    let b = Bracket::new(96, 64, 0.0);
    std::fs::write(&p, b.write()).unwrap();
    let s = merge::load(&Input::new(&p, 1), &AtomicBool::new(false)).unwrap();
    let mut rgb = cpu::Rgb::new(96, 64);
    let n = Dng::new(1, 1).neutral;
    for y in 0..64 {
        for x in 0..96 {
            let light = if x >= 64 {
                4.0
            } else {
                0.05 + x as f32 * 0.004
            };
            for c in 0..3 {
                rgb.data[(y * 96 + x) * 3 + c] = light * n[c];
            }
        }
    }
    merge::Merged {
        rgb,
        reference: 0,
        meta: s.meta,
        as_shot: s.as_shot,
        metadata: s.metadata,
    }
}

fn record() -> Record {
    Record {
        kind: "hdr".into(),
        sources: vec![
            ("DSC_0041.NEF".into(), "ab".repeat(32)),
            ("DSC_0042.NEF".into(), "cd".repeat(32)),
        ],
    }
}

#[test]
fn a_float_dng_keeps_light_beyond_white() {
    let t = tempfile::tempdir().unwrap();
    let m = merged(t.path());
    let bytes = merge::dng(&m, &record()).unwrap();
    // rawler: LinearRaw, three samples, white 1.0, floats.
    let back = shoebox_decode::raw::decode(&bytes).unwrap();
    assert!(back.linear.is_some());
    assert_eq!((back.width, back.height), (96, 64));
    for (i, (a, b)) in back.data.iter().zip(&m.rgb.data).enumerate() {
        // Half floats: 11 significant bits.
        assert!((a - b).abs() <= b.abs() * 1e-3 + 1e-6, "{i}: {a} for {b}");
    }
    let hi = back.data.iter().copied().fold(0.0f32, f32::max);
    assert!((hi - 4.0).abs() < 0.01, "{hi}");
    // The reference's colour.
    assert_eq!(back.matrices.len(), m.meta.matrices.len());
    for c in 0..3 {
        let want = m.as_shot[c];
        assert!((back.as_shot.unwrap()[c] - want).abs() < 1e-3);
    }
    // Previews for Finder and the library: 256 in IFD 0, a 1024-or-less
    // JPEG in a SubIFD.
    let previews = shoebox_decode::dng::previews(&bytes).unwrap();
    assert_eq!(previews.len(), 2, "{previews:?}");
    // EXIF from the reference, without sub-seconds: the merge doesn't
    // stack with its sources by capture time.
    let md = merge::metadata(&rawler::rawsource::RawSource::new_from_slice(&bytes)).unwrap();
    assert_eq!(
        md.exif.date_time_original.as_deref(),
        Some("2026:10:22 14:30:00")
    );
    assert!(md.exif.exposure_time.is_some() && md.exif.lens_model.is_some());
    assert!(md.exif.sub_sec_time_original.is_none());
    // The record, in XMP.
    let xmp = String::from_utf8_lossy(&bytes);
    assert!(xmp.contains(merge::XMP_NS));
    assert!(xmp.contains("<shoebox:File>DSC_0042.NEF</shoebox:File>"));
    assert!(xmp.contains("shoebox:Merge=\"hdr\""));
}

#[test]
fn develop_renders_an_hdr_dng_and_auto_brings_it_down() {
    let t = tempfile::tempdir().unwrap();
    let m = merged(t.path());
    let bytes = merge::dng(&m, &record()).unwrap();
    let raw = shoebox_decode::raw::decode(&bytes).unwrap();
    let s = shoebox_develop::edit::Settings::default();
    let src = cpu::demosaic(&raw, &s.modules.raw, None);
    // Past the sensor's white, kept.
    let hi = src.rgb.data.iter().copied().fold(0.0f32, f32::max);
    assert!(hi > 2.0, "{hi}");
    // The display transform rolls it off: below white, brighter than the
    // room.
    let o = cpu::Options::default();
    let img = cpu::render_from(&raw, &src, &s, &o, true);
    let window = img.px(80, 30);
    let room = img.px(30, 30);
    assert!(
        window[1] > room[1] && window[1] < 1.0,
        "{window:?} {room:?}"
    );
    // Auto measures the highlights and turns exposure down for them, within
    // its two stops.
    let stats = shoebox_develop::auto::measure(&raw, &src, None);
    let auto = shoebox_develop::auto::settings(&s, &stats, Some(400), false, None);
    let ev = auto.modules.exposure.ev;
    assert!((-2.0..0.0).contains(&ev), "{ev}");
}

/// macOS's raw engine reads the float DNG from its raw data.
#[cfg(target_os = "macos")]
#[test]
fn imageio_renders_a_float_dng() {
    let t = tempfile::tempdir().unwrap();
    let m = merged(t.path());
    let bytes = merge::dng(&m, &record()).unwrap();
    let img = shoebox_decode::imageio::thumbnail_rgb(
        &bytes,
        4096,
        false,
        Some("com.adobe.raw-image"),
        shoebox_decode::Target::Srgb,
    )
    .unwrap();
    // Not the 96 px... the raw at full size.
    assert_eq!((img.width, img.height), (96, 64));
    let at = |x: usize, y: usize| {
        let i = (y * 96 + x) * 3;
        [0, 1, 2].map(|c| i32::from(img.px[i + c]))
    };
    let room = at(30, 30);
    let window = at(80, 30);
    assert!(
        room.iter().max().unwrap() - room.iter().min().unwrap() <= 12,
        "grey stays grey: {room:?}"
    );
    assert!(window[1] > room[1], "{window:?} {room:?}");
}
