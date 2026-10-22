//! Registration: the frames' cameras from their pairwise matches (Brown and
//! Lowe, "Automatic Panoramic Image Stitching using Invariant Features",
//! 2007, §4).
//!
//! - **A camera turning about one point** ([`Rotations`]): each frame a
//!   rotation, all one focal length (one lens, one zoom setting), the
//!   principal point the frame's centre (lens corrections already put it
//!   there). Started from a maximum spanning tree of the matches (each
//!   pair's homography gives its relative rotation), then **bundle
//!   adjusted**: Levenberg–Marquardt on every match's transfer error both
//!   ways, Huber-weighted (scale from the residuals' MAD), one frame held
//!   still.
//! - **A plane** ([`Planes`]): each frame a homography onto the first
//!   frame's plane, adjusted the same way: scans, murals, anything flat
//!   photographed from more than one place.
//! - **Straightening** ([`straighten`]): the horizon from the frames' axes
//!   ("wave correction", Brown and Lowe §5.1): the frames' x axes lie in
//!   the horizontal plane when a row is shot level, so the world's up is
//!   the direction least like any of them.

use std::sync::atomic::AtomicBool;

use rayon::prelude::*;

use super::geom::{self, V3};
use crate::colour::{self, M3};
use crate::merge::align::{self, Pair};
use crate::merge::{Result, check};

/// Two frames that overlap, and what they matched.
#[derive(Debug, Clone)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    /// Each match: frame `a`'s corrected pixel and frame `b`'s (full
    /// size), the agreeing ones only.
    pub pairs: Vec<Pair>,
    /// How many agreed (before thinning).
    pub inliers: usize,
    /// `a` → `b`, full-size pixels.
    pub h: M3,
}

impl Link {
    /// The homography from `from` to the other frame.
    pub fn from(&self, from: usize) -> Option<M3> {
        if from == self.a {
            Some(self.h)
        } else {
            colour::inv(&self.h)
        }
    }

    pub fn other(&self, i: usize) -> usize {
        if i == self.a { self.b } else { self.a }
    }
}

/// A camera turning about one point.
#[derive(Debug, Clone)]
pub struct Rotations {
    /// World → camera, per frame.
    pub r: Vec<M3>,
    /// Focal length, full-size pixels.
    pub f: f64,
    /// Each frame's centre (principal point), full-size pixels.
    pub centres: Vec<[f64; 2]>,
}

/// Where frame `a`'s pixel `p` is seen in frame `b` (None: behind it).
#[inline]
fn transfer(ra: &M3, rb: &M3, f: f64, ca: [f64; 2], cb: [f64; 2], p: [f64; 2]) -> Option<[f64; 2]> {
    let c = [(p[0] - ca[0]) / f, (p[1] - ca[1]) / f, 1.0];
    let q = geom::mv(rb, geom::mtv(ra, c));
    (q[2] > 1e-6).then(|| [f * q[0] / q[2] + cb[0], f * q[1] / q[2] + cb[1]])
}

impl Rotations {
    /// The direction (world) frame `i`'s pixel looks along.
    pub fn ray(&self, i: usize, x: f64, y: f64) -> V3 {
        let c = self.centres[i];
        geom::mtv(&self.r[i], [(x - c[0]) / self.f, (y - c[1]) / self.f, 1.0])
    }

    /// Where direction `d` falls in frame `i` (None: behind it).
    #[inline]
    pub fn pixel(&self, i: usize, d: V3) -> Option<[f64; 2]> {
        let q = geom::mv(&self.r[i], d);
        let c = self.centres[i];
        (q[2] > 1e-6).then(|| [self.f * q[0] / q[2] + c[0], self.f * q[1] / q[2] + c[1]])
    }
}

/// Each frame a homography onto frame `fixed`'s plane.
#[derive(Debug, Clone)]
pub struct Planes {
    /// Frame → plane, full-size pixels.
    pub g: Vec<M3>,
    /// Each frame's normalising (centred, its long edge −1 to 1).
    norm: Vec<M3>,
}

impl Planes {
    pub fn new(g: Vec<M3>, sizes: &[(usize, usize)]) -> Self {
        let norm = sizes
            .iter()
            .map(|&(w, h)| {
                let s = 2.0 / w.max(h).max(2) as f64;
                let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
                [[s, 0.0, -s * cx], [0.0, s, -s * cy], [0.0, 0.0, 1.0]]
            })
            .collect();
        Self { g, norm }
    }

    /// `g[i]` moved by `d` (eight parameters of `I + d`, in frame `i`'s
    /// normalised coordinates).
    fn moved(&self, i: usize, d: &[f64]) -> M3 {
        if d.iter().all(|v| *v == 0.0) {
            return self.g[i];
        }
        let n = self.norm[i];
        let ni = colour::inv(&n).unwrap_or(colour::IDENTITY);
        let m = [
            [1.0 + d[0], d[1], d[2]],
            [d[3], 1.0 + d[4], d[5]],
            [d[6], d[7], 1.0],
        ];
        geom::mul(&self.g[i], &geom::mul(&ni, &geom::mul(&m, &n)))
    }
}

/// What the cameras are fitted to, and how they move.
pub trait Problem: Clone + Sync + Send {
    /// Parameters per frame, and shared by all.
    fn per_frame(&self) -> usize;
    fn shared(&self) -> usize;
    /// A match's residuals with these increments to its frames (and the
    /// shared parameters); None if it can't be evaluated there.
    fn residual(&self, l: &Link, p: &Pair, da: &[f64], db: &[f64], ds: &[f64]) -> Option<[f64; 4]>;
    /// Take the increments: one slice a frame (empty for the fixed one),
    /// then the shared ones.
    fn apply(&mut self, frames: &[&[f64]], shared: &[f64]);
}

impl Problem for Rotations {
    fn per_frame(&self) -> usize {
        3
    }

    fn shared(&self) -> usize {
        1
    }

    #[inline]
    fn residual(&self, l: &Link, p: &Pair, da: &[f64], db: &[f64], ds: &[f64]) -> Option<[f64; 4]> {
        let turn = |r: &M3, d: &[f64]| {
            if d.is_empty() || d.iter().all(|v| *v == 0.0) {
                *r
            } else {
                geom::mul(&geom::exp([d[0], d[1], d[2]]), r)
            }
        };
        let ra = turn(&self.r[l.a], da);
        let rb = turn(&self.r[l.b], db);
        let f = self.f * ds.first().map_or(1.0, |v| v.exp());
        let (ca, cb) = (self.centres[l.a], self.centres[l.b]);
        let q = transfer(&ra, &rb, f, ca, cb, p.0)?;
        let r = transfer(&rb, &ra, f, cb, ca, p.1)?;
        Some([q[0] - p.1[0], q[1] - p.1[1], r[0] - p.0[0], r[1] - p.0[1]])
    }

    fn apply(&mut self, frames: &[&[f64]], shared: &[f64]) {
        for (r, d) in self.r.iter_mut().zip(frames) {
            if d.len() == 3 {
                *r = geom::mul(&geom::exp([d[0], d[1], d[2]]), r);
            }
        }
        if let Some(s) = shared.first() {
            self.f *= s.exp();
        }
    }
}

impl Problem for Planes {
    fn per_frame(&self) -> usize {
        8
    }

    fn shared(&self) -> usize {
        0
    }

    #[inline]
    fn residual(
        &self,
        l: &Link,
        p: &Pair,
        da: &[f64],
        db: &[f64],
        _ds: &[f64],
    ) -> Option<[f64; 4]> {
        let ga = if da.is_empty() {
            self.g[l.a]
        } else {
            self.moved(l.a, da)
        };
        let gb = if db.is_empty() {
            self.g[l.b]
        } else {
            self.moved(l.b, db)
        };
        let q = align::apply(&ga, p.0[0], p.0[1]);
        let r = align::apply(&gb, p.1[0], p.1[1]);
        let v = [q.0 - r.0, q.1 - r.1, 0.0, 0.0];
        v.iter().all(|x| x.is_finite()).then_some(v)
    }

    fn apply(&mut self, frames: &[&[f64]], _shared: &[f64]) {
        for i in 0..self.g.len() {
            if frames.get(i).is_some_and(|d| d.len() == 8) {
                self.g[i] = self.moved(i, frames[i]);
            }
        }
    }
}

/// How well the cameras fit the matches.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fit {
    /// Root mean square and median transfer error (full-size pixels).
    pub rms: f64,
    pub median: f64,
    /// Matches used.
    pub pairs: usize,
}

#[inline]
fn huber(e: f64, delta: f64) -> f64 {
    if e <= delta {
        e * e
    } else {
        2.0 * delta * e - delta * delta
    }
}

/// Every match's transfer errors (each direction's length).
fn errors<P: Problem>(p: &P, links: &[Link]) -> Vec<f64> {
    links
        .par_iter()
        .flat_map_iter(|l| {
            l.pairs.iter().flat_map(move |m| {
                let r = p.residual(l, m, &[], &[], &[]);
                match r {
                    Some(r) => vec![r[0].hypot(r[1]), r[2].hypot(r[3])],
                    None => vec![f64::INFINITY],
                }
            })
        })
        .collect()
}

fn cost<P: Problem>(p: &P, links: &[Link], delta: f64) -> f64 {
    errors(p, links).iter().map(|&e| huber(e, delta)).sum()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return f64::INFINITY;
    }
    let mid = v.len() / 2;
    v.select_nth_unstable_by(mid, f64::total_cmp);
    v[mid]
}

/// How well `p` fits.
pub fn fit<P: Problem>(p: &P, links: &[Link]) -> Fit {
    let e = errors(p, links);
    let finite: Vec<f64> = e.iter().copied().filter(|v| v.is_finite()).collect();
    let rms = if finite.is_empty() {
        f64::INFINITY
    } else {
        (finite.iter().map(|v| v * v).sum::<f64>() / finite.len() as f64).sqrt()
    };
    Fit {
        rms,
        median: median(finite),
        pairs: e.len() / 2,
    }
}

/// Bundle adjustment: Levenberg–Marquardt over every frame but `fixed`
/// (and the shared parameters), Huber-weighted, numerical derivatives per
/// match (each touches two frames).
pub fn adjust<P: Problem>(
    p: &mut P,
    n: usize,
    fixed: usize,
    links: &[Link],
    cancel: &AtomicBool,
) -> Result<Fit> {
    let k = p.per_frame();
    let s = p.shared();
    let np = k * (n - 1) + s;
    let slot = |i: usize| -> Option<usize> {
        match i.cmp(&fixed) {
            std::cmp::Ordering::Less => Some(i * k),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some((i - 1) * k),
        }
    };
    let shared_at = k * (n - 1);
    const EPS: f64 = 1e-6;
    let mut lambda = 1e-3;
    for _ in 0..100 {
        check(cancel)?;
        // The robust scale from what's left now.
        let delta = (1.345 * 1.4826 * median(errors(p, links))).max(0.3);
        let cost0 = cost(p, links, delta);
        let zero = || (vec![0.0f64; np * np], vec![0.0f64; np]);
        let pr: &P = p;
        let (ata, atb) = links
            .par_iter()
            .fold(zero, |(mut ata, mut atb), l| {
                let (sa, sb) = (slot(l.a), slot(l.b));
                // Columns: a's, b's, shared.
                let mut cols: Vec<usize> = Vec::with_capacity(2 * k + s);
                if let Some(o) = sa {
                    cols.extend(o..o + k);
                }
                if let Some(o) = sb {
                    cols.extend(o..o + k);
                }
                cols.extend(shared_at..shared_at + s);
                let mut j = vec![[0.0f64; 4]; cols.len()];
                for m in &l.pairs {
                    let Some(r0) = pr.residual(l, m, &[], &[], &[]) else {
                        continue;
                    };
                    let mut ok = true;
                    let mut da = vec![0.0; if sa.is_some() { k } else { 0 }];
                    let mut db = vec![0.0; if sb.is_some() { k } else { 0 }];
                    let mut ds = vec![0.0; s];
                    for (c, jc) in j.iter_mut().enumerate() {
                        let set =
                            |da: &mut Vec<f64>, db: &mut Vec<f64>, ds: &mut Vec<f64>, v: f64| {
                                let mut c = c;
                                if c < da.len() {
                                    da[c] = v;
                                    return;
                                }
                                c -= da.len();
                                if c < db.len() {
                                    db[c] = v;
                                    return;
                                }
                                c -= db.len();
                                ds[c] = v;
                            };
                        set(&mut da, &mut db, &mut ds, EPS);
                        let plus = pr.residual(l, m, &da, &db, &ds);
                        set(&mut da, &mut db, &mut ds, -EPS);
                        let minus = pr.residual(l, m, &da, &db, &ds);
                        set(&mut da, &mut db, &mut ds, 0.0);
                        match (plus, minus) {
                            (Some(a), Some(b)) => {
                                for q in 0..4 {
                                    jc[q] = (a[q] - b[q]) / (2.0 * EPS);
                                }
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        continue;
                    }
                    // Huber weights, one per direction.
                    let w = |e: f64| if e <= delta { 1.0 } else { delta / e };
                    let wr = [
                        w(r0[0].hypot(r0[1])),
                        w(r0[0].hypot(r0[1])),
                        w(r0[2].hypot(r0[3])),
                        w(r0[2].hypot(r0[3])),
                    ];
                    for (x, &cx) in cols.iter().enumerate() {
                        for q in 0..4 {
                            atb[cx] += wr[q] * j[x][q] * r0[q];
                        }
                        for (y, &cy) in cols.iter().enumerate().skip(x) {
                            let mut v = 0.0;
                            for q in 0..4 {
                                v += wr[q] * j[x][q] * j[y][q];
                            }
                            ata[cx * np + cy] += v;
                            if cy != cx {
                                ata[cy * np + cx] += v;
                            }
                        }
                    }
                }
                (ata, atb)
            })
            .reduce(zero, |(mut a1, mut b1), (a2, b2)| {
                a1.iter_mut().zip(&a2).for_each(|(x, y)| *x += y);
                b1.iter_mut().zip(&b2).for_each(|(x, y)| *x += y);
                (a1, b1)
            });
        let mut improved = None;
        while lambda < 1e10 {
            let mut a = vec![vec![0.0; np]; np];
            for i in 0..np {
                for jj in 0..np {
                    a[i][jj] = ata[i * np + jj];
                }
                a[i][i] = ata[i * np + i] * (1.0 + lambda) + 1e-12;
            }
            let Some(step) = align::solve(a, atb.iter().map(|v| -v).collect()) else {
                lambda *= 4.0;
                continue;
            };
            let mut trial = p.clone();
            let frames: Vec<&[f64]> = (0..n)
                .map(|i| match slot(i) {
                    Some(o) => &step[o..o + k],
                    None => &[][..],
                })
                .collect();
            trial.apply(&frames, &step[shared_at..]);
            let c1 = cost(&trial, links, delta);
            if c1 < cost0 {
                *p = trial;
                lambda = (lambda / 3.0).max(1e-9);
                improved = Some((cost0 - c1, step.iter().fold(0.0f64, |m, v| m.max(v.abs()))));
                break;
            }
            lambda *= 4.0;
        }
        match improved {
            Some((gain, biggest)) if gain > 1e-10 * cost0.max(1e-30) && biggest > 1e-10 => {}
            _ => break,
        }
    }
    Ok(fit(p, links))
}

/// The frames reached from the best connected one by the matches with the
/// most agreeing points (a maximum spanning tree): the root, then each
/// other frame with the link it was reached by, in the order reached.
pub fn tree(links: &[Link], n: usize) -> (usize, Vec<(usize, usize)>) {
    let mut score = vec![(0usize, 0usize); n];
    for l in links {
        for i in [l.a, l.b] {
            score[i].0 += 1;
            score[i].1 += l.inliers;
        }
    }
    // The frame with the most neighbours, the middle one of a row (in the
    // order taken), then the most agreeing points.
    let off_middle = |i: usize| (2 * i).abs_diff(n - 1);
    let root = (0..n)
        .max_by_key(|&i| (score[i].0, std::cmp::Reverse(off_middle(i)), score[i].1))
        .unwrap_or(0);
    let mut seen = vec![false; n];
    seen[root] = true;
    let mut order = Vec::new();
    loop {
        let next = links
            .iter()
            .enumerate()
            .filter(|(_, l)| seen[l.a] != seen[l.b])
            .max_by_key(|(_, l)| l.inliers);
        let Some((li, l)) = next else {
            break;
        };
        let new = if seen[l.a] { l.b } else { l.a };
        seen[new] = true;
        order.push((new, li));
    }
    (root, order)
}

/// Starting rotations from the tree: each frame's relative to the one it
/// was reached from, from their homography (`K⁻¹·H·K`, the nearest
/// rotation).
pub fn initial_rotations(
    links: &[Link],
    n: usize,
    root: usize,
    order: &[(usize, usize)],
    f: f64,
    centres: &[[f64; 2]],
) -> Rotations {
    let mut r = vec![colour::IDENTITY; n];
    let k = [[f, 0.0, 0.0], [0.0, f, 0.0], [0.0, 0.0, 1.0]];
    let ki = [[1.0 / f, 0.0, 0.0], [0.0, 1.0 / f, 0.0], [0.0, 0.0, 1.0]];
    let shift = |c: [f64; 2], sign: f64| {
        [
            [1.0, 0.0, sign * c[0]],
            [0.0, 1.0, sign * c[1]],
            [0.0, 0.0, 1.0],
        ]
    };
    let mut done = vec![false; n];
    done[root] = true;
    for &(j, li) in order {
        let l = &links[li];
        let i = l.other(j);
        let rel = l.from(i).and_then(|h| {
            // Centred: H_c = T_j · H · T_i⁻¹.
            let hc = geom::mul(
                &shift(centres[j], -1.0),
                &geom::mul(&h, &shift(centres[i], 1.0)),
            );
            geom::nearest_rotation(&geom::mul(&ki, &geom::mul(&hc, &k)))
        });
        r[j] = match rel {
            Some(rel) if done[i] => geom::mul(&rel, &r[i]),
            _ => r[i],
        };
        done[j] = true;
    }
    Rotations {
        r,
        f,
        centres: centres.to_vec(),
    }
}

/// Starting planes from the tree: each frame onto the root's (the
/// identity) through its parent's homography.
pub fn initial_planes(
    links: &[Link],
    n: usize,
    order: &[(usize, usize)],
    sizes: &[(usize, usize)],
) -> Planes {
    let mut g = vec![colour::IDENTITY; n];
    for &(j, li) in order {
        let l = &links[li];
        let i = l.other(j);
        // j → i, then i → plane.
        if let Some(h) = l.from(j) {
            g[j] = geom::mul(&g[i], &h);
        }
    }
    Planes::new(g, sizes)
}

/// The focal length a homography between two frames of one camera turning
/// implies (centred pixels; OpenCV's `focalsFromHomography`, after
/// Szeliski and Shum), as the geometric mean of its two estimates.
pub fn focal_from_homography(h: &M3) -> Option<f64> {
    let h = [
        h[0][0], h[0][1], h[0][2], h[1][0], h[1][1], h[1][2], h[2][0], h[2][1], h[2][2],
    ];
    let pick = |d1: f64, d2: f64, v1: f64, v2: f64| -> Option<f64> {
        let (v1, v2) = if v1 < v2 { (v2, v1) } else { (v1, v2) };
        if v1 > 0.0 && v2 > 0.0 {
            Some(if d1.abs() > d2.abs() { v1 } else { v2 }.sqrt())
        } else if v1 > 0.0 {
            Some(v1.sqrt())
        } else {
            None
        }
    };
    let d1 = h[6] * h[7];
    let d2 = (h[7] - h[6]) * (h[7] + h[6]);
    let v1 = -(h[0] * h[1] + h[3] * h[4]) / d1;
    let v2 = (h[0] * h[0] + h[3] * h[3] - h[1] * h[1] - h[4] * h[4]) / d2;
    let f1 = pick(d1, d2, v1, v2)?;
    let d1 = h[0] * h[3] + h[1] * h[4];
    let d2 = h[0] * h[0] + h[1] * h[1] - h[3] * h[3] - h[4] * h[4];
    let v1 = -h[2] * h[5] / d1;
    let v2 = (h[5] * h[5] - h[2] * h[2]) / d2;
    let f0 = pick(d1, d2, v1, v2)?;
    let f = (f0 * f1).sqrt();
    f.is_finite().then_some(f)
}

/// Straighten (wave correction): turn the world so its up is the
/// direction least like the frames' x axes (a level row's lie in the
/// horizontal plane), down agreeing with the frames' own, and forward the
/// frames' mean view. With the x axes all alike (a single column, or one
/// frame), up is the frames' mean up. Returns the world's turn (old →
/// new); `r` becomes the frames' rotations from the new world.
pub fn straighten(r: &mut [M3]) -> M3 {
    let mut c = [[0.0f64; 3]; 3];
    let (mut ys, mut zs) = ([0.0f64; 3], [0.0f64; 3]);
    for m in r.iter() {
        let x = m[0];
        for i in 0..3 {
            for j in 0..3 {
                c[i][j] += x[i] * x[j];
            }
            ys[i] += m[1][i];
            zs[i] += m[2][i];
        }
    }
    let (vals, vecs) = geom::eigen_sym(&c);
    let mut down = if vals[1] > 1e-3 * vals[2].max(1e-12) {
        vecs[0]
    } else {
        geom::unit(ys).unwrap_or([0.0, 1.0, 0.0])
    };
    if geom::dot(down, ys) < 0.0 {
        down = down.map(|v| -v);
    }
    // Forward: the mean view, level (or the first frame's, all round).
    let level = |z: V3| geom::unit(sub(z, down.map(|v| v * geom::dot(z, down))));
    let forward = level(zs)
        .filter(|_| geom::norm(zs) > 0.2 * r.len() as f64)
        .or_else(|| r.first().and_then(|m| level(m[2])))
        .unwrap_or([0.0, 0.0, 1.0]);
    let right = geom::cross(down, forward);
    let w = [right, down, forward];
    for m in r.iter_mut() {
        *m = geom::mul(m, &geom::t(&w));
    }
    w
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
