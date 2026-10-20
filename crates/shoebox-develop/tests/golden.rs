//! Golden images (EDITING-SPEC, Testing): renders compared with stored
//! goldens by ΔE2000, mean ≤ 0.5 and max ≤ 3.
//!
//! - The synthetic set: DNGs made here, rendered with a few edits, against
//!   `tests/golden/*.tif`.
//! - The real set: `SHOEBOX_GOLDEN_DIR=<dir>` holding raw files and their
//!   edit documents (`<name>.json`, whose `source.path` names the raw file
//!   in the same folder); goldens in `<dir>/golden/`. Rendered at a 2048 px
//!   long edge, on the CPU (the GPU pipe is checked against the CPU one).
//!   High efficiency NEFs go through Adobe DNG Converter (cache in
//!   `<dir>/golden/dng`).
//!
//! `SHOEBOX_GOLDEN_BLESS=1` writes the goldens instead of comparing: do it
//! deliberately, after a process-version bump or a change meant to alter
//! renders. Each set's `manifest.json` records the process version its
//! goldens were made with; a mismatch fails until they're blessed again.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use shoebox_develop::cpu::{self, Options, Target};
use shoebox_develop::edit::{Demosaic, EditDoc, Highlights, PROCESS_VERSION, Settings, WbMode};
use shoebox_develop::golden::{Image, MAX_ANY, MAX_MEAN, difference};
use shoebox_fixtures::Dng;

fn blessing() -> bool {
    std::env::var_os("SHOEBOX_GOLDEN_BLESS").is_some_and(|v| v == "1")
}

/// Compare `renders` with the goldens in `dir` (or write them), checking
/// the manifest's process version. Returns failures.
fn check(dir: &Path, renders: Vec<(String, Image, Target)>) -> Vec<String> {
    let manifest = dir.join("manifest.json");
    if blessing() {
        std::fs::create_dir_all(dir).unwrap();
        for (name, img, _) in &renders {
            std::fs::write(dir.join(format!("{name}.tif")), img.to_tiff().unwrap()).unwrap();
        }
        std::fs::write(
            &manifest,
            format!("{{ \"process_version\": {PROCESS_VERSION} }}\n"),
        )
        .unwrap();
        eprintln!("blessed {} goldens in {}", renders.len(), dir.display());
        return Vec::new();
    }
    let made: u32 = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["process_version"].as_u64())
        .unwrap_or(0) as u32;
    if made != PROCESS_VERSION {
        return vec![format!(
            "goldens in {} are for process version {made}, not {PROCESS_VERSION}: \
             check the renders, then bless them (SHOEBOX_GOLDEN_BLESS=1)",
            dir.display()
        )];
    }
    let mut failures = Vec::new();
    for (name, img, target) in renders {
        let path = dir.join(format!("{name}.tif"));
        let golden = match Image::read(&path) {
            Ok(g) => g,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        match difference(&img, &golden, target) {
            None => failures.push(format!(
                "{name}: {}×{} now, {}×{} golden",
                img.width, img.height, golden.width, golden.height
            )),
            Some((mean, max)) => {
                eprintln!("{name}: ΔE2000 mean {mean:.3}, max {max:.3}");
                if mean > MAX_MEAN || max > MAX_ANY {
                    failures.push(format!("{name}: ΔE2000 mean {mean:.3}, max {max:.3}"));
                }
            }
        }
    }
    failures
}

/// A colourful synthetic raw: smooth hues, a dark corner and a clipped
/// highlight.
fn synthetic() -> Vec<u8> {
    let d = Dng::new(128, 96);
    let (black, white) = (d.black, d.white);
    d.write(|x, y| {
        let (fx, fy) = (x as f32 / 127.0, y as f32 / 95.0);
        let c = (y % 2) * 2 + (x % 2); // R G / G B
        let level = match c {
            0 => 0.15 + 0.6 * fx,
            3 => 0.15 + 0.6 * (1.0 - fx),
            _ => 0.1 + 0.5 * fy,
        };
        let spot = ((fx - 0.7).powi(2) + (fy - 0.3).powi(2)).sqrt() < 0.08;
        let v = if spot {
            1.2
        } else {
            level * (0.3 + 0.7 * fy.max(0.2))
        };
        let span = f32::from(white - black);
        (f32::from(black) + v.min(1.0) * span).round() as u16
    })
}

fn synthetic_cases() -> Vec<(&'static str, Settings, Target)> {
    let mut bright = Settings::default();
    bright.modules.exposure.ev = 1.0;
    bright.modules.white_balance.mode = WbMode::Daylight;
    bright.modules.display_transform.contrast = 30.0;
    let mut turned = Settings::default();
    turned.modules.raw.demosaic = Demosaic::Bilinear;
    turned.modules.raw.highlights = Highlights::Clip;
    turned.geometry.quarter = 1;
    turned.geometry.rotate = 3.0;
    turned.geometry.crop = [0.1, 0.1, 0.9, 0.85];
    vec![
        ("default", Settings::default(), Target::Srgb),
        ("bright-daylight", bright, Target::Srgb),
        ("turned-cropped-p3", turned, Target::DisplayP3),
    ]
}

#[test]
fn synthetic_goldens() {
    let raw = shoebox_decode::raw::decode(&synthetic()).unwrap();
    let renders = synthetic_cases()
        .into_iter()
        .map(|(name, s, target)| {
            let o = Options {
                target,
                ..Options::default()
            };
            (
                name.to_owned(),
                Image::from_render(&cpu::render(&raw, &s, &o)),
                target,
            )
        })
        .collect();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let failures = check(&dir, renders);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A golden compared with itself is 0; a small shift is small; a big one
/// fails.
#[test]
fn differences_are_measured() {
    let raw = shoebox_decode::raw::decode(&synthetic()).unwrap();
    let o = Options::default();
    let a = Image::from_render(&cpu::render(&raw, &Settings::default(), &o));
    assert_eq!(difference(&a, &a, Target::Srgb), Some((0.0, 0.0)));
    let mut s = Settings::default();
    s.modules.exposure.ev = 0.02;
    let b = Image::from_render(&cpu::render(&raw, &s, &o));
    let (mean, _) = difference(&a, &b, Target::Srgb).unwrap();
    assert!(mean > 0.0 && mean < MAX_MEAN, "{mean}");
    s.modules.exposure.ev = 1.0;
    let c = Image::from_render(&cpu::render(&raw, &s, &o));
    let (mean, _) = difference(&a, &c, Target::Srgb).unwrap();
    assert!(mean > MAX_MEAN, "{mean}");
    let small = Image {
        width: 1,
        height: 1,
        px: vec![0; 3],
    };
    assert_eq!(difference(&a, &small, Target::Srgb), None);
}

/// The real set (skipped without `SHOEBOX_GOLDEN_DIR`).
#[test]
fn real_goldens() {
    let Some(dir) = std::env::var_os("SHOEBOX_GOLDEN_DIR").map(PathBuf::from) else {
        eprintln!("skipping: SHOEBOX_GOLDEN_DIR isn't set");
        return;
    };
    let out = dir.join("golden");
    let dngs = shoebox_develop::dng::DngCache::new(&out, u64::MAX);
    let converter = shoebox_develop::dng::Converter::find(None);
    let mut docs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    docs.sort();
    assert!(!docs.is_empty(), "no edit documents in {}", dir.display());
    let mut renders = Vec::new();
    for doc in docs {
        let name = doc.file_stem().unwrap().to_string_lossy().into_owned();
        let edit = EditDoc::from_json(&std::fs::read_to_string(&doc).unwrap()).unwrap();
        let file = Path::new(&edit.source.path).file_name().unwrap();
        let raw_path = dir.join(file);
        let bytes = std::fs::read(&raw_path).unwrap();
        let source = if shoebox_develop::dng::needs_dng(&bytes) {
            let conv = converter
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: needs Adobe DNG Converter"));
            dngs.get_or_convert(conv, &raw_path, &bytes).unwrap()
        } else {
            raw_path.clone()
        };
        let orientation = shoebox_decode::nef::ifd0_orientation(&bytes)
            .ok()
            .flatten()
            .unwrap_or(1);
        let raw = shoebox_decode::raw::decode_path(&source).unwrap();
        let o = Options {
            long_edge: Some(2048),
            orientation,
            target: Target::Srgb,
        };
        renders.push((
            name,
            Image::from_render(&cpu::render(&raw, edit.settings(), &o)),
            Target::Srgb,
        ));
    }
    let failures = check(&out, renders);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
