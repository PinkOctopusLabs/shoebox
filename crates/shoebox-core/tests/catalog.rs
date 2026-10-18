//! Catalog refresh against the synthetic library.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_core::cache::Cache;
use shoebox_core::catalog::Catalog;
use shoebox_core::scan::{Role, Root};

fn roots(lib: &std::path::Path) -> Vec<Root> {
    vec![
        Root::new(&lib.join("ref"), Role::Reference).unwrap(),
        Root::new(&lib.join("scan"), Role::Scan).unwrap(),
    ]
}

#[test]
fn refresh_reads_once_then_uses_cache() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let mut cache = Cache::open(&t.path().join("cache.sqlite")).unwrap();

    assert!(
        Catalog::from_cache(&cache, roots(&lib))
            .unwrap()
            .photos
            .is_empty()
    );
    let (c, s) = Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();
    assert_eq!(
        (s.files, s.read, s.cached, s.failed),
        (c.photos.len(), c.photos.len(), 0, 0)
    );

    // Instant reopen from cache gives the same list.
    let again = Catalog::from_cache(&cache, roots(&lib)).unwrap();
    let names = |c: &Catalog| c.photos.iter().map(|p| p.rel.clone()).collect::<Vec<_>>();
    assert_eq!(names(&again), names(&c));

    // NEF on its side: upright dimensions and orientation.
    let nef = c
        .photos
        .iter()
        .find(|p| p.rel.ends_with("DSC_0002.NEF"))
        .unwrap();
    assert_eq!((nef.meta.width, nef.meta.height), (Some(1200), Some(1800)));
    assert_eq!(nef.meta.orientation(), 6);
    // Sorted by capture time: dated frames before undated files.
    assert!(c.photos[0].meta.capture_seconds().is_some());
    assert!(c.photos.last().unwrap().meta.capture_seconds().is_none());

    // Edit one file, delete another.
    let edited = lib.join("scan/recovered/f0003.jpg");
    let mut b = std::fs::read(&edited).unwrap();
    b.extend(b"x");
    std::fs::write(&edited, b).unwrap();
    std::fs::remove_file(lib.join("scan/recovered/f0006.jpg")).unwrap(); // test fixture, not library
    let (c2, s2) = Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();
    assert_eq!((s2.read, s2.forgotten), (1, 1));
    assert_eq!(c2.photos.len(), c.photos.len() - 1);
}

/// Reading only the first 256 KB gives the same metadata as the whole file.
#[test]
fn prefix_metadata_matches_full_read_on_real_files() {
    let dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/real");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries {
        let p = e.unwrap().path();
        let Some(fmt) = shoebox_decode::Format::from_path(&p) else {
            continue;
        };
        let fast = shoebox_core::meta::read(&p, fmt).unwrap();
        let data = std::fs::read(&p).unwrap();
        let full_exif = shoebox_core::exif::read(&data);
        assert_eq!(fast.exif, full_exif, "{}", p.display());
        assert!(fast.width.is_some(), "{}: no dimensions", p.display());
        eprintln!("{}: {:?}x{:?}", p.display(), fast.width, fast.height);
    }
}

#[test]
fn marks_round_trip_through_sidecars_and_rescans() {
    use shoebox_core::fileops::{FileOps, Journal};
    use shoebox_core::marks::{Label, Marks, Pick};
    use shoebox_core::xmp::{self, Prop, Value};

    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let mut cache = Cache::open(&t.path().join("cache.sqlite")).unwrap();
    let ref_root = lib.join("ref").canonicalize().unwrap();
    let fo = FileOps::new(
        Journal::open(&t.path().join("journal.sqlite")).unwrap(),
        vec![ref_root],
    );
    let (mut c, _) = Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();

    let i = c
        .photos
        .iter()
        .position(|p| p.rel == "recovered/f0003.jpg")
        .unwrap();
    assert_eq!(c.photos[i].marks, Marks::default());
    let new = Marks {
        rating: 4,
        pick: Some(Pick::Pick),
        label: Some(Label::Blue),
        keywords: vec!["Places|Italy|Rome".into()],
        event: None,
        gps_e7: None,
    };
    let batch = c
        .set_marks(&[(i, new.clone())], vec![], &fo, &cache, false, "test")
        .unwrap()
        .unwrap();
    let side = lib.join("scan/recovered/f0003.jpg.xmp");
    assert!(side.exists());
    assert_eq!(c.photos[i].marks, new);

    // Unchanged rescan: marks come from the cache, no re-read.
    let (c2, s2) = Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();
    assert_eq!(s2.read, 0);
    let j = c2
        .photos
        .iter()
        .position(|p| p.rel == "recovered/f0003.jpg")
        .unwrap();
    assert_eq!(c2.photos[j].marks, new);

    // Another app (say digiKam) changes the rating; a rescan picks it up.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let edited = xmp::edit(
        Some(&std::fs::read(&side).unwrap()),
        &[(Prop::Rating, Some(Value::Text("1".into())))],
    )
    .unwrap();
    std::fs::write(&side, edited).unwrap();
    let (c3, s3) = Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();
    assert_eq!(
        s3.read, 1,
        "only the photo whose sidecar changed is re-read"
    );
    let k = c3
        .photos
        .iter()
        .position(|p| p.rel == "recovered/f0003.jpg")
        .unwrap();
    assert_eq!(c3.photos[k].marks.rating, 1);
    assert_eq!(c3.photos[k].marks.label, Some(Label::Blue));

    // Our batch can't be undone over the other app's change.
    assert!(fo.undo(batch).is_err());

    // Reference roots are read-only: their photos are skipped, the others
    // still written.
    let r = c
        .photos
        .iter()
        .position(|p| p.rel.ends_with("DSC_0001.JPG"))
        .unwrap();
    let w = c
        .photos
        .iter()
        .position(|p| p.rel == "recovered/f0004.jpg")
        .unwrap();
    assert!(c.is_reference(r) && !c.is_reference(w));
    let rated = Marks {
        rating: 5,
        ..Marks::default()
    };
    assert_eq!(
        c.set_marks(&[(r, rated.clone())], vec![], &fo, &cache, false, "test")
            .unwrap(),
        None
    );
    assert!(!lib.join("ref/2026/2026-04-24/DSC_0001.JPG.xmp").exists());
    assert!(
        c.set_marks(
            &[(r, rated.clone()), (w, rated)],
            vec![],
            &fo,
            &cache,
            false,
            "test"
        )
        .unwrap()
        .is_some()
    );
    assert!(!lib.join("ref/2026/2026-04-24/DSC_0001.JPG.xmp").exists());
    assert_eq!(c.photos[w].marks.rating, 5, "the writable photo still is");
}

/// Changes made by other apps are noticed by a rescan or a targeted re-read,
/// attributed, and cleared by Mark as Seen; first sight is not a change.
#[test]
fn changes_are_tracked_until_seen() {
    use shoebox_core::changes::{By, What};
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let lib = lib.canonicalize().unwrap(); // roots are canonical
    let mut cache = Cache::open(&t.path().join("cache.sqlite")).unwrap();
    let (c, _) = Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();
    assert!(c.photos.iter().all(|p| p.change.is_none()), "first scan");

    // Another app writes a sidecar: a rescan notices it.
    let jpg = lib.join("scan/recovered/f0003.jpg");
    let side = shoebox_core::sidecar::per_file(&jpg);
    let xmp = shoebox_core::xmp::edit(
        None,
        &[(
            shoebox_core::xmp::Prop::Rating,
            Some(shoebox_core::xmp::Value::Text("2".into())),
        )],
    )
    .unwrap();
    std::fs::write(&side, &xmp).unwrap(); // test fixture, not library
    let (mut c, s) = Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();
    assert_eq!(s.changed, 1);
    let i = c.photos.iter().position(|p| p.path == jpg).unwrap();
    let ch = c.photos[i].change.clone().unwrap();
    assert_eq!((ch.what, ch.by), (What::SIDECAR, By::Other));
    assert_eq!(c.photos[i].marks.rating, 2);
    // Survives a reopen from the cache.
    let again = Catalog::from_cache(&cache, roots(&lib)).unwrap();
    assert!(
        again
            .photos
            .iter()
            .any(|p| p.path == jpg && p.change.is_some())
    );

    c.mark_seen(&[i], &mut cache).unwrap();
    assert!(c.photos[i].change.is_none());
    let again = Catalog::from_cache(&cache, roots(&lib)).unwrap();
    assert!(again.photos.iter().all(|p| p.change.is_none()));

    // The watcher's path: the sidecar maps to its photo, a re-read updates it.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let xmp = shoebox_core::xmp::edit(
        Some(&xmp),
        &[(
            shoebox_core::xmp::Prop::Rating,
            Some(shoebox_core::xmp::Value::Text("5".into())),
        )],
    )
    .unwrap();
    std::fs::write(&side, &xmp).unwrap();
    let a = c.affected(&[side.clone(), lib.join("scan/.DS_Store")]);
    assert_eq!(
        (a.photos.clone(), a.rescan, a.events),
        (vec![i], false, false)
    );
    let r = c.reread(&a.photos, &By::Other, &mut cache).unwrap();
    assert_eq!(r.changed, vec![jpg.clone()]);
    assert_eq!(c.photos[i].marks.rating, 5);
    assert!(c.photos[i].change.as_ref().unwrap().by_other());
    // Nothing more to see on a rescan: the cache is current.
    let (_, s) = Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();
    assert_eq!((s.changed, s.read), (0, 0));

    // A new image or folder asks for a rescan; events.json is flagged.
    let a = c.affected(&[
        lib.join("scan/new.jpg"),
        lib.join("scan/.shoebox/events.json"),
    ]);
    assert!(a.rescan && a.events && a.photos.is_empty());
}
