//! Looks (`.cube`): tiny synthetic files read back (3D, 1D, a shaper
//! before a 3D table, domains, comments), malformed ones refused, the
//! Amount, and renders finding a look by contents.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb};
use shoebox_develop::cube::{self, Cube, CubeError, Stage};
use shoebox_develop::edit::{FileRef, Settings};

/// A 3D table of `n` a side, red fastest, from `f` on 0–1 inputs.
fn cube3(n: usize, head: &str, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
    let mut s = format!("{head}\nLUT_3D_SIZE {n}\n");
    let k = (n - 1) as f32;
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let o = f([r as f32 / k, g as f32 / k, b as f32 / k]);
                s += &format!("{:.6} {:.6} {:.6}\n", o[0], o[1], o[2]);
            }
        }
    }
    s
}

fn parse(text: &str) -> Cube {
    cube::parse(text, "test", "hash").unwrap()
}

fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < tol)
}

#[test]
fn a_3d_cube_reads_back() {
    let text = cube3(
        3,
        "# A comment\nTITLE \"Fade\"\n\nLUT_IN_VIDEO_RANGE\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 1 1",
        |p| p.map(|v| 0.1 + 0.8 * v),
    );
    let c = parse(&text);
    assert_eq!(c.title.as_deref(), Some("Fade"));
    assert_eq!(c.lut.size, 3);
    assert_eq!(c.lut.data.len(), 27);
    assert!(close(c.sample([0.0; 3]), [0.1; 3], 1e-6));
    assert!(close(c.sample([0.5, 0.25, 1.0]), [0.5, 0.3, 0.9], 1e-5));
    // Red fastest: the second row is red's next step.
    assert!(close(c.lut.data[1], [0.5, 0.1, 0.1], 1e-6));
}

#[test]
fn domains_and_input_ranges_scale_the_input() {
    let c = parse(&cube3(2, "DOMAIN_MAX 2 2 2", |p| p));
    assert!(close(c.sample([1.0, 0.5, 2.0]), [0.5, 0.25, 1.0], 1e-6));
    let c = parse(&cube3(2, "LUT_3D_INPUT_RANGE 0 4", |p| p));
    assert!(close(c.sample([1.0, 2.0, 4.0]), [0.25, 0.5, 1.0], 1e-6));
}

#[test]
fn a_1d_cube_becomes_a_table() {
    let mut text = "TITLE \"Gamma\"\nLUT_1D_SIZE 5\n".to_owned();
    for i in 0..5 {
        let x = i as f32 / 4.0;
        text += &format!("{} {} {}\n", x * x, x, 1.0 - x);
    }
    let c = parse(&text);
    assert_eq!(c.lut.size, 33);
    // Per channel, linear between the file's points.
    let e = [0.5, 0.3, 0.75];
    let curve = |pts: &dyn Fn(f32) -> f32, x: f32| {
        let p = x * 4.0;
        let i = (p.floor() as usize).min(3);
        let f = p - i as f32;
        pts(i as f32 / 4.0) + (pts((i + 1) as f32 / 4.0) - pts(i as f32 / 4.0)) * f
    };
    let want = [
        curve(&|x| x * x, e[0]),
        curve(&|x| x, e[1]),
        curve(&|x| 1.0 - x, e[2]),
    ];
    assert!(
        close(c.sample(e), want, 2e-3),
        "{:?} vs {want:?}",
        c.sample(e)
    );
}

#[test]
fn a_shaper_before_a_3d_table_is_folded_in() {
    // A shaper that halves, then a 3D table that doubles: about the
    // identity.
    let mut text = "LUT_1D_SIZE 2\nLUT_3D_SIZE 5\n0 0 0\n0.5 0.5 0.5\n".to_owned();
    let body = cube3(5, "", |p| p.map(|v| (v * 2.0).min(1.0)));
    for l in body
        .lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
    {
        text += l;
        text += "\n";
    }
    let c = parse(&text);
    assert_eq!(c.lut.size, 5);
    assert!(close(c.sample([0.25, 0.5, 0.75]), [0.25, 0.5, 0.75], 1e-5));
}

#[test]
fn malformed_files_are_refused() {
    let err = |t: &str| cube::parse(t, "x", "h").unwrap_err();
    assert_eq!(err("TITLE \"x\"\n0 0 0\n"), CubeError::NoSize);
    assert_eq!(err("LUT_3D_SIZE 2\n0 0 0\n"), CubeError::Count(1, 8));
    assert_eq!(err("LUT_3D_SIZE 66\n"), CubeError::TooLarge(66));
    assert!(matches!(
        err("LUT_3D_SIZE 2\n0 0 zero\n"),
        CubeError::Line(2, _)
    ));
    assert!(matches!(err("LUT_3D_SIZE 1\n"), CubeError::Line(1, _)));
    assert!(matches!(err("LUT_3D_SIZE 2\n0 0\n"), CubeError::Line(2, _)));
    let text = cube3(2, "DOMAIN_MIN 1 1 1\nDOMAIN_MAX 1 1 1", |p| p);
    assert!(matches!(err(&text), CubeError::Line(0, _)));
    // 65 is the largest read.
    assert_eq!(parse(&cube3(65, "", |p| p)).lut.size, 65);
}

/// The display-linear Rec.2020 value of an sRGB-encoded colour.
fn from_srgb(e: [f32; 3], st: &Stage) -> [f32; 3] {
    let back = st.back;
    cpu::mul3(&back, e.map(cpu::srgb_eotf))
}

#[test]
fn the_amount_mixes_the_look_in() {
    let c = std::sync::Arc::new(parse(&cube3(2, "", |p| p.map(|v| 1.0 - v))));
    let identity: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let full = Stage::new(c.clone(), 100.0, &identity);
    let p = from_srgb([0.2, 0.5, 0.9], &full);
    // Inverted, as encoded sRGB.
    let q = cpu::mul3(&full.to_srgb, full.apply(p)).map(cpu::srgb_oetf);
    assert!(close(q, [0.8, 0.5, 0.1], 1e-4), "{q:?}");
    // None of it at 0, half at 50.
    let none = Stage::new(c.clone(), 0.0, &identity);
    assert_eq!(none.apply(p), p);
    let half = Stage::new(c, 50.0, &identity);
    let h = half.apply(p);
    let f = full.apply(p);
    for k in 0..3 {
        assert!((h[k] - (p[k] + f[k]) / 2.0).abs() < 1e-6);
    }
    // An identity look changes nothing in sRGB's gamut.
    let id = Stage::new(
        std::sync::Arc::new(parse(&cube3(17, "", |p| p))),
        100.0,
        &identity,
    );
    let p = from_srgb([0.3, 0.6, 0.05], &id);
    assert!(close(id.apply(p), p, 1e-5));
}

#[test]
fn renders_find_looks_by_contents() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Fade.cube"),
        cube3(5, "", |p| p.map(|v| 0.1 + 0.8 * v)),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("Warm.CUBE"),
        cube3(2, "", |p| [p[0], p[1], p[2] * 0.8]),
    )
    .unwrap();
    std::fs::write(dir.path().join("broken.cube"), "LUT_3D_SIZE 2\n").unwrap();
    std::fs::write(dir.path().join("readme.txt"), "hi").unwrap();
    assert_eq!(cube::load_dir(dir.path()), 2);
    let fade = cube::named("Fade").unwrap();
    let names: Vec<_> = cube::all().iter().map(|c| c.name.clone()).collect();
    assert!(names.contains(&"Fade".into()) && names.contains(&"Warm".into()));
    let raw = shoebox_decode::raw::RawImage {
        width: 2,
        height: 2,
        cfa: [[0, 1], [1, 2]],
        data: vec![0.0; 4],
        as_shot: None,
        matrices: Vec::new(),
        make: String::new(),
        model: String::new(),
        linear: None,
    };
    let src = Demosaiced::new(Rgb::new(8, 8), [1.0; 3], Referred::Scene);
    let mut s = Settings::default();
    s.modules.look_lut.lut = Some(fade.file_ref());
    assert!(
        cpu::frame(&raw, &src, &s, &Options::default())
            .display
            .cube
            .is_some()
    );
    s.modules.look_lut.amount = 0.0;
    assert!(
        cpu::frame(&raw, &src, &s, &Options::default())
            .display
            .cube
            .is_none()
    );
    s.modules.look_lut.amount = 100.0;
    s.modules.look_lut.lut = Some(FileRef {
        name: "Fade".into(),
        blake3: "0".repeat(64),
    });
    assert!(
        cpu::frame(&raw, &src, &s, &Options::default())
            .display
            .cube
            .is_none()
    );
    // Only read.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
}
