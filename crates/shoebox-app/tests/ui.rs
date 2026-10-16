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
    let p = paths(t.path(), &[lib]);
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
