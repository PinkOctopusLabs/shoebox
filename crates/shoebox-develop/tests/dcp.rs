//! DNG camera profiles: a synthetic `.dcp` read back, identity tables
//! rendering as the matrix does, a known hue shift, the tables' blend by
//! temperature, an sRGB-encoded LookTable, the tone curve keeping hue,
//! and finding profiles by contents.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use shoebox_decode::raw::{ColorMatrix, RawImage};
use shoebox_develop::cpu::{self, Demosaiced, Options, Referred, Rgb, mul3};
use shoebox_develop::dcp::{self, Dcp, Stage, Table};
use shoebox_develop::edit::{FileRef, Profile, Settings};

const ZF_A: [f32; 9] = [
    1.3904, -0.7947, 0.0654, -0.432, 1.2105, 0.2497, -0.0235, 0.083, 0.9243,
];
const ZF_D65: [f32; 9] = [
    1.1607, -0.4491, -0.0977, -0.4522, 1.246, 0.2304, -0.0458, 0.1519, 0.7616,
];

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
                temperature: 2850.0,
                xyz_to_cam: m(ZF_A),
            },
            ColorMatrix {
                temperature: 6500.0,
                xyz_to_cam: m(ZF_D65),
            },
        ],
        make: "Nikon".into(),
        model: "Z f".into(),
        linear: None,
    }
}

/// A TIFF value for the writer.
enum Val {
    Short(Vec<u16>),
    Long(Vec<u32>),
    Srational(Vec<f32>),
    Float(Vec<f32>),
    Ascii(&'static str),
}

/// A little-endian DCP ("IIRC") with these tags in IFD 0.
fn dcp_bytes(mut tags: Vec<(u16, Val)>) -> Vec<u8> {
    tags.sort_by_key(|t| t.0);
    let n = tags.len();
    let ifd_len = 2 + n * 12 + 4;
    let mut out = b"IIRC".to_vec();
    out.extend(8u32.to_le_bytes());
    let mut data = Vec::new();
    let mut ifd = Vec::new();
    ifd.extend((n as u16).to_le_bytes());
    for (tag, v) in tags {
        let (typ, count, bytes): (u16, usize, Vec<u8>) = match v {
            Val::Short(v) => (3, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            Val::Long(v) => (4, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            Val::Srational(v) => (
                10,
                v.len(),
                v.iter()
                    .flat_map(|x| {
                        let d = 1_000_000i32;
                        let n = (x * d as f32).round() as i32;
                        n.to_le_bytes().into_iter().chain(d.to_le_bytes())
                    })
                    .collect(),
            ),
            Val::Float(v) => (
                11,
                v.len(),
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
            Val::Ascii(s) => {
                let mut b = s.as_bytes().to_vec();
                b.push(0);
                (2, b.len(), b)
            }
        };
        ifd.extend(tag.to_le_bytes());
        ifd.extend(typ.to_le_bytes());
        ifd.extend((count as u32).to_le_bytes());
        if bytes.len() <= 4 {
            let mut b = bytes.clone();
            b.resize(4, 0);
            ifd.extend(b);
        } else {
            let at = 8 + ifd_len + data.len();
            ifd.extend((at as u32).to_le_bytes());
            data.extend(bytes);
            if data.len() % 2 == 1 {
                data.push(0);
            }
        }
    }
    ifd.extend(0u32.to_le_bytes());
    out.extend(ifd);
    out.extend(data);
    out
}

/// A table's entries, value-major, then hue, then saturation.
fn entries(
    hues: usize,
    sats: usize,
    vals: usize,
    f: impl Fn(usize, usize, usize) -> [f32; 3],
) -> Vec<f32> {
    let mut v = Vec::new();
    for vi in 0..vals {
        for h in 0..hues {
            for s in 0..sats {
                v.extend(f(vi, h, s));
            }
        }
    }
    v
}

/// The Z f's matrices, identity tables, and the extras.
fn identity_profile(name: &'static str) -> Vec<u8> {
    dcp_bytes(vec![
        (50708, Val::Ascii("Nikon Z f")),
        (50936, Val::Ascii(name)),
        (50721, Val::Srational(ZF_A.to_vec())),
        (50722, Val::Srational(ZF_D65.to_vec())),
        (50778, Val::Short(vec![17])),
        (50779, Val::Short(vec![21])),
        (50937, Val::Long(vec![90, 30, 1])),
        (
            50938,
            Val::Float(entries(90, 30, 1, |_, _, _| [0.0, 1.0, 1.0])),
        ),
        (
            50939,
            Val::Float(entries(90, 30, 1, |_, _, _| [0.0, 1.0, 1.0])),
        ),
        (50981, Val::Long(vec![36, 8, 16])),
        (
            50982,
            Val::Float(entries(36, 8, 16, |_, _, _| [0.0, 1.0, 1.0])),
        ),
        (51108, Val::Long(vec![1])),
        (51110, Val::Long(vec![1])),
    ])
}

fn synthetic(w: usize, h: usize) -> Rgb {
    let mut img = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let px = match (y * 3) / h {
                0 => [u * u * 4.0; 3],
                1 => [u * 2.0, v * 1.5, (1.0 - u) * 0.8],
                _ => [0.9 * u, 0.2 + 0.3 * v, 0.05 + u * v],
            };
            let i = (y * w + x) * 3;
            img.data[i..i + 3].copy_from_slice(&px);
        }
    }
    img
}

#[test]
fn a_profile_reads_back() {
    let mut tone = Vec::new();
    for i in 0..=16 {
        let x = i as f32 / 16.0;
        tone.extend([x, x.powf(0.8)]);
    }
    let bytes = dcp_bytes(vec![
        (50708, Val::Ascii("Nikon Z f")),
        (50936, Val::Ascii("Z f ColorChecker")),
        (50721, Val::Srational(ZF_D65.to_vec())),
        (50722, Val::Srational(ZF_A.to_vec())),
        (50778, Val::Short(vec![21])),
        (50779, Val::Short(vec![17])),
        (50937, Val::Long(vec![6, 2, 1])),
        (
            50938,
            Val::Float(entries(6, 2, 1, |_, h, _| [h as f32, 1.0, 1.0])),
        ),
        (
            50939,
            Val::Float(entries(6, 2, 1, |_, _, _| [0.0, 1.0, 1.0])),
        ),
        (50940, Val::Float(tone)),
        (51109, Val::Srational(vec![-0.5])),
        (51110, Val::Long(vec![1])),
    ]);
    let d = dcp::parse(&bytes, "fallback").unwrap();
    assert_eq!(d.name, "Z f ColorChecker");
    assert_eq!(d.camera, "Nikon Z f");
    assert_eq!(d.matrices.len(), 2);
    assert!(d.forward.is_empty());
    // The tables follow their illuminants, the cooler first.
    assert_eq!(d.hue_sat.len(), 2);
    assert_eq!(d.hue_sat[0].0, 2850.0);
    assert_eq!(d.hue_sat[1].0, 6500.0);
    assert_eq!(d.hue_sat[0].1.data[2], [0.0, 1.0, 1.0]);
    assert_eq!(d.hue_sat[1].1.data[2], [1.0, 1.0, 1.0]);
    assert_eq!(d.tone_points.len(), 17);
    assert!(d.tone.is_some());
    assert!((d.exposure_offset + 0.5).abs() < 1e-6);
    assert_eq!(d.black_render, 1);
    assert!(d.fits("Nikon", "Z f"));
    assert!(d.fits("NIKON CORPORATION", "z  f"));
    assert!(!d.fits("Nikon", "Z 8"));
    assert!(dcp::parse(b"II*\0nope", "x").is_err());
    assert!(dcp::parse(b"not a profile", "x").is_err());
}

#[test]
fn identity_tables_render_as_the_matrix() {
    let raw = zf();
    let d = dcp::register(dcp::parse(&identity_profile("Identity"), "identity").unwrap());
    let src = Demosaiced::new(
        synthetic(48, 36),
        [1.1425778, 1.0, 1.7753916],
        Referred::Scene,
    );
    let o = Options::default();
    let matrix = Settings::default();
    let mut with = Settings::default();
    with.modules.input_colour.profile = Profile::Dcp;
    with.modules.input_colour.dcp = Some(d.file_ref());
    let f = cpu::frame(&raw, &src, &with, &o);
    assert!(f.dcp.is_some(), "the tables are on");
    for display in [true, false] {
        let a = cpu::apply(&src.rgb, &cpu::frame(&raw, &src, &matrix, &o), display);
        let b = cpu::apply(&src.rgb, &f, display);
        let scale = if display {
            1.0
        } else {
            a.data.iter().fold(1.0f32, |m, v| m.max(v.abs()))
        };
        let worst = a
            .data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| (x - y).abs() / scale)
            .fold(0.0f32, f32::max);
        // The profile's matrices are its rationals, scaled as the DNG SDK
        // does (by 5e-5 here); the tables change nothing.
        assert!(worst < 2e-4, "display {display}: {worst}");
    }
    // A profile that's gone renders with the matrix.
    let mut gone = with.clone();
    gone.modules.input_colour.dcp = Some(FileRef {
        name: "Identity".into(),
        blake3: "0".repeat(64),
    });
    let f = cpu::frame(&raw, &src, &gone, &o);
    assert!(f.dcp.is_none());
}

/// A profile with these HueSatMap tables (cooler first) and no other.
fn with_tables(tables: Vec<(f64, Table)>, look: Option<Table>) -> Arc<Dcp> {
    Arc::new(Dcp {
        name: "test".into(),
        camera: "Nikon Z f".into(),
        blake3: "test".into(),
        matrices: Vec::new(),
        forward: Vec::new(),
        hue_sat: tables,
        look,
        tone_points: Vec::new(),
        tone: None,
        exposure_offset: 0.0,
        black_render: 0,
    })
}

fn shifted(degrees: f32) -> Table {
    Table {
        data: vec![[degrees, 1.0, 1.0]; 6 * 2],
        ..Table::identity(6, 2, 1)
    }
}

#[test]
fn a_hue_shift_turns_hues() {
    let st = Stage::new(
        with_tables(vec![(5000.0, shifted(120.0))], None),
        5000.0,
        1.0,
    )
    .unwrap();
    // ProPhoto (0.5, 0.1, 0.1), red, turned 120°: green.
    let lin = mul3(&st.from_pp, [0.5, 0.1, 0.1]);
    let out = mul3(&st.to_pp, st.apply(lin));
    for (a, b) in out.iter().zip([0.1, 0.5, 0.1]) {
        assert!((a - b).abs() < 1e-5, "{out:?}");
    }
    // Two tables blend by the white's temperature (inverse temperature).
    let d = with_tables(vec![(2850.0, shifted(120.0)), (6500.0, shifted(0.0))], None);
    let hue = |temp: f64| {
        let st = Stage::new(d.clone(), temp, 1.0).unwrap();
        let out = mul3(&st.to_pp, st.apply(mul3(&st.from_pp, [0.5, 0.1, 0.1])));
        dcp::hsv(out)[0] * 60.0
    };
    assert!((hue(2850.0) - 120.0).abs() < 1e-3);
    assert!((hue(2000.0) - 120.0).abs() < 1e-3);
    assert!(hue(6500.0).abs() < 1e-3);
    let w = shoebox_develop::wb::weight(2850.0, 6500.0, 4000.0);
    assert!(
        (hue(4000.0) - 120.0 * w as f32).abs() < 1e-2,
        "{}",
        hue(4000.0)
    );
    assert!(w > 0.3 && w < 0.7);
}

#[test]
fn an_srgb_look_table_scales_encoded_values() {
    let look = Table {
        data: vec![[0.0, 1.0, 0.5]; 6 * 2 * 3],
        srgb: true,
        ..Table::identity(6, 2, 3)
    };
    let st = Stage::new(with_tables(Vec::new(), Some(look)), 5000.0, 4.0).unwrap();
    for v in [0.05f32, 0.2, 0.7] {
        let lin = mul3(&st.from_pp, [v, v * 0.5, v * 0.25]);
        let out = mul3(&st.to_pp, st.apply(lin));
        let want = cpu::srgb_eotf(cpu::srgb_oetf(v) * 0.5);
        assert!((out[0] - want).abs() < 1e-5, "{v}: {out:?} vs {want}");
        // Hue and saturation stay.
        assert!((out[1] / out[0] - 0.5).abs() < 1e-4);
    }
}

#[test]
fn the_tone_curve_keeps_hue() {
    let line: Vec<[f32; 2]> = (0..=10)
        .map(|i| [i as f32 / 10.0, i as f32 / 10.0])
        .collect();
    let bright: Vec<[f32; 2]> = (0..=100)
        .map(|i| {
            let x = i as f32 / 100.0;
            [x, x.powf(0.6)]
        })
        .collect();
    let d = |pts: &[[f32; 2]]| {
        let mut bytes = vec![];
        for p in pts {
            bytes.extend(p);
        }
        dcp::parse(
            &dcp_bytes(vec![
                (50721, Val::Srational(ZF_D65.to_vec())),
                (50940, Val::Float(bytes)),
            ]),
            "tone",
        )
        .unwrap()
    };
    let identity = d(&line).tone.unwrap();
    let p = [0.6, 0.3, 0.1];
    let q = dcp::rgb_tone(&identity, p);
    assert!(p.iter().zip(q).all(|(a, b)| (a - b).abs() < 2e-4), "{q:?}");
    let t = d(&bright).tone.unwrap();
    let q = dcp::rgb_tone(&t, p);
    assert!(q[0] > p[0] && q[2] > p[2]);
    assert!((dcp::hsv(q)[0] - dcp::hsv(p)[0]).abs() < 1e-4);
    // Greys go through the curve as they are.
    let g = dcp::rgb_tone(&t, [0.25; 3]);
    assert!((g[0] - 0.25f32.powf(0.6)).abs() < 1e-3, "{g:?}");
}

#[test]
fn profiles_load_from_a_folder_and_are_found_by_contents() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("one.dcp"), identity_profile("One")).unwrap();
    std::fs::write(dir.path().join("Two.DCP"), identity_profile("Two")).unwrap();
    std::fs::write(dir.path().join("broken.dcp"), b"IIRC").unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"hello").unwrap();
    assert_eq!(dcp::load_dir(dir.path()), 2);
    let list = dcp::for_camera("Nikon", "Z f");
    let names: Vec<_> = list.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"One") && names.contains(&"Two"),
        "{names:?}"
    );
    assert!(dcp::for_camera("Nikon", "Z 8").is_empty());
    let one = dcp::named("One").unwrap();
    assert!(dcp::find(&one.file_ref()).is_some());
    let changed = FileRef {
        name: "One".into(),
        blake3: "f".repeat(64),
    };
    assert!(dcp::find(&changed).is_none());
    // Nothing was written into the folder.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
}
