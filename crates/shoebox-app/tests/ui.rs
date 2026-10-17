//! Headless UI tests: run the real app against the synthetic library,
//! render the views to PNGs (for review) and check behaviour.
//!
//! Snapshots go to `$SHOEBOX_UI_SNAPSHOTS` (default: a temp folder).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use shoebox_app::{AppPaths, ShoeboxApp, View};
use shoebox_core::settings::Settings;

#[cfg(target_os = "macos")]
const REVEAL: &str = "Reveal in Finder";
#[cfg(not(target_os = "macos"))]
const REVEAL: &str = "Show in File Manager";

fn paths(tmp: &Path, roots: &[PathBuf]) -> AppPaths {
    let settings = tmp.join("data/settings.json");
    let mut s = Settings::default();
    for r in roots {
        s.add_root(r).unwrap();
    }
    s.save(&settings).unwrap();
    AppPaths {
        settings,
        cache_db: tmp.join("data/cache.sqlite"),
        journal: tmp.join("data/journal.sqlite"),
        thumbs: tmp.join("cache/thumbs"),
        trash_dir: Some(tmp.join("trash")),
        tile_cache: None, // no network in tests
        embeddings_db: tmp.join("data/embeddings.sqlite"),
        // Never downloaded: semantic search is off unless a test turns it
        // on, and then it uses a test model.
        model: shoebox_app::semantic::ModelSource::Download {
            dir: tmp.join("data/models"),
            staging: tmp.join("cache/model-download"),
        },
    }
}

fn harness<'a>(paths: AppPaths, size: egui::Vec2) -> Harness<'a, ShoeboxApp> {
    Harness::builder()
        .with_size(size)
        .wgpu()
        .build_eframe(move |cc| ShoeboxApp::new(cc, paths))
}

/// Step until the library is loaded and no image loads are outstanding.
fn settle(h: &mut Harness<'_, ShoeboxApp>, timeout: Duration) {
    let start = Instant::now();
    let mut quiet = 0;
    loop {
        h.step();
        if h.state().idle() && h.state().photo_count() > 0 {
            quiet += 1;
            if quiet > 4 {
                return;
            }
        } else {
            quiet = 0;
        }
        assert!(
            start.elapsed() < timeout,
            "UI did not settle in {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn snapshot(h: &mut Harness<'_, ShoeboxApp>, name: &str) {
    let dir = std::env::var_os("SHOEBOX_UI_SNAPSHOTS")
        .map_or_else(|| std::env::temp_dir().join("shoebox-ui"), PathBuf::from);
    std::fs::create_dir_all(&dir).unwrap();
    let img = h.render().unwrap();
    let path = dir.join(format!("{name}.png"));
    img.save(&path).unwrap();
    eprintln!("snapshot: {}", path.display());
}

#[test]
fn browse_views_render() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let before = std::fs::read_dir(lib.join("scan/recovered"))
        .unwrap()
        .count();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    assert_eq!(h.state().photo_count(), 17); // 7 reference + 10 scan images
    // The library panel counts photos (stacks), as the grid shows them.
    let photos = h.state().shown_count();
    assert!(photos < 17, "the fixtures stack NEF + JPG");
    assert!(
        h.query_by_label(&format!("All photos  {photos}")).is_some(),
        "All photos counts stacks"
    );
    snapshot(&mut h, "1-grid");

    // Right-click a photo: it becomes the selection and the menu offers
    // Reveal in Finder. (The item isn't clicked, so Finder doesn't open.)
    h.get_by_label("f0005.jpg").click_secondary();
    h.run_steps(3);
    assert_eq!(h.state().selected_count(), 1);
    assert!(
        h.query_by_label(REVEAL).is_some(),
        "context menu shows {REVEAL}"
    );
    snapshot(&mut h, "1b-context-menu");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    // With two selected, right-clicking one of them reveals both.
    h.get_by_label("f0003.jpg")
        .click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(h.state().selected_count(), 2);
    h.get_by_label("f0003.jpg").click_secondary();
    h.run_steps(3);
    assert_eq!(
        h.state().selected_count(),
        2,
        "right-click inside the selection keeps it"
    );
    assert!(h.query_by_label(&format!("{REVEAL} (2 photos)")).is_some());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    h.key_press(egui::Key::M);
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "2-masonry");

    // Select the portrait NEF (Orientation 6) and open it in the loupe.
    h.key_press(egui::Key::G);
    h.step();
    for _ in 0..2 {
        h.key_press(egui::Key::ArrowRight);
        h.step();
    }
    h.key_press(egui::Key::E);
    settle(&mut h, Duration::from_secs(30));
    assert_eq!(h.state().view_name(), View::Loupe);
    snapshot(&mut h, "3-loupe");

    // Space toggles 100% and back.
    h.key_press(egui::Key::Space);
    settle(&mut h, Duration::from_secs(30));
    assert!(h.state().loupe_zoomed());
    h.key_press(egui::Key::Space);
    h.step();
    assert!(!h.state().loupe_zoomed());

    // The app never writes into the library.
    assert_eq!(
        std::fs::read_dir(lib.join("scan/recovered"))
            .unwrap()
            .count(),
        before
    );
}

/// Scroll a 10,000-photo grid and measure UI frame time.
/// `cargo test --release -p shoebox-app --test ui -- --ignored --nocapture`
#[test]
#[ignore = "slow: builds a 10,000-file library"]
fn ten_thousand_photos_scroll_smoothly() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("big");
    let seed = t.path().join("seed.jpg");
    std::fs::write(
        &seed,
        shoebox_fixtures::jpeg(&shoebox_fixtures::scene(9, 1800, 1200), 85).unwrap(),
    )
    .unwrap();
    let start = Instant::now();
    for d in 0..100 {
        let dir = lib.join(format!("2026/day-{d:03}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..100 {
            // std::fs::copy clones on APFS: instant and space-free.
            std::fs::copy(&seed, dir.join(format!("IMG_{f:04}.jpg"))).unwrap();
        }
    }
    eprintln!(
        "built 10,000 files in {:.1} s",
        start.elapsed().as_secs_f64()
    );

    let p = paths(t.path(), std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1600.0, 1000.0));
    let open = Instant::now();
    settle(&mut h, Duration::from_secs(300));
    eprintln!(
        "first open (scan + visible thumbnails): {:.1} s",
        open.elapsed().as_secs_f64()
    );
    assert_eq!(h.state().photo_count(), 10_000);

    // Scroll continuously: pointer over the grid, wheel events every frame.
    h.event(egui::Event::PointerMoved(egui::pos2(800.0, 500.0)));
    let mut frames = Vec::new();
    for _ in 0..600 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::NONE,
        });
        let f = Instant::now();
        h.step();
        frames.push(f.elapsed().as_secs_f64() * 1e3);
    }
    frames.sort_by(f64::total_cmp);
    let pct = |p: f64| frames[((frames.len() - 1) as f64 * p) as usize];
    eprintln!(
        "UI frame (CPU) over 600 scrolling frames: p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        pct(0.5),
        pct(0.95),
        pct(0.99),
        frames[frames.len() - 1]
    );
    settle(&mut h, Duration::from_secs(120));
    snapshot(&mut h, "4-grid-10k-scrolled");
    assert!(
        pct(0.99) < 16.0,
        "p99 UI frame {:.2} ms exceeds 16 ms",
        pct(0.99)
    );
}

/// Frame times (CPU, ms) over `n` steps, each after `before(h, i)`.
fn time_frames(
    h: &mut Harness<'_, ShoeboxApp>,
    n: usize,
    mut before: impl FnMut(&mut Harness<'_, ShoeboxApp>, usize),
) -> Vec<f64> {
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        before(h, i);
        let f = Instant::now();
        h.step();
        frames.push(f.elapsed().as_secs_f64() * 1e3);
    }
    frames.sort_by(f64::total_cmp);
    frames
}

/// "p50 / p99 / max" of sorted frame times, and the p99.
fn frame_stats(frames: &[f64]) -> (String, f64) {
    let pct = |p: f64| frames[((frames.len() - 1) as f64 * p) as usize];
    (
        format!(
            "p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
            pct(0.5),
            pct(0.99),
            frames[frames.len() - 1]
        ),
        pct(0.99),
    )
}

fn wheel(h: &mut Harness<'_, ShoeboxApp>, dy: f32) {
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, dy),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: egui::Modifiers::NONE,
    });
}

/// The spec's 200,000-file targets, on a library from
/// `cargo run --release -p shoebox-fixtures --example big_library -- <dir>`:
/// `SHOEBOX_UI_BIG=<dir> cargo test --release -p shoebox-app --test ui
/// two_hundred_thousand -- --ignored --nocapture`. Set `SHOEBOX_UI_BIG_DATA`
/// to keep the cache between runs (then the first open is from cache too).
#[test]
#[ignore = "needs SHOEBOX_UI_BIG"]
fn two_hundred_thousand_files() {
    let Some(lib) = std::env::var_os("SHOEBOX_UI_BIG").map(PathBuf::from) else {
        return;
    };
    let t = tempfile::tempdir().unwrap();
    let data = std::env::var_os("SHOEBOX_UI_BIG_DATA")
        .map_or_else(|| t.path().to_path_buf(), PathBuf::from);
    let p = paths(&data, std::slice::from_ref(&lib));
    let size = egui::vec2(1600.0, 1000.0);
    let mut report: Vec<String> = Vec::new();
    let mut note = |s: String| {
        eprintln!("{s}");
        report.push(s);
    };

    // First open: scan, read metadata, hash (unless the cache is kept).
    let open = Instant::now();
    let mut h = harness(p.clone(), size);
    settle(&mut h, Duration::from_secs(3600));
    let files = h.state().photo_count();
    note(format!(
        "first open: {files} files, {} shown, {:.1} s",
        h.state().shown_count(),
        open.elapsed().as_secs_f64()
    ));
    drop(h);

    // Open again from the cache: until the grid lists everything, then until
    // the visible thumbnails are in.
    let open = Instant::now();
    let mut h = harness(p.clone(), size);
    let mut first_frame = None;
    while h.state().photo_count() < files {
        h.step();
        first_frame.get_or_insert(open.elapsed());
        assert!(open.elapsed() < Duration::from_secs(120), "never loaded");
    }
    let listed = open.elapsed();
    // Thumbnails: until no image load is outstanding for a few frames (the
    // rescan from disk carries on in the background).
    let mut quiet = 0;
    while quiet < 3 {
        h.step();
        quiet = if h.state().loading() == 0 {
            quiet + 1
        } else {
            0
        };
        std::thread::sleep(Duration::from_millis(2));
    }
    let thumbs = open.elapsed();
    settle(&mut h, Duration::from_secs(300));
    note(format!(
        "open from cache: first frame {:.0} ms, grid lists all {:.0} ms, visible thumbnails {:.0} ms, rescan done {:.0} ms",
        first_frame.unwrap_or_default().as_secs_f64() * 1e3,
        listed.as_secs_f64() * 1e3,
        thumbs.as_secs_f64() * 1e3,
        open.elapsed().as_secs_f64() * 1e3
    ));
    let listed_ms = listed.as_secs_f64() * 1e3;

    h.event(egui::Event::PointerMoved(egui::pos2(800.0, 500.0)));
    let scroll = time_frames(&mut h, 600, |h, _| wheel(h, -120.0));
    let (s, scroll_p99) = frame_stats(&scroll);
    note(format!("grid scroll (600 frames): {s}"));
    // A long jump (End) and back (Home).
    let jump = time_frames(&mut h, 20, |h, i| {
        if i % 2 == 0 {
            h.key_press(egui::Key::End);
        } else {
            h.key_press(egui::Key::Home);
        }
    });
    note(format!("Home/End jumps: {}", frame_stats(&jump).0));
    settle(&mut h, Duration::from_secs(120));
    snapshot(&mut h, "big-1-grid");

    // Views: the switch frame, then steady frames.
    for (view, name) in [
        (View::Masonry, "masonry"),
        (View::Timeline, "timeline"),
        (View::Map, "map"),
        (View::Grid, "grid"),
    ] {
        let f = Instant::now();
        h.state_mut().set_view(view);
        h.step();
        let switch = f.elapsed().as_secs_f64() * 1e3;
        let steady = time_frames(&mut h, 60, |h, i| {
            wheel(h, if i % 2 == 0 { -60.0 } else { 60.0 })
        });
        note(format!(
            "{name}: switch {switch:.1} ms; then {}",
            frame_stats(&steady).0
        ));
        settle(&mut h, Duration::from_secs(120));
        snapshot(&mut h, &format!("big-2-{name}"));
    }

    // Filters: rating ≥ 3, a file-name search, then none.
    for (filter, name) in [
        (
            shoebox_core::query::Filter {
                min_rating: 3,
                ..Default::default()
            },
            "rating ≥ 3",
        ),
        (
            shoebox_core::query::Filter {
                search: "DSC_12".into(),
                ..Default::default()
            },
            "search DSC_12",
        ),
        (shoebox_core::query::Filter::default(), "no filter"),
    ] {
        let f = Instant::now();
        h.state_mut().set_filter(filter);
        h.step();
        let ms = f.elapsed().as_secs_f64() * 1e3;
        let steady = time_frames(&mut h, 30, |_, _| {});
        note(format!(
            "filter {name}: {} shown, first frame {ms:.1} ms; then {}",
            h.state().shown_count(),
            frame_stats(&steady).0
        ));
    }

    // Select everything.
    let f = Instant::now();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    let ms = f.elapsed().as_secs_f64() * 1e3;
    let steady = time_frames(&mut h, 60, |h, _| wheel(h, -120.0));
    note(format!(
        "select all ({}): {ms:.1} ms; scrolling then {}",
        h.state().selected_count(),
        frame_stats(&steady).0
    ));
    h.key_press(egui::Key::Escape);
    h.step();

    // Rate a photo: the catalog is republished, which must not stall a frame.
    h.key_press(egui::Key::ArrowRight);
    h.step();
    let name = h.state().current_name().expect("a current photo");
    let before = h.state().marks_of(&name).map_or(0, |m| m.rating);
    let rating = if before == 4 { 2 } else { 4 };
    h.key_press(if rating == 4 {
        egui::Key::Num4
    } else {
        egui::Key::Num2
    });
    let f = Instant::now();
    let mut rate_frames = Vec::new();
    while rate_frames.len() < 30 || h.state().marks_of(&name).map_or(0, |m| m.rating) != rating {
        let s = Instant::now();
        h.step();
        rate_frames.push(s.elapsed().as_secs_f64() * 1e3);
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            f.elapsed() < Duration::from_secs(30),
            "rating never applied"
        );
    }
    rate_frames.sort_by(f64::total_cmp);
    let (s, rate_p99) = frame_stats(&rate_frames);
    note(format!("rate a photo: frames meanwhile {s}"));

    // Duplicate search with every hash cached.
    let f = Instant::now();
    h.state_mut().find_duplicates();
    let mut ui_frames = Vec::new();
    while !h.state().dupes_idle() {
        let s = Instant::now();
        h.step();
        ui_frames.push(s.elapsed().as_secs_f64() * 1e3);
        std::thread::sleep(Duration::from_millis(5));
        assert!(
            f.elapsed() < Duration::from_secs(600),
            "duplicate search hung"
        );
    }
    ui_frames.sort_by(f64::total_cmp);
    note(format!(
        "duplicates (hashes cached): {} groups in {:.1} s; UI meanwhile {}",
        h.state().dupe_groups().unwrap_or(0),
        f.elapsed().as_secs_f64(),
        frame_stats(&ui_frames).0
    ));
    h.state_mut().set_view(View::Duplicates);
    let steady = time_frames(&mut h, 60, |h, _| wheel(h, -120.0));
    note(format!(
        "duplicates list scroll: {}",
        frame_stats(&steady).0
    ));
    settle(&mut h, Duration::from_secs(120));
    snapshot(&mut h, "big-3-duplicates");

    eprintln!("\n{}", report.join("\n"));
    assert!(
        listed_ms < 1000.0,
        "grid took {listed_ms:.0} ms to list the library"
    );
    assert!(scroll_p99 < 16.0, "grid scroll p99 {scroll_p99:.2} ms");
    assert!(
        rate_p99 < 16.0,
        "rating a photo: p99 frame {rate_p99:.2} ms"
    );
}

/// Browse a real library read-only: `SHOEBOX_UI_LIBRARY=/path/to/copy
/// cargo test --release -p shoebox-app --test ui real_library -- --ignored --nocapture`.
/// Settings, cache and thumbnails go to a temp folder; the library is only read.
#[test]
#[ignore = "needs SHOEBOX_UI_LIBRARY"]
fn real_library() {
    let Some(lib) = std::env::var_os("SHOEBOX_UI_LIBRARY").map(PathBuf::from) else {
        return;
    };
    let t = tempfile::tempdir().unwrap();
    let mut p = paths(t.path(), &[lib]);
    // Opt in to real map tiles (network): SHOEBOX_UI_TILES=<cache dir>.
    p.tile_cache = std::env::var_os("SHOEBOX_UI_TILES").map(PathBuf::from);
    let mut h = harness(p, egui::vec2(1600.0, 1000.0));
    let open = Instant::now();
    settle(&mut h, Duration::from_secs(600));
    eprintln!(
        "{} photos; first open with visible thumbnails: {:.1} s",
        h.state().photo_count(),
        open.elapsed().as_secs_f64()
    );
    snapshot(&mut h, "real-1-grid");
    h.key_press(egui::Key::M);
    settle(&mut h, Duration::from_secs(120));
    snapshot(&mut h, "real-2-masonry");
    // Step to a NEF and open it.
    h.key_press(egui::Key::G);
    h.step();
    for _ in 0..2000 {
        h.key_press(egui::Key::ArrowRight);
        h.step();
        if h.state().current_is_nef() {
            break;
        }
    }
    h.key_press(egui::Key::E);
    let t0 = Instant::now();
    settle(&mut h, Duration::from_secs(120));
    eprintln!(
        "loupe (1024 preview + full decode): {:.2} s",
        t0.elapsed().as_secs_f64()
    );
    snapshot(&mut h, "real-3-loupe-nef");
    // Timeline: build time and frame times.
    h.key_press(egui::Key::T);
    let t0 = Instant::now();
    h.step();
    eprintln!(
        "timeline first frame: {:.1} ms",
        t0.elapsed().as_secs_f64() * 1e3
    );
    settle(&mut h, Duration::from_secs(120));
    let mut frames = Vec::new();
    for _ in 0..30 {
        let t = Instant::now();
        h.step();
        frames.push(t.elapsed().as_secs_f64() * 1e3);
    }
    frames.sort_by(f64::total_cmp);
    eprintln!(
        "timeline days: {:?}; frame p50 {:.2} ms, max {:.2} ms",
        h.state().timeline_days().map(|d| d.0.len()),
        frames[15],
        frames[29]
    );
    snapshot(&mut h, "real-4-timeline");
    // Map: give tiles a few seconds to arrive.
    h.get_by_label("Map").click();
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(8) {
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("map: {} located", h.state().located_count());
    snapshot(&mut h, "real-5-map");
}

/// Marking through the keyboard writes sidecars, undo restores them, and
/// stacks and filters shape the grid.
#[test]
fn organise_marks_stacks_and_filters() {
    use shoebox_core::marks::{Label, Pick};
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    // 17 files → 14 items: DSC_0001.JPG and f0004 (same capture time and
    // serial) join DSC_0001.NEF's stack; DSC_0001-2.tif is a variant.
    assert_eq!(h.state().photo_count(), 17);
    assert_eq!(h.state().shown_count(), 14);
    snapshot(&mut h, "5-stacks");

    let side = lib.join("scan/recovered/f0003.jpg.xmp");
    h.get_by_label("f0003.jpg").click();
    h.run_steps(2);
    for k in [egui::Key::Num4, egui::Key::P, egui::Key::Num6] {
        h.key_press(k);
        h.run_steps(2);
    }
    let wait_for = |h: &mut Harness<'_, ShoeboxApp>, ok: &dyn Fn() -> bool| {
        let start = Instant::now();
        while !ok() {
            h.step();
            std::thread::sleep(Duration::from_millis(20));
            assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        }
    };
    let read = |p: &Path| -> shoebox_core::marks::Marks {
        let props = std::fs::read(p)
            .ok()
            .and_then(|b| shoebox_core::xmp::read(&b).ok())
            .unwrap_or_default();
        shoebox_core::marks::Marks::from_props(&props)
    };
    wait_for(&mut h, &|| read(&side).label == Some(Label::Red));
    let m = read(&side);
    assert_eq!(
        (m.rating, m.pick, m.label),
        (4, Some(Pick::Pick), Some(Label::Red))
    );
    assert_eq!(h.state().marks_of("f0003.jpg").unwrap().rating, 4);
    snapshot(&mut h, "6-marked");

    // Keyword via ⌘K, typed with '>' separators.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
    h.run_steps(2);
    h.event(egui::Event::Text("Places > Rome".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    wait_for(&mut h, &|| {
        read(&side).keywords == vec!["Places|Rome".to_owned()]
    });

    // ⌘Z undoes the keyword, then the label.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    for _ in 0..2 {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run_steps(2);
        settle(&mut h, Duration::from_secs(30));
    }
    let m = read(&side);
    assert!(m.keywords.is_empty() && m.label.is_none(), "{m:?}");
    assert_eq!((m.rating, m.pick), (4, Some(Pick::Pick)));
    assert_eq!(
        h.state().marks_of("f0003.jpg").unwrap().label,
        None,
        "UI reflects the undo"
    );

    // Filter: rating ≥ 4 leaves just that stack.
    h.state_mut().set_filter(shoebox_core::query::Filter {
        min_rating: 4,
        ..Default::default()
    });
    h.run_steps(2);
    assert_eq!(h.state().shown_count(), 1);
}

/// Cmd+E makes an event from the selection: events.json in every writable
/// root, the event in members' sidecars, and one undo step.
#[test]
fn events_from_selection_and_undo() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("DSC_0001.NEF").click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    h.run_steps(3);
    h.event(egui::Event::Text("Japan Trip".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);

    let nef_side = lib.join("ref/2026/2026-04-24/DSC_0001.NEF.xmp");
    let read = |p: &Path| -> shoebox_core::marks::Marks {
        let props = std::fs::read(p)
            .ok()
            .and_then(|b| shoebox_core::xmp::read(&b).ok())
            .unwrap_or_default();
        shoebox_core::marks::Marks::from_props(&props)
    };
    let start = Instant::now();
    while read(&nef_side).event.is_none() {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "event not written"
        );
    }
    settle(&mut h, Duration::from_secs(30));
    let m = read(&nef_side);
    assert_eq!(m.event.as_deref(), Some("Japan Trip"));
    assert_eq!(m.keywords, vec!["Events|Japan Trip"]);
    for root in ["ref", "scan"] {
        let f = lib.join(root).join(".shoebox/events.json");
        assert!(
            std::fs::read_to_string(&f).unwrap().contains("Japan Trip"),
            "{}",
            f.display()
        );
    }
    assert!(
        !lib.join("scan/recovered/f0010.jpg.xmp").exists(),
        "the burst frame a second later isn't in it"
    );
    assert_eq!(h.state().event_names(), vec!["Japan Trip"]);
    snapshot(&mut h, "7-event");

    // One undo removes the event everywhere.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    settle(&mut h, Duration::from_secs(30));
    let m = read(&nef_side);
    assert_eq!((m.event, m.keywords.len()), (None, 0));
    assert!(h.state().event_names().is_empty());
}

/// Step until `cond` holds (or fail after `timeout`).
fn wait_for(
    h: &mut Harness<'_, ShoeboxApp>,
    timeout: Duration,
    what: &str,
    cond: impl Fn(&ShoeboxApp) -> bool,
) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Another app edits a sidecar while Shoebox is open: the watcher picks it
/// up, the photo gets an "other app" badge, and viewing it clears the badge.
/// Shoebox's own writes are badged as Shoebox's, and undo too.
#[test]
fn watcher_tracks_changes_by_other_apps() {
    use shoebox_core::changes::By;
    use shoebox_core::xmp::{Prop, Value};
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let lib = lib.canonicalize().unwrap();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let cache_db = p.cache_db.clone();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    assert!(h.state().change_of("f0003.jpg").is_none());

    // "DxO" writes a rating into a new sidecar.
    let jpg = lib.join("scan/recovered/f0003.jpg");
    let xmp =
        shoebox_core::xmp::edit(None, &[(Prop::Rating, Some(Value::Text("3".into())))]).unwrap();
    std::fs::write(shoebox_core::sidecar::per_file(&jpg), xmp).unwrap(); // fixture
    wait_for(&mut h, Duration::from_secs(10), "the watcher", |a| {
        a.marks_of("f0003.jpg").is_some_and(|m| m.rating == 3)
    });
    let ch = h.state().change_of("f0003.jpg").unwrap();
    assert_eq!(ch.by, By::Other);
    snapshot(&mut h, "9-changed-badge");

    // Shoebox's own rating: badged as Shoebox's, and the watcher seeing the
    // write doesn't turn it into "another app".
    h.get_by_label("f0005.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::Num4);
    wait_for(&mut h, Duration::from_secs(10), "own change", |a| {
        a.change_of("f0005.jpg").is_some()
    });
    let settle_until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < settle_until {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    let own = h.state().change_of("f0005.jpg").unwrap();
    assert!(matches!(own.by, By::Shoebox { .. }), "{own:?}");

    // Viewing f0003 in the loupe clears its badge, and that's remembered.
    h.get_by_label("f0003.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::E);
    h.run_steps(3);
    assert!(h.state().change_of("f0003.jpg").is_none());
    h.key_press(egui::Key::G);
    h.run_steps(3);
    let start = Instant::now();
    loop {
        let cache = shoebox_core::cache::Cache::open(&cache_db).unwrap();
        let roots = vec![
            shoebox_core::scan::Root::new(&lib.join("scan"), shoebox_core::scan::Role::Scan)
                .unwrap(),
        ];
        let c = shoebox_core::catalog::Catalog::from_cache(&cache, roots).unwrap();
        if c.photos.iter().any(|p| p.path == jpg && p.change.is_none()) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "seen not saved");
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// M4 acceptance: the `_undated` clean-up done entirely in the app. Carved
/// previews and copies sit in `scan/_undated/…`; the reference library is
/// the keeper. Find, auto-select, look at a group, decide a burst frame by
/// hand, apply (after the dry-run table), then undo.
#[test]
fn undated_cleanup_in_the_app() {
    use shoebox_core::review::Decision;
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let lib = lib.canonicalize().unwrap();
    let undated = lib.join("scan/_undated/1800x1200");
    std::fs::create_dir_all(undated.parent().unwrap()).unwrap();
    std::fs::rename(lib.join("scan/recovered"), &undated).unwrap(); // fixture layout
    let ref_before = std::fs::read_dir(lib.join("ref/lightroom"))
        .unwrap()
        .count();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut s = Settings::load(&p.settings).unwrap();
    s.set_reference(&lib.join("ref"), true);
    s.save(&p.settings).unwrap();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    // Tasks → Duplicates → Find.
    h.get_by_label("Duplicates").click();
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Duplicates);
    h.get_by_label("Find duplicates").click();
    wait_for(&mut h, Duration::from_secs(60), "duplicates", |a| {
        a.dupes_idle()
    });
    // Each file to decide on is shown with its closest copy: five groups.
    // Matches inside the reference folder are shown only on request.
    assert_eq!(h.state().dupe_groups(), Some(5));
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "10-duplicates");

    h.get_by_label("Auto-select all").click();
    h.run_steps(2);
    assert_eq!(h.state().decision_of("f0001.jpg"), Decision::Move);
    assert_eq!(h.state().decision_of("DSC_0001.tif"), Decision::Keep);
    assert_eq!(
        h.state().decision_of("f0010.jpg"),
        Decision::Undecided,
        "a burst frame 1 s later is left for a person"
    );
    assert_eq!(
        h.state().decision_of("f0003.jpg"),
        Decision::Undecided,
        "a resized copy with no capture time is left for a person"
    );

    // Open the two-file group (f0005/f0006) in Compare, and the big one in
    // Survey; decide the burst frame there.
    h.get_all_by_label("Open in Compare")
        .next()
        .unwrap()
        .click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Compare);
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "11-compare-group");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Duplicates);
    h.get_by_label("f0001.jpg").click(); // opens its group (the burst frame is in it)
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Survey);
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "12-survey-group");
    // Looking at them, a person confirms the resized copies (clicking Move
    // under f0003's tile) and keeps the burst frame as a variant.
    let moves = h.get_all_by_label("Move").count();
    assert!(moves >= 3, "a Move button under each tile");
    h.state_mut().decide("f0010.jpg", Decision::Variant);
    h.state_mut().decide("f0003.jpg", Decision::Move);
    h.state_mut().decide("f0007.png", Decision::Move);
    h.key_press(egui::Key::CloseBracket); // next group: the rotated copy
    h.run_steps(3);
    h.state_mut().decide("f0008.jpg", Decision::Move);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    // Apply: the dry-run table first, then one batch.
    h.get_by_label("Apply… (7 to move)").click();
    wait_for(&mut h, Duration::from_secs(10), "dry run", |a| {
        a.dupe_preview().is_some()
    });
    assert_eq!(h.state().dupe_preview(), Some(Ok(8)), "7 files + 1 sidecar");
    h.run_steps(2);
    snapshot(&mut h, "13-apply-dry-run");
    assert!(
        undated.join("f0001.jpg").exists(),
        "nothing moved before confirming"
    );
    h.get_by_label("Move 7 files").click();
    wait_for(&mut h, Duration::from_secs(60), "apply", |a| {
        a.dupe_message().is_some() && a.dupes_idle()
    });

    let left: Vec<String> = {
        let mut v: Vec<String> = std::fs::read_dir(&undated)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    // Unique pictures stay: f0005 (its copy f0006 went), the blank frame,
    // and the burst frame, now a variant in its NEF's stack.
    assert_eq!(left, vec!["f0005.jpg", "f0009.jpg", "f0010.jpg"]);
    let moved = lib.join("scan/_duplicates/_undated/1800x1200");
    for f in [
        "f0001.jpg",
        "f0001.jpg.xmp",
        "f0003.jpg",
        "f0004.jpg",
        "f0006.jpg",
    ] {
        assert!(moved.join(f).exists(), "{f} moved");
    }
    assert!(lib.join("scan/_duplicates/rotated/f0008.jpg").exists());
    assert_eq!(
        std::fs::read_dir(lib.join("ref/lightroom"))
            .unwrap()
            .count(),
        ref_before,
        "reference untouched"
    );
    // Nothing left to review: the remaining matches are reference-only and
    // the burst frame is settled as a variant.
    assert_eq!(h.state().dupe_groups(), Some(0));
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "14-after-apply");

    // One undo puts it all back.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(60), "undo", |_| {
        undated.join("f0001.jpg").exists()
    });
    assert!(undated.join("f0001.jpg.xmp").exists());
    assert!(lib.join("scan/rotated/f0008.jpg").exists());
}

/// Compare and Survey from a grid selection: arrows step the candidate (or
/// the active tile), marks land on the active photo, the X removes a tile.
#[test]
fn compare_and_survey_from_the_grid() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let p = paths(t.path(), &[lib.join("scan")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("f0003.jpg").click();
    h.get_by_label("f0005.jpg")
        .click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    h.key_press(egui::Key::C);
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Compare);
    let (a, b) = h.state().compare_names();
    assert_eq!(
        (a.as_deref(), b.as_deref()),
        (Some("f0003.jpg"), Some("f0005.jpg"))
    );
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    let (a2, b2) = h.state().compare_names();
    assert_eq!(a2, a, "the select stays");
    assert_ne!(b2, b, "the candidate moved on");
    // Marks go to the active side (the candidate, after stepping).
    h.key_press(egui::Key::Num2);
    h.run_steps(2);
    let cand = b2.unwrap();
    assert_eq!(h.state().marks_of(&cand).unwrap().rating, 2);
    assert_eq!(h.state().marks_of("f0003.jpg").unwrap().rating, 0);
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "15-compare");

    // Survey three photos.
    h.key_press(egui::Key::G);
    h.run_steps(2);
    h.get_by_label("f0003.jpg").click();
    for n in ["f0004.jpg", "f0005.jpg"] {
        h.get_by_label(n).click_modifiers(egui::Modifiers::COMMAND);
    }
    h.run_steps(2);
    h.key_press(egui::Key::N);
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Survey);
    let (names, active) = h.state().survey_names();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["f0003.jpg", "f0004.jpg", "f0005.jpg"]);
    // Step the active tile left (or right from the first) and flag it.
    let pos = names
        .iter()
        .position(|n| Some(n) == active.as_ref())
        .unwrap();
    let (key, want) = if pos > 0 {
        (egui::Key::ArrowLeft, &names[pos - 1])
    } else {
        (egui::Key::ArrowRight, &names[1])
    };
    h.key_press(key);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    let (_, active) = h.state().survey_names();
    assert_eq!(active.as_ref(), Some(want));
    assert_eq!(
        h.state().marks_of(want).unwrap().pick,
        Some(shoebox_core::marks::Pick::Pick)
    );
    assert_eq!(
        h.state().survey_names().0.len(),
        3,
        "marking keeps the survey"
    );
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "16-survey");
    h.get_all_by_label("Remove from survey")
        .next()
        .unwrap()
        .click();
    h.run_steps(2);
    assert_eq!(h.state().survey_names().0.len(), 2);
}

/// `name → (size, mtime)` for every file under `dir` (no reading: cheap on
/// a large library, and enough to prove nothing was touched).
fn listing(dir: &Path) -> std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
    let mut out = std::collections::BTreeMap::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let md = e.metadata().unwrap();
            if md.is_dir() {
                dirs.push(p);
            } else if e.file_name() != ".DS_Store" {
                out.insert(
                    p.strip_prefix(dir).unwrap().to_path_buf(),
                    (md.len(), md.modified().unwrap()),
                );
            }
        }
    }
    out
}

/// The `_undated` clean-up against real files, in the app. Opt-in:
/// `SHOEBOX_UI_REF=<APFS clone of the library>` (reference, read-only) and
/// `SHOEBOX_UI_UNDATED=<folder holding _undated/>`. Run with `--release
/// --ignored`. Applies the auto-selected moves, checks the reference
/// listing is unchanged, then undoes everything.
#[test]
#[ignore = "needs SHOEBOX_UI_REF and SHOEBOX_UI_UNDATED"]
fn real_undated_cleanup() {
    use shoebox_core::review::Decision;
    let (Some(lib), Some(scan)) = (
        std::env::var_os("SHOEBOX_UI_REF").map(PathBuf::from),
        std::env::var_os("SHOEBOX_UI_UNDATED").map(PathBuf::from),
    ) else {
        eprintln!("skipping: set SHOEBOX_UI_REF and SHOEBOX_UI_UNDATED");
        return;
    };
    let (lib, scan) = (lib.canonicalize().unwrap(), scan.canonicalize().unwrap());
    let lib_before = listing(&lib);
    let scan_before = listing(&scan);
    let t = tempfile::tempdir().unwrap();
    let p = paths(t.path(), &[lib.clone(), scan.clone()]);
    let mut s = Settings::load(&p.settings).unwrap();
    s.set_reference(&lib, true);
    s.save(&p.settings).unwrap();
    let mut h = harness(p, egui::vec2(1600.0, 1000.0));
    settle(&mut h, Duration::from_secs(600));

    h.get_by_label("Duplicates").click();
    h.run_steps(2);
    let start = Instant::now();
    h.get_by_label("Find duplicates").click();
    wait_for(&mut h, Duration::from_secs(1800), "duplicates", |a| {
        a.dupes_idle()
    });
    eprintln!("find: {:.1} s", start.elapsed().as_secs_f64());
    h.get_by_label("Auto-select all").click();
    h.run_steps(2);
    settle(&mut h, Duration::from_secs(120));
    snapshot(&mut h, "real-1-duplicates");

    let rows = h.state().review_rows();
    let scan_rows: Vec<_> = rows.iter().filter(|r| r.0.starts_with(&scan)).collect();
    let mut by_decision: std::collections::BTreeMap<String, usize> = Default::default();
    for (path, d, how, keeper) in &scan_rows {
        *by_decision.entry(format!("{d:?}")).or_default() += 1;
        eprintln!(
            "{:?}\t{}\t{how}\t{}",
            d,
            path.strip_prefix(&scan).unwrap().display(),
            keeper
                .strip_prefix(&lib)
                .or_else(|_| keeper.strip_prefix(&scan))
                .unwrap()
                .display()
        );
    }
    eprintln!(
        "groups shown: {:?}; scan files: {by_decision:?}",
        h.state().dupe_groups()
    );
    let n = scan_rows.iter().filter(|r| r.1 == Decision::Move).count();
    assert!(n > 0, "something to move");

    h.get_by_label(&format!("Apply… ({n} to move)")).click();
    wait_for(&mut h, Duration::from_secs(60), "dry run", |a| {
        a.dupe_preview().is_some()
    });
    assert!(matches!(h.state().dupe_preview(), Some(Ok(_))));
    h.run_steps(2);
    snapshot(&mut h, "real-2-dry-run");
    assert_eq!(
        listing(&scan),
        scan_before,
        "nothing moved before confirming"
    );
    let label = if n == 1 {
        "Move 1 file".to_owned()
    } else {
        format!("Move {n} files")
    };
    h.get_by_label(&label).click();
    wait_for(&mut h, Duration::from_secs(600), "apply", |a| {
        a.dupe_message().is_some() && a.dupes_idle()
    });
    let after = listing(&scan);
    let moved = after
        .keys()
        .filter(|k| k.starts_with("_duplicates"))
        .count();
    let left: Vec<_> = after.keys().filter(|k| k.starts_with("_undated")).collect();
    eprintln!("moved {moved}; left in _undated: {left:?}");
    assert_eq!(moved, n);
    assert_eq!(listing(&lib), lib_before, "reference library untouched");
    settle(&mut h, Duration::from_secs(120));
    snapshot(&mut h, "real-3-after");

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(600), "undo", |_| {
        listing(&scan)
            .keys()
            .filter(|k| k.starts_with("_undated"))
            .count()
            == scan_before.len()
    });
    let restored: Vec<_> = listing(&scan)
        .into_iter()
        .filter(|(k, _)| k.starts_with("_undated"))
        .collect();
    let original: Vec<_> = scan_before.into_iter().collect();
    assert_eq!(
        restored
            .iter()
            .map(|(k, (s, _))| (k, s))
            .collect::<Vec<_>>(),
        original
            .iter()
            .map(|(k, (s, _))| (k, s))
            .collect::<Vec<_>>(),
        "undo restores every file"
    );
    assert_eq!(listing(&lib), lib_before, "reference library untouched");
}

/// M5: import a card in the app (dry-run table, then verified copies with
/// the default names), then undo it from History.
#[test]
fn import_in_the_app_and_undo() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let fx = base.join("fixtures");
    shoebox_fixtures::build_library(&fx).unwrap();
    let card = fx.join("ref/2026"); // two NEFs and a JPG with EXIF
    let lib = base.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    // A photo so the library isn't empty.
    std::fs::copy(fx.join("scan/recovered/f0005.jpg"), lib.join("f0005.jpg")).unwrap();
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Import…").click();
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Import);
    h.state_mut().set_import_source(card.clone());
    h.run_steps(2);
    h.get_by_label("Find photos").click();
    wait_for(&mut h, Duration::from_secs(30), "import plan", |a| {
        a.import_counts().is_some()
    });
    assert_eq!(h.state().import_counts(), Some((3, 0)));
    h.run_steps(2);
    snapshot(&mut h, "17-import-plan");
    assert!(
        !lib.join("2026").exists(),
        "nothing copied before confirming"
    );
    h.get_by_label("Import 3 files").click();
    wait_for(&mut h, Duration::from_secs(30), "import", |a| {
        a.import_message().is_some()
    });
    let d = lib.join("2026/2026-04-24");
    for f in [
        "20260424-140311-42.nef",
        "20260424-140311-42.jpg",
        "20260424-140502-07.nef",
    ] {
        assert!(d.join(f).exists(), "{f}");
    }
    wait_for(&mut h, Duration::from_secs(30), "catalog", |a| {
        a.photo_count() == 4
    });

    // Importing again finds nothing new.
    h.get_by_label("Find photos").click();
    wait_for(&mut h, Duration::from_secs(30), "re-plan", |a| {
        a.import_counts().is_some()
    });
    assert_eq!(h.state().import_counts(), Some((0, 3)));

    // Undo from History: the copies go to the (test) trash.
    h.get_by_label("History").click();
    wait_for(&mut h, Duration::from_secs(10), "history", |a| {
        !a.batches().is_empty()
    });
    h.run_steps(2);
    snapshot(&mut h, "18-history");
    // (The toolbar has an Undo too; the History row's comes after it.)
    h.get_all_by_label("Undo").last().unwrap().click();
    wait_for(&mut h, Duration::from_secs(30), "undo import", |_| {
        !d.join("20260424-140311-42.nef").exists()
    });
    assert_eq!(std::fs::read_dir(base.join("trash")).unwrap().count(), 3);
    assert!(
        card.join("2026-04-24/DSC_0001.NEF").exists(),
        "the card is untouched"
    );
}

/// M5: rename, new folder + move, reject (Cmd+Backspace), a collection, and
/// undo; each through the dry-run table.
#[test]
fn organise_files_in_the_app() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let fx = base.join("fixtures");
    shoebox_fixtures::build_library(&fx).unwrap();
    let lib = fx.join("scan");
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    let preview = |h: &mut Harness<'_, ShoeboxApp>| {
        wait_for(h, Duration::from_secs(10), "dry run", |a| {
            a.org_preview().is_some()
        });
        h.run_steps(2);
    };

    // Rename f0004 (it has EXIF) with the default template.
    h.get_by_label("f0004.jpg").click_secondary();
    h.run_steps(2);
    h.get_by_label("Rename…").click();
    h.run_steps(2);
    snapshot(&mut h, "19-rename-dialog");
    h.get_by_label("Preview…").click();
    preview(&mut h);
    assert_eq!(h.state().org_preview(), Some(Ok(1)));
    snapshot(&mut h, "20-rename-preview");
    h.get_by_label("Rename 1 file").click();
    let renamed = lib.join("recovered/20260424-140311-42.jpg");
    wait_for(&mut h, Duration::from_secs(30), "rename", |_| {
        renamed.exists()
    });
    assert!(!lib.join("recovered/f0004.jpg").exists());

    // New folder, then move f0003 there.
    let keep = lib.join("keepers");
    h.state().new_folder(keep.clone());
    wait_for(&mut h, Duration::from_secs(10), "folder", |_| keep.is_dir());
    h.state_mut().select_names(&["f0003.jpg"]);
    h.state_mut().move_selection_to(keep.clone());
    preview(&mut h);
    h.get_by_label("Move 1 file").click();
    wait_for(&mut h, Duration::from_secs(30), "move", |_| {
        keep.join("f0003.jpg").exists()
    });

    // Reject f0001 (it has a sidecar) and send it to _rejected with ⌘⌫.
    h.state_mut().select_names(&["f0001.jpg"]);
    h.key_press(egui::Key::X);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Backspace);
    preview(&mut h);
    assert_eq!(h.state().org_preview(), Some(Ok(2)), "image and sidecar");
    h.get_by_label("Move to _rejected 2 files").click();
    let rej = lib.join("_rejected/recovered");
    wait_for(&mut h, Duration::from_secs(30), "reject", |_| {
        rej.join("f0001.jpg").exists() && rej.join("f0001.jpg.xmp").exists()
    });

    // A collection of two photos filters the grid and lives in the root.
    h.state_mut().select_names(&["f0005.jpg", "f0006.jpg"]);
    h.state_mut().new_collection("Best");
    wait_for(&mut h, Duration::from_secs(10), "collection", |_| {
        lib.join(".shoebox/collections.json").exists()
    });
    h.run_steps(2);
    h.get_by_label("Best  2").click();
    h.run_steps(2);
    assert_eq!(h.state().shown_count(), 2);
    snapshot(&mut h, "21-collection");

    // Undo the reject and the rename from History; the move with ⌘Z order.
    wait_for(&mut h, Duration::from_secs(10), "batches", |a| {
        a.batches().len() >= 5
    });
    let batches = h.state().batches();
    let id = |label: &str| batches.iter().find(|b| b.1.contains(label)).unwrap().0;
    let (rename, reject) = (id("rename"), id("rejects"));
    h.state_mut().undo_batch(reject);
    wait_for(&mut h, Duration::from_secs(30), "undo reject", |_| {
        lib.join("recovered/f0001.jpg.xmp").exists()
    });
    h.state_mut().undo_batch(rename);
    wait_for(&mut h, Duration::from_secs(30), "undo rename", |_| {
        lib.join("recovered/f0004.jpg").exists()
    });
    assert!(!renamed.exists());
}

/// An operation interrupted in an earlier session is flagged at startup,
/// and History's Resume finishes it.
#[test]
fn interrupted_operation_resumes_from_history() {
    use shoebox_core::fileops::{Journal, Op};
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let fx = base.join("fixtures");
    shoebox_fixtures::build_library(&fx).unwrap();
    let lib = fx.join("scan");
    let p = paths(&base, std::slice::from_ref(&lib));
    // A move journaled but never run: the app "crashed" before it started.
    let (src, dst) = (lib.join("recovered/f0005.jpg"), lib.join("keep/f0005.jpg"));
    {
        let j = Journal::open(&p.journal).unwrap();
        let b = j.begin_batch("move", "move to keep", None).unwrap();
        j.record(b, 0, Op::Mkdir, &lib.join("keep"), &lib.join("keep"), None)
            .unwrap();
        j.record(b, 1, Op::Move, &src, &dst, None).unwrap();
    }
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    assert!(
        h.query_by_label("An operation was interrupted: review in History")
            .is_some(),
        "flagged in the status bar"
    );
    h.get_by_label("An operation was interrupted: review in History")
        .click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::History);
    snapshot(&mut h, "22-interrupted");
    h.get_by_label("Resume").click();
    wait_for(&mut h, Duration::from_secs(30), "resume", |_| dst.exists());
    assert!(!src.exists());
    wait_for(&mut h, Duration::from_secs(10), "status", |a| {
        a.batches().iter().all(|b| b.2 == "done")
    });
}

/// M6 Timeline: days with their stacks under a heat strip; clicking a day
/// jumps to it in Grid.
#[test]
fn timeline_groups_days_and_jumps_to_grid() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    h.key_press(egui::Key::T);
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Timeline);
    let (days, undated) = h.state().timeline_days().unwrap();
    // The fixture shoots on one day (two frames, a camera JPG copy and a
    // burst neighbour, stacked); everything else has no date.
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].0, "Fri 24 Apr 2026");
    assert!(undated > 0);
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "23-timeline");

    h.get_by_label("Fri 24 Apr 2026").click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Grid);
    let current = h.state().current_name().unwrap();
    assert!(
        current.starts_with("DSC_0001") || current.starts_with("f00"),
        "{current}"
    );
    assert_eq!(h.state().selected_count(), 1);
}

/// M6 Map: located photos become markers (nearby ones grouped); clicking a
/// marker selects its photos; Show on Map from the Info panel. No tiles
/// (no network) in tests.
#[test]
fn map_places_located_photos() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let shot = shoebox_fixtures::Shot {
        datetime: "2026:06:01 12:00:00",
        subsec: "90",
        orientation: 1,
        serial: "1",
    };
    let geo = lib.join("scan/geo");
    std::fs::create_dir_all(&geo).unwrap();
    for (i, (name, lat, lon)) in [
        ("sydney-a.jpg", -33.85678, 151.21530),
        ("sydney-b.jpg", -33.85588, 151.21530), // 100 m away
        ("paris.jpg", 48.85826, 2.29450),
    ]
    .into_iter()
    .enumerate()
    {
        let img = shoebox_fixtures::scene(10 + i as u64, 600, 400);
        let jpg = shoebox_fixtures::jpeg(&img, 85).unwrap();
        let s = shoebox_fixtures::Shot {
            subsec: ["10", "20", "30"][i],
            ..shot.clone()
        };
        std::fs::write(
            geo.join(name),
            shoebox_fixtures::with_exif_gps(&jpg, &s, Some((lat, lon))),
        )
        .unwrap();
    }
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Map").click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Map);
    assert_eq!(h.state().located_count(), 3);
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "24-map");
    // Fitted to Sydney and Paris, the two Sydney photos share a marker.
    assert!(h.query_by_label("paris.jpg").is_some());
    let sydney: Vec<_> = ["sydney-a.jpg", "sydney-b.jpg"]
        .iter()
        .filter(|n| h.query_by_label(n).is_some())
        .collect();
    assert_eq!(sydney.len(), 1, "one marker for both Sydney photos");
    h.get_by_label(sydney[0]).click();
    h.run_steps(2);
    assert_eq!(h.state().selected_count(), 2);

    // Info panel → Show on Map centres on one photo, close enough to split
    // the Sydney pair.
    h.key_press(egui::Key::G);
    h.run_steps(2);
    h.state_mut().select_names(&["paris.jpg"]);
    h.run_steps(2);
    h.get_by_label("Show on Map").click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Map);
    assert!(h.query_by_label("paris.jpg").is_some());
}

/// The Settings window: opened with ⌘,, each change saved to settings.json
/// as it's made, invalid templates not saved, Escape closes it.
#[test]
fn settings_window() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let mut p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    // Tiles "on" but the map is never shown, so nothing is fetched.
    p.tile_cache = Some(t.path().join("tiles"));
    let file = p.settings.clone();
    let saved = move || Settings::load(&file).unwrap();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    assert!(h.state().map_tiles_on());

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    h.run_steps(2);
    assert!(h.state().settings_open());
    snapshot(&mut h, "settings-1-library");

    // Library: mark the scan folder as Reference, then back.
    let scan = std::fs::canonicalize(lib.join("scan")).unwrap();
    let is_ref = move |a: &ShoeboxApp| {
        a.settings()
            .roots
            .iter()
            .any(|r| r.path == scan && r.reference)
    };
    h.get_all_by_label("Reference").last().unwrap().click();
    wait_for(
        &mut h,
        Duration::from_secs(30),
        "scan marked reference",
        |a| is_ref(a),
    );
    h.get_all_by_label("Reference").last().unwrap().click();
    wait_for(
        &mut h,
        Duration::from_secs(30),
        "scan writable again",
        |a| !is_ref(a),
    );

    // Sidecars.
    h.get_by_label("Sidecars").click();
    h.run_steps(2);
    h.get_by_label("Write ratings, flags and labels to every file of a photo")
        .click();
    h.run_steps(2);
    assert!(h.state().settings().marks_to_all_stack_members);
    snapshot(&mut h, "settings-2-sidecars");

    // Naming: an unknown token isn't saved; a valid template is.
    h.get_by_label("Naming").click();
    h.run_steps(2);
    let replace = |h: &mut Harness<'_, ShoeboxApp>, text: &str| {
        // The import template's field (the first starting with a year folder).
        h.get_all_by_role(egui::accesskit::Role::TextInput)
            .find(|n| n.value().is_some_and(|v| v.starts_with("{YYYY}/")))
            .unwrap()
            .click();
        h.run_steps(1);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text(text.into()));
        h.run_steps(2);
    };
    replace(&mut h, "{YYYY}/{nope}.{ext}");
    snapshot(&mut h, "settings-3-naming-invalid");
    assert_eq!(h.state().settings().import_template, "");
    replace(&mut h, "{YYYY}/{YYYYMMDD}-{seq}.{ext}");
    assert_eq!(
        h.state().settings().import_template,
        "{YYYY}/{YYYYMMDD}-{seq}.{ext}"
    );

    // Map tiles off, and memory.
    // (The toolbar has a Map button too; the window's tab comes last.)
    h.get_all_by_label("Map").last().unwrap().click();
    h.run_steps(2);
    h.get_by_label("Load map tiles from OpenStreetMap").click();
    h.run_steps(2);
    assert!(h.state().settings().offline_map);
    assert!(!h.state().map_tiles_on());
    h.get_by_label("Updates").click();
    h.run_steps(2);
    assert!(
        h.query_by_label_contains("doesn't update itself").is_some(),
        "tests have no updater"
    );
    snapshot(&mut h, "settings-4-updates");

    // Everything reached settings.json.
    wait_for(&mut h, Duration::from_secs(10), "settings saved", |_| {
        let s = saved();
        s.offline_map && s.marks_to_all_stack_members
    });
    let s = saved();
    assert_eq!(s.import_template, "{YYYY}/{YYYYMMDD}-{seq}.{ext}");
    assert_eq!(s.roots.len(), 2);
    assert!(s.roots.iter().all(|r| !r.reference));

    // Escape closes it; keys reach the grid again.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().settings_open());
}

/// A stand-in for SigLIP: an image's embedding is its average colour (minus
/// grey), and "red", "green" and "blue" embed as those colours.
struct ColourModel;

static EMBEDDED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn unit(v: [f32; 3]) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.iter().map(|x| x / n).collect()
}

impl shoebox_app::semantic::Model for ColourModel {
    fn embed_images(&mut self, images: &[shoebox_decode::Rgb8]) -> Result<Vec<Vec<f32>>, String> {
        EMBEDDED.fetch_add(images.len(), std::sync::atomic::Ordering::Relaxed);
        Ok(images
            .iter()
            .map(|img| {
                let mut sum = [0f32; 3];
                for px in img.px.as_chunks::<3>().0 {
                    for c in 0..3 {
                        sum[c] += f32::from(px[c]);
                    }
                }
                let m = (sum[0] + sum[1] + sum[2]) / 3.0;
                unit([sum[0] - m, sum[1] - m, sum[2] - m])
            })
            .collect())
    }

    fn embed_text(&mut self, text: &str) -> Result<Vec<f32>, String> {
        match text {
            "red" => Ok(unit([1.0, -0.5, -0.5])),
            "green" => Ok(unit([-0.5, 1.0, -0.5])),
            "blue" => Ok(unit([-0.5, -0.5, 1.0])),
            _ => Err(format!("the test model doesn't know \"{text}\"")),
        }
    }

    fn labels(&self) -> Vec<shoebox_ml::Label> {
        ["red", "green", "blue"]
            .into_iter()
            .map(|name| shoebox_ml::Label {
                name,
                prompt: name.to_owned(),
            })
            .collect()
    }
}

/// Semantic search end to end with a test model: indexing, a text query
/// (best match first, non-matches hidden), Find Similar, and embeddings
/// reused (not recomputed) after a restart.
#[test]
fn semantic_search_in_the_app() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    for (name, rgb) in [
        ("red.jpg", [210, 30, 30]),
        ("reddish.jpg", [180, 90, 70]),
        ("green.jpg", [30, 200, 40]),
        ("greenish.jpg", [70, 170, 90]),
        ("blue.jpg", [30, 40, 210]),
    ] {
        let img = image::RgbImage::from_pixel(64, 48, image::Rgb(rgb));
        std::fs::write(lib.join(name), shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
    }
    let mut p = paths(t.path(), std::slice::from_ref(&lib));
    p.model = shoebox_app::semantic::ModelSource::Fixed(|| Box::new(ColourModel));
    let mut s = Settings::load(&p.settings).unwrap();
    s.semantic_search = true;
    s.save(&p.settings).unwrap();

    let mut h = harness(p.clone(), egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    wait_for(&mut h, Duration::from_secs(30), "indexing", |a| {
        let s = a.semantic_status();
        s.ready() && s.indexed == 5
    });
    assert_eq!(EMBEDDED.load(std::sync::atomic::Ordering::Relaxed), 5);

    // Search by content.
    h.get_by_label("Content").click();
    h.run_steps(2);
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .unwrap()
        .click();
    h.run_steps(1);
    h.event(egui::Event::Text("red".into()));
    wait_for(&mut h, Duration::from_secs(10), "red results", |a| {
        a.shown_count() == 2
    });
    assert_eq!(h.state().shown_names(), ["red.jpg", "reddish.jpg"]);
    assert!(h.query_by_label("Best match first").is_some());
    snapshot(&mut h, "semantic-1-red");

    // A query the model can't embed shows why, and no photos.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("teal".into()));
    wait_for(&mut h, Duration::from_secs(10), "error", |a| {
        a.shown_count() == 0
    });
    assert!(h.query_by_label_contains("doesn't know").is_some());

    // Back to names: everything again.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Backspace);
    h.get_by_label("Content").click();
    wait_for(&mut h, Duration::from_secs(10), "all photos", |a| {
        a.shown_count() == 5
    });

    // Find Similar from the context menu.
    h.get_by_label("green.jpg").click_secondary();
    h.run_steps(3);
    h.get_by_label("Find Similar Photos").click();
    wait_for(&mut h, Duration::from_secs(10), "similar photos", |a| {
        a.shown_count() == 2
    });
    assert_eq!(h.state().shown_names(), ["green.jpg", "greenish.jpg"]);
    assert!(h.query_by_label("Photos like green.jpg").is_some());
    snapshot(&mut h, "semantic-2-similar");
    h.get_by_label("✖").click();
    wait_for(&mut h, Duration::from_secs(10), "all photos", |a| {
        a.shown_count() == 5
    });

    // Suggested tags: Things lists them with counts; one shows its photos.
    wait_for(&mut h, Duration::from_secs(10), "tags", |a| {
        a.tags_of("reddish.jpg") == ["red"]
    });
    assert_eq!(h.state().tags_of("blue.jpg"), ["blue"]);
    h.run_steps(3); // the toolbar's height changed with the chip gone
    h.get_by_label("red  2").click();
    wait_for(&mut h, Duration::from_secs(10), "tagged red", |a| {
        a.shown_count() == 2
    });
    assert_eq!(h.state().shown_names(), ["red.jpg", "reddish.jpg"]);
    snapshot(&mut h, "semantic-3-things");
    // Accepting a suggestion makes it a keyword (in the sidecar) and it's
    // no longer suggested.
    h.get_by_label("reddish.jpg").click();
    h.run_steps(2);
    h.get_by_label("+ red").click();
    wait_for(&mut h, Duration::from_secs(10), "keyword", |a| {
        a.marks_of("reddish.jpg")
            .is_some_and(|m| m.keywords == ["red"])
    });
    assert!(h.state().tags_of("reddish.jpg").is_empty());
    assert!(lib.join("reddish.jpg.xmp").exists());
    h.get_by_label("✖").click();
    wait_for(&mut h, Duration::from_secs(10), "all photos", |a| {
        a.shown_count() == 5
    });
    drop(h);

    // A restart reuses the stored embeddings.
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    wait_for(&mut h, Duration::from_secs(30), "index loaded", |a| {
        let s = a.semantic_status();
        s.ready() && s.indexed == 5
    });
    assert_eq!(
        EMBEDDED.load(std::sync::atomic::Ordering::Relaxed),
        5,
        "nothing recomputed"
    );
}

/// Semantic search with the real model, on a folder of your own test photos
/// (copied; the originals are only read). Needs a downloaded model folder:
/// `cargo run --release -p shoebox-ml --example fetch -- <dir> <staging>`, then
/// `SHOEBOX_ML_MODEL=<dir> SHOEBOX_UI_PHOTOS=<folder> SHOEBOX_UI_QUERIES='a cat=cat.jpg;…'
/// cargo test --release -p shoebox-app --test ui -- --ignored real_model`
/// (each query lists the file it should put first).
#[test]
#[ignore]
fn semantic_search_real_model() {
    let (Some(model), Some(photos)) = (
        std::env::var_os("SHOEBOX_ML_MODEL"),
        std::env::var_os("SHOEBOX_UI_PHOTOS"),
    ) else {
        eprintln!("set SHOEBOX_ML_MODEL and SHOEBOX_UI_PHOTOS");
        return;
    };
    let queries = std::env::var("SHOEBOX_UI_QUERIES").unwrap_or_default();
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    for e in std::fs::read_dir(&photos).unwrap() {
        let p = e.unwrap().path();
        if p.is_file() {
            std::fs::copy(&p, lib.join(p.file_name().unwrap())).unwrap();
        }
    }
    let mut p = paths(t.path(), std::slice::from_ref(&lib));
    p.model = shoebox_app::semantic::ModelSource::Download {
        dir: PathBuf::from(model),
        staging: t.path().join("staging"),
    };
    let mut s = Settings::load(&p.settings).unwrap();
    s.semantic_search = true;
    s.save(&p.settings).unwrap();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    let start = Instant::now();
    wait_for(&mut h, Duration::from_secs(300), "indexing", |a| {
        let s = a.semantic_status();
        s.ready() && !s.indexing() && s.total > 0
    });
    let s = h.state().semantic_status().clone();
    eprintln!(
        "indexed {} photos ({} skipped) in {:.1} s",
        s.indexed,
        s.skipped,
        start.elapsed().as_secs_f32()
    );
    wait_for(&mut h, Duration::from_secs(60), "tags", |a| {
        a.shown_names().iter().any(|n| !a.tags_of(n).is_empty())
    });
    for n in h.state().shown_names() {
        eprintln!("tags {n}: {:?}", h.state().tags_of(&n));
    }
    h.get_by_label("Content").click();
    h.run_steps(2);
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .unwrap()
        .click();
    for (i, q) in queries.split(';').filter(|q| !q.is_empty()).enumerate() {
        let (text, best) = q.split_once('=').unwrap();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text(text.into()));
        let start = Instant::now();
        wait_for(&mut h, Duration::from_secs(30), text, |a| {
            a.shown_names().first().is_some_and(|n| n == best)
        });
        eprintln!(
            "\"{text}\": {:?} ({:.0} ms incl. typing pause)",
            h.state().shown_names(),
            start.elapsed().as_secs_f64() * 1e3
        );
        snapshot(&mut h, &format!("semantic-real-{i}"));
    }
}
