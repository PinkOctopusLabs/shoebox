//! The edit document (`docs/EDITING-SPEC.md`, Edit document and storage):
//! one photo's edit as small versioned JSON. The image is always re-rendered
//! from the untouched original with these settings.
//!
//! - Settings hold only what differs from the defaults, which live here in
//!   code per process version, so a module added later never breaks an old
//!   edit. Modules this build doesn't know (written by a newer one) are kept
//!   as they are.
//! - Each change appends an RFC 6902 JSON Patch from the previous settings,
//!   so undo, redo and "jump to step 12" replay patches from the defaults.
//!   Snapshots are named points in that history.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The document format. A newer file than this build understands is refused
/// rather than half-read.
pub const SCHEMA: u32 = 1;

/// History longer than this, or a step with more operations, isn't
/// replayed (and is dropped on load): far beyond any real edit.
const MAX_STEPS: usize = 10_000;
const MAX_OPS: usize = 10_000;

/// The rendering behaviour. Any change to an algorithm that alters output
/// bumps it; older edits keep rendering the old way until updated.
pub const PROCESS_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("not an edit document: {0}")]
    Json(#[from] serde_json::Error),
    #[error("edit document schema {0} is newer than this build ({SCHEMA})")]
    Newer(u32),
}

/// Which file the edit belongs to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// Path relative to the library root, with `/` separators.
    pub path: String,
    /// BLAKE3 of the original's contents (hex), so an edit left behind by a
    /// move in Finder finds its photo again.
    pub blake3: String,
    /// The original's size in bytes: narrows the search for a moved photo
    /// before anything is hashed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
}

/// One of a photo's edits: "main" is the master; virtual copies have their
/// own ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Copy {
    pub id: String,
    pub name: String,
}

impl Default for Copy {
    fn default() -> Self {
        Self {
            id: "main".into(),
            name: "Master".into(),
        }
    }
}

/// How the raw data becomes RGB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Raw {
    pub demosaic: Demosaic,
    pub highlights: Highlights,
}

impl Default for Raw {
    fn default() -> Self {
        Self {
            demosaic: Demosaic::Rcd,
            highlights: Highlights::InpaintOpposed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Demosaic {
    Bilinear,
    Rcd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Highlights {
    /// Clipped channels filled from unclipped neighbours (darktable's
    /// default since 4.2).
    InpaintOpposed,
    /// Clip to white (cheap; the proxy's fallback).
    Clip,
}

/// White balance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhiteBalance {
    pub mode: WbMode,
    /// Kelvin, for [`WbMode::Custom`] (and shown for the others).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<f64>,
    /// Green–magenta, for [`WbMode::Custom`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint: Option<f64>,
}

impl Default for WhiteBalance {
    fn default() -> Self {
        Self {
            mode: WbMode::AsShot,
            temp: None,
            tint: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WbMode {
    AsShot,
    Daylight,
    Cloudy,
    Shade,
    Tungsten,
    Fluorescent,
    Flash,
    Custom,
}

/// The camera profile that takes camera RGB to the working space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputColour {
    pub profile: Profile,
}

impl Default for InputColour {
    fn default() -> Self {
        Self {
            profile: Profile::Matrix,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// The file's colour matrices, interpolated by white balance.
    Matrix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Exposure {
    pub enabled: bool,
    /// Stops, −5 to +5.
    pub ev: f64,
}

impl Default for Exposure {
    fn default() -> Self {
        Self {
            enabled: true,
            ev: 0.0,
        }
    }
}

/// The scene → display tone mapper; the sliders are offsets from its
/// default curve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayTransform {
    /// −100 to +100.
    pub contrast: f64,
    /// Stops above mid-grey that map to white, relative to the default.
    pub white_ev: f64,
    /// Stops below mid-grey that map to black, relative to the default.
    pub black_ev: f64,
    pub preserve_hue: bool,
}

impl Default for DisplayTransform {
    fn default() -> Self {
        Self {
            contrast: 0.0,
            white_ev: 0.0,
            black_ev: 0.0,
            preserve_hue: true,
        }
    }
}

/// The global modules. Phase 1 has these; unknown ones are carried along.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Modules {
    pub raw: Raw,
    pub white_balance: WhiteBalance,
    pub input_colour: InputColour,
    pub exposure: Exposure,
    pub display_transform: DisplayTransform,
    /// Modules from a newer build, kept untouched.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

/// Rotation and crop, in the upright image (EXIF orientation applied).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Geometry {
    /// Degrees, clockwise positive, about the image centre.
    pub rotate: f64,
    /// The kept area after rotation, as fractions of the frame:
    /// `[left, top, right, bottom]`.
    pub crop: [f64; 4],
    /// The crop's aspect lock: "free", "original" or "w:h".
    pub aspect: String,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            rotate: 0.0,
            crop: [0.0, 0.0, 1.0, 1.0],
            aspect: "free".into(),
        }
    }
}

/// Everything that decides how the photo renders: what history patches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub modules: Modules,
    pub geometry: Geometry,
    /// Local adjustment layers (Phase 3); kept as written until then.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<Value>,
}

impl Settings {
    /// The settings as JSON with defaults left out (`{}` for an untouched
    /// photo).
    pub fn to_value(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        let d = serde_json::to_value(Settings::default()).unwrap_or(Value::Null);
        prune(&mut v, &d);
        if v.is_null() {
            Value::Object(Default::default())
        } else {
            v
        }
    }

    /// Settings from JSON that leaves defaults out.
    pub fn from_value(v: &Value) -> Result<Self, EditError> {
        Ok(serde_json::from_value(v.clone())?)
    }

    pub fn is_default(&self) -> bool {
        *self == Settings::default()
    }
}

/// Remove from `v` whatever equals `default`, recursively; an object left
/// empty becomes null (and is then removed from its parent).
fn prune(v: &mut Value, default: &Value) {
    if v == default {
        *v = Value::Null;
        return;
    }
    if let (Value::Object(map), Value::Object(dmap)) = (&mut *v, default) {
        let keys: Vec<String> = map.keys().cloned().collect();
        for k in keys {
            if let (Some(child), Some(d)) = (map.get_mut(&k), dmap.get(&k)) {
                prune(child, d);
                if child.is_null() {
                    map.remove(&k);
                }
            }
        }
        if map.is_empty() {
            *v = Value::Null;
        }
    }
}

/// One undoable change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// When, in local time with its offset (RFC 3339).
    pub t: String,
    /// What the user did ("Exposure +0.70").
    pub label: String,
    /// From the settings before this step to after it.
    pub patch: json_patch::Patch,
}

/// A named point in history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub name: String,
    /// Steps applied at that point (0: the defaults).
    pub history_index: usize,
}

/// One photo's edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditDoc {
    pub schema: u32,
    pub process_version: u32,
    pub source: Source,
    #[serde(default)]
    pub copy: Copy,
    #[serde(flatten)]
    settings: Settings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<Step>,
    /// Steps applied, when some have been undone (absent: all of them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history_at: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshots: Vec<Snapshot>,
}

impl EditDoc {
    /// A new, untouched edit of `source`.
    pub fn new(source: Source) -> Self {
        Self {
            schema: SCHEMA,
            process_version: PROCESS_VERSION,
            source,
            copy: Copy::default(),
            settings: Settings::default(),
            history: Vec::new(),
            history_at: None,
            snapshots: Vec::new(),
        }
    }

    /// Read a document. History that doesn't replay to the stored settings
    /// (edited by hand, say) is dropped: the settings win.
    pub fn from_json(text: &str) -> Result<Self, EditError> {
        let raw: Value = serde_json::from_str(text)?;
        let schema = raw.get("schema").and_then(Value::as_u64).unwrap_or(0);
        if schema > u64::from(SCHEMA) {
            return Err(EditError::Newer(schema as u32));
        }
        let mut doc: EditDoc = serde_json::from_value(raw)?;
        if doc.history_at.is_some_and(|at| at > doc.history.len()) {
            doc.history_at = None;
        }
        if doc.replay(doc.at()).as_ref() != Some(&doc.settings) {
            tracing::warn!(
                "edit history of {} doesn't match its settings; dropped",
                doc.source.path
            );
            doc.history.clear();
            doc.history_at = None;
            doc.snapshots.clear();
        }
        Ok(doc)
    }

    /// The document as pretty JSON, defaults left out.
    pub fn to_json(&self) -> String {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let (Value::Object(map), Value::Object(s)) = (&mut v, self.settings.to_value()) {
            for k in ["modules", "geometry", "locals"] {
                map.remove(k);
            }
            map.extend(s);
        }
        serde_json::to_string_pretty(&v).unwrap_or_default()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// A hash of what decides the rendered image (process version and
    /// settings, not history or names): equal hashes render the same, so
    /// it keys rendered previews and tells a stale export.
    pub fn render_hash(&self) -> String {
        let v = serde_json::json!({
            "process_version": self.process_version,
            "settings": self.settings.to_value(),
        });
        // serde_json's maps are ordered (BTreeMap), so this is canonical.
        let bytes = serde_json::to_vec(&v).unwrap_or_default();
        blake3::hash(&bytes).to_hex().to_string()
    }

    /// Steps applied now.
    pub fn at(&self) -> usize {
        self.history_at.unwrap_or(self.history.len())
    }

    pub fn history(&self) -> &[Step] {
        &self.history
    }

    pub fn can_undo(&self) -> bool {
        self.at() > 0
    }

    pub fn can_redo(&self) -> bool {
        self.at() < self.history.len()
    }

    /// The settings after the first `n` steps. Only `add`, `remove`,
    /// `replace` and `test` are replayed (what [`json_patch::diff`] makes):
    /// `copy` can double the document each step, so a crafted file could
    /// exhaust memory. With those, the result is no bigger than the file.
    fn replay(&self, n: usize) -> Option<Settings> {
        use json_patch::PatchOperation as Op;
        if n > MAX_STEPS {
            return None;
        }
        let mut v = Settings::default().to_value();
        for step in self.history.get(..n)? {
            if step.patch.0.len() > MAX_OPS
                || step
                    .patch
                    .0
                    .iter()
                    .any(|op| matches!(op, Op::Copy(_) | Op::Move(_)))
            {
                return None;
            }
            json_patch::patch(&mut v, &step.patch).ok()?;
        }
        Settings::from_value(&v).ok()
    }

    /// Change the settings as one undoable step labelled `label`. Steps
    /// that were undone are discarded. Returns false if nothing changed.
    pub fn change(&mut self, label: &str, f: impl FnOnce(&mut Settings)) -> bool {
        let before = self.settings.to_value();
        let mut next = self.settings.clone();
        f(&mut next);
        let after = next.to_value();
        let patch = json_patch::diff(&before, &after);
        if patch.0.is_empty() {
            return false;
        }
        let at = self.at();
        self.history.truncate(at);
        self.history_at = None;
        self.snapshots.retain(|s| s.history_index <= at);
        self.history.push(Step {
            t: now(),
            label: label.to_owned(),
            patch,
        });
        self.settings = next;
        true
    }

    /// Fold a change into the last step (a slider drag is one step), and
    /// relabel it. With no step to fold into, or after an undo, it's a new
    /// step.
    pub fn amend(&mut self, label: &str, f: impl FnOnce(&mut Settings)) -> bool {
        if !self.can_undo() || self.can_redo() {
            return self.change(label, f);
        }
        let n = self.history.len();
        let Some(base) = self.replay(n - 1) else {
            return self.change(label, f);
        };
        let mut next = self.settings.clone();
        f(&mut next);
        let patch = json_patch::diff(&base.to_value(), &next.to_value());
        if patch.0.is_empty() {
            // Back where the step started: it undoes itself.
            self.history.pop();
            self.snapshots.retain(|s| s.history_index < n);
        } else if let Some(last) = self.history.last_mut() {
            last.t = now();
            last.label = label.to_owned();
            last.patch = patch;
        }
        self.settings = next;
        true
    }

    /// Go to the point after `n` steps (0: the defaults). False if there's
    /// no such step.
    pub fn jump(&mut self, n: usize) -> bool {
        if n > self.history.len() {
            return false;
        }
        let Some(s) = self.replay(n) else {
            return false;
        };
        self.settings = s;
        self.history_at = (n < self.history.len()).then_some(n);
        true
    }

    pub fn undo(&mut self) -> bool {
        self.can_undo() && self.jump(self.at() - 1)
    }

    pub fn redo(&mut self) -> bool {
        self.can_redo() && self.jump(self.at() + 1)
    }

    /// Name the current point in history.
    pub fn snapshot(&mut self, name: &str) {
        self.snapshots.push(Snapshot {
            name: name.to_owned(),
            history_index: self.at(),
        });
    }
}

fn now() -> String {
    jiff::Zoned::now()
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}
