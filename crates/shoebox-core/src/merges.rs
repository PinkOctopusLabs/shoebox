//! Photo merges in the library (SPEC, Merges; DECISIONS, "Photo merges:
//! groundwork"): where a merge's DNG goes, what it was made from, and the
//! one batch that adds it ([`crate::fileops::FileOps::add_merge`]).
//!
//! - **The file** goes beside its first source, named after it
//!   (`DSC_1234-HDR.dng`), `-1`, `-2` when taken. Never in a reference
//!   folder, and never from sources there: a merge reads its sources only.
//! - **The record** of what it was made from is kept twice. In the DNG's
//!   own XMP (file names and BLAKE3s, written once with it: it travels with
//!   the file and other tools can read it), and in
//!   `<root>/.shoebox/merges.json` in every writable root, like stacks'
//!   links, which the library reads (the Info panel's sources) and Shoebox's
//!   moves rewrite in the same batch. Undoing the merge empties it again.
//! - A merge is a photo of its own: not stacked with its sources (its EXIF
//!   has no sub-seconds, so capture time doesn't join them), and never
//!   offered as a perceptual duplicate of anything (it looks like its
//!   reference by design).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result, io};
use crate::scan::{Role, Root};

/// What kind of merge, and the suffix its file takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Hdr,
    Panorama,
    Focus,
}

impl Kind {
    pub fn suffix(self) -> &'static str {
        match self {
            Kind::Hdr => "HDR",
            Kind::Panorama => "Pano",
            Kind::Focus => "Focus",
        }
    }

    /// "HDR merge", for labels and the history.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Hdr => "HDR merge",
            Kind::Panorama => "panorama",
            Kind::Focus => "focus stack",
        }
    }

    /// The name in the DNG's record ("hdr").
    pub fn key(self) -> &'static str {
        match self {
            Kind::Hdr => "hdr",
            Kind::Panorama => "panorama",
            Kind::Focus => "focus",
        }
    }
}

/// A source as recorded: where it was, and its contents' BLAKE3 (hex).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub path: PathBuf,
    pub blake3: String,
}

/// One merge's record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merge {
    /// The DNG it made.
    pub file: PathBuf,
    pub kind: Kind,
    /// In the order merged; the first is where the file went.
    pub sources: Vec<Source>,
    /// Milliseconds since the epoch.
    #[serde(default)]
    pub made: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct MergesFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    merges: Vec<Merge>,
}

/// `<root>/.shoebox/merges.json`.
pub fn file_for(root: &Path) -> PathBuf {
    root.join(".shoebox").join("merges.json")
}

/// Every root's merges (one per file; later roots win), by file.
pub fn load(roots: &[PathBuf]) -> Vec<Merge> {
    let mut by_file: HashMap<PathBuf, Merge> = HashMap::new();
    for root in roots {
        let Ok(bytes) = std::fs::read(file_for(root)) else {
            continue;
        };
        match serde_json::from_slice::<MergesFile>(&bytes) {
            Ok(f) => {
                for m in f.merges {
                    by_file.insert(m.file.clone(), m);
                }
            }
            Err(e) => tracing::warn!("ignoring unreadable {}: {e}", file_for(root).display()),
        }
    }
    let mut v: Vec<Merge> = by_file.into_values().collect();
    v.sort_by(|a, b| a.file.cmp(&b.file));
    v
}

/// The bytes of `merges.json` for every writable root.
pub fn plan_save(writable_roots: &[PathBuf], merges: &[Merge]) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let body = serde_json::to_vec_pretty(&MergesFile {
        version: 1,
        merges: merges.to_vec(),
    })
    .map_err(|e| CoreError::Invalid(format!("merges: {e}")))?;
    Ok(writable_roots
        .iter()
        .map(|r| (file_for(r), body.clone()))
        .collect())
}

/// What a merge will write, and from what: the dry run the dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub kind: Kind,
    /// Where the DNG will go (free now; checked again as it's added).
    pub dst: PathBuf,
    /// Each source and its BLAKE3 as the merge reads it.
    pub sources: Vec<(PathBuf, [u8; 32])>,
}

impl Plan {
    /// The record for `merges.json`.
    pub fn record(&self) -> Merge {
        Merge {
            file: self.dst.clone(),
            kind: self.kind,
            sources: self
                .sources
                .iter()
                .map(|(p, h)| Source {
                    path: p.clone(),
                    blake3: blake3::Hash::from_bytes(*h).to_hex().to_string(),
                })
                .collect(),
            made: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64),
        }
    }
}

/// Where a merge of files whose first is `first` goes: beside it,
/// `<stem>-<suffix>.dng`, with `-1`, `-2` while that's taken.
pub fn destination(first: &Path, kind: Kind) -> PathBuf {
    let stem = first
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = first.with_file_name(format!("{stem}-{}.dng", kind.suffix()));
    let mut dst = base.clone();
    let mut n = 0;
    while std::fs::symlink_metadata(&dst).is_ok() {
        n += 1;
        dst = crate::fileops::suffixed(&base, n);
    }
    dst
}

/// Plan a merge of `sources` (in the order to merge): each must be a file
/// in a library folder, none in a reference one. Reads and hashes them.
pub fn plan(roots: &[Root], kind: Kind, sources: &[PathBuf]) -> Result<Plan> {
    let first = sources
        .first()
        .ok_or_else(|| CoreError::Invalid("a merge needs photos".into()))?;
    for s in sources {
        match roots.iter().find(|r| s.starts_with(&r.path)) {
            None => {
                return Err(CoreError::Refused(format!(
                    "{} isn't in the library",
                    s.display()
                )));
            }
            Some(r) if r.role == Role::Reference => {
                return Err(CoreError::Refused(format!(
                    "{} is in a reference folder, which is read-only: merges aren't made there",
                    s.display()
                )));
            }
            Some(_) => {}
        }
    }
    let hashed = sources
        .iter()
        .map(|p| {
            let mut h = blake3::Hasher::new();
            let f = std::fs::File::open(p).map_err(io(p))?;
            h.update_reader(f).map_err(io(p))?;
            Ok((p.clone(), *h.finalize().as_bytes()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Plan {
        kind,
        dst: destination(first, kind),
        sources: hashed,
    })
}

/// The merges made from `file` (it's one of their sources).
pub fn made_from<'a>(merges: &'a [Merge], file: &Path) -> impl Iterator<Item = &'a Merge> {
    merges
        .iter()
        .filter(move |m| m.sources.iter().any(|s| s.path == file))
}

/// The record of the merge that made `file`, if it's one.
pub fn of_file<'a>(merges: &'a [Merge], file: &Path) -> Option<&'a Merge> {
    merges.iter().find(|m| m.file == file)
}
