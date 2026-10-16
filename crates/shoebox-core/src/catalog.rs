//! The browsable library: every image under the library roots with its
//! metadata, loaded from the cache instantly and refreshed from disk.

use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;
use shoebox_decode::Format;

use crate::cache::{Cache, CachedMarks, MetaEntry};
use crate::error::Result;
use crate::marks::Marks;
use crate::meta::{self, Meta};
use crate::scan::{self, Role, Root, Sidecars};
use crate::sidecar;

#[derive(Debug, Clone)]
pub struct Photo {
    /// Index into [`Catalog::roots`].
    pub root: usize,
    pub rel: String,
    pub path: PathBuf,
    pub format: Format,
    pub size: u64,
    pub mtime_ns: i64,
    pub meta: Meta,
    /// Effective marks (sidecar > embedded XMP > EXIF).
    pub marks: Marks,
    /// The per-file sidecar (`name.ext.xmp`), if it exists.
    pub sidecar: Option<PathBuf>,
    /// Signature of the sidecars the marks were read from.
    pub sidecar_sig: String,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub roots: Vec<Root>,
    /// Sorted by capture time (undated last), then path.
    pub photos: Vec<Photo>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RefreshStats {
    pub files: usize,
    pub cached: usize,
    pub read: usize,
    pub failed: usize,
    pub forgotten: usize,
}

const STORE_CHUNK: usize = 512;

impl Catalog {
    /// Everything the cache knows about these roots. Doesn't touch the
    /// library, so a large library opens instantly; follow with `refresh`.
    pub fn from_cache(cache: &Cache, roots: Vec<Root>) -> Result<Self> {
        let mut photos = Vec::new();
        for (i, root) in roots.iter().enumerate() {
            let id = cache.root_id(&root.path, root.role == Role::Reference)?;
            for (rel, (size, mtime_ns, meta, marks)) in cache.load_meta(id)? {
                let marks = marks.unwrap_or_else(|| CachedMarks {
                    marks: Marks::default(),
                    sidecar: None,
                    sig: String::new(),
                });
                let path = root.path.join(&rel);
                let Some(format) = Format::from_path(&path) else {
                    continue;
                };
                photos.push(Photo {
                    root: i,
                    rel,
                    path,
                    format,
                    size,
                    mtime_ns,
                    meta,
                    marks: marks.marks,
                    sidecar: marks.sidecar,
                    sidecar_sig: marks.sig,
                });
            }
        }
        let mut c = Self { roots, photos };
        c.sort();
        Ok(c)
    }

    /// Scan the roots; reuse cached metadata for unchanged files and read the
    /// rest in parallel. `progress(done, total)` is called as files are read.
    pub fn refresh(
        cache: &mut Cache,
        roots: Vec<Root>,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> Result<(Self, RefreshStats)> {
        let (files, sidecars) = scan::scan_with_sidecars(&roots, &[])?;
        let mut stats = RefreshStats {
            files: files.len(),
            ..Default::default()
        };
        let ids = roots
            .iter()
            .map(|r| cache.root_id(&r.path, r.role == Role::Reference))
            .collect::<Result<Vec<_>>>()?;
        let known: Vec<_> = ids
            .iter()
            .map(|&id| cache.load_meta(id))
            .collect::<Result<_>>()?;

        let mut photos = Vec::with_capacity(files.len());
        // (file, metadata if it's still valid) for files needing a read.
        let mut todo: Vec<(scan::ScannedFile, Option<Meta>)> = Vec::new();
        for f in files {
            let sig = signature(&f.path, &sidecars);
            match known[f.root].get(&f.rel) {
                Some((size, mtime, m, cm)) if *size == f.size && *mtime == f.mtime_ns => match cm {
                    Some(cm) if cm.sig == sig => {
                        stats.cached += 1;
                        photos.push(Photo {
                            root: f.root,
                            rel: f.rel,
                            path: f.path,
                            format: f.format,
                            size: f.size,
                            mtime_ns: f.mtime_ns,
                            meta: m.clone(),
                            marks: cm.marks.clone(),
                            sidecar: cm.sidecar.clone(),
                            sidecar_sig: sig,
                        });
                    }
                    // Image unchanged, sidecar changed: re-read marks only.
                    _ => todo.push((f, Some(m.clone()))),
                },
                _ => todo.push((f, None)),
            }
        }

        let total = todo.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        for chunk in todo.chunks(STORE_CHUNK) {
            let read: Vec<(Photo, bool)> = chunk
                .par_iter()
                .map(|(f, known_meta)| {
                    let (m, ok) = match known_meta {
                        Some(m) => (m.clone(), true),
                        None => match meta::read(&f.path, f.format) {
                            Ok(m) => (m, true),
                            Err(e) => {
                                tracing::warn!("metadata: {e}");
                                (Meta::default(), false)
                            }
                        },
                    };
                    let (stem, file) = sidecar::present(&f.path, &|p| sidecars.contains_key(p));
                    let loaded = sidecar::load(&f.path, f.format, stem.as_deref(), file.as_deref())
                        .unwrap_or_else(|e| {
                            tracing::warn!("marks: {e}");
                            sidecar::Loaded::default()
                        });
                    let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    progress(n, total);
                    let p = Photo {
                        root: f.root,
                        rel: f.rel.clone(),
                        path: f.path.clone(),
                        format: f.format,
                        size: f.size,
                        mtime_ns: f.mtime_ns,
                        meta: m,
                        marks: loaded.marks,
                        sidecar: loaded.sidecar,
                        sidecar_sig: signature(&f.path, &sidecars),
                    };
                    (p, ok)
                })
                .collect();
            let exts: Vec<String> = read
                .iter()
                .map(|(p, _)| p.format.label().to_ascii_lowercase())
                .collect();
            let cached: Vec<CachedMarks> = read
                .iter()
                .map(|(p, _)| CachedMarks {
                    marks: p.marks.clone(),
                    sidecar: p.sidecar.clone(),
                    sig: p.sidecar_sig.clone(),
                })
                .collect();
            let entries: Vec<MetaEntry<'_>> = read
                .iter()
                .zip(&exts)
                .zip(&cached)
                .filter(|(((_, ok), _), _)| *ok)
                .map(|(((p, _), ext), marks)| MetaEntry {
                    root_id: ids[p.root],
                    rel: &p.rel,
                    ext,
                    size: p.size,
                    mtime_ns: p.mtime_ns,
                    meta: &p.meta,
                    marks,
                })
                .collect();
            cache.store_meta(&entries)?;
            stats.read += entries.len();
            stats.failed += read.len() - entries.len();
            photos.extend(read.into_iter().map(|(p, _)| p));
        }

        for (i, &id) in ids.iter().enumerate() {
            let present: HashSet<String> = photos
                .iter()
                .filter(|p| p.root == i)
                .map(|p| p.rel.clone())
                .collect();
            stats.forgotten += cache.forget_missing(id, &present)?;
        }
        let mut c = Self { roots, photos };
        c.sort();
        Ok((c, stats))
    }

    /// Indices of the files of the same frame as `i` (same folder and base
    /// name, as duplicate detection defines siblings), including `i`.
    pub fn siblings(&self, i: usize) -> Vec<usize> {
        let Some(dir) = self.photos.get(i).and_then(|p| p.path.parent()) else {
            return Vec::new();
        };
        let here: Vec<usize> = (0..self.photos.len())
            .filter(|&j| self.photos[j].path.parent() == Some(dir))
            .collect();
        let stems: HashSet<crate::pipeline::FolderStem> = here
            .iter()
            .filter_map(|&j| crate::pipeline::folder_stem(&self.photos[j].path))
            .collect();
        let key = |j: usize| crate::pipeline::sibling_key(&self.photos[j].path, &stems);
        let mine = key(i);
        here.into_iter().filter(|&j| key(j) == mine).collect()
    }

    /// Write new marks for photo `i` (via `fileops`, journaled), and update
    /// the catalog and cache. Returns the journal batch, or None if nothing
    /// changed.
    pub fn set_marks(
        &mut self,
        i: usize,
        new: Marks,
        fileops: &crate::fileops::FileOps,
        cache: &Cache,
        also_stem: bool,
    ) -> Result<Option<i64>> {
        let p = &self.photos[i];
        let writes = sidecar::plan_write(&p.path, &p.marks, &new, also_stem)?;
        if writes.is_empty() {
            return Ok(None);
        }
        let name = p
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let batch = fileops.write_sidecars(&format!("marks: {name}"), writes)?;
        let root = &self.roots[p.root];
        let root_id = cache.root_id(&root.path, root.role == Role::Reference)?;
        // Re-derive the signature from disk so the next rescan sees it as current.
        let mut found = Sidecars::new();
        for side in [sidecar::per_stem(&p.path), sidecar::per_file(&p.path)] {
            for cand in [side.clone(), side.with_extension("XMP")] {
                if let Ok(m) = std::fs::metadata(&cand) {
                    found.insert(cand, scan::mtime_of(&m));
                }
            }
        }
        let sig = signature(&p.path, &found);
        let (_, file) = sidecar::present(&p.path, &|q| found.contains_key(q));
        let photo = &mut self.photos[i];
        photo.marks = new;
        photo.sidecar = file;
        photo.sidecar_sig = sig;
        cache.update_marks(
            root_id,
            &photo.rel,
            &CachedMarks {
                marks: photo.marks.clone(),
                sidecar: photo.sidecar.clone(),
                sig: photo.sidecar_sig.clone(),
            },
        )?;
        Ok(Some(batch))
    }

    fn sort(&mut self) {
        self.photos.sort_by(|a, b| {
            let (ta, tb) = (a.meta.capture_seconds(), b.meta.capture_seconds());
            match (ta, tb) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.path.cmp(&b.path))
        });
    }
}

/// Which sidecars exist for `image`, and when they last changed.
fn signature(image: &std::path::Path, sidecars: &Sidecars) -> String {
    let (stem, file) = sidecar::present(image, &|p| sidecars.contains_key(p));
    [stem, file]
        .into_iter()
        .flatten()
        .map(|p| format!("{}@{}", p.display(), sidecars.get(&p).copied().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join(";")
}
