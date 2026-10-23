//! Placing a downloaded asset in the library: its names (by the import
//! template, one frame per asset), its date, the sidecars for its copies
//! (favourite, albums, and Photos' date and place where the file lacks
//! them) and its sync record.
//!
//! The files are already in the cache (staged); nothing here writes. The
//! copies and writes go into the sync's batch through
//! [`crate::fileops::FileOps`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::{Offset, TimeZone};
use shoebox_decode::Format;

use super::ledger::{AlbumChoice, AlbumUse, Entry, File, Status};
use super::model::{Asset, ResourceKind};
use super::plan::{Favourite, Options};
use crate::catalog::Catalog;
use crate::error::{Result, io};
use crate::events::{self, Event};
use crate::fileops::{PlannedCopy, suffixed};
use crate::marks::{Marks, Pick};
use crate::meta::Meta;
use crate::sidecar::{self, SidecarDate};
use crate::template::{Fields, Template};
use crate::xmp::{self, Prop, Value};

/// A file of an asset, downloaded into the cache and read.
#[derive(Debug, Clone)]
pub struct Staged {
    pub kind: ResourceKind,
    pub path: PathBuf,
    pub hash: [u8; 32],
    pub size: u64,
    pub meta: Meta,
}

impl Staged {
    /// Hash and read a downloaded file (metadata it can't read is left
    /// empty: the file is still copied).
    pub fn read(kind: ResourceKind, path: &Path) -> Result<Self> {
        let mut h = blake3::Hasher::new();
        h.update_reader(std::fs::File::open(path).map_err(io(path))?)
            .map_err(io(path))?;
        let size = std::fs::metadata(path).map_err(io(path))?.len();
        let meta = Format::from_path(path)
            .and_then(|f| crate::meta::read(path, f).ok())
            .unwrap_or_default();
        Ok(Self {
            kind,
            path: path.to_path_buf(),
            hash: *h.finalize().as_bytes(),
            size,
            meta,
        })
    }
}

/// The library's files by size, hashed when a download has that size, so
/// a file the library has already isn't copied again; and the copies this
/// sync has made.
#[derive(Debug, Default)]
pub struct LibraryIndex {
    by_size: HashMap<u64, Vec<PathBuf>>,
    hashed: HashMap<PathBuf, Option<[u8; 32]>>,
    copied: HashMap<[u8; 32], PathBuf>,
}

impl LibraryIndex {
    pub fn new(library: &Catalog) -> Self {
        let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
        for p in library.photos.iter().filter(|p| !p.cloud) {
            by_size.entry(p.size).or_default().push(p.path.clone());
        }
        Self {
            by_size,
            ..Self::default()
        }
    }

    /// The library file (or this sync's copy) with these bytes.
    pub fn find(&mut self, size: u64, hash: &[u8; 32]) -> Option<PathBuf> {
        if let Some(p) = self.copied.get(hash) {
            return Some(p.clone());
        }
        for p in self.by_size.get(&size)? {
            let h = *self.hashed.entry(p.clone()).or_insert_with(|| {
                let mut h = blake3::Hasher::new();
                std::fs::File::open(p)
                    .and_then(|f| h.update_reader(f).map(|_| ()))
                    .ok()
                    .map(|()| *h.finalize().as_bytes())
            });
            if h.as_ref() == Some(hash) {
                return Some(p.clone());
            }
        }
        None
    }

    /// A copy this sync made.
    pub fn add(&mut self, hash: [u8; 32], path: PathBuf) {
        self.copied.insert(hash, path);
    }
}

/// What placing assets needs, kept across the assets of one sync.
pub struct Context<'a> {
    /// Where new assets go.
    pub root: &'a Path,
    pub template: &'a Template,
    pub options: &'a Options,
    /// What each album (by id) becomes, as chosen.
    pub albums: &'a HashMap<String, AlbumChoice>,
    /// For dates Photos gives without one (the Mac's).
    pub tz: TimeZone,
    pub index: LibraryIndex,
    /// Events, with any the albums make added.
    pub events: Vec<Event>,
    /// Destinations planned so far.
    pub taken: HashSet<PathBuf>,
    /// Files named so far (`{seq}`).
    pub seq: usize,
}

/// What to do with one asset.
#[derive(Debug, Clone, Default)]
pub struct Placement {
    /// The root it goes into (for an updated edit: where its original is).
    pub root: PathBuf,
    pub copies: Vec<PlannedCopy>,
    /// New sidecars beside the copies.
    pub sidecars: Vec<(PathBuf, Vec<u8>)>,
    /// Its sync record (for [`Placement::root`]).
    pub entry: Option<Entry>,
    /// Files the library had already: (kind, library file).
    pub in_library: Vec<(ResourceKind, PathBuf)>,
    /// No date in the file or in Photos (it goes to `_undated/`).
    pub undated: bool,
    /// Dated by Photos (the file has no date, or Photos' was changed).
    pub dated_by_photos: bool,
}

/// A capture time, and where it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Dated {
    date: Option<SidecarDate>,
    by_photos: bool,
}

fn parse_exif(time: &str) -> Option<DateTime> {
    DateTime::strptime("%Y:%m:%d %H:%M:%S", time.trim()).ok()
}

fn parse_offset(o: &str) -> Option<Offset> {
    let o = o.trim();
    let sign = match o.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (h, m) = o[1..].split_once(':')?;
    let secs = sign * (h.parse::<i32>().ok()? * 3600 + m.parse::<i32>().ok()? * 60);
    Offset::from_seconds(secs).ok()
}

/// When a file says it was taken, in EXIF's terms, and when Photos says:
/// the file's date stands unless it has none or Photos' was changed.
fn date_for(asset: &Asset, meta: Option<&Meta>, tz: &TimeZone) -> Dated {
    let exif = meta.and_then(|m| {
        let t = m.exif.capture_time.as_deref()?;
        Some((t, m.exif.subsec.as_deref(), m.exif.offset.as_deref()))
    });
    let from_exif = |(t, s, o): (&str, Option<&str>, Option<&str>)| Dated {
        date: Some(SidecarDate {
            time: t.to_owned(),
            subsec: s.map(str::to_owned),
            offset: o.map(str::to_owned),
        }),
        by_photos: false,
    };
    let Some(photos) = asset.created else {
        return exif.map(from_exif).unwrap_or_default();
    };
    let zone = |o: Option<&str>| match o.and_then(parse_offset) {
        Some(off) => TimeZone::fixed(off),
        None => tz.clone(),
    };
    if let Some(e @ (t, subsec, offset)) = exif
        && let Some(civil) = parse_exif(t)
    {
        let ms = subsec
            .and_then(|s| format!("0.{s}").parse::<f64>().ok())
            .unwrap_or(0.0);
        let agrees = civil.to_zoned(zone(offset)).ok().is_some_and(|z| {
            let d = (z.timestamp().as_millisecond() as f64 + ms * 1000.0
                - photos.as_millisecond() as f64)
                .abs()
                / 1000.0;
            // Without an offset, the camera's clock may have been in any
            // time zone: a whole number of quarter hours, up to 14 hours,
            // is the same moment.
            let q = d % 900.0;
            d <= 1.0 || (offset.is_none() && d <= 14.0 * 3600.0 && (q <= 1.0 || q >= 899.0))
        });
        if agrees {
            return from_exif(e);
        }
        return photos_date(photos, &zone(offset));
    }
    photos_date(photos, tz)
}

fn photos_date(at: Timestamp, tz: &TimeZone) -> Dated {
    let z = at.to_zoned(tz.clone());
    let ms = z.subsec_nanosecond() / 1_000_000;
    Dated {
        date: Some(SidecarDate {
            time: z.strftime("%Y:%m:%d %H:%M:%S").to_string(),
            subsec: (ms > 0).then(|| format!("{ms:03}")),
            offset: Some(z.strftime("%:z").to_string()),
        }),
        by_photos: true,
    }
}

/// An album choice's name as event or keyword levels.
pub fn album_parts(c: &AlbumChoice) -> Vec<String> {
    c.name
        .split('›')
        .map(|p| p.trim().replace('|', "-"))
        .filter(|p| !p.is_empty())
        .collect()
}

/// Mark `m` as a favourite, as chosen.
pub fn favourite(m: &mut Marks, how: Favourite) {
    match how {
        Favourite::Rating(n) => m.rating = n.min(5),
        Favourite::Keyword => {
            let k = "Photos|Favourite".to_owned();
            if !m.keywords.contains(&k) {
                m.keywords.push(k);
            }
        }
        Favourite::Pick => m.pick = Some(Pick::Pick),
        Favourite::Nothing => {}
    }
}

/// XMP's `GPSCoordinate`: `51,30.123456N`.
fn gps_text(deg: f64, pos: char, neg: char) -> String {
    let h = if deg < 0.0 { neg } else { pos };
    let a = deg.abs();
    format!("{},{:.6}{h}", a.trunc() as u32, a.fract() * 60.0)
}

fn stem_of(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => name,
    }
}

fn ext_of(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => e,
        _ => "",
    }
}

/// A record's path, if it stays inside its root (relative, no `..`): the
/// record is a file anyone could edit, and a copy goes beside it.
fn inside(rel: &str) -> Option<&Path> {
    let p = Path::new(rel);
    (p.components().next().is_some()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_))))
    .then_some(p)
}

/// `dir/20240603-141502-120.heic` → `dir/20240603-141502-120-photos.jpg`.
fn photos_variant(of: &Path, ext: &str) -> PathBuf {
    let stem = of
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    of.with_file_name(format!("{stem}-photos.{}", ext.to_ascii_lowercase()))
}

impl Context<'_> {
    fn free(&self, p: &Path) -> bool {
        !self.taken.contains(p) && !p.exists()
    }

    /// The marks and extra properties for a copy of `asset`.
    fn sidecar_for(
        &mut self,
        asset: &Asset,
        dated: &Dated,
        meta: &Meta,
    ) -> Result<Option<Vec<u8>>> {
        let mut m = Marks::default();
        if asset.favourite {
            favourite(&mut m, self.options.favourite);
        }
        if asset.hidden {
            m.keywords.push("Photos|Hidden".into());
        }
        let mut ids = Vec::new();
        for album in &asset.albums {
            let Some(c) = self.albums.get(album) else {
                continue;
            };
            let parts = album_parts(c);
            if parts.is_empty() {
                continue;
            }
            match c.kind {
                AlbumUse::Event => ids.extend(
                    super::sync::event_of(c, &self.events)
                        .or_else(|| events::ensure_path(&mut self.events, &parts)),
                ),
                AlbumUse::Keyword => {
                    let k = format!("Albums|{}", parts.join("|"));
                    if !m.keywords.contains(&k) {
                        m.keywords.push(k);
                    }
                }
                AlbumUse::Nothing => {}
            }
        }
        let m = events::join(&m, &ids, &self.events);
        let mut changes = m.changes_from(&Marks::default());
        if dated.by_photos
            && let Some(d) = &dated.date
        {
            let v = d.to_xmp();
            changes.push((Prop::DateTimeOriginal, Some(Value::Text(v.clone()))));
            changes.push((Prop::DateCreated, Some(Value::Text(v))));
        }
        if let Some((lat, lon)) = asset.location
            && meta.exif.gps.is_none()
        {
            changes.push((
                Prop::GpsLatitude,
                Some(Value::Text(gps_text(lat, 'N', 'S'))),
            ));
            changes.push((
                Prop::GpsLongitude,
                Some(Value::Text(gps_text(lon, 'E', 'W'))),
            ));
        }
        if changes.is_empty() {
            return Ok(None);
        }
        xmp::edit(None, &changes).map(Some)
    }

    fn fields(
        &mut self,
        asset: &Asset,
        dated: &Dated,
        meta: Option<&Meta>,
        file_name: String,
    ) -> Fields {
        self.seq += 1;
        Fields {
            capture_time: dated.date.as_ref().map(|d| d.time.clone()),
            subsec: dated.date.as_ref().and_then(|d| d.subsec.clone()),
            file_name,
            camera: meta.and_then(|m| m.exif.model.clone()),
            width: meta
                .and_then(|m| m.width)
                .or(Some(asset.width))
                .filter(|w| *w > 0),
            height: meta
                .and_then(|m| m.height)
                .or(Some(asset.height))
                .filter(|h| *h > 0),
            seq: self.seq,
        }
    }
}

/// Plan the copies, sidecars and sync record for `asset`, whose wanted
/// files are `staged`. `previous`: its record from an earlier sync, and
/// that record's root (an updated edit goes beside its original there).
pub fn place(
    cx: &mut Context<'_>,
    asset: &Asset,
    previous: Option<(&Path, &Entry)>,
    staged: &[Staged],
) -> Result<Placement> {
    let root = previous.map_or(cx.root, |(r, _)| r).to_path_buf();
    let mut out = Placement {
        root: root.clone(),
        ..Placement::default()
    };
    let original_name = asset
        .resource(ResourceKind::Original)
        .map(|r| r.file_name.clone())
        .unwrap_or_else(|| "unnamed".into());
    let lead = staged.iter().find(|s| s.kind == ResourceKind::Original);
    let dated = date_for(asset, lead.map(|s| &s.meta), &cx.tz);
    out.undated = dated.date.is_none();
    out.dated_by_photos = dated.by_photos;

    // What's new: not in the library, not recorded for this asset already.
    let known: HashSet<String> = previous
        .map(|(_, e)| e.files.iter().map(|f| f.blake3.clone()).collect())
        .unwrap_or_default();
    let mut fresh: Vec<&Staged> = Vec::new();
    for s in staged {
        let hex = blake3::Hash::from(s.hash).to_hex().to_string();
        if known.contains(&hex) {
            continue;
        }
        match cx.index.find(s.size, &s.hash) {
            Some(p) => out.in_library.push((s.kind, p)),
            None => fresh.push(s),
        }
    }
    let frame: Vec<&Staged> = fresh
        .iter()
        .copied()
        .filter(|s| s.kind.is_original())
        .collect();
    let edit = fresh
        .iter()
        .copied()
        .find(|s| s.kind == ResourceKind::Edited);

    // The frame's names share one stem (a Live Photo's movie and a RAW+JPEG
    // pair are named from the photo), and one suffix.
    let mut wants: Vec<PathBuf> = Vec::new();
    for s in &frame {
        let name = match s.kind {
            ResourceKind::Original => original_name.clone(),
            _ => {
                let theirs = asset
                    .resource(s.kind)
                    .map(|r| r.file_name.as_str())
                    .unwrap_or("");
                format!("{}.{}", stem_of(&original_name), ext_of(theirs))
            }
        };
        let f = cx.fields(asset, &dated, lead.map(|l| &l.meta), name);
        wants.push(root.join(cx.template.render(&f)));
    }
    let edit_ext = edit
        .and_then(|_| asset.resource(ResourceKind::Edited))
        .map(|r| ext_of(&r.file_name).to_owned())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "jpg".into());
    // An edit goes beside its original: this sync's copy, the copy an
    // earlier sync made (if it's still there), or where the template puts it.
    let beside: PathBuf = match (
        frame.iter().position(|s| s.kind == ResourceKind::Original),
        previous.and_then(|(r, e)| {
            e.file(ResourceKind::Original)
                .and_then(|f| inside(&f.rel))
                .map(|rel| r.join(rel))
        }),
    ) {
        (Some(i), _) => wants[i].clone(),
        (None, Some(p)) if p.exists() => p,
        _ => {
            let f = cx.fields(asset, &dated, lead.map(|l| &l.meta), original_name.clone());
            root.join(cx.template.render(&f))
        }
    };
    let lead_at = frame.iter().position(|s| s.kind == ResourceKind::Original);
    let mut n = 0;
    let (dsts, edit_dst) = loop {
        let cand: Vec<PathBuf> = wants
            .iter()
            .map(|w| if n == 0 { w.clone() } else { suffixed(w, n) })
            .collect();
        // Beside this sync's original: its stem and suffix. Beside an
        // earlier one: `-photos`, then `-photos-1`, `-photos-2`.
        let e = edit.map(|_| match lead_at {
            Some(i) => photos_variant(&cand[i], &edit_ext),
            None if n == 0 => photos_variant(&beside, &edit_ext),
            None => suffixed(&photos_variant(&beside, &edit_ext), n),
        });
        let all: Vec<&PathBuf> = cand.iter().chain(e.as_ref()).collect();
        let unique = all.iter().collect::<HashSet<_>>().len() == all.len();
        if unique && all.iter().all(|p| cx.free(p)) {
            break (cand, e);
        }
        n += 1;
    };

    let mut files: Vec<File> = previous.map(|(_, e)| e.files.clone()).unwrap_or_default();
    let copies: Vec<(&Staged, PathBuf)> = frame
        .iter()
        .copied()
        .zip(dsts)
        .chain(edit.zip(edit_dst))
        .collect();
    for (s, dst) in copies {
        cx.taken.insert(dst.clone());
        cx.index.add(s.hash, dst.clone());
        if let Some(bytes) = cx.sidecar_for(asset, &dated, &s.meta)? {
            let car = sidecar::per_file(&dst);
            cx.taken.insert(car.clone());
            out.sidecars.push((car, bytes));
        }
        files.push(File {
            kind: s.kind,
            rel: dst
                .strip_prefix(&root)
                .unwrap_or(&dst)
                .to_string_lossy()
                .into_owned(),
            blake3: blake3::Hash::from(s.hash).to_hex().to_string(),
            size: s.size,
        });
        out.copies.push(PlannedCopy {
            src: s.path.clone(),
            dst,
            hash: s.hash,
        });
    }
    for (kind, p) in &out.in_library {
        if let Some(s) = staged.iter().find(|s| s.kind == *kind) {
            files.push(File {
                kind: *kind,
                rel: p.to_string_lossy().into_owned(),
                blake3: blake3::Hash::from(s.hash).to_hex().to_string(),
                size: s.size,
            });
        }
    }
    let copied_any =
        !out.copies.is_empty() || previous.is_some_and(|(_, e)| e.status == Status::Copied);
    out.entry = Some(Entry {
        local_id: asset.id.local.clone(),
        status: if copied_any {
            Status::Copied
        } else {
            Status::InLibrary
        },
        files,
        albums: asset.albums.clone(),
        favourite: asset.favourite,
        edit_modified: if staged.iter().any(|s| s.kind == ResourceKind::Edited) {
            asset.modified.map(|m| m.to_string())
        } else {
            previous.and_then(|(_, e)| e.edit_modified.clone())
        },
        updated: events::now_ms(),
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn asset(created: Option<&str>) -> Asset {
        Asset {
            id: super::super::model::AssetId {
                cloud: Some("C".into()),
                local: "L".into(),
            },
            media: super::super::model::Media::Photo,
            created: created.map(ts),
            modified: None,
            location: None,
            favourite: false,
            hidden: false,
            burst: None,
            live: false,
            width: 4032,
            height: 3024,
            resources: vec![],
            albums: vec![],
            edited: None,
        }
    }

    fn meta(t: &str, sub: Option<&str>, off: Option<&str>) -> Meta {
        let mut m = Meta::default();
        m.exif.capture_time = Some(t.into());
        m.exif.subsec = sub.map(Into::into);
        m.exif.offset = off.map(Into::into);
        m
    }

    #[test]
    fn the_files_date_stands_when_photos_agrees() {
        let tz = TimeZone::fixed(Offset::from_seconds(0).unwrap());
        let a = asset(Some("2024-06-03T13:15:02.120Z"));
        let d = date_for(
            &a,
            Some(&meta("2024:06:03 14:15:02", Some("120"), Some("+01:00"))),
            &tz,
        );
        assert!(!d.by_photos);
        assert_eq!(d.date.unwrap().time, "2024:06:03 14:15:02");
        // No offset: the camera's clock in another time zone still agrees.
        let d = date_for(
            &a,
            Some(&meta("2024:06:03 22:15:02", Some("12"), None)),
            &tz,
        );
        assert!(!d.by_photos);
    }

    #[test]
    fn photos_wins_when_the_date_was_changed_there_or_the_file_has_none() {
        let tz = TimeZone::fixed(Offset::from_seconds(3600).unwrap());
        let a = asset(Some("2019-01-01T10:00:00.500Z"));
        let d = date_for(
            &a,
            Some(&meta("2024:06:03 14:15:02", None, Some("+02:00"))),
            &tz,
        );
        assert!(d.by_photos);
        let date = d.date.unwrap();
        assert_eq!(date.time, "2019:01:01 12:00:00");
        assert_eq!(date.offset.as_deref(), Some("+02:00"));
        assert_eq!(date.subsec.as_deref(), Some("500"));
        // A screenshot: no EXIF date, so the Mac's time zone.
        let d = date_for(&a, Some(&Meta::default()), &tz);
        assert!(d.by_photos);
        assert_eq!(d.date.as_ref().unwrap().time, "2019:01:01 11:00:00");
        assert_eq!(d.date.unwrap().to_xmp(), "2019-01-01T11:00:00.500+01:00");
        assert_eq!(date_for(&asset(None), None, &tz), Dated::default());
    }

    #[test]
    fn record_paths_stay_inside_the_root() {
        assert!(inside("2024/a.jpg").is_some());
        for bad in ["../a.jpg", "/etc/a.jpg", "2024/../../a.jpg", ""] {
            assert!(inside(bad).is_none(), "{bad}");
        }
        assert!(SidecarDate::parse("2024-06-03T14:15:0é.1").is_none());
        assert!(SidecarDate::parse("ééééééééééééééééééééé").is_none());
    }

    #[test]
    fn gps_in_xmp_form_reads_back() {
        let t = gps_text(-33.865143, 'N', 'S');
        assert!(t.ends_with('S'));
        let mut props = HashMap::new();
        props.insert(Prop::GpsLatitude, Value::Text(t));
        props.insert(
            Prop::GpsLongitude,
            Value::Text(gps_text(151.2099, 'E', 'W')),
        );
        let (lat, lon) = Marks::from_props(&props).gps().unwrap();
        assert!((lat + 33.865143).abs() < 1e-6 && (lon - 151.2099).abs() < 1e-6);
    }
}
