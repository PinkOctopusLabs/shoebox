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
