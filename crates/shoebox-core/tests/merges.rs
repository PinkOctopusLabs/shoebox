//! Photo merges in the library (rule 3): the planned file beside its first
//! source, one journaled batch that adds it with its record, never
//! replacing anything, sources only read, refused in reference folders,
//! undone to the Trash; the library sees a photo of its own.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use shoebox_core::cache::Cache;
use shoebox_core::catalog::Catalog;
use shoebox_core::fileops::{FileOps, Journal, OpStatus, PlannedCopy, TrashMode};
use shoebox_core::merges::{self, Kind};
use shoebox_core::scan::{Role, Root};
use shoebox_fixtures::Bracket;

fn b3(p: &Path) -> [u8; 32] {
    *blake3::hash(&std::fs::read(p).unwrap()).as_bytes()
}

/// A library with a −2/0/+2 bracket, and a reference folder with one.
struct Lib {
    _t: tempfile::TempDir,
    lib: PathBuf,
    reference: PathBuf,
    cache: PathBuf,
    sources: Vec<PathBuf>,
    roots: Vec<Root>,
    fileops: FileOps,
    trash: PathBuf,
}

fn lib() -> Lib {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib");
    let reference = base.join("ref");
    let day = lib.join("2026-10-22");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::create_dir_all(&reference).unwrap();
    let sources: Vec<PathBuf> = [-2.0, 0.0, 2.0]
        .iter()
        .enumerate()
        .map(|(i, &ev)| {
            let p = day.join(format!("DSC_00{}.dng", 41 + i));
            let b = Bracket {
                seed: i as u64 + 1,
                subsec: ["10", "30", "50"][i],
                ..Bracket::new(96, 64, ev)
            };
            std::fs::write(&p, b.write()).unwrap();
            p
        })
        .collect();
    std::fs::write(
        reference.join("REF_0001.dng"),
        Bracket::new(96, 64, 0.0).write(),
    )
    .unwrap();
    let roots = vec![
        Root::new(&lib, Role::Scan).unwrap(),
        Root::new(&reference, Role::Reference).unwrap(),
    ];
    let trash = base.join("trash");
    let fileops = FileOps::new(
        Journal::open(&base.join("journal.sqlite")).unwrap(),
        vec![reference.clone()],
    )
    .with_trash(TrashMode::Folder(trash.clone()));
    let cache = base.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    Lib {
        _t: t,
        lib,
        reference,
        cache,
        sources,
        roots,
        fileops,
        trash,
    }
}

impl Lib {
    /// A stand-in for the merge's DNG, written in the cache (core never
    /// decodes it), with the reference frame's EXIF (sub-seconds and all:
    /// the library mustn't stack it with its sources anyway).
    fn result(&self) -> PlannedCopy {
        let src = self.cache.join("merge.dng");
        let b = Bracket {
            seed: 99,
            subsec: "30",
            ..Bracket::new(96, 64, 0.0)
        };
        std::fs::write(&src, b.write()).unwrap();
        PlannedCopy {
            hash: b3(&src),
            dst: PathBuf::new(),
            src,
        }
    }

    fn add(&self, plan: &merges::Plan) -> shoebox_core::Result<i64> {
        let copy = PlannedCopy {
            dst: plan.dst.clone(),
            ..self.result()
        };
        let writes = merges::plan_save(std::slice::from_ref(&self.lib), &[plan.record()]).unwrap();
        self.fileops
            .add_merge("HDR merge", &copy, &plan.sources, writes)
    }
}

#[test]
fn a_merge_is_added_beside_its_first_source_and_undone() {
    let l = lib();
    let before: Vec<[u8; 32]> = l.sources.iter().map(|p| b3(p)).collect();
    let plan = merges::plan(&l.roots, Kind::Hdr, &l.sources).unwrap();
    let dir = l.sources[0].parent().unwrap();
    assert_eq!(plan.dst, dir.join("DSC_0041-HDR.dng"));
    assert_eq!(plan.sources.len(), 3);
    let batch = l.add(&plan).unwrap();

    // Journaled as a `merge` batch: the copy, then the record.
    let j = l.fileops.journal();
    let b = j.batch(batch).unwrap().unwrap();
    assert_eq!((b.kind.as_str(), b.status.as_str()), ("merge", "done"));
    let ops = j.ops(batch).unwrap();
    assert_eq!(ops[0].dst, plan.dst);
    assert!(ops.iter().all(|o| o.status == OpStatus::Done));
    assert!(plan.dst.is_file());
    assert_eq!(l.fileops.last_undoable().unwrap().unwrap().id, batch);
    // The sources are exactly as they were.
    let after: Vec<[u8; 32]> = l.sources.iter().map(|p| b3(p)).collect();
    assert_eq!(before, after);

    // The library: a photo of its own, its record read.
    let mut cache = Cache::open(&l.cache.join("cache.sqlite")).unwrap();
    let (c, _) = Catalog::refresh(&mut cache, l.roots.clone(), &|_, _| {}).unwrap();
    let at = |p: &Path| c.photos.iter().position(|x| x.path == p).unwrap();
    let m = at(&plan.dst);
    for s in &l.sources {
        assert_ne!(
            c.stack_of[m],
            c.stack_of[at(s)],
            "stacked with {}",
            s.display()
        );
    }
    let rec = merges::of_file(&c.merges, &plan.dst).unwrap();
    assert_eq!(rec.kind, Kind::Hdr);
    let names: Vec<_> = rec.sources.iter().map(|s| s.path.clone()).collect();
    assert_eq!(names, l.sources);
    assert_eq!(merges::made_from(&c.merges, &l.sources[1]).count(), 1);

    // A second merge of the same photos never replaces the first.
    let again = merges::plan(&l.roots, Kind::Hdr, &l.sources).unwrap();
    assert_eq!(again.dst, dir.join("DSC_0041-HDR-1.dng"));
    // And a file put where it was planned is never overwritten.
    std::fs::write(&again.dst, b"someone else's").unwrap();
    assert!(l.add(&again).is_err());
    assert_eq!(std::fs::read(&again.dst).unwrap(), b"someone else's");
    std::fs::remove_file(&again.dst).unwrap();

    // Undo: the DNG to the Trash, the record emptied, sources untouched.
    l.fileops.undo(batch).unwrap();
    assert!(!plan.dst.exists());
    assert!(l.trash.join("DSC_0041-HDR.dng").is_file());
    assert!(merges::load(std::slice::from_ref(&l.lib)).is_empty());
    let after: Vec<[u8; 32]> = l.sources.iter().map(|p| b3(p)).collect();
    assert_eq!(before, after);
}

#[test]
fn a_focus_stack_is_added_as_its_own_kind_and_undone() {
    let l = lib();
    let before: Vec<[u8; 32]> = l.sources.iter().map(|p| b3(p)).collect();
    let plan = merges::plan(&l.roots, Kind::Focus, &l.sources).unwrap();
    let dir = l.sources[0].parent().unwrap();
    assert_eq!(plan.dst, dir.join("DSC_0041-Focus.dng"));
    // Beside an HDR merge of the same first photo, under its own name.
    let hdr = merges::plan(&l.roots, Kind::Hdr, &l.sources).unwrap();
    l.add(&hdr).unwrap();
    let batch = l.add(&plan).unwrap();
    let j = l.fileops.journal();
    let b = j.batch(batch).unwrap().unwrap();
    assert_eq!((b.kind.as_str(), b.status.as_str()), ("merge", "done"));
    assert!(plan.dst.is_file() && hdr.dst.is_file());
    assert_eq!(before, l.sources.iter().map(|p| b3(p)).collect::<Vec<_>>());
    // Its record says what it is (this test's `add` writes one record).
    let rec = merges::load(std::slice::from_ref(&l.lib));
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].kind, Kind::Focus);
    assert_eq!(rec[0].file, plan.dst);
    // Never over a file there, and never from a reference folder.
    let again = merges::plan(&l.roots, Kind::Focus, &l.sources).unwrap();
    assert_eq!(again.dst, dir.join("DSC_0041-Focus-1.dng"));
    let mut with_ref = l.sources.clone();
    with_ref.push(l.reference.join("REF_0001.dng"));
    assert!(merges::plan(&l.roots, Kind::Focus, &with_ref).is_err());
    // Undo: to the Trash, the sources untouched.
    l.fileops.undo(batch).unwrap();
    assert!(!plan.dst.exists());
    assert!(l.trash.join("DSC_0041-Focus.dng").is_file());
    assert_eq!(before, l.sources.iter().map(|p| b3(p)).collect::<Vec<_>>());
}

#[test]
fn a_panorama_is_added_as_pano_beside_its_first_source_and_undone() {
    let l = lib();
    let before: Vec<[u8; 32]> = l.sources.iter().map(|p| b3(p)).collect();
    let plan = merges::plan(&l.roots, Kind::Panorama, &l.sources).unwrap();
    let dir = l.sources[0].parent().unwrap();
    assert_eq!(plan.dst, dir.join("DSC_0041-Pano.dng"));
    let copy = PlannedCopy {
        dst: plan.dst.clone(),
        ..l.result()
    };
    let writes = merges::plan_save(std::slice::from_ref(&l.lib), &[plan.record()]).unwrap();
    let batch = l
        .fileops
        .add_merge("panorama", &copy, &plan.sources, writes)
        .unwrap();
    let b = l.fileops.journal().batch(batch).unwrap().unwrap();
    assert_eq!((b.kind.as_str(), b.status.as_str()), ("merge", "done"));
    assert!(plan.dst.is_file());
    let after: Vec<[u8; 32]> = l.sources.iter().map(|p| b3(p)).collect();
    assert_eq!(before, after);
    // A photo of its own, recorded as a panorama of its sources.
    let mut cache = Cache::open(&l.cache.join("cache.sqlite")).unwrap();
    let (c, _) = Catalog::refresh(&mut cache, l.roots.clone(), &|_, _| {}).unwrap();
    let at = |p: &Path| c.photos.iter().position(|x| x.path == p).unwrap();
    let m = at(&plan.dst);
    assert!(l.sources.iter().all(|s| c.stack_of[m] != c.stack_of[at(s)]));
    let rec = merges::of_file(&c.merges, &plan.dst).unwrap();
    assert_eq!(rec.kind, Kind::Panorama);
    assert_eq!(rec.sources.len(), 3);
    // A second panorama of the same photos never replaces the first.
    let again = merges::plan(&l.roots, Kind::Panorama, &l.sources).unwrap();
    assert_eq!(again.dst, dir.join("DSC_0041-Pano-1.dng"));
    // Undo: to the Trash.
    l.fileops.undo(batch).unwrap();
    assert!(!plan.dst.exists());
    assert!(l.trash.join("DSC_0041-Pano.dng").is_file());
    assert!(merges::load(std::slice::from_ref(&l.lib)).is_empty());
}

#[test]
fn merges_read_their_sources_only_and_never_in_reference_folders() {
    let l = lib();
    // A source in a reference folder.
    let mut with_ref = l.sources.clone();
    with_ref.push(l.reference.join("REF_0001.dng"));
    assert!(matches!(
        merges::plan(&l.roots, Kind::Hdr, &with_ref),
        Err(shoebox_core::CoreError::Refused(_))
    ));
    // Outside the library.
    let outside = l.cache.join("x.dng");
    std::fs::write(&outside, b"x").unwrap();
    assert!(merges::plan(&l.roots, Kind::Hdr, &[outside]).is_err());
    // A result planned into a reference folder is refused by fileops too.
    let mut plan = merges::plan(&l.roots, Kind::Hdr, &l.sources).unwrap();
    plan.dst = l.reference.join("REF_0001-HDR.dng");
    assert!(l.add(&plan).is_err());
    assert!(!plan.dst.exists());
    // A source that changed after the merge read it: refused, nothing
    // written.
    let plan = merges::plan(&l.roots, Kind::Hdr, &l.sources).unwrap();
    std::fs::write(&l.sources[2], Bracket::new(96, 64, 1.0).write()).unwrap();
    assert!(l.add(&plan).is_err());
    assert!(!plan.dst.exists());
}

#[test]
fn a_copy_that_fails_verification_is_journaled_and_leaves_nothing() {
    let l = lib();
    let plan = merges::plan(&l.roots, Kind::Hdr, &l.sources).unwrap();
    let mut copy = l.result();
    copy.dst = plan.dst.clone();
    // The hash recorded before writing isn't the file's: the copy is
    // checked against it and refused.
    copy.hash = [7; 32];
    let r = l
        .fileops
        .add_merge("HDR merge", &copy, &plan.sources, Vec::new());
    assert!(r.is_err());
    assert!(!plan.dst.exists());
    let j = l.fileops.journal();
    let last = j.batches().unwrap().into_iter().last().unwrap();
    assert_eq!(
        (last.kind.as_str(), last.status.as_str()),
        ("merge", "failed")
    );
    let ops = j.ops(last.id).unwrap();
    assert_eq!(ops[0].dst, plan.dst);
    assert_eq!(ops[0].status, OpStatus::Failed);
}

#[test]
fn moves_carry_the_record_and_merges_are_never_perceptual_duplicates() {
    let l = lib();
    let plan = merges::plan(&l.roots, Kind::Hdr, &l.sources).unwrap();
    l.add(&plan).unwrap();
    // Moving a source rewrites the record in the same batch.
    let moved = l.lib.join("elsewhere/DSC_0042.dng");
    let moves = vec![shoebox_core::fileops::PlannedMove {
        src: l.sources[1].clone(),
        dst: moved.clone(),
    }];
    let writes = shoebox_core::organise::reference_writes(
        std::slice::from_ref(&l.lib),
        std::slice::from_ref(&l.lib),
        &moves,
    )
    .unwrap();
    assert!(writes.iter().any(|(p, _)| p == &merges::file_for(&l.lib)));
    l.fileops
        .execute_with_writes("move", &shoebox_core::fileops::Plan { moves }, writes)
        .unwrap();
    let rec = merges::load(std::slice::from_ref(&l.lib));
    assert_eq!(rec[0].sources[1].path, moved);

    // Duplicate detection: the merge is marked, and matches nothing
    // perceptually.
    let roots = vec![Root::new(&l.lib, Role::Scan).unwrap()];
    let files = shoebox_core::scan::scan(&roots, &[]).unwrap();
    let mut cache = Cache::open(&l.cache.join("dupes.sqlite")).unwrap();
    let (analyzed, _) = shoebox_core::pipeline::analyze_all(&mut cache, &roots, files).unwrap();
    let items = shoebox_core::pipeline::items(&roots, &analyzed);
    let at = |p: &Path| analyzed.iter().position(|a| a.file.path == p).unwrap();
    let m = &items[at(&plan.dst)];
    assert!(m.merged);
    let cfg = shoebox_core::dupes::Config::default();
    for s in [&l.sources[0], &moved, &l.sources[2]] {
        assert!(shoebox_core::dupes::compare(m, &items[at(s)], &cfg).is_none());
    }
}
