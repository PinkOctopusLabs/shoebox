//! Panorama on synthetic frames: rectilinear views of a textured scene all
//! round the camera (`shoebox_fixtures::pano`), turned to known rotations
//! with a known focal length, through a known lens's distortion and
//! vignetting, each with its own exposure and noise.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_range_loop)]

use std::path::Path;
use std::sync::atomic::AtomicBool;

use shoebox_develop::merge::pano::{self, Projection, geom, optics::Optics};
use shoebox_develop::merge::{self, Input, MergeError, Record};
use shoebox_fixtures::PanoFrame;
use shoebox_fixtures::pano::{Dir, Rotation, lon_lat, pano_light, ray, rotation};

const W: u32 = 600;
const H: u32 = 400;

/// The test lens's corrections, as panorama will find them (the frames'
/// EXIF names a lens Lensfun knows: a 40 mm at f/2.8, vignetting
/// strongly).
fn test_optics(dir: &Path) -> Optics {
    let p = dir.join("probe.dng");
    std::fs::write(&p, PanoFrame::new(W, H).write(|_, _| 0.2)).unwrap();
    let input = Input::new(&p, 1);
    let src = merge::load(&input, &AtomicBool::new(false)).unwrap();
    let shot = pano::optics::shot(&input, &src);
    let (o, what) = pano::optics::optics(&shot, &src, 1, true);
    std::fs::remove_file(&p).unwrap();
    assert!(o.corrects(), "Lensfun knows the test lens ({what:?})");
    o
}

/// The ideal pixel a raw pixel shows (the lens's distortion undone).
fn ideal(o: &Optics, x: f64, y: f64) -> (f64, f64, f32) {
    let mut p = (x, y);
    let mut a = o.at(x, y);
    for _ in 0..4 {
        p = (p.0 + x - a.g[0], p.1 + y - a.g[1]);
        a = o.at(p.0, p.1);
    }
    (p.0, p.1, a.gain)
}

struct Truth {
    rotations: Vec<Rotation>,
    gains: Vec<f64>,
    #[allow(dead_code)]
    focal: f64,
}

/// Write frames turned to `(yaw, pitch, roll)` with these gains.
fn row(
    dir: &Path,
    views: &[(f64, f64, f64)],
    gains: &[f64],
    focal: f64,
    optics: Option<&Optics>,
) -> (Vec<Input>, Truth) {
    row_seeing(dir, views, gains, focal, optics, &|_, d| pano_light(d))
}

/// [`row`], frame `i` seeing `scene(i, direction)`.
fn row_seeing(
    dir: &Path,
    views: &[(f64, f64, f64)],
    gains: &[f64],
    focal: f64,
    optics: Option<&Optics>,
    scene: &dyn Fn(usize, Dir) -> f64,
) -> (Vec<Input>, Truth) {
    let mut inputs = Vec::new();
    let mut rotations = Vec::new();
    for (i, (&(yaw, pitch, roll), &gain)) in views.iter().zip(gains).enumerate() {
        let r = rotation(yaw, pitch, roll);
        let f = PanoFrame {
            gain,
            seed: 31 + i as u64,
            subsec: ["10", "20", "30", "40", "50", "60", "70"][i % 7],
            ..PanoFrame::new(W, H)
        };
        let bytes = f.write(|x, y| {
            let (u, v, g) = match optics {
                Some(o) => ideal(o, x, y),
                None => (x, y, 1.0),
            };
            scene(i, ray(&r, focal, W, H, u, v)) / f64::from(g)
        });
        let p = dir.join(format!("DSC_{:04}.dng", 100 + i));
        std::fs::write(&p, bytes).unwrap();
        inputs.push(Input::new(p, 1));
        rotations.push(r);
    }
    (
        inputs,
        Truth {
            rotations,
            gains: gains.to_vec(),
            focal,
        },
    )
}

/// The focal length EXIF's 40 mm means on these frames.
fn exif_focal() -> f64 {
    40.0 / 36f64.hypot(24.0) * f64::from(W).hypot(f64::from(H))
}

fn angle_between(a: &Rotation, b: &[[f64; 3]; 3]) -> f64 {
    // a · bᵀ's angle.
    let m: [[f64; 3]; 3] =
        std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[j][k]).sum()));
    geom::angle(&m).to_degrees()
}

fn relative(r: &[[f64; 3]; 3], r0: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| r[i][k] * r0[j][k]).sum()))
}

/// The result's green against what it should be, pixel by pixel.
struct Compare {
    w: usize,
    h: usize,
    got: Vec<f64>,
    want: Vec<f64>,
}

impl Compare {
    fn new(img: &shoebox_develop::cpu::Rgb, want: impl Fn(usize, usize) -> f64) -> Self {
        let (w, h) = (img.width, img.height);
        let mut c = Compare {
            w,
            h,
            got: Vec::with_capacity(w * h),
            want: Vec::with_capacity(w * h),
        };
        for y in 0..h {
            for x in 0..w {
                c.got.push(f64::from(img.data[(y * w + x) * 3 + 1]));
                c.want.push(want(x, y));
            }
        }
        c
    }

    /// Ratios of `n`×`m` blocks' sums.
    fn blocks(&self, n: usize, m: usize) -> Vec<f64> {
        let mut out = Vec::new();
        for by in 0..self.h / m {
            for bx in 0..self.w / n {
                let (mut g, mut t) = (0.0, 0.0);
                for y in by * m..by * m + m {
                    for x in bx * n..bx * n + n {
                        g += self.got[y * self.w + x];
                        t += self.want[y * self.w + x];
                    }
                }
                out.push(g / t);
            }
        }
        out
    }

    /// The median 32-pixel block's ratio and the 95th percentile of their
    /// errors; and across the result, 16-pixel strips the full height (a
    /// seam, a frame's exposure or vignetting shows there; texture
    /// averages out): the worst strip's error and the largest step between
    /// neighbours.
    fn report(&self) -> (f64, f64, f64, f64) {
        let mut b = self.blocks(32, 32);
        b.sort_by(f64::total_cmp);
        let median = b[b.len() / 2];
        let mut e: Vec<f64> = b.iter().map(|v| (v - 1.0).abs()).collect();
        e.sort_by(f64::total_cmp);
        let p95 = e[e.len() * 95 / 100];
        let strips = self.blocks(16, self.h);
        let worst = strips.iter().map(|v| (v - 1.0).abs()).fold(0.0, f64::max);
        let step = strips
            .windows(2)
            .map(|p| (p[1] - p[0]).abs())
            .fold(0.0, f64::max);
        if std::env::var("PANO_DEBUG").is_ok() {
            eprintln!(
                "strips: {:?}",
                strips
                    .iter()
                    .map(|v| ((v - 1.0) * 1000.0).round())
                    .collect::<Vec<_>>()
            );
        }
        (median, p95, worst, step)
    }

    fn check(&self, what: &str) {
        let (median, p95, worst, step) = self.report();
        eprintln!(
            "{what}: median {median:.4}, p95 {p95:.4}, strips worst {worst:.4} step {step:.4}"
        );
        assert!(
            (median - 1.0).abs() < 0.01,
            "{what}: median ratio {median:.4}"
        );
        assert!(p95 < 0.01, "{what}: 95% of blocks within {p95:.4}");
        assert!(worst < 0.006, "{what}: a strip {worst:.4} off");
        assert!(step < 0.006, "{what}: a step of {step:.4} between strips");
    }
}

/// The straightened world's directions → the scene's.
fn to_scene(p: &pano::Panorama, t: &Truth) -> [[f64; 3]; 3] {
    let root = p.merged.reference;
    let est = p.frames[root].rotation.unwrap();
    let truth = t.rotations[root];
    // truthᵀ · est.
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| truth[k][i] * est[k][j]).sum()))
}

/// The result against the scene's light (in the reference frame's
/// exposure).
fn against_scene(p: &pano::Panorama, t: &Truth) -> Compare {
    let root = p.merged.reference;
    let est = p.frames[root].rotation.unwrap();
    let truth = t.rotations[root];
    // Straightened world → the scene's: truthᵀ · est.
    let m: [[f64; 3]; 3] = std::array::from_fn(|i| {
        std::array::from_fn(|j| (0..3).map(|k| truth[k][i] * est[k][j]).sum())
    });
    Compare::new(&p.merged.rgb, |x, y| {
        let d = p.layout.direction(x as f64, y as f64).unwrap();
        pano_light(geom::mv(&m, d)) * t.gains[root]
    })
}

#[test]
fn a_hand_held_row_is_registered_and_stitched_without_seams() {
    let t = tempfile::tempdir().unwrap();
    let optics = test_optics(t.path());
    let focal = exif_focal() * 1.03;
    let views = [
        (-56.0, 1.5, -1.0),
        (-28.5, -1.0, 2.0),
        (0.0, 0.5, 0.0),
        (27.5, -2.0, -1.5),
        (55.0, 1.0, 0.8),
    ];
    let gains = [1.0, 1.25, 0.85, 1.1, 0.9];
    let (inputs, truth) = row(t.path(), &views, &gains, focal, Some(&optics));
    let cancel = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let p = pano::merge(&inputs, &pano::Options::default(), &|_| {}, &cancel).unwrap();
    eprintln!(
        "{:.1} s: {}×{} (uncropped {:?}), {:?}, focal {:?} (true {focal:.1}, EXIF {:?}), fit {:?}, notes {:?}",
        started.elapsed().as_secs_f64(),
        p.merged.rgb.width,
        p.merged.rgb.height,
        p.uncropped,
        p.projection,
        p.focal,
        p.focal_exif,
        p.fit,
        p.notes
    );
    assert!(!p.flat);
    assert_eq!(p.projection, Projection::Cylindrical, "a 160° row");
    // The focal length, to 0.5%.
    let f = p.focal.unwrap();
    assert!(
        (f / focal - 1.0).abs() < 0.005,
        "focal {f:.2} for {focal:.2}"
    );
    // Rotations relative to the reference, to 0.05°.
    let root = p.merged.reference;
    assert_eq!(root, 2, "the middle frame is the reference");
    let r0 = p.frames[root].rotation.unwrap();
    for i in 0..inputs.len() {
        let est = relative(&p.frames[i].rotation.unwrap(), &r0);
        let tru = relative(&truth.rotations[i], &truth.rotations[root]);
        let err = angle_between(&tru, &est);
        eprintln!("frame {i}: {err:.4}° off");
        assert!(err < 0.05, "frame {i}: {err:.3}° off");
    }
    // Exposure gains, to 1%.
    for i in 0..inputs.len() {
        let want = truth.gains[root] / truth.gains[i];
        let got = p.frames[i].gain;
        assert!(
            (got / want - 1.0).abs() < 0.01,
            "frame {i}: gain {got:.4} for {want:.4}"
        );
    }
    // Straightened: the reference frame's roll and pitch are what it was
    // shot at (the others' average out level).
    // The scene, re-projected: no seam, no vignetting.
    against_scene(&p, &truth).check("row");
    // Cropped: no empty pixels.
    let empty = p
        .merged
        .rgb
        .data
        .chunks(3)
        .filter(|px| px[1] <= 0.0)
        .count();
    assert_eq!(empty, 0);

    // The DNG: the panorama's size, linear, no lens to correct again, and
    // its record.
    let record = Record {
        kind: "panorama".into(),
        sources: inputs
            .iter()
            .map(|i| {
                (
                    i.path.file_name().unwrap().to_string_lossy().into_owned(),
                    "00".into(),
                )
            })
            .collect(),
    };
    let bytes = merge::dng(&p.merged, &record).unwrap();
    let back = shoebox_decode::raw::decode(&bytes).unwrap();
    assert!(back.linear.is_some());
    assert_eq!(
        (back.width, back.height),
        (p.merged.rgb.width, p.merged.rgb.height)
    );
    let src = rawler::rawsource::RawSource::new_from_slice(&bytes);
    let md = merge::metadata(&src).unwrap();
    assert!(md.exif.lens_model.is_none() && md.exif.focal_length.is_none());
    assert!(String::from_utf8_lossy(&bytes).contains("shoebox:Merge=\"panorama\""));
}

#[test]
fn a_frame_that_overlaps_nothing_is_named() {
    let t = tempfile::tempdir().unwrap();
    let focal = exif_focal();
    let (inputs, _) = row(
        t.path(),
        &[(0.0, 0.0, 0.0), (28.0, 0.0, 0.0), (150.0, 0.0, 0.0)],
        &[1.0; 3],
        focal,
        None,
    );
    let o = pano::Options {
        lens: false,
        ..Default::default()
    };
    match pano::merge(&inputs, &o, &|_| {}, &AtomicBool::new(false)) {
        Err(MergeError::Refused(why)) => {
            assert!(
                why.contains("DSC_0102.dng") && why.contains("overlap"),
                "{why}"
            );
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("stitched a frame that overlaps nothing"),
    }
    // Too few, and Perspective too wide.
    assert!(matches!(
        pano::merge(&inputs[..1], &o, &|_| {}, &AtomicBool::new(false)),
        Err(MergeError::Refused(_))
    ));
}

#[test]
fn perspective_is_refused_beyond_its_reach_and_drawn_within_it() {
    let t = tempfile::tempdir().unwrap();
    let focal = exif_focal();
    let views: Vec<(f64, f64, f64)> = (0..4).map(|i| (i as f64 * 30.0, 0.0, 0.0)).collect();
    let (inputs, truth) = row(t.path(), &views, &[1.0; 4], focal, None);
    let o = |projection| pano::Options {
        projection,
        lens: false,
        ..Default::default()
    };
    match pano::merge(
        &inputs,
        &o(Projection::Perspective),
        &|_| {},
        &AtomicBool::new(false),
    ) {
        Err(MergeError::Refused(why)) => assert!(why.contains("Perspective"), "{why}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("drew 135° in Perspective"),
    }
    // Two frames are narrow enough: Auto draws them so, straight lines
    // straight.
    let p = pano::merge(
        &inputs[..2],
        &o(Projection::Auto),
        &|_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(p.projection, Projection::Perspective);
    against_scene(&p, &truth).check("perspective");
    // Spherical, uncropped: the boundary kept, the corners empty.
    let s = pano::merge(
        &inputs[..2],
        &pano::Options {
            projection: Projection::Spherical,
            crop: false,
            lens: false,
        },
        &|_| {},
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(s.projection, Projection::Spherical);
    assert_eq!((s.merged.rgb.width, s.merged.rgb.height), s.uncropped);
    assert!(s.merged.rgb.data.chunks(3).any(|px| px[1] == 0.0));
}

/// A flat thing (a mural, a scan) photographed from several places: no
/// turning camera explains it, a plane does.
#[test]
fn a_plane_shot_from_several_places_is_laid_flat() {
    let t = tempfile::tempdir().unwrap();
    let focal = exif_focal();
    // The wall: 1 unit ahead; the camera steps 0.35 units right each time,
    // turning a little.
    let wall = |x: f64, y: f64| pano_light([x * 0.9, y * 0.9 - 0.15, 1.0]);
    let cams = [(-0.35, 0.02, 2.0), (0.0, 0.0, -1.0), (0.35, -0.03, 1.5)];
    let mut inputs = Vec::new();
    let mut seen = Vec::new();
    for (i, &(tx, ty, yaw)) in cams.iter().enumerate() {
        let r = rotation(yaw, 0.5 * yaw, 0.3 * yaw);
        let at = move |x: f64, y: f64| {
            let d = ray(&r, focal, W, H, x, y);
            let s = 1.0 / d[2];
            (tx + s * d[0], ty + s * d[1])
        };
        let f = PanoFrame {
            seed: 50 + i as u64,
            ..PanoFrame::new(W, H)
        };
        let p = t.path().join(format!("SCAN_{i}.dng"));
        std::fs::write(
            &p,
            f.write(|x, y| {
                let (u, v) = at(x, y);
                wall(u, v)
            }),
        )
        .unwrap();
        inputs.push(Input::new(p, 1));
        seen.push(at);
    }
    let o = pano::Options {
        lens: false,
        ..Default::default()
    };
    let p = pano::merge(&inputs, &o, &|_| {}, &AtomicBool::new(false)).unwrap();
    eprintln!("flat: {:?} {:?} {:?}", p.fit, p.notes, p.uncropped);
    assert!(p.flat, "laid flat");
    let root = p.merged.reference;
    Compare::new(&p.merged.rgb, |x, y| {
        let q = p.layout.to_frame(root, x as f64, y as f64).unwrap();
        let (u, v) = seen[root](q[0], q[1]);
        wall(u, v)
    })
    .check("flat");
}

#[test]
fn stop_stops_it() {
    let t = tempfile::tempdir().unwrap();
    let (inputs, _) = row(
        t.path(),
        &[(0.0, 0.0, 0.0), (25.0, 0.0, 0.0)],
        &[1.0; 2],
        exif_focal(),
        None,
    );
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        pano::merge(&inputs, &pano::Options::default(), &|_| {}, &cancel),
        Err(MergeError::Cancelled)
    ));
}

/// Someone who walked through the overlap of two frames, seen in one only:
/// the seam goes round them (they're whole or gone), not through them.
#[test]
fn a_seam_goes_round_what_moved() {
    let t = tempfile::tempdir().unwrap();
    let focal = exif_focal();
    // Astride the centre-weighted seam (about −14°), seen by the middle
    // frame only.
    let person = |d: Dir| {
        let (lon, lat) = lon_lat(d);
        let (lon, lat) = (lon.to_degrees(), lat.to_degrees());
        (-17.0..-11.0).contains(&lon) && (-6.0..6.0).contains(&lat)
    };
    let scene = |i: usize, d: Dir| {
        if i == 1 && person(d) {
            0.9
        } else {
            pano_light(d)
        }
    };
    let (inputs, truth) = row_seeing(
        t.path(),
        &[(-28.5, 0.5, 0.0), (0.0, 0.0, 0.0), (28.0, -0.5, 0.0)],
        &[1.0; 3],
        focal,
        None,
        &scene,
    );
    let o = pano::Options {
        lens: false,
        ..Default::default()
    };
    let p = pano::merge(&inputs, &o, &|_| {}, &AtomicBool::new(false)).unwrap();
    let m = to_scene(&p, &truth);
    let img = &p.merged.rgb;
    let (mut inside, mut bright) = (0, 0);
    for y in 0..img.height {
        for x in 0..img.width {
            let d = geom::mv(&m, p.layout.direction(x as f64, y as f64).unwrap());
            // Well inside the box (blending softens its edge).
            let (lon, lat) = lon_lat(d);
            let (lon, lat) = (lon.to_degrees(), lat.to_degrees());
            if (-16.5..-11.5).contains(&lon) && (-5.5..5.5).contains(&lat) {
                inside += 1;
                if img.data[(y * img.width + x) * 3 + 1] > 0.7 {
                    bright += 1;
                }
            }
        }
    }
    let share = bright as f64 / inside as f64;
    eprintln!("the person: {:.1}% of their box", share * 100.0);
    assert!(inside > 1000);
    assert!(
        !(0.03..0.97).contains(&share),
        "cut through: {:.1}% of them shown",
        share * 100.0
    );
}

/// Two rows, tilted up and down: registered as one, and straightened (the
/// result's down is the scene's).
#[test]
fn two_rows_are_registered_and_straightened() {
    let t = tempfile::tempdir().unwrap();
    let focal = exif_focal();
    let views = [
        (-25.0, 14.0, 0.0),
        (0.0, 15.0, 0.0),
        (25.0, 14.5, 0.0),
        (25.5, -14.0, 0.0),
        (0.5, -15.0, 0.0),
        (-24.5, -14.5, 0.0),
    ];
    let (inputs, truth) = row(t.path(), &views, &[1.0; 6], focal, None);
    let o = pano::Options {
        lens: false,
        ..Default::default()
    };
    let p = pano::merge(&inputs, &o, &|_| {}, &AtomicBool::new(false)).unwrap();
    eprintln!(
        "two rows: {:?} {:?} {:?} {:?}",
        p.projection, p.fit, p.uncropped, p.notes
    );
    let root = p.merged.reference;
    let r0 = p.frames[root].rotation.unwrap();
    for i in 0..inputs.len() {
        let est = relative(&p.frames[i].rotation.unwrap(), &r0);
        let tru = relative(&truth.rotations[i], &truth.rotations[root]);
        let err = angle_between(&tru, &est);
        eprintln!("frame {i}: {err:.4}° off");
        assert!(err < 0.05, "frame {i}: {err:.3}° off");
    }
    // Down is down.
    let m = to_scene(&p, &truth);
    let down = geom::mv(&m, [0.0, 1.0, 0.0]);
    let tilt = down[1].clamp(-1.0, 1.0).acos().to_degrees();
    assert!(tilt < 0.1, "tilted {tilt:.3}°");
    against_scene(&p, &truth).check("two rows");
}
