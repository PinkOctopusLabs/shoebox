//! Disjoint-set union with path halving and union by size.

pub struct UnionFind {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl UnionFind {
    pub fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            size: vec![1; n],
        }
    }

    pub fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    pub fn union(&mut self, a: usize, b: usize) {
        let (mut a, mut b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        if self.size[a] < self.size[b] {
            std::mem::swap(&mut a, &mut b);
        }
        self.parent[b] = a;
        self.size[a] += self.size[b];
    }

    /// Sets with two or more members, each sorted, ordered by first member.
    pub fn groups(&mut self) -> Vec<Vec<usize>> {
        let n = self.parent.len();
        let mut by_root: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
        for i in 0..n {
            let r = self.find(i);
            by_root.entry(r).or_default().push(i);
        }
        let mut out: Vec<_> = by_root.into_values().filter(|g| g.len() > 1).collect();
        out.sort_by_key(|g| g[0]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_transitively() {
        let mut uf = UnionFind::new(6);
        uf.union(0, 1);
        uf.union(4, 5);
        uf.union(1, 3);
        assert_eq!(uf.groups(), vec![vec![0, 1, 3], vec![4, 5]]);
    }
}
