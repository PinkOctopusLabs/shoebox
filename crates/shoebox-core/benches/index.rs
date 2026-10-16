//! Hashing and duplicate-index benchmarks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use shoebox_core::dupes::{self, Config, Item};
use shoebox_core::hash::{self, PHashes};
use shoebox_core::scan::Role;
use shoebox_decode::Format;

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// `n` files where every tenth is a near-copy (a few bits off) of an earlier one.
fn items(n: usize) -> Vec<Item> {
    let mut s = 0x1234_5678_9abc_def1u64;
    let mut out: Vec<Item> = Vec::with_capacity(n);
    for i in 0..n {
        let (p, d) = if i % 10 == 9 {
            let src = out[i - 5].hashes[0];
            let flip = 1u64 << (xorshift(&mut s) % 64);
            (src.phash ^ flip, src.dhash ^ flip)
        } else {
            (xorshift(&mut s), xorshift(&mut s))
        };
        let h = PHashes { dhash: d, phash: p };
        let mut b3 = [0u8; 32];
        b3[..8].copy_from_slice(&(i as u64).to_le_bytes());
        out.push(Item {
            role: if i % 2 == 0 {
                Role::Reference
            } else {
                Role::Scan
            },
            format: Format::Jpeg,
            blake3: b3,
            image_data_hash: None,
            hashes: vec![h; 4],
            pixels: 24_000_000,
        });
    }
    out
}

fn bench(c: &mut Criterion) {
    let data = vec![0x5Au8; 25 * 1024 * 1024];
    c.bench_function("blake3_25MB_nef", |b| {
        b.iter(|| hash::blake3_of(black_box(&data)))
    });

    let mut g = c.benchmark_group("find_groups_t10");
    g.sample_size(10);
    for n in [1_000usize, 10_000] {
        let it = items(n);
        g.bench_with_input(BenchmarkId::from_parameter(n), &it, |b, it| {
            b.iter(|| dupes::find_groups(black_box(it), &Config::default()))
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
