//! DNG camera profiles (EDITING-SPEC, Colour science, levels 3 and 4):
//! `.dcp` files, as Adobe's and dcamprof's are, read and applied as the DNG
//! specification (1.6, chapter 6, "Camera Profiles") and its SDK's
//! reference code describe them, which RawTherapee's `rtengine/dcp.cc`
//! follows too. Written from the specification; no code is ported.
//!
//! - **Matrices.** ColorMatrix1/2, and ForwardMatrix1/2 when there are
//!   any, by CalibrationIlluminant1/2, stand in for the raw file's
//!   ([`Dcp::camera`], through [`crate::wb::resolve_with`]): white balance,
//!   the as-shot white and the camera → Rec.2020 matrix all come from them.
//! - **Tables** ([`Stage`]): ProfileHueSatMap (two blended by the white's
//!   temperature, looked up at the scene's values before exposure), then
//!   ProfileLookTable (after it): each a hue shift, a saturation scale and
//!   a value scale in HSV of linear ProPhoto RGB, or with the value
//!   sRGB-encoded when the encoding tag says so. Per pixel, right after the
//!   colour matrix, before the local tone maps.
//! - **Tone curve** ([`Tone`]): ProfileToneCurve, when the edit asks for
//!   it, in place of the display transform's curve: on ProPhoto, with hue
//!   kept as Adobe's RGB tone does it (the brightest and darkest channels
//!   through the curve, the middle one in proportion).
//! - BaselineExposureOffset adds to the baseline exposure. DefaultBlackRender
//!   is read, but Shoebox never adds an automatic black, so both of its
//!   values render the same.
//!
//! Profiles live in `<data>/profiles/*.dcp` beside the camera match
//! profiles, are read once and kept here by content; edits name them by
//! name and BLAKE3 ([`crate::edit::FileRef`]), so one that's gone or
//! changed is noticed rather than silently rendering differently.

// Channel and matrix loops read clearer indexed.
#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::colour::{self, M3};
use crate::edit::FileRef;

/// Entries in a baked tone curve, indexed by the 2.2-encoded input.
pub const TONE_SIZE: usize = 1024;

/// A hue/saturation/value table (HueSatMap or LookTable).
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub hues: usize,
    pub sats: usize,
    pub vals: usize,
    /// Value-major, then hue, then saturation: hue shift (degrees),
    /// saturation scale, value scale.
    pub data: Vec<[f32; 3]>,
    /// The value axis sRGB-encoded (the encoding tag is 1).
    pub srgb: bool,
}

impl Table {
    /// An identity table of these divisions.
    pub fn identity(hues: usize, sats: usize, vals: usize) -> Self {
        Self {
            hues,
            sats,
            vals,
            data: vec![[0.0, 1.0, 1.0]; hues * sats * vals],
            srgb: false,
        }
    }
}

/// A DNG camera profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Dcp {
    /// ProfileName, else the file's name.
    pub name: String,
    /// UniqueCameraModel ("Nikon Z f").
    pub camera: String,
    /// BLAKE3 of the file (hex).
    pub blake3: String,
    /// XYZ → camera, each with its illuminant's temperature.
    pub matrices: Vec<(f64, M3)>,
    /// White-balanced camera → XYZ (D50), likewise; often none.
    pub forward: Vec<(f64, M3)>,
    /// HueSatMap tables with their illuminants' temperatures, the cooler
    /// first (one, or two to blend).
    pub hue_sat: Vec<(f64, Table)>,
    pub look: Option<Table>,
    /// ProfileToneCurve as (x, y) points, and baked (see [`tone_at`]).
    pub tone_points: Vec<[f32; 2]>,
    pub tone: Option<Arc<[f32]>>,
    /// Stops added to the baseline exposure.
    pub exposure_offset: f64,
    /// DefaultBlackRender: 0 auto, 1 none.
    pub black_render: u32,
}

/// What can go wrong reading a profile.
#[derive(Debug, thiserror::Error)]
pub enum DcpError {
    #[error("not a DNG camera profile")]
    Format,
    #[error("a malformed {0}")]
    Malformed(&'static str),
}

// The tags (DNG specification 1.6).
const UNIQUE_CAMERA_MODEL: u16 = 50708;
const COLOR_MATRIX_1: u16 = 50721;
const COLOR_MATRIX_2: u16 = 50722;
const CALIBRATION_ILLUMINANT_1: u16 = 50778;
const CALIBRATION_ILLUMINANT_2: u16 = 50779;
const PROFILE_NAME: u16 = 50936;
const HUE_SAT_MAP_DIMS: u16 = 50937;
const HUE_SAT_MAP_DATA_1: u16 = 50938;
const HUE_SAT_MAP_DATA_2: u16 = 50939;
const PROFILE_TONE_CURVE: u16 = 50940;
const FORWARD_MATRIX_1: u16 = 50964;
const FORWARD_MATRIX_2: u16 = 50965;
const LOOK_TABLE_DIMS: u16 = 50981;
const LOOK_TABLE_DATA: u16 = 50982;
const HUE_SAT_MAP_ENCODING: u16 = 51107;
const LOOK_TABLE_ENCODING: u16 = 51108;
const BASELINE_EXPOSURE_OFFSET: u16 = 51109;
const DEFAULT_BLACK_RENDER: u16 = 51110;

/// Values read from one entry at most (a 3D table is far smaller).
const MAX_VALUES: usize = 1 << 22;

/// The EXIF LightSource codes' temperatures, as the DNG SDK takes them
/// (0: unknown).
pub fn illuminant_temperature(code: u32) -> f64 {
    match code {
        17 | 3 => 2850.0,
        24 => 3200.0,
        23 => 5000.0,
        20 | 1 | 9 | 4 | 18 => 5500.0,
        21 | 19 | 10 => 6500.0,
        22 | 11 => 7500.0,
        12 => (5700.0 + 7100.0) * 0.5,
        13 => (4600.0 + 5500.0) * 0.5,
        14 | 2 => (3800.0 + 4500.0) * 0.5,
        15 => (3250.0 + 3800.0) * 0.5,
        16 => (2600.0 + 3250.0) * 0.5,
        _ => 0.0,
    }
}

/// The file's IFD 0, read just far enough.
struct Ifd<'a> {
    data: &'a [u8],
    little: bool,
    /// tag → (type, count, where the values are)
    entries: HashMap<u16, (u16, usize, usize)>,
}

impl<'a> Ifd<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, DcpError> {
        let little = match data.get(0..4) {
            // DCPs have their own magic ("IIRC"); plain TIFF is accepted too.
            Some(b"IIRC") | Some(b"II*\0") => true,
            Some(b"MMCR") | Some(b"MM\0*") => false,
            _ => return Err(DcpError::Format),
        };
        let mut ifd = Self {
            data,
            little,
            entries: HashMap::new(),
        };
        let at = ifd.u32(4).ok_or(DcpError::Format)? as usize;
        let n = ifd.u16(at).ok_or(DcpError::Format)? as usize;
        for i in 0..n.min(4096) {
            let e = at + 2 + i * 12;
            let (Some(tag), Some(typ), Some(count)) = (ifd.u16(e), ifd.u16(e + 2), ifd.u32(e + 4))
            else {
                return Err(DcpError::Format);
            };
            let Some(size) = type_size(typ) else { continue };
            let count = count as usize;
            let bytes = size.saturating_mul(count);
            let value_at = if bytes <= 4 {
                e + 8
            } else {
                ifd.u32(e + 8).ok_or(DcpError::Format)? as usize
            };
            if count > MAX_VALUES || value_at.saturating_add(bytes) > data.len() {
                continue;
            }
            ifd.entries.insert(tag, (typ, count, value_at));
        }
        Ok(ifd)
    }

    fn bytes<const N: usize>(&self, at: usize) -> Option<[u8; N]> {
        let mut b: [u8; N] = self.data.get(at..at + N)?.try_into().ok()?;
        if !self.little {
            b.reverse();
        }
        Some(b)
    }

    fn u16(&self, at: usize) -> Option<u16> {
        self.bytes::<2>(at).map(u16::from_le_bytes)
    }

    fn u32(&self, at: usize) -> Option<u32> {
        self.bytes::<4>(at).map(u32::from_le_bytes)
    }

    /// An entry's values as numbers.
    fn values(&self, tag: u16) -> Option<Vec<f64>> {
        let &(typ, count, at) = self.entries.get(&tag)?;
        let size = type_size(typ)?;
        (0..count)
            .map(|i| {
                let p = at + i * size;
                Some(match typ {
                    1 | 7 => f64::from(*self.data.get(p)?),
                    6 => f64::from(*self.data.get(p)? as i8),
                    3 => f64::from(self.u16(p)?),
                    8 => f64::from(self.u16(p)? as i16),
                    4 => f64::from(self.u32(p)?),
                    9 => f64::from(self.u32(p)? as i32),
                    11 => f64::from(f32::from_bits(self.u32(p)?)),
                    12 => f64::from_bits(u64::from_le_bytes(self.bytes::<8>(p)?)),
                    5 => {
                        let d = self.u32(p + 4)?;
                        if d == 0 {
                            0.0
                        } else {
                            f64::from(self.u32(p)?) / f64::from(d)
                        }
                    }
                    10 => {
                        let d = self.u32(p + 4)? as i32;
                        if d == 0 {
                            0.0
                        } else {
                            f64::from(self.u32(p)? as i32) / f64::from(d)
                        }
                    }
                    _ => return None,
                })
            })
            .collect()
    }

    fn text(&self, tag: u16) -> Option<String> {
        let &(typ, count, at) = self.entries.get(&tag)?;
        if typ != 2 && typ != 1 && typ != 7 {
            return None;
        }
        let b = self.data.get(at..at + count)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Some(String::from_utf8_lossy(&b[..end]).trim().to_owned())
    }

    fn uint(&self, tag: u16) -> Option<u32> {
        self.values(tag)?.first().map(|v| *v as u32)
    }
}

fn type_size(typ: u16) -> Option<usize> {
    Some(match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

fn matrix(v: &[f64]) -> Option<M3> {
    (v.len() == 9).then(|| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
}

/// XYZ (D50) of the profile connection space's white.
fn d50() -> [f64; 3] {
    colour::xy_to_xyz(colour::D50)
}

/// A colour matrix scaled as the DNG SDK does: the white's camera values
/// peak at 1.
fn normalise_colour(m: M3) -> M3 {
    let w = colour::mulv(&m, d50());
    let peak = w[0].max(w[1]).max(w[2]);
    if peak > 1e-9 {
        m.map(|r| r.map(|v| v / peak))
    } else {
        m
    }
}

/// A forward matrix scaled as the DNG SDK does: camera (1, 1, 1) goes to
/// D50 exactly.
fn normalise_forward(m: M3) -> M3 {
    let xyz = colour::mulv(&m, [1.0; 3]);
    let w = d50();
    let mut out = m;
    for r in 0..3 {
        if xyz[r].abs() > 1e-9 {
            out[r] = m[r].map(|v| v * w[r] / xyz[r]);
        }
    }
    out
}

fn table(ifd: &Ifd, dims: u16, data: u16, encoding: u16) -> Result<Option<Table>, DcpError> {
    let Some(v) = ifd.values(data) else {
        return Ok(None);
    };
    let d = ifd.values(dims).ok_or(DcpError::Malformed("table size"))?;
    let (hues, sats, vals) = match d.as_slice() {
        [h, s] => (*h as usize, *s as usize, 1),
        [h, s, v] => (*h as usize, *s as usize, (*v as usize).max(1)),
        _ => return Err(DcpError::Malformed("table size")),
    };
    if hues == 0 || sats < 2 || v.len() != hues * sats * vals * 3 {
        return Err(DcpError::Malformed("table"));
    }
    Ok(Some(Table {
        hues,
        sats,
        vals,
        data: v
            .as_chunks::<3>()
            .0
            .iter()
            .map(|e| e.map(|x| x as f32))
            .collect(),
        srgb: ifd.uint(encoding) == Some(1),
    }))
}

/// Read a profile; `fallback` names it when it doesn't name itself.
pub fn parse(bytes: &[u8], fallback: &str) -> Result<Dcp, DcpError> {
    let ifd = Ifd::parse(bytes)?;
    let temp = |tag| {
        ifd.uint(tag)
            .map(illuminant_temperature)
            .filter(|t| *t > 0.0)
    };
    // An unknown illuminant counts as D50, as a single calibration would.
    let (t1, t2) = (
        temp(CALIBRATION_ILLUMINANT_1).unwrap_or(5000.0),
        temp(CALIBRATION_ILLUMINANT_2),
    );
    let read = |tag| ifd.values(tag).and_then(|v| matrix(&v));
    let cm1 = read(COLOR_MATRIX_1).ok_or(DcpError::Malformed("colour matrix"))?;
    let mut matrices = vec![(t1, normalise_colour(cm1))];
    let mut forward: Vec<(f64, M3)> = read(FORWARD_MATRIX_1)
        .map(|f| (t1, normalise_forward(f)))
        .into_iter()
        .collect();
    let mut hue_sat = Vec::new();
    if let Some(t) = table(
        &ifd,
        HUE_SAT_MAP_DIMS,
        HUE_SAT_MAP_DATA_1,
        HUE_SAT_MAP_ENCODING,
    )? {
        hue_sat.push((t1, t));
    }
    // The second calibration counts only with an illuminant of its own.
    if let Some(t2) = t2.filter(|t| (t - t1).abs() > 1.0) {
        if let Some(cm2) = read(COLOR_MATRIX_2) {
            matrices.push((t2, normalise_colour(cm2)));
            if let Some(f) = read(FORWARD_MATRIX_2)
                && !forward.is_empty()
            {
                forward.push((t2, normalise_forward(f)));
            }
        }
        if let Some(t) = table(
            &ifd,
            HUE_SAT_MAP_DIMS,
            HUE_SAT_MAP_DATA_2,
            HUE_SAT_MAP_ENCODING,
        )? && !hue_sat.is_empty()
        {
            hue_sat.push((t2, t));
        }
    }
    hue_sat.sort_by(|a, b| a.0.total_cmp(&b.0));
    // A pair of forward matrices or none (one alone, with two colour
    // matrices, would mismatch them).
    if forward.len() != matrices.len() {
        forward.clear();
    }
    let look = table(&ifd, LOOK_TABLE_DIMS, LOOK_TABLE_DATA, LOOK_TABLE_ENCODING)?;
    let tone_points: Vec<[f32; 2]> = ifd
        .values(PROFILE_TONE_CURVE)
        .map(|v| {
            v.as_chunks::<2>()
                .0
                .iter()
                .map(|p| [p[0] as f32, p[1] as f32])
                .collect()
        })
        .unwrap_or_default();
    let tone = (tone_points.len() >= 2).then(|| tone_table(&tone_points));
    Ok(Dcp {
        name: ifd
            .text(PROFILE_NAME)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| fallback.to_owned()),
        camera: ifd.text(UNIQUE_CAMERA_MODEL).unwrap_or_default(),
        blake3: blake3::hash(bytes).to_hex().to_string(),
        matrices,
        forward,
        hue_sat,
        look,
        tone_points,
        tone,
        exposure_offset: ifd
            .values(BASELINE_EXPOSURE_OFFSET)
            .and_then(|v| v.first().copied())
            .filter(|v| v.is_finite())
            .unwrap_or(0.0),
        black_render: ifd.uint(DEFAULT_BLACK_RENDER).unwrap_or(0),
    })
}

/// The curve through these points (sorted by x; linear between them, as
/// profiles' curves are dense), as [`TONE_SIZE`] entries at 2.2-encoded
/// inputs.
pub fn tone_table(points: &[[f32; 2]]) -> Arc<[f32]> {
    let mut p = points.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let at = |x: f32| -> f32 {
        let i = p.partition_point(|q| q[0] <= x);
        if i == 0 {
            return p[0][1];
        }
        if i >= p.len() {
            return p[p.len() - 1][1];
        }
        let (a, b) = (p[i - 1], p[i]);
        let t = if b[0] > a[0] {
            (x - a[0]) / (b[0] - a[0])
        } else {
            0.0
        };
        a[1] + (b[1] - a[1]) * t
    };
    (0..TONE_SIZE)
        .map(|i| at((i as f32 / (TONE_SIZE - 1) as f32).powf(2.2)))
        .collect()
}

/// A baked tone curve at linear `v` (clipped to 0–1).
pub fn tone_at(t: &[f32], v: f32) -> f32 {
    let x = v.clamp(0.0, 1.0).powf(1.0 / 2.2) * (TONE_SIZE - 1) as f32;
    let i = (x.floor() as usize).min(TONE_SIZE - 2);
    let f = x - i as f32;
    t[i] + (t[i + 1] - t[i]) * f
}

/// Adobe's RGB tone: the brightest and darkest channels through the curve,
/// the middle one placed between them as it was, so hue holds.
pub fn rgb_tone(t: &[f32], p: [f32; 3]) -> [f32; 3] {
    let p = p.map(|v| v.clamp(0.0, 1.0));
    let hi = p[0].max(p[1]).max(p[2]);
    let lo = p[0].min(p[1]).min(p[2]);
    let (th, tl) = (tone_at(t, hi), tone_at(t, lo));
    if hi - lo <= 0.0 {
        return [th; 3];
    }
    p.map(|v| tl + (th - tl) * (v - lo) / (hi - lo))
}

impl Dcp {
    /// The profile's calibration, for white balance and the camera matrix.
    pub fn camera(&self) -> crate::wb::Camera {
        crate::wb::Camera::with_forward(self.matrices.clone(), self.forward.clone())
    }

    /// Whether it has tables to apply per pixel.
    pub fn has_tables(&self) -> bool {
        !self.hue_sat.is_empty() || self.look.is_some()
    }

    /// For the edit: its name and contents.
    pub fn file_ref(&self) -> FileRef {
        FileRef {
            name: self.name.clone(),
            blake3: self.blake3.clone(),
        }
    }

    /// Whether it's for this camera (its UniqueCameraModel is the model,
    /// or the make and model).
    pub fn fits(&self, make: &str, model: &str) -> bool {
        let norm = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase()
        };
        let (cam, model) = (norm(&self.camera), norm(model));
        !model.is_empty()
            && (cam == model
                || cam == format!("{} {model}", norm(make))
                || cam.ends_with(&format!(" {model}")))
    }
}

/// Linear Rec.2020 (D65) → linear ProPhoto RGB (D50, Bradford), where the
/// tables and the tone curve work.
pub fn to_pro_photo() -> M3 {
    colour::mul(
        &colour::xyz_to_rgb(&colour::PRO_PHOTO, colour::D50),
        &colour::mul(
            &colour::bradford(colour::D65, colour::D50),
            &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
        ),
    )
}

/// The per-pixel tables' numbers for one render.
#[derive(Debug, Clone)]
pub struct Stage {
    pub dcp: Arc<Dcp>,
    /// How much of the cooler HueSatMap (1 with one table).
    pub weight: f32,
    /// Scales the colour matrix's output back to before exposure, where
    /// HueSatMap is looked up.
    pub unexposed: f32,
    /// Linear Rec.2020 → linear ProPhoto, and back.
    pub to_pp: [[f32; 3]; 3],
    pub from_pp: [[f32; 3]; 3],
}

impl Stage {
    /// For a white at `temp` K, and the exposure gain the colour matrix
    /// carries. None when the profile has no tables.
    pub fn new(dcp: Arc<Dcp>, temp: f64, gain: f64) -> Option<Self> {
        if !dcp.has_tables() {
            return None;
        }
        let weight = match dcp.hue_sat.as_slice() {
            [a, b] => crate::wb::weight(a.0, b.0, temp) as f32,
            _ => 1.0,
        };
        let to = to_pro_photo();
        Some(Self {
            dcp,
            weight,
            unexposed: (1.0 / gain.max(1e-9)) as f32,
            to_pp: crate::cpu::f32m(&to),
            from_pp: crate::cpu::f32m(&colour::inv(&to).unwrap_or(colour::IDENTITY)),
        })
    }

    /// The tables on one pixel of linear Rec.2020.
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let mut q = crate::cpu::mul3(&self.to_pp, p);
        let d = &self.dcp;
        if let Some((_, a)) = d.hue_sat.first() {
            let b = d.hue_sat.get(1).map(|t| &t.1);
            q = hsv_table(a, b, self.weight, q, self.unexposed);
        }
        if let Some(t) = &d.look {
            q = hsv_table(t, None, 1.0, q, 1.0);
        }
        crate::cpu::mul3(&self.from_pp, q)
    }
}

/// The profile's tone curve in the display transform's place.
#[derive(Debug, Clone)]
pub struct Tone {
    pub table: Arc<[f32]>,
    pub to_pp: [[f32; 3]; 3],
    pub from_pp: [[f32; 3]; 3],
}

impl Tone {
    pub fn new(table: Arc<[f32]>) -> Self {
        let to = to_pro_photo();
        Self {
            table,
            to_pp: crate::cpu::f32m(&to),
            from_pp: crate::cpu::f32m(&colour::inv(&to).unwrap_or(colour::IDENTITY)),
        }
    }

    /// Scene-linear Rec.2020 → display-linear Rec.2020.
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let q = crate::cpu::mul3(&self.to_pp, p);
        crate::cpu::mul3(&self.from_pp, rgb_tone(&self.table, q))
    }
}

/// The DNG SDK's RGB → HSV: hue 0–6, saturation, value (the largest
/// channel).
pub fn hsv(p: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = p;
    let v = r.max(g).max(b);
    let gap = v - r.min(g).min(b);
    if gap <= 0.0 || v <= 0.0 {
        return [0.0, 0.0, v];
    }
    let mut h = if r == v {
        (g - b) / gap
    } else if g == v {
        2.0 + (b - r) / gap
    } else {
        4.0 + (r - g) / gap
    };
    if h < 0.0 {
        h += 6.0;
    }
    [h, gap / v, v]
}

/// The inverse of [`hsv`] (hue wrapped into 0–6).
pub fn rgb(hsv: [f32; 3]) -> [f32; 3] {
    let [mut h, s, v] = hsv;
    if s <= 0.0 {
        return [v; 3];
    }
    h -= 6.0 * (h / 6.0).floor();
    let i = (h.floor() as usize).min(5);
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// A table's change at hue `h` (0–6), saturation `s` and (encoded) value
/// `v`: hue shift (degrees), saturation scale, value scale; `b`, a second
/// table, blended in by `1 − w`. Bilinear in hue and saturation (hue
/// wrapping round), and linear in value for a 3D table.
pub fn lookup(a: &Table, b: Option<&Table>, w: f32, h: f32, s: f32, v: f32) -> [f32; 3] {
    let entry = |i: usize| -> [f32; 3] {
        let x = a.data[i];
        match b.and_then(|b| b.data.get(i)) {
            Some(y) => [0, 1, 2].map(|k| w * x[k] + (1.0 - w) * y[k]),
            None => x,
        }
    };
    let hs = if a.hues < 2 {
        0.0
    } else {
        h * a.hues as f32 / 6.0
    };
    let mut h0 = hs.max(0.0).floor() as usize;
    let mut h1 = h0 + 1;
    if h0 >= a.hues - 1 {
        h0 = a.hues - 1;
        h1 = 0;
    }
    let hf = hs - h0 as f32;
    let ss = s.clamp(0.0, 1.0) * (a.sats - 1) as f32;
    let s0 = (ss.floor() as usize).min(a.sats - 2);
    let sf = ss - s0 as f32;
    let (v0, vf) = if a.vals < 2 {
        (0, 0.0)
    } else {
        let vs = v.clamp(0.0, 1.0) * (a.vals - 1) as f32;
        let v0 = (vs.floor() as usize).min(a.vals - 2);
        (v0, vs - v0 as f32)
    };
    let mix = |x: [f32; 3], y: [f32; 3], t: f32| [0, 1, 2].map(|k| x[k] + (y[k] - x[k]) * t);
    let plane = |vi: usize| {
        let at = |hi: usize, si: usize| entry((vi * a.hues + hi) * a.sats + si);
        mix(
            mix(at(h0, s0), at(h1, s0), hf),
            mix(at(h0, s0 + 1), at(h1, s0 + 1), hf),
            sf,
        )
    };
    let out = plane(v0);
    if a.vals < 2 {
        out
    } else {
        mix(out, plane(v0 + 1), vf)
    }
}

/// One table on a linear ProPhoto pixel; `pre` scales the value it's
/// looked up (and, sRGB-encoded, scaled) at. Saturation isn't pushed past
/// the gamut's edge (but a colour already outside isn't pulled in), and
/// values aren't clipped: the pipe is scene-referred.
pub fn hsv_table(a: &Table, b: Option<&Table>, w: f32, p: [f32; 3], pre: f32) -> [f32; 3] {
    let [h, s, v] = hsv(p);
    if v <= 0.0 {
        return p;
    }
    let at = v * pre;
    let enc = if a.srgb {
        crate::cpu::srgb_oetf(at)
    } else {
        at
    };
    let [dh, ks, kv] = lookup(a, b, w, h, s, enc);
    let s2 = (s * ks).min(s.max(1.0)).max(0.0);
    let v2 = if a.srgb {
        crate::cpu::srgb_eotf(enc * kv) / pre
    } else {
        v * kv
    };
    rgb([h + dh * (6.0 / 360.0), s2, v2])
}

/// Profiles registered by hand, and those read from the folder (replaced
/// on each read).
#[derive(Default)]
struct Registry {
    added: Vec<Arc<Dcp>>,
    loaded: Vec<Arc<Dcp>>,
}

impl Registry {
    fn all(&self) -> impl Iterator<Item = &Arc<Dcp>> {
        self.added.iter().chain(&self.loaded)
    }
}

fn registry() -> &'static RwLock<Registry> {
    static R: OnceLock<RwLock<Registry>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Make a profile available to renders (one with the same contents is
/// replaced).
pub fn register(d: Dcp) -> Arc<Dcp> {
    let d = Arc::new(d);
    if let Ok(mut r) = registry().write() {
        r.added.retain(|x| x.blake3 != d.blake3);
        r.added.push(d.clone());
    }
    d
}

/// The profile an edit names, by its contents.
pub fn find(r: &FileRef) -> Option<Arc<Dcp>> {
    registry()
        .read()
        .ok()?
        .all()
        .find(|d| d.blake3 == r.blake3)
        .cloned()
}

/// The profile these settings choose, when it's there.
pub fn chosen(s: &crate::edit::Settings) -> Option<Arc<Dcp>> {
    let ic = &s.modules.input_colour;
    if ic.profile != crate::edit::Profile::Dcp {
        return None;
    }
    ic.dcp.as_ref().and_then(find)
}

/// A profile by name (to offer one that's changed since an edit used it).
pub fn named(name: &str) -> Option<Arc<Dcp>> {
    registry()
        .read()
        .ok()?
        .all()
        .find(|d| d.name == name)
        .cloned()
}

/// The profiles for a camera, by name.
pub fn for_camera(make: &str, model: &str) -> Vec<Arc<Dcp>> {
    let mut v: Vec<Arc<Dcp>> = registry()
        .read()
        .map(|r| r.all().filter(|d| d.fits(make, model)).cloned().collect())
        .unwrap_or_default();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v.dedup_by(|a, b| a.blake3 == b.blake3);
    v
}

/// Read every profile in a folder (`*.dcp`), in place of those read
/// before; the folder is only read. Returns how many.
pub fn load_dir(dir: &std::path::Path) -> usize {
    let mut found = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp")) {
                continue;
            }
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            match std::fs::read(&p)
                .map_err(|e| e.to_string())
                .and_then(|b| parse(&b, &stem).map_err(|e| e.to_string()))
            {
                Ok(d) => found.push(Arc::new(d)),
                Err(e) => tracing::warn!("{}: {e}", p.display()),
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found.dedup_by(|a, b| a.blake3 == b.blake3);
    let n = found.len();
    if let Ok(mut r) = registry().write() {
        r.loaded = found;
    }
    n
}
