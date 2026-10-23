//! Background work. The UI thread never touches the disk or decodes: it
//! states what it wants each frame, and worker threads deliver results over
//! a channel.
//!
//! - [`Loader`]: images (thumbnails and full decodes) by priority. Demand is
//!   replaced every frame, so work for thumbnails scrolled out of view is
//!   cancelled before it starts.
//! - [`Library`]: settings and catalog scans, one at a time. It also watches
//!   the roots, so changes made by other apps show up without a rescan.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use crossbeam_channel::{Receiver, Sender};
use shoebox_core::catalog::Catalog;
use shoebox_core::changes::By;
use shoebox_core::collections::Collection;
use shoebox_core::events::{self, Event};
use shoebox_core::fileops::{FileOps, Journal, PlannedMove};
use shoebox_core::marks::Marks;
use shoebox_core::review::{self, Decision, Review};
use shoebox_core::settings::Settings;
use shoebox_core::thumbs::{self, Source, ThumbCache};
use shoebox_decode::{Format, Rgb8};

/// Stable id for a photo: hash of its path (survives catalog refreshes).
pub type PhotoId = u64;

pub fn photo_id(path: &std::path::Path) -> PhotoId {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

/// The id of a photo's virtual copy's tile (its textures, selection and
/// place in the grid).
pub fn copy_id(photo: PhotoId, copy: &str) -> PhotoId {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (photo, "copy", copy).hash(&mut h);
    h.finish()
}

/// The id an edited photo's unedited image is kept under (the loupe's \).
pub fn original_id(id: PhotoId) -> PhotoId {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (id, "original").hash(&mut h);
    h.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Size {
    /// 256 px, cached on disk.
    Small,
    /// 1024 px, cached on disk.
    Large,
    /// Full resolution, not cached.
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub photo: PhotoId,
    pub size: Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// On screen now.
    Visible = 0,
    /// Likely next (loupe neighbours).
    Near = 1,
}

/// Everything a worker needs to load an image, owned.
#[derive(Debug, Clone)]
pub struct Job {
    pub path: PathBuf,
    pub format: Format,
    pub size: u64,
    pub mtime_ns: i64,
    pub orientation: u16,
    pub target: shoebox_decode::Target,
    /// The edited preview to show instead of the original (a JPEG made by
    /// Develop), if the photo has one.
    pub edited: Option<PathBuf>,
    /// In the cloud only and not to be downloaded: cached thumbnails only.
    pub cloud: bool,
    /// Keep thumbnails in the on-disk cache (not for import previews,
    /// whose sources aren't in the library).
    pub keep: bool,
}

/// A load's error for a cloud-only photo with no cached thumbnail.
pub const IN_CLOUD: &str = "in the cloud only";

pub struct Loaded {
    pub key: Key,
    pub result: Result<Rgb8, String>,
    /// R, G, B, luma histograms (Small thumbnails only, for the Info panel).
    pub histogram: Option<Box<[[u32; 256]; 4]>>,
}

#[derive(Default)]
struct QueueState {
    pending: HashMap<Key, (Priority, Job)>,
    in_flight: HashSet<Key>,
    shutdown: bool,
}

struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

pub struct Loader {
    queue: Arc<Queue>,
    results: Receiver<Loaded>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Loader {
    pub fn new(
        threads: usize,
        thumbs: ThumbCache,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let queue = Arc::new(Queue {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(repaint);
        let workers = (0..threads.max(1))
            .filter_map(|i| {
                let (q, tx, thumbs, repaint) =
                    (queue.clone(), tx.clone(), thumbs.clone(), repaint.clone());
                std::thread::Builder::new()
                    .name(format!("loader-{i}"))
                    .spawn(move || worker(&q, &tx, &thumbs, &*repaint))
                    .map_err(|e| tracing::error!("could not start loader thread: {e}"))
                    .ok()
            })
            .collect();
        Self {
            queue,
            results: rx,
            workers,
        }
    }

    /// Replace the set of wanted images. Anything pending that isn't wanted
    /// any more (scrolled away) is dropped; in-flight work completes.
    pub fn demand(&self, wanted: impl IntoIterator<Item = (Key, Priority, Job)>) {
        let Ok(mut s) = self.queue.state.lock() else {
            return;
        };
        s.pending.clear();
        for (k, p, j) in wanted {
            if s.in_flight.contains(&k) {
                continue;
            }
            let e = s.pending.entry(k).or_insert((p, j));
            e.0 = e.0.min(p);
        }
        if !s.pending.is_empty() {
            self.queue.ready.notify_all();
        }
    }

    pub fn drain(&self) -> Vec<Loaded> {
        self.results.try_iter().collect()
    }

    /// Images queued or being loaded.
    pub fn busy(&self) -> usize {
        self.queue
            .state
            .lock()
            .map_or(0, |s| s.pending.len() + s.in_flight.len())
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        if let Ok(mut s) = self.queue.state.lock() {
            s.shutdown = true;
            s.pending.clear();
        }
        self.queue.ready.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn next_job(q: &Queue) -> Option<(Key, Job)> {
    let mut s = q.state.lock().ok()?;
    loop {
        if s.shutdown {
            return None;
        }
        let best = s
            .pending
            .iter()
            .min_by_key(|(k, (p, _))| (*p, k.size))
            .map(|(k, _)| *k);
        if let Some(k) = best
            && let Some((_, job)) = s.pending.remove(&k)
        {
            s.in_flight.insert(k);
            return Some((k, job));
        }
        s = q.ready.wait(s).ok()?;
    }
}

fn worker(q: &Queue, tx: &Sender<Loaded>, thumbs: &ThumbCache, repaint: &(dyn Fn() + Send + Sync)) {
    while let Some((key, job)) = next_job(q) {
        // An edited photo shows its preview (the original if that fails).
        if let Some(preview) = &job.edited {
            let px = match key.size {
                Size::Small => thumbs::SMALL,
                Size::Large => thumbs::LARGE,
                Size::Full => 1 << 14,
            };
            if let Ok(img) =
                shoebox_decode::render_path(Format::Jpeg, preview, Some(px), 1, job.target)
            {
                let histogram = (key.size == Size::Small).then(|| Box::new(img.histogram()));
                let _ = tx.send(Loaded {
                    key,
                    result: Ok(img),
                    histogram,
                });
                if let Ok(mut s) = q.state.lock() {
                    s.in_flight.remove(&key);
                }
                repaint();
                continue;
            }
        }
        let src = Source {
            path: &job.path,
            format: job.format,
            size: job.size,
            mtime_ns: job.mtime_ns,
            orientation: job.orientation,
            target: job.target,
        };
        // A photo in the cloud only that's to be read (opened): download it
        // first, waiting out the drive's timeouts, rather than having the
        // decoder's read time out part-way.
        let fetched = if job.cloud {
            Ok(())
        } else {
            shoebox_core::cloud::download(&job.path)
                .map_err(|e| format!("{}: downloading from the cloud: {e}", job.path.display()))
        };
        let result = match key.size {
            _ if fetched.is_err() => Err(fetched.err().unwrap_or_default()),
            // Reading the file would download it.
            _ if job.cloud => match key.size {
                Size::Small => thumbs.cached(&src, thumbs::SMALL),
                Size::Large => thumbs.cached(&src, thumbs::LARGE),
                Size::Full => None,
            }
            .ok_or_else(|| IN_CLOUD.to_owned()),
            Size::Small | Size::Large if !job.keep => shoebox_decode::render_path(
                job.format,
                &job.path,
                Some(if key.size == Size::Small {
                    thumbs::SMALL
                } else {
                    thumbs::LARGE
                }),
                job.orientation,
                job.target,
            )
            .map_err(|e| format!("{}: {e}", job.path.display())),
            Size::Small => thumbs.get(&src, thumbs::SMALL).map_err(|e| e.to_string()),
            Size::Large => thumbs.get(&src, thumbs::LARGE).map_err(|e| e.to_string()),
            Size::Full => shoebox_decode::render_path(
                job.format,
                &job.path,
                None,
                job.orientation,
                job.target,
            )
            .map_err(|e| format!("{}: {e}", job.path.display())),
        };
        let histogram = match (&result, key.size) {
            (Ok(img), Size::Small) if job.keep => Some(Box::new(img.histogram())),
            _ => None,
        };
        let _ = tx.send(Loaded {
            key,
            result,
            histogram,
        });
        if let Ok(mut s) = q.state.lock() {
            s.in_flight.remove(&key);
        }
        repaint();
    }
}

// ---------------------------------------------------------------------------

pub enum LibraryCommand {
    /// Load settings, show the cached catalog, then rescan from disk.
    Open,
    Rescan,
    AddRoot(PathBuf),
    RemoveRoot(PathBuf),
    SetReference(PathBuf, bool),
    /// Write marks to sidecars as one undoable batch.
    SetMarks {
        updates: Vec<(PathBuf, Marks)>,
        label: String,
    },
    /// Replace the events and, optionally, add photos to one or take them
    /// out: writes `.shoebox/events.json` in every writable root and the
    /// affected sidecars (where membership lives), as one undoable batch.
    SaveEvents {
        events: Vec<Event>,
        change: Option<events::EventChange>,
        label: String,
    },
    Undo,
    Redo,
    /// A newer action elsewhere (People's names): nothing to redo here.
    ClearRedo,
    /// Replace settings other than roots (options, smart filters).
    SaveSettings(Box<Settings>),
    /// Clear the unseen changes of these photos.
    MarkSeen(Vec<PathBuf>),
    /// Work out an import (reads and hashes the source; nothing is copied).
    PlanImport {
        source: PathBuf,
        root: PathBuf,
        template: String,
    },
    /// Plan the last import again (without reading the source again),
    /// copying these left-out source files anyway.
    ReplanImport(std::collections::HashSet<PathBuf>),
    /// Run the last planned import: verified copies as one batch, with the
    /// folders' names put on the photos as chosen.
    RunImport(Vec<shoebox_core::import::Folder>),
    /// Work out a move, reject, rename or trash (the dry-run table).
    PreviewOrganise(OrgRequest),
    /// Run it as one batch (references to moved files updated with it).
    RunOrganise(OrgRequest),
    /// Create an empty folder (journaled).
    NewFolder(PathBuf),
    /// Replace the collections (written to every writable root).
    SaveCollections {
        list: Vec<Collection>,
        label: String,
    },
    /// Send the journal's batches (for the History panel).
    LoadHistory,
    /// The operations of one batch.
    LoadBatch(i64),
    /// Undo any batch still consistent with the disk (History panel).
    UndoBatch(i64),
    /// Finish an interrupted batch.
    ResumeBatch(i64),
    /// Eject the card or disk mounted here (after importing from it).
    Eject(PathBuf),
    /// Find duplicate groups across the library (hashing what isn't cached).
    /// Find duplicates, among just these files when given (a scope).
    FindDuplicates(shoebox_core::dupes::Config, Option<Arc<HashSet<PathBuf>>>),
    /// Work out what applying these decisions would do (the dry-run table).
    PreviewDuplicates(HashMap<PathBuf, Decision>),
    /// Apply them: moves to `_duplicates/` and stack links as one undoable
    /// batch, and remember dismissed pairs.
    ApplyDuplicates(HashMap<PathBuf, Decision>),
    /// Download cloud-only files (by reading them; nothing is written) and
    /// read them as photos.
    Download(Vec<PathBuf>),
    /// A photo's edit (Develop): the catalog's working copy, or its
    /// `.shoebox/edits` file when that's newer. Answered by `EditLoaded`.
    LoadEdit {
        root: PathBuf,
        rel: String,
        copy: String,
    },
    /// Keep a changed edit in the catalog; `SaveEdits` writes it to its
    /// file. The document's source hash and size are filled in if missing.
    PutEdit {
        root: PathBuf,
        rel: String,
        copy: String,
        json: String,
    },
    /// Move a virtual copy's edit file to the Trash (journaled) and forget
    /// it.
    RemoveCopy {
        root: PathBuf,
        rel: String,
        copy: String,
    },
    /// Write every edit changed since its last save to its `.shoebox` file.
    SaveEdits,
    /// Apply a preset (or pasted settings, or a matched exposure) to each
    /// of these photos' edits, as one step in each one's history, and
    /// write them as one batch.
    ApplyPreset {
        /// (root, rel, copy, preset): a virtual copy's tile pastes onto
        /// that copy.
        photos: Vec<(PathBuf, String, String, shoebox_develop::preset::Preset)>,
        label: String,
    },
    /// Remember files just exported.
    RecordExports(Vec<shoebox_core::cache::ExportRow>),
    /// A photo's exports (answered by `Exports`).
    LoadExports(PathBuf),
    /// Whether a file came from iCloud Photos (answered by `FromPhotos`).
    LoadFromPhotos(PathBuf),
    /// Copy exports rendered into the cache into the library beside their
    /// photos and join each to its photo's stack, as one journaled batch
    /// (replacing only the earlier exports they redo, which go to the
    /// Trash), then remove them from the cache.
    AddExports(Vec<crate::develop::LibraryExport>),
    /// Add a photo merge written in the cache to the library beside its
    /// first source, with its record, as one journaled batch (rule 3), then
    /// remove it from the cache. Answered by `Merged`.
    AddMerge(Box<crate::merge::Done>),
    /// A step of an iCloud Photos sync (`crate::photos`), which runs on its
    /// own thread: the batch's begin, its steps and its end, each answered
    /// on the channel (the batch id, or why it failed).
    PhotosBatch(
        PhotosOp,
        crossbeam_channel::Sender<Result<i64, shoebox_core::CoreError>>,
    ),
    /// A send to Photos was undone there: now undo its batch (its records).
    UndoPushed(i64),
}

/// A photo's last send to Photos (its main edit's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentToPhotos {
    /// When (ms since 1970).
    pub at: i64,
    /// The edit sent (None: unedited).
    pub render_hash: Option<String>,
}

/// What an iCloud Photos sync asks of the library worker.
#[derive(Debug)]
pub enum PhotosOp {
    Begin(String),
    /// A send to Photos' batch.
    BeginPush(String),
    Step {
        batch: i64,
        copies: Vec<shoebox_core::fileops::PlannedCopy>,
        writes: Vec<(PathBuf, Vec<u8>)>,
    },
    End(i64),
}

/// A move, reject, rename or trash of stacks, by the paths of their files.
#[derive(Debug, Clone, PartialEq)]
pub enum OrgRequest {
    MoveTo {
        photos: Vec<PathBuf>,
        folder: PathBuf,
    },
    Reject {
        photos: Vec<PathBuf>,
    },
    Rename {
        photos: Vec<PathBuf>,
        template: String,
    },
    Trash {
        photos: Vec<PathBuf>,
    },
}

impl OrgRequest {
    pub fn photos(&self) -> &[PathBuf] {
        match self {
            OrgRequest::MoveTo { photos, .. }
            | OrgRequest::Reject { photos }
            | OrgRequest::Rename { photos, .. }
            | OrgRequest::Trash { photos } => photos,
        }
    }

    /// "Move", "Rename", "Move to Trash".
    pub fn verb(&self) -> &'static str {
        match self {
            OrgRequest::MoveTo { .. } => "Move",
            OrgRequest::Reject { .. } => "Move to _rejected",
            OrgRequest::Rename { .. } => "Rename",
            OrgRequest::Trash { .. } => "Move to Trash",
        }
    }

    fn label(&self) -> String {
        match self {
            OrgRequest::MoveTo { folder, .. } => format!("move to {}", folder.display()),
            OrgRequest::Reject { .. } => "move rejects to _rejected".to_owned(),
            OrgRequest::Rename { template, .. } => format!("rename with {template}"),
            OrgRequest::Trash { .. } => "move to Trash".to_owned(),
        }
    }
}

/// The dry-run table for an organise request.
#[derive(Debug, Clone)]
pub struct OrgPreview {
    pub request: OrgRequest,
    /// Source → destination (for the Trash, destination = source).
    pub moves: Vec<PlannedMove>,
    /// Things left alone, and why.
    pub notes: Vec<String>,
}

/// One batch for the History panel.
pub type BatchRow = shoebox_core::fileops::BatchSummary;

/// The dry-run table for applying duplicate decisions.
#[derive(Debug, Clone, Default)]
pub struct DupePreview {
    /// Every move, sidecars included, with final (suffixed) destinations.
    pub moves: Vec<PlannedMove>,
    pub variants: usize,
    pub ignores: usize,
}

/// A catalog and what the UI derives from it, prepared on the library worker
/// so that swapping in a 200,000-file catalog (after every rating, too)
/// doesn't stall a frame.
pub struct Published {
    pub catalog: Arc<Catalog>,
    pub ids: Vec<PhotoId>,
    pub index_of: HashMap<PhotoId, usize>,
    pub tree: Vec<crate::app::FolderNode>,
    pub keywords: Vec<(String, usize)>,
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    /// Photos whose file changed since the last publish (they reload).
    pub stale: HashSet<PhotoId>,
}

/// Builds [`Published`]s, remembering each photo's size and mtime to spot
/// files that changed between publishes.
#[derive(Default)]
struct Publisher {
    last: HashMap<PhotoId, (u64, i64)>,
}

impl Publisher {
    fn prepare(&mut self, catalog: &Catalog) -> Published {
        let ids: Vec<PhotoId> = catalog.photos.iter().map(|p| photo_id(&p.path)).collect();
        let stale = ids
            .iter()
            .zip(&catalog.photos)
            .filter(|(id, p)| {
                self.last
                    .get(id)
                    .is_some_and(|&b| b != (p.size, p.mtime_ns))
            })
            .map(|(&id, _)| id)
            .collect();
        self.last = ids
            .iter()
            .zip(&catalog.photos)
            .map(|(&id, p)| (id, (p.size, p.mtime_ns)))
            .collect();
        Published {
            index_of: ids.iter().enumerate().map(|(i, &id)| (id, i)).collect(),
            ids,
            stale,
            tree: crate::app::build_tree(catalog),
            keywords: shoebox_core::query::keywords(catalog),
            cameras: shoebox_core::query::distinct(catalog, |x| x.model.as_ref()),
            lenses: shoebox_core::query::distinct(catalog, |x| x.lens.as_ref()),
            catalog: Arc::new(catalog.clone()),
        }
    }
}

pub enum LibraryEvent {
    Catalog {
        published: Box<Published>,
        settings: Settings,
        /// False while a refresh from disk is still running.
        complete: bool,
    },
    Progress {
        done: usize,
        total: usize,
    },
    /// The events (after loading, saving or undo).
    Events(Vec<Event>),
    /// Unseen changes after Shoebox wrote these photos' sidecars (lighter
    /// than republishing the catalog on every keystroke).
    Changes(Vec<(PathBuf, Option<shoebox_core::changes::Change>)>),
    ImportProgress {
        done: usize,
        total: usize,
    },
    /// Downloading from the cloud; `done == total` when finished.
    Downloading {
        done: usize,
        total: usize,
    },
    /// The import plan (or why it couldn't be made; `None`: cancelled).
    ImportPlanned(Option<Result<Arc<shoebox_core::import::Plan>, String>>),
    /// The import planned again with files forced in (folders unchanged).
    ImportReplanned(Arc<shoebox_core::import::Plan>),
    /// Copying: `done` of `files` copies, `bytes` of `total_bytes`, and the
    /// file being copied.
    ImportCopying {
        done: usize,
        files: usize,
        bytes: u64,
        total_bytes: u64,
        current: Option<PathBuf>,
    },
    /// An import's copying is over, however it ended (after `Imported`,
    /// `ImportCancelled` or an error).
    ImportEnded,
    /// A card or disk was ejected (or why it couldn't be).
    Ejected(Result<String, String>),
    /// The import was cancelled part-way: `copied` files stay, as `batch`.
    ImportCancelled {
        batch: i64,
        copied: usize,
    },
    /// Import done as this batch.
    Imported {
        batch: i64,
        copied: usize,
        /// Photos given a folder's name.
        named: usize,
    },
    /// The dry-run table for an organise request (or why it's refused).
    OrgPreview(Result<OrgPreview, String>),
    /// An organise request ran as this batch.
    Organised {
        batch: i64,
        files: usize,
        verb: &'static str,
    },
    Collections(Vec<Collection>),
    /// Something for the status bar.
    Notice(String),
    /// A photo merge was added to the library as this file.
    Merged(PathBuf),
    /// Every journal batch, oldest first.
    Batches(Vec<BatchRow>),
    /// One batch's operations: (op, source, destination, done).
    BatchOps(i64, Vec<(String, PathBuf, PathBuf, bool)>),
    /// A History undo finished (Ok: the undo batch).
    BatchUndone(Result<i64, String>),
    /// A resume finished (Ok: steps run).
    BatchResumed(Result<usize, String>),
    /// Duplicate groups found (also after applying decisions, or undo).
    Duplicates(Arc<Review>),
    DupeProgress {
        done: usize,
        total: usize,
    },
    /// The dry-run table, or why the decisions can't be applied.
    DupePreview(Result<DupePreview, String>),
    /// Decisions applied as this batch (None if nothing moved).
    DupesApplied {
        batch: Option<i64>,
        moved: usize,
    },
    /// Undo/redo changed: when the step ⌘Z would undo was made, and when
    /// the step ⌘⇧Z would redo was undone (ms since the epoch), so the app
    /// can interleave these with People's own undo; `fresh` when a new
    /// action cleared redo.
    History {
        undo_at: Option<i64>,
        redo_at: Option<i64>,
        fresh: bool,
    },
    /// Photos' edits, for their previews elsewhere: every one after the
    /// library opens, then each as it changes.
    Edits(Vec<EditInfo>),
    /// A photo's exports, newest first, each with whether its file is still
    /// there.
    Exports(PathBuf, Vec<(shoebox_core::cache::ExportRow, bool)>),
    /// Whether a file came from iCloud Photos: what it was there and when
    /// it was synced (ms).
    FromPhotos(
        PathBuf,
        Option<(shoebox_core::photos::ResourceKind, i64)>,
        Option<SentToPhotos>,
    ),
    /// Undo asked for a send to Photos: the Photos thread undoes it there
    /// first, then sends `UndoPushed`.
    UndoPush(i64),
    /// Edit files whose photo wasn't found after a scan (kept, never
    /// deleted; listed in Settings).
    Unattached(Vec<PathBuf>),
    /// A photo's edit, if it has one (answer to `LoadEdit`).
    /// These photos' virtual copies (each list replaces the photo's).
    Copies(Vec<(PathBuf, Vec<CopyInfo>)>),
    EditLoaded {
        /// Which copy (`edits::MAIN`, or a virtual copy's id).
        copy: String,
        /// The photo's copies: (id, name), the master first.
        copies: Vec<(String, String)>,
        root: PathBuf,
        rel: String,
        json: Option<String>,
    },
    Error(String),
}

/// A photo's edit, as the rest of the app sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct EditInfo {
    pub path: PathBuf,
    /// Names its rendered preview (the photo's content and the edit's
    /// render hash); None when the edit changes nothing.
    pub key: Option<String>,
    pub json: String,
}

/// A photo's virtual copy, as the library shows it (its own tile when the
/// photo's stack is open).
#[derive(Debug, Clone, PartialEq)]
pub struct CopyInfo {
    pub id: String,
    pub name: String,
    /// Names its rendered preview, as [`EditInfo::key`].
    pub key: Option<String>,
    pub json: String,
}

/// Bumped when previews must be rendered again (a new process version).
const PREVIEW_VERSION: &str = "1";

/// What names an edit's rendered preview; None when it changes nothing.
fn preview_key(doc: &shoebox_develop::edit::EditDoc) -> Option<String> {
    (!doc.settings().is_default()).then(|| {
        let id = format!(
            "{}:{}:{PREVIEW_VERSION}",
            doc.source.blake3,
            doc.render_hash()
        );
        blake3::hash(id.as_bytes()).to_hex()[..32].to_owned()
    })
}

fn copy_info(copy: &str, json: String) -> Option<CopyInfo> {
    let doc = shoebox_develop::edit::EditDoc::from_json(&json).ok()?;
    Some(CopyInfo {
        id: copy.to_owned(),
        name: if doc.copy.name.is_empty() {
            copy.to_owned()
        } else {
            doc.copy.name.clone()
        },
        key: preview_key(&doc),
        json,
    })
}

/// Copies in the order they were made (copy-2 before copy-10).
fn sort_copies(list: &mut [CopyInfo]) {
    list.sort_by(|a, b| (a.id.len(), &a.id).cmp(&(b.id.len(), &b.id)));
}

fn edit_info(root: &Path, rel: &str, json: String) -> Option<EditInfo> {
    let doc = shoebox_develop::edit::EditDoc::from_json(&json).ok()?;
    let key = preview_key(&doc);
    Some(EditInfo {
        path: root.join(rel),
        key,
        json,
    })
}

pub struct Library {
    commands: Sender<LibraryCommand>,
    pub events: Receiver<LibraryEvent>,
    /// Set to stop an import's planning or copying (the worker is busy with
    /// it, so this can't wait in the command queue).
    import_stop: Arc<std::sync::atomic::AtomicBool>,
}

pub struct LibraryPaths {
    pub settings: PathBuf,
    pub cache_db: PathBuf,
    pub journal: PathBuf,
    /// Send trashed files here instead of the system Trash (tests).
    pub trash_dir: Option<PathBuf>,
}

impl Library {
    pub fn new(paths: LibraryPaths, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (ctx_tx, ctx_rx) = crossbeam_channel::unbounded::<LibraryCommand>();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let import_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = import_stop.clone();
        let spawned = std::thread::Builder::new()
            .name("library".into())
            .spawn(move || library_worker(&paths, &ctx_rx, &ev_tx, &repaint, &stop));
        if let Err(e) = spawned {
            tracing::error!("could not start library thread: {e}");
        }
        Self {
            commands: ctx_tx,
            events: ev_rx,
            import_stop,
        }
    }

    /// A sender for threads that send commands themselves (an iCloud
    /// Photos sync).
    pub fn sender(&self) -> crossbeam_channel::Sender<LibraryCommand> {
        self.commands.clone()
    }

    pub fn send(&self, c: LibraryCommand) {
        // A new plan or import starts uncancelled.
        if matches!(
            c,
            LibraryCommand::PlanImport { .. } | LibraryCommand::RunImport(_)
        ) {
            self.import_stop
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
        let _ = self.commands.send(c);
    }

    /// Stop the import being planned or copied (what's copied stays, as a
    /// batch that can be undone).
    pub fn cancel_import(&self) {
        self.import_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether `batch` is a send to Photos (undone there first).
fn is_push(fileops: &FileOps, batch: i64) -> bool {
    fileops
        .journal()
        .batch(batch)
        .ok()
        .flatten()
        .is_some_and(|b| b.kind == shoebox_core::fileops::PUSH_KIND)
}

/// Sends `ImportEnded` when dropped.
struct Ended<'a>(&'a dyn Fn(LibraryEvent));

impl Drop for Ended<'_> {
    fn drop(&mut self) {
        (self.0)(LibraryEvent::ImportEnded);
    }
}

/// Eject the card or disk mounted at `volume` (the system's own eject, which
/// unmounts it first). Returns its name.
fn eject(volume: &Path) -> Result<String, String> {
    let name = volume.file_name().map_or_else(
        || volume.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    #[cfg(target_os = "macos")]
    let out = std::process::Command::new("/usr/sbin/diskutil")
        .arg("eject")
        .arg(volume)
        .output();
    #[cfg(not(target_os = "macos"))]
    let out = std::process::Command::new("gio")
        .args(["mount", "--eject"])
        .arg(volume)
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(name),
        Ok(o) => Err(format!(
            "{name} couldn't be ejected: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!("{name} couldn't be ejected: {e}")),
    }
}

fn reference_roots(settings: &Settings) -> Vec<PathBuf> {
    settings
        .roots
        .iter()
        .filter(|r| r.reference)
        .map(|r| r.path.clone())
        .collect()
}

/// Set while the library worker handles a command from the app (any of which
/// may move or write files), so an app update never relaunches mid-way.
static WORKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the library worker is handling a command right now.
pub fn working() -> bool {
    WORKING.load(std::sync::atomic::Ordering::Acquire)
}

struct Working;

impl Working {
    fn start() -> Self {
        WORKING.store(true, std::sync::atomic::Ordering::Release);
        Self
    }
}

impl Drop for Working {
    fn drop(&mut self) {
        WORKING.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn library_worker(
    paths: &LibraryPaths,
    commands: &Receiver<LibraryCommand>,
    events: &Sender<LibraryEvent>,
    repaint: &(dyn Fn() + Send + Sync),
    import_stop: &std::sync::atomic::AtomicBool,
) {
    let emit = |e: LibraryEvent| {
        let _ = events.send(e);
        repaint();
    };
    let mut cache = match shoebox_core::cache::Cache::open(&paths.cache_db) {
        Ok(c) => c,
        Err(e) => {
            emit(LibraryEvent::Error(format!("opening cache: {e}")));
            return;
        }
    };
    let mut settings = Settings::load(&paths.settings).unwrap_or_else(|e| {
        emit(LibraryEvent::Error(format!("settings: {e}")));
        Settings::default()
    });
    let mut fileops = match Journal::open(&paths.journal) {
        Ok(j) => {
            let fo = FileOps::new(j, reference_roots(&settings));
            match &paths.trash_dir {
                Some(d) => fo.with_trash(shoebox_core::fileops::TrashMode::Folder(d.clone())),
                None => fo,
            }
        }
        Err(e) => {
            emit(LibraryEvent::Error(format!("opening journal: {e}")));
            return;
        }
    };
    let mut catalog = Catalog::default();
    // The last duplicate search, kept so decisions can be applied to it.
    let mut dupes: Option<Review> = None;
    // The files the last find was limited to (found again after applying).
    let mut dupes_only: Option<Arc<HashSet<PathBuf>>> = None;
    // The last import plan, run as planned when confirmed, and the scan it
    // came from (with where to and how named), for planning again.
    let mut import_plan: Option<shoebox_core::import::Plan> = None;
    // Library files iCloud Photos syncs recorded, read when first asked
    // for and dropped when a command could change the records.
    #[allow(clippy::type_complexity)]
    let mut from_photos: Option<(
        HashMap<PathBuf, (shoebox_core::photos::ResourceKind, i64)>,
        shoebox_core::photos::ledger::Ledgers,
    )> = None;
    let mut import_scan: Option<(
        shoebox_core::import::Scan,
        PathBuf,
        shoebox_core::template::Template,
    )> = None;
    let batches = |fileops: &FileOps| {
        if let Ok(b) = fileops.journal().batches() {
            // Edit saves are undone in Develop's own history, not here.
            emit(LibraryEvent::Batches(
                b.into_iter().filter(|b| b.kind != "edit").collect(),
            ));
        }
    };
    batches(&fileops);
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    // Batches made in this session, for Cmd+Z / Cmd+Shift+Z.
    let (mut undo, mut redo): (Vec<i64>, Vec<i64>) = (Vec::new(), Vec::new());

    let publisher = std::cell::RefCell::new(Publisher::default());
    let publish = |catalog: &Catalog, settings: &Settings, complete: bool| {
        emit(LibraryEvent::Catalog {
            published: Box::new(publisher.borrow_mut().prepare(catalog)),
            settings: settings.clone(),
            complete,
        });
    };
    // When each batch went onto the undo or redo stack (it's the top when
    // `history` runs right after, so "now" is close enough).
    let pushed_at = std::cell::RefCell::new(HashMap::<i64, i64>::new());
    let fresh = std::cell::Cell::new(false);
    let history = |undo: &[i64], redo: &[i64]| {
        let mut at = pushed_at.borrow_mut();
        let mut time =
            |b: Option<&i64>| b.map(|&b| *at.entry(b).or_insert_with(shoebox_core::events::now_ms));
        emit(LibraryEvent::History {
            undo_at: time(undo.last()),
            redo_at: time(redo.last()),
            fresh: fresh.replace(false),
        });
    };

    // Changed paths from the file watcher, re-created when the roots change.
    let (fs_tx, fs_rx) = crossbeam_channel::unbounded::<Vec<PathBuf>>();
    let roots_of =
        |s: &Settings| -> Vec<PathBuf> { s.roots.iter().map(|r| r.path.clone()).collect() };
    let mut watched = roots_of(&settings);
    let mut watcher = crate::watch::watch(&watched, fs_tx.clone());
    let reindex = |catalog: &Catalog| -> HashMap<PathBuf, usize> {
        catalog
            .photos
            .iter()
            .enumerate()
            .map(|(i, p)| (p.path.clone(), i))
            .collect()
    };

    loop {
        let cmd = crossbeam_channel::select! {
            recv(commands) -> c => match c {
                Ok(c) => Command::App(c),
                Err(_) => break, // the app is gone
            },
            recv(fs_rx) -> p => match p {
                Ok(paths) => Command::Changed(paths),
                Err(_) => continue,
            },
        };
        let cmd = match cmd {
            Command::App(c) => c,
            Command::Changed(paths) => {
                // Made elsewhere (Shoebox's own writes already updated the
                // catalog, so re-reading finds nothing new for them).
                let a = catalog.affected(&paths);
                if a.events {
                    let roots: Vec<PathBuf> =
                        settings.roots.iter().map(|r| r.path.clone()).collect();
                    emit(LibraryEvent::Events(events::load(&roots)));
                }
                let mut rescan = a.rescan;
                if !a.photos.is_empty() {
                    match catalog.reread(&a.photos, &By::Other, &mut cache) {
                        Ok(r) => {
                            rescan |= r.missing;
                            if !r.changed.is_empty() {
                                tracing::info!("{} photos changed on disk", r.changed.len());
                                index = reindex(&catalog);
                                publish(&catalog, &settings, true);
                            }
                        }
                        Err(e) => emit(LibraryEvent::Error(format!("re-reading: {e}"))),
                    }
                }
                if rescan {
                    refresh_impl(
                        &mut cache,
                        &settings,
                        &mut catalog,
                        &mut index,
                        &publish,
                        &emit,
                    );
                }
                continue;
            }
        };
        let _working = Working::start();
        if !matches!(
            cmd,
            LibraryCommand::LoadFromPhotos(_)
                | LibraryCommand::LoadExports(_)
                | LibraryCommand::SetMarks { .. }
                | LibraryCommand::MarkSeen(_)
                | LibraryCommand::PutEdit { .. }
                | LibraryCommand::LoadEdit { .. }
        ) {
            from_photos = None;
        }
        let (settings_changed, rescan) = match cmd {
            LibraryCommand::Open => {
                // Edits kept but not yet written (the app quit first).
                if let Err(e) = shoebox_core::edits::save_pending(&cache, &fileops) {
                    tracing::warn!("saving edits left from last time: {e}");
                }
                if let Ok(c) = Catalog::from_cache(&cache, settings.scan_roots()) {
                    catalog = c;
                    index = catalog
                        .photos
                        .iter()
                        .enumerate()
                        .map(|(i, p)| (p.path.clone(), i))
                        .collect();
                    publish(&catalog, &settings, false);
                }
                match cache.edits() {
                    Ok(rows) => {
                        let mut copies: HashMap<PathBuf, Vec<CopyInfo>> = HashMap::new();
                        for r in rows.iter().filter(|r| r.2 != shoebox_core::edits::MAIN) {
                            if let Some(c) = copy_info(&r.2, r.3.clone()) {
                                copies
                                    .entry(Path::new(&r.0).join(&r.1))
                                    .or_default()
                                    .push(c);
                            }
                        }
                        for list in copies.values_mut() {
                            sort_copies(list);
                        }
                        emit(LibraryEvent::Copies(copies.into_iter().collect()));
                        emit(LibraryEvent::Edits(
                            rows.into_iter()
                                .filter(|r| r.2 == shoebox_core::edits::MAIN)
                                .filter_map(|(root, rel, _, json)| {
                                    edit_info(Path::new(&root), &rel, json)
                                })
                                .collect(),
                        ));
                    }
                    Err(e) => tracing::warn!("listing edits: {e}"),
                }
                (false, true)
            }
            LibraryCommand::Rescan => (false, true),
            LibraryCommand::AddRoot(p) => match settings.add_root(&p) {
                Ok(_) => (true, true),
                Err(e) => {
                    emit(LibraryEvent::Error(e.to_string()));
                    continue;
                }
            },
            LibraryCommand::RemoveRoot(p) => {
                settings.remove_root(&p);
                (true, true)
            }
            LibraryCommand::SetReference(p, r) => {
                settings.set_reference(&p, r);
                (true, true)
            }
            LibraryCommand::SaveSettings(new) => {
                settings = Settings {
                    roots: settings.roots.clone(),
                    ..*new
                };
                (true, false)
            }
            LibraryCommand::SetMarks { updates, label } => {
                let resolved: Vec<(usize, Marks)> = updates
                    .into_iter()
                    .filter_map(|(path, m)| index.get(&path).map(|&i| (i, m)))
                    .collect();
                match catalog.set_marks(
                    &resolved,
                    Vec::new(),
                    &fileops,
                    &cache,
                    settings.write_stem_sidecar_for_raw,
                    &label,
                ) {
                    Ok(Some(batch)) => {
                        undo.push(batch);
                        redo.clear();
                        fresh.set(true);
                        history(&undo, &redo);
                        emit(LibraryEvent::Changes(
                            resolved
                                .iter()
                                .map(|&(i, _)| {
                                    let p = &catalog.photos[i];
                                    (p.path.clone(), p.change.clone())
                                })
                                .collect(),
                        ));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        emit(LibraryEvent::Error(format!("saving marks: {e}")));
                        publish(&catalog, &settings, true); // revert the optimistic display
                    }
                }
                (false, false)
            }
            LibraryCommand::SaveEvents {
                events: new,
                change,
                label,
            } => {
                let writable: Vec<PathBuf> = settings
                    .roots
                    .iter()
                    .filter(|r| !r.reference)
                    .map(|r| r.path.clone())
                    .collect();
                let all: Vec<PathBuf> = settings.roots.iter().map(|r| r.path.clone()).collect();
                let old = events::load(&all);
                let updates = events::mark_updates(
                    &catalog,
                    &old,
                    &new,
                    change.as_ref(),
                    settings.marks_to_all_stack_members,
                );
                // Reference photos are skipped (read-only); say so.
                let skipped = updates
                    .iter()
                    .filter(|&&(i, _)| catalog.is_reference(i))
                    .count();
                if skipped > 0 {
                    emit(LibraryEvent::Notice(crate::app::reference_notice(skipped)));
                }
                let result = events::plan_save(&writable, &new).and_then(|files| {
                    catalog.set_marks(
                        &updates,
                        files,
                        &fileops,
                        &cache,
                        settings.write_stem_sidecar_for_raw,
                        &label,
                    )
                });
                match result {
                    Ok(batch) => {
                        if let Some(b) = batch {
                            undo.push(b);
                            redo.clear();
                            fresh.set(true);
                            history(&undo, &redo);
                        }
                        emit(LibraryEvent::Events(new));
                        publish(&catalog, &settings, true);
                    }
                    Err(e) => emit(LibraryEvent::Error(format!("saving events: {e}"))),
                }
                (false, false)
            }
            LibraryCommand::Undo | LibraryCommand::Redo => {
                let is_undo = matches!(cmd, LibraryCommand::Undo);
                let (from, to) = if is_undo {
                    (&mut undo, &mut redo)
                } else {
                    (&mut redo, &mut undo)
                };
                let Some(batch) = from.pop() else { continue };
                if is_push(&fileops, batch) {
                    // Undone in Photos first; kept on the stack until then.
                    from.push(batch);
                    emit(LibraryEvent::UndoPush(batch));
                    continue;
                }
                match undo_batch(batch, &fileops, &mut catalog, &mut cache, &emit) {
                    Ok(u) => to.push(u),
                    Err(e) => emit(LibraryEvent::Error(format!(
                        "{}: {e}",
                        if is_undo { "undo" } else { "redo" }
                    ))),
                }
                history(&undo, &redo);
                batches(&fileops);
                if let Some(config) = dupes.as_ref().map(|r| r.config) {
                    refresh_impl(
                        &mut cache,
                        &settings,
                        &mut catalog,
                        &mut index,
                        &publish,
                        &emit,
                    );
                    let only = dupes_only.as_deref();
                    find_dupes(
                        &mut cache, &settings, &catalog, config, only, &mut dupes, &emit,
                    );
                    (false, false)
                } else {
                    (false, true)
                }
            }
            LibraryCommand::ClearRedo => {
                if !redo.is_empty() {
                    redo.clear();
                    history(&undo, &redo);
                }
                (false, false)
            }
            LibraryCommand::UndoBatch(batch) if is_push(&fileops, batch) => {
                emit(LibraryEvent::UndoPush(batch));
                (false, false)
            }
            LibraryCommand::UndoPushed(batch) => {
                // Never redone: sending again does it.
                let r = undo_batch(batch, &fileops, &mut catalog, &mut cache, &emit);
                if r.is_ok() {
                    undo.retain(|&b| b != batch);
                    history(&undo, &redo);
                }
                emit(LibraryEvent::BatchUndone(r));
                batches(&fileops);
                (false, true)
            }
            LibraryCommand::UndoBatch(batch) => {
                let r = undo_batch(batch, &fileops, &mut catalog, &mut cache, &emit);
                if r.is_ok() {
                    undo.retain(|&b| b != batch);
                    history(&undo, &redo);
                }
                emit(LibraryEvent::BatchUndone(r));
                batches(&fileops);
                (false, true)
            }
            LibraryCommand::ResumeBatch(batch) => {
                let r = fileops.resume(batch).map_err(|e| e.to_string());
                if r.is_ok() {
                    undo.push(batch);
                    redo.clear();
                    fresh.set(true);
                    history(&undo, &redo);
                }
                emit(LibraryEvent::BatchResumed(r));
                batches(&fileops);
                (false, true)
            }
            LibraryCommand::LoadHistory => {
                batches(&fileops);
                (false, false)
            }
            LibraryCommand::LoadBatch(id) => {
                let ops = fileops
                    .journal()
                    .ops(id)
                    .map(|ops| {
                        ops.into_iter()
                            .map(|o| {
                                (
                                    format!("{:?}", o.op).to_lowercase(),
                                    o.src,
                                    o.dst,
                                    o.status == shoebox_core::fileops::OpStatus::Done,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                emit(LibraryEvent::BatchOps(id, ops));
                (false, false)
            }
            LibraryCommand::PlanImport {
                source,
                root,
                template,
            } => {
                let progress = |done: usize, total: usize| {
                    if done.is_multiple_of(8) || done == total {
                        emit(LibraryEvent::ImportProgress { done, total });
                    }
                };
                use shoebox_core::import;
                import_plan = None;
                import_scan = None;
                let r = shoebox_core::template::Template::parse(&template).and_then(|t| {
                    let scan = import::scan(&source, &catalog, &progress, import_stop)?;
                    let plan = import::decide(&scan, &root, &t, &Default::default())?;
                    Ok((plan, (scan, root, t)))
                });
                match r {
                    Ok((p, scan)) => {
                        emit(LibraryEvent::ImportPlanned(Some(Ok(Arc::new(p.clone())))));
                        import_plan = Some(p);
                        import_scan = Some(scan);
                    }
                    Err(shoebox_core::CoreError::Cancelled) => {
                        emit(LibraryEvent::ImportPlanned(None));
                    }
                    Err(e) => emit(LibraryEvent::ImportPlanned(Some(Err(e.to_string())))),
                }
                (false, false)
            }
            LibraryCommand::ReplanImport(force) => {
                let Some((scan, root, t)) = &import_scan else {
                    continue;
                };
                match shoebox_core::import::decide(scan, root, t, &force) {
                    Ok(p) => {
                        emit(LibraryEvent::ImportReplanned(Arc::new(p.clone())));
                        import_plan = Some(p);
                    }
                    Err(e) => emit(LibraryEvent::Error(format!("import: {e}"))),
                }
                (false, false)
            }
            LibraryCommand::RunImport(folders) => {
                // However it ends (done, cancelled, refused, nothing to do).
                let _ended = Ended(&emit);
                let Some(mut p) = import_plan.take() else {
                    continue;
                };
                import_scan = None;
                if p.copies.is_empty() && p.origins.is_empty() {
                    continue;
                }
                let writable: Vec<PathBuf> = settings
                    .roots
                    .iter()
                    .filter(|r| !r.reference)
                    .map(|r| r.path.clone())
                    .collect();
                let all: Vec<PathBuf> = settings.roots.iter().map(|r| r.path.clone()).collect();
                let mut evs = events::load(&all);
                let named = shoebox_core::import::folder_writes(
                    &mut p,
                    &folders,
                    &mut evs,
                    &catalog,
                    settings.write_stem_sidecar_for_raw,
                )
                .and_then(|mut w| {
                    if w.new_events > 0 {
                        w.writes.extend(events::plan_save(&writable, &evs)?);
                    }
                    Ok(w)
                });
                let named = match named {
                    Ok(w) => w,
                    Err(e) => {
                        emit(LibraryEvent::Error(format!("import: {e}")));
                        continue;
                    }
                };
                if p.copies.is_empty() && named.writes.is_empty() {
                    continue;
                }
                let source = p
                    .copies
                    .first()
                    .and_then(|c| c.src.parent())
                    .map(|d| d.display().to_string())
                    .unwrap_or_default();
                let photos = named.photos;
                let images = p.photos.len();
                let copied = std::cell::Cell::new(0);
                // At most ~10 updates a second (a fast disk copies many
                // small files a second), and every finished file count.
                let last = std::cell::Cell::new((std::time::Instant::now(), usize::MAX));
                let progress = |p: shoebox_core::fileops::CopyProgress<'_>| {
                    copied.set(p.done);
                    let (at, done) = last.get();
                    if done == p.done && at.elapsed() < std::time::Duration::from_millis(100) {
                        return;
                    }
                    last.set((std::time::Instant::now(), p.done));
                    emit(LibraryEvent::ImportCopying {
                        done: p.done,
                        files: p.files,
                        bytes: p.bytes,
                        total_bytes: p.total_bytes,
                        current: p.current.map(Path::to_path_buf),
                    });
                };
                let label = format!("import from {source}");
                match fileops.import_until(&label, &p.copies, named.writes, &progress, import_stop)
                {
                    Ok(b) => {
                        undo.push(b);
                        redo.clear();
                        fresh.set(true);
                        history(&undo, &redo);
                        emit(LibraryEvent::Imported {
                            batch: b,
                            copied: p.copies.len(),
                            named: photos,
                        });
                        if named.new_events > 0 {
                            emit(LibraryEvent::Events(evs));
                        }
                    }
                    Err(shoebox_core::CoreError::BatchCancelled { batch, .. }) => {
                        undo.push(batch);
                        redo.clear();
                        fresh.set(true);
                        history(&undo, &redo);
                        // Copies of images (sidecars come after them).
                        emit(LibraryEvent::ImportCancelled {
                            batch,
                            copied: copied.get().min(images),
                        });
                    }
                    Err(e) => emit(LibraryEvent::Error(format!("import: {e}"))),
                }
                batches(&fileops);
                (false, true)
            }
            LibraryCommand::PhotosBatch(op, reply) => {
                let stop = std::sync::atomic::AtomicBool::new(false);
                let (answer, closed) = match op {
                    PhotosOp::Begin(label) => (fileops.begin_import(&label), None),
                    PhotosOp::BeginPush(label) => (fileops.begin_push(&label), None),
                    PhotosOp::Step {
                        batch,
                        copies,
                        writes,
                    } => match fileops.import_step(batch, &copies, writes, &|_| {}, &stop) {
                        Ok(()) => (Ok(batch), None),
                        // A failed step closes its batch: it's done with.
                        Err(e) => (Err(e), Some(batch)),
                    },
                    PhotosOp::End(batch) => {
                        (fileops.end_import(batch).map(|()| batch), Some(batch))
                    }
                };
                let _ = reply.send(answer);
                match closed {
                    Some(batch) => {
                        undo.push(batch);
                        redo.clear();
                        fresh.set(true);
                        history(&undo, &redo);
                        batches(&fileops);
                        (false, true)
                    }
                    None => (false, false),
                }
            }
            LibraryCommand::Eject(volume) => {
                emit(LibraryEvent::Ejected(eject(&volume)));
                (false, false)
            }
            LibraryCommand::Download(paths) => {
                let total = paths.len();
                let mut failed = 0;
                for (n, p) in paths.iter().enumerate() {
                    if let Err(e) = shoebox_core::cloud::download(p) {
                        tracing::warn!("downloading {}: {e}", p.display());
                        failed += 1;
                    }
                    emit(LibraryEvent::Downloading { done: n, total });
                }
                let photos: Vec<usize> =
                    paths.iter().filter_map(|p| index.get(p).copied()).collect();
                match catalog.reread(&photos, &By::Other, &mut cache) {
                    Ok(r) if !r.changed.is_empty() => {
                        index = reindex(&catalog);
                        publish(&catalog, &settings, true);
                    }
                    Ok(_) => {}
                    Err(e) => emit(LibraryEvent::Error(format!("re-reading: {e}"))),
                }
                emit(LibraryEvent::Downloading { done: total, total });
                if failed > 0 {
                    emit(LibraryEvent::Error(format!(
                        "{failed} of {total} couldn't be downloaded (offline?)"
                    )));
                } else if total > 1 {
                    emit(LibraryEvent::Notice(format!(
                        "Downloaded {total} photos from the cloud."
                    )));
                }
                (false, false)
            }
            LibraryCommand::PreviewOrganise(req) => {
                let r = plan_organise(&catalog, &req).map(|(plan, notes)| OrgPreview {
                    moves: plan,
                    notes,
                    request: req,
                });
                emit(LibraryEvent::OrgPreview(r));
                (false, false)
            }
            LibraryCommand::RunOrganise(req) => {
                match run_organise(&req, &settings, &mut catalog, &fileops, &mut cache, &emit) {
                    Ok((b, files)) => {
                        undo.push(b);
                        redo.clear();
                        fresh.set(true);
                        history(&undo, &redo);
                        emit(LibraryEvent::Organised {
                            batch: b,
                            files,
                            verb: req.verb(),
                        });
                    }
                    Err(e) => emit(LibraryEvent::OrgPreview(Err(e))),
                }
                batches(&fileops);
                (false, true)
            }
            LibraryCommand::NewFolder(dir) => {
                match fileops.create_folder(&format!("new folder {}", dir.display()), &dir) {
                    Ok(_) => batches(&fileops),
                    Err(e) => emit(LibraryEvent::Error(format!("new folder: {e}"))),
                }
                (false, true)
            }
            LibraryCommand::SaveCollections { list, label } => {
                let r = shoebox_core::collections::plan_save(&settings.writable_roots(), &list)
                    .and_then(|w| fileops.write_sidecars(&label, w));
                match r {
                    Ok(b) => {
                        undo.push(b);
                        redo.clear();
                        fresh.set(true);
                        history(&undo, &redo);
                        emit(LibraryEvent::Collections(list));
                    }
                    Err(e) => emit(LibraryEvent::Error(format!("saving collections: {e}"))),
                }
                batches(&fileops);
                (false, false)
            }
            LibraryCommand::FindDuplicates(config, only) => {
                dupes_only = only;
                let only = dupes_only.as_deref();
                find_dupes(
                    &mut cache, &settings, &catalog, config, only, &mut dupes, &emit,
                );
                (false, false)
            }
            LibraryCommand::PreviewDuplicates(decisions) => {
                let preview = dupes
                    .as_ref()
                    .ok_or_else(|| "find duplicates first".to_owned())
                    .and_then(|r| review::plan(r, &decisions).map_err(|e| e.to_string()))
                    .and_then(|plan| {
                        let moves = shoebox_core::fileops::plan_moves(&plan.moves)
                            .map_err(|e| e.to_string())?;
                        Ok(DupePreview {
                            moves: moves.moves,
                            variants: plan.links.len(),
                            ignores: plan.ignores.len(),
                        })
                    });
                emit(LibraryEvent::DupePreview(preview));
                (false, false)
            }
            LibraryCommand::ApplyDuplicates(decisions) => {
                let Some(r) = dupes.as_ref() else { continue };
                match apply_dupes(r, &decisions, &settings, &catalog, &fileops, &mut cache) {
                    Ok((batch, moved)) => {
                        if let Some(b) = batch {
                            undo.push(b);
                            redo.clear();
                            fresh.set(true);
                            history(&undo, &redo);
                        }
                        emit(LibraryEvent::DupesApplied { batch, moved });
                        let config = r.config;
                        refresh_impl(
                            &mut cache,
                            &settings,
                            &mut catalog,
                            &mut index,
                            &publish,
                            &emit,
                        );
                        let only = dupes_only.as_deref();
                        find_dupes(
                            &mut cache, &settings, &catalog, config, only, &mut dupes, &emit,
                        );
                    }
                    Err(e) => emit(LibraryEvent::DupePreview(Err(e))),
                }
                (false, false)
            }
            LibraryCommand::LoadEdit { root, rel, copy } => {
                let json = shoebox_core::edits::load(&cache, &root, &rel, &copy, render_hash)
                    .unwrap_or_else(|e| {
                        tracing::warn!("loading the edit of {rel}: {e}");
                        None
                    });
                let copies =
                    shoebox_core::edits::copies_of(&cache, &root, &rel).unwrap_or_default();
                emit(LibraryEvent::EditLoaded {
                    root,
                    rel,
                    copy,
                    copies,
                    json,
                });
                (false, false)
            }
            LibraryCommand::RemoveCopy { root, rel, copy } => {
                if copy != shoebox_core::edits::MAIN {
                    // Written first, so the file is there to move.
                    if let Err(e) = shoebox_core::edits::save_pending(&cache, &fileops) {
                        tracing::warn!("saving edits: {e}");
                    }
                    let file = shoebox_core::edits::path(&root, Path::new(&rel), &copy);
                    let hash = std::fs::read(&file).map(|b| *blake3::hash(&b).as_bytes());
                    let result = match hash {
                        Ok(h) => fileops
                            .trash("Remove virtual copy", &[(file.clone(), h)])
                            .map(Some)
                            .map_err(|e| e.to_string()),
                        Err(_) => Ok(None), // never written: nothing to move
                    };
                    match result {
                        Ok(batch) => {
                            if let Some(r) = root.to_str() {
                                let _ = cache.remove_edit(&shoebox_core::cache::EditKey {
                                    root: r,
                                    rel: &rel,
                                    copy: &copy,
                                });
                            }
                            if let Some(b) = batch {
                                undo.push(b);
                                redo.clear();
                                fresh.set(true);
                                history(&undo, &redo);
                            }
                        }
                        Err(e) => emit(LibraryEvent::Error(format!("removing the copy: {e}"))),
                    }
                    emit(copies_of(&cache, &root, &rel));
                }
                (false, false)
            }
            LibraryCommand::PutEdit {
                root,
                rel,
                copy,
                json,
            } => {
                match complete_source(&root, &rel, &json) {
                    Ok((json, hash)) => {
                        if let Err(e) =
                            shoebox_core::edits::put(&cache, &root, &rel, &copy, &json, &hash)
                        {
                            emit(LibraryEvent::Error(format!("keeping the edit: {e}")));
                        }
                        if copy != shoebox_core::edits::MAIN {
                            emit(copies_of(&cache, &root, &rel));
                        }
                        // The master's edit is what the library shows.
                        if copy == shoebox_core::edits::MAIN
                            && let Some(info) = edit_info(&root, &rel, json)
                        {
                            emit(LibraryEvent::Edits(vec![info]));
                        }
                    }
                    Err(e) => emit(LibraryEvent::Error(e)),
                }
                (false, false)
            }
            LibraryCommand::RecordExports(rows) => {
                for r in &rows {
                    if let Err(e) = cache.add_export(r) {
                        tracing::warn!("recording an export: {e}");
                    }
                }
                (false, false)
            }
            LibraryCommand::AddExports(exports) => {
                let n = exports.len();
                let r = add_exports(&exports, &settings, &catalog, &fileops);
                let copies: Vec<_> = exports.into_iter().map(|e| e.copy).collect();
                // The cache's copies, whatever happened (an interrupted batch
                // is resumed from the journal, so keep them then).
                if !matches!(r, Err(shoebox_core::CoreError::BatchCancelled { .. })) {
                    for c in &copies {
                        let _ = std::fs::remove_file(&c.src);
                    }
                }
                match r {
                    Ok(b) => {
                        undo.push(b);
                        redo.clear();
                        fresh.set(true);
                        history(&undo, &redo);
                        emit(LibraryEvent::Notice(format!(
                            "Exported {n} file{} into the library. ⌘Z undoes it.",
                            if n == 1 { "" } else { "s" }
                        )));
                    }
                    Err(e) => emit(LibraryEvent::Error(format!("export: {e}"))),
                }
                batches(&fileops);
                (false, true)
            }
            LibraryCommand::AddMerge(done) => {
                let r = add_merge(&done, &settings, &fileops);
                // The cache's copy, whatever happened (an interrupted batch
                // is resumed from the journal, so keep it then).
                if !matches!(r, Err(shoebox_core::CoreError::BatchCancelled { .. })) {
                    let _ = std::fs::remove_file(&done.copy.src);
                }
                match r {
                    Ok(b) => {
                        undo.push(b);
                        redo.clear();
                        fresh.set(true);
                        history(&undo, &redo);
                        let name = done
                            .plan
                            .dst
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let mut notice = format!(
                            "Merged {} photos into {name}. ⌘Z undoes it.",
                            done.plan.sources.len()
                        );
                        for n in &done.notes {
                            notice.push_str(&format!(" {n}."));
                        }
                        emit(LibraryEvent::Notice(notice));
                        emit(LibraryEvent::Merged(done.plan.dst.clone()));
                    }
                    Err(e) => emit(LibraryEvent::Error(format!(
                        "{}: {e}",
                        done.plan.kind.label()
                    ))),
                }
                batches(&fileops);
                (false, true)
            }
            LibraryCommand::LoadFromPhotos(photo) => {
                let roots: Vec<PathBuf> = settings.roots.iter().map(|r| r.path.clone()).collect();
                let (index, ledgers) = from_photos.get_or_insert_with(|| {
                    let l = shoebox_core::photos::ledger::Ledgers::load(&roots);
                    (l.files(&roots), l)
                });
                let found = index.get(&photo).copied();
                let sent = ledgers
                    .pushed_from(&roots, &photo, shoebox_core::edits::MAIN)
                    .map(|(_, p)| SentToPhotos {
                        at: p.pushed_at,
                        render_hash: p.render_hash.clone(),
                    });
                emit(LibraryEvent::FromPhotos(photo, found, sent));
                (false, false)
            }
            LibraryCommand::LoadExports(photo) => {
                let rows = photo
                    .to_str()
                    .map(|p| cache.exports_of(p).unwrap_or_default())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| {
                        let there = Path::new(&r.path).is_file();
                        (r, there)
                    })
                    .collect();
                emit(LibraryEvent::Exports(photo, rows));
                (false, false)
            }
            LibraryCommand::ApplyPreset { photos, label } => {
                use shoebox_develop::edit::{EditDoc, Source};
                let mut infos = Vec::new();
                let mut copied = Vec::new();
                for (root, rel, copy, preset) in photos {
                    let json = shoebox_core::edits::load(&cache, &root, &rel, &copy, render_hash)
                        .unwrap_or_default();
                    let mut doc = json
                        .and_then(|j| EditDoc::from_json(&j).ok())
                        .unwrap_or_else(|| {
                            EditDoc::new(Source {
                                path: rel.clone(),
                                blake3: String::new(),
                                size: None,
                                camera: None,
                                lens: None,
                            })
                        });
                    if !doc.change(&label, |s| *s = preset.apply(s, 1.0)) {
                        continue;
                    }
                    let kept =
                        complete_source(&root, &rel, &doc.to_json()).and_then(|(json, hash)| {
                            shoebox_core::edits::put(&cache, &root, &rel, &copy, &json, &hash)
                                .map_err(|e| e.to_string())?;
                            Ok(json)
                        });
                    match kept {
                        Ok(_) if copy != shoebox_core::edits::MAIN => {
                            copied.push(copies_of(&cache, &root, &rel));
                        }
                        Ok(json) => infos.extend(edit_info(&root, &rel, json)),
                        Err(e) => emit(LibraryEvent::Error(format!("{label}: {rel}: {e}"))),
                    }
                }
                if let Err(e) = shoebox_core::edits::save_pending(&cache, &fileops) {
                    emit(LibraryEvent::Error(format!("saving edits: {e}")));
                }
                if !infos.is_empty() {
                    emit(LibraryEvent::Edits(infos));
                }
                for ev in copied {
                    emit(ev);
                }
                (false, false)
            }
            LibraryCommand::SaveEdits => {
                if let Err(e) = shoebox_core::edits::save_pending(&cache, &fileops) {
                    emit(LibraryEvent::Error(format!("saving edits: {e}")));
                }
                (false, false)
            }
            LibraryCommand::MarkSeen(paths) => {
                let idx: Vec<usize> = paths.iter().filter_map(|p| index.get(p).copied()).collect();
                if let Err(e) = catalog.mark_seen(&idx, &mut cache) {
                    tracing::warn!("mark seen: {e}");
                }
                (false, false)
            }
        };
        if settings_changed {
            fileops.set_protected(reference_roots(&settings));
            // Re-watch only when the folders change: stopping a watcher
            // blocks (FSEvents), seconds at a time on a busy system, and
            // most settings have nothing to do with it.
            let roots = roots_of(&settings);
            if roots != watched {
                drop(watcher.take()); // stop watching before re-watching the roots
                watcher = crate::watch::watch(&roots, fs_tx.clone());
                watched = roots;
            }
            if let Err(e) = settings.save(&paths.settings) {
                emit(LibraryEvent::Error(format!("saving settings: {e}")));
            }
            if !rescan {
                publish(&catalog, &settings, true);
            }
        }
        if rescan {
            refresh_impl(
                &mut cache,
                &settings,
                &mut catalog,
                &mut index,
                &publish,
                &emit,
            );
            reattach_edits(&cache, &catalog, &settings, &fileops, &emit);
        }
    }
    drop(watcher);

    /// Rescan from disk (reusing the cache) and publish the result.
    fn refresh_impl(
        cache: &mut shoebox_core::cache::Cache,
        settings: &Settings,
        catalog: &mut Catalog,
        index: &mut HashMap<PathBuf, usize>,
        publish: &dyn Fn(&Catalog, &Settings, bool),
        emit: &(dyn Fn(LibraryEvent) + Sync),
    ) {
        let progress = |done: usize, total: usize| {
            if done.is_multiple_of(64) || done == total {
                emit(LibraryEvent::Progress { done, total });
            }
        };
        match Catalog::refresh(cache, settings.scan_roots(), &progress) {
            Ok((c, _)) => {
                *catalog = c;
                *index = catalog
                    .photos
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (p.path.clone(), i))
                    .collect();
                let roots: Vec<PathBuf> = settings.roots.iter().map(|r| r.path.clone()).collect();
                emit(LibraryEvent::Events(events::load(&roots)));
                emit(LibraryEvent::Collections(shoebox_core::collections::load(
                    &roots,
                )));
                publish(catalog, settings, true);
            }
            Err(e) => emit(LibraryEvent::Error(format!("scanning: {e}"))),
        }
    }
}

/// After a scan: edits whose photo has gone (moved or renamed in Finder)
/// are matched to a photo by size, then content, and moved to its edit
/// path as one journaled batch; the rest are reported as unattached.
fn reattach_edits(
    cache: &shoebox_core::cache::Cache,
    catalog: &Catalog,
    settings: &Settings,
    fileops: &FileOps,
    emit: &(dyn Fn(LibraryEvent) + Sync),
) {
    use shoebox_core::edits;
    let writable: Vec<PathBuf> = settings
        .roots
        .iter()
        .filter(|r| !r.reference)
        .map(|r| r.path.clone())
        .collect();
    let mut orphans = Vec::new();
    let mut edited: HashSet<PathBuf> = HashSet::new();
    let mut orphan_of: HashMap<PathBuf, (PathBuf, edits::EditFile)> = HashMap::new();
    for root in &writable {
        let Ok(files) = edits::all(root) else {
            continue;
        };
        for f in files {
            let photo = root.join(&f.rel);
            if photo.exists() {
                edited.insert(photo);
            } else if f.blake3.is_some() {
                orphan_of.insert(f.path.clone(), (root.clone(), f.clone()));
                orphans.push(f);
            }
        }
    }
    if orphans.is_empty() {
        emit(LibraryEvent::Unattached(Vec::new()));
        return;
    }
    let candidates: Vec<(PathBuf, u64)> = catalog
        .photos
        .iter()
        .filter(|p| !edited.contains(&p.path) && writable.iter().any(|r| p.path.starts_with(r)))
        .map(|p| (p.path.clone(), p.size))
        .collect();
    let plan = edits::reattach(
        &orphans,
        |p| writable.iter().find(|r| p.starts_with(r)).cloned(),
        &candidates,
        |p| {
            std::fs::read(p)
                .ok()
                .map(|b| shoebox_develop::dng::DngCache::key(&b))
        },
    );
    let moved: HashSet<PathBuf> = plan.iter().map(|(src, _)| src.clone()).collect();
    if !plan.is_empty() {
        let p = shoebox_core::fileops::Plan {
            moves: plan
                .iter()
                .map(|(src, dst)| PlannedMove {
                    src: src.clone(),
                    dst: dst.clone(),
                })
                .collect(),
        };
        match fileops.execute("Reattach edits", &p) {
            Ok(_) => {
                for (src, dst) in &plan {
                    let (Some((old_root, old)), Some(new_root)) = (
                        orphan_of.get(src),
                        writable.iter().find(|r| dst.starts_with(edits::dir(r))),
                    ) else {
                        continue;
                    };
                    // The photo's path from the edit's: drop `.json` or
                    // `.<copy>.json`.
                    let suffix = if old.copy == edits::MAIN {
                        ".json".to_owned()
                    } else {
                        format!(".{}.json", old.copy)
                    };
                    let new_rel = dst
                        .strip_prefix(edits::dir(new_root))
                        .ok()
                        .and_then(|r| r.to_str())
                        .and_then(|r| r.strip_suffix(&suffix));
                    if let (Some(a), Some(b), Some(ra), Some(rb)) = (
                        old_root.to_str(),
                        new_root.to_str(),
                        old.rel.to_str(),
                        new_rel,
                    ) {
                        if let Err(e) = cache.move_edits((a, ra), (b, rb)) {
                            tracing::warn!("carrying an edit's row: {e}");
                        }
                        if old.copy == edits::MAIN
                            && let Ok(Some(json)) = edits::read(dst)
                            && let Some(info) = edit_info(new_root, rb, json)
                        {
                            emit(LibraryEvent::Edits(vec![info]));
                        }
                    }
                }
                emit(LibraryEvent::Notice(format!(
                    "Found the photos of {} edit{} moved outside Shoebox",
                    plan.len(),
                    if plan.len() == 1 { "" } else { "s" }
                )));
            }
            Err(e) => emit(LibraryEvent::Error(format!("reattaching edits: {e}"))),
        }
    }
    emit(LibraryEvent::Unattached(
        orphans
            .into_iter()
            .map(|o| o.path)
            .filter(|p| !moved.contains(p))
            .collect(),
    ));
}

/// Undo one batch, carrying catalog entries back across reversed moves and
/// recording the reversal as Shoebox's change. Returns the undo batch.
fn undo_batch(
    batch: i64,
    fileops: &FileOps,
    catalog: &mut Catalog,
    cache: &mut shoebox_core::cache::Cache,
    emit: &(dyn Fn(LibraryEvent) + Sync),
) -> Result<i64, String> {
    let ops = fileops.journal().ops(batch).unwrap_or_default();
    let touched: Vec<PathBuf> = ops
        .iter()
        .flat_map(|o| [o.src.clone(), o.dst.clone()])
        .collect();
    let u = fileops.undo(batch).map_err(|e| e.to_string())?;
    let back: Vec<PlannedMove> = ops
        .iter()
        .filter(|o| o.op == shoebox_core::fileops::Op::Move)
        .map(|o| PlannedMove {
            src: o.dst.clone(),
            dst: o.src.clone(),
        })
        .collect();
    if let Err(e) = catalog.note_moved(&back, cache) {
        tracing::warn!("carrying the cache across undo: {e}");
    }
    edits_moved(cache, catalog, &back, emit);
    let label = fileops
        .journal()
        .batch(u)
        .ok()
        .flatten()
        .map_or_else(String::new, |b| b.label);
    let a = catalog.affected(&touched);
    if let Err(e) = catalog.reread(&a.photos, &By::Shoebox { batch: u, label }, cache) {
        tracing::warn!("re-reading after undo: {e}");
    }
    Ok(u)
}

/// Stacks of these files.
/// An edit document's render hash (for the catalog row); empty if it
/// doesn't parse.
/// A photo's virtual copies (for their tiles and the grid's badge).
fn copies_of(cache: &shoebox_core::cache::Cache, root: &Path, rel: &str) -> LibraryEvent {
    let mut list: Vec<CopyInfo> = root
        .to_str()
        .and_then(|r| {
            let ids = cache.edit_copies(r, rel).ok()?;
            Some(
                ids.iter()
                    .filter(|c| *c != shoebox_core::edits::MAIN)
                    .filter_map(|c| {
                        let key = shoebox_core::cache::EditKey {
                            root: r,
                            rel,
                            copy: c,
                        };
                        let json = cache.edit(&key).ok()??.json;
                        copy_info(c, json)
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    sort_copies(&mut list);
    LibraryEvent::Copies(vec![(root.join(rel), list)])
}

fn render_hash(json: &str) -> String {
    shoebox_develop::edit::EditDoc::from_json(json)
        .map_or_else(|_| String::new(), |d| d.render_hash())
}

/// Fill in an edit's source (BLAKE3, size, and path as it is now) from the
/// photo, and return it with its render hash.
fn complete_source(root: &Path, rel: &str, json: &str) -> Result<(String, String), String> {
    let mut doc = shoebox_develop::edit::EditDoc::from_json(json).map_err(|e| e.to_string())?;
    doc.source.path = rel.to_owned();
    if doc.source.blake3.is_empty() {
        let path = root.join(rel);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        doc.source.blake3 = shoebox_develop::dng::DngCache::key(&bytes);
        doc.source.size = Some(bytes.len() as u64);
    }
    let hash = doc.render_hash();
    Ok((doc.to_json(), hash))
}

fn stacks_of(catalog: &Catalog, photos: &[PathBuf]) -> Vec<usize> {
    let mut v: Vec<usize> = photos
        .iter()
        .filter_map(|p| catalog.photos.iter().position(|x| &x.path == p))
        .map(|i| catalog.stack_of[i])
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The moves (or, for the Trash, the files) an organise request means.
fn plan_organise(
    catalog: &Catalog,
    req: &OrgRequest,
) -> Result<(Vec<PlannedMove>, Vec<String>), String> {
    use shoebox_core::organise;
    let stacks = stacks_of(catalog, req.photos());
    if stacks.is_empty() {
        return Err("nothing selected".into());
    }
    let mut notes = Vec::new();
    let moves = match req {
        OrgRequest::MoveTo { folder, .. } => {
            if !catalog.roots.iter().any(|r| folder.starts_with(&r.path)) {
                return Err(format!("{} isn't in the library", folder.display()));
            }
            organise::plan_move_to(catalog, &stacks, folder)
                .map_err(|e| e.to_string())?
                .moves
        }
        OrgRequest::Reject { .. } => {
            organise::plan_reject(catalog, &stacks)
                .map_err(|e| e.to_string())?
                .moves
        }
        OrgRequest::Rename { template, .. } => {
            let t = shoebox_core::template::Template::parse(template).map_err(|e| e.to_string())?;
            let r = organise::plan_rename(catalog, &stacks, &t).map_err(|e| e.to_string())?;
            for u in &r.undated {
                notes.push(format!(
                    "{} has no capture date; left as it is",
                    u.display()
                ));
            }
            r.plan.moves
        }
        OrgRequest::Trash { .. } => organise::trash_files(catalog, &stacks)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|p| PlannedMove {
                src: p.clone(),
                dst: p,
            })
            .collect(),
    };
    Ok((moves, notes))
}

/// Run an organise request as one batch: moves with the reference files
/// they affect, or the Trash. Returns (batch, files).
fn run_organise(
    req: &OrgRequest,
    settings: &Settings,
    catalog: &mut Catalog,
    fileops: &FileOps,
    cache: &mut shoebox_core::cache::Cache,
    emit: &(dyn Fn(LibraryEvent) + Sync),
) -> Result<(i64, usize), String> {
    let (moves, _) = plan_organise(catalog, req)?;
    if moves.is_empty() {
        return Err("nothing to do: every file already has that name".into());
    }
    if let OrgRequest::Trash { .. } = req {
        let files: Vec<(PathBuf, [u8; 32])> = moves
            .iter()
            .map(|m| {
                let mut h = blake3::Hasher::new();
                std::fs::File::open(&m.src)
                    .and_then(|f| h.update_reader(f).map(|_| ()))
                    .map(|()| (m.src.clone(), *h.finalize().as_bytes()))
                    .map_err(|e| format!("{}: {e}", m.src.display()))
            })
            .collect::<Result<_, _>>()?;
        let b = fileops
            .trash(&req.label(), &files)
            .map_err(|e| e.to_string())?;
        return Ok((b, files.len()));
    }
    let plan = shoebox_core::fileops::Plan {
        moves: moves.clone(),
    };
    let writes = shoebox_core::organise::reference_writes(
        &settings.all_roots(),
        &settings.writable_roots(),
        &moves,
    )
    .map_err(|e| e.to_string())?;
    let b = fileops
        .execute_with_writes(&req.label(), &plan, writes)
        .map_err(|e| e.to_string())?;
    if let Err(e) = catalog.note_moved(&moves, cache) {
        tracing::warn!("carrying the cache across moves: {e}");
    }
    edits_moved(cache, catalog, &moves, emit);
    Ok((b, moves.len()))
}

/// Exports into the library: the earlier ones they redo (still as they
/// were) to the Trash, the new ones copied in, each joined to its photo's
/// stack as a variant. One batch.
fn add_exports(
    exports: &[crate::develop::LibraryExport],
    settings: &Settings,
    catalog: &Catalog,
    fileops: &FileOps,
) -> shoebox_core::Result<i64> {
    let replaced: Vec<(PathBuf, [u8; 32])> = exports
        .iter()
        .filter_map(|e| e.replaces.as_ref())
        .filter_map(|p| Some((p.clone(), *blake3::hash(&std::fs::read(p).ok()?).as_bytes())))
        .collect();
    let copies: Vec<_> = exports.iter().map(|e| e.copy.clone()).collect();
    let mut links = catalog.links.clone();
    links.retain(|l| !exports.iter().any(|e| e.copy.dst == l.file));
    links.extend(exports.iter().map(|e| shoebox_core::stacks::Link {
        file: e.copy.dst.clone(),
        of: e.photo.clone(),
    }));
    let writes = shoebox_core::stacks::plan_save_links(&settings.writable_roots(), &links)?;
    fileops.add_exports("export into the library", &replaced, &copies, writes)
}

/// A photo merge into the library: the DNG copied in beside its first
/// source and its record added to `merges.json`, one batch.
fn add_merge(
    done: &crate::merge::Done,
    settings: &Settings,
    fileops: &FileOps,
) -> shoebox_core::Result<i64> {
    let mut list = shoebox_core::merges::load(&settings.all_roots());
    list.retain(|m| m.file != done.plan.dst);
    list.push(done.plan.record());
    let writes = shoebox_core::merges::plan_save(&settings.writable_roots(), &list)?;
    let label = format!(
        "{} of {} photos",
        done.plan.kind.label(),
        done.plan.sources.len()
    );
    fileops.add_merge(&label, &done.copy, &done.plan.sources, writes)
}

/// Photos Shoebox moved take their edits along (`Catalog::note_moved`):
/// the app hears of the master's edit and the virtual copies at the new
/// path, and forgets them at the old one.
fn edits_moved(
    cache: &shoebox_core::cache::Cache,
    catalog: &Catalog,
    moves: &[PlannedMove],
    emit: &(dyn Fn(LibraryEvent) + Sync),
) {
    let from: HashMap<&Path, &Path> = moves
        .iter()
        .map(|m| (m.dst.as_path(), m.src.as_path()))
        .collect();
    let mut infos = Vec::new();
    let mut copies = Vec::new();
    for p in &catalog.photos {
        let Some(&src) = from.get(p.path.as_path()) else {
            continue;
        };
        let root = &catalog.roots[p.root].path;
        let Some(r) = root.to_str() else { continue };
        let key = shoebox_core::cache::EditKey {
            root: r,
            rel: &p.rel,
            copy: shoebox_core::edits::MAIN,
        };
        if let Ok(Some(e)) = cache.edit(&key) {
            infos.extend(edit_info(root, &p.rel, e.json));
            infos.push(EditInfo {
                path: src.to_path_buf(),
                key: None,
                json: String::new(),
            });
        }
        if let LibraryEvent::Copies(list) = copies_of(cache, root, &p.rel)
            && list.iter().any(|(_, c)| !c.is_empty())
        {
            copies.extend(list);
            copies.push((src.to_path_buf(), Vec::new()));
        }
    }
    if !infos.is_empty() {
        emit(LibraryEvent::Edits(infos));
    }
    if !copies.is_empty() {
        emit(LibraryEvent::Copies(copies));
    }
}

/// Run duplicate detection and publish the groups.
fn find_dupes(
    cache: &mut shoebox_core::cache::Cache,
    settings: &Settings,
    catalog: &Catalog,
    config: shoebox_core::dupes::Config,
    only: Option<&HashSet<PathBuf>>,
    dupes: &mut Option<Review>,
    emit: &(dyn Fn(LibraryEvent) + Sync),
) {
    let progress = |done: usize, total: usize| {
        if done.is_multiple_of(16) || done == total {
            emit(LibraryEvent::DupeProgress { done, total });
        }
    };
    let settled: std::collections::HashSet<PathBuf> =
        catalog.links.iter().map(|l| l.file.clone()).collect();
    match review::find(
        cache,
        settings.scan_roots(),
        config,
        &settled,
        only,
        &progress,
    ) {
        Ok(r) => {
            tracing::info!(
                "{} duplicate groups ({} files analysed, {} cached)",
                r.groups.len(),
                r.stats.analyzed,
                r.stats.cached
            );
            emit(LibraryEvent::Duplicates(Arc::new(r.clone())));
            *dupes = Some(r);
        }
        Err(e) => emit(LibraryEvent::Error(format!("finding duplicates: {e}"))),
    }
}

/// Apply duplicate decisions: check each file to move is unchanged, then
/// move them and save stack links as one batch; remember dismissed pairs.
fn apply_dupes(
    r: &Review,
    decisions: &HashMap<PathBuf, Decision>,
    settings: &Settings,
    catalog: &Catalog,
    fileops: &FileOps,
    cache: &mut shoebox_core::cache::Cache,
) -> Result<(Option<i64>, usize), String> {
    let plan = review::plan(r, decisions).map_err(|e| e.to_string())?;
    for (src, _) in &plan.moves {
        review::check_unchanged(r, src).map_err(|e| e.to_string())?;
    }
    let moves = shoebox_core::fileops::plan_moves(&plan.moves).map_err(|e| e.to_string())?;
    let writes = if plan.links.is_empty() {
        Vec::new()
    } else {
        let writable: Vec<PathBuf> = settings
            .roots
            .iter()
            .filter(|r| !r.reference)
            .map(|r| r.path.clone())
            .collect();
        let mut links = catalog.links.clone();
        links.retain(|l| !plan.links.iter().any(|n| n.file == l.file));
        links.extend(plan.links.iter().cloned());
        shoebox_core::stacks::plan_save_links(&writable, &links).map_err(|e| e.to_string())?
    };
    let batch = if moves.moves.is_empty() && writes.is_empty() {
        None
    } else {
        let label = format!(
            "duplicates: move {}, {} variants",
            plan.moves.len(),
            plan.links.len()
        );
        Some(
            fileops
                .execute_with_writes(&label, &moves, writes)
                .map_err(|e| e.to_string())?,
        )
    };
    cache
        .ignore_pairs(&plan.ignores)
        .map_err(|e| e.to_string())?;
    Ok((batch, plan.moves.len()))
}

/// What the library worker acts on: the app's commands and watcher batches.
enum Command {
    App(LibraryCommand),
    Changed(Vec<PathBuf>),
}
