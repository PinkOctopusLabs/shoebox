//! End-to-end duplicate detection over the synthetic fixture library.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::Path;

use shoebox_core::cache::Cache;
use shoebox_core::dupes::{self, Config, Item, Level};
use shoebox_core::pipeline;
use shoebox_core::scan::{self, Role, Root};

struct Run {
    names: Vec<String>,
    items: Vec<Item>,
    stats: pipeline::Stats,
}

fn run(root: &Path, cache: &mut Cache) -> Run {
    let roots = vec![
        Root::new(&root.join("ref"), Role::Reference).unwrap(),
        Root::new(&root.join("scan"), Role::Scan).unwrap(),
    ];
    let files = scan::scan(&roots, &[]).unwrap();
    let (analyzed, stats) = pipeline::analyze_all(cache, &roots, files).unwrap();
    let names = analyzed
        .iter()
        .map(|a| {
            format!(
                "{}/{}",
                roots[a.file.root]
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy(),
                a.file.rel
            )
        })
        .collect();
    let items = pipeline::items(&roots, &analyzed);
    Run {
        names,
        items,
        stats,
    }
}

fn idx(r: &Run, name: &str) -> usize {
    r.names
        .iter()
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("no {name} in {:?}", r.names))
}

fn library() -> (tempfile::TempDir, Run) {
    let t = tempfile::tempdir().unwrap();
    shoebox_fixtures::build_library(t.path()).unwrap();
    let mut cache = Cache::open_in_memory().unwrap();
    let r = run(t.path(), &mut cache);
    (t, r)
}

const NEF1: &str = "ref/2026/2026-04-24/DSC_0001.NEF";
const NEF2: &str = "ref/2026/2026-04-24/DSC_0002.NEF";

#[test]
fn carved_previews_match_their_nef_at_distance_zero() {
    let (_t, r) = library();
    let cfg = Config::default();
    for (nef, carved) in [
        (NEF1, "scan/recovered/f0001.jpg"),
        (NEF2, "scan/recovered/f0002.jpg"),
    ] {
        let m = dupes::compare(&r.items[idx(&r, nef)], &r.items[idx(&r, carved)], &cfg).unwrap();
        assert_eq!(
            (m.level, m.distance),
            (Level::ImageData, 0),
            "{nef} vs {carved}"
        );
    }
}

#[test]
fn unrotated_preview_matches_through_rotation() {
    let (_t, r) = library();
    let m = dupes::compare(
        &r.items[idx(&r, "ref/lightroom/DSC_0002.tif")],
        &r.items[idx(&r, "scan/rotated/f0008.jpg")],
        &Config::default(),
    )
    .unwrap();
    assert_eq!(m.level, Level::NearIdentical);
    assert_eq!(m.rotation, 90);
}

#[test]
fn groups_keepers_and_moves() {
    let (_t, r) = library();
    let groups = dupes::find_groups(&r.items, &Config::default());
    let by_keeper: HashMap<&str, Vec<&str>> = groups
        .iter()
        .map(|g| {
            let mut ms: Vec<&str> = g.members.iter().map(|m| r.names[m.item].as_str()).collect();
            ms.sort_unstable();
            (r.names[g.keeper].as_str(), ms)
        })
        .collect();
    assert_eq!(
        by_keeper[NEF1],
        // The camera JPG is the NEF's sibling (same folder and name), so it's
        // a different rendition of the frame, not a duplicate.
        vec![
            "ref/lightroom/DSC_0001.tif",
            "scan/misc/f0007.png",
            "scan/recovered/f0001.jpg",
            "scan/recovered/f0003.jpg",
            "scan/recovered/f0004.jpg",
            "scan/recovered/f0010.jpg",
        ]
    );
    assert_eq!(
        by_keeper[NEF2],
        vec![
            "ref/lightroom/DSC_0002.tif",
            "scan/recovered/f0002.jpg",
            "scan/rotated/f0008.jpg"
        ]
    );
    assert_eq!(
        by_keeper["scan/recovered/f0005.jpg"],
        vec!["scan/recovered/f0006.jpg"]
    );
    assert_eq!(
        groups.len(),
        3,
        "the -2 variant is not a duplicate: {by_keeper:?}"
    );

    let mut movable: Vec<&str> = groups
        .iter()
        .flat_map(|g| g.members.iter())
        .filter(|m| m.suggestion(&r.items) == dupes::Suggestion::Duplicate)
        .map(|m| r.names[m.item].as_str())
        .collect();
    movable.sort_unstable();
    assert_eq!(
        movable,
        vec![
            "scan/misc/f0007.png",
            "scan/recovered/f0001.jpg",
            "scan/recovered/f0002.jpg",
            "scan/recovered/f0003.jpg",
            "scan/recovered/f0004.jpg",
            "scan/rotated/f0008.jpg",
        ]
    );
}

#[test]
fn second_run_hits_the_cache() {
    let t = tempfile::tempdir().unwrap();
    shoebox_fixtures::build_library(t.path()).unwrap();
    let db = t.path().join("cache.sqlite");
    let first = run(t.path(), &mut Cache::open(&db).unwrap());
    assert_eq!(first.stats.analyzed, first.stats.files);
    let second = run(t.path(), &mut Cache::open(&db).unwrap());
    assert_eq!(second.stats.cached, second.stats.files);
    assert_eq!(second.stats.analyzed, 0);
    // Cached analysis is identical to fresh analysis.
    for (a, b) in first.items.iter().zip(&second.items) {
        assert_eq!(a.blake3, b.blake3);
        assert_eq!(a.image_data_hash, b.image_data_hash);
        assert_eq!(a.hashes, b.hashes);
        assert_eq!(a.pixels, b.pixels);
    }
    // A changed file is re-analysed.
    let f = t.path().join("scan/recovered/f0005.jpg");
    let mut bytes = std::fs::read(&f).unwrap();
    bytes.extend(b"trailer");
    std::fs::write(&f, bytes).unwrap();
    let third = run(t.path(), &mut Cache::open(&db).unwrap());
    assert_eq!(third.stats.analyzed, 1);
}

#[test]
fn nef_metadata_reads() {
    let (t, r) = library();
    let i = idx(&r, NEF2);
    assert_eq!(r.items[i].pixels, 1200 * 1800);
    let a = shoebox_core::hash::analyze_path(&t.path().join(NEF2)).unwrap();
    assert_eq!(a.exif.model.as_deref(), Some("NIKON Z f"));
    assert_eq!(a.exif.orientation, Some(6));
    assert_eq!(a.exif.capture_time.as_deref(), Some("2026:04:24 14:05:02"));
    assert_eq!((a.width, a.height), (Some(1200), Some(1800)));
}
