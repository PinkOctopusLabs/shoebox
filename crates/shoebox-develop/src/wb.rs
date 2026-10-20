//! White balance and the camera → working-space matrix (`docs/EDITING-SPEC.md`,
//! Colour science), the way the DNG SDK does it: the camera's colour
//! matrices (XYZ → camera, calibrated under two illuminants) are blended by
//! inverse colour temperature, the white's chromaticity and its camera-RGB
//! neutral are found together, and the result is adapted (Bradford) to the
//! working space's D65 white.

// Channel and matrix loops read clearer indexed (c, r) than zipped.
#![allow(clippy::needless_range_loop)]

use shoebox_decode::raw::{ColorMatrix, RawImage};

use crate::colour::{self, M3, Xy};
use crate::edit::{WbMode, WhiteBalance};

/// Temperature and tint of the presets (Lightroom's values).
pub fn preset(mode: WbMode) -> Option<(f64, f64)> {
    Some(match mode {
        WbMode::Daylight => (5500.0, 10.0),
        WbMode::Cloudy => (6500.0, 10.0),
        WbMode::Shade => (7500.0, 10.0),
        WbMode::Tungsten => (2850.0, 0.0),
        WbMode::Fluorescent => (3800.0, 21.0),
        WbMode::Flash => (5500.0, 0.0),
        WbMode::AsShot | WbMode::Custom => return None,
    })
}

/// A camera's calibration: its matrices by temperature.
pub struct Camera {
    matrices: Vec<(f64, M3)>,
}

impl Camera {
    pub fn new(matrices: &[ColorMatrix]) -> Self {
        let mut m: Vec<(f64, M3)> = matrices
            .iter()
            .map(|c| {
                let mut a = [[0.0; 3]; 3];
                for (i, row) in c.xyz_to_cam.iter().enumerate() {
                    for (j, v) in row.iter().enumerate() {
                        a[i][j] = f64::from(*v);
                    }
                }
                (f64::from(c.temperature), a)
            })
            .collect();
        m.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { matrices: m }
    }

    /// XYZ → camera for a white at `temp` K: the two calibrations either
    /// side, blended by inverse temperature (the nearest one outside them).
    pub fn xyz_to_cam(&self, temp: f64) -> M3 {
        let m = &self.matrices;
        match m.len() {
            0 => colour::IDENTITY,
            1 => m[0].1,
            _ => {
                let (lo, hi) = (m[0], m[m.len() - 1]);
                if temp <= lo.0 {
                    return lo.1;
                }
                if temp >= hi.0 {
                    return hi.1;
                }
                let i = m.iter().position(|(t, _)| *t >= temp).unwrap_or(1).max(1);
                let (a, b) = (m[i - 1], m[i]);
                let g = ((1.0 / temp - 1.0 / b.0) / (1.0 / a.0 - 1.0 / b.0)).clamp(0.0, 1.0);
                let mut out = [[0.0; 3]; 3];
                for r in 0..3 {
                    for c in 0..3 {
                        out[r][c] = g * a.1[r][c] + (1.0 - g) * b.1[r][c];
                    }
                }
                out
            }
        }
    }

    /// The camera RGB of a white with this chromaticity (Y = 1).
    pub fn neutral_of(&self, xy: Xy) -> [f64; 3] {
        let (t, _) = colour::xy_to_temp_tint(xy);
        colour::mulv(&self.xyz_to_cam(t), colour::xy_to_xyz(xy))
    }

    /// The white's chromaticity for a camera-RGB neutral, iterating
    /// because the matrix depends on the white's temperature.
    pub fn xy_of(&self, neutral: [f64; 3]) -> Xy {
        let mut xy = colour::D50;
        for _ in 0..30 {
            let (t, _) = colour::xy_to_temp_tint(xy);
            let Some(cam_to_xyz) = colour::inv(&self.xyz_to_cam(t)) else {
                return xy;
            };
            let next = colour::xyz_to_xy(colour::mulv(&cam_to_xyz, neutral));
            if (next.0 - xy.0).abs() + (next.1 - xy.1).abs() < 1e-7 {
                return next;
            }
            xy = next;
        }
        xy
    }
}

/// White balance for a finished image (JPEG and the like), relative to
/// how it was shot: as shot is no change (shown as D65's temperature and
/// tint); another white is adapted (Bradford) to D65, so "Tungsten"
/// neutralises a warm light. Returns the temperature, tint and the
/// Rec.2020 matrix.
pub fn relative(wb: &WhiteBalance) -> (f64, f64, M3) {
    let (t, tint) = match wb.mode {
        WbMode::AsShot => {
            let (t, n) = colour::xy_to_temp_tint(colour::D65);
            return (t, n, colour::IDENTITY);
        }
        WbMode::Custom => (wb.temp.unwrap_or(6504.0), wb.tint.unwrap_or(0.0)),
        mode => preset(mode).unwrap_or((6504.0, 0.0)),
    };
    let xy = colour::temp_tint_to_xy(t, tint);
    let m = colour::mul(
        &colour::xyz_to_rgb(&colour::REC2020, colour::D65),
        &colour::mul(
            &colour::bradford(xy, colour::D65),
            &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
        ),
    );
    (t, tint, m)
}

/// The white balance that makes `patch` neutral (the eyedropper): the
/// temperature and tint of a custom white balance. `patch` is the source's
/// mean around the point clicked: for a raw, camera RGB balanced at
/// `as_shot`; for a finished image, linear Rec.2020.
pub fn neutral_at(raw: &RawImage, src: &crate::cpu::Demosaiced, patch: [f64; 3]) -> (f64, f64) {
    let xy = match src.referred {
        crate::cpu::Referred::Scene => {
            // Undo the as-shot balance: the patch's raw camera RGB is the
            // white's neutral.
            let n = [0, 1, 2].map(|c| patch[c].max(1e-9) / f64::from(src.as_shot[c]));
            Camera::new(&raw.matrices).xy_of(n)
        }
        crate::cpu::Referred::Display => colour::xyz_to_xy(colour::mulv(
            &colour::rgb_to_xyz(&colour::REC2020, colour::D65),
            patch.map(|v| v.max(1e-9)),
        )),
    };
    colour::xy_to_temp_tint(xy)
}

/// White balance worked out for one photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    /// Multipliers for the raw channels (R, G, B), green 1.0.
    pub multipliers: [f32; 3],
    /// The white's temperature and tint, for the sliders.
    pub temp: f64,
    pub tint: f64,
    /// White-balanced camera RGB → linear Rec.2020 (D65): a white-balanced
    /// (1, 1, 1) is neutral, at Y = 1 / green neutral.
    pub cam_to_rec2020: M3,
}

/// Resolve a white balance setting against the photo's calibration.
pub fn resolve(raw: &RawImage, wb: &WhiteBalance) -> Resolved {
    let cam = Camera::new(&raw.matrices);
    let fallback = || {
        let n = cam.neutral_of(colour::D65);
        let xy = colour::D65;
        (xy, n)
    };
    let (xy, neutral) = match wb.mode {
        WbMode::AsShot => match raw.as_shot {
            Some(m) => {
                let n = [1.0 / f64::from(m[0]), 1.0, 1.0 / f64::from(m[2])];
                (cam.xy_of(n), n)
            }
            None => fallback(),
        },
        mode => {
            let (t, tint) = match preset(mode) {
                Some(p) => p,
                None => (wb.temp.unwrap_or(5500.0), wb.tint.unwrap_or(0.0)),
            };
            let xy = colour::temp_tint_to_xy(t, tint);
            (xy, cam.neutral_of(xy))
        }
    };
    let (temp, tint) = colour::xy_to_temp_tint(xy);
    let cam_to_xyz = colour::inv(&cam.xyz_to_cam(temp)).unwrap_or(colour::IDENTITY);
    // Scale the neutral so the white it stands for has Y = 1.
    let y = colour::mulv(&cam_to_xyz, neutral)[1];
    let neutral = if y.abs() > 1e-12 {
        neutral.map(|v| v / y)
    } else {
        neutral
    };
    let multipliers = [
        (neutral[1] / neutral[0]) as f32,
        1.0,
        (neutral[1] / neutral[2]) as f32,
    ];
    // Undo the multipliers (green 1) back to camera RGB, to XYZ, adapt the
    // white to D65, then to Rec.2020.
    // Physically: a raw value keeps its meaning whatever the white balance,
    // so changing white balance never changes brightness. (DNG matrices are
    // normalised so a white's camera neutral peaks near 1, so white-balanced
    // (1, 1, 1), the green clip, sits near Y = 1.)
    let undo = colour::diag([neutral[0] / neutral[1], 1.0, neutral[2] / neutral[1]]);
    let to_xyz = colour::mul(&cam_to_xyz, &undo);
    let adapt = colour::bradford(xy, colour::D65);
    let to_2020 = colour::xyz_to_rgb(&colour::REC2020, colour::D65);
    let cam_to_rec2020 = colour::mul(&to_2020, &colour::mul(&adapt, &to_xyz));
    Resolved {
        multipliers,
        temp,
        tint,
        cam_to_rec2020,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shoebox_decode::raw::ColorMatrix;

    /// The Z f's matrices (rawler's camera file), as a RawImage would carry.
    fn zf() -> RawImage {
        let m = |v: [f32; 9]| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
        RawImage {
            width: 2,
            height: 2,
            cfa: [[0, 1], [1, 2]],
            data: vec![0.0; 4],
            as_shot: Some([1.1425778, 1.0, 1.7753916]),
            matrices: vec![
                ColorMatrix {
                    temperature: 2856.0,
                    xyz_to_cam: m([
                        1.3904, -0.7947, 0.0654, -0.432, 1.2105, 0.2497, -0.0235, 0.083, 0.9243,
                    ]),
                },
                ColorMatrix {
                    temperature: 6504.0,
                    xyz_to_cam: m([
                        1.1607, -0.4491, -0.0977, -0.4522, 1.246, 0.2304, -0.0458, 0.1519, 0.7616,
                    ]),
                },
            ],
            make: "Nikon".into(),
            model: "Z f".into(),
        }
    }

    #[test]
    fn white_balanced_white_is_working_space_white() {
        let raw = zf();
        for wb in [
            WhiteBalance::default(),
            WhiteBalance {
                mode: WbMode::Tungsten,
                ..Default::default()
            },
            WhiteBalance {
                mode: WbMode::Custom,
                temp: Some(4300.0),
                tint: Some(-12.0),
            },
        ] {
            let r = resolve(&raw, &wb);
            let w = colour::mulv(&r.cam_to_rec2020, [1.0; 3]);
            assert!(w.iter().all(|v| (v - w[1]).abs() < 1e-6), "{wb:?}: {w:?}");
            assert!(w[1] > 0.7 && w[1] < 1.5, "{w:?}");
            assert_eq!(r.multipliers[1], 1.0);
        }
    }

    #[test]
    fn as_shot_round_trips_through_temp_and_tint() {
        let raw = zf();
        let shot = resolve(&raw, &WhiteBalance::default());
        // The fixture's night scene: warm light.
        assert!(shot.temp > 2500.0 && shot.temp < 4500.0, "{}", shot.temp);
        let custom = resolve(
            &raw,
            &WhiteBalance {
                mode: WbMode::Custom,
                temp: Some(shot.temp),
                tint: Some(shot.tint),
            },
        );
        for c in 0..3 {
            assert!(
                (custom.multipliers[c] - shot.multipliers[c]).abs() < 1e-3,
                "{custom:?} vs {shot:?}"
            );
        }
        assert!((shot.multipliers[0] - 1.1425778).abs() < 1e-4);
        // Warmer light needs more blue gain.
        let tungsten = resolve(
            &raw,
            &WhiteBalance {
                mode: WbMode::Tungsten,
                ..Default::default()
            },
        );
        let day = resolve(
            &raw,
            &WhiteBalance {
                mode: WbMode::Daylight,
                ..Default::default()
            },
        );
        assert!(tungsten.multipliers[2] > day.multipliers[2]);
        assert!(tungsten.multipliers[0] < day.multipliers[0]);
    }
}

#[cfg(test)]
mod brightness {
    use super::*;

    #[test]
    fn white_balance_never_changes_brightness_of_green() {
        // A pure raw-green stimulus has the same luminance whatever the WB.
        let raw = tests_zf();
        let lum = |wb: WhiteBalance| {
            let r = resolve(&raw, &wb);
            let m = &r.cam_to_rec2020;
            // Raw (0, g, 0) after WB is (0, g, 0); luminance of its Rec.2020.
            let rgb = colour::mulv(m, [0.0, 1.0, 0.0]);
            let y = colour::rgb_to_xyz(&colour::REC2020, colour::D65)[1];
            y[0] * rgb[0] + y[1] * rgb[1] + y[2] * rgb[2]
        };
        let a = lum(WhiteBalance::default());
        let b = lum(WhiteBalance {
            mode: WbMode::Tungsten,
            ..Default::default()
        });
        let c = lum(WhiteBalance {
            mode: WbMode::Shade,
            ..Default::default()
        });
        // The matrix blend and adaptation move a saturated stimulus a little;
        // the bug this guards against (scaling by the neutral) moves it ~2×.
        assert!(
            (a - b).abs() / a < 0.12 && (a - c).abs() / a < 0.12,
            "{a} {b} {c}"
        );
    }

    fn tests_zf() -> RawImage {
        let m = |v: [f32; 9]| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
        RawImage {
            width: 2,
            height: 2,
            cfa: [[0, 1], [1, 2]],
            data: vec![0.0; 4],
            as_shot: Some([1.1425778, 1.0, 1.7753916]),
            matrices: vec![
                ColorMatrix {
                    temperature: 2856.0,
                    xyz_to_cam: m([
                        1.3904, -0.7947, 0.0654, -0.432, 1.2105, 0.2497, -0.0235, 0.083, 0.9243,
                    ]),
                },
                ColorMatrix {
                    temperature: 6504.0,
                    xyz_to_cam: m([
                        1.1607, -0.4491, -0.0977, -0.4522, 1.246, 0.2304, -0.0458, 0.1519, 0.7616,
                    ]),
                },
            ],
            make: "Nikon".into(),
            model: "Z f".into(),
        }
    }
}
