//! Golden tests for sidecar editing against real sidecars (LightCraft,
//! copied into the gitignored `tests/fixtures/real/`) and ExifTool as an
//! independent reader and writer. Skips when either is unavailable.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value as Json;
use shoebox_core::xmp::{self, Prop, Value};

fn real_sidecars() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/real");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "xmp"))
        .collect();
    v.sort();
    v
}

fn exiftool_available() -> bool {
    Command::new("exiftool")
        .arg("-ver")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Every tag ExifTool sees in an XMP file, minus file-system details.
fn exiftool_tags(path: &Path) -> BTreeMap<String, Json> {
    let out = Command::new("exiftool")
        .args(["-j", "-a", "-u", "-G1", "-struct"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Vec<BTreeMap<String, Json>> = serde_json::from_slice(&out.stdout).unwrap();
    v.into_iter()
        .next()
        .unwrap()
        .into_iter()
        .filter(|(k, _)| {
            !(k == "SourceFile"
                || k.starts_with("System:")
                || k.starts_with("File:")
                || k.starts_with("ExifTool:"))
        })
        .collect()
}

/// Tags Shoebox owns, as ExifTool names them.
fn owned(tag: &str) -> bool {
    [
        "XMP-xmp:Rating",
        "XMP-xmp:Label",
        "XMP-dc:Subject",
        "XMP-lr:HierarchicalSubject",
        "XMP-digiKam:PickLabel",
        "XMP-digiKam:ColorLabel",
        "XMP-digiKam:TagsList",
        "XMP-iptcExt:Event",
    ]
    .contains(&tag)
        || tag.starts_with("XMP-shoebox:")
}

fn marks() -> Vec<(Prop, Option<Value>)> {
    vec![
        (Prop::Rating, Some(Value::Text("4".into()))),
        (Prop::Label, Some(Value::Text("Red".into()))),
        (Prop::ColorLabel, Some(Value::Text("1".into()))),
        (Prop::PickLabel, Some(Value::Text("3".into()))),
        (Prop::Pick, Some(Value::Text("pick".into()))),
        (
            Prop::Subject,
            Some(Value::Bag(vec!["Kyoto".into(), "temple".into()])),
        ),
        (
            Prop::HierarchicalSubject,
            Some(Value::Bag(vec![
                "Places|Japan|Kyoto".into(),
                "temple".into(),
            ])),
        ),
        (
            Prop::TagsList,
            Some(Value::Seq(vec![
                "Places/Japan/Kyoto".into(),
                "temple".into(),
            ])),
        ),
    ]
}

#[test]
fn real_sidecars_keep_every_property_we_dont_own() {
    let files = real_sidecars();
    if files.is_empty() || !exiftool_available() {
        eprintln!("skipping: no real sidecars or no exiftool");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    for src in files {
        let orig = std::fs::read(&src).unwrap();
        let out = xmp::edit(Some(&orig), &marks()).unwrap();
        let edited = t.path().join(src.file_name().unwrap());
        std::fs::write(&edited, &out).unwrap();

        let before = exiftool_tags(&src);
        let after = exiftool_tags(&edited);
        let keep = |m: &BTreeMap<String, Json>| -> BTreeMap<String, Json> {
            m.iter()
                .filter(|(k, _)| !owned(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        assert_eq!(
            keep(&before),
            keep(&after),
            "{}: a property we don't own changed",
            src.display()
        );
        assert!(
            before.keys().filter(|k| !owned(k)).count() > 10,
            "fixture should carry plenty of foreign data"
        );

        // ExifTool reads our values.
        assert_eq!(after["XMP-xmp:Rating"], Json::from(4));
        assert_eq!(after["XMP-xmp:Label"], Json::from("Red"));
        assert_eq!(after["XMP-digiKam:PickLabel"], Json::from(3));
        assert_eq!(
            after["XMP-dc:Subject"],
            serde_json::json!(["Kyoto", "temple"])
        );
        assert_eq!(
            after["XMP-lr:HierarchicalSubject"],
            serde_json::json!(["Places|Japan|Kyoto", "temple"])
        );
        assert_eq!(
            after["XMP-digiKam:TagsList"],
            serde_json::json!(["Places/Japan/Kyoto", "temple"])
        );

        // Byte-level: apart from removing the owned lines, the edit only adds
        // bytes. The rest of the original appears in the output, in order
        // (as a subsequence), byte for byte.
        let kept: Vec<u8> = String::from_utf8_lossy(&orig)
            .split_inclusive('\n')
            .filter(|l| !l.contains("xmp:Rating"))
            .collect::<String>()
            .into_bytes();
        let mut it = out.iter();
        assert!(
            kept.iter().all(|b| it.any(|x| x == b)),
            "{}: original bytes were changed, not just added to",
            src.display()
        );
        eprintln!(
            "{}: {} → {} bytes; {} foreign tags preserved",
            src.display(),
            orig.len(),
            out.len(),
            keep(&after).len()
        );
    }
}

/// The other direction: a sidecar written by another tool (ExifTool) is read.
#[test]
fn reads_sidecars_written_by_exiftool() {
    if !exiftool_available() {
        eprintln!("skipping: no exiftool");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let jpg = t.path().join("a.jpg");
    std::fs::write(
        &jpg,
        shoebox_fixtures::jpeg(&shoebox_fixtures::scene(1, 64, 48), 80).unwrap(),
    )
    .unwrap();
    let side = t.path().join("a.xmp");
    let out = Command::new("exiftool")
        .arg("-o")
        .arg(&side)
        .args([
            "-XMP-xmp:Rating=3",
            "-XMP-xmp:Label=Green",
            "-XMP-dc:Subject=one",
            "-XMP-dc:Subject=two & more",
            "-XMP-lr:HierarchicalSubject=Places|Italy|Rome",
            "-XMP-digiKam:PickLabel=1",
        ])
        .arg(&jpg)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = xmp::read(&std::fs::read(&side).unwrap()).unwrap();
    assert_eq!(v[&Prop::Rating].first().as_deref(), Some("3"));
    assert_eq!(v[&Prop::Label].first().as_deref(), Some("Green"));
    assert_eq!(v[&Prop::Subject].items(), vec!["one", "two & more"]);
    assert_eq!(
        v[&Prop::HierarchicalSubject].items(),
        vec!["Places|Italy|Rome"]
    );
    assert_eq!(v[&Prop::PickLabel].first().as_deref(), Some("1"));

    // And our edit of ExifTool's sidecar is still read correctly by ExifTool.
    let edited = xmp::edit(
        Some(&std::fs::read(&side).unwrap()),
        &[(Prop::Rating, Some(Value::Text("5".into())))],
    )
    .unwrap();
    std::fs::write(&side, edited).unwrap();
    let tags = exiftool_tags(&side);
    assert_eq!(tags["XMP-xmp:Rating"], Json::from(5));
    assert_eq!(tags["XMP-xmp:Label"], Json::from("Green"));
}
