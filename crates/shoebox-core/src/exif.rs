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

/// Seconds since the Unix epoch (ignoring time zone) from EXIF
/// `YYYY:MM:DD HH:MM:SS` plus optional sub-seconds. Only used to compare two
/// capture times, so the missing offset doesn't matter.
pub fn capture_seconds(datetime: &str, subsec: Option<&str>) -> Option<f64> {
    let b = datetime.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { datetime.get(r)?.trim().parse().ok() };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None; // e.g. "0000:00:00 00:00:00" from unset camera clocks
    }
    // Days from civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let frac = subsec
        .and_then(|ss| {
            let digits: String = ss.trim().chars().take_while(char::is_ascii_digit).collect();
            (!digits.is_empty()).then(|| format!("0.{digits}").parse::<f64>().ok())?
        })
        .unwrap_or(0.0);
    Some((days * 86_400 + h * 3600 + mi * 60 + s) as f64 + frac)
}

#[cfg(test)]
mod tests {
    use super::capture_seconds;

    #[test]
    fn parses_capture_times() {
        assert_eq!(capture_seconds("1970:01:01 00:00:00", None), Some(0.0));
        let a = capture_seconds("2026:05:01 09:15:02", Some("65")).unwrap();
        let b = capture_seconds("2026:05:01 09:15:04", Some("5")).unwrap();
        assert!((b - a - 1.85).abs() < 1e-6);
        assert_eq!(capture_seconds("0000:00:00 00:00:00", None), None);
        assert_eq!(capture_seconds("garbage", None), None);
    }
}
