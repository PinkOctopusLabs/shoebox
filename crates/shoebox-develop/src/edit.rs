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
    /// Fix hot and dead photosites before demosaicing.
    pub hot_pixels: bool,
    /// Raw black point, a fraction of the range to move it by (−0.05 to
    /// +0.05: lifts or deepens the blacks the camera recorded).
    pub black: f64,
    /// Raw white point as a fraction of the camera's (0.5–1.2): below 1,
    /// highlights clip sooner and come out brighter.
    pub white: f64,
}

impl Default for Raw {
    fn default() -> Self {
        Self {
            demosaic: Demosaic::Rcd,
            highlights: Highlights::InpaintOpposed,
            hot_pixels: false,
            black: 0.0,
            white: 1.0,
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
    /// The matrices, then a look fitted to the camera's own JPEGs (see
    /// `profile`); the matrix alone when there's no profile for the camera.
    CameraMatch,
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

/// Lens corrections (module 1): distortion and vignetting from the camera's
/// own data or Lensfun, chromatic aberration from Lensfun.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensCorrection {
    pub enabled: bool,
    /// How much of the distortion and vignetting corrections, 0–200 (%).
    pub distortion: f64,
    pub vignetting: f64,
    pub chromatic: bool,
    /// A Lensfun lens chosen by hand (its model name); None: matched from
    /// the photo's EXIF.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
}

impl Default for LensCorrection {
    fn default() -> Self {
        Self {
            enabled: false,
            distortion: 100.0,
            vignetting: 100.0,
            chromatic: true,
            lens: None,
        }
    }
}

/// Defringe (Tools beyond PhotoLab): purple and green fringes along
/// high-contrast edges lose their colour, 0–100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defringe {
    pub purple: f64,
    pub green: f64,
}

/// Highlights and shadows (module 7): −100 to +100, applied through an
/// edge-aware base layer so detail keeps its contrast.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HighlightsShadows {
    pub highlights: f64,
    pub shadows: f64,
}

/// Smart Lighting (module 6): local tone compression, 0–100, and a bias
/// from −100 (tame highlights more) to +100 (lift shadows more).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartLighting {
    pub strength: f64,
    pub bias: f64,
    /// Spot weighted: a box `[x0, y0, x1, y1]` whose tone it favours (a
    /// face, say), in the stored image (before the file's orientation, the
    /// edit's turns and crop), in units of its long edge. None: uniform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spot: Option<[f64; 4]>,
}

/// Clarity and texture (module 8): local contrast at a coarse scale
/// (midtones) and a fine one, −100 to +100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clarity {
    pub amount: f64,
    pub texture: f64,
}

/// Sharpening (module 13): amount 0–150, radius 0.5–3 px (of the
/// original), detail 0–100 (how much halo is allowed), masking 0–100
/// (sharpen edges only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharpening {
    pub amount: f64,
    pub radius: f64,
    pub detail: f64,
    pub masking: f64,
}

impl Default for Sharpening {
    fn default() -> Self {
        Self {
            amount: 0.0,
            radius: 1.0,
            detail: 25.0,
            masking: 0.0,
        }
    }
}

/// Classic noise reduction: luminance and colour strengths 0–100, and how
/// much luminance detail to keep (0–100).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoiseReduction {
    pub luminance: f64,
    pub chroma: f64,
    pub detail: f64,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        Self {
            luminance: 0.0,
            chroma: 0.0,
            detail: 50.0,
        }
    }
}

/// AI raw denoise (tier 2): the model's denoised mosaic in place of RCD's
/// demosaic, blended with it by `strength` (0–100). Raw photos only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DenoiseAi {
    pub enabled: bool,
    pub strength: f64,
}

impl Default for DenoiseAi {
    fn default() -> Self {
        Self {
            enabled: false,
            strength: 100.0,
        }
    }
}

/// Dehaze (module 5): −100 (add haze) to +100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dehaze {
    pub amount: f64,
}

/// The eight hue bands of HSL and the black-and-white mix, in order.
pub const HUES: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];

/// Calibration: the working space's primaries nudged (hue and saturation
/// of red, green and blue) and a green–magenta tint in the shadows. All
/// −100 to +100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calibration {
    pub shadows_tint: f64,
    pub red_hue: f64,
    pub red_saturation: f64,
    pub green_hue: f64,
    pub green_saturation: f64,
    pub blue_hue: f64,
    pub blue_saturation: f64,
}

/// Vibrance, saturation, and hue, saturation and luminance per hue band
/// ([`HUES`]). All −100 to +100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Colour {
    pub vibrance: f64,
    pub saturation: f64,
    pub hue: [f64; 8],
    pub hsl_saturation: [f64; 8],
    pub luminance: [f64; 8],
}

/// Spot removal (EDITING-SPEC, Healing): spots healed or cloned from
/// elsewhere in the photo, in order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Heal {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub spots: Vec<Spot>,
}

/// Heal blends the copied texture into its surroundings (matching their
/// brightness and colour); clone copies it as it is. Red eye takes the red
/// out of the pupil found in the circle; pet eye darkens the bright
/// reflection of an animal's pupil. The eyes have no source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotKind {
    #[default]
    Heal,
    Clone,
    RedEye,
    PetEye,
}

impl SpotKind {
    pub fn is_eye(self) -> bool {
        matches!(self, SpotKind::RedEye | SpotKind::PetEye)
    }
}

/// One spot: a circle at `dest` filled from the same circle at `source`.
/// Positions and radius are in the stored image, in units of its long edge
/// (as local adjustment masks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spot {
    pub kind: SpotKind,
    pub dest: [f64; 2],
    pub source: [f64; 2],
    pub radius: f64,
    /// The soft edge, 0–1 of the radius.
    pub feather: f64,
    /// 0–100.
    pub opacity: f64,
    /// Eyes: how dark the pupil turns, 0–100.
    pub darken: f64,
}

impl Default for Spot {
    fn default() -> Self {
        Self {
            kind: SpotKind::Heal,
            dest: [0.5, 0.33],
            source: [0.6, 0.33],
            radius: 0.01,
            feather: 0.3,
            opacity: 100.0,
            darken: 50.0,
        }
    }
}

/// Point Color (Tools beyond PhotoLab): colours picked on the photo, each
/// with its hue, saturation and luminance shifted wherever that colour is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointColour {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<ColourPoint>,
}

/// The most points Point Color takes.
pub const COLOUR_POINTS: usize = 8;

/// One picked colour and what's done to it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColourPoint {
    /// The colour picked, in Oklab: lightness, chroma, hue (degrees).
    pub lightness: f64,
    pub chroma: f64,
    pub hue: f64,
    /// Hue (−100 to +100: up to 30° either way), saturation and luminance
    /// (−100 to +100).
    pub hue_shift: f64,
    pub saturation: f64,
    pub luminance: f64,
    /// How far from the picked colour it reaches in hue, saturation and
    /// luminance, 0–100.
    pub hue_range: f64,
    pub saturation_range: f64,
    pub luminance_range: f64,
}

impl Default for ColourPoint {
    fn default() -> Self {
        Self {
            lightness: 0.5,
            chroma: 0.1,
            hue: 30.0,
            hue_shift: 0.0,
            saturation: 0.0,
            luminance: 0.0,
            hue_range: 50.0,
            saturation_range: 50.0,
            luminance_range: 50.0,
        }
    }
}

/// Colour grading (module 10): a tint and brightness for the shadows,
/// midtones, highlights and the whole photo, how far the three ranges
/// overlap (blending, 0–100) and where they meet (balance, −100 to +100:
/// towards the shadows or the highlights).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColourGrading {
    pub shadows: Wheel,
    pub midtones: Wheel,
    pub highlights: Wheel,
    pub global: Wheel,
    pub blending: f64,
    pub balance: f64,
}

impl Default for ColourGrading {
    fn default() -> Self {
        Self {
            shadows: Wheel::default(),
            midtones: Wheel::default(),
            highlights: Wheel::default(),
            global: Wheel::default(),
            blending: 50.0,
            balance: 0.0,
        }
    }
}

/// One colour wheel: hue (degrees, Oklab), saturation (0–100) and
/// luminance (−100 to +100).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wheel {
    pub hue: f64,
    pub saturation: f64,
    pub luminance: f64,
}

/// Effects (module 15): a post-crop vignette and film grain, on the
/// photo as shown.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effects {
    pub vignette: Vignette,
    pub grain: Grain,
}

/// Amount −100 (darker edges) to +100 (lighter); midpoint (where it's
/// half, 0–100 from the middle to the corners), roundness (−100 the
/// frame's shape, +100 a circle), feather (0–100) and highlights (0–100:
/// how much bright edges keep their brightness when darkened).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vignette {
    pub amount: f64,
    pub midpoint: f64,
    pub roundness: f64,
    pub feather: f64,
    pub highlights: f64,
}

impl Default for Vignette {
    fn default() -> Self {
        Self {
            amount: 0.0,
            midpoint: 50.0,
            roundness: 0.0,
            feather: 50.0,
            highlights: 0.0,
        }
    }
}

/// Amount 0–100; size 0–100 (grains from about one to six pixels of the
/// full-size photo); roughness 0–100 (finer grains mixed in).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Grain {
    pub amount: f64,
    pub size: f64,
    pub roughness: f64,
}

impl Default for Grain {
    fn default() -> Self {
        Self {
            amount: 0.0,
            size: 25.0,
            roughness: 50.0,
        }
    }
}

/// A monochrome treatment: how bright each hue band ([`HUES`]) turns out,
/// −100 to +100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlackAndWhite {
    pub enabled: bool,
    pub mix: [f64; 8],
}

/// The tone curve: a parametric curve (four regions between three
/// splits), then point curves for all channels and for red, green and
/// blue. On display values (0–1), as the photo is shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToneCurve {
    /// Shadows, darks, lights, highlights: −100 to +100.
    pub regions: [f64; 4],
    /// Where the regions meet, 0–1, increasing.
    pub splits: [f64; 3],
    /// Points `[x, y]` (0–1) besides the ends (0, 0) and (1, 1).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rgb: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub red: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub green: Vec<[f64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blue: Vec<[f64; 2]>,
}

impl Default for ToneCurve {
    fn default() -> Self {
        Self {
            regions: [0.0; 4],
            splits: [0.25, 0.5, 0.75],
            rgb: Vec::new(),
            red: Vec::new(),
            green: Vec::new(),
            blue: Vec::new(),
        }
    }
}

impl ToneCurve {
    pub fn is_identity(&self) -> bool {
        self.regions.iter().all(|&r| r == 0.0)
            && self.rgb.is_empty()
            && self.red.is_empty()
            && self.green.is_empty()
            && self.blue.is_empty()
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
    pub lens: LensCorrection,
    pub defringe: Defringe,
    pub heal: Heal,
    pub noise_reduction: NoiseReduction,
    pub denoise_ai: DenoiseAi,
    pub sharpening: Sharpening,
    pub dehaze: Dehaze,
    pub highlights_shadows: HighlightsShadows,
    pub smart_lighting: SmartLighting,
    pub clarity: Clarity,
    pub calibration: Calibration,
    pub colour: Colour,
    pub point_colour: PointColour,
    pub colour_grading: ColourGrading,
    pub black_and_white: BlackAndWhite,
    pub tone_curve: ToneCurve,
    pub display_transform: DisplayTransform,
    pub effects: Effects,
    /// Modules from a newer build, kept untouched.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

/// Rotation and crop, in the upright image (EXIF orientation applied).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Geometry {
    /// Quarter turns clockwise (0–3), after the file's own orientation.
    pub quarter: u8,
    /// Mirrored left to right (after the quarter turns).
    pub flip: bool,
    /// Degrees, clockwise positive, about the image centre.
    pub rotate: f64,
    /// The kept area after rotation, as fractions of the frame:
    /// `[left, top, right, bottom]`.
    pub crop: [f64; 4],
    /// The crop's aspect lock: "free", "original" or "w:h".
    pub aspect: String,
    /// Perspective and framing, before rotation and crop.
    pub transform: Transform,
    /// Guided upright's lines (two to four), each end as fractions of the
    /// upright frame before rotation and perspective: `[[x, y], [x, y]]`.
    /// Kept so they can be shown and moved again; the rotation and
    /// perspective they gave are stored as ever.
    pub guides: Vec<[[f64; 2]; 2]>,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            quarter: 0,
            flip: false,
            rotate: 0.0,
            crop: [0.0, 0.0, 1.0, 1.0],
            aspect: "free".into(),
            transform: Transform::default(),
            guides: Vec::new(),
        }
    }
}

/// The manual transform (Tools beyond PhotoLab): vertical and horizontal
/// perspective (−100 to +100, a camera tilted or panned up to 30°), aspect
/// (−100 to +100: narrower to wider), scale (50–150%) and offsets (−100 to
/// +100, a quarter of the frame), in the upright frame. With `constrain`,
/// it zooms in just enough that no blank edge shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    pub vertical: f64,
    pub horizontal: f64,
    pub aspect: f64,
    pub scale: f64,
    pub offset_x: f64,
    pub offset_y: f64,
    pub constrain: bool,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            vertical: 0.0,
            horizontal: 0.0,
            aspect: 0.0,
            scale: 100.0,
            offset_x: 0.0,
            offset_y: 0.0,
            constrain: true,
        }
    }
}

impl Transform {
    pub fn is_identity(&self) -> bool {
        let d = Self::default();
        self.vertical == 0.0
            && self.horizontal == 0.0
            && self.aspect == 0.0
            && self.scale == d.scale
            && self.offset_x == 0.0
            && self.offset_y == 0.0
    }
}

impl Geometry {
    /// A quarter turn clockwise (or anticlockwise), the crop turning with
    /// the frame.
    pub fn turn(&mut self, clockwise: bool) {
        let [l, t, r, b] = self.crop;
        if clockwise {
            self.quarter = (self.quarter + 1) % 4;
            self.crop = [1.0 - b, l, 1.0 - t, r];
        } else {
            self.quarter = (self.quarter + 3) % 4;
            self.crop = [t, 1.0 - r, b, 1.0 - l];
        }
        if self.flip {
            // A turn after a mirror is the other way round.
            self.quarter = (self.quarter + 2) % 4;
        }
        self.aspect = flip_aspect(&self.aspect);
        // The transform turns with the frame: what was vertical is now
        // horizontal.
        let t = &mut self.transform;
        let (v, h, x, y) = (t.vertical, t.horizontal, t.offset_x, t.offset_y);
        if clockwise {
            (t.vertical, t.horizontal, t.offset_x, t.offset_y) = (h, -v, -y, x);
        } else {
            (t.vertical, t.horizontal, t.offset_x, t.offset_y) = (-h, v, y, -x);
        }
        t.aspect = -t.aspect;
    }

    /// Mirror left to right, the crop mirroring with it.
    pub fn mirror(&mut self) {
        let [l, t, r, b] = self.crop;
        self.flip = !self.flip;
        self.crop = [1.0 - r, t, 1.0 - l, b];
        self.transform.horizontal = -self.transform.horizontal;
        self.transform.offset_x = -self.transform.offset_x;
    }
}

/// "3:2" ↔ "2:3" (an aspect turned a quarter).
fn flip_aspect(a: &str) -> String {
    match a.split_once(':') {
        Some((w, h)) => format!("{h}:{w}"),
        None => a.to_owned(),
    }
}

/// Everything that decides how the photo renders: what history patches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub modules: Modules,
    pub geometry: Geometry,
    /// Local adjustment layers, applied in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<Local>,
}

/// A local adjustment (EDITING-SPEC, Local adjustments): a mask and the
/// slider values applied through it, in linear light before the colour
/// stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Local {
    /// Unique within the edit ("l1", "l2", …).
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// How much of the adjustment applies, 0–100.
    pub amount: f64,
    pub mask: Mask,
    pub adjust: LocalAdjust,
}

impl Default for Local {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            amount: 100.0,
            mask: Mask::default(),
            adjust: LocalAdjust::default(),
        }
    }
}

/// A mask: its parts, combined in order, each by its own [`Op`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    pub parts: Vec<Part>,
}

/// One part of a mask. A part this build doesn't know (written by a newer
/// one) is kept as it was and leaves the mask alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    Known(MaskPart),
    Other(Value),
}

/// How a part combines with the parts before it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    #[default]
    Add,
    Subtract,
    Intersect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskPart {
    #[serde(default)]
    pub op: Op,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
    #[serde(flatten)]
    pub shape: Shape,
}

/// What a part covers. Positions are in the stored image (before the
/// file's orientation, the edit's turns and crop), in units of its long
/// edge from the top left, so masks stay on what they cover whatever the
/// framing, and mean the same on the proxy and the full image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shape {
    /// Full effect at `from`, fading to none at `to`.
    Linear { from: [f64; 2], to: [f64; 2] },
    /// An ellipse: full effect inside, fading over `feather` (0–1 of the
    /// radius) to none at the edge. `angle` turns it (degrees, clockwise).
    Radial {
        centre: [f64; 2],
        radii: [f64; 2],
        #[serde(default)]
        angle: f64,
        #[serde(default = "half")]
        feather: f64,
    },
    /// Painted strokes.
    Brush {
        #[serde(default)]
        strokes: Vec<Stroke>,
    },
    /// Pixels by their brightness as shown (0–1), full effect inside
    /// `range`, fading over `smooth` either side.
    Luminance {
        range: [f64; 2],
        #[serde(default = "tenth")]
        smooth: f64,
    },
    /// Pixels by their colour: hue (degrees, Oklab) within `width` either
    /// side of the one picked, fading over as much again; greys never.
    Colour {
        hue: f64,
        #[serde(default = "twenty")]
        width: f64,
    },
    /// The photo's subject (foreground objects), found by Apple's Vision.
    Subject,
    /// People (with hair and clothes), found by Apple's Vision.
    Person,
    /// The object at these clicks, found by SAM 2.1: clicks that add it,
    /// and clicks that take parts away.
    Object {
        #[serde(default)]
        clicks: Vec<ObjectClick>,
    },
    /// A part of every face, from Apple's Vision's face landmarks.
    Face { part: FacePart },
}

/// Parts of faces a mask can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacePart {
    /// The face less its eyes, eyebrows and lips.
    Skin,
    Eyes,
    Eyebrows,
    Lips,
    Teeth,
}

/// A click for an object part: where (as other positions) and whether it
/// adds (else takes away).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ObjectClick {
    pub at: [f64; 2],
    #[serde(default = "yes")]
    pub add: bool,
}

fn yes() -> bool {
    true
}

fn half() -> f64 {
    0.5
}

fn tenth() -> f64 {
    0.1
}

fn twenty() -> f64 {
    20.0
}

/// One brush stroke: a path, its radius (the long edge's units), feather
/// (0–1 of the radius) and flow (0–1: how much one stroke paints).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub points: Vec<[f64; 2]>,
    pub radius: f64,
    #[serde(default = "half")]
    pub feather: f64,
    #[serde(default = "one")]
    pub flow: f64,
    /// Erases what earlier strokes painted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub erase: bool,
}

fn one() -> f64 {
    1.0
}

/// A local adjustment's sliders: exposure in stops (−4 to +4), the rest
/// −100 to +100.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalAdjust {
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub temperature: f64,
    pub tint: f64,
    pub saturation: f64,
    pub clarity: f64,
    pub dehaze: f64,
    /// Sharpening (+) or softening (−) of fine detail.
    pub sharpness: f64,
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

/// The settings that decide the rendered image: all but Guided upright's
/// guides (the rotation and perspective they gave are what's rendered).
fn rendered(s: &Settings) -> Value {
    let mut v = s.to_value();
    let empty = match v.get_mut("geometry") {
        Some(Value::Object(g)) => {
            g.remove("guides");
            g.is_empty()
        }
        _ => false,
    };
    if empty && let Value::Object(map) = &mut v {
        map.remove("geometry");
    }
    v
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
    /// Its settings, once the steps it pointed to were discarded (a change
    /// after an undo): a snapshot outlives the history it was taken in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
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

    /// A virtual copy of this edit: the same settings, its own id and name,
    /// a history of its own. That history starts with one step from the
    /// defaults to the settings copied (history replays from the defaults,
    /// so without it a reopened copy's history wouldn't match its settings).
    pub fn copy_of(&self, id: &str, name: &str) -> Self {
        let patch = json_patch::diff(&Settings::default().to_value(), &self.settings.to_value());
        let from = if self.copy.name.is_empty() {
            "Master"
        } else {
            &self.copy.name
        };
        let history = if patch.0.is_empty() {
            Vec::new()
        } else {
            vec![Step {
                t: now(),
                label: format!("Copied from {from}"),
                patch,
            }]
        };
        Self {
            copy: Copy {
                id: id.to_owned(),
                name: name.to_owned(),
            },
            history,
            history_at: None,
            snapshots: Vec::new(),
            ..self.clone()
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
            doc.snapshots.retain(|s| s.settings.is_some());
        }
        let n = doc.history.len();
        doc.snapshots
            .retain(|s| s.settings.is_some() || s.history_index <= n);
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
            "settings": rendered(&self.settings),
        });
        // serde_json's maps are ordered (BTreeMap), so this is canonical.
        let bytes = serde_json::to_vec(&v).unwrap_or_default();
        blake3::hash(&bytes).to_hex().to_string()
    }

    /// [`Self::render_hash`] for settings outside a document.
    pub fn settings_hash(s: &Settings) -> String {
        let v = serde_json::json!({
            "process_version": PROCESS_VERSION,
            "settings": rendered(s),
        });
        blake3::hash(&serde_json::to_vec(&v).unwrap_or_default())
            .to_hex()
            .to_string()
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
        self.detach_snapshots(at);
        self.history.truncate(at);
        self.history_at = None;
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
        let n = self.history.len();
        // A snapshot of the step keeps it as it was.
        if !self.can_undo()
            || self.can_redo()
            || self
                .snapshots
                .iter()
                .any(|s| s.settings.is_none() && s.history_index == n)
        {
            return self.change(label, f);
        }
        let Some(base) = self.replay(n - 1) else {
            return self.change(label, f);
        };
        let mut next = self.settings.clone();
        f(&mut next);
        let patch = json_patch::diff(&base.to_value(), &next.to_value());
        if patch.0.is_empty() {
            // Back where the step started: it undoes itself.
            self.history.pop();
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
            settings: None,
        });
    }

    /// Snapshots pointing past step `keep` hold their settings from now on,
    /// so discarding those steps doesn't lose them.
    fn detach_snapshots(&mut self, keep: usize) {
        let mut held = std::mem::take(&mut self.snapshots);
        for s in &mut held {
            if s.settings.is_none() && s.history_index > keep {
                s.settings = self.replay(s.history_index).map(|x| x.to_value());
                s.history_index = keep;
            }
        }
        held.retain(|s| s.settings.is_some() || s.history_index <= keep);
        self.snapshots = held;
    }

    /// The settings snapshot `i` names.
    pub fn snapshot_settings(&self, i: usize) -> Option<Settings> {
        let s = self.snapshots.get(i)?;
        match &s.settings {
            Some(v) => Settings::from_value(v).ok(),
            None => self.replay(s.history_index),
        }
    }

    /// Go back to snapshot `i`, as a new step (history is kept). False if
    /// there's no such snapshot or it's what's shown already.
    pub fn restore(&mut self, i: usize) -> bool {
        let (Some(to), Some(s)) = (self.snapshot_settings(i), self.snapshots.get(i)) else {
            return false;
        };
        let label = format!("Snapshot: {}", s.name);
        self.change(&label, |x| *x = to)
    }

    pub fn rename_snapshot(&mut self, i: usize, name: &str) {
        if let Some(s) = self.snapshots.get_mut(i) {
            name.clone_into(&mut s.name);
        }
    }

    pub fn remove_snapshot(&mut self, i: usize) {
        if i < self.snapshots.len() {
            self.snapshots.remove(i);
        }
    }
}

fn now() -> String {
    jiff::Zoned::now()
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}
