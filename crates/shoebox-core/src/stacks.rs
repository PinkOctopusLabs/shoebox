//! Stacks: all files of one frame shown as one item.
//!
//! Files join a stack when they share a base name in the same folder
//! (`x.nef`, `x.jpg`, `x.tif`, `x-1.tif`), or the same capture time
//! including sub-seconds plus camera serial (so an export in another folder
//! joins its NEF, but burst frames a fraction of a second apart don't).
//! The primary is the NEF, else a DNG, TIFF, JPG, PNG, HEIC, AVIF. A
//! camera DNG beside a NEF of the same name (one converted from it) stacks
//! with it, the NEF first. A **variant** is a
//! member carrying a `-N` suffix whose format another member already has
//! (Lightroom's `-1`/`-2` virtual-copy exports): different pixels, shown
//! only when the stack is expanded.
//!
//! A person can also add a file to another stack as a variant by hand (from
//! duplicate review): such links live in `<root>/.shoebox/stacks.json` in
//! every writable root, like events.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use shoebox_decode::Format;

use crate::catalog::Photo;

/// Disjoint sets with path halving.
struct UnionFind(Vec<usize>);

impl UnionFind {
    fn new(n: usize) -> Self {
        Self((0..n).collect())
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[b.max(a)] = a.min(b);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    /// Photo index shown in the grid.
    pub primary: usize,
    /// Every member except variants, primary first, in primary order.
    pub members: Vec<usize>,
    /// Different-pixel members (virtual copies), shown when expanded.
    pub variants: Vec<usize>,
}

impl Stack {
    pub fn len(&self) -> usize {
        self.members.len() + self.variants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every member, primary first, variants last.
    pub fn all(&self) -> impl Iterator<Item = usize> + '_ {
        self.members.iter().chain(&self.variants).copied()
    }

    /// `NEF+JPG+TIF` (one entry per format among non-variant members).
    pub fn badge(&self, photos: &[Photo]) -> String {
        let mut seen: Vec<&'static str> = Vec::new();
        for &m in &self.members {
            let l = photos[m].format.label();
            if !seen.contains(&l) {
                seen.push(l);
            }
        }
        seen.join("+")
    }
}

fn rank(f: Format) -> u8 {
    match f {
        Format::Nef => 0,
        Format::Dng => 1,
        Format::Tiff => 2,
        Format::Jpeg => 3,
        Format::Png => 4,
        Format::Heic => 5,
        Format::Avif => 6,
        // A Live Photo's video is never the photo shown.
        Format::Mov | Format::Mp4 => 7,
    }
}

/// True if `stem` ends in `-N` (1–2 digits) whose removal gives `base`.
fn suffix_of(stem: &str, base: &str) -> bool {
    stem.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()))
}

/// `file` joins the stack of `of` as a variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub file: PathBuf,
    pub of: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LinksFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    variants: Vec<Link>,
}

/// `<root>/.shoebox/stacks.json`.
pub fn links_file(root: &Path) -> PathBuf {
    root.join(".shoebox").join("stacks.json")
}

/// Every root's links, merged (one link per file; later roots win).
pub fn load_links(roots: &[PathBuf]) -> Vec<Link> {
    let mut by_file: HashMap<PathBuf, Link> = HashMap::new();
    for root in roots {
        let Ok(bytes) = std::fs::read(links_file(root)) else {
            continue;
        };
        match serde_json::from_slice::<LinksFile>(&bytes) {
            Ok(f) => {
                for l in f.variants {
                    by_file.insert(l.file.clone(), l);
                }
            }
            Err(e) => tracing::warn!("ignoring unreadable {}: {e}", links_file(root).display()),
        }
    }
    let mut v: Vec<Link> = by_file.into_values().collect();
    v.sort_by(|a, b| a.file.cmp(&b.file));
    v
}

/// The bytes of `stacks.json` for every writable root.
pub fn plan_save_links(
    writable_roots: &[PathBuf],
    links: &[Link],
) -> crate::error::Result<Vec<(PathBuf, Vec<u8>)>> {
    let body = serde_json::to_vec_pretty(&LinksFile {
        version: 1,
        variants: links.to_vec(),
    })
    .map_err(|e| crate::error::CoreError::Invalid(format!("stacks: {e}")))?;
    writable_roots
        .iter()
        .map(|r| {
            let dir = r.join(".shoebox");
            std::fs::create_dir_all(&dir).map_err(crate::error::io(&dir))?;
            Ok((links_file(r), body.clone()))
        })
        .collect()
}

/// Group photos into stacks. Returns the stacks and, for each photo, the
/// index of its stack. `links` add files to other stacks as variants.
pub fn build(photos: &[Photo], links: &[Link]) -> (Vec<Stack>, Vec<usize>) {
    build_apart(photos, links, &HashSet::new())
}

/// [`build`], with `apart` (photo merges) never joined by capture time:
/// a merge carries its reference's EXIF but is a photo of its own.
pub fn build_apart(
    photos: &[Photo],
    links: &[Link],
    apart: &HashSet<&Path>,
) -> (Vec<Stack>, Vec<usize>) {
    let n = photos.len();
    let mut uf = UnionFind::new(n);
    let mut linked: HashSet<usize> = HashSet::new();
    if !links.is_empty() {
        let at: HashMap<&Path, usize> = photos
            .iter()
            .enumerate()
            .map(|(i, p)| (p.path.as_path(), i))
            .collect();
        for l in links {
            if let (Some(&a), Some(&b)) = (at.get(l.file.as_path()), at.get(l.of.as_path()))
                && a != b
            {
                uf.union(a, b);
                linked.insert(a);
            }
        }
    }

    // Same folder, same base name (the rule duplicate detection uses, see
    // `pipeline::sibling_key`). Folders are numbered by their path's bytes,
    // which hash far faster than a `Path` (hashed component by component).
    let mut folders: HashMap<&std::ffi::OsStr, u32> = HashMap::new();
    let named: Vec<Option<(u32, String)>> = photos
        .iter()
        .map(|p| {
            let dir = p.path.parent()?.as_os_str();
            let next = folders.len() as u32;
            let id = *folders.entry(dir).or_insert(next);
            Some((id, p.path.file_stem()?.to_str()?.to_lowercase()))
        })
        .collect();
    let stems: HashSet<(u32, &str)> = named
        .iter()
        .flatten()
        .map(|(d, s)| (*d, s.as_str()))
        .collect();
    let mut first: HashMap<(u32, &str), usize> = HashMap::new();
    for (i, k) in named.iter().enumerate() {
        if let Some((dir, stem)) = k {
            let base = crate::pipeline::sibling_stem(stem, |h| stems.contains(&(*dir, h)));
            match first.get(&(*dir, base)) {
                Some(&j) => uf.union(i, j),
                None => {
                    first.insert((*dir, base), i);
                }
            }
        }
    }

    // Same exact capture time (with sub-seconds) and camera serial.
    let mut by_time: HashMap<(&str, &str, &str), usize> = HashMap::new();
    for (i, p) in photos.iter().enumerate() {
        let x = &p.meta.exif;
        if apart.contains(p.path.as_path()) {
            continue;
        }
        if let (Some(t), Some(ss), Some(serial)) = (&x.capture_time, &x.subsec, &x.serial) {
            let key = (t.as_str(), ss.trim(), serial.as_str());
            match by_time.get(&key) {
                Some(&j) => uf.union(i, j),
                None => {
                    by_time.insert(key, i);
                }
            }
        }
    }

    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        groups.entry(uf.find(i)).or_default().push(i);
    }
    let mut stacks: Vec<Stack> = groups
        .into_values()
        .map(|mut ids| {
            let stem = |i: usize| -> String {
                photos[i]
                    .path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default()
            };
            ids.sort_by(|&a, &b| {
                (rank(photos[a].format), stem(a).len(), &photos[a].path).cmp(&(
                    rank(photos[b].format),
                    stem(b).len(),
                    &photos[b].path,
                ))
            });
            // A variant: `-N` suffix on a name, with that format already present
            // under the unsuffixed name.
            let (mut members, mut variants) = (Vec::new(), Vec::new());
            for &i in &ids {
                let is_variant = linked.contains(&i)
                    || ids.iter().any(|&j| {
                        j != i
                            && photos[j].format == photos[i].format
                            && suffix_of(&stem(i), &stem(j))
                    });
                if is_variant {
                    variants.push(i)
                } else {
                    members.push(i)
                }
            }
            if members.is_empty() {
                members.push(variants.remove(0));
            }
            Stack {
                primary: members[0],
                members,
                variants,
            }
        })
        .collect();
    stacks.sort_by_key(|s| s.primary);
    let mut stack_of = vec![0; n];
    for (si, s) in stacks.iter().enumerate() {
        for m in s.all() {
            stack_of[m] = si;
        }
    }
    (stacks, stack_of)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::Meta;
    use std::path::PathBuf;

    fn photo(path: &str, time: Option<(&str, &str)>) -> Photo {
        let p = PathBuf::from(path);
        let mut meta = Meta::default();
        if let Some((t, ss)) = time {
            meta.exif.capture_time = Some(t.into());
            meta.exif.subsec = Some(ss.into());
            meta.exif.serial = Some("1234567".into());
        }
        Photo {
            root: 0,
            rel: path.trim_start_matches('/').into(),
            format: Format::from_path(&p).unwrap(),
            path: p,
            size: 0,
            mtime_ns: 0,
            meta,
            marks: Default::default(),
            sidecar: None,
            sidecar_sig: String::new(),
            change: None,
            cloud: false,
        }
    }

    #[test]
    fn frame_stacks_with_variants_and_exports_elsewhere() {
        let t = Some(("2026:05:01 09:15:02", "65"));
        let photos = vec![
            photo("/a/20260501-091502-65.jpg", t),
            photo("/a/20260501-091502-65.nef", t),
            photo("/a/20260501-091502-65.tif", t),
            photo("/a/20260501-091502-65-1.tif", t),
            photo("/exports/holiday-12.tif", t), // same frame elsewhere
            photo(
                "/a/20260501-091502-80.nef",
                Some(("2026:05:01 09:15:02", "80")),
            ), // burst neighbour
        ];
        let (stacks, of) = build(&photos, &[]);
        assert_eq!(stacks.len(), 2);
        let s = &stacks[of[0]];
        assert_eq!(photos[s.primary].format, Format::Nef);
        assert_eq!(s.badge(&photos), "NEF+TIF+JPG");
        assert_eq!(s.variants, vec![3], "the -1 TIFF is a virtual copy");
        assert!(
            s.members.contains(&4),
            "export in another folder joins by capture time"
        );
        assert_ne!(of[5], of[0], "a burst frame 0.15 s later is its own stack");

        // A carved preview elsewhere, linked by hand, joins as a variant.
        let mut photos = photos;
        photos.push(photo("/_undated/6048x4032/f0001.jpg", None));
        let link = Link {
            file: "/_undated/6048x4032/f0001.jpg".into(),
            of: "/a/20260501-091502-65.nef".into(),
        };
        let (stacks, of) = build(&photos, &[link]);
        let s = &stacks[of[6]];
        assert_eq!(of[6], of[1]);
        assert!(s.variants.contains(&6));
        assert_eq!(photos[s.primary].format, Format::Nef);
    }

    #[test]
    fn copies_without_a_base_are_plain_members() {
        let photos = vec![photo("/b/x-1.jpg", None), photo("/b/y.jpg", None)];
        let (stacks, _) = build(&photos, &[]);
        assert_eq!(stacks.len(), 2);
        assert!(stacks.iter().all(|s| s.variants.is_empty()));
    }
}
