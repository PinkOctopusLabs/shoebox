//! Duplicate detection: exact, same-image-data and perceptual matches,
//! merged into groups.

pub mod bktree;
pub mod report;
pub mod unionfind;

use std::collections::HashMap;

use shoebox_decode::Format;

use crate::hash::{Digest, PHashes};
use crate::scan::Role;
use bktree::BkTree;
use unionfind::UnionFind;

/// Defaults from the spec; starting points to tune against real files.
pub const DEFAULT_NEAR_IDENTICAL: u32 = 4;
pub const DEFAULT_SIMILAR: u32 = 10;

#[derive(Debug, Clone)]
pub struct Item {
    pub role: Role,
    pub format: Format,
    pub blake3: Digest,
    pub image_data_hash: Option<Digest>,
    /// Upright image at rotations 0/90/180/270; empty if undecodable.
    pub hashes: Vec<PHashes>,
    pub pixels: u64,
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
    let upright = a.hashes.first()?;
    let (rot, distance) = b
        .hashes
        .iter()
        .enumerate()
        .map(|(r, h)| (r, upright.distance(h)))
        .min_by_key(|&(r, d)| (d, r))?;
    (distance <= cfg.threshold).then(|| Match {
        level: if distance <= cfg.near_identical {
            Level::NearIdentical
        } else {
            Level::Similar
        },
        distance,
        rotation: (rot * 90) as u16,
    })
}

#[derive(Debug, Clone)]
pub struct Member {
    pub item: usize,
    /// Direct match to the group's keeper (None if only chained through others).
    pub to_keeper: Option<Match>,
    /// For scan files: the closest direct match to a reference file in the group.
    pub matched_ref: Option<(usize, Match)>,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub keeper: usize,
    pub has_reference: bool,
    /// Every member except the keeper.
    pub members: Vec<Member>,
}

impl Member {
    /// A scan file is proposed for moving only when it directly matches a
    /// reference file. Files that only chain into a group are left alone.
    pub fn movable(&self, items: &[Item]) -> bool {
        items[self.item].role == Role::Scan && self.matched_ref.is_some()
    }
}

fn format_rank(f: Format) -> u8 {
    match f {
        Format::Nef => 0,
        Format::Tiff => 1,
        Format::Jpeg => 2,
        Format::Png => 3,
    }
}

/// Keeper preference: reference copy, then NEF > TIFF > JPG > PNG, then the
/// highest resolution, then scan order.
fn keeper_key(items: &[Item], i: usize) -> (bool, u8, std::cmp::Reverse<u64>, usize) {
    let it = &items[i];
    (
        it.role != Role::Reference,
        format_rank(it.format),
        std::cmp::Reverse(it.pixels),
        i,
    )
}

fn link_equal(uf: &mut UnionFind, keys: impl Iterator<Item = (usize, Digest)>) {
    let mut first: HashMap<Digest, usize> = HashMap::new();
    for (i, k) in keys {
        match first.get(&k) {
            Some(&j) => uf.union(i, j),
            None => {
                first.insert(k, i);
            }
        }
    }
}

pub fn find_groups(items: &[Item], cfg: &Config) -> Vec<Group> {
    let mut uf = UnionFind::new(items.len());
    link_equal(
        &mut uf,
        items.iter().enumerate().map(|(i, it)| (i, it.blake3)),
    );
    link_equal(
        &mut uf,
        items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| it.image_data_hash.map(|h| (i, h))),
    );

    let mut tree = BkTree::new();
    for (i, it) in items.iter().enumerate() {
        if let Some(h) = it.hashes.first() {
            tree.insert(h.phash, i);
        }
    }
    for (i, it) in items.iter().enumerate() {
        for h in &it.hashes {
            for (j, _) in tree.find(h.phash, cfg.threshold) {
                if j != i && compare(&items[j], it, cfg).is_some() {
                    uf.union(i, j);
                }
            }
        }
    }

    uf.groups()
        .into_iter()
        .map(|ids| {
            let keeper = ids
                .iter()
                .copied()
                .min_by_key(|&i| keeper_key(items, i))
                .unwrap_or(ids[0]);
            let refs: Vec<usize> = ids
                .iter()
                .copied()
                .filter(|&i| items[i].role == Role::Reference)
                .collect();
            let members = ids
                .iter()
                .copied()
                .filter(|&i| i != keeper)
                .map(|i| {
                    let matched_ref = if items[i].role == Role::Scan {
                        refs.iter()
                            .filter_map(|&r| compare(&items[r], &items[i], cfg).map(|m| (r, m)))
                            .min_by_key(|(r, m)| (m.level, m.distance, keeper_key(items, *r)))
                    } else {
                        None
                    };
                    Member {
                        item: i,
                        to_keeper: compare(&items[keeper], &items[i], cfg),
                        matched_ref,
                    }
                })
                .collect();
            Group {
                keeper,
                has_reference: !refs.is_empty(),
                members,
            }
        })
        .collect()
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
        }
    }

    #[test]
    fn groups_by_each_level() {
        let items = vec![
            item(Role::Reference, 1, Some(1), Some(0)),     // 0
            item(Role::Scan, 1, Some(9), None),             // 1 exact of 0
            item(Role::Scan, 2, Some(1), None),             // 2 image data of 0
            item(Role::Scan, 3, None, Some(0b111)),         // 3 similar to 0 (d=3)
            item(Role::Scan, 4, None, Some(u64::MAX >> 1)), // 4 unrelated to 0
            item(Role::Scan, 5, None, Some((u64::MAX >> 1) ^ 0xFF)), // 5 similar to 4 (d=8)
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
        assert!(g[0].members.iter().all(|m| m.movable(&items)));
        // Scan-only group: nothing movable.
        assert!(!g[1].has_reference);
        assert!(g[1].members.iter().all(|m| !m.movable(&items)));
    }

    #[test]
    fn reference_files_are_never_movable() {
        let items = vec![
            item(Role::Reference, 1, None, None),
            item(Role::Reference, 1, None, None),
        ];
        let g = find_groups(&items, &Config::default());
        assert!(g[0].members.iter().all(|m| !m.movable(&items)));
    }

    #[test]
    fn chained_members_are_not_moved() {
        // 0 (ref) ~ 1 (d=8) ~ 2 (d=8 from 1, 16 from 0)
        let items = vec![
            item(Role::Reference, 1, None, Some(0)),
            item(Role::Scan, 2, None, Some(0xFF)),
            item(Role::Scan, 3, None, Some(0xFFFF)),
        ];
        let g = find_groups(&items, &Config::default());
        assert_eq!(g.len(), 1);
        let m2 = g[0].members.iter().find(|m| m.item == 2).unwrap();
        assert!(m2.to_keeper.is_none());
        assert!(!m2.movable(&items));
    }
}
