//! Compare perceptual hashes from our decode path and from ImageIO on the
//! given files. `cargo run --release -p shoebox-core --example imageio_agreement -- <files>`
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("imageio_agreement needs macOS (ImageIO)");
}

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use shoebox_core::hash::{HASH_DECODE_EDGE, PHashes};
    use shoebox_decode::{Format, imageio};

    for p in std::env::args().skip(1) {
        let path = std::path::Path::new(&p);
        let Some(fmt) = Format::from_path(path) else {
            continue;
        };
        let data = std::fs::read(path)?;
        let ours = shoebox_decode::luma_thumbnail(fmt, &data, HASH_DECODE_EDGE)?;
        let theirs = imageio::thumbnail_luma(
            &data,
            HASH_DECODE_EDGE,
            false,
            Some(imageio::type_hint(fmt)),
        )?;
        let (a, b) = (PHashes::of(&ours), PHashes::of(&theirs));
        let mad: f64 = if (ours.width, ours.height) == (theirs.width, theirs.height) {
            ours.px
                .iter()
                .zip(&theirs.px)
                .map(|(x, y)| f64::from(x.abs_diff(*y)))
                .sum::<f64>()
                / ours.px.len() as f64
        } else {
            f64::NAN
        };
        let time = |f: &dyn Fn()| {
            let mut v: Vec<f64> = (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    f();
                    t.elapsed().as_secs_f64() * 1e3
                })
                .collect();
            v.sort_by(f64::total_cmp);
            v[2]
        };
        let t_ours = time(&|| {
            let _ = shoebox_decode::luma_thumbnail(fmt, &data, HASH_DECODE_EDGE);
        });
        let t_io = time(&|| {
            let _ = imageio::thumbnail_luma(
                &data,
                HASH_DECODE_EDGE,
                false,
                Some(imageio::type_hint(fmt)),
            );
        });
        println!("    time ours {t_ours:6.1} ms   imageio {t_io:6.1} ms");
        println!(
            "{:28} ours {}x{} imageio {}x{}  dhash {:2} phash {:2}  mean|Δ| {mad:.1}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            ours.width,
            ours.height,
            theirs.width,
            theirs.height,
            (a.dhash ^ b.dhash).count_ones(),
            (a.phash ^ b.phash).count_ones(),
        );
    }
    Ok(())
}
