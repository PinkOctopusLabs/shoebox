//! The modules that need a neighbourhood (EDITING-SPEC modules 5, 7 and 8:
//! dehaze, highlights and shadows, clarity), as a *fast guided filter*:
//!
//! - A small copy of the source (long edge [`SMALL`]) is analysed on the CPU:
//!   guided-filter coefficients `(a, b)` per small pixel, box-averaged. They
//!   live in source space, so crop, rotation, the proxy, 100% regions and
//!   export all use the same maps.
//! - The live pipe samples the coefficients (bilinear) at each pixel's
//!   source position and evaluates `a·I + b` with that pixel's own guide
//!   `I`, so edges stay sharp at any resolution (He and Sun, "Fast Guided
//!   Filter", 2015).
//!
//! Highlights and shadows: a base layer of log luminance (stops from mid
//! grey, guided by itself) decides each pixel's gain, so detail keeps its
//! contrast and edges don't halo. Clarity: the same at a coarser scale,
//! boosting the detail above that base in the midtones. Dehaze: the dark
//! channel prior (He, Sun and Tang, 2009) for airlight and transmission,
//! the transmission refined by a guided filter on luminance.
//!
//! The maps depend on the colour matrix and the dehaze amount (and Smart
//! Lighting's spot), not on the slider amounts, so dragging highlights,
//! shadows or clarity reuses them.

use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use super::{Demosaiced, Rgb};
use crate::colour;
use crate::edit::Settings;

/// The analysis copy's long edge.
pub const SMALL: usize = 512;
/// Floats per map pixel: highlights and shadows `(a, b)`, clarity `(a, b)`,
/// dehaze transmission `(a, b)`, Smart Lighting's gain `(a, b)`.
pub const STRIDE: usize = 8;

/// Guided filter radii (fractions of the long edge) and regularisation.
const HS_RADIUS: f32 = 0.025;
const HS_EPS: f32 = 0.25;
const CLARITY_RADIUS: f32 = 0.06;
const CLARITY_EPS: f32 = 1.0;
const HAZE_PATCH: f32 = 0.01;
const HAZE_RADIUS: f32 = 0.04;
const HAZE_EPS: f32 = 1e-3;
const HAZE_OMEGA: f32 = 0.95;
const HAZE_FLOOR: f32 = 0.1;
/// Smart Lighting's gain map, fitted to log luminance for the full image.
const SMART_RADIUS: f32 = 0.02;
const SMART_EPS: f32 = 0.05;

fn same(a: Option<&Arc<Rgb>>, b: Option<&Arc<Rgb>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

/// The maps, `width`×`height`, [`STRIDE`] floats each.
#[derive(Debug, Clone, PartialEq)]
pub struct Maps {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    /// Dehaze's airlight, linear Rec.2020.
    pub airlight: [f32; 3],
}

/// A source's small copy and its last maps (keyed by what they depend on).
#[derive(Clone, Default)]
pub struct Cache(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    small: Option<Arc<Rgb>>,
    maps: Option<(u64, Arc<Maps>)>,
    /// Smart Lighting's uniform gain on the small copy (keyed by what the
    /// luminance depends on), so moving a spot doesn't run the filter again.
    smart: Option<(u64, Arc<Vec<f32>>)>,
    noise: Option<super::detail::Noise>,
    detailed: Option<(super::detail::Detail, Option<Arc<Rgb>>, Arc<Rgb>)>,
    masks: Option<(u64, Arc<crate::mask::Masks>)>,
    /// The AI-denoised source (as `rgb`), once it's arrived, and the last
    /// blend with `rgb` (strength bits).
    ai: Option<Arc<Rgb>>,
    blended: Option<(u32, Arc<Rgb>)>,
    /// The healed source: what it was made from, the spots' key, and it.
    healed: Option<(Option<Arc<Rgb>>, u64, Arc<Rgb>)>,
    mask_source: Option<Arc<Rgb>>,
    /// Subjects and people found (see `layers`), by what, orientation and
    /// grid size; bumped whenever one arrives.
    found: std::collections::HashMap<FoundKey, Found>,
    found_generation: u64,
    /// Find them on a thread of their own (the live view) rather than
    /// waiting (exports, 100% regions).
    background: bool,
    /// The images beside the source for local adjustments (see
    /// `cpu::Aux`): their key, what they were made from, and them.
    aux: Option<(u64, Vec<Arc<Rgb>>, Arc<super::Aux>)>,
}

/// What was found, the orientation it was found in, and the grid size.
pub type FoundKey = (crate::mask::Find, u16, usize, usize);

/// A subject or person search.
#[derive(Debug, Clone)]
pub enum Found {
    Pending,
    Done(Arc<Vec<u8>>),
    Failed(String),
}

impl Cache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The source's noise, estimated once.
    pub fn noise(&self, img: &Rgb) -> super::detail::Noise {
        *self
            .lock()
            .noise
            .get_or_insert_with(|| super::detail::estimate(img))
    }

    /// Find subjects and people on a thread of their own (for the live
    /// view, which mustn't wait), redrawing when they arrive.
    pub fn set_background(&self, on: bool) {
        self.lock().background = on;
    }

    pub fn background(&self) -> bool {
        self.lock().background
    }

    pub fn found(&self, key: &FoundKey) -> Option<Found> {
        self.lock().found.get(key).cloned()
    }

    pub fn set_found(&self, key: FoundKey, f: Found) {
        let mut inner = self.lock();
        inner.found.insert(key, f);
        inner.found_generation += 1;
    }

    /// Forget searches that failed (a model has arrived since), so they're
    /// tried again.
    pub fn forget_failed(&self) {
        let mut inner = self.lock();
        inner.found.retain(|_, f| !matches!(f, Found::Failed(_)));
        inner.found_generation += 1;
    }

    /// Changes whenever a search finishes (renders key on it).
    pub fn found_generation(&self) -> u64 {
        self.lock().found_generation
    }

    /// A search still running, and the last one that failed (why).
    pub fn finding(&self) -> (bool, Option<String>) {
        let inner = self.lock();
        let pending = inner.found.values().any(|f| matches!(f, Found::Pending));
        let failed = inner.found.values().find_map(|f| match f {
            Found::Failed(e) => Some(e.clone()),
            _ => None,
        });
        (pending, failed)
    }

    /// The local adjustments' masks for `key` (`make` runs on a miss).
    pub fn masks(
        &self,
        key: u64,
        make: impl FnOnce() -> crate::mask::Masks,
    ) -> Arc<crate::mask::Masks> {
        if let Some((k, m)) = &self.lock().masks
            && *k == key
        {
            return m.clone();
        }
        let m = Arc::new(make());
        self.lock().masks = Some((key, m.clone()));
        m
    }

    /// `img` (the source) on the masks' `w`×`h` grid, made once.
    pub fn mask_source(&self, img: &Rgb, w: usize, h: usize) -> Arc<Rgb> {
        if let Some(m) = &self.lock().mask_source
            && (m.width, m.height) == (w, h)
        {
            return m.clone();
        }
        let m = Arc::new(if (img.width, img.height) == (w, h) {
            img.clone()
        } else {
            resize(img, w, h)
        });
        self.lock().mask_source = Some(m.clone());
        m
    }

    /// The source after spot removal, from `from` (the detail stage's
    /// result; None: the source itself) and spots keyed `key`.
    pub fn healed(
        &self,
        from: Option<&Arc<Rgb>>,
        key: u64,
        make: impl FnOnce() -> Rgb,
    ) -> Arc<Rgb> {
        if let Some((f, k, img)) = &self.lock().healed
            && *k == key
            && match (f, from) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
        {
            return img.clone();
        }
        let img = Arc::new(make());
        self.lock().healed = Some((from.cloned(), key, img.clone()));
        img
    }

    /// The detail stage's result for `d` on `base` (None: the source;
    /// `make` runs on a miss).
    pub fn detailed(
        &self,
        d: &super::detail::Detail,
        base: Option<&Arc<Rgb>>,
        make: impl FnOnce() -> Rgb,
    ) -> Arc<Rgb> {
        if let Some((k, b, img)) = &self.lock().detailed
            && k == d
            && same(b.as_ref(), base)
        {
            return img.clone();
        }
        let img = Arc::new(make());
        self.lock().detailed = Some((*d, base.cloned(), img.clone()));
        img
    }

    /// The AI-denoised source arrived (as the source's size): renders
    /// redraw with it.
    pub fn set_ai(&self, img: Arc<Rgb>) {
        let mut inner = self.lock();
        inner.ai = Some(img);
        inner.blended = None;
        inner.found_generation += 1;
    }

    pub fn ai(&self) -> Option<Arc<Rgb>> {
        self.lock().ai.clone()
    }

    /// The source and the AI's blended at `strength` (0–1), kept.
    pub fn blended(&self, src: &Rgb, ai: &Rgb, strength: f32) -> Arc<Rgb> {
        if let Some((k, img)) = &self.lock().blended
            && *k == strength.to_bits()
        {
            return img.clone();
        }
        let mut out = src.clone();
        out.data
            .par_iter_mut()
            .zip(&ai.data)
            .for_each(|(o, a)| *o += (a - *o) * strength);
        let img = Arc::new(out);
        self.lock().blended = Some((strength.to_bits(), img.clone()));
        img
    }

    /// The images beside the source for `key` (`make` runs on a miss);
    /// `from` (what they're made from) is kept with them.
    pub fn aux(
        &self,
        key: u64,
        from: Vec<Arc<Rgb>>,
        make: impl FnOnce() -> super::Aux,
    ) -> Arc<super::Aux> {
        if let Some((k, _, a)) = &self.lock().aux
            && *k == key
        {
            return a.clone();
        }
        // The old ones go first: they can be as large as the source.
        self.lock().aux = None;
        let a = Arc::new(make());
        self.lock().aux = Some((key, from, a.clone()));
        a
    }

    /// No layer needs the images beside the source any more.
    pub fn forget_aux(&self) {
        self.lock().aux = None;
    }
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("maps::Cache")
    }
}

/// What the live pipe needs for these modules.
#[derive(Debug, Clone)]
pub struct Local {
    /// −1 to 1.
    pub shadows: f32,
    pub highlights: f32,
    pub clarity: f32,
    pub dehaze: f32,
    /// Smart Lighting: 0 to 1, and its bias −1 to 1.
    pub smart: f32,
    pub smart_bias: f32,
    /// None: nothing to do.
    pub maps: Option<Arc<Maps>>,
    /// Source pixels → map pixels.
    pub scale: [f32; 2],
    /// Rec.2020 luminance weights.
    pub luma: [f32; 3],
}

impl Local {
    pub fn none() -> Self {
        Self {
            shadows: 0.0,
            highlights: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            smart: 0.0,
            smart_bias: 0.0,
            maps: None,
            scale: [1.0; 2],
            luma: luma(),
        }
    }
}

fn luma() -> [f32; 3] {
    colour::rgb_to_xyz(&colour::REC2020, colour::D65)[1].map(|v| v as f32)
}

/// The modules' numbers for `src` under `colour` (the frame's matrix).
pub fn local(src: &Demosaiced, colour: &[[f32; 3]; 3], s: &Settings) -> Local {
    let m = &s.modules;
    let unit = |v: f64| (v / 100.0) as f32;
    let mut l = Local {
        shadows: unit(m.highlights_shadows.shadows),
        highlights: unit(m.highlights_shadows.highlights),
        clarity: unit(m.clarity.amount),
        dehaze: unit(m.dehaze.amount),
        smart: unit(m.smart_lighting.strength).clamp(0.0, 1.0),
        smart_bias: unit(m.smart_lighting.bias).clamp(-1.0, 1.0),
        ..Local::none()
    };
    // Local adjustments use the same maps.
    let layers = s.locals.iter().filter(|l| l.enabled).map(|l| &l.adjust);
    let (mut tones, mut clarity, mut haze) = (false, false, false);
    for a in layers {
        tones |= a.shadows != 0.0 || a.highlights != 0.0;
        clarity |= a.clarity != 0.0;
        haze |= a.dehaze != 0.0;
    }
    let need = Need {
        tones: tones || l.shadows != 0.0 || l.highlights != 0.0,
        clarity: clarity || l.clarity != 0.0,
        haze: haze || l.dehaze != 0.0,
        smart: l.smart != 0.0,
        spot: m
            .smart_lighting
            .spot
            .filter(|b| b.iter().all(|v| v.is_finite()))
            .map(|b| b.map(|v| v as f32)),
        dehaze: l.dehaze,
    };
    if !need.tones && !need.clarity && !need.haze && !need.smart {
        return l;
    }
    let hash = |extra: &dyn Fn(&mut std::collections::hash_map::DefaultHasher)| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for r in colour {
            for v in r {
                v.to_bits().hash(&mut h);
            }
        }
        (need.haze, need.dehaze.to_bits()).hash(&mut h);
        extra(&mut h);
        h.finish()
    };
    // What the luminance depends on (Smart Lighting's uniform gain), and
    // everything the maps do.
    let base = hash(&|_| {});
    let key = hash(&|h| {
        use std::hash::Hash;
        (need.tones, need.clarity, need.smart).hash(h);
        need.spot.map(|b| b.map(f32::to_bits)).hash(h);
    });
    let mut cache = src.cache.lock();
    let small = cache
        .small
        .get_or_insert_with(|| Arc::new(small(&src.rgb)))
        .clone();
    let maps = match &cache.maps {
        Some((k, maps)) if *k == key => maps.clone(),
        _ => {
            let uniform = match &cache.smart {
                Some((k, g)) if *k == base => Some(g.clone()),
                _ => None,
            };
            let (maps, uniform) = analyse(&small, colour, &need, uniform);
            let maps = Arc::new(maps);
            if let Some(g) = uniform {
                cache.smart = Some((base, g));
            }
            cache.maps = Some((key, maps.clone()));
            maps
        }
    };
    l.scale = [
        maps.width as f32 / src.rgb.width as f32,
        maps.height as f32 / src.rgb.height as f32,
    ];
    l.maps = Some(maps);
    l
}

struct Need {
    tones: bool,
    clarity: bool,
    /// The transmission map (for the global dehaze or a layer's).
    haze: bool,
    /// Smart Lighting's gain map (at full strength: the slider scales it).
    smart: bool,
    /// Its spot (stored image, long edges); None: uniform.
    spot: Option<[f32; 4]>,
    /// The global dehaze, applied before the tone maps are analysed.
    dehaze: f32,
}

/// An area-averaged copy with its long edge at most [`SMALL`].
pub fn small(img: &Rgb) -> Rgb {
    let long = img.width.max(img.height);
    if long <= SMALL {
        return img.clone();
    }
    let k = SMALL as f64 / long as f64;
    let w = ((img.width as f64 * k).round() as usize).max(1);
    let h = ((img.height as f64 * k).round() as usize).max(1);
    resize(img, w, h)
}

/// An area-averaged copy at `w`×`h` (no larger than `img`).
pub fn resize(img: &Rgb, w: usize, h: usize) -> Rgb {
    let xs = weights(img.width, w);
    let ys = weights(img.height, h);
    let mut rows = vec![0f32; w * img.height * 3];
    rows.par_chunks_mut(w * 3).enumerate().for_each(|(y, out)| {
        for (x, ws) in xs.iter().enumerate() {
            for &(i, wt) in ws {
                let p = img.px(i, y);
                for c in 0..3 {
                    out[x * 3 + c] += p[c] * wt;
                }
            }
        }
    });
    let mut out = Rgb::new(w, h);
    out.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, line)| {
            for &(i, wt) in &ys[y] {
                let src = &rows[i * w * 3..(i + 1) * w * 3];
                for (o, v) in line.iter_mut().zip(src) {
                    *o += v * wt;
                }
            }
        });
    out
}

/// Each output sample as the area-weighted mean of the input it covers.
fn weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|o| {
            let (start, end) = (o as f64 * scale, (o + 1) as f64 * scale);
            let mut ws = Vec::new();
            let mut i = start.floor() as usize;
            while (i as f64) < end && i < src {
                let lo = (i as f64).max(start);
                let hi = ((i + 1) as f64).min(end);
                if hi > lo {
                    ws.push((i, ((hi - lo) / scale) as f32));
                }
                i += 1;
            }
            ws
        })
        .collect()
}

fn mul3(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Stops from mid grey.
pub(crate) fn stops(y: f32) -> f32 {
    (y.max(1e-6) / super::MID_GREY).log2()
}

/// The maps for `small`, and Smart Lighting's uniform gain (`uniform` if
/// given: it depends only on the colour matrix and dehaze).
fn analyse(
    small: &Rgb,
    colour: &[[f32; 3]; 3],
    need: &Need,
    uniform: Option<Arc<Vec<f32>>>,
) -> (Maps, Option<Arc<Vec<f32>>>) {
    let (w, h) = (small.width, small.height);
    let long = w.max(h) as f32;
    let radius = |f: f32| ((f * long).round() as usize).max(1);
    let y = luma();
    let mut lin: Vec<[f32; 3]> = small
        .data
        .as_chunks::<3>()
        .0
        .par_iter()
        .map(|p| mul3(colour, *p))
        .collect();
    let mut data = vec![0f32; w * h * STRIDE];
    let mut airlight = [1.0f32; 3];
    if need.haze {
        let dark = min_filter(
            &lin.iter()
                .map(|p| p[0].min(p[1]).min(p[2]))
                .collect::<Vec<_>>(),
            w,
            h,
            radius(HAZE_PATCH),
        );
        // Airlight: the mean colour of the haziest 0.1%.
        let mut order: Vec<usize> = (0..w * h).collect();
        let n = (w * h / 1000).max(1);
        order.select_nth_unstable_by(n - 1, |&a, &b| dark[b].total_cmp(&dark[a]));
        let mut sum = [0f32; 3];
        for &i in &order[..n] {
            for c in 0..3 {
                sum[c] += lin[i][c];
            }
        }
        airlight = sum.map(|v| (v / n as f32).max(1e-4));
        let norm: Vec<f32> = lin
            .iter()
            .map(|p| (0..3).map(|c| p[c] / airlight[c]).fold(f32::MAX, f32::min))
            .collect();
        let dn = min_filter(&norm, w, h, radius(HAZE_PATCH));
        let t: Vec<f32> = dn
            .iter()
            .map(|d| (1.0 - HAZE_OMEGA * d).clamp(0.0, 1.0))
            .collect();
        let ay = dot(y, airlight);
        let guide: Vec<f32> = lin.par_iter().map(|p| haze_guide(dot(y, *p), ay)).collect();
        let (a, b) = guided(&guide, &t, w, h, radius(HAZE_RADIUS), HAZE_EPS);
        data.par_chunks_mut(STRIDE)
            .zip(lin.par_iter_mut())
            .enumerate()
            .for_each(|(i, (d, p))| {
                d[4] = a[i];
                d[5] = b[i];
                *p = dehaze_px(*p, a[i] * guide[i] + b[i], airlight, need.dehaze);
            });
    }
    let l: Vec<f32> = lin.par_iter().map(|p| stops(dot(y, *p))).collect();
    for (on, at, r, eps) in [
        (need.tones, 0, HS_RADIUS, HS_EPS),
        (need.clarity, 2, CLARITY_RADIUS, CLARITY_EPS),
    ] {
        if on {
            let (a, b) = guided(&l, &l, w, h, radius(r), eps);
            data.par_chunks_mut(STRIDE).enumerate().for_each(|(i, d)| {
                d[at] = a[i];
                d[at + 1] = b[i];
            });
        }
    }
    let mut uniform = uniform.filter(|g| g.len() == w * h);
    if need.smart {
        let g = uniform
            .get_or_insert_with(|| Arc::new(super::smart::gain(&l, w, h)))
            .clone();
        // Spot weighted: its box in the small copy's pixels.
        let g = match need.spot {
            Some(b) => Arc::new(super::smart::spotted(&l, &g, w, h, b.map(|v| v * long))),
            None => g,
        };
        let (a, b) = guided(&l, &g, w, h, radius(SMART_RADIUS), SMART_EPS);
        data.par_chunks_mut(STRIDE).enumerate().for_each(|(i, d)| {
            d[6] = a[i];
            d[7] = b[i];
        });
    }
    (
        Maps {
            width: w,
            height: h,
            data,
            airlight,
        },
        uniform,
    )
}

fn haze_guide(y: f32, airlight_y: f32) -> f32 {
    (y / airlight_y.max(1e-6)).clamp(0.0, 2.0)
}

/// Remove (amount > 0) or add (< 0) haze, given transmission `t`.
pub fn dehaze_px(p: [f32; 3], t: f32, airlight: [f32; 3], amount: f32) -> [f32; 3] {
    if amount > 0.0 {
        let t = t.clamp(HAZE_FLOOR, 1.0);
        [0, 1, 2].map(|c| {
            let j = (p[c] - airlight[c]) / t + airlight[c];
            (p[c] + amount * (j - p[c])).max(0.0)
        })
    } else {
        [0, 1, 2].map(|c| p[c] + (-amount) * 0.5 * (airlight[c] - p[c]))
    }
}

/// The mean of a `(2r+1)²` box (clipped at the edges), by running sums.
pub(crate) fn box_mean(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let line = &v[y * w..(y + 1) * w];
        let mut prefix = vec![0f64; w + 1];
        for x in 0..w {
            prefix[x + 1] = prefix[x] + f64::from(line[x]);
        }
        for (x, o) in out.iter_mut().enumerate() {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            *o = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
        }
    });
    // Down the columns: running sums of whole rows.
    let mut prefix = vec![0f64; (h + 1) * w];
    for y in 0..h {
        let (done, next) = prefix.split_at_mut((y + 1) * w);
        let above = &done[y * w..];
        for (x, p) in next[..w].iter_mut().enumerate() {
            *p = above[x] + f64::from(rows[y * w + x]);
        }
    }
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
        let (lo, hi) = (y.saturating_sub(r), (y + r + 1).min(h));
        let n = (hi - lo) as f64;
        let (top, bottom) = (&prefix[lo * w..], &prefix[hi * w..]);
        for (x, o) in line.iter_mut().enumerate() {
            *o = ((bottom[x] - top[x]) / n) as f32;
        }
    });
    out
}

/// The minimum over a `(2r+1)²` box (clipped at the edges).
fn min_filter(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0f32; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let line = &v[y * w..(y + 1) * w];
        for (x, o) in out.iter_mut().enumerate() {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            *o = line[lo..hi].iter().copied().fold(f32::MAX, f32::min);
        }
    });
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, line)| {
        let (lo, hi) = (y.saturating_sub(r), (y + r + 1).min(h));
        for (x, o) in line.iter_mut().enumerate() {
            *o = (lo..hi).map(|yy| rows[yy * w + x]).fold(f32::MAX, f32::min);
        }
    });
    out
}

/// A guided filter's box-averaged coefficients `(ā, b̄)`: the output at a
/// pixel with guide `I` is `ā·I + b̄`.
fn guided(
    guide: &[f32],
    p: &[f32],
    w: usize,
    h: usize,
    r: usize,
    eps: f32,
) -> (Vec<f32>, Vec<f32>) {
    let mean_i = box_mean(guide, w, h, r);
    let mean_p = box_mean(p, w, h, r);
    let ip: Vec<f32> = guide.par_iter().zip(p).map(|(i, p)| i * p).collect();
    let ii: Vec<f32> = guide.par_iter().map(|i| i * i).collect();
    let mean_ip = box_mean(&ip, w, h, r);
    let mean_ii = box_mean(&ii, w, h, r);
    let (a, b): (Vec<f32>, Vec<f32>) = (0..w * h)
        .into_par_iter()
        .map(|k| {
            let var = (mean_ii[k] - mean_i[k] * mean_i[k]).max(0.0);
            let cov = mean_ip[k] - mean_i[k] * mean_p[k];
            let a = cov / (var + eps);
            (a, mean_p[k] - a * mean_i[k])
        })
        .unzip();
    (box_mean(&a, w, h, r), box_mean(&b, w, h, r))
}

/// The maps at source position `(sx, sy)`.
pub fn at_source(l: &Local, maps: &Maps, sx: f32, sy: f32) -> [f32; STRIDE] {
    sample(
        maps,
        (sx + 0.5) * l.scale[0] - 0.5,
        (sy + 0.5) * l.scale[1] - 0.5,
    )
}

/// The maps at map position `(x, y)` (bilinear, clamped to the edges).
pub fn sample(m: &Maps, x: f32, y: f32) -> [f32; STRIDE] {
    let x = x.clamp(0.0, (m.width - 1) as f32);
    let y = y.clamp(0.0, (m.height - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(m.width - 1), (y0 + 1).min(m.height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let at = |xx: usize, yy: usize| &m.data[(yy * m.width + xx) * STRIDE..][..STRIDE];
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        top + (bot - top) * fy
    })
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The modules for one linear Rec.2020 pixel at source position
/// `(sx, sy)`.
pub fn local_pixel(l: &Local, p: [f32; 3], sx: f32, sy: f32) -> [f32; 3] {
    let Some(maps) = &l.maps else {
        return p;
    };
    let m = at_source(l, maps, sx, sy);
    let mut p = p;
    if l.dehaze != 0.0 {
        p = dehaze_at(maps, &m, p, l.luma, l.dehaze);
    }
    let s = stops(dot(l.luma, p));
    let mut gain = tone_gain(&m, s, l.shadows, l.highlights, l.clarity);
    if l.smart != 0.0 {
        gain += smart_gain(&m, s, l.smart, l.smart_bias);
    }
    if gain == 0.0 {
        return p;
    }
    let k = gain.exp2();
    p.map(|v| v * k)
}

/// Smart Lighting's gain in stops for a pixel `s` stops from mid grey with
/// maps `m` there: the full-strength gain scaled, lifts by `1 + bias` and
/// cuts by `1 − bias`.
pub fn smart_gain(m: &[f32; STRIDE], s: f32, strength: f32, bias: f32) -> f32 {
    let g = m[6] * s + m[7];
    let k = if g > 0.0 { 1.0 + bias } else { 1.0 - bias };
    strength * g * k
}

/// Highlights, shadows and clarity as a gain in stops, for a pixel `s`
/// stops from mid grey with maps `m` there.
pub fn tone_gain(m: &[f32; STRIDE], s: f32, shadows: f32, highlights: f32, clarity: f32) -> f32 {
    let mut gain = 0.0;
    if shadows != 0.0 || highlights != 0.0 {
        let base = m[0] * s + m[1];
        gain += shadows * 1.5 * (1.0 - smoothstep(-3.0, 0.0, base));
        gain += highlights * 1.5 * smoothstep(-0.5, 2.5, base);
    }
    if clarity != 0.0 {
        let base = m[2] * s + m[3];
        let detail = (s - base).clamp(-3.0, 3.0);
        let mid = 1.0 - smoothstep(2.0, 4.0, (base + 0.5).abs());
        gain += clarity * 0.7 * detail * mid;
    }
    gain
}

/// Dehaze at a pixel with maps `m` there.
pub fn dehaze_at(
    maps: &Maps,
    m: &[f32; STRIDE],
    p: [f32; 3],
    luma: [f32; 3],
    amount: f32,
) -> [f32; 3] {
    let g = haze_guide(dot(luma, p), dot(luma, maps.airlight));
    dehaze_px(p, m[4] * g + m[5], maps.airlight, amount)
}
