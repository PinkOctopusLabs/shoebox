//! Looks (EDITING-SPEC, Auto mode, presets and packs: "Look LUTs"):
//! Adobe and Resolve `.cube` files, the cheap FilmPack-style looks, with
//! an Amount.
//!
//! - **Reading** ([`parse`]): TITLE, LUT_3D_SIZE (2–65), LUT_1D_SIZE,
//!   DOMAIN_MIN/MAX and Resolve's LUT_1D/3D_INPUT_RANGE, comments and
//!   unknown keywords skipped. A 3D table is kept as it is (red fastest,
//!   as [`Lut3`] has it); a 1D one becomes a 3D table (33 points a side,
//!   exact between them, as trilinear is on a per-channel curve); a 1D
//!   shaper before a 3D table is folded into it.
//! - **Where** ([`Stage`]): on the display transform's output, after the
//!   camera match look and before the tone curve, in the space nearly
//!   every look is made for (the files don't say): sRGB primaries,
//!   sRGB-encoded, clipped to 0–1. Trilinear, as the camera match look is
//!   (the same [`Lut3::sample`]). Amount mixes the result with what went
//!   in (0–200%: past 100% it goes further).
//!
//! Looks live in `<data>/looks/*.cube`; edits name them by name and
//! BLAKE3 ([`crate::edit::FileRef`]), as DNG camera profiles are.

use std::sync::{Arc, OnceLock, RwLock};

use crate::colour;
use crate::edit::FileRef;
use crate::profile::Lut3;

/// The largest 3D table read.
pub const MAX_SIZE: usize = 65;
/// Points a side of the 3D table a 1D look becomes.
const FROM_1D: usize = 33;
/// The largest file read (a 65³ table is about 8 MB of text).
pub const MAX_BYTES: u64 = 64 << 20;

/// A look.
#[derive(Debug, Clone, PartialEq)]
pub struct Cube {
    /// The file's name (without `.cube`).
    pub name: String,
    /// TITLE, if it has one.
    pub title: Option<String>,
    /// BLAKE3 of the file (hex).
    pub blake3: String,
    pub lut: Lut3,
    /// The input range: values here map to the table's ends.
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CubeError {
    #[error("line {0}: {1}")]
    Line(usize, String),
    #[error("no LUT_3D_SIZE or LUT_1D_SIZE")]
    NoSize,
    #[error("{0} entries for a table of {1}")]
    Count(usize, usize),
    #[error("a table of {0} a side is larger than {MAX_SIZE}")]
    TooLarge(usize),
}

fn floats<const N: usize>(words: &[&str], line: usize) -> Result<[f32; N], CubeError> {
    if words.len() != N {
        return Err(CubeError::Line(line, format!("{N} numbers expected")));
    }
    let mut out = [0f32; N];
    for (o, w) in out.iter_mut().zip(words) {
        *o = w
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| CubeError::Line(line, format!("“{w}” isn't a number")))?;
    }
    Ok(out)
}

fn size(words: &[&str], line: usize, max: usize) -> Result<usize, CubeError> {
    let [n] = floats::<1>(words, line)?;
    let n = n as usize;
    if n < 2 {
        return Err(CubeError::Line(line, "a size below 2".into()));
    }
    if n > max {
        return Err(CubeError::TooLarge(n));
    }
    Ok(n)
}

/// Read a `.cube` file's text; `name` names it and `blake3` is the file's
/// hash.
pub fn parse(text: &str, name: &str, blake3: &str) -> Result<Cube, CubeError> {
    let mut title = None;
    let (mut n3, mut n1) = (None, None);
    let (mut min, mut max) = ([0f32; 3], [1f32; 3]);
    let (mut range3, mut range1) = (None, None);
    let mut rows: Vec<[f32; 3]> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let body = raw.split('#').next().unwrap_or("").trim();
        if body.is_empty() {
            continue;
        }
        let words: Vec<&str> = body.split_whitespace().collect();
        let key = words[0].to_ascii_uppercase();
        let rest = &words[1..];
        match key.as_str() {
            "TITLE" => {
                let t = body[5..].trim().trim_matches('"').trim();
                title = (!t.is_empty()).then(|| t.to_owned());
            }
            "LUT_3D_SIZE" => n3 = Some(size(rest, line, MAX_SIZE)?),
            "LUT_1D_SIZE" => n1 = Some(size(rest, line, 1 << 16)?),
            "DOMAIN_MIN" => min = floats(rest, line)?,
            "DOMAIN_MAX" => max = floats(rest, line)?,
            "LUT_3D_INPUT_RANGE" => range3 = Some(floats::<2>(rest, line)?),
            "LUT_1D_INPUT_RANGE" => range1 = Some(floats::<2>(rest, line)?),
            k if k.starts_with(|c: char| c.is_ascii_alphabetic()) => {
                // Other keywords (LUT_IN_VIDEO_RANGE, …) don't change the
                // table.
            }
            _ => rows.push(floats(&words, line)?),
        }
    }
    let n1_rows = n1.unwrap_or(0);
    let n3_rows = n3.map_or(0, |n| n * n * n);
    if n1.is_none() && n3.is_none() {
        return Err(CubeError::NoSize);
    }
    if rows.len() != n1_rows + n3_rows {
        return Err(CubeError::Count(rows.len(), n1_rows + n3_rows));
    }
    for k in 0..3 {
        if max[k] <= min[k] {
            return Err(CubeError::Line(0, "DOMAIN_MAX not above DOMAIN_MIN".into()));
        }
    }
    let across = |r: Option<[f32; 2]>| r.map(|[a, b]| ([a; 3], [b; 3]));
    let (lut, min, max) = match (n1, n3) {
        (None, Some(n)) => {
            let (lo, hi) = across(range3).unwrap_or((min, max));
            (
                Lut3 {
                    size: n,
                    data: rows,
                },
                lo,
                hi,
            )
        }
        (Some(n), shaped) => {
            let (lo, hi) = across(range1).unwrap_or((min, max));
            let curve = &rows[..n];
            // The 1D curve at a fraction of its range, per channel.
            let at = |c: usize, e: f32| {
                let x = e.clamp(0.0, 1.0) * (n - 1) as f32;
                let i = (x.floor() as usize).min(n - 2);
                let f = x - i as f32;
                curve[i][c] + (curve[i + 1][c] - curve[i][c]) * f
            };
            let third = shaped.map(|s| {
                let (lo3, hi3) = across(range3).unwrap_or(([0.0; 3], [1.0; 3]));
                (
                    Lut3 {
                        size: s,
                        data: rows[n..].to_vec(),
                    },
                    lo3,
                    hi3,
                )
            });
            let side = third.as_ref().map_or(FROM_1D, |t| t.0.size);
            let mut lut = Lut3::identity(side);
            for cell in &mut lut.data {
                let s = [0, 1, 2].map(|c| at(c, cell[c]));
                *cell = match &third {
                    Some((t, lo3, hi3)) => {
                        t.sample([0, 1, 2].map(|c| (s[c] - lo3[c]) / (hi3[c] - lo3[c])))
                    }
                    None => s,
                };
            }
            (lut, lo, hi)
        }
        (None, None) => return Err(CubeError::NoSize),
    };
    for k in 0..3 {
        if max[k] <= min[k] {
            return Err(CubeError::Line(0, "an empty input range".into()));
        }
    }
    Ok(Cube {
        name: name.to_owned(),
        title,
        blake3: blake3.to_owned(),
        lut,
        min,
        max,
    })
}

impl Cube {
    /// For the edit: its name and contents.
    pub fn file_ref(&self) -> FileRef {
        FileRef {
            name: self.name.clone(),
            blake3: self.blake3.clone(),
        }
    }

    /// The look on sRGB-encoded values (no Amount).
    pub fn sample(&self, e: [f32; 3]) -> [f32; 3] {
        self.lut
            .sample([0, 1, 2].map(|k| (e[k] - self.min[k]) / (self.max[k] - self.min[k])))
    }
}

/// The look's numbers for one render.
#[derive(Debug, Clone)]
pub struct Stage {
    pub cube: Arc<Cube>,
    /// 0–2.
    pub amount: f32,
    /// The display transform's output (before its outset) → linear sRGB,
    /// and back.
    pub to_srgb: [[f32; 3]; 3],
    pub back: [[f32; 3]; 3],
}

impl Stage {
    /// `outset` is the display transform's (see `cpu::Display`).
    pub fn new(cube: Arc<Cube>, amount: f64, outset: &[[f32; 3]; 3]) -> Self {
        let srgb = colour::mul(
            &colour::xyz_to_rgb(&colour::SRGB, colour::D65),
            &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
        );
        let out = outset.map(|r| r.map(f64::from));
        let to = colour::mul(&srgb, &out);
        Self {
            cube,
            amount: (amount / 100.0).clamp(0.0, 2.0) as f32,
            to_srgb: crate::cpu::f32m(&to),
            back: crate::cpu::f32m(&colour::inv(&to).unwrap_or(colour::IDENTITY)),
        }
    }

    /// The look on one display-linear pixel (before the outset).
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let s = crate::cpu::mul3(&self.to_srgb, p);
        let e = s.map(|v| crate::cpu::srgb_oetf(v.clamp(0.0, 1.0)));
        let out = self.cube.sample(e).map(crate::cpu::srgb_eotf);
        let q = crate::cpu::mul3(&self.back, out);
        [0, 1, 2].map(|k| p[k] + (q[k] - p[k]) * self.amount)
    }
}

/// Looks registered by hand, and those read from the folder (replaced on
/// each read).
#[derive(Default)]
struct Registry {
    added: Vec<Arc<Cube>>,
    loaded: Vec<Arc<Cube>>,
}

fn registry() -> &'static RwLock<Registry> {
    static R: OnceLock<RwLock<Registry>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Make a look available to renders (one with the same contents is
/// replaced).
pub fn register(c: Cube) -> Arc<Cube> {
    let c = Arc::new(c);
    if let Ok(mut r) = registry().write() {
        r.added.retain(|x| x.blake3 != c.blake3);
        r.added.push(c.clone());
    }
    c
}

/// The look an edit names, by its contents.
pub fn find(r: &FileRef) -> Option<Arc<Cube>> {
    let reg = registry().read().ok()?;
    reg.added
        .iter()
        .chain(&reg.loaded)
        .find(|c| c.blake3 == r.blake3)
        .cloned()
}

/// A look by name (to offer one that's changed since an edit used it).
pub fn named(name: &str) -> Option<Arc<Cube>> {
    let reg = registry().read().ok()?;
    reg.added
        .iter()
        .chain(&reg.loaded)
        .find(|c| c.name == name)
        .cloned()
}

/// Every look, by name.
pub fn all() -> Vec<Arc<Cube>> {
    let mut v: Vec<Arc<Cube>> = registry()
        .read()
        .map(|r| r.added.iter().chain(&r.loaded).cloned().collect())
        .unwrap_or_default();
    v.sort_by_key(|c| c.name.to_lowercase());
    v.dedup_by(|a, b| a.blake3 == b.blake3);
    v
}

/// Read every look in a folder (`*.cube`), in place of those read
/// before; the folder is only read. Returns how many.
pub fn load_dir(dir: &std::path::Path) -> usize {
    let mut found = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if !p
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("cube"))
            {
                continue;
            }
            if e.metadata().is_ok_and(|m| m.len() > MAX_BYTES) {
                tracing::warn!("{}: too large for a look", p.display());
                continue;
            }
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| {
                let hash = blake3::hash(&b).to_hex().to_string();
                parse(&String::from_utf8_lossy(&b), &name, &hash).map_err(|e| e.to_string())
            }) {
                Ok(c) => found.push(Arc::new(c)),
                Err(e) => tracing::warn!("{}: {e}", p.display()),
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    let n = found.len();
    if let Ok(mut r) = registry().write() {
        r.loaded = found;
    }
    n
}
