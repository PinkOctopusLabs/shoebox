//! Time a panorama and its DNG:
//! `cargo run --release -p shoebox-develop --example pano_probe -- <out dir> [<raw file>...]`
//! With no files, writes a synthetic hand-held row to `<out dir>` first:
//! six 6048×4032 frames (the Z f's size) of `shoebox_fixtures::pano`'s
//! scene, 30° apart, through the Nikkor Z 40 mm's distortion and
//! vignetting, exposures differing. Prints when each stage ends, what the
//! merge found, and writes `<out dir>/Pano.dng`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use rayon::prelude::*;
use shoebox_develop::merge::{self, Input, Record, pano};
use shoebox_fixtures::PanoFrame;
use shoebox_fixtures::pano::{pano_light, ray, rotation};

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().expect("out dir"));
    std::fs::create_dir_all(&out).unwrap();
    let mut files: Vec<PathBuf> = args.map(PathBuf::from).collect();
    let cancel = AtomicBool::new(false);
    if files.is_empty() && !out.join("PANO_0.dng").exists() {
        let t = Instant::now();
        let (w, h) = (6048u32, 4032u32);
        // The lens, as the merge will find it.
        let probe = out.join("probe.dng");
        std::fs::write(&probe, PanoFrame::new(w, h).write(|_, _| 0.2)).unwrap();
        let input = Input::new(&probe, 1);
        let src = merge::load(&input, &cancel).unwrap();
        let shot = pano::optics::shot(&input, &src);
        let (optics, what) = pano::optics::optics(&shot, &src, 1, true);
        drop(src);
        std::fs::remove_file(&probe).unwrap();
        println!("lens: {what}");
        let focal = 40.0 / 36f64.hypot(24.0) * f64::from(w).hypot(f64::from(h)) * 1.02;
        for (i, (yaw, pitch, roll, gain)) in [
            (-75.0, 1.0, -0.8, 1.0),
            (-45.5, -0.6, 1.2, 1.15),
            (-15.0, 0.4, 0.0, 0.9),
            (15.5, -1.2, -0.7, 1.05),
            (45.0, 0.8, 0.5, 0.95),
            (74.5, -0.3, 1.0, 1.1),
        ]
        .into_iter()
        .enumerate()
        {
            let r = rotation(yaw, pitch, roll);
            // The light at each photosite, worked out in parallel.
            let light: Vec<f64> = (0..(w * h) as usize)
                .into_par_iter()
                .map(|k| {
                    let (x, y) = ((k % w as usize) as f64, (k / w as usize) as f64);
                    let mut p = (x, y);
                    let mut a = optics.at(x, y);
                    for _ in 0..4 {
                        p = (p.0 + x - a.g[0], p.1 + y - a.g[1]);
                        a = optics.at(p.0, p.1);
                    }
                    pano_light(ray(&r, focal, w, h, p.0, p.1)) / f64::from(a.gain)
                })
                .collect();
            let f = PanoFrame {
                gain,
                seed: i as u64 + 1,
                subsec: ["10", "20", "30", "40", "50", "60"][i],
                ..PanoFrame::new(w, h)
            };
            let p = out.join(format!("PANO_{i}.dng"));
            std::fs::write(
                &p,
                f.write(|x, y| light[y as usize * w as usize + x as usize]),
            )
            .unwrap();
            files.push(p);
        }
        println!(
            "synthetic row written in {:.1} s",
            t.elapsed().as_secs_f64()
        );
    }
    if files.is_empty() {
        files = (0..6).map(|i| out.join(format!("PANO_{i}.dng"))).collect();
    }
    let inputs: Vec<Input> = files.iter().map(|p| Input::new(p, 1)).collect();
    // Resident memory, sampled: the most since the last stage ended.
    let peak = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let peak = peak.clone();
        std::thread::spawn(move || {
            let pid = std::process::id().to_string();
            loop {
                if let Ok(o) = std::process::Command::new("ps")
                    .args(["-o", "rss=", "-p", &pid])
                    .output()
                    && let Ok(kb) = String::from_utf8_lossy(&o.stdout).trim().parse::<u64>()
                {
                    peak.fetch_max(kb, std::sync::atomic::Ordering::Relaxed);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
    }
    let t = Instant::now();
    let last = std::sync::Mutex::new(-1.0f32);
    let progress = |f: f32| {
        let mut l = last.lock().unwrap();
        // Stage ends: 0.40 frames read, 0.43 matched, 0.46 adjusted, 0.48
        // gains, 0.50 seams, then each frame blended.
        if f - *l > 0.009 {
            let mb = peak.swap(0, std::sync::atomic::Ordering::Relaxed) / 1024;
            println!(
                "  {:4.0}% at {:.2} s (resident up to {mb} MB)",
                f * 100.0,
                t.elapsed().as_secs_f64()
            );
            *l = f;
        }
    };
    let p = pano::merge(&inputs, &pano::Options::default(), &progress, &cancel).unwrap();
    println!(
        "whole merge: {:.2} s: {}×{} ({:.0} MP; uncropped {:?}), {:?}, flat {}, focal {:?} (EXIF {:?}), fit {:?}",
        t.elapsed().as_secs_f64(),
        p.merged.rgb.width,
        p.merged.rgb.height,
        (p.merged.rgb.width * p.merged.rgb.height) as f64 / 1e6,
        p.uncropped,
        p.projection,
        p.flat,
        p.focal,
        p.focal_exif,
        p.fit
    );
    for (i, f) in p.frames.iter().enumerate() {
        println!(
            "  {}: gain {:.4}, {} neighbours, lens {:?}",
            files[i].display(),
            f.gain,
            f.neighbours,
            f.lens
        );
    }
    for n in &p.notes {
        println!("  note: {n}");
    }
    let record = Record {
        kind: "panorama".into(),
        sources: files
            .iter()
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    String::new(),
                )
            })
            .collect(),
    };
    peak.store(0, std::sync::atomic::Ordering::Relaxed);
    let t = Instant::now();
    let bytes = merge::dng(&p.merged, &record).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(60));
    println!(
        "DNG: {:.2} s, {:.1} MB (resident up to {} MB)",
        t.elapsed().as_secs_f64(),
        bytes.len() as f64 / 1e6,
        peak.load(std::sync::atomic::Ordering::Relaxed) / 1024
    );
    std::fs::write(out.join("Pano.dng"), &bytes).unwrap();
}
