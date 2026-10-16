//! NEF embedded-preview extraction by walking the TIFF IFDs.

use std::ops::Range;

use crate::DecodeError;
use crate::tiffparse::{
    TAG_IMAGE_LENGTH, TAG_IMAGE_WIDTH, TAG_JPEG_IF_LENGTH, TAG_JPEG_IF_OFFSET, TAG_ORIENTATION,
    Tiff,
};

/// Byte range of the largest embedded JPEG (`JpgFromRaw`) in a NEF.
///
/// Every IFD in the file (IFD0 chain and SubIFDs) is considered; the one whose
/// JPEG is largest wins. For Nikon bodies that is the full-size preview.
pub fn jpg_from_raw(data: &[u8]) -> Result<Option<Range<usize>>, DecodeError> {
    Ok(embedded_jpegs(data)?.into_iter().max_by_key(|r| r.len()))
}

/// Byte ranges of every embedded JPEG referenced from the file's IFDs.
pub fn embedded_jpegs(data: &[u8]) -> Result<Vec<Range<usize>>, DecodeError> {
    let tiff = Tiff::parse(data)?;
    let mut out: Vec<Range<usize>> = Vec::new();
    for ifd in tiff.all_ifds()? {
        let (Some(off), Some(len)) = (
            tiff.uint(&ifd, TAG_JPEG_IF_OFFSET)?,
            tiff.uint(&ifd, TAG_JPEG_IF_LENGTH)?,
        ) else {
            continue;
        };
        let (off, len) = (off as usize, len as usize);
        let Some(bytes) = off.checked_add(len).and_then(|end| data.get(off..end)) else {
            continue; // out of bounds: corrupt pointer, ignore this IFD
        };
        if !bytes.starts_with(&[0xFF, 0xD8]) {
            continue;
        }
        if !out.contains(&(off..off + len)) {
            out.push(off..off + len);
        }
    }
    Ok(out)
}

/// Orientation and declared dimensions from IFD0 of a TIFF-based file.
pub fn ifd0_orientation(data: &[u8]) -> Result<Option<u16>, DecodeError> {
    let tiff = Tiff::parse(data)?;
    let ifd0 = tiff.ifd0()?;
    Ok(tiff
        .uint(&ifd0, TAG_ORIENTATION)?
        .and_then(|v| u16::try_from(v).ok()))
}

/// Width and height declared in IFD0.
pub fn ifd0_dimensions(data: &[u8]) -> Result<Option<(u32, u32)>, DecodeError> {
    let tiff = Tiff::parse(data)?;
    let ifd0 = tiff.ifd0()?;
    match (
        tiff.uint(&ifd0, TAG_IMAGE_WIDTH)?,
        tiff.uint(&ifd0, TAG_IMAGE_LENGTH)?,
    ) {
        (Some(w), Some(h)) => Ok(Some((w as u32, h as u32))),
        _ => Ok(None),
    }
}

/// The largest ImageWidth × ImageLength declared in any IFD (for a NEF, the
/// raw image; close enough to the JpgFromRaw size for layout).
pub fn max_dimensions(data: &[u8]) -> Result<Option<(u32, u32)>, DecodeError> {
    let tiff = Tiff::parse(data)?;
    let mut best: Option<(u32, u32)> = None;
    for ifd in tiff.all_ifds()? {
        if let (Some(w), Some(h)) = (
            tiff.uint(&ifd, TAG_IMAGE_WIDTH)?,
            tiff.uint(&ifd, TAG_IMAGE_LENGTH)?,
        ) {
            let (w, h) = (w as u32, h as u32);
            if best
                .is_none_or(|(bw, bh)| u64::from(w) * u64::from(h) > u64::from(bw) * u64::from(bh))
            {
                best = Some((w, h));
            }
        }
    }
    Ok(best)
}
