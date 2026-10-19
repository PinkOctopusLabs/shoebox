//! What Develop's raw decode makes of some files: compression, size, CFA,
//! white balance, matrices and time.
//! `cargo run --release -p shoebox-decode --example raw_probe -- <file>...`
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

fn main() {
    for p in std::env::args().skip(1) {
        let path = Path::new(&p);
        let data = std::fs::read(path).expect("read");
        let compression = shoebox_decode::nef::compression(&data).ok().flatten();
        let t = std::time::Instant::now();
        match shoebox_decode::raw::decode(&data) {
            Ok(r) => {
                let max = r.data.iter().copied().fold(0.0f32, f32::max);
                let clipped = r
                    .data
                    .iter()
                    .filter(|&&v| v >= shoebox_decode::raw::CLIP)
                    .count();
                println!(
                    "{p}: {} {} {}×{} cfa {:?} as shot {:?} matrices {:?} compression {:?}; \
                     max {max:.3}, {clipped} clipped; {:?}",
                    r.make,
                    r.model,
                    r.width,
                    r.height,
                    r.cfa,
                    r.as_shot,
                    r.matrices.iter().map(|m| m.temperature).collect::<Vec<_>>(),
                    compression.map(|c| c.label()),
                    t.elapsed()
                );
            }
            Err(e) => println!(
                "{p}: {e} (compression {:?})",
                compression.map(|c| c.label())
            ),
        }
    }
}
