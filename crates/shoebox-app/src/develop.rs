//! The develop thread (`docs/EDITING-SPEC.md`, Develop view): gets a photo
//! ready to edit off the UI thread. For a raw file, a High efficiency NEF
//! is converted to a DNG first (Adobe DNG Converter, cached); then the raw
//! data is decoded, balanced at as shot, its highlights rebuilt and
//! demosaiced to the interactive proxy. A finished image (JPEG, TIFF, PNG,
//! HEIC) is decoded with its colour profile to the proxy size. The newest request wins; the last
//! few results are kept so stepping back and forth is instant.

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use shoebox_decode::raw::RawImage;
use shoebox_develop::cpu::{self, Demosaiced};
use shoebox_develop::dng::{Converter, DngCache};
use shoebox_develop::edit::Raw;

use crate::jobs::PhotoId;

/// The proxy's long edge: the spec's interactive size.
pub const PROXY: u32 = 2560;

/// How many prepared photos to keep (current and neighbours).
const KEEP: usize = 4;

pub struct Request {
    pub id: PhotoId,
    pub path: PathBuf,
    pub format: shoebox_decode::Format,
    pub raw: Raw,
}

/// A photo ready to edit.
#[derive(Clone)]
pub struct Ready {
    pub id: PhotoId,
    pub raw: Raw,
    /// The raw file's metadata (its data dropped: only the matrices and
    /// as-shot white balance are needed from here on).
    pub meta: Arc<RawImage>,
    pub src: Arc<Demosaiced>,
    /// Already upright (decoded by ImageIO, as HEIC is): don't apply the
    /// EXIF orientation again.
    pub upright: bool,
}

/// Render an edit's preview (for the grid, loupe and the rest) unless it's
/// already there.
pub struct PreviewJob {
    pub id: PhotoId,
    pub path: PathBuf,
    pub format: shoebox_decode::Format,
    pub orientation: u16,
    pub settings: shoebox_develop::edit::Settings,
    pub key: String,
    pub target: cpu::Target,
}

enum Message {
    Open(Request),
    Preview(Box<PreviewJob>),
    /// The converter's location (None: where Adobe installs it) and the
    /// DNG cache's limit in bytes.
    Configure(Option<PathBuf>, u64),
    /// Find the converter and measure the cache (Settings → Raw).
    Check,
    ClearCache,
    Export(Box<ExportJob>),
    LoadPresets,
    SavePresets(Vec<Preset>),
}

/// Export these photos with these presets.
pub struct ExportJob {
    pub items: Vec<ExportItem>,
    pub presets: Vec<Preset>,
    /// Library folders: exports never go inside them.
    pub roots: Vec<PathBuf>,
}

pub struct ExportItem {
    pub id: PhotoId,
    pub path: PathBuf,
    pub format: shoebox_decode::Format,
    pub orientation: u16,
    pub settings: shoebox_develop::edit::Settings,
    /// The edit's preview key (None: unedited), recorded with the export
    /// to tell when it's stale.
    pub edit_key: Option<String>,
    /// Capture date, YYYY-MM-DD.
    pub date: Option<String>,
    /// The photo's XMP sidecar, for keywords, rating and title.
    pub sidecar: Option<PathBuf>,
}

/// A file an export wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Exported {
    pub photo: PathBuf,
    pub file: PathBuf,
    pub preset: Preset,
    pub edit_key: Option<String>,
}

/// What Settings → Raw shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawStatus {
    /// The converter found: its executable and version.
    pub converter: Option<(PathBuf, Option<String>)>,
    pub cache_bytes: u64,
}

/// Edited previews' long edge (as the loupe's "large" size and the proxy).
const PREVIEW: u32 = PROXY;

pub enum Event {
    /// Exporting: files written of the total, and the last one.
    ExportProgress {
        done: usize,
        total: usize,
        last: Option<PathBuf>,
    },
    /// An export finished: the files written, and what failed.
    ExportDone {
        written: Vec<Exported>,
        errors: Vec<String>,
        /// Metadata couldn't be copied (exiftool isn't installed).
        no_metadata: bool,
    },
    /// The export presets (saved, or the starters).
    Presets(Vec<Preset>),
    /// The converter and cache, as checked.
    Status(RawStatus),
    /// An edit's preview is in the previews folder.
    PreviewReady(PhotoId, String),
    /// Converting a High efficiency NEF (the first time it's opened).
    Converting(PhotoId),
    Ready(Ready),
    /// High efficiency, and Adobe DNG Converter isn't installed.
    NeedsConverter(PhotoId),
    Failed(PhotoId, String),
}

pub struct Develop {
    requests: Sender<Message>,
    pub events: Receiver<Event>,
    previews: PathBuf,
}

use shoebox_develop::export::Preset;

impl Develop {
    /// Export photos (after any photo being opened).
    pub fn export(&self, job: ExportJob) {
        let _ = self.requests.send(Message::Export(Box::new(job)));
    }

    /// Load the export presets (answered by `Presets`).
    pub fn load_presets(&self) {
        let _ = self.requests.send(Message::LoadPresets);
    }

    pub fn save_presets(&self, presets: Vec<Preset>) {
        let _ = self.requests.send(Message::SavePresets(presets));
    }

    pub fn new(
        cache_dir: PathBuf,
        presets_file: PathBuf,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (rtx, rrx) = crossbeam_channel::unbounded::<Message>();
        let (etx, erx) = crossbeam_channel::unbounded();
        let previews = cache_dir.join("edited");
        let spawned = std::thread::Builder::new()
            .name("develop".into())
            .spawn(move || worker(&cache_dir, &presets_file, &rrx, &etx, Arc::new(repaint)));
        if let Err(e) = spawned {
            tracing::error!("could not start the develop thread: {e}");
        }
        Self {
            requests: rtx,
            events: erx,
            previews,
        }
    }

    pub fn open(&self, r: Request) {
        let _ = self.requests.send(Message::Open(r));
    }

    /// Where the converter is (None: the usual place), and the cache limit.
    pub fn configure(&self, converter: Option<PathBuf>, limit_bytes: u64) {
        let _ = self
            .requests
            .send(Message::Configure(converter, limit_bytes));
    }

    /// Check the converter and the cache's size (answered by `Status`).
    pub fn check(&self) {
        let _ = self.requests.send(Message::Check);
    }

    /// Empty the DNG cache (they're made again as needed).
    pub fn clear_cache(&self) {
        let _ = self.requests.send(Message::ClearCache);
    }

    /// Queue an edit's preview (after any photo being opened).
    pub fn preview(&self, p: PreviewJob) {
        let _ = self.requests.send(Message::Preview(Box::new(p)));
    }

    /// Where an edit's preview is (or will be).
    pub fn preview_path(&self, key: &str) -> PathBuf {
        preview_path(&self.previews, key)
    }
}

fn preview_path(dir: &std::path::Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.jpg"))
}

fn worker(
    cache_dir: &std::path::Path,
    presets_file: &std::path::Path,
    requests: &Receiver<Message>,
    events: &Sender<Event>,
    repaint: Arc<dyn Fn() + Send + Sync>,
) {
    let emit = |e: Event| {
        let _ = events.send(e);
        repaint();
    };
    let mut dngs = DngCache::new(cache_dir, shoebox_develop::dng::DEFAULT_LIMIT);
    let mut converter: Option<PathBuf> = None;
    let mut limit = shoebox_develop::dng::DEFAULT_LIMIT;
    // Exports run on a thread of their own, so Develop never waits on one.
    let (exporter, jobs) = crossbeam_channel::unbounded::<(ExportJob, Option<PathBuf>, u64)>();
    {
        let (cache_dir, events, repaint) =
            (cache_dir.to_path_buf(), events.clone(), repaint.clone());
        let spawned = std::thread::Builder::new()
            .name("export".into())
            .spawn(move || {
                while let Ok((job, conv, limit)) = jobs.recv() {
                    let dngs = DngCache::new(&cache_dir, limit);
                    run_export(&job, conv.as_deref(), &dngs, &|e| {
                        let _ = events.send(e);
                        repaint();
                    });
                }
            });
        if let Err(e) = spawned {
            tracing::error!("could not start the export thread: {e}");
        }
    }
    let previews = cache_dir.join("edited");
    let mut kept: Vec<Ready> = Vec::new();
    let mut open: Option<Request> = None;
    let mut queue: std::collections::VecDeque<PreviewJob> = Default::default();
    loop {
        // Take everything waiting; with nothing to do, wait.
        let first = if open.is_none() && queue.is_empty() {
            match requests.recv() {
                Ok(m) => Some(m),
                Err(_) => return,
            }
        } else {
            None
        };
        for m in first.into_iter().chain(requests.try_iter()) {
            match m {
                // Only the newest photo opened matters.
                Message::Open(r) => open = Some(r),
                Message::Preview(p) => {
                    queue.retain(|q| q.id != p.id);
                    queue.push_back(*p);
                }
                Message::Configure(path, new_limit) => {
                    converter = path;
                    limit = new_limit;
                    dngs = DngCache::new(cache_dir, limit);
                    if let Err(e) = dngs.evict(None) {
                        tracing::warn!("DNG cache: {e}");
                    }
                }
                Message::Check => emit(Event::Status(status(converter.as_deref(), &dngs))),
                Message::Export(job) => {
                    if exporter.send((*job, converter.clone(), limit)).is_err() {
                        tracing::warn!("the export thread has stopped");
                    }
                }
                Message::LoadPresets => emit(Event::Presets(load_presets(presets_file))),
                Message::SavePresets(list) => {
                    if let Err(e) = save_presets(presets_file, &list) {
                        tracing::warn!("saving export presets: {e}");
                    }
                }
                Message::ClearCache => {
                    if let Err(e) = dngs.clear() {
                        tracing::warn!("clearing the DNG cache: {e}");
                    }
                    emit(Event::Status(status(converter.as_deref(), &dngs)));
                }
            }
        }
        if let Some(r) = open.take() {
            let ready = match kept.iter().position(|k| k.id == r.id && k.raw == r.raw) {
                Some(hit) => {
                    let k = kept.remove(hit);
                    Ok(Some(k))
                }
                None => prepare(&r, Some(PROXY), converter.as_deref(), &dngs, &emit),
            };
            match ready {
                Ok(Some(ready)) => {
                    kept.push(ready.clone());
                    if kept.len() > KEEP {
                        kept.remove(0);
                    }
                    emit(Event::Ready(ready));
                }
                Ok(None) => emit(Event::NeedsConverter(r.id)),
                Err(e) => emit(Event::Failed(r.id, e)),
            }
            continue;
        }
        if let Some(p) = queue.pop_front() {
            match preview(&p, &previews, converter.as_deref(), &dngs, &kept) {
                Ok(true) => emit(Event::PreviewReady(p.id, p.key)),
                Ok(false) => {}
                Err(e) => tracing::warn!("previewing the edit of {}: {e}", p.path.display()),
            }
        }
    }
}

/// Render an edit's preview JPEG unless it exists. False when it can't be
/// made yet (a High efficiency NEF without the converter).
fn status(converter: Option<&std::path::Path>, dngs: &DngCache) -> RawStatus {
    RawStatus {
        converter: Converter::find(converter).map(|c| (c.program().to_path_buf(), c.version())),
        cache_bytes: dngs.size().unwrap_or(0),
    }
}

fn preview(
    p: &PreviewJob,
    dir: &std::path::Path,
    converter: Option<&std::path::Path>,
    dngs: &DngCache,
    kept: &[Ready],
) -> Result<bool, String> {
    let out = preview_path(dir, &p.key);
    if out.is_file() {
        return Ok(true);
    }
    let raw = p.settings.modules.raw.clone();
    let ready = match kept.iter().find(|k| k.id == p.id && k.raw == raw) {
        Some(k) => k.clone(),
        None => {
            let r = Request {
                id: p.id,
                path: p.path.clone(),
                format: p.format,
                raw,
            };
            match prepare(&r, Some(PROXY), converter, dngs, &|_| {})? {
                Some(k) => k,
                None => return Ok(false),
            }
        }
    };
    let o = cpu::Options {
        long_edge: None,
        orientation: if ready.upright { 1 } else { p.orientation },
        target: p.target,
    };
    let src = if ready.src.rgb.width.max(ready.src.rgb.height) > PREVIEW as usize {
        Arc::new(Demosaiced {
            rgb: cpu::downscale(&ready.src.rgb, PREVIEW as usize),
            ..(*ready.src).clone()
        })
    } else {
        ready.src.clone()
    };
    let frame = cpu::frame(&ready.meta, &src, &p.settings, &o);
    let img = cpu::apply(&src.rgb, &frame, true);
    let bytes = shoebox_develop::encode::jpeg(&img, p.target, Default::default())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // The app's own cache: written whole, then renamed into place.
    let tmp = dir.join(format!(".{}.tmp", p.key));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(true)
}

/// Decode and demosaic one photo; None if it needs the converter and it
/// isn't installed.
fn prepare(
    r: &Request,
    long_edge: Option<u32>,
    converter: Option<&std::path::Path>,
    dngs: &DngCache,
    emit: &dyn Fn(Event),
) -> Result<Option<Ready>, String> {
    if r.format != shoebox_decode::Format::Nef {
        return finished(r, long_edge).map(Some);
    }
    let bytes = std::fs::read(&r.path).map_err(|e| format!("{}: {e}", r.path.display()))?;
    let source = if shoebox_develop::dng::needs_dng(&bytes) {
        let Some(conv) = Converter::find(converter) else {
            return Ok(None);
        };
        let key = DngCache::key(&bytes);
        match dngs.get(&key) {
            Some(p) => p,
            None => {
                emit(Event::Converting(r.id));
                dngs.get_or_convert(&conv, &r.path, &bytes)
                    .map_err(|e| e.to_string())?
            }
        }
    } else {
        r.path.clone()
    };
    drop(bytes);
    let mut raw = shoebox_decode::raw::decode_path(&source).map_err(|e| e.to_string())?;
    let src = cpu::demosaic(&raw, &r.raw, long_edge);
    raw.data = Vec::new();
    Ok(Some(Ready {
        id: r.id,
        raw: r.raw.clone(),
        meta: Arc::new(raw),
        src: Arc::new(src),
        upright: false,
    }))
}

/// A finished image: decoded (colour-managed to Display P3, the widest the
/// decoder offers) at the proxy size, as a display-referred source.
fn finished(r: &Request, long_edge: Option<u32>) -> Result<Ready, String> {
    use shoebox_decode::{Format, Target};
    let img = shoebox_decode::render_path(r.format, &r.path, long_edge, 1, Target::DisplayP3)
        .map_err(|e| e.to_string())?;
    let src = cpu::from_display(
        img.width as usize,
        img.height as usize,
        &img.px,
        cpu::Target::DisplayP3,
    );
    Ok(Ready {
        id: r.id,
        raw: r.raw.clone(),
        meta: Arc::new(RawImage {
            width: 0,
            height: 0,
            cfa: [[0, 1], [1, 2]],
            data: Vec::new(),
            as_shot: None,
            matrices: Vec::new(),
            make: String::new(),
            model: String::new(),
        }),
        src: Arc::new(src),
        upright: r.format == Format::Heic,
    })
}

/// The export presets file, or the starters if there's none (or it can't
/// be read).
fn load_presets(file: &std::path::Path) -> Vec<Preset> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Preset>>(&t).ok())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(shoebox_develop::export::starters)
}

fn save_presets(file: &std::path::Path, list: &[Preset]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // Settings of the app's own (not the library): written whole, renamed.
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
}

/// The default export folder for a preset: `~/Pictures/Shoebox Exports/<name>`.
pub fn default_folder(preset: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let name: String = preset
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '_'
            } else {
                c
            }
        })
        .collect();
    Some(
        PathBuf::from(home)
            .join("Pictures/Shoebox Exports")
            .join(name),
    )
}

/// Run an export: each photo rendered once at full size, then written for
/// each preset.
fn run_export(
    job: &ExportJob,
    converter: Option<&std::path::Path>,
    dngs: &DngCache,
    emit: &dyn Fn(Event),
) {
    let total = job.items.len() * job.presets.len();
    let mut done = 0;
    let mut written = Vec::new();
    let mut errors = Vec::new();
    let exiftool = exiftool();
    for (seq, item) in job.items.iter().enumerate() {
        let name = item
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let r = Request {
            id: item.id,
            path: item.path.clone(),
            format: item.format,
            raw: item.settings.modules.raw.clone(),
        };
        let ready = match prepare(&r, None, converter, dngs, &|_| {}) {
            Ok(Some(k)) => k,
            Ok(None) => {
                errors.push(format!("{name}: needs Adobe DNG Converter"));
                done += job.presets.len();
                continue;
            }
            Err(e) => {
                errors.push(format!("{name}: {e}"));
                done += job.presets.len();
                continue;
            }
        };
        for preset in &job.presets {
            let result = (|| -> Result<PathBuf, String> {
                let folder = match &preset.folder {
                    Some(f) => f.clone(),
                    None => default_folder(&preset.name).ok_or("no home folder")?,
                };
                // Compared as real paths: /var is /private/var on macOS,
                // and a symlink mustn't sneak an export into the library.
                let real = real_path(&folder);
                if job.roots.iter().any(|r| real.starts_with(real_path(r))) {
                    return Err(format!(
                        "{} is inside your library; exports go elsewhere",
                        folder.display()
                    ));
                }
                let o = cpu::Options {
                    long_edge: None,
                    orientation: if ready.upright { 1 } else { item.orientation },
                    target: preset.space.target(),
                };
                let frame = cpu::frame(&ready.meta, &ready.src, &item.settings, &o);
                let img = cpu::apply(&ready.src.rgb, &frame, true);
                let (w, h) =
                    shoebox_develop::export::output_size(img.width, img.height, preset.size);
                let img = shoebox_develop::export::resize(&img, w, h);
                let bytes = shoebox_develop::export::encode(&img, preset)?;
                let stem = shoebox_develop::export::file_stem(
                    &preset.naming,
                    &name,
                    &preset.name,
                    item.date.as_deref(),
                    seq + 1,
                );
                let out = shoebox_develop::export::write_new(
                    &folder,
                    &stem,
                    preset.format.extension(),
                    &bytes,
                )?;
                if let Some(tool) = &exiftool {
                    copy_metadata(tool, item, preset, &out);
                }
                Ok(out)
            })();
            done += 1;
            match result {
                Ok(file) => {
                    emit(Event::ExportProgress {
                        done,
                        total,
                        last: Some(file.clone()),
                    });
                    written.push(Exported {
                        photo: item.path.clone(),
                        file,
                        preset: preset.clone(),
                        edit_key: item.edit_key.clone(),
                    });
                }
                Err(e) => {
                    errors.push(format!("{name} ({}): {e}", preset.name));
                    emit(Event::ExportProgress {
                        done,
                        total,
                        last: None,
                    });
                }
            }
        }
    }
    emit(Event::ExportDone {
        written,
        errors,
        no_metadata: exiftool.is_none(),
    });
}

/// `p` with symlinks resolved, as far as it exists (the rest appended).
fn real_path(p: &std::path::Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => break,
        }
    }
    let mut out = existing.canonicalize().unwrap_or(existing);
    for name in rest.into_iter().rev() {
        out.push(name);
    }
    out
}

/// exiftool, if installed (Homebrew's, or on the PATH).
fn exiftool() -> Option<PathBuf> {
    [
        "/opt/homebrew/bin/exiftool",
        "/usr/local/bin/exiftool",
        "/usr/bin/exiftool",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Copy the original's metadata (and its sidecar's) into an exported file,
/// with exiftool. The pixels are already upright, so Orientation is 1; the
/// export's own colour profile is kept.
fn copy_metadata(
    tool: &std::path::Path,
    item: &ExportItem,
    preset: &Preset,
    out: &std::path::Path,
) {
    use shoebox_develop::export::Metadata;
    let mut cmd = std::process::Command::new(tool);
    cmd.args(["-q", "-q", "-overwrite_original", "-m"]);
    match preset.metadata {
        Metadata::CopyrightOnly => {
            cmd.arg("-TagsFromFile").arg(&item.path);
            cmd.args(["-Copyright", "-Artist", "-XMP-dc:Rights", "-XMP-dc:Creator"]);
        }
        Metadata::All | Metadata::AllButLocation => {
            cmd.arg("-TagsFromFile").arg(&item.path);
            cmd.args([
                "-all:all",
                "--ICC_Profile:all",
                "--Orientation",
                "--ThumbnailImage",
                "--PreviewImage",
            ]);
            if let Some(s) = &item.sidecar {
                cmd.arg("-TagsFromFile").arg(s).arg("-XMP:all");
            }
            if preset.metadata == Metadata::AllButLocation {
                cmd.args(["-GPS:all=", "-XMP:Location*=", "-XMP-exif:GPS*="]);
            }
        }
    }
    cmd.arg("-Orientation#=1").arg(out);
    match cmd.output() {
        Ok(o) if o.status.success() => {}
        Ok(o) => tracing::warn!(
            "exiftool on {}: {}",
            out.display(),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(e) => tracing::warn!("exiftool: {e}"),
    }
}
