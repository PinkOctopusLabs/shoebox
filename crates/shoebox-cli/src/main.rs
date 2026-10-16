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
    /// Find duplicates and write a report for review. Never moves files.
    Dupes(DupesArgs),
    /// Move the files a person confirmed in a `dupes` report (`confirm` = y).
    Apply {
        /// The reviewed report CSV.
        report: PathBuf,
        /// Show what would be moved without moving anything.
        #[arg(long)]
        dry_run: bool,
        /// Don't ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
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
    /// Folder that confirmed duplicates will be moved into (by `shoebox apply`),
    /// keeping their relative paths. Fills the report's `dest` column.
    #[arg(long, value_name = "DIR", requires = "report")]
    move_matched: Option<PathBuf>,
    /// Accepted for compatibility; `dupes` never moves files.
    #[arg(long, hide = true)]
    dry_run: bool,
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
        Cmd::Apply {
            report,
            dry_run,
            yes,
        } => apply_cmd(&data_dir, &report, dry_run, yes),
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

    // Report any interrupted batches; dupes itself never moves anything.
    open_fileops(data_dir, refs.clone())?;
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

    // Proposed destinations for every scan file in a group. Nothing moves
    // until a person confirms rows and runs `shoebox apply`.
    let mut wanted = Vec::new();
    if let Some(d) = &dest {
        let multi_scan = a.scans.len() > 1;
        for g in &groups {
            let ids = std::iter::once(g.keeper).chain(g.members.iter().map(|m| m.item));
            for i in ids.filter(|&i| items[i].role == Role::Scan) {
                let f = &analyzed[i].file;
                let mut to = d.clone();
                if multi_scan {
                    to.push(
                        roots[f.root]
                            .path
                            .file_name()
                            .context("scan root has no name")?,
                    );
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
    let (mut suggested, mut review) = (0, 0);
    for (gi, g) in groups.iter().enumerate() {
        let k = &analyzed[g.keeper];
        rows.push(report::Row {
            confirm: "",
            group: gi + 1,
            level: "keep".into(),
            distance_to_keeper: Some(0),
            rotation: None,
            role: role(items[g.keeper].role),
            suggestion: "keep",
            same_exposure: "",
            path: k.file.path.display().to_string(),
            root: roots[k.file.root].path.display().to_string(),
            width: k.analysis.width,
            height: k.analysis.height,
            size: k.file.size,
            blake3: hex(&k.analysis.blake3),
            matched_ref: None,
            matched_ref_level: None,
            matched_ref_distance: None,
            dest: dest_of
                .get(k.file.path.as_path())
                .map(|d| d.display().to_string()),
            note: k.analysis.error.clone(),
        });
        for m in &g.members {
            let f = &analyzed[m.item];
            let sug = m.suggestion(&items);
            let suggestion = sug.as_str();
            suggested += usize::from(sug == dupes::Suggestion::Duplicate);
            review += usize::from(sug == dupes::Suggestion::Review);
            // Compare exposures against the matched reference file, else the keeper.
            let other = m.matched_ref.map_or(g.keeper, |(r, _)| r);
            let (a_t, b_t) = (items[other].capture, items[m.item].capture);
            let gap = dupes::capture_gap(&items[other], &items[m.item]);
            let same_exposure = match (a_t, b_t, gap) {
                (Some(_), Some(_), None) => "yes",
                (Some(_), Some(_), Some(_)) => "no",
                _ => "unknown",
            };
            let mut notes = Vec::new();
            if let Some(g) = gap {
                notes.push(format!(
                    "different exposure: shot {} apart from {}",
                    human_gap(g),
                    analyzed[other].file.path.display()
                ));
            }
            if sug == dupes::Suggestion::Review && m.matched_ref.is_none() {
                notes.push(if g.has_reference {
                    "matches another scan file, not a reference file".to_string()
                } else {
                    "no reference file in group".to_string()
                });
            }
            notes.extend(f.analysis.error.clone());
            let note = (!notes.is_empty()).then(|| notes.join("; "));
            rows.push(report::Row {
                confirm: "",
                group: gi + 1,
                level: m
                    .to_keeper
                    .map_or("chained", |x| x.level.as_str())
                    .to_string(),
                distance_to_keeper: m.to_keeper.map(|x| x.distance),
                rotation: m.to_keeper.map(|x| x.rotation),
                role: role(items[m.item].role),
                suggestion,
                same_exposure,
                path: f.file.path.display().to_string(),
                root: roots[f.file.root].path.display().to_string(),
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
        "{} duplicate groups ({} with a reference file): {suggested} suggested duplicates, {review} to review",
        groups.len(),
        groups.iter().filter(|g| g.has_reference).count(),
    );
    println!("Nothing was moved.");
    if let Some(rp) = &a.report {
        println!("Report: {}", rp.display());
        if dest.is_some() {
            println!(
                "To move files: put y in the `confirm` column of the rows you agree with, save as CSV, then run\n  shoebox apply {}",
                rp.display()
            );
        }
    }
    Ok(())
}

fn apply_cmd(data_dir: &Path, report_path: &Path, dry_run: bool, yes: bool) -> Result<()> {
    let file = std::fs::File::open(report_path)
        .with_context(|| format!("opening {}", report_path.display()))?;
    let rows =
        report::read_csv(file).context("reading report (is it the CSV from `shoebox dupes`?)")?;
    let confirmed: Vec<&report::ReviewRow> = rows.iter().filter(|r| r.is_confirmed()).collect();
    if confirmed.is_empty() {
        println!("No rows are confirmed; put y in the `confirm` column to approve a move.");
        return Ok(());
    }

    // Reference roots named in the report are read-only.
    let mut protected: Vec<PathBuf> = rows
        .iter()
        .filter(|r| r.role == "ref")
        .filter_map(|r| Path::new(&r.root).canonicalize().ok())
        .collect();
    protected.sort();
    protected.dedup();

    // Never empty a group: at least one file must stay.
    let mut per_group: HashMap<usize, (usize, usize)> = HashMap::new();
    for r in &rows {
        let e = per_group.entry(r.group).or_default();
        e.0 += 1;
        e.1 += usize::from(r.is_confirmed());
    }
    if let Some((g, _)) = per_group.iter().find(|(_, (total, conf))| conf == total) {
        bail!("every file in group {g} is confirmed; leave at least one unconfirmed to keep");
    }

    let mut wanted = Vec::new();
    for r in &confirmed {
        if r.role != "scan" {
            bail!("{} is a reference file and can't be moved", r.path);
        }
        let dest = r
            .dest
            .as_deref()
            .filter(|d| !d.is_empty())
            .with_context(|| {
                format!(
                    "{} has no destination; re-run `shoebox dupes` with --move-matched",
                    r.path
                )
            })?;
        let src = PathBuf::from(&r.path);
        // The file being moved must be the file that was reviewed.
        let data = std::fs::read(&src)
            .with_context(|| format!("{} is no longer readable", src.display()))?;
        let hex: String = shoebox_core::hash::blake3_of(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if hex != r.blake3 {
            bail!(
                "{} has changed since the report was written; re-run `shoebox dupes`",
                src.display()
            );
        }
        wanted.push((src, PathBuf::from(dest)));
    }

    let fo = open_fileops(data_dir, protected)?;
    let plan = fileops::plan_moves(&wanted)?;
    print_moves(&plan.moves);
    println!(
        "\n{} confirmed files ({} moves including sidecars).",
        confirmed.len(),
        plan.moves.len()
    );
    if dry_run {
        println!("Dry run: nothing was moved.");
        return Ok(());
    }
    if !confirm(&format!("Move {} files?", plan.moves.len()), yes)? {
        println!("Cancelled; nothing was moved.");
        return Ok(());
    }
    let batch = fo.execute(&format!("apply {}", report_path.display()), &plan)?;
    println!(
        "Moved {} files as batch {batch}. Undo with: shoebox undo {batch}",
        plan.moves.len()
    );
    Ok(())
}

fn human_gap(secs: f64) -> String {
    match secs {
        s if s < 60.0 => format!("{s:.1} s"),
        s if s < 3600.0 => format!("{:.0} min", s / 60.0),
        s if s < 172_800.0 => format!("{:.1} h", s / 3600.0),
        s => format!("{:.0} days", s / 86_400.0),
    }
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
