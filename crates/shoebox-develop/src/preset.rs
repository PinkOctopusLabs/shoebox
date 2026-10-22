//! Presets (EDITING-SPEC, Auto mode, presets and packs): a partial edit
//! document, some of the settings' parts ([`Part`]) carried whole, applied
//! with an Amount (0–200%) that scales every value relative to its default.
//! The same machinery copies and pastes settings between photos.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::edit::Settings;

/// A group of settings that travels together (a preset carries whole
/// parts; copy and paste choose parts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    WhiteBalance,
    Exposure,
    Tone,
    Presence,
    ToneCurve,
    Colour,
    BlackAndWhite,
    Calibration,
    Detail,
    Lens,
    Transform,
    Crop,
    Raw,
    Locals,
    Spots,
}

impl Part {
    pub const ALL: [Part; 15] = [
        Part::WhiteBalance,
        Part::Exposure,
        Part::Tone,
        Part::Presence,
        Part::ToneCurve,
        Part::Colour,
        Part::BlackAndWhite,
        Part::Calibration,
        Part::Detail,
        Part::Lens,
        Part::Transform,
        Part::Crop,
        Part::Raw,
        Part::Locals,
        Part::Spots,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Part::WhiteBalance => "White balance",
            Part::Exposure => "Exposure",
            Part::Tone => "Tone",
            Part::Presence => "Presence",
            Part::ToneCurve => "Tone curve",
            Part::Colour => "Colour",
            Part::BlackAndWhite => "Black & white",
            Part::Calibration => "Calibration",
            Part::Detail => "Detail",
            Part::Lens => "Lens corrections",
            Part::Transform => "Transform",
            Part::Crop => "Crop & rotate",
            Part::Raw => "Raw",
            Part::Locals => "Local adjustments",
            Part::Spots => "Spot removal",
        }
    }

    /// Ticked when copying settings: geometry, raw handling, local
    /// adjustments and spots rarely carry over from one frame to another
    /// (EDITING-SPEC, Batch editing).
    pub fn copied_by_default(self) -> bool {
        !matches!(
            self,
            Part::Transform | Part::Crop | Part::Raw | Part::Locals | Part::Spots
        )
    }

    /// Where it lives in the settings' JSON.
    fn paths(self) -> &'static [&'static [&'static str]] {
        match self {
            Part::WhiteBalance => &[&["modules", "white_balance"]],
            Part::Exposure => &[&["modules", "exposure"]],
            Part::Tone => &[
                &["modules", "display_transform"],
                &["modules", "highlights_shadows"],
            ],
            Part::Presence => &[&["modules", "clarity"], &["modules", "dehaze"]],
            Part::ToneCurve => &[&["modules", "tone_curve"]],
            Part::Colour => &[
                &["modules", "colour"],
                &["modules", "point_colour"],
                &["modules", "input_colour"],
            ],
            Part::BlackAndWhite => &[&["modules", "black_and_white"]],
            Part::Calibration => &[&["modules", "calibration"]],
            Part::Detail => &[
                &["modules", "sharpening"],
                &["modules", "noise_reduction"],
                &["modules", "denoise_ai"],
            ],
            Part::Lens => &[&["modules", "lens"], &["modules", "defringe"]],
            Part::Transform => &[&["geometry", "transform"]],
            Part::Crop => &[
                &["geometry", "quarter"],
                &["geometry", "flip"],
                &["geometry", "rotate"],
                &["geometry", "crop"],
                &["geometry", "aspect"],
            ],
            Part::Raw => &[&["modules", "raw"]],
            Part::Locals => &[&["locals"]],
            Part::Spots => &[&["modules", "heal"]],
        }
    }
}

/// A named preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub favourite: bool,
    /// The parts it carries, each whole, as the settings' JSON has them:
    /// `{"modules": {"colour": {…}}, "geometry": {…}}`.
    pub settings: Value,
}

fn get<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |v, k| v.get(*k))
}

fn set(v: &mut Value, path: &[&str], x: Value) {
    let mut cur = v;
    for k in &path[..path.len() - 1] {
        if !cur.get(*k).is_some_and(Value::is_object) {
            cur[*k] = Value::Object(Default::default());
        }
        cur = &mut cur[*k];
    }
    cur[path[path.len() - 1]] = x;
}

/// Settings as full JSON (defaults included, unlike `Settings::to_value`;
/// no local adjustments is an empty list, so it travels too).
fn full(s: &Settings) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.entry("locals")
            .or_insert_with(|| Value::Array(Vec::new()));
    }
    v
}

impl Preset {
    /// A preset of `parts` of these settings.
    pub fn from_settings(name: &str, s: &Settings, parts: &[Part]) -> Self {
        let all = full(s);
        let mut settings = Value::Object(Default::default());
        for p in parts {
            for path in p.paths() {
                if let Some(v) = get(&all, path) {
                    set(&mut settings, path, v.clone());
                }
            }
        }
        Self {
            name: name.to_owned(),
            group: String::new(),
            favourite: false,
            settings,
        }
    }

    /// The parts it carries.
    pub fn parts(&self) -> Vec<Part> {
        Part::ALL
            .into_iter()
            .filter(|p| {
                p.paths()
                    .iter()
                    .any(|path| get(&self.settings, path).is_some())
            })
            .collect()
    }

    /// `s` with this preset applied at `amount` (1: as saved; 0: its parts
    /// at their defaults; 2: twice as far from them). Numbers scale from
    /// their defaults; anything else (on/off, modes, curve points) is the
    /// preset's from half the amount up, else the default.
    pub fn apply(&self, s: &Settings, amount: f64) -> Settings {
        let defaults = full(&Settings::default());
        let mut out = full(s);
        for part in self.parts() {
            for path in part.paths() {
                let (Some(p), Some(d)) = (get(&self.settings, path), get(&defaults, path)) else {
                    continue;
                };
                set(&mut out, path, blend(d, p, amount));
            }
        }
        serde_json::from_value(out).unwrap_or_else(|_| s.clone())
    }
}

fn blend(d: &Value, p: &Value, amount: f64) -> Value {
    match (d, p) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap_or(0.0), b.as_f64().unwrap_or(0.0));
            let v = a + (b - a) * amount;
            // Whole numbers stay whole (quarter turns, say).
            if p.is_i64() || p.is_u64() {
                serde_json::json!(v.round() as i64)
            } else {
                serde_json::json!(v)
            }
        }
        (Value::Object(dm), Value::Object(pm)) => {
            let mut out = dm.clone();
            for (k, pv) in pm {
                let v = match dm.get(k) {
                    Some(dv) => blend(dv, pv, amount),
                    None => {
                        if amount >= 0.5 {
                            pv.clone()
                        } else {
                            continue;
                        }
                    }
                };
                out.insert(k.clone(), v);
            }
            Value::Object(out)
        }
        (Value::Array(da), Value::Array(pa))
            if da.len() == pa.len() && pa.iter().all(Value::is_number) =>
        {
            Value::Array(
                da.iter()
                    .zip(pa)
                    .map(|(a, b)| blend(a, b, amount))
                    .collect(),
            )
        }
        _ => {
            if amount >= 0.5 {
                p.clone()
            } else {
                d.clone()
            }
        }
    }
}

/// Read a presets file (a JSON list); empty if it isn't there.
pub fn load(file: &std::path::Path) -> Vec<Preset> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Write a presets file whole (temporary file, then rename).
pub fn save(file: &std::path::Path, list: &[Preset]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
}
