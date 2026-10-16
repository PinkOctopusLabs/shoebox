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
