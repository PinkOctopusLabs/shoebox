//! Volume deformation (EDITING-SPEC, Optics and geometry: ViewPoint's
//! wide-angle fix). A rectilinear lens keeps lines straight by stretching
//! what's off-centre: radially by `1/cos θ` against across, θ the angle off
//! the axis, so a face in a corner of a 16 mm frame comes out a third too
//! wide. Two remedies, which combine:
//!
//! - **A global projection** ([`VolumeMode`]): from rectilinear part of the
//!   way (Amount) to cylindrical (across or down: verticals stay vertical,
//!   what's at the sides keeps its shape on the axis) or stereographic
//!   (conformal everywhere). Both keep the centre as it was (same scale
//!   there) and lines through it straight.
//! - **Faces** (Shih, Lai, Liang, "Distortion-Free Wide-Angle Portraits on
//!   Camera Phones", SIGGRAPH 2019, simplified): near each face found, the
//!   stereographic projection, moved and scaled by a similarity so the face
//!   stays where it was and keeps its area; feathered into the rest, which
//!   stays as the global projection left it, so straight lines away from
//!   faces stay straight. Shih et al. solve one mesh for all of it with line
//!   and smoothness terms; here each face's patch is blended by a smooth
//!   weight instead (a gentler feather, no solve), on the lens grid's
//!   nodes, which is the coarse mesh.
//!
//! Where: after lens corrections (the projections are only defined on a
//! rectilinear picture, about its optical centre), before the manual
//! transform's perspective (which moves that centre). It's folded into the
//! lens grid ([`crate::lens`]): each node holds where the lens put the point
//! the deformation brought there, so the live pipe still makes one
//! resample, and the GPU is unchanged. The lens grid's fill zoom keeps the
//! frame filled, as it does for barrel distortion.
//!
//! Coordinates: the frame's pixels (the raw's size for a raw photo), centre
//! at `((w − 1)/2, (h − 1)/2)`, in units of the focal length (in pixels)
//! from there: `x = (p − c)/f`, so `|x| = tan θ`.

use crate::edit::{Volume, VolumeMode};
use crate::lens::Shot;

/// Full frame's diagonal, mm.
const FULL_FRAME_DIAGONAL: f64 = 43.27;
/// When the photo doesn't say: a 24 mm lens on full frame (84° across the
/// diagonal), about the widest a phone or compact shoots.
const FALLBACK_EQUIVALENT: f64 = 24.0;
/// At most this many faces count (an edit is JSON anyone could write).
pub const MAX_FACES: usize = 64;
/// A face's patch is whole within this many face radii of its centre and
/// gone by [`OUTER`].
const INNER: f64 = 1.0;
const OUTER: f64 = 2.5;
/// A face's radius, from its box's longer side: the head, not just the
/// eyes to the chin.
const HEAD: f64 = 0.75;

/// What a frame's deformation is made from, resolved from the edit (part of
/// the lens grid's key).
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub mode: VolumeMode,
    /// 0–1.
    pub amount: f64,
    /// The upright frame's horizontal is the stored image's vertical (the
    /// file's orientation and the edit's turns: 5–8).
    pub transposed: bool,
    /// Faces' boxes, as the edit has them; empty when Faces is off.
    pub faces: Vec<[f64; 4]>,
    /// 0–1.
    pub face_amount: f64,
}

impl Spec {
    /// The deformation `v` asks for, for a frame in this orientation (the
    /// file's and the edit's turns, composed). None: nothing to do.
    pub fn of(v: &Volume, orientation: u16) -> Option<Spec> {
        let amount = match v.mode {
            VolumeMode::Off => 0.0,
            _ if v.amount.is_finite() => (v.amount / 100.0).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let face_amount = if v.face_amount.is_finite() {
            (v.face_amount / 100.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let faces: Vec<[f64; 4]> = if v.faces && face_amount > 0.0 {
            v.boxes
                .iter()
                .filter(|b| b.iter().all(|x| x.is_finite() && x.abs() < 4.0))
                .filter(|b| b[2] > b[0] && b[3] > b[1])
                .take(MAX_FACES)
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        if amount == 0.0 && faces.is_empty() {
            return None;
        }
        Some(Spec {
            mode: if amount > 0.0 {
                v.mode
            } else {
                VolumeMode::Off
            },
            amount,
            transposed: matches!(orientation, 5..=8),
            faces,
            face_amount,
        })
    }
}

/// The focal length in pixels of a `w`×`h` frame of this shot, and the
/// diagonal field of view it means (degrees). From the EXIF focal length
/// and the camera's crop factor (Lensfun's); a camera Lensfun doesn't know
/// is taken as full frame unless the focal length is a small sensor's
/// (under 10 mm), and no focal length as a 24 mm equivalent.
pub fn focal_px(shot: &Shot, w: usize, h: usize) -> (f64, f64) {
    let diag = (w.max(2) as f64).hypot(h.max(2) as f64);
    let focal = f64::from(shot.focal);
    let crop = crate::lens::crop_factor(shot).map(f64::from);
    let equivalent = match crop {
        _ if !(focal.is_finite() && focal > 0.0) => FALLBACK_EQUIVALENT,
        Some(c) if c.is_finite() && c > 0.0 => focal * c,
        _ if focal >= 10.0 => focal,
        _ => FALLBACK_EQUIVALENT,
    };
    // No wider than 150° across the diagonal (a fisheye isn't rectilinear
    // anyway).
    let f = (equivalent / FULL_FRAME_DIAGONAL * diag).max(diag / 2.0 / 75f64.to_radians().tan());
    (f, 2.0 * (diag / 2.0 / f).atan().to_degrees())
}

/// One face's patch.
#[derive(Debug, Clone)]
struct Face {
    /// Its centre and radius (normalised).
    at: [f64; 2],
    radius: f64,
    /// Where the global projection puts its centre.
    to: [f64; 2],
    /// Its centre stereographically.
    stereo: [f64; 2],
    /// Stereographic → the global projection about the centre: the
    /// similarity nearest the global map's own Jacobian there, scaled to
    /// keep its area.
    m: [[f64; 2]; 2],
    weight: f64,
}

/// The deformation of one frame: forward (rectilinear → shown) and back.
#[derive(Debug, Clone)]
pub struct Deform {
    c: [f64; 2],
    f: f64,
    mode: VolumeMode,
    amount: f64,
    /// The cylinder's axis across (0) or down (1) the stored image: the
    /// coordinate that bends.
    along: usize,
    faces: Vec<Face>,
}

/// Stereographic radius for rectilinear `tan θ`: `2 tan(θ/2)`.
fn stereo_r(r: f64) -> f64 {
    2.0 * r / (1.0 + (1.0 + r * r).sqrt())
}

fn stereo(x: [f64; 2]) -> [f64; 2] {
    let r = x[0].hypot(x[1]);
    if r < 1e-12 {
        return x;
    }
    let k = stereo_r(r) / r;
    [x[0] * k, x[1] * k]
}

/// Back from stereographic (`|y|` below 2: in front of the camera).
fn unstereo(y: [f64; 2]) -> [f64; 2] {
    let r2 = (y[0] * y[0] + y[1] * y[1]).min(3.99);
    let k = 1.0 / (1.0 - r2 / 4.0);
    [y[0] * k, y[1] * k]
}

/// `x` in `lo..hi` where increasing `g(x)` reaches `want`, by bisection.
fn solve(g: impl Fn(f64) -> f64, want: f64, mut lo: f64, mut hi: f64) -> f64 {
    for _ in 0..64 {
        let mid = 0.5 * (lo + hi);
        if g(mid) < want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Rectilinear points further out than this (normalised: 84° off the axis)
/// aren't followed: nothing in the frame comes from there.
const FAR: f64 = 10.0;

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Deform {
    /// The deformation for a `w`×`h` frame with focal length `f` (pixels).
    /// `unlens` takes a position in the raw (where a face was found) to the
    /// lens-corrected frame.
    pub fn new(
        spec: &Spec,
        f: f64,
        w: usize,
        h: usize,
        unlens: impl Fn([f64; 2]) -> [f64; 2],
    ) -> Self {
        let (wf, hf) = (w.max(2) as f64, h.max(2) as f64);
        let along = match (spec.mode, spec.transposed) {
            (VolumeMode::Vertical, false) | (VolumeMode::Horizontal, true) => 1,
            _ => 0,
        };
        let mut d = Self {
            c: [(wf - 1.0) / 2.0, (hf - 1.0) / 2.0],
            f: f.max(1.0),
            mode: spec.mode,
            amount: spec.amount,
            along,
            faces: Vec::new(),
        };
        let long = wf.max(hf);
        d.faces = spec
            .faces
            .iter()
            .map(|b| {
                let raw = [
                    (b[0] + b[2]) / 2.0 * long - 0.5,
                    (b[1] + b[3]) / 2.0 * long - 0.5,
                ];
                let at = d.norm(unlens(raw));
                let radius = HEAD * (b[2] - b[0]).max(b[3] - b[1]) * long / d.f;
                d.face(at, radius.max(1e-4), spec.face_amount)
            })
            .collect();
        d
    }

    fn face(&self, at: [f64; 2], radius: f64, weight: f64) -> Face {
        let to = self.global(at);
        let s = stereo(at);
        // The Jacobian of global ∘ unstereo at the face, numerically.
        let e = 1e-5;
        let g = |y: [f64; 2]| self.global(unstereo(y));
        let (gx0, gx1) = (g([s[0] - e, s[1]]), g([s[0] + e, s[1]]));
        let (gy0, gy1) = (g([s[0], s[1] - e]), g([s[0], s[1] + e]));
        let j = [
            [(gx1[0] - gx0[0]) / (2.0 * e), (gy1[0] - gy0[0]) / (2.0 * e)],
            [(gx1[1] - gx0[1]) / (2.0 * e), (gy1[1] - gy0[1]) / (2.0 * e)],
        ];
        let (a, b) = ((j[0][0] + j[1][1]) / 2.0, (j[1][0] - j[0][1]) / 2.0);
        let norm = a.hypot(b).max(1e-12);
        let scale = (j[0][0] * j[1][1] - j[0][1] * j[1][0]).abs().sqrt() / norm;
        Face {
            at,
            radius,
            to,
            stereo: s,
            m: [[a * scale, -b * scale], [b * scale, a * scale]],
            weight,
        }
    }

    fn norm(&self, p: [f64; 2]) -> [f64; 2] {
        [(p[0] - self.c[0]) / self.f, (p[1] - self.c[1]) / self.f]
    }

    fn pixels(&self, x: [f64; 2]) -> [f64; 2] {
        [self.c[0] + x[0] * self.f, self.c[1] + x[1] * self.f]
    }

    /// The global projection (normalised).
    fn global(&self, x: [f64; 2]) -> [f64; 2] {
        let a = self.amount;
        match self.mode {
            VolumeMode::Off => x,
            VolumeMode::Spherical => {
                let r = x[0].hypot(x[1]);
                if r < 1e-12 {
                    return x;
                }
                let k = ((1.0 - a) * r + a * stereo_r(r)) / r;
                [x[0] * k, x[1] * k]
            }
            VolumeMode::Horizontal | VolumeMode::Vertical => {
                let (u, v) = (x[self.along], x[1 - self.along]);
                let phi = u.atan();
                let mut q = [0.0; 2];
                q[self.along] = (1.0 - a) * u + a * phi;
                q[1 - self.along] = v * ((1.0 - a) + a * phi.cos());
                q
            }
        }
    }

    /// Back from the global projection (normalised).
    fn global_inverse(&self, q: [f64; 2]) -> [f64; 2] {
        let a = self.amount;
        match self.mode {
            VolumeMode::Off => q,
            VolumeMode::Spherical => {
                let rq = q[0].hypot(q[1]);
                if rq < 1e-12 {
                    return q;
                }
                // g(r) ≥ r(1 − a) and ≥ the stereographic radius, which
                // bounds r.
                let mut hi = FAR;
                if a < 1.0 {
                    hi = hi.min(rq / (1.0 - a));
                }
                if rq < 2.0 {
                    hi = hi.min(rq / (1.0 - rq * rq / 4.0));
                }
                let g = |r: f64| (1.0 - a) * r + a * stereo_r(r);
                let r = solve(g, rq, rq.min(hi), hi.max(rq));
                [q[0] * r / rq, q[1] * r / rq]
            }
            VolumeMode::Horizontal | VolumeMode::Vertical => {
                let (uq, vq) = (q[self.along], q[1 - self.along]);
                let mut hi = FAR;
                if a < 1.0 {
                    hi = hi.min(uq.abs() / (1.0 - a));
                }
                if uq.abs() < std::f64::consts::FRAC_PI_2 {
                    hi = hi.min(uq.abs().tan());
                }
                let g = |u: f64| (1.0 - a) * u + a * u.atan();
                let u = solve(g, uq.abs(), uq.abs().min(hi), hi.max(uq.abs())).copysign(uq);
                let k = (1.0 - a) + a * u.atan().cos();
                let mut x = [0.0; 2];
                x[self.along] = u;
                x[1 - self.along] = vq / k.max(1e-6);
                x
            }
        }
    }

    /// Each face's weight at `x`, normalised so they sum to at most 1.
    fn weights(&self, x: [f64; 2]) -> Vec<f64> {
        let mut w: Vec<f64> = self
            .faces
            .iter()
            .map(|fc| {
                let d = (x[0] - fc.at[0]).hypot(x[1] - fc.at[1]) / fc.radius;
                fc.weight * (1.0 - smoothstep(INNER, OUTER, d))
            })
            .collect();
        let sum: f64 = w.iter().sum();
        if sum > 1.0 {
            for v in &mut w {
                *v /= sum;
            }
        }
        w
    }

    /// Rectilinear → shown (normalised).
    fn forward_n(&self, x: [f64; 2]) -> [f64; 2] {
        let g = self.global(x);
        if self.faces.is_empty() {
            return g;
        }
        let s = stereo(x);
        let mut q = g;
        for (fc, w) in self.faces.iter().zip(self.weights(x)) {
            if w <= 0.0 {
                continue;
            }
            let d = [s[0] - fc.stereo[0], s[1] - fc.stereo[1]];
            let t = [
                fc.to[0] + fc.m[0][0] * d[0] + fc.m[0][1] * d[1],
                fc.to[1] + fc.m[1][0] * d[0] + fc.m[1][1] * d[1],
            ];
            q[0] += w * (t[0] - g[0]);
            q[1] += w * (t[1] - g[1]);
        }
        q
    }

    /// Where the lens-corrected (rectilinear) frame's point `p` (pixels) is
    /// shown.
    pub fn forward(&self, p: [f64; 2]) -> [f64; 2] {
        self.pixels(self.forward_n(self.norm(p)))
    }

    /// The lens-corrected point shown at `q` (pixels): what the grid's
    /// nodes need.
    pub fn inverse(&self, q: [f64; 2]) -> [f64; 2] {
        let qn = self.norm(q);
        let mut x = self.global_inverse(qn);
        // Faces near: Newton on the whole map from there.
        let near = self
            .faces
            .iter()
            .any(|fc| (x[0] - fc.at[0]).hypot(x[1] - fc.at[1]) < (OUTER + 1.0) * fc.radius);
        if near {
            let e = 1e-6;
            for _ in 0..30 {
                let fx = self.forward_n(x);
                let r = [fx[0] - qn[0], fx[1] - qn[1]];
                if r[0].hypot(r[1]) < 1e-12 {
                    break;
                }
                let (a0, a1) = (
                    self.forward_n([x[0] + e, x[1]]),
                    self.forward_n([x[0], x[1] + e]),
                );
                let j = [
                    [(a0[0] - fx[0]) / e, (a1[0] - fx[0]) / e],
                    [(a0[1] - fx[1]) / e, (a1[1] - fx[1]) / e],
                ];
                let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
                if det.abs() < 1e-9 {
                    break;
                }
                let step = [
                    (j[1][1] * r[0] - j[0][1] * r[1]) / det,
                    (-j[1][0] * r[0] + j[0][0] * r[1]) / det,
                ];
                x = [x[0] - step[0], x[1] - step[1]];
            }
        }
        let x = [x[0].clamp(-FAR, FAR), x[1].clamp(-FAR, FAR)];
        self.pixels(x)
    }

    /// Faces are being made round.
    pub fn has_faces(&self) -> bool {
        !self.faces.is_empty()
    }
}
