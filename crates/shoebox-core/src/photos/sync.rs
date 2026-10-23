//! Running a sync: download each planned asset's files into the cache,
//! place them, and copy them in a few assets at a time, all in one batch
//! (one History entry; undo reverses the whole sync). Each step writes its
//! assets' records with its copies, so a sync that stops (cancelled, the
//! app quit, a crash) loses at most the step in flight, and the next sync
//! carries on from there.
//!
//! Assets synced before that joined albums or were favourited in Photos
//! get those marks in their existing sidecars (never a reference root's),
//! in the first step.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::ledger::{self, AlbumChoice, AlbumUse, Entry, Ledgers};
use super::model::{AssetId, Library, ResourceKind};
use super::place::{self, Context, Staged, album_parts, favourite};
use super::plan::Plan;
use super::source::PhotoSource;
use crate::catalog::Catalog;
use crate::error::{CoreError, Result, io};
use crate::events;
use crate::fileops::{FileOps, PlannedCopy};
use crate::sidecar;

/// Where a sync stands.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    /// Assets finished (copied, found in the library, or failed).
    pub done: usize,
    pub total: usize,
    /// The file downloading, and how far it is (0–1).
    pub current: Option<String>,
    pub fraction: f64,
    /// Bytes downloaded so far.
    pub bytes: u64,
}

/// What a sync did.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The sync's batch (None if there was nothing to do).
    pub batch: Option<i64>,
    /// Assets with files copied in.
    pub copied: usize,
    /// Files copied.
    pub files: usize,
    /// Assets whose files were all in the library already.
    pub in_library: usize,
    /// Assets that failed, with why (not recorded: tried again next time).
    pub failed: Vec<(AssetId, String)>,
    /// Dated by Photos (the file had no date, or Photos' was changed).
    pub dated_by_photos: usize,
    pub undated: usize,
    /// Library photos whose album events, keywords or favourite marks
    /// changed to follow Photos.
    pub marked: usize,
    /// Events renamed to follow their albums in Photos.
    pub renamed: usize,
    /// Stopped before the end, as asked.
    pub stopped: bool,
    /// Stopped before the end: too little free space.
    pub low_space: bool,
}

/// Where a sync's steps run: [`FileOps`] itself, or (in the app) the
/// library worker, which owns it.
pub trait Batches {
    /// Begin the sync's batch.
    fn begin(&self, label: &str) -> Result<i64>;
    /// Copy and write one step (see [`FileOps::import_step`]).
    fn step(
        &self,
        batch: i64,
        copies: Vec<PlannedCopy>,
        writes: Vec<(PathBuf, Vec<u8>)>,
    ) -> Result<()>;
    /// Close the batch.
    fn end(&self, batch: i64) -> Result<()>;
}

impl Batches for FileOps {
    fn begin(&self, label: &str) -> Result<i64> {
        self.begin_import(label)
    }

    fn step(
        &self,
        batch: i64,
        copies: Vec<PlannedCopy>,
        writes: Vec<(PathBuf, Vec<u8>)>,
    ) -> Result<()> {
        self.import_step(batch, &copies, writes, &|_| {}, &AtomicBool::new(false))
    }

    fn end(&self, batch: i64) -> Result<()> {
        self.end_import(batch)
    }
}

/// What a sync works with.
pub struct Job<'a> {
    pub source: &'a dyn PhotoSource,
    pub library: &'a Library,
    pub catalog: &'a Catalog,
    pub batches: &'a dyn Batches,
    /// Every root's path, in the order the ledgers were loaded.
    pub roots: &'a [PathBuf],
    /// Roots that can be written (events go to each).
    pub writable: &'a [PathBuf],
    /// A folder in the cache for downloads (made if needed; emptied).
    pub staging: &'a Path,
    /// The sync's start (names its record files).
    pub run: &'a str,
    pub label: &'a str,
    /// Assets per step.
    pub step: usize,
    /// Downloads at once.
    pub parallel: usize,
    /// Stop before a step when the library's or the cache's volume has less
    /// free than this (bytes; 0: don't check).
    pub min_free: u64,
}

/// What the download threads need (all of it shareable).
#[derive(Clone, Copy)]
struct Fetch<'a> {
    source: &'a dyn PhotoSource,
    library: &'a Library,
    staging: &'a Path,
    parallel: usize,
}

/// An asset's files, read (and its file list), or why they didn't arrive.
type Fetched = std::result::Result<(Vec<Staged>, Vec<super::model::Resource>), Missed>;

/// Why an asset's files didn't arrive.
enum Missed {
    Stopped,
    Failed(String),
}

/// Download and read one asset's wanted files into the staging folder.
fn fetch_one(
    job: Fetch<'_>,
    planned: &super::plan::Planned,
    shared: &std::sync::Mutex<(Progress, Vec<PathBuf>)>,
    progress: &(dyn Fn(&Progress) + Sync),
    stop: &AtomicBool,
) -> Fetched {
    let asset = &job.library.assets[planned.asset];
    // Listed without its files: look them up now.
    let files = if asset.resources.is_empty() {
        job.source
            .resources(&asset.id)
            .map_err(|e| Missed::Failed(e.to_string()))?
    } else {
        asset.resources.clone()
    };
    if !files.iter().any(|r| r.kind == ResourceKind::Original) {
        return Err(Missed::Failed("Photos lists no original for it".into()));
    }
    let mut staged = Vec::new();
    for kind in &planned.resources {
        if stop.load(Ordering::Relaxed) {
            return Err(Missed::Stopped);
        }
        // A kind it may have (a RAW+JPEG pair's other half) but hasn't.
        let Some(res) = files.iter().find(|r| r.kind == *kind) else {
            continue;
        };
        let name = res.file_name.clone();
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e)
            .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("bin");
        let to = job
            .staging
            .join(format!("{}-{kind:?}.{ext}", planned.asset));
        if let Ok(mut g) = shared.lock() {
            g.0.current = Some(name);
            g.0.fraction = 0.0;
            g.1.push(to.clone());
            progress(&g.0);
        }
        let on = |f: f64| {
            if let Ok(mut g) = shared.try_lock() {
                g.0.fraction = f;
                progress(&g.0);
            }
        };
        match job.source.download(&asset.id, *kind, &to, &on, stop) {
            Ok(()) => {}
            Err(CoreError::Cancelled) => return Err(Missed::Stopped),
            Err(e) => return Err(Missed::Failed(e.to_string())),
        }
        let s = Staged::read(*kind, &to).map_err(|e| Missed::Failed(e.to_string()))?;
        if let Ok(mut g) = shared.lock() {
            g.0.bytes += s.size;
        }
        staged.push(s);
    }
    Ok((staged, files))
}

/// Download a step's assets, `job.parallel` at a time; results in order.
fn fetch_step(
    job: Fetch<'_>,
    step: &[super::plan::Planned],
    shared: &std::sync::Mutex<(Progress, Vec<PathBuf>)>,
    progress: &(dyn Fn(&Progress) + Sync),
    stop: &AtomicBool,
) -> Vec<Fetched> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<Option<Fetched>>> =
        step.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..job.parallel.clamp(1, step.len().max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(planned) = step.get(i) else { break };
                    // A panic in one asset's download (PhotoKit giving
                    // something its header says it never does) fails that
                    // asset, not the sync.
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        fetch_one(job, planned, shared, progress, stop)
                    }))
                    .unwrap_or_else(|_| {
                        Err(Missed::Failed(
                            "Photos gave Shoebox something it didn't expect".into(),
                        ))
                    });
                    if let Ok(mut g) = slots[i].lock() {
                        *g = Some(r);
                    }
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|m| {
            m.into_inner()
                .ok()
                .flatten()
                .unwrap_or(Err(Missed::Failed("not downloaded".into())))
        })
        .collect()
}

/// Run `plan` (from [`super::plan::plan`]). `cx.albums` holds the album
/// choices as made in the plan; they're saved in the destination root for
/// the next sync.
pub fn run(
    job: &Job<'_>,
    plan: &Plan,
    ledgers: &mut Ledgers,
    cx: &mut Context<'_>,
    progress: &(dyn Fn(&Progress) + Sync),
    stop: &AtomicBool,
) -> Result<Report> {
    let mut report = Report::default();
    let total = plan.new.len() + plan.updated.len();
    if total == 0
        && plan.joined.is_empty()
        && plan.departed.is_empty()
        && plan.favourited.is_empty()
        && plan.renamed.is_empty()
        && plan.gone.is_empty()
    {
        return Ok(report);
    }
    // Each sync downloads into a folder of its own, and clears what earlier
    // ones left (one that stopped mid-step leaves its files): the cache's
    // own files, so removing them breaks no rule.
    clear_staging(job.staging);
    let staging = job.staging.join(format!("sync-{}", job.run));
    std::fs::create_dir_all(&staging).map_err(io(&staging))?;
    let root_index = |p: &Path| job.roots.iter().position(|r| r == p);
    let dest = root_index(cx.root)
        .ok_or_else(|| CoreError::Invalid(format!("{} isn't a library root", cx.root.display())))?;
    let batch = job.batches.begin(job.label)?;
    report.batch = Some(batch);
    let mut events_seen = cx.events.clone();

    // First step: marks on photos synced before, and the album choices.
    let mut writes = marks_for_synced(job, plan, ledgers, cx, &mut report)?;
    // The album choices go with the last step, when the events new albums
    // make are there to link.
    let choices_base = ledgers.choices.get(dest).cloned().unwrap_or_default();
    let mut records: HashMap<usize, Vec<(AssetId, Entry)>> = HashMap::new();
    for ((root, id), e) in synced_entries(plan, job, ledgers) {
        records.entry(root).or_default().push((id, e));
    }
    // Each root's record file this run is appending to.
    let mut segments: HashMap<usize, ledger::Segment> = HashMap::new();
    let mut flush = |copies: Vec<PlannedCopy>,
                     mut writes: Vec<(PathBuf, Vec<u8>)>,
                     records: &mut HashMap<usize, Vec<(AssetId, Entry)>>,
                     cx: &mut Context<'_>,
                     ledgers: &mut Ledgers|
     -> Result<()> {
        for (root, list) in records.drain() {
            if list.is_empty() {
                continue;
            }
            writes.push(ledger::plan_synced(
                &job.roots[root],
                job.run,
                segments.entry(root).or_default(),
                &list,
            )?);
            for (id, e) in list {
                ledgers.record(root, &id, e);
            }
        }
        if cx.events != events_seen {
            writes.extend(events::plan_save(job.writable, &cx.events)?);
            events_seen = cx.events.clone();
        }
        if copies.is_empty() && writes.is_empty() {
            return Ok(());
        }
        job.batches.step(batch, copies, writes)
    };

    let shared = std::sync::Mutex::new((
        Progress {
            total,
            ..Progress::default()
        },
        Vec::<PathBuf>::new(),
    ));
    if let Ok(g) = shared.lock() {
        progress(&g.0);
    }
    let todo: Vec<_> = plan.downloads().cloned().collect();
    let result = (|| -> Result<()> {
        // The first step's marks and choices go with the first downloads.
        let mut pending = std::mem::take(&mut writes);
        for step in todo.chunks(job.step.max(1)) {
            if stop.load(Ordering::Relaxed) {
                report.stopped = true;
                break;
            }
            if job.min_free > 0 {
                let low = [cx.root, job.staging]
                    .iter()
                    .any(|p| crate::fileops::free_bytes(p).is_ok_and(|free| free < job.min_free));
                if low {
                    report.low_space = true;
                    break;
                }
            }
            let fetch = Fetch {
                source: job.source,
                library: job.library,
                staging: &staging,
                parallel: job.parallel,
            };
            let fetched = fetch_step(fetch, step, &shared, progress, stop);
            let mut copies = Vec::new();
            for (planned, got) in step.iter().zip(fetched) {
                let listed = &job.library.assets[planned.asset];
                let (staged, files) = match got {
                    Ok(s) => s,
                    Err(Missed::Stopped) => {
                        report.stopped = true;
                        continue;
                    }
                    Err(Missed::Failed(why)) => {
                        tracing::warn!("photos: {} failed: {why}", listed.id.local);
                        report.failed.push((listed.id.clone(), why));
                        continue;
                    }
                };
                let with_files;
                let asset = if listed.resources.is_empty() {
                    with_files = super::model::Asset {
                        resources: files,
                        ..listed.clone()
                    };
                    &with_files
                } else {
                    listed
                };
                let previous = ledgers.find(&asset.id);
                let prev = previous.map(|(r, e)| (job.roots[r].as_path(), e));
                let placed = place::place(cx, asset, prev, &staged)?;
                let root = root_index(&placed.root).unwrap_or(dest);
                if placed.copies.is_empty() {
                    if previous.is_none() {
                        report.in_library += 1;
                    }
                } else {
                    report.copied += 1;
                    report.files += placed.copies.len();
                    report.dated_by_photos += usize::from(placed.dated_by_photos);
                    report.undated += usize::from(placed.undated);
                }
                copies.extend(placed.copies);
                pending.extend(placed.sidecars);
                if let Some(e) = placed.entry {
                    records.entry(root).or_default().push((asset.id.clone(), e));
                }
            }
            if let Ok(mut g) = shared.lock() {
                g.0.done += step.len();
                progress(&g.0);
            }
            flush(
                copies,
                std::mem::take(&mut pending),
                &mut records,
                cx,
                ledgers,
            )?;
            remove_staged(&shared);
            if report.stopped {
                break;
            }
        }
        // Marks and records with no downloads after them (or none at all),
        // and the album choices.
        let choices = ledger::Choices {
            albums: choices_to_keep(plan, cx),
            ..choices_base.clone()
        };
        pending.push(ledger::plan_choices(cx.root, &choices)?);
        flush(Vec::new(), pending, &mut records, cx, ledgers)
    })();
    // The cache's own files: removing them breaks no rule.
    remove_staged(&shared);
    let _ = std::fs::remove_dir_all(&staging);
    match result {
        Ok(()) => {
            job.batches.end(batch)?;
            Ok(report)
        }
        // A step that failed or was stopped closed the batch itself.
        Err(e @ (CoreError::BatchFailed { .. } | CoreError::BatchCancelled { .. })) => Err(e),
        Err(e) => {
            job.batches.end(batch)?;
            Err(e)
        }
    }
}

/// Remove earlier syncs' downloads from the staging folder: loose files
/// (from before syncs had folders) and `sync-*` folders. Other folders
/// (Send to Photos' `push`) are left alone.
fn clear_staging(staging: &Path) {
    let Ok(rd) = std::fs::read_dir(staging) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let is_sync = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("sync-"));
        match e.file_type() {
            Ok(t) if t.is_file() => {
                let _ = std::fs::remove_file(&p);
            }
            Ok(t) if t.is_dir() && is_sync => {
                let _ = std::fs::remove_dir_all(&p);
            }
            _ => {}
        }
    }
}

fn remove_staged(shared: &std::sync::Mutex<(Progress, Vec<PathBuf>)>) {
    if let Ok(mut g) = shared.lock() {
        for f in g.1.drain(..) {
            let _ = std::fs::remove_file(f);
        }
    }
}

/// Records for assets synced before whose albums or favourite changed and
/// that download nothing: (root, id) → updated entry.
fn synced_entries(plan: &Plan, job: &Job<'_>, ledgers: &Ledgers) -> Vec<((usize, AssetId), Entry)> {
    let downloading: std::collections::HashSet<usize> =
        plan.updated.iter().map(|p| p.asset).collect();
    let mut out: BTreeMap<usize, ((usize, AssetId), Entry)> = BTreeMap::new();
    let changed = plan
        .joined
        .iter()
        .chain(&plan.departed)
        .map(|(i, _)| *i)
        .chain(plan.favourited.iter().copied());
    for i in changed {
        if downloading.contains(&i) {
            continue; // its record comes with the download
        }
        let a = &job.library.assets[i];
        if let Some((root, e)) = ledgers.find(&a.id) {
            let mut e = e.clone();
            e.albums = a.albums.clone();
            e.favourite = a.favourite;
            e.updated = events::now_ms();
            out.insert(i, ((root, a.id.clone()), e));
        }
    }
    out.into_values().collect()
}

/// An album's choice: as made in this sync's plan, else as remembered.
fn choice_of<'a>(cx: &'a Context<'_>, plan: &'a Plan, album: &str) -> Option<&'a AlbumChoice> {
    cx.albums.get(album).or_else(|| plan.remembered.get(album))
}

/// The event an album made (its linked id, if still there, else by name).
pub(crate) fn event_of(c: &AlbumChoice, events: &[events::Event]) -> Option<String> {
    c.event
        .clone()
        .filter(|id| events.iter().any(|e| &e.id == id))
        .or_else(|| events::find_path(events, &album_parts(c)))
}

/// Follow albums renamed in Photos: an event an album made takes the new
/// name (and folders), unless it was renamed in Shoebox since. Returns how
/// many events were renamed; `cx.events` holds the new names.
fn follow_renames(plan: &Plan, cx: &mut Context<'_>) -> usize {
    let mut n = 0;
    for r in &plan.renamed {
        let Some(was) = plan.remembered.get(&r.id) else {
            continue;
        };
        if was.kind != AlbumUse::Event {
            continue;
        }
        let Some(id) = event_of(was, &cx.events) else {
            continue;
        };
        // Renamed in Shoebox since: leave it.
        let current: Vec<String> = events::path_of(&cx.events, &id)
            .split('|')
            .map(str::to_owned)
            .collect();
        if current != album_parts(was) {
            continue;
        }
        let to: Vec<String> =
            r.to.split('›')
                .map(|p| p.trim().replace('|', "-"))
                .filter(|p| !p.is_empty())
                .collect();
        let Some((leaf, parents)) = to.split_last() else {
            continue;
        };
        let parent = if parents.is_empty() {
            None
        } else {
            events::ensure_path(&mut cx.events, parents)
        };
        if let Some(e) = cx.events.iter_mut().find(|e| e.id == id) {
            e.name = leaf.clone();
            e.parent = parent;
            e.updated = events::now_ms();
            n += 1;
        }
    }
    n
}

/// Sidecar edits for library photos of assets synced before, following
/// Photos: renamed albums' events and keywords, albums joined and left,
/// favourites. One write a photo, whatever changed. A rating set in Shoebox
/// is never replaced; reference roots are never written.
fn marks_for_synced(
    job: &Job<'_>,
    plan: &Plan,
    ledgers: &Ledgers,
    cx: &mut Context<'_>,
    report: &mut Report,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let by_path: HashMap<&Path, usize> = job
        .catalog
        .photos
        .iter()
        .enumerate()
        .map(|(i, p)| (p.path.as_path(), i))
        .collect();
    // Each library file of an asset's record.
    let files_of = |root: usize, e: &Entry| -> Vec<usize> {
        e.files
            .iter()
            .filter(|f| f.kind != ResourceKind::PairedVideo)
            .filter_map(|f| {
                let path = if Path::new(&f.rel).is_absolute() {
                    PathBuf::from(&f.rel)
                } else {
                    job.roots[root].join(&f.rel)
                };
                by_path.get(path.as_path()).copied()
            })
            .filter(|&pi| !job.catalog.is_reference(pi))
            .collect()
    };
    let mut updated: BTreeMap<usize, crate::marks::Marks> = BTreeMap::new();

    // Renames: events (their keywords on every member), and keywords.
    let before = cx.events.clone();
    report.renamed = follow_renames(plan, cx);
    if report.renamed > 0 {
        for (pi, m) in events::mark_updates(job.catalog, &before, &cx.events, None, false) {
            if !job.catalog.is_reference(pi) {
                updated.insert(pi, m);
            }
        }
    }
    let keyword = |c: &AlbumChoice| format!("Albums|{}", album_parts(c).join("|"));
    let renamed_keywords: Vec<(String, String, String)> = plan
        .renamed
        .iter()
        .filter_map(|r| {
            let was = plan
                .remembered
                .get(&r.id)
                .filter(|c| c.kind == AlbumUse::Keyword)?;
            let now = choice_of(cx, plan, &r.id)?;
            (keyword(was) != keyword(now)).then(|| (r.id.clone(), keyword(was), keyword(now)))
        })
        .collect();
    if !renamed_keywords.is_empty() {
        for (_, root, e) in ledgers.iter() {
            for (album, from, to) in &renamed_keywords {
                if !e.albums.contains(album) {
                    continue;
                }
                for pi in files_of(root, e) {
                    let m = updated
                        .entry(pi)
                        .or_insert_with(|| job.catalog.photos[pi].marks.clone());
                    for k in &mut m.keywords {
                        if k == from {
                            k.clone_from(to);
                        }
                    }
                }
            }
        }
    }

    // Albums joined and left, favourites.
    #[derive(Default)]
    struct Change {
        joined: Vec<String>,
        left: Vec<String>,
        favourited: bool,
    }
    let mut changes: BTreeMap<usize, Change> = BTreeMap::new();
    for (i, albums) in &plan.joined {
        changes.entry(*i).or_default().joined = albums.clone();
    }
    for (i, albums) in &plan.departed {
        changes.entry(*i).or_default().left = albums.clone();
    }
    for i in &plan.favourited {
        changes.entry(*i).or_default().favourited = true;
    }
    let mut marked = std::collections::HashSet::new();
    for (i, ch) in changes {
        let a = &job.library.assets[i];
        let Some((root, e)) = ledgers.find(&a.id) else {
            continue;
        };
        let (mut join, mut add_kw) = (Vec::new(), Vec::new());
        for album in &ch.joined {
            let Some(c) = choice_of(cx, plan, album).cloned() else {
                continue;
            };
            let parts = album_parts(&c);
            if parts.is_empty() {
                continue;
            }
            match c.kind {
                AlbumUse::Event => join.extend(
                    event_of(&c, &cx.events)
                        .or_else(|| events::ensure_path(&mut cx.events, &parts)),
                ),
                AlbumUse::Keyword => add_kw.push(keyword(&c)),
                AlbumUse::Nothing => {}
            }
        }
        let (mut leave, mut drop_kw) = (Vec::new(), Vec::new());
        // An album deleted in Photos: its photos stay in its event.
        let gone: std::collections::HashSet<&str> =
            plan.gone.iter().map(|(id, _)| id.as_str()).collect();
        for album in ch.left.iter().filter(|a| !gone.contains(a.as_str())) {
            let Some(c) = choice_of(cx, plan, album) else {
                continue;
            };
            match c.kind {
                AlbumUse::Event => leave.extend(event_of(c, &cx.events)),
                AlbumUse::Keyword => drop_kw.push(keyword(c)),
                AlbumUse::Nothing => {}
            }
        }
        for pi in files_of(root, e) {
            let photo = &job.catalog.photos[pi];
            let base = updated
                .get(&pi)
                .cloned()
                .unwrap_or_else(|| photo.marks.clone());
            let mut m = events::leave(&base, &leave, &cx.events);
            m = events::join(&m, &join, &cx.events);
            m.keywords.retain(|k| !drop_kw.contains(k));
            for k in &add_kw {
                if !m.keywords.contains(k) {
                    m.keywords.push(k.clone());
                }
            }
            if ch.favourited && photo.marks.rating == 0 {
                favourite(&mut m, cx.options.favourite);
            }
            if m != base {
                marked.insert(pi);
            }
            updated.insert(pi, m);
        }
    }
    report.marked = marked.len();
    let mut writes = Vec::new();
    for (pi, m) in updated {
        let photo = &job.catalog.photos[pi];
        writes.extend(sidecar::plan_write(&photo.path, &photo.marks, &m, false)?);
    }
    Ok(writes)
}

/// The album choices to remember: this sync's, and earlier ones of albums
/// still in Photos without photos now; each with its event's id.
fn choices_to_keep(plan: &Plan, cx: &Context<'_>) -> BTreeMap<String, AlbumChoice> {
    let gone: std::collections::HashSet<&str> =
        plan.gone.iter().map(|(id, _)| id.as_str()).collect();
    let mut out: BTreeMap<String, AlbumChoice> = plan
        .remembered
        .iter()
        .filter(|(id, _)| !gone.contains(id.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in cx.albums {
        let mut v = v.clone();
        if v.event.is_none() {
            v.event = plan.remembered.get(k).and_then(|c| c.event.clone());
        }
        out.insert(k.clone(), v);
    }
    for c in out.values_mut() {
        // The event it's linked to (renamed in Shoebox or not), else the
        // one of its name.
        c.event = (c.kind == AlbumUse::Event)
            .then(|| event_of(c, &cx.events))
            .flatten();
    }
    out
}
