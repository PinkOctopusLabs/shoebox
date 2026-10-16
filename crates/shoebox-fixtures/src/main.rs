//! `cargo run -p shoebox-fixtures -- <empty dir>` writes the synthetic library
//! to disk, for manual CLI runs.

use anyhow::{Result, bail};

fn main() -> Result<()> {
    let Some(dir) = std::env::args_os().nth(1).map(std::path::PathBuf::from) else {
        bail!("usage: shoebox-fixtures <empty directory>");
    };
    if dir.exists() && std::fs::read_dir(&dir)?.next().is_some() {
        bail!("{} is not empty; refusing to write into it", dir.display());
    }
    let lib = shoebox_fixtures::build_library(&dir)?;
    println!(
        "ref:  {}\nscan: {}",
        lib.reference.display(),
        lib.scan.display()
    );
    Ok(())
}
