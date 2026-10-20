//! TIFF image-data ranges and grayscale decode (8- and 16-bit).

use std::io::Cursor;
use std::ops::Range;

use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};

use crate::tiffparse::{
    TAG_STRIP_BYTE_COUNTS, TAG_STRIP_OFFSETS, TAG_TILE_BYTE_COUNTS, TAG_TILE_OFFSETS, Tiff,
};
use crate::{DecodeError, Gray};

/// Strip or tile byte ranges of the main image (IFD0), in file order.
pub fn image_data_ranges(data: &[u8]) -> Result<Vec<Range<usize>>, DecodeError> {
    let t = Tiff::parse(data)?;
    let ifd0 = t.ifd0()?;
    if let Some(r) = t.chunk_ranges(&ifd0, TAG_STRIP_OFFSETS, TAG_STRIP_BYTE_COUNTS)? {
        return Ok(r);
    }
    if let Some(r) = t.chunk_ranges(&ifd0, TAG_TILE_OFFSETS, TAG_TILE_BYTE_COUNTS)? {
        return Ok(r);
    }
    Err(DecodeError::Format("TIFF has no strips or tiles".into()))
}

/// 200 MP of 16-bit RGBA fits in 1.6 GB; anything larger is refused before
/// allocation.
fn limits() -> Limits {
    let mut l = Limits::default();
    l.decoding_buffer_size = (crate::MAX_PIXELS * 8) as usize;
    l.intermediate_buffer_size = 256 * 1024 * 1024;
    l
}

fn tiff_err(e: tiff::TiffError) -> DecodeError {
    DecodeError::Format(format!("TIFF: {e}"))
}

/// The ICC profile in IFD0 (tag 34675), if any.
pub fn icc_profile(data: &[u8]) -> Option<Vec<u8>> {
    let t = Tiff::parse(data).ok()?;
    let ifd0 = t.ifd0().ok()?;
    t.raw(ifd0.get(0x8773)?).ok().map(<[u8]>::to_vec)
}

/// Decode the first image to 8-bit RGB (16-bit samples are reduced to 8).
pub fn decode_rgb(data: &[u8]) -> Result<(crate::Rgb8, Option<Vec<u8>>), DecodeError> {
    let mut d = Decoder::new(Cursor::new(data))
        .map_err(tiff_err)?
        .with_limits(limits());
    let (w, h) = d.dimensions().map_err(tiff_err)?;
    crate::check_dimensions(w, h)?;
    let channels = match d.colortype().map_err(tiff_err)? {
        ColorType::Gray(_) => 1,
        ColorType::GrayA(_) => 2,
        ColorType::RGB(_) => 3,
        ColorType::RGBA(_) => 4,
        other => {
            return Err(DecodeError::Unsupported(format!(
                "TIFF colour type {other:?}"
            )));
        }
    };
    let px8: Vec<u8> = match d.read_image().map_err(tiff_err)? {
        DecodingResult::U8(v) => v,
        DecodingResult::U16(v) => v.into_iter().map(|s| (s >> 8) as u8).collect(),
        _ => return Err(DecodeError::Unsupported("TIFF sample format".into())),
    };
    let rgb: Vec<u8> = match channels {
        1 | 2 => px8
            .chunks_exact(channels)
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        3 => px8,
        n => px8
            .chunks_exact(n)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
    };
    Ok((crate::Rgb8::new(w, h, rgb)?, icc_profile(data)))
}

/// Decode the first image to 16-bit RGB if it has 16-bit samples (None
/// otherwise), with its ICC profile.
#[allow(clippy::type_complexity)]
pub fn decode_rgb16(
    data: &[u8],
) -> Result<Option<(crate::rgb16::Rgb16, Option<Vec<u8>>)>, DecodeError> {
    let mut d = Decoder::new(Cursor::new(data))
        .map_err(tiff_err)?
        .with_limits(limits());
    let (w, h) = d.dimensions().map_err(tiff_err)?;
    crate::check_dimensions(w, h)?;
    let channels = match d.colortype().map_err(tiff_err)? {
        ColorType::Gray(16) => 1,
        ColorType::GrayA(16) => 2,
        ColorType::RGB(16) => 3,
        ColorType::RGBA(16) => 4,
        _ => return Ok(None),
    };
    let DecodingResult::U16(px) = d.read_image().map_err(tiff_err)? else {
        return Ok(None);
    };
    let rgb = expand(&px, channels);
    Ok(Some((
        crate::rgb16::Rgb16::new(w, h, rgb)?,
        icc_profile(data),
    )))
}

/// Grey (with or without alpha), RGB or RGBA samples as RGB.
pub(crate) fn expand<T: Copy>(px: &[T], channels: usize) -> Vec<T> {
    match channels {
        1 | 2 => px
            .chunks_exact(channels)
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        3 => px.to_vec(),
        n => px
            .chunks_exact(n)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
    }
}

/// Decode the first image to 8-bit luma.
pub fn decode_luma(data: &[u8]) -> Result<Gray, DecodeError> {
    let mut d = Decoder::new(Cursor::new(data))
        .map_err(tiff_err)?
        .with_limits(limits());
    let (w, h) = d.dimensions().map_err(tiff_err)?;
    crate::check_dimensions(w, h)?;
    let ct = d.colortype().map_err(tiff_err)?;
    let channels = match ct {
        ColorType::Gray(_) => 1,
        ColorType::GrayA(_) => 2,
        ColorType::RGB(_) => 3,
        ColorType::RGBA(_) => 4,
        other => {
            return Err(DecodeError::Unsupported(format!(
                "TIFF colour type {other:?}"
            )));
        }
    };
    let px8: Vec<u8> = match d.read_image().map_err(tiff_err)? {
        DecodingResult::U8(v) => v,
        DecodingResult::U16(v) => v.into_iter().map(|s| (s >> 8) as u8).collect(),
        _ => return Err(DecodeError::Unsupported("TIFF sample format".into())),
    };
    let luma = match channels {
        1 => px8,
        2 => px8.as_chunks::<2>().0.iter().map(|p| p[0]).collect(),
        n => crate::gray::luma_from_rgb8(&px8, n),
    };
    Gray::new(w, h, luma)
}
