//! Multi-index hashing over 64-bit hashes (Norouzi et al.).
//!
//! Each hash is split into four 16-bit chunks, each with its own table of
//! 65,536 buckets. If two hashes are within Hamming distance `r`, then by
//! pigeonhole at least one chunk differs in at most `r / 4` bits. A query
//! therefore probes, per chunk, every bucket within `r / 4` bits of its own
//! chunk (137 buckets at r = 10), then verifies the full distance. Unlike a
//! BK-tree at r = 10, the work per query doesn't grow with the library.

use crate::hash::hamming;

const CHUNKS: usize = 4;
const CHUNK_BITS: u32 = 16;
const BUCKETS: usize = 1 << CHUNK_BITS;
/// Above this per-chunk radius, probing costs more than a linear scan.
const MAX_SUB_RADIUS: u32 = 3;

pub struct MultiIndex {
    hashes: Vec<u64>,
    ids: Vec<usize>,
    /// `tables[c][chunk value]` → entry indices.
    tables: Vec<Vec<Vec<u32>>>,
}

thread_local! {
    /// Per-thread stamp per entry, so an entry reached through several chunks
    /// is checked once per query. Stamps only ever increase, so marks left by
    /// earlier queries (on any index) never collide with the current one.
    static SEEN: std::cell::RefCell<(u32, Vec<u32>)> = const { std::cell::RefCell::new((0, Vec::new())) };
}

impl Default for MultiIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn chunk(h: u64, c: usize) -> usize {
    ((h >> (c as u32 * CHUNK_BITS)) & 0xFFFF) as usize
}

/// Every 16-bit mask with at most `s` bits set, computed once per radius.
fn masks(s: u32) -> &'static [u16] {
    static TABLES: std::sync::OnceLock<Vec<Vec<u16>>> = std::sync::OnceLock::new();
    let t = TABLES.get_or_init(|| {
        (0..=MAX_SUB_RADIUS)
            .map(|s| (0..=u16::MAX).filter(|m| m.count_ones() <= s).collect())
            .collect()
    });
    &t[s.min(MAX_SUB_RADIUS) as usize]
}

impl MultiIndex {
    pub fn new() -> Self {
        Self {
            hashes: Vec::new(),
            ids: Vec::new(),
            tables: (0..CHUNKS).map(|_| vec![Vec::new(); BUCKETS]).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    pub fn insert(&mut self, hash: u64, id: usize) {
        let entry = self.hashes.len() as u32;
        self.hashes.push(hash);
        self.ids.push(id);
        for (c, table) in self.tables.iter_mut().enumerate() {
            table[chunk(hash, c)].push(entry);
        }
    }

    /// Every `(id, distance)` within `radius` of `hash`, inclusive. An id
    /// inserted under several hashes may appear more than once.
    pub fn find(&self, hash: u64, radius: u32) -> Vec<(usize, u32)> {
        let sub = radius / CHUNKS as u32;
        if sub > MAX_SUB_RADIUS {
            return self.scan(hash, radius);
        }
        SEEN.with_borrow_mut(|(stamp, seen)| self.probe(hash, radius, sub, stamp, seen))
    }

    fn probe(
        &self,
        hash: u64,
        radius: u32,
        sub: u32,
        stamp: &mut u32,
        seen: &mut Vec<u32>,
    ) -> Vec<(usize, u32)> {
        if seen.len() < self.hashes.len() {
            seen.resize(self.hashes.len(), 0);
        }
        *stamp = stamp.wrapping_add(1);
        if *stamp == 0 {
            seen.fill(0);
            *stamp = 1;
        }
        let mut out = Vec::new();
        for &m in masks(sub) {
            for (c, table) in self.tables.iter().enumerate() {
                for &e in &table[chunk(hash, c) ^ usize::from(m)] {
                    let slot = &mut seen[e as usize];
                    if *slot == *stamp {
                        continue;
                    }
                    *slot = *stamp;
                    let d = hamming(self.hashes[e as usize], hash);
                    if d <= radius {
                        out.push((self.ids[e as usize], d));
                    }
                }
            }
        }
        out
    }

    fn scan(&self, hash: u64, radius: u32) -> Vec<(usize, u32)> {
        self.hashes
            .iter()
            .zip(&self.ids)
            .filter_map(|(&h, &id)| {
                let d = hamming(h, hash);
                (d <= radius).then_some((id, d))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn brute(hashes: &[u64], q: u64, radius: u32) -> Vec<(usize, u32)> {
        let mut v: Vec<_> = hashes
            .iter()
            .enumerate()
            .map(|(i, &h)| (i, hamming(h, q)))
            .filter(|&(_, d)| d <= radius)
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn mask_counts() {
        assert_eq!(masks(0).len(), 1);
        assert_eq!(masks(2).len(), 1 + 16 + 120);
    }

    proptest! {
        #[test]
        fn matches_brute_force(
            hashes in proptest::collection::vec(any::<u64>(), 0..300),
            flips in proptest::collection::vec(any::<u64>(), 1..20),
            radius in 0u32..24,
        ) {
            let mut idx = MultiIndex::new();
            for (i, &h) in hashes.iter().enumerate() {
                idx.insert(h, i);
            }
            for (k, &f) in flips.iter().enumerate() {
                let base = hashes.get(k % hashes.len().max(1)).copied().unwrap_or(0);
                // Near an existing hash, so radii up to ~20 find something.
                let q = base ^ (f & f.rotate_left(17) & f.rotate_left(31));
                let mut got = idx.find(q, radius);
                got.sort_unstable();
                prop_assert_eq!(got, brute(&hashes, q, radius));
            }
        }
    }

    #[test]
    fn repeated_queries_reuse_scratch() {
        let mut idx = MultiIndex::new();
        idx.insert(0, 0);
        idx.insert(0b111, 1);
        for _ in 0..3 {
            let mut v = idx.find(0, 3);
            v.sort_unstable();
            assert_eq!(v, vec![(0, 0), (1, 3)]);
        }
    }
}
