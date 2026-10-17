//! File analysis: BLAKE3, image-data hash, EXIF and perceptual hashes.

pub mod perceptual;

use std::path::Path;

use shoebox_decode::Format;

pub use perceptual::{PHashes, hamming};

use crate::error::{CoreError, Result, io};
use crate::exif::{self, ExifInfo};

/// Bump when hashing changes, so cached results are recomputed.
pub const ANALYZER_VERSION: i64 = 2;

/// Long edge of the grayscale decode that perceptual hashes come from.
pub const HASH_DECODE_EDGE: u32 = 256;

pub type Digest = [u8; 32];

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub blake3: Digest,
    /// BLAKE3 of the image data without metadata, when the structure parses.
    pub image_data_hash: Option<Digest>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub exif: ExifInfo,
    /// Hashes of the upright image rotated by 0°, 90°, 180° and 270°.
    /// Empty if the pixels could not be decoded.
    pub hashes: Vec<PHashes>,
    /// Standard deviation of the 256 px luma thumbnail. Near-uniform images
    /// (blank, blown-out, lens cap) have tiny values and meaningless hashes.
    pub luma_stddev: Option<f32>,
    /// Why decoding or structure parsing failed, if it did.
    pub error: Option<String>,
}

pub fn blake3_of(data: &[u8]) -> Digest {
    let mut h = blake3::Hasher::new();
    h.update_rayon(data);
    *h.finalize().as_bytes()
}

/// BLAKE3 of the image data without metadata, or None for formats without a
/// defined image-data hash (HEIC).
pub fn image_data_hash(
    format: Format,
    data: &[u8],
) -> Result<Option<Digest>, shoebox_decode::DecodeError> {
    let Some(ranges) = shoebox_decode::image_data_ranges(format, data)? else {
        return Ok(None);
    };
    let mut h = blake3::Hasher::new();
    for r in ranges {
        h.update(&data[r]);
    }
    Ok(Some(*h.finalize().as_bytes()))
}

/// Analyse a file from its bytes. Never fails on bad image data: problems are
/// recorded in `error` so the exact hash still counts.
pub fn analyze_bytes(format: Format, data: &[u8]) -> Analysis {
    let mut errors = Vec::new();
    let image_data_hash = image_data_hash(format, data)
        .map_err(|e| errors.push(format!("image data: {e}")))
        .ok()
        .flatten();
    let exif = exif::read(data);
    let (width, height, hashes, luma_stddev) =
        match shoebox_decode::luma_thumbnail(format, data, HASH_DECODE_EDGE) {
            Ok(g) => {
                let upright = g.orient(exif.orientation.unwrap_or(1));
                let dims = shoebox_decode::dimensions(format, data).ok();
                // Report upright dimensions.
                let (w, h) = match (dims, exif.orientation) {
                    (Some((w, h)), Some(5..=8)) => (Some(h), Some(w)),
                    (Some((w, h)), _) => (Some(w), Some(h)),
                    (None, _) => (None, None),
                };
                let r90 = upright.rotate90();
                let hashes = vec![
                    PHashes::of(&upright),
                    PHashes::of(&r90),
                    PHashes::of(&upright.rotate180()),
                    PHashes::of(&r90.rotate180()),
                ];
                (w, h, hashes, Some(stddev(&upright.px)))
            }
            Err(e) => {
                errors.push(format!("decode: {e}"));
                (None, None, Vec::new(), None)
            }
        };
    Analysis {
        blake3: blake3_of(data),
        image_data_hash,
        width,
        height,
        exif,
        hashes,
        luma_stddev,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

fn stddev(px: &[u8]) -> f32 {
    let n = px.len().max(1) as f64;
    let mean = px.iter().map(|&v| f64::from(v)).sum::<f64>() / n;
    let var = px
        .iter()
        .map(|&v| (f64::from(v) - mean).powi(2))
        .sum::<f64>()
        / n;
    var.sqrt() as f32
}

pub fn analyze_path(path: &Path) -> Result<Analysis> {
    let format = Format::from_path(path)
        .ok_or_else(|| CoreError::Invalid(format!("unsupported file type: {}", path.display())))?;
    if format == Format::Mov {
        return analyze_movie(path);
    }
    let data = std::fs::read(path).map_err(io(path))?;
    Ok(analyze_bytes(format, &data))
}

/// A movie: exact copies only (BLAKE3, streamed, so a big clip isn't read
/// into memory), with its size from the header. No perceptual hashes.
fn analyze_movie(path: &Path) -> Result<Analysis> {
    let mut f = std::fs::File::open(path).map_err(io(path))?;
    let info = shoebox_decode::mov::info(&mut f).ok();
    let mut f = std::fs::File::open(path).map_err(io(path))?;
    let mut h = blake3::Hasher::new();
    h.update_reader(&mut f).map_err(io(path))?;
    Ok(Analysis {
        blake3: *h.finalize().as_bytes(),
        image_data_hash: None,
        width: info.as_ref().and_then(|i| i.width),
        height: info.as_ref().and_then(|i| i.height),
        exif: crate::exif::ExifInfo::default(),
        hashes: Vec::new(),
        luma_stddev: None,
        error: None,
    })
}
