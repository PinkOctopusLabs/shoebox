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

fn tiff_err(e: tiff::TiffError) -> DecodeError {
    DecodeError::Format(format!("TIFF: {e}"))
}

/// Decode the first image to 8-bit luma.
pub fn decode_luma(data: &[u8]) -> Result<Gray, DecodeError> {
    let mut d = Decoder::new(Cursor::new(data))
        .map_err(tiff_err)?
        .with_limits(Limits::unlimited());
    let (w, h) = d.dimensions().map_err(tiff_err)?;
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
