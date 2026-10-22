//! Local adjustments in the live pipe (EDITING-SPEC, Local adjustments):
//! after the global highlights, shadows, clarity and dehaze, before the
//! colour stage, in linear Rec.2020. Each layer's sliders make an adjusted
//! pixel, and its mask (see [`crate::mask`]) blends it in.
//!
//! Per layer: dehaze (the global transmission map), then exposure, white
//! balance and saturation as one matrix, contrast about mid grey, and
//! highlights, shadows and clarity through the global maps' base layers.
//! The shader (`develop.wgsl`) follows these operation for operation.

use std::sync::Arc;

use rayon::prelude::*;

use super::{Demosaiced, Display, Rgb, look, maps};
use crate::colour::{self, M3};
use crate::edit::{Local, Settings};
use crate::mask::{self, Masks, Tones};

/// A layer's numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    /// Any slider moved (a layer with none is kept for its mask to show).
    pub active: bool,
    /// Exposure, white balance and saturation.
    pub matrix: [[f32; 3]; 3],
    /// −1 to 1.
    pub contrast: f32,
    pub shadows: f32,
    pub highlights: f32,
    pub clarity: f32,
    pub dehaze: f32,
    /// Fine detail added (+) or taken away (−): −1 to 1.
    pub sharpness: f32,
}

/// What the live pipe needs for the local adjustments.
#[derive(Debug, Clone, Default)]
pub struct Layers {
    pub list: Vec<Layer>,
    /// The layers' ids, as `list`, and where each is in the settings'
    /// `locals`.
    pub ids: Vec<String>,
    pub from: Vec<usize>,
    /// One plane per layer in `list`.
    pub masks: Option<Arc<Masks>>,
    /// Source pixels → mask pixels.
    pub scale: [f32; 2],
    /// A layer sharpens or softens: the pipe works out each pixel's fine
    /// detail.
    pub detail: bool,
}

impl Layers {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// The layers with a mask, and where each is in `locals`.
fn active(s: &Settings) -> (Vec<&Local>, Vec<usize>) {
    s.locals
        .iter()
        .enumerate()
        .filter(|(_, l)| l.enabled && !l.mask.parts.is_empty())
        .map(|(i, l)| (l, i))
        .unzip()
}

/// The layers for `src` under the frame's colour matrix and display.
/// `orientation`: the frame's orientation (the file's, then the edit's
/// turns and mirror), so subjects and people are found upright.
pub fn layers(
    src: &Demosaiced,
    colour: &[[f32; 3]; 3],
    display: &Display,
    s: &Settings,
    orientation: u16,
) -> Layers {
    let (locals, from) = active(s);
    if locals.is_empty() {
        return Layers::default();
    }
    let (sw, sh) = (src.rgb.width, src.rgb.height);
    let (gw, gh) = mask::grid(sw, sh);
    let long = sw.max(sh).max(1) as f64;
    let units = [sw as f64 / long, sh as f64 / long];
    let tones = mask::needs_tones(&locals);
    let finds = mask::finds(&locals);
    let found: Vec<(mask::Find, Arc<Vec<u8>>)> = finds
        .iter()
        .filter_map(|(f, clicks)| {
            Some((
                *f,
                found(src, colour, display, *f, clicks, orientation, gw, gh)?,
            ))
        })
        .collect();
    // Larger than the grid with AI parts (exports, 100% regions): a finer
    // grid, the AI parts upsampled along the source (`mask::fine`).
    let fine = (!found.is_empty())
        .then(|| mask::fine::grid(sw, sh, locals.len()))
        .flatten();
    let (mw, mh) = fine.unwrap_or((gw, gh));
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for l in &locals {
            serde_json::to_string(&(&l.mask, l.amount))
                .unwrap_or_default()
                .hash(&mut h);
        }
        (gw, gh, mw, mh).hash(&mut h);
        if !finds.is_empty() {
            (orientation, src.cache.found_generation()).hash(&mut h);
        }
        if tones {
            // Ranges read the picture as the frame shows it.
            for v in colour.iter().flatten() {
                v.to_bits().hash(&mut h);
            }
            format!("{:?}{:?}", display.curve, display.referred).hash(&mut h);
        }
        h.finish()
    };
    let masks = src.cache.masks(key, || {
        let t = tones.then(|| {
            let small = src.cache.mask_source(&src.rgb, gw, gh);
            tones_of(&small, colour, display)
        });
        let lookup = |f: mask::Find| found.iter().find(|x| x.0 == f).map(|x| x.1.clone());
        match fine {
            Some(f) => {
                let small = src.cache.mask_source(&src.rgb, gw, gh);
                let guide = mask::fine::Guide::new(&src.rgb, &small);
                let grid = (gw, gh);
                mask::fine::rasterise(&locals, grid, f, units, t.as_ref(), &lookup, &guide)
            }
            None => mask::rasterise_with(&locals, gw, gh, units, t.as_ref(), &lookup),
        }
    });
    let list: Vec<Layer> = locals.iter().map(|l| layer(l)).collect();
    Layers {
        detail: list.iter().any(|l| l.active && l.sharpness != 0.0),
        list,
        ids: locals.iter().map(|l| l.id.clone()).collect(),
        from,
        masks: Some(masks),
        scale: [mw as f32 / sw as f32, mh as f32 / sh as f32],
    }
}

/// A subject or person on the masks' grid, in the stored image's
/// orientation, once found. The search runs once per source (and
/// orientation): on its own thread for the live view, which redraws when
/// it's done; otherwise here.
#[allow(clippy::too_many_arguments)]
fn found(
    src: &Demosaiced,
    colour: &[[f32; 3]; 3],
    display: &Display,
    what: mask::Find,
    clicks: &[crate::edit::ObjectClick],
    orientation: u16,
    gw: usize,
    gh: usize,
) -> Option<Arc<Vec<u8>>> {
    let key = (what, orientation, gw, gh);
    match src.cache.found(&key) {
        Some(maps::Found::Done(m)) => return Some(m),
        Some(_) => return None,
        None => {}
    }
    let (bytes, uw, uh) = shown_upright(src, colour, display, orientation, gw, gh);
    // An object's clicks, in the upright picture's pixels.
    let points: Vec<(f32, f32, bool)> = {
        let (m, _, _) = super::geometry_map(gw, gh, orientation, &crate::edit::Geometry::default());
        let to_upright = crate::colour::inv(&m).unwrap_or(crate::colour::IDENTITY);
        let glong = gw.max(gh) as f64;
        clicks
            .iter()
            .map(|c| {
                let s = [c.at[0] * glong - 0.5, c.at[1] * glong - 0.5, 1.0];
                let u = crate::colour::mulv(&to_upright, s);
                ((u[0] / u[2]) as f32, (u[1] / u[2]) as f32, c.add)
            })
            .collect()
    };
    let work = move || -> maps::Found {
        let result = match what {
            mask::Find::Object(_) => mask::segment(&bytes, uw, uh, &points),
            _ => mask::find(&bytes, uw, uh, what),
        };
        match result {
            Ok(m) if m.len() == uw * uh => {
                // Back to the stored image's orientation.
                let mut img = Rgb::new(uw, uh);
                for (i, v) in m.iter().enumerate() {
                    img.data[i * 3] = f32::from(*v);
                }
                let back = super::orient(&img, undo_orientation(orientation));
                let data: Vec<u8> = back
                    .data
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|p| p[0].round().clamp(0.0, 255.0) as u8)
                    .collect();
                maps::Found::Done(Arc::new(data))
            }
            Ok(_) => maps::Found::Failed("the mask came back the wrong size".into()),
            Err(e) => maps::Found::Failed(e),
        }
    };
    if src.cache.background() {
        src.cache.set_found(key, maps::Found::Pending);
        let cache = src.cache.clone();
        std::thread::spawn(move || cache.set_found(key, work()));
        return None;
    }
    let f = work();
    src.cache.set_found(key, f.clone());
    match f {
        maps::Found::Done(m) => Some(m),
        _ => None,
    }
}

/// The picture as shown on the masks' `gw`×`gh` grid, upright, in 8
/// bits, and its size.
fn shown_upright(
    src: &Demosaiced,
    colour: &[[f32; 3]; 3],
    display: &Display,
    orientation: u16,
    gw: usize,
    gh: usize,
) -> (Vec<u8>, usize, usize) {
    let small = src.cache.mask_source(&src.rgb, gw, gh);
    let mut shown = Rgb::new(gw, gh);
    shown
        .data
        .as_chunks_mut::<3>()
        .0
        .par_iter_mut()
        .zip(small.data.as_chunks::<3>().0.par_iter())
        .for_each(|(o, p)| *o = super::display_pixel(display, super::mul3(colour, *p)));
    let upright = super::orient(&shown, orientation);
    (super::to_u8(&upright), upright.width, upright.height)
}

/// The faces in `src` as the frame shows it (colour matrix and display),
/// found upright (`orientation`: the file's, then the edit's turns and
/// mirror) by the registered face finder: boxes `[x0, y0, x1, y1]` in the
/// stored image, in units of its long edge, the largest first. It waits
/// for the search (the app runs it on a thread of its own).
pub fn faces(
    src: &Demosaiced,
    colour: &[[f32; 3]; 3],
    display: &Display,
    orientation: u16,
) -> Result<Vec<[f64; 4]>, String> {
    let (gw, gh) = mask::grid(src.rgb.width, src.rgb.height);
    let (bytes, uw, uh) = shown_upright(src, colour, display, orientation, gw, gh);
    let found = mask::find_faces(&bytes, uw, uh)?;
    // Upright pixels → the grid's (the frame's map with no crop), then
    // long edges.
    let (m, _, _) = super::geometry_map(gw, gh, orientation, &crate::edit::Geometry::default());
    let glong = gw.max(gh) as f64;
    let mut boxes: Vec<[f64; 4]> = found
        .iter()
        .filter(|b| b.iter().all(|v| v.is_finite()))
        .map(|b| {
            let corners = [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]];
            let mut out = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for c in corners {
                // Pixel edges to pixel centres and back.
                let u = [f64::from(c[0]) - 0.5, f64::from(c[1]) - 0.5, 1.0];
                let s = colour::mulv(&m, u);
                let (x, y) = ((s[0] / s[2] + 0.5) / glong, (s[1] / s[2] + 0.5) / glong);
                out = [out[0].min(x), out[1].min(y), out[2].max(x), out[3].max(y)];
            }
            out
        })
        .filter(|b| b[2] > b[0] && b[3] > b[1])
        .collect();
    let area = |b: &[f64; 4]| (b[2] - b[0]) * (b[3] - b[1]);
    boxes.sort_by(|a, b| area(b).total_cmp(&area(a)));
    Ok(boxes)
}

/// The orientation that turns an image in orientation `o` back.
fn undo_orientation(o: u16) -> u16 {
    match o {
        6 => 8,
        8 => 6,
        o => o,
    }
}

fn layer(l: &Local) -> Layer {
    let a = &l.adjust;
    let unit = |v: f64| (v / 100.0) as f32;
    Layer {
        active: l.amount > 0.0 && *a != Default::default(),
        matrix: super::f32m(&matrix(a.exposure, a.temperature, a.tint, a.saturation)),
        contrast: unit(a.contrast),
        shadows: unit(a.shadows),
        highlights: unit(a.highlights),
        clarity: unit(a.clarity),
        dehaze: unit(a.dehaze),
        sharpness: unit(a.sharpness).clamp(-1.0, 1.0),
    }
}

/// Exposure (stops), temperature and tint (−100 to +100: warmer, more
/// magenta) and saturation (−100 to +100) as one matrix on linear
/// Rec.2020.
fn matrix(exposure: f64, temperature: f64, tint: f64, saturation: f64) -> M3 {
    let mut m = colour::diag([2f64.powf(exposure); 3]);
    if temperature != 0.0 || tint != 0.0 {
        // Adapt from a light as much warmer or cooler than daylight; at
        // +100, about 10 700 K.
        let base = 1.0e6 / 6504.0;
        let from = colour::temp_tint_to_xy(1.0e6 / (base - temperature * 0.6), tint * 0.5);
        let to = colour::temp_tint_to_xy(1.0e6 / base, 0.0);
        let wb = colour::mul(
            &colour::xyz_to_rgb(&colour::REC2020, colour::D65),
            &colour::mul(
                &colour::bradford(from, to),
                &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
            ),
        );
        m = colour::mul(&wb, &m);
    }
    if saturation != 0.0 {
        let s = (saturation / 100.0).max(-1.0);
        let y = colour::rgb_to_xyz(&colour::REC2020, colour::D65)[1];
        let mut sat = [[0.0; 3]; 3];
        for (r, row) in sat.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = if r == c { 1.0 + s } else { 0.0 } - s * y[c];
            }
        }
        m = colour::mul(&sat, &m);
    }
    m
}

/// Brightness as shown, hue and relative chroma of each pixel of `img`.
fn tones_of(img: &Rgb, colour: &[[f32; 3]; 3], display: &Display) -> Tones {
    let (to_lms, _) = look::lms_matrices();
    let luma = maps::Local::none().luma;
    let px: Vec<(f32, f32, f32)> = img
        .data
        .as_chunks::<3>()
        .0
        .par_iter()
        .map(|p| tone_with(*p, colour, display, &to_lms, luma))
        .collect();
    Tones {
        shown: px.iter().map(|p| p.0).collect(),
        hue: px.iter().map(|p| p.1).collect(),
        chroma: px.iter().map(|p| p.2).collect(),
    }
}

/// A source pixel's brightness as shown (0–1), Oklab hue (degrees) and
/// chroma relative to lightness, under the frame's colour matrix and
/// display: what luminance and colour range parts read (and pick).
pub fn tone_of(p: [f32; 3], colour: &[[f32; 3]; 3], display: &Display) -> (f32, f32, f32) {
    let luma = maps::Local::none().luma;
    tone_with(p, colour, display, &look::lms_matrices().0, luma)
}

fn tone_with(
    p: [f32; 3],
    colour: &[[f32; 3]; 3],
    display: &Display,
    to_lms: &[[f32; 3]; 3],
    luma: [f32; 3],
) -> (f32, f32, f32) {
    let lin = super::mul3(colour, p);
    let y = luma[0] * lin[0] + luma[1] * lin[1] + luma[2] * lin[2];
    let shown = super::shape(display, [y; 3])[0].max(0.0).powf(1.0 / 2.2);
    let lms = super::mul3(to_lms, lin).map(|v| v.signum() * v.abs().cbrt());
    let [l, a, b] = super::mul3(&look::LMS_TO_LAB, lms);
    let hue = b.atan2(a).to_degrees().rem_euclid(360.0);
    (shown, hue, a.hypot(b) / l.max(1e-4))
}

/// The local adjustments for one linear Rec.2020 pixel: masks read at
/// source position `(mx, my)` (before lens corrections, where they were
/// drawn), the global maps at `(sx, sy)`.
/// `detail`: the pixel less the mean of its four neighbours (when
/// `ls.detail`; else zeros).
#[allow(clippy::too_many_arguments)]
pub fn layers_pixel(
    ls: &Layers,
    local: &maps::Local,
    p: [f32; 3],
    detail: [f32; 3],
    mx: f32,
    my: f32,
    sx: f32,
    sy: f32,
) -> [f32; 3] {
    let Some(masks) = &ls.masks else {
        return p;
    };
    let (gx, gy) = (
        (mx + 0.5) * ls.scale[0] - 0.5,
        (my + 0.5) * ls.scale[1] - 0.5,
    );
    let mut p = p;
    for (i, l) in ls.list.iter().enumerate() {
        if !l.active {
            continue;
        }
        let w = mask::sample(masks, i, gx, gy);
        if w <= 0.0 {
            continue;
        }
        let q = adjust(l, local, p, detail, sx, sy);
        p = [0, 1, 2].map(|c| p[c] + (q[c] - p[c]) * w);
    }
    p
}

/// One layer's sliders on a pixel.
fn adjust(
    l: &Layer,
    local: &maps::Local,
    p: [f32; 3],
    detail: [f32; 3],
    sx: f32,
    sy: f32,
) -> [f32; 3] {
    let m = local
        .maps
        .as_ref()
        .map(|maps| (maps, maps::at_source(local, maps, sx, sy)));
    // Sharpness first: at −1 the pixel is averaged with its neighbours'
    // mean.
    let mut q = if l.sharpness != 0.0 {
        let k = if l.sharpness > 0.0 { 1.5 } else { 0.5 } * l.sharpness;
        [0, 1, 2].map(|c| (p[c] + k * detail[c]).max(0.0))
    } else {
        p
    };
    if l.dehaze != 0.0
        && let Some((maps, m)) = &m
    {
        q = maps::dehaze_at(maps, m, q, local.luma, l.dehaze);
    }
    q = super::mul3(&l.matrix, q);
    let s = maps::stops(local.luma[0] * q[0] + local.luma[1] * q[1] + local.luma[2] * q[2]);
    let mut gain = s.clamp(-4.0, 4.0) * 0.35 * l.contrast;
    if let Some((_, m)) = &m {
        gain += maps::tone_gain(m, s, l.shadows, l.highlights, l.clarity);
    }
    if gain == 0.0 {
        return q;
    }
    let k = gain.exp2();
    q.map(|v| v * k)
}

/// Layer `k`'s mask shown over display values `d` at source position
/// `(mx, my)`: tinted red, or (`only`) the mask alone in grey.
pub fn mask_shown(ls: &Layers, k: usize, only: bool, d: [f32; 3], mx: f32, my: f32) -> [f32; 3] {
    let Some(masks) = ls.masks.as_ref().filter(|m| k < m.count) else {
        return d;
    };
    let w = mask::sample(
        masks,
        k,
        (mx + 0.5) * ls.scale[0] - 0.5,
        (my + 0.5) * ls.scale[1] - 0.5,
    );
    if only {
        return [w; 3];
    }
    let t = 0.5 * w;
    [0, 1, 2].map(|c| d[c] + (MASK_TINT[c] - d[c]) * t)
}

/// The mask overlay's colour.
pub const MASK_TINT: [f32; 3] = [1.0, 0.0, 0.0];
