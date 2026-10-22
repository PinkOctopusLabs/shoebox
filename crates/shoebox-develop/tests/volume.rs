//! Volume deformation: the map itself (identity at 0%, the centre kept,
//! verticals kept by the cylinder, a disc near the corner made round), and
//! rendered: through the global projections and through a face found by a
//! stand-in face finder, with a line away from the face left straight.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::edit::{Settings, Volume, VolumeMode};
use shoebox_develop::lens::Shot;
use shoebox_develop::preset::{Part, Preset};
use shoebox_develop::volume::{self, Deform, Spec};

const W: usize = 900;
const H: usize = 600;

fn spec(mode: VolumeMode, amount: f64) -> Spec {
    Spec::of(
        &Volume {
            mode,
            amount,
            ..Default::default()
        },
        1,
    )
    .unwrap()
}

/// The focal length (pixels) the frame gets with no EXIF: 24 mm on full
/// frame.
fn focal() -> f64 {
    volume::focal_px(&Shot::default(), W, H).0
}

fn centre() -> [f64; 2] {
    [(W as f64 - 1.0) / 2.0, (H as f64 - 1.0) / 2.0]
}

/// Direction `deg` off the axis, towards the bottom right corner, in the
/// rectilinear frame (pixels).
fn towards_corner(deg: f64) -> [f64; 2] {
    let (c, f) = (centre(), focal());
    let d = (W as f64).hypot(H as f64);
    let r = f * deg.to_radians().tan();
    [c[0] + r * W as f64 / d, c[1] + r * H as f64 / d]
}

/// Points on a small circle seen through a rectilinear lens: a cone of
/// directions `radius` degrees about the direction to pixel `at`.
fn cone(at: [f64; 2], radius: f64) -> Vec<[f64; 2]> {
    let (c, f) = (centre(), focal());
    let a = [(at[0] - c[0]) / f, (at[1] - c[1]) / f, 1.0];
    let n = (a[0] * a[0] + a[1] * a[1] + 1.0).sqrt();
    let a = a.map(|v| v / n);
    // Two directions across it.
    let u = {
        let u = [-a[1], a[0], 0.0];
        let k = (u[0] * u[0] + u[1] * u[1]).sqrt().max(1e-12);
        u.map(|v| v / k)
    };
    let v = [
        a[1] * u[2] - a[2] * u[1],
        a[2] * u[0] - a[0] * u[2],
        a[0] * u[1] - a[1] * u[0],
    ];
    let t = radius.to_radians().tan();
    (0..72)
        .map(|k| {
            let p = f64::from(k) * std::f64::consts::TAU / 72.0;
            let d = [0, 1, 2].map(|i| a[i] + t * (p.cos() * u[i] + p.sin() * v[i]));
            [c[0] + f * d[0] / d[2], c[1] + f * d[1] / d[2]]
        })
        .collect()
}

/// Longest over shortest axis of a closed outline (from its second
/// moments).
fn aspect(pts: &[[f64; 2]]) -> f64 {
    let n = pts.len() as f64;
    let m = pts
        .iter()
        .fold([0.0; 2], |m, p| [m[0] + p[0] / n, m[1] + p[1] / n]);
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in pts {
        let (x, y) = (p[0] - m[0], p[1] - m[1]);
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
    }
    let tr = sxx + syy;
    let det = sxx * syy - sxy * sxy;
    let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
    ((tr / 2.0 + disc) / (tr / 2.0 - disc)).sqrt()
}

#[test]
fn nothing_at_zero() {
    assert_eq!(Spec::of(&Volume::default(), 1), None);
    let zero = Volume {
        mode: VolumeMode::Spherical,
        amount: 0.0,
        ..Default::default()
    };
    assert_eq!(Spec::of(&zero, 1), None);
    // Faces on but none found: nothing either.
    let faces = Volume {
        faces: true,
        ..Default::default()
    };
    assert_eq!(Spec::of(&faces, 1), None);
    // An untouched edit renders exactly as before: no grid.
    let src = Demosaiced::new(Rgb::new(60, 40), [1.0; 3], Referred::Scene);
    let f = cpu::frame(&raw(), &src, &Settings::default(), &Options::default());
    assert!(f.lens.is_none());
    // At a sliver of amount, the map is all but the identity.
    for mode in [
        VolumeMode::Horizontal,
        VolumeMode::Vertical,
        VolumeMode::Spherical,
    ] {
        let d = Deform::new(&spec(mode, 1e-9), focal(), W, H, |p| p);
        for p in [[0.0, 0.0], [899.0, 599.0], [100.0, 500.0], centre()] {
            let q = d.inverse(p);
            assert!((q[0] - p[0]).abs() < 1e-6 && (q[1] - p[1]).abs() < 1e-6);
        }
    }
}

#[test]
fn the_centre_stays_and_the_map_inverts() {
    for mode in [
        VolumeMode::Horizontal,
        VolumeMode::Vertical,
        VolumeMode::Spherical,
    ] {
        let d = Deform::new(&spec(mode, 100.0), focal(), W, H, |p| p);
        let c = centre();
        let q = d.forward(c);
        assert!((q[0] - c[0]).abs() < 1e-9 && (q[1] - c[1]).abs() < 1e-9);
        // Near the centre, scale 1.
        let q = d.forward([c[0] + 1.0, c[1] + 1.0]);
        assert!((q[0] - c[0] - 1.0).abs() < 1e-3 && (q[1] - c[1] - 1.0).abs() < 1e-3);
        for p in [[0.0, 0.0], [899.0, 599.0], [100.0, 500.0], [700.0, 50.0]] {
            let back = d.forward(d.inverse(p));
            assert!(
                (back[0] - p[0]).abs() < 1e-6 && (back[1] - p[1]).abs() < 1e-6,
                "{mode:?} {p:?} {back:?}"
            );
        }
    }
}

#[test]
fn the_cylinder_keeps_verticals() {
    let d = Deform::new(&spec(VolumeMode::Horizontal, 100.0), focal(), W, H, |p| p);
    for x in [20.0, 200.0, 449.5, 800.0] {
        let xs: Vec<f64> = (0..=12)
            .map(|k| d.forward([x, f64::from(k) * 50.0])[0])
            .collect();
        let spread =
            xs.iter().fold(f64::MIN, |a, &b| a.max(b)) - xs.iter().fold(f64::MAX, |a, &b| a.min(b));
        assert!(spread < 1e-9, "x = {x}: {xs:?}");
    }
    // The vertical one keeps horizontals instead.
    let d = Deform::new(&spec(VolumeMode::Vertical, 100.0), focal(), W, H, |p| p);
    let ys: Vec<f64> = (0..=12)
        .map(|k| d.forward([f64::from(k) * 70.0, 40.0])[1])
        .collect();
    let spread =
        ys.iter().fold(f64::MIN, |a, &b| a.max(b)) - ys.iter().fold(f64::MAX, |a, &b| a.min(b));
    assert!(spread < 1e-9, "{ys:?}");
    // A turned frame swaps them: horizontal is the stored image's down.
    let turned = Spec::of(
        &Volume {
            mode: VolumeMode::Horizontal,
            ..Default::default()
        },
        6,
    )
    .unwrap();
    let d = Deform::new(&turned, focal(), W, H, |p| p);
    let ys: Vec<f64> = (0..=12)
        .map(|k| d.forward([f64::from(k) * 70.0, 40.0])[1])
        .collect();
    let spread =
        ys.iter().fold(f64::MIN, |a, &b| a.max(b)) - ys.iter().fold(f64::MAX, |a, &b| a.min(b));
    assert!(spread < 1e-9, "{ys:?}");
}

#[test]
fn a_disc_near_the_corner_comes_out_rounder() {
    let disc = cone(towards_corner(36.0), 3.0);
    let before = aspect(&disc);
    // Rectilinear stretches it radially by about 1/cos 36°.
    assert!(before > 1.2, "{before}");
    let round = |mode, amount| {
        let d = Deform::new(&spec(mode, amount), focal(), W, H, |p| p);
        aspect(&disc.iter().map(|&p| d.forward(p)).collect::<Vec<_>>())
    };
    let spherical = round(VolumeMode::Spherical, 100.0);
    assert!(spherical < 1.02, "{spherical}");
    let half = round(VolumeMode::Spherical, 50.0);
    assert!(half < before - 0.08 && half > spherical, "{half}");
    // The cylinder helps too, though not on its diagonal as much.
    let cyl = round(VolumeMode::Horizontal, 100.0);
    assert!(cyl < before - 0.04, "{cyl}");
}

#[test]
fn a_face_is_made_round_where_it_is() {
    let at = towards_corner(36.0);
    let disc = cone(at, 3.0);
    let before = aspect(&disc);
    let r = disc
        .iter()
        .map(|p| (p[0] - at[0]).hypot(p[1] - at[1]))
        .fold(0.0, f64::max);
    let long = W as f64;
    let b = [
        (at[0] - r + 0.5) / long,
        (at[1] - r + 0.5) / long,
        (at[0] + r + 0.5) / long,
        (at[1] + r + 0.5) / long,
    ];
    let v = Volume {
        faces: true,
        boxes: vec![b],
        ..Default::default()
    };
    let d = Deform::new(&Spec::of(&v, 1).unwrap(), focal(), W, H, |p| p);
    let out: Vec<[f64; 2]> = disc.iter().map(|&p| d.forward(p)).collect();
    let after = aspect(&out);
    assert!(after < 1.03, "{before} → {after}");
    // It stays where it was, its area kept.
    let m = d.forward(at);
    assert!((m[0] - at[0]).hypot(m[1] - at[1]) < 1.0, "{m:?}");
    // Far from it, nothing moves.
    for p in [[50.0, 50.0], centre(), [100.0, 550.0]] {
        let q = d.forward(p);
        assert!((q[0] - p[0]).abs() < 1e-9 && (q[1] - p[1]).abs() < 1e-9);
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

/// A rectilinear picture of a disc 3° across at `at` (pixels), and
/// vertical lines every 150 px.
fn scene(at: [f64; 2]) -> Rgb {
    let (c, f) = (centre(), focal());
    let a = [(at[0] - c[0]) / f, (at[1] - c[1]) / f, 1.0];
    let na = (a[0] * a[0] + a[1] * a[1] + 1.0).sqrt();
    let cos_r = 3f64.to_radians().cos();
    let mut img = Rgb::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let d = [(x as f64 - c[0]) / f, (y as f64 - c[1]) / f, 1.0];
            let nd = (d[0] * d[0] + d[1] * d[1] + 1.0).sqrt();
            let cos = (a[0] * d[0] + a[1] * d[1] + a[2] * d[2]) / (na * nd);
            let line = (x as f64 - 75.0).rem_euclid(150.0) < 2.0;
            let v = if cos > cos_r {
                0.8
            } else if line {
                0.4
            } else {
                0.0
            };
            let i = (y * W + x) * 3;
            img.data[i..i + 3].copy_from_slice(&[v as f32; 3]);
        }
    }
    img
}

/// Between the lines' brightness and the disc's, after the render's
/// exposure.
fn disc_level(img: &Rgb) -> f32 {
    0.75 * img.data.iter().fold(0.0f32, |m, &v| m.max(v))
}

/// The bright disc's aspect in a render (by moments).
fn blob_aspect(img: &Rgb) -> f64 {
    let level = disc_level(img);
    let pts: Vec<[f64; 2]> = (0..img.height)
        .flat_map(|y| (0..img.width).map(move |x| (x, y)))
        .filter(|&(x, y)| img.px(x, y)[1] > level)
        .map(|(x, y)| [x as f64, y as f64])
        .collect();
    assert!(pts.len() > 200, "the disc is in the frame ({})", pts.len());
    aspect(&pts)
}

/// How far the line nearest column `x` wanders from top to bottom: the
/// brightest column in each row, among rows away from the disc.
fn line_spread(img: &Rgb, x: usize, rows: std::ops::Range<usize>) -> usize {
    let level = disc_level(img);
    let cols: Vec<usize> = rows
        .step_by(10)
        .map(|y| {
            (x.saturating_sub(12)..(x + 12).min(img.width))
                .filter(|&c| img.px(c, y)[1] < level)
                .max_by(|&a, &b| img.px(a, y)[1].total_cmp(&img.px(b, y)[1]))
                .unwrap()
        })
        .collect();
    cols.iter().max().unwrap() - cols.iter().min().unwrap()
}

fn render(img: &Rgb, s: &Settings) -> (Rgb, f32) {
    let src = Demosaiced::new(img.clone(), [1.0; 3], Referred::Scene);
    let f = cpu::frame(&raw(), &src, s, &Options::default());
    let zoom = f.lens.as_ref().map_or(1.0, |g| g.zoom);
    (cpu::apply(&src.rgb, &f, false), zoom)
}

#[test]
fn rendered_spherical_rounds_the_disc() {
    let img = scene(towards_corner(34.0));
    let (plain, _) = render(&img, &Settings::default());
    let before = blob_aspect(&plain);
    assert!(before > 1.15, "{before}");
    let mut s = Settings::default();
    s.geometry.volume.mode = VolumeMode::Spherical;
    let (round, zoom) = render(&img, &s);
    let after = blob_aspect(&round);
    assert!(after < 1.04, "{before} → {after}");
    // Filled: zoomed in a little.
    assert!(zoom > 1.02 && zoom < 1.5, "{zoom}");
    // The centre's pixel stays.
    let c = (W / 2, H / 2);
    assert!((round.px(c.0, c.1)[1] - plain.px(c.0, c.1)[1]).abs() < 1e-6);

    // Cylindrical: the vertical lines stay straight.
    s.geometry.volume.mode = VolumeMode::Horizontal;
    let (cyl, _) = render(&img, &s);
    for x in [75, 225, 375] {
        let spread = line_spread(&cyl, x, 10..H - 10);
        assert!(spread <= 1, "the line near x = {x} wanders by {spread}");
    }
    assert!(blob_aspect(&cyl) < before - 0.04);
}

/// Faces found by a stand-in face finder (it reports the disc), stored in
/// the edit, rendered without it.
#[test]
fn rendered_faces_are_made_round_and_lines_elsewhere_stay() {
    let at = towards_corner(34.0);
    let img = scene(at);
    // The finder is shown the picture upright on the masks' grid: here the
    // whole 900×600, so the disc's box in its pixels.
    let r = 0.07 * focal();
    let found = [
        (at[0] - r) as f32,
        (at[1] - r) as f32,
        (at[0] + r) as f32,
        (at[1] + r) as f32,
    ];
    shoebox_develop::mask::register_face_finder(Box::new(move |_, w, h| {
        assert_eq!((w, h), (W, H));
        Ok(vec![found])
    }));
    let src = Demosaiced::new(img.clone(), [1.0; 3], Referred::Scene);
    let f = cpu::frame(&raw(), &src, &Settings::default(), &Options::default());
    let boxes = cpu::layers::faces(&src, &f.colour, &f.display, 1).unwrap();
    assert_eq!(boxes.len(), 1);

    let (plain, _) = render(&img, &Settings::default());
    let before = blob_aspect(&plain);
    let mut s = Settings::default();
    s.geometry.volume.faces = true;
    s.geometry.volume.boxes = boxes.clone();
    let (fixed, zoom) = render(&img, &s);
    let after = blob_aspect(&fixed);
    assert!(after < 1.05 && after < before - 0.1, "{before} → {after}");
    // The lines on the left, away from the face, stay straight (and where
    // the zoom puts them).
    for x0 in [75.0, 225.0] {
        let x = (centre()[0] + (x0 - centre()[0]) * f64::from(zoom)).round() as usize;
        let spread = line_spread(&fixed, x, 10..H - 10);
        assert!(spread <= 1, "the line near x = {x} wanders by {spread}");
    }
    // Half the fix, half the change.
    s.geometry.volume.face_amount = 50.0;
    let (half, _) = render(&img, &s);
    let mid = blob_aspect(&half);
    assert!(mid < before - 0.03 && mid > after, "{mid}");
    // A face drawn where it shows: back through the grid, near the disc's
    // place (the patch keeps its centre).
    let src = Demosaiced::new(img, [1.0; 3], Referred::Scene);
    s.geometry.volume.face_amount = 100.0;
    let f = cpu::frame(&raw(), &src, &s, &Options::default());
    let (px, py) = f.unlensed(W, H, at[0] as f32, at[1] as f32);
    assert!(
        (f64::from(px) - at[0]).hypot(f64::from(py) - at[1]) < 2.0,
        "({px}, {py}) for {at:?}"
    );
}

#[test]
fn the_focal_length_decides_how_wide() {
    let mut shot = Shot {
        focal: 14.0,
        ..Default::default()
    };
    let (_, fov) = volume::focal_px(&shot, 6048, 4032);
    // 14 mm on full frame: 114° across the diagonal.
    assert!((fov - 114.2).abs() < 0.5, "{fov}");
    shot.focal = 0.0;
    let (_, fov) = volume::focal_px(&shot, 6048, 4032);
    assert!((fov - 84.1).abs() < 0.5, "{fov}");
    // A phone's 4 mm without a known sensor: the fallback, not 158°.
    shot.focal = 4.0;
    let (_, fov) = volume::focal_px(&shot, 4000, 3000);
    assert!((fov - 84.1).abs() < 0.5, "{fov}");
}

#[test]
fn the_edit_round_trips_and_presets_leave_faces() {
    let mut s = Settings::default();
    assert_eq!(s.to_value(), serde_json::json!({}));
    s.geometry.volume = Volume {
        mode: VolumeMode::Horizontal,
        amount: 60.0,
        faces: true,
        face_amount: 80.0,
        boxes: vec![[0.1, 0.2, 0.15, 0.27]],
    };
    let v = s.to_value();
    assert_eq!(
        v,
        serde_json::json!({"geometry": {"volume": {
            "mode": "horizontal", "amount": 60.0, "faces": true,
            "face_amount": 80.0, "boxes": [[0.1, 0.2, 0.15, 0.27]],
        }}})
    );
    assert_eq!(Settings::from_value(&v).unwrap(), s);
    // The faces decide the render.
    let h = shoebox_develop::edit::EditDoc::settings_hash(&s);
    let mut moved = s.clone();
    moved.geometry.volume.boxes[0][0] = 0.11;
    assert_ne!(h, shoebox_develop::edit::EditDoc::settings_hash(&moved));
    // A preset carries the projection, not the faces.
    let p = Preset::from_settings("Wide", &s, &[Part::Transform]);
    let out = p.apply(&Settings::default(), 1.0);
    assert_eq!(out.geometry.volume.mode, VolumeMode::Horizontal);
    assert_eq!(out.geometry.volume.amount, 60.0);
    assert!(!out.geometry.volume.faces);
    assert!(out.geometry.volume.boxes.is_empty());
    // Junk boxes count for nothing.
    let mut junk = Volume {
        faces: true,
        boxes: vec![[f64::NAN, 0.0, 1.0, 1.0], [0.5, 0.5, 0.4, 0.6]],
        ..Default::default()
    };
    assert_eq!(Spec::of(&junk, 1), None);
    junk.boxes = vec![[0.0, 0.0, 0.1, 0.1]; 500];
    assert_eq!(Spec::of(&junk, 1).unwrap().faces.len(), volume::MAX_FACES);
}
