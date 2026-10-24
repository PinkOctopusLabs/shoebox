//! The sync record: which Photos assets have been copied into a library
//! root, so a later sync copies only new ones.
//!
//! It lives in the root (`<root>/.shoebox/photos/`), like events and
//! collections, so it survives a catalog rebuild. Each sync appends its
//! records, a step's assets at a time and one JSON line each, to files of
//! its own (`synced-<run>-<n>.jsonl`, a new one once one passes about
//! 1 MB), in the sync's batch: the journal keeps only what each step adds,
//! and undoing a sync cuts exactly its lines off, so the records from
//! before it are what's left. Syncs before these logs wrote a file per
//! step (`synced-<run>-<n>.json`); those are read as they were. When
//! several records have the same asset, the latest wins. `choices.json`
//! holds what each album becomes and the change token, rewritten each run.

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

impl File {
    /// Where it is, in the record's `root`.
    pub fn path(&self, root: &Path) -> PathBuf {
        let p = Path::new(&self.rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    }
}

/// A record as a line of a sync's log.
#[derive(Serialize)]
struct LineOut<'a> {
    key: &'a str,
    #[serde(flatten)]
    entry: &'a Entry,
}

#[derive(Deserialize)]
struct LineIn {
    key: String,
    #[serde(flatten)]
    entry: Entry,
}

/// A sync's record file grows to about this before the next is started.
pub const ROLL_AT: u64 = 1 << 20;

/// Which of a sync's record files in a root it's appending to, and how
/// long that is.
#[derive(Debug, Default, Clone, Copy)]
pub struct Segment {
    n: u32,
    len: u64,
}

/// A step's records before these logs (one file per step).
#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncedFile {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    assets: BTreeMap<String, Entry>,
}

/// A photo Shoebox sent to Photos (PHOTOS-SPEC, Push).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pushed {
    pub local_id: String,
    /// The library photo it came from, relative to the record's root.
    pub from: String,
    /// Which of its edits: `main`, or a virtual copy's id.
    pub copy: String,
    /// A new asset Shoebox made (else an edit of an asset that was there).
    pub created: bool,
    /// The edit sent (its preview key; None: unedited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_hash: Option<String>,
    /// When (ms since 1970); the latest record wins.
    pub pushed_at: i64,
    /// The batch that recorded it (undoing it undoes the push, and empties
    /// this record).
    pub batch: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PushedFile {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    pushed: BTreeMap<String, Pushed>,
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
    /// Its name in Photos at the last sync (folders and name, `›`-joined),
    /// to tell a rename in Photos from one in Shoebox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photos_name: Option<String>,
    /// The event it made (an event's id), which follows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
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
    /// Pushes by asset key: (root index, record).
    pushed: HashMap<String, (usize, Pushed)>,
}

impl Ledgers {
    /// Read the records of every root (unreadable files are skipped and
    /// logged; a file emptied by an undo reads as nothing).
    pub fn load(roots: &[PathBuf]) -> Self {
        Self::load_from(roots, false).0
    }

    /// [`Self::load`] without the record files that are in a cloud drive
    /// only (reading one waits for the drive to download it, which can take
    /// minutes): for work that can go without them (Find dates). Returns
    /// how many were left out.
    pub fn load_here(roots: &[PathBuf]) -> (Self, usize) {
        Self::load_from(roots, true)
    }

    fn load_from(roots: &[PathBuf], here_only: bool) -> (Self, usize) {
        let mut out = Ledgers::default();
        let mut skipped = 0;
        let away = |p: &Path| {
            here_only
                && std::fs::symlink_metadata(p).is_ok_and(|m| crate::cloud::is_cloud_only(p, &m))
        };
        for (i, root) in roots.iter().enumerate() {
            let dir = dir_for(root);
            let choices = dir.join("choices.json");
            out.choices.push(if away(&choices) {
                skipped += 1;
                Choices::default()
            } else {
                read_json::<Choices>(&choices).unwrap_or_default()
            });
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut names: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        (n.starts_with("synced-") || n.starts_with("pushed-"))
                            && n.ends_with(".json")
                            || n.starts_with("synced-") && n.ends_with(".jsonl")
                    })
                })
                .collect();
            names.sort();
            for p in names {
                if away(&p) {
                    skipped += 1;
                    continue;
                }
                let pushed = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("pushed-"));
                if pushed {
                    let Some(f) = read_json::<PushedFile>(&p) else {
                        continue;
                    };
                    for (key, e) in f.pushed {
                        let newer = out
                            .pushed
                            .get(&key)
                            .is_none_or(|(_, old)| old.pushed_at <= e.pushed_at);
                        if newer {
                            out.pushed.insert(key, (i, e));
                        }
                    }
                    continue;
                }
                if p.extension().is_some_and(|e| e == "jsonl") {
                    for line in read_lines(&p) {
                        out.insert(i, line.key, line.entry);
                    }
                    continue;
                }
                let Some(f) = read_json::<SyncedFile>(&p) else {
                    continue;
                };
                for (key, e) in f.assets {
                    out.insert(i, key, e);
                }
            }
        }
        (out, skipped)
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

    /// Every library file a sync copied or found, by its path: what it is
    /// and when it was recorded (ms). `roots` as given to [`Ledgers::load`].
    pub fn files(&self, roots: &[PathBuf]) -> HashMap<PathBuf, (ResourceKind, i64)> {
        let mut out = HashMap::new();
        for (r, e) in self.entries.values() {
            for f in &e.files {
                let p = Path::new(&f.rel);
                let path = if p.is_absolute() {
                    p.to_path_buf()
                } else if let Some(root) = roots.get(*r) {
                    root.join(p)
                } else {
                    continue;
                };
                out.insert(path, (f.kind, e.updated));
            }
        }
        out
    }

    /// The push record of an asset, by its id.
    pub fn pushed(&self, id: &AssetId) -> Option<(usize, &Pushed)> {
        let hit = self
            .pushed
            .get(&id.key())
            .or_else(|| self.pushed.values().find(|(_, p)| p.local_id == id.local));
        hit.map(|(r, p)| (*r, p))
    }

    /// The latest push of a library photo's edit: its asset key
    /// and record. `roots` as given to [`Ledgers::load`].
    pub fn pushed_from(
        &self,
        roots: &[PathBuf],
        photo: &Path,
        copy: &str,
    ) -> Option<(&str, &Pushed)> {
        self.pushed
            .iter()
            .filter(|(_, (r, p))| {
                p.copy == copy
                    && roots
                        .get(*r)
                        .is_some_and(|root| root.join(&p.from) == photo)
            })
            .max_by_key(|(_, (_, p))| p.pushed_at)
            .map(|(k, (_, p))| (k.as_str(), p))
    }

    /// The pushes a batch recorded: (asset key, root, record).
    pub fn pushes_of(&self, batch: i64) -> Vec<(String, usize, Pushed)> {
        self.pushed
            .iter()
            .filter(|(_, (_, p))| p.batch == batch)
            .map(|(k, (r, p))| (k.clone(), *r, p.clone()))
            .collect()
    }

    /// The asset a library file came from (copied by a sync), if any.
    pub fn asset_of(&self, roots: &[PathBuf], photo: &Path) -> Option<(AssetId, ResourceKind)> {
        for (key, (r, e)) in &self.entries {
            for f in &e.files {
                let p = Path::new(&f.rel);
                let path = if p.is_absolute() {
                    p.to_path_buf()
                } else if let Some(root) = roots.get(*r) {
                    root.join(p)
                } else {
                    continue;
                };
                if path == photo {
                    let cloud = (!key.starts_with("local:")).then(|| key.clone());
                    return Some((
                        AssetId {
                            cloud,
                            local: e.local_id.clone(),
                        },
                        f.kind,
                    ));
                }
            }
        }
        None
    }

    /// Record (in memory) a push just written.
    pub fn record_push(&mut self, root: usize, id: &AssetId, p: Pushed) {
        self.pushed.insert(id.key(), (root, p));
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

/// A log's records. A line that can't be read (cut short when the app
/// stopped, before its batch was resumed) is skipped.
fn read_lines(p: &Path) -> Vec<LineIn> {
    let bytes = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("photos: can't read {}: {e}", p.display());
            return Vec::new();
        }
    };
    let mut skipped = 0;
    let lines = bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_slice(l).map_err(|_| skipped += 1).ok())
        .collect();
    if skipped > 0 {
        tracing::warn!(
            "photos: ignoring {skipped} unreadable lines in {}",
            p.display()
        );
    }
    lines
}

/// The records of one step of a sync (`run`: the sync's start), as lines
/// to append to its log in this root, `segment` saying which (a new one
/// once it passes [`ROLL_AT`]). A write for its batch: fileops appends to
/// `.jsonl` files. The folder is made here (inside `.shoebox`, as
/// collections' is).
pub fn plan_synced(
    root: &Path,
    run: &str,
    segment: &mut Segment,
    assets: &[(AssetId, Entry)],
) -> Result<(PathBuf, Vec<u8>)> {
    let mut bytes = Vec::new();
    for (id, e) in assets {
        let key = id.key();
        serde_json::to_writer(
            &mut bytes,
            &LineOut {
                key: &key,
                entry: e,
            },
        )
        .map_err(|e| CoreError::Invalid(format!("photos record: {e}")))?;
        bytes.push(b'\n');
    }
    let len = bytes.len() as u64;
    if segment.n == 0 || segment.len > 0 && segment.len + len > ROLL_AT {
        segment.n += 1;
        segment.len = 0;
    }
    segment.len += len;
    let dir = dir_for(root);
    std::fs::create_dir_all(&dir).map_err(io(&dir))?;
    Ok((
        dir.join(format!("synced-{run}-{:03}.jsonl", segment.n)),
        bytes,
    ))
}

/// The file recording these pushes (`run`: when), as a write for the push's
/// batch.
pub fn plan_pushed(
    root: &Path,
    run: &str,
    pushes: &[(AssetId, Pushed)],
) -> Result<(PathBuf, Vec<u8>)> {
    let body = PushedFile {
        schema: 1,
        pushed: pushes.iter().map(|(id, p)| (id.key(), p.clone())).collect(),
    };
    let bytes = serde_json::to_vec_pretty(&body)
        .map_err(|e| CoreError::Invalid(format!("photos push record: {e}")))?;
    let dir = dir_for(root);
    std::fs::create_dir_all(&dir).map_err(io(&dir))?;
    Ok((dir.join(format!("pushed-{run}.json")), bytes))
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

    /// Append a step's records, as fileops does.
    fn put(root: &Path, run: &str, seg: &mut Segment, assets: &[(AssetId, Entry)]) -> PathBuf {
        let (p, bytes) = plan_synced(root, run, seg, assets).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&p)
            .and_then(|mut f| std::io::Write::write_all(&mut f, &bytes))
            .unwrap();
        p
    }

    #[test]
    fn records_from_every_root_latest_wins() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let one = id(Some("C1"), "L1");
        let mut b3 = Segment::default();
        for (root, run, updated) in [(a.path(), "1", 1), (a.path(), "2", 5)] {
            put(
                root,
                run,
                &mut Segment::default(),
                &[(one.clone(), entry("L1", updated))],
            );
        }
        put(b.path(), "3", &mut b3, &[(one.clone(), entry("L1", 3))]);
        put(b.path(), "3", &mut b3, &[(id(None, "L2"), entry("L2", 1))]);
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
        let p = put(
            a.path(),
            "1",
            &mut Segment::default(),
            &[(id(Some("C1"), "L1"), entry("L1", 1))],
        );
        // fileops' undo of the first append leaves the log empty, and of a
        // step file from before the logs, `{}`.
        std::fs::write(&p, b"").unwrap();
        let dir = dir_for(a.path());
        std::fs::write(dir.join("synced-0-00001.json"), b"{}").unwrap();
        std::fs::write(dir.join("synced-junk.json"), b"not json").unwrap();
        let l = Ledgers::load(&[a.path().into()]);
        assert!(l.is_empty());
    }

    /// Step files from before the logs are read with them; the latest
    /// record wins whichever kind it's in.
    #[test]
    fn old_step_files_are_read_with_the_logs() {
        let a = tempfile::tempdir().unwrap();
        let dir = dir_for(a.path());
        std::fs::create_dir_all(&dir).unwrap();
        let old = SyncedFile {
            schema: 1,
            assets: [
                (id(Some("C1"), "L1").key(), entry("L1", 1)),
                (id(Some("C2"), "L2").key(), entry("L2", 9)),
            ]
            .into_iter()
            .collect(),
        };
        std::fs::write(
            dir.join("synced-20261023T131748-00001.json"),
            serde_json::to_vec_pretty(&old).unwrap(),
        )
        .unwrap();
        put(
            a.path(),
            "20261024T090000",
            &mut Segment::default(),
            &[
                (id(Some("C1"), "L1"), entry("L1", 5)),
                (id(Some("C2"), "L2"), entry("L2", 2)),
            ],
        );
        let l = Ledgers::load(&[a.path().into()]);
        assert_eq!(l.len(), 2);
        assert_eq!(l.find(&id(Some("C1"), "L1")).unwrap().1.updated, 5);
        assert_eq!(l.find(&id(Some("C2"), "L2")).unwrap().1.updated, 9);
    }

    /// A sync's records go into one log until it passes about 1 MB, then
    /// the next; a line cut short is skipped.
    #[test]
    fn logs_roll_over_and_skip_a_cut_line() {
        let a = tempfile::tempdir().unwrap();
        let mut seg = Segment::default();
        let mut paths = Vec::new();
        let mut n = 0;
        while paths.len() < 3 {
            let step: Vec<_> = (0..20)
                .map(|_| {
                    n += 1;
                    let local = format!("L{n}");
                    (id(Some(&format!("C{n}")), &local), entry(&local, 1))
                })
                .collect();
            let p = put(a.path(), "1", &mut seg, &step);
            if paths.last() != Some(&p) {
                paths.push(p);
            }
        }
        let names: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "synced-1-001.jsonl",
                "synced-1-002.jsonl",
                "synced-1-003.jsonl"
            ]
        );
        let len = std::fs::metadata(&paths[0]).unwrap().len();
        assert!(len <= ROLL_AT && len > ROLL_AT - 20_000, "{len}");
        // The last line, cut short.
        let mut last = std::fs::read(&paths[2]).unwrap();
        last.truncate(last.len() - 10);
        std::fs::write(&paths[2], last).unwrap();
        let l = Ledgers::load(&[a.path().into()]);
        assert_eq!(l.len(), n - 1);
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
                photos_name: Some("Trips › Japan".into()),
                event: Some("e1".into()),
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
