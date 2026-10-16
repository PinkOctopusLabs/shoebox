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
    /// Canonical reference roots: never moved from, into or renamed.
    protected: Vec<PathBuf>,
}

impl FileOps {
    pub fn new(journal: Journal, protected: Vec<PathBuf>) -> Self {
        Self { journal, protected }
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
        if !is_xmp {
            return Err(CoreError::Refused(format!(
                "{} is not an .xmp sidecar; image files are never written",
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
                    // Restore the previous bytes; a sidecar Shoebox created is
                    // emptied of our properties rather than deleted.
                    let bytes = match (&op.before, &current) {
                        (Some(b), _) => b.clone(),
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

    /// The most recent move batch that hasn't been undone.
    pub fn last_undoable(&self) -> Result<Option<BatchSummary>> {
        Ok(self
            .journal
            .batches()?
            .into_iter()
            .rev()
            .find(|b| b.kind == "move" && b.status != "undone"))
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
