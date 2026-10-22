//! Auto-upright: lines found in a drawn image, and corrections that make
//! leaning lines vertical and level again.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::colour;
use shoebox_develop::upright::{
    Correction, Lines, Mode, Segment, guide_leans, lines, solve, solve_guided, worst_lean,
};

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

/// A line's two ends.
type Ends = ([f64; 2], [f64; 2]);

/// Guides drawn along a scene's verticals and levels (`ends`, where they
/// are once corrected), as a camera turned and tilted by `c` recorded them.
fn drawn(c: &Correction, ends: &[Ends]) -> Lines {
    let (w, h) = (1200.0, 800.0);
    let undo = colour::inv(&forward(c, w, h)).unwrap();
    let map = |p: [f64; 2]| {
        let q = colour::mulv(&undo, [p[0], p[1], 1.0]);
        [q[0] / q[2], q[1] / q[2]]
    };
    Lines {
        segments: ends
            .iter()
            .map(|&(a, b)| Segment {
                a: map(a),
                b: map(b),
            })
            .collect(),
        width: w,
        height: h,
    }
}

/// The guides' largest lean once corrected (degrees).
fn worst(l: &Lines, c: &Correction) -> f64 {
    guide_leans(l, c)
        .into_iter()
        .map(f64::abs)
        .fold(0.0, f64::max)
}

#[test]
fn guides_come_out_vertical_and_level() {
    let left = ([-450.0, -300.0], [-450.0, 320.0]);
    let right = ([380.0, -280.0], [380.0, 300.0]);
    let top = ([-400.0, -250.0], [420.0, -250.0]);
    let bottom = ([-380.0, 260.0], [350.0, 260.0]);
    let cases: [(Correction, Vec<Ends>); 5] = [
        // Two verticals: turned and tilted.
        (
            Correction {
                rotate: 2.0,
                vertical: 30.0,
                horizontal: 0.0,
            },
            vec![left, right],
        ),
        // Two levels: turned and panned.
        (
            Correction {
                rotate: -3.0,
                vertical: 0.0,
                horizontal: 25.0,
            },
            vec![top, bottom],
        ),
        // One of each.
        (
            Correction {
                rotate: 4.0,
                vertical: 15.0,
                horizontal: 0.0,
            },
            vec![left, bottom],
        ),
        // Two verticals and a level: all three.
        (
            Correction {
                rotate: 1.5,
                vertical: 35.0,
                horizontal: -20.0,
            },
            vec![left, right, top],
        ),
        // Four.
        (
            Correction {
                rotate: -2.0,
                vertical: -25.0,
                horizontal: 15.0,
            },
            vec![left, right, top, bottom],
        ),
    ];
    for (truth, ends) in cases {
        let l = drawn(&truth, &ends);
        assert!(worst(&l, &Correction::default()) > 1.0, "{truth:?}");
        let c = solve_guided(&l).unwrap();
        assert!(
            worst(&l, &c) < 0.3,
            "{truth:?} → {c:?}: {:?}",
            guide_leans(&l, &c)
        );
    }
}

#[test]
fn guides_recover_the_camera() {
    // Two verticals and two levels pin all three down.
    let truth = Correction {
        rotate: 2.5,
        vertical: 30.0,
        horizontal: -15.0,
    };
    let l = drawn(
        &truth,
        &[
            ([-450.0, -300.0], [-450.0, 320.0]),
            ([400.0, -280.0], [400.0, 300.0]),
            ([-400.0, -250.0], [420.0, -250.0]),
            ([-380.0, 260.0], [350.0, 260.0]),
        ],
    );
    let c = solve_guided(&l).unwrap();
    assert!((c.rotate - truth.rotate).abs() < 0.3, "{c:?}");
    assert!((c.vertical - truth.vertical).abs() < 2.0, "{c:?}");
    assert!((c.horizontal - truth.horizontal).abs() < 2.0, "{c:?}");
    // Two verticals that are only turned: turned back, no tilt.
    let turned = Correction {
        rotate: -3.0,
        ..Default::default()
    };
    let l = drawn(
        &turned,
        &[
            ([-300.0, -300.0], [-300.0, 300.0]),
            ([250.0, -300.0], [250.0, 300.0]),
        ],
    );
    let c = solve_guided(&l).unwrap();
    assert!((c.rotate + 3.0).abs() < 0.05, "{c:?}");
    assert!(c.vertical.abs() < 0.5 && c.horizontal == 0.0, "{c:?}");
}

#[test]
fn fewer_than_two_guides_do_nothing() {
    let t = Correction {
        rotate: 3.0,
        ..Default::default()
    };
    let one = drawn(&t, &[([-300.0, -300.0], [-300.0, 300.0])]);
    assert_eq!(solve_guided(&one), None);
    // A guide just started (barely a line) doesn't count.
    let mut l = one.clone();
    l.segments.push(Segment {
        a: [10.0, 10.0],
        b: [12.0, 11.0],
    });
    assert_eq!(solve_guided(&l), None);
    assert_eq!(solve_guided(&Lines::default()), None);
}

#[test]
fn guides_are_kept_but_not_rendered() {
    use shoebox_develop::edit::{EditDoc, Settings};
    let mut s = Settings::default();
    s.geometry.guides = vec![[[0.2, 0.1], [0.21, 0.9]], [[0.8, 0.1], [0.79, 0.9]]];
    // Kept in the edit, so they can be shown and moved again...
    let back = Settings::from_value(&s.to_value()).unwrap();
    assert_eq!(back.geometry.guides, s.geometry.guides);
    // ...but only what they did decides the render.
    assert_eq!(
        EditDoc::settings_hash(&s),
        EditDoc::settings_hash(&Settings::default())
    );
}
