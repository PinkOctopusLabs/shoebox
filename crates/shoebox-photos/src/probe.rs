//! The Phase 0 probe (PHOTOS-SPEC, "Plan"): what PhotoKit makes of a
//! test library, and the facts the sync relies on. Run from a bundled
//! build in a test macOS account: `Shoebox.app/Contents/MacOS/Shoebox
//! --photos-probe <empty folder>` (MANUAL-CHECKS, "iCloud Photos").
//!
//! It refuses any library without an album named [`TEST_ALBUM`] (a guard
//! against mistakes, not a security boundary), reads only, and writes only
//! into the folder given.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use shoebox_core::photos::{Access, PhotoSource, ResourceKind};
use shoebox_core::{CoreError, Result};

use crate::PhotoKitSource;

/// The album a test library must have.
pub const TEST_ALBUM: &str = "Shoebox test library";

/// Originals downloaded twice to compare.
const SAMPLE: usize = 5;

fn hash(p: &Path) -> Result<String> {
    let bytes = std::fs::read(p).map_err(shoebox_core::error::io(p))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Run the probe, writing downloads and `report.txt` into `out` (which
/// must be empty or new). `keep`: leave the probe's sync in place (to
/// look at `out/library`) rather than undoing it. The report, as written.
pub fn run(out: &Path, keep: bool) -> Result<String> {
    let src = PhotoKitSource::new();
    if !PhotoKitSource::can_ask() {
        return Err(CoreError::Photos(
            "run the probe from a bundled build (Shoebox.app/Contents/MacOS/Shoebox)".into(),
        ));
    }
    match src.request_access() {
        Access::Full => {}
        a => {
            return Err(CoreError::Photos(format!(
                "Photos access is {a:?}; the probe needs full access"
            )));
        }
    }
    if !src.album_titles().iter().any(|t| t == TEST_ALBUM) {
        return Err(CoreError::Refused(format!(
            "this Photos library has no album named “{TEST_ALBUM}”, so it isn't a test library; nothing was read beyond the album list"
        )));
    }
    if std::fs::read_dir(out).is_ok_and(|mut d| d.next().is_some()) {
        return Err(CoreError::Refused(format!("{} isn't empty", out.display())));
    }
    std::fs::create_dir_all(out).map_err(shoebox_core::error::io(out))?;
    let t = Instant::now();
    let lib = src.fetch()?;
    let fetched = t.elapsed();

    let mut r = String::new();
    let n = lib.assets.len();
    let _ = writeln!(
        r,
        "Shoebox Photos probe\n\nListed {n} assets and {} albums in {:.1} s.",
        lib.albums.len(),
        fetched.as_secs_f64()
    );
    let with_cloud = lib.assets.iter().filter(|a| a.id.cloud.is_some()).count();
    let videos = lib
        .assets
        .iter()
        .filter(|a| a.media == shoebox_core::photos::Media::Video)
        .count();
    let _ = writeln!(
        r,
        "Videos: {videos}. Hidden: {}. Favourites: {}. Burst frames: {}.",
        lib.assets.iter().filter(|a| a.hidden).count(),
        lib.assets.iter().filter(|a| a.favourite).count(),
        lib.assets.iter().filter(|a| a.burst.is_some()).count()
    );
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut formats: BTreeMap<String, usize> = BTreeMap::new();
    for a in &lib.assets {
        for res in &a.resources {
            *kinds.entry(format!("{:?}", res.kind)).or_default() += 1;
        }
        if let Some(e) = &a.edited {
            *formats.entry(format!("{e:?}")).or_default() += 1;
        }
    }
    let _ = writeln!(r, "Files by kind: {kinds:?}");
    let _ = writeln!(r, "Edits by who made them: {formats:?}");
    for al in &lib.albums {
        let _ = writeln!(r, "Album: {}", al.path().join(" › "));
    }

    let _ = writeln!(
        r,
        "\nFact 2, cloud ids: {with_cloud} of {n} assets have a PHCloudIdentifier{}.",
        if with_cloud == n {
            ""
        } else {
            " (the others are keyed by their local id)"
        }
    );
    let sized = lib
        .assets
        .iter()
        .flat_map(|a| &a.resources)
        .filter(|x| x.size.is_some())
        .count();
    let _ = writeln!(
        r,
        "Fact 4, file sizes: {sized} files have a size (PhotoKit has no public size; the plan shows unknown sizes, and free space is checked before each step)."
    );

    // Fact 1: the same original, downloaded twice, is the same bytes.
    let stop = AtomicBool::new(false);
    let mut same = 0;
    let mut tried = 0;
    let mut bytes = 0u64;
    let t = Instant::now();
    for (i, a) in lib
        .assets
        .iter()
        .filter(|a| a.resource(ResourceKind::Original).is_some())
        .take(SAMPLE)
        .enumerate()
    {
        let name = a
            .resource(ResourceKind::Original)
            .map(|x| x.file_name.clone())
            .unwrap_or_default();
        let (p1, p2) = (
            out.join(format!("{i}-a-{name}")),
            out.join(format!("{i}-b-{name}")),
        );
        tried += 1;
        let got = src
            .download(&a.id, ResourceKind::Original, &p1, &|_| {}, &stop)
            .and_then(|()| src.download(&a.id, ResourceKind::Original, &p2, &|_| {}, &stop))
            .and_then(|()| Ok((hash(&p1)?, hash(&p2)?)));
        match got {
            Ok((h1, h2)) => {
                bytes += std::fs::metadata(&p1).map(|m| m.len() * 2).unwrap_or(0);
                if h1 == h2 {
                    same += 1;
                }
                let _ = writeln!(
                    r,
                    "  {name}: {}",
                    if h1 == h2 {
                        "same bytes"
                    } else {
                        "DIFFERENT bytes"
                    }
                );
            }
            Err(e) => {
                let _ = writeln!(r, "  {name}: failed: {e}");
            }
        }
    }
    let secs = t.elapsed().as_secs_f64().max(1e-3);
    let _ = writeln!(
        r,
        "Fact 1, originals byte-identical: {same} of {tried} ({:.1} MB/s).",
        bytes as f64 / 1e6 / secs
    );
    let _ = writeln!(
        r,
        "Fact 3, adjustment data round trip: needs a push (Phase 5); not checked yet."
    );
    if let Err(e) = sync_check(&src, &lib, out, keep, &mut r) {
        let _ = writeln!(r, "Sync: failed: {e}");
    }
    let path = out.join("report.txt");
    std::fs::write(&path, &r).map_err(shoebox_core::error::io(&path))?;
    Ok(r)
}

/// Phase 3's check without the app: sync the test library into a new root
/// inside `out`, plan again (nothing new), then undo the sync.
fn sync_check(
    src: &PhotoKitSource,
    lib: &shoebox_core::photos::Library,
    out: &Path,
    keep: bool,
    r: &mut String,
) -> Result<()> {
    use shoebox_core::fileops::{FileOps, Journal, TrashMode};
    use shoebox_core::photos::ledger::Ledgers;
    use shoebox_core::photos::place::{Context, LibraryIndex};
    use shoebox_core::photos::plan::{self, Options};
    use shoebox_core::photos::sync::{self, Job};

    let root = out.join("library");
    std::fs::create_dir_all(&root).map_err(shoebox_core::error::io(&root))?;
    let roots = vec![root.clone()];
    let fo = FileOps::new(Journal::open(&out.join("journal.sqlite"))?, vec![])
        .with_trash(TrashMode::Folder(out.join("trash")));
    let catalog = shoebox_core::catalog::Catalog::default();
    let options = Options::default();
    let template = shoebox_core::template::Template::parse(shoebox_core::template::DEFAULT_IMPORT)?;
    let mut ledgers = Ledgers::load(&roots);
    let p = plan::plan(lib, &ledgers, &Default::default(), &options);
    let albums = p
        .albums
        .iter()
        .map(|a| (a.id.clone(), a.choice.clone()))
        .collect();
    let mut cx = Context {
        root: &root,
        template: &template,
        options: &options,
        albums: &albums,
        tz: jiff::tz::TimeZone::system(),
        index: LibraryIndex::new(&catalog),
        events: Vec::new(),
        taken: Default::default(),
        seq: 0,
    };
    let job = Job {
        source: src,
        library: lib,
        catalog: &catalog,
        batches: &fo,
        roots: &roots,
        writable: &roots,
        staging: &out.join("staging"),
        run: "probe",
        label: "probe sync",
        step: 20,
        parallel: 4,
        min_free: 10 * 1024 * 1024 * 1024,
    };
    let t = Instant::now();
    let report = sync::run(
        &job,
        &p,
        &mut ledgers,
        &mut cx,
        &|_| {},
        &AtomicBool::new(false),
    )?;
    let _ = writeln!(
        r,
        "\nSync into {}: {} planned ({} hidden, {} burst frames left out); {} copied ({} files), {} already there, {} dated by Photos, {} failed, in {:.1} s.",
        root.display(),
        p.new.len(),
        p.hidden,
        p.burst_frames,
        report.copied,
        report.files,
        report.in_library,
        report.dated_by_photos,
        report.failed.len(),
        t.elapsed().as_secs_f64()
    );
    for (id, why) in &report.failed {
        let _ = writeln!(r, "  failed {}: {why}", id.local);
    }
    let again = plan::plan(lib, &Ledgers::load(&roots), &Default::default(), &options);
    let _ = writeln!(
        r,
        "Sync again: {} new, {} synced before (passes when 0 new).",
        again.new.len(),
        again.synced
    );
    if let Some(b) = report.batch.filter(|_| !keep) {
        fo.undo(b)?;
        let after = Ledgers::load(&roots);
        let _ = writeln!(r, "Undone: {} records left (passes when 0).", after.len());
    }
    Ok(())
}
