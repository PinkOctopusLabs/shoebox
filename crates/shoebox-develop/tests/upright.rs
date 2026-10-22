//! Auto-upright: lines found in a drawn image, and corrections that make
//! leaning lines vertical and level again.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::colour;
use shoebox_develop::upright::{Correction, Lines, Mode, Segment, lines, solve, worst_lean};

/// The matrix `solve` corrects with, rebuilt here to distort test lines by
/// its inverse.
fn forward(c: &Correction, w: f64, h: f64) -> [[f64; 3]; 3] {
    let t = shoebox_develop::edit::Transform {
        vertical: c.vertical,
        horizontal: c.horizontal,
        constrain: false,
        ..Default::default()
    };
    let transform = colour::inv(&shoebox_develop::cpu::transform_inverse(&t, w, h)).unwrap();
    let (sin, cos) = c.rotate.to_radians().sin_cos();
    colour::mul(
        &[[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]],
        &transform,
    )
}

/// A building's straight lines (verticals and levels), as a camera tilted
/// and turned by `c` would have recorded them.
fn building(c: &Correction) -> Lines {
    let (w, h) = (1000.0, 700.0);
    let undo = colour::inv(&forward(c, w, h)).unwrap();
    let map = |p: [f64; 2]| {
        let q = colour::mulv(&undo, [p[0], p[1], 1.0]);
        [q[0] / q[2], q[1] / q[2]]
    };
    let mut segments = Vec::new();
    for x in [-400.0, -250.0, -100.0, 60.0, 220.0, 380.0] {
        segments.push(Segment {
            a: map([x, -300.0]),
            b: map([x, 280.0]),
        });
    }
    for y in [-250.0, -80.0, 90.0, 250.0] {
        segments.push(Segment {
            a: map([-420.0, y]),
            b: map([400.0, y]),
        });
    }
    Lines {
        segments,
        width: w,
        height: h,
    }
}

#[test]
fn full_undoes_a_tilted_and_turned_camera() {
    let truth = Correction {
        rotate: 2.0,
        vertical: 35.0,
        horizontal: -20.0,
    };
    let l = building(&truth);
    assert!(worst_lean(&l, &Correction::default(), Mode::Full) > 3.0);
    let c = solve(&l, Mode::Full);
    assert!(worst_lean(&l, &c, Mode::Full) < 0.5, "{c:?}");
    assert!((c.rotate - truth.rotate).abs() < 0.5, "{c:?}");
    assert!((c.vertical - truth.vertical).abs() < 4.0, "{c:?}");
    assert!((c.horizontal - truth.horizontal).abs() < 4.0, "{c:?}");
    // Vertical only straightens the verticals.
    let v = solve(&l, Mode::Vertical);
    assert_eq!(v.horizontal, 0.0);
    assert!(worst_lean(&l, &v, Mode::Vertical) < 0.5, "{v:?}");
    // Level only turns.
    let lv = solve(&l, Mode::Level);
    assert_eq!((lv.vertical, lv.horizontal), (0.0, 0.0));
    // Auto holds back: no more than Full.
    let a = solve(&l, Mode::Auto);
    assert!(a.vertical.abs() <= c.vertical.abs() + 1.0, "{a:?} vs {c:?}");
}

#[test]
fn lines_are_found_and_levelled() {
    // Dark lines on a light ground, turned 3° clockwise: a window grid.
    let (w, h) = (640, 480);
    let a = 3f64.to_radians();
    let mut lum = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (cx, cy) = (x as f64 - w as f64 / 2.0, y as f64 - h as f64 / 2.0);
            // Back to the unturned grid.
            let (u, v) = (cx * a.cos() + cy * a.sin(), -cx * a.sin() + cy * a.cos());
            let on = (u.rem_euclid(80.0) < 3.0 || v.rem_euclid(80.0) < 3.0)
                && u.abs() < 260.0
                && v.abs() < 200.0;
            lum[y * w + x] = if on { -4.0 } else { 0.0 };
        }
    }
    let found = lines(&lum, w, h);
    assert!(found.segments.len() >= 8, "{}", found.segments.len());
    let before = worst_lean(&found, &Correction::default(), Mode::Level);
    assert!((before - 3.0).abs() < 1.0, "they lean 3°: {before}");
    let c = solve(&found, Mode::Level);
    assert!((c.rotate.abs() - 3.0).abs() < 0.2, "{c:?}");
    // The pieces between crossings are short, so each is a pixel's
    // rounding from straight.
    assert!(worst_lean(&found, &c, Mode::Level) < 1.0, "{c:?}");

    // Through the render with that rotation, the lines come out level:
    // the solver turns the way the pipe does.
    let mut img = shoebox_develop::cpu::Rgb::new(w, h);
    for (i, v) in lum.iter().enumerate() {
        let lin = v.exp2() * 0.5;
        img.data[i * 3..i * 3 + 3].copy_from_slice(&[lin; 3]);
    }
    // Cropped inside, as the app does, so the turned frame's own edges
    // don't count.
    let g = shoebox_develop::edit::Geometry {
        rotate: c.rotate,
        crop: shoebox_develop::cpu::max_crop(w as f64, h as f64, c.rotate),
        ..Default::default()
    };
    let out = shoebox_develop::cpu::rotate_crop(&img, &g);
    let back: Vec<f32> = out
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| (p[1].max(0.0) + 1e-3).log2())
        .collect();
    let again = lines(&back, out.width, out.height);
    let mean = |l: &Lines| {
        let n = l.segments.len().max(1) as f64;
        l.segments
            .iter()
            .map(|sg| {
                let one = Lines {
                    segments: vec![*sg],
                    width: l.width,
                    height: l.height,
                };
                worst_lean(&one, &Correction::default(), Mode::Level)
            })
            .sum::<f64>()
            / n
    };
    assert!(mean(&found) > 2.5, "{}", mean(&found));
    assert!(
        mean(&again) < 0.5,
        "level after the render: {}",
        mean(&again)
    );
}

#[test]
fn nothing_to_go_on_changes_nothing() {
    let flat = vec![0f32; 200 * 150];
    let l = lines(&flat, 200, 150);
    assert!(l.segments.is_empty());
    assert_eq!(solve(&l, Mode::Auto), Correction::default());
}
