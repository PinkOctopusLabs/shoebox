//! Lens corrections (EDITING-SPEC, Optics; module 1): distortion,
//! vignetting and transverse chromatic aberration, folded into the live
//! pipe's one resample.
//!
//! - **Where the numbers come from.** A Nikon Z body writes its lens's
//!   distortion and vignetting into the NEF (`shoebox_decode::nef::LensData`);
//!   those come first. Lensfun (the `lensfun` crate and its bundled database,
//!   LGPL-3.0 / CC-BY-SA 3.0) fills in the rest: chromatic aberration, and
//!   distortion and vignetting for lenses the camera says nothing about.
//! - **How they're applied.** On a grid over the frame ([`Grid`]): at each
//!   node, where red, green and blue come from in the raw (as fractions of
//!   the frame) and the vignetting gain. The live pass interpolates the grid
//!   per pixel. Lensfun's own scalar passes compute the nodes, so its models
//!   needn't be ported; fractions make one grid serve the proxy, 100%
//!   regions and export.
//! - A zoom keeps the corrected frame filled (no blank corners), folded into
//!   the geometry map.

use std::sync::{Arc, Mutex, OnceLock};

use crate::edit::LensCorrection;

/// What a photo says about how it was taken (from its EXIF, and a NEF's
/// own lens data).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shot {
    pub make: String,
    pub model: String,
    pub lens: String,
    /// Millimetres, and f-number.
    pub focal: f32,
    pub aperture: f32,
    pub camera_data: shoebox_decode::nef::LensData,
}

/// Floats per grid node: red, green, blue source offsets (x, y as fractions
/// of the frame) and the vignetting gain.
pub const GRID_STRIDE: usize = 7;

/// Nodes across the long edge.
const GRID_LONG: usize = 65;

#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub data: Vec<f32>,
    /// Zoom about the centre that keeps the corrected frame filled.
    pub zoom: f32,
    /// Red and blue sampled apart from green.
    pub chromatic: bool,
    /// What it was made from, for the panel.
    pub source: String,
}

impl Grid {
    /// The node values at grid position `(gx, gy)` (bilinear, clamped).
    pub fn at(&self, gx: f32, gy: f32) -> [f32; GRID_STRIDE] {
        let gx = gx.clamp(0.0, (self.cols - 1) as f32);
        let gy = gy.clamp(0.0, (self.rows - 1) as f32);
        let (x0, y0) = (gx.floor() as usize, gy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.cols - 1), (y0 + 1).min(self.rows - 1));
        let (fx, fy) = (gx - x0 as f32, gy - y0 as f32);
        let node =
            |x: usize, y: usize| &self.data[(y * self.cols + x) * GRID_STRIDE..][..GRID_STRIDE];
        let (a, b, c, d) = (node(x0, y0), node(x1, y0), node(x0, y1), node(x1, y1));
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            top + (bot - top) * fy
        })
    }
}

/// The bundled Lensfun database, loaded once.
pub fn lensfun() -> Option<&'static lensfun::Database> {
    static DB: OnceLock<Option<lensfun::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        lensfun::Database::load_bundled()
            .map_err(|e| tracing::warn!("Lensfun database: {e}"))
            .ok()
    })
    .as_ref()
}

fn camera<'a>(db: &'a lensfun::Database, shot: &Shot) -> Option<&'a lensfun::Camera> {
    db.find_cameras(Some(&shot.make), &shot.model)
        .into_iter()
        .next()
}

/// The Lensfun lens for this shot: the one chosen by hand, else the best
/// match for its EXIF lens name.
pub fn find<'a>(
    db: &'a lensfun::Database,
    shot: &Shot,
    chosen: Option<&str>,
) -> Option<&'a lensfun::Lens> {
    if let Some(name) = chosen {
        return db.lenses.iter().find(|l| l.model == name);
    }
    if shot.lens.is_empty() {
        return None;
    }
    db.find_lenses(camera(db, shot), &shot.lens)
        .into_iter()
        .next()
}

/// Lensfun's lenses that fit this shot's camera, by name (for choosing one
/// by hand).
pub fn lenses_for(shot: &Shot) -> Vec<String> {
    let Some(db) = lensfun() else {
        return Vec::new();
    };
    let mut names: Vec<String> = db
        .find_lenses(camera(db, shot), "")
        .into_iter()
        .map(|l| l.model.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The grid for this shot and these settings (None: nothing to correct).
/// `width`×`height`: the frame's size (its aspect and the normalisation).
/// Kept, so a slider elsewhere doesn't recompute it.
pub fn grid(shot: &Shot, lc: &LensCorrection, width: usize, height: usize) -> Option<Arc<Grid>> {
    if !lc.enabled {
        return None;
    }
    type Key = (Shot, LensCorrection, usize, usize);
    type Kept = Mutex<Vec<(Key, Option<Arc<Grid>>)>>;
    static KEPT: OnceLock<Kept> = OnceLock::new();
    let key = (shot.clone(), lc.clone(), width, height);
    let kept = KEPT.get_or_init(Default::default);
    if let Some((_, g)) = kept
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
    {
        return g.clone();
    }
    let g = make(shot, lc, width, height).map(Arc::new);
    let mut list = kept.lock().unwrap_or_else(|e| e.into_inner());
    list.push((key, g.clone()));
    if list.len() > 8 {
        list.remove(0);
    }
    g
}

fn make(shot: &Shot, lc: &LensCorrection, width: usize, height: usize) -> Option<Grid> {
    let (w, h) = (width.max(2) as f32, height.max(2) as f32);
    let db = lensfun();
    let lens = db.and_then(|db| find(db, shot, lc.lens.as_deref()));
    let modifier = lens.map(|l| {
        let crop = db
            .and_then(|db| camera(db, shot))
            .map_or(1.0, |c| c.crop_factor);
        let mut m = lensfun::Modifier::new(l, shot.focal, crop, width as u32, height as u32, true);
        let (d, t, v) = (
            m.enable_distortion_correction(l),
            lc.chromatic && m.enable_tca_correction(l),
            m.enable_vignetting_correction(l, shot.aperture, 1000.0),
        );
        (m, d, t, v)
    });
    let cam = &shot.camera_data;
    let by_camera = (cam.distortion, cam.vignette);
    let distortion = by_camera.0.is_some() || modifier.as_ref().is_some_and(|m| m.1);
    let chromatic = modifier.as_ref().is_some_and(|m| m.2);
    let vignetting = by_camera.1.is_some() || modifier.as_ref().is_some_and(|m| m.3);
    if !distortion && !chromatic && !vignetting {
        return None;
    }
    let mut parts = Vec::new();
    if by_camera.0.is_some() || by_camera.1.is_some() {
        parts.push("camera".to_owned());
    }
    if let Some(l) = lens {
        parts.push(format!("Lensfun: {}", l.model));
    }
    let (cx, cy) = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
    let half_diag = cx.hypot(cy);
    let amount_d = (lc.distortion / 100.0) as f32;
    let amount_v = (lc.vignetting / 100.0) as f32;
    let (cols, rows) = if w >= h {
        (
            GRID_LONG,
            ((GRID_LONG - 1) as f32 * h / w).round() as usize + 1,
        )
    } else {
        (
            ((GRID_LONG - 1) as f32 * w / h).round() as usize + 1,
            GRID_LONG,
        )
    };
    let mut data = Vec::with_capacity(cols * rows * GRID_STRIDE);
    for j in 0..rows {
        for i in 0..cols {
            let px = i as f32 / (cols - 1) as f32 * (w - 1.0);
            let py = j as f32 / (rows - 1) as f32 * (h - 1.0);
            // Where green comes from: the camera's model first, else Lensfun.
            let (mut qx, mut qy) = (px, py);
            if let Some(k) = by_camera.0 {
                let r2 = ((px - cx).hypot(py - cy) / half_diag).powi(2);
                let f = 1.0 + r2 * (k[0] as f32 + r2 * (k[1] as f32 + r2 * k[2] as f32));
                (qx, qy) = (cx + (px - cx) * f, cy + (py - cy) * f);
            } else if let Some((m, true, _, _)) = &modifier {
                let mut c = [0f32; 2];
                m.apply_geometry_distortion(px, py, 1, 1, &mut c);
                (qx, qy) = (c[0], c[1]);
            }
            let (gx, gy) = (px + amount_d * (qx - px), py + amount_d * (qy - py));
            // Red and blue relative to green (Lensfun's TCA, at full strength).
            let (mut dr, mut db_) = ((0.0, 0.0), (0.0, 0.0));
            if let Some((m, _, true, _)) = &modifier {
                let mut c = [0f32; 6];
                m.apply_subpixel_distortion(px, py, 1, 1, &mut c);
                dr = (c[0] - c[2], c[1] - c[3]);
                db_ = (c[4] - c[2], c[5] - c[3]);
            }
            // Vignetting, where the light fell on the sensor.
            let gain = if let Some(v) = by_camera.1 {
                let r2 = ((gx - cx).hypot(gy - cy) / half_diag).powi(2);
                1.0 + r2 * (v[0] as f32 + r2 * (v[1] as f32 + r2 * v[2] as f32))
            } else if let Some((m, _, _, true)) = &modifier {
                // With `reverse` set, Lensfun's colour pass *simulates* the
                // falloff (the opposite of its geometry passes): the
                // correction is its reciprocal.
                let mut p = [1.0f32];
                m.apply_color_modification_f32(&mut p, gx, gy, 1, 1, 1);
                1.0 / p[0].max(1e-3)
            } else {
                1.0
            };
            let gain = gain.max(1e-3).powf(amount_v);
            data.extend([
                (gx + dr.0 - px) / w,
                (gy + dr.1 - py) / h,
                (gx - px) / w,
                (gy - py) / h,
                (gx + db_.0 - px) / w,
                (gy + db_.1 - py) / h,
                gain,
            ]);
        }
    }
    let mut grid = Grid {
        cols,
        rows,
        data,
        zoom: 1.0,
        chromatic,
        source: parts.join(" + "),
    };
    if distortion || chromatic {
        grid.zoom = fill_zoom(&grid, w, h);
    }
    Some(grid)
}

/// The zoom (about the centre) at which every edge point of the corrected
/// frame still comes from inside the raw: as far out as possible without
/// blank corners.
fn fill_zoom(g: &Grid, w: f32, h: f32) -> f32 {
    let (cx, cy) = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
    let edge: Vec<(f32, f32)> = (0..=64)
        .flat_map(|k| {
            let t = k as f32 / 64.0;
            [
                (t * (w - 1.0), 0.0),
                (t * (w - 1.0), h - 1.0),
                (0.0, t * (h - 1.0)),
                (w - 1.0, t * (h - 1.0)),
            ]
        })
        .collect();
    let inside = |zoom: f32| {
        edge.iter().all(|&(px, py)| {
            let (ux, uy) = (cx + (px - cx) / zoom, cy + (py - cy) / zoom);
            let n = g.at(
                ux / (w - 1.0) * (g.cols - 1) as f32,
                uy / (h - 1.0) * (g.rows - 1) as f32,
            );
            [0, 2, 4].iter().all(|&k| {
                let (sx, sy) = (ux + n[k] * w, uy + n[k + 1] * h);
                sx >= -0.5 && sy >= -0.5 && sx <= w - 0.5 && sy <= h - 0.5
            })
        })
    };
    let (mut lo, mut hi) = (0.5f32, 2.0f32);
    if !inside(hi) {
        return 1.0;
    }
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if inside(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}
