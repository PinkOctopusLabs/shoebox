//! Duplicate detection: exact, same-image-data and perceptual matches,
//! merged into groups.

pub mod mih;
pub mod report;

use std::collections::HashMap;

use shoebox_decode::Format;

use crate::hash::{Digest, PHashes};
use crate::scan::Role;
use mih::MultiIndex;

/// Defaults from the spec; starting points to tune against real files.
pub const DEFAULT_NEAR_IDENTICAL: u32 = 4;
pub const DEFAULT_SIMILAR: u32 = 10;

/// Luma standard deviation at or below which an image is treated as blank
/// (blown-out, lens cap): its perceptual hashes are noise, so it only matches
/// exactly or by image data. Measured on a real library: blank frames are 0.0,
/// the 1st percentile is 2.4, the median 45.
pub const FLAT_STDDEV: f32 = 3.0;
/// Below this an image is low-detail (mostly dark night shots); perceptual
/// matches are only accepted when near-identical.
pub const LOW_DETAIL_STDDEV: f32 = 8.0;

#[derive(Debug, Clone)]
pub struct Item {
    pub role: Role,
    pub format: Format,
    pub blake3: Digest,
    pub image_data_hash: Option<Digest>,
    /// Upright image at rotations 0/90/180/270; empty if undecodable.
    pub hashes: Vec<PHashes>,
    pub pixels: u64,
    /// Files of the same frame (same folder and base name, ignoring `-N`
    /// suffixes) share an id. Siblings are different renditions, not
    /// duplicates, so they only match each other exactly or by image data.
    pub siblings: Option<usize>,
    pub luma_stddev: Option<f32>,
    /// Capture time in seconds (see `exif::capture_seconds`), if known.
    pub capture: Option<f64>,
    /// A photo merge Shoebox made (`merges`): it looks like its reference
    /// by design, so it matches only exactly or by image data.
    pub merged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Exact,
    ImageData,
    NearIdentical,
    Similar,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Exact => "exact",
            Level::ImageData => "image_data",
            Level::NearIdentical => "near_identical",
            Level::Similar => "similar",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub level: Level,
    pub distance: u32,
    /// Degrees clockwise that `b` was rotated to match `a` (perceptual only).
    pub rotation: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Maximum combined Hamming distance (inclusive) for a perceptual match.
    pub threshold: u32,
    /// Distances at or below this are labelled near-identical.
    pub near_identical: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_SIMILAR,
            near_identical: DEFAULT_NEAR_IDENTICAL,
        }
    }
}

pub fn same_frame(a: &Item, b: &Item) -> bool {
    a.siblings.is_some() && a.siblings == b.siblings
}

/// Perceptual distance allowed between two files, or None if their hashes
/// shouldn't be compared at all.
fn perceptual_limit(a: &Item, b: &Item, cfg: &Config) -> Option<u32> {
    if same_frame(a, b) || a.merged || b.merged {
        return None;
    }
    let detail = match (a.luma_stddev, b.luma_stddev) {
        (Some(x), Some(y)) => x.min(y),
        _ => f32::INFINITY,
    };
    if detail <= FLAT_STDDEV {
        None
    } else if detail <= LOW_DETAIL_STDDEV {
        Some(cfg.threshold.min(cfg.near_identical))
    } else {
        Some(cfg.threshold)
    }
}

/// Best direct match between two files, if any.
pub fn compare(a: &Item, b: &Item, cfg: &Config) -> Option<Match> {
    if a.blake3 == b.blake3 {
        return Some(Match {
            level: Level::Exact,
            distance: 0,
            rotation: 0,
        });
    }
    if a.image_data_hash.is_some() && a.image_data_hash == b.image_data_hash {
        return Some(Match {
            level: Level::ImageData,
            distance: 0,
            rotation: 0,
        });
    }
    let limit = perceptual_limit(a, b, cfg)?;
    let upright = a.hashes.first()?;
    let (rot, distance) = b
        .hashes
        .iter()
        .enumerate()
        .map(|(r, h)| (r, upright.distance(h)))
        .min_by_key(|&(r, d)| (d, r))?;
    (distance <= limit).then(|| Match {
        level: if distance <= cfg.near_identical {
            Level::NearIdentical
        } else {
            Level::Similar
        },
        distance,
        rotation: (rot * 90) as u16,
    })
}

/// Seconds between two capture times, if both are known and they differ.
/// Different capture times mean different exposures (such as burst frames),
/// which look alike but are not copies of each other.
pub fn capture_gap(a: &Item, b: &Item) -> Option<f64> {
    let gap = (a.capture? - b.capture?).abs();
    (gap >= 0.005).then_some(gap)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suggestion {
    /// A scan file that directly matches a reference file taken at the same time.
    Duplicate,
    /// A scan file worth a look: no reference match, or a different exposure.
    Review,
    /// Reference files are never moved.
    None,
}

impl Suggestion {
    pub fn as_str(self) -> &'static str {
        match self {
            Suggestion::Duplicate => "duplicate",
            Suggestion::Review => "review",
            Suggestion::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Member {
    pub item: usize,
    /// Direct match to the group's keeper. Always present: groups don't chain.
    pub to_keeper: Option<Match>,
    /// For scan files: the closest direct match to any reference file.
    pub matched_ref: Option<(usize, Match)>,
}

impl Member {
    pub fn suggestion(&self, items: &[Item]) -> Suggestion {
        let me = &items[self.item];
        if me.role != Role::Scan {
            return Suggestion::None;
        }
        match self.matched_ref {
            Some((r, _)) if capture_gap(&items[r], me).is_none() => Suggestion::Duplicate,
            _ => Suggestion::Review,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Group {
    pub keeper: usize,
    pub has_reference: bool,
    /// Every member except the keeper; each matches the keeper directly.
    pub members: Vec<Member>,
}

fn format_rank(f: Format) -> u8 {
    match f {
        Format::Nef | Format::Arw => 0,
        // Raw data too (a camera's DNG, or a NEF converted).
        Format::Dng => 1,
        Format::Tiff | Format::Psd => 2,
        // HEIC is a camera original (phone), so it ranks with the JPG.
        Format::Jpeg | Format::Heic => 3,
        Format::Png | Format::Avif | Format::Webp => 4,
        Format::Mov | Format::Mp4 | Format::Mpg => 5,
    }
}

/// Keeper preference: reference copy, then NEF > DNG > TIFF > JPG > PNG,
/// then the highest resolution, then scan order.
fn keeper_key(items: &[Item], i: usize) -> (bool, u8, std::cmp::Reverse<u64>, usize) {
    let it = &items[i];
    (
        it.role != Role::Reference,
        format_rank(it.format),
        std::cmp::Reverse(it.pixels),
        i,
    )
}

/// Candidate lookups: equal hashes, plus a multi-index hash over every
/// rotation of every file's pHash.
struct Index {
    exact: HashMap<Digest, Vec<usize>>,
    data: HashMap<Digest, Vec<usize>>,
    tree: MultiIndex,
}

impl Index {
    fn new(items: &[Item]) -> Self {
        let mut exact: HashMap<Digest, Vec<usize>> = HashMap::new();
        let mut data: HashMap<Digest, Vec<usize>> = HashMap::new();
        let mut tree = MultiIndex::new();
        for (i, it) in items.iter().enumerate() {
            exact.entry(it.blake3).or_default().push(i);
            if let Some(h) = it.image_data_hash {
                data.entry(h).or_default().push(i);
            }
            for h in &it.hashes {
                tree.insert(h.phash, i);
            }
        }
        Self { exact, data, tree }
    }

    /// Superset of the files `j` for which `compare(items[k], items[j])` can
    /// succeed (`k` as the first argument). The index holds every rotation,
    /// so `k`'s upright hash alone is enough.
    fn matches_of(&self, items: &[Item], k: usize, cfg: &Config) -> Vec<usize> {
        self.collect(items, k, cfg, items[k].hashes.first().into_iter())
    }

    /// Superset of the files `r` for which `compare(items[r], items[j])` can
    /// succeed (`j` as the second argument): `r`'s upright hash is in the
    /// index, so query with every rotation of `j`.
    fn matched_by(&self, items: &[Item], j: usize, cfg: &Config) -> Vec<usize> {
        self.collect(items, j, cfg, items[j].hashes.iter())
    }

    fn collect<'a>(
        &self,
        items: &[Item],
        k: usize,
        cfg: &Config,
        queries: impl Iterator<Item = &'a PHashes>,
    ) -> Vec<usize> {
        let it = &items[k];
        let mut out: Vec<usize> = self.exact.get(&it.blake3).cloned().unwrap_or_default();
        if let Some(h) = it.image_data_hash.and_then(|h| self.data.get(&h)) {
            out.extend(h);
        }
        for h in queries {
            out.extend(
                self.tree
                    .find(h.phash, cfg.threshold)
                    .into_iter()
                    .map(|(j, _)| j),
            );
        }
        out.sort_unstable();
        out.dedup();
        out.retain(|&j| j != k);
        out
    }
}

/// Groups built around keepers: files are taken in keeper-preference order,
/// and each unassigned file collects the unassigned files that match it
/// directly. Matches of matches never join a group.
pub fn find_groups(items: &[Item], cfg: &Config) -> Vec<Group> {
    find_groups_ignoring(items, cfg, &|_, _| false)
}

/// [`find_groups`], skipping matches between pairs a person dismissed
/// (`ignored(a, b)` is checked both ways round by the caller's set).
pub fn find_groups_ignoring(
    items: &[Item],
    cfg: &Config,
    ignored: &(dyn Fn(usize, usize) -> bool + Sync),
) -> Vec<Group> {
    use rayon::prelude::*;

    let index = Index::new(items);
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| keeper_key(items, i));

    // The expensive part, every file's direct matches, runs in parallel; the
    // greedy assignment below is cheap and sequential.
    let matches: Vec<Vec<(usize, Match)>> = (0..items.len())
        .into_par_iter()
        .map(|k| {
            index
                .matches_of(items, k, cfg)
                .into_iter()
                .filter(|&j| !ignored(k, j))
                .filter_map(|j| compare(&items[k], &items[j], cfg).map(|m| (j, m)))
                .collect()
        })
        .collect();

    let mut assigned = vec![false; items.len()];
    let mut raw: Vec<(usize, Vec<(usize, Match)>)> = Vec::new();
    for &k in &order {
        if assigned[k] {
            continue;
        }
        let members: Vec<(usize, Match)> = matches[k]
            .iter()
            .copied()
            .filter(|&(j, _)| !assigned[j])
            .collect();
        if members.is_empty() {
            continue;
        }
        assigned[k] = true;
        for &(j, _) in &members {
            assigned[j] = true;
        }
        raw.push((k, members));
    }

    let best_ref = |j: usize| -> Option<(usize, Match)> {
        index
            .matched_by(items, j, cfg)
            .into_iter()
            .filter(|&r| items[r].role == Role::Reference && !ignored(r, j))
            .filter_map(|r| compare(&items[r], &items[j], cfg).map(|m| (r, m)))
            .min_by_key(|(r, m)| (m.level, m.distance, keeper_key(items, *r)))
    };
    let mut groups: Vec<Group> = raw
        .into_par_iter()
        .map(|(k, mut members)| {
            members.sort_by_key(|&(j, _)| j);
            let has_reference = std::iter::once(k)
                .chain(members.iter().map(|&(j, _)| j))
                .any(|i| items[i].role == Role::Reference);
            Group {
                keeper: k,
                has_reference,
                members: members
                    .into_iter()
                    .map(|(j, m)| Member {
                        item: j,
                        to_keeper: Some(m),
                        matched_ref: if items[j].role == Role::Scan {
                            best_ref(j)
                        } else {
                            None
                        },
                    })
                    .collect(),
            }
        })
        .collect();
    groups.sort_by_key(|g| g.keeper);
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(role: Role, b: u8, idh: Option<u8>, ph: Option<u64>) -> Item {
        Item {
            role,
            format: Format::Jpeg,
            blake3: [b; 32],
            image_data_hash: idh.map(|x| [x; 32]),
            hashes: ph
                .map(|p| vec![PHashes { dhash: p, phash: p }; 4])
                .unwrap_or_default(),
            pixels: 100,
            siblings: None,
            luma_stddev: Some(40.0),
            capture: None,
            merged: false,
        }
    }

    fn suggestions(g: &Group, items: &[Item]) -> Vec<Suggestion> {
        g.members.iter().map(|m| m.suggestion(items)).collect()
    }

    #[test]
    fn groups_by_each_level() {
        let items = vec![
            item(Role::Reference, 1, Some(1), Some(0)),     // 0
            item(Role::Scan, 1, Some(9), None),             // 1 exact of 0
            item(Role::Scan, 2, Some(1), None),             // 2 image data of 0
            item(Role::Scan, 3, None, Some(0b111)),         // 3 near 0 (d=3)
            item(Role::Scan, 4, None, Some(u64::MAX >> 1)), // 4 unrelated
            item(Role::Scan, 5, None, Some((u64::MAX >> 1) ^ 0xFF)), // 5 similar to 4
        ];
        let g = find_groups(&items, &Config::default());
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].keeper, 0);
        let lv: Vec<_> = g[0]
            .members
            .iter()
            .map(|m| m.to_keeper.map(|x| x.level))
            .collect();
        assert_eq!(
            lv,
            vec![
                Some(Level::Exact),
                Some(Level::ImageData),
                Some(Level::NearIdentical)
            ]
        );
        assert_eq!(suggestions(&g[0], &items), vec![Suggestion::Duplicate; 3]);
        assert!(!g[1].has_reference);
        assert_eq!(suggestions(&g[1], &items), vec![Suggestion::Review]);
    }

    #[test]
    fn reference_files_are_never_suggested() {
        let items = vec![
            item(Role::Reference, 1, None, None),
            item(Role::Reference, 1, None, None),
        ];
        let g = find_groups(&items, &Config::default());
        assert_eq!(suggestions(&g[0], &items), vec![Suggestion::None]);
    }

    #[test]
    fn groups_do_not_chain() {
        // 0 (ref) ~ 1 (d=8) ~ 2 (d=8 from 1, 16 from 0): 2 is not in 0's group.
        let items = vec![
            item(Role::Reference, 1, None, Some(0)),
            item(Role::Scan, 2, None, Some(0xFF)),
            item(Role::Scan, 3, None, Some(0xFFFF)),
        ];
        let g = find_groups(&items, &Config::default());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].keeper, 0);
        assert_eq!(g[0].members.len(), 1);
        assert_eq!(g[0].members[0].item, 1);
    }

    #[test]
    fn siblings_only_match_exactly() {
        let mut a = item(Role::Scan, 1, Some(1), Some(0));
        let mut b = item(Role::Scan, 2, Some(2), Some(0)); // looks identical
        a.siblings = Some(7);
        b.siblings = Some(7);
        assert!(compare(&a, &b, &Config::default()).is_none());
        b.blake3 = a.blake3; // a byte-identical -1 copy is still a duplicate
        assert_eq!(
            compare(&a, &b, &Config::default()).unwrap().level,
            Level::Exact
        );
    }

    #[test]
    fn blank_and_low_detail_frames() {
        let cfg = Config::default();
        let mut a = item(Role::Reference, 1, None, Some(0));
        let mut b = item(Role::Scan, 2, None, Some(0));
        a.luma_stddev = Some(0.0);
        assert!(
            compare(&a, &b, &cfg).is_none(),
            "blank: no perceptual match"
        );
        a.luma_stddev = Some(6.0);
        assert!(compare(&a, &b, &cfg).is_some(), "low detail, identical");
        b.hashes = vec![
            PHashes {
                dhash: 0xFF,
                phash: 0xFF
            };
            4
        ]; // d=8
        assert!(compare(&a, &b, &cfg).is_none(), "low detail, only similar");
        a.luma_stddev = Some(40.0);
        assert!(compare(&a, &b, &cfg).is_some());
    }

    #[test]
    fn different_exposures_are_review_not_duplicate() {
        let mut items = vec![
            item(Role::Reference, 1, None, Some(0)),
            item(Role::Scan, 2, None, Some(0b11)),
        ];
        items[0].capture = Some(1000.0);
        items[1].capture = Some(1002.5);
        let g = find_groups(&items, &Config::default());
        assert_eq!(suggestions(&g[0], &items), vec![Suggestion::Review]);
        items[1].capture = None; // carved preview: no EXIF
        let g = find_groups(&items, &Config::default());
        assert_eq!(suggestions(&g[0], &items), vec![Suggestion::Duplicate]);
    }
}
