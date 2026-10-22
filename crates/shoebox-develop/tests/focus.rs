//! Focus stacking on synthetic focus brackets (`shoebox_fixtures::
//! FocusFrame`): a textured wall slanting from near to far and a disc
//! near the camera in front of its far side, each frame sharp at one
//! depth, blurred elsewhere by how far it is from it, magnified a little
//! more each frame (focus breathing), with noise. The all-in-focus truth
//! is known.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_range_loop)]

use std::path::Path;
use std::sync::atomic::AtomicBool;

use shoebox_develop::colour::M3;
use shoebox_develop::cpu::Rgb;
use shoebox_develop::merge::focus::{self, Method};
use shoebox_develop::merge::{self, Input, MergeError, Record, align};
use shoebox_fixtures::FocusFrame;

const W: u32 = 384;
const H: u32 = 256;

/// `n` frames focused from near to far, each 0.3% more magnified than the
/// last and shifted a little, written in `dir`; with the truth (all in
/// focus, no noise) as the reference frame sees it.
fn stack(dir: &Path, n: usize) -> (Vec<Input>, Vec<FocusFrame>, Rgb) {
    let r = (n - 1) / 2;
    let frames: Vec<FocusFrame> = (0..n)
        .map(|k| {
            let d = k as f64 - r as f64;
            FocusFrame {
                scale: 1.0 + 0.003 * d,
                shift: (0.4 * d, -0.25 * d),
                seed: k as u64 + 1,
                ..FocusFrame::new(W, H, k as f64 / (n - 1) as f64)
            }
        })
        .collect();
    let inputs = frames
        .iter()
        .enumerate()
        .map(|(k, f)| {
            let p = dir.join(format!("DSC_{:04}.dng", 100 + k));
            std::fs::write(&p, f.write()).unwrap();
            Input::new(p, 1)
        })
        .collect();
    let truth = FocusFrame {
        sharp: true,
        noise: false,
        ..frames[r].clone()
    };
    let p = dir.join("truth.dng");
    std::fs::write(&p, truth.write()).unwrap();
    let t = merge::load(&Input::new(&p, 1), &AtomicBool::new(false)).unwrap();
    (inputs, frames, t.rgb)
}

/// The worst distance between where `h` and the truth put a grid of the
/// reference's points (pixels).
fn worst(h: &M3, reference: &FocusFrame, frame: &FocusFrame) -> f64 {
    let mut worst = 0.0f64;
    for j in 0..=8 {
        for i in 0..=8 {
            let (x, y) = (
                i as f64 * (W - 1) as f64 / 8.0,
                j as f64 * (H - 1) as f64 / 8.0,
            );
            let (sx, sy) = reference.to_scene(x, y);
            let q = frame.to_frame(sx, sy);
            let p = align::apply(h, x, y);
            worst = worst.max((p.0 - q.0).hypot(p.1 - q.1));
        }
    }
    worst
}

fn luma(p: [f32; 3]) -> f64 {
    f64::from(0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2])
}

/// Detail in a block: the mean square of a band of the luminance (a 3×3
/// box blur less a 7×7 one: texture of a few pixels, not the noise of
/// one).
fn detail(img: &Rgb, x0: usize, y0: usize, n: usize) -> f64 {
    let boxed = |x: usize, y: usize, r: usize| {
        let mut s = 0.0;
        for yy in y - r..=y + r {
            for xx in x - r..=x + r {
                s += luma(img.px(xx, yy));
            }
        }
        s / ((2 * r + 1) * (2 * r + 1)) as f64
    };
    let mut s = 0.0;
    for y in y0..y0 + n {
        for x in x0..x0 + n {
            let d = boxed(x, y, 1) - boxed(x, y, 3);
            s += d * d;
        }
    }
    s / (n * n) as f64
}

/// The disc's edge in reference pixels: centre and radius.
fn disc(reference: &FocusFrame) -> ((f64, f64), f64) {
    let c = reference.to_frame(0.72 * W as f64, 0.5 * H as f64);
    (c, 0.2 * H as f64 * reference.scale)
}

/// An image cut as the result is.
fn cut(t: &Rgb, [x0, y0, w, h]: [usize; 4]) -> Rgb {
    let mut out = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = t.px(x0 + x, y0 + y);
            out.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&p);
        }
    }
    out
}

#[test]
fn a_focus_stack_is_sharp_throughout() {
    let t = tempfile::tempdir().unwrap();
    let n = 6;
    let (inputs, frames, truth) = stack(t.path(), n);
    let r = (n - 1) / 2;
    let cancel = AtomicBool::new(false);
    // Each frame, loaded, for comparison.
    let loaded: Vec<Rgb> = inputs
        .iter()
        .map(|i| merge::load(i, &cancel).unwrap().rgb)
        .collect();
    for method in Method::ALL {
        let o = focus::Options {
            method,
            ..Default::default()
        };
        let out = focus::merge(&inputs, &o, &|_| {}, &cancel).unwrap();
        // The middle frame is the reference; two frames held at most.
        assert_eq!(out.merged.reference, r);
        assert_eq!(out.peak_frames, 2);
        // Alignment: each frame's breathing and shift recovered along the
        // chain (the far frames least well: little is sharp in both).
        for k in 0..n {
            let f = &out.frames[k];
            let err = worst(&f.h, &frames[r], &frames[k]);
            let want = frames[k].scale / frames[r].scale;
            let got = focus::scale(&f.h);
            assert!(err < 0.25, "{method:?}: frame {k} {err:.3} px off");
            assert!(
                (got / want - 1.0).abs() < 1e-3,
                "{method:?}: frame {k} scale {got:.5} for {want:.5}"
            );
            assert!((f.gain - 1.0).abs() < 0.005, "frame {k} gain {}", f.gain);
            assert!(f.align_failed.is_none());
        }
        // Cut to what every frame covers: the most magnified frame (the
        // last) falls short of the reference's edges.
        let crop = out.crop;
        let res = &out.merged.rgb;
        assert_eq!((res.width, res.height), (crop[2], crop[3]));
        assert_eq!(
            (out.merged.meta.width, out.merged.meta.height),
            (crop[2], crop[3])
        );
        assert!(crop[0] >= 1 && crop[2] < W as usize && crop[2] >= W as usize - 12);
        assert!(crop[3] < H as usize && crop[3] >= H as usize - 8);
        let truth = cut(&truth, crop);
        // Each frame warped into the reference, cut the same way.
        let warped: Vec<Rgb> = (0..n)
            .map(|k| {
                let (img, _) = align::warp(&loaded[k], &out.frames[k].h, W as usize, H as usize);
                cut(&img, crop)
            })
            .collect();
        let ((cx, cy), rad) = disc(&frames[r]);
        let (cx, cy) = (cx - crop[0] as f64, cy - crop[1] as f64);
        // Block by block away from the disc's edge: as much detail as the
        // sharpest frame there, and as close to the truth.
        let b = 16;
        let (mut least, mut most_err, mut blocks) = (f64::MAX, 0.0f64, 0);
        for by in 0..res.height / b {
            for bx in 0..res.width / b {
                let (x0, y0) = (bx * b, by * b);
                if x0 < 4 || y0 < 4 || x0 + b + 4 > res.width || y0 + b + 4 > res.height {
                    continue;
                }
                let d = (x0 as f64 + 7.5 - cx).hypot(y0 as f64 + 7.5 - cy);
                if (d - rad).abs() < 18.0 {
                    continue;
                }
                blocks += 1;
                let sharpest = warped
                    .iter()
                    .map(|f| detail(f, x0, y0, b))
                    .fold(0.0, f64::max);
                least = least.min(detail(res, x0, y0, b) / sharpest);
                // Error against the truth, relative to its detail.
                let error = |img: &Rgb| {
                    let mut mean = 0.0;
                    for y in y0..y0 + b {
                        for x in x0..x0 + b {
                            mean += luma(truth.px(x, y));
                        }
                    }
                    mean /= (b * b) as f64;
                    let (mut se, mut st) = (0.0, 0.0);
                    for y in y0..y0 + b {
                        for x in x0..x0 + b {
                            se += (luma(img.px(x, y)) - luma(truth.px(x, y))).powi(2);
                            st += (luma(truth.px(x, y)) - mean).powi(2);
                        }
                    }
                    (se / st).sqrt()
                };
                let best = warped.iter().map(error).fold(f64::MAX, f64::min);
                let e = error(res);
                assert!(e < 0.25, "{method:?}: ({x0}, {y0}) {e:.3} from the truth");
                most_err = most_err.max(e / best);
            }
        }
        println!(
            "{method:?}: {blocks} blocks; detail at least {least:.3} of the sharpest frame's; \
             error at most {most_err:.3} of the best frame's"
        );
        assert!(blocks > 200);
        assert!(
            least > 0.92,
            "{method:?}: detail {least:.3} of the sharpest"
        );
        assert!(
            most_err < 1.15,
            "{method:?}: error {most_err:.3} of the best"
        );

        // The depth edge: rings around the disc, inside and out. The far
        // frames show the disc's blur spilling over the wall (a bright
        // halo outside, dark inside); the stack keeps neither.
        let ring = |img: &Rgb, lo: f64, hi: f64| {
            let (mut sum, mut abs, mut cnt) = (0.0, 0.0, 0);
            for y in 0..img.height {
                for x in 0..img.width {
                    let d = (x as f64 - cx).hypot(y as f64 - cy) - rad;
                    if d >= lo && d < hi {
                        let e = luma(img.px(x, y)) - luma(truth.px(x, y));
                        sum += e;
                        abs += e.abs();
                        cnt += 1;
                    }
                }
            }
            (sum / cnt as f64, abs / cnt as f64)
        };
        assert!(
            ring(&warped[n - 1], 2.0, 6.0).0 > 0.03,
            "the far frame's halo"
        );
        assert!(ring(&warped[n - 1], -6.0, -2.0).0 < -0.03);
        for (lo, hi) in [
            (-12.0, -6.0),
            (-6.0, -2.0),
            (-2.0, 2.0),
            (2.0, 6.0),
            (6.0, 12.0),
        ] {
            let (bias, abs) = ring(res, lo, hi);
            // The edge's contrast is about 0.25.
            assert!(
                bias.abs() < 0.005,
                "{method:?}: halo {bias:+.4} at {lo}..{hi} px"
            );
            assert!(abs < 0.02, "{method:?}: {abs:.4} off at {lo}..{hi} px");
        }
        // In range: nothing below black.
        assert!(res.data.iter().all(|v| *v >= 0.0 && v.is_finite()));
    }
}

#[test]
fn a_rail_stack_merges_without_alignment() {
    // No breathing, no movement (a focusing rail's frames, near enough):
    // Align off leaves them where they are, and the result is full size.
    let t = tempfile::tempdir().unwrap();
    let inputs: Vec<Input> = [0.0, 0.5, 1.0]
        .iter()
        .enumerate()
        .map(|(k, &f)| {
            let p = t.path().join(format!("R{k}.dng"));
            let frame = FocusFrame {
                seed: k as u64 + 7,
                ..FocusFrame::new(160, 112, f)
            };
            std::fs::write(&p, frame.write()).unwrap();
            Input::new(p, 1)
        })
        .collect();
    let o = focus::Options {
        align: false,
        method: Method::DepthMap,
    };
    let out = focus::merge(&inputs, &o, &|_| {}, &AtomicBool::new(false)).unwrap();
    assert!(out.frames.iter().all(|f| f.alignment.is_none()));
    assert_eq!(out.crop, [0, 0, 160, 112]);
    assert_eq!(out.merged.reference, 1);
    // Nothing warped: one frame at a time.
    assert_eq!(out.peak_frames, 1);
    // The float DNG, read back: the stack's size, its record.
    let record = Record {
        kind: "focus".into(),
        sources: vec![("R0.dng".into(), "00".repeat(32))],
    };
    let bytes = merge::dng(&out.merged, &record).unwrap();
    let back = shoebox_decode::raw::decode(&bytes).unwrap();
    assert!(back.linear.is_some());
    assert_eq!((back.width, back.height), (160, 112));
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("shoebox:Merge=\"focus\""));
}

#[test]
fn focus_stacks_refuse_and_stop() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a.dng");
    std::fs::write(&a, FocusFrame::new(96, 64, 0.0).write()).unwrap();
    let b = t.path().join("b.dng");
    std::fs::write(&b, FocusFrame::new(64, 48, 1.0).write()).unwrap();
    let jpg = t.path().join("c.jpg");
    let img = shoebox_fixtures::scene(1, 96, 64);
    std::fs::write(&jpg, shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
    let cancel = AtomicBool::new(false);
    let run = |paths: &[&Path], cancel: &AtomicBool| {
        let inputs: Vec<Input> = paths.iter().map(|p| Input::new(*p, 1)).collect();
        focus::merge(&inputs, &focus::Options::default(), &|_| {}, cancel)
    };
    assert!(matches!(run(&[&a], &cancel), Err(MergeError::Refused(_))));
    assert!(matches!(
        run(&[&a, &jpg], &cancel),
        Err(MergeError::Refused(_))
    ));
    assert!(matches!(
        run(&[&a, &b], &cancel),
        Err(MergeError::Refused(_))
    ));
    assert!(matches!(
        run(&[&a, &a], &AtomicBool::new(true)),
        Err(MergeError::Cancelled)
    ));
}
