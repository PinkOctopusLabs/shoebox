//! JPEG structure walking and grayscale decode.

use std::ops::Range;

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::{DecodeError, Gray};

const SOI: u8 = 0xD8;
const EOI: u8 = 0xD9;
const SOS: u8 = 0xDA;

fn seg_len(data: &[u8], at: usize) -> Option<usize> {
    let b = data.get(at..at + 2)?;
    Some(usize::from(u16::from_be_bytes([b[0], b[1]])))
}

/// The image data of a JPEG: from the first SOS marker through EOI, inclusive.
///
/// Header segments are walked by length, so an EXIF thumbnail in APP1 (which
/// has its own SOS) is never mistaken for the main image. After the first SOS,
/// entropy-coded data is scanned for markers; stuffed `FF 00`, restart markers
/// and fill bytes are skipped, and any other marker segment (DHT, further SOS in
/// progressive files) is skipped by its length. Bytes after EOI are excluded,
/// since cameras and carving tools sometimes append trailers.
pub fn scan_data_range(data: &[u8]) -> Result<Range<usize>, DecodeError> {
    if data.get(0..2) != Some(&[0xFF, SOI]) {
        return Err(DecodeError::Format("not a JPEG (no SOI)".into()));
    }
    let mut i = 2;
    // Header: marker segments until the first SOS.
    let start = loop {
        while data.get(i) == Some(&0xFF) && data.get(i + 1) == Some(&0xFF) {
            i += 1; // fill bytes
        }
        match (data.get(i), data.get(i + 1)) {
            (Some(0xFF), Some(&SOS)) => break i,
            (Some(0xFF), Some(_)) => {
                let len = seg_len(data, i + 2).ok_or(DecodeError::Truncated)?;
                i += 2 + len;
            }
            _ => return Err(DecodeError::Format("corrupt JPEG header".into())),
        }
    };
    // Skip the SOS header itself, then scan entropy-coded data.
    i = start + 2 + seg_len(data, start + 2).ok_or(DecodeError::Truncated)?;
    while i + 1 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        match data[i + 1] {
            0x00 | 0xFF | 0xD0..=0xD7 => i += 1,
            EOI => return Ok(start..i + 2),
            _ => {
                let len = seg_len(data, i + 2).ok_or(DecodeError::Truncated)?;
                i += 2 + len;
            }
        }
    }
    Err(DecodeError::Format("JPEG has no EOI".into()))
}

/// `(marker, payload range)` of every APPn/COM segment before the first SOS.
pub fn header_segments(data: &[u8]) -> Vec<(u8, Range<usize>)> {
    let mut out = Vec::new();
    if data.get(0..2) != Some(&[0xFF, SOI]) {
        return out;
    }
    let mut i = 2;
    loop {
        while data.get(i) == Some(&0xFF) && data.get(i + 1) == Some(&0xFF) {
            i += 1;
        }
        match (data.get(i), data.get(i + 1)) {
            (Some(0xFF), Some(&SOS)) | (Some(0xFF), Some(&EOI)) => return out,
            (Some(0xFF), Some(&m)) => {
                let Some(len) = seg_len(data, i + 2) else {
                    return out;
                };
                let (start, end) = (i + 4, i + 2 + len);
                if len < 2 || end > data.len() {
                    return out;
                }
                out.push((m, start..end));
                i = end;
            }
            _ => return out,
        }
    }
}

/// Byte ranges of the previews listed in a JPEG's MPF index (CIPA DC-007):
/// the images typed as large thumbnails, such as Nikon's 640×424 and full-HD
/// ones. The primary image (offset 0) is not included, and nor are other
/// kinds: an iPhone's Portrait-mode depth map and matte are listed too (as
/// "undefined"), grey pictures of the same size that are not the photo.
pub fn mpf_images(data: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    for (marker, r) in header_segments(data) {
        if marker != 0xE2 || !data[r.clone()].starts_with(b"MPF\0") {
            continue;
        }
        let base = r.start + 4; // offsets are relative to the MP endian field
        let Ok(tiff) = crate::tiffparse::Tiff::parse(&data[base..r.end]) else {
            continue;
        };
        let Ok(ifd) = tiff.ifd0() else { continue };
        let Some(entries) = ifd.get(0xB002).and_then(|e| tiff.raw(e).ok()) else {
            continue;
        };
        for chunk in entries.as_chunks::<16>().0 {
            let (Some(attr), Some(size), Some(off)) = (
                tiff.u32_in(chunk, 0),
                tiff.u32_in(chunk, 4),
                tiff.u32_in(chunk, 8),
            ) else {
                continue;
            };
            // Type code: 0x010001 large thumbnail (VGA), 0x010002 (full HD).
            if !matches!(attr & 0x00FF_FFFF, 0x01_0001 | 0x01_0002) {
                continue;
            }
            if off == 0 {
                continue; // the primary image
            }
            let start = base + off as usize;
            let end = start.saturating_add(size as usize);
            if end <= data.len() && data[start..].starts_with(&[0xFF, SOI]) {
                out.push(start..end);
            }
        }
    }
    out
}

/// Decode to 8-bit RGB, returning the embedded ICC profile if any.
pub fn decode_rgb(data: &[u8]) -> Result<(crate::Rgb8, Option<Vec<u8>>), DecodeError> {
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(1 << 15)
        .set_max_height(1 << 15);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), opts);
    d.decode_headers()
        .map_err(|e| DecodeError::Format(format!("JPEG: {e:?}")))?;
    let info = d.info().ok_or(DecodeError::Truncated)?;
    crate::check_dimensions(u32::from(info.width), u32::from(info.height))?;
    let icc = d.icc_profile();
    let px = d
        .decode()
        .map_err(|e| DecodeError::Format(format!("JPEG: {e:?}")))?;
    Ok((
        crate::Rgb8::new(u32::from(info.width), u32::from(info.height), px)?,
        icc,
    ))
}

/// Dimensions from the frame header, without decoding.
pub fn dimensions(data: &[u8]) -> Result<(u32, u32), DecodeError> {
    let mut d = JpegDecoder::new(ZCursor::new(data));
    d.decode_headers()
        .map_err(|e| DecodeError::Format(format!("JPEG: {e:?}")))?;
    let info = d.info().ok_or(DecodeError::Truncated)?;
    Ok((u32::from(info.width), u32::from(info.height)))
}

/// Full-resolution luma decode with zune-jpeg.
pub fn decode_luma(data: &[u8]) -> Result<Gray, DecodeError> {
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::Luma)
        .set_max_width(1 << 15)
        .set_max_height(1 << 15);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), opts);
    d.decode_headers()
        .map_err(|e| DecodeError::Format(format!("JPEG: {e:?}")))?;
    let info = d.info().ok_or(DecodeError::Truncated)?;
    // Check before decode() allocates the output buffer.
    crate::check_dimensions(u32::from(info.width), u32::from(info.height))?;
    let px = d
        .decode()
        .map_err(|e| DecodeError::Format(format!("JPEG: {e:?}")))?;
    let info = d.info().ok_or(DecodeError::Truncated)?;
    Gray::new(u32::from(info.width), u32::from(info.height), px)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JPEG whose MPF index lists the primary and these extra images
    /// (type code, bytes), appended after it.
    fn with_mpf(extra: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let n = extra.len() + 1;
        // MPF payload: "MPF\0", then a little-endian TIFF: header, an IFD
        // with MPEntry (0xB002) pointing at n 16-byte entries after it.
        let entries_at = 8 + 2 + 12 + 4;
        let payload_len = 4 + entries_at + 16 * n;
        let primary_len = 2 + 2 + 2 + payload_len + 2 + 2 + 2;
        let mut tiff = b"II*\0".to_vec();
        tiff.extend(8u32.to_le_bytes());
        tiff.extend(1u16.to_le_bytes());
        tiff.extend(0xB002u16.to_le_bytes());
        tiff.extend(7u16.to_le_bytes());
        tiff.extend((16 * n as u32).to_le_bytes());
        tiff.extend((entries_at as u32).to_le_bytes());
        tiff.extend(0u32.to_le_bytes());
        // Offsets count from the TIFF header (4 bytes into the payload).
        let base = 2 + 2 + 2 + 4;
        let entry = |attr: u32, size: usize, off: usize| {
            let mut e = attr.to_le_bytes().to_vec();
            e.extend((size as u32).to_le_bytes());
            e.extend((off as u32).to_le_bytes());
            e.extend([0; 4]);
            e
        };
        tiff.extend(entry(0x03_0000, primary_len, 0));
        let mut at = primary_len;
        for (code, img) in extra {
            tiff.extend(entry(*code, img.len(), at - base));
            at += img.len();
        }
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE2];
        j.extend(((payload_len + 2) as u16).to_be_bytes());
        j.extend(b"MPF\0");
        j.extend(tiff);
        j.extend([0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        assert_eq!(j.len(), primary_len);
        for (_, img) in extra {
            j.extend(img);
        }
        j
    }

    #[test]
    fn mpf_previews_are_large_thumbnails_only() {
        let img = |b: u8| vec![0xFF, 0xD8, b, b, 0xFF, 0xD9];
        let j = with_mpf(&[
            (0x01_0001, img(1)),
            (0x00_0000, img(2)),
            (0x01_0002, img(3)),
        ]);
        let found: Vec<u8> = mpf_images(&j).into_iter().map(|r| j[r.start + 2]).collect();
        // The depth map (type undefined) is left out.
        assert_eq!(found, [1, 3]);
    }

    #[test]
    fn rejects_non_jpeg() {
        assert!(scan_data_range(b"not a jpeg").is_err());
    }

    #[test]
    fn skips_app1_thumbnail_sos() {
        // SOI, APP1 containing a fake embedded SOS+EOI, then the real SOS.
        let mut j = vec![0xFF, 0xD8];
        let app1_body = [0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02, 0x11, 0xFF, 0xD9];
        j.extend([0xFF, 0xE1]);
        j.extend(((app1_body.len() + 2) as u16).to_be_bytes());
        j.extend(app1_body);
        let sos_at = j.len();
        j.extend([
            0xFF, 0xDA, 0x00, 0x02, 0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56,
        ]);
        j.extend([0xFF, 0xD9]);
        let eoi_end = j.len();
        j.extend(b"trailer");
        let r = scan_data_range(&j).unwrap();
        assert_eq!(r, sos_at..eoi_end);
    }
}
