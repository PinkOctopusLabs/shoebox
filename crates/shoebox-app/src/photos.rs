//! The iCloud Photos thread (PHOTOS-SPEC): lists Photos, plans a sync, and
//! runs it: downloads (four at a time) into the cache and places them here,
//! while the library worker copies each step into the library in the
//! sync's one batch, so the library stays usable during a sync that takes
//! hours. The source is a [`PhotoSource`]: PhotoKit in the app, a fake in
//! tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use shoebox_core::catalog::Catalog;
use shoebox_core::fileops::PlannedCopy;
use shoebox_core::photos::ledger::{AlbumChoice, Ledgers};
use shoebox_core::photos::place::{Context, LibraryIndex};
use shoebox_core::photos::plan::{self, Options, Plan};
use shoebox_core::photos::sync::{self, Batches, Job, Progress, Report};
use shoebox_core::photos::{Access, Library, PhotoSource, push};
use shoebox_core::template::Template;
use shoebox_core::{CoreError, Result};

use crate::jobs::{LibraryCommand, PhotosOp};

/// Assets per step: copied and recorded together.
const STEP: usize = 20;
/// Downloads at once.
const PARALLEL: usize = 4;
/// Stop before a step with less free than this on the library's or the
/// cache's volume.
const MIN_FREE: u64 = 10 * 1024 * 1024 * 1024;

/// What to plan.
#[derive(Clone)]
pub struct PlanRequest {
    /// Where new photos go.
    pub root: PathBuf,
    /// Every root, and those that can be written.
    pub roots: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub template: String,
    pub options: Options,
    pub catalog: Arc<Catalog>,
}

pub enum Command {
    /// Ask for access if it hasn't been asked, list Photos and plan.
    Plan(Box<PlanRequest>),
    /// Run the last plan with these album choices (by album id).
    Run(HashMap<String, AlbumChoice>),
    /// What sending these photos to Photos would do.
    PlanPush {
        items: Vec<push::Item>,
        roots: Vec<PathBuf>,
    },
    /// Send rendered files to Photos (and remove them from the cache).
    Push {
        rendered: Vec<push::Rendered>,
        roots: Vec<PathBuf>,
        album: Option<String>,
    },
    /// Undo a send in Photos, then have the library undo its batch.
    UndoPush { batch: i64, roots: Vec<PathBuf> },
}

/// A plan, ready to show.
pub struct Planned {
    pub plan: Plan,
    pub library: Library,
    pub root: PathBuf,
}

pub enum Event {
    Access(Access),
    /// Listing Photos.
    Listing,
    /// How far listing has got.
    ListingProgress(shoebox_core::photos::Listing),
    /// Listing was stopped, as asked.
    Stopped,
    Planned(Arc<Planned>),
    Progress(Progress),
    Done(Report),
    Failed(String),
    PushPlanned(Vec<push::Row>),
    Pushed(push::Report),
    /// A send undone in Photos (how many photos), or why not.
    PushUndone(std::result::Result<usize, String>),
}

/// The thread's handle.
pub struct Photos {
    commands: Sender<Command>,
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
}

impl Photos {
    pub fn new(
        source: Arc<dyn PhotoSource>,
        library: Sender<LibraryCommand>,
        staging: PathBuf,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (etx, erx) = crossbeam_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("photos".into())
            .spawn(move || {
                let emit = |e: Event| {
                    let _ = etx.send(e);
                    repaint();
                };
                worker(&*source, &library, &staging, &rx, &emit, &s);
            });
        if let Err(e) = spawned {
            tracing::error!("could not start the Photos thread: {e}");
        }
        Self {
            commands: tx,
            events: erx,
            stop,
        }
    }

    pub fn send(&self, c: Command) {
        self.stop.store(false, Ordering::Relaxed);
        let _ = self.commands.send(c);
    }

    /// Stop the sync between files (what's copied stays, as its batch).
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The library worker, as the sync's [`Batches`].
struct Worker<'a>(&'a Sender<LibraryCommand>);

impl Worker<'_> {
    fn ask(&self, op: PhotosOp) -> Result<i64> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.0
            .send(LibraryCommand::PhotosBatch(op, tx))
            .map_err(|_| CoreError::Photos("the library has closed".into()))?;
        rx.recv()
            .map_err(|_| CoreError::Photos("the library has closed".into()))?
    }
}

/// The library worker, for a send's batch (kind "photos-push").
struct PushWorker<'a>(Worker<'a>);

impl Batches for PushWorker<'_> {
    fn begin(&self, label: &str) -> Result<i64> {
        self.0.ask(PhotosOp::BeginPush(label.to_owned()))
    }

    fn step(
        &self,
        batch: i64,
        copies: Vec<PlannedCopy>,
        writes: Vec<(PathBuf, Vec<u8>)>,
    ) -> Result<()> {
        self.0.step(batch, copies, writes)
    }

    fn end(&self, batch: i64) -> Result<()> {
        self.0.end(batch)
    }
}

impl Batches for Worker<'_> {
    fn begin(&self, label: &str) -> Result<i64> {
        self.ask(PhotosOp::Begin(label.to_owned()))
    }

    fn step(
        &self,
        batch: i64,
        copies: Vec<PlannedCopy>,
        writes: Vec<(PathBuf, Vec<u8>)>,
    ) -> Result<()> {
        self.ask(PhotosOp::Step {
            batch,
            copies,
            writes,
        })
        .map(|_| ())
    }

    fn end(&self, batch: i64) -> Result<()> {
        self.ask(PhotosOp::End(batch)).map(|_| ())
    }
}

fn worker(
    source: &dyn PhotoSource,
    library: &Sender<LibraryCommand>,
    staging: &std::path::Path,
    commands: &Receiver<Command>,
    emit: &(dyn Fn(Event) + Sync),
    stop: &AtomicBool,
) {
    let mut last: Option<(PlanRequest, Arc<Planned>)> = None;
    while let Ok(c) = commands.recv() {
        match c {
            Command::Plan(req) => {
                let access = source.request_access();
                emit(Event::Access(access));
                if !matches!(access, Access::Full | Access::Limited) {
                    continue;
                }
                emit(Event::Listing);
                // At most ~10 updates a second.
                let shown = std::sync::Mutex::new(Instant::now() - Duration::from_secs(1));
                let progress = |l: shoebox_core::photos::Listing| {
                    if let Ok(mut at) = shown.lock()
                        && at.elapsed() >= Duration::from_millis(100)
                    {
                        *at = Instant::now();
                        emit(Event::ListingProgress(l));
                    }
                };
                let lib = match source.fetch_with(&progress, stop) {
                    Ok(l) => l,
                    Err(CoreError::Cancelled) => {
                        emit(Event::Stopped);
                        continue;
                    }
                    Err(e) => {
                        emit(Event::Failed(e.to_string()));
                        continue;
                    }
                };
                let ledgers = Ledgers::load(&req.roots);
                let at = req.roots.iter().position(|r| r == &req.root);
                let choices = at
                    .and_then(|i| ledgers.choices.get(i))
                    .map(|c| c.albums.clone())
                    .unwrap_or_default();
                let mut p = plan::plan(&lib, &ledgers, &choices, &req.options);
                plan::undated_copies(&mut p, &lib, &ledgers, &req.catalog, &req.roots);
                let planned = Arc::new(Planned {
                    plan: p,
                    library: lib,
                    root: req.root.clone(),
                });
                emit(Event::Planned(planned.clone()));
                last = Some((*req, planned));
            }
            Command::PlanPush { items, roots } => {
                let access = source.request_access();
                emit(Event::Access(access));
                if !matches!(access, Access::Full | Access::Limited) {
                    continue;
                }
                let ledgers = Ledgers::load(&roots);
                let look = push::assets_to_look_up(&items, &ledgers, &roots);
                match source.lookup(&look) {
                    Ok(now) => emit(Event::PushPlanned(push::plan(
                        &items, &ledgers, &roots, &now,
                    ))),
                    Err(e) => emit(Event::Failed(e.to_string())),
                }
            }
            Command::Push {
                rendered,
                roots,
                album,
            } => {
                let mut ledgers = Ledgers::load(&roots);
                let run_id = jiff::Zoned::now().strftime("%Y%m%dT%H%M%S").to_string();
                let r = push::run(
                    source,
                    &PushWorker(Worker(library)),
                    &mut ledgers,
                    &roots,
                    &rendered,
                    album.as_deref(),
                    &run_id,
                    "send to Photos",
                );
                // The renders are the cache's own files.
                for f in &rendered {
                    let _ = std::fs::remove_file(&f.file);
                }
                match r {
                    Ok(r) => emit(Event::Pushed(r)),
                    Err(e) => emit(Event::Failed(e.to_string())),
                }
            }
            Command::UndoPush { batch, roots } => {
                let ledgers = Ledgers::load(&roots);
                match push::undo(source, &ledgers, batch) {
                    Ok(n) => {
                        let _ = library.send(LibraryCommand::UndoPushed(batch));
                        emit(Event::PushUndone(Ok(n)));
                    }
                    Err(e) => emit(Event::PushUndone(Err(e.to_string()))),
                }
            }
            Command::Run(albums) => {
                let Some((req, planned)) = last.take() else {
                    continue;
                };
                // A panic ends this import, not the thread: the screen
                // says so instead of waiting for ever. What was copied stays
                // (each step's records go with its copies); its batch is
                // left for crash recovery to finish at the next start.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(
                        source, library, staging, &req, &planned, &albums, emit, stop,
                    )
                }));
                match r {
                    Ok(Ok(r)) => emit(Event::Done(r)),
                    Ok(Err(e)) => emit(Event::Failed(e.to_string())),
                    Err(_) => emit(Event::Failed(
                        "The import stopped unexpectedly. What was copied is kept: Find photos \
                         again to copy the rest."
                            .into(),
                    )),
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    source: &dyn PhotoSource,
    library: &Sender<LibraryCommand>,
    staging: &std::path::Path,
    req: &PlanRequest,
    planned: &Planned,
    albums: &HashMap<String, AlbumChoice>,
    emit: &(dyn Fn(Event) + Sync),
    stop: &AtomicBool,
) -> Result<Report> {
    let _awake = crate::platform::keep_awake("Copying photos from iCloud Photos");
    let template = Template::parse(&req.template)?;
    let mut ledgers = Ledgers::load(&req.roots);
    let mut cx = Context {
        root: &req.root,
        template: &template,
        options: &req.options,
        albums,
        tz: jiff::tz::TimeZone::system(),
        index: LibraryIndex::new(&req.catalog),
        events: shoebox_core::events::load(&req.roots),
        taken: Default::default(),
        seq: 0,
    };
    let worker = Worker(library);
    let run_id = jiff::Zoned::now().strftime("%Y%m%dT%H%M%S").to_string();
    let job = Job {
        source,
        library: &planned.library,
        catalog: &req.catalog,
        batches: &worker,
        roots: &req.roots,
        writable: &req.writable,
        staging,
        run: &run_id,
        label: "copy from iCloud Photos",
        step: STEP,
        parallel: PARALLEL,
        min_free: MIN_FREE,
    };
    // At most ~10 updates a second, and every finished asset.
    let last = std::sync::Mutex::new((Instant::now(), usize::MAX));
    let progress = |p: &Progress| {
        if let Ok(mut g) = last.lock() {
            if g.1 == p.done && g.0.elapsed() < Duration::from_millis(100) {
                return;
            }
            *g = (Instant::now(), p.done);
        }
        emit(Event::Progress(p.clone()));
    };
    sync::run(&job, &planned.plan, &mut ledgers, &mut cx, &progress, stop)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use shoebox_core::photos::fake::FakeSource;
    use shoebox_core::photos::{Asset, AssetId, Media, Resource, ResourceKind};

    fn photo(n: u32) -> (Asset, Vec<u8>) {
        let bytes = format!("not really photo {n}").into_bytes();
        let a = Asset {
            id: AssetId {
                cloud: Some(format!("C{n}")),
                local: format!("L{n}"),
            },
            media: Media::Photo,
            created: Some(format!("2024-06-0{n}T10:00:00Z").parse().unwrap()),
            modified: None,
            location: None,
            favourite: n == 1,
            hidden: false,
            burst: None,
            live: false,
            width: 10,
            height: 10,
            resources: vec![Resource {
                kind: ResourceKind::Original,
                file_name: format!("IMG_000{n}.JPG"),
                size: Some(bytes.len() as u64),
            }],
            albums: vec![],
            edited: None,
        };
        (a, bytes)
    }

    fn wait<T>(rx: &Receiver<Event>, mut f: impl FnMut(Event) -> Option<T>) -> T {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(e) => {
                    if let Event::Failed(why) = &e {
                        panic!("failed: {why}");
                    }
                    if let Some(t) = f(e) {
                        return t;
                    }
                }
                Err(_) => panic!("timed out"),
            }
        }
    }

    /// A sync through the library worker: its copies land in one batch the
    /// worker can undo, and a second plan has nothing new.
    #[test]
    fn syncs_through_the_library_worker() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let root = base.join("lib");
        std::fs::create_dir_all(&root).unwrap();
        let lib = crate::jobs::Library::new(
            crate::jobs::LibraryPaths {
                settings: base.join("settings.json"),
                cache_db: base.join("cache.sqlite"),
                journal: base.join("journal.sqlite"),
                trash_dir: Some(base.join("trash")),
            },
            crate::activity::Board::default(),
            || {},
        );
        let fake = FakeSource::new();
        fake.answer(Access::Full);
        for n in 1..=3 {
            let (a, b) = photo(n);
            fake.add(a, vec![b]);
        }
        let photos = Photos::new(
            Arc::new(fake),
            lib.sender(),
            base.join("cache/staging"),
            || {},
        );
        let req = PlanRequest {
            root: root.clone(),
            roots: vec![root.clone()],
            writable: vec![root.clone()],
            template: shoebox_core::template::DEFAULT_IMPORT.into(),
            options: Options::default(),
            catalog: Arc::new(Catalog::default()),
        };
        photos.send(Command::Plan(Box::new(req.clone())));
        let planned = wait(&photos.events, |e| match e {
            Event::Planned(p) => Some(p),
            _ => None,
        });
        assert_eq!(planned.plan.new.len(), 3);
        photos.send(Command::Run(HashMap::new()));
        let report = wait(&photos.events, |e| match e {
            Event::Done(r) => Some(r),
            _ => None,
        });
        assert_eq!(report.copied, 3, "{report:?}");
        let batch = report.batch.unwrap();
        // Named by Photos' dates (the fake photos have no EXIF), in the
        // Mac's time zone: count them rather than name them.
        let copied = std::fs::read_dir(root.join("2024"))
            .unwrap()
            .flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap())
            .filter(|f| {
                f.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|e| e == "jpg")
            })
            .count();
        assert_eq!(copied, 3);
        let j = shoebox_core::fileops::Journal::open(&base.join("journal.sqlite")).unwrap();
        let b = j.batch(batch).unwrap().unwrap();
        assert_eq!(b.status, "done");
        photos.send(Command::Plan(Box::new(req)));
        let again = wait(&photos.events, |e| match e {
            Event::Planned(p) => Some(p),
            _ => None,
        });
        assert!(again.plan.new.is_empty());
        assert_eq!(again.plan.synced, 3);
    }
}
