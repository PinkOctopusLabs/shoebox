//! The M1 acceptance flow, end to end through the real binary, on a fresh
//! synthetic library in a temp folder.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

/// Every file under `root` with its BLAKE3, keyed by relative path.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(
                    rel,
                    blake3::hash(&std::fs::read(&p).unwrap())
                        .to_hex()
                        .to_string(),
                );
            }
        }
    }
    out
}

fn shoebox(data: &Path, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_shoebox"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "shoebox {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn dupes_move_and_undo() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    let data = t.path().join("data");
    shoebox_fixtures::build_library(&lib).unwrap();
    let (r, s, out) = (lib.join("ref"), lib.join("scan"), t.path().join("matches"));
    let report = t.path().join("dupes.csv");
    let before = snapshot(&lib);
    let ref_before = snapshot(&r);
    let args = |extra: &[&'static str]| {
        let mut v = vec![
            "dupes".to_string(),
            "--ref".into(),
            r.display().to_string(),
            "--scan".into(),
            s.display().to_string(),
            "--threshold".into(),
            "10".into(),
            "--report".into(),
            report.display().to_string(),
            "--move-matched".into(),
            out.display().to_string(),
        ];
        v.extend(extra.iter().map(|x| x.to_string()));
        v
    };

    // Dry run: nothing changes, report lists the moves.
    let a = args(&["--dry-run"]);
    shoebox(&data, &a.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(snapshot(&lib), before);
    assert!(!out.exists());
    let csv = std::fs::read_to_string(&report).unwrap();
    assert_eq!(csv.lines().filter(|l| l.contains(",move,")).count(), 6);
    assert!(csv.contains("image_data"));

    // Without --yes and without a terminal, it refuses rather than guessing.
    let refused = Command::new(env!("CARGO_BIN_EXE_shoebox"))
        .arg("--data-dir")
        .arg(&data)
        .args(args(&[]))
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(snapshot(&lib), before);

    // Real move.
    let a = args(&["--yes"]);
    let moved = shoebox(&data, &a.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(String::from_utf8_lossy(&moved.stdout).contains("as batch 1"));
    assert_eq!(snapshot(&r), ref_before, "reference root untouched");
    let after = snapshot(&out);
    for f in [
        "recovered/f0001.jpg",
        "recovered/f0001.jpg.xmp",
        "recovered/f0002.jpg",
        "recovered/f0003.jpg",
        "recovered/f0004.jpg",
        "misc/f0007.png",
        "rotated/f0008.jpg",
    ] {
        assert!(after.contains_key(f), "{f} not moved: {after:?}");
        assert!(!s.join(f).exists());
    }
    assert!(s.join("recovered/f0005.jpg").exists() && s.join("recovered/f0006.jpg").exists());
    // Moved bytes are identical to the originals.
    for (k, v) in &after {
        assert_eq!(before.get(&format!("scan/{k}")), Some(v));
    }

    // Second run hits the cache and finds nothing more to move.
    let a = args(&["--dry-run"]);
    let again = shoebox(&data, &a.iter().map(String::as_str).collect::<Vec<_>>());
    let text = String::from_utf8_lossy(&again.stdout);
    assert!(text.contains("8 files (8 cached, 0 analysed"), "{text}");
    assert!(!text.contains("Dry run"), "nothing left to move: {text}");

    let hist = shoebox(&data, &["history"]);
    assert!(String::from_utf8_lossy(&hist.stdout).contains("done"));

    // Undo restores the library byte for byte.
    shoebox(&data, &["undo", "--yes"]);
    let restored = snapshot(&lib);
    assert_eq!(restored, before);
    let hist = String::from_utf8_lossy(&shoebox(&data, &["history"]).stdout).into_owned();
    assert!(
        hist.contains("undone") && hist.contains("undo batch 1"),
        "{hist}"
    );
}

#[test]
fn refuses_destination_inside_reference() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    shoebox_fixtures::build_library(&lib).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_shoebox"))
        .arg("--data-dir")
        .arg(t.path().join("data"))
        .args(["dupes", "--ref"])
        .arg(lib.join("ref"))
        .arg("--scan")
        .arg(lib.join("scan"))
        .arg("--move-matched")
        .arg(lib.join("ref/_duplicates"))
        .arg("--yes")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("inside reference root"));
}
