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
