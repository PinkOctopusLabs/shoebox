//! Try the scene model (landscape masks) on photos: timings, how much of
//! each picture each part covers, and with an out dir each part as a PNG
//! (`<name>.<part>.png`, at the model's grid, white where it's found) and
//! the picture seen (`<name>.png`).
//!
//! cargo run --release -p shoebox-ml --example scene_probe -- <checkpoint> <out dir> <photos…>

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use shoebox_decode::{Format, Target};
use shoebox_ml::scene::{Part, SceneModel};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let checkpoint = PathBuf::from(args.next().context("checkpoint")?);
    let out = PathBuf::from(args.next().context("out dir")?);
    std::fs::create_dir_all(&out)?;
    let device = shoebox_ml::device();
    println!("device {device:?}");
    let t = Instant::now();
    let model = SceneModel::load(&checkpoint, &device)?;
    println!("load {:.2} s", t.elapsed().as_secs_f32());
    for p in args.map(PathBuf::from) {
        let name = p
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let fmt = Format::from_path(&p).context("format")?;
        let img = shoebox_decode::render(fmt, &std::fs::read(&p)?, Some(2560), 1, Target::Srgb)?;
        let (w, h) = (img.width as usize, img.height as usize);
        image::RgbImage::from_raw(img.width, img.height, img.px.clone())
            .context("image")?
            .save(out.join(format!("{name}.png")))?;
        for run in 0..2 {
            let t = Instant::now();
            let s = model.segment(&img.px, w, h)?;
            let ms = t.elapsed().as_secs_f64() * 1e3;
            if run == 0 {
                println!(
                    "{name}: {w}×{h} → {}×{}, first {ms:.0} ms",
                    s.width, s.height
                );
                continue;
            }
            print!("{name}: again {ms:.0} ms;");
            for part in Part::ALL {
                let v = s.part(part);
                let cover = v.iter().sum::<f32>() / v.len().max(1) as f32;
                print!(" {part:?} {:.0}%", cover * 100.0);
                let bytes: Vec<u8> = v.iter().map(|x| (x * 255.0).round() as u8).collect();
                image::GrayImage::from_raw(s.width as u32, s.height as u32, bytes)
                    .context("mask")?
                    .save(out.join(format!("{name}.{part:?}.png").to_lowercase()))?;
            }
            println!();
        }
    }
    Ok(())
}
