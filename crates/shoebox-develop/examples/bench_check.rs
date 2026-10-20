//! Fail on a benchmark regression: reads criterion's comparison with a
//! baseline (`cargo bench … -- --baseline <name>`) from `target/criterion`
//! and exits 1 if any benchmark's mean is more than 20% slower.
//!
//!   cargo run -p shoebox-develop --example bench_check [-- <criterion group dir>]
//!
//! By default it reads the `develop` group (the pipeline benchmarks).

use std::path::{Path, PathBuf};

const LIMIT: f64 = 0.20;

fn main() -> std::process::ExitCode {
    let dir = std::env::args().nth(1).map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/criterion/develop"),
        PathBuf::from,
    );
    let mut found = 0;
    let mut slower = Vec::new();
    for entry in walk(&dir) {
        if !entry.ends_with("change/estimates.json") {
            continue;
        }
        let Some(change) = std::fs::read_to_string(&entry)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["mean"]["point_estimate"].as_f64())
        else {
            continue;
        };
        found += 1;
        // <group>/<bench>/change/estimates.json
        let name = entry
            .strip_prefix(&dir)
            .ok()
            .and_then(|p| p.parent()?.parent())
            .map_or_else(|| entry.display().to_string(), |p| p.display().to_string());
        println!("{:+6.1}%  {name}", change * 100.0);
        if change > LIMIT {
            slower.push(name);
        }
    }
    if found == 0 {
        eprintln!(
            "no comparisons in {}: run the benches with --baseline first",
            dir.display()
        );
        return std::process::ExitCode::FAILURE;
    }
    if slower.is_empty() {
        println!("no regression over {:.0}%", LIMIT * 100.0);
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!(
            "slower by more than {:.0}%: {}",
            LIMIT * 100.0,
            slower.join(", ")
        );
        std::process::ExitCode::FAILURE
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}
