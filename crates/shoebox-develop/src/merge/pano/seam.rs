//! Seams: which frame each part of the result comes from, decided on a
//! small canvas (about a megapixel) from the frames' previews, then blended
//! across at full size ([`super::blend`]).
//!
//! 1. **Centre-weighted** (a Voronoi diagram of the frames' centres, as
//!    Enblend starts from): each pixel to the frame it's furthest inside,
//!    by the product of its distances from the frame's edges.
//! 2. **Graph cut** (Kwatra et al., "Graphcut Textures", 2003; OpenCV's
//!    `GraphCutSeamFinder`): for each pair of neighbouring frames, the
//!    pixels both cover well are relabelled by a minimum cut whose cost is
//!    where the two differ (exposure-compensated, square-root encoded),
//!    plus a little for length and for leaving step 1's choice. A seam then
//!    runs where the frames agree: round a person who moved, not through
//!    them. Max-flow by Dinic's algorithm.
//! 3. **The largest rectangle** inside what's covered, for the crop.

use std::sync::atomic::AtomicBool;

use rayon::prelude::*;

use super::optics::Preview;
use super::project::Layout;
use crate::merge::{Result, check};

/// No frame.
pub const NONE: u16 = u16::MAX;

/// One frame on the canvas: its box, exposure-compensated luminance (NaN
/// where it has nothing), how far inside it each pixel is (0 at its edge,
/// 1 at its centre), and its well-covered pixels (one pixel in from its
/// edge).
struct OnCanvas {
    roi: [usize; 4],
    val: Vec<f32>,
    inside: Vec<f32>,
    core: Vec<bool>,
}

impl OnCanvas {
    #[inline]
    fn at(&self, x: usize, y: usize) -> Option<usize> {
        let [x0, y0, x1, y1] = self.roi;
        (x >= x0 && x < x1 && y >= y0 && y < y1).then(|| (y - y0) * (x1 - x0) + x - x0)
    }

    fn value(&self, x: usize, y: usize) -> Option<f32> {
        self.at(x, y).map(|i| self.val[i]).filter(|v| v.is_finite())
    }

    fn is_core(&self, x: usize, y: usize) -> bool {
        self.at(x, y).is_some_and(|i| self.core[i])
    }
}

fn on_canvas(
    c: &Layout,
    i: usize,
    size: (usize, usize),
    p: &Preview,
    gain: f32,
) -> Option<OnCanvas> {
    let roi = c.reach(i, size)?;
    let [x0, y0, x1, y1] = roi;
    let w = x1 - x0;
    let (fw, fh) = (size.0 as f64 - 1.0, size.1 as f64 - 1.0);
    let mut val = vec![f32::NAN; w * (y1 - y0)];
    let mut inside = vec![0f32; w * (y1 - y0)];
    val.par_chunks_mut(w)
        .zip(inside.par_chunks_mut(w))
        .enumerate()
        .for_each(|(r, (vrow, irow))| {
            let y = (y0 + r) as f64;
            for k in 0..w {
                let Some([fx, fy]) = c.to_frame(i, (x0 + k) as f64, y) else {
                    continue;
                };
                if !(fx >= 0.0 && fy >= 0.0 && fx <= fw && fy <= fh) {
                    continue;
                }
                let (px, py) = p.from_full(fx, fy);
                if let Some(v) = p.gray.sample(px, py) {
                    vrow[k] = v * gain;
                    irow[k] = (fx.min(fw - fx) / (fw / 2.0) * fy.min(fh - fy) / (fh / 2.0)) as f32;
                }
            }
        });
    let h = y1 - y0;
    let covered = |x: isize, y: isize| -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && val[y as usize * w + x as usize].is_finite()
    };
    let mut core = vec![false; w * h];
    for y in 0..h as isize {
        for x in 0..w as isize {
            core[y as usize * w + x as usize] =
                (-1..=1).all(|dy| (-1..=1).all(|dx| covered(x + dx, y + dy)));
        }
    }
    Some(OnCanvas {
        roi,
        val,
        inside,
        core,
    })
}

/// Seams on `canvas` (the result's layout, shrunk): each pixel's frame, or
/// [`NONE`].
pub fn find(
    canvas: &Layout,
    sizes: &[(usize, usize)],
    previews: &[&Preview],
    gains: &[f64],
    cancel: &AtomicBool,
) -> Result<Vec<u16>> {
    let (w, h) = (canvas.width, canvas.height);
    let frames: Vec<Option<OnCanvas>> = (0..sizes.len())
        .map(|i| on_canvas(canvas, i, sizes[i], previews[i], gains[i] as f32))
        .collect();
    check(cancel)?;
    // 1: centre-weighted, from the well-covered first.
    let mut labels = vec![NONE; w * h];
    labels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, l) in row.iter_mut().enumerate() {
            let mut best = (false, -1f32, NONE);
            for (i, f) in frames.iter().enumerate() {
                let Some(f) = f else { continue };
                let Some(k) = f.at(x, y) else { continue };
                if !f.val[k].is_finite() {
                    continue;
                }
                let cand = (f.core[k], f.inside[k]);
                if (cand.0, cand.1) > (best.0, best.1) {
                    best = (cand.0, cand.1, i as u16);
                }
            }
            *l = best.2;
        }
    });
    // A typical level, to make differences relative.
    let mut levels: Vec<f32> = frames
        .iter()
        .flatten()
        .flat_map(|f| {
            f.val
                .iter()
                .step_by(7)
                .copied()
                .filter(|v| v.is_finite() && *v > 0.0)
        })
        .collect();
    let norm = if levels.is_empty() {
        1.0
    } else {
        let mid = levels.len() / 2;
        levels.select_nth_unstable_by(mid, f32::total_cmp);
        levels[mid].sqrt().max(1e-4)
    };
    // 2: graph cuts between neighbours, the largest overlaps first.
    let mut pairs = Vec::new();
    for i in 0..frames.len() {
        for j in i + 1..frames.len() {
            let (Some(a), Some(b)) = (&frames[i], &frames[j]) else {
                continue;
            };
            let x0 = a.roi[0].max(b.roi[0]);
            let x1 = a.roi[2].min(b.roi[2]);
            let y0 = a.roi[1].max(b.roi[1]);
            let y1 = a.roi[3].min(b.roi[3]);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let n = (y0..y1)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|&(x, y)| a.is_core(x, y) && b.is_core(x, y))
                .count();
            if n > 0 {
                pairs.push((n, i, j, [x0, y0, x1, y1]));
            }
        }
    }
    pairs.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, i, j, bx) in pairs {
        check(cancel)?;
        if let (Some(a), Some(b)) = (&frames[i], &frames[j]) {
            cut(&mut labels, w, (i as u16, a), (j as u16, b), bx, norm);
        }
    }
    Ok(labels)
}

/// Relabel the pixels frames `a` and `b` both cover well (and are labelled
/// one of them) by a minimum cut.
fn cut(
    labels: &mut [u16],
    w: usize,
    (la, a): (u16, &OnCanvas),
    (lb, b): (u16, &OnCanvas),
    bx: [usize; 4],
    norm: f32,
) {
    let [x0, y0, x1, y1] = bx;
    let bw = x1 - x0;
    let mut node = vec![u32::MAX; bw * (y1 - y0)];
    let mut nodes: Vec<(usize, usize)> = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let l = labels[y * w + x];
            if (l == la || l == lb) && a.is_core(x, y) && b.is_core(x, y) {
                node[(y - y0) * bw + x - x0] = nodes.len() as u32;
                nodes.push((x, y));
            }
        }
    }
    if nodes.len() < 2 {
        return;
    }
    // Costs, as whole numbers.
    const SCALE: f64 = 1e4;
    const LENGTH: f64 = 0.03;
    const KEEP: f64 = 0.002;
    let diff = |x: usize, y: usize| -> Option<f64> {
        let (p, q) = (a.value(x, y)?, b.value(x, y)?);
        Some(f64::from(
            (p.max(0.0).sqrt() - q.max(0.0).sqrt()).abs() / norm,
        ))
    };
    let n = nodes.len();
    let (s, t) = (n, n + 1);
    let mut g = Graph::new(n + 2);
    for (k, &(x, y)) in nodes.iter().enumerate() {
        let dp = diff(x, y).unwrap_or(0.0);
        let (mut to_s, mut to_t) = (0.0, 0.0);
        if labels[y * w + x] == la {
            to_s += KEEP;
        } else {
            to_t += KEEP;
        }
        for (dx, dy) in [(1isize, 0isize), (0, 1), (-1, 0), (0, -1)] {
            let (qx, qy) = (x as isize + dx, y as isize + dy);
            if qx < 0 || qy < 0 || qx as usize >= w || qy as usize >= labels.len() / w {
                continue;
            }
            let (qx, qy) = (qx as usize, qy as usize);
            let inside = qx >= x0 && qx < x1 && qy >= y0 && qy < y1;
            let q = if inside {
                node[(qy - y0) * bw + qx - x0]
            } else {
                u32::MAX
            };
            if q != u32::MAX {
                // Each neighbour pair once.
                if dx > 0 || dy > 0 {
                    let c = LENGTH + (dp + diff(qx, qy).unwrap_or(dp)) / 2.0;
                    let c = (c * SCALE).round().max(1.0) as i64;
                    g.edge(k, q as usize, c, c);
                }
            } else {
                // A fixed neighbour: the seam may pass between.
                let l = labels[qy * w + qx];
                let c = LENGTH + dp;
                if l == la {
                    to_s += c;
                } else if l == lb {
                    to_t += c;
                }
            }
        }
        if to_s > 0.0 {
            g.edge(s, k, (to_s * SCALE).round().max(1.0) as i64, 0);
        }
        if to_t > 0.0 {
            g.edge(k, t, (to_t * SCALE).round().max(1.0) as i64, 0);
        }
    }
    g.max_flow(s, t);
    let side = g.source_side(s);
    for (k, &(x, y)) in nodes.iter().enumerate() {
        labels[y * w + x] = if side[k] { la } else { lb };
    }
}

/// A flow network (Dinic's algorithm, iterative).
struct Graph {
    head: Vec<u32>,
    next: Vec<u32>,
    to: Vec<u32>,
    cap: Vec<i64>,
}

impl Graph {
    fn new(n: usize) -> Self {
        Self {
            head: vec![u32::MAX; n],
            next: Vec::new(),
            to: Vec::new(),
            cap: Vec::new(),
        }
    }

    /// An arc `u → v` of capacity `c`, and its reverse of `back`.
    fn edge(&mut self, u: usize, v: usize, c: i64, back: i64) {
        for (from, to, cap) in [(u, v, c), (v, u, back)] {
            self.next.push(self.head[from]);
            self.head[from] = self.to.len() as u32;
            self.to.push(to as u32);
            self.cap.push(cap);
        }
    }

    fn levels(&self, s: usize, t: usize) -> Option<Vec<i32>> {
        let mut level = vec![-1i32; self.head.len()];
        level[s] = 0;
        let mut queue = std::collections::VecDeque::from([s]);
        while let Some(u) = queue.pop_front() {
            let mut e = self.head[u];
            while e != u32::MAX {
                let v = self.to[e as usize] as usize;
                if self.cap[e as usize] > 0 && level[v] < 0 {
                    level[v] = level[u] + 1;
                    queue.push_back(v);
                }
                e = self.next[e as usize];
            }
        }
        (level[t] >= 0).then_some(level)
    }

    fn max_flow(&mut self, s: usize, t: usize) -> i64 {
        let mut total = 0;
        while let Some(mut level) = self.levels(s, t) {
            let mut it = self.head.clone();
            let mut path: Vec<u32> = Vec::new();
            let mut u = s;
            loop {
                if u == t {
                    let f = path
                        .iter()
                        .map(|&e| self.cap[e as usize])
                        .min()
                        .unwrap_or(0);
                    for &e in &path {
                        self.cap[e as usize] -= f;
                        self.cap[(e ^ 1) as usize] += f;
                    }
                    total += f;
                    path.clear();
                    u = s;
                    continue;
                }
                // Advance along the level graph.
                let mut found = false;
                while it[u] != u32::MAX {
                    let e = it[u] as usize;
                    let v = self.to[e] as usize;
                    if self.cap[e] > 0 && level[v] == level[u] + 1 {
                        path.push(e as u32);
                        u = v;
                        found = true;
                        break;
                    }
                    it[u] = self.next[e];
                }
                if found {
                    continue;
                }
                // A dead end: retreat.
                if u == s {
                    break;
                }
                level[u] = -1;
                let Some(e) = path.pop() else { break };
                u = self.to[(e ^ 1) as usize] as usize;
                it[u] = self.next[e as usize];
            }
        }
        total
    }

    /// Nodes still reachable from `s` (the cut's source side).
    fn source_side(&self, s: usize) -> Vec<bool> {
        let mut seen = vec![false; self.head.len()];
        seen[s] = true;
        let mut stack = vec![s];
        while let Some(u) = stack.pop() {
            let mut e = self.head[u];
            while e != u32::MAX {
                let v = self.to[e as usize] as usize;
                if self.cap[e as usize] > 0 && !seen[v] {
                    seen[v] = true;
                    stack.push(v);
                }
                e = self.next[e as usize];
            }
        }
        seen
    }
}

/// The largest axis-aligned rectangle of `ok` pixels (`[x0, y0, x1, y1)`),
/// by the histogram method.
pub fn largest_rectangle(ok: &[bool], w: usize, h: usize) -> Option<[usize; 4]> {
    let mut heights = vec![0usize; w];
    let mut best = (0usize, [0usize; 4]);
    let mut stack: Vec<usize> = Vec::with_capacity(w + 1);
    for y in 0..h {
        for x in 0..w {
            heights[x] = if ok[y * w + x] { heights[x] + 1 } else { 0 };
        }
        stack.clear();
        for x in 0..=w {
            let hx = if x < w { heights[x] } else { 0 };
            while let Some(&top) = stack.last() {
                if heights[top] < hx {
                    break;
                }
                stack.pop();
                let height = heights[top];
                let left = stack.last().map_or(0, |&l| l + 1);
                let area = height * (x - left);
                if area > best.0 {
                    best = (area, [left, y + 1 - height, x, y + 1]);
                }
            }
            stack.push(x);
        }
    }
    (best.0 > 0).then_some(best.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_goes_where_it_costs_least() {
        // Source and sink joined by a chain: the cheap link is cut.
        let mut g = Graph::new(5);
        g.edge(3, 0, 100, 0);
        g.edge(0, 1, 7, 7);
        g.edge(1, 2, 2, 2);
        g.edge(2, 4, 100, 0);
        assert_eq!(g.max_flow(3, 4), 2);
        let side = g.source_side(3);
        assert!(side[0] && side[1] && !side[2]);
    }

    #[test]
    fn the_largest_rectangle() {
        let (w, h) = (6, 4);
        let rows = ["..##..", ".####.", "######", ".####."];
        let ok: Vec<bool> = rows
            .iter()
            .flat_map(|r| r.chars().map(|c| c == '#'))
            .collect();
        assert_eq!(largest_rectangle(&ok, w, h), Some([1, 1, 5, 4]));
    }
}
