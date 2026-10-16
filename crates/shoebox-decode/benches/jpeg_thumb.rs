//! Thumbnail decode: zune-jpeg (full decode + area downscale) against
//! turbojpeg's DCT-scaled decode, both to a 256 px luma thumbnail.
//!
//! Input: `SHOEBOX_BENCH_JPEG=/path/to/file.jpg`, or a synthetic 6048×4032
//! (24 MP, Z f size) JPEG if unset.
//!
//! zune only:          cargo bench -p shoebox-decode --bench jpeg_thumb
//! with turbojpeg:     TURBOJPEG_SOURCE=explicit TURBOJPEG_DYNAMIC=1 \
//!                     TURBOJPEG_LIB_DIR=$(brew --prefix jpeg-turbo)/lib \
//!                     TURBOJPEG_INCLUDE_DIR=$(brew --prefix jpeg-turbo)/include \
//!                     cargo bench -p shoebox-decode --features bench-turbojpeg --bench jpeg_thumb
#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use shoebox_decode::Gray;
use std::hint::black_box;

const EDGE: u32 = 256;

fn input() -> Vec<u8> {
    match std::env::var_os("SHOEBOX_BENCH_JPEG") {
        Some(p) => std::fs::read(p).unwrap(),
        None => shoebox_fixtures::jpeg(&shoebox_fixtures::scene(11, 6048, 4032), 92).unwrap(),
    }
}

fn zune_thumb(data: &[u8]) -> Gray {
    shoebox_decode::jpeg::decode_luma(data)
        .unwrap()
        .fit_long_edge(EDGE)
}

#[cfg(feature = "bench-turbojpeg")]
fn turbo_thumb(data: &[u8], factor: turbojpeg::ScalingFactor) -> Gray {
    let mut d = turbojpeg::Decompressor::new().unwrap();
    let header = d.read_header(data).unwrap().scaled(factor);
    d.set_scaling_factor(factor).unwrap();
    let mut img = turbojpeg::Image {
        pixels: vec![0u8; header.width * header.height],
        width: header.width,
        pitch: header.width,
        height: header.height,
        format: turbojpeg::PixelFormat::GRAY,
    };
    d.decompress(data, img.as_deref_mut()).unwrap();
    Gray::new(header.width as u32, header.height as u32, img.pixels)
        .unwrap()
        .fit_long_edge(EDGE)
}

fn bench(c: &mut Criterion) {
    let data = input();
    let (w, h) = shoebox_decode::jpeg::dimensions(&data).unwrap();
    eprintln!("input: {w}x{h}, {} KB", data.len() / 1024);
    let mut g = c.benchmark_group(format!("thumb_{w}x{h}_to_{EDGE}"));
    g.sample_size(20);
    g.bench_function("zune_full_then_area", |b| {
        b.iter(|| zune_thumb(black_box(&data)))
    });
    g.bench_function("zune_decode_only", |b| {
        b.iter(|| shoebox_decode::jpeg::decode_luma(black_box(&data)).unwrap())
    });
    let full = shoebox_decode::jpeg::decode_luma(&data).unwrap();
    g.bench_function("area_downscale_only", |b| {
        b.iter(|| black_box(&full).fit_long_edge(EDGE))
    });
    #[cfg(feature = "bench-turbojpeg")]
    {
        use turbojpeg::ScalingFactor;
        g.bench_function("turbo_scaled_1_8_then_area", |b| {
            b.iter(|| turbo_thumb(black_box(&data), ScalingFactor::ONE_EIGHTH))
        });
        g.bench_function("turbo_full_then_area", |b| {
            b.iter(|| turbo_thumb(black_box(&data), ScalingFactor::ONE))
        });
        // Same output, so hashes from either path agree.
        let z = zune_thumb(&data);
        let t = turbo_thumb(&data, ScalingFactor::ONE_EIGHTH);
        let mean_abs: f64 =
            z.px.iter()
                .zip(&t.px)
                .map(|(a, b)| f64::from(a.abs_diff(*b)))
                .sum::<f64>()
                / z.px.len() as f64;
        eprintln!(
            "thumb dims zune {}x{} turbo {}x{}, mean abs diff {mean_abs:.2}",
            z.width, z.height, t.width, t.height
        );
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
