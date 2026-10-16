//! Checks against real camera files in `tests/fixtures/real/` (gitignored).
//! Each test passes trivially, with a note, when the folder is absent.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use shoebox_core::dupes::{self, Config, Level};
use shoebox_core::hash;
use shoebox_core::scan::{self, Role, Root};
use shoebox_decode::Format;

fn real_dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/real");
    if d.is_dir() {
        Some(d)
    } else {
        eprintln!("skipping: {} not present", d.display());
        None
    }
}

fn real_files() -> Vec<PathBuf> {
    let Some(d) = real_dir() else {
        return Vec::new();
    };
    let root = Root::new(&d, Role::Scan).unwrap();
    scan::scan(&[root], &[])
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect()
}

#[test]
fn every_real_file_analyses_cleanly() {
    for p in real_files() {
        let a = hash::analyze_path(&p).unwrap();
        assert!(a.error.is_none(), "{}: {:?}", p.display(), a.error);
        assert_eq!(a.hashes.len(), 4, "{}", p.display());
    }
}

#[test]
fn real_nefs_have_full_size_jpg_from_raw() {
    for p in real_files()
        .into_iter()
        .filter(|p| Format::from_path(p) == Some(Format::Nef))
    {
        let data = std::fs::read(&p).unwrap();
        let r = shoebox_decode::nef::jpg_from_raw(&data)
            .unwrap()
            .expect("JpgFromRaw");
        let (w, h) = shoebox_decode::jpeg::dimensions(&data[r]).unwrap();
        assert!(
            w.max(h) >= 4000,
            "{}: embedded JPEG only {w}x{h}",
            p.display()
        );
    }
}

/// Carve each NEF's embedded JPEG byte for byte (in memory) and check it
/// matches the NEF at the image-data level, distance 0.
#[test]
fn carved_real_previews_match_at_distance_zero() {
    let cfg = Config::default();
    for p in real_files()
        .into_iter()
        .filter(|p| Format::from_path(p) == Some(Format::Nef))
    {
        let data = std::fs::read(&p).unwrap();
        let r = shoebox_decode::nef::jpg_from_raw(&data).unwrap().unwrap();
        let carved = hash::analyze_bytes(Format::Jpeg, &data[r]);
        let nef = hash::analyze_bytes(Format::Nef, &data);
        let item = |a: &hash::Analysis, f| dupes::Item {
            role: Role::Scan,
            format: f,
            blake3: a.blake3,
            image_data_hash: a.image_data_hash,
            hashes: a.hashes.clone(),
            pixels: 0,
        };
        let m =
            dupes::compare(&item(&nef, Format::Nef), &item(&carved, Format::Jpeg), &cfg).unwrap();
        assert_eq!(
            (m.level, m.distance),
            (Level::ImageData, 0),
            "{}",
            p.display()
        );
    }
}
