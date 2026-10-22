//! Local adjustments: mask parts rasterise where they're drawn, combine by
//! their ops, and a layer changes only what its mask covers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::{
    EditDoc, Local, LocalAdjust, Mask, MaskPart, Op, Part, Settings, Shape, Source, Stroke,
};
use shoebox_develop::mask::{self, Tones};

fn part(op: Op, shape: Shape) -> Part {
    Part::Known(MaskPart {
        op,
        invert: false,
        shape,
    })
}

fn layer(parts: Vec<Part>) -> Local {
    Local {
        id: "l1".into(),
        mask: Mask { parts },
        adjust: LocalAdjust {
            exposure: 1.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A 200×100 grid's value at a point given in the long edge's units.
fn at(m: &mask::Masks, layer: usize, p: [f64; 2]) -> f32 {
    let (x, y) = (p[0] * 200.0 - 0.5, p[1] * 200.0 - 0.5);
    mask::sample(m, layer, x as f32, y as f32)
}

fn raster(l: &Local, tones: Option<&Tones>) -> mask::Masks {
    mask::rasterise(&[l], 200, 100, [1.0, 0.5], tones)
}

#[test]
fn linear_and_radial_parts_cover_what_they_should() {
    let lin = layer(vec![part(
        Op::Add,
        Shape::Linear {
            from: [0.2, 0.25],
            to: [0.6, 0.25],
        },
    )]);
    let m = raster(&lin, None);
    assert!(at(&m, 0, [0.1, 0.25]) > 0.99, "full before the start");
    assert!((at(&m, 0, [0.4, 0.1]) - 0.5).abs() < 0.02, "half way");
    assert!(at(&m, 0, [0.7, 0.4]) < 0.01, "none past the end");

    let radial = layer(vec![part(
        Op::Add,
        Shape::Radial {
            centre: [0.5, 0.25],
            radii: [0.3, 0.1],
            angle: 0.0,
            feather: 0.5,
        },
    )]);
    let m = raster(&radial, None);
    assert!(at(&m, 0, [0.5, 0.25]) > 0.99);
    assert!(
        at(&m, 0, [0.62, 0.25]) > 0.99,
        "inside the unfeathered part"
    );
    assert!(at(&m, 0, [0.85, 0.25]) < 0.01, "outside the ellipse");
    assert!(at(&m, 0, [0.5, 0.4]) < 0.01, "the short axis is short");
    // Turned a quarter, the long axis is vertical.
    let mut turned = radial.clone();
    if let Part::Known(p) = &mut turned.mask.parts[0]
        && let Shape::Radial { angle, radii, .. } = &mut p.shape
    {
        *angle = 90.0;
        *radii = [0.2, 0.05];
    }
    let m = raster(&turned, None);
    assert!(at(&m, 0, [0.5, 0.38]) > 0.5, "long axis now vertical");
    assert!(at(&m, 0, [0.6, 0.25]) < 0.01);
}

#[test]
fn brush_strokes_paint_build_up_and_erase() {
    let stroke = |points: Vec<[f64; 2]>, flow, erase| Stroke {
        points,
        radius: 0.05,
        feather: 0.0,
        flow,
        erase,
    };
    let mut l = layer(vec![part(
        Op::Add,
        Shape::Brush {
            strokes: vec![
                // Over itself: still one stroke's flow.
                stroke(vec![[0.1, 0.25], [0.5, 0.25], [0.1, 0.25]], 0.5, false),
                stroke(vec![[0.3, 0.25]], 0.5, false),
            ],
        },
    )]);
    let m = raster(&l, None);
    assert!((at(&m, 0, [0.2, 0.25]) - 0.5).abs() < 0.01);
    assert!(
        (at(&m, 0, [0.3, 0.25]) - 0.75).abs() < 0.01,
        "two strokes build up"
    );
    assert!(at(&m, 0, [0.3, 0.4]) < 0.01, "nothing beyond the radius");
    if let Part::Known(p) = &mut l.mask.parts[0]
        && let Shape::Brush { strokes } = &mut p.shape
    {
        strokes.push(stroke(vec![[0.3, 0.25]], 1.0, true));
    }
    let m = raster(&l, None);
    assert!(at(&m, 0, [0.3, 0.25]) < 0.01, "erased");
    assert!(
        (at(&m, 0, [0.45, 0.25]) - 0.5).abs() < 0.01,
        "the rest stays"
    );
}

#[test]
fn ranges_and_ops_combine() {
    // Brightness rises left to right; hue is 30° on the top half, 200°
    // below.
    let mut t = Tones::default();
    for y in 0..100 {
        for x in 0..200 {
            t.shown.push(x as f32 / 199.0);
            t.hue.push(if y < 50 { 30.0 } else { 200.0 });
            t.chroma.push(0.5);
        }
    }
    let lum = Shape::Luminance {
        range: [0.6, 1.0],
        smooth: 0.1,
    };
    let m = raster(&layer(vec![part(Op::Add, lum.clone())]), Some(&t));
    assert!(at(&m, 0, [0.9, 0.1]) > 0.99);
    assert!(at(&m, 0, [0.3, 0.1]) < 0.01);
    let colour = Shape::Colour {
        hue: 25.0,
        width: 20.0,
    };
    let m = raster(&layer(vec![part(Op::Add, colour.clone())]), Some(&t));
    assert!(at(&m, 0, [0.5, 0.1]) > 0.99);
    assert!(at(&m, 0, [0.5, 0.4]) < 0.01);
    // Bright and orange: intersect; bright but not orange: subtract.
    let both = layer(vec![
        part(Op::Add, lum.clone()),
        part(Op::Intersect, colour.clone()),
    ]);
    let m = raster(&both, Some(&t));
    assert!(at(&m, 0, [0.9, 0.1]) > 0.99);
    assert!(at(&m, 0, [0.9, 0.4]) < 0.01);
    assert!(at(&m, 0, [0.3, 0.1]) < 0.01);
    let minus = layer(vec![part(Op::Add, lum), part(Op::Subtract, colour)]);
    let m = raster(&minus, Some(&t));
    assert!(at(&m, 0, [0.9, 0.1]) < 0.01);
    assert!(at(&m, 0, [0.9, 0.4]) > 0.99);
    // Inverted, and the layer's amount.
    let mut inv = layer(vec![Part::Known(MaskPart {
        op: Op::Add,
        invert: true,
        shape: Shape::Luminance {
            range: [0.6, 1.0],
            smooth: 0.1,
        },
    })]);
    inv.amount = 40.0;
    let m = raster(&inv, Some(&t));
    assert!((at(&m, 0, [0.3, 0.1]) - 0.4).abs() < 0.01);
    assert!(at(&m, 0, [0.9, 0.1]) < 0.01);
}

fn source() -> Demosaiced {
    let mut img = Rgb::new(120, 80);
    for (i, v) in img.data.iter_mut().enumerate() {
        *v = 0.05 + (i % 360) as f32 / 1200.0;
    }
    Demosaiced::new(img, [1.0; 3], Referred::Display)
}

#[test]
fn a_layer_changes_only_what_its_mask_covers() {
    let raw = shoebox_decode::raw::RawImage {
        width: 0,
        height: 0,
        cfa: [[0, 1], [1, 2]],
        data: Vec::new(),
        as_shot: None,
        matrices: Vec::new(),
        make: String::new(),
        model: String::new(),
    };
    let src = source();
    let o = Options::default();
    let plain = cpu::render_from(&raw, &src, &Settings::default(), &o, false);
    // Exposure +1 on the left half (the image is 1 × 2/3 long edges).
    let mut s = Settings {
        locals: vec![layer(vec![part(
            Op::Add,
            Shape::Brush {
                strokes: vec![Stroke {
                    points: vec![[0.25, 0.0], [0.25, 0.67]],
                    radius: 0.25,
                    feather: 0.0,
                    flow: 1.0,
                    erase: false,
                }],
            },
        )])],
        ..Default::default()
    };
    let edited = cpu::render_from(&raw, &src, &s, &o, false);
    let (l, r) = ((10, 40), (110, 40));
    for c in 0..3 {
        let k = edited.px(l.0, l.1)[c] / plain.px(l.0, l.1)[c];
        assert!((k - 2.0).abs() < 1e-3, "left doubled: {k}");
        assert_eq!(
            edited.px(r.0, r.1)[c],
            plain.px(r.0, r.1)[c],
            "right untouched"
        );
    }
    // Switched off, or with nothing to do, it does nothing.
    s.locals[0].enabled = false;
    assert_eq!(cpu::render_from(&raw, &src, &s, &o, false).data, plain.data);
    s.locals[0].enabled = true;
    s.locals[0].adjust = LocalAdjust::default();
    assert_eq!(cpu::render_from(&raw, &src, &s, &o, false).data, plain.data);
}

#[test]
fn layers_round_trip_and_keep_parts_from_newer_builds() {
    let mut s = Settings::default();
    let mut l = layer(vec![part(
        Op::Subtract,
        Shape::Radial {
            centre: [0.5, 0.3],
            radii: [0.2, 0.1],
            angle: 15.0,
            feather: 0.4,
        },
    )]);
    l.mask.parts.push(Part::Other(serde_json::json!({
        "op": "add", "kind": "ai_sky", "model": "segformer"
    })));
    s.locals = vec![l];
    let mut doc = EditDoc::new(Source::default());
    doc.change("Local", |d| *d = s.clone());
    let back = EditDoc::from_json(&doc.to_json()).unwrap();
    assert_eq!(back.settings(), &s);
    let text = doc.to_json();
    assert!(text.contains("\"kind\": \"radial\""), "{text}");
    assert!(text.contains("\"kind\": \"ai_sky\""), "{text}");
    // The unknown part leaves the mask as the known parts make it.
    let only = layer(vec![s.locals[0].mask.parts[1].clone()]);
    assert!(raster(&only, None).data.iter().all(|&v| v == 0));
}

#[test]
fn a_proxy_mask_rasterises_quickly() {
    // A 2560×1707 proxy's grid with a gradient, an ellipse and a long
    // brush stroke: what dragging a handle redraws.
    let points: Vec<[f64; 2]> = (0..400)
        .map(|i| {
            let t = f64::from(i) / 400.0;
            [0.1 + 0.8 * t, 0.33 + 0.2 * (t * 12.0).sin()]
        })
        .collect();
    let l = layer(vec![
        part(
            Op::Add,
            Shape::Linear {
                from: [0.0, 0.0],
                to: [0.0, 0.3],
            },
        ),
        part(
            Op::Add,
            Shape::Radial {
                centre: [0.5, 0.33],
                radii: [0.3, 0.2],
                angle: 10.0,
                feather: 0.5,
            },
        ),
        part(
            Op::Add,
            Shape::Brush {
                strokes: vec![Stroke {
                    points,
                    radius: 0.03,
                    feather: 0.5,
                    flow: 1.0,
                    erase: false,
                }],
            },
        ),
    ]);
    let run = || mask::rasterise(&[&l], 2560, 1707, [1.0, 1707.0 / 2560.0], None);
    run();
    let t = std::time::Instant::now();
    let m = run();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    eprintln!("mask: {ms:.1} ms");
    assert_eq!(m.data.len(), 2560 * 1707);
    if !cfg!(debug_assertions) {
        assert!(ms < 60.0, "{ms} ms");
    }
}

#[test]
fn local_sharpness_sharpens_and_softens() {
    let raw = shoebox_decode::raw::RawImage {
        width: 0,
        height: 0,
        cfa: [[0, 1], [1, 2]],
        data: Vec::new(),
        as_shot: None,
        matrices: Vec::new(),
        make: String::new(),
        model: String::new(),
    };
    // A fine checker.
    let mut img = Rgb::new(64, 48);
    for y in 0..48 {
        for x in 0..64 {
            let v = if (x + y) % 2 == 0 { 0.3 } else { 0.1 };
            img.data[(y * 64 + x) * 3..][..3].copy_from_slice(&[v; 3]);
        }
    }
    let src = Demosaiced::new(img, [1.0; 3], Referred::Display);
    let o = Options::default();
    let contrast = |s: &Settings| {
        let out = cpu::render_from(&raw, &src, s, &o, false);
        (out.px(20, 20)[1] - out.px(21, 20)[1]).abs()
    };
    let plain = contrast(&Settings::default());
    let with = |sharpness| {
        let mut l = layer(vec![part(
            Op::Add,
            Shape::Radial {
                centre: [0.5, 0.375],
                radii: [0.6, 0.6],
                angle: 0.0,
                feather: 0.0,
            },
        )]);
        l.adjust = LocalAdjust {
            sharpness,
            ..Default::default()
        };
        Settings {
            locals: vec![l],
            ..Default::default()
        }
    };
    assert!(contrast(&with(80.0)) > plain * 1.5, "sharper");
    assert!(contrast(&with(-100.0)) < plain * 0.2, "softer");
}

/// A Z f's matrices and as-shot balance over a noisy grey mosaic.
fn noisy_raw() -> shoebox_decode::raw::RawImage {
    use shoebox_decode::raw::{ColorMatrix, RawImage};
    let m = |v: [f32; 9]| [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
    let as_shot = [1.1425778, 1.0, 1.7753916];
    let (w, h) = (96, 64);
    let cfa = [[0, 1], [1, 2]];
    let mut seed = 0x9e37_79b9u32;
    let mut data = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed as f32 / u32::MAX as f32 - 0.5) * 0.08;
            let c = usize::from(cfa[y % 2][x % 2]);
            data[y * w + x] = (0.2 + noise) / as_shot[c];
        }
    }
    RawImage {
        width: w,
        height: h,
        cfa,
        data,
        as_shot: Some(as_shot),
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

/// A stand-in AI denoiser (this test binary's own): each packed plane
/// box-blurred 5×5, then red, the greens' mean and blue over each 2×2.
fn register_blurring_denoiser() {
    shoebox_develop::denoise::register(
        "blurring stand-in",
        Box::new(|p, h, w, progress, _cancel| {
            let n = h * w;
            let blurred: Vec<f32> = (0..4 * n)
                .map(|i| {
                    let (c, y, x) = (i / n, (i % n) / w, i % w);
                    let mut sum = 0.0;
                    for dy in -2isize..=2 {
                        for dx in -2isize..=2 {
                            let yy = (y as isize + dy).clamp(0, h as isize - 1) as usize;
                            let xx = (x as isize + dx).clamp(0, w as isize - 1) as usize;
                            sum += p[c * n + yy * w + xx];
                        }
                    }
                    sum / 25.0
                })
                .collect();
            let mut out = vec![0f32; 3 * 4 * n];
            for y in 0..2 * h {
                for x in 0..2 * w {
                    let i = (y / 2) * w + x / 2;
                    let rgb = [
                        blurred[i],
                        0.5 * (blurred[n + i] + blurred[2 * n + i]),
                        blurred[3 * n + i],
                    ];
                    for c in 0..3 {
                        out[(c * 2 * h + y) * 2 * w + x] = rgb[c];
                    }
                }
            }
            progress(1.0);
            Ok(out)
        }),
    );
}

/// Green's standard deviation over columns `xs` (rows 6 to 58).
fn spread(img: &Rgb, xs: std::ops::Range<usize>) -> f32 {
    let v: Vec<f32> = (6..58)
        .flat_map(|y| xs.clone().map(move |x| (x, y)))
        .map(|(x, y)| img.px(x, y)[1])
        .collect();
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|g| (g - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
}

#[test]
fn a_layers_ai_strength_denoises_inside_its_mask() {
    register_blurring_denoiser();
    let raw = noisy_raw();
    let o = Options::default();
    let plain_settings = Settings::default();
    let ai = shoebox_develop::denoise::run(
        &raw,
        &plain_settings.modules.raw,
        &|_| {},
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let src = cpu::demosaic(&raw, &plain_settings.modules.raw, None);
    // The left half (the photo is 1 × 2/3 long edges).
    let left = |denoise_ai| {
        let mut l = layer(vec![part(
            Op::Add,
            Shape::Brush {
                strokes: vec![Stroke {
                    points: vec![[0.25, 0.0], [0.25, 0.67]],
                    radius: 0.25,
                    feather: 0.0,
                    flow: 1.0,
                    erase: false,
                }],
            },
        )]);
        l.adjust = LocalAdjust {
            denoise_ai,
            ..Default::default()
        };
        l
    };
    let mut s = Settings {
        locals: vec![left(100.0)],
        ..Default::default()
    };
    // No AI result yet: the layer does nothing.
    let plain = cpu::render_from(&raw, &src, &plain_settings, &o, false);
    assert_eq!(cpu::render_from(&raw, &src, &s, &o, false).data, plain.data);
    assert!(src.aux(&s).is_none());
    assert!(
        cpu::layers::wants_ai(&s),
        "the layer asks for the AI's result"
    );

    // With it: smoother inside the mask, untouched outside.
    src.cache.set_ai(std::sync::Arc::new(ai));
    let out = cpu::render_from(&raw, &src, &s, &o, false);
    let (inside, outside) = (spread(&out, 6..40), spread(&out, 56..90));
    assert!(
        inside < 0.5 * outside,
        "inside {inside} vs outside {outside}"
    );
    assert!((spread(&plain, 6..40) - outside).abs() < 0.3 * outside);
    for y in 6..58 {
        for x in 56..90 {
            assert_eq!(out.px(x, y), plain.px(x, y), "outside at ({x}, {y})");
        }
    }

    // −100 cancels a global 100 inside the mask.
    s.modules.denoise_ai.enabled = true;
    s.locals[0] = left(-100.0);
    let mut global = s.clone();
    global.locals.clear();
    let all_ai = cpu::render_from(&raw, &src, &global, &o, false);
    let out = cpu::render_from(&raw, &src, &s, &o, false);
    for y in 6..58 {
        for x in 6..40 {
            let (a, b) = (out.px(x, y), plain.px(x, y));
            for c in 0..3 {
                assert!(
                    (a[c] - b[c]).abs() < 1e-5,
                    "inside at ({x}, {y}): {a:?} vs {b:?}"
                );
            }
        }
        for x in 56..90 {
            assert_eq!(out.px(x, y), all_ai.px(x, y), "outside at ({x}, {y})");
        }
    }

    // Spots are healed on the image beside the source too.
    s.modules.denoise_ai.enabled = false;
    s.locals[0] = left(100.0);
    s.modules.heal.spots = vec![shoebox_develop::edit::Spot {
        dest: [0.3, 0.3],
        source: [0.7, 0.3],
        radius: 0.05,
        ..Default::default()
    }];
    let aux = src.aux(&s).unwrap();
    let want = shoebox_develop::heal::heal(&src.cache.ai().unwrap(), &s.modules.heal.spots);
    assert_eq!(aux.more_ai.as_ref().unwrap().data, want.data);
    assert!(aux.less_ai.is_none(), "only what a layer needs");
}

/// Texture, edges and noise (a display-referred source, so the detail
/// stage sees it as it is).
fn textured(scale: f32) -> Demosaiced {
    let (w, h) = (128, 96);
    let mut img = Rgb::new(w, h);
    let mut seed = 12345u32;
    for y in 0..h {
        for x in 0..w {
            let u = x as f32 / w as f32;
            let base = 0.03 + 0.5 * u * u;
            let tex = 0.04 * ((x as f32 * 0.9).sin() * (y as f32 * 0.7).cos());
            let edge = if (x / 13 + y / 17) % 2 == 0 {
                0.12
            } else {
                0.0
            };
            for c in 0..3 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = (seed as f32 / u32::MAX as f32 - 0.5) * 0.06 * (base + 0.01).sqrt();
                img.data[(y * w + x) * 3 + c] = (base + tex + edge + noise) * [1.0, 0.9, 0.7][c];
            }
        }
    }
    Demosaiced::new(img, [1.0; 3], Referred::Display).with_scale(scale)
}

#[test]
fn a_layers_sharpening_matches_the_global_modules() {
    let raw = shoebox_decode::raw::RawImage {
        width: 0,
        height: 0,
        cfa: [[0, 1], [1, 2]],
        data: Vec::new(),
        as_shot: None,
        matrices: Vec::new(),
        make: String::new(),
        model: String::new(),
    };
    let o = Options::default();
    // The left half (the photo is 1 × 3/4 long edges), hard-edged.
    let left = |sharpness| {
        let mut l = layer(vec![part(
            Op::Add,
            Shape::Brush {
                strokes: vec![Stroke {
                    points: vec![[0.25, 0.0], [0.25, 0.75]],
                    radius: 0.25,
                    feather: 0.0,
                    flow: 1.0,
                    erase: false,
                }],
            },
        )]);
        l.adjust = LocalAdjust {
            sharpness,
            ..Default::default()
        };
        l
    };
    // Full size and a proxy's scale; a layer alone, and on top of the
    // global module's 40.
    for scale in [1.0, 0.42] {
        let src = textured(scale);
        for (global, layer) in [(0.0, 25.0), (0.0, 60.0), (0.0, 100.0), (40.0, 60.0)] {
            let mut s = Settings {
                locals: vec![left(layer)],
                ..Default::default()
            };
            s.modules.sharpening.amount = global;
            let mut alone = s.clone();
            alone.locals.clear();
            let mut want = alone.clone();
            want.modules.sharpening.amount = global + layer;
            let before = cpu::render_from(&raw, &src, &alone, &o, false);
            let want = cpu::render_from(&raw, &src, &want, &o, false);
            let got = cpu::render_from(&raw, &src, &s, &o, false);
            // Inside, clear of the mask's edge (x = 64) and the frame's.
            let (mut worst, mut err, mut effect) = (0f32, 0f64, 0f64);
            for y in 4..92 {
                for x in 4..58 {
                    let (g, w, b) = (got.px(x, y), want.px(x, y), before.px(x, y));
                    for c in 0..3 {
                        worst = worst.max((g[c] - w[c]).abs());
                        err += f64::from(g[c] - w[c]).powi(2);
                        effect += f64::from(w[c] - b[c]).powi(2);
                    }
                }
                // Outside, as it was.
                for x in 70..124 {
                    assert_eq!(got.px(x, y), before.px(x, y), "outside at ({x}, {y})");
                }
            }
            let rel = (err / effect).sqrt();
            eprintln!(
                "scale {scale}, global {global} + layer {layer}: max error {worst:.5}, rms error {:.1}% of the effect",
                rel * 100.0
            );
            assert!(effect > 0.0 && rel < 0.03, "{rel}");
            assert!(worst < 0.0025, "{worst}");
        }
    }
}
