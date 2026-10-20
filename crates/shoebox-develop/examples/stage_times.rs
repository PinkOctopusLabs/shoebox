//! Time the raw stages on real files (DNG or Lossless NEF):
//! `cargo run --release -p shoebox-develop --example stage_times -- <file>...`
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Instant;

use shoebox_develop::cpu::{self, raw};
use shoebox_develop::edit::Settings;

fn main() {
    for p in std::env::args().skip(1) {
        let t = Instant::now();
        let r = shoebox_decode::raw::decode_path(std::path::Path::new(&p)).unwrap();
        let decode = t.elapsed();
        let mult = shoebox_develop::wb::resolve(&r, &Default::default()).multipliers;
        let bal: Vec<f32> = r
            .data
            .iter()
            .enumerate()
            .map(|(i, v)| v * mult[usize::from(r.cfa[(i / r.width) % 2][(i % r.width) % 2])])
            .collect();
        let m = raw::Mosaic {
            width: r.width,
            height: r.height,
            cfa: r.cfa,
            data: &bal,
        };
        let t = Instant::now();
        let healed = raw::inpaint_opposed(&m, mult.map(|v| v * raw::OPPOSED_CLIP));
        let hl = t.elapsed();
        let m = raw::Mosaic { data: &healed, ..m };
        let t = Instant::now();
        let rgb = raw::rcd(&m);
        let rcd = t.elapsed();
        let t = Instant::now();
        let _ = cpu::downscale(&rgb, 2560);
        let down = t.elapsed();
        let t = Instant::now();
        let _ = cpu::demosaic(&r, &Settings::default().modules.raw, Some(2560));
        let all = t.elapsed();
        println!(
            "{p}: decode {decode:?}, highlights {hl:?}, rcd {rcd:?}, downscale {down:?}; demosaic() total {all:?}"
        );
    }
}
