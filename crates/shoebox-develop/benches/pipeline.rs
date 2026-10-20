//! The edit pipeline's costs on a 24 MP (Z f size) synthetic raw: decode,
//! demosaic (proxy and full), the full-pipe proxy render a slider drag
//! pays, and an export's resize and encode.
//!
//!   cargo bench -p shoebox-develop --bench pipeline
//!
//! Regressions: save a baseline on main, compare a branch with it, and
//! check (fails on any benchmark more than 20% slower):
//!
//!   cargo bench -p shoebox-develop --bench pipeline -- --save-baseline main
//!   cargo bench -p shoebox-develop --bench pipeline -- --baseline main
//!   cargo run -p shoebox-develop --example bench_check
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use shoebox_develop::cpu::{self, Options};
use shoebox_develop::edit::{Raw, Settings};
use shoebox_fixtures::Dng;

fn raw_file() -> Vec<u8> {
    let d = Dng::new(6048, 4032);
    let (black, span) = (f32::from(d.black), f32::from(d.white - d.black));
    d.write(|x, y| {
        let v = 0.05 + 0.6 * ((x as f32 * 0.003).sin() * (y as f32 * 0.002).cos()).abs();
        (black + v * span) as u16
    })
}

fn pipeline(c: &mut Criterion) {
    let bytes = raw_file();
    let raw = shoebox_decode::raw::decode(&bytes).unwrap();
    let mut g = c.benchmark_group("develop");
    g.sample_size(10);
    g.bench_function("decode 24 MP", |b| {
        b.iter(|| shoebox_decode::raw::decode(black_box(&bytes)).unwrap())
    });
    g.bench_function("demosaic RCD to proxy", |b| {
        b.iter(|| cpu::demosaic(black_box(&raw), &Raw::default(), Some(2560)))
    });
    g.bench_function("demosaic RCD full", |b| {
        b.iter(|| cpu::demosaic(black_box(&raw), &Raw::default(), None))
    });
    let proxy = cpu::demosaic(&raw, &Raw::default(), Some(2560));
    let mut s = Settings::default();
    s.modules.exposure.ev = 0.7;
    s.modules.display_transform.contrast = 20.0;
    s.geometry.rotate = 1.5;
    let o = Options::default();
    g.bench_function("proxy render (frame + apply)", |b| {
        b.iter(|| {
            let f = cpu::frame(&raw, &proxy, black_box(&s), &o);
            cpu::apply(&proxy.rgb, &f, true)
        })
    });
    let mut local = s.clone();
    local.modules.highlights_shadows.shadows = 50.0;
    local.modules.clarity.amount = 30.0;
    local.modules.dehaze.amount = 20.0;
    let mut ev = 0.0;
    g.bench_function("local maps (frame, cache miss)", |b| {
        b.iter(|| {
            // A new exposure each time: the maps are worked out again.
            ev += 0.001;
            local.modules.exposure.ev = ev;
            cpu::frame(&raw, &proxy, black_box(&local), &o)
        })
    });
    let full = cpu::render(&raw, &s, &o);
    g.bench_function("export resize to 2048 (Lanczos)", |b| {
        b.iter(|| shoebox_develop::export::resize(black_box(&full), 2048, 1365))
    });
    let preset = shoebox_develop::export::starters()[0].clone();
    g.bench_function("export JPEG encode 24 MP", |b| {
        b.iter(|| shoebox_develop::export::encode(black_box(&full), &preset).unwrap())
    });
    g.finish();
}

criterion_group!(benches, pipeline);
criterion_main!(benches);
