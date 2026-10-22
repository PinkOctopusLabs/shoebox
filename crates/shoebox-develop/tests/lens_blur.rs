//! Lens Blur with a fake depth model: a textured red disc (near) on a fine
//! green and blue checkerboard (far). Focused on the disc, the disc stays
//! exactly as it was, the background blurs, and the disc's red doesn't
//! spread onto the background; a layer's Depth brings part of the
//! background into focus; highlights boost; the proxy and the full size
//! agree; edits read and write it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::{
    Bokeh, LensBlur, Local, LocalAdjust, Mask, MaskPart, Op, Part, Settings, Shape,
};
use shoebox_develop::mask::{Coarse, store};

/// Depth maps made.
static FINDS: AtomicUsize = AtomicUsize::new(0);
/// One test at a time (they share the model, the store and the count).
static ONE: Mutex<()> = Mutex::new(());
/// The fake model's cell, in picture pixels.
const CELL: usize = 4;

/// A fake depth model: per 4×4 cell, near (5) in proportion to its red
/// pixels, far (1) otherwise.
fn register() {
    shoebox_develop::depth::register(
        "fake-depth/1",
        Box::new(|rgb, w, h| {
            FINDS.fetch_add(1, Ordering::SeqCst);
            let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
            let mut data = vec![0f32; cw * ch];
            for (i, d) in data.iter_mut().enumerate() {
                let (cx, cy) = (i % cw, i / cw);
                let (mut hit, mut n) = (0.0, 0.0);
                for y in cy * CELL..((cy + 1) * CELL).min(h) {
                    for x in cx * CELL..((cx + 1) * CELL).min(w) {
                        let p = &rgb[(y * w + x) * 3..][..3];
                        n += 1.0;
                        if p[0] > 120 && p[1] < 100 {
                            hit += 1.0;
                        }
                    }
                }
                *d = 1.0 + 4.0 * hit / n;
            }
            Ok(Coarse {
                width: cw,
                height: ch,
                data,
            })
        }),
    );
}

fn fresh() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
    let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    register();
    let dir = tempfile::tempdir().unwrap();
    store::set(Some(store::Store::new(
        &dir.path().join("ai-masks"),
        store::DEFAULT_LIMIT,
    )));
    (one, dir)
}

/// The disc: centre and radius, as fractions of the width.
const DISC: (f32, f32, f32) = (0.5, 0.333, 0.145);

/// The test picture at `w`×`h` (linear values): the disc red with a fine
/// texture, the background a two-pixel checker of green and blue (`k`
/// pixels a square, so a larger picture is the same scene), with an
/// optional bright point.
fn picture(w: usize, h: usize, point: bool) -> Rgb {
    let mut img = Rgb::new(w, h);
    let k = (w / 240).max(1) * 2;
    let (cx, cy, r) = (DISC.0 * w as f32, DISC.1 * w as f32, DISC.2 * w as f32);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let p = if (fx - cx).hypot(fy - cy) < r {
                let t = if (x / k + y / k).is_multiple_of(2) {
                    1.1
                } else {
                    0.9
                };
                [0.6 * t, 0.04 * t, 0.04 * t]
            } else if (x / k + y / k).is_multiple_of(2) {
                [0.05, 0.3, 0.05]
            } else {
                [0.05, 0.1, 0.5]
            };
            img.data[(y * w + x) * 3..][..3].copy_from_slice(&p);
        }
    }
    if point {
        // A clipped highlight in the background's lower right.
        let (px, py) = (w * 4 / 5, h * 4 / 5);
        for y in py - 1..=py + 1 {
            for x in px - 1..=px + 1 {
                img.data[(y * w + x) * 3..][..3].copy_from_slice(&[1.0, 1.0, 1.0]);
            }
        }
    }
    img
}

fn source(img: Rgb) -> Demosaiced {
    Demosaiced::new(img, [1.0; 3], Referred::Display)
}

fn blurred(amount: f64) -> Settings {
    let mut s = Settings::default();
    s.modules.lens_blur = LensBlur {
        enabled: true,
        amount,
        // On the disc's centre (long edges: the width).
        focus: Some([f64::from(DISC.0), f64::from(DISC.1)]),
        ..Default::default()
    };
    s
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
        linear: None,
    }
}

fn render(src: &Demosaiced, s: &Settings, display: bool) -> Rgb {
    cpu::render_from(&raw(), src, s, &Options::default(), display)
}

/// Pixels of `img` whose centre is `lo`–`hi` pixels from the disc's edge
/// (negative inside).
fn ring(img: &Rgb, lo: f32, hi: f32) -> Vec<[f32; 3]> {
    let w = img.width as f32;
    let (cx, cy, r) = (DISC.0 * w, DISC.1 * w, DISC.2 * w);
    let mut out = Vec::new();
    for y in 0..img.height {
        for x in 0..img.width {
            let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy) - r;
            if d >= lo && d < hi {
                out.push(img.px(x, y));
            }
        }
    }
    out
}

fn spread(px: &[[f32; 3]]) -> f32 {
    let g: Vec<f32> = px.iter().map(|p| p[1] - p[2]).collect();
    let mean = g.iter().sum::<f32>() / g.len() as f32;
    (g.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / g.len() as f32).sqrt()
}

#[test]
fn focus_stays_sharp_and_the_background_blurs_without_halos() {
    let (_one, _dir) = fresh();
    let src = source(picture(240, 160, false));
    let sharp = render(&src, &Settings::default(), true);
    let s = blurred(100.0);
    let soft = render(&src, &s, true);
    // The disc, a few pixels in from its edge: exactly as it was.
    let a = ring(&sharp, -100.0, -4.0);
    let b = ring(&soft, -100.0, -4.0);
    let worst = a
        .iter()
        .zip(&b)
        .flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs()))
        .fold(0f32, f32::max);
    assert!(worst < 1e-6, "the disc changed by {worst}");
    // The background, well away from the disc: the checker is gone.
    let (far_a, far_b) = (ring(&sharp, 25.0, 1e9), ring(&soft, 25.0, 1e9));
    let (before, after) = (spread(&far_a), spread(&far_b));
    eprintln!("checker contrast {before:.3} → {after:.3}");
    assert!(after < 0.1 * before, "{before} → {after}");
    // No red beside the disc: the background's red (0.05 in linear light
    // everywhere) is the same next to it as far away.
    let linear = render(&src, &s, false);
    let red = |px: &[[f32; 3]]| px.iter().map(|p| p[0]).sum::<f32>() / px.len() as f32;
    let far = red(&ring(&linear, 25.0, 1e9));
    for (lo, hi) in [(2.0, 4.0), (4.0, 8.0), (8.0, 16.0)] {
        let near = red(&ring(&linear, lo, hi));
        eprintln!("red {lo}–{hi} px beside the disc {near:.4}, far away {far:.4}");
        assert!(
            near < far + 0.003,
            "a red halo {lo}–{hi} px out: {near} against {far}"
        );
    }
    // Found once, kept.
    let before = FINDS.load(Ordering::SeqCst);
    render(&src, &blurred(60.0), true);
    assert_eq!(
        FINDS.load(Ordering::SeqCst),
        before,
        "the depth map is kept"
    );
}

/// A larger picture, where the blur is gathered on a working copy a third
/// of its size: still no red spreads past a working pixel or two.
#[test]
fn no_halos_from_the_working_copy() {
    let (_one, _dir) = fresh();
    let src = source(picture(1200, 800, false));
    let s = blurred(100.0);
    let f = cpu::frame(&raw(), &src, &s, &Options::default());
    let st = f.lens_blur.as_ref().unwrap();
    assert_eq!(st.work, (400, 267));
    let linear = render(&src, &s, false);
    let red = |px: &[[f32; 3]]| px.iter().map(|p| p[0]).sum::<f32>() / px.len() as f32;
    let far = red(&ring(&linear, 100.0, 1e9));
    for (lo, hi) in [(3.0, 6.0), (6.0, 12.0), (12.0, 40.0)] {
        let near = red(&ring(&linear, lo, hi));
        eprintln!("red {lo}–{hi} px beside the disc {near:.4}, far away {far:.4}");
        assert!(
            near < far + 0.003,
            "a red halo {lo}–{hi} px out: {near} against {far}"
        );
    }
    // And the disc is still exactly itself inside.
    let sharp = render(&src, &Settings::default(), false);
    let (a, b) = (ring(&sharp, -1e9, -6.0), ring(&linear, -1e9, -6.0));
    let worst = a
        .iter()
        .zip(&b)
        .flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs()))
        .fold(0f32, f32::max);
    assert!(worst < 1e-6, "the disc changed by {worst}");
}

#[test]
fn off_by_default_and_old_edits_unchanged() {
    let (_one, _dir) = fresh();
    let src = source(picture(240, 160, false));
    let s = Settings::default();
    let f = cpu::frame(&raw(), &src, &s, &Options::default());
    assert!(f.lens_blur.is_none());
    // Off, with every other setting of it changed: nothing.
    let mut off = blurred(100.0);
    off.modules.lens_blur.enabled = false;
    assert!(
        cpu::frame(&raw(), &src, &off, &Options::default())
            .lens_blur
            .is_none()
    );
    // An edit without it reads as off and writes nothing for it.
    assert_eq!(s.to_value(), serde_json::json!({}));
    let v = serde_json::json!({"modules": {"exposure": {"ev": 0.5}}});
    let back = Settings::from_value(&v).unwrap();
    assert_eq!(back.modules.lens_blur, LensBlur::default());
}

#[test]
fn edits_round_trip() {
    let mut s = blurred(70.0);
    s.modules.lens_blur.bokeh = Bokeh::Blades;
    s.modules.lens_blur.boost = 40.0;
    s.modules.lens_blur.range = 25.0;
    s.locals.push(Local {
        id: "l1".into(),
        mask: Mask {
            parts: vec![Part::Known(MaskPart {
                op: Op::Add,
                invert: false,
                shape: Shape::Radial {
                    centre: [0.2, 0.2],
                    radii: [0.1, 0.1],
                    angle: 0.0,
                    feather: 0.5,
                },
            })],
        },
        adjust: LocalAdjust {
            depth: 35.0,
            ..Default::default()
        },
        ..Default::default()
    });
    let v = s.to_value();
    let lb = &v["modules"]["lens_blur"];
    assert_eq!(lb["enabled"], true);
    assert_eq!(lb["bokeh"], "blades");
    assert_eq!(lb["focus"], serde_json::json!([0.5, f64::from(DISC.1)]));
    assert_eq!(v["locals"][0]["adjust"]["depth"], 35.0);
    assert_eq!(Settings::from_value(&v).unwrap(), s);
    // Only what differs from the defaults is written.
    let mut on = Settings::default();
    on.modules.lens_blur.enabled = true;
    assert_eq!(
        on.to_value(),
        serde_json::json!({"modules": {"lens_blur": {"enabled": true}}})
    );
}

#[test]
fn a_layer_brings_the_background_into_focus() {
    let (_one, _dir) = fresh();
    let src = source(picture(240, 160, false));
    let sharp = render(&src, &Settings::default(), true);
    let mut s = blurred(100.0);
    // A hard circle over the upper left of the background, pushed all the
    // way near: there the background is as near as the disc.
    s.locals.push(Local {
        id: "l1".into(),
        mask: Mask {
            parts: vec![Part::Known(MaskPart {
                op: Op::Add,
                invert: false,
                shape: Shape::Radial {
                    centre: [0.15, 0.15],
                    radii: [0.1, 0.1],
                    angle: 0.0,
                    feather: 0.0,
                },
            })],
        },
        adjust: LocalAdjust {
            depth: 100.0,
            ..Default::default()
        },
        ..Default::default()
    });
    let soft = render(&src, &s, true);
    // Inside the circle (well in from its edge) the checker is sharp.
    let inside = |img: &Rgb| -> Vec<[f32; 3]> {
        let mut v = Vec::new();
        for y in 26..46 {
            for x in 26..46 {
                v.push(img.px(x, y));
            }
        }
        v
    };
    let (a, b) = (inside(&sharp), inside(&soft));
    let worst = a
        .iter()
        .zip(&b)
        .flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs()))
        .fold(0f32, f32::max);
    assert!(worst < 1e-6, "the refined part changed by {worst}");
    // Elsewhere in the background it's still blurred.
    let lower_right = |img: &Rgb| -> Vec<[f32; 3]> {
        let mut v = Vec::new();
        for y in 120..150 {
            for x in 190..230 {
                v.push(img.px(x, y));
            }
        }
        v
    };
    assert!(spread(&lower_right(&soft)) < 0.1 * spread(&lower_right(&sharp)));
}

#[test]
fn highlights_boost_brightens_bokeh() {
    let (_one, _dir) = fresh();
    let src = source(picture(240, 160, true));
    let light = |s: &Settings| -> f32 {
        let img = render(&src, s, false);
        let (px, py) = (192usize, 128usize);
        let mut sum = 0.0;
        for y in py - 8..=py + 8 {
            for x in px - 8..=px + 8 {
                let p = img.px(x, y);
                sum += p[0] + p[1] + p[2];
            }
        }
        sum
    };
    let plain = light(&blurred(100.0));
    let mut s = blurred(100.0);
    s.modules.lens_blur.boost = 100.0;
    let boosted = light(&s);
    eprintln!("light around the point {plain:.2} → {boosted:.2}");
    assert!(boosted > 1.5 * plain, "{plain} → {boosted}");
}

#[test]
fn the_proxy_and_full_size_agree() {
    let (_one, _dir) = fresh();
    let full = picture(480, 320, false);
    let proxy = cpu::downscale(&full, 240);
    let s = blurred(60.0);
    let a = render(&source(full).with_scale(1.0), &s, true);
    let b = render(&source(proxy).with_scale(0.5), &s, true);
    let a = cpu::downscale(&a, 240);
    // The background well away from the disc.
    let (ra, rb) = (ring(&a, 20.0, 1e9), ring(&b, 20.0, 1e9));
    let mean = ra
        .iter()
        .zip(&rb)
        .map(|(p, q)| (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>() / 3.0)
        .sum::<f32>()
        / ra.len() as f32;
    eprintln!("mean difference {mean:.4}");
    assert!(mean < 0.02, "{mean}");
}

#[test]
fn bladed_bokeh_is_a_hexagon() {
    let (_one, _dir) = fresh();
    // One bright point on black, far behind a near disc in the corner (so
    // the depth map has a range).
    let (w, h) = (240usize, 160usize);
    let mut img = Rgb::new(w, h);
    for y in 0..20 {
        for x in 0..20 {
            img.data[(y * w + x) * 3..][..3].copy_from_slice(&[0.6, 0.04, 0.04]);
        }
    }
    img.data[(100 * w + 150) * 3..][..3].copy_from_slice(&[1.0, 1.0, 1.0]);
    let src = source(img);
    let mut s = blurred(100.0);
    s.modules.lens_blur.focus = Some([10.0 / 240.0, 10.0 / 240.0]);
    let shape = |b: Bokeh| -> (f32, f32) {
        let mut s = s.clone();
        s.modules.lens_blur.bokeh = b;
        let img = render(&src, &s, false);
        // How far the disc reaches straight up, and along the diagonal.
        let reach = |dx: i32, dy: i32| -> f32 {
            let mut far = 0.0;
            for t in 1..20 {
                let (x, y) = (150 + dx * t, 100 + dy * t);
                if img.px(x as usize, y as usize)[1] > 1e-4 {
                    far = t as f32 * ((dx * dx + dy * dy) as f32).sqrt();
                }
            }
            far
        };
        (reach(0, -1), reach(1, 0))
    };
    let (up, across) = shape(Bokeh::Circle);
    assert!((up - across).abs() <= 1.0, "a circle: {up} {across}");
    // A hexagon with a corner at the top reaches further up than across
    // (to its flat side): cos 30° of it.
    let (up, across) = shape(Bokeh::Blades);
    eprintln!("hexagon up {up}, across {across}");
    assert!(across < up - 0.5, "{up} {across}");
}

fn max_diff(a: &Rgb, b: &Rgb) -> (f32, usize) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.data
        .iter()
        .zip(&b.data)
        .enumerate()
        .map(|(i, (x, y))| ((x - y).abs(), i / 3))
        .fold((0.0, 0), |m, d| if d.0 > m.0 { d } else { m })
}

/// The GPU's two passes and its mix in `texel` against the CPU: within
/// 1e-4, for the circle and the blades, boosted highlights, a refined
/// depth map, a working copy smaller than the source, a turned frame, and
/// the depth map shown.
#[test]
fn gpu_matches_the_cpu() {
    use shoebox_develop::gpu::{Output, Pipe, headless};
    let (_one, _dir) = fresh();
    let Some((device, queue)) = headless() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let pipe = Pipe::new(&device, Output::Rgba32Float).unwrap();
    let small = source(picture(240, 160, true));
    let large = source(picture(720, 480, true));
    let mut cases: Vec<(&str, &Demosaiced, Settings, bool)> = Vec::new();
    cases.push(("circle", &small, blurred(100.0), false));
    let mut s = blurred(80.0);
    s.modules.lens_blur.bokeh = Bokeh::Blades;
    s.modules.lens_blur.boost = 70.0;
    s.modules.lens_blur.range = 30.0;
    cases.push(("blades, boosted", &small, s, false));
    let mut s = blurred(100.0);
    s.locals.push(Local {
        id: "l1".into(),
        mask: Mask {
            parts: vec![Part::Known(MaskPart {
                op: Op::Add,
                invert: false,
                shape: Shape::Radial {
                    centre: [0.15, 0.15],
                    radii: [0.1, 0.1],
                    angle: 0.0,
                    feather: 0.6,
                },
            })],
        },
        adjust: LocalAdjust {
            depth: 60.0,
            exposure: 0.3,
            ..Default::default()
        },
        ..Default::default()
    });
    cases.push(("refined", &small, s, false));
    let mut s = blurred(100.0);
    s.modules.lens_blur.focus = None;
    cases.push(("working copy a third the size", &large, s.clone(), false));
    s.geometry.rotate = 3.0;
    cases.push(("turned", &large, s, false));
    cases.push(("depth shown", &small, blurred(50.0), true));
    for (name, src, s, depth_view) in &cases {
        let mut f = cpu::frame(&raw(), src, s, &Options::default());
        f.depth_view = *depth_view;
        let st = f.lens_blur.as_ref().expect("the blur is on");
        eprintln!("{name}: working copy {:?}, {} taps", st.work, st.count);
        let gsrc = pipe.upload(&device, &queue, &src.rgb);
        for display in [true, false] {
            let want = cpu::apply(&src.rgb, &f, display);
            let got = pipe.render(&device, &queue, &gsrc, &f, display).unwrap();
            let (d, at) = max_diff(&want, &got);
            let scale = if display {
                1.0
            } else {
                want.data.iter().fold(1.0f32, |m, v| m.max(v.abs()))
            };
            // A turned frame samples between pixels: float rounding there
            // (as in the develop pipe's own tests).
            let tolerance = if s.geometry.rotate != 0.0 { 1e-3 } else { 1e-4 };
            assert!(
                d / scale <= tolerance,
                "{name} (display {display}): differs by {d} at pixel {at}: cpu {:?} gpu {:?}",
                want.px(at % want.width, at / want.width),
                got.px(at % got.width, at / got.width)
            );
        }
    }
}

/// An export's whole render on the CPU at 24 MP (timings; run in release
/// with `--ignored`).
#[test]
#[ignore = "timing: a 24 MP render on the CPU"]
fn a_full_size_render_on_the_cpu() {
    let (_one, _dir) = fresh();
    let src = source(picture(6048, 4032, true));
    for amount in [100.0, 50.0, 10.0] {
        let s = blurred(amount);
        let t = std::time::Instant::now();
        let f = cpu::frame(&raw(), &src, &s, &Options::default());
        let depth = t.elapsed();
        let none = Settings::default();
        let pf = cpu::frame(&raw(), &src, &none, &Options::default());
        let t = std::time::Instant::now();
        let plain = cpu::apply_from(&src, &none, &pf, true);
        let without = t.elapsed();
        let t = std::time::Instant::now();
        let out = cpu::apply_from(&src, &s, &f, true);
        eprintln!(
            "amount {amount}: depth map {depth:?}; render {:?} (without Lens Blur {without:?})",
            t.elapsed()
        );
        assert_eq!((out.width, plain.width), (6048, 6048));
    }
}

/// A 2560×1707 proxy with Lens Blur strong, middling and light: both
/// passes and the develop pass, per frame.
#[test]
fn a_blurred_proxy_renders_quickly() {
    use shoebox_develop::gpu::{Output, Pipe, headless};
    let (_one, _dir) = fresh();
    let Some((device, queue)) = headless() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let src = source(picture(2560, 1707, true));
    let pipe = Pipe::new(&device, Output::Rgba16Float).unwrap();
    let gsrc = pipe.upload(&device, &queue, &src.rgb);
    for amount in [100.0, 50.0, 10.0] {
        let mut s = blurred(amount);
        s.modules.lens_blur.boost = 50.0;
        let t = std::time::Instant::now();
        let f = cpu::frame(&raw(), &src, &s, &Options::default());
        let first = t.elapsed();
        let st = f.lens_blur.as_ref().unwrap();
        let out = pipe.output_texture(&device, f.width as u32, f.height as u32);
        let view = out.create_view(&Default::default());
        let run = || {
            let mut enc = device.create_command_encoder(&Default::default());
            pipe.encode(&device, &mut enc, &gsrc, &f, true, &view);
            queue.submit([enc.finish()]);
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .unwrap();
        };
        run();
        let t = std::time::Instant::now();
        for _ in 0..10 {
            run();
        }
        let per = t.elapsed() / 10;
        let t = std::time::Instant::now();
        let _ = cpu::lens_blur::blur(&src.rgb, st);
        let cpu_blur = t.elapsed();
        eprintln!(
            "amount {amount}: working copy {:?}, {} taps; first frame (depth map) {first:?}; \
             GPU {per:?} per frame; CPU blur {cpu_blur:?}",
            st.work, st.count,
        );
        // The budget is the M2 Max's (7–21 ms there); CI's virtual GPU takes
        // two or three times longer, so there the time is only reported.
        if std::env::var_os("CI").is_none() {
            assert!(per.as_millis() < 33, "{per:?}");
        }
    }
}
