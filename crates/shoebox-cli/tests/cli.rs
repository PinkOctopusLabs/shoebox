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

fn run(data: &Path, args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shoebox"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .output()
        .unwrap()
}

/// Rewrite the report, setting `confirm` on rows whose path ends with one of `names`.
fn confirm_rows(report: &Path, names: &[&str]) {
    let mut rdr = csv::Reader::from_path(report).unwrap();
    let headers = rdr.headers().unwrap().clone();
    let (ci, pi) = (
        headers.iter().position(|h| h == "confirm").unwrap(),
        headers.iter().position(|h| h == "path").unwrap(),
    );
    let mut out = csv::Writer::from_writer(Vec::new());
    out.write_record(&headers).unwrap();
    for rec in rdr.records() {
        let rec = rec.unwrap();
        let mut fields: Vec<String> = rec.iter().map(str::to_owned).collect();
        if names.iter().any(|n| fields[pi].ends_with(n)) {
            fields[ci] = "y".into();
        }
        out.write_record(&fields).unwrap();
    }
    std::fs::write(report, out.into_inner().unwrap()).unwrap();
}

#[test]
fn flag_review_apply_undo() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    let data = t.path().join("data");
    shoebox_fixtures::build_library(&lib).unwrap();
    let (r, s, out) = (lib.join("ref"), lib.join("scan"), t.path().join("matches"));
    let report = t.path().join("dupes.csv");
    let before = snapshot(&lib);
    let ref_before = snapshot(&r);

    // `dupes` (with the spec's flags) only reports; nothing moves.
    let dupes: Vec<String> = [
        "dupes",
        "--ref",
        &r.display().to_string(),
        "--scan",
        &s.display().to_string(),
        "--threshold",
        "10",
        "--report",
        &report.display().to_string(),
        "--move-matched",
        &out.display().to_string(),
        "--dry-run",
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    let o = shoebox(&data, &dupes.iter().map(String::as_str).collect::<Vec<_>>());
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(
        text.contains("6 suggested duplicates") && text.contains("Nothing was moved"),
        "{text}"
    );
    assert_eq!(snapshot(&lib), before);
    assert!(!out.exists());
    let csv_text = std::fs::read_to_string(&report).unwrap();
    assert!(csv_text.starts_with("confirm,"));
    assert_eq!(
        csv_text
            .lines()
            .filter(|l| l.contains(",duplicate,"))
            .count(),
        6
    );

    // Nothing confirmed: apply is a no-op.
    let apply = |extra: &[&str]| {
        let mut v = vec!["apply".to_string(), report.display().to_string()];
        v.extend(extra.iter().map(|x| x.to_string()));
        v
    };
    let o = shoebox(&data, &["apply", report.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("No rows are confirmed"));
    assert_eq!(snapshot(&lib), before);

    // Confirm two rows; dry run moves nothing; refuses without --yes off a terminal.
    confirm_rows(&report, &["recovered/f0001.jpg", "rotated/f0008.jpg"]);
    shoebox(
        &data,
        &apply(&["--dry-run"])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_eq!(snapshot(&lib), before);
    assert!(!run(&data, &apply(&[])).status.success());
    assert_eq!(snapshot(&lib), before);

    // Apply: only the confirmed files (and the sidecar) move.
    let o = shoebox(
        &data,
        &apply(&["--yes"])
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("as batch 1"));
    let moved = snapshot(&out);
    assert_eq!(
        moved.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "recovered/f0001.jpg",
            "recovered/f0001.jpg.xmp",
            "rotated/f0008.jpg"
        ]
    );
    for (k, v) in &moved {
        assert_eq!(before.get(&format!("scan/{k}")), Some(v), "bytes preserved");
    }
    assert!(
        s.join("recovered/f0003.jpg").exists(),
        "unconfirmed duplicate stays"
    );
    assert_eq!(snapshot(&r), ref_before, "reference root untouched");

    // Undo restores the library byte for byte.
    shoebox(&data, &["undo", "--yes"]);
    assert_eq!(snapshot(&lib), before);
}

#[test]
fn apply_refuses_unsafe_confirmations() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    let data = t.path().join("data");
    shoebox_fixtures::build_library(&lib).unwrap();
    let report = t.path().join("dupes.csv");
    let dupes = |rep: &Path| {
        shoebox(
            &data,
            &[
                "dupes",
                "--ref",
                lib.join("ref").to_str().unwrap(),
                "--scan",
                lib.join("scan").to_str().unwrap(),
                "--report",
                rep.to_str().unwrap(),
                "--move-matched",
                t.path().join("out").to_str().unwrap(),
            ],
        );
    };
    let apply = || {
        run(
            &data,
            &["apply".into(), report.display().to_string(), "--yes".into()],
        )
    };
    let before = snapshot(&lib);

    // A reference file.
    dupes(&report);
    confirm_rows(&report, &["DSC_0001.JPG"]);
    let o = apply();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("reference file"));

    // Every file of a group (the scan-only pair f0005/f0006).
    dupes(&report);
    confirm_rows(&report, &["f0005.jpg", "f0006.jpg"]);
    let o = apply();
    assert!(String::from_utf8_lossy(&o.stderr).contains("leave at least one"));

    // A file that changed after review.
    dupes(&report);
    confirm_rows(&report, &["f0003.jpg"]);
    let f = lib.join("scan/recovered/f0003.jpg");
    let mut bytes = std::fs::read(&f).unwrap();
    bytes.push(0);
    std::fs::write(&f, &bytes).unwrap();
    let o = apply();
    assert!(String::from_utf8_lossy(&o.stderr).contains("changed since the report"));
    bytes.pop();
    std::fs::write(&f, &bytes).unwrap();

    assert_eq!(snapshot(&lib), before, "nothing moved by refused applies");
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
        .arg("--report")
        .arg(t.path().join("r.csv"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("inside reference root"));
}
