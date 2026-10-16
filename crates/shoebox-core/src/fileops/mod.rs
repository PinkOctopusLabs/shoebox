//! The only module that changes the library on disk.
//!
//! Every operation is journaled before it runs, never overwrites (collisions
//! get `-1`, `-2` suffixes and the rename itself is exclusive), never deletes,
//! never touches reference roots, and can be undone batch by batch.

pub mod journal;
mod sys;

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub use journal::{BatchSummary, Journal, Op, OpRecord, OpStatus};

use crate::error::{CoreError, Result, io};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMove {
    pub src: PathBuf,
    pub dst: PathBuf,
}

/// One step of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Planned {
    Move(PlannedMove),
    /// Replace a sidecar's contents (or create it).
    Sidecar {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    /// Copy, verified against the BLAKE3 the source had when planned.
    Copy {
        src: PathBuf,
        dst: PathBuf,
        hash: [u8; 32],
    },
    /// Move to the Trash, if the file still has this content.
    Trash {
        path: PathBuf,
        hash: [u8; 32],
    },
}

/// Where "Move to Trash" sends files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrashMode {
    /// The system Trash (macOS Trash, freedesktop trash on Linux).
    System,
    /// A folder (tests, so they never touch the real Trash).
    Folder(PathBuf),
}

/// A planned import copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCopy {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub hash: [u8; 32],
}

fn hash_file(p: &Path) -> std::io::Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(p)?)?;
    Ok(*h.finalize().as_bytes())
}

/// Write via a temporary file in the same folder, then rename over the
/// target, so readers never see a half-written sidecar. If the rename
/// fails, the temporary file (ours, just created) is removed.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.shoebox-tmp"));
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub moves: Vec<PlannedMove>,
}

/// `dir/name.ext` → `dir/name-n.ext`. Only the last extension is kept apart,
/// so `a.jpg` becomes `a-1.jpg`.
pub fn suffixed(path: &Path, n: u32) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match path.extension() {
        Some(ext) => format!("{stem}-{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    path.with_file_name(name)
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidecarKind {
    /// `name.ext.xmp`, owned by exactly one file.
    PerFile,
    /// `name.xmp` (Lightroom style), possibly shared by siblings.
    PerStem,
}

/// Existing sidecars that belong to `image` alone. A `name.xmp` is included
/// only if no other file in the folder shares the stem, so moving a JPG never
/// takes its NEF's sidecar with it.
fn sidecars(image: &Path) -> Result<Vec<(PathBuf, SidecarKind)>> {
    let (Some(dir), Some(name), Some(stem)) =
        (image.parent(), image.file_name(), image.file_stem())
    else {
        return Ok(Vec::new());
    };
    let name = name.to_string_lossy().to_lowercase();
    let stem = stem.to_string_lossy().to_lowercase();
    let mut out = Vec::new();
    let mut shared_stem = false;
    let mut stem_sidecar = None;
    for entry in std::fs::read_dir(dir).map_err(io(dir))? {
        let entry = entry.map_err(io(dir))?;
        let fname = entry.file_name().to_string_lossy().to_lowercase();
        if fname == format!("{name}.xmp") {
            out.push((entry.path(), SidecarKind::PerFile));
        } else if fname == format!("{stem}.xmp") {
            stem_sidecar = Some(entry.path());
        } else if fname != name
            && !fname.ends_with(".xmp")
            && Path::new(&fname)
                .file_stem()
                .is_some_and(|s| s.to_string_lossy() == stem)
        {
            shared_stem = true;
        }
    }
    match stem_sidecar {
        Some(p) if !shared_stem => out.push((p, SidecarKind::PerStem)),
        Some(p) => tracing::info!(
            "leaving shared sidecar {} in place (other files share its name)",
            p.display()
        ),
        None => {}
    }
    Ok(out)
}

fn sidecar_dst(image_dst: &Path, kind: SidecarKind, original: &Path) -> PathBuf {
    // Keep the sidecar's own extension spelling (.xmp / .XMP).
    let ext = original
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "xmp".into());
    let base = match kind {
        SidecarKind::PerFile => image_dst.file_name(),
        SidecarKind::PerStem => image_dst.file_stem(),
    }
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default();
    image_dst.with_file_name(format!("{base}.{ext}"))
}

/// Plan moving each `(src, wanted_dst)` with its sidecars. Destinations that
/// are taken on disk or earlier in the plan get a `-N` suffix; the suffix is
/// chosen so the image and all its sidecars are free together.
pub fn plan_moves(files: &[(PathBuf, PathBuf)]) -> Result<Plan> {
    let mut taken: HashSet<PathBuf> = HashSet::new();
    let mut plan = Plan::default();
    for (src, want) in files {
        let cars = sidecars(src)?;
        let mut n = 0;
        let dst = loop {
            let cand = if n == 0 {
                want.clone()
            } else {
                suffixed(want, n)
            };
            let all_free = std::iter::once(cand.clone())
                .chain(cars.iter().map(|(p, k)| sidecar_dst(&cand, *k, p)))
                .all(|p| !exists(&p) && !taken.contains(&p));
            if all_free {
                break cand;
            }
            n += 1;
        };
        taken.insert(dst.clone());
        plan.moves.push(PlannedMove {
            src: src.clone(),
            dst: dst.clone(),
        });
        for (car, kind) in cars {
            let cdst = sidecar_dst(&dst, kind, &car);
            taken.insert(cdst.clone());
            plan.moves.push(PlannedMove {
                src: car,
                dst: cdst,
            });
        }
    }
    Ok(plan)
}

/// Copy `src` to a temporary name beside `dst`, check its BLAKE3, keep the
/// source's modification time, then rename it to `dst` without replacing.
/// On any failure the temporary file (ours) is removed and `dst` untouched.
fn copy_verified(src: &Path, dst: &Path, hash: &[u8; 32]) -> std::result::Result<(), String> {
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dst.with_file_name(format!(".{name}.shoebox-tmp"));
    if exists(&tmp) {
        return Err(format!("{} is in the way", tmp.display()));
    }
    let result = (|| {
        std::fs::copy(src, &tmp).map_err(|e| e.to_string())?;
        if hash_file(&tmp).map_err(|e| e.to_string())? != *hash {
            return Err("the copy doesn't match the source (changed, or a read error)".into());
        }
        if let Ok(m) = std::fs::metadata(src).and_then(|m| m.modified()) {
            let f = std::fs::File::options()
                .write(true)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            f.set_modified(m).map_err(|e| e.to_string())?;
        }
        sys::rename_excl(&tmp, dst).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The system Trash: NSFileManager on macOS (no Finder scripting), the
/// freedesktop trash on Linux.
fn system_trash(path: &Path) -> std::result::Result<(), String> {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx.delete(path).map_err(|e| e.to_string())
}

fn lower_stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Plan moving or renaming whole stacks: each group is the `(src, wanted
/// dst)` of every file of one stack. A group moves together: one `-N`
/// suffix for all its files when anything is taken, so they keep sharing a
/// base name. Each file's `name.ext.xmp` goes with it; a shared `name.xmp`
/// goes along only when every image sharing that name is in the group and
/// they all get the same new name (otherwise it stays, as in [`plan_moves`]).
/// Files already at their wanted path are left out.
pub fn plan_group_moves(groups: &[Vec<(PathBuf, PathBuf)>]) -> Result<Plan> {
    let mut taken: HashSet<PathBuf> = HashSet::new();
    let mut plan = Plan::default();
    for group in groups {
        let group: Vec<&(PathBuf, PathBuf)> = group.iter().filter(|(s, d)| s != d).collect();
        if group.is_empty() {
            continue;
        }
        // Per-file sidecars, and stem sidecars that can move with the group.
        let mut per_file: Vec<Vec<PathBuf>> = Vec::new();
        let mut stem_cars: Vec<(PathBuf, usize)> = Vec::new(); // (sidecar, member it follows)
        for (k, (src, _)) in group.iter().enumerate() {
            let cars = sidecars(src)?;
            per_file.push(
                cars.iter()
                    .filter(|(_, kind)| *kind == SidecarKind::PerFile)
                    .map(|(p, _)| p.clone())
                    .collect(),
            );
            if let Some((dir, stem)) = src.parent().zip(Some(lower_stem(src))) {
                if stem_cars
                    .iter()
                    .any(|(c, _)| c.parent() == Some(dir) && lower_stem(c) == stem)
                {
                    continue;
                }
                let Some(car) = std::fs::read_dir(dir)
                    .map_err(io(dir))?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .find(|p| {
                        p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
                            && p.file_name().is_some_and(|n| {
                                n.to_string_lossy().to_lowercase() == format!("{stem}.xmp")
                            })
                    })
                else {
                    continue;
                };
                // Every image with this stem must be moving, to one new stem.
                let sharing: Vec<PathBuf> = std::fs::read_dir(dir)
                    .map_err(io(dir))?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
                            && lower_stem(p) == stem
                            && p.is_file()
                    })
                    .collect();
                let targets: HashSet<(Option<PathBuf>, String)> = sharing
                    .iter()
                    .filter_map(|p| group.iter().find(|(s, _)| s == p))
                    .map(|(_, d)| (d.parent().map(Path::to_path_buf), lower_stem(d)))
                    .collect();
                let all_moving = sharing.iter().all(|p| group.iter().any(|(s, _)| s == p));
                if all_moving && targets.len() == 1 {
                    stem_cars.push((car, k));
                } else {
                    tracing::info!(
                        "leaving shared sidecar {} in place (not every file sharing it moves together)",
                        car.display()
                    );
                }
            }
        }
        let mut n = 0;
        let dsts = loop {
            let cand: Vec<PathBuf> = group
                .iter()
                .map(|(_, w)| if n == 0 { w.clone() } else { suffixed(w, n) })
                .collect();
            let mut all: Vec<PathBuf> = cand.clone();
            for (k, cars) in per_file.iter().enumerate() {
                all.extend(
                    cars.iter()
                        .map(|c| sidecar_dst(&cand[k], SidecarKind::PerFile, c)),
                );
            }
            all.extend(
                stem_cars
                    .iter()
                    .map(|(c, k)| sidecar_dst(&cand[*k], SidecarKind::PerStem, c)),
            );
            let srcs: HashSet<&PathBuf> = group.iter().map(|(s, _)| s).collect();
            let unique = all.iter().collect::<HashSet<_>>().len() == all.len();
            // A destination that is itself one of the group's sources frees up
            // as the group moves only if it's moved earlier; keep it simple
            // and treat it as taken.
            if unique
                && all
                    .iter()
                    .all(|p| !exists(p) && !taken.contains(p) && !srcs.contains(p))
            {
                taken.extend(all);
                break cand;
            }
            n += 1;
        };
        for (k, (src, _)) in group.iter().enumerate() {
            plan.moves.push(PlannedMove {
                src: src.clone(),
                dst: dsts[k].clone(),
            });
            for c in &per_file[k] {
                plan.moves.push(PlannedMove {
                    src: c.clone(),
                    dst: sidecar_dst(&dsts[k], SidecarKind::PerFile, c),
                });
            }
        }
        for (c, k) in stem_cars {
            plan.moves.push(PlannedMove {
                dst: sidecar_dst(&dsts[k], SidecarKind::PerStem, &c),
                src: c,
            });
        }
    }
    Ok(plan)
}

/// Deepest existing ancestor of `p`, and the missing folders below it (top first).
fn missing_dirs(p: &Path) -> (PathBuf, Vec<PathBuf>) {
    let mut missing = Vec::new();
    let mut cur = p.to_path_buf();
    while !exists(&cur) {
        missing.push(cur.clone());
        match cur.parent() {
            Some(parent) => cur = parent.to_path_buf(),
            None => break,
        }
    }
    missing.reverse();
    (cur, missing)
}

fn device(p: &Path) -> Result<u64> {
    Ok(std::fs::metadata(p).map_err(io(p))?.dev())
}

pub struct FileOps {
    journal: Journal,
    trash: TrashMode,
    /// Canonical reference roots: never moved from, into or renamed.
    protected: Vec<PathBuf>,
}

impl FileOps {
    pub fn new(journal: Journal, protected: Vec<PathBuf>) -> Self {
        Self {
            journal,
            trash: TrashMode::System,
            protected,
        }
    }

    /// Send trashed files to a folder instead of the system Trash.
    pub fn with_trash(mut self, trash: TrashMode) -> Self {
        self.trash = trash;
        self
    }

    /// Replace the reference roots (after the user marks or unmarks one).
    pub fn set_protected(&mut self, protected: Vec<PathBuf>) {
        self.protected = protected;
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    fn check_unprotected(&self, p: &Path) -> Result<()> {
        if let Some(root) = self.protected.iter().find(|r| p.starts_with(r)) {
            return Err(CoreError::Refused(format!(
                "{} is inside reference root {}",
                p.display(),
                root.display()
            )));
        }
        Ok(())
    }

    /// Check every move before anything runs, so a bad plan touches nothing.
    fn preflight(&self, moves: &[PlannedMove]) -> Result<()> {
        let mut dsts = HashSet::new();
        for m in moves {
            self.check_unprotected(&m.src)?;
            self.check_unprotected(&m.dst)?;
            let meta = std::fs::symlink_metadata(&m.src).map_err(io(&m.src))?;
            if !meta.file_type().is_file() {
                return Err(CoreError::Refused(format!(
                    "{} is not a regular file",
                    m.src.display()
                )));
            }
            if exists(&m.dst) {
                return Err(CoreError::Refused(format!(
                    "{} already exists",
                    m.dst.display()
                )));
            }
            if !dsts.insert(&m.dst) {
                return Err(CoreError::Refused(format!(
                    "{} is planned twice",
                    m.dst.display()
                )));
            }
            let parent = m.dst.parent().unwrap_or(Path::new("/"));
            let (anchor, _) = missing_dirs(parent);
            if meta.dev() != device(&anchor)? {
                return Err(CoreError::Refused(format!(
                    "{} and {} are on different volumes; moves across volumes would need copy + delete",
                    m.src.display(),
                    anchor.display()
                )));
            }
        }
        Ok(())
    }

    /// Run moves as one journaled batch. Returns the batch id.
    pub fn execute(&self, label: &str, plan: &Plan) -> Result<i64> {
        self.preflight(&plan.moves)?;
        let ops: Vec<Planned> = plan.moves.iter().cloned().map(Planned::Move).collect();
        self.run("move", label, None, &ops, |_| Ok(()))
    }

    /// Moves plus writes of the app's own `.shoebox/*.json` files (or
    /// sidecars) as one journaled batch, so one undo reverses both.
    pub fn execute_with_writes(
        &self,
        label: &str,
        plan: &Plan,
        writes: Vec<(PathBuf, Vec<u8>)>,
    ) -> Result<i64> {
        self.preflight(&plan.moves)?;
        for (path, _) in &writes {
            self.check_sidecar_target(path)?;
        }
        let ops: Vec<Planned> = plan
            .moves
            .iter()
            .cloned()
            .map(Planned::Move)
            .chain(
                writes
                    .into_iter()
                    .map(|(path, bytes)| Planned::Sidecar { path, bytes }),
            )
            .collect();
        self.run("move", label, None, &ops, |_| Ok(()))
    }

    /// Import: copy each file, verify the copy's BLAKE3 against the source's,
    /// and only then give it its name (never replacing anything). One batch.
    pub fn import(&self, label: &str, copies: &[PlannedCopy]) -> Result<i64> {
        let mut dsts = HashSet::new();
        for c in copies {
            self.check_unprotected(&c.dst)?;
            if exists(&c.dst) || !dsts.insert(&c.dst) {
                return Err(CoreError::Refused(format!(
                    "{} already exists or is planned twice",
                    c.dst.display()
                )));
            }
            let meta = std::fs::symlink_metadata(&c.src).map_err(io(&c.src))?;
            if !meta.file_type().is_file() {
                return Err(CoreError::Refused(format!(
                    "{} is not a regular file",
                    c.src.display()
                )));
            }
        }
        let ops: Vec<Planned> = copies
            .iter()
            .map(|c| Planned::Copy {
                src: c.src.clone(),
                dst: c.dst.clone(),
                hash: c.hash,
            })
            .collect();
        self.run("import", label, None, &ops, |_| Ok(()))
    }

    /// Move files to the Trash (an explicit command), each only if it still
    /// has the content it had when the person chose it. One batch.
    pub fn trash(&self, label: &str, files: &[(PathBuf, [u8; 32])]) -> Result<i64> {
        for (p, _) in files {
            self.check_unprotected(p)?;
        }
        let ops: Vec<Planned> = files
            .iter()
            .map(|(path, hash)| Planned::Trash {
                path: path.clone(),
                hash: *hash,
            })
            .collect();
        self.run("trash", label, None, &ops, |_| Ok(()))
    }

    /// Rewrite XMP sidecars as one journaled batch: each write is recorded
    /// (with the previous contents) before it happens, and lands atomically
    /// (temporary file, then rename). Only `.xmp` files outside reference
    /// roots can be written, so image files are never touched.
    pub fn write_sidecars(&self, label: &str, writes: Vec<(PathBuf, Vec<u8>)>) -> Result<i64> {
        for (path, _) in &writes {
            self.check_sidecar_target(path)?;
        }
        let ops: Vec<Planned> = writes
            .into_iter()
            .map(|(path, bytes)| Planned::Sidecar { path, bytes })
            .collect();
        self.run("sidecar", label, None, &ops, |_| Ok(()))
    }

    fn check_sidecar_target(&self, path: &Path) -> Result<()> {
        let is_xmp = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xmp"));
        // The app's own files in a root (`.shoebox/events.json` etc.).
        let is_app_file = path.extension().is_some_and(|e| e == "json")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|d| d == ".shoebox");
        if !is_xmp && !is_app_file {
            return Err(CoreError::Refused(format!(
                "{} is not an .xmp sidecar or a .shoebox file; image files are never written",
                path.display()
            )));
        }
        self.check_unprotected(path)?;
        if let Ok(meta) = std::fs::symlink_metadata(path)
            && !meta.file_type().is_file()
        {
            return Err(CoreError::Refused(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        let parent = path.parent().unwrap_or(Path::new("/"));
        if !parent.is_dir() {
            return Err(CoreError::Refused(format!(
                "{} does not exist",
                parent.display()
            )));
        }
        Ok(())
    }

    fn run(
        &self,
        kind: &str,
        label: &str,
        undo_of: Option<i64>,
        ops: &[Planned],
        mut after_each: impl FnMut(usize) -> Result<()>,
    ) -> Result<i64> {
        let j = &self.journal;
        let batch = j.begin_batch(kind, label, undo_of)?;
        let mut seq = 0usize;
        let fail = |done: usize, reason: String| -> Result<i64> {
            j.set_batch_status(batch, "failed")?;
            Err(CoreError::BatchFailed {
                batch,
                done,
                total: ops.len(),
                reason,
            })
        };
        for (i, planned) in ops.iter().enumerate() {
            match planned {
                Planned::Move(m) => {
                    let parent = m.dst.parent().unwrap_or(Path::new("/"));
                    for dir in missing_dirs(parent).1 {
                        let op = j.record(batch, seq, Op::Mkdir, &dir, &dir, None)?;
                        seq += 1;
                        match std::fs::create_dir(&dir) {
                            Ok(()) => j.complete(op)?,
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                j.complete(op)?
                            }
                            Err(e) => {
                                j.fail(op, &e.to_string())?;
                                return fail(i, format!("creating {}: {e}", dir.display()));
                            }
                        }
                    }
                    let size = std::fs::symlink_metadata(&m.src).ok().map(|md| md.len());
                    let op = j.record(batch, seq, Op::Move, &m.src, &m.dst, size)?;
                    seq += 1;
                    if let Err(e) = sys::rename_excl(&m.src, &m.dst) {
                        j.fail(op, &e.to_string())?;
                        return fail(i, format!("{} → {}: {e}", m.src.display(), m.dst.display()));
                    }
                    j.complete(op)?;
                }
                Planned::Copy { src, dst, hash } => {
                    let parent = dst.parent().unwrap_or(Path::new("/"));
                    for dir in missing_dirs(parent).1 {
                        let op = j.record(batch, seq, Op::Mkdir, &dir, &dir, None)?;
                        seq += 1;
                        match std::fs::create_dir(&dir) {
                            Ok(()) => j.complete(op)?,
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                j.complete(op)?
                            }
                            Err(e) => {
                                j.fail(op, &e.to_string())?;
                                return fail(i, format!("creating {}: {e}", dir.display()));
                            }
                        }
                    }
                    let size = std::fs::metadata(src).ok().map(|m| m.len());
                    let op = j.record_hashed(batch, seq, Op::Copy, src, dst, size, hash)?;
                    seq += 1;
                    if let Err(e) = copy_verified(src, dst, hash) {
                        j.fail(op, &e)?;
                        return fail(i, format!("{} → {}: {e}", src.display(), dst.display()));
                    }
                    j.complete(op)?;
                }
                Planned::Trash { path, hash } => {
                    let dst = match &self.trash {
                        TrashMode::System => path.clone(),
                        TrashMode::Folder(dir) => {
                            let name = path.file_name().unwrap_or_default();
                            let mut d = dir.join(name);
                            let mut n = 0;
                            while exists(&d) {
                                n += 1;
                                d = suffixed(&dir.join(name), n);
                            }
                            d
                        }
                    };
                    let size = std::fs::metadata(path).ok().map(|m| m.len());
                    let op = j.record_hashed(batch, seq, Op::Trash, path, &dst, size, hash)?;
                    seq += 1;
                    let result = match hash_file(path) {
                        Ok(h) if h == *hash => match &self.trash {
                            TrashMode::System => system_trash(path),
                            TrashMode::Folder(dir) => std::fs::create_dir_all(dir)
                                .and_then(|()| sys::rename_excl(path, &dst))
                                .map_err(|e| e.to_string()),
                        },
                        Ok(_) => Err("changed since it was planned; left in place".into()),
                        Err(e) => Err(e.to_string()),
                    };
                    if let Err(e) = result {
                        j.fail(op, &e)?;
                        return fail(i, format!("trashing {}: {e}", path.display()));
                    }
                    j.complete(op)?;
                }
                Planned::Sidecar { path, bytes } => {
                    let before = match std::fs::read(path) {
                        Ok(b) => Some(b),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                        Err(e) => return fail(i, format!("reading {}: {e}", path.display())),
                    };
                    let hash = *blake3::hash(bytes).as_bytes();
                    let op = j.record_sidecar(batch, seq, path, before.as_deref(), &hash)?;
                    seq += 1;
                    if let Err(e) = write_atomic(path, bytes) {
                        j.fail(op, &e.to_string())?;
                        return fail(i, format!("writing {}: {e}", path.display()));
                    }
                    j.complete(op)?;
                }
            }
            after_each(i)?;
        }
        j.set_batch_status(batch, "done")?;
        Ok(batch)
    }

    /// Operations of `batch` that actually happened and are not yet undone,
    /// latest first, with what reverses each. A `pending` op (the process died
    /// mid-batch) counts if the disk shows it ran.
    pub fn undo_plan(&self, batch: i64) -> Result<Vec<(OpRecord, Planned)>> {
        let summary = self
            .journal
            .batch(batch)?
            .ok_or_else(|| CoreError::Invalid(format!("no batch {batch}")))?;
        if summary.status == "undone" {
            return Err(CoreError::Refused(format!(
                "batch {batch} is already undone"
            )));
        }
        let mut out = Vec::new();
        for op in self.journal.ops(batch)?.into_iter().rev() {
            if op.undone {
                continue;
            }
            match op.op {
                Op::Mkdir => {}
                Op::Move => {
                    let happened = match op.status {
                        OpStatus::Done => true,
                        OpStatus::Pending => exists(&op.dst) && !exists(&op.src),
                        OpStatus::Failed => false,
                    };
                    if happened {
                        let back = Planned::Move(PlannedMove {
                            src: op.dst.clone(),
                            dst: op.src.clone(),
                        });
                        out.push((op, back));
                    }
                }
                Op::Copy => {
                    let Some(hash) = op
                        .after_hash
                        .as_deref()
                        .and_then(|h| <[u8; 32]>::try_from(h).ok())
                    else {
                        continue;
                    };
                    let happened = match op.status {
                        OpStatus::Done => true,
                        OpStatus::Pending => hash_file(&op.dst).is_ok_and(|h| h == hash),
                        OpStatus::Failed => false,
                    };
                    if happened {
                        // Undoing an import moves the copy to the Trash (the
                        // source, such as the card, still has the original).
                        let back = Planned::Trash {
                            path: op.dst.clone(),
                            hash,
                        };
                        out.push((op, back));
                    }
                }
                Op::Trash => {
                    let happened = match op.status {
                        OpStatus::Done => true,
                        OpStatus::Pending => !exists(&op.src),
                        OpStatus::Failed => false,
                    };
                    if !happened {
                        continue;
                    }
                    if op.dst == op.src {
                        return Err(CoreError::Refused(format!(
                            "{} is in the system Trash; use Finder's Put Back to restore it",
                            op.src.display()
                        )));
                    }
                    let back = Planned::Move(PlannedMove {
                        src: op.dst.clone(),
                        dst: op.src.clone(),
                    });
                    out.push((op, back));
                }
                Op::Sidecar => {
                    let current = std::fs::read(&op.dst).ok();
                    let wrote = current
                        .as_ref()
                        .zip(op.after_hash.as_ref())
                        .is_some_and(|(c, h)| blake3::hash(c).as_bytes()[..] == h[..]);
                    let happened = match op.status {
                        OpStatus::Done => true,
                        OpStatus::Pending => wrote,
                        OpStatus::Failed => false,
                    };
                    if !happened {
                        continue;
                    }
                    // Restore the previous bytes. A file Shoebox created is
                    // emptied rather than deleted: a sidecar loses our
                    // properties, an app JSON file becomes `{}`.
                    let is_json = op.dst.extension().is_some_and(|e| e == "json");
                    let bytes = match (&op.before, &current) {
                        (Some(b), _) => b.clone(),
                        (None, Some(_)) if is_json => b"{}".to_vec(),
                        (None, Some(c)) => {
                            let all: Vec<_> =
                                crate::xmp::Prop::ALL.iter().map(|p| (*p, None)).collect();
                            crate::xmp::edit(Some(c), &all)?
                        }
                        (None, None) => continue,
                    };
                    let back = Planned::Sidecar {
                        path: op.dst.clone(),
                        bytes,
                    };
                    out.push((op, back));
                }
            }
        }
        Ok(out)
    }

    /// Reverse a batch as a new journaled batch. Directories the batch created
    /// are left in place (nothing is ever deleted).
    pub fn undo(&self, batch: i64) -> Result<i64> {
        let plan = self.undo_plan(batch)?;
        // Undo is only allowed while it is consistent with the disk.
        let mut moves = Vec::new();
        for (op, back) in &plan {
            match back {
                Planned::Move(m) => {
                    let meta = std::fs::symlink_metadata(&m.src).map_err(|_| {
                        CoreError::Refused(format!(
                            "{} is no longer there; cannot undo",
                            m.src.display()
                        ))
                    })?;
                    if op.size.is_some_and(|s| s != meta.len()) {
                        return Err(CoreError::Refused(format!(
                            "{} changed size since it was moved; cannot undo",
                            m.src.display()
                        )));
                    }
                    moves.push(m.clone());
                }
                Planned::Sidecar { path, .. } => {
                    let current = std::fs::read(path).unwrap_or_default();
                    let unchanged = op
                        .after_hash
                        .as_ref()
                        .is_some_and(|h| blake3::hash(&current).as_bytes()[..] == h[..]);
                    if !unchanged {
                        return Err(CoreError::Refused(format!(
                            "{} was changed since (by another app?); cannot undo",
                            path.display()
                        )));
                    }
                    self.check_sidecar_target(path)?;
                }
                Planned::Trash { path, hash } => {
                    if !hash_file(path).is_ok_and(|h| h == *hash) {
                        return Err(CoreError::Refused(format!(
                            "{} is gone or was changed since it was copied; cannot undo",
                            path.display()
                        )));
                    }
                    self.check_unprotected(path)?;
                }
                Planned::Copy { .. } => {}
            }
        }
        self.preflight(&moves)?;
        let ids: Vec<i64> = plan.iter().map(|(op, _)| op.id).collect();
        let ops: Vec<Planned> = plan.into_iter().map(|(_, b)| b).collect();
        let j = &self.journal;
        let undo = self.run(
            "undo",
            &format!("undo batch {batch}"),
            Some(batch),
            &ops,
            |i| j.mark_undone(ids[i]),
        )?;
        j.set_batch_status(batch, "undone")?;
        Ok(undo)
    }

    /// The most recent batch (not itself an undo) that hasn't been undone.
    pub fn last_undoable(&self) -> Result<Option<BatchSummary>> {
        Ok(self
            .journal
            .batches()?
            .into_iter()
            .rev()
            .find(|b| b.kind != "undo" && b.status != "undone"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn ops(dir: &Path, protected: Vec<PathBuf>) -> FileOps {
        FileOps::new(
            Journal::open(&dir.join("journal.sqlite")).unwrap(),
            protected,
        )
    }

    #[test]
    fn suffix_names() {
        assert_eq!(
            suffixed(Path::new("/a/f0001.jpg"), 2),
            Path::new("/a/f0001-2.jpg")
        );
        assert_eq!(
            suffixed(Path::new("/a/README"), 1),
            Path::new("/a/README-1")
        );
    }

    #[test]
    fn collisions_get_suffix_and_sidecars_follow() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let src = root.join("scan/a.jpg");
        touch(&src, "new");
        touch(&root.join("scan/a.jpg.xmp"), "sidecar");
        touch(&root.join("dupes/a.jpg"), "existing");
        let plan = plan_moves(&[(src.clone(), root.join("dupes/a.jpg"))]).unwrap();
        assert_eq!(
            plan.moves,
            vec![
                PlannedMove {
                    src: src.clone(),
                    dst: root.join("dupes/a-1.jpg")
                },
                PlannedMove {
                    src: root.join("scan/a.jpg.xmp"),
                    dst: root.join("dupes/a-1.jpg.xmp")
                },
            ]
        );
        let fo = ops(&root, vec![]);
        fo.execute("test", &plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("dupes/a.jpg")).unwrap(),
            "existing"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("dupes/a-1.jpg")).unwrap(),
            "new"
        );
        assert!(root.join("dupes/a-1.jpg.xmp").exists());
    }

    fn h(p: &Path) -> [u8; 32] {
        hash_file(p).unwrap()
    }

    #[test]
    fn import_copies_verify_and_undo_to_trash() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let card = root.join("card/DCIM/100NIKON");
        touch(&card.join("DSC_0001.NEF"), "raw bytes");
        touch(&root.join("lib/2026/a.nef"), "already here");
        let src = card.join("DSC_0001.NEF");
        let fo = ops(&root, vec![]).with_trash(TrashMode::Folder(root.join("trash")));
        let copies = vec![PlannedCopy {
            src: src.clone(),
            dst: root.join("lib/2026/b.nef"),
            hash: h(&src),
        }];
        let batch = fo.import("import", &copies).unwrap();
        assert_eq!(
            std::fs::read(root.join("lib/2026/b.nef")).unwrap(),
            b"raw bytes"
        );
        assert!(src.exists(), "import copies, never moves");
        let mtime = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(mtime(&src), mtime(&root.join("lib/2026/b.nef")));
        // Never onto an existing file.
        let clash = vec![PlannedCopy {
            dst: root.join("lib/2026/a.nef"),
            ..copies[0].clone()
        }];
        assert!(fo.import("again", &clash).is_err());
        assert_eq!(
            std::fs::read(root.join("lib/2026/a.nef")).unwrap(),
            b"already here"
        );
        // A source that changed after planning fails verification.
        let wrong = vec![PlannedCopy {
            dst: root.join("lib/2026/c.nef"),
            hash: [0; 32],
            ..copies[0].clone()
        }];
        assert!(fo.import("bad", &wrong).is_err());
        assert!(!root.join("lib/2026/c.nef").exists());
        assert!(!root.join("lib/2026/.c.nef.shoebox-tmp").exists());

        // Undo trashes the copy (to the test trash folder); the card keeps it.
        fo.undo(batch).unwrap();
        assert!(!root.join("lib/2026/b.nef").exists());
        assert_eq!(
            std::fs::read(root.join("trash/b.nef")).unwrap(),
            b"raw bytes"
        );
        assert!(src.exists());
    }

    #[test]
    fn trash_and_undo_with_a_trash_folder() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let f = root.join("lib/x.jpg");
        touch(&f, "x");
        let fo = ops(&root, vec![]).with_trash(TrashMode::Folder(root.join("trash")));
        // Changed since chosen: refused, left in place.
        assert!(fo.trash("trash", &[(f.clone(), [1; 32])]).is_err());
        assert!(f.exists());
        let b = fo.trash("trash", &[(f.clone(), h(&f))]).unwrap();
        assert!(!f.exists() && root.join("trash/x.jpg").exists());
        fo.undo(b).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"x");
    }

    #[test]
    fn stacks_move_together_with_their_sidecars() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let d = root.join("in");
        for f in [
            "x.nef",
            "x.jpg",
            "x-1.tif",
            "x.xmp",
            "x.jpg.xmp",
            "y.jpg",
            "y.xmp",
        ] {
            touch(&d.join(f), f);
        }
        touch(&root.join("out/new.nef"), "taken");
        let group = vec![
            (d.join("x.nef"), root.join("out/new.nef")),
            (d.join("x.jpg"), root.join("out/new.jpg")),
            (d.join("x-1.tif"), root.join("out/new-1.tif")),
        ];
        let plan = plan_group_moves(&[group]).unwrap();
        let pairs: Vec<(String, String)> = plan
            .moves
            .iter()
            .map(|m| {
                (
                    m.src.strip_prefix(&root).unwrap().display().to_string(),
                    m.dst.strip_prefix(&root).unwrap().display().to_string(),
                )
            })
            .collect();
        let want = [
            ("in/x.nef", "out/new-1.nef"),
            ("in/x.jpg", "out/new-1.jpg"),
            ("in/x.jpg.xmp", "out/new-1.jpg.xmp"),
            ("in/x-1.tif", "out/new-1-1.tif"),
            ("in/x.xmp", "out/new-1.xmp"),
        ];
        assert_eq!(
            pairs,
            want.iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect::<Vec<_>>(),
            "one suffix for the whole stack; the shared x.xmp follows"
        );
        // Moving only the JPG of y leaves y.xmp (shared with nothing else
        // here, but y.jpg is its only image so it does move).
        let plan = plan_group_moves(&[vec![(d.join("y.jpg"), root.join("out/z.jpg"))]]).unwrap();
        assert_eq!(plan.moves.len(), 2);
        // Moving x.jpg alone leaves the shared x.xmp for x.nef.
        let plan = plan_group_moves(&[vec![(d.join("x.jpg"), root.join("out/q.jpg"))]]).unwrap();
        assert!(plan.moves.iter().all(|m| m.src != d.join("x.xmp")));
    }

    #[test]
    fn shared_stem_sidecar_stays() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        touch(&root.join("s/IMG.JPG"), "j");
        touch(&root.join("s/IMG.NEF"), "n");
        touch(&root.join("s/IMG.xmp"), "x");
        let plan = plan_moves(&[(root.join("s/IMG.JPG"), root.join("d/IMG.JPG"))]).unwrap();
        assert_eq!(plan.moves.len(), 1);
    }

    #[test]
    fn journal_row_exists_before_move() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let src = root.join("a.jpg");
        touch(&src, "x");
        let fo = ops(&root, vec![]);
        // A destination that can't be created: the op must be journaled and failed.
        touch(&root.join("blocker"), "file, not a folder");
        let plan = Plan {
            moves: vec![PlannedMove {
                src: src.clone(),
                dst: root.join("blocker/sub/a.jpg"),
            }],
        };
        assert!(fo.execute("t", &plan).is_err());
        let b = fo.journal().batches().unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].status, "failed");
        let recorded = fo.journal().ops(b[0].id).unwrap();
        assert_eq!(recorded[0].op, Op::Mkdir);
        assert_eq!(recorded[0].status, OpStatus::Failed);
        assert!(src.exists());
    }

    #[test]
    fn move_then_undo_restores() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let src = root.join("scan/x/a.jpg");
        touch(&src, "a");
        let fo = ops(&root, vec![]);
        let plan = plan_moves(&[(src.clone(), root.join("dupes/x/a.jpg"))]).unwrap();
        let b = fo.execute("t", &plan).unwrap();
        assert!(!src.exists());
        let u = fo.undo(b).unwrap();
        assert!(src.exists());
        assert!(!root.join("dupes/x/a.jpg").exists());
        assert!(
            root.join("dupes/x").is_dir(),
            "created folders are left, never deleted"
        );
        assert!(fo.undo(b).is_err(), "double undo refused");
        let undo_batch = fo.journal().batch(u).unwrap().unwrap();
        assert_eq!(undo_batch.undo_of, Some(b));
    }

    #[test]
    fn undo_refuses_when_disk_changed() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let src = root.join("a.jpg");
        touch(&src, "a");
        let fo = ops(&root, vec![]);
        let b = fo
            .execute(
                "t",
                &plan_moves(&[(src.clone(), root.join("d/a.jpg"))]).unwrap(),
            )
            .unwrap();
        touch(&src, "someone put a new file here");
        assert!(matches!(fo.undo(b), Err(CoreError::Refused(_))));
        assert!(root.join("d/a.jpg").exists());
    }

    #[test]
    fn reference_roots_are_untouchable() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let src = root.join("ref/a.jpg");
        touch(&src, "a");
        let fo = ops(&root, vec![root.join("ref")]);
        let plan = Plan {
            moves: vec![PlannedMove {
                src: src.clone(),
                dst: root.join("out/a.jpg"),
            }],
        };
        assert!(matches!(fo.execute("t", &plan), Err(CoreError::Refused(_))));
        assert!(
            fo.journal().batches().unwrap().is_empty(),
            "nothing journaled or run"
        );
        assert!(src.exists());
    }

    #[test]
    fn sidecar_edits_are_journaled_and_undone_exactly() {
        use crate::xmp::{Prop, Value};
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let fo = ops(&root, vec![]);
        let side = root.join("a.jpg.xmp");
        let rating = |v: &str| vec![(Prop::Rating, Some(Value::Text(v.into())))];

        // New sidecar: undo empties our properties, never deletes the file.
        let b1 = fo
            .write_sidecars(
                "rate",
                vec![(side.clone(), crate::xmp::edit(None, &rating("3")).unwrap())],
            )
            .unwrap();
        assert_eq!(fo.journal().ops(b1).unwrap()[0].op, Op::Sidecar);
        fo.undo(b1).unwrap();
        assert!(side.exists(), "never deleted");
        assert!(
            crate::xmp::read(&std::fs::read(&side).unwrap())
                .unwrap()
                .is_empty()
        );

        // Existing sidecar with foreign data: undo restores it byte for byte.
        let foreign = std::fs::read(&side).unwrap();
        let edited = crate::xmp::edit(Some(&foreign), &rating("5")).unwrap();
        let b2 = fo
            .write_sidecars("rate", vec![(side.clone(), edited)])
            .unwrap();
        fo.undo(b2).unwrap();
        assert_eq!(std::fs::read(&side).unwrap(), foreign);

        // Changed by another app after our write: undo refuses.
        let b3 = fo
            .write_sidecars(
                "rate",
                vec![(
                    side.clone(),
                    crate::xmp::edit(Some(&foreign), &rating("2")).unwrap(),
                )],
            )
            .unwrap();
        std::fs::write(&side, b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>").unwrap();
        assert!(matches!(fo.undo(b3), Err(CoreError::Refused(_))));
    }

    #[test]
    fn sidecar_writes_refuse_images_and_reference_roots() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        touch(&root.join("ref/a.jpg"), "image");
        touch(&root.join("lib/b.jpg"), "image");
        let fo = ops(&root, vec![root.join("ref")]);
        let r = fo.write_sidecars(
            "x",
            vec![(root.join("lib/b.jpg"), b"not an image".to_vec())],
        );
        assert!(
            matches!(r, Err(CoreError::Refused(_))),
            "image files are never written"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("lib/b.jpg")).unwrap(),
            "image"
        );
        let r = fo.write_sidecars("x", vec![(root.join("ref/a.jpg.xmp"), b"x".to_vec())]);
        assert!(
            matches!(r, Err(CoreError::Refused(_))),
            "reference roots are read-only"
        );
        assert!(!root.join("ref/a.jpg.xmp").exists());
        assert!(
            fo.journal().batches().unwrap().is_empty(),
            "refusals run nothing"
        );
    }

    #[test]
    fn old_journals_are_migrated_in_place() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("journal.sqlite");
        {
            // An M1-era journal: no before/after_hash columns, one batch.
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE batches (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, kind TEXT NOT NULL,
                   label TEXT NOT NULL, status TEXT NOT NULL, undo_of INTEGER);
                 CREATE TABLE journal (id INTEGER PRIMARY KEY, batch_id INTEGER NOT NULL, seq INTEGER NOT NULL,
                   ts INTEGER NOT NULL, op TEXT NOT NULL, src TEXT NOT NULL, dst TEXT NOT NULL, size INTEGER,
                   status TEXT NOT NULL, undone INTEGER NOT NULL DEFAULT 0, error TEXT);
                 INSERT INTO batches VALUES (1, 0, 'move', 'old', 'done', NULL);
                 INSERT INTO journal(batch_id, seq, ts, op, src, dst, size, status) VALUES (1, 0, 0, 'move', '/a', '/b', 1, 'done');",
            )
            .unwrap();
        }
        let j = Journal::open(&db).unwrap();
        let ops = j.ops(1).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].op, Op::Move);
        assert!(ops[0].before.is_none());
        assert_eq!(j.batches().unwrap()[0].label, "old");
    }

    #[test]
    fn crashed_batch_is_reported_and_undoable() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let (src, dst) = (root.join("a.jpg"), root.join("b.jpg"));
        touch(&src, "a");
        let fo = ops(&root, vec![]);
        // Simulate a crash between the rename and marking the op done.
        let batch = fo.journal().begin_batch("move", "crash", None).unwrap();
        fo.journal()
            .record(batch, 0, Op::Move, &src, &dst, Some(1))
            .unwrap();
        std::fs::rename(&src, &dst).unwrap();
        assert_eq!(fo.journal().incomplete().unwrap().len(), 1);
        fo.undo(batch).unwrap();
        assert!(src.exists() && !dst.exists());
    }
}
