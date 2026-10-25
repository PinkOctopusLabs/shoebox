//! Duplicate review (Tasks → Duplicates): find groups across the library,
//! let a person decide per file, and turn the decisions into one journaled
//! batch.
//!
//! Nothing moves without a person: auto-select only proposes, and only for
//! files taken at the same moment as the copy being kept (bursts and other
//! exposures stay undecided). Reference files are never moved, and every
//! group keeps at least one file. A moved file goes to `_duplicates/` in its
//! own root, keeping its relative path, with its sidecars.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use shoebox_decode::Format;

use crate::cache::Cache;
use crate::dupes::{self, Config, Match};
use crate::error::{CoreError, Result};
use crate::hash::Digest;
use crate::pipeline;
use crate::scan::{self, Role, Root};
use crate::stacks::Link;

/// A file in some group.
#[derive(Debug, Clone)]
pub struct File {
    pub path: PathBuf,
    pub root: usize,
    pub rel: String,
    pub role: Role,
    pub format: Format,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub blake3: Digest,
    /// Capture time in seconds, if known.
    pub capture: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Index into [`Review::files`].
    pub file: usize,
    /// Direct match to the keeper (None for the keeper itself).
    pub to_keeper: Option<Match>,
    /// The closest copy anywhere: a reference file, the keeper or another
    /// member (index into [`Review::files`]). A carved preview's best match
    /// is its own NEF, even when a burst neighbour became the keeper.
    pub best: Option<(usize, Match)>,
    /// Taken at the same moment as the best match, or if either time is
    /// unknown, as the keeper (or that's unknown too).
    pub same_exposure: bool,
    /// The copy is certain: identical bytes or image data, or both capture
    /// times are known and equal. Only these are proposed for moving.
    pub certain: bool,
}

#[derive(Debug, Clone)]
pub struct Group {
    /// The keeper first, then its matches.
    pub entries: Vec<Entry>,
    pub has_reference: bool,
}

impl Group {
    /// Only reference files: nothing here can move (such as a NEF and its
    /// Lightroom export, both in the library).
    pub fn reference_only(&self, r: &Review) -> bool {
        self.entries
            .iter()
            .all(|e| r.files[e.file].role == Role::Reference)
    }

    /// The closest match level and largest distance in the group, for the list.
    pub fn summary(&self) -> Option<Match> {
        self.entries
            .iter()
            .filter_map(|e| e.best.map(|(_, m)| m).or(e.to_keeper))
            .max_by_key(|m| (m.level, m.distance))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Review {
    pub roots: Vec<Root>,
    pub files: Vec<File>,
    pub groups: Vec<Group>,
    pub config: Config,
    pub stats: pipeline::Stats,
    /// What the search read, to group again at another threshold
    /// ([`regroup`]) without reading a file.
    pub found: std::sync::Arc<Found>,
}

/// A search's files and hashes before grouping: the slow part of
/// [`find`] (scanning, hashing), kept so the list can follow the
/// threshold as it moves.
#[derive(Default)]
pub struct Found {
    roots: Vec<Root>,
    analyzed: Vec<pipeline::Analyzed>,
    items: Vec<dupes::Item>,
    ignored: HashSet<(Digest, Digest)>,
    settled: HashSet<PathBuf>,
    stats: pipeline::Stats,
}

/// What a person decided for one file of a group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Decision {
    #[default]
    Undecided,
    Keep,
    /// Move to `_duplicates/`.
    Move,
    /// Keep, and add it to the keeper's stack as a variant.
    Variant,
    /// Not a duplicate of this group's keeper; never suggest the pair again.
    Ignore,
}

/// (file, match to the kept copy, best match, same exposure, certain).
type RawEntry = (usize, Option<Match>, Option<(usize, Match)>, bool, bool);

/// Find duplicate groups across the roots (hashes come from the cache when
/// files are unchanged). Dismissed pairs are left out, and so are files in
/// `settled` (already added to a stack as a variant: decided). With
/// `only`, just those files are looked at, so both files of every match
/// are among them (an event, a collection or a folder).
pub fn find(
    cache: &mut Cache,
    roots: Vec<Root>,
    config: Config,
    settled: &HashSet<PathBuf>,
    only: Option<&HashSet<PathBuf>>,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Review> {
    let mut files = scan::scan(&roots, &[])?;
    if let Some(only) = only {
        files.retain(|f| only.contains(&f.path));
    }
    let (analyzed, stats) = pipeline::analyze_all_progress(cache, &roots, files, progress)?;
    let items = pipeline::items(&roots, &analyzed);
    let found = Found {
        ignored: cache.ignored_pairs()?,
        settled: settled.clone(),
        roots,
        analyzed,
        items,
        stats,
    };
    Ok(regroup(std::sync::Arc::new(found), config))
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Found")
            .field("files", &self.items.len())
            .finish_non_exhaustive()
    }
}

/// Group a search's files again at another threshold (fast: nothing is
/// read).
pub fn regroup(found: std::sync::Arc<Found>, config: Config) -> Review {
    let Found {
        roots,
        analyzed,
        items,
        ignored,
        settled,
        stats,
    } = &*found;
    let roots = roots.clone();
    let stats = *stats;
    let is_ignored = |a: usize, b: usize| {
        if settled.contains(&analyzed[a].file.path) || settled.contains(&analyzed[b].file.path) {
            return true;
        }
        let (x, y) = (items[a].blake3, items[b].blake3);
        ignored.contains(&if x <= y { (x, y) } else { (y, x) })
    };
    let raw = dupes::find_groups_ignoring(items, &config, &is_ignored);

    // Keep only files that are in a group, renumbered.
    let mut index: HashMap<usize, usize> = HashMap::new();
    let mut files = Vec::new();
    let mut add = |i: usize| -> usize {
        *index.entry(i).or_insert_with(|| {
            let a = &analyzed[i];
            files.push(File {
                path: a.file.path.clone(),
                root: a.file.root,
                rel: a.file.rel.clone(),
                role: roots[a.file.root].role,
                format: a.file.format,
                size: a.file.size,
                width: a.analysis.width,
                height: a.analysis.height,
                blake3: a.analysis.blake3,
                capture: items[i].capture,
            });
            files.len() - 1
        })
    };
    // Strongest level first; among equals, a copy from the same moment
    // beats a look-alike burst neighbour, and a reference copy wins ties.
    let rank = |me: usize, (j, x): &(usize, Match)| {
        (
            x.level,
            dupes::capture_gap(&items[*j], &items[me]).is_some(),
            x.distance,
            items[*j].role != Role::Reference,
        )
    };
    let entry = |me: usize, root: usize, best: Option<(usize, Match)>| {
        let other = best.map_or(root, |(j, _)| j);
        let gap = dupes::capture_gap(&items[other], &items[me]);
        let times_known = items[other].capture.is_some() && items[me].capture.is_some();
        let exact = best.is_some_and(|(_, x)| x.level <= dupes::Level::ImageData);
        // With no time on the closest copy, the kept copy's tells (a burst
        // neighbour of the keeper whose closest copy is an undated export).
        let same_exposure = if times_known {
            gap.is_none()
        } else {
            dupes::capture_gap(&items[root], &items[me]).is_none()
        };
        (
            me,
            dupes::compare(&items[root], &items[me], &config),
            best,
            same_exposure,
            exact || (times_known && gap.is_none()),
        )
    };
    let mut raw_groups: Vec<(usize, Vec<RawEntry>, bool)> = Vec::new();
    for g in &raw {
        let all: Vec<usize> = std::iter::once(g.keeper)
            .chain(g.members.iter().map(|m| m.item))
            .collect();
        if all.iter().all(|&i| items[i].role == Role::Reference) {
            // Reference only: shown (on request) as found.
            let entries = g
                .members
                .iter()
                .map(|m| entry(m.item, g.keeper, m.to_keeper.map(|x| (g.keeper, x))))
                .collect();
            raw_groups.push((g.keeper, entries, true));
            continue;
        }
        // Each file to decide on goes with its closest copy: the best
        // reference match, or another file of the group.
        let mut best: HashMap<usize, (usize, Match)> = HashMap::new();
        for m in g
            .members
            .iter()
            .filter(|m| items[m.item].role == Role::Scan)
        {
            let me = m.item;
            let mut cands: Vec<(usize, Match)> = m.matched_ref.into_iter().collect();
            for &o in &all {
                if o != me
                    && let Some(x) = dupes::compare(&items[o], &items[me], &config)
                {
                    cands.push((o, x));
                }
            }
            if let Some(b) = cands.into_iter().min_by_key(|c| rank(me, c)) {
                best.insert(me, b);
            }
        }
        // The kept copy of a file: follow best matches through other files
        // to decide on until a reference file or a file with none.
        let root_of = |start: usize| -> usize {
            let mut x = start;
            for _ in 0..16 {
                match best.get(&x) {
                    None => return x,
                    Some(&(b, _)) if items[b].role == Role::Reference => return b,
                    // Two files each other's best: the larger one is kept.
                    Some(&(b, _)) if best.get(&b).is_some_and(|&(c, _)| c == x) => {
                        return if (std::cmp::Reverse(items[b].pixels), b)
                            < (std::cmp::Reverse(items[x].pixels), x)
                        {
                            b
                        } else {
                            x
                        };
                    }
                    Some(&(b, _)) => x = b,
                }
            }
            x
        };
        let mut buckets: Vec<(usize, Vec<usize>)> = Vec::new();
        for &me in all.iter().filter(|&&i| items[i].role == Role::Scan) {
            let root = root_of(me);
            if root == me {
                continue;
            }
            match buckets.iter_mut().find(|(r, _)| *r == root) {
                Some((_, v)) => v.push(me),
                None => buckets.push((root, vec![me])),
            }
        }
        for (root, members) in buckets {
            let entries = members
                .into_iter()
                .map(|me| entry(me, root, best.get(&me).copied()))
                .collect();
            raw_groups.push((root, entries, false));
        }
    }
    raw_groups.sort_by_key(|(root, _, _)| *root);
    let groups = raw_groups
        .into_iter()
        .map(|(root, members, _)| {
            let mut entries = vec![Entry {
                file: add(root),
                to_keeper: None,
                best: None,
                same_exposure: true,
                certain: true,
            }];
            let mut has_reference = items[root].role == Role::Reference;
            for (me, to_keeper, best, same_exposure, certain) in members {
                has_reference |= items[me].role == Role::Reference;
                entries.push(Entry {
                    file: add(me),
                    to_keeper,
                    best: best.map(|(j, x)| (add(j), x)),
                    same_exposure,
                    certain,
                });
            }
            Group {
                entries,
                has_reference,
            }
        })
        .collect();
    Review {
        roots,
        files,
        groups,
        config,
        stats,
        found,
    }
}

/// Proposed decisions for a group, entry by entry: keep the keeper (the
/// reference copy, else NEF over TIFF over JPG, else the highest
/// resolution), keep other reference files, move the copies taken at the
/// same moment (or with no capture time to tell, like a resized export or
/// a carved preview), and leave other exposures (a burst's neighbours,
/// different photos) for a person to look at. `rule` says how alike a
/// copy must be to be moved ([`AutoSelect`]).
pub fn auto_select(r: &Review, g: &Group, rule: AutoSelect) -> Vec<Decision> {
    g.entries
        .iter()
        .enumerate()
        .map(|(k, e)| {
            let move_it = e.best.is_some_and(|(_, m)| {
                m.level <= dupes::Level::ImageData
                    || (rule.up_to.is_some_and(|d| m.distance <= d)
                        && (e.same_exposure || !rule.same_moment))
            });
            if k == 0 || r.files[e.file].role == Role::Reference {
                Decision::Keep
            } else if move_it {
                Decision::Move
            } else {
                Decision::Undecided
            }
        })
        .collect()
}

/// Which copies auto-select marks to move (the rest stay undecided).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoSelect {
    /// The largest perceptual distance marked ([`dupes::percent`] shows it
    /// as a percentage); None marks only identical files and the same image
    /// data (100%), which are always marked.
    pub up_to: Option<u32>,
    /// Below 100%, only copies taken at the same moment as the copy they
    /// match, or with no capture time to tell (burst frames stay
    /// undecided).
    pub same_moment: bool,
}

impl Default for AutoSelect {
    fn default() -> Self {
        Self {
            up_to: None,
            same_moment: true,
        }
    }
}

/// What applying the decisions will do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// (file, wanted destination) before collision suffixes and sidecars.
    pub moves: Vec<(PathBuf, PathBuf)>,
    pub links: Vec<Link>,
    pub ignores: Vec<(Digest, Digest)>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty() && self.links.is_empty() && self.ignores.is_empty()
    }
}

/// `<root>/_duplicates/<rel>`.
pub fn duplicates_dest(root: &Path, rel: &str) -> PathBuf {
    root.join("_duplicates").join(rel)
}

/// Turn decisions (by path) into a plan, refusing anything unsafe: moving a
/// reference file, or moving every file of a group.
pub fn plan(r: &Review, decisions: &HashMap<PathBuf, Decision>) -> Result<Plan> {
    let mut out = Plan::default();
    let mut moved: HashSet<usize> = HashSet::new();
    let decision = |f: usize| decisions.get(&r.files[f].path).copied().unwrap_or_default();
    for g in &r.groups {
        let ds: Vec<Decision> = g.entries.iter().map(|e| decision(e.file)).collect();
        let staying = ds
            .iter()
            .filter(|d| !matches!(d, Decision::Move | Decision::Ignore))
            .count();
        if ds.contains(&Decision::Move) && staying == 0 {
            return Err(CoreError::Refused(format!(
                "every file matching {} would be moved; keep at least one",
                r.files[g.entries[0].file].path.display()
            )));
        }
        // Variants join the stack of the copy being kept.
        let anchor = g
            .entries
            .iter()
            .zip(&ds)
            .find(|(_, d)| **d == Decision::Keep)
            .map_or(g.entries[0].file, |(e, _)| e.file);
        let keeper = g.entries[0].file;
        for (k, (e, d)) in g.entries.iter().zip(&ds).enumerate() {
            let f = &r.files[e.file];
            match d {
                Decision::Move => {
                    if f.role == Role::Reference {
                        return Err(CoreError::Refused(format!(
                            "{} is in a reference folder and can't be moved",
                            f.path.display()
                        )));
                    }
                    if moved.insert(e.file) {
                        out.moves.push((
                            f.path.clone(),
                            duplicates_dest(&r.roots[f.root].path, &f.rel),
                        ));
                    }
                }
                Decision::Variant if e.file != anchor => out.links.push(Link {
                    file: f.path.clone(),
                    of: r.files[anchor].path.clone(),
                }),
                Decision::Ignore if k == 0 => {
                    // The keeper itself: dismiss the whole group.
                    for o in &g.entries[1..] {
                        out.ignores.push((f.blake3, r.files[o.file].blake3));
                    }
                }
                Decision::Ignore => out.ignores.push((f.blake3, r.files[keeper].blake3)),
                _ => {}
            }
        }
    }
    Ok(out)
}

/// The file about to move must be the file that was reviewed.
/// [`check_unchanged`] for many files at once, read in parallel, calling
/// `progress(done, total)` as each is checked. The first changed or
/// unreadable file refuses the lot.
pub fn check_all_unchanged(
    r: &Review,
    paths: &[&Path],
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<()> {
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let by_path: HashMap<&Path, &Digest> = r
        .files
        .iter()
        .map(|f| (f.path.as_path(), &f.blake3))
        .collect();
    let done = AtomicUsize::new(0);
    paths.par_iter().try_for_each(|&path| {
        let want = by_path
            .get(path)
            .ok_or_else(|| CoreError::Invalid(format!("{} isn't in the review", path.display())))?;
        let data = std::fs::read(path).map_err(crate::error::io(path))?;
        if crate::hash::blake3_of(&data) != **want {
            return Err(CoreError::Refused(format!(
                "{} changed since duplicates were found; find them again",
                path.display()
            )));
        }
        progress(done.fetch_add(1, Ordering::Relaxed) + 1, paths.len());
        Ok(())
    })
}

pub fn check_unchanged(r: &Review, path: &Path) -> Result<()> {
    let f = r
        .files
        .iter()
        .find(|f| f.path == path)
        .ok_or_else(|| CoreError::Invalid(format!("{} isn't in the review", path.display())))?;
    let data = std::fs::read(path).map_err(crate::error::io(path))?;
    if crate::hash::blake3_of(&data) != f.blake3 {
        return Err(CoreError::Refused(format!(
            "{} changed since duplicates were found; find them again",
            path.display()
        )));
    }
    Ok(())
}
