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
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use crossbeam_channel::{Receiver, Sender};
use shoebox_core::catalog::Catalog;
use shoebox_core::changes::By;
use shoebox_core::events::{self, Event};
use shoebox_core::fileops::{FileOps, Journal};
use shoebox_core::marks::Marks;
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
}

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
        let src = Source {
            path: &job.path,
            format: job.format,
            size: job.size,
            mtime_ns: job.mtime_ns,
            orientation: job.orientation,
            target: job.target,
        };
        let result = match key.size {
            Size::Small => thumbs.get(&src, thumbs::SMALL).map_err(|e| e.to_string()),
            Size::Large => thumbs.get(&src, thumbs::LARGE).map_err(|e| e.to_string()),
            Size::Full => std::fs::read(&job.path)
                .map_err(|e| format!("{}: {e}", job.path.display()))
                .and_then(|data| {
                    shoebox_decode::render(job.format, &data, None, job.orientation, job.target)
                        .map_err(|e| format!("{}: {e}", job.path.display()))
                }),
        };
        let histogram = match (&result, key.size) {
            (Ok(img), Size::Small) => Some(Box::new(img.histogram())),
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
    /// Replace the events: writes `.shoebox/events.json` in every writable
    /// root and the affected sidecars, as one undoable batch.
    SaveEvents {
        events: Vec<Event>,
        label: String,
    },
    Undo,
    Redo,
    /// Replace settings other than roots (options, smart filters).
    SaveSettings(Box<Settings>),
    /// Clear the unseen changes of these photos.
    MarkSeen(Vec<PathBuf>),
}

pub enum LibraryEvent {
    Catalog {
        catalog: Arc<Catalog>,
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
    /// Undo/redo availability changed.
    History {
        can_undo: bool,
        can_redo: bool,
    },
    Error(String),
}

pub struct Library {
    commands: Sender<LibraryCommand>,
    pub events: Receiver<LibraryEvent>,
}

pub struct LibraryPaths {
    pub settings: PathBuf,
    pub cache_db: PathBuf,
    pub journal: PathBuf,
}

impl Library {
    pub fn new(paths: LibraryPaths, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (ctx_tx, ctx_rx) = crossbeam_channel::unbounded::<LibraryCommand>();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("library".into())
            .spawn(move || library_worker(&paths, &ctx_rx, &ev_tx, &repaint));
        if let Err(e) = spawned {
            tracing::error!("could not start library thread: {e}");
        }
        Self {
            commands: ctx_tx,
            events: ev_rx,
        }
    }

    pub fn send(&self, c: LibraryCommand) {
        let _ = self.commands.send(c);
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

fn library_worker(
    paths: &LibraryPaths,
    commands: &Receiver<LibraryCommand>,
    events: &Sender<LibraryEvent>,
    repaint: &(dyn Fn() + Send + Sync),
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
        Ok(j) => FileOps::new(j, reference_roots(&settings)),
        Err(e) => {
            emit(LibraryEvent::Error(format!("opening journal: {e}")));
            return;
        }
    };
    let mut catalog = Catalog::default();
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    // Batches made in this session, for Cmd+Z / Cmd+Shift+Z.
    let (mut undo, mut redo): (Vec<i64>, Vec<i64>) = (Vec::new(), Vec::new());

    let publish = |catalog: &Catalog, settings: &Settings, complete: bool| {
        emit(LibraryEvent::Catalog {
            catalog: Arc::new(catalog.clone()),
            settings: settings.clone(),
            complete,
        });
    };
    let history = |undo: &[i64], redo: &[i64]| {
        emit(LibraryEvent::History {
            can_undo: !undo.is_empty(),
            can_redo: !redo.is_empty(),
        });
    };

    // Changed paths from the file watcher, re-created when the roots change.
    let (fs_tx, fs_rx) = crossbeam_channel::unbounded::<Vec<PathBuf>>();
    let watch_roots = |s: &Settings| {
        let roots: Vec<PathBuf> = s.roots.iter().map(|r| r.path.clone()).collect();
        crate::watch::watch(&roots, fs_tx.clone())
    };
    let mut watcher = watch_roots(&settings);
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
                    refresh_impl(&mut cache, &settings, &mut catalog, &mut index, &emit);
                }
                continue;
            }
        };
        let (settings_changed, rescan) = match cmd {
            LibraryCommand::Open => {
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
            LibraryCommand::SaveEvents { events: new, label } => {
                let writable: Vec<PathBuf> = settings
                    .roots
                    .iter()
                    .filter(|r| !r.reference)
                    .map(|r| r.path.clone())
                    .collect();
                let updates =
                    events::mark_updates(&catalog, &new, settings.marks_to_all_stack_members);
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
                let touched: Vec<PathBuf> = fileops
                    .journal()
                    .ops(batch)
                    .map(|ops| ops.into_iter().flat_map(|o| [o.src, o.dst]).collect())
                    .unwrap_or_default();
                match fileops.undo(batch) {
                    Ok(u) => {
                        to.push(u);
                        // Record the reversal as Shoebox's own change.
                        let label = fileops
                            .journal()
                            .batch(u)
                            .ok()
                            .flatten()
                            .map_or_else(String::new, |b| b.label);
                        let a = catalog.affected(&touched);
                        if let Err(e) =
                            catalog.reread(&a.photos, &By::Shoebox { batch: u, label }, &mut cache)
                        {
                            tracing::warn!("re-reading after undo: {e}");
                        }
                    }
                    Err(e) => emit(LibraryEvent::Error(format!(
                        "{}: {e}",
                        if is_undo { "undo" } else { "redo" }
                    ))),
                }
                history(&undo, &redo);
                (false, true)
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
            drop(watcher.take()); // stop watching before re-watching the roots
            watcher = watch_roots(&settings);
            if let Err(e) = settings.save(&paths.settings) {
                emit(LibraryEvent::Error(format!("saving settings: {e}")));
            }
            if !rescan {
                publish(&catalog, &settings, true);
            }
        }
        if rescan {
            refresh_impl(&mut cache, &settings, &mut catalog, &mut index, &emit);
        }
    }
    drop(watcher);

    /// Rescan from disk (reusing the cache) and publish the result.
    fn refresh_impl(
        cache: &mut shoebox_core::cache::Cache,
        settings: &Settings,
        catalog: &mut Catalog,
        index: &mut HashMap<PathBuf, usize>,
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
                emit(LibraryEvent::Catalog {
                    catalog: Arc::new(catalog.clone()),
                    settings: settings.clone(),
                    complete: true,
                });
            }
            Err(e) => emit(LibraryEvent::Error(format!("scanning: {e}"))),
        }
    }
}

/// What the library worker acts on: the app's commands and watcher batches.
enum Command {
    App(LibraryCommand),
    Changed(Vec<PathBuf>),
}
