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
//! - Volume deformation ([`crate::volume`]) is folded into the same grid:
//!   a node holds where the lens put the point the deformation shows there,
//!   so it costs the live pipe nothing more.

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
/// With faces made round (see [`crate::volume`]).
const GRID_LONG_FACES: usize = 129;

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
    grid_with(shot, lc, None, width, height)
}

/// [`grid`] with volume deformation folded in (see [`crate::volume`]): each
/// node holds where the lens put the point the deformation shows there.
pub fn grid_with(
    shot: &Shot,
    lc: &LensCorrection,
    volume: Option<&crate::volume::Spec>,
    width: usize,
    height: usize,
) -> Option<Arc<Grid>> {
    if !lc.enabled && volume.is_none() {
        return None;
    }
    type Key = (
        Shot,
        LensCorrection,
        Option<crate::volume::Spec>,
        usize,
        usize,
    );
    type Kept = Mutex<Vec<(Key, Option<Arc<Grid>>)>>;
    static KEPT: OnceLock<Kept> = OnceLock::new();
    let key = (shot.clone(), lc.clone(), volume.cloned(), width, height);
    let kept = KEPT.get_or_init(Default::default);
    if let Some((_, g)) = kept
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
    {
        return g.clone();
    }
    let g = make(shot, lc, volume, width, height).map(Arc::new);
    let mut list = kept.lock().unwrap_or_else(|e| e.into_inner());
    list.push((key, g.clone()));
    if list.len() > 8 {
        list.remove(0);
    }
    g
}

/// The camera's crop factor, when Lensfun knows the camera.
pub fn crop_factor(shot: &Shot) -> Option<f32> {
    if shot.model.is_empty() {
        return None;
    }
    let db = lensfun()?;
    camera(db, shot).map(|c| c.crop_factor)
}

/// The lens's corrections at any point of the frame.
struct Model {
    modifier: Option<(lensfun::Modifier, bool, bool, bool)>,
    by_camera: (Option<[f64; 3]>, Option<[f64; 3]>),
    centre: (f32, f32),
    half_diag: f32,
    amount_d: f32,
    amount_v: f32,
    distortion: bool,
    chromatic: bool,
    source: String,
}

impl Model {
    /// Where red, green and blue come from for the corrected point
    /// `(px, py)`, and the vignetting gain: `[rx, ry, gx, gy, bx, by, gain]`
    /// in the frame's pixels.
    fn at(&self, px: f32, py: f32) -> [f32; GRID_STRIDE] {
        let (cx, cy) = self.centre;
        let half_diag = self.half_diag;
        // Where green comes from: the camera's model first, else Lensfun.
        let (mut qx, mut qy) = (px, py);
        if let Some(k) = self.by_camera.0 {
            let r2 = ((px - cx).hypot(py - cy) / half_diag).powi(2);
            let f = 1.0 + r2 * (k[0] as f32 + r2 * (k[1] as f32 + r2 * k[2] as f32));
            (qx, qy) = (cx + (px - cx) * f, cy + (py - cy) * f);
        } else if let Some((m, true, _, _)) = &self.modifier {
            let mut c = [0f32; 2];
            m.apply_geometry_distortion(px, py, 1, 1, &mut c);
            (qx, qy) = (c[0], c[1]);
        }
        let amount_d = self.amount_d;
        let (gx, gy) = (px + amount_d * (qx - px), py + amount_d * (qy - py));
        // Red and blue relative to green (Lensfun's TCA, at full strength).
        let (mut dr, mut db_) = ((0.0, 0.0), (0.0, 0.0));
        if let Some((m, _, true, _)) = &self.modifier {
            let mut c = [0f32; 6];
            m.apply_subpixel_distortion(px, py, 1, 1, &mut c);
            dr = (c[0] - c[2], c[1] - c[3]);
            db_ = (c[4] - c[2], c[5] - c[3]);
        }
        // Vignetting, where the light fell on the sensor.
        let gain = if let Some(v) = self.by_camera.1 {
            let r2 = ((gx - cx).hypot(gy - cy) / half_diag).powi(2);
            1.0 + r2 * (v[0] as f32 + r2 * (v[1] as f32 + r2 * v[2] as f32))
        } else if let Some((m, _, _, true)) = &self.modifier {
            // With `reverse` set, Lensfun's colour pass *simulates* the
            // falloff (the opposite of its geometry passes): the
            // correction is its reciprocal.
            let mut p = [1.0f32];
            m.apply_color_modification_f32(&mut p, gx, gy, 1, 1, 1);
            1.0 / p[0].max(1e-3)
        } else {
            1.0
        };
        let gain = gain.max(1e-3).powf(self.amount_v);
        [gx + dr.0, gy + dr.1, gx, gy, gx + db_.0, gy + db_.1, gain]
    }

    /// The corrected point whose green comes from raw point `p` (by fixed
    /// point iteration: the corrections move a point little).
    fn unlens(&self, p: [f64; 2]) -> [f64; 2] {
        if !self.distortion {
            return p;
        }
        let mut q = p;
        for _ in 0..30 {
            let n = self.at(q[0] as f32, q[1] as f32);
            let e = [p[0] - f64::from(n[2]), p[1] - f64::from(n[3])];
            q = [q[0] + e[0], q[1] + e[1]];
            if e[0].hypot(e[1]) < 1e-3 {
                break;
            }
        }
        q
    }
}

fn model(shot: &Shot, lc: &LensCorrection, width: usize, height: usize) -> Option<Model> {
    if !lc.enabled {
        return None;
    }
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
    Some(Model {
        modifier,
        by_camera,
        centre: (cx, cy),
        half_diag: cx.hypot(cy),
        amount_d: (lc.distortion / 100.0) as f32,
        amount_v: (lc.vignetting / 100.0) as f32,
        distortion,
        chromatic,
        source: parts.join(" + "),
    })
}

fn make(
    shot: &Shot,
    lc: &LensCorrection,
    volume: Option<&crate::volume::Spec>,
    width: usize,
    height: usize,
) -> Option<Grid> {
    let (w, h) = (width.max(2) as f32, height.max(2) as f32);
    let lens = model(shot, lc, width, height);
    let deform = volume.map(|spec| {
        let (f, _) = crate::volume::focal_px(shot, width, height);
        crate::volume::Deform::new(spec, f, width, height, |p| {
            lens.as_ref().map_or(p, |l| l.unlens(p))
        })
    });
    if lens.is_none() && deform.is_none() {
        return None;
    }
    // Faces bend within a few faces' widths: a finer mesh for them.
    let long = if deform.as_ref().is_some_and(|d| d.has_faces()) {
        GRID_LONG_FACES
    } else {
        GRID_LONG
    };
    let (cols, rows) = if w >= h {
        (long, ((long - 1) as f32 * h / w).round() as usize + 1)
    } else {
        (((long - 1) as f32 * w / h).round() as usize + 1, long)
    };
    let mut data = Vec::with_capacity(cols * rows * GRID_STRIDE);
    for j in 0..rows {
        for i in 0..cols {
            let px = i as f32 / (cols - 1) as f32 * (w - 1.0);
            let py = j as f32 / (rows - 1) as f32 * (h - 1.0);
            // The lens-corrected point shown here.
            let (rx, ry) = match &deform {
                Some(d) => {
                    let r = d.inverse([f64::from(px), f64::from(py)]);
                    (r[0] as f32, r[1] as f32)
                }
                None => (px, py),
            };
            let n = match &lens {
                Some(l) => l.at(rx, ry),
                None => [rx, ry, rx, ry, rx, ry, 1.0],
            };
            data.extend([
                (n[0] - px) / w,
                (n[1] - py) / h,
                (n[2] - px) / w,
                (n[3] - py) / h,
                (n[4] - px) / w,
                (n[5] - py) / h,
                n[6],
            ]);
        }
    }
    let mut source = lens.as_ref().map(|l| l.source.clone()).unwrap_or_default();
    if deform.is_some() {
        if !source.is_empty() {
            source.push_str(" + ");
        }
        source.push_str("volume deformation");
    }
    let mut grid = Grid {
        cols,
        rows,
        data,
        zoom: 1.0,
        chromatic: lens.as_ref().is_some_and(|l| l.chromatic),
        source,
    };
    // The deformation draws the edges in further than any lens: more room.
    if deform.is_some() {
        grid.zoom = fill_zoom(&grid, w, h, 4.0);
    } else if lens.as_ref().is_some_and(|l| l.distortion || l.chromatic) {
        grid.zoom = fill_zoom(&grid, w, h, 2.0);
    }
    Some(grid)
}

/// The zoom (about the centre) at which every edge point of the corrected
/// frame still comes from inside the raw: as far out as possible without
/// blank corners.
fn fill_zoom(g: &Grid, w: f32, h: f32, most: f32) -> f32 {
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
    let (mut lo, mut hi) = (0.5f32, most);
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
