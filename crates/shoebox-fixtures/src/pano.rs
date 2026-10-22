//! Synthetic panoramas: a textured scene all round a camera, seen by
//! rectilinear frames turned to known rotations, written as raw DNGs.
//! Tests pass each frame's optics (where a raw pixel looks, how much light
//! its lens lets through), so lens distortion and vignetting are theirs to
//! choose.

use super::{Dng, Rng, Shot, value_noise};

/// A direction: x right, y down, z forward.
pub type Dir = [f64; 3];

/// A rotation (world → camera), row by row.
pub type Rotation = [[f64; 3]; 3];

/// The rotation of a camera turned `yaw` degrees right, then `pitch` up,
/// then `roll` clockwise about its axis (world → camera).
pub fn rotation(yaw: f64, pitch: f64, roll: f64) -> Rotation {
    let (sy, cy) = yaw.to_radians().sin_cos();
    let (sp, cp) = pitch.to_radians().sin_cos();
    let (sr, cr) = roll.to_radians().sin_cos();
    // Camera → world: yaw about y, then pitch about x (up is −y), then
    // roll about z.
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rx = [[1.0, 0.0, 0.0], [0.0, cp, sp], [0.0, -sp, cp]];
    let rz = [[cr, -sr, 0.0], [sr, cr, 0.0], [0.0, 0.0, 1.0]];
    let c2w = mul(&ry, &mul(&rx, &rz));
    // World → camera is its transpose.
    std::array::from_fn(|i| std::array::from_fn(|j| c2w[j][i]))
}

fn mul(a: &Rotation, b: &Rotation) -> Rotation {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

/// Longitude (right of forward) and latitude (up), radians.
pub fn lon_lat(d: Dir) -> (f64, f64) {
    let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-12);
    (d[0].atan2(d[2]), (-d[1] / n).clamp(-1.0, 1.0).asin())
}

/// The scene's light in direction `d`, relative to the sensor's white:
/// a sky brightening upwards over textured land (0.04–0.6), with detail
/// at a few scales so every frame has corners to match.
pub fn pano_light(d: Dir) -> f64 {
    let (lon, lat) = lon_lat(d);
    let (x, y) = (lon.to_degrees() * 10.0, -lat.to_degrees() * 10.0);
    let fine = 0.5 * value_noise(x, y, 3.5, 21) + 0.3 * value_noise(x, y, 8.0, 22);
    let coarse = value_noise(x, y, 25.0, 23);
    let sky = (lat.to_degrees() / 30.0).clamp(-1.0, 1.0);
    0.06 + 0.12 * (1.0 + sky) + 0.25 * fine * (0.4 + 0.6 * coarse)
}

/// Where a pixel of a `width`×`height` frame with focal length `focal`
/// (pixels) and rotation `r` looks: `(x, y)` in the ideal (corrected)
/// frame's pixels, centred on the frame.
pub fn ray(r: &Rotation, focal: f64, width: u32, height: u32, x: f64, y: f64) -> Dir {
    let (cx, cy) = (
        (f64::from(width) - 1.0) / 2.0,
        (f64::from(height) - 1.0) / 2.0,
    );
    let c = [(x - cx) / focal, (y - cy) / focal, 1.0];
    // World = rᵀ · camera.
    std::array::from_fn(|i| (0..3).map(|k| r[k][i] * c[k]).sum())
}

/// One frame of a synthetic panorama as a raw DNG (the HDR fixtures'
/// sensor: ISO 400's noise, the EXIF of a 40 mm lens at f/2.8, every
/// frame exposed 1/250 s whatever its `gain`).
#[derive(Debug, Clone)]
pub struct PanoFrame {
    pub width: u32,
    pub height: u32,
    /// Light reaching the sensor relative to the scene's (an exposure the
    /// EXIF doesn't record: changing light, auto exposure).
    pub gain: f64,
    pub seed: u64,
    pub datetime: &'static str,
    pub subsec: &'static str,
}

impl PanoFrame {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            gain: 1.0,
            seed: 1,
            datetime: "2026:10:22 15:00:00",
            subsec: "10",
        }
    }

    /// The file, with each photosite seeing `light(x, y)` (relative to
    /// the sensor's white) times `gain`, and noise.
    pub fn write(&self, light: impl Fn(f64, f64) -> f64) -> Vec<u8> {
        let dng = Dng {
            shot: Some(Shot {
                datetime: self.datetime,
                subsec: self.subsec,
                orientation: 1,
                serial: "7654321",
            }),
            exposure: Some((4, 1000)),
            ..Dng::new(self.width, self.height)
        };
        let range = f64::from(dng.white - dng.black);
        let (a, b) = (8e-5, 1.6e-8);
        let mut rng = Rng(self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
        let mut noise = move || {
            let u1 = (f64::from(rng.f()) + 1e-4).min(1.0);
            let u2 = f64::from(rng.f());
            (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
        };
        let mut px = vec![0u16; (self.width * self.height) as usize];
        for y in 0..self.height {
            for x in 0..self.width {
                let c = usize::from(dng.cfa[(y % 2) as usize][(x % 2) as usize]);
                let v = light(f64::from(x), f64::from(y)) * f64::from(dng.neutral[c]) * self.gain;
                let v = v + noise() * (a * v.max(0.0) + b).sqrt();
                let counts = f64::from(dng.black) + v.clamp(0.0, 1.0) * range;
                px[(y * self.width + x) as usize] = counts.round() as u16;
            }
        }
        let w = self.width;
        dng.write(|x, y| px[(y * w + x) as usize])
    }
}
