//! Fast metadata for browsing: EXIF, upright dimensions and orientation,
//! read from the start of the file where possible.

use std::io::Read;
use std::path::Path;

use shoebox_decode::Format;

use crate::error::{Result, io};
use crate::exif::{self, ExifInfo};

/// Bump when metadata extraction changes, so cached rows are re-read.
pub const META_VERSION: i64 = 4;

/// Real NEFs and Lightroom TIFFs keep IFD0, the Exif IFD, XMP and the ICC
/// profile in their first ~40 KB; JPEG EXIF lives in APP1 at the start.
const PREFIX: u64 = 256 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    /// Upright (orientation applied) pixel size, when known.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub exif: ExifInfo,
}

impl Meta {
    pub fn orientation(&self) -> u16 {
        self.exif.orientation.unwrap_or(1)
    }

    /// Capture time in seconds (for sorting), if known.
    pub fn capture_seconds(&self) -> Option<f64> {
        exif::capture_seconds(
            self.exif.capture_time.as_deref()?,
            self.exif.subsec.as_deref(),
        )
    }

    /// Width / height of the upright image, if known.
    pub fn aspect(&self) -> Option<f32> {
        match (self.width, self.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some(w as f32 / h as f32),
            _ => None,
        }
    }
}

/// Upright size from stored (sensor/file) size and EXIF orientation.
pub fn upright(dims: Option<(u32, u32)>, orientation: Option<u16>) -> (Option<u32>, Option<u32>) {
    match (dims, orientation) {
        (Some((w, h)), Some(5..=8)) => (Some(h), Some(w)),
        (Some((w, h)), _) => (Some(w), Some(h)),
        (None, _) => (None, None),
    }
}

fn dims_from(format: Format, data: &[u8]) -> Option<(u32, u32)> {
    match format {
        Format::Jpeg => shoebox_decode::jpeg::dimensions(data).ok(),
        Format::Png => shoebox_decode::dimensions(Format::Png, data).ok(),
        Format::Tiff => shoebox_decode::nef::ifd0_dimensions(data).ok().flatten(),
        // The largest image described in any IFD (the raw or JpgFromRaw).
        Format::Nef => shoebox_decode::nef::max_dimensions(data).ok().flatten(),
        Format::Heic | Format::Mov | Format::Mp4 => None,
    }
}

fn from_bytes(format: Format, data: &[u8]) -> (Meta, bool) {
    let exif = exif::read(data);
    let dims = dims_from(format, data);
    let (width, height) = upright(dims, exif.orientation);
    // Complete if we found dimensions (HEIC: EXIF is enough).
    let complete = dims.is_some() || (format == Format::Heic && exif != ExifInfo::default());
    (
        Meta {
            width,
            height,
            exif,
        },
        complete,
    )
}

/// A movie's metadata from its header: display size and creation time
/// (Apple's local creation date, else the UTC header time in local time).
/// A movie with neither (an editor's export often has none) is dated by
/// its modified time: unlike a photo's, that's usually when it was made.
fn movie(path: &Path) -> Result<Meta> {
    let mut f = std::fs::File::open(path).map_err(io(path))?;
    let info = shoebox_decode::mov::info(&mut f).map_err(|e| crate::error::CoreError::Decode {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut exif = ExifInfo::default();
    let local = movie_time(&info).or_else(|| {
        let modified = f.metadata().ok()?.modified().ok()?;
        let t = jiff::Timestamp::try_from(modified).ok()?;
        Some(t.to_zoned(jiff::tz::TimeZone::system()))
    });
    if let Some(z) = local {
        exif.capture_time = Some(z.strftime("%Y:%m:%d %H:%M:%S").to_string());
        exif.offset = Some(z.strftime("%:z").to_string());
    }
    Ok(Meta {
        width: info.width,
        height: info.height,
        exif,
    })
}

/// When a movie was made, in local time: Apple's creation date (which keeps
/// its offset), else the header's UTC time in this Mac's time zone.
fn movie_time(info: &shoebox_decode::mov::MovInfo) -> Option<jiff::Zoned> {
    let apple = info.created_local.as_deref().and_then(|s| {
        let t = jiff::fmt::strtime::parse("%Y-%m-%dT%H:%M:%S%z", s).ok()?;
        let offset = t.offset()?;
        Some(
            t.to_timestamp()
                .ok()?
                .to_zoned(jiff::tz::TimeZone::fixed(offset)),
        )
    });
    apple.or_else(|| {
        let t = jiff::Timestamp::from_second(info.created_utc?).ok()?;
        Some(t.to_zoned(jiff::tz::TimeZone::system()))
    })
}

/// Read metadata, trying the first 256 KB before falling back to the whole file.
pub fn read(path: &Path, format: Format) -> Result<Meta> {
    if format.is_movie() {
        return movie(path);
    }
    let mut f = std::fs::File::open(path).map_err(io(path))?;
    let mut prefix = Vec::with_capacity(PREFIX as usize);
    (&mut f)
        .take(PREFIX)
        .read_to_end(&mut prefix)
        .map_err(io(path))?;
    let (meta, complete) = from_bytes(format, &prefix);
    if complete || (prefix.len() as u64) < PREFIX {
        return Ok(meta);
    }
    let all = std::fs::read(path).map_err(io(path))?;
    Ok(from_bytes(format, &all).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upright_swaps_for_rotations() {
        assert_eq!(
            upright(Some((6000, 4000)), Some(6)),
            (Some(4000), Some(6000))
        );
        assert_eq!(
            upright(Some((6000, 4000)), Some(1)),
            (Some(6000), Some(4000))
        );
        assert_eq!(upright(Some((6000, 4000)), None), (Some(6000), Some(4000)));
    }

    #[test]
    fn movie_times() {
        let info = shoebox_decode::mov::MovInfo {
            created_local: Some("2026-04-24T13:36:30+0700".into()),
            created_utc: Some(0),
            ..Default::default()
        };
        let z = movie_time(&info).unwrap();
        assert_eq!(
            z.strftime("%Y:%m:%d %H:%M:%S %:z").to_string(),
            "2026:04:24 13:36:30 +07:00"
        );
        let utc_only = shoebox_decode::mov::MovInfo {
            created_utc: Some(1_777_000_000),
            ..Default::default()
        };
        assert_eq!(
            movie_time(&utc_only).unwrap().timestamp().as_second(),
            1_777_000_000
        );
        assert!(movie_time(&Default::default()).is_none());
    }

    #[test]
    fn undated_movie_uses_its_modified_time() {
        fn bx(kind: &[u8], body: &[u8]) -> Vec<u8> {
            let mut b = (8 + body.len() as u32).to_be_bytes().to_vec();
            b.extend_from_slice(kind);
            b.extend_from_slice(body);
            b
        }
        let mut file = bx(b"ftyp", b"qt  \0\0\0\0");
        file.extend(bx(b"moov", &bx(b"mvhd", &[0; 100]))); // created: 0
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("export.mov");
        std::fs::write(&p, &file).unwrap();
        let when = jiff::Timestamp::from_second(1_733_033_644).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(std::time::SystemTime::from(when))
            .unwrap();
        let m = read(&p, Format::Mov).unwrap();
        let local = when.to_zoned(jiff::tz::TimeZone::system());
        assert_eq!(
            m.exif.capture_time.as_deref(),
            Some(local.strftime("%Y:%m:%d %H:%M:%S").to_string().as_str())
        );
    }
}
