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
        faces: shoebox_app::people::FaceSource::Download {
            dir: tmp.join("data/models/faces"),
            staging: tmp.join("cache/model-download"),
        },
        // The object model: a real one when SHOEBOX_SAM_MODEL names it,
        // else an empty folder (never downloaded in tests).
        objects: (
            std::env::var_os("SHOEBOX_SAM_MODEL")
                .map_or_else(|| tmp.join("data/models/sam"), PathBuf::from),
            tmp.join("cache/model-download"),
        ),
        // The AI denoise model: a real one when SHOEBOX_RAWNIND_MODEL names
        // it, else an empty folder.
        denoise: (
            std::env::var_os("SHOEBOX_RAWNIND_MODEL")
                .map_or_else(|| tmp.join("data/models/rawnind"), PathBuf::from),
            tmp.join("cache/model-download"),
        ),
        // The landscape model: a real one when SHOEBOX_SKY_MODEL names it,
        // else an empty folder.
        landscape: (
            std::env::var_os("SHOEBOX_SKY_MODEL")
                .map_or_else(|| tmp.join("data/models/scene"), PathBuf::from),
            tmp.join("cache/model-download"),
        ),
        // The object removal model: a real one when SHOEBOX_INPAINT_MODEL
        // names it, else an empty folder.
        inpaint: (
            std::env::var_os("SHOEBOX_INPAINT_MODEL")
                .map_or_else(|| tmp.join("data/models/inpaint"), PathBuf::from),
            tmp.join("cache/model-download"),
        ),
        // The depth model: a real one when SHOEBOX_DEPTH_MODEL names it,
        // else an empty folder.
        depth: (
            std::env::var_os("SHOEBOX_DEPTH_MODEL")
                .map_or_else(|| tmp.join("data/models/depth"), PathBuf::from),
            tmp.join("cache/model-download"),
        ),
        people: tmp.join("data/people.json"),
        raw_cache: tmp.join("cache"),
        export_presets: tmp.join("data/export-presets.json"),
        // A fake Photos library: tests never touch PhotoKit.
        photos: std::sync::Arc::new(shoebox_core::photos::fake::FakeSource::new()),
        photos_staging: tmp.join("cache/photos-staging"),
    }
}

/// At most this many apps at once (half the cores, at least four): every
/// test runs a GPU renderer, worker threads and a library, and with all of
/// them at once the slowest missed its timeouts.
fn slots() -> usize {
    std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(4))
}

static RUNNING: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static FREED: std::sync::Condvar = std::sync::Condvar::new();

/// A running app's slot, given back when its test's thread ends.
struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        *RUNNING.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        FREED.notify_one();
    }
}

thread_local! {
    static SLOT: std::cell::RefCell<Option<Slot>> = const { std::cell::RefCell::new(None) };
}

/// Wait for a slot (once per test).
fn take_slot() {
    if SLOT.with(|s| s.borrow().is_some()) {
        return;
    }
    let mut n = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    while *n >= slots() {
        n = FREED.wait(n).unwrap_or_else(|e| e.into_inner());
    }
    *n += 1;
    drop(n);
    SLOT.with(|s| *s.borrow_mut() = Some(Slot));
}

fn harness<'a>(paths: AppPaths, size: egui::Vec2) -> Harness<'a, ShoeboxApp> {
    take_slot();
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
    // The biggest fixture library (NEFs too), loading while every other UI
    // test runs: give it longer.
    settle(&mut h, Duration::from_secs(120));
    assert_eq!(h.state().photo_count(), 17); // 7 reference + 10 scan images
    // The library panel counts photos (stacks), as the grid shows them.
    let photos = h.state().shown_count();
    assert!(photos < 17, "the fixtures stack NEF + JPG");
    assert!(
        h.query_by_label(&format!("All photos  {photos}")).is_some(),
        "All photos counts stacks"
    );
    snapshot(&mut h, "1-grid");

    // Left-panel items toggle: a second click shows everything again.
    h.get_by_label("Recently changed").click();
    h.run_steps(2);
    assert!(h.state().filtered());
    h.get_by_label("Recently changed").click();
    h.run_steps(2);
    assert!(!h.state().filtered(), "Recently changed deselects");
    h.get_by_label(&format!("scan  {}", h.state().folder_count("scan")))
        .click();
    h.run_steps(2);
    assert!(h.state().filtered());
    h.get_by_label(&format!("scan  {}", h.state().folder_count("scan")))
        .click();
    h.run_steps(2);
    assert!(!h.state().filtered(), "a folder deselects");
    // …and what's narrowing the grid shows above it, with a ✖.
    h.get_by_label(&format!("scan  {}", h.state().folder_count("scan")))
        .click();
    h.run_steps(3);
    snapshot(&mut h, "1a-folder-chip");
    h.get_by_label("📁 scan  ✖").click();
    h.run_steps(2);
    // The side panels fold away from buttons at the top bar's ends; Tab
    // hides both, then shows both.
    assert!(h.query_by_label("Recently changed").is_some());
    h.get_by_label("Library panel").click();
    h.run_steps(3);
    assert!(
        h.query_by_label("Recently changed").is_none(),
        "left hidden"
    );
    for _ in 0..2 {
        h.key_press(egui::Key::Tab);
        h.run_steps(3);
    }
    assert!(h.query_by_label("Recently changed").is_some(), "left back");
    // A quick double-click on a top-bar button toggles it twice; it never
    // zooms the window (only the bar's empty space does; AppKit's own
    // title-bar handling is turned off in `platform::own_titlebar_clicks`).
    let mut zoomed = false;
    for _ in 0..2 {
        h.get_by_label("Info panel").click();
        h.step();
        zoomed |= h.output().viewport_output.values().any(|v| {
            v.commands
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::Maximized(_)))
        });
    }
    h.run_steps(2);
    assert!(!zoomed, "double-clicking a button zoomed the window");
    assert!(!h.state().filtered(), "the chip clears it");
    assert_eq!(h.state().shown_count(), photos);
    // The funnel in the top bar shows the rating, flag and label filters.
    assert!(h.query_by_label("More ⏷").is_none());
    h.get_by_label("Filters").click();
    h.run_steps(2);
    assert!(h.query_by_label("More ⏷").is_some());
    h.get_by_label("Filters").click();
    h.run_steps(2);
    assert!(h.query_by_label("More ⏷").is_none());

    // Right-click a photo: it becomes the selection and the menu offers
    // Reveal in Finder. (The item isn't clicked, so Finder doesn't open.)
    h.get_by_label("f0005.jpg").click_secondary();
    h.run_steps(3);
    assert_eq!(h.state().selected_count(), 1);
    // The view bar offers its marks (not clicked: the library stays as is).
    assert!(h.query_by_label("Rate 3").is_some() && h.query_by_label("Pick").is_some());
    // The menu's item, and the info panel's button beside the path.
    assert_eq!(
        h.query_all_by_label(REVEAL).count(),
        2,
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
    // A plain scroll (say, the grid's momentum) never zooms; it opens at Fit.
    for _ in 0..5 {
        wheel(&mut h, -120.0);
        h.step();
    }
    assert!(!h.state().loupe_zoomed(), "scrolling doesn't zoom");

    // Space toggles 100% and back.
    h.key_press(egui::Key::Space);
    settle(&mut h, Duration::from_secs(30));
    assert!(h.state().loupe_zoomed());
    h.key_press(egui::Key::Space);
    h.step();
    assert!(!h.state().loupe_zoomed());

    // Escape goes back to the grid it was opened from.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Grid);
    // And to Masonry, when opened from there with Enter.
    h.state_mut().set_view(View::Masonry);
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Loupe);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Masonry);

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
    // The info panel lists the stack's files; clicking the JPG shows it
    // (the stack opens so it has a tile of its own). Moving within it keeps
    // it open; another photo folds it.
    h.get_by_label("DSC_0001.NEF").click();
    h.run_steps(2);
    h.get_by_label("DSC_0001.JPG").click();
    h.run_steps(3);
    assert_eq!(h.state().current_name().as_deref(), Some("DSC_0001.JPG"));
    let open = h.state().shown_count();
    assert!(open > 14, "the stack is open");
    h.get_all_by_label("DSC_0001.NEF").next().unwrap().click();
    h.run_steps(3);
    assert_eq!(h.state().shown_count(), open, "still open on its NEF");
    h.get_by_label("f0003.jpg").click();
    h.run_steps(3);
    assert_eq!(h.state().shown_count(), 14, "folded");

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

    // File type, from the filter bar: the library's types with their
    // counts; ticking one shows the stacks with a file of that type.
    h.state_mut()
        .set_filter(shoebox_core::query::Filter::default());
    h.run_steps(2);
    h.get_by_label("Filters").click();
    h.run_steps(2);
    h.get_by_value("Any type").click();
    h.run_steps(2);
    use egui_kittest::kittest::NodeT;
    let tif = h.get_by_label_contains("TIF  (");
    let n: usize = tif
        .accesskit_node()
        .label()
        .and_then(|l| l.split('(').nth(1)?.trim_end_matches(')').parse().ok())
        .unwrap();
    tif.click();
    h.run_steps(2);
    assert!(h.state().filtered());
    assert!(n > 0 && n < 14);
    assert_eq!(h.state().shown_count(), n);
    assert!(
        h.query_by_label_contains("JPG  (").is_some(),
        "the menu stays open for more"
    );
}

/// Several folders at once (⌘-click), photos added to an event from the
/// photo menu, and one source at a time: an event replaces the folders.
#[test]
fn folders_and_events() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    let all = h.state().shown_count();

    let folder = |h: &Harness<'_, ShoeboxApp>, name: &str| {
        format!("{name}  {}", h.state().folder_count(name))
    };
    let (blank, lightroom) = (folder(&h, "blank"), folder(&h, "lightroom"));
    h.get_by_label(&blank).click();
    h.run_steps(2);
    let one = h.state().shown_count();
    h.get_by_label(&lightroom)
        .click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(3);
    let two = h.state().shown_count();
    assert_eq!(h.state().scope().0, 2);
    assert!(one < two && two < all, "{one} < {two} < {all}");
    assert!(h.query_by_label("📁 2 folders  ✖").is_some());
    assert!(h.query_by_label(&format!("{two} photos")).is_some());
    snapshot(&mut h, "25-two-folders");
    h.get_by_label("📁 2 folders  ✖").click();
    h.run_steps(3);
    assert_eq!(h.state().shown_count(), all);

    // An event from one photo, then another added from the photo menu.
    h.get_by_label("DSC_0001.NEF").click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    h.run_steps(3);
    h.event(egui::Event::Text("Trip".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    let start = Instant::now();
    while h.state().event_count("Trip") == 0 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
        assert!(start.elapsed() < Duration::from_secs(10), "no event");
    }
    let before = h.state().event_count("Trip");
    let other = h
        .state()
        .shown_names()
        .into_iter()
        .find(|n| n != "DSC_0001.NEF")
        .unwrap();
    h.get_by_label(&other).click_secondary();
    h.run_steps(3);
    h.get_by_label("Add to Event").click();
    h.run_steps(3);
    h.get_by_label("Trip").click();
    let start = Instant::now();
    while h.state().event_count("Trip") == before {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
        assert!(start.elapsed() < Duration::from_secs(10), "not added");
    }
    settle(&mut h, Duration::from_secs(30));

    // Showing the event clears the folders.
    let n = h.state().event_count("Trip");
    h.get_by_label(&format!("Trip  {n}")).click();
    h.run_steps(3);
    assert_eq!(h.state().scope(), (0, Some("Trip".to_owned())));
    assert_eq!(h.state().shown_count(), n);
    // …and a folder clears the event.
    let blank = folder(&h, "blank");
    h.get_by_label(&blank).click();
    h.run_steps(3);
    assert_eq!(h.state().scope(), (1, None));

    // Picking a folder from the loupe goes back to the grid, nothing selected.
    let name = h.state().shown_names()[0].clone();
    h.get_by_label(&name).click();
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Loupe);
    let lightroom = folder(&h, "lightroom");
    h.get_by_label(&lightroom).click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Grid);
    assert_eq!(h.state().selected_count(), 0);
    assert_eq!(h.state().current_name(), None);

    // Show in All Photos, from the loupe: back to everything in the grid,
    // with that photo selected.
    let name = h.state().shown_names().last().unwrap().clone();
    h.get_by_label(&name).click();
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    h.get_by_label(&name).click_secondary();
    h.run_steps(3);
    h.get_by_label("Show in All Photos").click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Grid);
    assert_eq!(h.state().scope(), (0, None));
    assert_eq!(h.state().shown_count(), all);
    assert_eq!(h.state().current_name(), Some(name));
    assert_eq!(h.state().selected_count(), 1);
    snapshot(&mut h, "26-show-in-all");
}

/// Show in All Photos scrolls a long grid to the photo, from the grid and
/// from the loupe.
#[test]
fn show_in_all_photos_scrolls_to_it() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    let seed = t.path().join("seed.jpg");
    std::fs::write(
        &seed,
        shoebox_fixtures::jpeg(&shoebox_fixtures::scene(9, 300, 200), 85).unwrap(),
    )
    .unwrap();
    for d in 0..8 {
        let dir = lib.join(format!("day-{d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..60 {
            std::fs::copy(&seed, dir.join(format!("IMG_{d}_{f:02}.jpg"))).unwrap();
        }
    }
    let p = paths(t.path(), std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(120));
    let visible = |h: &Harness<'_, ShoeboxApp>, name: &str| {
        h.query_by_label(name)
            .is_some_and(|n| n.rect().min.y > 40.0 && n.rect().max.y < 860.0)
    };
    for (folder, name, loupe) in [
        ("day-6", "IMG_6_55.jpg", false),
        ("day-1", "IMG_1_03.jpg", true),
        ("day-7", "IMG_7_59.jpg", false),
    ] {
        h.get_by_label(&format!("{folder}  60")).click();
        h.run_steps(3);
        // The folder's grid shows its own photos; scroll to the one wanted.
        let pos = h
            .state()
            .shown_names()
            .iter()
            .position(|n| n == name)
            .unwrap();
        assert_eq!(h.state().shown_count(), 60);
        h.state_mut().select_names(&[name]);
        h.run_steps(3);
        assert!(visible(&h, name), "{name} in its folder");
        if loupe {
            h.key_press(egui::Key::Enter);
            h.run_steps(2);
        }
        h.get_by_label(name).click_secondary();
        h.run_steps(3);
        h.get_by_label("Show in All Photos").click();
        h.run_steps(3);
        assert_eq!(h.state().shown_count(), 480);
        assert_eq!(h.state().current_name().as_deref(), Some(name));
        h.run_steps(10); // and stays there
        assert!(
            visible(&h, name),
            "{name} (at {pos} in its folder) on screen"
        );
    }
}

/// Duplicates within a folder or an event: only its own photos are
/// compared; the "In" menu goes back to everything.
#[test]
fn duplicates_within_a_scope() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let lib = lib.canonicalize().unwrap();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    // Everything first.
    h.get_by_label("Duplicates").click();
    h.run_steps(2);
    h.get_by_label("Find duplicates").click();
    wait_for(&mut h, Duration::from_secs(60), "all", |a| a.dupes_idle());
    let all = h.state().dupe_files().len();
    assert_eq!(
        h.state().duplicates_found_in().as_deref(),
        Some("All photos")
    );

    // A folder's menu: only that folder's files (here a root: the scan
    // folder without the reference one).
    let scan = lib.join("scan");
    let label = format!("scan  {}", h.state().folder_count("scan"));
    h.get_by_label(&label).click_secondary();
    h.run_steps(3);
    h.get_by_label("Find Duplicates in This Folder").click();
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(60), "folder", |a| {
        a.dupes_idle() && a.duplicates_found_in().as_deref() == Some("scan")
    });
    assert_eq!(h.state().view_name(), View::Duplicates);
    let files = h.state().dupe_files();
    assert!(
        !files.is_empty() && files.len() < all,
        "{} of {all}",
        files.len()
    );
    assert!(files.iter().all(|f| f.starts_with(&scan)), "{files:?}");
    assert!(h.query_by_label_contains("in scan").is_some());
    snapshot(&mut h, "dupes-in-folder");

    // Larger thumbnails from the size slider in the bar under the list.
    let r = h
        .get_all_by_role(egui::accesskit::Role::Slider)
        .map(|n| n.rect())
        .max_by(|a, b| a.min.y.total_cmp(&b.min.y)) // not the header's
        .unwrap();
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(r.left_center()));
    h.event(button(r.left_center(), true));
    h.run_steps(1);
    h.event(egui::Event::PointerMoved(r.center()));
    h.run_steps(1);
    h.event(button(r.center(), false));
    h.run_steps(3);
    let tile = h.get_all_by_label("f0005.jpg").next().unwrap().rect();
    assert!(tile.width() > 200.0, "{tile:?}");
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "dupes-larger");

    // An event of one photo: only its stack's files (the NEF, its JPG and
    // f0004, an exact copy of the JPG), not the TIFF or other copies.
    h.state_mut().set_view(View::Grid);
    h.run_steps(2);
    h.get_by_label("DSC_0001.NEF").click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    h.run_steps(3);
    h.event(egui::Event::Text("Trip".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    wait_for(&mut h, Duration::from_secs(10), "event", |a| {
        a.event_count("Trip") > 0
    });
    let n = h.state().event_count("Trip");
    h.get_by_label(&format!("Trip  {n}")).click_secondary();
    h.run_steps(3);
    h.get_by_label("Find Duplicates in Event").click();
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(60), "event", |a| {
        a.dupes_idle() && a.duplicates_found_in().as_deref() == Some("Trip")
    });
    let files = h.state().dupe_files();
    assert!(!files.is_empty());
    assert!(
        files.iter().all(|f| {
            let name = f.file_name().unwrap().to_string_lossy();
            ["DSC_0001.NEF", "DSC_0001.JPG", "f0004.jpg"].contains(&name.as_ref())
        }),
        "{files:?}"
    );

    // Back to everything from the In menu.
    h.get_by_value("Trip").click(); // In: Trip
    h.run_steps(2);
    h.get_by_label("All photos").click();
    h.run_steps(2);
    h.get_by_label("Find again").click();
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(60), "all again", |a| {
        a.dupes_idle() && a.duplicates_found_in().as_deref() == Some("All photos")
    });
    assert_eq!(h.state().dupe_files().len(), all);
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

    // Membership is marked on each photo, not taken from dates or folders:
    // two events from photos taken a second apart in the same folder.
    let new_event = |h: &mut Harness<'_, ShoeboxApp>, photo: &str, name: &str| {
        h.get_by_label(photo).click();
        h.run_steps(2);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
        h.run_steps(3);
        h.event(egui::Event::Text(name.into()));
        h.run_steps(1);
        h.key_press(egui::Key::Enter);
        wait_for(h, Duration::from_secs(10), name, |a| {
            a.event_count(name) == 1
        });
    };
    new_event(&mut h, "f0001.jpg", "Wedding");
    new_event(&mut h, "f0010.jpg", "Party");
    assert_eq!(h.state().event_count("Wedding"), 1, "Party took nothing");
    let burst = lib.join("scan/recovered/f0010.jpg.xmp");
    assert_eq!(read(&burst).keywords, vec!["Events|Party"]);

    // Taken out again from the photo menu.
    h.get_by_label("f0010.jpg").click_secondary();
    h.run_steps(3);
    h.get_by_label("Remove from Event").click();
    h.run_steps(3);
    h.get_by_label("Party").click();
    wait_for(&mut h, Duration::from_secs(10), "removed", |a| {
        a.event_count("Party") == 0
    });
    settle(&mut h, Duration::from_secs(30));
    assert_eq!((read(&burst).event, read(&burst).keywords.len()), (None, 0));
    assert_eq!(h.state().event_count("Wedding"), 1);
}

/// Marks and events on a selection that includes reference photos: the
/// others are changed, the reference ones are left alone (read-only), and
/// the status bar says so; nothing fails.
#[test]
fn marks_skip_reference_photos() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let p = paths(t.path(), &[lib.join("ref"), lib.join("scan")]);
    let mut s = Settings::load(&p.settings).unwrap();
    s.set_reference(&lib.join("ref").canonicalize().unwrap(), true);
    s.save(&p.settings).unwrap();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("DSC_0001.NEF").click(); // in the reference root
    h.run_steps(2);
    h.get_by_label("f0005.jpg")
        .click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(h.state().selected_count(), 2);
    let read = |p: &Path| -> shoebox_core::marks::Marks {
        let props = std::fs::read(p)
            .ok()
            .and_then(|b| shoebox_core::xmp::read(&b).ok())
            .unwrap_or_default();
        shoebox_core::marks::Marks::from_props(&props)
    };
    let side = lib.join("scan/recovered/f0005.jpg.xmp");
    h.key_press(egui::Key::Num3);
    wait_for(&mut h, Duration::from_secs(10), "rated", |_| {
        read(&side).rating == 3
    });
    settle(&mut h, Duration::from_secs(30));
    assert_eq!(h.state().error_message(), None);
    assert!(
        h.state().notice().is_some_and(|n| n.contains("reference")),
        "{:?}",
        h.state().notice()
    );
    assert!(!lib.join("ref/2026/2026-04-24/DSC_0001.NEF.xmp").exists());

    // An event from the same selection takes the writable photo only.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    h.run_steps(3);
    h.event(egui::Event::Text("Trip".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    wait_for(&mut h, Duration::from_secs(10), "event", |a| {
        a.event_count("Trip") == 1
    });
    settle(&mut h, Duration::from_secs(30));
    assert_eq!(h.state().error_message(), None);
    assert_eq!(read(&side).keywords, vec!["Events|Trip"]);
    assert!(!lib.join("ref/2026/2026-04-24/DSC_0001.NEF.xmp").exists());
}

/// Step until `cond` holds (or fail after `timeout`). The whole suite runs
/// in parallel with GPU and export work, so the limit is three times the
/// one given: a passing test still returns as soon as `cond` holds.
fn wait_for(
    h: &mut Harness<'_, ShoeboxApp>,
    timeout: Duration,
    what: &str,
    cond: impl Fn(&ShoeboxApp) -> bool,
) {
    let timeout = timeout * 3;
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
    // Each match says how alike: 100% only for the same file or image data.
    assert!(
        h.query_all_by_label_contains("Same image data · 100%")
            .next()
            .is_some()
    );

    // The slider narrows the list without searching again: at its
    // strictest (99%), only the closest matches are left.
    let files = h.state().dupe_files().len();
    h.state_mut().set_dupe_threshold(0);
    wait_for(&mut h, Duration::from_secs(30), "regroup", |a| {
        a.dupes_idle()
    });
    assert!(h.state().dupe_files().len() < files);
    h.state_mut()
        .set_dupe_threshold(shoebox_core::dupes::DEFAULT_SIMILAR);
    wait_for(&mut h, Duration::from_secs(30), "regroup", |a| {
        a.dupes_idle()
    });
    assert_eq!(h.state().dupe_files().len(), files);

    // Auto-select at 100% (the default): the carved preview moves, the
    // resized copy waits.
    assert!(h.query_by_label("Only taken at the same moment").is_some());
    h.get_by_label("Auto-select all").click();
    h.run_steps(2);
    assert_eq!(h.state().decision_of("f0001.jpg"), Decision::Move);
    assert_eq!(h.state().decision_of("f0003.jpg"), Decision::Undecided);
    h.get_by_label("Clear decisions").click();
    h.run_steps(2);
    // Down to the lowest, taken at the same moment.
    h.state_mut()
        .set_dupe_auto(shoebox_core::review::AutoSelect {
            up_to: Some(shoebox_core::dupes::MAX_THRESHOLD),
            same_moment: true,
        });

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
        Decision::Move,
        "a resized copy with no capture time to tell it apart"
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
    h.state_mut()
        .set_dupe_auto(shoebox_core::review::AutoSelect {
            up_to: Some(shoebox_core::dupes::MAX_THRESHOLD),
            same_moment: true,
        });
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
    // Each left-out file shows beside the library photo it matched; one can
    // be copied anyway, and left out again.
    h.get_by_label("Already in the library (3)").click();
    h.run_steps(2);
    let nef = "2026-04-24/DSC_0001.NEF";
    h.get_by_label(&format!("Import anyway: {nef}")).click();
    wait_for(&mut h, Duration::from_secs(10), "forced", |a| {
        a.import_counts() == Some((1, 2))
    });
    settle(&mut h, Duration::from_secs(10));
    snapshot(&mut h, "17b-import-left-out");
    h.get_by_label(&format!("Leave out: {nef}")).click();
    wait_for(&mut h, Duration::from_secs(10), "left out again", |a| {
        a.import_counts() == Some((0, 3))
    });

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

/// Photos in the cloud only (Google Drive streaming and the like) are
/// listed without being read or downloaded; Download from Cloud, or opening
/// one in the loupe, fetches and reads it.
#[test]
fn cloud_only_photos() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("drive");
    std::fs::create_dir_all(&lib).unwrap();
    for (name, n) in [("a.jpg", 1u64), ("b.jpg", 2), ("c.jpg", 3), ("d.jpg", 4)] {
        let shot = shoebox_fixtures::Shot {
            datetime: Box::leak(format!("2025:06:01 10:00:{n:02}").into_boxed_str()),
            subsec: "",
            orientation: 1,
            serial: "1",
        };
        let img = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(n, 320, 240), 85).unwrap();
        std::fs::write(lib.join(name), shoebox_fixtures::with_exif(&img, &shot)).unwrap();
    }
    for name in ["b.jpg", "c.jpg", "d.jpg"] {
        shoebox_core::cloud::pretend_cloud_only(&lib.join(name));
    }
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1200.0, 800.0));
    settle(&mut h, Duration::from_secs(60));
    assert_eq!(h.state().photo_count(), 4);
    assert_eq!(h.state().cloud_state("a.jpg"), Some((false, true)));
    assert_eq!(h.state().cloud_state("b.jpg"), Some((true, false)));
    assert!(h.query_by_label_contains("3 in the cloud only").is_some());
    snapshot(&mut h, "cloud-1-grid");

    // Clicking one downloads just that one.
    h.get_by_label("b.jpg").click();
    wait_for(
        &mut h,
        Duration::from_secs(30),
        "downloaded on click",
        |a| a.cloud_state("b.jpg") == Some((false, true)),
    );
    assert_eq!(h.state().cloud_state("c.jpg"), Some((true, false)));
    assert!(h.query_by_label_contains("2 in the cloud only").is_some());

    // Selecting all doesn't download them; Download from Cloud in the
    // photo's menu does (the current photo, a, isn't in the cloud).
    h.get_by_label("a.jpg").click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(3);
    assert_eq!(h.state().cloud_state("c.jpg"), Some((true, false)));
    h.get_by_label("a.jpg").click_secondary();
    h.run_steps(3);
    h.get_by_label_contains("Download from Cloud").click();
    wait_for(
        &mut h,
        Duration::from_secs(30),
        "downloaded from the menu",
        |a| {
            a.cloud_state("c.jpg") == Some((false, true))
                && a.cloud_state("d.jpg") == Some((false, true))
        },
    );
    h.run_steps(3);
    assert!(h.query_by_label_contains("in the cloud only").is_none());
}

/// Import keeps the source's folder names: events nested as the folders
/// are, a keyword where chosen, nothing for dates; ⌘Z takes it back.
#[test]
fn import_folder_names() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let (src, lib) = (base.join("old"), base.join("library"));
    let put = |rel: &str, n: u64| {
        let shot = shoebox_fixtures::Shot {
            datetime: Box::leak(format!("2019:07:14 10:00:{n:02}").into_boxed_str()),
            subsec: "",
            orientation: 1,
            serial: "1",
        };
        let img = shoebox_fixtures::scene(n, 320, 240);
        let p = src.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let bytes = shoebox_fixtures::jpeg(&img, 85).unwrap();
        std::fs::write(p, shoebox_fixtures::with_exif(&bytes, &shot)).unwrap();
    };
    put("Trips/Italy 2019/a.jpg", 1);
    put("Trips/Italy 2019/Rome/b.jpg", 2);
    put("Saved/c.jpg", 3);
    put("2019/07/d.jpg", 4);
    // A photo so the library isn't empty.
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(
        lib.join("earlier.jpg"),
        shoebox_fixtures::jpeg(&shoebox_fixtures::scene(9, 320, 240), 85).unwrap(),
    )
    .unwrap();
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Import…").click();
    h.run_steps(2);
    h.state_mut().set_import_source(src.clone());
    h.run_steps(2);
    h.get_by_label("Find photos").click();
    wait_for(&mut h, Duration::from_secs(30), "import plan", |a| {
        a.import_counts().is_some()
    });
    h.run_steps(2);
    // Dates start as nothing, other names as events.
    use shoebox_core::import::FolderUse::{Event, Ignore};
    assert_eq!(
        h.state().import_folders(),
        [
            ("2019".to_owned(), Ignore),
            ("2019/07".to_owned(), Ignore),
            ("Saved".to_owned(), Event),
            ("Trips".to_owned(), Event),
            ("Trips/Italy 2019".to_owned(), Event),
            ("Trips/Italy 2019/Rome".to_owned(), Event),
        ]
    );
    // Each photo to copy shows as a thumbnail, rendered from the source
    // but not kept in the thumbnail cache.
    wait_for(&mut h, Duration::from_secs(30), "previews", |a| {
        a.import_previews() == 4
    });
    assert!(h.query_by_label("Trips/Italy 2019/Rome/b.jpg").is_some());
    // A folder's photo count shows just the photos its name goes on.
    h.get_by_label("Trips/Italy 2019: photos").click();
    h.run_steps(2);
    assert!(h.query_by_label("Trips/Italy 2019/a.jpg").is_some());
    assert!(h.query_by_label("Trips/Italy 2019/Rome/b.jpg").is_some());
    assert!(h.query_by_label("Saved/c.jpg").is_none());
    h.get_by_label("Show all").click();
    h.run_steps(2);
    assert!(h.query_by_label("Saved/c.jpg").is_some());
    fn cached(d: &Path) -> usize {
        std::fs::read_dir(d).map_or(0, |rd| {
            rd.filter_map(Result::ok)
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => cached(&e.path()),
                    _ => 1,
                })
                .sum()
        })
    }
    let thumbs = cached(&base.join("cache/thumbs"));
    h.get_by_label("Trips: Nothing").click();
    h.get_by_label("Saved: Keyword").click();
    h.run_steps(2);
    assert!(h.query_by_label("Event Italy 2019 › Rome").is_some());
    snapshot(&mut h, "import-folder-names");
    // While copying: a bar, files and bytes, speed, time left, the file
    // being copied, and Cancel (here and in the status bar).
    h.state_mut().show_copying(Some((
        1,
        4,
        3 << 30,
        12 << 30,
        src.join("Trips/Italy 2019/a.jpg"),
    )));
    h.run_steps(2);
    assert!(h.query_by_label("Cancel").is_some());
    assert!(h.query_by_label_contains("Importing 1/4").is_some());
    snapshot(&mut h, "import-copying");
    // Quitting while it copies asks first; keeping on importing carries on.
    h.state_mut().ask_quit();
    h.run_steps(2);
    assert!(h.state().asking_to_quit());
    assert!(h.query_by_label("An import is still copying").is_some());
    snapshot(&mut h, "import-quit");
    h.get_by_label("Keep Importing").click();
    h.run_steps(2);
    assert!(!h.state().asking_to_quit());
    h.state_mut().show_copying(None);
    h.run_steps(2);
    assert_eq!(
        cached(&base.join("cache/thumbs")),
        thumbs,
        "no previews cached"
    );
    h.get_by_label("Import 4 files").click();
    wait_for(&mut h, Duration::from_secs(30), "import", |a| {
        a.import_message().is_some()
    });
    wait_for(&mut h, Duration::from_secs(30), "marks", |a| {
        a.photo_count() == 5
            && a.marks_of("20190714-100002.jpg")
                .is_some_and(|m| !m.keywords.is_empty())
    });
    let kw = |h: &Harness<'_, ShoeboxApp>, f: &str| h.state().marks_of(f).unwrap().keywords;
    assert_eq!(kw(&h, "20190714-100001.jpg"), ["Events|Italy 2019"]);
    assert_eq!(kw(&h, "20190714-100002.jpg"), ["Events|Italy 2019|Rome"]);
    assert_eq!(kw(&h, "20190714-100003.jpg"), ["Saved"]);
    assert!(kw(&h, "20190714-100004.jpg").is_empty());
    let mut events = h.state().event_names();
    events.sort();
    assert_eq!(events, ["Italy 2019", "Rome"]);
    let dir = lib.join("2019/2019-07-14");
    assert!(dir.join("20190714-100001.jpg.xmp").exists());

    // One undo takes back the copies, their sidecars and the events.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(30), "undo", |a| {
        a.photo_count() == 1 && a.event_names().is_empty()
    });
    assert!(!dir.join("20190714-100001.jpg").exists());
    assert!(!dir.join("20190714-100001.jpg.xmp").exists());
    assert!(
        src.join("Trips/Italy 2019/a.jpg").exists(),
        "source untouched"
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
    // Out of it, and another in, from the photo menu.
    h.get_by_label("f0005.jpg").click_secondary();
    h.run_steps(3);
    h.get_by_label("Remove from Collection").click();
    h.run_steps(3);
    h.get_by_label("Best").click();
    wait_for(&mut h, Duration::from_secs(10), "removed", |a| {
        a.collection_count("Best") == 1
    });
    h.run_steps(2);
    assert_eq!(h.state().shown_count(), 1);
    h.get_by_label("Best  1").click(); // back to every photo
    h.run_steps(2);
    h.get_by_label("f0003.jpg").click_secondary();
    h.run_steps(3);
    h.get_by_label("Add to Collection").click();
    h.run_steps(3);
    h.get_by_label("Best").click();
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(10), "added", |a| {
        a.collection_count("Best") == 2
    });

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
    // Generous: the worker can be slow while other tests load the machine.
    wait_for(&mut h, Duration::from_secs(30), "settings saved", |_| {
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
    // A to Z, not by count: blue (1 photo) above red (2).
    let things: Vec<(String, usize)> = h.state().things();
    assert_eq!(
        things.first().map(|t| t.0.as_str()),
        Some("red"),
        "{things:?}"
    );
    let y = |h: &Harness<'_, ShoeboxApp>, (name, n): &(String, usize)| {
        h.get_by_label(&format!("{name}  {n}")).rect().min.y
    };
    let mut listed = things.clone();
    listed.sort_by_key(|t| y(&h, t) as i32);
    let mut az = things.clone();
    az.sort_by_key(|t| t.0.to_lowercase());
    assert_eq!(listed, az);
    // The heading's button sorts by count instead (remembered), and back.
    h.get_by_label("Sort Things").click();
    h.run_steps(2);
    assert!(h.state().settings().things_by_count);
    let mut listed = things.clone();
    listed.sort_by_key(|t| y(&h, t) as i32);
    assert_eq!(listed, things, "most photos first");
    h.get_by_label("Sort Things").click();
    h.run_steps(2);
    // Counts follow the photos in view: a name search for "green" leaves
    // only green.
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .unwrap()
        .click();
    h.run_steps(1);
    h.event(egui::Event::Text("green".into()));
    wait_for(&mut h, Duration::from_secs(10), "green by name", |a| {
        a.shown_count() == 2
    });
    assert_eq!(h.state().things(), [("green".to_owned(), 2)]);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Backspace);
    wait_for(&mut h, Duration::from_secs(10), "all photos", |a| {
        a.shown_count() == 5
    });
    assert_eq!(h.state().things(), things);
    h.get_by_label("red  2").click();
    wait_for(&mut h, Duration::from_secs(10), "tagged red", |a| {
        a.shown_count() == 2
    });
    assert_eq!(h.state().shown_names(), ["red.jpg", "reddish.jpg"]);
    // Choosing a tag doesn't recount the others.
    assert_eq!(h.state().things(), things);
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
    wait_for(&mut h, Duration::from_secs(10), "sidecar", |_| {
        lib.join("reddish.jpg.xmp").exists()
    });
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

/// A stand-in for the face models: each half of the image that isn't grey
/// is a "face", described by its colour.
struct HalvesModel;

static FACE_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl shoebox_app::people::FaceModel for HalvesModel {
    fn faces(
        &mut self,
        img: &shoebox_decode::Rgb8,
    ) -> Result<Vec<shoebox_app::people::FoundFace>, String> {
        FACE_RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let w = img.width as usize;
        let mut out = Vec::new();
        for half in 0..2 {
            let mut sum = [0f32; 3];
            let mut n = 0f32;
            for (i, px) in img.px.as_chunks::<3>().0.iter().enumerate() {
                if (i % w) / (w / 2) == half {
                    for c in 0..3 {
                        sum[c] += f32::from(px[c]);
                    }
                    n += 1.0;
                }
            }
            let m = (sum[0] + sum[1] + sum[2]) / 3.0;
            let v = [sum[0] - m, sum[1] - m, sum[2] - m];
            if v.iter().map(|x| x.abs()).fold(0.0, f32::max) / n > 30.0 {
                // A face-sized box in the middle of the half.
                let x = half as f32 * 0.5 + 0.15;
                out.push(([x, 0.3, 0.2, 0.3], 0.95, unit(v)));
            }
        }
        Ok(out)
    }
}

/// People end to end with a test model: groups, naming, the People
/// keyword, "Not …", and names kept (faces not recomputed) after a restart.
#[test]
fn people_in_the_app() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let (grey, red, blue, green) = ([128, 128, 128], [210, 40, 40], [40, 40, 210], [40, 200, 40]);
    for (name, left, right) in [
        ("a1.jpg", red, grey),
        ("a2.jpg", red, grey),
        ("a3.jpg", grey, red),
        ("b1.jpg", blue, grey),
        ("b2.jpg", blue, red),
        ("b3.jpg", grey, blue),
        ("c1.jpg", green, grey),
        ("none.jpg", grey, grey),
    ] {
        let img =
            image::RgbImage::from_fn(64, 48, |x, _| image::Rgb(if x < 32 { left } else { right }));
        std::fs::write(lib.join(name), shoebox_fixtures::jpeg(&img, 95).unwrap()).unwrap();
    }
    let mut p = paths(t.path(), std::slice::from_ref(&lib));
    p.faces = shoebox_app::people::FaceSource::Fixed(|| Box::new(HalvesModel));
    let mut s = Settings::load(&p.settings).unwrap();
    s.people = true;
    s.save(&p.settings).unwrap();

    let mut h = harness(p.clone(), egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    wait_for(&mut h, Duration::from_secs(30), "faces", |a| {
        a.people_groups().len() == 2
    });
    assert_eq!(FACE_RUNS.load(std::sync::atomic::Ordering::Relaxed), 8);
    // Red is in four photos, blue in three; green's single face isn't listed.
    assert_eq!(
        h.state().people_groups(),
        [("Unnamed 1".to_owned(), 4), ("Unnamed 2".to_owned(), 3)]
    );
    h.run_steps(3);
    // Faces are outlined with nobody chosen (the view bar's toggle).
    let face = |n: &str, chosen| (n.to_owned(), chosen);
    assert_eq!(
        h.state().faces_drawn("b2.jpg"),
        [face("Unnamed 1", false), face("Unnamed 2", false)]
    );

    // Show the first group and name it.
    h.get_by_label("Unnamed 1  4").click();
    wait_for(&mut h, Duration::from_secs(10), "red photos", |a| {
        a.shown_count() == 4
    });
    h.run_steps(3); // the toolbar now has the name field
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .unwrap()
        .click();
    h.run_steps(1);
    h.event(egui::Event::Text("Ann".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    wait_for(&mut h, Duration::from_secs(10), "named", |a| {
        a.people_groups().first().is_some_and(|g| g.0 == "Ann")
    });
    assert!(t.path().join("data/people.json").exists());
    assert_eq!(h.state().shown_count(), 4, "still showing Ann");
    snapshot(&mut h, "people-1-ann");

    // Everyone's face is outlined, hers marked (amber). The view bar's
    // toggle turns them off.
    let ann = |shown| ("Ann".to_owned(), shown);
    assert_eq!(
        h.state().faces_drawn("b2.jpg"),
        [ann(true), face("Unnamed 1", false)]
    );
    h.get_by_label("b2.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "people-2-loupe-faces");
    // Pointing at a name in the info panel outlines that face too.
    h.get_by_label("Unnamed 1").hover();
    h.run_steps(2);
    assert_eq!(
        h.state().faces_drawn("b2.jpg"),
        [ann(true), face("Unnamed 1", true)]
    );
    h.get_by_label("Face boxes").click();
    h.run_steps(2);
    assert!(h.state().settings().hide_face_boxes, "remembered");
    assert!(h.state().faces_drawn("b2.jpg").is_empty());
    h.get_by_label("Face boxes").click();
    h.key_press(egui::Key::Escape);
    h.run_steps(3);

    // Her keyword, on all four (sidecars).
    h.get_by_label("Add keyword to 4 photos").click();
    wait_for(&mut h, Duration::from_secs(10), "keywords", |a| {
        ["a1.jpg", "a2.jpg", "a3.jpg", "b2.jpg"]
            .iter()
            .all(|f| a.marks_of(f).is_some_and(|m| m.keywords == ["People|Ann"]))
    });
    // (The app shows marks at once; the sidecar follows in the background.)
    wait_for(&mut h, Duration::from_secs(10), "a3 sidecar", |_| {
        lib.join("a3.jpg.xmp").exists()
    });
    assert!(
        h.state()
            .marks_of("b1.jpg")
            .is_some_and(|m| m.keywords.is_empty())
    );

    // a3 isn't Ann.
    h.get_by_label("a3.jpg").click_secondary();
    h.run_steps(3);
    h.get_by_label("Not Ann").click();
    wait_for(&mut h, Duration::from_secs(10), "not Ann", |a| {
        a.shown_count() == 3
    });
    assert_eq!(h.state().people_groups()[0], ("Ann".to_owned(), 3));

    // ⌘-click another group: both shown, then marked as the same person.
    h.get_by_label("Unnamed 1  3")
        .click_modifiers(egui::Modifiers::COMMAND);
    wait_for(&mut h, Duration::from_secs(10), "both shown", |a| {
        a.shown_count() == 5 // a1, a2, b2 (Ann's) and b1, b2, b3
    });
    h.run_steps(2);
    snapshot(&mut h, "people-3-two-shown");
    h.get_by_label("All Ann").click();
    wait_for(&mut h, Duration::from_secs(10), "merged", |a| {
        a.people_groups() == [("Ann".to_owned(), 5)]
    });
    assert_eq!(h.state().shown_count(), 5, "still showing them, now one");

    // ⌘Z steps back through names and file changes in the order they were
    // made: the merge, "Not Ann", then (the library's) Ann's keyword.
    let groups = |a: &ShoeboxApp| a.people_groups();
    let ann = |n: usize| ("Ann".to_owned(), n);
    let undo = |h: &mut Harness<'_, ShoeboxApp>, shift: bool| {
        let m = if shift {
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::COMMAND
        };
        h.key_press_modifiers(m, egui::Key::Z);
        h.run_steps(2);
    };
    undo(&mut h, false);
    wait_for(&mut h, Duration::from_secs(10), "merge undone", |a| {
        groups(a) == [ann(3), ("Unnamed 1".to_owned(), 3)]
    });
    undo(&mut h, false);
    wait_for(&mut h, Duration::from_secs(10), "not Ann undone", |a| {
        groups(a)[0] == ann(4)
    });
    undo(&mut h, false);
    wait_for(&mut h, Duration::from_secs(10), "keyword undone", |a| {
        a.marks_of("a1.jpg").is_some_and(|m| m.keywords.is_empty())
    });
    assert_eq!(groups(h.state())[0], ann(4), "names untouched by that");
    // …and ⇧⌘Z forward again.
    undo(&mut h, true);
    wait_for(&mut h, Duration::from_secs(10), "keyword redone", |a| {
        a.marks_of("a1.jpg")
            .is_some_and(|m| m.keywords == ["People|Ann"])
    });
    undo(&mut h, true);
    wait_for(&mut h, Duration::from_secs(10), "not Ann redone", |a| {
        groups(a)[0] == ann(3)
    });
    undo(&mut h, true);
    wait_for(&mut h, Duration::from_secs(10), "merge redone", |a| {
        groups(a) == [ann(5)]
    });
    h.get_by_label("✖").click();
    h.run_steps(2);
    drop(h);

    // A restart keeps the names, the merge and the correction, without
    // finding faces again: the blue faces are Ann now (her examples).
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    wait_for(&mut h, Duration::from_secs(30), "people again", |a| {
        !a.people_groups().is_empty()
    });
    assert_eq!(h.state().people_groups(), [("Ann".to_owned(), 5)]);

    // The People list counts the photos shown (here, a search), not all.
    h.state_mut().set_filter(shoebox_core::query::Filter {
        search: "b".into(),
        ..Default::default()
    });
    h.run_steps(2);
    assert_eq!(h.state().shown_count(), 3, "b1, b2, b3");
    assert_eq!(h.state().people_listed(), [("Ann".to_owned(), 3)]);
    h.state_mut().set_filter(shoebox_core::query::Filter {
        search: "none".into(),
        ..Default::default()
    });
    h.run_steps(2);
    assert!(h.state().people_listed().is_empty(), "nobody in none.jpg");
    assert_eq!(FACE_RUNS.load(std::sync::atomic::Ordering::Relaxed), 8);
}

/// People with the real face models on a folder of your test photos
/// (copied). `cargo run --release -p shoebox-ml --example fetch -- <dir> <staging> faces`,
/// then `SHOEBOX_FACES_MODEL=<dir> SHOEBOX_UI_PHOTOS=<folder>
/// cargo test --release -p shoebox-app --test ui -- --ignored people_real`
#[test]
#[ignore]
fn people_real_model() {
    let (Some(model), Some(photos)) = (
        std::env::var_os("SHOEBOX_FACES_MODEL"),
        std::env::var_os("SHOEBOX_UI_PHOTOS"),
    ) else {
        eprintln!("set SHOEBOX_FACES_MODEL and SHOEBOX_UI_PHOTOS");
        return;
    };
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
    p.faces = shoebox_app::people::FaceSource::Download {
        dir: PathBuf::from(model),
        staging: t.path().join("staging"),
    };
    let mut s = Settings::load(&p.settings).unwrap();
    s.people = true;
    s.save(&p.settings).unwrap();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    let start = Instant::now();
    wait_for(&mut h, Duration::from_secs(300), "faces", |a| {
        let s = a.people_status();
        s.ready() && !s.indexing() && s.total > 0 && !a.people_groups().is_empty()
    });
    eprintln!(
        "looked at {} photos in {:.1} s",
        h.state().people_status().indexed,
        start.elapsed().as_secs_f32()
    );
    for (label, n) in h.state().people_groups() {
        eprintln!("{label}: {n} photos");
    }
    h.run_steps(3);
    for (i, (label, n)) in h.state().people_groups().into_iter().enumerate() {
        h.get_by_label(&format!("{label}  {n}")).click();
        h.run_steps(4);
        eprintln!("  {label}: {:?}", h.state().shown_names());
        snapshot(&mut h, &format!("people-real-{i}"));
    }
}

/// Tags and People on a real library, opened in place (it must be an APFS
/// clone: `cp -cR Photos Photos-test`; nothing is written there, but don't
/// risk the original). Prints tag counts, sample tags and people groups.
/// `SHOEBOX_UI_LIBRARY=<clone> SHOEBOX_ML_MODEL=<dir> SHOEBOX_FACES_MODEL=<dir>
/// [SHOEBOX_UI_DATA=<dir to keep analysis between runs>]
/// cargo test --release -p shoebox-app --test ui -- --ignored analysis_real --nocapture`
#[test]
#[ignore]
fn analysis_real_library() {
    let (Some(lib), Some(model), Some(faces)) = (
        std::env::var_os("SHOEBOX_UI_LIBRARY"),
        std::env::var_os("SHOEBOX_ML_MODEL"),
        std::env::var_os("SHOEBOX_FACES_MODEL"),
    ) else {
        eprintln!("set SHOEBOX_UI_LIBRARY, SHOEBOX_ML_MODEL and SHOEBOX_FACES_MODEL");
        return;
    };
    let t = tempfile::tempdir().unwrap();
    let data =
        std::env::var_os("SHOEBOX_UI_DATA").map_or_else(|| t.path().to_owned(), PathBuf::from);
    let mut p = paths(&data, &[PathBuf::from(lib)]);
    p.model = shoebox_app::semantic::ModelSource::Download {
        dir: PathBuf::from(model),
        staging: data.join("staging"),
    };
    p.faces = shoebox_app::people::FaceSource::Download {
        dir: PathBuf::from(faces),
        staging: data.join("staging"),
    };
    let mut s = Settings::load(&p.settings).unwrap();
    s.semantic_search = true;
    s.people = true;
    s.save(&p.settings).unwrap();
    let mut h = harness(p, egui::vec2(1600.0, 1000.0));
    settle(&mut h, Duration::from_secs(600));
    let start = Instant::now();
    let done = |a: &ShoeboxApp| {
        let (s, f) = (a.semantic_status(), a.people_status());
        s.ready() && !s.indexing() && f.ready() && !f.indexing() && !a.things().is_empty()
    };
    wait_for(&mut h, Duration::from_secs(7200), "analysis", done);
    h.run_steps(5);
    let (s, f) = (
        h.state().semantic_status().clone(),
        h.state().people_status().clone(),
    );
    eprintln!(
        "analysed in {:.0} s: {} photos embedded ({} skipped), {} looked at for faces ({} skipped)",
        start.elapsed().as_secs_f32(),
        s.indexed,
        s.skipped,
        f.indexed,
        f.skipped
    );
    snapshot(&mut h, "real-1-grid");
    let first = h.state().shown_names()[0].clone();
    h.get_by_label(&first).click();
    h.run_steps(6);
    snapshot(&mut h, "real-1b-selected");
    // (The data folder may be kept between runs: set, don't toggle.)
    let set = |h: &mut Harness<'_, ShoeboxApp>, label: &str, on: bool| {
        let current = |a: &ShoeboxApp| match label {
            "Group by day" => a.settings().day_headers,
            _ => a.settings().justified_rows,
        };
        if current(h.state()) != on {
            h.get_by_label(label).click();
            h.run_steps(4);
        }
        assert_eq!(current(h.state()), on);
    };
    set(&mut h, "Group by day", true);
    snapshot(&mut h, "real-2-days");
    set(&mut h, "Group by day", false);
    h.state_mut().set_view(View::Masonry);
    h.run_steps(4);
    set(&mut h, "Justified rows", true);
    h.run_steps(4);
    snapshot(&mut h, "real-3-rows");
    set(&mut h, "Justified rows", false);
    h.state_mut().set_view(View::Grid);
    h.run_steps(4);
    let things = h.state().things();
    eprintln!("{} tags in use; most common:", things.len());
    for (name, n) in things.iter().take(60) {
        eprintln!("  {name}: {n}");
    }
    let names = h.state().shown_names();
    let tagged = names
        .iter()
        .filter(|n| !h.state().tags_of(n).is_empty())
        .count();
    eprintln!(
        "{tagged} of {} photos have suggested tags; a sample:",
        names.len()
    );
    for n in names.iter().step_by((names.len() / 40).max(1)) {
        eprintln!("  {n}: {:?}", h.state().tags_of(n));
    }
    for tag in std::env::var("SHOEBOX_UI_TAGS")
        .unwrap_or_default()
        .split(',')
    {
        if tag.is_empty() {
            continue;
        }
        let with: Vec<&String> = names
            .iter()
            .filter(|n| h.state().tags_of(n).iter().any(|t| t == tag))
            .collect();
        eprintln!("tag {tag}: {with:?}");
    }
    let groups = h.state().people_groups();
    eprintln!("{} people groups:", groups.len());
    for (label, n) in groups.iter().take(40) {
        eprintln!("  {label}: {n} photos");
    }
    h.run_steps(3);
    for (label, n) in groups.iter().take(10) {
        h.get_by_label(&format!("{label}  {n}")).click();
        h.run_steps(4);
        eprintln!("group {label}: {:?}", h.state().shown_names());
        h.get_by_label(&format!("{label}  {n}")).click();
        h.run_steps(4);
    }
}

/// A Live Photo (JPG + MOV, one base name) stacks as one photo; lone clips
/// (MOV and MP4) show their poster frames, and the loupe offers to open it. Needs ffmpeg to
/// make the movies (skipped without it).
#[test]
fn live_photos_and_movies() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let ffmpeg = |out: &Path, colour: &str| {
        std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c={colour}:s=320x240:d=2"))
            .args(["-c:v", "h264", "-pix_fmt", "yuv420p"])
            .arg(out)
            .status()
            .is_ok_and(|s| s.success())
    };
    if !ffmpeg(&lib.join("IMG_0001.mov"), "red")
        || !ffmpeg(&lib.join("clip.mov"), "blue")
        || !ffmpeg(&lib.join("phone.mp4"), "green")
    {
        eprintln!("ffmpeg not available: skipped");
        return;
    }
    let img = image::RgbImage::from_pixel(64, 48, image::Rgb([210, 40, 40]));
    std::fs::write(
        lib.join("IMG_0001.jpg"),
        shoebox_fixtures::jpeg(&img, 90).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1200.0, 800.0));
    settle(&mut h, Duration::from_secs(60));
    assert_eq!(h.state().photo_count(), 4);
    assert_eq!(h.state().shown_count(), 3, "the Live Photo is one photo");
    assert_eq!(h.state().failed_count(), 0, "the clips' poster frames load");
    snapshot(&mut h, "movies-1-grid");
    h.get_by_label("phone.mp4").click();
    h.run_steps(2);
    h.state_mut().set_view(View::Loupe);
    settle(&mut h, Duration::from_secs(30));
    assert!(h.query_by_label("▶ Open in QuickTime Player").is_some());
    assert_eq!(
        h.state().failed_count(),
        0,
        "the full-size poster loads too"
    );
    snapshot(&mut h, "movies-2-loupe");
}

/// Develop: a raw photo (a synthetic DNG, which rawler reads whatever its
/// name) opens with D, renders from its raw data, takes a slider drag as
/// one history step, keeps the edit in `.shoebox/edits`, and undoes with ⌘Z.
#[test]
fn develop_a_raw_photo() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    let dng = shoebox_fixtures::Dng::new(600, 400);
    let bytes = dng.write(|x, y| 1500 + ((x * 7 + y * 13) % 600) as u16 * 15);
    std::fs::create_dir_all(root.join("2026")).unwrap();
    std::fs::write(root.join("2026/DSC_0001.NEF"), &bytes).unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("DSC_0001.NEF").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Develop);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    // The render's histogram arrives (every pixel counted); J shows clipping.
    wait_for(&mut h, Duration::from_secs(10), "histogram", |a| {
        a.develop_histogram().0.is_some_and(|n| n > 0)
    });
    h.key_press(egui::Key::J);
    h.run_steps(2);
    assert!(h.state().develop_histogram().1);
    snapshot(&mut h, "develop-clipping");
    h.key_press(egui::Key::J);
    h.run_steps(3);
    snapshot(&mut h, "develop");

    // What Auto set as the photo opened.
    let auto_ev = h
        .state()
        .develop_doc()
        .unwrap()
        .settings()
        .modules
        .exposure
        .ev;
    // Drag Exposure (the third slider: temperature, tint, exposure).
    let r = h
        .get_all_by_role(egui::accesskit::Role::Slider)
        .map(|n| n.rect())
        .nth(2)
        .unwrap();
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(r.center()));
    h.event(button(r.center(), true));
    h.run_steps(1);
    for dx in [10.0, 20.0, 30.0] {
        h.event(egui::Event::PointerMoved(r.center() + egui::vec2(dx, 0.0)));
        h.run_steps(1);
    }
    h.event(button(r.center() + egui::vec2(30.0, 0.0), false));
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    let ev = doc.settings().modules.exposure.ev;
    assert!(ev > 0.1, "{ev}");
    // Auto ran as the photo opened (a step of its own); the drag is one more.
    let labels: Vec<_> = doc.history().iter().map(|s| s.label.clone()).collect();
    assert_eq!(labels.len(), 2, "one drag, one step: {labels:?}");
    assert_eq!(labels[0], "Auto");
    // A second go at the same slider is the same step.
    let from = r.center() + egui::vec2(30.0, 0.0);
    h.event(egui::Event::PointerMoved(from));
    h.event(button(from, true));
    h.run_steps(1);
    h.event(egui::Event::PointerMoved(from + egui::vec2(15.0, 0.0)));
    h.run_steps(1);
    h.event(button(from + egui::vec2(15.0, 0.0), false));
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    assert!(doc.settings().modules.exposure.ev > ev, "{ev}");
    let labels: Vec<_> = doc.history().iter().map(|s| s.label.clone()).collect();
    assert_eq!(labels.len(), 2, "same setting, same step: {labels:?}");
    let ev = doc.settings().modules.exposure.ev;
    snapshot(&mut h, "develop-brighter");

    // Y: the original on the left, the edit on the right; drag the divider.
    use shoebox_app::views::develop::Compare;
    h.key_press(egui::Key::Y);
    wait_for(&mut h, Duration::from_secs(10), "original", |a| {
        a.develop_compare().2
    });
    assert_eq!(h.state().develop_compare().0, Compare::Split);
    let photo = h.get_by_label("Split").rect().center_bottom().y;
    // The divider sits in the middle of the photo, below the bar.
    let mid = egui::pos2(h.state().develop_divider_x().unwrap(), photo + 200.0);
    h.event(egui::Event::PointerMoved(mid));
    h.event(button(mid, true));
    h.run_steps(1);
    h.event(egui::Event::PointerMoved(mid - egui::vec2(200.0, 0.0)));
    h.run_steps(1);
    h.event(button(mid - egui::vec2(200.0, 0.0), false));
    h.run_steps(2);
    let split = h.state().develop_compare().1;
    assert!(split < 0.4, "{split}");
    snapshot(&mut h, "develop-split");
    // \ flicks to the original and back; an edit shows the edit again.
    h.key_press(egui::Key::Backslash);
    h.run_steps(2);
    assert_eq!(h.state().develop_compare().0, Compare::Original);
    snapshot(&mut h, "develop-original");
    h.key_press(egui::Key::Backslash);
    h.run_steps(2);
    assert_eq!(h.state().develop_compare().0, Compare::Edited);

    // S: soft proofing, in sRGB to start; Gamut marks what sRGB can't hold.
    use shoebox_develop::export::Space;
    h.key_press(egui::Key::S);
    h.run_steps(2);
    assert_eq!(h.state().develop_overlays().proof, Some(Space::Srgb));
    h.get_by_label("Gamut").click();
    h.run_steps(3);
    assert!(h.state().develop_overlays().gamut);
    snapshot(&mut h, "develop-proof");
    h.key_press(egui::Key::S);
    h.run_steps(2);
    assert_eq!(h.state().develop_overlays().proof, None);

    // Kept, then written to the library's .shoebox/edits after a pause.
    let file = root.join(".shoebox/edits/2026/DSC_0001.NEF.json");
    // (Auto's step was written first; wait for the drag's.)
    wait_for(&mut h, Duration::from_secs(15), "edit file", |_| {
        std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| shoebox_develop::edit::EditDoc::from_json(&s).ok())
            .is_some_and(|d| d.settings().modules.exposure.ev == ev)
    });
    let saved = shoebox_develop::edit::EditDoc::from_json(&std::fs::read_to_string(&file).unwrap())
        .unwrap();
    assert_eq!(saved.settings().modules.exposure.ev, ev);
    assert_eq!(saved.source.size, Some(bytes.len() as u64));
    assert!(!saved.source.blake3.is_empty());

    // ⌘Z undoes the edit (not a library action); Escape goes back.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    assert_eq!(
        h.state()
            .develop_doc()
            .unwrap()
            .settings()
            .modules
            .exposure
            .ev,
        auto_ev
    );

    // R: crop. Drag the left edge in a quarter of the way; Enter applies it
    // as one step, and the render is that much narrower.
    let wide = h.state().develop_photo_rect().unwrap();
    h.key_press(egui::Key::R);
    h.run_steps(3);
    assert!(h.state().develop_cropping());
    let frame = h.state().develop_photo_rect().unwrap();
    let from = egui::pos2(frame.left() + 2.0, frame.center().y);
    let to = from + egui::vec2(frame.width() / 4.0, 0.0);
    h.event(egui::Event::PointerMoved(from));
    h.event(button(from, true));
    h.run_steps(1);
    h.event(egui::Event::PointerMoved(from + egui::vec2(20.0, 0.0)));
    h.run_steps(1);
    h.event(egui::Event::PointerMoved(to));
    h.run_steps(1);
    h.event(button(to, false));
    h.run_steps(2);
    snapshot(&mut h, "develop-crop");
    h.key_press(egui::Key::Enter);
    h.run_steps(4);
    assert!(!h.state().develop_cropping());
    let doc = h.state().develop_doc().unwrap();
    let crop = doc.settings().geometry.crop;
    assert!(crop[0] > 0.2 && crop[0] < 0.3, "{crop:?}");
    assert_eq!(doc.history().last().unwrap().label, "Crop");
    let narrow = h.state().develop_photo_rect().unwrap();
    assert!(
        narrow.aspect_ratio() < wide.aspect_ratio(),
        "{narrow:?} vs {wide:?}"
    );
    // Escape cancels a crop without changing anything.
    let steps = doc.history().len();
    h.key_press(egui::Key::R);
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().develop_cropping());
    assert_eq!(h.state().view_name(), View::Develop);
    assert_eq!(h.state().develop_doc().unwrap().history().len(), steps);

    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Grid);
    // Leaving wrote the undo too; the photo itself was never touched.
    wait_for(&mut h, Duration::from_secs(15), "saved undo", |_| {
        std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| shoebox_develop::edit::EditDoc::from_json(&s).ok())
            .is_some_and(|d| d.settings().modules.exposure.ev == auto_ev)
    });
    assert_eq!(
        std::fs::read(root.join("2026/DSC_0001.NEF")).unwrap(),
        bytes
    );

    // The grid shows the edit (it's cropped): a preview is made in the
    // cache, and the tile is badged.
    // (One per state the edit settled in: Auto's, the drag's, the undo's.)
    wait_for(&mut h, Duration::from_secs(30), "edited preview", |a| {
        let (edited, ready) = a.edited_previews();
        edited == 1 && ready >= 1
    });
    let previews: Vec<_> = std::fs::read_dir(t.path().join("cache/edited"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(previews.len(), h.state().edited_previews().1);
    settle(&mut h, Duration::from_secs(30));
    snapshot(&mut h, "develop-edited-grid");
}

/// Develop edits finished images too: a JPEG opens, renders, and has no
/// raw module.
#[test]
fn develop_a_jpeg() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(3, 800, 600);
    std::fs::write(
        root.join("holiday.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("holiday.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(2);
    assert!(h.query_all_by_label("Exposure").next().is_some());
    assert!(
        h.query_by_label("Demosaic").is_none(),
        "no raw module for a JPEG"
    );
    snapshot(&mut h, "develop-jpeg");

    // W, then a click: the white balance that makes that spot neutral.
    h.key_press(egui::Key::W);
    h.run_steps(2);
    let photo = h.state().develop_photo_rect().unwrap();
    h.event(egui::Event::PointerMoved(photo.center()));
    h.run_steps(1);
    h.get_by_label("Pick neutral"); // still there
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(button(photo.center(), true));
    h.event(button(photo.center(), false));
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    assert_eq!(
        doc.settings().modules.white_balance.mode,
        shoebox_develop::edit::WbMode::Custom
    );
    assert!(
        doc.history()
            .last()
            .unwrap()
            .label
            .starts_with("White balance picked")
    );

    // ⌘] turns it a quarter clockwise (portrait now); ⌘[ back.
    let wide = h.state().develop_photo_rect().unwrap();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(4);
    assert_eq!(
        h.state().develop_doc().unwrap().settings().geometry.quarter,
        1
    );
    let tall = h.state().develop_photo_rect().unwrap();
    assert!(
        wide.aspect_ratio() > 1.0 && tall.aspect_ratio() < 1.0,
        "{wide:?} {tall:?}"
    );
    snapshot(&mut h, "develop-jpeg-turned");

    // Z: 100%, and the sharp full-size part arrives; dragging pans; Z fits.
    h.key_press(egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(20), "zoomed region", |a| {
        a.develop_zoom().1.is_some()
    });
    let centre = h.state().develop_zoom().0.unwrap();
    let photo = h.state().develop_photo_rect().unwrap();
    assert!(photo.width() > 600.0 / 2.0 - 1.0, "{photo:?}");
    let at = egui::pos2(660.0, 400.0);
    h.event(egui::Event::PointerMoved(at));
    h.event(button(at, true));
    h.run_steps(1);
    h.event(egui::Event::PointerMoved(at + egui::vec2(40.0, 0.0)));
    h.run_steps(1);
    h.event(button(at + egui::vec2(40.0, 0.0), false));
    h.run_steps(2);
    assert!(
        h.state().develop_zoom().0.unwrap().x < centre.x,
        "dragging right moves the view left"
    );
    snapshot(&mut h, "develop-jpeg-zoomed");
    h.key_press(egui::Key::Z);
    h.run_steps(2);
    assert!(h.state().develop_zoom().0.is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::OpenBracket);
    h.run_steps(4);
    assert_eq!(
        h.state().develop_doc().unwrap().settings().geometry.quarter,
        0
    );

    // A snapshot, named as it's made; turn the photo; the snapshot brings
    // it back, as a step of its own.
    h.get_by_label("New snapshot").click();
    h.run_steps(3);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("Upright".into()));
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(
        h.state().develop_doc().unwrap().snapshots[0].name,
        "Upright"
    );
    assert_eq!(
        h.state().view_name(),
        View::Develop,
        "typing stayed in the field"
    );
    snapshot(&mut h, "develop-snapshots");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(4);
    h.get_by_label("Upright").click();
    h.run_steps(4);
    let doc = h.state().develop_doc().unwrap();
    assert_eq!(doc.settings().geometry.quarter, 0);
    assert_eq!(doc.history().last().unwrap().label, "Snapshot: Upright");

    // Turned, and back in the library: the loupe shows the edit (portrait);
    // \ shows the photo before it (landscape), and again the edit.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(4);
    h.key_press(egui::Key::Escape);
    wait_for(&mut h, Duration::from_secs(30), "edited preview", |a| {
        a.edited_previews() == (1, 1)
    });
    h.key_press(egui::Key::Enter);
    wait_for(&mut h, Duration::from_secs(20), "edit in the loupe", |a| {
        a.loupe_drawn().is_some_and(|(_, [w, h])| w < h)
    });
    h.key_press(egui::Key::Backslash);
    wait_for(
        &mut h,
        Duration::from_secs(20),
        "original in the loupe",
        |a| a.loupe_drawn() == Some((true, [800, 600])),
    );
    snapshot(&mut h, "loupe-original");
    h.key_press(egui::Key::Backslash);
    h.run_steps(2);
    assert!(!h.state().loupe_drawn().unwrap().0);
}

/// Develop's look modules: dragging on the tone curve adds a point (one
/// history step however long the drag), double-clicking it removes it;
/// HSL and black and white sliders are there and change the document.
#[test]
fn develop_look_modules() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(4, 800, 600);
    std::fs::write(
        root.join("look.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    // Tall enough that the panel doesn't scroll (kittest's positions don't
    // follow a scroll).
    let mut h = harness(p, egui::vec2(1400.0, 2100.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("look.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    // The histogram's row comes in above the modules: wait for it, so the
    // curve isn't moved down between finding it and clicking on it.
    wait_for(&mut h, Duration::from_secs(30), "histogram", |a| {
        a.develop_histogram().0.is_some()
    });
    h.run_steps(3);
    assert!(h.query_by_label("Vibrance").is_some());
    assert!(h.query_by_label("Black & white").is_some());
    assert!(h.query_by_label("Clarity").is_some() && h.query_by_label("Dehaze").is_some());
    assert!(h.query_by_label("Texture").is_some() && h.query_by_label("Detail").is_some());
    assert!(h.query_by_label("Lens corrections").is_some());
    // The curve sits under its channel buttons.
    let rgb = h.get_by_label("RGB").rect();
    let curve = egui::Rect::from_min_size(
        egui::pos2(rgb.min.x, rgb.max.y + 6.0),
        egui::vec2(200.0, 200.0),
    );
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let start = curve.min + egui::vec2(100.0, 100.0);
    h.event(egui::Event::PointerMoved(start));
    h.event(button(start, true));
    h.run_steps(1);
    for k in 1..=5 {
        h.event(egui::Event::PointerMoved(
            start + egui::vec2(0.0, -6.0 * k as f32),
        ));
        h.run_steps(1);
    }
    let end = start + egui::vec2(0.0, -30.0);
    h.event(button(end, false));
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    let pts = doc.settings().modules.tone_curve.rgb.clone();
    assert_eq!(pts.len(), 1, "{pts:?}");
    assert!(pts[0][1] > pts[0][0] + 0.05, "dragged up: {pts:?}");
    let steps = doc.history().len();
    assert_eq!(doc.history().last().unwrap().label, "Curve RGB");
    snapshot(&mut h, "develop-look");
    // Right-click the point: gone, and with it the step that added it (the
    // same setting, back where it began).
    let right = |pressed| egui::Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(end));
    h.run_steps(1);
    h.event(right(true));
    h.run_steps(1);
    h.event(right(false));
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    assert!(doc.settings().modules.tone_curve.rgb.is_empty());
    assert_eq!(doc.history().len(), steps - 1);
    // Black and white on (its section starts closed; clicking the title
    // opens it).
    h.get_by_label("Black & white").click();
    h.run_steps(2);
    h.get_by_label("Black and white").click();
    h.run_steps(3);
    assert!(
        h.state()
            .develop_doc()
            .unwrap()
            .settings()
            .modules
            .black_and_white
            .enabled
    );
}

/// Presets and copied settings: save a preset from one photo, hover it on
/// another (a preview: the edit is untouched), click to apply; copy
/// settings (⇧⌘C) and paste them onto a third from the grid (⇧⌘V), which
/// writes its edit file.
#[test]
fn presets_and_pasted_settings() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    for (n, name) in [(1, "a.jpg"), (2, "b.jpg"), (3, "c.jpg")] {
        let img = shoebox_fixtures::scene(n, 400, 300);
        std::fs::write(root.join(name), shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
    }
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 1500.0));
    settle(&mut h, Duration::from_secs(60));
    let open_in_develop = |h: &mut Harness<'_, ShoeboxApp>, name: &str| {
        h.get_by_label(name).click();
        h.run_steps(2);
        h.key_press(egui::Key::D);
        wait_for(h, Duration::from_secs(30), "develop", |a| a.develop_ready());
        h.run_steps(3);
    };
    open_in_develop(&mut h, "a.jpg");
    // A quarter turn (a step), then a preset of everything copied by
    // default plus Crop & rotate.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(3);
    h.get_by_label("New preset").click();
    h.run_steps(2);
    let tick = |h: &mut Harness<'_, ShoeboxApp>, name: &str| {
        h.get_by(|n| {
            n.role() == egui::accesskit::Role::CheckBox && n.label().as_deref() == Some(name)
        })
        .click();
    };
    tick(&mut h, "Crop & rotate"); // in the window's list
    h.run_steps(1);
    h.get_by_label("Save").click();
    h.run_steps(3);
    assert_eq!(h.state().develop_presets(), ["Preset 1"]);
    wait_for(&mut h, Duration::from_secs(10), "presets file", |_| {
        t.path().join("data/presets/user.json").exists()
    });

    // On b: hover previews, the edit doesn't change; a click applies it.
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(30), "b in develop", |a| {
        a.develop_ready() && a.develop_doc().is_some()
    });
    h.run_steps(3);
    snapshot(&mut h, "develop-presets");
    h.get_by_label("Preset 1").hover();
    h.run_steps(3);
    assert_eq!(
        h.state().develop_doc().unwrap().settings().geometry.quarter,
        0
    );
    let wide = h.state().develop_photo_rect().unwrap();
    assert!(wide.aspect_ratio() < 1.0, "the preview is turned: {wide:?}");
    h.get_by_label("Preset 1").click();
    h.run_steps(3);
    assert_eq!(
        h.state().develop_doc().unwrap().settings().geometry.quarter,
        1
    );

    // Copy b's settings (Crop & rotate included) and paste onto c from the grid.
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::C,
    );
    h.run_steps(2);
    tick(&mut h, "Crop & rotate");
    h.run_steps(1);
    h.get_by_label("Copy").click();
    h.run_steps(2);
    h.key_press(egui::Key::G);
    h.run_steps(3);
    h.get_by_label("c.jpg").click();
    h.run_steps(2);
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::V,
    );
    let edit = root.join(".shoebox/edits/c.jpg.json");
    wait_for(&mut h, Duration::from_secs(20), "c's edit", |_| {
        edit.exists()
    });
    let doc = shoebox_develop::edit::EditDoc::from_json(&std::fs::read_to_string(&edit).unwrap())
        .unwrap();
    assert_eq!(doc.settings().geometry.quarter, 1);
    assert_eq!(doc.history().last().unwrap().label, "Paste settings");
}

/// Virtual copies: a copy of the edit opens as a second document with its
/// own settings; switching keeps each; both are written; deleting the copy
/// moves its file to the Trash and goes back to the master.
#[test]
fn virtual_copies() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(6, 400, 300);
    std::fs::write(
        root.join("pier.jpg"),
        shoebox_fixtures::jpeg(&img, 90).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 1500.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("pier.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    let turn = |h: &mut Harness<'_, ShoeboxApp>| {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::CloseBracket);
        h.run_steps(3);
    };
    let quarter =
        |h: &Harness<'_, ShoeboxApp>| h.state().develop_doc().unwrap().settings().geometry.quarter;
    turn(&mut h); // the master: a quarter turn
    h.get_by_label("New copy").click();
    h.run_steps(3);
    assert_eq!(h.state().develop_copies().1, "copy-1");
    assert_eq!(quarter(&h), 1, "a copy starts from the edit as it was");
    turn(&mut h); // the copy: half a turn
    assert_eq!(quarter(&h), 2);
    h.get_by_label("Master").click();
    wait_for(&mut h, Duration::from_secs(10), "master", |a| {
        a.develop_doc().is_some() && a.develop_copies().1 == "main"
    });
    assert_eq!(quarter(&h), 1);
    h.get_by_label("Copy 1").click();
    wait_for(&mut h, Duration::from_secs(10), "copy", |a| {
        a.develop_doc().is_some() && a.develop_copies().1 == "copy-1"
    });
    assert_eq!(quarter(&h), 2);
    let edits = root.join(".shoebox/edits");
    wait_for(&mut h, Duration::from_secs(20), "both written", |a| {
        edits.join("pier.jpg.json").exists()
            && edits.join("pier.jpg.copy-1.json").exists()
            && a.copy_count("pier.jpg") == 1
    });
    // The copy has its own tile in the library: its stack opened when it
    // was made, the copy current. Its tile shows its own edit.
    wait_for(&mut h, Duration::from_secs(20), "the copy's tile", |a| {
        a.shown_names() == ["pier.jpg", "pier.jpg (Copy 1)"]
    });
    assert_eq!(h.state().current_copy().as_deref(), Some("copy-1"));
    h.key_press(egui::Key::G);
    h.run_steps(3);
    wait_for(&mut h, Duration::from_secs(30), "previews", |a| {
        a.edited_previews().1 >= 2
    });
    snapshot(&mut h, "copies-grid");
    // Exported beside the original from the copy's tile: listed for the
    // copy (not the master), and current.
    let lib = h.state().copy_path("pier.jpg").unwrap();
    let lib = lib.parent().unwrap().to_path_buf();
    h.state_mut().edit_export_presets(|p| {
        p.destination = shoebox_develop::export::Destination::Library;
    });
    h.state_mut().export_with(0);
    let beside = lib.join("pier_Copy 1_JPEG · Full size.jpg");
    wait_for(&mut h, Duration::from_secs(30), "exported", |a| {
        beside.exists() && a.exports_stale() == [false]
    });
    // The master's tile opens the master in Develop; the copy's, the copy.
    h.get_by_label("pier.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(10), "master", |a| {
        a.develop_doc().is_some() && a.develop_copies().1 == "main"
    });
    assert_eq!(quarter(&h), 1);
    h.key_press(egui::Key::G);
    h.run_steps(2);
    h.get_by_label("pier.jpg (Copy 1)").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(10), "copy", |a| {
        a.develop_doc().is_some() && a.develop_copies().1 == "copy-1"
    });
    assert_eq!(quarter(&h), 2);
    // The master was opened, not changed: the copy's export is current.
    // Changing the copy's edit makes it stale; the master lists none.
    turn(&mut h);
    h.key_press(egui::Key::G);
    wait_for(&mut h, Duration::from_secs(30), "stale", |a| {
        a.current_copy().as_deref() == Some("copy-1") && a.exports_stale() == [true]
    });
    h.get_by_label("pier.jpg").click();
    wait_for(&mut h, Duration::from_secs(10), "the master's", |a| {
        a.current_copy().is_none() && a.exports_stale().is_empty()
    });
    h.get_by_label("pier.jpg (Copy 1)").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(10), "copy", |a| {
        a.develop_doc().is_some() && a.develop_copies().1 == "copy-1"
    });

    // Delete the copy (right-click): to the Trash, back to the master.
    // Under load the copies list can take a few frames to show again; once
    // it's there, let the panel settle so the click lands on the row.
    let start = Instant::now();
    while h.query_by_label("Copy 1").is_none() {
        assert!(start.elapsed() < Duration::from_secs(30), "no Copy 1 row");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    h.run_steps(5);
    h.get_by_label("Copy 1").click_secondary();
    h.run_steps(2);
    let start = Instant::now();
    while h.query_by_label("Delete").is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "no Delete in the menu"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    h.get_by_label("Delete").click();
    wait_for(&mut h, Duration::from_secs(20), "copy removed", |a| {
        !edits.join("pier.jpg.copy-1.json").exists() && a.copy_count("pier.jpg") == 0
    });
    assert!(t.path().join("trash/pier.jpg.copy-1.json").exists());
    assert_eq!(h.state().develop_copies().1, "main");
    assert_eq!(quarter(&h), 1);
    // Its tile is gone (its export stays, in the stack); the master is
    // current.
    assert!(
        !h.state()
            .shown_names()
            .contains(&"pier.jpg (Copy 1)".to_owned())
    );
    assert_eq!(h.state().current_copy(), None);
}

/// Moving a photo (through Shoebox, journaled) takes its edit and its
/// virtual copy along: the library knows them at the new place, and ⌘Z
/// brings everything back.
#[test]
fn copies_follow_a_moved_photo() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(root.join("later")).unwrap();
    let img = shoebox_fixtures::scene(7, 400, 300);
    std::fs::write(
        root.join("dock.jpg"),
        shoebox_fixtures::jpeg(&img, 90).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 1500.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("dock.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(3);
    h.get_by_label("New copy").click();
    h.run_steps(3);
    let edits = root.join(".shoebox/edits");
    wait_for(&mut h, Duration::from_secs(20), "written", |a| {
        edits.join("dock.jpg.copy-1.json").exists() && a.copy_count("dock.jpg") == 1
    });
    h.key_press(egui::Key::G);
    h.run_steps(3);
    // The catalog's own path (the temp folder is reached through a link).
    let here = h.state().copy_path("dock.jpg").unwrap();
    let lib = here.parent().unwrap().to_path_buf();
    let moved = lib.join("later/dock.jpg");
    h.state_mut()
        .request_org(shoebox_app::jobs::OrgRequest::MoveTo {
            photos: vec![here.clone()],
            folder: lib.join("later"),
        });
    wait_for(&mut h, Duration::from_secs(20), "dry run", |a| {
        a.org_preview().is_some()
    });
    // The photo and its two edit files (master and copy).
    assert_eq!(h.state().org_preview(), Some(Ok(3)));
    h.state_mut().confirm_org();
    wait_for(&mut h, Duration::from_secs(20), "moved", |a| {
        moved.exists()
            && a.copy_path("dock.jpg") == Some(moved.clone())
            && a.edited_previews().0 == 1
    });
    assert!(edits.join("later/dock.jpg.copy-1.json").exists());
    assert_eq!(h.state().copy_count("dock.jpg"), 1);
    // Undo: back where it was, edit and copy with it.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(20), "back", |a| {
        here.exists() && a.copy_path("dock.jpg") == Some(here.clone()) && a.edited_previews().0 == 1
    });
}

/// A photo in a reference folder opens in Develop read-only, with an offer
/// to import its folder: that plans an import of it (nothing copied yet).
#[test]
fn develop_offers_to_import_a_reference_photo() {
    let t = tempfile::tempdir().unwrap();
    let (lib, old) = (t.path().join("lib"), t.path().join("old"));
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::create_dir_all(old.join("trip")).unwrap();
    for (n, name) in [(1, "a.jpg"), (2, "b.jpg")] {
        let img = shoebox_fixtures::scene(n, 400, 300);
        std::fs::write(
            old.join("trip").join(name),
            shoebox_fixtures::jpeg(&img, 90).unwrap(),
        )
        .unwrap();
    }
    let p = paths(t.path(), &[lib.clone(), old.clone()]);
    let mut s = Settings::load(&p.settings).unwrap();
    s.set_reference(&old.canonicalize().unwrap(), true);
    s.save(&p.settings).unwrap();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("a.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(2);
    h.get_by_label("Import this folder…").click();
    h.run_steps(2);
    assert_eq!(h.state().view_name(), View::Import);
    wait_for(&mut h, Duration::from_secs(30), "import plan", |a| {
        a.import_counts().is_some()
    });
    assert_eq!(h.state().import_counts(), Some((2, 0)));
    assert_eq!(
        std::fs::read_dir(&lib).unwrap().count(),
        0,
        "nothing copied"
    );
}

/// A photo renamed in Finder keeps its edit: after the scan the edit is
/// found by content and moved beside the photo's new name (journaled);
/// an edit whose photo is gone stays, listed in Settings → Raw.
#[test]
fn edits_follow_photos_moved_in_finder() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(root.join("trips")).unwrap();
    let bytes = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(3, 400, 300), 92).unwrap();
    std::fs::write(root.join("trips/beach.jpg"), &bytes).unwrap();
    std::fs::write(
        root.join("other.jpg"),
        shoebox_fixtures::jpeg(&shoebox_fixtures::scene(4, 400, 300), 92).unwrap(),
    )
    .unwrap();
    let write_edit = |rel: &str, blake3: String, size: u64| {
        let mut doc = shoebox_develop::edit::EditDoc::new(shoebox_develop::edit::Source {
            path: rel.to_owned(),
            blake3,
            size: Some(size),
            camera: None,
            lens: None,
        });
        doc.change("Exposure", |s| s.modules.exposure.ev = 1.0);
        let p = shoebox_core::edits::path(&root, Path::new(rel), shoebox_core::edits::MAIN);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, doc.to_json()).unwrap();
        p
    };
    // Edited as IMG_1.jpg, since renamed trips/beach.jpg.
    let old = write_edit(
        "IMG_1.jpg",
        blake3::hash(&bytes).to_hex().to_string(),
        bytes.len() as u64,
    );
    // And one whose photo is gone.
    let gone = write_edit("gone.jpg", "00".repeat(32), bytes.len() as u64);

    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    wait_for(&mut h, Duration::from_secs(20), "reattached", |a| {
        a.edited_previews().0 == 1 && !a.unattached_edits().is_empty()
    });
    let new = shoebox_core::edits::path(
        &root,
        Path::new("trips/beach.jpg"),
        shoebox_core::edits::MAIN,
    );
    assert!(new.exists() && !old.exists());
    assert!(gone.exists(), "never deleted");
    let listed = h.state().unattached_edits();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].ends_with("lib/.shoebox/edits/gone.jpg.json"));
    assert!(h.state().notice().is_some_and(|n| n.contains("1 edit")));

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    h.run_steps(2);
    h.get_by_label("Raw").click();
    h.run_steps(5);
    assert!(h.query_by_label_contains("Edits without a photo").is_some());
}

/// Settings → Raw: the converter's status, the High efficiency count, and
/// the DNG cache.
#[test]
fn settings_raw_tab() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("DSC_0001.NEF"),
        shoebox_fixtures::nikon_compression_stub(14, true),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    h.run_steps(2);
    h.get_by_label("Raw").click();
    wait_for(&mut h, Duration::from_secs(10), "raw status", |_| true);
    h.run_steps(5);
    assert!(h.query_by_label_contains("1 of your photos use").is_some());
    assert!(h.query_by_label_contains("In use:").is_some());
    assert!(h.query_by_label("Download Adobe DNG Converter").is_some());
    snapshot(&mut h, "settings-raw");
}

/// Idle pre-convert (Settings → Raw, off by default): after a while
/// without input, a 3-star High efficiency NEF is converted to a DNG in the
/// cache; an unrated one isn't. A stand-in script plays the converter.
#[cfg(unix)]
#[test]
fn likely_edits_convert_when_idle() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("DSC_0001.NEF"),
        shoebox_fixtures::nikon_compression_stub(14, true),
    )
    .unwrap();
    std::fs::write(
        root.join("DSC_0001.NEF.xmp"),
        shoebox_core::xmp::edit(
            None,
            &[(
                shoebox_core::xmp::Prop::Rating,
                Some(shoebox_core::xmp::Value::Text("3".into())),
            )],
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("DSC_0002.NEF"),
        shoebox_fixtures::nikon_compression_stub(13, true),
    )
    .unwrap();
    let dng = t.path().join("made.dng");
    std::fs::write(
        &dng,
        shoebox_fixtures::Dng::new(32, 24).write(|x, y| 2000 + (x * y) as u16),
    )
    .unwrap();
    let log = t.path().join("runs.log");
    let script = t.path().join("fake-converter.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$5\" >> '{}'\nout=\"$4\"; nef=\"$5\"; stem=$(basename \"$nef\"); \
             stem=${{stem%.*}}\ncp '{}' \"$out/$stem.dng\"\n",
            log.display(),
            dng.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut s = Settings::load(&p.settings).unwrap();
    s.dng_converter = Some(script);
    s.preconvert = true;
    s.save(&p.settings).unwrap();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    h.state_mut().set_idle_after(Duration::from_millis(300));
    settle(&mut h, Duration::from_secs(60));
    let cached = || {
        std::fs::read_dir(t.path().join("cache/dng"))
            .map(|d| {
                d.filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "dng"))
                    .count()
            })
            .unwrap_or(0)
    };
    wait_for(
        &mut h,
        Duration::from_secs(20),
        "converted when idle",
        |_| cached() == 1,
    );
    std::thread::sleep(Duration::from_millis(500));
    let runs = std::fs::read_to_string(&log).unwrap();
    assert_eq!(runs.lines().count(), 1, "{runs}");
    assert!(runs.contains("DSC_0001.NEF"), "{runs}");
}

/// An export preset with a border and a text watermark: the editor shows
/// their rows, and the file has the border (its width kept).
#[test]
fn export_with_a_border_and_watermark() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(5, 640, 480);
    std::fs::write(
        root.join("beach.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 1100.0));
    settle(&mut h, Duration::from_secs(60));
    wait_for(&mut h, Duration::from_secs(10), "presets", |a| {
        a.export_state().0 > 0
    });
    let out = t.path().join("exports");
    h.state_mut().set_export_folder(&out);
    h.state_mut().edit_export_presets(|p| {
        p.border = Some(shoebox_develop::overlay::Border {
            width: 4.0,
            colour: [255, 255, 255],
        });
        p.watermark = Some(shoebox_develop::overlay::Watermark::text("© Shoebox"));
    });
    h.get_by_label("beach.jpg").click();
    h.run_steps(2);
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::E,
    );
    h.run_steps(3);
    // The first preset's editor shows the border and watermark rows.
    h.get_by_label("JPEG · Full size").click();
    h.run_steps(3);
    assert!(h.query_by_label("Watermark").is_some());
    assert!(h.query_by_label("Position").is_some());
    snapshot(&mut h, "export-watermark");
    h.get_all_by_label("Export").last().unwrap().click();
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(30), "export", |a| {
        !a.export_state().1 && a.export_state().3.is_some()
    });
    let files: Vec<_> = walk(&out);
    assert_eq!(files.len(), 1, "{files:?}");
    let d = shoebox_decode::render_path(
        shoebox_decode::Format::Jpeg,
        &files[0],
        None,
        1,
        shoebox_decode::Target::Srgb,
    )
    .unwrap();
    // Still 640 wide, a white border (4%: 26 px) all round.
    assert_eq!(d.width, 640);
    let at = |x: u32, y: u32| &d.px[((y * d.width + x) * 3) as usize..][..3];
    for (x, y) in [(2, 2), (637, 2), (2, d.height - 3), (320, 5)] {
        assert!(at(x, y).iter().all(|&v| v > 240), "{:?}", at(x, y));
    }
}

/// Export: ⇧⌘E, the first preset (JPEG, full size), into a folder outside
/// the library: not kept track of; exporting onto it again asks first.
/// Beside the original, in the library: a new file in the photo's stack
/// (journaled, no question asked), listed in the Info panel, stale once the
/// edit changes; re-exporting puts the new file in its place and the old
/// one in the Trash, and ⌘Z brings it back. The original is untouched.
#[test]
fn export_a_photo() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(5, 640, 480);
    std::fs::write(
        root.join("beach.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    wait_for(&mut h, Duration::from_secs(10), "presets", |a| {
        a.export_state().0 > 0
    });
    let out = t.path().join("exports");
    h.state_mut().set_export_folder(&out);
    h.get_by_label("beach.jpg").click();
    h.run_steps(2);
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::E,
    );
    h.run_steps(3);
    snapshot(&mut h, "export-dialog");
    // The window's title is "Export" too; the button comes after it.
    h.get_all_by_label("Export").last().unwrap().click();
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(30), "export", |a| {
        !a.export_state().1 && a.export_state().3.is_some()
    });
    let files: Vec<_> = walk(&out);
    assert_eq!(files.len(), 1, "{files:?}");
    let f = &files[0];
    assert!(
        f.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("beach_JPEG"),
        "{f:?}"
    );
    let decoded = shoebox_decode::render_path(
        shoebox_decode::Format::Jpeg,
        f,
        None,
        1,
        shoebox_decode::Target::Srgb,
    )
    .unwrap();
    assert_eq!((decoded.width, decoded.height), (640, 480));
    // Outside the library: not kept track of.
    h.run_steps(5);
    assert_eq!(h.state().export_state().2, Some(0));

    // Exporting again lands on that file: asked first. Cancel writes
    // nothing; "Save as a new file" keeps both; "Replace" writes over it.
    let opt_shift_cmd = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT | egui::Modifiers::ALT;
    h.key_press_modifiers(opt_shift_cmd, egui::Key::E);
    wait_for(&mut h, Duration::from_secs(30), "asked", |a| {
        a.export_asking()
    });
    snapshot(&mut h, "export-clash");
    h.run_steps(3); // the question's window drawn before clicking in it
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(!h.state().export_asking());
    assert_eq!(walk(&out).len(), 1);
    h.key_press_modifiers(opt_shift_cmd, egui::Key::E);
    wait_for(&mut h, Duration::from_secs(30), "asked", |a| {
        a.export_asking()
    });
    h.get_by_label("Save as a new file").click();
    wait_for(&mut h, Duration::from_secs(30), "kept both", |_| {
        walk(&out).len() == 2
    });
    let first = f.clone();
    let before = std::fs::metadata(&first).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    h.key_press_modifiers(opt_shift_cmd, egui::Key::E);
    wait_for(&mut h, Duration::from_secs(30), "asked", |a| {
        a.export_asking()
    });
    h.get_by_label("Replace").click();
    wait_for(&mut h, Duration::from_secs(30), "replaced", |_| {
        std::fs::metadata(&first).unwrap().modified().unwrap() > before
    });
    settle(&mut h, Duration::from_secs(30));
    assert_eq!(walk(&out).len(), 2, "{:?}", walk(&out));

    // Beside the original: a new file in the photo's stack, no question.
    let original = std::fs::read(root.join("beach.jpg")).unwrap();
    h.state_mut().edit_export_presets(|p| {
        p.destination = shoebox_develop::export::Destination::Library;
    });
    h.key_press_modifiers(opt_shift_cmd, egui::Key::E);
    let beside = root.join("beach_JPEG · Full size.jpg");
    wait_for(&mut h, Duration::from_secs(30), "exported beside", |_| {
        beside.exists()
    });
    assert!(!h.state().export_asking());
    wait_for(&mut h, Duration::from_secs(30), "in its stack", |a| {
        a.photo_count() == 2 && a.shown_names() == ["beach.jpg"]
    });
    assert!(
        h.state()
            .notice()
            .is_some_and(|n| n.contains("into the library"))
    );
    assert!(walk(&t.path().join("cache/export-staging")).is_empty());
    // Listed in the Info panel, current.
    wait_for(&mut h, Duration::from_secs(10), "listed", |a| {
        a.export_state().2 == Some(1)
    });
    assert!(h.query_by_label("stale").is_none());
    let exported = std::fs::read(&beside).unwrap();

    // The edit changes: stale. Re-export: the new file in its place, the
    // old one in the Trash.
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(3);
    h.key_press(egui::Key::G);
    wait_for(&mut h, Duration::from_secs(30), "stale", |a| {
        a.edited_previews().0 == 1
    });
    h.run_steps(3);
    wait_for(&mut h, Duration::from_secs(10), "flagged", |_| true);
    assert!(h.query_by_label("stale").is_some());
    h.get_all_by_label("Re-export").last().unwrap().click();
    wait_for(&mut h, Duration::from_secs(30), "re-exported", |_| {
        std::fs::read(&beside).is_ok_and(|b| b != exported)
    });
    assert!(!h.state().export_asking());
    let turned = shoebox_decode::render_path(
        shoebox_decode::Format::Jpeg,
        &beside,
        None,
        1,
        shoebox_decode::Target::Srgb,
    )
    .unwrap();
    assert_eq!((turned.width, turned.height), (480, 640), "the turned edit");
    let trashed = t.path().join("trash/beach_JPEG · Full size.jpg");
    assert_eq!(std::fs::read(&trashed).unwrap(), exported);
    wait_for(&mut h, Duration::from_secs(10), "listed once", |a| {
        a.export_state().2 == Some(1)
    });
    // ⌘Z: the earlier export is back in its place.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(30), "undone", |_| {
        std::fs::read(&beside).is_ok_and(|b| b == exported)
    });
    assert_eq!(std::fs::read(root.join("beach.jpg")).unwrap(), original);
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// Local adjustments: a radial mask from the panel, its exposure from its
/// own slider, its centre dragged on the photo (one step), the mask shown
/// with O; a brush mask painted, and erased with ⌥; ⌘Z takes a stroke
/// back; Escape puts the mask down without leaving Develop.
#[test]
fn develop_local_adjustments() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(5, 800, 600);
    std::fs::write(
        root.join("local.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("local.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    wait_for(&mut h, Duration::from_secs(30), "histogram", |a| {
        a.develop_histogram().0.is_some()
    });
    h.run_steps(3);
    let locals =
        |h: &Harness<'_, ShoeboxApp>| h.state().develop_doc().unwrap().settings().locals.clone();

    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Radial gradient").click();
    h.run_steps(3);
    let l = locals(&h);
    assert_eq!(l.len(), 1);
    assert_eq!(h.state().develop_local().0.as_deref(), Some("l1"));
    assert_eq!(h.state().develop_local().2, 1, "rendered with the mask");
    assert!(
        h.query_by_label("AI denoise").is_none(),
        "no AI denoise strength for a JPEG"
    );

    // The mask's Temperature (the last of the two) moves its own value.
    let temp = h.query_all_by_label("Temperature").last().unwrap().rect();
    let on_rail = egui::pos2(temp.right() + 30.0, temp.center().y);
    let button = |pos, pressed, modifiers| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    };
    h.event(egui::Event::PointerMoved(on_rail));
    h.run_steps(1);
    h.event(button(on_rail, true, egui::Modifiers::NONE));
    h.event(button(on_rail, false, egui::Modifiers::NONE));
    h.run_steps(3);
    let l = locals(&h);
    assert_ne!(l[0].adjust.temperature, 0.0, "{:?}", l[0].adjust);
    let doc = h.state().develop_doc().unwrap();
    assert_eq!(
        doc.settings().modules.white_balance,
        Default::default(),
        "the global white balance is untouched"
    );
    assert!(
        doc.history()
            .last()
            .unwrap()
            .label
            .starts_with("Mask temperature")
    );

    // Drag the ellipse's centre right: one step however many frames.
    let photo = h.state().develop_photo_rect().unwrap();
    let steps = h.state().develop_doc().unwrap().history().len();
    let drag = |h: &mut Harness<'_, ShoeboxApp>, from: egui::Pos2, by: egui::Vec2, m| {
        // Held keys, as the system reports them while the mouse moves.
        h.event(egui::Event::ModifiersChanged(m));
        h.event(egui::Event::PointerMoved(from));
        h.run_steps(1);
        h.event(button(from, true, m));
        h.run_steps(1);
        for k in 1..=6 {
            h.event(egui::Event::PointerMoved(from + by * (k as f32 / 6.0)));
            h.run_steps(1);
        }
        h.event(button(from + by, false, m));
        h.run_steps(3);
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    };
    drag(
        &mut h,
        photo.center(),
        egui::vec2(80.0, 0.0),
        egui::Modifiers::NONE,
    );
    let l = locals(&h);
    let shoebox_develop::edit::Part::Known(p) = &l[0].mask.parts[0] else {
        panic!()
    };
    let shoebox_develop::edit::Shape::Radial { centre, .. } = p.shape else {
        panic!("{:?}", p.shape)
    };
    assert!(centre[0] > 0.55, "moved right: {centre:?}");
    let doc = h.state().develop_doc().unwrap();
    assert_eq!(doc.history().len(), steps + 1);
    assert_eq!(doc.history().last().unwrap().label, "Move radial gradient");

    // O shows the mask.
    h.key_press(egui::Key::O);
    h.run_steps(3);
    assert_eq!(h.state().develop_overlays().mask, Some((0, false)));
    snapshot(&mut h, "develop-local-radial");
    h.key_press(egui::Key::O);
    h.run_steps(2);

    // A brush mask: paint a stroke, then erase part of it.
    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Brush").click();
    h.run_steps(3);
    assert_eq!(h.state().develop_local().0.as_deref(), Some("l2"));
    let from = photo.center() + egui::vec2(-120.0, 60.0);
    drag(&mut h, from, egui::vec2(240.0, 0.0), egui::Modifiers::NONE);
    let brush = |h: &Harness<'_, ShoeboxApp>| {
        let l = locals(h);
        match &l[1].mask.parts[0] {
            shoebox_develop::edit::Part::Known(shoebox_develop::edit::MaskPart {
                shape: shoebox_develop::edit::Shape::Brush { strokes },
                ..
            }) => strokes.clone(),
            other => panic!("{other:?}"),
        }
    };
    let strokes = brush(&h);
    assert_eq!(strokes.len(), 1);
    assert!(strokes[0].points.len() >= 3, "{:?}", strokes[0].points);
    assert!(!strokes[0].erase);
    assert_eq!(
        h.state()
            .develop_doc()
            .unwrap()
            .history()
            .last()
            .unwrap()
            .label,
        "Brush"
    );
    drag(
        &mut h,
        from + egui::vec2(100.0, -20.0),
        egui::vec2(0.0, 40.0),
        egui::Modifiers::ALT,
    );
    let strokes = brush(&h);
    assert_eq!(strokes.len(), 2);
    assert!(strokes[1].erase);
    // ⌘Z takes the erasing back.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(brush(&h).len(), 1);

    // Escape puts the mask down; Develop stays.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().develop_local().0, None);
    assert_eq!(h.state().view_name(), shoebox_app::app::View::Develop);
}

/// Smart Lighting, spot weighted: Spot weighted puts a box in the middle,
/// a corner dragged grows it (one step), a drag outside it draws a new
/// one; Face looks for a face (none in a synthetic scene: it says so);
/// Uniform takes the box away.
#[test]
fn develop_smart_lighting_spot() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(9, 800, 600);
    std::fs::write(
        root.join("spot.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("spot.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    wait_for(&mut h, Duration::from_secs(30), "histogram", |a| {
        a.develop_histogram().0.is_some()
    });
    h.run_steps(3);
    let smart = |h: &Harness<'_, ShoeboxApp>| {
        h.state()
            .develop_doc()
            .unwrap()
            .settings()
            .modules
            .smart_lighting
            .clone()
    };
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    // Some Smart Lighting (Auto may have set it already).
    if smart(&h).strength == 0.0 {
        let s = h.get_by_label("Smart Lighting").rect();
        let on_rail = egui::pos2(s.right() + 60.0, s.center().y);
        h.event(egui::Event::PointerMoved(on_rail));
        h.run_steps(1);
        h.event(button(on_rail, true));
        h.event(button(on_rail, false));
        h.run_steps(3);
    }
    assert!(smart(&h).strength > 0.0);
    assert_eq!(smart(&h).spot, None);

    h.get_by_label("Spot weighted").click();
    h.run_steps(3);
    let b = smart(&h).spot.expect("a box");
    assert!(
        (b[0] - 0.35).abs() < 1e-9 && (b[2] - 0.65).abs() < 1e-9,
        "{b:?}"
    );
    assert!(h.state().develop_smart().0, "the box is shown");
    let label = |h: &Harness<'_, ShoeboxApp>| {
        let doc = h.state().develop_doc().unwrap();
        (
            doc.history().len(),
            doc.history().last().unwrap().label.clone(),
        )
    };
    assert_eq!(label(&h).1, "Smart Lighting spot weighted");

    // Drag the bottom right corner out: one step.
    let photo = h.state().develop_photo_rect().unwrap();
    // Stored-image units → screen (800×600, upright: [1, 0.75]).
    let at = |u: [f64; 2]| {
        egui::pos2(
            photo.left() + u[0] as f32 * photo.width(),
            photo.top() + (u[1] / 0.75) as f32 * photo.height(),
        )
    };
    let drag = |h: &mut Harness<'_, ShoeboxApp>, from: egui::Pos2, by: egui::Vec2| {
        h.event(egui::Event::PointerMoved(from));
        h.run_steps(1);
        h.event(button(from, true));
        h.run_steps(1);
        for k in 1..=6 {
            h.event(egui::Event::PointerMoved(from + by * (k as f32 / 6.0)));
            h.run_steps(1);
        }
        h.event(button(from + by, false));
        h.run_steps(3);
    };
    let steps = label(&h).0;
    drag(&mut h, at([b[2], b[3]]), egui::vec2(60.0, 40.0));
    let moved = smart(&h).spot.unwrap();
    assert!(
        moved[2] > b[2] + 0.02 && moved[3] > b[3] + 0.02,
        "{moved:?}"
    );
    assert_eq!(moved[0], b[0], "the opposite corner stays");
    assert_eq!(label(&h), (steps + 1, "Move Smart Lighting spot".into()));
    snapshot(&mut h, "develop-smart-spot");

    // A drag outside the box draws a new one.
    drag(&mut h, at([0.05, 0.05]), egui::vec2(80.0, 60.0));
    let drawn = smart(&h).spot.unwrap();
    assert!(drawn[0] < 0.08 && drawn[2] < 0.3, "{drawn:?}");

    // Face: nothing like one here, and it says so.
    h.get_by_label("Face").click();
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(30), "the face search", |a| {
        !a.develop_smart().1
    });
    h.run_steps(2);
    assert!(h.state().develop_smart().2.is_some());
    assert_eq!(smart(&h).spot, Some(drawn));

    // Uniform: no box.
    h.get_by_label("Uniform").click();
    h.run_steps(3);
    assert_eq!(smart(&h).spot, None);
    assert!(!h.state().develop_smart().0);
}

/// Point Color: pick a colour on the photo (a point with that colour),
/// widen its range, show what it reaches; Reset takes the points away.
#[test]
fn develop_point_colour() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(6, 800, 600);
    std::fs::write(
        root.join("colour.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("colour.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    wait_for(&mut h, Duration::from_secs(30), "histogram", |a| {
        a.develop_histogram().0.is_some()
    });
    h.run_steps(3);
    let points = |h: &Harness<'_, ShoeboxApp>| {
        h.state()
            .develop_doc()
            .unwrap()
            .settings()
            .modules
            .point_colour
            .points
            .clone()
    };
    // The section starts closed; its title opens it.
    h.get_by_label("Point colour").click();
    h.run_steps(2);
    h.get_by_label("Pick colour").click();
    h.run_steps(2);
    let photo = h.state().develop_photo_rect().unwrap();
    let at = photo.center() + egui::vec2(-100.0, -60.0);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(1);
    h.event(button(at, true));
    h.event(button(at, false));
    h.run_steps(3);
    let p = points(&h);
    assert_eq!(p.len(), 1);
    assert!(p[0].chroma > 0.0 && p[0].lightness > 0.0, "{:?}", p[0]);
    assert_eq!(
        h.state()
            .develop_doc()
            .unwrap()
            .history()
            .last()
            .unwrap()
            .label,
        "Colour picked"
    );
    // Its saturation, from the slider's rail.
    let row = h.get_by_label("Saturation range").rect();
    let rail = egui::pos2(row.right() + 30.0, row.center().y);
    h.event(egui::Event::PointerMoved(rail));
    h.run_steps(1);
    h.event(button(rail, true));
    h.event(button(rail, false));
    h.run_steps(3);
    assert_ne!(points(&h)[0].saturation_range, 50.0, "{:?}", points(&h)[0]);
    h.get_by_label("Show affected").click();
    h.run_steps(3);
    assert_eq!(h.state().develop_overlays().point, Some(0));
    snapshot(&mut h, "develop-point-colour");
    h.get_by_label("Remove colour").click();
    h.run_steps(3);
    assert!(points(&h).is_empty());
}

/// Control points: K, a click on the red half makes a mask of a circle
/// there intersected with red and its brightness (one step, selected);
/// its circle's handle sizes it; ⌘Z takes both back. The panel's button
/// does the same, and Escape stops it without placing one.
#[test]
fn develop_control_point() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = image::RgbImage::from_fn(800, 600, |x, _| {
        image::Rgb(if x < 400 {
            [200, 50, 40]
        } else {
            [40, 60, 200]
        })
    });
    std::fs::write(
        root.join("point.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("point.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    wait_for(&mut h, Duration::from_secs(30), "histogram", |a| {
        a.develop_histogram().0.is_some()
    });
    h.run_steps(3);
    use shoebox_develop::edit::{MaskPart, Op, Part, Shape};
    let locals =
        |h: &Harness<'_, ShoeboxApp>| h.state().develop_doc().unwrap().settings().locals.clone();
    let last = |h: &Harness<'_, ShoeboxApp>| {
        let doc = h.state().develop_doc().unwrap();
        doc.history().last().unwrap().label.clone()
    };
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let click = |h: &mut Harness<'_, ShoeboxApp>, at: egui::Pos2| {
        h.event(egui::Event::PointerMoved(at));
        h.run_steps(1);
        h.event(button(at, true));
        h.event(button(at, false));
        h.run_steps(3);
    };
    let photo = h.state().develop_photo_rect().unwrap();
    let at = photo.center() + egui::vec2(-photo.width() / 4.0, 0.0);
    h.key_press(egui::Key::K);
    h.run_steps(2);
    click(&mut h, at);
    let l = locals(&h);
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].name, "Control point 1");
    assert_eq!(last(&h), "New control point");
    assert_eq!(h.state().develop_local().0.as_deref(), Some("l1"));
    assert_eq!(h.state().develop_local().1, 0, "the circle's handles");
    let parts: Vec<&MaskPart> = l[0]
        .mask
        .parts
        .iter()
        .map(|p| match p {
            Part::Known(p) => p,
            Part::Other(v) => panic!("{v}"),
        })
        .collect();
    assert_eq!(parts.len(), 3, "{parts:?}");
    let Shape::Radial { centre, radii, .. } = parts[0].shape else {
        panic!("{:?}", parts[0].shape)
    };
    assert_eq!(parts[0].op, Op::Add);
    assert!(
        (centre[0] - 0.25).abs() < 0.01 && (centre[1] - 0.375).abs() < 0.01,
        "{centre:?}"
    );
    match parts[1].shape {
        // Red's Oklab hue.
        Shape::Colour { hue, .. } => assert!((0.0..60.0).contains(&hue), "{hue}"),
        ref s => panic!("{s:?}"),
    }
    assert_eq!(parts[1].op, Op::Intersect);
    assert!(matches!(parts[2].shape, Shape::Luminance { .. }));
    assert_eq!(parts[2].op, Op::Intersect);
    wait_for(&mut h, Duration::from_secs(30), "rendered", |a| {
        a.develop_local().2 == 1
    });

    // The circle's handle on its right sizes it: one step.
    let handle = at + egui::vec2(radii[0] as f32 * photo.width(), 0.0);
    h.event(egui::Event::PointerMoved(handle));
    h.run_steps(1);
    h.event(button(handle, true));
    h.run_steps(1);
    for k in 1..=6 {
        h.event(egui::Event::PointerMoved(
            handle + egui::vec2(10.0 * k as f32, 0.0),
        ));
        h.run_steps(1);
    }
    h.event(button(handle + egui::vec2(60.0, 0.0), false));
    h.run_steps(3);
    let grown = match &locals(&h)[0].mask.parts[0] {
        Part::Known(MaskPart {
            shape: Shape::Radial { radii, .. },
            ..
        }) => *radii,
        other => panic!("{other:?}"),
    };
    assert!(grown[0] > radii[0] + 0.03, "{grown:?}");
    assert_eq!(last(&h), "Move radial gradient");
    snapshot(&mut h, "develop-control-point");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert!(locals(&h).is_empty());

    // The panel's button; Escape stops it, so a click places nothing.
    h.get_by_label("Control point").click();
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    click(&mut h, at);
    assert!(locals(&h).is_empty());
    assert_eq!(h.state().view_name(), View::Develop);
    // Pressed again, a click on the blue half takes blue.
    h.get_by_label("Control point").click();
    h.run_steps(2);
    click(
        &mut h,
        photo.center() + egui::vec2(photo.width() / 4.0, 0.0),
    );
    let l = locals(&h);
    assert_eq!(l.len(), 1);
    match &l[0].mask.parts[1] {
        Part::Known(MaskPart {
            shape: Shape::Colour { hue, .. },
            ..
        }) => assert!((220.0..300.0).contains(hue), "{hue}"),
        other => panic!("{other:?}"),
    }
}

/// Spot removal: Q, a click heals a spot there from a source found
/// nearby; dragging the spot moves it (one step); Backspace removes it; a
/// red eye has no source; Escape puts the tool down without leaving
/// Develop.
#[test]
fn develop_spot_removal() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(7, 800, 600);
    std::fs::write(
        root.join("spots.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("spots.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    let spots = |h: &Harness<'_, ShoeboxApp>| {
        h.state()
            .develop_doc()
            .unwrap()
            .settings()
            .modules
            .heal
            .spots
            .clone()
    };
    h.key_press(egui::Key::Q);
    h.run_steps(2);
    assert!(h.state().develop_spots().0);
    let photo = h.state().develop_photo_rect().unwrap();
    let at = photo.center();
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(1);
    h.event(button(at, true));
    h.event(button(at, false));
    h.run_steps(3);
    let s = spots(&h);
    assert_eq!(s.len(), 1);
    assert_ne!(s[0].source, s[0].dest, "a source elsewhere");
    assert_eq!(h.state().develop_spots().1, Some(0));
    let doc = h.state().develop_doc().unwrap();
    assert_eq!(doc.history().last().unwrap().label, "Heal spot");
    snapshot(&mut h, "develop-spot");

    // Drag it 40 points left: one step.
    let steps = h.state().develop_doc().unwrap().history().len();
    h.event(button(at, true));
    h.run_steps(1);
    for k in 1..=4 {
        h.event(egui::Event::PointerMoved(
            at + egui::vec2(-10.0 * k as f32, 0.0),
        ));
        h.run_steps(1);
    }
    h.event(button(at + egui::vec2(-40.0, 0.0), false));
    h.run_steps(3);
    let moved = spots(&h);
    assert!(moved[0].dest[0] < s[0].dest[0] - 0.01, "{:?}", moved[0]);
    assert_eq!(moved[0].source, s[0].source, "the source stays");
    assert_eq!(h.state().develop_doc().unwrap().history().len(), steps + 1);

    h.key_press(egui::Key::Backspace);
    h.run_steps(3);
    assert!(spots(&h).is_empty());

    // Red eye: a spot with no source of its own.
    h.get_by_label("Spot removal").click();
    h.run_steps(2);
    h.get_by_label("Red eye").click();
    h.run_steps(2);
    let eye = at + egui::vec2(60.0, 30.0);
    h.event(egui::Event::PointerMoved(eye));
    h.run_steps(1);
    h.event(button(eye, true));
    h.event(button(eye, false));
    h.run_steps(3);
    let s = spots(&h);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].kind, shoebox_develop::edit::SpotKind::RedEye);
    assert_eq!(s[0].source, s[0].dest);
    assert_eq!(
        h.state()
            .develop_doc()
            .unwrap()
            .history()
            .last()
            .unwrap()
            .label,
        "Red eye"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().develop_spots().0);
    assert_eq!(h.state().view_name(), shoebox_app::app::View::Develop);
}

/// Object removal: with the spot tool on Remove, a drag down a post paints
/// a removal (one step, "Remove object"); without the model the panel
/// offers it and the fill says why it couldn't; dragging the removal moves
/// its strokes; ⌥-dragging erases from it. With the model
/// (`SHOEBOX_INPAINT_MODEL`), the post is filled.
#[test]
fn develop_object_removal() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    // A sky-like gradient with a dark post standing in it.
    let img = image::RgbImage::from_fn(640, 480, |x, y| {
        if (300..330).contains(&x) && (100..380).contains(&y) {
            return image::Rgb([30, 28, 25]);
        }
        let t = y as f32 / 480.0;
        image::Rgb([
            (90.0 + 100.0 * t) as u8,
            (140.0 + 70.0 * t) as u8,
            (220.0 + 25.0 * t) as u8,
        ])
    });
    std::fs::write(
        root.join("post.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let with_model = std::env::var_os("SHOEBOX_INPAINT_MODEL").is_some();
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("post.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    let spots = |h: &Harness<'_, ShoeboxApp>| {
        h.state()
            .develop_doc()
            .unwrap()
            .settings()
            .modules
            .heal
            .spots
            .clone()
    };
    h.key_press(egui::Key::Q);
    h.run_steps(2);
    h.get_by_label("Spot removal").click();
    h.run_steps(2);
    h.get_by_label("Remove").click();
    h.run_steps(2);
    let photo = h.state().develop_photo_rect().unwrap();
    // The post's top and bottom on screen.
    let at = |fx: f32, fy: f32| {
        egui::pos2(
            photo.left() + fx * photo.width(),
            photo.top() + fy * photo.height(),
        )
    };
    let button = |pos, pressed, modifiers| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    };
    let drag = |h: &mut Harness<'_, ShoeboxApp>, from: egui::Pos2, to: egui::Pos2, m| {
        h.event(egui::Event::ModifiersChanged(m));
        h.event(egui::Event::PointerMoved(from));
        h.run_steps(1);
        h.event(button(from, true, m));
        h.run_steps(1);
        for k in 1..=8 {
            h.event(egui::Event::PointerMoved(
                from + (to - from) * (k as f32 / 8.0),
            ));
            h.run_steps(1);
        }
        h.event(button(to, false, m));
        h.run_steps(3);
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    };
    let steps = h.state().develop_doc().unwrap().history().len();
    drag(
        &mut h,
        at(0.49, 0.22),
        at(0.49, 0.78),
        egui::Modifiers::NONE,
    );
    let s = spots(&h);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].kind, shoebox_develop::edit::SpotKind::Remove);
    assert!(s[0].strokes[0].points.len() >= 3, "{:?}", s[0].strokes);
    assert!(
        (s[0].dest[1] - 0.5 * 480.0 / 640.0).abs() < 0.03,
        "{:?}",
        s[0].dest
    );
    let doc = h.state().develop_doc().unwrap();
    assert_eq!(doc.history().len(), steps + 1);
    assert_eq!(doc.history().last().unwrap().label, "Remove object");
    assert_eq!(h.state().develop_spots().1, Some(0));

    if with_model {
        wait_for(&mut h, Duration::from_secs(120), "the post filled", |a| {
            let (removing, failed, filled) = a.develop_removing();
            assert_eq!(failed, None);
            !removing && filled
        });
        // The post's middle is as the sky beside it.
        let y = 0.5 * 480.0 / 640.0;
        let post = h.state().develop_source_at([315.0 / 640.0, y]).unwrap();
        let sky = h.state().develop_source_at([0.4, y]).unwrap();
        for c in 0..3 {
            assert!((post[c] - sky[c]).abs() < 0.1 * sky[c], "{post:?} {sky:?}");
        }
        snapshot(&mut h, "develop-object-removal");
    } else {
        assert!(
            h.query_by_label_contains("Download the object removal model")
                .is_some(),
            "the download is offered"
        );
        wait_for(&mut h, Duration::from_secs(30), "the fill tried", |a| {
            let (removing, failed, _) = a.develop_removing();
            !removing && failed.is_some()
        });
        snapshot(&mut h, "develop-object-removal");
    }

    // Dragged by its ring: the strokes go with it, one step.
    let steps = h.state().develop_doc().unwrap().history().len();
    let before = spots(&h)[0].clone();
    let grab = photo.left_top()
        + (photo.size() * egui::vec2(before.dest[0] as f32, before.dest[1] as f32 * 640.0 / 480.0));
    drag(
        &mut h,
        grab,
        grab + egui::vec2(-60.0, 0.0),
        egui::Modifiers::NONE,
    );
    let moved = spots(&h);
    assert_eq!(moved.len(), 1, "moved, not painted");
    let dx = moved[0].strokes[0].points[0][0] - before.strokes[0].points[0][0];
    assert!(dx < -0.02, "{dx}");
    assert!((moved[0].dest[0] - before.dest[0] - dx).abs() < 1e-9);
    assert_eq!(h.state().develop_doc().unwrap().history().len(), steps + 1);

    // ⌥-drag across it erases from it.
    let y = 0.5;
    let x = moved[0].dest[0] as f32;
    drag(
        &mut h,
        at(x - 0.05, y),
        at(x + 0.05, y),
        egui::Modifiers::ALT,
    );
    let s = spots(&h);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].strokes.len(), 2);
    assert!(s[0].strokes[1].erase);
    assert_eq!(
        h.state()
            .develop_doc()
            .unwrap()
            .history()
            .last()
            .unwrap()
            .label,
        "Erase from removal"
    );
}

/// Auto-upright: a photo of a window grid turned 3° comes out level with
/// Upright → Level (one step), the crop the largest that fits.
#[test]
fn develop_upright_levels_a_tilted_photo() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let (w, h) = (900u32, 600u32);
    let a = 3f64.to_radians();
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let (cx, cy) = (f64::from(x) - 450.0, f64::from(y) - 300.0);
        let (u, v) = (cx * a.cos() + cy * a.sin(), -cx * a.sin() + cy * a.cos());
        let on = (u.rem_euclid(90.0) < 4.0 || v.rem_euclid(90.0) < 4.0)
            && u.abs() < 380.0
            && v.abs() < 250.0;
        if on {
            image::Rgb([30, 30, 35])
        } else {
            image::Rgb([200, 195, 185])
        }
    });
    std::fs::write(
        root.join("tilted.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("tilted.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("Transform").click();
    h.run_steps(2);
    h.get_by_label("Level").click();
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    let g = &doc.settings().geometry;
    assert!((g.rotate.abs() - 3.0).abs() < 0.3, "{g:?}");
    assert!(g.crop[0] > 0.0, "cropped to fit: {g:?}");
    assert_eq!(doc.history().last().unwrap().label, "Upright: Level");
}

/// Guided upright: two guides drawn along a turned grid's verticals turn
/// it level (a step each); an end dragged is one more step; removing a
/// guide leaves too few, and the correction goes with it.
#[test]
fn develop_guided_upright_follows_drawn_lines() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let (w, h) = (900u32, 600u32);
    let a = 3f64.to_radians();
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let (cx, cy) = (f64::from(x) - 450.0, f64::from(y) - 300.0);
        let (u, v) = (cx * a.cos() + cy * a.sin(), -cx * a.sin() + cy * a.cos());
        let on = (u.rem_euclid(90.0) < 4.0 || v.rem_euclid(90.0) < 4.0)
            && u.abs() < 380.0
            && v.abs() < 250.0;
        if on {
            image::Rgb([30, 30, 35])
        } else {
            image::Rgb([200, 195, 185])
        }
    });
    std::fs::write(
        root.join("guided.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("guided.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("Transform").click();
    h.run_steps(2);
    h.get_by_label("Guided").click();
    h.run_steps(3);
    assert!(h.state().develop_guided().0);
    let photo = h.state().develop_photo_rect().unwrap();
    // A point on the turned grid (u across it, v along it), on screen.
    let at = |u: f64, v: f64| {
        let (x, y) = (
            u * a.cos() - v * a.sin() + 450.0,
            u * a.sin() + v * a.cos() + 300.0,
        );
        photo.min
            + egui::vec2(
                x as f32 / 900.0 * photo.width(),
                y as f32 / 600.0 * photo.height(),
            )
    };
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let drag = |h: &mut Harness<'_, ShoeboxApp>, from: egui::Pos2, to: egui::Pos2| {
        h.event(egui::Event::PointerMoved(from));
        h.run_steps(1);
        h.event(button(from, true));
        h.run_steps(1);
        for k in 1..=5 {
            h.event(egui::Event::PointerMoved(
                from + (to - from) * k as f32 / 5.0,
            ));
            h.run_steps(1);
        }
        h.event(button(to, false));
        h.run_steps(3);
    };
    let steps = h.state().develop_doc().unwrap().history().len();
    // Two of the grid's verticals (their centres: u = 90k + 2).
    drag(&mut h, at(-268.0, -200.0), at(-268.0, 200.0));
    let doc = h.state().develop_doc().unwrap();
    let g = &doc.settings().geometry;
    assert_eq!(g.guides.len(), 1);
    assert_eq!(g.rotate, 0.0, "one guide alone does nothing");
    drag(&mut h, at(182.0, -220.0), at(182.0, 180.0));
    let doc = h.state().develop_doc().unwrap();
    let g = doc.settings().geometry.clone();
    assert_eq!(g.guides.len(), 2);
    assert!((g.rotate.abs() - 3.0).abs() < 0.3, "{g:?}");
    assert!(g.transform.vertical.abs() < 3.0, "{g:?}");
    assert!(g.crop[0] > 0.0, "cropped to fit: {g:?}");
    assert_eq!(doc.history().len(), steps + 2);
    assert_eq!(doc.history().last().unwrap().label, "Draw guide");
    snapshot(&mut h, "develop-guided");

    // An end moved along its line: one step, the same result.
    drag(&mut h, at(182.0, 180.0), at(182.0, 230.0));
    let doc = h.state().develop_doc().unwrap();
    let moved = &doc.settings().geometry;
    assert_eq!(doc.history().len(), steps + 3);
    assert_eq!(doc.history().last().unwrap().label, "Move guide");
    assert!((moved.rotate - g.rotate).abs() < 0.2, "{moved:?}");

    // Select the first and remove it: too few guides, no correction.
    let mid = at(-268.0, 0.0);
    h.event(egui::Event::PointerMoved(mid));
    h.run_steps(1);
    h.event(button(mid, true));
    h.event(button(mid, false));
    h.run_steps(3);
    assert_eq!(h.state().develop_guided(), (true, Some(0)));
    h.key_press(egui::Key::Backspace);
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    let g = &doc.settings().geometry;
    assert_eq!(g.guides.len(), 1);
    assert_eq!((g.rotate, g.transform.vertical), (0.0, 0.0));
    assert_eq!(doc.history().last().unwrap().label, "Remove guide");

    // Escape leaves Guided, and stays in Develop.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().develop_guided().0);
    assert_eq!(h.state().view_name(), shoebox_app::app::View::Develop);
}

/// Match Total Exposures and the reference view: a's Exposure raised in
/// Develop, then matched onto b (taken with the same settings, so it gets
/// the same Exposure); Shift-R makes a the reference, and moving to b
/// keeps a beside it.
#[test]
fn develop_reference_and_matched_exposures() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    for (name, n) in [("a.jpg", 1u64), ("b.jpg", 2)] {
        let shot = shoebox_fixtures::Shot {
            datetime: Box::leak(format!("2025:06:01 10:00:{n:02}").into_boxed_str()),
            subsec: "",
            orientation: 1,
            serial: "1",
        };
        let img = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(n + 10, 600, 400), 90).unwrap();
        std::fs::write(root.join(name), shoebox_fixtures::with_exif(&img, &shot)).unwrap();
    }
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 1400.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("a.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    // a's Exposure, from its slider's rail.
    let row = h.query_all_by_label("Exposure").last().unwrap().rect();
    let rail = egui::pos2(row.right() + 30.0, row.center().y);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(rail));
    h.run_steps(1);
    h.event(button(rail, true));
    h.event(button(rail, false));
    h.run_steps(3);
    let ev = h
        .state()
        .develop_doc()
        .unwrap()
        .settings()
        .modules
        .exposure
        .ev;
    assert_ne!(ev, 0.0);

    // Shift-R: a is the reference; moving on keeps it.
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::R);
    h.run_steps(2);
    assert_eq!(h.state().develop_reference().as_deref(), Some("a.jpg"));
    assert_eq!(
        h.state().develop_compare().0,
        shoebox_app::views::develop::Compare::Reference
    );

    // Back to the grid (a's edit kept), both selected, a current: match.
    h.key_press(egui::Key::G);
    wait_for(&mut h, Duration::from_secs(10), "a's edit kept", |a| {
        a.exposure_of("a.jpg") == Some(ev)
    });
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(2);
    shoebox_app::views::presets::match_exposures(h.state_mut());
    wait_for(&mut h, Duration::from_secs(10), "b matched", |a| {
        a.exposure_of("b.jpg") == Some((ev * 100.0).round() / 100.0)
    });

    // In Develop on b, a stays beside it.
    h.get_by_label("b.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop b", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    assert_eq!(h.state().develop_reference().as_deref(), Some("a.jpg"));
    snapshot(&mut h, "develop-reference");
    h.get_by_label("Edited").click();
    h.run_steps(2);
    assert_eq!(
        h.state().develop_compare().0,
        shoebox_app::views::develop::Compare::Edited
    );
}

/// A Subject mask: Vision finds the ball in the background, the photo
/// redraws with it, and the mask covers the ball, not the whole frame.
#[cfg(target_os = "macos")]
#[test]
fn develop_subject_mask() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let (w, h) = (640u32, 480u32);
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (x as f32 - 320.0, y as f32 - 264.0);
        let r = (dx * dx + dy * dy).sqrt() / 134.0;
        if r < 1.0 {
            let s = 1.0 - 0.5 * r * r;
            image::Rgb([(240.0 * s) as u8, (120.0 * s) as u8, (30.0 * s) as u8])
        } else {
            image::Rgb([120, 130, 140])
        }
    });
    std::fs::write(
        root.join("ball.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("ball.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Subject").click();
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(30), "subject found", |a| {
        let (cover, finding) = a.develop_mask_coverage();
        !finding && cover > 0.0
    });
    let (cover, _) = h.state().develop_mask_coverage();
    // The ball is about a fifth of the frame.
    assert!(cover > 0.08 && cover < 0.4, "{cover}");
    // Background: everything else (from the subject already found).
    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Background").click();
    h.run_steps(3);
    let (rest, _) = h.state().develop_mask_coverage_of(1);
    assert!((rest + cover - 1.0).abs() < 0.02, "{rest} + {cover}");
    // The faces' parts are in their own menu.
    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Face").click();
    h.run_steps(2);
    assert!(h.query_by_label("Eyebrows").is_some());
}

/// An Object mask: without the model, its part offers the download; with
/// it (`SHOEBOX_SAM_MODEL`), a click on the ball makes a mask of the ball.
#[test]
fn develop_object_mask() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = image::RgbImage::from_fn(640, 480, |x, y| {
        let (dx, dy) = (x as f32 - 220.0, y as f32 - 240.0);
        let r = (dx * dx + dy * dy).sqrt() / 110.0;
        if r < 1.0 {
            let s = 1.0 - 0.45 * r * r;
            image::Rgb([(235.0 * s) as u8, (125.0 * s) as u8, (35.0 * s) as u8])
        } else {
            image::Rgb([140, 140, 135])
        }
    });
    std::fs::write(
        root.join("object.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let with_model = std::env::var_os("SHOEBOX_SAM_MODEL").is_some();
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("object.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Object (click)").click();
    h.run_steps(3);
    if !with_model {
        assert!(
            h.query_by_label_contains("Download the object model")
                .is_some(),
            "the download is offered"
        );
        return;
    }
    // A click on the ball.
    let photo = h.state().develop_photo_rect().unwrap();
    let at = photo.min + egui::vec2(220.0 / 640.0 * photo.width(), 0.5 * photo.height());
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(1);
    h.event(button(at, true));
    h.event(button(at, false));
    h.run_steps(2);
    wait_for(&mut h, Duration::from_secs(60), "object found", |a| {
        let (cover, finding) = a.develop_mask_coverage();
        !finding && cover > 0.0
    });
    let (cover, _) = h.state().develop_mask_coverage();
    // The ball: π·110² of 640×480, about 0.12.
    assert!((cover - 0.124).abs() < 0.03, "{cover}");
}

/// Sky and landscape masks: the mask menu's Landscape submenu (Sky first)
/// makes a mask of that part; without the model its part offers the
/// download; with it (`SHOEBOX_SKY_MODEL`), the sky of a drawn landscape
/// is found.
#[test]
fn develop_landscape_mask() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    // Blue sky over the top 40%, paler towards the horizon; grass below.
    let img = image::RgbImage::from_fn(640, 480, |x, y| {
        let n = ((x * 7 + y * 13) % 11) as f32;
        if y < 192 {
            let t = y as f32 / 192.0;
            image::Rgb([
                (70.0 + 110.0 * t) as u8,
                (130.0 + 80.0 * t) as u8,
                (225.0 + 20.0 * t) as u8,
            ])
        } else {
            image::Rgb([
                (60.0 + n * 3.0) as u8,
                (130.0 + n * 3.0) as u8,
                (40.0 + n) as u8,
            ])
        }
    });
    std::fs::write(
        root.join("field.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let with_model = std::env::var_os("SHOEBOX_SKY_MODEL").is_some();
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("field.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Landscape").click();
    h.run_steps(2);
    for label in ["Vegetation", "Water", "Mountains", "Buildings", "Ground"] {
        assert!(h.query_by_label(label).is_some(), "{label} in the menu");
    }
    h.get_by_label("Sky").click();
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    let l = doc.settings().locals.last().unwrap();
    assert_eq!(l.name, "Sky");
    assert!(matches!(
        &l.mask.parts[..],
        [shoebox_develop::edit::Part::Known(
            shoebox_develop::edit::MaskPart {
                shape: shoebox_develop::edit::Shape::Landscape {
                    part: shoebox_develop::edit::LandscapePart::Sky
                },
                invert: false,
                ..
            }
        )]
    ));
    if !with_model {
        assert!(
            h.query_by_label_contains("Download the landscape model")
                .is_some(),
            "the download is offered"
        );
        snapshot(&mut h, "develop-landscape-mask");
        return;
    }
    wait_for(&mut h, Duration::from_secs(120), "sky found", |a| {
        let (cover, finding) = a.develop_mask_coverage();
        !finding && cover > 0.0
    });
    let (cover, _) = h.state().develop_mask_coverage();
    assert!((cover - 0.4).abs() < 0.05, "{cover}");
    snapshot(&mut h, "develop-landscape-mask");
}

/// Lens Blur: Apply turns it on (the depth model offered when it isn't
/// there), Set focus then a click on the photo sets the focus there, Show
/// depth and Nearest; a mask's Depth slider shows while it's on. With the
/// model (`SHOEBOX_DEPTH_MODEL`), the depth is found and the photo
/// blurred.
#[test]
fn develop_lens_blur() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    // A red ball near the camera on a chequered floor under a sky.
    let img = image::RgbImage::from_fn(640, 480, |x, y| {
        let (fx, fy) = (x as f32, y as f32);
        if (fx - 220.0).hypot(fy - 330.0) < 80.0 {
            image::Rgb([200, 40, 30])
        } else if y < 160 {
            image::Rgb([110, 160, 235])
        } else {
            let z = 40.0 / (fy - 159.0);
            let c = (((fx - 320.0) * z / 40.0).floor() as i64 + (z * 4.0).floor() as i64) & 1;
            let g = if c == 0 { 200 } else { 70 };
            image::Rgb([g, g, g])
        }
    });
    std::fs::write(
        root.join("ball.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let with_model = std::env::var_os("SHOEBOX_DEPTH_MODEL").is_some();
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("ball.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("Lens Blur").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    assert!(doc.settings().modules.lens_blur.enabled);
    assert_eq!(doc.history().last().unwrap().label, "Lens Blur on");
    if !with_model {
        assert!(
            h.query_by_label_contains("Download the depth model")
                .is_some(),
            "the download is offered"
        );
    }
    // Set focus, then a click on the ball.
    h.get_by_label("Set focus").click();
    h.run_steps(2);
    let photo = h.state().develop_photo_rect().unwrap();
    let at = photo.min
        + egui::vec2(
            photo.width() * 220.0 / 640.0,
            photo.height() * 330.0 / 480.0,
        );
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(1);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(button(at, true));
    h.event(button(at, false));
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    let focus = doc.settings().modules.lens_blur.focus.unwrap();
    assert!(
        (focus[0] - 220.0 / 640.0).abs() < 0.02 && (focus[1] - 330.0 / 640.0).abs() < 0.02,
        "{focus:?}"
    );
    assert_eq!(doc.history().last().unwrap().label, "Focus set");
    h.get_by_label("Show depth").click();
    h.run_steps(2);
    if with_model {
        wait_for(&mut h, Duration::from_secs(120), "depth found", |a| {
            let (blurred, finding) = a.develop_lens_blur();
            blurred && !finding
        });
        snapshot(&mut h, "develop-lens-blur-depth");
        h.get_by_label("Show depth").click();
        h.run_steps(3);
        snapshot(&mut h, "develop-lens-blur");
    } else {
        assert_eq!(h.state().develop_lens_blur(), (false, false));
        snapshot(&mut h, "develop-lens-blur");
    }
    h.get_by_label("Nearest").click();
    h.run_steps(2);
    let doc = h.state().develop_doc().unwrap();
    assert_eq!(doc.settings().modules.lens_blur.focus, None);
    // A mask has a Depth slider while Lens Blur is on.
    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Radial gradient").click();
    h.run_steps(3);
    assert!(h.query_by_label("Depth").is_some(), "the Depth slider");
}

/// AI denoise on a raw photo: without the model the panel offers it; with
/// it (`SHOEBOX_RAWNIND_MODEL`), the denoised source arrives and the photo
/// redraws.
#[test]
fn develop_ai_denoise() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    let dng = shoebox_fixtures::Dng::new(600, 400);
    let bytes = dng.write(|x, y| 1500 + ((x * 7 + y * 13) % 600) as u16 * 15);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("DSC_0002.NEF"), &bytes).unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let with_model = std::env::var_os("SHOEBOX_RAWNIND_MODEL").is_some();
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("DSC_0002.NEF").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("Detail").click();
    h.run_steps(2);
    h.get_by_label("AI denoise").click();
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    assert!(doc.settings().modules.denoise_ai.enabled);
    assert_eq!(doc.history().last().unwrap().label, "AI denoise on");
    if !with_model {
        assert!(
            h.query_by_label_contains("Download the AI denoise model")
                .is_some(),
            "the download is offered"
        );
        return;
    }
    wait_for(&mut h, Duration::from_secs(60), "AI denoise", |a| {
        let (arrived, _, err) = a.develop_ai();
        assert!(err.is_none(), "{err:?}");
        arrived
    });
}

/// A mask's AI denoise strength on a raw photo: its slider moves the
/// layer's own value (not the global module's) and the photo renders with
/// it; without the model the panel says what's missing.
#[test]
fn develop_local_ai_denoise() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    let dng = shoebox_fixtures::Dng::new(600, 400);
    let bytes = dng.write(|x, y| 1500 + ((x * 7 + y * 13) % 600) as u16 * 15);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("DSC_0003.NEF"), &bytes).unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let with_model = std::env::var_os("SHOEBOX_RAWNIND_MODEL").is_some();
    let mut h = harness(p, egui::vec2(1400.0, 2900.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("DSC_0003.NEF").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("New mask").click();
    h.run_steps(2);
    h.get_by_label("Radial gradient").click();
    h.run_steps(3);
    assert_eq!(h.state().develop_local().0.as_deref(), Some("l1"));

    // The mask's AI denoise slider, on the rail's positive half (the
    // sliders start after the widest label, the mask's Temperature). The
    // window is tall enough for the panel to show it.
    let ai = h.get_by_label("AI denoise").rect();
    let temp = h.query_all_by_label("Temperature").last().unwrap().rect();
    let on_rail = egui::pos2(temp.right() + 100.0, ai.center().y);
    let button = |pressed| egui::Event::PointerButton {
        pos: on_rail,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(on_rail));
    h.run_steps(1);
    h.event(button(true));
    h.event(button(false));
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap().clone();
    let l = &doc.settings().locals[0];
    assert_ne!(l.adjust.denoise_ai, 0.0, "{:?}", l.adjust);
    assert!(
        !doc.settings().modules.denoise_ai.enabled,
        "the global module is untouched"
    );
    assert!(
        doc.history()
            .last()
            .unwrap()
            .label
            .starts_with("Mask AI denoise"),
        "{}",
        doc.history().last().unwrap().label
    );
    // It renders with the mask.
    wait_for(&mut h, Duration::from_secs(30), "rendered", |a| {
        a.develop_local().2 == 1
    });
    if !with_model && l.adjust.denoise_ai > 0.0 {
        assert!(
            h.query_by_label_contains("AI denoise needs its model")
                .is_some(),
            "says what's missing"
        );
    }
    snapshot(&mut h, "develop-local-ai-denoise");
}

/// AI denoise in Auto from an ISO (Settings → Raw, off by default): ticked,
/// it's saved at ISO 3200; Auto then turns AI denoise on for an ISO 6400
/// raw (with little classic luminance noise reduction) and not for an
/// ISO 400 one.
#[test]
fn auto_ai_denoise_from_iso() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    for (name, iso) in [("DSC_0010.NEF", 6400), ("DSC_0011.NEF", 400)] {
        let dng = shoebox_fixtures::Dng {
            iso: Some(iso),
            ..shoebox_fixtures::Dng::new(600, 400)
        };
        let bytes = dng.write(|x, y| 1500 + ((x * 7 + y * 13) % 600) as u16 * 15);
        std::fs::write(root.join(name), &bytes).unwrap();
    }
    let p = paths(t.path(), std::slice::from_ref(&root));
    let file = p.settings.clone();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
    h.run_steps(2);
    h.get_by_label("Raw").click();
    h.run_steps(3);
    assert_eq!(h.state().settings().ai_denoise_from_iso, None);
    h.get_by_label("AI denoise in Auto from ISO").click();
    h.run_steps(3);
    assert_eq!(h.state().settings().ai_denoise_from_iso, Some(3200));
    wait_for(&mut h, Duration::from_secs(10), "settings saved", |_| {
        Settings::load(&file).unwrap().ai_denoise_from_iso == Some(3200)
    });
    snapshot(&mut h, "settings-raw-ai-denoise");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    for (name, ai) in [("DSC_0010.NEF", true), ("DSC_0011.NEF", false)] {
        h.key_press(egui::Key::G);
        h.run_steps(2);
        h.get_by_label(name).click();
        h.run_steps(2);
        h.key_press(egui::Key::D);
        // A frame in Develop drops the last photo's edit.
        h.run_steps(2);
        wait_for(&mut h, Duration::from_secs(30), "auto", |a| {
            a.develop_ready()
                && a.develop_doc()
                    .is_some_and(|d| d.history().first().is_some_and(|s| s.label == "Auto"))
        });
        let s = h.state().develop_doc().unwrap().settings().clone();
        assert_eq!(s.modules.denoise_ai.enabled, ai, "{name}");
        if ai {
            assert_eq!(s.modules.noise_reduction.luminance, 10.0);
        } else {
            assert_eq!(s.modules.noise_reduction.luminance, 0.0);
        }
    }
}

/// Colour grading: pick Shadows, click right of the wheel's middle: a
/// red-ish hue (Oklab 0°) at part saturation, one step.
#[test]
fn develop_colour_grading_wheel() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(8, 800, 600);
    std::fs::write(
        root.join("grade.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("grade.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("Colour grading").click();
    h.run_steps(2);
    // The grading's Shadows (Tone and the tone curve have one too).
    h.query_all_by_label("Shadows").last().unwrap().click();
    h.run_steps(2);
    // The wheel spans the panel under the range buttons, up to 180 high.
    let shadows = h.query_all_by_label("Shadows").last().unwrap().rect();
    let top = shadows.max.y + 4.0;
    let bottom = h.query_all_by_label("Hue").last().unwrap().rect().min.y - 4.0;
    // Across the panel, from the buttons' left to the window's right edge
    // less its margin.
    let centre = egui::pos2((shadows.min.x + 1400.0 - 12.0) / 2.0, (top + bottom) / 2.0);
    let at = centre + egui::vec2(35.0, 0.0);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(1);
    h.event(button(at, true));
    h.event(button(at, false));
    h.run_steps(3);
    snapshot(&mut h, "develop-grading");
    let doc = h.state().develop_doc().unwrap();
    let w = doc.settings().modules.colour_grading.shadows;
    assert!(w.saturation > 10.0 && w.saturation < 90.0, "{w:?}");
    assert!(w.hue < 40.0 || w.hue > 320.0, "{w:?}");
    assert!(doc.history().last().unwrap().label.starts_with("Shadows"));
}

/// Effects: the section opens and its sliders change the vignette.
#[test]
fn develop_effects() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(9, 800, 600);
    std::fs::write(
        root.join("fx.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("fx.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    h.get_by_label("Effects").click();
    h.run_steps(2);
    let row = h.get_by_label("Roundness").rect();
    let rail = egui::pos2(row.right() + 30.0, row.center().y);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(rail));
    h.run_steps(1);
    h.event(button(rail, true));
    h.event(button(rail, false));
    h.run_steps(3);
    let doc = h.state().develop_doc().unwrap();
    assert_ne!(doc.settings().modules.effects.vignette.roundness, 0.0);
    assert!(
        doc.history()
            .last()
            .unwrap()
            .label
            .starts_with("Vignette roundness")
    );
}

/// Volume deformation: Spherical folds into the render's lens grid (zoomed
/// to stay filled) as one step; Fix faces looks for faces on a thread
/// (none in this scene, and it says so); Reset takes it all away.
#[test]
fn develop_volume_deformation() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let img = shoebox_fixtures::scene(9, 800, 600);
    std::fs::write(
        root.join("wide.jpg"),
        shoebox_fixtures::jpeg(&img, 92).unwrap(),
    )
    .unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 2600.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label("wide.jpg").click();
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    h.run_steps(3);
    let volume = |h: &Harness<'_, ShoeboxApp>| {
        h.state()
            .develop_doc()
            .unwrap()
            .settings()
            .geometry
            .volume
            .clone()
    };
    let last = |h: &Harness<'_, ShoeboxApp>| {
        let doc = h.state().develop_doc().unwrap();
        doc.history().last().map(|s| s.label.clone())
    };
    h.get_by_label("Volume deformation").click();
    h.run_steps(2);
    h.get_by_label("Spherical").click();
    h.run_steps(3);
    assert_eq!(
        volume(&h).mode,
        shoebox_develop::edit::VolumeMode::Spherical
    );
    assert_eq!(last(&h).as_deref(), Some("Volume deformation: Spherical"));
    wait_for(
        &mut h,
        Duration::from_secs(30),
        "the deformed render",
        |a| a.develop_volume().3.is_some_and(|z| z > 1.01),
    );
    snapshot(&mut h, "develop-volume");

    h.get_by_label("Fix faces").click();
    h.run_steps(2);
    assert!(volume(&h).faces);
    wait_for(&mut h, Duration::from_secs(30), "the face search", |a| {
        !a.develop_volume().1
    });
    h.run_steps(2);
    assert!(h.state().develop_volume().2.is_some(), "it says none");
    assert!(volume(&h).boxes.is_empty());

    // The section's own Reset (on its header's line).
    let header = h.get_by_label("Volume deformation").rect();
    h.get_all_by_label("Reset")
        .find(|n| (n.rect().center().y - header.center().y).abs() < 10.0)
        .expect("the section's Reset")
        .click();
    h.run_steps(3);
    assert_eq!(volume(&h), shoebox_develop::edit::Volume::default());
    assert_eq!(last(&h).as_deref(), Some("Reset volume deformation"));
}

/// AVIF and linear DNG beside the original: AVIF picked in the export
/// dialog's preset editor, with Beside the original (offered for both
/// formats), lands in the library in the camera DNG's stack; so does a
/// linear DNG. That opens in Develop past demosaic (no AI denoise to run),
/// and the AVIF as a finished image.
#[test]
fn avif_and_dng_beside_the_original() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let preview = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(6, 600, 400), 90).unwrap();
    let dng = shoebox_fixtures::Dng {
        preview: Some(preview),
        shot: Some(shoebox_fixtures::Shot {
            datetime: "2026:05:01 09:00:00",
            subsec: "10",
            orientation: 1,
            serial: "7001234",
        }),
        ..shoebox_fixtures::Dng::new(600, 400)
    };
    let original = dng.write(|x, y| 1500 + ((x * 7 + y * 13) % 600) as u16 * 15);
    std::fs::write(root.join("DSC_0300.dng"), &original).unwrap();
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 1100.0));
    settle(&mut h, Duration::from_secs(60));
    wait_for(&mut h, Duration::from_secs(10), "presets", |a| {
        a.export_state().0 > 0
    });
    h.get_by_label("DSC_0300.dng").click();
    h.run_steps(2);
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::E,
    );
    h.run_steps(3);
    // The first preset's editor: AVIF, beside the original.
    h.get_by_label("JPEG · Full size").click();
    h.run_steps(3);
    h.get_by_value("JPEG").click();
    h.run_steps(3);
    h.get_by_label("AVIF").click();
    h.run_steps(3);
    h.get_by_value("A folder outside the library").click();
    h.run_steps(3);
    h.get_by_label("Beside the original, in the library")
        .click();
    h.run_steps(3);
    assert!(
        h.query_by_value("Beside the original, in the library")
            .is_some()
    );
    snapshot(&mut h, "export-avif-beside");
    h.get_all_by_label("Export").last().unwrap().click();
    let avif = root.join("DSC_0300_JPEG · Full size.avif");
    wait_for(&mut h, Duration::from_secs(60), "AVIF beside", |_| {
        avif.exists()
    });
    wait_for(&mut h, Duration::from_secs(30), "in its stack", |a| {
        a.photo_count() == 2 && a.shown_names() == ["DSC_0300.dng"]
    });
    wait_for(&mut h, Duration::from_secs(10), "listed", |a| {
        a.export_state().2 == Some(1)
    });

    // A linear DNG beside it too, with the same preset (no dialog).
    h.state_mut().edit_export_presets(|p| {
        p.format = shoebox_develop::export::FileFormat::LinearDng;
    });
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT | egui::Modifiers::ALT,
        egui::Key::E,
    );
    let linear = root.join("DSC_0300_JPEG · Full size.dng");
    wait_for(&mut h, Duration::from_secs(60), "DNG beside", |_| {
        linear.exists()
    });
    wait_for(&mut h, Duration::from_secs(30), "in its stack", |a| {
        a.photo_count() == 3 && a.shown_names() == ["DSC_0300.dng"]
    });
    assert_eq!(std::fs::read(root.join("DSC_0300.dng")).unwrap(), original);
    let back = shoebox_decode::raw::decode(&std::fs::read(&linear).unwrap()).unwrap();
    assert!(back.linear.is_some());

    // The linear DNG in Develop: past demosaic, nothing for AI denoise.
    h.state_mut()
        .select_names(&["DSC_0300_JPEG · Full size.dng"]);
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    assert_eq!(h.state().develop_linear(), Some(Some(0.0)));
    snapshot(&mut h, "develop-linear-dng");
    // The AVIF: a finished image.
    h.key_press(egui::Key::G);
    h.run_steps(2);
    h.state_mut()
        .select_names(&["DSC_0300_JPEG · Full size.avif"]);
    h.run_steps(2);
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop AVIF", |a| {
        a.develop_ready() && a.develop_linear().is_none()
    });
    assert_eq!(h.state().develop_ai(), (false, None, None));
}

/// Merge to HDR (⌃H): the dialog lists the bracket with each frame's
/// exposure and what it will write; Merge runs in the background, the DNG
/// lands beside the first source, the new photo is selected, a photo of
/// its own (not stacked with its sources), its Info panel names them; it
/// opens in Develop as a linear raw; ⌘Z sends it to the Trash. The sources
/// are never written.
#[test]
fn hdr_merge_from_the_photo_menu() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let mut originals = Vec::new();
    for (i, ev) in [-2.0, 0.0, 2.0].into_iter().enumerate() {
        let b = shoebox_fixtures::Bracket {
            shift: (1.5 * ev, -0.8 * ev),
            seed: i as u64 + 1,
            subsec: ["10", "30", "50"][i],
            ..shoebox_fixtures::Bracket::new(240, 160, ev)
        };
        let p = root.join(format!("DSC_00{}.dng", 41 + i));
        let bytes = b.write();
        std::fs::write(&p, &bytes).unwrap();
        originals.push((p, bytes));
    }
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    h.state_mut()
        .select_names(&["DSC_0041.dng", "DSC_0042.dng", "DSC_0043.dng"]);
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::H);
    h.run_steps(3);
    assert_eq!(h.state().merge_state(), (true, false, 0));
    // Listed (and in the grid behind).
    assert!(h.get_all_by_label("DSC_0043.dng").count() >= 2);
    // EXIF exposures relative to the middle one, the reference.
    assert!(h.query_by_label("+2.0 EV").is_some());
    assert!(h.query_by_label("reference").is_some());
    snapshot(&mut h, "merge-hdr-dialog");
    h.get_by_label("Merge").click();
    h.run_steps(2);
    let merged = root.join("DSC_0041-HDR.dng");
    wait_for(&mut h, Duration::from_secs(60), "the merge", |_| {
        merged.exists()
    });
    wait_for(&mut h, Duration::from_secs(30), "selected", |a| {
        a.photo_count() == 4 && a.current_name().as_deref() == Some("DSC_0041-HDR.dng")
    });
    let (open, running, merges) = h.state().merge_state();
    assert!(!open && !running && merges == 1);
    // A photo of its own: four tiles, not a stack.
    assert_eq!(h.state().shown_count(), 4);
    h.run_steps(3);
    assert!(h.query_by_label("HDR MERGE OF").is_some());
    snapshot(&mut h, "merge-hdr-done");
    for (p, bytes) in &originals {
        assert_eq!(&std::fs::read(p).unwrap(), bytes, "{}", p.display());
    }
    let back = shoebox_decode::raw::decode(&std::fs::read(&merged).unwrap()).unwrap();
    assert!(back.linear.is_some());
    assert!(back.data.iter().any(|&v| v > 1.0), "the window, past white");

    // In Develop: a linear raw, starting from the defaults.
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    assert!(h.state().develop_linear().is_some());
    h.key_press(egui::Key::G);
    h.run_steps(2);

    // ⌘Z: to the Trash.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(30), "undone", |a| {
        !merged.exists() && a.photo_count() == 3
    });
    assert!(t.path().join("trash/DSC_0041-HDR.dng").is_file());
    for (p, bytes) in &originals {
        assert_eq!(&std::fs::read(p).unwrap(), bytes);
    }
}

/// Merge to Focus Stack… from the photo menu: three frames focused near,
/// middle and far (breathing a little); the dialog lists them with the
/// middle one as the reference, Method and Align; the stack lands beside
/// the first as `-Focus.dng`, a photo of its own, selected, its Info
/// listing the frames; ⌘Z sends it to the Trash. The sources are never
/// written.
#[test]
fn focus_stack_from_the_photo_menu() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let mut originals = Vec::new();
    for (i, focus) in [0.0, 0.5, 1.0].into_iter().enumerate() {
        let d = i as f64 - 1.0;
        let f = shoebox_fixtures::FocusFrame {
            scale: 1.0 + 0.004 * d,
            shift: (0.6 * d, -0.4 * d),
            seed: i as u64 + 1,
            subsec: ["10", "30", "50"][i],
            ..shoebox_fixtures::FocusFrame::new(240, 160, focus)
        };
        let p = root.join(format!("DSC_0{}.dng", 101 + i));
        let bytes = f.write();
        std::fs::write(&p, &bytes).unwrap();
        originals.push((p, bytes));
    }
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    h.state_mut()
        .select_names(&["DSC_0101.dng", "DSC_0102.dng", "DSC_0103.dng"]);
    h.run_steps(2);
    h.get_by_label("DSC_0102.dng").click_secondary();
    h.run_steps(2);
    h.get_by_label("Merge to Focus Stack…").click();
    h.run_steps(3);
    assert_eq!(h.state().merge_state(), (true, false, 0));
    assert!(h.query_by_label("Merge to Focus Stack").is_some());
    assert!(h.get_all_by_label("DSC_0103.dng").count() >= 2);
    // The middle frame is the reference; no exposures compared.
    assert!(h.query_by_label("reference").is_some());
    assert!(h.query_by_label("+0.0 EV").is_none());
    assert!(h.query_by_label("Method").is_some());
    assert!(h.query_by_label("Align the photos").is_some());
    snapshot(&mut h, "merge-focus-dialog");
    h.get_by_label("Merge").click();
    h.run_steps(2);
    let merged = root.join("DSC_0101-Focus.dng");
    wait_for(&mut h, Duration::from_secs(60), "the stack", |_| {
        merged.exists()
    });
    wait_for(&mut h, Duration::from_secs(30), "selected", |a| {
        a.photo_count() == 4 && a.current_name().as_deref() == Some("DSC_0101-Focus.dng")
    });
    let (open, running, merges) = h.state().merge_state();
    assert!(!open && !running && merges == 1);
    assert_eq!(h.state().shown_count(), 4);
    h.run_steps(3);
    assert!(h.query_by_label("FOCUS STACK OF").is_some());
    snapshot(&mut h, "merge-focus-done");
    for (p, bytes) in &originals {
        assert_eq!(&std::fs::read(p).unwrap(), bytes, "{}", p.display());
    }
    // A linear raw, cut a little to what all three frames cover.
    let back = shoebox_decode::raw::decode(&std::fs::read(&merged).unwrap()).unwrap();
    assert!(back.linear.is_some());
    assert!(back.width < 240 && back.width > 230, "{}", back.width);
    assert!(back.height < 160 && back.height > 152, "{}", back.height);

    // ⌘Z: to the Trash.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(30), "undone", |a| {
        !merged.exists() && a.photo_count() == 3
    });
    assert!(t.path().join("trash/DSC_0101-Focus.dng").is_file());
    for (p, bytes) in &originals {
        assert_eq!(&std::fs::read(p).unwrap(), bytes);
    }
}

/// Merge to Panorama (⌃M): the dialog lists the frames with the projection
/// and crop; Merge stitches them in the background, the DNG lands beside
/// the first as `-Pano`, wider than any frame, the new photo is selected,
/// a photo of its own whose Info panel names its sources; it opens in
/// Develop as a linear raw; ⌘Z sends it to the Trash. The sources are never
/// written.
#[test]
fn panorama_from_the_photo_menu() {
    use shoebox_fixtures::pano::{pano_light, ray, rotation};
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("lib");
    std::fs::create_dir_all(&root).unwrap();
    let (w, h) = (600u32, 400u32);
    let focal = 40.0 / 36f64.hypot(24.0) * f64::from(w).hypot(f64::from(h));
    let mut originals = Vec::new();
    for (i, yaw) in [-28.0, 0.0, 28.0].into_iter().enumerate() {
        let r = rotation(yaw, 0.5 * i as f64, 0.0);
        let f = shoebox_fixtures::PanoFrame {
            seed: i as u64 + 1,
            subsec: ["10", "30", "50"][i],
            ..shoebox_fixtures::PanoFrame::new(w, h)
        };
        let p = root.join(format!("DSC_02{:02}.dng", i + 1));
        let bytes = f.write(|x, y| pano_light(ray(&r, focal, w, h, x, y)));
        std::fs::write(&p, &bytes).unwrap();
        originals.push((p, bytes));
    }
    let p = paths(t.path(), std::slice::from_ref(&root));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    h.state_mut()
        .select_names(&["DSC_0201.dng", "DSC_0202.dng", "DSC_0203.dng"]);
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::M);
    h.run_steps(3);
    assert_eq!(h.state().merge_state(), (true, false, 0));
    assert!(h.query_by_label("Merge to Panorama").is_some());
    assert!(h.query_by_label("Projection").is_some());
    assert!(h.query_by_label("Crop to the largest rectangle").is_some());
    assert!(h.get_all_by_label("DSC_0203.dng").count() >= 2);
    snapshot(&mut h, "merge-pano-dialog");
    h.get_by_label("Merge").click();
    h.run_steps(2);
    let merged = root.join("DSC_0201-Pano.dng");
    wait_for(&mut h, Duration::from_secs(120), "the panorama", |a| {
        assert_eq!(a.error_message(), None);
        merged.exists()
    });
    wait_for(&mut h, Duration::from_secs(30), "selected", |a| {
        a.photo_count() == 4 && a.current_name().as_deref() == Some("DSC_0201-Pano.dng")
    });
    let (open, running, merges) = h.state().merge_state();
    assert!(!open && !running && merges == 1);
    assert_eq!(h.state().shown_count(), 4);
    h.run_steps(3);
    assert!(h.query_by_label("PANORAMA OF").is_some());
    snapshot(&mut h, "merge-pano-done");
    for (p, bytes) in &originals {
        assert_eq!(&std::fs::read(p).unwrap(), bytes, "{}", p.display());
    }
    let back = shoebox_decode::raw::decode(&std::fs::read(&merged).unwrap()).unwrap();
    assert!(back.linear.is_some());
    assert!(
        back.width > w as usize * 3 / 2,
        "{}×{} from {w}-pixel frames",
        back.width,
        back.height
    );

    // In Develop: a linear raw, starting from the defaults.
    h.key_press(egui::Key::D);
    wait_for(&mut h, Duration::from_secs(30), "develop", |a| {
        a.develop_ready()
    });
    assert!(h.state().develop_linear().is_some());
    h.key_press(egui::Key::G);
    h.run_steps(2);

    // ⌘Z: to the Trash.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(30), "undone", |a| {
        !merged.exists() && a.photo_count() == 3
    });
    assert!(t.path().join("trash/DSC_0201-Pano.dng").is_file());
    for (p, bytes) in &originals {
        assert_eq!(&std::fs::read(p).unwrap(), bytes);
    }
}

/// PHOTOS-SPEC Phase 4: iCloud Photos as Import's source, against a fake
/// Photos library: the plan (hidden left out), an album made a keyword,
/// the copies with ★5 for the favourite, the Info panel's row, nothing new
/// the second time, Settings' tab, and undo from History.
#[test]
fn import_from_icloud_photos() {
    use shoebox_core::photos::fake::FakeSource;
    use shoebox_core::photos::{Access, Album, Asset, AssetId, Media, Resource, ResourceKind};
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let img = |n: u64, when: &'static str| {
        let shot = shoebox_fixtures::Shot {
            datetime: when,
            subsec: "",
            orientation: 1,
            serial: "1",
        };
        let j = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(n, 320, 240), 85).unwrap();
        shoebox_fixtures::with_exif(&j, &shot)
    };
    // A photo so the library isn't empty.
    std::fs::write(lib.join("old.jpg"), img(9, "2020:01:01 10:00:00")).unwrap();
    let fake = std::sync::Arc::new(FakeSource::new());
    fake.answer(Access::Full);
    // The fixtures' EXIF says +09:00: Photos' UTC dates agree 9 hours earlier.
    for (n, when, utc) in [
        (1u32, "2024:06:03 14:15:02", "2024-06-03T05:15:02Z"),
        (2, "2024:06:04 09:00:00", "2024-06-04T00:00:00Z"),
        (3, "2024:06:05 09:00:00", "2024-06-05T00:00:00Z"),
        (4, "2024:06:06 09:00:00", "2024-06-06T00:00:00Z"),
    ] {
        let bytes = img(u64::from(n), when);
        fake.add(
            Asset {
                id: AssetId {
                    cloud: Some(format!("C{n}")),
                    local: format!("L{n}"),
                },
                media: Media::Photo,
                created: Some(utc.parse().unwrap()),
                modified: None,
                location: None,
                favourite: n == 1,
                hidden: n == 4,
                burst: None,
                live: false,
                width: 320,
                height: 240,
                resources: vec![Resource {
                    kind: ResourceKind::Original,
                    file_name: format!("IMG_000{n}.JPG"),
                    size: Some(bytes.len() as u64),
                }],
                albums: if n == 2 { vec!["A1".into()] } else { vec![] },
                edited: None,
            },
            vec![bytes],
        );
    }
    fake.add_album(Album {
        id: "A1".into(),
        name: "Japan".into(),
        folders: vec!["Trips".into()],
    });
    let mut p = paths(&base, std::slice::from_ref(&lib));
    p.photos = fake.clone();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Import…").click();
    h.run_steps(2);
    h.get_by_label("iCloud Photos").click();
    h.run_steps(2);
    h.get_by_label("Find photos").click();
    wait_for(&mut h, Duration::from_secs(30), "iCloud plan", |a| {
        a.icloud_counts().is_some()
    });
    assert_eq!(
        h.state().icloud_counts(),
        Some((3, 0)),
        "the hidden one left out"
    );
    // A large library's listing shows its stage and progress, with Stop.
    h.state_mut()
        .show_listing(Some(shoebox_core::photos::Listing::Photos {
            done: 12_000,
            total: 28_431,
        }));
    h.run_steps(2);
    assert!(
        h.query_by_label_contains("Reading your photos: 12,000 of 28,431")
            .is_some()
    );
    assert!(h.query_by_label("Stop").is_some());
    snapshot(&mut h, "icloud-0-listing");
    h.state_mut().show_listing(None);
    h.run_steps(2);
    h.run_steps(2);
    assert!(
        h.query_by_label_contains("hidden in Photos, left out")
            .is_some()
    );
    h.get_by_label("Trips › Japan: Keyword").click();
    h.run_steps(2);
    snapshot(&mut h, "icloud-1-plan");
    assert_eq!(fake.downloads(), 0, "nothing downloaded before confirming");
    h.get_by_label("Import 3 photos").click();
    wait_for(&mut h, Duration::from_secs(30), "iCloud import", |a| {
        a.icloud_report().is_some()
    });
    assert_eq!(h.state().icloud_report(), Some((3, 0)));
    let d = lib.join("2024/2024-06-03");
    let fav = d.join("20240603-141502.jpg");
    assert!(fav.exists());
    let side = |p: &Path| std::fs::read_to_string(shoebox_core::sidecar::per_file(p)).unwrap();
    assert!(side(&fav).contains("Rating"), "★5 for the favourite");
    let album = lib.join("2024/2024-06-04/20240604-090000.jpg");
    assert!(
        side(&album).contains("Albums|Trips|Japan"),
        "the album as a keyword"
    );
    wait_for(&mut h, Duration::from_secs(30), "catalog", |a| {
        a.photo_count() == 4
    });
    snapshot(&mut h, "icloud-2-done");

    // The Info panel says where it came from.
    h.get_by_label("Grid").click();
    h.run_steps(2);
    h.get_by_label("20240603-141502.jpg").click();
    wait_for(&mut h, Duration::from_secs(10), "info row", |_| true);
    let start = Instant::now();
    while h.query_by_label("From iCloud Photos").is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "no From iCloud Photos row"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }

    // Again: nothing new, nothing downloaded.
    let downloads = fake.downloads();
    h.get_by_label("Import…").click();
    h.run_steps(2);
    h.get_by_label("Find photos").click();
    wait_for(&mut h, Duration::from_secs(30), "re-plan", |a| {
        a.icloud_counts() == Some((0, 3))
    });
    assert_eq!(fake.downloads(), downloads);

    h.state_mut().open_icloud_settings();
    h.run_steps(3);
    assert!(h.query_by_label("Copy Live Photos' movies").is_some());
    snapshot(&mut h, "icloud-3-settings");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    // Undo from History: the copies go to the (test) trash.
    h.get_by_label("History").click();
    wait_for(&mut h, Duration::from_secs(10), "history", |a| {
        !a.batches().is_empty()
    });
    h.run_steps(2);
    h.get_all_by_label("Undo").last().unwrap().click();
    wait_for(&mut h, Duration::from_secs(30), "undo", |_| !fav.exists());
    assert!(!album.exists());
}

/// PHOTOS-SPEC Phase 5: Send to Photos against a fake Photos library. A
/// photo copied from Photos and not edited isn't sent; one of the
/// library's own becomes a new photo there (rendered as an export); the
/// Info panel says so; sending again is up to date; History's Undo takes it
/// back out of Photos.
#[test]
fn send_to_photos() {
    use shoebox_core::photos::fake::FakeSource;
    use shoebox_core::photos::{Access, Asset, AssetId, Media, Resource, ResourceKind};
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let img = |n: u64, when: &'static str| {
        let shot = shoebox_fixtures::Shot {
            datetime: when,
            subsec: "",
            orientation: 1,
            serial: "1",
        };
        let j = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(n, 320, 240), 85).unwrap();
        shoebox_fixtures::with_exif(&j, &shot)
    };
    std::fs::write(lib.join("mine.jpg"), img(9, "2020:01:01 10:00:00")).unwrap();
    let fake = std::sync::Arc::new(FakeSource::new());
    fake.answer(Access::Full);
    let bytes = img(1, "2024:06:03 14:15:02");
    fake.add(
        Asset {
            id: AssetId {
                cloud: Some("C1".into()),
                local: "L1".into(),
            },
            media: Media::Photo,
            created: Some("2024-06-03T05:15:02Z".parse().unwrap()),
            modified: None,
            location: None,
            favourite: false,
            hidden: false,
            burst: None,
            live: false,
            width: 320,
            height: 240,
            resources: vec![Resource {
                kind: ResourceKind::Original,
                file_name: "IMG_0001.JPG".into(),
                size: Some(bytes.len() as u64),
            }],
            albums: vec![],
            edited: None,
        },
        vec![bytes],
    );
    let mut p = paths(&base, std::slice::from_ref(&lib));
    p.photos = fake.clone();
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    // Copy the Photos one in first.
    h.get_by_label("Import…").click();
    h.run_steps(2);
    h.get_by_label("iCloud Photos").click();
    h.run_steps(2);
    h.get_by_label("Find photos").click();
    wait_for(&mut h, Duration::from_secs(30), "plan", |a| {
        a.icloud_counts().is_some()
    });
    h.get_by_label("Import 1 photo").click();
    wait_for(&mut h, Duration::from_secs(30), "import", |a| {
        a.icloud_report().is_some()
    });
    wait_for(&mut h, Duration::from_secs(30), "catalog", |a| {
        a.photo_count() == 2
    });

    // Both in the grid, both selected, Send to Photos.
    h.get_by_label("Grid").click();
    h.run_steps(2);
    h.get_by_label("mine.jpg").click();
    h.get_by_label("20240603-141502.jpg")
        .click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    h.state_mut().open_send_to_photos();
    wait_for(&mut h, Duration::from_secs(30), "send plan", |a| {
        a.send_to_photos_rows().len() == 2
    });
    let rows = h.state().send_to_photos_rows();
    assert_eq!(rows["mine.jpg"], "A new photo in Photos");
    assert!(rows["20240603-141502.jpg"].starts_with("Not sent: already in Photos"));
    h.run_steps(3);
    snapshot(&mut h, "push-1-dialog");
    h.get_by_label("Send 1 photo").click();
    wait_for(&mut h, Duration::from_secs(60), "sent", |a| {
        !a.send_to_photos_open()
    });
    assert_eq!(fake.pushes(), 1);
    let sent = fake.asset("pushed-1").expect("a new asset in Photos");
    assert_eq!(sent.resources[0].file_name, "mine.jpg");
    assert!(
        fake.file("pushed-1", ResourceKind::Original)
            .unwrap()
            .starts_with(&[0xFF, 0xD8]),
        "a JPEG"
    );

    // The Info panel says it was sent.
    h.get_by_label("mine.jpg").click();
    let start = Instant::now();
    while h.query_by_label("Sent to Photos").is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "no Sent to Photos row"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    snapshot(&mut h, "push-2-info");

    // Again: up to date.
    h.state_mut().open_send_to_photos();
    wait_for(&mut h, Duration::from_secs(30), "again", |a| {
        a.send_to_photos_rows()
            .get("mine.jpg")
            .is_some_and(|r| r.contains("up to date"))
    });
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    // Undo (the toolbar's, as ⌘Z) takes the newest batch, the send, back
    // out of Photos first, and its record with it.
    h.get_by_label("History").click();
    wait_for(&mut h, Duration::from_secs(10), "history", |a| {
        a.batches().len() == 2
    });
    h.run_steps(2);
    h.get_all_by_label("Undo").next().unwrap().click();
    wait_for(&mut h, Duration::from_secs(30), "undone in Photos", |_| {
        fake.asset("pushed-1").is_none()
    });
    h.state_mut().open_send_to_photos();
    wait_for(&mut h, Duration::from_secs(30), "new again", |a| {
        a.send_to_photos_rows()
            .get("mine.jpg")
            .is_some_and(|r| r == "A new photo in Photos")
    });
}

/// All Photos opens at its newest photos (the end, sorted by capture
/// time), as Photos does; choosing All photos again goes back there.
/// Tasks → Activity lists what's running; a download that has waited
/// says it's waiting for its drive, here and in the status bar.
#[test]
fn activity_shows_what_waits_for_the_drive() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let img = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(1, 320, 240), 85).unwrap();
    std::fs::write(lib.join("a.jpg"), img).unwrap();
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1200.0, 800.0));
    settle(&mut h, Duration::from_secs(60));
    h.get_by_label_contains("Activity").click();
    h.run_steps(3);
    assert!(h.query_by_label("Nothing running.").is_some());

    let file = base
        .join("Library/CloudStorage/GoogleDrive-someone@example.com/My Drive/Photo Library/b.jpg");
    let board = h.state().activity();
    let task = board.start_since(
        shoebox_app::activity::Kind::Download,
        "Downloading",
        Some(file.clone()),
        std::time::Instant::now() - Duration::from_secs(190),
    );
    h.run_steps(3);
    assert_eq!(
        h.query_all_by_label("Waiting for Google Drive").count(),
        2,
        "in the list and the status bar"
    );
    assert!(h.query_by_label("3 min").is_some());
    assert!(
        h.query_by_label_contains("b.jpg  ·  ").is_some(),
        "the file, by name"
    );
    assert!(
        h.query_by_label_contains("Activity  1").is_some(),
        "the Tasks row counts it"
    );
    snapshot(&mut h, "activity-waiting");
    drop(task);
    h.run_steps(3);
    assert!(h.query_by_label("Nothing running.").is_some());

    // The duplicate finder shows its count, and says when it has stood
    // still: a file its drive sent back to the cloud mid-run.
    let task = board.start(
        shoebox_app::activity::Kind::Library,
        "Finding duplicates",
        None,
    );
    let now = std::time::Instant::now();
    h.state_mut().pretend_dupe_progress(48, 900, now);
    h.run_steps(3);
    assert!(h.query_by_label("48 of 900").is_some());
    assert!(
        h.query_by_label_contains("waiting for a cloud drive")
            .is_none()
    );
    h.state_mut()
        .pretend_dupe_progress(48, 900, now - Duration::from_secs(400));
    h.run_steps(3);
    assert_eq!(
        h.query_all_by_label(
            "Finding duplicates · no file finished for 6 min: waiting for a cloud drive?"
        )
        .count(),
        2,
        "in the list and the status bar"
    );
    drop(task);
}

/// The first click after opening doesn't move the grid: the photos on
/// screen stay where they were.
#[test]
fn the_first_click_keeps_the_grid_still() {
    let t = tempfile::tempdir().unwrap();
    let lib = t
        .path()
        .join("CloudStorage/GoogleDrive-someone@example.com/My Drive/Pictures/Photo Library");
    let dir = lib.join("_undated/960x640");
    std::fs::create_dir_all(&dir).unwrap();
    let img = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(9, 300, 200), 85).unwrap();
    for n in 0..200u32 {
        let shot = shoebox_fixtures::Shot {
            datetime: Box::leak(
                format!(
                    "2024:01:{:02} {:02}:{:02}:00",
                    1 + n / 100,
                    (n / 60) % 24,
                    n % 60
                )
                .into_boxed_str(),
            ),
            subsec: "",
            orientation: 1,
            serial: "1",
        };
        std::fs::write(
            dir.join(format!("2012-04-06T10-53-{n:03}.jpg")),
            shoebox_fixtures::with_exif(&img, &shot),
        )
        .unwrap();
    }
    let p = paths(t.path(), std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(120));
    h.run_steps(3);
    // Up into the middle, where a small change in row height adds up.
    h.event(egui::Event::PointerMoved(egui::pos2(800.0, 500.0)));
    for _ in 0..6 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 400.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(5);
    // The highest photo wholly on screen.
    let name = (0..200u32)
        .map(|n| format!("2012-04-06T10-53-{n:03}.jpg"))
        .filter_map(|n| {
            let y = h.query_by_label(&n)?.rect().min.y;
            Some((y, n))
        })
        .filter(|(y, _)| *y > 100.0)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, n)| n)
        .unwrap();
    let before = h.get_by_label(&name).rect();
    // The first click fills the Info panel, which widens it: fewer pixels
    // for the grid, smaller tiles.
    h.get_by_label(&name).click();
    h.run_steps(5);
    let after = h.query_by_label(&name).map(|t| t.rect());
    snapshot(&mut h, "first-click");
    let after = after.unwrap_or_else(|| panic!("{name} scrolled away"));
    assert!(
        (after.min.y - before.min.y).abs() < 8.0,
        "{name} moved from {before:?} to {after:?}"
    );
}

#[test]
fn all_photos_opens_at_the_newest() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let img = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(9, 300, 200), 85).unwrap();
    for n in 0..200u32 {
        let shot = shoebox_fixtures::Shot {
            datetime: Box::leak(
                format!(
                    "2024:01:{:02} {:02}:{:02}:00",
                    1 + n / 100,
                    (n / 60) % 24,
                    n % 60
                )
                .into_boxed_str(),
            ),
            subsec: "",
            orientation: 1,
            serial: "1",
        };
        std::fs::write(
            lib.join(format!("P_{n:03}.jpg")),
            shoebox_fixtures::with_exif(&img, &shot),
        )
        .unwrap();
    }
    let p = paths(t.path(), std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(120));
    h.run_steps(3);
    let visible = |h: &Harness<'_, ShoeboxApp>, name: &str| {
        h.query_by_label(name)
            .is_some_and(|n| n.rect().min.y > 40.0 && n.rect().max.y < 860.0)
    };
    assert!(visible(&h, "P_199.jpg"), "the newest on screen");
    assert!(!visible(&h, "P_000.jpg"), "not the oldest");
    snapshot(&mut h, "all-photos-newest");

    // Scroll to the top, then choose All photos: back at the newest.
    h.event(egui::Event::PointerMoved(egui::pos2(800.0, 500.0)));
    for _ in 0..60 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 400.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(3);
    assert!(visible(&h, "P_000.jpg"), "scrolled to the top");
    h.get_by_label("All photos  200").click();
    h.run_steps(5);
    assert!(visible(&h, "P_199.jpg"), "back at the newest");
}

/// Find dates (SPEC "Dates"): clues from names and folders, grouped;
/// Accept dates each photo by its own after the dry run, ⌘Z takes it back;
/// a date typed in dates a clueless group; Set Date… from the photo menu.
#[test]
fn find_dates_suggests_and_sets_dates() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib");
    let img = |seed| shoebox_fixtures::jpeg(&shoebox_fixtures::scene(seed, 320, 240), 85).unwrap();
    let chat = lib.join("_undated/320x240");
    std::fs::create_dir_all(&chat).unwrap();
    std::fs::write(chat.join("IMG_20190312_101530.jpg"), img(1)).unwrap();
    std::fs::write(chat.join("IMG_20190312_181002.jpg"), img(2)).unwrap();
    let trip = lib.join("Scans/March 2014");
    std::fs::create_dir_all(&trip).unwrap();
    std::fs::write(trip.join("scan1.jpg"), img(3)).unwrap();
    let none = lib.join("Misc");
    std::fs::create_dir_all(&none).unwrap();
    std::fs::write(none.join("photo.jpg"), img(4)).unwrap();
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Find dates").click();
    h.run_steps(2);
    h.get_by_label_contains("Look for clues").click();
    wait_for(&mut h, Duration::from_secs(60), "clues", |a| {
        shoebox_app::views::dates::looked_at(a) == 4
            && shoebox_app::views::dates::progress(a).is_none()
    });
    h.run_steps(3);
    assert!(h.query_by_label_contains("4 undated photos").is_some());
    assert!(
        h.query_by_label_contains("12 March 2019, each at its own time · sure")
            .is_some(),
        "the file names' day"
    );
    assert!(
        h.query_by_label_contains("March 2014 (approximate) · likely")
            .is_some()
    );
    assert!(h.query_by_label_contains("No clues").is_some());
    snapshot(&mut h, "find-dates");

    // Accept the names' dates: the dry run, then one batch.
    h.get_all_by_label("Accept…").next().unwrap().click();
    h.run_steps(3);
    assert!(
        h.query_by_label("12 March 2019, 10:15:30").is_some(),
        "the dry run"
    );
    h.get_by_label("Set 2 dates").click();
    wait_for(&mut h, Duration::from_secs(10), "dated", |a| {
        a.capture_time_of("IMG_20190312_181002.jpg").is_some()
    });
    assert_eq!(
        h.state()
            .capture_time_of("IMG_20190312_101530.jpg")
            .as_deref(),
        Some("2019:03:12 10:15:30")
    );
    let side = std::fs::read_to_string(chat.join("IMG_20190312_181002.jpg.xmp")).unwrap();
    assert!(side.contains("2019-03-12T18:10:02"), "{side}");
    h.run_steps(3);
    assert!(h.query_by_label_contains("2 undated photos").is_some());

    // ⌘Z takes both back.
    settle(&mut h, Duration::from_secs(30));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_for(&mut h, Duration::from_secs(10), "undone", |a| {
        a.capture_time_of("IMG_20190312_181002.jpg").is_none()
            && a.capture_time_of("IMG_20190312_101530.jpg").is_none()
    });

    // A date typed for the clueless one.
    h.run_steps(3);
    let field = h
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .last()
        .unwrap();
    field.click();
    h.run_steps(1);
    h.event(egui::Event::Text("2001-06".into()));
    h.run_steps(2);
    h.get_by_label("Set June 2001…").click();
    h.run_steps(3);
    h.get_by_label("Set 1 date").click();
    wait_for(&mut h, Duration::from_secs(10), "typed date", |a| {
        a.capture_time_of("photo.jpg").as_deref() == Some("2001:06:15 12:00:00")
    });
    let side = std::fs::read_to_string(none.join("photo.jpg.xmp")).unwrap();
    assert!(
        side.contains("2001-06") && !side.contains("DateTimeOriginal"),
        "{side}"
    );
}

/// Find dates, Phase 2: a frame between two dated frames of the camera is
/// dated by them; Show beside… opens Grid at the nearest dated photo.
#[test]
fn find_dates_from_the_library_and_show_beside() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib");
    let roll = lib.join("roll");
    std::fs::create_dir_all(&roll).unwrap();
    let shot = |dt: &'static str| shoebox_fixtures::Shot {
        datetime: dt,
        subsec: "",
        orientation: 1,
        serial: "1",
    };
    let jpg = |seed| shoebox_fixtures::jpeg(&shoebox_fixtures::scene(seed, 320, 240), 85).unwrap();
    std::fs::write(
        roll.join("DSC_0010.JPG"),
        shoebox_fixtures::with_exif(&jpg(1), &shot("2019:03:12 10:00:00")),
    )
    .unwrap();
    std::fs::write(roll.join("DSC_0011.JPG"), jpg(2)).unwrap();
    std::fs::write(
        roll.join("DSC_0012.JPG"),
        shoebox_fixtures::with_exif(&jpg(3), &shot("2019:03:12 10:00:40")),
    )
    .unwrap();
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Find dates").click();
    h.run_steps(2);
    h.get_by_label_contains("Look for clues").click();
    wait_for(&mut h, Duration::from_secs(60), "clues", |a| {
        shoebox_app::views::dates::looked_at(a) == 1
            && shoebox_app::views::dates::progress(a).is_none()
    });
    h.run_steps(3);
    assert!(
        h.query_by_label_contains("12 March 2019 (approximate) · sure")
            .is_some()
    );
    assert!(
        h.query_by_label_contains("between frames DSC_0010 and DSC_0012")
            .is_some()
    );
    assert!(
        h.query_by_label_contains("likely…").is_none(),
        "the neighbours agree: no second button for the same day"
    );
    // Duplicates hasn't run: the note says so.
    assert!(
        h.query_by_label_contains("none of the dated photos has been hashed yet")
            .is_some()
    );
    snapshot(&mut h, "find-dates-library");

    h.get_by_label("Show beside…").click();
    h.run_steps(4);
    assert_eq!(h.state().view_name(), View::Grid);
    assert_eq!(h.state().current_name().as_deref(), Some("DSC_0012.JPG"));

    h.get_by_label("Find dates").click();
    h.run_steps(3);
    h.get_by_label("Open Duplicates").click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Duplicates);
}

/// Find dates, Phase 3: a date printed in the picture is read (Vision) and
/// suggested; the picture pass shows in Activity's progress.
#[cfg(target_os = "macos")]
#[test]
fn find_dates_reads_text_in_the_picture() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib/Misc");
    std::fs::create_dir_all(&lib).unwrap();
    let ticket = shoebox_fixtures::printed("25/12/2003", 10);
    std::fs::write(
        lib.join("ticket.jpg"),
        shoebox_fixtures::jpeg(&ticket, 92).unwrap(),
    )
    .unwrap();
    let p = paths(&base, &[base.join("lib")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Find dates").click();
    h.run_steps(2);
    h.get_by_label_contains("Look for clues").click();
    wait_for(&mut h, Duration::from_secs(120), "the picture pass", |a| {
        shoebox_app::views::dates::looked_at(a) == 1
            && shoebox_app::views::dates::progress(a).is_none()
    });
    h.run_steps(3);
    snapshot(&mut h, "find-dates-text");
    assert!(
        h.query_by_label_contains("25 December 2003 (approximate) · likely")
            .is_some(),
        "the date in the picture"
    );
    assert!(
        h.query_by_label_contains("text in the photo: “25/12/2003”")
            .is_some()
    );
}

/// Find dates, Phase 4: Accept all sure ones takes only the sure
/// suggestions, after the dry run; then the dated photo in `_undated/` is
/// offered a move where the import template puts it, as a move like any
/// other.
#[test]
fn find_dates_accepts_the_sure_ones_and_files_them() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib");
    let undated = lib.join("_undated/320x240");
    std::fs::create_dir_all(&undated).unwrap();
    let jpg = |seed| shoebox_fixtures::jpeg(&shoebox_fixtures::scene(seed, 320, 240), 85).unwrap();
    // Sure: the camera's name. Likely only: a chat app's day.
    std::fs::write(undated.join("IMG_20190312_101530.jpg"), jpg(1)).unwrap();
    std::fs::write(undated.join("IMG-20180701-WA0001.jpg"), jpg(2)).unwrap();
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Find dates").click();
    h.run_steps(2);
    h.get_by_label_contains("Look for clues").click();
    wait_for(&mut h, Duration::from_secs(60), "clues", |a| {
        shoebox_app::views::dates::looked_at(a) == 2
            && shoebox_app::views::dates::progress(a).is_none()
    });
    h.run_steps(3);
    h.get_by_label("Accept all sure ones (1 photo)…").click();
    h.run_steps(3);
    h.get_by_label("Set 1 date").click();
    wait_for(&mut h, Duration::from_secs(10), "dated", |a| {
        a.capture_time_of("IMG_20190312_101530.jpg").is_some()
    });
    assert_eq!(
        h.state().capture_time_of("IMG-20180701-WA0001.jpg"),
        None,
        "not sure"
    );
    settle(&mut h, Duration::from_secs(30));
    h.run_steps(3);
    assert!(
        h.query_by_label_contains("1 photo in _undated/ has a date now")
            .is_some()
    );
    snapshot(&mut h, "find-dates-file");

    h.get_by_label("Move them…").click();
    // The dry run comes back from the library thread.
    let start = Instant::now();
    while h.query_by_label("Move 2 files").is_none() {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
        assert!(start.elapsed() < Duration::from_secs(30), "no dry run");
    }
    h.get_by_label("Move 2 files").click();
    let filed = lib.join("2019/2019-03-12/20190312-101530.jpg");
    let start = Instant::now();
    while !filed.exists() {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
        assert!(start.elapsed() < Duration::from_secs(30), "not filed");
    }
    assert!(
        lib.join("2019/2019-03-12/20190312-101530.jpg.xmp").exists(),
        "its date went with it"
    );
    assert!(!undated.join("IMG_20190312_101530.jpg").exists());
    assert!(undated.join("IMG-20180701-WA0001.jpg").exists());
}

/// Find dates waits for its button; Find Date… in the photo menu looks at
/// just the chosen photos, dated ones too, at once.
#[test]
fn find_dates_waits_and_finds_for_chosen_photos() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib/roll");
    std::fs::create_dir_all(&lib).unwrap();
    let jpg = |seed| shoebox_fixtures::jpeg(&shoebox_fixtures::scene(seed, 320, 240), 85).unwrap();
    let shot = shoebox_fixtures::Shot {
        datetime: "2019:03:12 10:00:00",
        subsec: "",
        orientation: 1,
        serial: "1",
    };
    std::fs::write(
        lib.join("IMG_20190312_100000.jpg"),
        shoebox_fixtures::with_exif(&jpg(1), &shot),
    )
    .unwrap();
    std::fs::write(lib.join("IMG_20180101_120000.jpg"), jpg(2)).unwrap();
    let p = paths(&base, &[base.join("lib")]);
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));

    h.get_by_label("Find dates").click();
    h.run_steps(10);
    assert_eq!(
        shoebox_app::views::dates::looked_at(h.state()),
        0,
        "nothing until asked"
    );
    assert!(h.query_by_label("Look for clues (1 photo)").is_some());

    // The dated photo, chosen: Find Date… looks at it alone.
    h.get_by_label("Grid").click();
    h.run_steps(2);
    h.get_by_label("IMG_20190312_100000.jpg").click();
    h.run_steps(2);
    h.get_by_label("IMG_20190312_100000.jpg").click_secondary();
    h.run_steps(3);
    h.get_by_label("Find Date…").click();
    wait_for(&mut h, Duration::from_secs(20), "the chosen photo", |a| {
        shoebox_app::views::dates::looked_at(a) == 1
            && shoebox_app::views::dates::progress(a).is_none()
    });
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Dates);
    assert!(h.query_by_label_contains("1 chosen photo").is_some());
    assert!(
        h.query_by_label_contains("12 March 2019, 10:00:00 · sure")
            .is_some()
    );

    // Back to every undated photo: the other one, not looked at yet.
    h.get_by_label("All undated photos").click();
    h.run_steps(3);
    assert!(h.query_by_label("Look for clues (1 photo)").is_some());
}

/// The middle of the title bar moves the window on screens without the
/// search box (Duplicates shows just its title there), and not where the
/// search box is.
#[test]
fn the_title_bar_drags_where_there_is_no_search_box() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let img = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(1, 320, 240), 85).unwrap();
    std::fs::write(lib.join("a.jpg"), img).unwrap();
    let p = paths(&base, std::slice::from_ref(&lib));
    let mut h = harness(p, egui::vec2(1400.0, 900.0));
    settle(&mut h, Duration::from_secs(60));
    let drags_at = |h: &mut Harness<'_, ShoeboxApp>, x: f32| {
        let at = egui::pos2(x, 16.0);
        h.event(egui::Event::PointerMoved(at));
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        let mut started = false;
        for k in 1..6 {
            h.event(egui::Event::PointerMoved(
                at + egui::vec2(8.0 * k as f32, 0.0),
            ));
            h.step();
            started |= h.output().viewport_output.values().any(|v| {
                v.commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::StartDrag))
            });
        }
        h.event(egui::Event::PointerButton {
            pos: at + egui::vec2(40.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(2);
        started
    };
    assert!(drags_at(&mut h, 1150.0), "the bar's empty space");
    assert!(!drags_at(&mut h, 700.0), "the search box isn't a handle");
    h.get_all_by_label("Duplicates").last().unwrap().click();
    h.run_steps(3);
    assert_eq!(h.state().view_name(), View::Duplicates);
    assert!(
        drags_at(&mut h, 700.0),
        "Duplicates' title drags the window"
    );
}
