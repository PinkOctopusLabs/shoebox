//! Finding dates for undated photos (SPEC "Dates", Find dates): clues to
//! when a photo was taken, each a suggested date with how sure it is and
//! the evidence. Phase 1's clues are in the file (dates other than the
//! capture date), its name and its folders' names. Nothing here writes.

use std::io::Read;
use std::path::Path;

use exif::{In, Tag, Value};
use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use shoebox_decode::Format;

use crate::sidecar::{Precision, SidecarDate};
use crate::xmp::{self, Prop};

/// Where a clue came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// EXIF's digitized date (`DateTimeDigitized`).
    Digitized,
    /// The GPS date and time stamp (UTC).
    Gps,
    /// A date in the file's own XMP.
    FileXmp,
    /// A PNG's `Creation Time`.
    PngCreated,
    /// EXIF's modified date (`DateTime`): an editor may have set it.
    Modified,
    FileName,
    Folder,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Digitized => "Digitized date in the file",
            Source::Gps => "GPS time in the file",
            Source::FileXmp => "Date in the file's XMP",
            Source::PngCreated => "Creation time in the PNG",
            Source::Modified => "Modified date in the file",
            Source::FileName => "File name",
            Source::Folder => "Folder",
        }
    }
}

/// How sure a clue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// A suggested date and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clue {
    pub date: SidecarDate,
    pub source: Source,
    pub confidence: Confidence,
    /// What it was read from: the name, the folders, the tag's value.
    pub evidence: String,
}

/// Every clue for a photo, best first. `rel`: its path in its root (the
/// folders above it are clues). A cloud-only file isn't read (`cloud`), so
/// only its name and folders give clues.
pub fn clues(path: &Path, rel: &Path, format: Format, cloud: bool) -> Vec<Clue> {
    let mut out = Vec::new();
    if !cloud {
        out.extend(in_file(path, format));
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        out.extend(in_name(name, &TimeZone::system()));
    }
    out.extend(in_folders(rel));
    rank(&mut out);
    out
}

/// Sort best first: more sure, then more precise. A clue another source
/// agrees with (the same day, or month or year at the coarser one's
/// precision) is one step surer.
pub fn rank(clues: &mut [Clue]) {
    let agreed: Vec<bool> = clues
        .iter()
        .enumerate()
        .map(|(i, c)| {
            clues
                .iter()
                .enumerate()
                .any(|(j, o)| i != j && o.source != c.source && agree(&c.date, &o.date))
        })
        .collect();
    for (c, a) in clues.iter_mut().zip(agreed) {
        if a {
            c.confidence = match c.confidence {
                Confidence::Low => Confidence::Medium,
                _ => Confidence::High,
            };
        }
    }
    clues.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then(a.date.precision.cmp(&b.date.precision))
            .then(a.source.cmp(&b.source))
    });
}

/// The same day, month or year, at the coarser of the two's precision.
fn agree(a: &SidecarDate, b: &SidecarDate) -> bool {
    let n = match a.precision.max(b.precision) {
        Precision::Moment | Precision::Day => 10,
        Precision::Month => 7,
        Precision::Year => 4,
    };
    a.time.get(..n).is_some() && a.time.get(..n) == b.time.get(..n)
}

// ---- In the file ------------------------------------------------------

/// Dates in the file other than its capture date: EXIF's digitized and
/// modified dates, the GPS stamp, its own XMP's, a PNG's creation time.
pub fn in_file(path: &Path, format: Format) -> Vec<Clue> {
    let mut data = Vec::new();
    let Ok(f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if f.take(1024 * 1024).read_to_end(&mut data).is_err() {
        return Vec::new();
    }
    in_bytes(&data, format, &TimeZone::system())
}

/// [`in_file`] on the file's first bytes; GPS times (UTC) in `tz`.
pub fn in_bytes(data: &[u8], format: Format, tz: &TimeZone) -> Vec<Clue> {
    let mut out = Vec::new();
    if let Ok(e) = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(data)) {
        let exif_date = |time: Tag, sub: Tag, off: Tag| -> Option<(SidecarDate, String)> {
            let t = ascii(&e, time)?;
            DateTime::strptime("%Y:%m:%d %H:%M:%S", t.trim()).ok()?;
            let d = SidecarDate::moment(t.trim().to_owned(), ascii(&e, sub), ascii(&e, off));
            Some((d, t))
        };
        if let Some((date, t)) = exif_date(
            Tag::DateTimeDigitized,
            Tag::SubSecTimeDigitized,
            Tag::OffsetTimeDigitized,
        ) {
            out.push(Clue {
                date,
                source: Source::Digitized,
                confidence: Confidence::High,
                evidence: t,
            });
        }
        if let Some((date, t)) = exif_date(Tag::DateTime, Tag::SubSecTime, Tag::OffsetTime) {
            out.push(Clue {
                date,
                source: Source::Modified,
                confidence: Confidence::Medium,
                evidence: t,
            });
        }
        if let Some(c) = gps_clue(&e, tz) {
            out.push(c);
        }
    }
    if let Some(r) = shoebox_decode::embedded_xmp(format, data)
        && let Ok(p) = xmp::read(&data[r])
    {
        let found = [Prop::DateTimeOriginal, Prop::DateCreated]
            .iter()
            .filter_map(|k| p.get(k).and_then(xmp::Value::first))
            .find_map(|v| SidecarDate::parse(&v).map(|d| (d, v)));
        if let Some((date, v)) = found {
            out.push(Clue {
                date,
                source: Source::FileXmp,
                confidence: Confidence::High,
                evidence: v,
            });
        }
    }
    if format == Format::Png
        && let Some((date, v)) = png_created(data)
    {
        out.push(Clue {
            date,
            source: Source::PngCreated,
            confidence: Confidence::Medium,
            evidence: v,
        });
    }
    out
}

fn ascii(e: &exif::Exif, tag: Tag) -> Option<String> {
    match &e.get_field(tag, In::PRIMARY)?.value {
        Value::Ascii(parts) => {
            let s = String::from_utf8_lossy(parts.first()?)
                .trim()
                .trim_end_matches('\0')
                .to_owned();
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

/// The GPS date and time stamp (UTC), as a time in `tz`.
fn gps_clue(e: &exif::Exif, tz: &TimeZone) -> Option<Clue> {
    let day = ascii(e, Tag::GPSDateStamp)?;
    let date = Date::strptime("%Y:%m:%d", day.trim()).ok()?;
    let hms = match &e.get_field(Tag::GPSTimeStamp, In::PRIMARY)?.value {
        Value::Rational(v) if v.len() == 3 => v.iter().map(|r| r.to_f64()).collect::<Vec<_>>(),
        _ => return None,
    };
    if hms.iter().any(|x| !x.is_finite() || *x < 0.0) || hms[0] >= 24.0 || hms[1] >= 60.0 {
        return None;
    }
    let secs = hms[0] * 3600.0 + hms[1] * 60.0 + hms[2];
    let utc = date
        .to_zoned(TimeZone::UTC)
        .ok()?
        .timestamp()
        .checked_add(jiff::SignedDuration::from_millis(
            (secs * 1000.0).round() as i64
        ))
        .ok()?;
    let z = utc.to_zoned(tz.clone());
    let ms = z.subsec_nanosecond() / 1_000_000;
    Some(Clue {
        date: SidecarDate::moment(
            z.strftime("%Y:%m:%d %H:%M:%S").to_string(),
            (ms > 0).then(|| format!("{ms:03}")),
            Some(z.strftime("%:z").to_string()),
        ),
        source: Source::Gps,
        confidence: Confidence::High,
        evidence: format!(
            "{} {} UTC",
            day.trim(),
            utc.to_zoned(TimeZone::UTC).strftime("%H:%M:%S")
        ),
    })
}

/// A PNG's `Creation Time` text chunk (ISO 8601, RFC 2822 or EXIF's form).
fn png_created(data: &[u8]) -> Option<(SidecarDate, String)> {
    let mut at = 8;
    while at + 8 <= data.len() {
        let len = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
        let kind = &data[at + 4..at + 8];
        let body = data.get(at + 8..at + 8 + len)?;
        if kind == b"tEXt"
            && let Some(text) = body.strip_prefix(b"Creation Time\0")
        {
            let v = String::from_utf8_lossy(text).trim().to_owned();
            let parsed = SidecarDate::parse(&v)
                .filter(|d| d.precision == Precision::Moment)
                .or_else(|| {
                    let z = jiff::fmt::rfc2822::parse(&v).ok()?;
                    Some(SidecarDate::moment(
                        z.strftime("%Y:%m:%d %H:%M:%S").to_string(),
                        None,
                        Some(z.strftime("%:z").to_string()),
                    ))
                });
            return parsed.map(|d| (d, v));
        }
        if kind == b"IDAT" || kind == b"IEND" {
            return None;
        }
        at += 12 + len;
    }
    None
}

// ---- In the name ------------------------------------------------------

/// A run of digits or of other characters in a name.
#[derive(Debug, Clone, Copy)]
struct Run<'a> {
    digits: bool,
    text: &'a str,
}

fn runs(s: &str) -> Vec<Run<'_>> {
    let mut out: Vec<Run<'_>> = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    for i in 1..=b.len() {
        if i == b.len() || b[i].is_ascii_digit() != b[start].is_ascii_digit() {
            out.push(Run {
                digits: b[start].is_ascii_digit(),
                text: &s[start..i],
            });
            start = i;
        }
    }
    out
}

fn year_ok(y: i16) -> bool {
    (1900..=jiff::Zoned::now().year() + 1).contains(&y)
}

fn ymd(y: &str, m: &str, d: &str) -> Option<Date> {
    let y: i16 = y.parse().ok()?;
    if !year_ok(y) {
        return None;
    }
    Date::new(y, m.parse().ok()?, d.parse().ok()?).ok()
}

/// `HHMMSS` (and `mmm` after it) or `HH`, `MM`, `SS` runs.
fn hms(h: &str, m: &str, s: &str) -> Option<(i8, i8, i8)> {
    let (h, m, s): (i8, i8, i8) = (h.parse().ok()?, m.parse().ok()?, s.parse().ok()?);
    (h < 24 && m < 60 && s < 61).then_some((h, m, s))
}

fn moment(d: Date, (h, m, s): (i8, i8, i8), subsec: Option<&str>) -> SidecarDate {
    SidecarDate::moment(
        format!(
            "{:04}:{:02}:{:02} {h:02}:{m:02}:{s:02}",
            d.year(),
            d.month(),
            d.day()
        ),
        subsec.map(str::to_owned),
        None,
    )
}

fn day_of(d: Date) -> Option<SidecarDate> {
    SidecarDate::approximate(d.year(), Some(d.month()), Some(d.day()))
}

/// A separator between a date and a time: `_`, `-`, ` `, `T`, ` at `.
fn time_sep(r: Option<&Run<'_>>) -> bool {
    r.is_some_and(|r| {
        !r.digits && matches!(r.text.trim(), "" | "_" | "-" | "T" | "at" | "um" | "à")
    })
}

/// Dates in a file name: `IMG_20190312_101530`, `PXL_20190312_101530123`,
/// `20190312-101530-120` (Shoebox's), `Screenshot 2019-03-12 at 10.15.30`,
/// `IMG-20190312-WA0001` (the day), `1552385730` or `1552385730123` (Unix
/// time, in `tz`).
pub fn in_name(name: &str, tz: &TimeZone) -> Vec<Clue> {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let r = runs(stem);
    let digits = |i: usize, n: usize| r.get(i).filter(|x| x.digits && x.text.len() == n);
    let sep = |i: usize, set: &[&str]| r.get(i).is_some_and(|x| !x.digits && set.contains(&x.text));
    let clue = |date: SidecarDate, confidence| Clue {
        date,
        source: Source::FileName,
        confidence,
        evidence: name.to_owned(),
    };
    for i in 0..r.len() {
        let t = r[i].text;
        if !r[i].digits {
            continue;
        }
        // YYYYMMDDHHMMSS.
        if t.len() == 14
            && let (Some(d), Some(time)) = (
                ymd(&t[0..4], &t[4..6], &t[6..8]),
                hms(&t[8..10], &t[10..12], &t[12..14]),
            )
        {
            return vec![clue(moment(d, time, None), Confidence::High)];
        }
        // YYYYMMDD, then perhaps _HHMMSS[mmm][-mmm].
        if t.len() == 8
            && let Some(d) = ymd(&t[0..4], &t[4..6], &t[6..8])
        {
            if time_sep(r.get(i + 1))
                && let Some(n) = r.get(i + 2).filter(|x| x.digits)
                && (n.text.len() == 6 || n.text.len() == 9)
                && let Some(time) = hms(&n.text[0..2], &n.text[2..4], &n.text[4..6])
            {
                let sub = if n.text.len() == 9 {
                    Some(&n.text[6..9])
                } else if sep(i + 3, &["-", "_", "."]) {
                    digits(i + 4, 3).map(|x| x.text)
                } else {
                    None
                };
                return vec![clue(moment(d, time, sub), Confidence::High)];
            }
            if let Some(day) = day_of(d) {
                return vec![clue(day, Confidence::Medium)];
            }
        }
        // YYYY-MM-DD, then perhaps a time as HH.MM.SS or HHMMSS.
        if t.len() == 4
            && sep(i + 1, &["-", "_", "."])
            && let (Some(m), Some(dd)) = (digits(i + 2, 2), digits(i + 4, 2))
            && r.get(i + 3).map(|x| x.text) == r.get(i + 1).map(|x| x.text)
            && let Some(d) = ymd(t, m.text, dd.text)
        {
            if time_sep(r.get(i + 5)) {
                if let Some(n) = digits(i + 6, 6)
                    && let Some(time) = hms(&n.text[0..2], &n.text[2..4], &n.text[4..6])
                {
                    return vec![clue(moment(d, time, None), Confidence::High)];
                }
                if let (Some(h), Some(mi), Some(s)) =
                    (digits(i + 6, 2), digits(i + 8, 2), digits(i + 10, 2))
                    && sep(i + 7, &[".", ":", "-", "_", " "])
                    && sep(i + 9, &[".", ":", "-", "_", " "])
                    && let Some(time) = hms(h.text, mi.text, s.text)
                {
                    return vec![clue(moment(d, time, None), Confidence::High)];
                }
            }
            if let Some(day) = day_of(d) {
                return vec![clue(day, Confidence::Medium)];
            }
        }
        // Unix time in seconds or milliseconds.
        if t.len() == 10 || t.len() == 13 {
            let n: i64 = t.parse().unwrap_or(0);
            let ms = if t.len() == 13 { n } else { n * 1000 };
            if let Ok(at) = jiff::Timestamp::from_millisecond(ms) {
                let z = at.to_zoned(tz.clone());
                if (2000..=jiff::Zoned::now().year()).contains(&z.year()) {
                    let sub = z.subsec_nanosecond() / 1_000_000;
                    let date = SidecarDate::moment(
                        z.strftime("%Y:%m:%d %H:%M:%S").to_string(),
                        (sub > 0).then(|| format!("{sub:03}")),
                        Some(z.strftime("%:z").to_string()),
                    );
                    return vec![clue(date, Confidence::Medium)];
                }
            }
        }
    }
    Vec::new()
}

// ---- In the folders ---------------------------------------------------

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// A month's name or its first three letters (`Sept` too).
fn month_named(word: &str) -> Option<i8> {
    let w = word.to_ascii_lowercase();
    if w.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|m| *m == w || (m.starts_with(&w) && (w.len() == 3 || w == "sept")))
        .and_then(|i| i8::try_from(i + 1).ok())
}

/// What one folder name says: a year, a month (with the year above it, if
/// it has none) or a day.
fn folder_date(name: &str, year_above: Option<i16>) -> Option<SidecarDate> {
    let r = runs(name);
    let first = r.first()?;
    let next_is_end = |i: usize| r.get(i).is_none_or(|x| !x.digits);
    if first.digits {
        let t = first.text;
        // YYYYMMDD.
        if t.len() == 8
            && next_is_end(1)
            && let Some(d) = ymd(&t[0..4], &t[4..6], &t[6..8])
        {
            return day_of(d);
        }
        if t.len() == 4
            && let Ok(y) = t.parse::<i16>()
            && year_ok(y)
        {
            // YYYY-MM-DD, YYYY-MM, YYYY.
            let s = r.get(1).map(|x| x.text);
            if matches!(s, Some("-" | "_" | "."))
                && let Some(m) = r.get(2).filter(|x| x.digits && x.text.len() == 2)
            {
                if r.get(3).map(|x| x.text) == s
                    && let Some(d) = r.get(4).filter(|x| x.digits && x.text.len() == 2)
                    && next_is_end(5)
                    && let Some(date) = ymd(t, m.text, d.text)
                {
                    return day_of(date);
                }
                if next_is_end(3) {
                    let m: i8 = m.text.parse().ok()?;
                    return SidecarDate::approximate(y, Some(m), None);
                }
            }
            // `2014 March`, `2014`, `2014 Japan`.
            let word = r
                .get(1)
                .and_then(|x| x.text.split_whitespace().next())
                .and_then(month_named);
            if r.len() == 1 || r.get(1).is_some_and(|x| x.text.starts_with(' ')) {
                return SidecarDate::approximate(y, word, None);
            }
            return None;
        }
        // `03 Japan` or `03` under a year.
        if t.len() == 2
            && let Some(y) = year_above
            && (r.len() == 1
                || r.get(1)
                    .is_some_and(|x| x.text.starts_with([' ', '-', '_'])))
            && let Ok(m) = t.parse::<i8>()
        {
            return SidecarDate::approximate(y, Some(m), None);
        }
        return None;
    }
    // `March 2014`, `Mar 2014`, `March` under a year.
    let word = first.text.split_whitespace().next()?;
    let m = month_named(word)?;
    let y = r
        .iter()
        .find(|x| x.digits && x.text.len() == 4)
        .and_then(|x| x.text.parse::<i16>().ok())
        .filter(|y| year_ok(*y))
        .or(year_above)?;
    SidecarDate::approximate(y, Some(m), None)
}

/// Dates in the names of the folders above a photo (`rel`, its path in
/// its root): `2014/03 Japan`, `March 2014`, `2014-03-12 Party`. The most
/// precise wins; the innermost if two are as precise.
pub fn in_folders(rel: &Path) -> Vec<Clue> {
    let Some(parent) = rel.parent() else {
        return Vec::new();
    };
    let mut year = None;
    let mut best: Option<SidecarDate> = None;
    for c in parent.components() {
        let Some(name) = c.as_os_str().to_str() else {
            continue;
        };
        if name.starts_with('_') || name.starts_with('.') {
            continue;
        }
        let Some(d) = folder_date(name, year) else {
            continue;
        };
        year = d.time.get(0..4).and_then(|y| y.parse().ok());
        if best.as_ref().is_none_or(|b| d.precision <= b.precision) {
            best = Some(d);
        }
    }
    best.map(|date| {
        let confidence = match date.precision {
            Precision::Year => Confidence::Low,
            _ => Confidence::Medium,
        };
        vec![Clue {
            date,
            source: Source::Folder,
            confidence,
            evidence: parent.display().to_string(),
        }]
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc() -> TimeZone {
        TimeZone::UTC
    }

    fn named(name: &str) -> Option<(String, Precision, Option<String>)> {
        in_name(name, &utc())
            .into_iter()
            .next()
            .map(|c| (c.date.time, c.date.precision, c.date.subsec))
    }

    #[test]
    fn dates_in_file_names() {
        let m = |t: &str| Some((t.to_owned(), Precision::Moment, None));
        assert_eq!(named("IMG_20190312_101530.jpg"), m("2019:03:12 10:15:30"));
        assert_eq!(named("20190312101530.jpg"), m("2019:03:12 10:15:30"));
        assert_eq!(
            named("PXL_20210101_123456789.jpg"),
            Some((
                "2021:01:01 12:34:56".into(),
                Precision::Moment,
                Some("789".into())
            ))
        );
        assert_eq!(
            named("20260312-102539-730-photos.heic"),
            Some((
                "2026:03:12 10:25:39".into(),
                Precision::Moment,
                Some("730".into())
            ))
        );
        assert_eq!(
            named("Screenshot 2019-03-12 at 10.15.30.png"),
            m("2019:03:12 10:15:30")
        );
        assert_eq!(named("2019-03-12_101530.jpg"), m("2019:03:12 10:15:30"));
        assert_eq!(
            named("IMG-20190312-WA0001.jpg"),
            Some(("2019:03:12 12:00:00".into(), Precision::Day, None))
        );
        assert_eq!(named("1552385730.jpg"), m("2019:03:12 10:15:30"));
        assert_eq!(
            named("1552385730123.jpg"),
            Some((
                "2019:03:12 10:15:30".into(),
                Precision::Moment,
                Some("123".into())
            ))
        );
        // Not dates: counters, impossible days, sizes.
        for n in [
            "DSC_1234.JPG",
            "IMG_0001.jpg",
            "20191332_101530.jpg",
            "4032x3024.jpg",
            "99991231.jpg",
            "IMG_20190312_256199.jpg",
        ] {
            let got = named(n);
            assert!(
                got.is_none() || got.as_ref().is_some_and(|g| g.1 == Precision::Day),
                "{n}: {got:?}"
            );
        }
        assert_eq!(named("DSC_1234.JPG"), None);
        assert_eq!(named("99991231.jpg"), None);
    }

    fn folder(rel: &str) -> Option<(String, Precision)> {
        in_folders(Path::new(rel))
            .into_iter()
            .next()
            .map(|c| (c.date.time, c.date.precision))
    }

    #[test]
    fn dates_in_folder_names() {
        let month = |t: &str| Some((t.to_owned(), Precision::Month));
        assert_eq!(
            folder("2014/2014-03-12 Party/a.jpg"),
            Some(("2014:03:12 12:00:00".into(), Precision::Day))
        );
        assert_eq!(folder("2014/03 Japan/a.jpg"), month("2014:03:15 12:00:00"));
        assert_eq!(
            folder("Trips/March 2014/a.jpg"),
            month("2014:03:15 12:00:00")
        );
        assert_eq!(folder("2014/Sept/a.jpg"), month("2014:09:15 12:00:00"));
        assert_eq!(folder("2014-07/a.jpg"), month("2014:07:15 12:00:00"));
        assert_eq!(
            folder("Old/2014 Japan/a.jpg"),
            Some(("2014:07:01 12:00:00".into(), Precision::Year))
        );
        assert_eq!(
            folder("20140312/a.jpg"),
            Some(("2014:03:12 12:00:00".into(), Precision::Day))
        );
        for none in [
            "a.jpg",
            "_undated/4032x3024/a.jpg",
            "Photos/Holiday/a.jpg",
            "100NIKON/a.jpg",
            "Maybe/a.jpg",
        ] {
            assert_eq!(folder(none), None, "{none}");
        }
    }

    #[test]
    fn agreeing_clues_rise_and_the_best_comes_first() {
        let mut c = in_name("IMG-20190312-WA0001.jpg", &utc());
        c.extend(in_folders(Path::new(
            "2019/2019-03-12/IMG-20190312-WA0001.jpg",
        )));
        c.extend(in_folders(Path::new("2018/x.jpg")));
        rank(&mut c);
        assert_eq!(c[0].confidence, Confidence::High);
        assert_eq!(c[0].date.time, "2019:03:12 12:00:00");
        assert_eq!(c.last().unwrap().confidence, Confidence::Low);
    }

    /// A JPEG with no capture date but the others: digitized, modified,
    /// GPS.
    fn jpeg_with_other_dates() -> Vec<u8> {
        use shoebox_fixtures::{TiffWriter, Val};
        let mut w = TiffWriter::new();
        let exif = w.ifd(
            vec![
                (0x9004, Val::Ascii("2019:03:12 10:15:30".into())),
                (0x9292, Val::Ascii("120".into())),
            ],
            0,
        );
        let gps = w.ifd(
            vec![
                (0x0007, Val::Rational(vec![(1, 1), (15, 1), (3050, 100)])),
                (0x001D, Val::Ascii("2019:03:12".into())),
            ],
            0,
        );
        let ifd0 = w.ifd(
            vec![
                (0x0132, Val::Ascii("2020:01:01 09:00:00".into())),
                (0x8769, Val::Long(vec![exif])),
                (0x8825, Val::Long(vec![gps])),
            ],
            0,
        );
        let tiff = w.finish(ifd0);
        let jpeg = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(1, 32, 24), 80).unwrap();
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let mut out = jpeg[..2].to_vec();
        out.extend([0xFF, 0xE1]);
        out.extend(((app1.len() + 2) as u16).to_be_bytes());
        out.extend(app1);
        out.extend(&jpeg[2..]);
        out
    }

    #[test]
    fn other_dates_in_the_file() {
        let tz = TimeZone::fixed(jiff::tz::Offset::from_seconds(9 * 3600).unwrap());
        let mut c = in_bytes(&jpeg_with_other_dates(), Format::Jpeg, &tz);
        rank(&mut c);
        let by = |s: Source| c.iter().find(|x| x.source == s).unwrap();
        let d = by(Source::Digitized);
        assert_eq!(d.date.time, "2019:03:12 10:15:30");
        assert_eq!(d.date.subsec.as_deref(), Some("120"));
        // 01:15:30.5 UTC is 10:15:30.5 at +09:00: it agrees with the
        // digitized date.
        let g = by(Source::Gps);
        assert_eq!(g.date.time, "2019:03:12 10:15:30");
        assert_eq!(g.date.offset.as_deref(), Some("+09:00"));
        assert_eq!(g.date.subsec.as_deref(), Some("500"));
        assert_eq!(g.evidence, "2019:03:12 01:15:30 UTC");
        let m = by(Source::Modified);
        assert_eq!(
            (m.confidence, m.date.time.as_str()),
            (Confidence::Medium, "2020:01:01 09:00:00")
        );
        assert_ne!(c[0].source, Source::Modified);
    }

    #[test]
    fn a_png_creation_time() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let text = b"Creation Time\0Tue, 12 Mar 2019 10:15:30 +0100";
        png.extend((text.len() as u32).to_be_bytes());
        png.extend(b"tEXt");
        png.extend(text);
        png.extend([0; 4]);
        let c = in_bytes(&png, Format::Png, &utc());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].date.time, "2019:03:12 10:15:30");
        assert_eq!(c[0].date.offset.as_deref(), Some("+01:00"));
    }

    #[test]
    fn a_date_in_the_files_own_xmp() {
        let xmp = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/" photoshop:DateCreated="2011-08"/></rdf:RDF></x:xmpmeta>"#;
        let jpeg = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(1, 32, 24), 80).unwrap();
        let mut seg = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        seg.extend_from_slice(xmp);
        let mut out = jpeg[..2].to_vec();
        out.extend([0xFF, 0xE1]);
        out.extend(((seg.len() + 2) as u16).to_be_bytes());
        out.extend(seg);
        out.extend(&jpeg[2..]);
        let c = in_bytes(&out, Format::Jpeg, &utc());
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].source, Source::FileXmp);
        assert_eq!(c[0].date.precision, Precision::Month);
        assert_eq!(c[0].date.to_xmp(), "2011-08");
    }
}
