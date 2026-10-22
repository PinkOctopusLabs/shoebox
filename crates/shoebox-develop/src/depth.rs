//! Depth maps for Lens Blur (EDITING-SPEC, Phase 4 additions): relative
//! inverse depth, 0 farthest to 1 nearest, per pixel of the masks' grid,
//! from a monocular depth model the app registers (Depth Anything V2
//! Small, `shoebox_ml::depth`).
//!
//! As with the AI masks, the model is shown the picture as the frame shows
//! it on the masks' grid, upright; what it returns (on its own coarser
//! grid) is normalised, kept on disk by [`crate::mask::store`] (a 16-bit
//! PNG keyed by the exact picture and the model's identity), then
//! upsampled to the grid by the colour guided filter `mask::fine` uses,
//! so the blur's edges follow the picture's, and turned back to the
//! stored image's orientation. Found once per source and orientation: on
//! a thread of its own for the live view (which redraws when it lands),
//! otherwise inline.

use std::sync::{Arc, OnceLock};

use crate::cpu::{Demosaiced, Display, Rgb, maps};
use crate::mask::{self, Coarse};

/// Finds relative inverse depth (larger nearer, any scale) in an upright
/// 8-bit sRGB image (`rgb`, width, height), on the model's own grid.
pub type DepthFinder = dyn Fn(&[u8], usize, usize) -> Result<Coarse, String> + Send + Sync;

struct Registered {
    id: String,
    run: Box<DepthFinder>,
}

static FINDER: OnceLock<Registered> = OnceLock::new();

/// Give Lens Blur a depth model (the app registers Depth Anything V2),
/// named `id` (the model's identity: maps are kept on disk by it; empty
/// keeps nothing). Once.
pub fn register(id: &str, f: Box<DepthFinder>) {
    let _ = FINDER.set(Registered {
        id: id.to_owned(),
        run: f,
    });
}

/// Whether a depth model has been registered.
pub fn registered() -> bool {
    FINDER.get().is_some()
}

/// The share of the map below and above which depths are clipped when
/// normalising (so a stray pixel doesn't set the range).
const CLIP: f64 = 0.005;

/// The model's output scaled to 0–1: its 0.5th percentile to 0, its
/// 99.5th to 1.
pub fn normalise(c: Coarse) -> Coarse {
    let mut sorted: Vec<f32> = c.data.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return Coarse {
            data: vec![0.0; c.data.len()],
            ..c
        };
    }
    sorted.sort_by(f32::total_cmp);
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    let (lo, hi) = (at(CLIP), at(1.0 - CLIP));
    let span = (hi - lo).max(1e-6);
    Coarse {
        data: c
            .data
            .iter()
            .map(|v| {
                if v.is_finite() {
                    ((v - lo) / span).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            })
            .collect(),
        ..c
    }
}

/// The depth of a picture (upright 8-bit sRGB, `w`×`h`) at its size,
/// 0–1: found by the registered model (or read from the store),
/// normalised and upsampled along the picture.
pub fn find(rgb: &[u8], w: usize, h: usize) -> Result<Vec<f32>, String> {
    let f = FINDER.get().ok_or("no depth model in this build")?;
    let coarse = mask::store::through_values(&f.id, "depth", rgb, w, h, || {
        let c = (f.run)(rgb, w, h)?;
        if c.width == 0 || c.height == 0 || c.data.len() != c.width * c.height {
            return Err("the depth map came back the wrong size".into());
        }
        Ok(normalise(c))
    })?;
    mask::guided(rgb, w, h, &coarse)
}

/// `src`'s depth map as the frame shows it (`colour`, `display`; turned
/// upright by `orientation` for the model), on the masks' grid in the
/// stored image's orientation, once found. None while it's being found
/// (the live view's search runs on its own thread and redraws when done)
/// or when it couldn't be (see `maps::Cache::depthing`).
pub fn map(
    src: &Demosaiced,
    colour: &[[f32; 3]; 3],
    display: &Display,
    orientation: u16,
) -> Option<Arc<Vec<f32>>> {
    let (gw, gh) = mask::grid(src.rgb.width, src.rgb.height);
    let key = (orientation, gw, gh);
    match src.cache.depth(&key) {
        Some(maps::Depthed::Done(m)) => return Some(m),
        Some(_) => return None,
        None => {}
    }
    if !registered() {
        return None;
    }
    let (bytes, uw, uh) =
        crate::cpu::layers::shown_upright(src, colour, display, orientation, gw, gh);
    let work = move || -> maps::Depthed {
        match find(&bytes, uw, uh) {
            Ok(d) if d.len() == uw * uh => {
                // Back to the stored image's orientation.
                let mut img = Rgb::new(uw, uh);
                for (i, v) in d.iter().enumerate() {
                    img.data[i * 3] = *v;
                }
                let back =
                    crate::cpu::orient(&img, crate::cpu::layers::undo_orientation(orientation));
                maps::Depthed::Done(Arc::new(
                    back.data.as_chunks::<3>().0.iter().map(|p| p[0]).collect(),
                ))
            }
            Ok(_) => maps::Depthed::Failed("the depth map came back the wrong size".into()),
            Err(e) => maps::Depthed::Failed(e),
        }
    };
    if src.cache.background() {
        src.cache.set_depth(key, maps::Depthed::Pending);
        let cache = src.cache.clone();
        std::thread::spawn(move || cache.set_depth(key, work()));
        return None;
    }
    let d = work();
    src.cache.set_depth(key, d.clone());
    match d {
        maps::Depthed::Done(m) => Some(m),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalising_clips_strays() {
        let mut data: Vec<f32> = (0..1000).map(|i| i as f32 / 999.0 * 4.0 + 2.0).collect();
        data[3] = 1e6;
        data[7] = f32::NAN;
        let n = normalise(Coarse {
            width: 1000,
            height: 1,
            data,
        });
        assert!(n.data.iter().all(|v| (0.0..=1.0).contains(v)));
        assert_eq!(n.data[7], 0.0);
        assert!(n.data[0] < 0.01 && n.data[999] > 0.99);
        assert!((n.data[500] - 0.5).abs() < 0.02);
    }
}
