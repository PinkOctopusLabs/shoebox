//! Stacks: all files of one frame shown as one item.
//!
//! Files join a stack when they share a base name in the same folder
//! (`x.nef`, `x.jpg`, `x.tif`, `x-1.tif`), or the same capture time
//! including sub-seconds plus camera serial (so an export in another folder
//! joins its NEF, but burst frames a fraction of a second apart don't).
//! The primary is the NEF, else TIFF, JPG, PNG, HEIC. A **variant** is a
//! member carrying a `-N` suffix whose format another member already has
//! (Lightroom's `-1`/`-2` virtual-copy exports): different pixels, shown
//! only when the stack is expanded.

use std::collections::{HashMap, HashSet};

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
        Format::Tiff => 1,
        Format::Jpeg => 2,
        Format::Png => 3,
        Format::Heic => 4,
    }
}

/// True if `stem` ends in `-N` (1–2 digits) whose removal gives `base`.
fn suffix_of(stem: &str, base: &str) -> bool {
    stem.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|n| !n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()))
}

/// Group photos into stacks. Returns the stacks and, for each photo, the
/// index of its stack.
pub fn build(photos: &[Photo]) -> (Vec<Stack>, Vec<usize>) {
    let n = photos.len();
    let mut uf = UnionFind::new(n);

    // Same folder, same base name (the rule duplicate detection uses).
    let stems: HashSet<crate::pipeline::FolderStem> = photos
        .iter()
        .filter_map(|p| crate::pipeline::folder_stem(&p.path))
        .collect();
    let keys: Vec<Option<crate::pipeline::FolderStem>> = photos
        .iter()
        .map(|p| crate::pipeline::sibling_key(&p.path, &stems))
        .collect();
    let mut first: HashMap<&crate::pipeline::FolderStem, usize> = HashMap::new();
    for (i, k) in keys.iter().enumerate() {
        if let Some(k) = k {
            match first.get(k) {
                Some(&j) => uf.union(i, j),
                None => {
                    first.insert(k, i);
                }
            }
        }
    }

    // Same exact capture time (with sub-seconds) and camera serial.
    let mut by_time: HashMap<(String, String, String), usize> = HashMap::new();
    for (i, p) in photos.iter().enumerate() {
        let x = &p.meta.exif;
        if let (Some(t), Some(ss), Some(serial)) = (&x.capture_time, &x.subsec, &x.serial) {
            let key = (t.clone(), ss.trim().to_owned(), serial.clone());
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
                let is_variant = ids.iter().any(|&j| {
                    j != i && photos[j].format == photos[i].format && suffix_of(&stem(i), &stem(j))
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
        let (stacks, of) = build(&photos);
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
    }

    #[test]
    fn copies_without_a_base_are_plain_members() {
        let photos = vec![photo("/b/x-1.jpg", None), photo("/b/y.jpg", None)];
        let (stacks, _) = build(&photos);
        assert_eq!(stacks.len(), 2);
        assert!(stacks.iter().all(|s| s.variants.is_empty()));
    }
}
