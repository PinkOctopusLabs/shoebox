//! Presets: parts carried whole, the Amount blend, and copy/paste parts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::edit::{Settings, WbMode};
use shoebox_develop::preset::{Part, Preset};

fn edited() -> Settings {
    let mut s = Settings::default();
    s.modules.exposure.ev = 1.0;
    s.modules.colour.saturation = 40.0;
    s.modules.colour.hue[0] = 20.0;
    s.modules.white_balance.mode = WbMode::Daylight;
    s.modules.black_and_white.enabled = true;
    s.geometry.rotate = 2.0;
    s.geometry.quarter = 1;
    s
}

#[test]
fn a_preset_carries_whole_parts() {
    let p = Preset::from_settings("Warm", &edited(), &[Part::Colour, Part::Exposure]);
    assert_eq!(p.parts(), [Part::Exposure, Part::Colour]);
    // Applied to another photo: those parts replaced whole (vibrance back
    // to 0 there too), the rest untouched.
    let mut other = Settings::default();
    other.modules.colour.vibrance = 30.0;
    other.modules.clarity.amount = 25.0;
    let out = p.apply(&other, 1.0);
    assert_eq!(out.modules.exposure.ev, 1.0);
    assert_eq!(out.modules.colour.saturation, 40.0);
    assert_eq!(out.modules.colour.vibrance, 0.0);
    assert_eq!(out.modules.clarity.amount, 25.0);
    assert_eq!(out.modules.white_balance.mode, WbMode::AsShot);
    // It round-trips as JSON.
    let back: Preset = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
}

#[test]
fn amount_scales_from_the_defaults() {
    let all: Vec<Part> = Part::ALL.to_vec();
    let p = Preset::from_settings("Look", &edited(), &all);
    let half = p.apply(&Settings::default(), 0.5);
    assert!((half.modules.exposure.ev - 0.5).abs() < 1e-9);
    assert!((half.modules.colour.saturation - 20.0).abs() < 1e-9);
    assert!((half.modules.colour.hue[0] - 10.0).abs() < 1e-9);
    // Modes and switches: the preset's from half the amount.
    assert_eq!(half.modules.white_balance.mode, WbMode::Daylight);
    assert!(half.modules.black_and_white.enabled);
    let low = p.apply(&Settings::default(), 0.3);
    assert_eq!(low.modules.white_balance.mode, WbMode::AsShot);
    let double = p.apply(&Settings::default(), 2.0);
    assert!((double.modules.exposure.ev - 2.0).abs() < 1e-9);
    // Quarter turns stay whole.
    assert_eq!(p.apply(&Settings::default(), 1.0).geometry.quarter, 1);
    assert_eq!(p.apply(&Settings::default(), 0.0), Settings::default());
}

#[test]
fn copying_leaves_geometry_out_by_default() {
    let parts: Vec<Part> = Part::ALL
        .into_iter()
        .filter(|p| p.copied_by_default())
        .collect();
    let p = Preset::from_settings("copy", &edited(), &parts);
    let out = p.apply(&Settings::default(), 1.0);
    assert_eq!(out.geometry.rotate, 0.0);
    assert_eq!(out.modules.exposure.ev, 1.0);
}

#[test]
fn smart_lightings_spot_stays_with_its_photo() {
    let mut s = Settings::default();
    s.modules.smart_lighting.strength = 40.0;
    s.modules.smart_lighting.spot = Some([0.1, 0.1, 0.3, 0.3]);
    let p = Preset::from_settings("Lit", &s, &[Part::Tone]);
    assert_eq!(p.parts(), [Part::Tone]);
    // Another photo keeps its own box (or none); the strength comes over.
    let mut other = Settings::default();
    other.modules.smart_lighting.spot = Some([0.5, 0.5, 0.6, 0.6]);
    let out = p.apply(&other, 1.0);
    assert_eq!(out.modules.smart_lighting.strength, 40.0);
    assert_eq!(out.modules.smart_lighting.spot, Some([0.5, 0.5, 0.6, 0.6]));
    let out = p.apply(&Settings::default(), 1.0);
    assert_eq!(out.modules.smart_lighting.spot, None);
}
