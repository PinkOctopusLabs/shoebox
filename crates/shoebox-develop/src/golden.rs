//! Golden images: renders kept as 16-bit TIFFs and compared by ΔE2000
//! (EDITING-SPEC: mean ≤ 0.5, max ≤ 3). The synthetic set lives in
//! `tests/golden/`; the real set (the 30 definition-of-done NEFs and their
//! edits) lives outside git and runs with `SHOEBOX_GOLDEN_DIR`.

use std::io::Cursor;
use std::path::Path;

use crate::colour;
use crate::cpu::{self, Target};

/// The spec's limits.
pub const MAX_MEAN: f64 = 0.5;
pub const MAX_ANY: f64 = 3.0;

/// A golden render: 16-bit display-encoded RGB in `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub px: Vec<u16>,
}

impl Image {
    pub fn from_render(img: &cpu::Rgb) -> Self {
        Self {
            width: img.width as u32,
            height: img.height as u32,
            px: cpu::to_u16(img),
        }
    }

    /// As a 16-bit, LZW-compressed TIFF.
    pub fn to_tiff(&self) -> Result<Vec<u8>, String> {
        let mut out = Cursor::new(Vec::new());
        let mut e = tiff::encoder::TiffEncoder::new(&mut out)
            .map_err(|e| e.to_string())?
            .with_compression(tiff::encoder::Compression::Lzw);
        e.write_image::<tiff::encoder::colortype::RGB16>(self.width, self.height, &self.px)
            .map_err(|e| e.to_string())?;
        Ok(out.into_inner())
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut d = tiff::decoder::Decoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
        let (width, height) = d.dimensions().map_err(|e| e.to_string())?;
        match d.read_image().map_err(|e| e.to_string())? {
            tiff::decoder::DecodingResult::U16(px) => Ok(Self { width, height, px }),
            _ => Err(format!("{}: not a 16-bit golden", path.display())),
        }
    }
}

/// Mean and largest ΔE2000 between two renders in `target` (sRGB or
/// Display P3, both with the sRGB curve). None if their sizes differ.
pub fn difference(a: &Image, b: &Image, target: Target) -> Option<(f64, f64)> {
    if (a.width, a.height) != (b.width, b.height) || a.px.len() != b.px.len() {
        return None;
    }
    let to_xyz = colour::rgb_to_xyz(target.primaries(), colour::D65);
    let lab = |p: &[u16]| {
        let lin = [0, 1, 2].map(|c| f64::from(cpu::srgb_eotf(f32::from(p[c]) / 65535.0)));
        colour::xyz_to_lab(colour::mulv(&to_xyz, lin), colour::D65)
    };
    let (mut sum, mut max) = (0.0, 0.0f64);
    let n = a.px.len() / 3;
    for (p, q) in a.px.as_chunks::<3>().0.iter().zip(b.px.as_chunks::<3>().0) {
        let d = colour::delta_e2000(lab(p), lab(q));
        sum += d;
        max = max.max(d);
    }
    Some((sum / n.max(1) as f64, max))
}
