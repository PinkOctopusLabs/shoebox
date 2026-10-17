//! Shoebox decoding: JPEG, TIFF, PNG and NEF embedded previews, and movie
//! poster frames.
//!
//! Everything here is read-only. Images decode from in-memory bytes; movies
//! from their path ([`render_path`]), since a poster frame shouldn't need the
//! whole file.

pub mod gray;
#[cfg(target_os = "macos")]
pub mod imageio;
pub mod jpeg;
pub mod mov;
pub mod nef;
pub mod png;
pub mod rgb;
pub mod tiff;
pub mod tiffparse;

use std::path::Path;

pub use gray::Gray;
pub use rgb::{Rgb8, Target};

/// Largest image we will decode (200 MP). Bounds memory for hostile or corrupt
/// headers; a Z f frame is 24 MP.
pub const MAX_PIXELS: u64 = 200_000_000;

/// Refuse dimensions whose decode buffer would exceed [`MAX_PIXELS`].
pub fn check_dimensions(width: u32, height: u32) -> Result<(), DecodeError> {
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(DecodeError::Unsupported(format!(
            "image dimensions {width}x{height} exceed the {MAX_PIXELS}-pixel limit"
        )));
    }
    Ok(())
}

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
    /// HEIC/HEIF: decoded by the platform (ImageIO on macOS); unsupported
    /// elsewhere until a libheif backend exists.
    Heic,
    /// QuickTime movie (a Live Photo's video, or a clip): a poster frame
    /// through AVFoundation on macOS; see [`render_path`].
    Mov,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "nef" => Format::Nef,
            "jpg" | "jpeg" => Format::Jpeg,
            "tif" | "tiff" => Format::Tiff,
            "png" => Format::Png,
            "heic" | "heif" | "hif" => Format::Heic,
            "mov" => Format::Mov,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Nef => "NEF",
            Format::Jpeg => "JPG",
            Format::Tiff => "TIF",
            Format::Png => "PNG",
            Format::Heic => "HEIC",
            Format::Mov => "MOV",
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

#[cfg(not(target_os = "macos"))]
fn heic_unsupported() -> DecodeError {
    DecodeError::Unsupported(
        "HEIC decoding needs macOS ImageIO (libheif backend not built yet)".into(),
    )
}

/// Full-resolution 8-bit luma of a file's display source (orientation not applied).
pub fn decode_luma(format: Format, data: &[u8]) -> Result<Gray, DecodeError> {
    let (format, data) = display_source(format, data)?;
    match format {
        Format::Jpeg | Format::Nef => jpeg::decode_luma(data),
        Format::Tiff => tiff::decode_luma(data),
        Format::Png => png::decode_luma(data),
        Format::Heic => heic_luma(data, 1 << 14),
        Format::Mov => Err(video_from_bytes()),
    }
}

fn video_from_bytes() -> DecodeError {
    DecodeError::Unsupported("a movie's picture comes from its path (render_path)".into())
}

#[cfg(target_os = "macos")]
fn heic_luma(data: &[u8], max: u32) -> Result<Gray, DecodeError> {
    imageio::thumbnail_luma(data, max, false, Some(imageio::type_hint(Format::Heic)))
}

#[cfg(not(target_os = "macos"))]
fn heic_luma(_data: &[u8], _max: u32) -> Result<Gray, DecodeError> {
    Err(heic_unsupported())
}

/// Luma thumbnail with the given long edge (orientation not applied).
pub fn luma_thumbnail(format: Format, data: &[u8], long_edge: u32) -> Result<Gray, DecodeError> {
    if format == Format::Heic {
        return heic_luma(data, long_edge);
    }
    Ok(decode_luma(format, data)?.fit_long_edge(long_edge))
}

/// Concatenation order of byte ranges that make up the image data, without
/// metadata: JPEG SOS..EOI, TIFF strips/tiles, PNG IDAT payloads. For a NEF this
/// is the image data of its embedded JPEG, so a byte-identical carved preview
/// produces the same hash. `None` for formats without a defined image-data hash
/// (HEIC).
pub fn image_data_ranges(
    format: Format,
    data: &[u8],
) -> Result<Option<Vec<std::ops::Range<usize>>>, DecodeError> {
    Ok(Some(match format {
        Format::Jpeg => vec![jpeg::scan_data_range(data)?],
        Format::Nef => {
            let outer = nef::jpg_from_raw(data)?
                .ok_or_else(|| DecodeError::Format("NEF has no embedded JPEG".into()))?;
            let inner = jpeg::scan_data_range(&data[outer.clone()])?;
            let abs = outer.start + inner.start..outer.start + inner.end;
            std::iter::once(abs).collect()
        }
        Format::Tiff => tiff::image_data_ranges(data)?,
        Format::Png => png::idat_ranges(data)?,
        Format::Heic | Format::Mov => return Ok(None),
    }))
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
        Format::Heic => {
            let g = heic_luma(data, 1 << 14)?;
            Ok((g.width, g.height))
        }
        Format::Mov => {
            let i = mov::info(&mut std::io::Cursor::new(data))?;
            i.width
                .zip(i.height)
                .ok_or_else(|| DecodeError::Format("movie has no video size".into()))
        }
    }
}

/// Embedded JPEG streams that can stand in for the image, with their sizes:
/// a JPEG's MPF previews (and the JPEG itself), or a NEF's embedded JPEGs.
fn jpeg_candidates(format: Format, data: &[u8]) -> Vec<(std::ops::Range<usize>, (u32, u32))> {
    let ranges = match format {
        Format::Jpeg => std::iter::once(0..data.len())
            .chain(jpeg::mpf_images(data))
            .collect(),
        Format::Nef => nef::embedded_jpegs(data).unwrap_or_default(),
        _ => Vec::new(),
    };
    ranges
        .into_iter()
        .filter_map(|r| jpeg::dimensions(&data[r.clone()]).ok().map(|d| (r, d)))
        .collect()
}

/// Smallest candidate whose long edge is at least `want` (or the largest, if
/// none is big enough or `want` is None).
fn pick(cands: &[(std::ops::Range<usize>, (u32, u32))], want: Option<u32>) -> Option<usize> {
    let long = |i: usize| cands[i].1.0.max(cands[i].1.1);
    let big_enough = want.and_then(|w| {
        (0..cands.len())
            .filter(|&i| long(i) >= w)
            .min_by_key(|&i| long(i))
    });
    big_enough.or_else(|| (0..cands.len()).max_by_key(|&i| long(i)))
}

/// A display-ready sRGB image (see [`render`]).
pub fn render_srgb(
    format: Format,
    data: &[u8],
    long_edge: Option<u32>,
    orientation: u16,
) -> Result<Rgb8, DecodeError> {
    render(format, data, long_edge, orientation, Target::Srgb)
}

/// A display-ready image in `target`'s colour space, upright, long edge at
/// most `long_edge` (full size if None). Uses the smallest embedded preview
/// that's big enough, so grid thumbnails of camera JPGs and NEFs decode a few
/// hundred KB instead of the whole file.
pub fn render(
    format: Format,
    data: &[u8],
    long_edge: Option<u32>,
    orientation: u16,
    target: Target,
) -> Result<Rgb8, DecodeError> {
    let (mut img, icc) = match format {
        Format::Jpeg | Format::Nef => {
            let cands = jpeg_candidates(format, data);
            let i = pick(&cands, long_edge)
                .ok_or_else(|| DecodeError::Format("no decodable JPEG stream".into()))?;
            let (img, icc) = jpeg::decode_rgb(&data[cands[i].0.clone()])?;
            // MPF previews usually carry no profile; they share the primary's.
            let icc = icc.or_else(|| {
                (format == Format::Jpeg && cands[i].0.start != 0)
                    .then(|| jpeg_icc(data))
                    .flatten()
            });
            (img, icc)
        }
        Format::Tiff => tiff::decode_rgb(data)?,
        Format::Png => png::decode_rgb(data)?,
        Format::Heic => return heic_render(data, long_edge.unwrap_or(1 << 14), target),
        Format::Mov => return Err(video_from_bytes()),
    };
    if let Some(l) = long_edge {
        img = img.fit_long_edge(l);
    }
    rgb::convert(&mut img, icc.as_deref(), target);
    Ok(img.orient(orientation))
}

/// [`render`] from a file. A movie gives its poster frame (macOS; through
/// AVFoundation, reading only what it needs), already upright.
pub fn render_path(
    format: Format,
    path: &std::path::Path,
    long_edge: Option<u32>,
    orientation: u16,
    target: Target,
) -> Result<Rgb8, DecodeError> {
    if format == Format::Mov {
        return video_poster(path, long_edge.unwrap_or(4096), target);
    }
    let data =
        std::fs::read(path).map_err(|e| DecodeError::Format(format!("{}: {e}", path.display())))?;
    render(format, &data, long_edge, orientation, target)
}

#[cfg(target_os = "macos")]
fn video_poster(path: &std::path::Path, max: u32, target: Target) -> Result<Rgb8, DecodeError> {
    // Not the first frame (often black): a second in, or the middle of a
    // shorter clip.
    let at = std::fs::File::open(path)
        .ok()
        .and_then(|mut f| mov::info(&mut f).ok())
        .and_then(|i| i.duration_secs)
        .map_or(0.0, |d| (d / 2.0).min(1.0));
    imageio::video_poster(path, max, at, target)
}

#[cfg(not(target_os = "macos"))]
fn video_poster(_path: &std::path::Path, _max: u32, _target: Target) -> Result<Rgb8, DecodeError> {
    Err(DecodeError::Unsupported(
        "movie poster frames need macOS AVFoundation".into(),
    ))
}

fn jpeg_icc(data: &[u8]) -> Option<Vec<u8>> {
    let mut d = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(data));
    d.decode_headers().ok()?;
    d.icc_profile()
}

/// HEIC through ImageIO, which also applies orientation and colour management.
#[cfg(target_os = "macos")]
fn heic_render(data: &[u8], max: u32, target: Target) -> Result<Rgb8, DecodeError> {
    imageio::thumbnail_rgb(
        data,
        max,
        true,
        Some(imageio::type_hint(Format::Heic)),
        target,
    )
}

#[cfg(not(target_os = "macos"))]
fn heic_render(_data: &[u8], _max: u32, _target: Target) -> Result<Rgb8, DecodeError> {
    Err(heic_unsupported())
}

/// Byte range of the XMP packet embedded in the file itself, if any: JPEG
/// APP1 (`http://ns.adobe.com/xap/1.0/`), TIFF/NEF tag 700 (XMLPacket), or
/// PNG iTXt `XML:com.adobe.xmp` (uncompressed).
pub fn embedded_xmp(format: Format, data: &[u8]) -> Option<std::ops::Range<usize>> {
    const JPEG_XMP: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    match format {
        Format::Jpeg => jpeg::header_segments(data).into_iter().find_map(|(m, r)| {
            (m == 0xE1 && data[r.clone()].starts_with(JPEG_XMP))
                .then(|| r.start + JPEG_XMP.len()..r.end)
        }),
        Format::Tiff | Format::Nef => {
            let t = tiffparse::Tiff::parse(data).ok()?;
            let ifd0 = t.ifd0().ok()?;
            let raw = t.raw(ifd0.get(0x02BC)?).ok()?;
            let start = raw.as_ptr() as usize - data.as_ptr() as usize;
            Some(start..start + raw.len())
        }
        Format::Png => png::itxt_xmp(data),
        Format::Heic | Format::Mov => None,
    }
}
