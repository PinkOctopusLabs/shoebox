//! File analysis: BLAKE3, image-data hash, EXIF and perceptual hashes.

pub mod perceptual;

use std::path::Path;

use shoebox_decode::Format;

pub use perceptual::{PHashes, hamming};

use crate::error::{CoreError, Result, io};
use crate::exif::{self, ExifInfo};

/// Bump when hashing changes, so cached results are recomputed.
pub const ANALYZER_VERSION: i64 = 1;

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
    /// Why decoding or structure parsing failed, if it did.
    pub error: Option<String>,
}

pub fn blake3_of(data: &[u8]) -> Digest {
    let mut h = blake3::Hasher::new();
    h.update_rayon(data);
    *h.finalize().as_bytes()
}

pub fn image_data_hash(format: Format, data: &[u8]) -> Result<Digest, shoebox_decode::DecodeError> {
    let mut h = blake3::Hasher::new();
    for r in shoebox_decode::image_data_ranges(format, data)? {
        h.update(&data[r]);
    }
    Ok(*h.finalize().as_bytes())
}

/// Analyse a file from its bytes. Never fails on bad image data: problems are
/// recorded in `error` so the exact hash still counts.
pub fn analyze_bytes(format: Format, data: &[u8]) -> Analysis {
    let mut errors = Vec::new();
    let image_data_hash = image_data_hash(format, data)
        .map_err(|e| errors.push(format!("image data: {e}")))
        .ok();
    let exif = exif::read(data);
    let (width, height, hashes) =
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
                (w, h, hashes)
            }
            Err(e) => {
                errors.push(format!("decode: {e}"));
                (None, None, Vec::new())
            }
        };
    Analysis {
        blake3: blake3_of(data),
        image_data_hash,
        width,
        height,
        exif,
        hashes,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

pub fn analyze_path(path: &Path) -> Result<Analysis> {
    let format = Format::from_path(path)
        .ok_or_else(|| CoreError::Invalid(format!("unsupported file type: {}", path.display())))?;
    let data = std::fs::read(path).map_err(io(path))?;
    Ok(analyze_bytes(format, &data))
}
