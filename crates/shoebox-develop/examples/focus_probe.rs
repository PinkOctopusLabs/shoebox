//! Time a focus stack and its DNG:
//! `cargo run --release -p shoebox-develop --example focus_probe -- <out dir> [pyramid|depth] [<raw file>...]`
//! With no files, writes a synthetic 10-frame focus bracket (6048×4032,
//! the Z f's size; near to far, 0.3% of breathing a frame) to `<out dir>`
//! first. Prints one frame's load and alignment alone, then each frame's
//! time in the whole stack, what it found per frame, and the DNG's; writes
//! `<out dir>/Focus.dng`. Peak memory: run it under `/usr/bin/time -l`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use shoebox_develop::merge::focus::{self, Method};
use shoebox_develop::merge::{self, Input, Record, align};

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let out = PathBuf::from(args.next().expect("out dir"));
    std::fs::create_dir_all(&out).unwrap();
    let method = match args.peek().map(String::as_str) {
        Some("depth") => {
            args.next();
            Method::DepthMap
        }
        Some("pyramid") => {
            args.next();
            Method::Pyramid
        }
        _ => Method::Pyramid,
    };
    let mut files: Vec<PathBuf> = args.map(PathBuf::from).collect();
    if files.is_empty() {
        let t = Instant::now();
        let n = 10;
        for k in 0..n {
            let p = out.join(format!("FOCUS_{k:02}.dng"));
            if !p.exists() {
                let d = k as f64 - 4.0;
                let f = shoebox_fixtures::FocusFrame {
                    scale: 1.0 + 0.003 * d,
                    shift: (1.5 * d, -0.8 * d),
                    blur: 3.0,
                    seed: k as u64 + 1,
                    ..shoebox_fixtures::FocusFrame::new(6048, 4032, k as f64 / (n - 1) as f64)
                };
                std::fs::write(&p, f.write()).unwrap();
            }
            files.push(p);
        }
        println!(
            "synthetic stack written in {:.1} s",
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
    let gray = |s: &merge::Source| {
        let mut g = align::Gray::new(s.width(), s.height());
        for (o, p) in g.data.iter_mut().zip(s.rgb.data.chunks(3)) {
            *o = 0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2];
        }
        g
    };
    let t = Instant::now();
    let pa = align::pyramid(gray(&a), 64);
    let pb = align::pyramid(gray(&b), 64);
    println!("pyramids: {:.2} s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    let o = align::Options {
        model: align::Model::Similarity,
        ..Default::default()
    };
    let al = align::align(&pa, &pb, &o).unwrap();
    println!(
        "align (features, RANSAC, refinement): {:.2} s, {} of {} matches, rms {:?}",
        t.elapsed().as_secs_f64(),
        al.inliers,
        al.matched,
        al.refined.map(|r| r.rms)
    );
    let t = Instant::now();
    let _ = align::warp(&b.rgb, &al.h, a.width(), a.height());
    println!("warp: {:.2} s", t.elapsed().as_secs_f64());
    drop((a, b, pa, pb));

    let t = Instant::now();
    let last = Mutex::new(Instant::now());
    let progress = |f: f32| {
        let mut l = last.lock().unwrap();
        println!("  {:3.0}%  +{:.2} s", f * 100.0, l.elapsed().as_secs_f64());
        *l = Instant::now();
    };
    let o = focus::Options {
        method,
        ..Default::default()
    };
    let r = focus::merge(&inputs, &o, &progress, &cancel).unwrap();
    println!(
        "whole stack ({}, {} frames): {:.2} s, crop {:?}, at most {} frames held",
        method.label(),
        files.len(),
        t.elapsed().as_secs_f64(),
        r.crop,
        r.peak_frames
    );
    for (i, f) in r.frames.iter().enumerate() {
        println!(
            "  {}: scale {:.5}, gain {:.4}, {}",
            files[i].display(),
            focus::scale(&f.h),
            f.gain,
            f.alignment
                .map(|a| format!(
                    "{} of {} matches, rms {:?}",
                    a.inliers,
                    a.matched,
                    a.refined.map(|r| r.rms)
                ))
                .unwrap_or_else(|| f.align_failed.unwrap_or("reference").into())
        );
    }
    let record = Record {
        kind: "focus".into(),
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
    std::fs::write(out.join("Focus.dng"), &bytes).unwrap();
}
