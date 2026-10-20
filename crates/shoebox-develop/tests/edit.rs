//! The edit document: defaults left out, unknown modules kept, history.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use shoebox_develop::edit::{EditDoc, EditError, SCHEMA, Settings, Source, WbMode};

fn doc() -> EditDoc {
    EditDoc::new(Source {
        path: "2026/2026-10/DSC_1234.NEF".into(),
        blake3: "9f2c".into(),
        size: None,
        camera: Some("NIKON Z f".into()),
        lens: None,
    })
}

fn parsed(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

#[test]
fn an_untouched_edit_stores_no_settings() {
    let d = doc();
    let v = parsed(&d.to_json());
    assert_eq!(
        v,
        json!({
            "schema": 1,
            "process_version": 1,
            "source": { "path": "2026/2026-10/DSC_1234.NEF", "blake3": "9f2c", "camera": "NIKON Z f" },
            "copy": { "id": "main", "name": "Master" }
        })
    );
    assert!(d.settings().is_default());
    assert_eq!(EditDoc::from_json(&d.to_json()).unwrap(), d);
}

#[test]
fn only_changed_values_are_written() {
    let mut d = doc();
    assert!(d.change("Exposure +0.70", |s| s.modules.exposure.ev = 0.7));
    assert!(d.change("Crop", |s| s.geometry.crop = [0.0, 0.0, 1.0, 0.5]));
    let v = parsed(&d.to_json());
    assert_eq!(v["modules"], json!({ "exposure": { "ev": 0.7 } }));
    assert_eq!(v["geometry"], json!({ "crop": [0.0, 0.0, 1.0, 0.5] }));
    assert_eq!(v["history"].as_array().unwrap().len(), 2);
    assert_eq!(v["history"][0]["label"], "Exposure +0.70");
    let back = EditDoc::from_json(&d.to_json()).unwrap();
    assert_eq!(back, d);
    assert_eq!(back.render_hash(), d.render_hash());
}

#[test]
fn a_change_that_changes_nothing_is_no_step() {
    let mut d = doc();
    assert!(!d.change("Nothing", |s| s.modules.exposure.ev = 0.0));
    assert!(!d.can_undo());
}

#[test]
fn undo_redo_jump_and_branching() {
    let mut d = doc();
    d.change("A", |s| s.modules.exposure.ev = 1.0);
    d.change("B", |s| s.modules.white_balance.mode = WbMode::Daylight);
    d.change("C", |s| s.geometry.rotate = -0.8);
    let hash_c = d.render_hash();
    assert!(d.undo());
    assert_eq!(d.settings().geometry.rotate, 0.0);
    assert_eq!(d.settings().modules.white_balance.mode, WbMode::Daylight);
    assert!(d.undo() && d.undo() && !d.undo());
    assert!(d.settings().is_default());
    assert_eq!(d.render_hash(), doc().render_hash());
    assert!(d.jump(3));
    assert_eq!(d.render_hash(), hash_c);
    assert!(!d.redo());

    // Undo twice, then a new change drops B and C.
    d.jump(1);
    d.snapshot("after A");
    let saved = EditDoc::from_json(&d.to_json()).unwrap();
    assert_eq!(saved.at(), 1);
    assert!(saved.can_redo());
    assert_eq!(saved.settings(), d.settings());
    d.change("D", |s| s.modules.exposure.ev = 2.0);
    let labels: Vec<_> = d.history().iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["A", "D"]);
    assert!(!d.can_redo());
    assert_eq!(d.snapshots.len(), 1);
}

#[test]
fn a_drag_is_one_step() {
    let mut d = doc();
    d.change("Exposure", |s| s.modules.exposure.ev = 0.1);
    for ev in [0.2, 0.3, 0.5] {
        d.amend(&format!("Exposure {ev:+.2}"), |s| {
            s.modules.exposure.ev = ev
        });
    }
    assert_eq!(d.history().len(), 1);
    assert_eq!(d.history()[0].label, "Exposure +0.50");
    assert!(d.undo());
    assert!(d.settings().is_default());
    assert!(d.redo());
    assert_eq!(d.settings().modules.exposure.ev, 0.5);
    // Dragged back to where it started: the step goes away.
    d.amend("Exposure", |s| s.modules.exposure.ev = 0.0);
    assert!(d.history().is_empty());
}

#[test]
fn modules_from_a_newer_build_are_kept() {
    let text = json!({
        "schema": 1,
        "process_version": 1,
        "source": { "path": "a.NEF", "blake3": "00" },
        "modules": {
            "exposure": { "ev": 0.5 },
            "dehaze": { "amount": 20 }
        },
        "geometry": { "rotate": 1.5 }
    })
    .to_string();
    let mut d = EditDoc::from_json(&text).unwrap();
    assert_eq!(d.settings().modules.exposure.ev, 0.5);
    assert!(d.settings().modules.exposure.enabled);
    assert_eq!(d.settings().geometry.crop, [0.0, 0.0, 1.0, 1.0]);
    d.change("More", |s| s.modules.exposure.ev = 1.0);
    let v = parsed(&d.to_json());
    assert_eq!(v["modules"]["dehaze"], json!({ "amount": 20 }));
    assert_eq!(v["copy"]["id"], "main");
}

#[test]
fn newer_schemas_and_junk_are_refused() {
    let newer = json!({ "schema": SCHEMA + 1, "process_version": 9, "source": {} }).to_string();
    assert!(matches!(
        EditDoc::from_json(&newer),
        Err(EditError::Newer(_))
    ));
    assert!(EditDoc::from_json("{").is_err());
}

#[test]
fn history_that_disagrees_with_the_settings_is_dropped() {
    let mut d = doc();
    d.change("A", |s| s.modules.exposure.ev = 1.0);
    let mut v = parsed(&d.to_json());
    v["modules"]["exposure"]["ev"] = json!(3.0);
    let back = EditDoc::from_json(&v.to_string()).unwrap();
    assert_eq!(back.settings().modules.exposure.ev, 3.0);
    assert!(back.history().is_empty());
}

#[test]
fn render_hash_ignores_history_and_names() {
    let mut a = doc();
    a.change("one", |s| s.modules.exposure.ev = 1.0);
    let mut b = doc();
    b.change("x", |s| s.modules.exposure.ev = 0.5);
    b.change("y", |s| s.modules.exposure.ev = 1.0);
    b.copy.name = "Other".into();
    assert_eq!(a.render_hash(), b.render_hash());
    b.change("z", |s| s.geometry.aspect = "3:2".into());
    assert_ne!(a.render_hash(), b.render_hash());
    assert_eq!(Settings::default().to_value(), json!({}));
}

#[test]
fn copy_operations_are_not_replayed() {
    // Each step would double the document; replay refuses, history goes.
    let mut d = doc();
    d.change("A", |s| s.modules.exposure.ev = 1.0);
    let mut v = parsed(&d.to_json());
    let bomb = json!([{ "op": "copy", "from": "/modules", "path": "/modules/x" }]);
    let mut steps = v["history"].as_array().unwrap().clone();
    for _ in 0..64 {
        steps.push(json!({ "t": "", "label": "x", "patch": bomb }));
    }
    v["history"] = json!(steps);
    let back = EditDoc::from_json(&v.to_string()).unwrap();
    assert!(back.history().is_empty());
    assert_eq!(back.settings().modules.exposure.ev, 1.0);
}
