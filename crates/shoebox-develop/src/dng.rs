//! DNGs for High efficiency NEFs (`docs/EDITING-SPEC.md`, RAW decode).
//!
//! No released open-source decoder reads Nikon's High efficiency (or ★)
//! compression, so the first time such a photo needs its raw data, Adobe's
//! free DNG Converter turns it into a DNG that `rawler` reads. The DNG lives
//! in Shoebox's cache folder, never in the library, named by the NEF's
//! BLAKE3, and is evicted least-recently-used beyond a size limit: it can
//! always be made again. The edit document still refers to the NEF.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use shoebox_decode::nef::{self, NefCompression};

use crate::{DevelopError, Result, io};

/// Where Adobe installs it.
#[cfg(target_os = "macos")]
const DEFAULT_APP: &str = "/Applications/Adobe DNG Converter.app";

/// The cache's default size limit (a setting): about 700 converted Z f files.
pub const DEFAULT_LIMIT: u64 = 20 << 30;

/// At most this many conversions at once.
const MAX_RUNNING: usize = 2;

/// A conversion that takes longer than this has hung (a few seconds is usual).
const TIMEOUT: Duration = Duration::from_secs(180);

/// Whether this NEF needs a DNG to be edited (from its bytes; the catalog's
/// `nef_compression` says the same without opening the file).
pub fn needs_dng(nef_bytes: &[u8]) -> bool {
    nef::compression(nef_bytes)
        .ok()
        .flatten()
        .is_some_and(NefCompression::needs_dng)
}

/// Adobe DNG Converter, run from the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converter {
    program: PathBuf,
}

impl Converter {
    /// The converter in `app` (an `.app` bundle, or the executable itself),
    /// or in its usual place when `app` is None. None if it isn't there.
    pub fn find(app: Option<&Path>) -> Option<Self> {
        let app = match app {
            Some(a) => a.to_path_buf(),
            None => default_app()?,
        };
        let program = if app.extension().is_some_and(|e| e == "app") {
            app.join("Contents/MacOS/Adobe DNG Converter")
        } else {
            app
        };
        program.is_file().then_some(Self { program })
    }

    /// A converter at exactly this executable (tests use a stand-in).
    pub fn at(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The bundle's version ("18.7"), if it's an app bundle that says.
    pub fn version(&self) -> Option<String> {
        let plist = self.program.parent()?.parent()?.join("Info.plist");
        let text = std::fs::read_to_string(plist).ok()?;
        let at = text.find("<key>CFBundleShortVersionString</key>")?;
        let rest = &text[at..];
        let start = rest.find("<string>")? + "<string>".len();
        let end = rest[start..].find("</string>")?;
        Some(rest[start..start + end].trim().to_string())
    }

    /// Convert `nef` to a lossless DNG in the empty directory `out_dir`,
    /// returning the DNG's path. The NEF is only read.
    pub fn convert(&self, nef: &Path, out_dir: &Path) -> Result<PathBuf> {
        // -c: lossless compression (not linear, not lossy); -p0: no preview.
        let mut child = Command::new(&self.program)
            .args(["-c", "-p0", "-d"])
            .arg(out_dir)
            .arg(nef)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(io(&self.program))?;
        let started = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().map_err(io(&self.program))? {
                break s;
            }
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(DevelopError::Converter(format!(
                    "{} took over {} s",
                    nef.display(),
                    TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        if !status.success() {
            return Err(DevelopError::Converter(format!(
                "{} failed ({status})",
                nef.display()
            )));
        }
        let made = std::fs::read_dir(out_dir)
            .map_err(io(out_dir))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dng")));
        made.ok_or_else(|| DevelopError::Converter(format!("{} produced no DNG", nef.display())))
    }
}

#[cfg(target_os = "macos")]
fn default_app() -> Option<PathBuf> {
    Some(PathBuf::from(DEFAULT_APP))
}

/// Adobe makes no Linux build: the converter is found only where a setting
/// points at one.
#[cfg(not(target_os = "macos"))]
fn default_app() -> Option<PathBuf> {
    None
}

/// The cache of converted DNGs: `<cache>/dng/<blake3 of the NEF>.dng`.
pub struct DngCache {
    dir: PathBuf,
    limit: u64,
    /// Keys being converted, and how many conversions are running.
    busy: Mutex<(HashSet<String>, usize)>,
    changed: Condvar,
}

impl DngCache {
    /// The cache in `<cache_dir>/dng`, holding at most `limit` bytes.
    pub fn new(cache_dir: &Path, limit: u64) -> Self {
        Self {
            dir: cache_dir.join("dng"),
            limit,
            busy: Mutex::new((HashSet::new(), 0)),
            changed: Condvar::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The cache key for a NEF's contents.
    pub fn key(nef_bytes: &[u8]) -> String {
        let mut h = blake3::Hasher::new();
        h.update_rayon(nef_bytes);
        h.finalize().to_hex().to_string()
    }

    fn path_of(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.dng"))
    }

    /// The cached DNG for `key`, marked as just used.
    pub fn get(&self, key: &str) -> Option<PathBuf> {
        let p = self.path_of(key);
        let f = std::fs::File::options().write(true).open(&p).ok()?;
        let _ = f.set_modified(SystemTime::now());
        Some(p)
    }

    /// The DNG for `nef` (whose bytes are `nef_bytes`): cached, or converted
    /// now. Waits while two conversions run, or while another thread
    /// converts this same file.
    pub fn get_or_convert(
        &self,
        converter: &Converter,
        nef: &Path,
        nef_bytes: &[u8],
    ) -> Result<PathBuf> {
        let key = Self::key(nef_bytes);
        {
            let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if !busy.0.contains(&key)
                    && let Some(p) = self.get(&key)
                {
                    return Ok(p);
                }
                if !busy.0.contains(&key) && busy.1 < MAX_RUNNING {
                    busy.0.insert(key.clone());
                    busy.1 += 1;
                    break;
                }
                busy = self.changed.wait(busy).unwrap_or_else(|e| e.into_inner());
            }
        }
        let made = self.convert_into_cache(converter, nef, &key);
        {
            let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
            busy.0.remove(&key);
            busy.1 -= 1;
        }
        self.changed.notify_all();
        let path = made?;
        if let Err(e) = self.evict(Some(&path)) {
            tracing::warn!("DNG cache eviction: {e}");
        }
        Ok(path)
    }

    fn convert_into_cache(&self, converter: &Converter, nef: &Path, key: &str) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dir).map_err(io(&self.dir))?;
        // A private folder per conversion: the converter names its output
        // after the NEF, and two NEFs can share a name.
        let tmp = self.dir.join(format!("tmp-{key}"));
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp).map_err(io(&tmp))?;
        }
        std::fs::create_dir(&tmp).map_err(io(&tmp))?;
        let result = (|| {
            let made = converter.convert(nef, &tmp)?;
            // Only keep what rawler can read.
            shoebox_decode::raw::decode_path(&made)?;
            let dest = self.path_of(key);
            std::fs::rename(&made, &dest).map_err(io(&dest))?;
            Ok(dest)
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        if let Ok(p) = &result {
            tracing::info!("converted {} to {}", nef.display(), p.display());
        }
        result
    }

    /// Every cached DNG: `(path, size, last used)`.
    fn entries(&self) -> Result<Vec<(PathBuf, u64, SystemTime)>> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(&self.dir)(e)),
        };
        Ok(rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "dng"))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                Some((e.path(), m.len(), m.modified().ok()?))
            })
            .collect())
    }

    /// Bytes the cache holds.
    pub fn size(&self) -> Result<u64> {
        Ok(self.entries()?.iter().map(|e| e.1).sum())
    }

    /// Remove least-recently-used DNGs until the cache fits its limit,
    /// never `keep` (the one just made or opened). Returns how many went.
    pub fn evict(&self, keep: Option<&Path>) -> Result<usize> {
        let mut entries = self.entries()?;
        let mut total: u64 = entries.iter().map(|e| e.1).sum();
        entries.sort_by_key(|e| e.2);
        let mut removed = 0;
        for (path, size, _) in entries {
            if total <= self.limit {
                break;
            }
            if Some(path.as_path()) == keep {
                continue;
            }
            std::fs::remove_file(&path).map_err(io(&path))?;
            total -= size;
            removed += 1;
        }
        Ok(removed)
    }

    /// Remove every cached DNG (Settings → Raw → Clear).
    pub fn clear(&self) -> Result<usize> {
        let entries = self.entries()?;
        for (path, ..) in &entries {
            std::fs::remove_file(path).map_err(io(path))?;
        }
        Ok(entries.len())
    }
}
