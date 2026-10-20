//! The DNG cache, with a shell script standing in for Adobe's converter.
//! An ignored test runs the real one on the real High efficiency★ fixture.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use shoebox_develop::dng::{Converter, DngCache, needs_dng};
use shoebox_fixtures::{Dng, nikon_compression_stub};

/// A stand-in converter: `-c -p0 -d <dir> <nef>` writes `<dir>/<stem>.dng`
/// (a copy of `dng`) and logs each run. With `fail`, it exits 1.
fn fake(dir: &Path, dng: &Path, fail: bool) -> (Converter, PathBuf) {
    let log = dir.join("runs.log");
    let script = dir.join("fake-converter.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             echo \"$@\" >> '{log}'\n\
             {fail}\
             out=\"$4\"; nef=\"$5\"; stem=$(basename \"$nef\"); stem=${{stem%.*}}\n\
             sleep 0.2\n\
             cp '{dng}' \"$out/$stem.dng\"\n",
            log = log.display(),
            dng = dng.display(),
            fail = if fail { "exit 1\n" } else { "" },
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (Converter::at(script), log)
}

fn runs(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn setup() -> (tempfile::TempDir, PathBuf, Vec<u8>, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let dng = t.path().join("made.dng");
    std::fs::write(&dng, Dng::new(32, 24).write(|x, y| 2000 + (x * y) as u16)).unwrap();
    let nef = t.path().join("library/DSC_0001.NEF");
    std::fs::create_dir_all(nef.parent().unwrap()).unwrap();
    let bytes = nikon_compression_stub(14, true);
    std::fs::write(&nef, &bytes).unwrap();
    (t, dng, bytes, nef)
}

#[test]
fn which_files_need_a_dng() {
    assert!(needs_dng(&nikon_compression_stub(14, true)));
    assert!(needs_dng(&nikon_compression_stub(13, true)));
    assert!(!needs_dng(&nikon_compression_stub(3, true)));
    assert!(!needs_dng(b"not a tiff"));
}

#[test]
fn converts_once_then_serves_the_cache() {
    let (t, dng, bytes, nef) = setup();
    let (conv, log) = fake(t.path(), &dng, false);
    let cache = DngCache::new(&t.path().join("cache"), 1 << 30);
    let a = cache.get_or_convert(&conv, &nef, &bytes).unwrap();
    assert_eq!(
        a,
        cache.dir().join(format!("{}.dng", DngCache::key(&bytes)))
    );
    assert!(shoebox_decode::raw::decode_path(&a).is_ok());
    let b = cache.get_or_convert(&conv, &nef, &bytes).unwrap();
    assert_eq!(a, b);
    let r = runs(&log);
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(r[0].starts_with("-c -p0 -d "), "{r:?}");
    // Nothing is written beside the NEF; no temporary folders are left.
    let lib: Vec<_> = std::fs::read_dir(nef.parent().unwrap()).unwrap().collect();
    assert_eq!(lib.len(), 1);
    let cached: Vec<_> = std::fs::read_dir(cache.dir()).unwrap().collect();
    assert_eq!(cached.len(), 1);
}

#[test]
fn the_same_file_from_two_threads_converts_once() {
    let (t, dng, bytes, nef) = setup();
    let (conv, log) = fake(t.path(), &dng, false);
    let cache = Arc::new(DngCache::new(&t.path().join("cache"), 1 << 30));
    let handles: Vec<_> = (0..3)
        .map(|_| {
            let (cache, conv, nef, bytes) =
                (cache.clone(), conv.clone(), nef.clone(), bytes.clone());
            std::thread::spawn(move || cache.get_or_convert(&conv, &nef, &bytes).unwrap())
        })
        .collect();
    let paths: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(paths.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(runs(&log).len(), 1);
}

#[test]
fn failures_are_errors_and_leave_nothing() {
    let (t, dng, bytes, nef) = setup();
    let (conv, _) = fake(t.path(), &dng, true);
    let cache = DngCache::new(&t.path().join("cache"), 1 << 30);
    assert!(cache.get_or_convert(&conv, &nef, &bytes).is_err());
    assert_eq!(std::fs::read_dir(cache.dir()).unwrap().count(), 0);

    // A "DNG" rawler can't read isn't kept either.
    let junk = t.path().join("junk.dng");
    std::fs::write(&junk, b"not a dng").unwrap();
    let (conv, _) = fake(t.path(), &junk, false);
    assert!(cache.get_or_convert(&conv, &nef, &bytes).is_err());
    assert_eq!(std::fs::read_dir(cache.dir()).unwrap().count(), 0);

    assert!(Converter::find(Some(&t.path().join("Missing.app"))).is_none());
}

#[test]
fn least_recently_used_go_first() {
    let (t, dng, _, _) = setup();
    let (conv, _) = fake(t.path(), &dng, false);
    let one = std::fs::metadata(&dng).unwrap().len();
    // Room for two.
    let cache = DngCache::new(&t.path().join("cache"), 2 * one + one / 2);
    let mut keys = Vec::new();
    for i in 0..3u16 {
        let bytes = nikon_compression_stub(14, true)
            .into_iter()
            .chain(i.to_le_bytes())
            .collect::<Vec<u8>>();
        let nef = t.path().join(format!("library/DSC_000{i}.NEF"));
        std::fs::write(&nef, &bytes).unwrap();
        cache.get_or_convert(&conv, &nef, &bytes).unwrap();
        keys.push(DngCache::key(&bytes));
        // Using the first keeps it; the second becomes the oldest.
        std::thread::sleep(std::time::Duration::from_millis(20));
        cache.get(&keys[0]);
    }
    assert!(cache.get(&keys[0]).is_some());
    assert!(cache.get(&keys[1]).is_none());
    assert!(cache.get(&keys[2]).is_some());
    assert!(cache.size().unwrap() <= 2 * one + one / 2);
    assert_eq!(cache.clear().unwrap(), 2);
    assert_eq!(cache.size().unwrap(), 0);
}

/// The real converter on the real High efficiency★ fixture (macOS, with
/// Adobe DNG Converter installed): `cargo test -p shoebox-develop -- --ignored`.
#[test]
#[ignore]
fn real_converter_on_the_real_nef() {
    let nef = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/real/20260501-203002-65.nef");
    let conv = Converter::find(None).expect("Adobe DNG Converter installed");
    eprintln!("converter {:?}", conv.version());
    let bytes = std::fs::read(&nef).unwrap();
    assert!(needs_dng(&bytes));
    let t = tempfile::tempdir().unwrap();
    let cache = DngCache::new(t.path(), 1 << 30);
    let start = std::time::Instant::now();
    let dng = cache.get_or_convert(&conv, &nef, &bytes).unwrap();
    eprintln!("converted in {:?}", start.elapsed());
    let raw = shoebox_decode::raw::decode_path(&dng).unwrap();
    assert_eq!((raw.width, raw.height), (6048, 4032));
    assert_eq!(raw.model, "Z f");
}
