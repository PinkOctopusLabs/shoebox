//! Fast metadata for browsing: EXIF, upright dimensions and orientation,
//! read from the start of the file where possible.

use std::io::Read;
use std::path::Path;

use shoebox_decode::Format;

use crate::error::{Result, io};
use crate::exif::{self, ExifInfo};

/// Bump when metadata extraction changes, so cached rows are re-read.
pub const META_VERSION: i64 = 1;

/// Real NEFs and Lightroom TIFFs keep IFD0, the Exif IFD, XMP and the ICC
/// profile in their first ~40 KB; JPEG EXIF lives in APP1 at the start.
const PREFIX: u64 = 256 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    /// Upright (orientation applied) pixel size, when known.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub exif: ExifInfo,
}

impl Meta {
    pub fn orientation(&self) -> u16 {
        self.exif.orientation.unwrap_or(1)
    }

    /// Capture time in seconds (for sorting), if known.
    pub fn capture_seconds(&self) -> Option<f64> {
        exif::capture_seconds(
            self.exif.capture_time.as_deref()?,
            self.exif.subsec.as_deref(),
        )
    }

    /// Width / height of the upright image, if known.
    pub fn aspect(&self) -> Option<f32> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some(w as f32 / h as f32),
            _ => None,
        }
    }
}

/// Upright size from stored (sensor/file) size and EXIF orientation.
pub fn upright(dims: Option<(u32, u32)>, orientation: Option<u16>) -> (Option<u32>, Option<u32>) {
    match (dims, orientation) {
        (Some((w, h)), Some(5..=8)) => (Some(h), Some(w)),
        (Some((w, h)), _) => (Some(w), Some(h)),
        (None, _) => (None, None),
    }
}

fn dims_from(format: Format, data: &[u8]) -> Option<(u32, u32)> {
    match format {
        Format::Jpeg => shoebox_decode::jpeg::dimensions(data).ok(),
        Format::Png => shoebox_decode::dimensions(Format::Png, data).ok(),
        Format::Tiff => shoebox_decode::nef::ifd0_dimensions(data).ok().flatten(),
        // The largest image described in any IFD (the raw or JpgFromRaw).
        Format::Nef => shoebox_decode::nef::max_dimensions(data).ok().flatten(),
        Format::Heic => None,
    }
}

fn from_bytes(format: Format, data: &[u8]) -> (Meta, bool) {
    let exif = exif::read(data);
    let dims = dims_from(format, data);
    let (width, height) = upright(dims, exif.orientation);
    // Complete if we found dimensions (HEIC: EXIF is enough).
    let complete = dims.is_some() || (format == Format::Heic && exif != ExifInfo::default());
    (
        Meta {
            width,
            height,
            exif,
        },
        complete,
    )
}

/// Read metadata, trying the first 256 KB before falling back to the whole file.
pub fn read(path: &Path, format: Format) -> Result<Meta> {
    let mut f = std::fs::File::open(path).map_err(io(path))?;
    let mut prefix = Vec::with_capacity(PREFIX as usize);
    (&mut f)
        .take(PREFIX)
        .read_to_end(&mut prefix)
        .map_err(io(path))?;
    let (meta, complete) = from_bytes(format, &prefix);
    if complete || (prefix.len() as u64) < PREFIX {
        return Ok(meta);
    }
    let all = std::fs::read(path).map_err(io(path))?;
    Ok(from_bytes(format, &all).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upright_swaps_for_rotations() {
        assert_eq!(
            upright(Some((6000, 4000)), Some(6)),
            (Some(4000), Some(6000))
        );
        assert_eq!(
            upright(Some((6000, 4000)), Some(1)),
            (Some(6000), Some(4000))
        );
        assert_eq!(upright(Some((6000, 4000)), None), (Some(6000), Some(4000)));
    }
}
