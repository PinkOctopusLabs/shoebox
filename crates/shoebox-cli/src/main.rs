//! `shoebox`: headless commands over the core library.

use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use shoebox_core::cache::Cache;
use shoebox_core::dupes::{self, Config, report};
use shoebox_core::fileops::{self, FileOps, Journal, PlannedMove};
use shoebox_core::scan::{self, Role, Root};
use shoebox_core::{paths, pipeline};
use shoebox_decode::Format;

#[derive(Parser)]
#[command(name = "shoebox", version, about = "Shoebox photo library tools")]
struct Cli {
    /// Where the cache and journal live (default: ~/Library/Application Support/<bundle id>).
    #[arg(long, global = true, env = "SHOEBOX_DATA_DIR", value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// More logging (-v info, -vv debug).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Find duplicates of reference files among scanned files.
    Dupes(DupesArgs),
    /// List journaled batches.
    History,
    /// Reverse a batch (default: the most recent one not yet undone).
    Undo {
        batch: Option<i64>,
        /// Don't ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args)]
struct DupesArgs {
    /// Folder whose files are kept (read-only). Repeatable.
    #[arg(long = "ref", value_name = "DIR")]
    refs: Vec<PathBuf>,
    /// Folder to check for duplicates. Repeatable.
    #[arg(long = "scan", value_name = "DIR", required = true)]
    scans: Vec<PathBuf>,
    /// Maximum perceptual distance (Hamming bits, inclusive) for a "similar" match.
    #[arg(long, default_value_t = dupes::DEFAULT_SIMILAR, value_parser = clap::value_parser!(u32).range(0..=64))]
    threshold: u32,
    /// Write a CSV report of every group.
    #[arg(long, value_name = "FILE")]
    report: Option<PathBuf>,
    /// Move scan files that match a reference file into this folder, keeping
    /// their relative paths. Sidecars move with them.
    #[arg(long, value_name = "DIR")]
    move_matched: Option<PathBuf>,
    /// Show what would be moved without moving anything.
    #[arg(long)]
    dry_run: bool,
    /// Don't ask for confirmation before moving.
    #[arg(long)]
    yes: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .init();

    let data_dir = match cli.data_dir {
        Some(d) => d,
        None => paths::default_data_dir()?,
    };
    match cli.cmd {
        Cmd::Dupes(args) => dupes_cmd(&data_dir, args),
        Cmd::History => history_cmd(&data_dir),
        Cmd::Undo { batch, yes } => undo_cmd(&data_dir, batch, yes),
    }
}

fn open_fileops(data_dir: &Path, protected: Vec<PathBuf>) -> Result<FileOps> {
    let journal = Journal::open(&paths::journal_db(data_dir)).context("opening journal")?;
    let fo = FileOps::new(journal, protected);
    for b in fo.journal().incomplete()? {
        eprintln!(
            "warning: batch {} ({}) was interrupted; review with `shoebox history` and `shoebox undo {}`",
            b.id, b.label, b.id
        );
    }
    Ok(fo)
}

/// Absolute path with the existing part canonicalised (the rest may not exist yet).
fn resolve_new_dir(p: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(p)?;
    let mut existing = abs.as_path();
    let mut rest = Vec::new();
    while !existing.exists() {
        rest.push(existing.file_name().context("bad path")?.to_owned());
        existing = existing.parent().context("bad path")?;
    }
    let mut out = existing.canonicalize()?;
    out.extend(rest.iter().rev());
    Ok(out)
}

fn confirm(prompt: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        bail!("refusing to run without confirmation; pass --yes");
    }
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn print_moves(moves: &[PlannedMove]) {
    println!("{:<4} {:<60} DESTINATION", "#", "SOURCE");
    for (i, m) in moves.iter().enumerate() {
        println!("{:<4} {:<60} {}", i + 1, m.src.display(), m.dst.display());
    }
}

fn dupes_cmd(data_dir: &Path, a: DupesArgs) -> Result<()> {
    let mut roots = Vec::new();
    for p in &a.refs {
        roots.push(Root::new(p, Role::Reference)?);
    }
    for p in &a.scans {
        roots.push(Root::new(p, Role::Scan)?);
    }
    for (i, x) in roots.iter().enumerate() {
        for y in roots.iter().skip(i + 1) {
            if x.path.starts_with(&y.path) || y.path.starts_with(&x.path) {
                bail!(
                    "roots overlap: {} and {}",
                    x.path.display(),
                    y.path.display()
                );
            }
        }
    }
    let refs: Vec<PathBuf> = roots
        .iter()
        .filter(|r| r.role == Role::Reference)
        .map(|r| r.path.clone())
        .collect();

    let dest = a.move_matched.as_deref().map(resolve_new_dir).transpose()?;
    if let Some(d) = &dest
        && let Some(r) = refs.iter().find(|r| d.starts_with(r))
    {
        bail!(
            "--move-matched {} is inside reference root {}",
            d.display(),
            r.display()
        );
    }
    if let Some(rp) = &a.report {
        let rp = resolve_new_dir(rp)?;
        let is_csv = rp
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("csv"));
        if rp.exists() && !is_csv {
            bail!(
                "--report {} exists and is not a .csv; refusing to replace it",
                rp.display()
            );
        }
        if Format::from_path(&rp).is_some() {
            bail!("--report must not be an image file: {}", rp.display());
        }
        if let Some(r) = refs.iter().find(|r| rp.starts_with(r)) {
            bail!(
                "--report {} is inside reference root {}",
                rp.display(),
                r.display()
            );
        }
    }

    let fo = open_fileops(data_dir, refs.clone())?;
    let mut cache = Cache::open(&paths::cache_db(data_dir)).context("opening cache")?;

    eprintln!("Scanning {} folder(s)…", roots.len());
    let exclude: Vec<PathBuf> = dest.iter().cloned().collect();
    let files = scan::scan(&roots, &exclude)?;
    eprintln!("Analysing {} files…", files.len());
    let (analyzed, stats) = pipeline::analyze_all(&mut cache, &roots, files)?;
    let items = pipeline::items(&roots, &analyzed);
    let cfg = Config {
        threshold: a.threshold,
        near_identical: dupes::DEFAULT_NEAR_IDENTICAL.min(a.threshold),
    };
    let groups = dupes::find_groups(&items, &cfg);

    // Plan moves for scan files that directly match a reference file.
    let mut wanted = Vec::new();
    if let Some(d) = &dest {
        let multi_scan = a.scans.len() > 1;
        for g in groups.iter().filter(|g| g.has_reference) {
            for m in g.members.iter().filter(|m| m.movable(&items)) {
                let f = &analyzed[m.item].file;
                let root = &roots[f.root].path;
                let mut to = d.clone();
                if multi_scan {
                    to.push(root.file_name().context("scan root has no name")?);
                }
                to.push(&f.rel);
                wanted.push((f.path.clone(), to));
            }
        }
    }
    let plan = fileops::plan_moves(&wanted)?;
    let dest_of: HashMap<&Path, &Path> = plan
        .moves
        .iter()
        .map(|m| (m.src.as_path(), m.dst.as_path()))
        .collect();

    let hex = |d: &[u8; 32]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let role = |r: Role| match r {
        Role::Reference => "ref",
        Role::Scan => "scan",
    };
    let mut rows = Vec::new();
    let mut movable = 0;
    for (gi, g) in groups.iter().enumerate() {
        let k = &analyzed[g.keeper];
        rows.push(report::Row {
            group: gi + 1,
            level: "keep".into(),
            distance_to_keeper: Some(0),
            rotation: None,
            role: role(items[g.keeper].role),
            action: "keep",
            path: k.file.path.display().to_string(),
            width: k.analysis.width,
            height: k.analysis.height,
            size: k.file.size,
            blake3: hex(&k.analysis.blake3),
            matched_ref: None,
            matched_ref_level: None,
            matched_ref_distance: None,
            dest: None,
            note: k.analysis.error.clone(),
        });
        for m in &g.members {
            let f = &analyzed[m.item];
            let is_movable = m.movable(&items);
            movable += usize::from(is_movable);
            let note = if !g.has_reference && items[m.item].role == Role::Scan {
                Some("no reference file in group".to_string())
            } else if items[m.item].role == Role::Scan && !is_movable {
                Some("only matches through another file; not moved".to_string())
            } else {
                f.analysis.error.clone()
            };
            rows.push(report::Row {
                group: gi + 1,
                level: m
                    .to_keeper
                    .map_or("chained", |x| x.level.as_str())
                    .to_string(),
                distance_to_keeper: m.to_keeper.map(|x| x.distance),
                rotation: m.to_keeper.map(|x| x.rotation),
                role: role(items[m.item].role),
                action: if is_movable && dest.is_some() {
                    "move"
                } else {
                    "none"
                },
                path: f.file.path.display().to_string(),
                width: f.analysis.width,
                height: f.analysis.height,
                size: f.file.size,
                blake3: hex(&f.analysis.blake3),
                matched_ref: m
                    .matched_ref
                    .map(|(r, _)| analyzed[r].file.path.display().to_string()),
                matched_ref_level: m.matched_ref.map(|(_, x)| x.level.as_str()),
                matched_ref_distance: m.matched_ref.map(|(_, x)| x.distance),
                dest: dest_of
                    .get(f.file.path.as_path())
                    .map(|d| d.display().to_string()),
                note,
            });
        }
    }
    if let Some(rp) = &a.report {
        let f = std::fs::File::create(rp).with_context(|| format!("creating {}", rp.display()))?;
        report::write_csv(f, &rows)?;
    }

    println!(
        "{} files ({} cached, {} analysed, {} unreadable, {} undecodable)",
        stats.files, stats.cached, stats.analyzed, stats.unreadable, stats.undecodable
    );
    println!(
        "{} duplicate groups ({} with a reference file); {} scan files match a reference file",
        groups.len(),
        groups.iter().filter(|g| g.has_reference).count(),
        movable
    );
    if let Some(rp) = &a.report {
        println!("Report: {}", rp.display());
    }

    if dest.is_none() || plan.moves.is_empty() {
        return Ok(());
    }
    println!();
    print_moves(&plan.moves);
    if a.dry_run {
        println!("\nDry run: nothing was moved.");
        return Ok(());
    }
    if !confirm(&format!("Move {} files?", plan.moves.len()), a.yes)? {
        println!("Cancelled; nothing was moved.");
        return Ok(());
    }
    let label = format!(
        "dupes → {}",
        dest.as_deref().unwrap_or(Path::new("")).display()
    );
    let batch = fo.execute(&label, &plan)?;
    println!(
        "Moved {} files as batch {batch}. Undo with: shoebox undo {batch}",
        plan.moves.len()
    );
    Ok(())
}

/// `YYYY-MM-DD HH:MM:SS` UTC from Unix milliseconds.
fn utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn history_cmd(data_dir: &Path) -> Result<()> {
    let fo = open_fileops(data_dir, Vec::new())?;
    let batches = fo.journal().batches()?;
    if batches.is_empty() {
        println!("No batches yet.");
        return Ok(());
    }
    println!(
        "{:<6} {:<20} {:<6} {:<8} {:>6}  LABEL",
        "BATCH", "WHEN (UTC)", "KIND", "STATUS", "FILES"
    );
    for b in batches {
        let label = match b.undo_of {
            Some(u) => format!("{} (of {u})", b.label),
            None => b.label,
        };
        println!(
            "{:<6} {:<20} {:<6} {:<8} {:>6}  {label}",
            b.id,
            utc(b.ts),
            b.kind,
            b.status,
            b.ops
        );
    }
    Ok(())
}

fn undo_cmd(data_dir: &Path, batch: Option<i64>, yes: bool) -> Result<()> {
    let fo = open_fileops(data_dir, Vec::new())?;
    let batch = match batch {
        Some(b) => b,
        None => match fo.last_undoable()? {
            Some(b) => b.id,
            None => {
                println!("Nothing to undo.");
                return Ok(());
            }
        },
    };
    let plan = fo.undo_plan(batch)?;
    if plan.is_empty() {
        println!("Batch {batch} has nothing left to undo.");
        return Ok(());
    }
    let moves: Vec<PlannedMove> = plan.into_iter().map(|(_, m)| m).collect();
    print_moves(&moves);
    if !confirm(&format!("Undo batch {batch} ({} files)?", moves.len()), yes)? {
        println!("Cancelled.");
        return Ok(());
    }
    let undo = fo.undo(batch)?;
    println!("Undid batch {batch} as batch {undo}.");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_formats() {
        assert_eq!(super::utc(0), "1970-01-01 00:00:00");
        assert_eq!(super::utc(1_776_960_000_000), "2026-04-23 16:00:00");
    }
}
