//! The browsable library: every image under the library roots with its
//! metadata, loaded from the cache instantly and refreshed from disk.

use std::collections::HashSet;
use std::path::PathBuf;

use rayon::prelude::*;
use shoebox_decode::Format;

use crate::cache::{Cache, MetaEntry};
use crate::error::Result;
use crate::meta::{self, Meta};
use crate::scan::{self, Role, Root};

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
            for (rel, (size, mtime_ns, meta)) in cache.load_meta(id)? {
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
        let files = scan::scan(&roots, &[])?;
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
        let mut todo = Vec::new();
        for f in files {
            match known[f.root].get(&f.rel) {
                Some((size, mtime, m)) if *size == f.size && *mtime == f.mtime_ns => {
                    stats.cached += 1;
                    photos.push(Photo {
                        root: f.root,
                        rel: f.rel,
                        path: f.path,
                        format: f.format,
                        size: f.size,
                        mtime_ns: f.mtime_ns,
                        meta: m.clone(),
                    });
                }
                _ => todo.push(f),
            }
        }

        let total = todo.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        for chunk in todo.chunks(STORE_CHUNK) {
            let read: Vec<(Photo, bool)> = chunk
                .par_iter()
                .map(|f| {
                    let (m, ok) = match meta::read(&f.path, f.format) {
                        Ok(m) => (m, true),
                        Err(e) => {
                            tracing::warn!("metadata: {e}");
                            (Meta::default(), false)
                        }
                    };
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
                    };
                    (p, ok)
                })
                .collect();
            let exts: Vec<String> = read
                .iter()
                .map(|(p, _)| p.format.label().to_ascii_lowercase())
                .collect();
            let entries: Vec<MetaEntry<'_>> = read
                .iter()
                .zip(&exts)
                .filter(|((_, ok), _)| *ok)
                .map(|((p, _), ext)| MetaEntry {
                    root_id: ids[p.root],
                    rel: &p.rel,
                    ext,
                    size: p.size,
                    mtime_ns: p.mtime_ns,
                    meta: &p.meta,
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
