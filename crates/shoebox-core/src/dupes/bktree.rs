//! BK-tree over 64-bit hashes with Hamming distance.

use crate::hash::hamming;

struct Node {
    hash: u64,
    id: usize,
    /// Children keyed by distance to this node (0..=64).
    children: Vec<(u32, usize)>,
}

#[derive(Default)]
pub struct BkTree {
    nodes: Vec<Node>,
}

impl BkTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn insert(&mut self, hash: u64, id: usize) {
        let new = self.nodes.len();
        self.nodes.push(Node {
            hash,
            id,
            children: Vec::new(),
        });
        if new == 0 {
            return;
        }
        let mut cur = 0;
        loop {
            let d = hamming(self.nodes[cur].hash, hash);
            match self.nodes[cur].children.iter().find(|(cd, _)| *cd == d) {
                Some(&(_, next)) => cur = next,
                None => {
                    self.nodes[cur].children.push((d, new));
                    return;
                }
            }
        }
    }

    /// Every `(id, distance)` within `radius` of `hash`, inclusive.
    pub fn find(&self, hash: u64, radius: u32) -> Vec<(usize, u32)> {
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            return out;
        }
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i];
            let d = hamming(n.hash, hash);
            if d <= radius {
                out.push((n.id, d));
            }
            let (lo, hi) = (d.saturating_sub(radius), d + radius);
            stack.extend(
                n.children
                    .iter()
                    .filter(|(cd, _)| (lo..=hi).contains(cd))
                    .map(|&(_, c)| c),
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn matches_brute_force(
            hashes in proptest::collection::vec(any::<u64>(), 0..200),
            // Queries near existing hashes, so non-trivial radii find something.
            flips in proptest::collection::vec(any::<u64>(), 1..20),
            radius in 0u32..20,
        ) {
            let mut t = BkTree::new();
            for (i, &h) in hashes.iter().enumerate() {
                t.insert(h, i);
            }
            for (k, &f) in flips.iter().enumerate() {
                let base = hashes.get(k % hashes.len().max(1)).copied().unwrap_or(0);
                // Flip a few low-entropy bits.
                let q = base ^ (f & f.rotate_left(17) & f.rotate_left(31));
                let mut got = t.find(q, radius);
                got.sort_unstable();
                let mut want: Vec<_> = hashes
                    .iter()
                    .enumerate()
                    .map(|(i, &h)| (i, hamming(h, q)))
                    .filter(|&(_, d)| d <= radius)
                    .collect();
                want.sort_unstable();
                prop_assert_eq!(got, want);
            }
        }
    }
}
