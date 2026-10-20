//! Camera match profiles (EDITING-SPEC, Colour science, level 2): a look
//! fitted to the camera's own JPEGs, so a raw can look like the camera's
//! Picture Control rendering, offered beside the neutral matrix.
//!
//! The look is a 3D table on the display transform's output (display-linear
//! Rec.2020, 2.2-encoded), applied before the tone curve. It's fitted from
//! pairs of the matrix rendering and the camera JPEG of the same frame
//! ([`Fitter`]): each pair's pixels splat into the table's cells, and cells
//! with little data stay near the identity. `examples/camera_match.rs`
//! fits one from a folder of NEF+JPEG pairs.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

/// Cells along each axis.
pub const SIZE: usize = 17;

/// A 3D lookup table on 2.2-encoded display values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lut3 {
    pub size: usize,
    /// `size³` entries (red fastest), each an output RGB.
    pub data: Vec<[f32; 3]>,
}

impl Lut3 {
    pub fn identity(size: usize) -> Self {
        let mut data = Vec::with_capacity(size * size * size);
        let s = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 / s, g as f32 / s, b as f32 / s]);
                }
            }
        }
        Self { size, data }
    }

    fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[(b * self.size + g) * self.size + r]
    }

    /// The look on one display-linear pixel.
    pub fn apply(&self, p: [f32; 3]) -> [f32; 3] {
        let e = p.map(|v| v.max(0.0).powf(1.0 / 2.2).min(1.0));
        let out = self.sample(e);
        out.map(|v| v.max(0.0).powf(2.2))
    }

    /// Trilinear on encoded values (0–1).
    pub fn sample(&self, e: [f32; 3]) -> [f32; 3] {
        let n = (self.size - 1) as f32;
        let pos = e.map(|v| v.clamp(0.0, 1.0) * n);
        let i = pos.map(|v| (v.floor() as usize).min(self.size - 2));
        let f = [0, 1, 2].map(|k| pos[k] - i[k] as f32);
        let mut out = [0f32; 3];
        for (db, wb) in [(0, 1.0 - f[2]), (1, f[2])] {
            for (dg, wg) in [(0, 1.0 - f[1]), (1, f[1])] {
                for (dr, wr) in [(0, 1.0 - f[0]), (1, f[0])] {
                    let w = wr * wg * wb;
                    let c = self.at(i[0] + dr, i[1] + dg, i[2] + db);
                    for k in 0..3 {
                        out[k] += w * c[k];
                    }
                }
            }
        }
        out
    }
}

/// A camera's profile file: which camera, the look, and how it was fitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// The camera model as its raw files name it ("NIKON Z f").
    pub camera: String,
    pub look: Lut3,
    /// Pairs fitted from, and the held-back pairs' mean ΔE2000 without and
    /// with the look.
    #[serde(default)]
    pub pairs: usize,
    #[serde(default)]
    pub delta_e: Option<(f64, f64)>,
}

fn registry() -> &'static RwLock<HashMap<String, Arc<Lut3>>> {
    static R: OnceLock<RwLock<HashMap<String, Arc<Lut3>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn key(model: &str) -> String {
    model.trim().to_lowercase()
}

/// Make a profile available to renders of its camera's photos.
pub fn register(p: &Profile) {
    if let Ok(mut r) = registry().write() {
        r.insert(key(&p.camera), Arc::new(p.look.clone()));
    }
}

/// The look for a camera model, if a profile was registered.
pub fn find(model: &str) -> Option<Arc<Lut3>> {
    registry().read().ok()?.get(&key(model)).cloned()
}

/// Whether there's a camera match profile for this model.
pub fn has(model: &str) -> bool {
    find(model).is_some()
}

/// Load and register every profile in a folder (`*.json`).
pub fn load_dir(dir: &std::path::Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        match std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Profile>(&t).map_err(|e| e.to_string()))
        {
            Ok(profile)
                if profile.look.size >= 2
                    && profile.look.data.len() == profile.look.size.pow(3) =>
            {
                register(&profile);
                n += 1;
            }
            Ok(_) => tracing::warn!("{}: a malformed table", p.display()),
            Err(e) => tracing::warn!("{}: {e}", p.display()),
        }
    }
    n
}

/// Accumulates pairs of (matrix rendering, camera JPEG) values, both
/// 2.2-encoded display-linear Rec.2020, into a table.
pub struct Fitter {
    size: usize,
    /// Per cell: weighted sum of outputs, and the weight.
    sums: Vec<([f64; 3], f64)>,
}

impl Fitter {
    pub fn new(size: usize) -> Self {
        Self {
            size,
            sums: vec![([0.0; 3], 0.0); size * size * size],
        }
    }

    /// One pixel: its value in our rendering (`from`) and the camera's
    /// (`to`). The output's offset from the input is splatted into the eight
    /// cells around it, so cells can be filled in from their neighbours.
    pub fn add(&mut self, from: [f32; 3], to: [f32; 3]) {
        let n = (self.size - 1) as f32;
        let pos = from.map(|v| v.clamp(0.0, 1.0) * n);
        let i = pos.map(|v| (v.floor() as usize).min(self.size - 2));
        let f = [0, 1, 2].map(|k| pos[k] - i[k] as f32);
        let delta = [0, 1, 2].map(|k| f64::from(to[k] - from[k]));
        for (db, wb) in [(0, 1.0 - f[2]), (1, f[2])] {
            for (dg, wg) in [(0, 1.0 - f[1]), (1, f[1])] {
                for (dr, wr) in [(0, 1.0 - f[0]), (1, f[0])] {
                    let w = f64::from(wr * wg * wb);
                    let c = &mut self.sums
                        [((i[2] + db) * self.size + i[1] + dg) * self.size + i[0] + dr];
                    for (acc, d) in c.0.iter_mut().zip(delta) {
                        *acc += w * d;
                    }
                    c.1 += w;
                }
            }
        }
    }

    /// The table: each cell the identity plus its mean offset, shrunk
    /// towards no offset where there's little data (`prior` is the weight
    /// of that pull), then smoothed with its neighbours.
    pub fn finish(&self, prior: f64) -> Lut3 {
        let s = self.size;
        let mut delta: Vec<[f64; 3]> = self
            .sums
            .iter()
            .map(|(sum, w)| sum.map(|v| v / (w + prior)))
            .collect();
        // Empty cells take their neighbours' offsets (a few passes), so
        // colours no pair covered still follow the camera's shift.
        for _ in 0..4 {
            let prev = delta.clone();
            for b in 0..s {
                for g in 0..s {
                    for r in 0..s {
                        let at = (b * s + g) * s + r;
                        if self.sums[at].1 > 0.5 {
                            continue;
                        }
                        let mut acc = [0.0; 3];
                        let mut n = 0.0;
                        for (dr, dg, db) in [
                            (-1i32, 0, 0),
                            (1, 0, 0),
                            (0, -1, 0),
                            (0, 1, 0),
                            (0, 0, -1),
                            (0, 0, 1),
                        ] {
                            let (rr, gg, bb) = (r as i32 + dr, g as i32 + dg, b as i32 + db);
                            if rr < 0
                                || gg < 0
                                || bb < 0
                                || rr >= s as i32
                                || gg >= s as i32
                                || bb >= s as i32
                            {
                                continue;
                            }
                            let q = prev[(bb as usize * s + gg as usize) * s + rr as usize];
                            for k in 0..3 {
                                acc[k] += q[k];
                            }
                            n += 1.0;
                        }
                        delta[at] = acc.map(|v| v / n * 0.9);
                    }
                }
            }
        }
        let mut lut = Lut3::identity(s);
        for (cell, d) in lut.data.iter_mut().zip(&delta) {
            for k in 0..3 {
                cell[k] = (cell[k] + d[k] as f32).clamp(0.0, 1.0);
            }
        }
        lut
    }
}
