//! EXIF reading via kamadak-exif. JPEG, TIFF and NEF (TIFF-based) all parse
//! through the same container reader.

use std::io::Cursor;

use exif::{In, Tag, Value};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExifInfo {
    pub capture_time: Option<String>,
    pub subsec: Option<String>,
    pub offset: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub lens: Option<String>,
    pub focal: Option<f64>,
    pub fnumber: Option<f64>,
    pub exposure: Option<String>,
    pub iso: Option<u32>,
    pub orientation: Option<u16>,
}

fn ascii(e: &exif::Exif, tag: Tag) -> Option<String> {
    match &e.get_field(tag, In::PRIMARY)?.value {
        Value::Ascii(parts) => {
            let s = parts.first().map(|b| {
                String::from_utf8_lossy(b)
                    .trim()
                    .trim_end_matches('\0')
                    .to_owned()
            })?;
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

fn rational(e: &exif::Exif, tag: Tag) -> Option<f64> {
    match &e.get_field(tag, In::PRIMARY)?.value {
        Value::Rational(v) => v.first().map(|r| r.to_f64()),
        _ => None,
    }
}

fn uint(e: &exif::Exif, tag: Tag) -> Option<u32> {
    e.get_field(tag, In::PRIMARY)?.value.get_uint(0)
}

/// Read EXIF; files without EXIF (such as carved previews) give all `None`.
pub fn read(data: &[u8]) -> ExifInfo {
    let parsed = exif::Reader::new().read_from_container(&mut Cursor::new(data));
    let Ok(e) = parsed else {
        return ExifInfo::default();
    };
    ExifInfo {
        capture_time: ascii(&e, Tag::DateTimeOriginal),
        subsec: ascii(&e, Tag::SubSecTimeOriginal),
        offset: ascii(&e, Tag::OffsetTimeOriginal),
        make: ascii(&e, Tag::Make),
        model: ascii(&e, Tag::Model),
        serial: ascii(&e, Tag::BodySerialNumber),
        lens: ascii(&e, Tag::LensModel),
        focal: rational(&e, Tag::FocalLength),
        fnumber: rational(&e, Tag::FNumber),
        exposure: e
            .get_field(Tag::ExposureTime, In::PRIMARY)
            .map(|f| f.display_value().to_string()),
        iso: uint(&e, Tag::PhotographicSensitivity),
        orientation: uint(&e, Tag::Orientation).and_then(|v| u16::try_from(v).ok()),
    }
}
