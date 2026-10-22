//! The browsable library: every image under the library roots with its
//! metadata, loaded from the cache instantly and refreshed from disk.

use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;
use shoebox_decode::Format;

use crate::cache::{Cache, CachedMarks, MetaEntry};
use crate::changes::{By, Change, What};
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
    /// A change since you last viewed this photo.
    pub change: Option<Change>,
    /// Its contents are in the cloud only (see [`crate::cloud`]): not read,
    /// so no metadata unless it was here before.
    pub cloud: bool,
}

impl Photo {
    /// Where it was taken: the sidecar's location (added or corrected in
    /// another app) over the camera's EXIF.
    pub fn location(&self) -> Option<(f64, f64)> {
        self.marks.gps().or(self.meta.exif.gps)
    }

    /// Latest sidecar modification time (ns), if it has a sidecar.
    pub fn sidecar_mtime_ns(&self) -> Option<i64> {
        crate::changes::sig_mtime(&self.sidecar_sig)
    }

    /// When the image or its sidecar last changed on disk (ns).
    pub fn modified_ns(&self) -> i64 {
        self.mtime_ns.max(self.sidecar_mtime_ns().unwrap_or(0))
    }
}

/// Photos affected by changed paths on disk (from the file watcher).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Affected {
    /// Photos whose image or sidecar changed.
    pub photos: Vec<usize>,
    /// Images were added or removed, or folders changed: rescan.
    pub rescan: bool,
    /// A root's `.shoebox/events.json` changed.
    pub events: bool,
}

/// Result of [`Catalog::reread`].
#[derive(Debug, Default)]
pub struct Reread {
    /// Photos that changed (indices before any re-sort).
    pub changed: Vec<std::path::PathBuf>,
    /// Photos were re-sorted (an image changed): indices are stale.
    pub reordered: bool,
    /// A photo's image is gone: rescan.
    pub missing: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub roots: Vec<Root>,
    /// Sorted by capture time (undated last), then path.
    pub photos: Vec<Photo>,
    /// Frames: each photo belongs to exactly one stack.
    pub stacks: Vec<crate::stacks::Stack>,
    /// `stack_of[i]` is the stack index of `photos[i]`.
    pub stack_of: Vec<usize>,
    /// Files added to other stacks by hand (from `.shoebox/stacks.json`).
    pub links: Vec<crate::stacks::Link>,
    /// Photo merges' records (from `.shoebox/merges.json`).
    pub merges: Vec<crate::merges::Merge>,
    /// Every folder under the roots, empty ones included (after a refresh).
    pub folders: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RefreshStats {
    pub files: usize,
    pub cached: usize,
    pub read: usize,
    pub failed: usize,
    pub forgotten: usize,
    /// Known files that changed on disk since the last scan.
    pub changed: usize,
    /// Files in the cloud only (listed, not read).
    pub cloud: usize,
}

/// A file to read during a refresh.
struct Todo {
    f: scan::ScannedFile,
    /// Metadata still valid (the image is unchanged).
    meta: Option<Meta>,
    /// What changed in a file Shoebox already knew.
    what: Option<What>,
    /// Its unseen change, if any.
    prev: Option<Change>,
}

const STORE_CHUNK: usize = 512;

impl Catalog {
    /// Everything the cache knows about these roots. Doesn't touch the
    /// library, so a large library opens instantly; follow with `refresh`.
    pub fn from_cache(cache: &Cache, roots: Vec<Root>) -> Result<Self> {
        let mut photos = Vec::new();
        for (i, root) in roots.iter().enumerate() {
            let id = cache.root_id(&root.path, root.role == Role::Reference)?;
            for (rel, k) in cache.load_meta(id)? {
                let marks = k.marks.unwrap_or_else(|| CachedMarks {
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
                    size: k.size,
                    mtime_ns: k.mtime_ns,
                    meta: k.meta,
                    marks: marks.marks,
                    sidecar: marks.sidecar,
                    sidecar_sig: marks.sig,
                    change: k.change,
                    cloud: false,
                });
            }
        }
        let paths: Vec<PathBuf> = roots.iter().map(|r| r.path.clone()).collect();
        let links = crate::stacks::load_links(&paths);
        let merges = crate::merges::load(&paths);
        let mut c = Self {
            roots,
            photos,
            links,
            merges,
            ..Default::default()
        };
        c.finish();
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
        let mut todo: Vec<Todo> = Vec::new();
        for f in files {
            let sig = signature(&f.path, &sidecars);
            match known[f.root].get(&f.rel) {
                Some(k) if k.size == f.size && k.mtime_ns == f.mtime_ns => match &k.marks {
                    Some(cm) if cm.sig == sig => {
                        stats.cached += 1;
                        photos.push(Photo {
                            root: f.root,
                            rel: f.rel,
                            path: f.path,
                            format: f.format,
                            size: f.size,
                            mtime_ns: f.mtime_ns,
                            meta: k.meta.clone(),
                            marks: cm.marks.clone(),
                            sidecar: cm.sidecar.clone(),
                            sidecar_sig: sig,
                            change: k.change.clone(),
                            cloud: f.cloud,
                        });
                    }
                    // Image unchanged, sidecar changed: re-read marks only.
                    cm => todo.push(Todo {
                        f,
                        meta: Some(k.meta.clone()),
                        what: cm.is_some().then_some(What::SIDECAR),
                        prev: k.change.clone(),
                    }),
                },
                Some(k) => {
                    let sidecar = k.marks.as_ref().is_some_and(|cm| cm.sig != sig);
                    todo.push(Todo {
                        f,
                        meta: None,
                        what: Some(What {
                            image: true,
                            sidecar,
                        }),
                        prev: k.change.clone(),
                    });
                }
                None => todo.push(Todo {
                    f,
                    meta: None,
                    what: None,
                    prev: None,
                }),
            }
        }

        let total = todo.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        for chunk in todo.chunks(STORE_CHUNK) {
            let read: Vec<(Photo, bool)> = chunk
                .par_iter()
                .map(|t| {
                    let f = &t.f;
                    // A cloud-only file isn't read (that would download it),
                    // nor cached, so it's read once it's here.
                    let (m, ok) = match &t.meta {
                        Some(m) => (m.clone(), !f.cloud),
                        None if f.cloud => (Meta::default(), false),
                        None => match meta::read(&f.path, f.format) {
                            Ok(m) => (m, true),
                            Err(e) => {
                                tracing::warn!("metadata: {e}");
                                (Meta::default(), false)
                            }
                        },
                    };
                    let (stem, file) = sidecar::present(&f.path, &|p| sidecars.contains_key(p));
                    let image = (!f.cloud).then_some(f.path.as_path());
                    let loaded = sidecar::load(image, f.format, stem.as_deref(), file.as_deref())
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
                        cloud: f.cloud,
                        change: match t.what {
                            // Noticed now, made by something other than Shoebox
                            // (Shoebox's own writes update the cache as they go).
                            Some(w) => {
                                Some(Change::merge(t.prev.as_ref(), Change::now(w, By::Other)))
                            }
                            None => t.prev.clone(),
                        },
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
            for ((p, ok), t) in read.iter().zip(chunk) {
                if *ok
                    && t.what.is_some()
                    && let Some(c) = &p.change
                {
                    cache.record_change(ids[p.root], &p.rel, c)?;
                    stats.changed += 1;
                }
            }
            let cloud = read.iter().filter(|(p, _)| p.cloud).count();
            stats.read += entries.len();
            stats.cloud += cloud;
            stats.failed += read.len() - entries.len() - cloud;
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
        let paths: Vec<PathBuf> = roots.iter().map(|r| r.path.clone()).collect();
        let links = crate::stacks::load_links(&paths);
        let merges = crate::merges::load(&paths);
        let folders = scan::folders(&roots);
        let mut c = Self {
            roots,
            photos,
            links,
            merges,
            folders,
            ..Default::default()
        };
        c.finish();
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

    /// Whether photo `i` is in a reference root (read-only: no sidecar
    /// writes).
    pub fn is_reference(&self, i: usize) -> bool {
        self.roots
            .get(self.photos[i].root)
            .is_some_and(|r| r.role == Role::Reference)
    }

    /// Write new marks for several photos as one journaled batch (one undo
    /// step), and update the catalog and cache. Returns the batch, or None if
    /// nothing changed. Raw files also get `name.xmp` when `stem_for_raw`.
    /// `extra` writes (such as `.shoebox/events.json`) join the same batch.
    /// Photos in reference roots are skipped (see [`Self::is_reference`]), so
    /// one read-only photo doesn't stop the rest.
    pub fn set_marks(
        &mut self,
        updates: &[(usize, Marks)],
        extra: Vec<(PathBuf, Vec<u8>)>,
        fileops: &crate::fileops::FileOps,
        cache: &Cache,
        stem_for_raw: bool,
        label: &str,
    ) -> Result<Option<i64>> {
        let updates: Vec<&(usize, Marks)> = updates
            .iter()
            .filter(|(i, _)| !self.is_reference(*i))
            .collect();
        let mut writes = extra;
        for (i, new) in &updates {
            let p = &self.photos[*i];
            let also_stem = stem_for_raw && p.format == Format::Nef;
            writes.extend(sidecar::plan_write(&p.path, &p.marks, new, also_stem)?);
        }
        if writes.is_empty() {
            return Ok(None);
        }
        let batch = fileops.write_sidecars(label, writes)?;
        let by = By::Shoebox {
            batch,
            label: label.to_owned(),
        };
        for (i, new) in updates {
            self.note_marks(*i, new.clone(), cache)?;
            self.note_change(*i, Change::now(What::SIDECAR, by.clone()), cache)?;
        }
        Ok(Some(batch))
    }

    /// Record marks now on disk for photo `i` in the catalog and cache.
    fn note_marks(&mut self, i: usize, marks: Marks, cache: &Cache) -> Result<()> {
        let path = self.photos[i].path.clone();
        let found = sidecars_of(&path);
        let sig = signature(&path, &found);
        let (_, file) = sidecar::present(&path, &|q| found.contains_key(q));
        let root = &self.roots[self.photos[i].root];
        let root_id = cache.root_id(&root.path, root.role == Role::Reference)?;
        let photo = &mut self.photos[i];
        photo.marks = marks;
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
        )
    }

    fn root_id(&self, i: usize, cache: &Cache) -> Result<i64> {
        let root = &self.roots[self.photos[i].root];
        cache.root_id(&root.path, root.role == Role::Reference)
    }

    /// Fold a change into photo `i`'s unseen change, in the catalog and cache.
    fn note_change(&mut self, i: usize, c: Change, cache: &Cache) -> Result<()> {
        let root_id = self.root_id(i, cache)?;
        let p = &mut self.photos[i];
        let merged = Change::merge(p.change.as_ref(), c);
        cache.record_change(root_id, &p.rel, &merged)?;
        p.change = Some(merged);
        Ok(())
    }

    /// Clear the unseen changes of these photos (viewed, or Mark as Seen).
    pub fn mark_seen(&mut self, photos: &[usize], cache: &mut Cache) -> Result<()> {
        let mut rows = Vec::new();
        for &i in photos {
            if self.photos[i].change.is_some() {
                rows.push((self.root_id(i, cache)?, i));
            }
        }
        let keys: Vec<(i64, &str)> = rows
            .iter()
            .map(|&(r, i)| (r, self.photos[i].rel.as_str()))
            .collect();
        cache.mark_seen(&keys)?;
        for (_, i) in rows {
            self.photos[i].change = None;
        }
        Ok(())
    }

    /// Which photos changed paths on disk affect. Hidden files, Shoebox's
    /// output folders and anything outside the roots are ignored.
    pub fn affected(&self, paths: &[PathBuf]) -> Affected {
        let mut out = Affected::default();
        let index: std::collections::HashMap<&std::path::Path, usize> = self
            .photos
            .iter()
            .enumerate()
            .map(|(i, p)| (p.path.as_path(), i))
            .collect();
        // (folder, lower-case stem) → photos, for `name.xmp` sidecars.
        let mut by_stem: Option<std::collections::HashMap<(&std::path::Path, String), Vec<usize>>> =
            None;
        for path in paths {
            let Some(root) = self.roots.iter().find(|r| path.starts_with(&r.path)) else {
                continue;
            };
            let Ok(rel) = path.strip_prefix(&root.path) else {
                continue;
            };
            let comps: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if comps.first().is_some_and(|c| c == ".shoebox") {
                out.events |= comps.last().is_some_and(|c| c == "events.json");
                continue;
            }
            let (dirs, name) = match comps.split_last() {
                Some((n, d)) => (d, n),
                None => {
                    out.rescan = true; // the root itself
                    continue;
                }
            };
            if dirs.iter().any(|d| scan::skip_dir(d)) || name.starts_with('.') {
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase());
            if ext.as_deref() == Some("xmp") {
                // `x.nef.xmp` → `x.nef`; `x.xmp` → every `x.*` in the folder.
                let image = path.with_extension("");
                if let Some(&i) = index.get(image.as_path()) {
                    out.photos.push(i);
                } else if let Some(i) = self
                    .photos
                    .iter()
                    .position(|p| p.path.as_os_str().eq_ignore_ascii_case(image.as_os_str()))
                {
                    out.photos.push(i);
                }
                let map = by_stem.get_or_insert_with(|| {
                    let mut m: std::collections::HashMap<_, Vec<usize>> = Default::default();
                    for (i, p) in self.photos.iter().enumerate() {
                        if let (Some(d), Some(s)) = (p.path.parent(), p.path.file_stem()) {
                            m.entry((d, s.to_string_lossy().to_lowercase()))
                                .or_default()
                                .push(i);
                        }
                    }
                    m
                });
                if let (Some(d), Some(s)) = (path.parent(), path.file_stem())
                    && let Some(v) = map.get(&(d, s.to_string_lossy().to_lowercase()))
                {
                    out.photos.extend(v);
                }
            } else if Format::from_path(path).is_some() {
                match index.get(path.as_path()) {
                    Some(&i) if path.is_file() => out.photos.push(i),
                    _ => out.rescan = true, // added, removed or renamed
                }
            } else if ext.is_none() && !path.is_file() {
                out.rescan = true; // a folder appeared, went or was renamed
            }
        }
        out.photos.sort_unstable();
        out.photos.dedup();
        out
    }

    /// Re-read these photos from disk (image metadata if the image changed
    /// or was just downloaded from the cloud, marks if a sidecar changed),
    /// recording each change as made `by` (a download isn't a change).
    pub fn reread(&mut self, photos: &[usize], by: &By, cache: &mut Cache) -> Result<Reread> {
        let mut out = Reread::default();
        let mut entries: Vec<(usize, bool)> = Vec::new();
        for &i in photos {
            let path = self.photos[i].path.clone();
            let Ok(md) = std::fs::metadata(&path) else {
                out.missing = true;
                continue;
            };
            let (size, mtime_ns) = (md.len(), scan::mtime_of(&md));
            let found = sidecars_of(&path);
            let sig = signature(&path, &found);
            let p = &self.photos[i];
            let cloud = crate::cloud::is_cloud_only(&path, &md);
            let fetched = p.cloud && !cloud;
            let what = What {
                image: size != p.size || mtime_ns != p.mtime_ns,
                sidecar: sig != p.sidecar_sig,
            };
            if !what.any() && cloud == p.cloud {
                continue;
            }
            // A cloud-only file isn't read, nor cached (see `refresh`).
            let mut ok = !cloud;
            if (what.image || fetched) && !cloud {
                match meta::read(&path, p.format) {
                    Ok(m) => self.photos[i].meta = m,
                    Err(e) => {
                        tracing::warn!("metadata: {e}");
                        ok = false;
                    }
                }
                out.reordered = true;
            }
            let p = &self.photos[i];
            let (stem, file) = sidecar::present(&path, &|q| found.contains_key(q));
            let image = (!cloud).then_some(path.as_path());
            let loaded = sidecar::load(image, p.format, stem.as_deref(), file.as_deref())
                .unwrap_or_else(|e| {
                    tracing::warn!("marks: {e}");
                    sidecar::Loaded::default()
                });
            let p = &mut self.photos[i];
            p.size = size;
            p.mtime_ns = mtime_ns;
            p.marks = loaded.marks;
            p.sidecar = loaded.sidecar;
            p.sidecar_sig = sig;
            p.cloud = cloud;
            if what.any() {
                p.change = Some(Change::merge(
                    p.change.as_ref(),
                    Change::now(what, by.clone()),
                ));
            }
            entries.push((i, ok));
            out.changed.push(path);
        }
        let ids: Vec<i64> = entries
            .iter()
            .map(|&(i, _)| self.root_id(i, cache))
            .collect::<Result<_>>()?;
        let exts: Vec<String> = entries
            .iter()
            .map(|&(i, _)| self.photos[i].format.label().to_ascii_lowercase())
            .collect();
        let marks: Vec<CachedMarks> = entries
            .iter()
            .map(|&(i, _)| {
                let p = &self.photos[i];
                CachedMarks {
                    marks: p.marks.clone(),
                    sidecar: p.sidecar.clone(),
                    sig: p.sidecar_sig.clone(),
                }
            })
            .collect();
        let rows: Vec<MetaEntry<'_>> = entries
            .iter()
            .enumerate()
            .filter(|(_, (_, ok))| *ok)
            .map(|(k, &(i, _))| {
                let p = &self.photos[i];
                MetaEntry {
                    root_id: ids[k],
                    rel: &p.rel,
                    ext: &exts[k],
                    size: p.size,
                    mtime_ns: p.mtime_ns,
                    meta: &p.meta,
                    marks: &marks[k],
                }
            })
            .collect();
        cache.store_meta(&rows)?;
        for (k, &(i, _)) in entries.iter().enumerate() {
            if let Some(c) = &self.photos[i].change {
                cache.record_change(ids[k], &self.photos[i].rel, c)?;
            }
        }
        if out.reordered {
            self.finish();
        }
        Ok(out)
    }

    /// Carry catalog entries and cache rows across moves Shoebox made (after
    /// the batch ran), so moved files aren't read again or reported as
    /// changed. Files moved out of the library (`_rejected/`, the Trash) are
    /// left for the next rescan to forget.
    pub fn note_moved(
        &mut self,
        moves: &[crate::fileops::PlannedMove],
        cache: &mut Cache,
    ) -> Result<()> {
        let index: std::collections::HashMap<PathBuf, usize> = self
            .photos
            .iter()
            .enumerate()
            .map(|(i, p)| (p.path.clone(), i))
            .collect();
        for m in moves {
            let Some(&i) = index.get(&m.src) else {
                continue;
            };
            let Some(new_root) = self.roots.iter().position(|r| m.dst.starts_with(&r.path)) else {
                continue;
            };
            let Some(rel) = m
                .dst
                .strip_prefix(&self.roots[new_root].path)
                .ok()
                .and_then(|r| r.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let hidden = rel.split('/').rev().skip(1).any(scan::skip_dir);
            if hidden {
                continue;
            }
            let old_id = self.root_id(i, cache)?;
            if let (Some(from), Some(to)) = (
                self.roots[self.photos[i].root].path.to_str(),
                self.roots[new_root].path.to_str(),
            ) {
                cache.move_edits((from, &self.photos[i].rel), (to, &rel))?;
                if let (Some(a), Some(b)) = (self.photos[i].path.to_str(), m.dst.to_str()) {
                    cache.move_exports(a, b)?;
                }
            }
            let new = &self.roots[new_root];
            let new_id = cache.root_id(&new.path, new.role == Role::Reference)?;
            cache.move_file((old_id, &self.photos[i].rel), (new_id, &rel))?;
            let found = sidecars_of(&m.dst);
            let sig = signature(&m.dst, &found);
            let (_, file) = sidecar::present(&m.dst, &|q| found.contains_key(q));
            let p = &mut self.photos[i];
            p.root = new_root;
            p.rel = rel;
            p.path = m.dst.clone();
            p.sidecar = file;
            p.sidecar_sig = sig;
            cache.update_marks(
                new_id,
                &p.rel,
                &CachedMarks {
                    marks: p.marks.clone(),
                    sidecar: p.sidecar.clone(),
                    sig: p.sidecar_sig.clone(),
                },
            )?;
        }
        self.finish();
        Ok(())
    }

    /// Replace the hand-made stack links and rebuild stacks (indices change).
    pub fn set_links(&mut self, links: Vec<crate::stacks::Link>) {
        self.links = links;
        self.finish();
    }

    /// Sort photos and rebuild stacks (indices change).
    fn finish(&mut self) {
        self.sort();
        let apart: HashSet<&std::path::Path> =
            self.merges.iter().map(|m| m.file.as_path()).collect();
        let (stacks, of) = crate::stacks::build_apart(&self.photos, &self.links, &apart);
        self.stacks = stacks;
        self.stack_of = of;
    }

    fn sort(&mut self) {
        // Parse each capture time once, not on every comparison.
        let mut keyed: Vec<(Option<f64>, Photo)> = std::mem::take(&mut self.photos)
            .into_iter()
            .map(|p| (p.meta.capture_seconds(), p))
            .collect();
        keyed.sort_by(|(ta, a), (tb, b)| {
            match (ta, tb) {
                (Some(x), Some(y)) => x.total_cmp(y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.path.cmp(&b.path))
        });
        self.photos = keyed.into_iter().map(|(_, p)| p).collect();
    }
}

/// The sidecars on disk for one image, with their modification times.
fn sidecars_of(path: &std::path::Path) -> Sidecars {
    let mut found = Sidecars::new();
    for side in [sidecar::per_stem(path), sidecar::per_file(path)] {
        for cand in [side.clone(), side.with_extension("XMP")] {
            if let Ok(m) = std::fs::metadata(&cand) {
                found.insert(cand, scan::mtime_of(&m));
            }
        }
    }
    found
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
