//! The GPU live pipe against the CPU reference: within 1e-4 on synthetic
//! inputs (gradients, colour ramps, a checkerboard, highlights above 1),
//! across settings. Skipped where there's no GPU adapter (Linux CI).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_decode::raw::{ColorMatrix, RawImage};
use shoebox_develop::cpu::{self, Demosaiced, Options, Rgb, Target};
use shoebox_develop::edit::{Settings, WbMode};
use shoebox_develop::gpu::{Output, Pipe, headless};

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

/// Gradients, colour ramps and a checkerboard, from black to well above 1.
fn synthetic(w: usize, h: usize) -> Rgb {
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let px = match (y * 4) / h {
                0 => [u * u * 4.0; 3],                    // grey ramp up to 4
                1 => [u * 2.0, v * 1.5, (1.0 - u) * 0.8], // colour ramps
                2 => {
                    let on = ((x / 4) + (y / 4)) % 2 == 0; // checkerboard
                    if on {
                        [0.9, 0.2, 0.05]
                    } else {
                        [0.01, 0.05, 0.3]
                    }
                }
                _ => [u.powf(3.0) * 8.0, 0.0, u * 0.001], // saturated, tiny
            };
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&px);
        }
    }
    img
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

#[test]
fn gpu_matches_the_cpu_reference() {
    let Some((device, queue)) = headless() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let raw = zf();
    let src = Demosaiced::new(
        synthetic(160, 120),
        [1.1425778, 1.0, 1.7753916],
        shoebox_develop::cpu::Referred::Scene,
    );
    let pipe = Pipe::new(&device, Output::Rgba32Float).unwrap();
    let gsrc = pipe.upload(&device, &queue, &src.rgb);

    let mut cases: Vec<(&str, Settings, Options)> = Vec::new();
    let base = Settings::default();
    let o = Options::default();
    cases.push(("defaults", base.clone(), o));
    let mut s = base.clone();
    s.modules.exposure.ev = 1.3;
    s.modules.display_transform.contrast = 40.0;
    s.modules.display_transform.white_ev = -1.0;
    s.modules.display_transform.black_ev = 2.0;
    cases.push(("exposure and curve", s, o));
    let mut s = base.clone();
    s.modules.display_transform.preserve_hue = false;
    cases.push(("per-channel", s, o));
    let mut s = base.clone();
    s.modules.white_balance.mode = WbMode::Custom;
    s.modules.white_balance.temp = Some(3400.0);
    s.modules.white_balance.tint = Some(15.0);
    cases.push(("white balance", s, o));
    let mut s = base.clone();
    s.geometry.rotate = -4.5;
    s.geometry.crop = [0.1, 0.05, 0.85, 0.9];
    cases.push((
        "rotate and crop",
        s,
        Options {
            orientation: 6,
            ..o
        },
    ));
    let mut s = base.clone();
    s.modules.colour.vibrance = 40.0;
    s.modules.colour.saturation = -20.0;
    s.modules.colour.hue = [30.0, -20.0, 10.0, 50.0, -40.0, 60.0, -10.0, 25.0];
    s.modules.colour.hsl_saturation = [50.0, 20.0, -30.0, 10.0, 0.0, -60.0, 40.0, 80.0];
    s.modules.colour.luminance = [-50.0, 30.0, 0.0, 20.0, -10.0, 60.0, -30.0, 10.0];
    s.modules.calibration.shadows_tint = 40.0;
    s.modules.calibration.red_hue = 30.0;
    s.modules.calibration.blue_saturation = -50.0;
    cases.push(("colour and calibration", s, o));
    let mut s = base.clone();
    s.modules.black_and_white.enabled = true;
    s.modules.black_and_white.mix = [40.0, 20.0, -30.0, 10.0, 0.0, -60.0, 40.0, 80.0];
    cases.push(("black and white", s, o));
    let mut s = base.clone();
    s.modules.tone_curve.regions = [30.0, -20.0, 15.0, -40.0];
    s.modules.tone_curve.rgb = vec![[0.3, 0.25], [0.7, 0.8]];
    s.modules.tone_curve.blue = vec![[0.5, 0.55]];
    cases.push(("tone curve", s.clone(), o));
    s.modules.display_transform.preserve_hue = false;
    cases.push(("tone curve, per channel", s, o));
    let mut s = base.clone();
    s.modules.highlights_shadows.shadows = 70.0;
    s.modules.highlights_shadows.highlights = -60.0;
    s.modules.clarity.amount = 50.0;
    cases.push(("highlights, shadows, clarity", s.clone(), o));
    s.modules.dehaze.amount = 60.0;
    s.geometry.rotate = 3.0;
    cases.push(("with dehaze, rotated", s.clone(), o));
    s.modules.dehaze.amount = -40.0;
    s.geometry.rotate = 0.0;
    cases.push(("adding haze", s, o));
    cases.push((
        "display p3, orientation 3",
        base.clone(),
        Options {
            orientation: 3,
            target: Target::DisplayP3,
            ..o
        },
    ));

    let mut s = base.clone();
    s.geometry.transform.vertical = 40.0;
    s.geometry.transform.horizontal = -20.0;
    s.geometry.transform.aspect = 30.0;
    s.geometry.rotate = 1.5;
    cases.push((
        "perspective",
        s,
        Options {
            orientation: 6,
            ..o
        },
    ));
    let mut s = base.clone();
    s.modules.defringe.purple = 80.0;
    s.modules.defringe.green = 60.0;
    cases.push(("defringe", s, o));
    // Local adjustments: every kind of part and every slider.
    let mut s = base.clone();
    s.modules.highlights_shadows.shadows = 20.0;
    s.locals = local_layers();
    cases.push(("local adjustments", s.clone(), o));
    cases.push(("local adjustments, mask shown", s.clone(), o));
    cases.push(("local adjustments, mask alone", s.clone(), o));
    s.geometry.rotate = 2.5;
    s.geometry.crop = [0.05, 0.1, 0.9, 0.95];
    cases.push((
        "local adjustments, turned",
        s,
        Options {
            orientation: 6,
            ..o
        },
    ));
    // Point Color: an orange and a blue, with a grey's for good measure.
    let mut s = base.clone();
    let point = |lightness, chroma, hue, hue_shift, saturation, luminance| {
        shoebox_develop::edit::ColourPoint {
            lightness,
            chroma,
            hue,
            hue_shift,
            saturation,
            luminance,
            ..Default::default()
        }
    };
    s.modules.point_colour.points = vec![
        point(0.7, 0.15, 45.0, 40.0, 50.0, -30.0),
        point(0.4, 0.12, 260.0, -60.0, -80.0, 40.0),
        point(0.6, 0.0, 0.0, 0.0, 0.0, 50.0),
    ];
    cases.push(("point colour", s.clone(), o));
    cases.push(("point colour, shown", s, o));
    // A camera match look for this camera (the test raw's model, "Z f").
    let mut table = shoebox_develop::profile::Lut3::identity(shoebox_develop::profile::SIZE);
    for c in &mut table.data {
        let m = (c[0] + c[1] + c[2]) / 3.0;
        *c = c.map(|v| (m + (v - m) * 1.3 + 0.03 * (1.0 - v)).clamp(0.0, 1.0));
    }
    shoebox_develop::profile::register(&shoebox_develop::profile::Profile {
        camera: "Z f".into(),
        look: table,
        pairs: 0,
        delta_e: None,
    });
    let mut s = base.clone();
    s.modules.input_colour.profile = shoebox_develop::edit::Profile::CameraMatch;
    cases.push(("camera match", s, o));
    // Lens corrections need the photo's shot: a second source with one.
    let lensed = src
        .clone()
        .with_lens(Some(std::sync::Arc::new(shoebox_develop::lens::Shot {
            make: "NIKON CORPORATION".into(),
            model: "NIKON Z f".into(),
            lens: "NIKKOR Z 40mm f/2".into(),
            focal: 40.0,
            aperture: 4.0,
            camera_data: shoebox_decode::nef::LensData {
                distortion: Some([-0.04, 0.01, 0.0]),
                vignette: Some([0.6, -0.2, 0.05]),
            },
        })));
    let mut s = base.clone();
    s.modules.lens.enabled = true;
    s.modules.highlights_shadows.shadows = 40.0;
    cases.push(("lens corrections", s.clone(), o));
    s.geometry.rotate = 2.0;
    cases.push(("lens corrections, rotated", s, o));

    for (name, s, o) in &cases {
        let src = if name.starts_with("lens") {
            &lensed
        } else {
            &src
        };
        let mut f = cpu::frame(&raw, src, s, o);
        if name.ends_with("mask shown") {
            f.mask_view = Some((1, false));
        } else if name.ends_with("mask alone") {
            f.mask_view = Some((0, true));
        }
        if name.ends_with("colour, shown") {
            f.point_view = Some(0);
        }
        if name.starts_with("local") {
            assert_eq!(f.layers.list.len(), 2, "{name}");
        }
        for display in [true, false] {
            let want = cpu::apply(&src.rgb, &f, display);
            let got = pipe.render(&device, &queue, &gsrc, &f, display).unwrap();
            let (d, at) = max_diff(&want, &got);
            // Scene values reach ~30; compare them relatively.
            let scale = if display {
                1.0
            } else {
                want.data.iter().fold(1.0f32, |m, v| m.max(v.abs()))
            };
            // A rotated sample position differs by float rounding (the GPU
            // fuses the map's multiply-adds), and a checkerboard edge
            // amplifies that: 1e-3 there, a quarter of an 8-bit step.
            let tolerance = if s.geometry.rotate != 0.0 || !s.geometry.transform.is_identity() {
                1e-3
            } else {
                1e-4
            };
            assert!(
                d / scale <= tolerance,
                "{name} (display {display}): differs by {d} at pixel {at} (scale {scale}): cpu {:?} gpu {:?}",
                want.px(at % want.width, at / want.width),
                got.px(at % got.width, at / got.width)
            );
        }
    }
}

#[test]
fn a_proxy_frame_renders_quickly() {
    let Some((device, queue)) = headless() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    // A 2560×1707 proxy (a 24 MP frame's), rendered repeatedly: the slider
    // path. Warm up once, then time.
    let raw = zf();
    let src = Demosaiced::new(
        synthetic(2560, 1707),
        [1.1425778, 1.0, 1.7753916],
        shoebox_develop::cpu::Referred::Scene,
    );
    let pipe = Pipe::new(&device, Output::Rgba16Float).unwrap();
    let gsrc = pipe.upload(&device, &queue, &src.rgb);
    let f = cpu::frame(&raw, &src, &Settings::default(), &Options::default());
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
    eprintln!("proxy render: {per:?} per frame");
    // The spec's target is 33 ms per frame for the whole slider path.
    assert!(per.as_millis() < 33, "{per:?}");
}

#[test]
fn histogram_and_clipping_match_the_cpu() {
    let Some((device, queue)) = headless() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let raw = zf();
    let src = Demosaiced::new(
        synthetic(200, 150),
        [1.1425778, 1.0, 1.7753916],
        shoebox_develop::cpu::Referred::Scene,
    );
    let pipe = Pipe::new(&device, Output::Rgba32Float).unwrap();
    let gsrc = pipe.upload(&device, &queue, &src.rgb);
    let mut f = cpu::frame(&raw, &src, &Settings::default(), &Options::default());
    let (w, h) = (f.width as u32, f.height as u32);

    // Histogram: the same counts, give or take a value on a rounding edge.
    let out = pipe.output_texture(&device, w, h);
    let view = out.create_view(&Default::default());
    let mut hist = shoebox_develop::gpu::Histogram::new(&device).unwrap();
    let mut enc = device.create_command_encoder(&Default::default());
    pipe.encode(&device, &mut enc, &gsrc, &f, true, &view);
    assert!(hist.encode(&device, &mut enc, &view, w, h));
    queue.submit([enc.finish()]);
    hist.start_read();
    let got = hist.poll(&device, true).expect("histogram read back");
    let want = cpu::histogram(&cpu::apply(&src.rgb, &f, true));
    for c in 0..4 {
        let total: u32 = got[c].iter().sum();
        assert_eq!(total, w * h, "channel {c}");
        let off: u32 = (0..256).map(|i| got[c][i].abs_diff(want[c][i])).sum();
        assert!(off <= w * h / 500, "channel {c}: {off} counts differ");
    }

    // Clipping shown the same way on both.
    f.warnings = true;
    let a = cpu::apply(&src.rgb, &f, true);
    let b = pipe.render(&device, &queue, &gsrc, &f, true).unwrap();
    let red = |img: &Rgb| {
        img.data
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|p| **p == [1.0, 0.1, 0.1])
            .count()
    };
    assert!(red(&a) > 0, "the synthetic image clips");
    assert!(
        red(&a).abs_diff(red(&b)) <= (w * h / 500) as usize,
        "{} vs {}",
        red(&a),
        red(&b)
    );
}

#[test]
fn display_referred_sources_match_too() {
    let Some((device, queue)) = headless() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let px: Vec<u8> = (0..160 * 120 * 3).map(|i| ((i * 37) % 256) as u8).collect();
    let src = cpu::from_display(160, 120, &px, Target::DisplayP3);
    let raw = zf();
    let pipe = Pipe::new(&device, Output::Rgba32Float).unwrap();
    let gsrc = pipe.upload(&device, &queue, &src.rgb);
    let mut s = Settings::default();
    for step in 0..3 {
        match step {
            1 => {
                s.modules.exposure.ev = 0.7;
                s.modules.display_transform.contrast = 35.0;
                s.modules.display_transform.black_ev = 1.0;
            }
            2 => {
                s.modules.white_balance.mode = WbMode::Tungsten;
                s.modules.display_transform.preserve_hue = false;
            }
            _ => {}
        }
        let f = cpu::frame(
            &raw,
            &src,
            &s,
            &Options {
                target: Target::DisplayP3,
                ..Default::default()
            },
        );
        let want = cpu::apply(&src.rgb, &f, true);
        let got = pipe.render(&device, &queue, &gsrc, &f, true).unwrap();
        let (d, at) = max_diff(&want, &got);
        assert!(d <= 1e-4, "step {step}: {d} at {at}");
    }
}

/// Soft proofing a saturated P3 render to sRGB: CPU and GPU agree, every
/// colour shown fits in sRGB, and the gamut warning marks the same pixels.
#[test]
fn soft_proofing_matches_the_cpu() {
    let Some((device, queue)) = headless() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let raw = zf();
    let src = Demosaiced::new(
        synthetic(200, 150),
        [1.1425778, 1.0, 1.7753916],
        shoebox_develop::cpu::Referred::Scene,
    );
    let pipe = Pipe::new(&device, Output::Rgba32Float).unwrap();
    let gsrc = pipe.upload(&device, &queue, &src.rgb);
    let mut s = Settings::default();
    s.modules.colour.saturation = 80.0;
    let o = Options {
        target: Target::DisplayP3,
        ..Options::default()
    };
    let mut f = cpu::frame(&raw, &src, &s, &o);
    let plain = cpu::apply(&src.rgb, &f, true);
    f.display.proof = Some(cpu::Proof::new(Target::Srgb));
    let a = cpu::apply(&src.rgb, &f, true);
    let b = pipe.render(&device, &queue, &gsrc, &f, true).unwrap();
    let (d, i) = max_diff(&a, &b);
    assert!(d < 1e-4, "{d} at {i}");
    assert_ne!(a.data, plain.data, "proofing changed something");
    // Shown in P3, every proofed colour is inside sRGB.
    let to_srgb = shoebox_develop::colour::mul(
        &Target::Srgb.from_rec2020(),
        &shoebox_develop::colour::inv(&Target::DisplayP3.from_rec2020()).unwrap(),
    );
    for p in a.data.as_chunks::<3>().0 {
        let lin = p.map(|v| f64::from(cpu::srgb_eotf(v)));
        let srgb = shoebox_develop::colour::mulv(&to_srgb, lin);
        assert!(
            srgb.iter().all(|&v| (-2e-3..=1.0 + 2e-3).contains(&v)),
            "{p:?} → {srgb:?}"
        );
    }
    // The warning: the same pixels, give or take rounding edges.
    f.gamut_warning = true;
    let a = cpu::apply(&src.rgb, &f, true);
    let b = pipe.render(&device, &queue, &gsrc, &f, true).unwrap();
    let marked = |img: &Rgb| {
        img.data
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|p| **p == cpu::GAMUT)
            .count()
    };
    let (w, h) = (f.width, f.height);
    assert!(marked(&a) > 0 && marked(&a) < w * h, "{}", marked(&a));
    assert!(
        marked(&a).abs_diff(marked(&b)) <= w * h / 500,
        "{} vs {}",
        marked(&a),
        marked(&b)
    );
}

/// Two layers using every part and slider.
fn local_layers() -> Vec<shoebox_develop::edit::Local> {
    use shoebox_develop::edit::{Local, LocalAdjust, Mask, MaskPart, Op, Part, Shape, Stroke};
    let part = |op, invert, shape| Part::Known(MaskPart { op, invert, shape });
    vec![
        Local {
            id: "l1".into(),
            mask: Mask {
                parts: vec![
                    part(
                        Op::Add,
                        false,
                        Shape::Linear {
                            from: [0.1, 0.2],
                            to: [0.8, 0.6],
                        },
                    ),
                    part(
                        Op::Add,
                        false,
                        Shape::Brush {
                            strokes: vec![Stroke {
                                points: vec![[0.2, 0.6], [0.5, 0.65], [0.7, 0.5]],
                                radius: 0.06,
                                feather: 0.5,
                                flow: 0.8,
                                erase: false,
                            }],
                        },
                    ),
                    part(
                        Op::Intersect,
                        true,
                        Shape::Luminance {
                            range: [0.0, 0.2],
                            smooth: 0.1,
                        },
                    ),
                ],
            },
            adjust: LocalAdjust {
                exposure: 0.7,
                contrast: 30.0,
                temperature: 40.0,
                tint: -20.0,
                saturation: 25.0,
                shadows: 40.0,
                highlights: -50.0,
                clarity: 30.0,
                dehaze: 20.0,
            },
            ..Default::default()
        },
        Local {
            id: "l2".into(),
            amount: 70.0,
            mask: Mask {
                parts: vec![
                    part(
                        Op::Add,
                        false,
                        Shape::Radial {
                            centre: [0.6, 0.4],
                            radii: [0.3, 0.15],
                            angle: 20.0,
                            feather: 0.6,
                        },
                    ),
                    part(
                        Op::Subtract,
                        false,
                        Shape::Colour {
                            hue: 30.0,
                            width: 25.0,
                        },
                    ),
                ],
            },
            adjust: LocalAdjust {
                exposure: -0.5,
                saturation: -60.0,
                ..Default::default()
            },
            ..Default::default()
        },
    ]
}
