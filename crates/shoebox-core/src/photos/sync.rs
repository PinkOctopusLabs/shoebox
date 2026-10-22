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

use super::ledger::{self, AlbumUse, Entry, Ledgers};
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
    /// Library photos given album events or favourite marks.
    pub marked: usize,
    /// Stopped before the end, as asked.
    pub stopped: bool,
}

/// What a sync works with.
pub struct Job<'a> {
    pub source: &'a dyn PhotoSource,
    pub library: &'a Library,
    pub catalog: &'a Catalog,
    pub fileops: &'a FileOps,
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
    if total == 0 && plan.joined.is_empty() && plan.favourited.is_empty() {
        return Ok(report);
    }
    std::fs::create_dir_all(job.staging).map_err(io(job.staging))?;
    let root_index = |p: &Path| job.roots.iter().position(|r| r == p);
    let dest = root_index(cx.root)
        .ok_or_else(|| CoreError::Invalid(format!("{} isn't a library root", cx.root.display())))?;
    let batch = job.fileops.begin_import(job.label)?;
    report.batch = Some(batch);
    let events_before = cx.events.clone();

    // First step: marks on photos synced before, and the album choices.
    let mut writes = marks_for_synced(job, plan, ledgers, cx, &mut report)?;
    let mut choices = ledgers.choices.get(dest).cloned().unwrap_or_default();
    choices.albums = cx
        .albums
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<BTreeMap<_, _>>();
    writes.push(ledger::plan_choices(cx.root, &choices)?);
    let mut records: HashMap<usize, Vec<(AssetId, Entry)>> = HashMap::new();
    for ((root, id), e) in synced_entries(plan, job, ledgers) {
        records.entry(root).or_default().push((id, e));
    }
    let mut step_n = 0usize;
    let mut flush = |copies: Vec<PlannedCopy>,
                     mut writes: Vec<(PathBuf, Vec<u8>)>,
                     records: &mut HashMap<usize, Vec<(AssetId, Entry)>>,
                     cx: &mut Context<'_>,
                     ledgers: &mut Ledgers,
                     events_before: &mut Vec<events::Event>|
     -> Result<()> {
        step_n += 1;
        for (root, list) in records.drain() {
            if list.is_empty() {
                continue;
            }
            writes.push(ledger::plan_synced(
                &job.roots[root],
                job.run,
                step_n,
                &list,
            )?);
            for (id, e) in list {
                ledgers.record(root, &id, e);
            }
        }
        if cx.events != *events_before {
            writes.extend(events::plan_save(job.writable, &cx.events)?);
            *events_before = cx.events.clone();
        }
        if copies.is_empty() && writes.is_empty() {
            return Ok(());
        }
        job.fileops
            .import_step(batch, &copies, writes, &|_| {}, &AtomicBool::new(false))
    };
    let mut events_seen = events_before;

    let mut p = Progress {
        total,
        ..Progress::default()
    };
    progress(&p);
    let todo: Vec<_> = plan.downloads().cloned().collect();
    let mut copies: Vec<PlannedCopy> = Vec::new();
    let mut staged_files: Vec<PathBuf> = Vec::new();
    let mut in_step = 0usize;
    let result = (|| -> Result<()> {
        for planned in &todo {
            if stop.load(Ordering::Relaxed) {
                report.stopped = true;
                break;
            }
            let asset = &job.library.assets[planned.asset];
            let mut staged = Vec::new();
            let mut failed = None;
            for kind in &planned.resources {
                let name = asset
                    .resource(*kind)
                    .map(|r| r.file_name.clone())
                    .unwrap_or_default();
                let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("bin");
                let to = job
                    .staging
                    .join(format!("{}-{kind:?}.{ext}", planned.asset));
                p.current = Some(name);
                p.fraction = 0.0;
                progress(&p);
                let r = job.source.download(&asset.id, *kind, &to, &|_| {}, stop);
                match r {
                    Ok(()) => {
                        staged_files.push(to.clone());
                        match Staged::read(*kind, &to) {
                            Ok(s) => {
                                p.bytes += s.size;
                                staged.push(s);
                            }
                            Err(e) => {
                                failed = Some(e.to_string());
                                break;
                            }
                        }
                    }
                    Err(CoreError::Cancelled) => {
                        report.stopped = true;
                        return Ok(());
                    }
                    Err(e) => {
                        failed = Some(e.to_string());
                        break;
                    }
                }
            }
            p.done += 1;
            if let Some(why) = failed {
                tracing::warn!("photos: {} failed: {why}", asset.id.local);
                report.failed.push((asset.id.clone(), why));
                progress(&p);
                continue;
            }
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
            }
            report.dated_by_photos +=
                usize::from(placed.dated_by_photos && !placed.copies.is_empty());
            report.undated += usize::from(placed.undated && !placed.copies.is_empty());
            copies.extend(placed.copies);
            writes.extend(placed.sidecars);
            if let Some(e) = placed.entry {
                records.entry(root).or_default().push((asset.id.clone(), e));
            }
            in_step += 1;
            progress(&p);
            if in_step >= job.step.max(1) {
                flush(
                    std::mem::take(&mut copies),
                    std::mem::take(&mut writes),
                    &mut records,
                    cx,
                    ledgers,
                    &mut events_seen,
                )?;
                for f in staged_files.drain(..) {
                    let _ = std::fs::remove_file(f);
                }
                in_step = 0;
            }
        }
        Ok(())
    })();
    let last = result.and_then(|()| {
        flush(
            std::mem::take(&mut copies),
            std::mem::take(&mut writes),
            &mut records,
            cx,
            ledgers,
            &mut events_seen,
        )
    });
    // The cache's own files: removing them breaks no rule.
    for f in staged_files.drain(..) {
        let _ = std::fs::remove_file(f);
    }
    match last {
        Ok(()) => {
            job.fileops.end_import(batch)?;
            Ok(report)
        }
        // A step that failed or was stopped closed the batch itself.
        Err(e @ (CoreError::BatchFailed { .. } | CoreError::BatchCancelled { .. })) => Err(e),
        Err(e) => {
            job.fileops.end_import(batch)?;
            Err(e)
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

/// Sidecar edits for library photos of assets synced before that joined
/// albums or were favourited in Photos since. A rating set in Shoebox is
/// never replaced; reference roots are never written.
fn marks_for_synced(
    job: &Job<'_>,
    plan: &Plan,
    ledgers: &Ledgers,
    cx: &mut Context<'_>,
    report: &mut Report,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut changes: BTreeMap<usize, (Vec<String>, bool)> = BTreeMap::new();
    for (i, albums) in &plan.joined {
        changes.entry(*i).or_default().0 = albums.clone();
    }
    for i in &plan.favourited {
        changes.entry(*i).or_default().1 = true;
    }
    let by_path: HashMap<&Path, usize> = job
        .catalog
        .photos
        .iter()
        .enumerate()
        .map(|(i, p)| (p.path.as_path(), i))
        .collect();
    let mut writes = Vec::new();
    for (i, (albums, favourited)) in changes {
        let a = &job.library.assets[i];
        let Some((root, e)) = ledgers.find(&a.id) else {
            continue;
        };
        let mut ids = Vec::new();
        let mut keywords = Vec::new();
        for album in &albums {
            let Some(c) = cx.albums.get(album) else {
                continue;
            };
            let parts = album_parts(c);
            if parts.is_empty() {
                continue;
            }
            match c.kind {
                AlbumUse::Event => ids.extend(events::ensure_path(&mut cx.events, &parts)),
                AlbumUse::Keyword => keywords.push(format!("Albums|{}", parts.join("|"))),
                AlbumUse::Nothing => {}
            }
        }
        for f in &e.files {
            let path = if f.kind == ResourceKind::PairedVideo {
                continue;
            } else if Path::new(&f.rel).is_absolute() {
                PathBuf::from(&f.rel)
            } else {
                job.roots[root].join(&f.rel)
            };
            let Some(&pi) = by_path.get(path.as_path()) else {
                continue;
            };
            if job.catalog.is_reference(pi) {
                continue;
            }
            let photo = &job.catalog.photos[pi];
            let mut m = events::join(&photo.marks, &ids, &cx.events);
            for k in &keywords {
                if !m.keywords.contains(k) {
                    m.keywords.push(k.clone());
                }
            }
            if favourited && photo.marks.rating == 0 {
                favourite(&mut m, cx.options.favourite);
            }
            let w = sidecar::plan_write(&photo.path, &photo.marks, &m, false)?;
            if !w.is_empty() {
                writes.extend(w);
                report.marked += 1;
            }
        }
    }
    Ok(writes)
}
