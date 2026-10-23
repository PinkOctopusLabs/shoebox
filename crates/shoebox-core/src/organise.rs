//! Moving and renaming stacks: every member and its sidecars together.
//!
//! These functions only plan (the dry-run table); `fileops` runs the plan as
//! one journaled batch. Files in reference roots are never planned.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::catalog::Catalog;
use crate::error::{CoreError, Result};
use crate::fileops::{self, Plan, PlannedMove};
use crate::scan::Role;
use crate::template::{Fields, Template};

/// The files of these stacks, refusing any in a reference root.
fn members(c: &Catalog, stacks: &[usize]) -> Result<Vec<Vec<usize>>> {
    stacks
        .iter()
        .map(|&s| {
            let all: Vec<usize> = c.stacks[s].all().collect();
            if let Some(&i) = all
                .iter()
                .find(|&&i| c.roots[c.photos[i].root].role == Role::Reference)
            {
                return Err(CoreError::Refused(format!(
                    "{} is in a reference folder, which is read-only",
                    c.photos[i].path.display()
                )));
            }
            Ok(all)
        })
        .collect()
}

/// Move stacks into `folder`, keeping file names (collisions get `-N`).
pub fn plan_move_to(c: &Catalog, stacks: &[usize], folder: &Path) -> Result<Plan> {
    if let Some(r) = c
        .roots
        .iter()
        .find(|r| r.role == Role::Reference && folder.starts_with(&r.path))
    {
        return Err(CoreError::Refused(format!(
            "{} is a reference folder, which is read-only",
            r.path.display()
        )));
    }
    let groups: Vec<Vec<(PathBuf, PathBuf)>> = members(c, stacks)?
        .into_iter()
        .map(|m| {
            m.into_iter()
                .map(|i| {
                    let p = &c.photos[i].path;
                    (p.clone(), folder.join(p.file_name().unwrap_or_default()))
                })
                .collect()
        })
        .collect();
    fileops::plan_group_moves(&groups)
}

/// Rejected stacks to `_rejected/` in their own root, keeping the relative
/// path (Cmd+Backspace).
pub fn plan_reject(c: &Catalog, stacks: &[usize]) -> Result<Plan> {
    let groups: Vec<Vec<(PathBuf, PathBuf)>> = members(c, stacks)?
        .into_iter()
        .map(|m| {
            m.into_iter()
                .map(|i| {
                    let p = &c.photos[i];
                    let root = &c.roots[p.root].path;
                    (p.path.clone(), root.join("_rejected").join(&p.rel))
                })
                .collect()
        })
        .collect();
    fileops::plan_group_moves(&groups)
}

/// The part of `stem` after the stack's base name (`-1` for `x-1` when the
/// base is `x`), so virtual copies stay distinct after a rename.
fn variant_suffix(stem: &str, base: &str) -> String {
    let (s, b) = (stem.to_lowercase(), base.to_lowercase());
    match s.strip_prefix(&b) {
        Some(rest) if rest.starts_with('-') && rest.len() <= 4 => stem
            .get(stem.len().saturating_sub(rest.len())..)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

/// What batch rename will skip.
#[derive(Debug, Default)]
pub struct RenamePlan {
    pub plan: Plan,
    /// Stacks left alone because they have no capture date.
    pub undated: Vec<PathBuf>,
}

/// Rename stacks with a template, rendered from each stack's primary (so a
/// NEF, its JPG and TIFF get one name); each file keeps its extension and
/// any `-N` copy suffix. A template without `/` renames in place; with `/`
/// it's a path from the file's root, so files move across folders too.
/// Stacks without a capture date are left alone.
pub fn plan_rename(c: &Catalog, stacks: &[usize], template: &Template) -> Result<RenamePlan> {
    let in_place = !template.to_string().contains('/');
    let mut out = RenamePlan::default();
    let mut groups = Vec::new();
    for (k, m) in members(c, stacks)?.into_iter().enumerate() {
        let primary = &c.photos[m[0]];
        if primary.meta.exif.capture_time.is_none() {
            out.undated.push(primary.path.clone());
            continue;
        }
        groups.push(rendered(c, &m, k, template, in_place));
    }
    out.plan = fileops::plan_group_moves(&groups)?;
    Ok(out)
}

/// What filing dated photos out of `_undated/` leaves alone.
#[derive(Debug, Default)]
pub struct FilePlan {
    pub plan: Plan,
    /// Stacks with only a month or year: no day folder to go to.
    pub approximate: Vec<PathBuf>,
}

/// File stacks dated since they went to `_undated/` (SPEC "Dates", Find
/// dates) where `template` (the import template) puts them: by the whole
/// template when the date is exact; into its folders, keeping their names,
/// when only the day is known (the template's time would be made up);
/// left alone with just a month or year, or no date. A template without
/// folders files nothing.
pub fn plan_file_dated(c: &Catalog, stacks: &[usize], template: &str) -> Result<FilePlan> {
    let mut out = FilePlan::default();
    let Some((dir, _)) = template.rsplit_once('/') else {
        return Ok(out);
    };
    let exact = Template::parse(template)?;
    let day = Template::parse(&format!("{dir}/{{name}}.{{ext}}"))?;
    let mut groups = Vec::new();
    for (k, m) in members(c, stacks)?.into_iter().enumerate() {
        let primary = &c.photos[m[0]];
        if primary.meta.exif.capture_time.is_none() {
            continue;
        }
        match primary.meta.precision() {
            crate::sidecar::Precision::Moment => groups.push(rendered(c, &m, k, &exact, false)),
            crate::sidecar::Precision::Day => groups.push(rendered(c, &m, k, &day, false)),
            _ => out.approximate.push(primary.path.clone()),
        }
    }
    out.plan = fileops::plan_group_moves(&groups)?;
    Ok(out)
}

/// A stack's files (`m`, primary first) renamed by `template` from the
/// primary's metadata, each keeping its extension and `-N` suffix: in its
/// folder (`in_place`) or from its root. `k`: its place (`{seq}`).
fn rendered(
    c: &Catalog,
    m: &[usize],
    k: usize,
    template: &Template,
    in_place: bool,
) -> Vec<(PathBuf, PathBuf)> {
    let primary = &c.photos[m[0]];
    let base = primary
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut g = Vec::new();
    for &i in m {
        let p = &c.photos[i];
        let fields = Fields {
            capture_time: primary.meta.exif.capture_time.clone(),
            subsec: primary.meta.exif.subsec.clone(),
            file_name: p
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            camera: primary.meta.exif.model.clone(),
            width: primary.meta.width,
            height: primary.meta.height,
            seq: k + 1,
        };
        let rendered = template.render(&fields);
        let stem = p
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let suffix = variant_suffix(&stem, &base);
        let rendered = if suffix.is_empty() {
            rendered
        } else {
            let name = rendered
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let new = match name.rsplit_once('.') {
                Some((s, e)) => format!("{s}{suffix}.{e}"),
                None => format!("{name}{suffix}"),
            };
            rendered.with_file_name(new)
        };
        let dst = if in_place {
            p.path
                .with_file_name(rendered.file_name().unwrap_or_default())
        } else {
            c.roots[p.root].path.join(rendered)
        };
        g.push((p.path.clone(), dst));
    }
    g
}

/// After moves, the app's own files that name moved files (collections,
/// stack links, merges' records) rewritten for every writable root. Empty when
/// nothing refers to a moved file. Join these to the move batch.
pub fn reference_writes(
    all_roots: &[PathBuf],
    writable_roots: &[PathBuf],
    moves: &[PlannedMove],
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let map: HashMap<&Path, &Path> = moves
        .iter()
        .map(|m| (m.src.as_path(), m.dst.as_path()))
        .collect();
    let fix = |p: &mut PathBuf| -> bool {
        match map.get(p.as_path()) {
            Some(d) => {
                *p = d.to_path_buf();
                true
            }
            None => false,
        }
    };
    // Events need nothing: their members are marked in sidecars, which move
    // with their files.
    let mut out = Vec::new();
    let mut cols = crate::collections::load(all_roots);
    let mut changed = false;
    for col in &mut cols {
        for p in &mut col.members {
            changed |= fix(p);
        }
    }
    if changed {
        out.extend(crate::collections::plan_save(writable_roots, &cols)?);
    }
    let mut links = crate::stacks::load_links(all_roots);
    let mut changed = false;
    for l in &mut links {
        changed |= fix(&mut l.file);
        changed |= fix(&mut l.of);
    }
    if changed {
        out.extend(crate::stacks::plan_save_links(writable_roots, &links)?);
    }
    // Merges: the file and its sources.
    let mut merges = crate::merges::load(all_roots);
    let mut changed = false;
    for m in &mut merges {
        changed |= fix(&mut m.file);
        for s in &mut m.sources {
            changed |= fix(&mut s.path);
        }
    }
    if changed {
        out.extend(crate::merges::plan_save(writable_roots, &merges)?);
    }
    Ok(out)
}

/// Every file "Move to Trash" takes for these stacks: their members and the
/// sidecars that go with them (the same set a move would take).
pub fn trash_files(c: &Catalog, stacks: &[usize]) -> Result<Vec<PathBuf>> {
    // Plan a move to a folder that doesn't exist; only the sources matter.
    let nowhere = Path::new("/nonexistent-shoebox-trash-plan");
    Ok(plan_move_to(c, stacks, nowhere)?
        .moves
        .into_iter()
        .map(|m| m.src)
        .collect())
}
