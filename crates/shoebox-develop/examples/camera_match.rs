//! Fit a camera match profile from NEF + camera JPEG pairs:
//! `cargo run --release -p shoebox-develop --example camera_match -- <folder> <profile.json>`
//!
//! Run it on an APFS clone (`cp -cR`), never on the library itself: it only
//! reads, but the rule is the rule. Pairs share a name (`DSC_1234.NEF` and
//! `DSC_1234.JPG`). Each raw is rendered with the matrix profile at the
//! exposure that matches its JPEG's median, both are shrunk, and the middle
//! of the frame (the JPEG's distortion correction moves its edges) feeds
//! the fitter. Every tenth pair is held back and the mean ΔE2000 against
//! the camera JPEGs printed, without and with the look. Copy the profile
//! into `~/Library/Application Support/<bundle id>/profiles/` for
//! Shoebox to offer "Camera match".
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use shoebox_develop::colour;
use shoebox_develop::cpu::{self, Options};
use shoebox_develop::dng::{Converter, DngCache, needs_dng};
use shoebox_develop::edit::Settings;
use shoebox_develop::profile::{Fitter, Lut3, Profile, SIZE};

const SIDE: u32 = 320;

fn pairs(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nef")) {
            for ext in ["JPG", "jpg", "JPEG", "jpeg"] {
                let j = p.with_extension(ext);
                if j.exists() {
                    out.push((p.clone(), j));
                    break;
                }
            }
        }
    }
    out.sort();
    out
}

fn inv32(m: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let d = m.map(|r| r.map(f64::from));
    colour::inv(&d)
        .unwrap_or(colour::IDENTITY)
        .map(|r| r.map(|v| v as f32))
}

fn mul3(m: &[[f32; 3]; 3], p: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2])
}

/// Pixel pairs (ours, the camera's) in the look's domain, and the Lab
/// colours of both renders (for ΔE), from the middle of the frame.
struct Sample {
    from: Vec<[f32; 3]>,
    to: Vec<[f32; 3]>,
    model: String,
}

fn sample(nef: &Path, jpg: &Path, cache: &DngCache) -> Option<Sample> {
    let bytes = std::fs::read(nef).ok()?;
    let source = if needs_dng(&bytes) {
        cache
            .get_or_convert(&Converter::find(None)?, nef, &bytes)
            .ok()?
    } else {
        nef.to_path_buf()
    };
    let raw = shoebox_decode::raw::decode_path(&source).ok()?;
    let src = cpu::demosaic(&raw, &Default::default(), Some(SIDE * 2));
    let cam = shoebox_decode::render_path(
        shoebox_decode::Format::Jpeg,
        jpg,
        Some(SIDE * 4),
        1,
        shoebox_decode::Target::Srgb,
    )
    .ok()?;
    // Exposure matched to the JPEG (Auto's measurement).
    let stats = shoebox_develop::auto::measure(&raw, &src, Some(&cam));
    let mut s = Settings::default();
    s.modules.exposure.ev = f64::from(stats.camera_ev.unwrap_or(0.0));
    let f = cpu::frame(&raw, &src, &s, &Options::default());
    let small = cpu::maps::small(&src.rgb);
    let (w, h) = (small.width, small.height);
    let cam = cam.resize(w as u32, h as u32);
    // The camera's sRGB → the look's domain: linear Rec.2020 display values
    // before the output matrix and the outset.
    let back = inv32(&f.display.to_target);
    let unout = inv32(&f.display.outset);
    let mut from = Vec::new();
    let mut to = Vec::new();
    for y in h * 15 / 100..h * 85 / 100 {
        for x in w * 15 / 100..w * 85 / 100 {
            let c = &cam.px[(y * w + x) * 3..][..3];
            if c.iter().any(|&v| !(4..=250).contains(&v)) {
                continue; // clipped in the JPEG
            }
            let lin = mul3(&f.colour, small.px(x, y));
            let ours = cpu::shape(&f.display, lin);
            let theirs = mul3(
                &unout,
                mul3(
                    &back,
                    [0, 1, 2].map(|k| cpu::srgb_eotf(f32::from(c[k]) / 255.0)),
                ),
            );
            let enc = |p: [f32; 3]| p.map(|v| v.max(0.0).powf(1.0 / 2.2).min(1.0));
            from.push(enc(ours));
            to.push(enc(theirs));
        }
    }
    Some(Sample {
        from,
        to,
        model: raw.model.clone(),
    })
}

/// Mean ΔE2000 between ours (through `lut`, if any) and the camera's, both
/// as sRGB.
fn delta_e(samples: &[Sample], lut: Option<&Lut3>) -> f64 {
    let to_xyz = colour::rgb_to_xyz(&colour::REC2020, colour::D65);
    let lab = |enc: [f32; 3]| {
        let lin = enc.map(|v| f64::from(v.max(0.0).powf(2.2)));
        colour::xyz_to_lab(colour::mulv(&to_xyz, lin), colour::D65)
    };
    let (mut sum, mut n) = (0.0, 0.0);
    for s in samples {
        for (a, b) in s.from.iter().zip(&s.to) {
            let a = lut.map_or(*a, |l| l.sample(*a));
            sum += colour::delta_e2000(lab(a), lab(*b));
            n += 1.0;
        }
    }
    if n > 0.0 { sum / n } else { 0.0 }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dir, out) = (Path::new(&args[0]), Path::new(&args[1]));
    let cache = DngCache::new(&std::env::temp_dir().join("shoebox-camera-match"), 1 << 35);
    let list = pairs(dir);
    println!("{} pairs", list.len());
    let mut fitter = Fitter::new(SIZE);
    let mut held = Vec::new();
    let mut model = String::new();
    let mut used = 0;
    // With few pairs nothing is held back: report on the ones fitted.
    let mut fitted = Vec::new();
    for (k, (nef, jpg)) in list.iter().enumerate() {
        let Some(s) = sample(nef, jpg, &cache) else {
            eprintln!("skipped {}", nef.display());
            continue;
        };
        if model.is_empty() {
            model = s.model.clone();
        }
        if k % 10 == 9 {
            held.push(s);
            continue;
        }
        for (a, b) in s.from.iter().zip(&s.to) {
            fitter.add(*a, *b);
        }
        used += 1;
        println!("{} ({} px)", nef.display(), s.from.len());
        if list.len() < 10 {
            fitted.push(s);
        }
    }
    let lut = fitter.finish(2.0);
    let delta = (!held.is_empty()).then(|| (delta_e(&held, None), delta_e(&held, Some(&lut))));
    if let Some((before, after)) = delta {
        println!("held back: mean ΔE2000 {before:.2} with the matrix, {after:.2} with the look");
    } else if !fitted.is_empty() {
        println!(
            "on the pairs fitted (too few to hold any back): mean ΔE2000 {:.2} with the matrix, {:.2} with the look",
            delta_e(&fitted, None),
            delta_e(&fitted, Some(&lut))
        );
    }
    let profile = Profile {
        camera: model,
        look: lut,
        pairs: used,
        delta_e: delta,
    };
    std::fs::write(out, serde_json::to_string(&profile).unwrap()).unwrap();
    println!("wrote {}", out.display());
}
