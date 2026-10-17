//! Try the model on a folder of images: speed, and how queries rank them.
//!
//! cargo run --release -p shoebox-ml --example probe -- <model dir> <images dir> "query" …
//!
//! `<model dir>` is a folder made by `download::fetch`, or one holding the
//! original `model.safetensors` + `tokenizer.json` (converted in place).

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use shoebox_decode::{Format, Target};
use shoebox_ml::{ImageEncoder, TextEncoder, preprocess};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let model = PathBuf::from(args.next().context("model dir")?);
    let images = PathBuf::from(args.next().context("images dir")?);
    let queries: Vec<String> = args.collect();
    if !model.join(shoebox_ml::convert::VISION_FILE).exists() {
        let t = Instant::now();
        shoebox_ml::convert::split_f16(&model.join("model.safetensors"), &model)?;
        println!("converted in {:.1} s", t.elapsed().as_secs_f32());
    }
    let device = shoebox_ml::device();
    println!("device: {device:?}");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&images)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| Format::from_path(p).is_some())
        .collect();
    files.sort();
    let t = Instant::now();
    let mut inputs = Vec::new();
    for p in &files {
        let data = std::fs::read(p)?;
        let fmt = Format::from_path(p).context("format")?;
        let img = shoebox_decode::render(fmt, &data, Some(512), 1, Target::Srgb)?;
        inputs.push(preprocess(&img));
    }
    println!(
        "decoded {} in {:.0} ms",
        files.len(),
        t.elapsed().as_secs_f64() * 1e3
    );

    let t = Instant::now();
    let enc = ImageEncoder::load(&model, &device)?;
    println!(
        "image encoder loaded in {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let t = Instant::now();
    let embs = enc.embed(&inputs)?;
    println!(
        "first batch of {} in {:.0} ms",
        inputs.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    for bs in [1usize, 8, 16, 32] {
        let batch: Vec<Vec<f32>> = inputs.iter().cycle().take(bs).cloned().collect();
        let t = Instant::now();
        let n = 3;
        for _ in 0..n {
            enc.embed(&batch)?;
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / n as f64;
        println!(
            "batch {bs:2}: {ms:6.1} ms = {:5.1} ms/image",
            ms / bs as f64
        );
    }
    drop(enc);
    println!("image-to-image cosines:");
    for (i, a) in embs.iter().enumerate() {
        let row: Vec<String> = embs
            .iter()
            .map(|b| format!("{:5.2}", a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()))
            .collect();
        println!(
            "  {} {}",
            row.join(" "),
            files[i]
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        );
    }

    let t = Instant::now();
    let text = TextEncoder::load(&model, &device)?;
    println!(
        "text encoder loaded in {:.0} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    for c in [0.0f32, 0.05, 0.08, 0.1, 0.12] {
        println!("p(cos = {c}) = {:.5}", text.probability(c));
    }
    for q in &queries {
        let t = Instant::now();
        let v = text.embed(q)?;
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let mut scored: Vec<(f32, &PathBuf)> = embs
            .iter()
            .zip(&files)
            .map(|(e, p)| (e.iter().zip(&v).map(|(a, b)| a * b).sum::<f32>(), p))
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        println!("\n\"{q}\" ({ms:.0} ms)");
        for (c, p) in scored.iter().take(4) {
            println!(
                "  {c:6.3}  p={:.4}  {}",
                text.probability(*c),
                p.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}
