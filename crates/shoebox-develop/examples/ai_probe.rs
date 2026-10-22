//! AI raw denoise on a real file, against RCD:
//! `cargo run --release -p shoebox-develop --example ai_probe -- <model dir> <out dir> <raw file>`
//! (a High efficiency NEF goes through Adobe DNG Converter, cached in
//! `<out dir>/cache`). Prints the time, each channel's mean over RCD's
//! (near 1 when the colours agree) and writes 100% crops of both renders.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use shoebox_develop::cpu::{self, Options, Target};
use shoebox_develop::dng::{Converter, DngCache, needs_dng};
use shoebox_develop::edit::Settings;

fn main() {
    let mut args = std::env::args().skip(1);
    let model = std::path::PathBuf::from(args.next().expect("model dir"));
    let out = std::path::PathBuf::from(args.next().expect("out dir"));
    let file = std::path::PathBuf::from(args.next().expect("raw file"));
    std::fs::create_dir_all(&out).unwrap();
    let device = shoebox_ml::device();
    let net =
        shoebox_ml::rawnind::RawDenoiser::load(&model.join(shoebox_ml::rawnind::FILE), &device)
            .unwrap();
    shoebox_develop::denoise::register(
        shoebox_ml::rawnind::MODEL_ID,
        Box::new(move |p, h, w, progress, cancel| {
            net.denoise(p, h, w, progress, cancel)
                .map_err(|e| e.to_string())
        }),
    );
    let bytes = std::fs::read(&file).unwrap();
    let source = if needs_dng(&bytes) {
        let cache = DngCache::new(&out.join("cache"), 1 << 34);
        let conv = Converter::find(None).expect("Adobe DNG Converter");
        cache.get_or_convert(&conv, &file, &bytes).unwrap()
    } else {
        file.clone()
    };
    let raw = shoebox_decode::raw::decode_path(&source).unwrap();
    println!("cfa {:?}, {}×{}", raw.cfa, raw.width, raw.height);
    let s = Settings::default();
    let t = Instant::now();
    let rcd = cpu::demosaic(&raw, &s.modules.raw, None);
    println!("rcd {:.2} s", t.elapsed().as_secs_f32());
    let t = Instant::now();
    let ai = shoebox_develop::denoise::run(&raw, &s.modules.raw, &|_| {}, &AtomicBool::new(false))
        .unwrap();
    println!("ai {:.2} s", t.elapsed().as_secs_f32());
    let mean = |img: &cpu::Rgb, c: usize| {
        img.data
            .iter()
            .skip(c)
            .step_by(3)
            .map(|&v| f64::from(v))
            .sum::<f64>()
            / (img.width * img.height) as f64
    };
    for c in 0..3 {
        println!(
            "channel {c}: ai/rcd {:.3}",
            mean(&ai, c) / mean(&rcd.rgb, c)
        );
    }
    // Alignment: the shift (whole pixels) at which the two agree best.
    let luma = |img: &cpu::Rgb, x: usize, y: usize| {
        let p = img.px(x, y);
        (p[0] + p[1] + p[2]).max(1e-6).ln()
    };
    let mut best = (f64::MAX, 0i64, 0i64);
    for dy in -2i64..=2 {
        for dx in -2i64..=2 {
            let mut ssd = 0.0;
            for y in (64..raw.height - 64).step_by(7) {
                for x in (64..raw.width - 64).step_by(7) {
                    let a = luma(&ai, x, y);
                    let b = luma(&rcd.rgb, (x as i64 + dx) as usize, (y as i64 + dy) as usize);
                    ssd += f64::from((a - b) * (a - b));
                }
            }
            if ssd < best.0 {
                best = (ssd, dx, dy);
            }
        }
    }
    println!(
        "best alignment: ai(x, y) ~ rcd(x {:+}, y {:+})",
        best.1, best.2
    );
    // 100% crops from the centre, both through the same pipe.
    let o = Options {
        orientation: 1,
        target: Target::Srgb,
        long_edge: None,
    };
    let (w, h) = (raw.width, raw.height);
    let crop = [w / 2 - 384, h / 2 - 256, 768, 512];
    for (name, img) in [("rcd", rcd.rgb.clone()), ("ai", ai)] {
        let src = cpu::Demosaiced::new(img, rcd.as_shot, cpu::Referred::Scene);
        let f = cpu::frame(&raw, &src, &s, &o).region(crop);
        let px = cpu::apply(&src.rgb, &f, true);
        let path = out.join(format!(
            "{}-{name}.png",
            Path::new(&file).file_stem().unwrap().to_string_lossy()
        ));
        let mut e = png::Encoder::new(
            std::io::BufWriter::new(std::fs::File::create(&path).unwrap()),
            px.width as u32,
            px.height as u32,
        );
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .unwrap()
            .write_image_data(&cpu::to_u8(&px))
            .unwrap();
        println!("{}", path.display());
        // A 4× close-up of its middle (nearest neighbour).
        let (zw, zh, k) = (160, 110, 4);
        let (x0, y0) = (px.width / 2 - zw / 2, px.height / 2 - zh / 2);
        let mut z = Vec::with_capacity(zw * zh * k * k * 3);
        let bytes = cpu::to_u8(&px);
        for y in 0..zh * k {
            for x in 0..zw * k {
                let i = ((y0 + y / k) * px.width + x0 + x / k) * 3;
                z.extend_from_slice(&bytes[i..i + 3]);
            }
        }
        let path = path.with_extension("zoom.png");
        let mut e = png::Encoder::new(
            std::io::BufWriter::new(std::fs::File::create(&path).unwrap()),
            (zw * k) as u32,
            (zh * k) as u32,
        );
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&z).unwrap();
    }
}
