//! Remove something from a real photo with MI-GAN, on the proxy and at
//! full size, and time each part:
//! `cargo run --release -p shoebox-develop --example remove_probe -- <model dir> <out dir> <raw file> <radius> <x,y> <x,y>…`
//! (the stroke's points and radius in long-edge units). Writes
//! `<name>-<proxy|full>-<before|after>.png`: the crop the model saw,
//! rendered with default settings. A High efficiency NEF goes through
//! Adobe DNG Converter first.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use shoebox_develop::cpu::{self, Options, Rgb, Target};
use shoebox_develop::edit::{Settings, Spot, SpotKind, Stroke};
use shoebox_develop::heal::remove;
use shoebox_ml::inpaint::{FILE, Inpainter, MODEL_ID, SIZE};

fn main() {
    let mut args = std::env::args().skip(1);
    let model = PathBuf::from(args.next().expect("model dir"));
    let out = PathBuf::from(args.next().expect("out dir"));
    let file = PathBuf::from(args.next().expect("raw file"));
    let radius: f64 = args.next().expect("radius").parse().unwrap();
    let points: Vec<[f64; 2]> = args
        .map(|a| {
            let (x, y) = a.split_once(',').expect("x,y");
            [x.parse().unwrap(), y.parse().unwrap()]
        })
        .collect();
    std::fs::create_dir_all(&out).unwrap();
    let t = Instant::now();
    let net = Inpainter::load(&model.join(FILE), &shoebox_ml::device()).unwrap();
    println!("load {:?}", t.elapsed());
    remove::register(
        MODEL_ID,
        SIZE,
        Box::new(move |rgb, known| net.inpaint(rgb, known).map_err(|e| e.to_string())),
    );
    let strokes = vec![Stroke {
        points,
        radius,
        feather: 0.0,
        flow: 1.0,
        erase: false,
    }];
    let spot = Spot {
        kind: SpotKind::Remove,
        dest: remove::middle(&strokes).unwrap(),
        radius,
        strokes,
        ..Default::default()
    };
    // High efficiency NEFs through Adobe DNG Converter (cached in
    // `<out dir>/cache`).
    let bytes = std::fs::read(&file).unwrap();
    let source = if shoebox_develop::dng::needs_dng(&bytes) {
        let cache = shoebox_develop::dng::DngCache::new(&out.join("cache"), 1 << 34);
        let conv = shoebox_develop::dng::Converter::find(None).expect("Adobe DNG Converter");
        cache.get_or_convert(&conv, &file, &bytes).unwrap()
    } else {
        file.clone()
    };
    let raw = shoebox_decode::raw::decode_path(&source).unwrap();
    let name = file.file_stem().unwrap().to_string_lossy().into_owned();
    for (label, long_edge) in [("proxy", Some(2560)), ("full", None)] {
        let t = Instant::now();
        let src = cpu::demosaic(&raw, &Default::default(), long_edge);
        let img = &src.rgb;
        println!(
            "{label}: {}×{} demosaic {:?}",
            img.width,
            img.height,
            t.elapsed()
        );
        let t = Instant::now();
        let job = remove::job(img, &spot).unwrap();
        println!("  job {:?} (crop {:?})", t.elapsed(), job.crop);
        for run in 0..2 {
            let t = Instant::now();
            let fill = remove::run(&job).unwrap();
            println!("  model ({run}) {:?}", t.elapsed());
            if run == 1 {
                let mut after = img.clone();
                let t = Instant::now();
                remove::apply(&mut after, &spot, &job, &fill);
                println!("  blend {:?}", t.elapsed());
                for (which, im) in [("before", img), ("after", &after)] {
                    let s = Settings::default();
                    let one = cpu::Demosaiced::new(im.clone(), src.as_shot, src.referred);
                    let o = Options {
                        long_edge: None,
                        orientation: 1,
                        target: Target::Srgb,
                    };
                    let frame = cpu::frame(&raw, &one, &s, &o);
                    let shown = cpu::apply_from(&one, &s, &frame, true);
                    write(
                        &crop(&shown, job.crop),
                        &out.join(format!("{name}-{label}-{which}.png")),
                    );
                }
            }
        }
    }
}

fn crop(img: &Rgb, [x, y, w, h]: [usize; 4]) -> Rgb {
    let mut out = Rgb::new(w, h);
    for r in 0..h {
        let from = ((y + r) * img.width + x) * 3;
        out.data[r * w * 3..(r + 1) * w * 3].copy_from_slice(&img.data[from..from + w * 3]);
    }
    out
}

fn write(img: &Rgb, path: &Path) {
    let f = std::fs::File::create(path).unwrap();
    let mut e = png::Encoder::new(
        std::io::BufWriter::new(f),
        img.width as u32,
        img.height as u32,
    );
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .unwrap()
        .write_image_data(&cpu::to_u8(img))
        .unwrap();
}
