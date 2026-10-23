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
        live: false,
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

/// Phase 5: sending to Photos. A photo that came from Photos goes back as
/// an edit of its asset (one with a Photos edit says it replaces it); any
/// other photo becomes a new asset; unedited ones from Photos and movies
/// aren't sent; sending again is up to date or an update of the same
/// asset; a sync afterwards brings nothing back; undo reverts in Photos and
/// takes the records back.
#[test]
fn sending_to_photos() {
    use shoebox_core::photos::push::{self, Action, Item, Rendered};
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    w.sync(&lib);
    let mine = lib.join("other/DSC_0001.jpg");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, photo(50, &shot("2025:01:01 10:00:00", ""))).unwrap();
    let item = |p: PathBuf, hash: Option<&str>, video: bool| Item {
        photo: p,
        copy: "main".into(),
        render_hash: hash.map(Into::into),
        video,
    };
    let fav = lib.join("2024/2024-06-03/20240603-141502-120.jpg");
    let edited_in_photos = lib.join("2024/2024-09-01/20240901-100000.jpg");
    let items = vec![
        item(fav.clone(), Some("e1"), false),
        item(edited_in_photos.clone(), Some("e4"), false),
        item(mine.clone(), Some("m1"), false),
        item(lib.join("2024/2024-07-01/20240701-140000.jpg"), None, false),
        item(
            lib.join("2024/2024-08-01/20240801-090000-5.mov"),
            None,
            true,
        ),
    ];
    let roots = w.roots();
    let ledgers = Ledgers::load(&roots);
    let look = push::assets_to_look_up(&items, &ledgers, &roots);
    let now = w.source.lookup(&look).unwrap();
    let rows = push::plan(&items, &ledgers, &roots, &now);
    assert!(
        matches!(&rows[0].action, Action::Edit { asset, replaces: false, kept: None } if asset.local == "L1")
    );
    match &rows[1].action {
        Action::Edit {
            asset,
            replaces,
            kept,
        } => {
            assert_eq!(asset.local, "L4");
            assert!(replaces, "asset 4 has an edit made in Photos");
            assert_eq!(
                kept.as_deref(),
                Some(
                    lib.join("2024/2024-09-01/20240901-100000-photos.jpg")
                        .as_path()
                )
            );
        }
        a => panic!("{a:?}"),
    }
    assert_eq!(rows[2].action, Action::New);
    assert_eq!(rows[3].action, Action::InPhotos);
    assert_eq!(rows[4].action, Action::Video);

    // Send the three, from renders in a cache folder.
    let cache = w.base.join("cache/push");
    std::fs::create_dir_all(&cache).unwrap();
    let rendered: Vec<Rendered> = rows
        .iter()
        .filter(|r| r.action.sends())
        .enumerate()
        .map(|(i, r)| {
            let f = cache.join(format!("{i}.jpg"));
            std::fs::write(&f, format!("render {i}")).unwrap();
            Rendered {
                row: r.clone(),
                file: f,
            }
        })
        .collect();
    let mut ledgers = Ledgers::load(&roots);
    let report = push::run(
        &w.source,
        &w.fo,
        &mut ledgers,
        &roots,
        &rendered,
        Some("From Shoebox"),
        "run1",
        "send to Photos",
    )
    .unwrap();
    assert_eq!(
        (report.created, report.edited, report.failed.len()),
        (1, 2, 0),
        "{report:?}"
    );
    assert_eq!(
        w.source.asset("L1").unwrap().edited,
        Some(EditedBy::Shoebox)
    );
    assert_eq!(
        w.source.file("L1", ResourceKind::Edited).unwrap(),
        b"render 0"
    );
    assert_eq!(w.source.album_members("From Shoebox").len(), 3);

    // Again: unchanged ones are up to date; a new edit updates the asset.
    let ledgers = Ledgers::load(&roots);
    let again = push::plan(
        &[
            item(fav.clone(), Some("e1"), false),
            item(mine.clone(), Some("m2"), false),
        ],
        &ledgers,
        &roots,
        &[],
    );
    assert_eq!(again[0].action, Action::UpToDate);
    assert!(matches!(
        &again[1].action,
        Action::Update { created: true, .. }
    ));

    // A sync brings nothing back: not the new asset, not the edits.
    let downloads = w.source.downloads();
    let (p, _) = w.plan();
    assert!(p.new.is_empty() && p.updated.is_empty(), "{p:?}");
    assert_eq!(p.sent, 1);
    assert_eq!(w.source.downloads(), downloads);

    // Undo: in Photos first, then the records.
    let batch = report.batch.unwrap();
    assert_eq!(push::undo(&w.source, &ledgers, batch).unwrap(), 3);
    assert_eq!(w.source.asset("L1").unwrap().edited, None);
    // The asset Shoebox made is gone; the edited ones stay in the album
    // (undo reverts the edit, not the album).
    let left = w.source.album_members("From Shoebox");
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.iter().all(|l| !l.starts_with("pushed-")));
    w.fo.undo(batch).unwrap();
    let ledgers = Ledgers::load(&roots);
    assert!(
        push::plan(&[item(mine, Some("m1"), false)], &ledgers, &roots, &[])[0].action
            == Action::New
    );
}

#[test]
fn undoing_a_send_is_refused_after_a_change_in_photos() {
    use shoebox_core::photos::push::{self, Item, Rendered};
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    w.sync(&lib);
    let roots = w.roots();
    let mut ledgers = Ledgers::load(&roots);
    let fav = lib.join("2024/2024-06-03/20240603-141502-120.jpg");
    let rows = push::plan(
        &[Item {
            photo: fav,
            copy: "main".into(),
            render_hash: Some("e1".into()),
            video: false,
        }],
        &ledgers,
        &roots,
        &[],
    );
    let f = w.base.join("r.jpg");
    std::fs::write(&f, b"render").unwrap();
    let report = push::run(
        &w.source,
        &w.fo,
        &mut ledgers,
        &roots,
        &[Rendered {
            row: rows[0].clone(),
            file: f,
        }],
        None,
        "run1",
        "send to Photos",
    )
    .unwrap();
    // Edited again on a phone, later.
    w.source.update("L1", |a, _| {
        a.edited = Some(EditedBy::Other("com.apple.photo".into()));
        a.modified = Some("2099-01-01T00:00:00Z".parse().unwrap());
    });
    let ledgers = Ledgers::load(&roots);
    let r = push::undo(&w.source, &ledgers, report.batch.unwrap());
    assert!(r.is_err(), "{r:?}");
    assert_eq!(
        w.source.asset("L1").unwrap().edited,
        Some(EditedBy::Other("com.apple.photo".into())),
        "left as it is"
    );
}

/// PhotoKit lists assets without their files (asking for each one's takes
/// minutes on a large library); the files are looked up at download. A
/// sync that way gives exactly the library a fully listed one does: Live
/// Photos' movies (by the Live flag), Photos' edits, no RAW halves that
/// aren't there.
#[test]
fn listing_without_files_syncs_the_same() {
    let mut full = World::new();
    library(&full);
    let lib = full.lib.clone();
    full.sync(&lib);
    let mut lazy = World::new();
    lazy.source.list_without_files();
    library(&lazy);
    let (listed, _) = lazy.plan();
    assert_eq!(listed.new.len(), 5);
    let lib2 = lazy.lib.clone();
    let (_, report) = lazy.sync(&lib2);
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(files(&lib2), files(&lib));
    let (p, _) = lazy.plan();
    assert!(p.new.is_empty() && p.updated.is_empty(), "{p:?}");
}

/// A photo in several albums in Photos (albums hold references, not
/// copies) is copied once, and is in each album's event: Shoebox's events
/// are marks on the photo too. Albums as keywords likewise.
#[test]
fn a_photo_in_several_albums_is_one_copy_in_several_events() {
    let mut w = World::new();
    let s1 = shot("2024:06:03 14:15:02", "120");
    let b1 = photo(1, &s1);
    let mut a1 = asset(
        1,
        "2024-06-03T05:15:02.120Z",
        vec![res(ResourceKind::Original, "IMG_0001.JPG", b1.len())],
    );
    a1.albums = vec!["A1".into(), "A2".into(), "A3".into()];
    w.source.add(a1, vec![b1]);
    for (id, name, folders) in [
        ("A1", "Japan", vec!["Trips".to_owned()]),
        ("A2", "Best of 2024", vec![]),
        ("A3", "Family", vec![]),
    ] {
        w.source.add_album(Album {
            id: id.into(),
            name: name.into(),
            folders,
        });
    }
    let lib = w.lib.clone();
    let (_, report) = w.sync(&lib);
    assert_eq!((report.copied, report.files), (1, 1));
    let jpgs: Vec<String> = files(&lib)
        .into_iter()
        .filter(|f| f.ends_with(".jpg"))
        .collect();
    assert_eq!(
        jpgs,
        ["2024/2024-06-03/20240603-141502-120.jpg"],
        "one copy"
    );
    let catalog = w.catalog();
    let p = catalog
        .photos
        .iter()
        .find(|p| p.rel.ends_with(".jpg"))
        .unwrap();
    for k in ["Events|Trips|Japan", "Events|Best of 2024", "Events|Family"] {
        assert!(
            p.marks.keywords.iter().any(|x| x == k),
            "{k} in {:?}",
            p.marks.keywords
        );
    }
    let evs = events::load(std::slice::from_ref(&lib));
    let members = events::memberships(&catalog, &evs);
    for name in ["Japan", "Best of 2024", "Family"] {
        let id = &evs.iter().find(|e| e.name == name).unwrap().id;
        assert_eq!(members.get(id).map(|m| m.len()), Some(1), "{name}");
    }
}

/// Events made from albums follow them at each sync (Photos → Shoebox):
/// a photo taken out of an album leaves its event, one added joins it, an
/// album renamed in Photos renames its event, unless the event was renamed
/// in Shoebox; photos that didn't come from Photos keep their events; an
/// album deleted in Photos leaves its event in place; undo takes it back.
#[test]
fn events_follow_their_albums() {
    let mut w = World::new();
    library(&w);
    let lib = w.lib.clone();
    w.sync(&lib);
    let p1 = lib.join("2024/2024-06-03/20240603-141502-120.jpg");
    let p3 = lib.join("2024/2024-08-01/20240801-090000-5.jpg");
    // One of the library's own photos, put in the same event in Shoebox.
    let mine = lib.join("mine/DSC_0001.jpg");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, photo(60, &shot("2024:06:05 10:00:00", ""))).unwrap();
    std::fs::write(
        shoebox_core::sidecar::per_file(&mine),
        shoebox_core::xmp::edit(
            None,
            &shoebox_core::marks::Marks {
                keywords: vec!["Events|Trips|Japan".into()],
                ..Default::default()
            }
            .changes_from(&Default::default()),
        )
        .unwrap(),
    )
    .unwrap();
    let keywords = |w: &mut World, p: &Path| -> Vec<String> {
        let c = w.catalog();
        c.photos
            .iter()
            .find(|x| x.path == p)
            .map(|x| x.marks.keywords.clone())
            .unwrap_or_default()
    };
    assert!(keywords(&mut w, &p1).contains(&"Events|Trips|Japan".to_owned()));

    // In Photos: 1 leaves the album, 3 joins it, and it's renamed.
    w.source.update("L1", |a, _| a.albums.clear());
    w.source.update("L3", |a, _| a.albums.push("A1".into()));
    w.source.rename_album("A1", "Japan 2024");
    let (p, _) = w.plan();
    assert_eq!(p.departed.len(), 1);
    assert_eq!(p.joined.len(), 1);
    assert_eq!(p.renamed.len(), 1);
    assert_eq!(
        p.albums[0].choice.name, "Trips › Japan 2024",
        "the new name, not renamed here"
    );
    let (_, report) = w.sync(&lib);
    assert_eq!((report.renamed, report.copied), (1, 0), "{report:?}");
    let evs = events::load(std::slice::from_ref(&lib));
    assert!(evs.iter().any(|e| e.name == "Japan 2024"));
    assert!(!evs.iter().any(|e| e.name == "Japan"));
    assert!(
        !keywords(&mut w, &p1)
            .iter()
            .any(|k| k.starts_with("Events|")),
        "left"
    );
    assert!(
        keywords(&mut w, &p3).contains(&"Events|Trips|Japan 2024".to_owned()),
        "joined"
    );
    assert!(
        keywords(&mut w, &mine).contains(&"Events|Trips|Japan 2024".to_owned()),
        "a photo of the library's own stays, renamed with its event"
    );
    let (again, _) = w.plan();
    assert!(again.departed.is_empty() && again.joined.is_empty() && again.renamed.is_empty());

    // Renamed in Shoebox, then in Photos: Shoebox's name stays.
    let mut evs = events::load(std::slice::from_ref(&lib));
    let old = evs.clone();
    let id = evs
        .iter()
        .find(|e| e.name == "Japan 2024")
        .unwrap()
        .id
        .clone();
    evs.iter_mut().find(|e| e.id == id).unwrap().name = "Kyoto".into();
    let catalog = w.catalog();
    let mut writes = events::plan_save(std::slice::from_ref(&lib), &evs).unwrap();
    for (pi, m) in events::mark_updates(&catalog, &old, &evs, None, false) {
        let ph = &catalog.photos[pi];
        writes.extend(shoebox_core::sidecar::plan_write(&ph.path, &ph.marks, &m, false).unwrap());
    }
    w.fo.write_sidecars("rename event", writes).unwrap();
    // ...and a photo added to the album in Photos joins it, under
    // Shoebox's name, not a new event of the album's.
    w.source.rename_album("A1", "Japan trip");
    w.source.update("L2", |a, _| a.albums.push("A1".into()));
    w.sync(&lib);
    let evs = events::load(std::slice::from_ref(&lib));
    assert!(evs.iter().any(|e| e.name == "Kyoto"));
    assert!(!evs.iter().any(|e| e.name == "Japan trip"));
    let p2 = lib.join("2024/2024-07-01/20240701-140000.jpg");
    assert!(keywords(&mut w, &p2).contains(&"Events|Trips|Kyoto".to_owned()));

    // Deleted in Photos: the event stays.
    w.source.remove_album("A1");
    let (p, _) = w.plan();
    assert_eq!(p.gone.len(), 1);
    let (_, report) = w.sync(&lib);
    assert!(
        events::load(std::slice::from_ref(&lib))
            .iter()
            .any(|e| e.name == "Kyoto")
    );
    assert!(
        keywords(&mut w, &p3).contains(&"Events|Trips|Kyoto".to_owned()),
        "{report:?}"
    );
    let (p, _) = w.plan();
    assert!(p.gone.is_empty(), "forgotten once seen");
}

/// A panic while downloading one asset (PhotoKit's binding once panicked
/// on a nil file name) fails that asset only; the rest are copied and the
/// batch is closed.
#[test]
fn a_panic_fails_one_asset_not_the_sync() {
    let mut w = World::new();
    library(&w);
    w.source.panic_on("L2");
    let lib = w.lib.clone();
    let (_, report) = w.sync(&lib);
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(report.failed[0].0.local, "L2");
    assert_eq!(report.copied, 3);
    assert!(w.fo.journal().incomplete().unwrap().is_empty());
}

/// Downloads a stopped sync left in the cache don't get in the way of the
/// next one (the first real import crashed mid-step; the next found "File
/// exists" for the same names).
#[test]
fn leftovers_in_the_cache_dont_block_the_next_sync() {
    let mut w = World::new();
    library(&w);
    let staging = w.base.join("cache/photos-staging");
    std::fs::create_dir_all(staging.join("sync-old")).unwrap();
    std::fs::create_dir_all(staging.join("push")).unwrap();
    for n in 0..8 {
        std::fs::write(staging.join(format!("{n}-Original.JPG")), b"half").unwrap();
    }
    std::fs::write(staging.join("push/keep.jpg"), b"a send's render").unwrap();
    let lib = w.lib.clone();
    let (_, report) = w.sync(&lib);
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(report.copied, 4);
    assert!(
        staging.join("push/keep.jpg").exists(),
        "Send to Photos' folder left alone"
    );
    let left: Vec<_> = std::fs::read_dir(&staging)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(left, ["push"], "nothing else left behind");
}

/// A sync the app stopped between steps leaves its batch "running" with
/// every step done: closed at the next start, nothing to ask. One with a
/// step still to run is left for the person to resume or roll back.
#[test]
fn interrupted_batches_with_nothing_left_are_closed() {
    let w = World::new();
    let finished = w.fo.begin_import("copy from iCloud Photos").unwrap();
    let left = w.fo.begin_import("another").unwrap();
    let src = w.base.join("x.txt");
    std::fs::write(&src, b"x").unwrap();
    w.fo.journal()
        .record(
            left,
            0,
            shoebox_core::fileops::Op::Mkdir,
            &src,
            &w.lib.join("later"),
            None,
        )
        .unwrap();
    assert_eq!(w.fo.journal().incomplete().unwrap().len(), 2);
    assert_eq!(w.fo.close_finished_interrupted().unwrap(), vec![finished]);
    let still: Vec<i64> =
        w.fo.journal()
            .incomplete()
            .unwrap()
            .iter()
            .map(|b| b.id)
            .collect();
    assert_eq!(still, vec![left]);
}
