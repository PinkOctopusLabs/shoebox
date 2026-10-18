//! Recursive scan of library roots. Read-only.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use shoebox_decode::Format;
use walkdir::WalkDir;

use crate::error::{CoreError, Result, io};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Read-only; the "keep" side of duplicate detection.
    Reference,
    Scan,
}

#[derive(Debug, Clone)]
pub struct Root {
    /// Canonical path.
    pub path: PathBuf,
    pub role: Role,
}

impl Root {
    pub fn new(path: &Path, role: Role) -> Result<Self> {
        let path = path.canonicalize().map_err(io(path))?;
        if !path.is_dir() {
            return Err(CoreError::Invalid(format!(
                "{} is not a folder",
                path.display()
            )));
        }
        Ok(Self { path, role })
    }
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    /// Index into the roots slice passed to [`scan`].
    pub root: usize,
    pub path: PathBuf,
    /// Path relative to the root, UTF-8.
    pub rel: String,
    pub format: Format,
    pub size: u64,
    pub mtime_ns: i64,
    /// In the cloud only (see [`crate::cloud`]).
    pub cloud: bool,
}

/// Folders the scanner never descends into: the app's own output folders and
/// hidden folders such as `.shoebox`.
pub(crate) fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || name == "_duplicates" || name == "_rejected"
}

/// Walk every root and return supported image files, sorted by path.
/// `exclude` lists canonical folders to skip (such as a move destination).
pub fn scan(roots: &[Root], exclude: &[PathBuf]) -> Result<Vec<ScannedFile>> {
    Ok(scan_with_sidecars(roots, exclude)?.0)
}

/// XMP sidecars found during a scan: path → modification time (ns).
pub type Sidecars = std::collections::HashMap<PathBuf, i64>;

/// Like [`scan`], also collecting every `.xmp` file in the same walk.
pub fn scan_with_sidecars(
    roots: &[Root],
    exclude: &[PathBuf],
) -> Result<(Vec<ScannedFile>, Sidecars)> {
    let mut out = Vec::new();
    let mut sidecars = Sidecars::new();
    for (idx, root) in roots.iter().enumerate() {
        let walker = WalkDir::new(&root.path).follow_links(false).into_iter();
        let walker = walker.filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_dir() {
                return !skip_dir(&name) && !exclude.iter().any(|x| e.path().starts_with(x));
            }
            !name.starts_with('.') // includes AppleDouble `._*` files
        });
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("skipping unreadable entry: {e}");
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let is_xmp = entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("xmp"));
            if is_xmp {
                if let Ok(m) = entry.metadata() {
                    sidecars.insert(entry.path().to_path_buf(), mtime_of(&m));
                }
                continue;
            }
            let Some(format) = Format::from_path(entry.path()) else {
                continue;
            };
            let Ok(rel) = entry.path().strip_prefix(&root.path) else {
                continue;
            };
            let Some(rel) = rel.to_str() else {
                tracing::warn!("skipping non-UTF-8 path {}", entry.path().display());
                continue;
            };
            let meta = entry.metadata().map_err(|e| CoreError::Io {
                path: entry.path().to_path_buf(),
                source: e.into(),
            })?;
            let mtime_ns = mtime_of(&meta);
            out.push(ScannedFile {
                root: idx,
                path: entry.path().to_path_buf(),
                rel: rel.to_owned(),
                format,
                size: meta.len(),
                mtime_ns,
                cloud: crate::cloud::is_cloud_only(entry.path(), &meta),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((out, sidecars))
}

pub fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// Every folder under the roots that the scanner would look in (including
/// empty ones, so they can be shown and used as move targets).
pub fn folders(roots: &[Root]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let walker = WalkDir::new(&root.path).follow_links(false).into_iter();
        for e in walker
            .filter_entry(|e| {
                e.depth() == 0
                    || (e.file_type().is_dir() && !skip_dir(&e.file_name().to_string_lossy()))
            })
            .flatten()
        {
            if e.depth() > 0 && e.file_type().is_dir() {
                out.push(e.into_path());
            }
        }
    }
    out.sort();
    out
}
