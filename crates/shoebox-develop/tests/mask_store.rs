//! AI masks kept on disk (`mask::store`): a fresh source (opening the
//! photo again, or a restart) reads the mask instead of searching; any
//! change to what the finder would see searches again; the least recently
//! used go past the limit; a damaged file is ignored and replaced.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::{
    Local, LocalAdjust, Mask, MaskPart, ObjectClick, Op, Part, Settings, Shape,
};
use shoebox_develop::mask::{self, store};

/// Searches run, by the finder and the segmenter.
static FINDS: AtomicUsize = AtomicUsize::new(0);
static SEGMENTS: AtomicUsize = AtomicUsize::new(0);
/// One test at a time (they share the store and the counts).
static ONE: Mutex<()> = Mutex::new(());

fn register() {
    mask::register_finder(
        "fake-finder/1",
        Box::new(|rgb, _w, _h, _what| {
            FINDS.fetch_add(1, Ordering::SeqCst);
            // "Subject": the red pixels.
            Ok(rgb
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| if p[0] > 150 && p[1] < 80 { 255 } else { 0 })
                .collect())
        }),
    );
    // A disc of radius 6 around the first click.
    mask::register_segmenter(
        "fake-segmenter/1",
        Box::new(|_rgb, w, h, clicks| {
            SEGMENTS.fetch_add(1, Ordering::SeqCst);
            let (cx, cy, _) = clicks[0];
            Ok((0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f32, (i / w) as f32);
                    if (x - cx).hypot(y - cy) < 6.0 { 255 } else { 0 }
                })
                .collect())
        }),
    );
}

/// 120×80, grey (`grey`), with a red square at the stored image's top left.
fn source(grey: f32) -> Demosaiced {
    let mut img = Rgb::new(120, 80);
    for y in 0..80 {
        for x in 0..120 {
            let p = if x < 30 && y < 20 {
                [0.8, 0.02, 0.02]
            } else {
                [grey; 3]
            };
            img.data[(y * 120 + x) * 3..][..3].copy_from_slice(&p);
        }
    }
    Demosaiced::new(img, [1.0; 3], Referred::Display)
}

fn with(shape: Shape) -> Settings {
    Settings {
        locals: vec![Local {
            id: "l1".into(),
            mask: Mask {
                parts: vec![Part::Known(MaskPart {
                    op: Op::Add,
                    invert: false,
                    shape,
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
        linear: None,
    }
}

/// A fresh store in a fresh folder.
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

fn pngs(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(d) = std::fs::read_dir(dir.join("ai-masks")) else {
        return Vec::new();
    };
    let mut v: Vec<_> = d
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .collect();
    v.sort();
    v
}

/// The mask on the square and off it.
fn masked(f: &cpu::Frame) -> (f32, f32) {
    let m = f.layers.masks.as_ref().unwrap();
    (
        mask::sample(m, 0, 10.0, 10.0),
        mask::sample(m, 0, 80.0, 50.0),
    )
}

#[test]
fn a_fresh_source_reads_the_mask_from_disk() {
    let (_one, dir) = fresh();
    let s = with(Shape::Subject);
    let o = Options::default();
    let before = FINDS.load(Ordering::SeqCst);
    let f = cpu::frame(&raw(), &source(0.2), &s, &o);
    assert_eq!(masked(&f), (1.0, 0.0));
    assert_eq!(FINDS.load(Ordering::SeqCst), before + 1);
    assert_eq!(pngs(dir.path()).len(), 1, "kept as a PNG");
    // The photo opened again: a new source, a new in-memory cache.
    let f = cpu::frame(&raw(), &source(0.2), &s, &o);
    assert_eq!(masked(&f), (1.0, 0.0));
    assert_eq!(FINDS.load(Ordering::SeqCst), before + 1, "read, not found");
    // In the background (the live view) too: it arrives without a search.
    let src = source(0.2);
    src.cache.set_background(true);
    let _ = cpu::frame(&raw(), &src, &s, &o);
    let t = std::time::Instant::now();
    while src.cache.finding().0 {
        assert!(t.elapsed().as_secs() < 10);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(masked(&cpu::frame(&raw(), &src, &s, &o)), (1.0, 0.0));
    assert_eq!(FINDS.load(Ordering::SeqCst), before + 1);
    // A PNG of the stored picture's size, 8-bit grey.
    let p = &pngs(dir.path())[0];
    let r = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(p).unwrap()))
        .read_info()
        .unwrap();
    assert_eq!((r.info().width, r.info().height), (120, 80));
    assert_eq!(r.info().color_type, png::ColorType::Grayscale);
    assert_eq!(r.info().bit_depth, png::BitDepth::Eight);
}

#[test]
fn anything_the_finder_would_see_differently_searches_again() {
    let (_one, dir) = fresh();
    let s = with(Shape::Subject);
    let n = || FINDS.load(Ordering::SeqCst);
    let start = n();
    let o = Options::default();
    let _ = cpu::frame(&raw(), &source(0.2), &s, &o);
    assert_eq!(n(), start + 1);
    // Turned: the finder sees it upright, another picture.
    let turned = Options {
        orientation: 6,
        ..Default::default()
    };
    let f = cpu::frame(&raw(), &source(0.2), &s, &turned);
    assert_eq!(masked(&f), (1.0, 0.0));
    assert_eq!(n(), start + 2);
    // Another exposure: shown differently.
    let mut brighter = s.clone();
    brighter.modules.exposure.ev = 0.5;
    let _ = cpu::frame(&raw(), &source(0.2), &brighter, &o);
    assert_eq!(n(), start + 3);
    // Another photo (different pixels).
    let _ = cpu::frame(&raw(), &source(0.3), &s, &o);
    assert_eq!(n(), start + 4);
    // Another kind of part.
    let _ = cpu::frame(&raw(), &source(0.2), &with(Shape::Person), &o);
    assert_eq!(n(), start + 5);
    assert_eq!(pngs(dir.path()).len(), 5);
    // And each of them again: all from disk.
    for (src, s, o) in [
        (source(0.2), s.clone(), o),
        (source(0.2), s.clone(), turned),
        (source(0.2), brighter, o),
        (source(0.3), s.clone(), o),
        (source(0.2), with(Shape::Person), o),
    ] {
        let _ = cpu::frame(&raw(), &src, &s, &o);
    }
    assert_eq!(n(), start + 5);
}

#[test]
fn objects_are_kept_by_their_clicks() {
    let (_one, dir) = fresh();
    let n = || SEGMENTS.load(Ordering::SeqCst);
    let start = n();
    let at = |x: f64, y: f64| ObjectClick {
        at: [(x + 0.5) / 120.0, (y + 0.5) / 120.0],
        add: true,
    };
    let object = |clicks: Vec<ObjectClick>| with(Shape::Object { clicks });
    let o = Options::default();
    let f = cpu::frame(&raw(), &source(0.2), &object(vec![at(90.0, 60.0)]), &o);
    let m = f.layers.masks.as_ref().unwrap();
    assert_eq!(mask::sample(m, 0, 90.0, 60.0), 1.0);
    assert_eq!(n(), start + 1);
    // Again on a fresh source: from disk.
    let f = cpu::frame(&raw(), &source(0.2), &object(vec![at(90.0, 60.0)]), &o);
    assert_eq!(
        mask::sample(f.layers.masks.as_ref().unwrap(), 0, 90.0, 60.0),
        1.0
    );
    assert_eq!(n(), start + 1);
    // Elsewhere, or another click: searched.
    let _ = cpu::frame(&raw(), &source(0.2), &object(vec![at(40.0, 50.0)]), &o);
    assert_eq!(n(), start + 2);
    let two = vec![at(90.0, 60.0), at(40.0, 50.0)];
    let _ = cpu::frame(&raw(), &source(0.2), &object(two.clone()), &o);
    assert_eq!(n(), start + 3);
    let _ = cpu::frame(&raw(), &source(0.2), &object(two), &o);
    assert_eq!(n(), start + 3);
    assert_eq!(pngs(dir.path()).len(), 3);
}

#[test]
fn a_damaged_file_is_ignored_and_redone() {
    let (_one, dir) = fresh();
    let s = with(Shape::Subject);
    let o = Options::default();
    let n = || FINDS.load(Ordering::SeqCst);
    let start = n();
    let _ = cpu::frame(&raw(), &source(0.2), &s, &o);
    let files = pngs(dir.path());
    assert_eq!(files.len(), 1);
    let good = std::fs::read(&files[0]).unwrap();
    for bad in [
        b"not a png at all".to_vec(),
        good[..good.len() / 2].to_vec(),
    ] {
        std::fs::write(&files[0], &bad).unwrap();
        let before = n();
        let f = cpu::frame(&raw(), &source(0.2), &s, &o);
        assert_eq!(masked(&f), (1.0, 0.0), "found again");
        assert_eq!(n(), before + 1);
        assert_eq!(std::fs::read(&files[0]).unwrap(), good, "and kept again");
    }
    assert_eq!(n(), start + 3);
    // A PNG of the wrong size is as good as damaged.
    let st = store::current().unwrap();
    let key = files[0].file_stem().unwrap().to_string_lossy().into_owned();
    st.put(&key, &[7u8; 12], 4, 3).unwrap();
    assert_eq!(st.get(&key, 120, 80), None);
    let _ = cpu::frame(&raw(), &source(0.2), &s, &o);
    assert_eq!(n(), start + 4);
}

#[test]
fn the_least_recently_used_go_past_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let mask = |seed: u8| -> Vec<u8> {
        // Noise, so each PNG is about its raw size.
        let mut v = Vec::with_capacity(64 * 64);
        let mut x = u32::from(seed).wrapping_mul(2_654_435_761) | 1;
        for _ in 0..64 * 64 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            v.push((x >> 24) as u8);
        }
        v
    };
    let one = {
        let s = store::Store::new(dir.path(), u64::MAX);
        s.put("probe", &mask(9), 64, 64).unwrap();
        let n = s.size();
        std::fs::remove_file(s.path("probe")).unwrap();
        n
    };
    // Room for two and a half.
    let s = store::Store::new(dir.path(), one * 5 / 2);
    let pause = || std::thread::sleep(std::time::Duration::from_millis(20));
    s.put("a", &mask(1), 64, 64).unwrap();
    pause();
    s.put("b", &mask(2), 64, 64).unwrap();
    pause();
    // Reading a makes it the most recently used.
    assert_eq!(s.get("a", 64, 64), Some(mask(1)));
    pause();
    s.put("c", &mask(3), 64, 64).unwrap();
    assert!(s.get("b", 64, 64).is_none(), "b was least recently used");
    assert_eq!(s.get("a", 64, 64), Some(mask(1)));
    assert_eq!(s.get("c", 64, 64), Some(mask(3)));
    assert!(s.size() <= one * 5 / 2);
    // Only its own PNGs are ever removed.
    std::fs::write(dir.path().join("notes.txt"), "keep me").unwrap();
    std::fs::write(dir.path().join("big.bin"), vec![0u8; 100_000]).unwrap();
    s.put("d", &mask(4), 64, 64).unwrap();
    assert!(dir.path().join("notes.txt").is_file());
    assert!(dir.path().join("big.bin").is_file());
}
