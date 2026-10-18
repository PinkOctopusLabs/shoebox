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
    let batch = fo.import("import card", &plan.copies, Vec::new()).unwrap();
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

/// Folder names become events (nested as the folders are) or keywords on
/// the copies' new sidecars; dates and generic names start as nothing; a
/// picture in two folders joins both; one already in the library gets the
/// name too. Undo takes it all back.
#[test]
fn folder_names_become_events_and_keywords() {
    use shoebox_core::import::FolderUse;
    use shoebox_core::marks::Marks;
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let (src, lib) = (base.join("old"), base.join("lib"));
    let shot = |sec: u8| Shot {
        datetime: Box::leak(format!("2019:07:14 10:00:{sec:02}").into_boxed_str()),
        subsec: "",
        orientation: 1,
        serial: "1",
    };
    let photo = |n: u64| with_exif(&jpeg(&scene(n, 320, 240), 85).unwrap(), &shot(n as u8));
    let a = photo(1);
    put(&src.join("Photo Library/Italy 2019/Rome/a.jpg"), &a);
    put(
        &src.join("Photo Library/Italy 2019/b.jpg.xmp"),
        &shoebox_core::xmp::edit(
            None,
            &[(
                shoebox_core::xmp::Prop::Rating,
                Some(shoebox_core::xmp::Value::Text("3".into())),
            )],
        )
        .unwrap(),
    );
    put(&src.join("Photo Library/Italy 2019/b.jpg"), &photo(2));
    put(&src.join("Photo Library/2019/07/c.jpg"), &photo(3));
    put(&src.join("Saved Pictures/d.jpg"), &photo(4));
    put(&src.join("Best of/a copy.jpg"), &a);
    let known = photo(5);
    put(&src.join("Best of/known.jpg"), &known);
    put(&lib.join("earlier/known.jpg"), &known);

    let roots = || vec![Root::new(&lib, Role::Scan).unwrap()];
    let mut cache = Cache::open_in_memory().unwrap();
    let (catalog, _) = Catalog::refresh(&mut cache, roots(), &|_, _| {}).unwrap();
    let before = tree(&lib);
    let source_before = tree(&src);
    let tpl = Template::parse(DEFAULT_IMPORT).unwrap();
    let mut plan = shoebox_core::import::plan(&src, &lib, &tpl, &catalog, &|_, _| {}).unwrap();

    let kinds: Vec<(String, FolderUse, usize)> = plan
        .folders
        .iter()
        .map(|f| (f.rel.display().to_string(), f.kind, f.photos))
        .collect();
    assert_eq!(
        kinds,
        [
            ("Best of".to_owned(), FolderUse::Event, 2),
            ("Photo Library".to_owned(), FolderUse::Ignore, 3),
            ("Photo Library/2019".to_owned(), FolderUse::Ignore, 1),
            ("Photo Library/2019/07".to_owned(), FolderUse::Ignore, 1),
            ("Photo Library/Italy 2019".to_owned(), FolderUse::Event, 2),
            (
                "Photo Library/Italy 2019/Rome".to_owned(),
                FolderUse::Event,
                1
            ),
            ("Saved Pictures".to_owned(), FolderUse::Ignore, 1),
        ]
    );
    let mut folders = plan.folders.clone();
    let saved = folders
        .iter_mut()
        .find(|f| f.name == "Saved Pictures")
        .unwrap();
    saved.kind = FolderUse::Keyword;
    saved.name = "Saved".into();

    let mut events = shoebox_core::events::load(std::slice::from_ref(&lib));
    let w = shoebox_core::import::folder_writes(&mut plan, &folders, &mut events, &catalog, false)
        .unwrap();
    assert_eq!(w.new_events, 3, "{events:?}");
    assert_eq!(w.photos, 4, "a, b, d and the library's known.jpg");
    let mut writes = w.writes;
    writes.extend(shoebox_core::events::plan_save(std::slice::from_ref(&lib), &events).unwrap());
    let fo = FileOps::new(Journal::open(&base.join("journal.sqlite")).unwrap(), vec![])
        .with_trash(TrashMode::Folder(base.join("trash")));
    let batch = fo.import("import old", &plan.copies, writes).unwrap();
    assert_eq!(tree(&src), source_before, "the source is untouched");
    for c in &plan.copies {
        assert_eq!(
            std::fs::read(&c.dst).unwrap(),
            std::fs::read(&c.src).unwrap()
        );
    }

    let (catalog, _) = Catalog::refresh(&mut cache, roots(), &|_, _| {}).unwrap();
    let marks = |src_rel: &str| -> Marks {
        let dst = plan
            .copies
            .iter()
            .find(|c| {
                c.src == src.join(src_rel)
                    || std::fs::read(&c.src).ok() == std::fs::read(src.join(src_rel)).ok()
            })
            .map(|c| c.dst.clone())
            .unwrap_or_else(|| lib.join(src_rel));
        catalog
            .photos
            .iter()
            .find(|p| p.path == dst)
            .unwrap_or_else(|| {
                panic!(
                    "{} not in {:?}",
                    dst.display(),
                    catalog.photos.iter().map(|p| &p.path).collect::<Vec<_>>()
                )
            })
            .marks
            .clone()
    };
    let a = marks("Photo Library/Italy 2019/Rome/a.jpg");
    assert_eq!(
        a.keywords,
        ["Events|Best of", "Events|Italy 2019|Rome"],
        "both folders it was in"
    );
    assert_eq!(a.event.as_deref(), Some("Rome"));
    let b = marks("Photo Library/Italy 2019/b.jpg");
    assert_eq!(b.keywords, ["Events|Italy 2019"]);
    assert_eq!(b.rating, 3, "the source's sidecar comes along");
    assert!(marks("Photo Library/2019/07/c.jpg").keywords.is_empty());
    assert_eq!(marks("Saved Pictures/d.jpg").keywords, ["Saved"]);
    assert_eq!(marks("earlier/known.jpg").keywords, ["Events|Best of"]);
    let loaded = shoebox_core::events::load(std::slice::from_ref(&lib));
    let rome = loaded.iter().find(|e| e.name == "Rome").unwrap();
    let italy = loaded.iter().find(|e| e.name == "Italy 2019").unwrap();
    assert_eq!(rome.parent.as_ref(), Some(&italy.id));

    // Undo: copies and the sidecars made for them go to the trash; the
    // events file is emptied (never deleted).
    fo.undo(batch).unwrap();
    let mut after = tree(&lib);
    assert_eq!(
        after.remove(".shoebox/events.json").as_deref(),
        Some(&b"{}"[..])
    );
    assert_eq!(after, before);
}
