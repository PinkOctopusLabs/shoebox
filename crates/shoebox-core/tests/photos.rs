//! iCloud Photos sync (docs/PHOTOS-SPEC.md, Phases 1–2) against a fake
//! Photos library: names, marks, the sync record, syncing again, and undo.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use jiff::tz::{Offset, TimeZone};
use shoebox_core::cache::Cache;
use shoebox_core::catalog::Catalog;
use shoebox_core::events;
use shoebox_core::fileops::{FileOps, Journal, TrashMode};
use shoebox_core::photos::fake::FakeSource;
use shoebox_core::photos::ledger::{AlbumChoice, Ledgers};
use shoebox_core::photos::model::{Burst, EditedBy};
use shoebox_core::photos::place::{Context, LibraryIndex};
use shoebox_core::photos::plan::{self, Options, Plan};
use shoebox_core::photos::sync::{self, Job, Report};
use shoebox_core::photos::{
    Access, Album, Asset, AssetId, Media, PhotoSource, Resource, ResourceKind,
};
use shoebox_core::scan::{Role, Root};
use shoebox_core::template::{DEFAULT_IMPORT, Template};
use shoebox_fixtures::{Shot, jpeg, scene, with_exif};

fn res(kind: ResourceKind, name: &str, size: usize) -> Resource {
    Resource {
        kind,
        file_name: name.into(),
        size: Some(size as u64),
    }
}

fn asset(n: u32, created: &str, resources: Vec<Resource>) -> Asset {
    Asset {
        id: AssetId {
            cloud: Some(format!("C{n}")),
            local: format!("L{n}"),
        },
        media: Media::Photo,
        created: Some(created.parse().unwrap()),
        modified: Some(created.parse().unwrap()),
        location: None,
        favourite: false,
        hidden: false,
        burst: None,
        width: 600,
        height: 400,
        resources,
        albums: vec![],
        edited: None,
    }
}

fn shot(datetime: &'static str, subsec: &'static str) -> Shot {
    Shot {
        datetime,
        subsec,
        orientation: 1,
        serial: "1",
    }
}

fn photo(seed: u64, s: &Shot) -> Vec<u8> {
    with_exif(&jpeg(&scene(seed, 600, 400), 85).unwrap(), s)
}

struct World {
    _t: tempfile::TempDir,
    base: PathBuf,
    lib: PathBuf,
    other: PathBuf,
    source: FakeSource,
    fo: FileOps,
    cache: Cache,
}

impl World {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let lib = base.join("lib");
        let other = base.join("other");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let fo = FileOps::new(Journal::open(&base.join("journal.sqlite")).unwrap(), vec![])
            .with_trash(TrashMode::Folder(base.join("trash")));
        let source = FakeSource::new();
        source.set_access(Access::Full);
        World {
            _t: t,
            base,
            lib,
            other,
            source,
            fo,
            cache: Cache::open_in_memory().unwrap(),
        }
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.lib.clone(), self.other.clone()]
    }

    fn catalog(&mut self) -> Catalog {
        let roots = self
            .roots()
            .iter()
            .map(|r| Root::new(r, Role::Scan).unwrap())
            .collect();
        Catalog::refresh(&mut self.cache, roots, &|_, _| {})
            .unwrap()
            .0
    }

    fn plan(&self) -> (Plan, Ledgers) {
        let ledgers = Ledgers::load(&self.roots());
        let library = self.source.fetch().unwrap();
        let p = plan::plan(
            &library,
            &ledgers,
            &ledgers.choices[0].albums,
            &Options::default(),
        );
        (p, ledgers)
    }

    /// Plan and run a sync into `into`, with the plan's album choices.
    fn sync(&mut self, into: &Path) -> (Plan, Report) {
        self.sync_with(into, 0, false)
    }

    /// [`World::sync`] with a free-space floor, stopped from the start.
    fn sync_with(&mut self, into: &Path, min_free: u64, stopped: bool) -> (Plan, Report) {
        let catalog = self.catalog();
        let roots = self.roots();
        let mut ledgers = Ledgers::load(&roots);
        let at = roots.iter().position(|r| r == into).unwrap();
        let library = self.source.fetch().unwrap();
        let options = Options::default();
        let p = plan::plan(&library, &ledgers, &ledgers.choices[at].albums, &options);
        let albums: HashMap<String, AlbumChoice> = p
            .albums
            .iter()
            .map(|a| (a.id.clone(), a.choice.clone()))
            .collect();
        let template = Template::parse(DEFAULT_IMPORT).unwrap();
        let mut cx = Context {
            root: into,
            template: &template,
            options: &options,
            albums: &albums,
            tz: TimeZone::fixed(Offset::from_seconds(3600).unwrap()),
            index: LibraryIndex::new(&catalog),
            events: events::load(&roots),
            taken: HashSet::new(),
            seq: 0,
        };
        let staging = self.base.join("cache/photos-staging");
        let job = Job {
            source: &self.source,
            library: &library,
            catalog: &catalog,
            batches: &self.fo,
            roots: &roots,
            writable: &roots,
            staging: &staging,
            run: &format!("run{}", events::now_ms()),
            label: "Sync iCloud Photos",
            step: 2,
            parallel: 3,
            min_free,
        };
        let report = sync::run(
            &job,
            &p,
            &mut ledgers,
            &mut cx,
            &|_| {},
            &AtomicBool::new(stopped),
        )
        .unwrap();
        (p, report)
    }
}

/// Files in a root, without `.shoebox` (relative paths, sorted).
fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().strip_prefix(dir).unwrap().display().to_string())
        .filter(|p| !p.starts_with(".shoebox"))
        .collect();
    v.sort();
    v
}

fn sidecar_text(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// A library of the kinds of assets Photos has.
fn library(w: &World) {
    // The fixtures' EXIF says +09:00, so Photos' dates (UTC) agree with
    // them 9 hours earlier.
    // 1. A camera photo, favourite, in the album Trips › Japan.
    let s1 = shot("2024:06:03 14:15:02", "120");
    let b1 = photo(1, &s1);
    let mut a1 = asset(
        1,
        "2024-06-03T05:15:02.120Z",
        vec![res(ResourceKind::Original, "IMG_0001.JPG", b1.len())],
    );
    a1.favourite = true;
    a1.albums = vec!["A1".into()];
    w.source.add(a1, vec![b1]);
    w.source.add_album(Album {
        id: "A1".into(),
        name: "Japan".into(),
        folders: vec!["Trips".into()],
    });
    // 2. A screenshot: no EXIF date, so Photos' (13:00 UTC, +01:00 here).
    let b2 = jpeg(&scene(2, 600, 400), 80).unwrap();
    let mut a2 = asset(
        2,
        "2024-07-01T13:00:00Z",
        vec![res(ResourceKind::Original, "IMG_0002.PNG.JPG", b2.len())],
    );
    a2.location = Some((35.0116, 135.7681));
    w.source.add(a2, vec![b2]);
    // 3. A Live Photo: the photo and its movie share a name.
    let s3 = shot("2024:08:01 09:00:00", "5");
    let b3 = photo(3, &s3);
    let mov = b"not really a movie".to_vec();
    w.source.add(
        asset(
            3,
            "2024-08-01T00:00:00.500Z",
            vec![
                res(ResourceKind::Original, "IMG_0003.JPG", b3.len()),
                res(ResourceKind::PairedVideo, "IMG_0003.MOV", mov.len()),
            ],
        ),
        vec![b3, mov],
    );
    // 4. Edited in Photos: the render comes as a `-photos` variant.
    let s4 = shot("2024:09:01 10:00:00", "");
    let (b4, e4) = (photo(4, &s4), photo(40, &s4));
    let mut a4 = asset(
        4,
        "2024-09-01T01:00:00Z",
        vec![
            res(ResourceKind::Original, "IMG_0004.JPG", b4.len()),
            res(ResourceKind::Edited, "FullSizeRender.jpg", e4.len()),
        ],
    );
    a4.edited = Some(EditedBy::Other("com.apple.photo".into()));
    w.source.add(a4, vec![b4, e4]);
    // 5. Hidden, and 6. a burst frame nobody kept: left out.
    let b5 = photo(5, &shot("2024:10:01 10:00:00", ""));
    let mut a5 = asset(
        5,
        "2024-10-01T09:00:00Z",
        vec![res(ResourceKind::Original, "IMG_0005.JPG", b5.len())],
    );
    a5.hidden = true;
    w.source.add(a5, vec![b5]);
    let b6 = photo(6, &shot("2024:10:02 10:00:00", ""));
    let mut a6 = asset(
        6,
        "2024-10-02T09:00:00Z",
        vec![res(ResourceKind::Original, "IMG_0006.JPG", b6.len())],
    );
    a6.burst = Some(Burst {
        id: "B".into(),
        representative: false,
        picked: false,
    });
    w.source.add(a6, vec![b6]);
    // 7. Already in the library (an export kept as a backup).
    let b7 = photo(7, &shot("2024:11:01 10:00:00", ""));
    std::fs::create_dir_all(w.lib.join("backup")).unwrap();
    std::fs::write(w.lib.join("backup/IMG_0007.JPG"), &b7).unwrap();
    w.source.add(
        asset(
            7,
            "2024-11-01T09:00:00Z",
            vec![res(ResourceKind::Original, "IMG_0007.JPG", b7.len())],
        ),
        vec![b7],
    );
}

#[test]
fn first_sync_names_marks_and_records() {
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    let (p, report) = w.sync(&lib);
    assert_eq!((p.new.len(), p.hidden, p.burst_frames), (5, 1, 1));
    assert_eq!(report.copied, 4, "{report:?}");
    assert_eq!(report.in_library, 1);
    assert!(report.failed.is_empty());
    assert_eq!(report.dated_by_photos, 1, "the screenshot");
    assert_eq!(
        files(&lib),
        vec![
            "2024/2024-06-03/20240603-141502-120.jpg",
            "2024/2024-06-03/20240603-141502-120.jpg.xmp",
            "2024/2024-07-01/20240701-140000.jpg",
            "2024/2024-07-01/20240701-140000.jpg.xmp",
            "2024/2024-08-01/20240801-090000-5.jpg",
            "2024/2024-08-01/20240801-090000-5.mov",
            "2024/2024-09-01/20240901-100000-photos.jpg",
            "2024/2024-09-01/20240901-100000.jpg",
            "backup/IMG_0007.JPG",
        ]
    );
    // The favourite: ★5 and the album's event.
    let car = sidecar_text(&lib.join("2024/2024-06-03/20240603-141502-120.jpg.xmp"));
    assert!(car.contains("Rating") && car.contains('5'), "{car}");
    assert!(car.contains("Events|Trips|Japan"), "{car}");
    let evs = events::load(std::slice::from_ref(&lib));
    assert_eq!(
        events::path_of(&evs, &evs.iter().find(|e| e.name == "Japan").unwrap().id),
        "Trips|Japan"
    );
    // The screenshot: Photos' date and place in its sidecar, read back as its
    // capture time.
    let car = sidecar_text(&lib.join("2024/2024-07-01/20240701-140000.jpg.xmp"));
    assert!(car.contains("2024-07-01T14:00:00+01:00"), "{car}");
    let catalog = w.catalog();
    let shot = catalog
        .photos
        .iter()
        .find(|p| p.rel.ends_with("20240701-140000.jpg"))
        .unwrap();
    assert_eq!(
        shot.meta.exif.capture_time.as_deref(),
        Some("2024:07:01 14:00:00")
    );
    let (lat, lon) = shot.location().unwrap();
    assert!((lat - 35.0116).abs() < 1e-5 && (lon - 135.7681).abs() < 1e-5);
    // One batch for the whole sync.
    let batches = w.fo.journal().batches().unwrap();
    assert_eq!(batches.len(), 1);
}

#[test]
fn syncing_again_copies_nothing_even_after_moves() {
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    w.sync(&lib);
    let downloads = w.source.downloads();
    // Moved in Finder: still synced.
    std::fs::rename(
        lib.join("2024/2024-06-03/20240603-141502-120.jpg"),
        lib.join("moved.jpg"),
    )
    .unwrap();
    let (p, report) = w.sync(&lib);
    assert!(p.new.is_empty() && p.updated.is_empty(), "{p:?}");
    assert_eq!(p.synced, 5);
    assert_eq!(report.batch, None, "nothing to do, no batch");
    assert_eq!(w.source.downloads(), downloads);
    // Synced into one root, so not copied into the other either.
    let other = w.other.clone();
    let (p, _) = w.sync(&other);
    assert!(p.new.is_empty());
    assert!(files(&other).is_empty());
}

#[test]
fn later_changes_in_photos() {
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    w.sync(&lib);
    // Asset 3 joins an album; asset 4 is edited again in Photos.
    w.source.add_album(Album {
        id: "A2".into(),
        name: "Summer".into(),
        folders: vec![],
    });
    w.source.update("L3", |a, _| a.albums.push("A2".into()));
    let e4b = photo(41, &shot("2024:09:01 10:00:00", ""));
    w.source.update("L4", |a, put| {
        a.modified = Some("2025-01-01T00:00:00Z".parse().unwrap());
        a.resources[1].size = Some(e4b.len() as u64);
        put(ResourceKind::Edited, e4b.clone());
    });
    let (p, report) = w.sync(&lib);
    assert_eq!(p.joined.len(), 1);
    assert_eq!(p.updated.len(), 1);
    assert_eq!(report.marked, 1, "{report:?}");
    let car = sidecar_text(&lib.join("2024/2024-08-01/20240801-090000-5.jpg.xmp"));
    assert!(car.contains("Summer"), "{car}");
    assert!(
        lib.join("2024/2024-09-01/20240901-100000-photos-1.jpg")
            .exists()
    );
    // Once recorded, neither comes back.
    let (p, _) = w.plan();
    assert!(p.joined.is_empty() && p.updated.is_empty(), "{p:?}");
}

#[test]
fn a_failed_download_is_tried_again_next_time() {
    let mut w = World::new();
    library(&w);
    w.source
        .fail("L3", ResourceKind::PairedVideo, "iCloud says no");
    let lib = w.lib.clone();
    let (_, report) = w.sync(&lib);
    assert_eq!(report.failed.len(), 1);
    assert!(!lib.join("2024/2024-08-01/20240801-090000-5.jpg").exists());
    let (p, _) = w.plan();
    assert_eq!(p.new.len(), 1);
}

#[test]
fn undo_takes_the_whole_sync_back() {
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    let before = files(&lib);
    let (_, report) = w.sync(&lib);
    w.fo.undo(report.batch.unwrap()).unwrap();
    assert_eq!(files(&lib), before);
    let (p, _) = w.plan();
    assert_eq!(p.new.len(), 5, "nothing recorded as synced any more");
}

#[test]
fn a_lost_record_falls_back_to_the_files() {
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    w.sync(&lib);
    std::fs::remove_dir_all(lib.join(".shoebox/photos")).unwrap();
    let before = files(&lib);
    let (p, report) = w.sync(&lib);
    assert_eq!(p.new.len(), 5);
    assert_eq!(report.copied, 0, "{report:?}");
    assert_eq!(report.in_library, 5);
    assert_eq!(files(&lib), before);
    let (p, _) = w.plan();
    assert_eq!(p.synced, 5);
}

#[test]
fn a_stop_or_a_full_disk_copies_nothing_more() {
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    let before = files(&lib);
    let (_, report) = w.sync_with(&lib, 0, true);
    assert!(report.stopped && report.copied == 0, "{report:?}");
    let (_, report) = w.sync_with(&lib, u64::MAX, false);
    assert!(report.low_space && report.copied == 0, "{report:?}");
    assert_eq!(files(&lib), before);
    assert_eq!(w.source.downloads(), 0);
    // Both batches were closed, so neither waits for crash recovery.
    assert!(w.fo.journal().incomplete().unwrap().is_empty());
    let (p, _) = w.plan();
    assert_eq!(p.new.len(), 5);
}
