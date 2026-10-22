//! Photo merges in the app (EDITING-SPEC, Phase 4 additions; DECISIONS,
//! "Photo merges: groundwork"): the job the dialog (`views::merge`) sends,
//! run on the develop thread's merge thread. It's planned first (where
//! the file goes, the sources' BLAKE3s, refusals), High efficiency NEFs go
//! through their converted DNGs, the merge runs, and its DNG is written in
//! the cache and hashed. The library worker then adds it beside its first
//! source as one journaled batch (`LibraryCommand::AddMerge`): never
//! replacing a file, the sources only read, ⌘Z sending it to the Trash.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use shoebox_core::fileops::PlannedCopy;
use shoebox_core::merges::{self, Kind};
use shoebox_core::scan::Root;
use shoebox_develop::dng::{Converter, DngCache};
use shoebox_develop::merge::{self as dm, MergeError, focus, hdr};

use crate::develop::Event;

/// A merge the person asked for.
#[derive(Clone)]
pub struct Job {
    pub kind: Kind,
    /// Each photo and its EXIF orientation, in the order to merge (the
    /// first is where the file goes).
    pub sources: Vec<(PathBuf, u16)>,
    pub options: hdr::Options,
    /// A focus stack's options.
    pub focus: focus::Options,
    /// The library's folders.
    pub roots: Vec<Root>,
    pub cancel: Arc<AtomicBool>,
}

/// A merge written in the cache, for the library worker to add.
#[derive(Debug, Clone)]
pub struct Done {
    pub copy: PlannedCopy,
    pub plan: merges::Plan,
    /// Worth telling the person (a frame that couldn't be aligned).
    pub notes: Vec<String>,
}

/// Run a merge, telling `emit` how it goes (`MergeProgress`) and how it
/// ended (`MergeDone`).
pub fn run(
    job: &Job,
    converter: Option<&Path>,
    dngs: &DngCache,
    staging: &Path,
    emit: &(dyn Fn(Event) + Sync),
) {
    let r = merge(job, converter, dngs, staging, emit);
    emit(Event::MergeDone(r));
}

fn merge(
    job: &Job,
    converter: Option<&Path>,
    dngs: &DngCache,
    staging: &Path,
    emit: &(dyn Fn(Event) + Sync),
) -> Result<Done, String> {
    emit(Event::MergeProgress(0.0));
    let paths: Vec<PathBuf> = job.sources.iter().map(|(p, _)| p.clone()).collect();
    let plan = merges::plan(&job.roots, job.kind, &paths).map_err(|e| e.to_string())?;
    let mut inputs = Vec::with_capacity(job.sources.len());
    for (path, orientation) in &job.sources {
        inputs.push(dm::Input {
            path: path.clone(),
            raw_path: raw_path(path, converter, dngs)?,
            orientation: *orientation,
        });
    }
    let progress = |f: f32| emit(Event::MergeProgress(0.05 + 0.85 * f));
    // The result, and why each frame fell short (None: fine).
    let (merged, failed): (dm::Merged, Vec<Option<&str>>) = match job.kind {
        Kind::Hdr => hdr::merge(&inputs, &job.options, &progress, &job.cancel)
            .map(|o| (o.merged, o.frames.iter().map(|f| f.align_failed).collect())),
        Kind::Focus => focus::merge(&inputs, &job.focus, &progress, &job.cancel)
            .map(|o| (o.merged, o.frames.iter().map(|f| f.align_failed).collect())),
        Kind::Panorama => Err(MergeError::Refused(format!(
            "{} isn't built yet",
            job.kind.label()
        ))),
    }
    .map_err(|e| e.to_string())?;
    let mut notes = Vec::new();
    for (f, (p, _)) in failed.iter().zip(&job.sources) {
        if let Some(why) = f {
            notes.push(format!("{} {why}", name(p)));
        }
    }
    let record = dm::Record {
        kind: job.kind.key().to_owned(),
        sources: plan
            .sources
            .iter()
            .map(|(p, h)| (name(p), blake3::Hash::from_bytes(*h).to_hex().to_string()))
            .collect(),
    };
    let bytes = dm::dng(&merged, &record).map_err(|e| e.to_string())?;
    drop(merged);
    emit(Event::MergeProgress(0.95));
    let stem = plan
        .dst
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "merge".into());
    let staged = shoebox_develop::export::write_new(staging, &stem, "dng", &bytes)?;
    let hash = *blake3::hash(&bytes).as_bytes();
    Ok(Done {
        copy: PlannedCopy {
            src: staged,
            dst: plan.dst.clone(),
            hash,
        },
        plan,
        notes,
    })
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The file to decode a photo from: itself, or a High efficiency NEF's
/// DNG (converted now if need be).
fn raw_path(path: &Path, converter: Option<&Path>, dngs: &DngCache) -> Result<PathBuf, String> {
    let is_nef = shoebox_decode::Format::from_path(path) == Some(shoebox_decode::Format::Nef);
    if !is_nef {
        return Ok(path.to_path_buf());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !shoebox_develop::dng::needs_dng(&bytes) {
        return Ok(path.to_path_buf());
    }
    let conv = Converter::find(converter).ok_or_else(|| {
        format!(
            "{} is a High efficiency NEF: merging it needs Adobe DNG Converter",
            name(path)
        )
    })?;
    dngs.get_or_convert(&conv, path, &bytes)
        .map_err(|e| e.to_string())
}

/// Where merges are written before the library worker copies them in.
pub fn staging(cache_dir: &Path) -> PathBuf {
    cache_dir.join("merge-staging")
}
