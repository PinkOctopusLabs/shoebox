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

use std::collections::HashMap;
use std::path::PathBuf;

use crate::catalog::{Catalog, Photo};
use crate::dupes::{self, mih::MultiIndex};
use crate::hash::{Analysis, Digest};
use crate::photos::ResourceKind;
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
    /// Another file of the same frame (its stack).
    Stack,
    /// Another file of the same iCloud Photos asset.
    Photos,
    /// The same picture (a copy, or a perceptual match).
    SamePicture,
    /// Between the camera's frames before and after it.
    Sequence,
    FileName,
    Folder,
    /// The dated photos in its folder.
    Neighbours,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Digitized => "Digitized date in the file",
            Source::Gps => "GPS time in the file",
            Source::FileXmp => "Date in the file's XMP",
            Source::PngCreated => "Creation time in the PNG",
            Source::Modified => "Modified date in the file",
            Source::Stack => "Same frame as",
            Source::Photos => "Same iCloud Photos asset as",
            Source::SamePicture => "Same picture as",
            Source::Sequence => "Between frames",
            Source::FileName => "File name",
            Source::Folder => "Folder",
            Source::Neighbours => "Photos in its folder",
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

impl Clue {
    /// The evidence in words: "File name: IMG_….jpg", "same picture as
    /// DSC_1234.NEF", "between frames DSC_ 1233 and 1236".
    pub fn describe(&self) -> String {
        let e = &self.evidence;
        match self.source {
            Source::Stack => format!("same frame as {e}"),
            Source::Photos => format!("same iCloud Photos asset as {e}"),
            Source::SamePicture => match self.confidence {
                Confidence::High => format!("same picture as {e}"),
                _ => format!("looks like {e}"),
            },
            Source::Sequence => format!("between frames {e}"),
            Source::Neighbours => format!("{e} photos in its folder"),
            s => format!("{}: {e}", s.label()),
        }
    }
}

/// A photo's own clues (in the file, its name and its folders), not yet
/// ranked ([`rank`] them with any [`Library::clues`]). `rel`: its path in
/// its root (the folders above it are clues). A cloud-only file isn't read
/// (`cloud`), so only its name and folders give clues.
pub fn clues(path: &Path, rel: &Path, format: Format, cloud: bool) -> Vec<Clue> {
    let mut out = Vec::new();
    if !cloud {
        out.extend(in_file(path, format));
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        out.extend(in_name(name, &TimeZone::system()));
    }
    out.extend(in_folders(rel));
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

// ---- In the library ---------------------------------------------------

/// Frames either side further than this aren't asked.
const SEQUENCE_REACH: u64 = 50;

/// Dated photos by frame number: (number, catalog index), sorted.
type Frames = Vec<(u64, usize)>;

/// The library as its clues need it: the dated photos by folder, by camera
/// sequence, by frame and iCloud Photos asset, and by picture.
pub struct Library<'a> {
    catalog: &'a Catalog,
    by_path: HashMap<&'a Path, usize>,
    /// Dated photos by folder (not `_` folders such as `_undated`).
    folders: HashMap<&'a Path, Vec<usize>>,
    /// Dated photos by folder and file-number series, by number.
    series: HashMap<(&'a Path, String, usize), Frames>,
    /// Dated photos by camera (model, serial), by shutter count.
    shutters: HashMap<(String, Option<String>), Frames>,
    /// Each iCloud Photos asset's library files, and which asset a file is.
    assets: Vec<Vec<(ResourceKind, PathBuf)>>,
    asset_of: HashMap<PathBuf, usize>,
    /// Hashed dated photos (as the duplicate finder compares them).
    hashed: Vec<(usize, dupes::Item)>,
    exact: HashMap<Digest, Vec<usize>>,
    data: HashMap<Digest, Vec<usize>>,
    tree: MultiIndex,
}

/// A photo's date as the catalog has it.
fn date_of(p: &Photo) -> Option<SidecarDate> {
    let x = &p.meta.exif;
    Some(SidecarDate {
        time: x.capture_time.clone()?,
        subsec: x.subsec.clone(),
        offset: x.offset.clone(),
        precision: p.meta.precision(),
    })
}

/// `exif::capture_seconds` back to EXIF's form.
fn time_at(secs: f64) -> Option<String> {
    let t = jiff::Timestamp::from_second(secs.floor() as i64).ok()?;
    Some(
        t.to_zoned(TimeZone::UTC)
            .strftime("%Y:%m:%d %H:%M:%S")
            .to_string(),
    )
}

/// The day, month or year around `secs`.
fn approx_at(secs: f64, precision: Precision) -> Option<SidecarDate> {
    let t = time_at(secs)?;
    let num = |r: std::ops::Range<usize>| t.get(r)?.parse::<i16>().ok();
    let (y, m, d) = (
        num(0..4)?,
        i8::try_from(num(5..7)?).ok()?,
        i8::try_from(num(8..10)?).ok()?,
    );
    match precision {
        Precision::Moment | Precision::Day => SidecarDate::approximate(y, Some(m), Some(d)),
        Precision::Month => SidecarDate::approximate(y, Some(m), None),
        Precision::Year => SidecarDate::approximate(y, None, None),
    }
}

/// How much two times share: the same day, month or year (by their
/// EXIF-form strings), or nothing.
fn shared(a: f64, b: f64) -> Option<Precision> {
    let (a, b) = (time_at(a)?, time_at(b)?);
    [
        (10, Precision::Day),
        (7, Precision::Month),
        (4, Precision::Year),
    ]
    .into_iter()
    .find(|(n, _)| a.get(..*n) == b.get(..*n))
    .map(|(_, p)| p)
}

/// A camera's file number: `DSC_1234`, `_DSC1234`, `IMG_0001`, `P1000123`,
/// `f0003`: a prefix of one to five letters or `_`, then four to seven
/// digits. (prefix, number, how many digits).
pub fn file_number(name: &str) -> Option<(String, u64, usize)> {
    let stem = Path::new(name).file_stem()?.to_str()?;
    let r = runs(stem);
    let (prefix, digits) = (r.first()?, r.get(1)?);
    let ok = !prefix.digits
        && (1..=5).contains(&prefix.text.len())
        && prefix
            .text
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'_')
        && digits.digits
        && (4..=7).contains(&digits.text.len());
    ok.then(|| {
        (
            prefix.text.to_ascii_uppercase(),
            digits.text.parse().ok(),
            digits.text.len(),
        )
    })
    .and_then(|(p, n, d)| Some((p, n?, d)))
}

/// Where `n` falls between the dated frames of `seq` (sorted by number):
/// the time between the nearest before and after, as precise as they agree.
fn between(seq: &[(u64, f64)], n: u64) -> Option<(SidecarDate, Confidence, Vec<u64>)> {
    let at = seq.partition_point(|(k, _)| *k < n);
    let before = at
        .checked_sub(1)
        .and_then(|i| seq.get(i))
        .filter(|(k, _)| n - k <= SEQUENCE_REACH);
    let after = seq
        .get(at)
        .filter(|(k, _)| *k > n && k - n <= SEQUENCE_REACH);
    match (before, after) {
        (Some(&(kb, tb)), Some(&(ka, ta))) if ta >= tb => {
            let t = tb + (ta - tb) * (n - kb) as f64 / (ka - kb) as f64;
            let precision = shared(tb, ta)?;
            let close = ka - kb <= 4;
            let confidence = match (precision, close) {
                (Precision::Day, true) => Confidence::High,
                (Precision::Day, false) | (Precision::Month, true) => Confidence::Medium,
                _ => Confidence::Low,
            };
            Some((approx_at(t, precision)?, confidence, vec![kb, ka]))
        }
        // One side only: close enough to say the day.
        (Some(&(k, t)), None) | (None, Some(&(k, t))) if k.abs_diff(n) <= 3 => {
            Some((approx_at(t, Precision::Day)?, Confidence::Medium, vec![k]))
        }
        _ => None,
    }
}

impl<'a> Library<'a> {
    /// `assets`: each iCloud Photos asset's library files (from the sync
    /// record); `hashed`: the dated photos the duplicate finder has
    /// analysed, by catalog index.
    pub fn new(
        catalog: &'a Catalog,
        assets: Vec<Vec<(ResourceKind, PathBuf)>>,
        hashed: Vec<(usize, Analysis)>,
    ) -> Self {
        let mut l = Library {
            catalog,
            by_path: HashMap::new(),
            folders: HashMap::new(),
            series: HashMap::new(),
            shutters: HashMap::new(),
            asset_of: HashMap::new(),
            assets,
            hashed: Vec::new(),
            exact: HashMap::new(),
            data: HashMap::new(),
            tree: MultiIndex::new(),
        };
        for (i, p) in catalog.photos.iter().enumerate() {
            l.by_path.insert(p.path.as_path(), i);
            if p.meta.capture_seconds().is_none() {
                continue;
            }
            let Some(dir) = p.path.parent() else {
                continue;
            };
            let underscored = Path::new(&p.rel)
                .components()
                .any(|c| c.as_os_str().to_str().is_some_and(|s| s.starts_with('_')));
            if !underscored {
                l.folders.entry(dir).or_default().push(i);
            }
            if let Some((prefix, n, digits)) = p
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(file_number)
            {
                l.series
                    .entry((dir, prefix, digits))
                    .or_default()
                    .push((n, i));
            }
            let x = &p.meta.exif;
            if let (Some(model), Some(n)) = (&x.model, x.shutter_count) {
                l.shutters
                    .entry((model.clone(), x.serial.clone()))
                    .or_default()
                    .push((u64::from(n), i));
            }
        }
        for v in l.series.values_mut().chain(l.shutters.values_mut()) {
            v.sort_unstable();
        }
        for (a, files) in l.assets.iter().enumerate() {
            for (_, f) in files {
                l.asset_of.insert(f.clone(), a);
            }
        }
        for (i, a) in hashed {
            let Some(p) = catalog.photos.get(i) else {
                continue;
            };
            let k = l.hashed.len();
            l.exact.entry(a.blake3).or_default().push(k);
            if let Some(h) = a.image_data_hash {
                l.data.entry(h).or_default().push(k);
            }
            for h in &a.hashes {
                l.tree.insert(h.phash, k);
            }
            l.hashed.push((i, item(p, &a)));
        }
        l
    }

    /// How many dated photos have hashes to compare with.
    pub fn hashed(&self) -> usize {
        self.hashed.len()
    }

    /// Photo `i`'s clues from the rest of the library. `own`: its analysis
    /// (hashes), if it could be made.
    pub fn clues(&self, i: usize, own: Option<&Analysis>) -> Vec<Clue> {
        let Some(p) = self.catalog.photos.get(i) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        out.extend(self.same_frame(i));
        out.extend(self.same_asset(p));
        if let Some(a) = own {
            out.extend(self.same_picture(i, a));
        }
        out.extend(self.sequence(p));
        out.extend(self.neighbours(i, p));
        out
    }

    fn name(&self, i: usize) -> String {
        self.catalog.photos[i]
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn same_frame(&self, i: usize) -> Option<Clue> {
        let s = self.catalog.stacks.get(*self.catalog.stack_of.get(i)?)?;
        let (j, date) = s
            .members
            .iter()
            .chain(&s.variants)
            .filter(|&&j| j != i)
            .find_map(|&j| Some((j, date_of(&self.catalog.photos[j])?)))?;
        Some(Clue {
            date,
            source: Source::Stack,
            confidence: Confidence::High,
            evidence: self.name(j),
        })
    }

    fn same_asset(&self, p: &Photo) -> Option<Clue> {
        let files = self.assets.get(*self.asset_of.get(&p.path)?)?;
        let mut others: Vec<&(ResourceKind, PathBuf)> =
            files.iter().filter(|(_, f)| *f != p.path).collect();
        // The original first.
        others.sort_by_key(|(k, _)| *k != ResourceKind::Original);
        let (j, date) = others.iter().find_map(|(_, f)| {
            let j = *self.by_path.get(f.as_path())?;
            Some((j, date_of(&self.catalog.photos[j])?))
        })?;
        Some(Clue {
            date,
            source: Source::Photos,
            confidence: Confidence::High,
            evidence: self.name(j),
        })
    }

    fn same_picture(&self, i: usize, a: &Analysis) -> Option<Clue> {
        let me = item(&self.catalog.photos[i], a);
        let mut cand: Vec<usize> = self.exact.get(&a.blake3).cloned().unwrap_or_default();
        if let Some(h) = a.image_data_hash.and_then(|h| self.data.get(&h)) {
            cand.extend(h);
        }
        let cfg = dupes::Config::default();
        if let Some(h) = a.hashes.first() {
            cand.extend(
                self.tree
                    .find(h.phash, cfg.threshold)
                    .into_iter()
                    .map(|(k, _)| k),
            );
        }
        cand.sort_unstable();
        cand.dedup();
        let (k, m) = cand
            .into_iter()
            .filter(|&k| self.hashed[k].0 != i)
            .filter_map(|k| Some((k, dupes::compare(&me, &self.hashed[k].1, &cfg)?)))
            .min_by_key(|(_, m)| (m.level, m.distance))?;
        let j = self.hashed[k].0;
        let date = date_of(&self.catalog.photos[j])?;
        let (date, confidence) = match m.level {
            dupes::Level::Similar => {
                let secs = self.catalog.photos[j].meta.capture_seconds()?;
                (approx_at(secs, Precision::Day)?, Confidence::Medium)
            }
            _ => (date, Confidence::High),
        };
        Some(Clue {
            date,
            source: Source::SamePicture,
            confidence,
            evidence: self.name(j),
        })
    }

    fn sequence(&self, p: &Photo) -> Option<Clue> {
        let secs = |list: &[(u64, usize)]| -> Vec<(u64, f64)> {
            list.iter()
                .filter_map(|&(n, j)| Some((n, self.catalog.photos[j].meta.capture_seconds()?)))
                .collect()
        };
        let x = &p.meta.exif;
        if let (Some(model), Some(n)) = (&x.model, x.shutter_count)
            && let Some(list) = self.shutters.get(&(model.clone(), x.serial.clone()))
            && let Some((date, confidence, frames)) = between(&secs(list), u64::from(n))
        {
            let frames: Vec<String> = frames.iter().map(u64::to_string).collect();
            return Some(Clue {
                date,
                source: Source::Sequence,
                confidence,
                evidence: format!("{model} shutter counts {}", frames.join(" and ")),
            });
        }
        let name = p.path.file_name()?.to_str()?;
        let (prefix, n, digits) = file_number(name)?;
        let list = self
            .series
            .get(&(p.path.parent()?, prefix.clone(), digits))?;
        let (date, confidence, frames) = between(&secs(list), n)?;
        // As the files are named: `DSC_0010 and DSC_0012`.
        let shown = name.get(..prefix.len())?;
        let frames: Vec<String> = frames
            .iter()
            .map(|k| format!("{shown}{k:0digits$}"))
            .collect();
        Some(Clue {
            date,
            source: Source::Sequence,
            confidence,
            evidence: frames.join(" and "),
        })
    }

    fn neighbours(&self, i: usize, p: &Photo) -> Option<Clue> {
        let list: Vec<f64> = self
            .folders
            .get(p.path.parent()?)?
            .iter()
            .filter(|&&j| j != i)
            .filter_map(|&j| self.catalog.photos[j].meta.capture_seconds())
            .collect();
        if list.len() < 2 {
            return None;
        }
        let (lo, hi) = list
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &t| {
                (a.min(t), b.max(t))
            });
        let precision = shared(lo, hi)?;
        let mut sorted = list.clone();
        sorted.sort_by(f64::total_cmp);
        let median = sorted[sorted.len() / 2];
        let confidence = match (precision, list.len()) {
            (Precision::Year, _) | (_, 2) => Confidence::Low,
            _ => Confidence::Medium,
        };
        Some(Clue {
            date: approx_at(median, precision)?,
            source: Source::Neighbours,
            confidence,
            evidence: format!("{} dated", list.len()),
        })
    }
}

/// A photo and its analysis as the duplicate finder compares them.
fn item(p: &Photo, a: &Analysis) -> dupes::Item {
    dupes::Item {
        role: crate::scan::Role::Scan,
        format: p.format,
        blake3: a.blake3,
        image_data_hash: a.image_data_hash,
        hashes: a.hashes.clone(),
        pixels: u64::from(a.width.unwrap_or(0)) * u64::from(a.height.unwrap_or(0)),
        siblings: None,
        luma_stddev: a.luma_stddev,
        capture: p.meta.capture_seconds(),
        merged: false,
    }
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

    #[test]
    fn file_numbers() {
        assert_eq!(file_number("DSC_1234.JPG"), Some(("DSC_".into(), 1234, 4)));
        assert_eq!(file_number("_dsc0007.nef"), Some(("_DSC".into(), 7, 4)));
        assert_eq!(
            file_number("P1000123.RW2"),
            Some(("P".into(), 1_000_123, 7))
        );
        assert_eq!(file_number("f0003.jpg"), Some(("F".into(), 3, 4)));
        for none in [
            "IMG_20190312_101530.jpg",
            "photo.jpg",
            "DSC_12.JPG",
            "1234.jpg",
        ] {
            assert_eq!(file_number(none), None, "{none}");
        }
    }

    /// A library with the library's kinds of clue: a camera sequence, a
    /// folder of dated photos, an undated copy of a dated picture, an
    /// iCloud Photos asset's edit, and a stack's other file.
    #[test]
    fn clues_from_the_library() {
        use crate::cache::Cache;
        use crate::scan::{Role, Root};
        use shoebox_fixtures::{Shot, jpeg, scene, with_exif};

        let t = tempfile::tempdir().unwrap();
        let lib = t.path().canonicalize().unwrap();
        let shot = |dt: &'static str| Shot {
            datetime: dt,
            subsec: "",
            orientation: 1,
            serial: "1",
        };
        let dated = |seed, dt| with_exif(&jpeg(&scene(seed, 160, 120), 85).unwrap(), &shot(dt));
        let plain = |seed| jpeg(&scene(seed, 160, 120), 85).unwrap();
        let write = |rel: &str, bytes: Vec<u8>| {
            let p = lib.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        };
        // A roll: frames 10 and 12 dated, 11 not.
        write("roll/DSC_0010.JPG", dated(1, "2019:03:12 10:00:00"));
        write("roll/DSC_0011.JPG", plain(2));
        write("roll/DSC_0012.JPG", dated(3, "2019:03:12 10:00:40"));
        // An undated copy of a dated picture, elsewhere.
        write("trip/beach.jpg", dated(4, "2018:07:01 15:30:00"));
        write("_undated/160x120/f0001.jpg", plain(4));
        // An iCloud Photos asset: the original dated, its edit not.
        write("2020/IMG_0001.jpg", dated(5, "2020:01:02 09:00:00"));
        write("2020/IMG_0001-photos.jpg", plain(6));
        // A stack: the TIFF of a dated JPEG.
        write("stack/frame.jpg", dated(7, "2021:05:05 05:05:05"));
        write(
            "stack/frame.tif",
            shoebox_fixtures::tiff(&scene(8, 160, 120)).unwrap(),
        );

        let mut cache = Cache::open_in_memory().unwrap();
        let roots = vec![Root::new(&lib, Role::Scan).unwrap()];
        let (c, _) = Catalog::refresh(&mut cache, roots, &|_, _| {}).unwrap();
        let find = |rel: &str| c.photos.iter().position(|p| p.rel == rel).unwrap();
        let hashed: Vec<(usize, Analysis)> = (0..c.photos.len())
            .filter(|&i| c.photos[i].meta.exif.capture_time.is_some())
            .map(|i| (i, crate::hash::analyze_path(&c.photos[i].path).unwrap()))
            .collect();
        let asset = vec![
            (ResourceKind::Original, lib.join("2020/IMG_0001.jpg")),
            (ResourceKind::Edited, lib.join("2020/IMG_0001-photos.jpg")),
        ];
        let l = Library::new(&c, vec![asset], hashed);
        let clues_of = |rel: &str| {
            let i = find(rel);
            let own = crate::hash::analyze_path(&c.photos[i].path).ok();
            let mut v = l.clues(i, own.as_ref());
            rank(&mut v);
            v
        };
        let by = |v: &[Clue], s: Source| v.iter().find(|c| c.source == s).cloned();

        let roll = clues_of("roll/DSC_0011.JPG");
        let seq = by(&roll, Source::Sequence).expect("sequence");
        assert_eq!(
            (seq.date.time.as_str(), seq.date.precision, seq.confidence),
            ("2019:03:12 12:00:00", Precision::Day, Confidence::High)
        );
        assert_eq!(seq.describe(), "between frames DSC_0010 and DSC_0012");
        let n = by(&roll, Source::Neighbours).expect("neighbours");
        assert_eq!(
            (n.date.precision, n.date.time.as_str()),
            (Precision::Day, "2019:03:12 12:00:00")
        );

        let copy = clues_of("_undated/160x120/f0001.jpg");
        let same = by(&copy, Source::SamePicture).expect("same picture");
        assert_eq!(
            (same.date.time.as_str(), same.confidence),
            ("2018:07:01 15:30:00", Confidence::High)
        );
        assert_eq!(same.describe(), "same picture as beach.jpg");
        assert!(by(&copy, Source::Neighbours).is_none(), "not from _undated");

        let edit = clues_of("2020/IMG_0001-photos.jpg");
        let photos = by(&edit, Source::Photos).expect("asset");
        assert_eq!(photos.date.time, "2020:01:02 09:00:00");
        assert_eq!(photos.evidence, "IMG_0001.jpg");

        let tif = clues_of("stack/frame.tif");
        let stack = by(&tif, Source::Stack).expect("stack");
        assert_eq!(
            (stack.date.time.as_str(), stack.evidence.as_str()),
            ("2021:05:05 05:05:05", "frame.jpg")
        );
    }

    #[test]
    fn frames_between() {
        let day = 86_400.0;
        // Out of order (a camera reset) or too far: nothing.
        assert!(between(&[(10, 2.0 * day), (20, day)], 15).is_none());
        assert!(between(&[(10, day), (200, day)], 100).is_none());
        // Months apart: the year at most.
        let (d, c, _) = between(&[(10, 0.0), (20, 100.0 * day)], 15).unwrap();
        assert_eq!((d.precision, c), (Precision::Year, Confidence::Low));
        // One side only, a frame away: the day.
        let (d, c, _) = between(&[(10, day)], 11).unwrap();
        assert_eq!((d.precision, c), (Precision::Day, Confidence::Medium));
    }
}
