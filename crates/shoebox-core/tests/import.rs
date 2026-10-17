//! M5 acceptance (core): importing a card produces exactly the names the
//! default template predicts, skips what the library already has, and the
//! batch can be undone.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;

use shoebox_core::cache::Cache;
use shoebox_core::catalog::Catalog;
use shoebox_core::fileops::{FileOps, Journal, TrashMode};
use shoebox_core::scan::{Role, Root};
use shoebox_core::template::{DEFAULT_IMPORT, Template};
use shoebox_fixtures::{Shot, jpeg, nef, resize_long, scene, with_exif};

fn put(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            (
                e.path().strip_prefix(dir).unwrap().display().to_string(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

/// A card: two NEF frames (one with a JPG and per-file sidecar, one with a
/// shared `name.xmp`), a JPG without sub-seconds, a carved preview without
/// EXIF, a file the library has, and a file repeated on the card.
fn card(dir: &Path, lib: &Path) {
    let a = scene(1, 1800, 1200);
    let b = scene(2, 1800, 1200);
    let c = scene(3, 1800, 1200);
    let s1 = Shot {
        datetime: "2026:04:24 14:03:11",
        subsec: "42",
        orientation: 1,
        serial: "6012345",
    };
    let s2 = Shot {
        datetime: "2026:04:24 14:05:02",
        subsec: "07",
        orientation: 1,
        serial: "6012345",
    };
    let s3 = Shot {
        datetime: "2026:04:25 09:00:00",
        subsec: "",
        orientation: 1,
        serial: "6012345",
    };
    let small = jpeg(&resize_long(&a, 320), 80).unwrap();
    let d = dir.join("DCIM/100NIKON");
    put(
        &d.join("DSC_0001.NEF"),
        &nef(&jpeg(&a, 92).unwrap(), (1800, 1200), &small, &s1),
    );
    put(
        &d.join("DSC_0001.JPG"),
        &with_exif(&jpeg(&a, 85).unwrap(), &s1),
    );
    put(
        &d.join("DSC_0001.NEF.xmp"),
        b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>",
    );
    put(
        &d.join("DSC_0002.NEF"),
        &nef(&jpeg(&b, 92).unwrap(), (1800, 1200), &small, &s2),
    );
    put(
        &d.join("DSC_0002.xmp"),
        b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>",
    );
    let no_subsec = with_exif(&jpeg(&c, 85).unwrap(), &s3);
    put(&d.join("DSC_0003.JPG"), &no_subsec);
    put(&d.join("DSC_0005.JPG"), &no_subsec); // the same file twice on the card
    put(&dir.join("MISC/f0001.jpg"), &jpeg(&c, 70).unwrap());
    let known = jpeg(&b, 60).unwrap();
    put(&d.join("DSC_0004.JPG"), &known);
    put(&lib.join("old/known.jpg"), &known); // already in the library
    // Something else already has DSC_0003's name: it gets -1.
    put(
        &lib.join("2026/2026-04-25/20260425-090000.jpg"),
        b"another picture",
    );
}

#[test]
fn import_names_skips_and_undo() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let (src, lib) = (base.join("card"), base.join("lib"));
    card(&src, &lib);
    let roots = || vec![Root::new(&lib, Role::Scan).unwrap()];
    let mut cache = Cache::open_in_memory().unwrap();
    let (catalog, _) = Catalog::refresh(&mut cache, roots(), &|_, _| {}).unwrap();
    let before = tree(&lib);

    let tpl = Template::parse(DEFAULT_IMPORT).unwrap();
    let plan = shoebox_core::import::plan(&src, &lib, &tpl, &catalog, &|_, _| {}).unwrap();
    let got: BTreeMap<String, String> = plan
        .copies
        .iter()
        .map(|c| {
            (
                c.src.strip_prefix(&src).unwrap().display().to_string(),
                c.dst.strip_prefix(&lib).unwrap().display().to_string(),
            )
        })
        .collect();
    let want: BTreeMap<String, String> = [
        (
            "DCIM/100NIKON/DSC_0001.NEF",
            "2026/2026-04-24/20260424-140311-42.nef",
        ),
        (
            "DCIM/100NIKON/DSC_0001.JPG",
            "2026/2026-04-24/20260424-140311-42.jpg",
        ),
        (
            "DCIM/100NIKON/DSC_0001.NEF.xmp",
            "2026/2026-04-24/20260424-140311-42.nef.xmp",
        ),
        (
            "DCIM/100NIKON/DSC_0002.NEF",
            "2026/2026-04-24/20260424-140502-07.nef",
        ),
        (
            "DCIM/100NIKON/DSC_0002.xmp",
            "2026/2026-04-24/20260424-140502-07.xmp",
        ),
        (
            "DCIM/100NIKON/DSC_0003.JPG",
            "2026/2026-04-25/20260425-090000-1.jpg",
        ),
        ("MISC/f0001.jpg", "_undated/1800x1200/f0001.jpg"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_owned(), b.to_owned()))
    .collect();
    assert_eq!(got, want);
    let skipped: Vec<(String, String)> = plan
        .skipped
        .iter()
        .map(|(s, e)| {
            (
                s.strip_prefix(&src).unwrap().display().to_string(),
                e.file_name().unwrap().to_string_lossy().into_owned(),
            )
        })
        .collect();
    assert_eq!(
        skipped,
        vec![
            (
                "DCIM/100NIKON/DSC_0004.JPG".to_owned(),
                "known.jpg".to_owned()
            ),
            (
                "DCIM/100NIKON/DSC_0005.JPG".to_owned(),
                "DSC_0003.JPG".to_owned()
            ),
        ]
    );
    assert_eq!(plan.undated, 1);

    // Copy, verified; the card is untouched.
    let card_before = tree(&src);
    let fo = FileOps::new(Journal::open(&base.join("journal.sqlite")).unwrap(), vec![])
        .with_trash(TrashMode::Folder(base.join("trash")));
    let batch = fo.import("import card", &plan.copies).unwrap();
    assert_eq!(tree(&src), card_before);
    for c in &plan.copies {
        assert_eq!(
            std::fs::read(&c.dst).unwrap(),
            std::fs::read(&c.src).unwrap()
        );
    }

    // Importing again finds everything already there.
    let (catalog, _) = Catalog::refresh(&mut cache, roots(), &|_, _| {}).unwrap();
    let again = shoebox_core::import::plan(&src, &lib, &tpl, &catalog, &|_, _| {}).unwrap();
    assert!(again.copies.is_empty(), "{:?}", again.copies);
    assert_eq!(again.skipped.len(), 7, "every image on the card");

    // Undo puts the library back (copies go to the trash, never deleted).
    fo.undo(batch).unwrap();
    assert_eq!(tree(&lib), before);
    assert_eq!(tree(&base.join("trash")).len(), plan.copies.len());
}

/// A minimal QuickTime movie: just a `moov` with an `mvhd` creation time.
fn movie(created_unix: i64) -> Vec<u8> {
    let bx = |kind: &[u8], body: &[u8]| {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    };
    let mut mvhd = vec![0u8; 100];
    let qt = (created_unix + 2_082_844_800) as u32;
    mvhd[4..8].copy_from_slice(&qt.to_be_bytes());
    [bx(b"ftyp", b"qt  "), bx(b"moov", &bx(b"mvhd", &mvhd))].concat()
}

/// A Live Photo's video keeps its photo's name, even when the video's own
/// clock says a different second.
#[test]
fn live_photo_pair_keeps_one_name() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let (src, lib) = (base.join("card"), base.join("lib"));
    std::fs::create_dir_all(&lib).unwrap();
    let shot = Shot {
        datetime: "2026:04:24 14:03:11",
        subsec: "",
        orientation: 1,
        serial: "1",
    };
    put(
        &src.join("IMG_0007.JPG"),
        &with_exif(&jpeg(&scene(9, 400, 300), 85).unwrap(), &shot),
    );
    // 3 s later by its own clock (UTC; the zone doesn't matter here).
    put(&src.join("IMG_0007.MOV"), &movie(1_777_039_394));
    let roots = vec![Root::new(&lib, Role::Scan).unwrap()];
    let mut cache = Cache::open_in_memory().unwrap();
    let (catalog, _) = Catalog::refresh(&mut cache, roots, &|_, _| {}).unwrap();
    let tpl = Template::parse(DEFAULT_IMPORT).unwrap();
    let plan = shoebox_core::import::plan(&src, &lib, &tpl, &catalog, &|_, _| {}).unwrap();
    let stems: std::collections::HashSet<String> = plan
        .copies
        .iter()
        .map(|c| c.dst.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(plan.copies.len(), 2);
    assert_eq!(stems.len(), 1, "one name for the pair: {stems:?}");
}
