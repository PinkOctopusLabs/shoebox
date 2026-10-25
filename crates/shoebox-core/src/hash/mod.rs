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
    if format.is_movie() {
        return analyze_movie(path, format);
    }
    let data = std::fs::read(path).map_err(io(path))?;
    Ok(analyze_bytes(format, &data))
}

/// A movie: exact copies only (BLAKE3, streamed, so a big clip isn't read
/// into memory), with its size and date as browsing reads them (the cache
/// keeps this analysis's as the file's metadata). No perceptual hashes.
fn analyze_movie(path: &Path, format: Format) -> Result<Analysis> {
    let meta = crate::meta::read(path, format).unwrap_or_default();
    let mut f = std::fs::File::open(path).map_err(io(path))?;
    let mut h = blake3::Hasher::new();
    h.update_reader(&mut f).map_err(io(path))?;
    Ok(Analysis {
        blake3: *h.finalize().as_bytes(),
        image_data_hash: None,
        width: meta.width,
        height: meta.height,
        exif: meta.exif,
        hashes: Vec::new(),
        luma_stddev: None,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache keeps a movie's analysis as its metadata, so it must keep
    /// the date (it once didn't, and every hashed movie became undated).
    #[test]
    fn a_movies_analysis_keeps_its_date() {
        let bx = |kind: &[u8], body: &[u8]| {
            let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
            v.extend_from_slice(kind);
            v.extend_from_slice(body);
            v
        };
        let mut mvhd = vec![0u8; 100];
        let qt = (1_556_000_000_i64 + 2_082_844_800) as u32;
        mvhd[4..8].copy_from_slice(&qt.to_be_bytes());
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("clip.mov");
        std::fs::write(
            &path,
            [bx(b"ftyp", b"qt  "), bx(b"moov", &bx(b"mvhd", &mvhd))].concat(),
        )
        .unwrap();
        let a = analyze_path(&path).unwrap();
        assert_eq!(
            a.exif.capture_time,
            crate::meta::read(&path, Format::Mov)
                .unwrap()
                .exif
                .capture_time
        );
        assert!(a.exif.capture_time.is_some());
    }
}
