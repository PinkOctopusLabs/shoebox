//! Time AVIF and linear DNG exports of raw files (default settings):
//! `cargo run --release -p shoebox-develop --example export_probe -- <out dir> <file>...`
//! A High efficiency NEF goes through Adobe DNG Converter first (cached in
//! `<out dir>/cache`). Writes `<name>_s<speed>.avif` (Display P3) for each
//! speed in `SHOEBOX_AVIF_SPEEDS` (default `4,6,8`; quality
//! `SHOEBOX_AVIF_QUALITY`, default 80; `SHOEBOX_AVIF_8BIT=1` for 8 bits),
//! `<name>.jpg` (quality 92, for comparison) and `<name>.dng`, and prints
//! times and sizes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Instant;

use shoebox_develop::cpu::{self, Options, Target};
use shoebox_develop::dng::{Converter, DngCache, needs_dng};
use shoebox_develop::edit::Settings;
use shoebox_develop::encode::{Avif, Jpeg, avif, jpeg};

fn main() {
    let mut args = std::env::args().skip(1);
    let out = std::path::PathBuf::from(args.next().expect("out dir"));
    std::fs::create_dir_all(&out).unwrap();
    let speeds: Vec<u8> = std::env::var("SHOEBOX_AVIF_SPEEDS")
        .unwrap_or_else(|_| "4,6,8".into())
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let quality = std::env::var("SHOEBOX_AVIF_QUALITY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(80);
    let ten_bit = std::env::var("SHOEBOX_AVIF_8BIT").map_or(true, |v| v != "1");
    let cache = DngCache::new(&out.join("cache"), 1 << 34);
    let mb = |n: usize| n as f64 / 1e6;
    for p in args {
        let path = Path::new(&p);
        let bytes = std::fs::read(path).unwrap();
        let orientation = shoebox_decode::nef::ifd0_orientation(&bytes)
            .ok()
            .flatten()
            .unwrap_or(1);
        let source = if needs_dng(&bytes) {
            let conv = Converter::find(None).expect("Adobe DNG Converter");
            cache.get_or_convert(&conv, path, &bytes).unwrap()
        } else {
            path.to_path_buf()
        };
        let raw = shoebox_decode::raw::decode_path(&source).unwrap();
        let s = Settings::default();
        let o = Options {
            long_edge: None,
            orientation,
            target: Target::DisplayP3,
        };
        let src = cpu::demosaic(&raw, &s.modules.raw, None);
        let t = Instant::now();
        let img = cpu::render_from(&raw, &src, &s, &o, true);
        println!(
            "{p}: {}×{}, render {:?}",
            img.width,
            img.height,
            t.elapsed()
        );
        let name = path.file_stem().unwrap().to_string_lossy();
        let t = Instant::now();
        let j = jpeg(
            &img,
            Target::DisplayP3,
            Jpeg {
                quality: 92,
                full_chroma: true,
                progressive: true,
            },
        )
        .unwrap();
        println!(
            "  JPEG 92: {:?}, {:.1} MB{}",
            t.elapsed(),
            mb(j.len()),
            psnr(&img, &j, "public.jpeg")
        );
        std::fs::write(out.join(format!("{name}.jpg")), &j).unwrap();
        for &speed in &speeds {
            let t = Instant::now();
            let a = avif(
                &img,
                Target::DisplayP3,
                Avif {
                    quality,
                    ten_bit,
                    speed,
                },
            )
            .unwrap();
            println!(
                "  AVIF q{quality} speed {speed}{}: {:?}, {:.1} MB{}",
                if ten_bit { " 10-bit" } else { " 8-bit" },
                t.elapsed(),
                mb(a.len()),
                psnr(&img, &a, "public.avif")
            );
            std::fs::write(out.join(format!("{name}_s{speed}.avif")), &a).unwrap();
        }
        let t = Instant::now();
        let d = shoebox_develop::export::linear_dng(&raw, &src, &s, orientation).unwrap();
        println!("  linear DNG: {:?}, {:.1} MB", t.elapsed(), mb(d.len()));
        std::fs::write(out.join(format!("{name}.dng")), &d).unwrap();
    }
}

/// The file read back by macOS (ImageIO, in Display P3) against the render,
/// as PSNR over 8-bit values.
#[cfg(target_os = "macos")]
fn psnr(img: &cpu::Rgb, file: &[u8], uti: &str) -> String {
    let Ok(back) = shoebox_decode::imageio::thumbnail_rgb(
        file,
        1 << 14,
        false,
        Some(uti),
        shoebox_decode::Target::DisplayP3,
    ) else {
        return ", unreadable by ImageIO".into();
    };
    let want = cpu::to_u8(img);
    let mse = want
        .iter()
        .zip(&back.px)
        .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
        .sum::<f64>()
        / want.len() as f64;
    format!(", PSNR {:.1} dB", 10.0 * (255.0f64.powi(2) / mse).log10())
}

#[cfg(not(target_os = "macos"))]
fn psnr(_img: &cpu::Rgb, _file: &[u8], _uti: &str) -> String {
    String::new()
}
