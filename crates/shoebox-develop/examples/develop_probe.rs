//! Render raw files through the CPU reference pipe with default settings:
//! `cargo run --release -p shoebox-develop --example develop_probe -- <out dir> <file>...`
//! A High efficiency NEF goes through Adobe DNG Converter first (cached in
//! `<out dir>/cache`). Writes `<name>.png` (sRGB) and prints stage timings.
//! `SHOEBOX_LONG_EDGE` renders a proxy; `SHOEBOX_EV`, `SHOEBOX_CONTRAST`,
//! `SHOEBOX_WHITE_EV` and `SHOEBOX_BLACK_EV` set those sliders;
//! `SHOEBOX_EDIT` takes any settings as JSON (as an edit document stores
//! them, e.g. `{"modules":{"clarity":{"amount":40}}}`) before those; with
//! `SHOEBOX_GPU=1` the live pipe also runs on the GPU and the largest
//! difference from the CPU render is printed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Instant;

use shoebox_develop::cpu::{self, Options, Target};
use shoebox_develop::dng::{Converter, DngCache, needs_dng};
use shoebox_develop::edit::Settings;

fn main() {
    let mut args = std::env::args().skip(1);
    let out = std::path::PathBuf::from(args.next().expect("out dir"));
    std::fs::create_dir_all(&out).unwrap();
    let long_edge = std::env::var("SHOEBOX_LONG_EDGE")
        .ok()
        .and_then(|v| v.parse().ok());
    let cache = DngCache::new(&out.join("cache"), 1 << 34);
    for p in args {
        let path = Path::new(&p);
        let bytes = std::fs::read(path).unwrap();
        let orientation = shoebox_decode::nef::ifd0_orientation(&bytes)
            .ok()
            .flatten()
            .unwrap_or(1);
        let t = Instant::now();
        let source = if needs_dng(&bytes) {
            let conv = Converter::find(None).expect("Adobe DNG Converter");
            cache.get_or_convert(&conv, path, &bytes).unwrap()
        } else {
            path.to_path_buf()
        };
        let t_conv = t.elapsed();
        let raw = shoebox_decode::raw::decode_path(&source).unwrap();
        let t_dec = t.elapsed();
        let mut s = std::env::var("SHOEBOX_EDIT").map_or_else(
            |_| Settings::default(),
            |j| Settings::from_value(&serde_json::from_str(&j).unwrap()).unwrap(),
        );
        let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
        if let Some(v) = env("SHOEBOX_EV") {
            s.modules.exposure.ev = v;
        }
        let d = &mut s.modules.display_transform;
        if let Some(v) = env("SHOEBOX_CONTRAST") {
            d.contrast = v;
        }
        if let Some(v) = env("SHOEBOX_WHITE_EV") {
            d.white_ev = v;
        }
        if let Some(v) = env("SHOEBOX_BLACK_EV") {
            d.black_ev = v;
        }
        let o = Options {
            long_edge,
            orientation,
            target: Target::Srgb,
        };
        let src = cpu::demosaic(&raw, &s.modules.raw, o.long_edge);
        let frame = cpu::frame(&raw, &src, &s, &o);
        let img = cpu::apply_from(&src, &s, &frame, true);
        let t_all = t.elapsed();
        if std::env::var_os("SHOEBOX_GPU").is_some() {
            use shoebox_develop::gpu::{Output, Pipe, headless};
            let (device, queue) = headless().expect("a GPU");
            let pipe = Pipe::new(&device, Output::Rgba32Float).unwrap();
            let detailed = src.detailed(&s);
            let gsrc = pipe.upload(&device, &queue, detailed.as_deref().unwrap_or(&src.rgb));
            let t = Instant::now();
            let g = pipe.render(&device, &queue, &gsrc, &frame, true).unwrap();
            let d = img
                .data
                .iter()
                .zip(&g.data)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            println!(
                "  gpu: {:?} with read-back; largest difference from cpu {d:.2e}",
                t.elapsed()
            );
        }
        let name = path.file_stem().unwrap().to_string_lossy();
        let dst = out.join(format!("{name}.png"));
        let f = std::fs::File::create(&dst).unwrap();
        let mut e = png::Encoder::new(
            std::io::BufWriter::new(f),
            img.width as u32,
            img.height as u32,
        );
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .unwrap()
            .write_image_data(&cpu::to_u8(&img))
            .unwrap();
        println!(
            "{p}: {}×{} convert {:?}, decode {:?}, render {:?} → {}",
            img.width,
            img.height,
            t_conv,
            t_dec - t_conv,
            t_all - t_dec,
            dst.display()
        );
    }
}
