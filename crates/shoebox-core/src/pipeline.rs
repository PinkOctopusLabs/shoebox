//! Scan → analyse (through the cache) → duplicate items.

use rayon::prelude::*;

use crate::cache::{self, Cache};
use crate::dupes::Item;
use crate::error::Result;
use crate::hash::{self, Analysis};
use crate::scan::{Role, Root, ScannedFile};

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub files: usize,
    pub cached: usize,
    pub analyzed: usize,
    pub unreadable: usize,
    pub undecodable: usize,
    /// In the cloud only and not analysed before: left out (reading them
    /// would download them), including any that went back to the cloud
    /// after the scan.
    pub cloud: usize,
}

/// A scanned file with its analysis.
pub struct Analyzed {
    pub file: ScannedFile,
    pub analysis: Analysis,
}

const STORE_CHUNK: usize = 256;

/// Analyse every file, reusing cached results for unchanged files. Files that
/// can't be read are skipped with a warning.
pub fn analyze_all(
    cache: &mut Cache,
    roots: &[Root],
    files: Vec<ScannedFile>,
) -> Result<(Vec<Analyzed>, Stats)> {
    analyze_all_progress(cache, roots, files, &|_, _| {})
}

/// [`analyze_all`], calling `progress(done, total)` as uncached files are
/// analysed.
pub fn analyze_all_progress(
    cache: &mut Cache,
    roots: &[Root],
    files: Vec<ScannedFile>,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<(Vec<Analyzed>, Stats)> {
    let root_ids = roots
        .iter()
        .map(|r| cache.root_id(&r.path, r.role == Role::Reference))
        .collect::<Result<Vec<_>>>()?;
    let mut stats = Stats {
        files: files.len(),
        ..Stats::default()
    };
    let mut done = Vec::with_capacity(files.len());
    let mut todo = Vec::new();
    for f in files {
        match cache.lookup(root_ids[f.root], &f.rel, f.size, f.mtime_ns)? {
            Some(a) => {
                stats.cached += 1;
                done.push(Analyzed {
                    file: f,
                    analysis: a,
                });
            }
            None if f.cloud => stats.cloud += 1,
            None => todo.push(f),
        }
    }
    tracing::info!("{} cached, {} to analyse", stats.cached, todo.len());
    let total = todo.len();
    let done_n = std::sync::atomic::AtomicUsize::new(0);

    for chunk in todo.chunks(STORE_CHUNK) {
        let went = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<_> = chunk
            .par_iter()
            .filter_map(|f| {
                // A cloud drive may have sent it back to the cloud since the
                // scan (Google Drive does when the disk is full): reading it
                // now would wait for a download.
                let r = if crate::cloud::path_is_cloud_only(&f.path) {
                    went.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    None
                } else {
                    match hash::analyze_path(&f.path) {
                        Ok(a) => Some(Analyzed {
                            file: f.clone(),
                            analysis: a,
                        }),
                        Err(e) => {
                            tracing::warn!("skipping {e}");
                            None
                        }
                    }
                };
                let n = done_n.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                progress(n, total);
                r
            })
            .collect();
        let went = went.into_inner();
        stats.cloud += went;
        stats.unreadable += chunk.len() - results.len() - went;
        stats.analyzed += results.len();
        let exts: Vec<String> = results
            .iter()
            .map(|r| r.file.format.label().to_ascii_lowercase())
            .collect();
        let entries: Vec<_> = results
            .iter()
            .zip(&exts)
            .map(|(r, ext)| cache::Entry {
                root_id: root_ids[r.file.root],
                rel: &r.file.rel,
                ext,
                size: r.file.size,
                mtime_ns: r.file.mtime_ns,
                analysis: &r.analysis,
            })
            .collect();
        cache.store_all(&entries)?;
        done.extend(results);
        tracing::info!(
            "analysed {}/{}",
            stats.analyzed + stats.unreadable,
            todo.len()
        );
    }
    stats.undecodable = done.iter().filter(|a| a.analysis.hashes.is_empty()).count();
    done.sort_by(|a, b| a.file.path.cmp(&b.file.path));
    Ok((done, stats))
}

pub(crate) type FolderStem = (std::path::PathBuf, String);

pub(crate) fn folder_stem(path: &std::path::Path) -> Option<FolderStem> {
    Some((
        path.parent()?.to_path_buf(),
        path.file_stem()?.to_str()?.to_lowercase(),
    ))
}

/// Same folder and base name, ignoring case and extension. A trailing `-N`
/// (copy or virtual-copy suffix) is ignored only when the name without it is
/// also present in the folder, so `x-1.tif` joins `x.nef`, but names that end
/// in a sub-second (`20260501-091502-65`) are not cut short.
pub(crate) fn sibling_key(
    path: &std::path::Path,
    stems: &std::collections::HashSet<FolderStem>,
) -> Option<FolderStem> {
    let (dir, stem) = folder_stem(path)?;
    let base = sibling_stem(&stem, |head| {
        stems.contains(&(dir.clone(), head.to_owned()))
    });
    let base = base.to_owned();
    Some((dir, base))
}

/// The base name `stem` (lower case) stacks under: a trailing `-N` is cut
/// while the name without it is `present` in the folder. Stripped
/// repeatedly, so `x-40-1` → `x-40` → `x` when each exists: every member of
/// a chain lands on the same key.
pub(crate) fn sibling_stem(mut stem: &str, present: impl Fn(&str) -> bool) -> &str {
    while let Some((head, tail)) = stem.rsplit_once('-')
        && !head.is_empty()
        && !tail.is_empty()
        && tail.len() <= 2
        && tail.chars().all(|c| c.is_ascii_digit())
        && present(head)
    {
        stem = head;
    }
    stem
}

pub fn items(roots: &[Root], analyzed: &[Analyzed]) -> Vec<Item> {
    let stems: std::collections::HashSet<FolderStem> = analyzed
        .iter()
        .filter_map(|a| folder_stem(&a.file.path))
        .collect();
    let mut keys: std::collections::HashMap<FolderStem, usize> = Default::default();
    let merged: std::collections::HashSet<std::path::PathBuf> =
        crate::merges::load(&roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>())
            .into_iter()
            .map(|m| m.file)
            .collect();
    analyzed
        .iter()
        .map(|a| {
            let siblings = sibling_key(&a.file.path, &stems).map(|k| {
                let next = keys.len();
                *keys.entry(k).or_insert(next)
            });
            let x = &a.analysis.exif;
            Item {
                role: roots[a.file.root].role,
                format: a.file.format,
                blake3: a.analysis.blake3,
                image_data_hash: a.analysis.image_data_hash,
                hashes: a.analysis.hashes.clone(),
                pixels: u64::from(a.analysis.width.unwrap_or(0))
                    * u64::from(a.analysis.height.unwrap_or(0)),
                siblings,
                luma_stddev: a.analysis.luma_stddev,
                capture: x
                    .capture_time
                    .as_deref()
                    .and_then(|t| crate::exif::capture_seconds(t, x.subsec.as_deref())),
                merged: merged.contains(&a.file.path),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{folder_stem, sibling_key};
    use std::collections::HashSet;
    use std::path::Path;

    #[test]
    fn sibling_keys() {
        let files = [
            "/a/20260501-091502-65.nef",
            "/a/20260501-091502-65.jpg",
            "/a/20260501-091502-65-1.tif",
            "/a/20260501-091502-80.nef",
            "/a/20260430-101010.tif",
            "/a/20260430-101010-1.tif",
            "/b/f0001.jpg",
        ];
        let stems: HashSet<_> = files
            .iter()
            .filter_map(|f| folder_stem(Path::new(f)))
            .collect();
        let k = |p: &str| sibling_key(Path::new(p), &stems).unwrap().1;
        assert_eq!(k(files[0]), "20260501-091502-65");
        assert_eq!(
            k(files[2]),
            "20260501-091502-65",
            "-1 variant joins its frame"
        );
        assert_eq!(
            k(files[3]),
            "20260501-091502-80",
            "same second, other frame"
        );
        assert_eq!(k(files[5]), "20260430-101010");
        assert_eq!(k(files[6]), "f0001");

        // A stray `x.nef` next to `x-40.*` pulls the whole chain together.
        let files = ["/c/x.nef", "/c/x-40.nef", "/c/x-40-1.tif"];
        let stems: HashSet<_> = files
            .iter()
            .filter_map(|f| folder_stem(Path::new(f)))
            .collect();
        let k = |p: &str| sibling_key(Path::new(p), &stems).unwrap().1;
        assert_eq!((k(files[1]), k(files[2])), ("x".into(), "x".into()));
    }
}
