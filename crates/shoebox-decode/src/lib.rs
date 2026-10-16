//! Shoebox decoding: JPEG, TIFF, PNG and NEF embedded previews.
//!
//! Everything here is read-only and works on in-memory bytes.

pub mod gray;
pub mod jpeg;
pub mod nef;
pub mod png;
pub mod tiff;
pub mod tiffparse;

use std::path::Path;

pub use gray::Gray;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("truncated or out-of-bounds data")]
    Truncated,
    #[error("{0}")]
    Format(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// File formats the library understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Nef,
    Jpeg,
    Tiff,
    Png,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "nef" => Format::Nef,
            "jpg" | "jpeg" => Format::Jpeg,
            "tif" | "tiff" => Format::Tiff,
            "png" => Format::Png,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Nef => "NEF",
            Format::Jpeg => "JPG",
            Format::Tiff => "TIF",
            Format::Png => "PNG",
        }
    }
}

/// The bytes whose pixels represent the file: the file itself, or for a NEF
/// its embedded full-size JPEG (`JpgFromRaw`).
pub fn display_source(format: Format, data: &[u8]) -> Result<(Format, &[u8]), DecodeError> {
    match format {
        Format::Nef => {
            let r = nef::jpg_from_raw(data)?
                .ok_or_else(|| DecodeError::Format("NEF has no embedded JPEG".into()))?;
            Ok((Format::Jpeg, &data[r]))
        }
        f => Ok((f, data)),
    }
}

/// Full-resolution 8-bit luma of a file's display source (orientation not applied).
pub fn decode_luma(format: Format, data: &[u8]) -> Result<Gray, DecodeError> {
    let (format, data) = display_source(format, data)?;
    match format {
        Format::Jpeg | Format::Nef => jpeg::decode_luma(data),
        Format::Tiff => tiff::decode_luma(data),
        Format::Png => png::decode_luma(data),
    }
}

/// Luma thumbnail with the given long edge (orientation not applied).
pub fn luma_thumbnail(format: Format, data: &[u8], long_edge: u32) -> Result<Gray, DecodeError> {
    Ok(decode_luma(format, data)?.fit_long_edge(long_edge))
}

/// Concatenation order of byte ranges that make up the image data, without
/// metadata: JPEG SOS..EOI, TIFF strips/tiles, PNG IDAT payloads. For a NEF this
/// is the image data of its embedded JPEG, so a byte-identical carved preview
/// produces the same hash.
pub fn image_data_ranges(
    format: Format,
    data: &[u8],
) -> Result<Vec<std::ops::Range<usize>>, DecodeError> {
    match format {
        Format::Jpeg => Ok(vec![jpeg::scan_data_range(data)?]),
        Format::Nef => {
            let outer = nef::jpg_from_raw(data)?
                .ok_or_else(|| DecodeError::Format("NEF has no embedded JPEG".into()))?;
            let inner = jpeg::scan_data_range(&data[outer.clone()])?;
            let abs = outer.start + inner.start..outer.start + inner.end;
            Ok(std::iter::once(abs).collect())
        }
        Format::Tiff => tiff::image_data_ranges(data),
        Format::Png => png::idat_ranges(data),
    }
}

/// Pixel dimensions of the display source, from headers only where possible.
pub fn dimensions(format: Format, data: &[u8]) -> Result<(u32, u32), DecodeError> {
    let (format, data) = display_source(format, data)?;
    match format {
        Format::Jpeg | Format::Nef => jpeg::dimensions(data),
        Format::Tiff => nef::ifd0_dimensions(data)?
            .ok_or_else(|| DecodeError::Format("TIFF has no dimensions".into())),
        Format::Png => {
            let d = data.get(16..24).ok_or(DecodeError::Truncated)?;
            Ok((
                u32::from_be_bytes([d[0], d[1], d[2], d[3]]),
                u32::from_be_bytes([d[4], d[5], d[6], d[7]]),
            ))
        }
    }
}
