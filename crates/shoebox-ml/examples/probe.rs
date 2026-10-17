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
    if std::env::var_os("PROBE_TAGS").is_some() {
        let labels = shoebox_ml::labels();
        let t = Instant::now();
        let lv: Vec<Vec<f32>> = labels
            .iter()
            .map(|l| text.embed(&l.prompt))
            .collect::<Result<_, _>>()?;
        println!(
            "{} labels embedded in {:.1} s",
            labels.len(),
            t.elapsed().as_secs_f32()
        );
        // Each label's mean and spread over these images.
        let stats: Vec<(f32, f32)> = lv
            .iter()
            .map(|v| {
                let c: Vec<f32> = embs
                    .iter()
                    .map(|e| v.iter().zip(e).map(|(a, b)| a * b).sum::<f32>())
                    .collect();
                let m = c.iter().sum::<f32>() / c.len() as f32;
                let var = c.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / c.len() as f32;
                (m, var.sqrt().max(1e-3))
            })
            .collect();
        let p25: Vec<f32> = lv
            .iter()
            .map(|v| {
                let mut c: Vec<f32> = embs
                    .iter()
                    .map(|e| v.iter().zip(e).map(|(a, b)| a * b).sum::<f32>())
                    .collect();
                c.sort_by(f32::total_cmp);
                c[c.len() / 4]
            })
            .collect();
        for (e, p) in embs.iter().zip(&files) {
            let mut z: Vec<(f32, f32, &str)> = lv
                .iter()
                .zip(&labels)
                .zip(&stats)
                .map(|((v, l), (m, sd))| {
                    let c = v.iter().zip(e).map(|(a, b)| a * b).sum::<f32>();
                    ((c - m) / sd, c, l.name)
                })
                .collect();
            z.sort_by(|a, b| b.0.total_cmp(&a.0));
            let top: Vec<String> = z
                .iter()
                .take(6)
                .map(|(z, c, n)| format!("{n} z{z:.1}/{c:.3}"))
                .collect();
            println!("  z {}", top.join(", "));
            // Double-centred: minus the label's 25th percentile over these
            // images, minus this image's median over labels.
            let raw: Vec<f32> = lv
                .iter()
                .map(|v| v.iter().zip(e).map(|(a, b)| a * b).sum::<f32>())
                .collect();
            let mut sorted = raw.clone();
            sorted.sort_by(f32::total_cmp);
            let med = sorted[sorted.len() / 2];
            let mut d: Vec<(f32, f32, &str)> = raw
                .iter()
                .zip(&labels)
                .zip(&p25)
                .map(|((c, l), q)| (c - q - med, *c, l.name))
                .collect();
            d.sort_by(|a, b| b.0.total_cmp(&a.0));
            let top: Vec<String> = d
                .iter()
                .take(6)
                .map(|(d, c, n)| format!("{n} d{d:.3}/{c:.3}"))
                .collect();
            println!("  d {}", top.join(", "));
            let mut s: Vec<(f32, &str)> = lv
                .iter()
                .zip(&labels)
                .map(|(v, l)| (v.iter().zip(e).map(|(a, b)| a * b).sum::<f32>(), l.name))
                .collect();
            s.sort_by(|a, b| b.0.total_cmp(&a.0));
            let top: Vec<String> = s
                .iter()
                .take(8)
                .map(|(c, n)| format!("{n} {c:.3}"))
                .collect();
            println!(
                "{}: {}",
                p.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default(),
                top.join(", ")
            );
        }
    }
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
