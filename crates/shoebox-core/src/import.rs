//! Import from a card or any folder: **copy** (never move) into a library
//! root, naming files with a template.
//!
//! Planning reads each source image's metadata and BLAKE3, skips files whose
//! exact content is already in the library or earlier in the same import,
//! and works out every destination: the frame's files (`DSC_0001.NEF`,
//! `DSC_0001.JPG`) share one `-N` suffix on a collision, and sidecars on the
//! card come along under the new name. The copies then run as one batch
//! through [`crate::fileops::FileOps::import`], each verified by BLAKE3.
//!
//! The source's folder names can come along: each folder holding photos
//! (at any depth) is listed in the plan with what its name becomes on the
//! photos in it: an event (nested as the folders are), a keyword, or
//! nothing. Names that say nothing (dates, camera folders such as
//! `100NIKON`, `DCIM`) start as nothing. [`folder_writes`] turns the
//! choices into sidecars for the copies (and edits for library photos found
//! again in those folders), written in the import's batch.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use shoebox_decode::Format;
use walkdir::WalkDir;

use crate::catalog::Catalog;
use crate::error::{Result, io};
use crate::events::{self, Event};
use crate::fileops::{PlannedCopy, suffixed};
use crate::marks::Marks;
use crate::sidecar;
use crate::template::{Fields, Template};
use crate::xmp;

/// What a source folder's name becomes on the photos imported from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderUse {
    /// An event (inside the events of the folders around it).
    Event,
    /// A keyword.
    Keyword,
    /// Nothing.
    Ignore,
}

/// A folder under the import source with photos in it (at any depth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// Relative to the source.
    pub rel: PathBuf,
    /// What it's called on the photos (the folder's name to start with).
    pub name: String,
    pub kind: FolderUse,
    /// Photos under it, at any depth (copied or already in the library).
    pub photos: usize,
}

/// What an import will do.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Images first, then sidecars, each with its final destination.
    pub copies: Vec<PlannedCopy>,
    /// Source files already in the library: (source, the library file).
    pub skipped: Vec<(PathBuf, PathBuf)>,
    /// Source files with the same content as another source file that's
    /// copied: (source, the source file copied instead).
    pub repeats: Vec<(PathBuf, PathBuf)>,
    /// Source files that couldn't be read: (source, why).
    pub unreadable: Vec<(PathBuf, String)>,
    /// Imported images without a capture date (they go to `_undated/`).
    pub undated: usize,
    /// Source images that were in the cloud only (planning reads every
    /// file, which downloaded them).
    pub downloaded: usize,
    /// Each photo to copy, in the order of `copies` (for previews).
    pub photos: Vec<PlannedPhoto>,
    /// Folders under the source with photos in them, parents first.
    pub folders: Vec<Folder>,
    /// Each photo to copy (by its new path), and each library photo the
    /// source has again, with the source folders it was found in.
    pub origins: Vec<(PathBuf, BTreeSet<PathBuf>)>,
}

/// A photo an import will copy: enough to preview it without reading the
/// source again.
#[derive(Debug, Clone)]
pub struct PlannedPhoto {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub format: Format,
    pub size: u64,
    pub mtime_ns: i64,
    pub orientation: u16,
}

/// Month names (and their short forms) that make a folder name a date.
const MONTHS: [&str; 24] = [
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
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
];

/// Folder names that say nothing about the photos in them.
const GENERIC: [&str; 22] = [
    "dcim",
    "camera roll",
    "camera uploads",
    "photos",
    "pictures",
    "images",
    "photo library",
    "photos library",
    "icloud photos",
    "saved pictures",
    "saved photos",
    "import",
    "imports",
    "imported",
    "export",
    "exports",
    "misc",
    "new folder",
    "untitled folder",
    "jpg",
    "raw",
    "originals",
];

/// A date (`2019`, `2019-07-14`, `July 2019`, `14th Jul`), a camera's
/// folder (`100NIKON`, `101APPLE`, `100NCZ_F`) or a generic name such as
/// `DCIM` or `Photos`: the folder's name adds nothing.
pub fn says_nothing(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    if lower.is_empty() || GENERIC.contains(&lower.as_str()) {
        return true;
    }
    let camera = lower.len() == 8
        && lower[..3].bytes().all(|b| b.is_ascii_digit())
        && lower[3..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if camera {
        return true;
    }
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .all(|w| {
            let digits = w.trim_end_matches(['s', 't', 'n', 'd', 'r', 'h']);
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                || MONTHS.contains(&w)
        })
}

/// What a folder's name starts as: an event, unless it says nothing.
fn default_use(name: &str) -> FolderUse {
    if says_nothing(name) {
        FolderUse::Ignore
    } else {
        FolderUse::Event
    }
}

/// The folder holding `p`, relative to `source` (empty at the top).
fn rel_dir(source: &Path, p: &Path) -> PathBuf {
    p.parent()
        .and_then(|d| d.strip_prefix(source).ok())
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn hash_file(p: &Path) -> std::io::Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(p)?)?;
    Ok(*h.finalize().as_bytes())
}

fn is_xmp(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

fn lower_stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

struct Source {
    path: PathBuf,
    fields: Fields,
    size: u64,
    mtime_ns: i64,
    format: Format,
    orientation: u16,
    hash: [u8; 32],
}

/// Plan importing everything under `source` into `root` with `template`.
/// `library` is the current catalog (for skipping what's already there).
/// `progress(done, total)` is called as source files are read.
pub fn plan(
    source: &Path,
    root: &Path,
    template: &Template,
    library: &Catalog,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Plan> {
    let mut images = Vec::new();
    let mut xmps = Vec::new();
    let walker = WalkDir::new(source).follow_links(false).into_iter();
    for e in
        walker.filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
    {
        let e = match e {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("import: skipping unreadable entry: {err}");
                continue;
            }
        };
        if !e.file_type().is_file() {
            continue;
        }
        let p = e.into_path();
        if is_xmp(&p) {
            xmps.push(p);
        } else if Format::from_path(&p).is_some() {
            images.push(p);
        }
    }
    images.sort();
    let downloaded = images
        .iter()
        .filter(|p| crate::cloud::path_is_cloud_only(p))
        .count();

    let total = images.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let read: Vec<std::result::Result<Source, (PathBuf, String)>> = images
        .par_iter()
        .map(|p| {
            let r = (|| {
                let format = Format::from_path(p).ok_or("not an image")?;
                let md = std::fs::metadata(p).map_err(|e| e.to_string())?;
                let meta = crate::meta::read(p, format).map_err(|e| e.to_string())?;
                let hash = hash_file(p).map_err(|e| e.to_string())?;
                Ok::<_, String>(Source {
                    path: p.clone(),
                    fields: Fields {
                        capture_time: meta.exif.capture_time.clone(),
                        subsec: meta.exif.subsec.clone(),
                        file_name: p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        camera: meta.exif.model.clone(),
                        width: meta.width,
                        height: meta.height,
                        seq: 0,
                    },
                    size: md.len(),
                    mtime_ns: crate::scan::mtime_of(&md),
                    format,
                    orientation: meta.orientation(),
                    hash,
                })
            })();
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            progress(n, total);
            r.map_err(|e| (p.clone(), e))
        })
        .collect();

    let mut out = Plan {
        downloaded,
        ..Plan::default()
    };
    let mut sources = Vec::new();
    for r in read {
        match r {
            Ok(s) => sources.push(s),
            Err(e) => out.unreadable.push(e),
        }
    }
    // The folders photos are in, with how many under each, and the folders
    // each picture (by content) turns up in.
    let mut counts: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut found_in: HashMap<[u8; 32], BTreeSet<PathBuf>> = HashMap::new();
    for s in &sources {
        let rel = rel_dir(source, &s.path);
        for a in rel.ancestors().filter(|a| !a.as_os_str().is_empty()) {
            *counts.entry(a.to_path_buf()).or_default() += 1;
        }
        if !rel.as_os_str().is_empty() {
            found_in.entry(s.hash).or_default().insert(rel);
        }
    }
    out.folders = counts
        .into_iter()
        .map(|(rel, photos)| {
            let name = rel
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Folder {
                kind: default_use(&name),
                name,
                rel,
                photos,
            }
        })
        .collect();

    // Already in the library? Only same-size files can match, so only
    // those are hashed.
    let mut by_size: HashMap<u64, Vec<&Path>> = HashMap::new();
    for p in &library.photos {
        by_size.entry(p.size).or_default().push(&p.path);
    }
    let wanted: HashSet<u64> = sources.iter().map(|s| s.size).collect();
    let candidates: Vec<&Path> = by_size
        .iter()
        .filter(|(s, _)| wanted.contains(s))
        .flat_map(|(_, v)| v.iter().copied())
        .collect();
    let library_hashes: HashMap<[u8; 32], PathBuf> = candidates
        .par_iter()
        .filter_map(|p| hash_file(p).ok().map(|h| (h, p.to_path_buf())))
        .collect();
    let mut seen: HashMap<[u8; 32], PathBuf> = HashMap::new();
    let mut keep = Vec::new();
    let mut found_again: Vec<(PathBuf, [u8; 32])> = Vec::new();
    for s in sources {
        if let Some(existing) = library_hashes.get(&s.hash) {
            if !found_again.iter().any(|(p, _)| p == existing) {
                found_again.push((existing.clone(), s.hash));
            }
            out.skipped.push((s.path, existing.clone()));
        } else if let Some(existing) = seen.get(&s.hash) {
            out.repeats.push((s.path, existing.clone()));
        } else {
            seen.insert(s.hash, s.path.clone());
            keep.push(s);
        }
    }

    // Frames on the card: same folder and base name.
    let mut frames: Vec<Vec<Source>> = Vec::new();
    let mut frame_of: HashMap<(PathBuf, String), usize> = HashMap::new();
    for s in keep {
        let key = (
            s.path.parent().map(Path::to_path_buf).unwrap_or_default(),
            lower_stem(&s.path),
        );
        match frame_of.get(&key) {
            Some(&f) => frames[f].push(s),
            None => {
                frame_of.insert(key, frames.len());
                frames.push(vec![s]);
            }
        }
    }

    let mut taken: HashSet<PathBuf> = HashSet::new();
    let mut seq = 0usize;
    let mut sidecar_copies = Vec::new();
    let card_sidecars: HashMap<String, PathBuf> = xmps
        .iter()
        .map(|p| (p.to_string_lossy().to_lowercase(), p.clone()))
        .collect();
    let find_sidecar = |p: PathBuf| {
        card_sidecars
            .get(&p.to_string_lossy().to_lowercase())
            .cloned()
    };
    for frame in &frames {
        // A Live Photo's video is named from its photo's capture time, so
        // the pair keeps one name (the video's own clock can differ).
        let is_movie = |s: &Source| Format::from_path(&s.path).is_some_and(Format::is_movie);
        let lead = frame
            .iter()
            .find(|s| !is_movie(s) && s.fields.capture_time.is_some());
        let wants: Vec<PathBuf> = frame
            .iter()
            .map(|s| {
                seq += 1;
                let mut f = Fields {
                    seq,
                    ..s.fields.clone()
                };
                if let Some(lead) = lead.filter(|_| is_movie(s)) {
                    f.capture_time = lead.fields.capture_time.clone();
                    f.subsec = lead.fields.subsec.clone();
                    f.camera = lead.fields.camera.clone();
                }
                root.join(template.render(&f))
            })
            .collect();
        out.undated += frame
            .iter()
            .filter(|s| s.fields.capture_time.is_none())
            .count();
        // One suffix for the whole frame.
        let mut n = 0;
        let dsts = loop {
            let cand: Vec<PathBuf> = wants
                .iter()
                .map(|w| if n == 0 { w.clone() } else { suffixed(w, n) })
                .collect();
            let unique = cand.iter().collect::<HashSet<_>>().len() == cand.len();
            if unique && cand.iter().all(|c| !c.exists() && !taken.contains(c)) {
                break cand;
            }
            n += 1;
        };
        for (s, dst) in frame.iter().zip(&dsts) {
            taken.insert(dst.clone());
            out.copies.push(PlannedCopy {
                src: s.path.clone(),
                dst: dst.clone(),
                hash: s.hash,
            });
            out.photos.push(PlannedPhoto {
                src: s.path.clone(),
                dst: dst.clone(),
                format: s.format,
                size: s.size,
                mtime_ns: s.mtime_ns,
                orientation: s.orientation,
            });
            // `DSC_0001.NEF.xmp` → `<new name>.nef.xmp`.
            let mut name = s.path.file_name().unwrap_or_default().to_os_string();
            name.push(".xmp");
            if let Some(car) = find_sidecar(s.path.with_file_name(name)) {
                let mut dname = dst.file_name().unwrap_or_default().to_os_string();
                dname.push(".xmp");
                sidecar_copies.push((car, dst.with_file_name(dname)));
            }
        }
        // `DSC_0001.xmp` (shared by the frame) → `<new stem>.xmp`, when the
        // frame's files all get one new stem.
        let stems: HashSet<String> = dsts.iter().map(|d| lower_stem(d)).collect();
        if let (Some(first), 1) = (frame.first(), stems.len())
            && let Some(car) = find_sidecar(first.path.with_extension("xmp"))
        {
            sidecar_copies.push((car, dsts[0].with_extension("xmp")));
        }
    }
    for c in &out.copies {
        if let Some(rels) = found_in.get(&c.hash) {
            out.origins.push((c.dst.clone(), rels.clone()));
        }
    }
    for (existing, hash) in found_again {
        if let Some(rels) = found_in.get(&hash) {
            out.origins.push((existing, rels.clone()));
        }
    }
    for (car, dst) in sidecar_copies {
        if dst.exists() || taken.contains(&dst) {
            tracing::warn!(
                "import: {} would land on an existing sidecar; left out",
                car.display()
            );
            continue;
        }
        let hash = hash_file(&car).map_err(io(&car))?;
        taken.insert(dst.clone());
        out.copies.push(PlannedCopy {
            src: car,
            dst,
            hash,
        });
    }
    Ok(out)
}

/// What [`folder_writes`] will write.
#[derive(Debug, Clone, Default)]
pub struct FolderWrites {
    /// Sidecars to write (new ones for copies, edits for library photos).
    pub writes: Vec<(PathBuf, Vec<u8>)>,
    /// Photos given a folder's name.
    pub photos: usize,
    /// Events made for folders (added to `events`).
    pub new_events: usize,
}

/// The event path (names, outermost first) and keywords the folders put on
/// a photo found in `rel`.
fn names_for(folders: &HashMap<&Path, &Folder>, rel: &Path) -> (Vec<String>, Vec<String>) {
    let mut chain: Vec<&Path> = rel
        .ancestors()
        .filter(|a| !a.as_os_str().is_empty())
        .collect();
    chain.reverse();
    let (mut path, mut keywords) = (Vec::new(), Vec::new());
    for a in chain {
        let Some(f) = folders.get(a) else { continue };
        // `|` separates the levels of a keyword.
        let name = f.name.trim().replace('|', "-");
        if name.is_empty() {
            continue;
        }
        match f.kind {
            FolderUse::Event => path.push(name),
            FolderUse::Keyword => keywords.push(name),
            FolderUse::Ignore => {}
        }
    }
    (path, keywords)
}

/// The event path (names, outermost first) and keywords the folders, as
/// chosen, put on a photo in `rel`.
pub fn names(folders: &[Folder], rel: &Path) -> (Vec<String>, Vec<String>) {
    let by_rel: HashMap<&Path, &Folder> = folders.iter().map(|f| (f.rel.as_path(), f)).collect();
    names_for(&by_rel, rel)
}

/// Put the folders' names (as chosen in `folders`) on the photos of
/// `plan`: new events go into `events`, and the sidecar writes are
/// returned. A copy's sidecar is made from the source's own (the plain
/// copy of it is dropped from `plan.copies`); a library photo found again
/// in a folder gets an edit to its sidecar, unless it's in a reference
/// root. `stem_for_raw`: also write `name.xmp` for a library NEF.
pub fn folder_writes(
    plan: &mut Plan,
    folders: &[Folder],
    events: &mut Vec<Event>,
    library: &Catalog,
    stem_for_raw: bool,
) -> Result<FolderWrites> {
    let mut out = FolderWrites::default();
    let by_rel: HashMap<&Path, &Folder> = folders.iter().map(|f| (f.rel.as_path(), f)).collect();
    let copied: HashMap<PathBuf, PathBuf> = plan
        .copies
        .iter()
        .filter(|c| !is_xmp(&c.src))
        .map(|c| (c.dst.clone(), c.src.clone()))
        .collect();
    let in_library: HashMap<&Path, usize> = library
        .photos
        .iter()
        .enumerate()
        .map(|(i, p)| (p.path.as_path(), i))
        .collect();
    let before = events.len();
    let origins = plan.origins.clone();
    for (target, rels) in &origins {
        let mut paths: BTreeSet<Vec<String>> = BTreeSet::new();
        let mut keywords: BTreeSet<String> = BTreeSet::new();
        for rel in rels {
            let (path, k) = names_for(&by_rel, rel);
            if !path.is_empty() {
                paths.insert(path);
            }
            keywords.extend(k);
        }
        if paths.is_empty() && keywords.is_empty() {
            continue;
        }
        let ids: Vec<String> = paths
            .iter()
            .filter_map(|p| events::ensure_path(events, p))
            .collect();
        let with = |old: &Marks| {
            let mut m = events::join(old, &ids, events);
            for k in &keywords {
                if !m.keywords.contains(k) {
                    m.keywords.push(k.clone());
                }
            }
            m
        };
        if let Some(src) = copied.get(target) {
            let Some(format) = Format::from_path(src) else {
                continue;
            };
            let (stem, file) = sidecar::present(src, &|p| p.exists());
            let old = sidecar::load(Some(src), format, stem.as_deref(), file.as_deref())?.marks;
            let new = with(&old);
            let changes = new.changes_from(&old);
            if changes.is_empty() {
                continue;
            }
            // The copy's own sidecar, made from the source's (if it has one).
            let path = sidecar::per_file(target);
            let base = match plan.copies.iter().position(|c| c.dst == path) {
                Some(at) => {
                    let c = plan.copies.remove(at);
                    Some(std::fs::read(&c.src).map_err(io(&c.src))?)
                }
                None => None,
            };
            out.writes
                .push((path, xmp::edit(base.as_deref(), &changes)?));
            out.photos += 1;
        } else if let Some(&i) = in_library.get(target.as_path()) {
            if library.is_reference(i) {
                continue;
            }
            let p = &library.photos[i];
            let new = with(&p.marks);
            let also_stem = stem_for_raw && p.format == Format::Nef;
            let w = sidecar::plan_write(&p.path, &p.marks, &new, also_stem)?;
            if !w.is_empty() {
                out.writes.extend(w);
                out.photos += 1;
            }
        }
    }
    out.new_events = events.len() - before;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_say_nothing() {
        for n in [
            "2019",
            "2019-07-14",
            "20190714",
            "July 2019",
            "14th Jul",
            "2019_07",
            "100NIKON",
            "101APPLE",
            "100NCZ_F",
            "DCIM",
            "Photos",
            "iCloud Photos",
            "",
        ] {
            assert!(says_nothing(n), "{n}");
        }
        for n in [
            "Italy 2019",
            "2025-photoshoot",
            "Rome",
            "Wedding",
            "Mayfair",
        ] {
            assert!(!says_nothing(n), "{n}");
        }
    }
}
