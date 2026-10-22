//! DNG files in the library (Adobe's DNG specification 1.7): the previews a
//! DNG carries, its raw image's size and data (for the image-data hash),
//! and what Shoebox's own linear DNGs record. A camera DNG and a linear one
//! are read the same way here; rendering from the raw data, when no preview
//! is big enough, is in `lib.rs`.

use std::ops::Range;

use crate::tiffparse::{
    Ifd, TAG_COMPRESSION, TAG_IMAGE_LENGTH, TAG_IMAGE_WIDTH, TAG_JPEG_IF_LENGTH,
    TAG_JPEG_IF_OFFSET, TAG_NEW_SUBFILE_TYPE, TAG_STRIP_BYTE_COUNTS, TAG_STRIP_OFFSETS,
    TAG_TILE_BYTE_COUNTS, TAG_TILE_OFFSETS, Tiff,
};
use crate::{DecodeError, Rgb8};

const TAG_BITS_PER_SAMPLE: u16 = 0x0102;
const TAG_PHOTOMETRIC: u16 = 0x0106;
const TAG_SAMPLES_PER_PIXEL: u16 = 0x0115;
const TAG_PLANAR_CONFIGURATION: u16 = 0x011C;
const TAG_DEFAULT_CROP_SIZE: u16 = 0xC620;
const TAG_NOISE_REDUCTION_APPLIED: u16 = 0xC6F7;
/// NewSubFileType of the main (raw) image, and of a reduced-resolution one.
const MAIN_IMAGE: u64 = 0;
const REDUCED: u64 = 1;
const PHOTOMETRIC_RGB: u64 = 2;
const PHOTOMETRIC_LINEAR_RAW: u64 = 34892;

/// A reduced-resolution image a DNG carries for viewers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// A baseline JPEG stream (Adobe DNG Converter's previews).
    Jpeg(Range<usize>),
    /// Uncompressed 8-bit RGB, row by row (IFD 0's thumbnail).
    Rgb(Range<usize>),
}

/// A preview and its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub preview: Preview,
    pub width: u32,
    pub height: u32,
}

impl Found {
    pub fn long_edge(&self) -> u32 {
        self.width.max(self.height)
    }
}

/// Every preview in the file (IFD 0 and its chain, and the SubIFDs): the
/// reduced-resolution images (NewSubFileType 1) stored as baseline JPEG or
/// uncompressed 8-bit RGB. Others (16-bit, YCbCr strips) are skipped.
pub fn previews(data: &[u8]) -> Result<Vec<Found>, DecodeError> {
    let t = Tiff::parse(data)?;
    let mut out: Vec<Found> = Vec::new();
    for ifd in t.all_ifds()? {
        if t.uint(&ifd, TAG_NEW_SUBFILE_TYPE)? != Some(REDUCED) {
            continue;
        }
        // A preview that doesn't parse is skipped, not the file.
        if let Ok(Some(f)) = preview_in(&t, &ifd)
            && !out.contains(&f)
        {
            out.push(f);
        }
    }
    Ok(out)
}

fn preview_in(t: &Tiff, ifd: &Ifd) -> Result<Option<Found>, DecodeError> {
    let data = t.data();
    let strips = t.chunk_ranges(ifd, TAG_STRIP_OFFSETS, TAG_STRIP_BYTE_COUNTS)?;
    match t.uint(ifd, TAG_COMPRESSION)?.unwrap_or(1) {
        // One JPEG stream: the strip, or the old-style interchange format.
        6 | 7 => {
            let r = match (
                t.uint(ifd, TAG_JPEG_IF_OFFSET)?,
                t.uint(ifd, TAG_JPEG_IF_LENGTH)?,
                strips.as_deref(),
            ) {
                (Some(o), Some(l), _) => {
                    let (o, l) = (o as usize, l as usize);
                    o..o.checked_add(l).ok_or(DecodeError::Truncated)?
                }
                (_, _, Some([r])) => r.clone(),
                _ => return Ok(None),
            };
            let bytes = data.get(r.clone()).ok_or(DecodeError::Truncated)?;
            if !bytes.starts_with(&[0xFF, 0xD8]) {
                return Ok(None);
            }
            let (width, height) = crate::jpeg::dimensions(bytes)?;
            Ok(Some(Found {
                preview: Preview::Jpeg(r),
                width,
                height,
            }))
        }
        1 => {
            let (Some(w), Some(h), Some(strips)) = (
                t.uint(ifd, TAG_IMAGE_WIDTH)?,
                t.uint(ifd, TAG_IMAGE_LENGTH)?,
                strips,
            ) else {
                return Ok(None);
            };
            let bits = match ifd.get(TAG_BITS_PER_SAMPLE) {
                Some(e) => t.uints(e)?,
                None => vec![1],
            };
            let rgb8 = t.uint(ifd, TAG_SAMPLES_PER_PIXEL)? == Some(3)
                && t.uint(ifd, TAG_PHOTOMETRIC)? == Some(PHOTOMETRIC_RGB)
                && t.uint(ifd, TAG_PLANAR_CONFIGURATION)?.unwrap_or(1) == 1
                && !bits.is_empty()
                && bits.iter().all(|&b| b == 8);
            // The strips must follow each other: one block of pixels.
            let contiguous = strips.windows(2).all(|p| p[0].end == p[1].start);
            let (Some(first), Some(last)) = (strips.first(), strips.last()) else {
                return Ok(None);
            };
            let (w, h) = (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0));
            let len = u64::from(w) * u64::from(h) * 3;
            if !rgb8 || !contiguous || w == 0 || h == 0 || (last.end - first.start) as u64 != len {
                return Ok(None);
            }
            Ok(Some(Found {
                preview: Preview::Rgb(first.start..last.end),
                width: w,
                height: h,
            }))
        }
        _ => Ok(None),
    }
}

/// A preview's pixels (not oriented) and its ICC profile, if any.
pub fn decode_preview(data: &[u8], f: &Found) -> Result<(Rgb8, Option<Vec<u8>>), DecodeError> {
    match &f.preview {
        Preview::Jpeg(r) => {
            crate::jpeg::decode_rgb(data.get(r.clone()).ok_or(DecodeError::Truncated)?)
        }
        Preview::Rgb(r) => {
            let px = data.get(r.clone()).ok_or(DecodeError::Truncated)?;
            // DNG previews are sRGB unless a PreviewColorSpace says
            // otherwise; Adobe's and Shoebox's are sRGB.
            Ok((Rgb8::new(f.width, f.height, px.to_vec())?, None))
        }
    }
}

/// The main image's IFD: NewSubFileType 0, the largest if there are several.
fn main_ifd(t: &Tiff) -> Result<Ifd, DecodeError> {
    let mut best: Option<(u64, Ifd)> = None;
    for ifd in t.all_ifds()? {
        if t.uint(&ifd, TAG_NEW_SUBFILE_TYPE)?.unwrap_or(MAIN_IMAGE) != MAIN_IMAGE {
            continue;
        }
        let (Some(w), Some(h)) = (
            t.uint(&ifd, TAG_IMAGE_WIDTH)?,
            t.uint(&ifd, TAG_IMAGE_LENGTH)?,
        ) else {
            continue;
        };
        let area = w.saturating_mul(h);
        if best.as_ref().is_none_or(|(a, _)| area > *a) {
            best = Some((area, ifd));
        }
    }
    best.map(|(_, ifd)| ifd)
        .ok_or_else(|| DecodeError::Format("DNG has no main image".into()))
}

/// The image's stored size (orientation not applied): the raw image's
/// DefaultCropSize, or its whole size without one.
pub fn dimensions(data: &[u8]) -> Result<(u32, u32), DecodeError> {
    let t = Tiff::parse(data)?;
    let ifd = main_ifd(&t)?;
    let whole = (
        t.uint(&ifd, TAG_IMAGE_WIDTH)?.unwrap_or(0),
        t.uint(&ifd, TAG_IMAGE_LENGTH)?.unwrap_or(0),
    );
    let crop = match ifd.get(TAG_DEFAULT_CROP_SIZE) {
        Some(e) => match t.numbers(e)?.as_slice() {
            [Some(w), Some(h)] if *w >= 1.0 && *h >= 1.0 => {
                Some((w.round() as u64, h.round() as u64))
            }
            _ => None,
        },
        None => None,
    };
    let (w, h) = crop.unwrap_or(whole);
    let (w, h) = (
        u32::try_from(w.min(whole.0)).unwrap_or(0),
        u32::try_from(h.min(whole.1)).unwrap_or(0),
    );
    if w == 0 || h == 0 {
        return Err(DecodeError::Format("DNG has no image size".into()));
    }
    Ok((w, h))
}

/// The raw image's strips or tiles, in file order: what the DNG's
/// image-data hash covers (DECISIONS, "DNG and AVIF in the library"), so
/// two DNGs of the same raw data with different metadata or previews match.
pub fn raw_data_ranges(data: &[u8]) -> Result<Vec<Range<usize>>, DecodeError> {
    let t = Tiff::parse(data)?;
    let ifd = main_ifd(&t)?;
    if let Some(r) = t.chunk_ranges(&ifd, TAG_TILE_OFFSETS, TAG_TILE_BYTE_COUNTS)? {
        return Ok(r);
    }
    if let Some(r) = t.chunk_ranges(&ifd, TAG_STRIP_OFFSETS, TAG_STRIP_BYTE_COUNTS)? {
        return Ok(r);
    }
    Err(DecodeError::Format("DNG has no image data".into()))
}

/// Whether the main image is LinearRaw: already demosaiced (Shoebox's
/// linear DNG exports, Adobe's linear DNGs).
pub fn is_linear(data: &[u8]) -> bool {
    let Ok(t) = Tiff::parse(data) else {
        return false;
    };
    main_ifd(&t)
        .ok()
        .and_then(|ifd| t.uint(&ifd, TAG_PHOTOMETRIC).ok().flatten())
        == Some(PHOTOMETRIC_LINEAR_RAW)
}

/// NoiseReductionApplied (0–1; DNG 1.2): how much noise reduction the raw
/// data has had. Shoebox's linear DNGs record their AI denoise in it.
pub fn noise_reduction_applied(data: &[u8]) -> Option<f32> {
    let t = Tiff::parse(data).ok()?;
    t.all_ifds().ok()?.iter().find_map(|ifd| {
        let e = ifd.get(TAG_NOISE_REDUCTION_APPLIED)?;
        let v = (*t.numbers(e).ok()?.first()?)?;
        (v.is_finite() && v >= 0.0).then_some(v.min(1.0) as f32)
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn not_a_dng() {
        assert!(previews(b"nope").is_err());
        assert!(!is_linear(b"nope"));
        assert_eq!(noise_reduction_applied(b"II*\0\0\0\0\0"), None);
    }
}
