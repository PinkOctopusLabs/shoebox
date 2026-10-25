//! Duplicate review over the synthetic library: find, auto-select, apply as
//! one batch, undo.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use shoebox_core::cache::Cache;
use shoebox_core::fileops::{self, FileOps, Journal};
use shoebox_core::review::{self, Decision};
use shoebox_core::scan::{Role, Root};

fn roots(lib: &Path) -> Vec<Root> {
    vec![
        Root::new(&lib.join("ref"), Role::Reference).unwrap(),
        Root::new(&lib.join("scan"), Role::Scan).unwrap(),
    ]
}

/// Every file under `dir` with its BLAKE3 (to prove a tree is untouched).
fn tree(dir: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let data = std::fs::read(e.path()).unwrap();
            (
                e.path().strip_prefix(dir).unwrap().to_path_buf(),
                *blake3::hash(&data).as_bytes(),
            )
        })
        .collect()
}

fn name(r: &review::Review, f: usize) -> String {
    let file = &r.files[f];
    format!(
        "{}/{}",
        r.roots[file.root]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy(),
        file.rel
    )
}

#[test]
fn review_auto_select_apply_and_undo() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let lib = lib.canonicalize().unwrap();
    let mut cache = Cache::open_in_memory().unwrap();
    let r = review::find(
        &mut cache,
        roots(&lib),
        Default::default(),
        &Default::default(),
        None,
        &|_, _| {},
    )
    .unwrap();

    // Each file to decide on sits with its closest copy (the kept copy
    // first), most alike groups first.
    let groups: Vec<Vec<String>> = r
        .groups
        .iter()
        .map(|g| g.entries.iter().map(|e| name(&r, e.file)).collect())
        .collect();
    assert_eq!(
        groups,
        vec![
            vec![
                "ref/2026/2026-04-24/DSC_0001.JPG",
                "scan/recovered/f0004.jpg"
            ],
            vec!["scan/recovered/f0005.jpg", "scan/recovered/f0006.jpg"],
            vec![
                "ref/2026/2026-04-24/DSC_0002.NEF",
                "scan/recovered/f0002.jpg",
                "scan/rotated/f0008.jpg"
            ],
            vec!["ref/lightroom/DSC_0001.tif", "scan/misc/f0007.png"],
            vec![
                "ref/2026/2026-04-24/DSC_0001.NEF",
                "scan/recovered/f0001.jpg",
                "scan/recovered/f0003.jpg",
                "scan/recovered/f0010.jpg"
            ],
        ]
    );
    // Most alike first: each group's least alike match, never rising.
    let least: Vec<u8> = r
        .groups
        .iter()
        .map(|g| g.summary().map_or(100, |m| m.percent()))
        .collect();
    assert!(least.windows(2).all(|w| w[0] >= w[1]), "{least:?}");

    // Within a scope (an event, collection or folder), both files of a
    // match are in it: f0001 goes with the JPG of its frame, the NEF being
    // out, and f0005's match is out too.
    let only: HashSet<PathBuf> = [
        "ref/2026/2026-04-24/DSC_0001.JPG",
        "scan/recovered/f0004.jpg",
        "scan/recovered/f0001.jpg",
        "scan/recovered/f0005.jpg",
    ]
    .iter()
    .map(|n| lib.join(n))
    .collect();
    let scoped = review::find(
        &mut cache,
        roots(&lib),
        Default::default(),
        &Default::default(),
        Some(&only),
        &|_, _| {},
    )
    .unwrap();
    let groups: Vec<Vec<String>> = scoped
        .groups
        .iter()
        .map(|g| g.entries.iter().map(|e| name(&scoped, e.file)).collect())
        .collect();
    assert_eq!(
        groups,
        [[
            "ref/2026/2026-04-24/DSC_0001.JPG",
            "scan/recovered/f0001.jpg",
            "scan/recovered/f0004.jpg"
        ]]
    );

    // Any copy as alike as a match can be, if taken at the same moment.
    let every = review::AutoSelect {
        up_to: Some(shoebox_core::dupes::MAX_THRESHOLD),
        within: Some(0.0),
    };
    let mut decisions: HashMap<PathBuf, Decision> = HashMap::new();
    let mut proposed: BTreeMap<String, Decision> = BTreeMap::new();
    for g in &r.groups {
        for (e, d) in g.entries.iter().zip(review::auto_select(&r, g, every)) {
            decisions.insert(r.files[e.file].path.clone(), d);
            proposed.insert(name(&r, e.file), d);
        }
    }
    use Decision::*;
    let expected: BTreeMap<String, Decision> = [
        ("ref/2026/2026-04-24/DSC_0001.JPG", Keep),
        ("ref/2026/2026-04-24/DSC_0001.NEF", Keep),
        ("ref/lightroom/DSC_0001.tif", Keep),
        // Resized copies with no capture time to tell them apart: moved.
        ("scan/misc/f0007.png", Move),
        ("scan/recovered/f0001.jpg", Move), // same image data as the NEF
        ("scan/recovered/f0003.jpg", Move),
        ("scan/recovered/f0004.jpg", Move),
        ("scan/recovered/f0010.jpg", Undecided), // a burst neighbour: a person decides
        ("ref/2026/2026-04-24/DSC_0002.NEF", Keep),
        ("scan/recovered/f0002.jpg", Move),
        ("scan/rotated/f0008.jpg", Move),
        ("scan/recovered/f0005.jpg", Keep), // scan-only: one copy stays
        ("scan/recovered/f0006.jpg", Move),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(proposed, expected);

    // At 100% (the default): just the same file or image data; down to
    // near-identical: those and the near-identical copies taken at the same
    // moment; not only at the same moment: the burst frame too.
    let moved = |mode| -> Vec<String> {
        let mut v: Vec<String> = r
            .groups
            .iter()
            .flat_map(|g| {
                g.entries
                    .iter()
                    .zip(review::auto_select(&r, g, mode))
                    .filter(|(_, d)| *d == Move)
                    .map(|(e, _)| name(&r, e.file))
            })
            .collect();
        v.sort();
        v
    };
    let identical = moved(review::AutoSelect::default());
    assert_eq!(
        identical,
        [
            "scan/recovered/f0001.jpg",
            "scan/recovered/f0002.jpg",
            "scan/recovered/f0004.jpg",
            "scan/recovered/f0006.jpg"
        ]
    );
    // Every copy taken at the same moment here is near-identical.
    let near = moved(review::AutoSelect {
        up_to: Some(shoebox_core::dupes::DEFAULT_NEAR_IDENTICAL),
        within: Some(0.0),
    });
    assert_eq!(near, moved(every));
    assert!(identical.iter().all(|f| near.contains(f)) && near.len() == 7);
    let any_time = moved(review::AutoSelect {
        within: None,
        ..every
    });
    assert!(any_time.contains(&"scan/recovered/f0010.jpg".to_owned()));
    assert_eq!(any_time.len(), 8);
    // The burst frame is 1 s from its copy: within 2 s takes it, within
    // half a second doesn't.
    let within = |w| {
        moved(review::AutoSelect {
            within: Some(w),
            ..every
        })
    };
    assert_eq!(within(2.0), any_time);
    assert_eq!(within(0.5), near);

    // The list follows the threshold without reading a file again: at the
    // strictest, only matches at 0 bits (99%) and 100% are left.
    let strict = review::regroup(
        r.found.clone(),
        shoebox_core::dupes::Config {
            threshold: 0,
            near_identical: 0,
        },
    );
    let worst = |r: &review::Review| {
        r.groups
            .iter()
            .flat_map(|g| g.entries.iter().filter_map(|e| e.best))
            .map(|(_, m)| m.distance)
            .max()
    };
    assert_eq!(worst(&strict), Some(0));
    assert!(worst(&r) > Some(0));
    assert!(strict.files.len() < r.files.len());
    let again = review::regroup(r.found.clone(), r.config);
    assert_eq!(again.files.len(), r.files.len());
    assert_eq!(again.groups.len(), r.groups.len());
    for g in &r.groups {
        for e in &g.entries {
            if identical.contains(&name(&r, e.file)) {
                let (_, m) = e.best.unwrap();
                assert_eq!(m.percent(), 100);
            } else if let Some((_, m)) = e.best {
                assert!(m.percent() < 100, "{}: {m:?}", name(&r, e.file));
            }
        }
    }

    // Unsafe decisions are refused.
    let path = |n: &str| lib.join(n);
    let mut bad = decisions.clone();
    bad.insert(path("ref/lightroom/DSC_0001.tif"), Move);
    assert!(
        review::plan(&r, &bad).is_err(),
        "reference files never move"
    );
    let mut bad = decisions.clone();
    bad.insert(path("scan/recovered/f0005.jpg"), Move);
    assert!(review::plan(&r, &bad).is_err(), "a group keeps one file");

    // A person makes the burst neighbour a variant of the NEF's stack.
    decisions.insert(path("scan/recovered/f0010.jpg"), Variant);
    let plan = review::plan(&r, &decisions).unwrap();
    assert_eq!(plan.moves.len(), 7);
    assert!(plan.moves.iter().all(|(src, dst)| {
        let rel = src.strip_prefix(lib.join("scan")).unwrap();
        *dst == lib.join("scan/_duplicates").join(rel)
    }));
    assert_eq!(plan.links.len(), 1);
    assert_eq!(plan.links[0].of, path("ref/2026/2026-04-24/DSC_0001.NEF"));
    for (src, _) in &plan.moves {
        review::check_unchanged(&r, src).unwrap();
    }

    // Apply as one batch: moves (with sidecars) and stacks.json.
    let ref_before = tree(&lib.join("ref"));
    let scan_before = tree(&lib.join("scan"));
    let ops = FileOps::new(
        Journal::open(&t.path().join("journal.sqlite")).unwrap(),
        vec![lib.join("ref")],
    );
    let moves = fileops::plan_moves(&plan.moves).unwrap();
    assert_eq!(moves.moves.len(), 8, "f0001.jpg.xmp moves with its image");
    let writes = shoebox_core::stacks::plan_save_links(&[lib.join("scan")], &plan.links).unwrap();
    let batch = ops
        .execute_with_writes("duplicates", &moves, writes)
        .unwrap();
    assert!(
        lib.join("scan/_duplicates/recovered/f0001.jpg.xmp")
            .exists()
    );
    assert!(!lib.join("scan/recovered/f0001.jpg").exists());
    assert_eq!(tree(&lib.join("ref")), ref_before, "reference untouched");
    let links = shoebox_core::stacks::load_links(&[lib.join("scan")]);
    assert_eq!(links, plan.links);

    // The catalog stacks the variant with its NEF; the rest are gone.
    let (c, _) =
        shoebox_core::catalog::Catalog::refresh(&mut cache, roots(&lib), &|_, _| {}).unwrap();
    let i = c
        .photos
        .iter()
        .position(|p| p.path == path("scan/recovered/f0010.jpg"))
        .unwrap();
    let st = &c.stacks[c.stack_of[i]];
    assert!(st.variants.contains(&i));
    assert_eq!(
        c.photos[st.primary].path,
        path("ref/2026/2026-04-24/DSC_0001.NEF")
    );

    // A re-run (with the variant settled) finds nothing left in the scan root.
    let settled = c.links.iter().map(|l| l.file.clone()).collect();
    let again = review::find(
        &mut cache,
        roots(&lib),
        Default::default(),
        &settled,
        None,
        &|_, _| {},
    )
    .unwrap();
    let left: Vec<String> = again
        .groups
        .iter()
        .flat_map(|g| g.entries.iter().map(|e| name(&again, e.file)))
        .filter(|n| n.starts_with("scan/"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(again.groups.iter().all(|g| g.reference_only(&again)));

    // One undo restores everything.
    ops.undo(batch).unwrap();
    let mut after = tree(&lib.join("scan"));
    // Undo can't delete the stacks.json it created; it leaves it empty.
    assert_eq!(
        after.remove(Path::new(".shoebox/stacks.json")),
        Some(*blake3::hash(b"{}").as_bytes())
    );
    assert_eq!(after, scan_before);
    assert!(shoebox_core::stacks::load_links(&[lib.join("scan")]).is_empty());
}

#[test]
fn ignored_pairs_never_come_back() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let lib = lib.canonicalize().unwrap();
    let mut cache = Cache::open_in_memory().unwrap();
    let r = review::find(
        &mut cache,
        roots(&lib),
        Default::default(),
        &Default::default(),
        None,
        &|_, _| {},
    )
    .unwrap();
    let f6 = lib.join("scan/recovered/f0006.jpg");
    let decisions = HashMap::from([(f6.clone(), Decision::Ignore)]);
    let plan = review::plan(&r, &decisions).unwrap();
    assert_eq!((plan.moves.len(), plan.ignores.len()), (0, 1));
    cache.ignore_pairs(&plan.ignores).unwrap();
    let again = review::find(
        &mut cache,
        roots(&lib),
        Default::default(),
        &Default::default(),
        None,
        &|_, _| {},
    )
    .unwrap();
    assert!(
        again
            .groups
            .iter()
            .all(|g| g.entries.iter().all(|e| again.files[e.file].path != f6)),
        "the dismissed pair is gone"
    );
    assert_eq!(again.groups.len(), r.groups.len() - 1);
}
