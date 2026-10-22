//! The sync record: which Photos assets have been copied into a library
//! root, so a later sync copies only new ones.
//!
//! It lives in the root (`<root>/.shoebox/photos/`), like events and
//! collections, so it survives a catalog rebuild. Each sync writes its
//! records as new files, a few assets at a time, in the sync's own batch
//! (`synced-<run>-<n>.json`), and never rewrites them: the journal keeps
//! only small files, and undoing a sync empties exactly its files, so the
//! records from before it come back. When several files have the same
//! asset, the latest record wins. `choices.json` holds what each album
//! becomes and the change token, rewritten each run.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::{AssetId, ResourceKind};
use crate::error::{CoreError, Result, io};

/// `<root>/.shoebox/photos`.
pub fn dir_for(root: &Path) -> PathBuf {
    root.join(".shoebox").join("photos")
}

/// What became of an asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Its files were copied in.
    Copied,
    /// Its original was in the library already (same bytes); nothing copied.
    InLibrary,
}

/// One file of an asset in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    pub kind: ResourceKind,
    /// Relative to the root whose record this is (for `in_library`: the
    /// library file's full path).
    pub rel: String,
    /// BLAKE3, hex.
    pub blake3: String,
    pub size: u64,
}

/// What a sync recorded for an asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub local_id: String,
    pub status: Status,
    #[serde(default)]
    pub files: Vec<File>,
    /// The albums it was in, so a later sync sees which it joined.
    #[serde(default)]
    pub albums: Vec<String>,
    #[serde(default)]
    pub favourite: bool,
    /// When Photos last changed the edit that was imported (RFC 3339), so
    /// a newer edit there is offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_modified: Option<String>,
    /// When this was recorded (ms since 1970); the latest record wins.
    pub updated: i64,
}

impl Entry {
    pub fn file(&self, kind: ResourceKind) -> Option<&File> {
        self.files.iter().rev().find(|f| f.kind == kind)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncedFile {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    assets: BTreeMap<String, Entry>,
}

/// What an album's name becomes on the photos in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlbumUse {
    /// An event, nested as Photos' album folders are.
    Event,
    /// A keyword under `Albums|`.
    Keyword,
    Nothing,
}

/// An album's choice, as last made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumChoice {
    /// The name it's given (folders and name, `›`-joined, as edited).
    pub name: String,
    #[serde(rename = "use")]
    pub kind: AlbumUse,
}

/// Choices remembered between syncs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choices {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub albums: BTreeMap<String, AlbumChoice>,
    /// Photos' persistent change token after the last sync (base64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_token: Option<String>,
}

/// The sync records of every root, read together: an asset copied into
/// any root isn't copied again.
#[derive(Debug, Clone, Default)]
pub struct Ledgers {
    /// By [`AssetId::key`]: (root index, record).
    entries: HashMap<String, (usize, Entry)>,
    /// Local id → key, for an asset whose cloud id came later.
    by_local: HashMap<String, String>,
    /// Per root, as given to [`Ledgers::load`].
    pub choices: Vec<Choices>,
}

impl Ledgers {
    /// Read the records of every root (unreadable files are skipped and
    /// logged; a file emptied by an undo reads as nothing).
    pub fn load(roots: &[PathBuf]) -> Self {
        let mut out = Ledgers::default();
        for (i, root) in roots.iter().enumerate() {
            let dir = dir_for(root);
            out.choices
                .push(read_json::<Choices>(&dir.join("choices.json")).unwrap_or_default());
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut names: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("synced-") && n.ends_with(".json"))
                })
                .collect();
            names.sort();
            for p in names {
                let Some(f) = read_json::<SyncedFile>(&p) else {
                    continue;
                };
                for (key, e) in f.assets {
                    out.insert(i, key, e);
                }
            }
        }
        out
    }

    fn insert(&mut self, root: usize, key: String, e: Entry) {
        if self
            .entries
            .get(&key)
            .is_some_and(|(_, old)| old.updated > e.updated)
        {
            return;
        }
        self.by_local.insert(e.local_id.clone(), key.clone());
        self.entries.insert(key, (root, e));
    }

    /// The record for this asset, and the root it's in.
    pub fn find(&self, id: &AssetId) -> Option<(usize, &Entry)> {
        let key = id.key();
        let hit = self.entries.get(&key).or_else(|| {
            self.by_local
                .get(&id.local)
                .and_then(|k| self.entries.get(k))
        });
        hit.map(|(r, e)| (*r, e))
    }

    /// Record (in memory) what a sync just wrote, so the rest of the run
    /// sees it.
    pub fn record(&mut self, root: usize, id: &AssetId, e: Entry) {
        self.insert(root, id.key(), e);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every record: (key, root index, entry).
    pub fn iter(&self) -> impl Iterator<Item = (&str, usize, &Entry)> {
        self.entries.iter().map(|(k, (r, e))| (k.as_str(), *r, e))
    }
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Option<T> {
    let bytes = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("photos: can't read {}: {e}", p.display());
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("photos: ignoring unreadable {}: {e}", p.display());
            None
        }
    }
}

/// The file recording these assets for one step of a sync (`run`: the
/// sync's start, `n`: the step), as a write for its batch. The folder is
/// made here (inside `.shoebox`, as collections' is).
pub fn plan_synced(
    root: &Path,
    run: &str,
    n: usize,
    assets: &[(AssetId, Entry)],
) -> Result<(PathBuf, Vec<u8>)> {
    let body = SyncedFile {
        schema: 1,
        assets: assets.iter().map(|(id, e)| (id.key(), e.clone())).collect(),
    };
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|e| CoreError::Invalid(format!("photos record: {e}")))?;
    let dir = dir_for(root);
    std::fs::create_dir_all(&dir).map_err(io(&dir))?;
    Ok((dir.join(format!("synced-{run}-{n:05}.json")), bytes))
}

/// `choices.json` for a root, as a write.
pub fn plan_choices(root: &Path, choices: &Choices) -> Result<(PathBuf, Vec<u8>)> {
    let body = Choices {
        schema: 1,
        ..choices.clone()
    };
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|e| CoreError::Invalid(format!("photos choices: {e}")))?;
    let dir = dir_for(root);
    std::fs::create_dir_all(&dir).map_err(io(&dir))?;
    Ok((dir.join("choices.json"), bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(local: &str, updated: i64) -> Entry {
        Entry {
            local_id: local.into(),
            status: Status::Copied,
            files: vec![],
            albums: vec![],
            favourite: false,
            edit_modified: None,
            updated,
        }
    }

    fn id(cloud: Option<&str>, local: &str) -> AssetId {
        AssetId {
            cloud: cloud.map(Into::into),
            local: local.into(),
        }
    }

    #[test]
    fn records_from_every_root_latest_wins() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let one = id(Some("C1"), "L1");
        for (root, run, updated) in [(a.path(), "1", 1), (a.path(), "2", 5), (b.path(), "3", 3)] {
            let (p, bytes) =
                plan_synced(root, run, 1, &[(one.clone(), entry("L1", updated))]).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
        let (p, bytes) =
            plan_synced(b.path(), "3", 2, &[(id(None, "L2"), entry("L2", 1))]).unwrap();
        std::fs::write(p, bytes).unwrap();
        let l = Ledgers::load(&[a.path().into(), b.path().into()]);
        assert_eq!(l.len(), 2);
        let (root, e) = l.find(&one).unwrap();
        assert_eq!((root, e.updated), (0, 5));
        // An asset recorded before it had a cloud id is found by its local id.
        assert!(l.find(&id(Some("C2"), "L2")).is_some());
        assert!(l.find(&id(Some("C3"), "L3")).is_none());
    }

    #[test]
    fn an_undone_record_reads_as_nothing() {
        let a = tempfile::tempdir().unwrap();
        let (p, bytes) =
            plan_synced(a.path(), "1", 1, &[(id(Some("C1"), "L1"), entry("L1", 1))]).unwrap();
        std::fs::write(&p, bytes).unwrap();
        // fileops' undo of a file it created leaves `{}`.
        std::fs::write(&p, b"{}").unwrap();
        std::fs::write(dir_for(a.path()).join("synced-junk.json"), b"not json").unwrap();
        let l = Ledgers::load(&[a.path().into()]);
        assert!(l.is_empty());
    }

    #[test]
    fn choices_round_trip() {
        let a = tempfile::tempdir().unwrap();
        let mut c = Choices::default();
        c.albums.insert(
            "A1".into(),
            AlbumChoice {
                name: "Trips › Japan".into(),
                kind: AlbumUse::Event,
            },
        );
        c.change_token = Some("tok".into());
        let (p, bytes) = plan_choices(a.path(), &c).unwrap();
        std::fs::write(p, bytes).unwrap();
        let l = Ledgers::load(&[a.path().into()]);
        assert_eq!(l.choices[0].albums, c.albums);
        assert_eq!(l.choices[0].change_token.as_deref(), Some("tok"));
    }
}
