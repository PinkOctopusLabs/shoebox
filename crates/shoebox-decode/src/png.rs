//! PNG image-data (IDAT) extraction and grayscale decode.

use std::io::Cursor;
use std::ops::Range;

use crate::{DecodeError, Gray};

const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Byte ranges of every IDAT chunk payload, in order.
pub fn idat_ranges(data: &[u8]) -> Result<Vec<Range<usize>>, DecodeError> {
    if !data.starts_with(SIGNATURE) {
        return Err(DecodeError::Format("not a PNG".into()));
    }
    let mut i = SIGNATURE.len();
    let mut out = Vec::new();
    while i + 8 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let kind = &data[i + 4..i + 8];
        let body = i + 8;
        let end = body.checked_add(len).ok_or(DecodeError::Truncated)?;
        if end + 4 > data.len() {
            return Err(DecodeError::Truncated);
        }
        if kind == b"IDAT" {
            out.push(body..end);
        }
        if kind == b"IEND" {
            break;
        }
        i = end + 4; // skip CRC
    }
    if out.is_empty() {
        return Err(DecodeError::Format("PNG has no IDAT".into()));
    }
    Ok(out)
}

/// Decode to 8-bit RGB, returning the embedded ICC profile (iCCP) if any.
pub fn decode_rgb(data: &[u8]) -> Result<(crate::Rgb8, Option<Vec<u8>>), DecodeError> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec
        .read_info()
        .map_err(|e| DecodeError::Format(format!("PNG: {e}")))?;
    crate::check_dimensions(reader.info().width, reader.info().height)?;
    let icc = reader.info().icc_profile.as_ref().map(|c| c.to_vec());
    let size = reader.output_buffer_size().ok_or(DecodeError::Truncated)?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| DecodeError::Format(format!("PNG: {e}")))?;
    buf.truncate(info.buffer_size());
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Grayscale => buf.iter().flat_map(|&v| [v, v, v]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        png::ColorType::Rgb => buf,
        png::ColorType::Rgba => buf
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        png::ColorType::Indexed => {
            return Err(DecodeError::Unsupported("indexed PNG after EXPAND".into()));
        }
    };
    Ok((crate::Rgb8::new(info.width, info.height, rgb)?, icc))
}

/// Decode to 16-bit RGB if the PNG has 16-bit samples (None otherwise),
/// with its ICC profile.
#[allow(clippy::type_complexity)]
pub fn decode_rgb16(
    data: &[u8],
) -> Result<Option<(crate::rgb16::Rgb16, Option<Vec<u8>>)>, DecodeError> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND);
    let mut reader = dec
        .read_info()
        .map_err(|e| DecodeError::Format(format!("PNG: {e}")))?;
    if reader.info().bit_depth != png::BitDepth::Sixteen {
        return Ok(None);
    }
    crate::check_dimensions(reader.info().width, reader.info().height)?;
    let icc = reader.info().icc_profile.as_ref().map(|c| c.to_vec());
    let size = reader.output_buffer_size().ok_or(DecodeError::Truncated)?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| DecodeError::Format(format!("PNG: {e}")))?;
    buf.truncate(info.buffer_size());
    let px: Vec<u16> = buf
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_be_bytes(*b))
        .collect();
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Ok(None),
    };
    let rgb = crate::tiff::expand(&px, channels);
    Ok(Some((
        crate::rgb16::Rgb16::new(info.width, info.height, rgb)?,
        icc,
    )))
}

/// The XMP packet in an uncompressed `iTXt` chunk keyed `XML:com.adobe.xmp`.
pub fn itxt_xmp(data: &[u8]) -> Option<Range<usize>> {
    const KEY: &[u8] = b"XML:com.adobe.xmp\0";
    if !data.starts_with(SIGNATURE) {
        return None;
    }
    let mut i = SIGNATURE.len();
    while i + 8 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        let body = i + 8;
        let end = body.checked_add(len)?;
        if end > data.len() {
            return None;
        }
        if &data[i + 4..i + 8] == b"iTXt" && data[body..end].starts_with(KEY) {
            // keyword\0, compression flag, method, language\0, translated\0, text
            let mut p = body + KEY.len();
            if data.get(p) != Some(&0) {
                return None; // compressed: not supported
            }
            p += 2;
            for _ in 0..2 {
                p += data[p..end].iter().position(|&c| c == 0)? + 1;
            }
            return Some(p..end);
        }
        if &data[i + 4..i + 8] == b"IEND" {
            return None;
        }
        i = end + 4;
    }
    None
}

pub fn decode_luma(data: &[u8]) -> Result<Gray, DecodeError> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec
        .read_info()
        .map_err(|e| DecodeError::Format(format!("PNG: {e}")))?;
    let size = reader.output_buffer_size().ok_or(DecodeError::Truncated)?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| DecodeError::Format(format!("PNG: {e}")))?;
    buf.truncate(info.buffer_size());
    let luma = match info.color_type {
        png::ColorType::Grayscale => buf,
        png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().map(|p| p[0]).collect(),
        png::ColorType::Rgb => crate::gray::luma_from_rgb8(&buf, 3),
        png::ColorType::Rgba => crate::gray::luma_from_rgb8(&buf, 4),
        png::ColorType::Indexed => {
            return Err(DecodeError::Unsupported("indexed PNG after EXPAND".into()));
        }
    };
    Gray::new(info.width, info.height, luma)
}
