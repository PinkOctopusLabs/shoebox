//! Where photo edits live on disk (`docs/EDITING-SPEC.md`, Edit document and
//! storage): `<root>/.shoebox/edits/<path relative to root>.json` for the
//! master, `….<copy id>.json` for a virtual copy. Written through
//! [`crate::fileops`] (journaled, atomic) and moved with their photos.
//!
//! The documents themselves belong to `shoebox-develop`; here they're JSON
//! with a few known fields (`copy.id`, `source.blake3`, `source.size`). The
//! file's location says which photo an edit belongs to; `source.path` inside
//! it is a note that the next save brings up to date.

use std::path::{Path, PathBuf};

use crate::error::{Result, io};

/// The master edit's copy id.
pub const MAIN: &str = "main";

/// Upper bound on an edit file read from disk: real ones are a few KB.
const MAX_BYTES: u64 = 16 << 20;

/// `<root>/.shoebox/edits`.
pub fn dir(root: &Path) -> PathBuf {
    root.join(".shoebox").join("edits")
}

/// The edit file of `rel` (a photo's path relative to `root`) and `copy`.
pub fn path(root: &Path, rel: &Path, copy: &str) -> PathBuf {
    let base = dir(root).join(rel);
    let name = base
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = if copy == MAIN {
        format!("{name}.json")
    } else {
        format!("{name}.{copy}.json")
    };
    base.with_file_name(file)
}

/// What's known of an edit file without understanding the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFile {
    pub path: PathBuf,
    /// The photo's path relative to the root.
    pub rel: PathBuf,
    pub copy: String,
    pub blake3: Option<String>,
    pub size: Option<u64>,
    /// When the file was last written (ns since the epoch).
    pub modified_ns: i64,
}

/// An edit file's contents, or None if it's missing or empty (an undone
/// first save leaves `{}`).
pub fn read(path: &Path) -> Result<Option<String>> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path)(e)),
    };
    if meta.len() > MAX_BYTES {
        tracing::warn!("ignoring oversized edit file {}", path.display());
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).map_err(io(path))?;
    let empty = serde_json::from_str::<serde_json::Value>(&text)
        .is_ok_and(|v| v.as_object().is_some_and(|o| o.is_empty()));
    Ok((!empty).then_some(text))
}

/// The known fields of an edit file, given where it is.
fn describe(root: &Path, path: &Path) -> Option<EditFile> {
    let text = read(path).ok()??;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let copy = v["copy"]["id"].as_str().unwrap_or(MAIN).to_owned();
    let rel_json = path.strip_prefix(dir(root)).ok()?;
    let name = rel_json.file_name()?.to_string_lossy();
    let suffix = if copy == MAIN {
        ".json".to_owned()
    } else {
        format!(".{copy}.json")
    };
    let image = name.strip_suffix(&suffix)?;
    if image.is_empty() {
        return None;
    }
    let modified_ns = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64);
    Some(EditFile {
        path: path.to_path_buf(),
        rel: rel_json.with_file_name(image),
        copy,
        blake3: v["source"]["blake3"].as_str().map(str::to_owned),
        size: v["source"]["size"].as_u64(),
        modified_ns,
    })
}

/// Every edit file under `root` (unreadable ones are skipped).
pub fn all(root: &Path) -> Result<Vec<EditFile>> {
    let base = dir(root);
    let mut out = Vec::new();
    let mut stack = vec![base.clone()];
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(&d)(e)),
        };
        for entry in rd {
            let entry = entry.map_err(io(&d))?;
            let p = entry.path();
            let ft = entry.file_type().map_err(io(&p))?;
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file()
                && p.extension().is_some_and(|e| e == "json")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                && let Some(f) = describe(root, &p)
            {
                out.push(f);
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The edit files of the photo at `image` (absolute), found by looking for
/// `<ancestor>/.shoebox/edits/<rest>.json` (and its copies) in each
/// ancestor: `(root, edit file)`.
pub fn of_image(image: &Path) -> Vec<(PathBuf, PathBuf)> {
    let Some(name) = image.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for root in image.ancestors().skip(1) {
        let Ok(rel) = image.strip_prefix(root) else {
            continue;
        };
        let main = path(root, rel, MAIN);
        let Some(folder) = main.parent() else {
            continue;
        };
        let Ok(rd) = std::fs::read_dir(folder) else {
            continue;
        };
        for entry in rd.flatten() {
            let fname = entry.file_name().to_string_lossy().into_owned();
            // "<name>.json", or "<name>.<copy>.json" (copy ids have no dots).
            let belongs = match fname
                .strip_prefix(&name)
                .and_then(|r| r.strip_suffix(".json"))
            {
                Some("") => true,
                Some(m) => m
                    .strip_prefix('.')
                    .is_some_and(|c| !c.is_empty() && !c.contains('.')),
                None => false,
            };
            if belongs && entry.file_type().is_ok_and(|t| t.is_file()) {
                out.push((root.to_path_buf(), entry.path()));
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

/// Where an edit file of a photo moving from `image_src` to `image_dst` goes:
/// under the same root if the photo stays inside it, else under the nearest
/// ancestor of the destination that is a library root (has `.shoebox`).
/// None when the photo leaves every root: the edit stays, and is reattached
/// by content if the photo comes back.
pub fn moved(root: &Path, edit: &Path, image_src: &Path, image_dst: &Path) -> Option<PathBuf> {
    let src_name = image_src.file_name()?.to_string_lossy().into_owned();
    let dst_name = image_dst.file_name()?.to_string_lossy().into_owned();
    let edit_name = edit.file_name()?.to_string_lossy().into_owned();
    // "<src_name><tail>" → "<dst_name><tail>" (tail: ".json" or ".<copy>.json").
    let tail = edit_name.strip_prefix(&src_name)?;
    let new_root = if image_dst.starts_with(root) {
        root.to_path_buf()
    } else {
        image_dst
            .ancestors()
            .skip(1)
            .find(|a| a.join(".shoebox").is_dir())?
            .to_path_buf()
    };
    let rel = image_dst.strip_prefix(&new_root).ok()?;
    let main = path(&new_root, rel, MAIN);
    Some(main.with_file_name(format!("{dst_name}{tail}")))
}

/// For edits whose photo is gone (moved or renamed in Finder): the photo
/// they belong to, matched by size and then BLAKE3. `photos` are
/// `(path, size)` of photos without an edit of that copy; `hash` gives a
/// photo's BLAKE3 (hex), computed only for size matches. Returns
/// `(edit file, where it should be)`; an edit with no match is left alone
/// (listed as unattached, never deleted).
pub fn reattach(
    orphans: &[EditFile],
    root_of: impl Fn(&Path) -> Option<PathBuf>,
    photos: &[(PathBuf, u64)],
    mut hash: impl FnMut(&Path) -> Option<String>,
) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let mut used = std::collections::HashSet::new();
    for o in orphans {
        let Some(want) = &o.blake3 else { continue };
        let found = photos.iter().find(|(p, size)| {
            !used.contains(&(p.clone(), o.copy.clone()))
                && o.size.is_none_or(|s| s == *size)
                && hash(p).is_some_and(|h| h.eq_ignore_ascii_case(want))
        });
        let Some((photo, _)) = found else { continue };
        let Some(root) = root_of(photo) else { continue };
        let Ok(rel) = photo.strip_prefix(&root) else {
            continue;
        };
        let dst = path(&root, rel, &o.copy);
        if dst.exists() {
            continue;
        }
        used.insert((photo.clone(), o.copy.clone()));
        out.push((o.path.clone(), dst));
    }
    out
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64)
}

fn modified_ns(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as i64)
}

/// The edit to open for `(root, rel, copy)`: the catalog's working copy,
/// unless the file was written after it (on another Mac, or restored from
/// a backup), in which case the file wins and becomes the working copy.
/// `hash` computes the document's render hash for the catalog.
pub fn load(
    cache: &crate::cache::Cache,
    root: &Path,
    rel: &str,
    copy: &str,
    hash: impl Fn(&str) -> String,
) -> Result<Option<String>> {
    let Some(root_str) = root.to_str() else {
        return Ok(None);
    };
    let key = crate::cache::EditKey {
        root: root_str,
        rel,
        copy,
    };
    let file = path(root, Path::new(rel), copy);
    let row = cache.edit(&key)?;
    let file_ns = modified_ns(&file);
    let file_wins = match (&row, file_ns) {
        (None, Some(_)) => true,
        (Some(r), Some(f)) => f > r.updated_ns.max(r.saved_ns.unwrap_or(0)),
        (_, None) => false,
    };
    if file_wins && let Some(text) = read(&file)? {
        let at = file_ns.unwrap_or(0);
        cache.put_edit(&key, &text, &hash(&text), at)?;
        cache.mark_edit_saved(&key, at, at)?;
        return Ok(Some(text));
    }
    Ok(row.map(|r| r.json))
}

/// A photo's edits by copy, the master first: `(copy id, name)`. From its
/// edit files (`<name>.json`, `<name>.<copy>.json`) and the catalog's rows
/// not yet written.
pub fn copies_of(
    cache: &crate::cache::Cache,
    root: &Path,
    rel: &str,
) -> Result<Vec<(String, String)>> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(r) = root.to_str() {
        ids.extend(cache.edit_copies(r, rel)?);
    }
    let main = path(root, Path::new(rel), MAIN);
    if let (Some(dir), Some(name)) = (main.parent(), main.file_name()) {
        let stem = name.to_string_lossy();
        let stem = stem.strip_suffix(".json").unwrap_or(&stem).to_owned();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n == format!("{stem}.json") {
                    ids.push(MAIN.to_owned());
                } else if let Some(copy) = n
                    .strip_prefix(&format!("{stem}."))
                    .and_then(|r| r.strip_suffix(".json"))
                    && !copy.is_empty()
                    && !copy.contains('.')
                {
                    ids.push(copy.to_owned());
                }
            }
        }
    }
    ids.sort();
    ids.dedup();
    // The master first, then the copies in order of their ids.
    ids.sort_by_key(|id| (id != MAIN, id.clone()));
    let name_of = |id: &str| -> String {
        let text = root
            .to_str()
            .and_then(|r| {
                cache
                    .edit(&crate::cache::EditKey {
                        root: r,
                        rel,
                        copy: id,
                    })
                    .ok()
                    .flatten()
                    .map(|e| e.json)
            })
            .or_else(|| read(&path(root, Path::new(rel), id)).ok().flatten());
        text.and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["copy"]["name"].as_str().map(str::to_owned))
            .unwrap_or_else(|| {
                if id == MAIN {
                    "Master".into()
                } else {
                    id.to_owned()
                }
            })
    };
    Ok(ids
        .into_iter()
        .map(|id| (name_of(&id), id))
        .map(|(n, id)| (id, n))
        .collect())
}

/// Keep a changed edit in the catalog (`json`, with render hash `hash`);
/// [`save_pending`] writes it to its file shortly after.
pub fn put(
    cache: &crate::cache::Cache,
    root: &Path,
    rel: &str,
    copy: &str,
    json: &str,
    hash: &str,
) -> Result<()> {
    let Some(root_str) = root.to_str() else {
        return Ok(());
    };
    let key = crate::cache::EditKey {
        root: root_str,
        rel,
        copy,
    };
    cache.put_edit(&key, json, hash, now_ns())
}

/// Write every edit changed since its last save to its `.shoebox/edits`
/// file, as one journaled batch (kind `edit`). Returns how many.
pub fn save_pending(
    cache: &crate::cache::Cache,
    fileops: &crate::fileops::FileOps,
) -> Result<usize> {
    let mut writes = Vec::new();
    let mut saved = Vec::new();
    for (root, rel, copy) in cache.unsaved_edits()? {
        let key = crate::cache::EditKey {
            root: &root,
            rel: &rel,
            copy: &copy,
        };
        let Some(row) = cache.edit(&key)? else {
            continue;
        };
        let file = path(Path::new(&root), Path::new(&rel), &copy);
        writes.push((file, row.json.into_bytes()));
        saved.push((root.clone(), rel.clone(), copy.clone(), row.updated_ns));
    }
    if writes.is_empty() {
        return Ok(0);
    }
    let n = writes.len();
    let label = if n == 1 {
        "Save edit".to_owned()
    } else {
        format!("Save {n} edits")
    };
    fileops.write_edits(&label, writes)?;
    let at = now_ns();
    for (root, rel, copy, updated) in saved {
        let key = crate::cache::EditKey {
            root: &root,
            rel: &rel,
            copy: &copy,
        };
        cache.mark_edit_saved(&key, updated, at)?;
    }
    Ok(n)
}
