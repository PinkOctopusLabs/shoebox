//! Time an HDR merge and its DNG:
//! `cargo run --release -p shoebox-develop --example merge_probe -- <out dir> [<raw file>...]`
//! With no files, writes a synthetic hand-held bracket (−2, 0, +2 EV,
//! 6048×4032, the Z f's size) to `<out dir>` first. Prints each stage's
//! time (one frame's load and alignment alone, then the whole merge and
//! the DNG), what the merge found per frame, and writes `<out dir>/HDR.dng`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use shoebox_develop::merge::{self, Input, Record, align, hdr};

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().expect("out dir"));
    std::fs::create_dir_all(&out).unwrap();
    let mut files: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if files.is_empty() {
        let t = Instant::now();
        for (i, (ev, shift, rotate)) in [
            (-2.0, (-3.1, 2.4), -0.12),
            (0.0, (0.0, 0.0), 0.0),
            (2.0, (5.6, -4.3), 0.2),
        ]
        .into_iter()
        .enumerate()
        {
            let b = shoebox_fixtures::Bracket {
                shift,
                rotate,
                seed: i as u64 + 1,
                ..shoebox_fixtures::Bracket::new(6048, 4032, ev)
            };
            let p = out.join(format!("BRACKET_{i}.dng"));
            std::fs::write(&p, b.write()).unwrap();
            files.push(p);
        }
        println!(
            "synthetic bracket written in {:.1} s",
            t.elapsed().as_secs_f64()
        );
    }
    let inputs: Vec<Input> = files.iter().map(|p| Input::new(p, 1)).collect();
    let cancel = AtomicBool::new(false);

    // One frame's stages on their own.
    let t = Instant::now();
    let a = merge::load(&inputs[0], &cancel).unwrap();
    println!(
        "load (decode, demosaic, upright) {}×{}: {:.2} s",
        a.width(),
        a.height(),
        t.elapsed().as_secs_f64()
    );
    let b = merge::load(&inputs[1], &cancel).unwrap();
    let gray = |s: &merge::Source, e: f32| {
        let mut g = align::Gray::new(s.width(), s.height());
        for (o, p) in g.data.iter_mut().zip(s.rgb.data.chunks(3)) {
            let y = 0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2];
            *o = if y > 2e-3 && p[1] < 0.9 {
                (y / e).sqrt()
            } else {
                f32::NAN
            };
        }
        g
    };
    let t = Instant::now();
    let pa = align::pyramid(gray(&a, 0.25), 64);
    let pb = align::pyramid(gray(&b, 1.0), 64);
    println!("pyramids: {:.2} s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    let al = align::align(&pb, &pa, &align::Options::default()).unwrap();
    println!(
        "align (features, RANSAC, refinement): {:.2} s, {} of {} matches, rms {:?}",
        t.elapsed().as_secs_f64(),
        al.inliers,
        al.matched,
        al.refined.map(|r| r.rms)
    );
    let t = Instant::now();
    let _ = align::warp(&a.rgb, &al.h, b.width(), b.height());
    println!("warp: {:.2} s", t.elapsed().as_secs_f64());
    drop((a, b, pa, pb));

    let t = Instant::now();
    let r = hdr::merge(&inputs, &hdr::Options::default(), &|_| {}, &cancel).unwrap();
    println!("whole merge: {:.2} s", t.elapsed().as_secs_f64());
    for (i, f) in r.frames.iter().enumerate() {
        let motion = f
            .alignment
            .map(|a| hdr::motion(&a.h, r.merged.rgb.width, r.merged.rgb.height));
        println!(
            "  {}: exposure ×{:.3} ({:?}, EXIF {:?}), motion {:?}, ghosted {:.1}%",
            files[i].display(),
            f.exposure,
            f.exposure_from,
            f.exif_exposure,
            motion,
            f.ghosted * 100.0
        );
    }
    let record = Record {
        kind: "hdr".into(),
        sources: files
            .iter()
            .map(|p| {
                let h = blake3::hash(&std::fs::read(p).unwrap())
                    .to_hex()
                    .to_string();
                (p.file_name().unwrap().to_string_lossy().into_owned(), h)
            })
            .collect(),
    };
    let t = Instant::now();
    let bytes = merge::dng(&r.merged, &record).unwrap();
    println!(
        "DNG: {:.2} s, {:.1} MB",
        t.elapsed().as_secs_f64(),
        bytes.len() as f64 / 1e6
    );
    std::fs::write(out.join("HDR.dng"), &bytes).unwrap();
}
