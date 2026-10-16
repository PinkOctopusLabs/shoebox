//! Import from a card or any folder: **copy** (never move) into a library
//! root, naming files with a template.
//!
//! Planning reads each source image's metadata and BLAKE3, skips files whose
//! exact content is already in the library (or earlier in the same import),
//! and works out every destination: the frame's files (`DSC_0001.NEF`,
//! `DSC_0001.JPG`) share one `-N` suffix on a collision, and sidecars on the
//! card come along under the new name. The copies then run as one batch
//! through [`crate::fileops::FileOps::import`], each verified by BLAKE3.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use shoebox_decode::Format;
use walkdir::WalkDir;

use crate::catalog::Catalog;
use crate::error::{Result, io};
use crate::fileops::{PlannedCopy, suffixed};
use crate::template::{Fields, Template};

/// What an import will do.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// Images first, then sidecars, each with its final destination.
    pub copies: Vec<PlannedCopy>,
    /// Source files already in the library: (source, the library file).
    pub skipped: Vec<(PathBuf, PathBuf)>,
    /// Source files that couldn't be read: (source, why).
    pub unreadable: Vec<(PathBuf, String)>,
    /// Imported images without a capture date (they go to `_undated/`).
    pub undated: usize,
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

    let total = images.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let read: Vec<std::result::Result<Source, (PathBuf, String)>> = images
        .par_iter()
        .map(|p| {
            let r = (|| {
                let format = Format::from_path(p).ok_or("not an image")?;
                let size = std::fs::metadata(p).map_err(|e| e.to_string())?.len();
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
                    size,
                    hash,
                })
            })();
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            progress(n, total);
            r.map_err(|e| (p.clone(), e))
        })
        .collect();

    let mut out = Plan::default();
    let mut sources = Vec::new();
    for r in read {
        match r {
            Ok(s) => sources.push(s),
            Err(e) => out.unreadable.push(e),
        }
    }

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
    for s in sources {
        if let Some(existing) = library_hashes.get(&s.hash).or_else(|| seen.get(&s.hash)) {
            out.skipped.push((s.path, existing.clone()));
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
        let wants: Vec<PathBuf> = frame
            .iter()
            .map(|s| {
                seq += 1;
                let f = Fields {
                    seq,
                    ..s.fields.clone()
                };
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
