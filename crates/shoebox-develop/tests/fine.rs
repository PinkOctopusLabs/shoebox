//! AI masks on sources larger than the masks' grid (exports, 100%
//! regions): upsampled by a guided filter on the source, so they follow
//! its edges far better than the grid read bilinearly does (between two
//! colours equally bright too); the finer grid stays within its budget;
//! and the GPU agrees with the CPU on it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::{
    Local, LocalAdjust, Mask, MaskPart, Op, Part, Settings, Shape, Stroke,
};
use shoebox_develop::mask::{self, Masks, fine};

/// Where the edge is: a disc, off the pixel grid.
fn inside(x: f32, y: f32) -> bool {
    (x - 131.3).hypot(y - 122.6) < 71.7
}

/// The errors near the edge of the guided and bilinear upsampling of a
/// 64 px mask to 256 px (mean absolute; pixels on the wrong side of one
/// half), and how many there are; the mask is blocky (each grid pixel all
/// in or out, by its centre), and with `soft`, blurred over 5 grid pixels
/// too, as a model's mask often is. The disc is brighter than around it,
/// or with `isoluminant` as bright but another colour.
fn upsampled(soft: bool, isoluminant: bool) -> ((f32, f32), (usize, usize), usize) {
    let (fw, fh) = (256, 256);
    let (gw, gh) = (64, 64);
    let k = fw / gw;
    let (inner, outer) = if isoluminant {
        ([0.8, 0.3, 0.3], [0.3, 0.55, 0.3])
    } else {
        ([0.8; 3], [0.15; 3])
    };
    // The full-size picture: a sharp disc, with a little texture.
    let tex = |x: usize, y: usize| ((x * 7 + y * 13) % 5) as f32 * 0.004;
    let guide: Vec<[f32; 3]> = (0..fw * fh)
        .map(|i| {
            let (x, y) = (i % fw, i / fw);
            let on = inside(x as f32 + 0.5, y as f32 + 0.5);
            (if on { inner } else { outer }).map(|v: f32| v + tex(x, y))
        })
        .collect();
    let truth: Vec<f32> = (0..fw * fh)
        .map(|i| {
            f32::from(u8::from(inside(
                (i % fw) as f32 + 0.5,
                (i / fw) as f32 + 0.5,
            )))
        })
        .collect();
    // On the grid: the guide area-averaged, and the mask.
    let guide_lo: Vec<[f32; 3]> = (0..gw * gh)
        .map(|i| {
            let (gx, gy) = (i % gw, i / gw);
            let mut s = [0.0; 3];
            for y in gy * k..(gy + 1) * k {
                for x in gx * k..(gx + 1) * k {
                    for c in 0..3 {
                        s[c] += guide[y * fw + x][c];
                    }
                }
            }
            s.map(|v| v / (k * k) as f32)
        })
        .collect();

    let blocky: Vec<f32> = (0..gw * gh)
        .map(|i| {
            let c = |v: usize| (v as f32 + 0.5) * k as f32;
            f32::from(u8::from(inside(c(i % gw), c(i / gw))))
        })
        .collect();
    let lo: Vec<u8> = (0..gw * gh)
        .map(|i| {
            let (x, y) = ((i % gw) as i64, (i / gw) as i64);
            let v = if soft {
                let mut s = 0.0;
                for dy in -2..=2i64 {
                    for dx in -2..=2i64 {
                        let xx = (x + dx).clamp(0, gw as i64 - 1) as usize;
                        let yy = (y + dy).clamp(0, gh as i64 - 1) as usize;
                        s += blocky[yy * gw + xx];
                    }
                }
                s / 25.0
            } else {
                blocky[i]
            };
            (v * 255.0).round() as u8
        })
        .collect();
    let up = fine::upsample(&lo, (gw, gh), &guide_lo, (fw, fh), &|y, row| {
        row.copy_from_slice(&guide[y * fw..(y + 1) * fw]);
    });
    let lo = Masks {
        width: gw,
        height: gh,
        count: 1,
        data: lo,
    };
    let s = gw as f32 / fw as f32;
    let bilinear: Vec<f32> = (0..fw * fh)
        .map(|i| {
            let (x, y) = ((i % fw) as f32, (i / fw) as f32);
            mask::sample(&lo, 0, (x + 0.5) * s - 0.5, (y + 0.5) * s - 0.5)
        })
        .collect();
    // Within 8 pixels of the edge (elsewhere both are right).
    let distance = |i: usize| {
        let (x, y) = ((i % fw) as f32 + 0.5, (i / fw) as f32 + 0.5);
        ((x - 131.3).hypot(y - 122.6) - 71.7).abs()
    };
    let near: Vec<usize> = (0..fw * fh).filter(|&i| distance(i) < 8.0).collect();
    let err = |v: &dyn Fn(usize) -> f32| {
        near.iter().map(|&i| (v(i) - truth[i]).abs()).sum::<f32>() / near.len() as f32
    };
    let wrong = |v: &dyn Fn(usize) -> f32| {
        near.iter()
            .filter(|&&i| (v(i) > 0.5) != (truth[i] > 0.5))
            .count()
    };
    let guided = |i: usize| f32::from(up[i]) / 255.0;
    // Away from the edge it's as sure as the mask.
    let far = if soft { 28.0 } else { 16.0 };
    for i in (0..fw * fh).filter(|&i| distance(i) > far) {
        assert!((guided(i) - truth[i]).abs() < 0.05, "{} at {i}", guided(i));
    }
    (
        (err(&guided), err(&|i| bilinear[i])),
        (wrong(&guided), wrong(&|i| bilinear[i])),
        near.len(),
    )
}

#[test]
fn guided_upsampling_follows_the_edge() {
    for (soft, isoluminant) in [(false, false), (true, false), (false, true), (true, true)] {
        let ((e_guided, e_bilinear), (w_guided, w_bilinear), n) = upsampled(soft, isoluminant);
        eprintln!(
            "soft {soft}, isoluminant {isoluminant}: mean error near the edge: guided {e_guided:.4}, bilinear \
             {e_bilinear:.4}; wrong side: {w_guided} vs {w_bilinear} of {n}"
        );
        let k = if soft { 0.5 } else { 0.2 };
        assert!(e_guided < k * e_bilinear, "{e_guided} vs {e_bilinear}");
        assert!(w_guided * 4 < w_bilinear, "{w_guided} vs {w_bilinear}");
    }
}

#[test]
fn the_fine_grid_keeps_to_its_budget() {
    // Small sources need none.
    assert_eq!(fine::grid(2560, 1707, 3), None);
    assert_eq!(fine::grid(1200, 800, 1), None);
    // A 45 MP export: full size with two masks, a little less with more.
    assert_eq!(fine::grid(8256, 5504, 2), Some((8256, 5504)));
    for layers in [3, 6, 12] {
        let (w, h) = fine::grid(8256, 5504, layers).unwrap();
        assert!(w * h * layers <= fine::BUDGET, "{layers}: {w}×{h}");
        assert!(w > mask::LONG, "{layers}: {w}×{h}");
        assert!(((w as f64 / h as f64) - 8256.0 / 5504.0).abs() < 1e-3);
    }
    // So many that it'd be no finer than the grid: the grid.
    assert_eq!(fine::grid(8256, 5504, 40), None);
}

/// A source wider than the grid: grey, with a red region left of a sharp
/// slanted edge.
fn wide() -> (Demosaiced, impl Fn(f32, f32) -> bool) {
    let (w, h) = (2700usize, 240usize);
    let left = |x: f32, y: f32| x < 1300.0 + 0.37 * y;
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = if left(x as f32 + 0.5, y as f32 + 0.5) {
                [0.8, 0.02, 0.02]
            } else {
                [0.05, 0.06, 0.05]
            };
            img.data[(y * w + x) * 3..][..3].copy_from_slice(&p);
        }
    }
    (Demosaiced::new(img, [1.0; 3], Referred::Display), left)
}

fn register() {
    // "Subject": the red pixels, but blurred over 5 pixels as a model's
    // mask would be.
    mask::register_finder(
        "",
        Box::new(|rgb, w, h, _what| {
            let red: Vec<f32> = rgb
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| if p[0] > 150 && p[1] < 80 { 1.0 } else { 0.0 })
                .collect();
            Ok((0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let mut s = 0.0;
                    for dx in -2i64..=2 {
                        let xx = (x as i64 + dx).clamp(0, w as i64 - 1) as usize;
                        s += red[y * w + xx];
                    }
                    (s / 5.0 * 255.0f32).round() as u8
                })
                .collect())
        }),
    );
}

fn layers(parts: Vec<Shape>) -> Settings {
    Settings {
        locals: vec![Local {
            id: "l1".into(),
            mask: Mask {
                parts: parts
                    .into_iter()
                    .enumerate()
                    .map(|(i, shape)| {
                        Part::Known(MaskPart {
                            op: if i == 0 { Op::Add } else { Op::Subtract },
                            invert: false,
                            shape,
                        })
                    })
                    .collect(),
            },
            adjust: LocalAdjust {
                exposure: 1.0,
                saturation: -40.0,
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn raw() -> shoebox_decode::raw::RawImage {
    shoebox_decode::raw::RawImage {
        width: 0,
        height: 0,
        cfa: [[0, 1], [1, 2]],
        data: Vec::new(),
        as_shot: None,
        matrices: Vec::new(),
        make: String::new(),
        model: String::new(),
    }
}

fn brush() -> Shape {
    Shape::Brush {
        strokes: vec![Stroke {
            points: vec![[0.1, 0.03], [0.3, 0.05]],
            radius: 0.01,
            feather: 0.0,
            flow: 1.0,
            erase: false,
        }],
    }
}

#[test]
fn big_sources_get_fine_masks_along_their_edges() {
    register();
    let (src, left) = wide();
    let s = layers(vec![Shape::Subject, brush()]);
    let f = cpu::frame(&raw(), &src, &s, &Options::default());
    let m = f.layers.masks.as_ref().unwrap();
    assert_eq!((m.width, m.height), (2700, 240), "the source's own pixels");
    assert_eq!(f.layers.scale, [1.0, 1.0]);
    let at = |x: usize, y: usize| mask::sample(m, 0, x as f32, y as f32);
    // The edge is where the picture's is, to the pixel, and sharp, though
    // the grid is coarser and the finder's mask soft (over 5 of its
    // pixels, so it steps by a fifth at most).
    for y in 0..240 {
        let edge = 1300.0 + 0.37 * (y as f32 + 0.5);
        let mut step = 0f32;
        for x in 1200..1500 {
            let v = at(x, y);
            let want = left(x as f32 + 0.5, y as f32 + 0.5);
            let d = (x as f32 + 0.5 - edge).abs();
            if d > 2.0 {
                assert_eq!(v > 0.5, want, "{v} at {x}, {y}");
            }
            if d > 12.0 {
                assert!(
                    (v - f32::from(u8::from(want))).abs() < 0.05,
                    "{v} at {x}, {y}"
                );
            }
            step = step.max(at(x - 1, y) - v);
        }
        assert!(step > 0.6, "row {y}: steps by {step} at most");
    }

    // The brush (taking away), rasterised at the fine size: hard edged.
    let soft = (60..160)
        .filter(|&y| (0.05..0.95).contains(&at(540, y)))
        .count();
    assert!(soft <= 2, "{soft}");
    assert!(at(540, 108) < 0.01 && at(540, 30) > 0.99);
    // Without an AI part the grid is as it was.
    let g = cpu::frame(&raw(), &src, &layers(vec![brush()]), &Options::default());
    let gm = g.layers.masks.as_ref().unwrap();
    assert_eq!((gm.width, gm.height), mask::grid(2700, 240));
}

#[test]
fn gpu_matches_the_cpu_on_fine_masks() {
    use shoebox_develop::gpu::{Output, Pipe, headless};
    let Some((device, queue)) = headless() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    register();
    let (src, _) = wide();
    let lum = Shape::Luminance {
        range: [0.0, 0.3],
        smooth: 0.1,
    };
    let s = layers(vec![Shape::Subject, brush(), lum]);
    let f = cpu::frame(&raw(), &src, &s, &Options::default());
    assert_eq!(f.layers.masks.as_ref().unwrap().width, 2700);
    let pipe = Pipe::new(&device, Output::Rgba32Float).unwrap();
    let gsrc = pipe.upload(&device, &queue, &src.rgb);
    let want = cpu::apply(&src.rgb, &f, true);
    let got = pipe.render(&device, &queue, &gsrc, &f, true).unwrap();
    let d = want
        .data
        .iter()
        .zip(&got.data)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(d <= 1e-4, "{d}");
    assert_ne!(
        want.data,
        cpu::apply(
            &src.rgb,
            &cpu::frame(&raw(), &src, &Settings::default(), &Options::default()),
            true
        )
        .data
    );
}

/// A 45 MP source with three AI masks and a brush: how long the fine
/// masks take, and how much they hold (`cargo test --release -p
/// shoebox-develop --test fine -- --ignored --nocapture`).
#[test]
#[ignore]
fn fine_masks_for_45_mp() {
    register();
    let (w, h) = (8256usize, 5504usize);
    let mut img = Rgb::new(w, h);
    img.data.chunks_mut(3).enumerate().for_each(|(i, p)| {
        let (x, y) = (i % w, i / w);
        let on = (x as f32 - 4000.0).hypot(y as f32 - 2700.0) < 1800.0;
        p.copy_from_slice(&if on { [0.8, 0.02, 0.02] } else { [0.2; 3] });
    });
    let src = Demosaiced::new(img, [1.0; 3], Referred::Display);
    let mut s = layers(vec![Shape::Subject, brush()]);
    for id in ["l2", "l3"] {
        let mut l = s.locals[0].clone();
        l.id = id.into();
        l.mask.parts[0] = Part::Known(MaskPart {
            op: Op::Add,
            invert: true,
            shape: Shape::Person,
        });
        s.locals.push(l);
    }
    let t = std::time::Instant::now();
    let f = cpu::frame(&raw(), &src, &s, &Options::default());
    let m = f.layers.masks.as_ref().unwrap();
    eprintln!(
        "{}×{} × {} masks ({} MB) in {:.2} s",
        m.width,
        m.height,
        m.count,
        m.data.len() >> 20,
        t.elapsed().as_secs_f32()
    );
    assert!(m.data.len() <= fine::BUDGET);
}
