//! Edit files in `.shoebox/edits`: written through fileops (journaled, kind
//! `edit`), carried along when photos move, found again after a move in
//! Finder, and the catalog's working copy versus the file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use shoebox_core::cache::{Cache, EditKey};
use shoebox_core::edits;
use shoebox_core::fileops::{FileOps, Journal, OpStatus, plan_group_moves, plan_moves};

fn put(p: &Path, b: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

fn doc(copy: &str, blake3: &str, size: u64) -> String {
    serde_json::json!({
        "schema": 1,
        "process_version": 1,
        "source": { "path": "x", "blake3": blake3, "size": size },
        "copy": { "id": copy, "name": copy },
        "modules": { "exposure": { "ev": 0.7 } }
    })
    .to_string()
}

struct Lib {
    _t: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    ops: FileOps,
}

fn lib() -> Lib {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let root = base.join("Photos");
    put(&root.join("2026/2026-10/DSC_1234.NEF"), b"nef bytes");
    put(&root.join("2026/2026-10/DSC_1234.JPG"), b"jpg bytes");
    std::fs::create_dir_all(root.join(".shoebox")).unwrap();
    let ops = FileOps::new(Journal::open(&base.join("journal.sqlite")).unwrap(), vec![]);
    Lib {
        _t: t,
        base,
        root,
        ops,
    }
}

#[test]
fn paths_of_edits() {
    let root = Path::new("/lib");
    let rel = Path::new("2026/2026-10/DSC_1234.NEF");
    assert_eq!(
        edits::path(root, rel, edits::MAIN),
        Path::new("/lib/.shoebox/edits/2026/2026-10/DSC_1234.NEF.json")
    );
    assert_eq!(
        edits::path(root, rel, "v2"),
        Path::new("/lib/.shoebox/edits/2026/2026-10/DSC_1234.NEF.v2.json")
    );
}

#[test]
fn writes_are_journaled_make_folders_and_stay_out_of_library_undo() {
    let l = lib();
    let rel = Path::new("2026/2026-10/DSC_1234.NEF");
    let file = edits::path(&l.root, rel, edits::MAIN);
    let batch = l
        .ops
        .write_edits(
            "Save edit",
            vec![(file.clone(), doc("main", "ab", 9).into_bytes())],
        )
        .unwrap();
    assert_eq!(edits::read(&file).unwrap().unwrap(), doc("main", "ab", 9));
    let ops = l.ops.journal().ops(batch).unwrap();
    assert!(ops.iter().all(|o| o.status == OpStatus::Done));
    assert!(
        ops.iter()
            .any(|o| o.dst == l.root.join(".shoebox/edits/2026/2026-10"))
    );
    let b = l.ops.journal().batches().unwrap();
    assert_eq!(b.last().unwrap().kind, "edit");
    assert!(l.ops.last_undoable().unwrap().is_none());

    // Only `.shoebox/edits` takes edit writes, and image files never.
    assert!(
        l.ops
            .write_edits(
                "x",
                vec![(l.root.join("2026/2026-10/DSC_1234.NEF"), b"{}".to_vec())]
            )
            .is_err()
    );
    assert!(
        l.ops
            .write_edits(
                "x",
                vec![(l.root.join(".shoebox/stacks.json"), b"{}".to_vec())]
            )
            .is_err()
    );

    // Undoing the first save leaves `{}`, which reads as no edit.
    l.ops.undo(batch).unwrap();
    assert_eq!(edits::read(&file).unwrap(), None);
}

#[test]
fn reference_roots_take_no_edits() {
    let l = lib();
    let ops = FileOps::new(
        Journal::open(&l.base.join("j2.sqlite")).unwrap(),
        vec![l.root.clone()],
    );
    let file = edits::path(&l.root, Path::new("2026/2026-10/DSC_1234.NEF"), edits::MAIN);
    assert!(ops.write_edits("x", vec![(file, b"{}".to_vec())]).is_err());
}

#[test]
fn edits_follow_moves_and_renames() {
    let l = lib();
    let nef = l.root.join("2026/2026-10/DSC_1234.NEF");
    let main = edits::path(&l.root, Path::new("2026/2026-10/DSC_1234.NEF"), edits::MAIN);
    let copy = edits::path(&l.root, Path::new("2026/2026-10/DSC_1234.NEF"), "v2");
    let jpg_edit = edits::path(&l.root, Path::new("2026/2026-10/DSC_1234.JPG"), edits::MAIN);
    put(&main, doc("main", "ab", 9).as_bytes());
    put(&copy, doc("v2", "ab", 9).as_bytes());
    put(&jpg_edit, doc("main", "cd", 9).as_bytes());
    assert_eq!(edits::of_image(&nef).len(), 2);

    // Rename and move to another folder: both of the NEF's edits go along,
    // the JPG's stays.
    let dst = l.root.join("Best/Zf_0001.NEF");
    let plan = plan_moves(&[(nef.clone(), dst.clone())]).unwrap();
    assert_eq!(plan.moves.len(), 3, "{:?}", plan.moves);
    let batch = l.ops.execute("Move", &plan).unwrap();
    let new_main = l.root.join(".shoebox/edits/Best/Zf_0001.NEF.json");
    let new_copy = l.root.join(".shoebox/edits/Best/Zf_0001.NEF.v2.json");
    assert!(new_main.is_file() && new_copy.is_file() && !main.exists());
    assert!(jpg_edit.is_file());
    let found = edits::all(&l.root).unwrap();
    let rels: Vec<_> = found
        .iter()
        .map(|f| (f.rel.clone(), f.copy.clone()))
        .collect();
    assert!(rels.contains(&(PathBuf::from("Best/Zf_0001.NEF"), "v2".into())));
    assert!(rels.contains(&(PathBuf::from("Best/Zf_0001.NEF"), "main".into())));

    // One undo puts photo and edits back.
    l.ops.undo(batch).unwrap();
    assert!(nef.is_file() && main.is_file() && copy.is_file() && !new_main.exists());

    // A name already taken moves photo and edits to the same `-1` name.
    put(&l.root.join("Best/Zf_0001.NEF"), b"other");
    let plan = plan_group_moves(&[vec![(nef.clone(), dst.clone())]]).unwrap();
    l.ops.execute("Move", &plan).unwrap();
    assert!(l.root.join("Best/Zf_0001-1.NEF").is_file());
    assert!(
        l.root
            .join(".shoebox/edits/Best/Zf_0001-1.NEF.json")
            .is_file()
    );

    // Leaving every library root: the edit stays where it is.
    let out = l.base.join("Elsewhere/Zf.NEF");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let plan = plan_moves(&[(l.root.join("Best/Zf_0001-1.NEF"), out)]).unwrap();
    assert_eq!(plan.moves.len(), 1);
}

#[test]
fn edits_left_behind_by_finder_are_found_by_content() {
    let l = lib();
    let orphan = edits::path(&l.root, Path::new("Gone/DSC_9.NEF"), edits::MAIN);
    let content = b"the moved photo";
    let hash = blake3::hash(content).to_hex().to_string();
    put(&orphan, doc("main", &hash, content.len() as u64).as_bytes());
    let moved = l.root.join("Here/Renamed.NEF");
    put(&moved, content);
    let other = l.root.join("Here/Other.NEF");
    put(&other, b"something else entirely");

    let all = edits::all(&l.root).unwrap();
    let orphans: Vec<_> = all
        .into_iter()
        .filter(|f| !l.root.join(&f.rel).exists())
        .collect();
    assert_eq!(orphans.len(), 1);
    let mut hashed = Vec::new();
    let plan = edits::reattach(
        &orphans,
        |_| Some(l.root.clone()),
        &[(other.clone(), 24), (moved.clone(), content.len() as u64)],
        |p| {
            hashed.push(p.to_path_buf());
            Some(
                blake3::hash(&std::fs::read(p).unwrap())
                    .to_hex()
                    .to_string(),
            )
        },
    );
    assert_eq!(
        plan,
        vec![(orphan, l.root.join(".shoebox/edits/Here/Renamed.NEF.json"))]
    );
    // Only the photo of the right size was hashed.
    assert_eq!(hashed, vec![moved]);
}

#[test]
fn the_catalog_holds_the_working_copy_and_a_newer_file_wins() {
    let l = lib();
    let cache = Cache::open_in_memory().unwrap();
    let root = l.root.to_str().unwrap();
    let rel = "2026/2026-10/DSC_1234.NEF";
    let key = EditKey {
        root,
        rel,
        copy: edits::MAIN,
    };
    let h = |s: &str| format!("h{}", s.len());

    assert_eq!(
        edits::load(&cache, &l.root, rel, edits::MAIN, h).unwrap(),
        None
    );
    edits::put(
        &cache,
        &l.root,
        rel,
        edits::MAIN,
        &doc("main", "1", 1),
        "h1",
    )
    .unwrap();
    assert_eq!(cache.unsaved_edits().unwrap().len(), 1);
    assert_eq!(edits::save_pending(&cache, &l.ops).unwrap(), 1);
    assert!(cache.unsaved_edits().unwrap().is_empty());
    assert_eq!(edits::save_pending(&cache, &l.ops).unwrap(), 0);
    let file = edits::path(&l.root, Path::new(rel), edits::MAIN);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), doc("main", "1", 1));

    // The working copy changes; until saved, the catalog's is what opens.
    edits::put(
        &cache,
        &l.root,
        rel,
        edits::MAIN,
        &doc("main", "2", 1),
        "h2",
    )
    .unwrap();
    assert_eq!(
        edits::load(&cache, &l.root, rel, edits::MAIN, h).unwrap(),
        Some(doc("main", "2", 1))
    );
    edits::save_pending(&cache, &l.ops).unwrap();

    // The file changes later (another Mac): it wins and becomes the row.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&file, doc("main", "elsewhere", 1)).unwrap();
    assert_eq!(
        edits::load(&cache, &l.root, rel, edits::MAIN, h).unwrap(),
        Some(doc("main", "elsewhere", 1))
    );
    let row = cache.edit(&key).unwrap().unwrap();
    assert_eq!(row.json, doc("main", "elsewhere", 1));
    assert!(row.saved_ns.is_some());

    // A cache with no row (rebuilt, or a new Mac) reads the file.
    let fresh = Cache::open_in_memory().unwrap();
    assert_eq!(
        edits::load(&fresh, &l.root, rel, edits::MAIN, h).unwrap(),
        Some(doc("main", "elsewhere", 1))
    );
}

#[test]
fn the_catalog_rows_follow_moves() {
    let cache = Cache::open_in_memory().unwrap();
    let key = |rel| EditKey {
        root: "/lib",
        rel,
        copy: edits::MAIN,
    };
    cache
        .put_edit(&key("a/DSC.NEF"), "{\"x\":1}", "h", 5)
        .unwrap();
    cache
        .move_edits(("/lib", "a/DSC.NEF"), ("/lib", "b/Renamed.NEF"))
        .unwrap();
    assert!(cache.edit(&key("a/DSC.NEF")).unwrap().is_none());
    assert_eq!(
        cache.edit(&key("b/Renamed.NEF")).unwrap().unwrap().hash,
        "h"
    );
}
