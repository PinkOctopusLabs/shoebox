//! Try the face models on a folder: faces found, and how alike they are.
//!
//! cargo run --release -p shoebox-ml --example faces_probe -- <yunet.onnx> <sface.onnx> <images dir> [out dir]
//!
//! With an out dir, writes each image as detected (`<name>.det.png`) and
//! each aligned face (`<name>.<n>.png`), to compare with OpenCV.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context;
use shoebox_decode::{Format, Target};
use shoebox_ml::faces::{DETECT_SIZE, FaceDetector, FaceEmbedder, align};

fn png(path: PathBuf, img: &shoebox_decode::Rgb8) -> anyhow::Result<()> {
    image::RgbImage::from_raw(img.width, img.height, img.px.clone())
        .context("image")?
        .save(path)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let device = if std::env::var_os("FACES_GPU").is_some() {
        shoebox_ml::device()
    } else {
        shoebox_ml::Device::Cpu
    };
    println!("device {device:?}");
    let det = FaceDetector::load(&std::fs::read(args.next().context("yunet")?)?, &device)?;
    let rec = FaceEmbedder::load(&std::fs::read(args.next().context("sface")?)?, &device)?;
    let dir = PathBuf::from(args.next().context("images")?);
    let out = args.next().map(PathBuf::from);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| Format::from_path(p).is_some())
        .collect();
    files.sort();
    let mut all: Vec<(String, Vec<f32>)> = Vec::new();
    for p in &files {
        let name = p
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let data = std::fs::read(p)?;
        let fmt = Format::from_path(p).context("format")?;
        let big = shoebox_decode::render(fmt, &data, Some(1600), 1, Target::Srgb)?;
        let small = big.fit_long_edge(DETECT_SIZE);
        let t = Instant::now();
        let faces = det.detect(&small, 0.8)?;
        let det_ms = t.elapsed().as_secs_f64() * 1e3;
        let k = big.width as f32 / small.width as f32;
        let crops: Vec<_> = faces.iter().map(|f| align(&big, &f.scaled(k))).collect();
        let t = Instant::now();
        let embs = rec.embed(&crops)?;
        let rec_ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "{name}: {} faces ({}x{}), detect {det_ms:.0} ms, embed {rec_ms:.0} ms",
            faces.len(),
            small.width,
            small.height
        );
        for (i, f) in faces.iter().enumerate() {
            println!(
                "  {i}: score {:.3} rect {:?} eyes {:?} {:?}",
                f.score,
                f.rect.map(|v| v.round()),
                f.landmarks[0].map(|v| v.round()),
                f.landmarks[1].map(|v| v.round())
            );
        }
        if let Some(o) = &out {
            std::fs::create_dir_all(o)?;
            png(o.join(format!("{name}.det.png")), &small)?;
            for (i, c) in crops.iter().enumerate() {
                png(o.join(format!("{name}.{i}.png")), c)?;
            }
        }
        for (i, e) in embs.into_iter().enumerate() {
            all.push((format!("{name}.{i}"), e));
        }
    }
    println!("\nmost similar other face (cosine):");
    for (i, (a, ea)) in all.iter().enumerate() {
        let mut best: Vec<(f32, &str)> = all
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, (b, eb))| {
                (
                    ea.iter().zip(eb).map(|(x, y)| x * y).sum::<f32>(),
                    b.as_str(),
                )
            })
            .collect();
        best.sort_by(|x, y| y.0.total_cmp(&x.0));
        let top: Vec<String> = best
            .iter()
            .take(3)
            .map(|(c, n)| format!("{n} {c:.2}"))
            .collect();
        println!("  {a}: {}", top.join(", "));
    }
    Ok(())
}
