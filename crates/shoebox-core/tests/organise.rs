//! Moving and renaming whole stacks: members and sidecars together,
//! references to them kept, cache rows carried, one undo.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use shoebox_core::cache::Cache;
use shoebox_core::catalog::Catalog;
use shoebox_core::fileops::{FileOps, Journal};
use shoebox_core::scan::{Role, Root};
use shoebox_core::template::{DEFAULT_RENAME, Template};

fn put(p: &Path, b: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

#[test]
fn rename_move_reject_and_undo() {
    let t = tempfile::tempdir().unwrap();
    let base = t.path().canonicalize().unwrap();
    let lib = base.join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    // A writable library: the fixture's ref tree, with a shared NEF sidecar.
    let root = lib.join("ref");
    put(
        &root.join("2026/2026-04-24/DSC_0001.xmp"),
        b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>",
    );
    let roots = || vec![Root::new(&root, Role::Scan).unwrap()];
    let mut cache = Cache::open_in_memory().unwrap();
    let (mut c, _) = Catalog::refresh(&mut cache, roots(), &|_, _| {}).unwrap();
    let find = |c: &Catalog, rel: &str| c.photos.iter().position(|p| p.rel == rel).unwrap();
    let nef = find(&c, "2026/2026-04-24/DSC_0001.NEF");
    let s = c.stack_of[nef];

    // An event names the NEF by path (added by hand).
    let ev = shoebox_core::events::Event {
        id: "e1".into(),
        name: "Trip".into(),
        parent: None,
        start: "2000-01-01 00:00:00".into(),
        end: "2000-01-01 00:00:00".into(),
        include: vec![c.photos[nef].path.clone()],
        exclude: vec![],
        updated: 1,
    };
    for (p, b) in shoebox_core::events::plan_save(std::slice::from_ref(&root), &[ev]).unwrap() {
        put(&p, &b);
    }

    // Rename the NEF's stack with the default template.
    let tpl = Template::parse(DEFAULT_RENAME).unwrap();
    let r = shoebox_core::organise::plan_rename(&c, &[s], &tpl).unwrap();
    let names: Vec<(String, String)> = r
        .plan
        .moves
        .iter()
        .map(|m| {
            (
                m.src.strip_prefix(&root).unwrap().display().to_string(),
                m.dst.strip_prefix(&root).unwrap().display().to_string(),
            )
        })
        .collect();
    let d = "2026/2026-04-24";
    let want = vec![
        (
            format!("{d}/DSC_0001.NEF"),
            format!("{d}/20260424-140311-42.nef"),
        ),
        (
            format!("{d}/DSC_0001.JPG"),
            format!("{d}/20260424-140311-42.jpg"),
        ),
        (
            format!("{d}/DSC_0001.xmp"),
            format!("{d}/20260424-140311-42.xmp"),
        ),
    ];
    assert_eq!(
        names, want,
        "one name for the frame; its shared sidecar follows"
    );

    let fo = FileOps::new(Journal::open(&base.join("j.sqlite")).unwrap(), vec![]);
    let all = vec![root.clone()];
    let refs = shoebox_core::organise::reference_writes(&all, &all, &r.plan.moves).unwrap();
    assert_eq!(refs.len(), 1, "events.json is rewritten in the same batch");
    let batch = fo.execute_with_writes("rename", &r.plan, refs).unwrap();
    let ev = shoebox_core::events::load(std::slice::from_ref(&root));
    assert_eq!(
        ev[0].include,
        vec![root.join(format!("{d}/20260424-140311-42.nef"))]
    );

    // The catalog and cache follow without re-reading anything.
    c.note_moved(&r.plan.moves, &mut cache).unwrap();
    let (c2, stats) = Catalog::refresh(&mut cache, roots(), &|_, _| {}).unwrap();
    assert_eq!((stats.read, stats.changed), (0, 0), "{stats:?}");
    assert_eq!(c2.photos.len(), c.photos.len());

    // Reject the stack: to _rejected/ in its root, keeping the folders.
    let i = find(&c2, &format!("{d}/20260424-140311-42.nef"));
    let rej = shoebox_core::organise::plan_reject(&c2, &[c2.stack_of[i]]).unwrap();
    assert!(
        rej.moves
            .iter()
            .all(|m| m.dst.starts_with(root.join("_rejected").join(d)))
    );
    let b2 = fo.execute("reject", &rej).unwrap();

    // Undo both: back to the original names.
    fo.undo(b2).unwrap();
    fo.undo(batch).unwrap();
    assert!(root.join(format!("{d}/DSC_0001.NEF")).exists());
    assert!(root.join(format!("{d}/DSC_0001.xmp")).exists());
    let ev = shoebox_core::events::load(std::slice::from_ref(&root));
    assert_eq!(ev[0].include, vec![root.join(format!("{d}/DSC_0001.NEF"))]);

    // Reference roots are refused up front.
    let ref_roots = vec![Root::new(&root, Role::Reference).unwrap()];
    let (c3, _) = Catalog::refresh(&mut cache, ref_roots, &|_, _| {}).unwrap();
    assert!(shoebox_core::organise::plan_reject(&c3, &[0]).is_err());
}
