//! Shoebox decoding: JPEG, TIFF, PNG, NEF embedded previews, DNG previews
//! (or the raw data, when they're too small), HEIC and AVIF through the
//! platform, and movie poster frames.
//!
//! Everything here is read-only. Images decode from in-memory bytes; movies
//! from their path ([`render_path`]), since a poster frame shouldn't need the
//! whole file.

pub mod dng;
pub mod gray;
pub mod heif;
#[cfg(target_os = "macos")]
pub mod imageio;
pub mod jpeg;
pub mod mov;
pub mod mpeg;
pub mod nef;
pub mod png;
pub mod raw;
pub mod rgb;
pub mod rgb16;
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

/// File formats the library understands, in the order the file type
/// filter lists them: raw, images, movies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Format {
    Nef,
    /// Sony's raw: TIFF-based like a NEF. Shown from an embedded JPEG only
    /// if one has the whole frame (older bodies' are cropped to 16:9), else
    /// rendered from the raw data like a DNG without previews.
    Arw,
    /// Any DNG: a camera's, Adobe DNG Converter's, or Shoebox's own linear
    /// DNG exports. Shown from its previews, or rendered from the raw data
    /// (see [`dng`]).
    Dng,
    Jpeg,
    Tiff,
    Png,
    /// HEIC/HEIF: decoded by the platform (ImageIO on macOS); unsupported
    /// elsewhere until a libheif backend exists.
    Heic,
    /// AVIF: like HEIC, decoded by the platform (ImageIO on macOS).
    Avif,
    /// WebP: decoded by the platform, like HEIC.
    Webp,
    /// Photoshop document: its flattened image, decoded by the platform.
    /// Shown and developed, never written to.
    Psd,
    /// QuickTime movie (a Live Photo's video, or a clip): a poster frame
    /// through AVFoundation on macOS; see [`render_path`].
    Mov,
    /// MPEG-4 movie (`.mp4`, `.m4v`): the same box structure as QuickTime,
    /// handled the same way.
    Mp4,
    /// MPEG-1 or MPEG-2 program stream (`.mpg`, `.mpeg`): an older camera's
    /// clips. Its header is read by [`mpeg`], its picture by AVFoundation.
    Mpg,
}

impl Format {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "nef" => Format::Nef,
            "arw" => Format::Arw,
            "dng" => Format::Dng,
            "jpg" | "jpeg" => Format::Jpeg,
            "tif" | "tiff" => Format::Tiff,
            "png" => Format::Png,
            "heic" | "heif" | "hif" => Format::Heic,
            "avif" => Format::Avif,
            "webp" => Format::Webp,
            "psd" => Format::Psd,
            "mov" => Format::Mov,
            "mp4" | "m4v" => Format::Mp4,
            "mpg" | "mpeg" => Format::Mpg,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Nef => "NEF",
            Format::Arw => "ARW",
            Format::Dng => "DNG",
            Format::Jpeg => "JPG",
            Format::Tiff => "TIF",
            Format::Png => "PNG",
            Format::Heic => "HEIC",
            Format::Avif => "AVIF",
            Format::Webp => "WEBP",
            Format::Psd => "PSD",
            Format::Mov => "MOV",
            Format::Mp4 => "MP4",
            Format::Mpg => "MPG",
        }
    }

    /// A movie (MOV, MP4 or MPG): its picture comes from [`render_path`].
    pub fn is_movie(self) -> bool {
        matches!(self, Format::Mov | Format::Mp4 | Format::Mpg)
    }

    /// Raw data from a camera (NEF, ARW or DNG): Develop decodes it rather
    /// than a finished image.
    pub fn is_raw(self) -> bool {
        matches!(self, Format::Nef | Format::Arw | Format::Dng)
    }

    /// Decoded by the platform (HEIC, AVIF, WebP, PSD): ImageIO applies the
    /// file's own rotation and colour.
    pub fn is_platform(self) -> bool {
        matches!(
            self,
            Format::Heic | Format::Avif | Format::Webp | Format::Psd
        )
    }
}

/// The bytes whose pixels represent the file: the file itself, for a NEF
/// its embedded full-size JPEG (`JpgFromRaw`), for an ARW its largest
/// whole-frame JPEG, for a DNG its largest JPEG preview (the ARW or DNG
/// itself if it has none).
pub fn display_source(format: Format, data: &[u8]) -> Result<(Format, &[u8]), DecodeError> {
    match format {
        Format::Nef => {
            let r = nef::jpg_from_raw(data)?
                .ok_or_else(|| DecodeError::Format("NEF has no embedded JPEG".into()))?;
            Ok((Format::Jpeg, &data[r]))
        }
        Format::Arw => Ok(match arw_previews(data).into_iter().last() {
            Some((r, _)) => (Format::Jpeg, &data[r]),
            None => (Format::Arw, data),
        }),
        Format::Dng => {
            let largest = dng::previews(data)?
                .into_iter()
                .filter_map(|f| match f.preview {
                    dng::Preview::Jpeg(r) => Some(r),
                    dng::Preview::Rgb(_) => None,
                })
                .max_by_key(|r| r.len());
            Ok(match largest {
                Some(r) => (Format::Jpeg, &data[r]),
                None => (Format::Dng, data),
            })
        }
        f => Ok((f, data)),
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_unsupported(format: Format) -> DecodeError {
    DecodeError::Unsupported(format!(
        "{} decoding needs macOS ImageIO (no other backend is built yet)",
        format.label()
    ))
}

/// Full-resolution 8-bit luma of a file's display source (orientation not applied).
pub fn decode_luma(format: Format, data: &[u8]) -> Result<Gray, DecodeError> {
    let (format, data) = display_source(format, data)?;
    match format {
        Format::Jpeg | Format::Nef => jpeg::decode_luma(data),
        // No whole-frame preview: from the raw data.
        Format::Arw => Ok(raw_render(Format::Arw, data, None, Target::Srgb)?.to_gray()),
        Format::Tiff => tiff::decode_luma(data),
        Format::Png => png::decode_luma(data),
        // No JPEG preview: from the raw data.
        Format::Dng => Ok(dng_render(data, None, 1, Target::Srgb)?.to_gray()),
        f if f.is_platform() => platform_luma(format, data, 1 << 14),
        _ => Err(video_from_bytes()),
    }
}

fn video_from_bytes() -> DecodeError {
    DecodeError::Unsupported("a movie's picture comes from its path (render_path)".into())
}

#[cfg(target_os = "macos")]
fn platform_luma(format: Format, data: &[u8], max: u32) -> Result<Gray, DecodeError> {
    imageio::thumbnail_luma(data, max, false, Some(imageio::type_hint(format)))
}

#[cfg(not(target_os = "macos"))]
fn platform_luma(format: Format, _data: &[u8], _max: u32) -> Result<Gray, DecodeError> {
    Err(platform_unsupported(format))
}

/// Luma thumbnail with the given long edge (orientation not applied).
pub fn luma_thumbnail(format: Format, data: &[u8], long_edge: u32) -> Result<Gray, DecodeError> {
    match format {
        f if f.is_platform() => platform_luma(format, data, long_edge),
        // The smallest preview big enough, or the raw data at that size.
        Format::Dng => Ok(dng_render(data, Some(long_edge), 1, Target::Srgb)?
            .to_gray()
            .fit_long_edge(long_edge)),
        Format::Arw => Ok(render(format, data, Some(long_edge), 1, Target::Srgb)?
            .to_gray()
            .fit_long_edge(long_edge)),
        _ => Ok(decode_luma(format, data)?.fit_long_edge(long_edge)),
    }
}

/// Concatenation order of byte ranges that make up the image data, without
/// metadata: JPEG SOS..EOI, TIFF strips/tiles, PNG IDAT payloads. For a NEF this
/// is the image data of its embedded JPEG, so a byte-identical carved preview
/// produces the same hash; for a DNG, its raw image's strips or tiles.
/// `None` for formats without a defined image-data hash (HEIC, AVIF, WebP,
/// PSD, movies).
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
        // A whole-frame preview's, like a NEF's.
        Format::Arw => match display_source(format, data)? {
            (Format::Jpeg, jpg) => {
                let start = jpg.as_ptr() as usize - data.as_ptr() as usize;
                let inner = jpeg::scan_data_range(jpg)?;
                std::iter::once(start + inner.start..start + inner.end).collect()
            }
            _ => return Ok(None),
        },
        Format::Dng => dng::raw_data_ranges(data)?,
        Format::Tiff => tiff::image_data_ranges(data)?,
        Format::Png => png::idat_ranges(data)?,
        _ => return Ok(None),
    }))
}

/// Pixel dimensions of the display source, from headers only where possible
/// (orientation not applied). A DNG's are its raw image's default crop; an
/// AVIF's its primary image's stored size.
pub fn dimensions(format: Format, data: &[u8]) -> Result<(u32, u32), DecodeError> {
    match format {
        Format::Dng => return dng::dimensions(data),
        Format::Avif => {
            if let Ok((size, _)) = heif::primary(data) {
                return Ok(size);
            }
        }
        Format::Webp => return webp_dimensions(data),
        Format::Psd => return psd_dimensions(data),
        Format::Mpg => {
            let i = mpeg::info(&mut std::io::Cursor::new(data))?;
            return i
                .width
                .zip(i.height)
                .ok_or_else(|| DecodeError::Format("movie has no video size".into()));
        }
        _ => {}
    }
    let (format, data) = display_source(format, data)?;
    match format {
        Format::Jpeg | Format::Nef => jpeg::dimensions(data),
        Format::Arw => nef::max_dimensions(data)?
            .ok_or_else(|| DecodeError::Format("ARW has no dimensions".into())),
        Format::Tiff => nef::ifd0_dimensions(data)?
            .ok_or_else(|| DecodeError::Format("TIFF has no dimensions".into())),
        Format::Png => {
            let d = data.get(16..24).ok_or(DecodeError::Truncated)?;
            Ok((
                u32::from_be_bytes([d[0], d[1], d[2], d[3]]),
                u32::from_be_bytes([d[4], d[5], d[6], d[7]]),
            ))
        }
        Format::Dng => dng::dimensions(data),
        f if f.is_platform() => {
            let g = platform_luma(format, data, 1 << 14)?;
            Ok((g.width, g.height))
        }
        _ => {
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
        Format::Nef | Format::Arw => nef::embedded_jpegs(data).unwrap_or_default(),
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
        Format::Arw => {
            let cands = arw_previews(data);
            // Full size is the raw's: a smaller preview would be blown up.
            let want = long_edge.or_else(|| {
                nef::max_dimensions(data)
                    .ok()
                    .flatten()
                    .map(|(w, h)| w.max(h))
            });
            // The smallest whole-frame preview that's big enough, else the
            // raw data.
            let Some(i) = want.and_then(|w| {
                pick(&cands, Some(w)).filter(|&i| {
                    u64::from(cands[i].1.0.max(cands[i].1.1)) * 20 >= u64::from(w) * 19
                })
            }) else {
                let img = raw_render(Format::Arw, data, long_edge, target)?;
                let img = match long_edge {
                    Some(l) => img.fit_long_edge(l),
                    None => img,
                };
                return Ok(img.orient(orientation));
            };
            jpeg::decode_rgb(&data[cands[i].0.clone()])?
        }
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
        Format::Dng => return dng_render(data, long_edge, orientation, target),
        f if f.is_platform() => {
            return platform_render(format, data, long_edge.unwrap_or(1 << 14), target);
        }
        _ => return Err(video_from_bytes()),
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
    if format.is_movie() {
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
    let at = Format::from_path(path)
        .and_then(|f| movie_info(f, path).ok())
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

/// HEIC, AVIF, WebP and PSD through ImageIO, which also applies the file's rotation
/// and colour management (an ICC profile or nclx colour).
#[cfg(target_os = "macos")]
fn platform_render(
    format: Format,
    data: &[u8],
    max: u32,
    target: Target,
) -> Result<Rgb8, DecodeError> {
    imageio::thumbnail_rgb(data, max, true, Some(imageio::type_hint(format)), target)
}

#[cfg(not(target_os = "macos"))]
fn platform_render(
    format: Format,
    _data: &[u8],
    _max: u32,
    _target: Target,
) -> Result<Rgb8, DecodeError> {
    Err(platform_unsupported(format))
}

/// A DNG: its smallest preview that's big enough (the size wanted, or the
/// image's own for full size, give or take 5%), else rendered from the raw
/// data. Previews and the raw data are stored unrotated: `orientation`
/// (IFD 0's) turns them upright.
fn dng_render(
    data: &[u8],
    long_edge: Option<u32>,
    orientation: u16,
    target: Target,
) -> Result<Rgb8, DecodeError> {
    let full = dng::dimensions(data).ok().map(|(w, h)| w.max(h));
    let want = match (long_edge, full) {
        (Some(l), Some(f)) => l.min(f),
        (Some(l), None) => l,
        (None, f) => f.unwrap_or(u32::MAX),
    };
    let previews = dng::previews(data).unwrap_or_default();
    let best = previews
        .iter()
        .filter(|p| u64::from(p.long_edge()) * 20 >= u64::from(want) * 19)
        .min_by_key(|p| p.long_edge());
    let mut img = match best {
        Some(p) => {
            let (img, icc) = dng::decode_preview(data, p)?;
            let mut img = match long_edge {
                Some(l) => img.fit_long_edge(l),
                None => img,
            };
            rgb::convert(&mut img, icc.as_deref(), target);
            img
        }
        None => raw_render(Format::Dng, data, long_edge, target)?,
    };
    if let Some(l) = long_edge {
        img = img.fit_long_edge(l);
    }
    Ok(img.orient(orientation))
}

/// An ARW's embedded JPEGs that show the whole frame (the raw's
/// proportions, within 2%), smallest first. Older Sony bodies keep only a
/// 16:9 crop and a 4:3 thumbnail, so they have none.
fn arw_previews(data: &[u8]) -> Vec<(std::ops::Range<usize>, (u32, u32))> {
    let Ok(Some((w, h))) = nef::max_dimensions(data) else {
        return Vec::new();
    };
    let ratio = |w: u32, h: u32| f64::from(w.max(h)) / f64::from(w.min(h).max(1));
    let want = ratio(w, h);
    let mut out: Vec<_> = jpeg_candidates(Format::Arw, data)
        .into_iter()
        .filter(|(_, (pw, ph))| (ratio(*pw, *ph) / want - 1.0).abs() < 0.02)
        .collect();
    out.sort_by_key(|(_, (w, h))| *w.max(h));
    out
}

/// A raw (DNG, ARW) rendered from its raw data, not oriented: by ImageIO
/// (Apple's raw engine) where it can, else by rawler ([`raw::render`]).
fn raw_render(
    format: Format,
    data: &[u8],
    long_edge: Option<u32>,
    target: Target,
) -> Result<Rgb8, DecodeError> {
    #[cfg(target_os = "macos")]
    {
        let max = long_edge.unwrap_or(1 << 14);
        let hint = Some(imageio::type_hint(format));
        match imageio::thumbnail_rgb(data, max, false, hint, target) {
            Ok(img) => return Ok(img),
            Err(e) => tracing::debug!(
                "ImageIO couldn't render a {} ({e}): trying rawler",
                format.label()
            ),
        }
    }
    let mut img = raw::render(data, long_edge)?;
    rgb::convert(&mut img, None, target);
    Ok(img)
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
        Format::Tiff | Format::Nef | Format::Arw | Format::Dng => {
            let t = tiffparse::Tiff::parse(data).ok()?;
            let ifd0 = t.ifd0().ok()?;
            let raw = t.raw(ifd0.get(0x02BC)?).ok()?;
            let start = raw.as_ptr() as usize - data.as_ptr() as usize;
            Some(start..start + raw.len())
        }
        Format::Png => png::itxt_xmp(data),
        _ => None,
    }
}

/// A movie's header: size, duration and creation date, read without the
/// media data (QuickTime boxes, or an MPEG stream's headers).
pub fn movie_info(format: Format, path: &std::path::Path) -> Result<mov::MovInfo, DecodeError> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| DecodeError::Format(format!("{}: {e}", path.display())))?;
    match format {
        Format::Mpg => mpeg::info(&mut f),
        _ => mov::info(&mut f),
    }
}

/// A WebP's size from its first chunk (`VP8 `, `VP8L` or `VP8X`).
fn webp_dimensions(data: &[u8]) -> Result<(u32, u32), DecodeError> {
    if data.get(0..4) != Some(b"RIFF") || data.get(8..12) != Some(b"WEBP") {
        return Err(DecodeError::Format("not a WebP".into()));
    }
    let b = |at: usize| {
        data.get(at)
            .copied()
            .map(u32::from)
            .ok_or(DecodeError::Truncated)
    };
    match data.get(12..16) {
        // Lossy: 14-bit sizes after the key frame's start code.
        Some(b"VP8 ") => Ok((
            (b(26)? | b(27)? << 8) & 0x3FFF,
            (b(28)? | b(29)? << 8) & 0x3FFF,
        )),
        // Lossless: 14 bits each, less one, after the signature byte.
        Some(b"VP8L") => {
            let bits = b(21)? | b(22)? << 8 | b(23)? << 16 | b(24)? << 24;
            Ok(((bits & 0x3FFF) + 1, (bits >> 14 & 0x3FFF) + 1))
        }
        // Extended: 24 bits each, less one, after the flags.
        Some(b"VP8X") => Ok((
            (b(24)? | b(25)? << 8 | b(26)? << 16) + 1,
            (b(27)? | b(28)? << 8 | b(29)? << 16) + 1,
        )),
        _ => Err(DecodeError::Format("WebP has no image chunk".into())),
    }
}

/// A Photoshop document's size from its header (PSD or PSB).
fn psd_dimensions(data: &[u8]) -> Result<(u32, u32), DecodeError> {
    if data.get(0..4) != Some(b"8BPS") {
        return Err(DecodeError::Format("not a Photoshop document".into()));
    }
    let be = |at: usize| {
        data.get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_be_bytes)
            .ok_or(DecodeError::Truncated)
    };
    Ok((be(18)?, be(14)?))
}

#[cfg(test)]
mod tests {
    use super::Format;
    use std::path::Path;

    #[test]
    fn movie_formats() {
        let f = |p: &str| Format::from_path(Path::new(p));
        assert_eq!(f("a.MOV"), Some(Format::Mov));
        assert_eq!(f("a.mp4"), Some(Format::Mp4));
        assert_eq!(f("a.M4V"), Some(Format::Mp4));
        assert!(f("a.mp4").is_some_and(Format::is_movie));
        assert!(!Format::Heic.is_movie());
        assert_eq!(Format::Mp4.label(), "MP4");
    }

    #[test]
    fn dng_and_avif() {
        let f = |p: &str| Format::from_path(Path::new(p));
        assert_eq!(f("a.DNG"), Some(Format::Dng));
        assert_eq!(f("a.avif"), Some(Format::Avif));
        assert!(Format::Dng.is_raw() && Format::Nef.is_raw() && !Format::Tiff.is_raw());
        assert!(Format::Avif.is_platform() && !Format::Avif.is_raw());
        assert_eq!((Format::Dng.label(), Format::Avif.label()), ("DNG", "AVIF"));
    }

    #[test]
    fn arw_webp_psd_mpg() {
        let f = |p: &str| Format::from_path(Path::new(p));
        assert_eq!(f("DSC01234.ARW"), Some(Format::Arw));
        assert_eq!(f("a.webp"), Some(Format::Webp));
        assert_eq!(f("a.PSD"), Some(Format::Psd));
        assert_eq!(f("a.mpg"), Some(Format::Mpg));
        assert_eq!(f("a.MPEG"), Some(Format::Mpg));
        assert!(Format::Arw.is_raw() && !Format::Arw.is_platform());
        assert!(Format::Webp.is_platform() && Format::Psd.is_platform());
        assert!(Format::Mpg.is_movie() && !Format::Psd.is_movie());
    }

    #[test]
    fn webp_and_psd_sizes_from_headers() {
        // Lossy: RIFF, size, WEBP, "VP8 ", chunk size, frame tag, start code.
        let mut lossy = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0".to_vec();
        lossy.extend([0, 0, 0, 0x9D, 0x01, 0x2A]);
        lossy.extend(540u16.to_le_bytes());
        lossy.extend(765u16.to_le_bytes());
        assert_eq!(super::webp_dimensions(&lossy).unwrap(), (540, 765));
        // Extended: flags, three reserved bytes, then 24-bit sizes less one.
        let mut ext = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0".to_vec();
        ext.extend([0, 0, 0, 0]);
        ext.extend(&1999u32.to_le_bytes()[..3]);
        ext.extend(&2999u32.to_le_bytes()[..3]);
        assert_eq!(super::webp_dimensions(&ext).unwrap(), (2000, 3000));
        // Lossless: signature 0x2F, then width-1 and height-1 in 14 bits each.
        let bits: u32 = 639 | 479 << 14;
        let mut ll = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2F".to_vec();
        ll.extend(bits.to_le_bytes());
        assert_eq!(super::webp_dimensions(&ll).unwrap(), (640, 480));

        let mut psd = b"8BPS\0\x01\0\0\0\0\0\0\0\x03".to_vec();
        psd.extend(2642u32.to_be_bytes());
        psd.extend(21316u32.to_be_bytes());
        assert_eq!(super::psd_dimensions(&psd).unwrap(), (21316, 2642));
        assert!(super::psd_dimensions(b"nope").is_err());
    }
}
