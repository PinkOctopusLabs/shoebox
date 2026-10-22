//! Download the model into a folder, as the app does when semantic search is
//! turned on: `cargo run --release -p shoebox-ml --example fetch -- <dir> <staging dir> [faces|sam|rawnind|scene|inpaint]`.

use std::sync::atomic::AtomicBool;

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).context("folder")?);
    let staging = std::path::PathBuf::from(std::env::args().nth(2).context("staging folder")?);
    let spec = match std::env::args().nth(3).as_deref() {
        Some("faces") => &shoebox_ml::download::FACES,
        Some("sam") => &shoebox_ml::download::SAM,
        Some("rawnind") => &shoebox_ml::download::RAWNIND,
        Some("scene") => &shoebox_ml::download::SCENE,
        Some("inpaint") => &shoebox_ml::download::INPAINT,
        _ => &shoebox_ml::download::SIGLIP,
    };
    let t = std::time::Instant::now();
    shoebox_ml::download::fetch(
        spec,
        &dir,
        &staging,
        &|s| eprintln!("{s:?}"),
        &AtomicBool::new(false),
    )?;
    println!(
        "ready: {} ({:.1} s)",
        spec.is_ready(&dir),
        t.elapsed().as_secs_f32()
    );
    Ok(())
}
