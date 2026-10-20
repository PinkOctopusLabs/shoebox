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

/// How a NEF's raw data is compressed (Nikon's `NEFCompression`), which
/// decides how Develop decodes it: rawler reads the classic modes, while High
/// efficiency files go through a DNG until an open decoder reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NefCompression {
    Lossless,
    Uncompressed,
    Lossy,
    HighEfficiency,
    HighEfficiencyStar,
    /// A mode this list doesn't name, by its tag value.
    Other(u16),
}

impl NefCompression {
    pub fn from_tag(v: u16) -> Self {
        match v {
            3 => Self::Lossless,
            2 | 5..=7 | 9 | 10 => Self::Uncompressed,
            1 | 4 | 8 => Self::Lossy,
            13 => Self::HighEfficiency,
            14 => Self::HighEfficiencyStar,
            v => Self::Other(v),
        }
    }

    pub fn tag(self) -> u16 {
        match self {
            Self::Lossless => 3,
            Self::Uncompressed => 2,
            Self::Lossy => 4,
            Self::HighEfficiency => 13,
            Self::HighEfficiencyStar => 14,
            Self::Other(v) => v,
        }
    }

    /// Whether rawler (or any released open decoder) can't read it, so
    /// editing needs a DNG made by Adobe's converter.
    pub fn needs_dng(self) -> bool {
        matches!(self, Self::HighEfficiency | Self::HighEfficiencyStar)
    }

    /// How the Info panel names it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Lossless => "Lossless",
            Self::Uncompressed => "Uncompressed",
            Self::Lossy => "Lossy",
            Self::HighEfficiency => "High efficiency",
            Self::HighEfficiencyStar => "High efficiency★",
            Self::Other(_) => "Other",
        }
    }
}

const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_MAKER_NOTE: u16 = 0x927C;
/// Z bodies: a block whose u16 at byte 10 is the compression.
const NIKON_MAKERNOTES_0X51: u16 = 0x0051;
/// Older bodies: `NEFCompression` itself.
const NIKON_NEF_COMPRESSION: u16 = 0x0093;

/// The NEF's compression, from Nikon's maker note (None if it has no maker
/// note or no compression tag, as for a file that isn't a NEF).
pub fn compression(data: &[u8]) -> Result<Option<NefCompression>, DecodeError> {
    let tiff = Tiff::parse(data)?;
    let Some(exif_at) = tiff.uint(&tiff.ifd0()?, TAG_EXIF_IFD)? else {
        return Ok(None);
    };
    let (exif, _) = tiff.read_ifd(exif_at as usize)?;
    let Some(note) = exif.get(TAG_MAKER_NOTE) else {
        return Ok(None);
    };
    Ok(compression_in_maker_note(tiff.raw(note)?))
}

/// The compression recorded in a Nikon type-3 maker note: "Nikon\0", a
/// two-byte version, two spare bytes, then a TIFF whose offsets count from
/// its own header.
pub fn compression_in_maker_note(note: &[u8]) -> Option<NefCompression> {
    let mn = Tiff::parse(note.strip_prefix(b"Nikon\0")?.get(4..)?).ok()?;
    let ifd = mn.ifd0().ok()?;
    if let Some(e) = ifd.get(NIKON_MAKERNOTES_0X51) {
        // rawler reads it little-endian whatever the maker note's order.
        let v = mn.raw(e).ok()?.get(10..12)?;
        return Some(NefCompression::from_tag(u16::from_le_bytes([v[0], v[1]])));
    }
    mn.uint(&ifd, NIKON_NEF_COMPRESSION)
        .ok()?
        .and_then(|v| u16::try_from(v).ok())
        .map(NefCompression::from_tag)
}

/// Nikon's `NikonNEFInfo` (tag 0xC7D5, in the raw image's SubIFD): "Nikon\0", a version, then a
/// TIFF whose tag 5 is DistortionInfo and tag 6 VignetteInfo.
const NIKON_NEF_INFO: u16 = 0xC7D5;
const NEF_INFO_DISTORTION: u16 = 0x0005;
const NEF_INFO_VIGNETTE: u16 = 0x0006;

/// The lens corrections a Nikon Z body records for its lens. Both are
/// polynomials in r², r = 1 at the half-diagonal of the frame (the blocks
/// store that radius, 21.6 mm):
/// - `distortion` `[k1, k2, k3]`: a pixel at radius r of the corrected image
///   comes from radius r·(1 + k1·r² + k2·r⁴ + k3·r⁶) of the raw.
/// - `vignette` `[v1, v2, v3]`: the gain that corrects the falloff,
///   1 + v1·r² + v2·r⁴ + v3·r⁶.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LensData {
    pub distortion: Option<[f64; 3]>,
    pub vignette: Option<[f64; 3]>,
}

/// The NEF's [`LensData`] (empty for other files, or a lens that sends none).
pub fn lens_data(data: &[u8]) -> LensData {
    lens_data_in(data).unwrap_or_default()
}

fn lens_data_in(data: &[u8]) -> Option<LensData> {
    let tiff = Tiff::parse(data).ok()?;
    // In the raw image's SubIFD (any IFD that has it).
    let ifds = tiff.all_ifds().ok()?;
    let e = ifds.iter().find_map(|ifd| ifd.get(NIKON_NEF_INFO))?;
    let block = tiff.raw(e).ok()?;
    let inner = block.strip_prefix(b"Nikon\0")?.get(4..)?;
    let info = Tiff::parse(inner).ok()?;
    let ifd = info.ifd0().ok()?;
    let big = inner.starts_with(b"MM");
    let coefficients = |tag: u16, at: [usize; 3]| -> Option<[f64; 3]> {
        let b = info.raw(ifd.get(tag)?).ok()?;
        let i32_at = |o: usize| -> Option<i32> {
            let v: [u8; 4] = b.get(o..o + 4)?.try_into().ok()?;
            Some(if big {
                i32::from_be_bytes(v)
            } else {
                i32::from_le_bytes(v)
            })
        };
        let rational = |o: usize| -> Option<f64> {
            let (n, d) = (i32_at(o)?, i32_at(o + 4)?);
            (d != 0).then(|| f64::from(n) / f64::from(d))
        };
        let k = [rational(at[0])?, rational(at[1])?, rational(at[2])?];
        (k.iter().any(|v| *v != 0.0) && k.iter().all(|v| v.abs() < 16.0)).then_some(k)
    };
    Some(LensData {
        distortion: coefficients(NEF_INFO_DISTORTION, [0x14, 0x1C, 0x24]),
        vignette: coefficients(NEF_INFO_VIGNETTE, [0x24, 0x34, 0x44]),
    })
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
