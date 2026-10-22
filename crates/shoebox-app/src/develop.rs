//! The develop thread (`docs/EDITING-SPEC.md`, Develop view): gets a photo
//! ready to edit off the UI thread. For a raw file, a High efficiency NEF
//! is converted to a DNG first (Adobe DNG Converter, cached); then the raw
//! data is decoded, balanced at as shot, its highlights rebuilt and
//! demosaiced to the interactive proxy. A finished image (JPEG, TIFF, PNG,
//! HEIC) is decoded with its colour profile to the proxy size. The newest request wins; the last
//! few results are kept so stepping back and forth is instant.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    /// How it was taken (lens corrections), from the catalog's EXIF.
    pub lens: Option<Arc<Shot>>,
}

use shoebox_develop::lens::Shot;

/// What a photo's EXIF says about its lens (None without a camera and
/// lens). A NEF's own lens data is added as the file is read.
pub fn shot(photo: &shoebox_core::catalog::Photo) -> Option<Arc<Shot>> {
    let x = &photo.meta.exif;
    Some(Arc::new(Shot {
        make: x.make.clone()?,
        model: x.model.clone()?,
        lens: x.lens.clone().unwrap_or_default(),
        focal: x.focal.unwrap_or(0.0) as f32,
        aperture: x.fnumber.unwrap_or(0.0) as f32,
        camera_data: Default::default(),
    }))
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
    /// What Auto measured (raw photos prepared for Develop).
    pub auto: Option<shoebox_develop::auto::Stats>,
}

/// Render an edit's preview (for the grid, loupe and the rest) unless it's
/// already there.
pub struct PreviewJob {
    pub id: PhotoId,
    pub path: PathBuf,
    pub lens: Option<Arc<Shot>>,
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
    /// Edit presets (`<data>/presets/user.json`): load (answered by
    /// `Looks`), and save the whole list.
    LoadLooks,
    SaveLooks(Vec<shoebox_develop::preset::Preset>),
    /// Render part of a photo at full resolution (zoomed in).
    Region(Box<RegionJob>),
    /// High efficiency NEFs to convert in the background (replaces the last
    /// list).
    ConvertAhead(Vec<PathBuf>),
    /// AI raw denoise for the photo open in Develop, at the proxy's size
    /// (on its own thread; a newer one stops the last).
    Ai(Request),
    /// Read the colour files again (DNG camera profiles, looks); with a
    /// folder, first make it if it isn't there and show it in Finder.
    /// Answered by `Colours`.
    Colours(Option<ColourFolder>),
}

/// The person's own colour files, in Shoebox's data folder: only read,
/// never written (the folder is made if it isn't there, to show it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColourFolder {
    /// `<data>/profiles`: DNG camera profiles (`.dcp`) and camera match
    /// profiles.
    Profiles,
    /// `<data>/looks`: looks (`.cube`).
    Looks,
}

impl ColourFolder {
    fn path(self, data: &std::path::Path) -> PathBuf {
        match self {
            ColourFolder::Profiles => data.join("profiles"),
            ColourFolder::Looks => data.join("looks"),
        }
    }
}

/// Read the DNG camera profiles and looks in the data folder (`data`).
fn load_colours(data: &std::path::Path) {
    let n = shoebox_develop::dcp::load_dir(&ColourFolder::Profiles.path(data));
    let k = shoebox_develop::cube::load_dir(&ColourFolder::Looks.path(data));
    if n + k > 0 {
        tracing::info!("{n} DNG camera profiles, {k} looks");
    }
}

/// What's drawn over a render: clipping (J), soft proofing in an export
/// space (S) and, with it, the colours that space can't hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Overlays {
    pub clipping: bool,
    pub proof: Option<shoebox_develop::export::Space>,
    pub gamut: bool,
    /// A local adjustment's mask shown (O): its place in the settings'
    /// `locals`, and whether alone.
    pub mask: Option<(usize, bool)>,
    /// A Point Color point's reach shown (its index).
    pub point: Option<usize>,
}

impl Overlays {
    pub fn apply(&self, f: &mut cpu::Frame) {
        f.warnings = self.clipping;
        f.display.proof = self.proof.map(|s| cpu::Proof::new(s.target()));
        f.gamut_warning = self.proof.is_some() && self.gamut;
        f.mask_view = self.mask.and_then(|(k, only)| {
            let at = f.layers.from.iter().position(|&j| j == k)?;
            Some((at, only))
        });
        f.point_view = self.point.filter(|&k| k < f.look.point_count);
    }

    /// Keys renders made with them.
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

/// A part of a photo to render at full resolution: `region` is
/// `[x, y, width, height]` in the full-size render's pixels.
pub struct RegionJob {
    pub request: Request,
    pub settings: shoebox_develop::edit::Settings,
    pub options: cpu::Options,
    pub overlays: Overlays,
    pub region: [usize; 4],
    /// Echoed back, to tell which request a result answers.
    pub key: u64,
}

/// Export these photos with these presets.
#[derive(Clone)]
pub struct ExportJob {
    pub items: Vec<ExportItem>,
    pub presets: Vec<Preset>,
    /// Library folders. An export into one is written to the cache first,
    /// then copied in by the library worker (journaled, never replacing).
    pub roots: Vec<PathBuf>,
    /// The library's reference folders: exports never go inside them.
    pub reference: Vec<PathBuf>,
    /// What to do where a file is already there; None: ask first.
    pub on_clash: Option<Clash>,
}

/// An export onto a file that's already there, as the person chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clash {
    /// Save a new file beside it (`-1`, `-2`).
    NewFile,
    /// Replace it.
    Replace,
}

#[derive(Clone)]
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
    /// How it was taken (lens corrections).
    pub lens: Option<Arc<Shot>>,
    /// A virtual copy's name, added to the file name ("DSC_1234_Copy 1").
    pub copy_name: Option<String>,
    /// Which edit: `edits::MAIN`, or a virtual copy's id.
    pub copy: String,
    /// Re-exporting an export in the library: its file, which the new one
    /// takes the place of (the old one goes to the Trash).
    pub replaces: Option<PathBuf>,
}

/// An export for the library, written in the cache: the library worker
/// copies it in beside its photo and joins it to the photo's stack.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryExport {
    pub copy: shoebox_core::fileops::PlannedCopy,
    pub photo: PathBuf,
    /// The earlier export it replaces (to the Trash in the same batch).
    pub replaces: Option<PathBuf>,
}

/// A file an export wrote.
#[derive(Debug, Clone, PartialEq)]
pub struct Exported {
    pub photo: PathBuf,
    /// Which edit: `edits::MAIN`, or a virtual copy's id.
    pub copy: String,
    pub file: PathBuf,
    pub preset: Preset,
    pub edit_key: Option<String>,
    /// In the library (kept track of: stale, re-export); outside it isn't.
    pub tracked: bool,
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
    /// A full-resolution region (answer to `region`), and the full render's
    /// size. `region` may be clamped to the render.
    Region {
        id: PhotoId,
        key: u64,
        region: [usize; 4],
        full: (usize, usize),
        rgb: cpu::Rgb,
    },
    /// Exporting: files written of the total, and the last one.
    ExportProgress {
        done: usize,
        total: usize,
        last: Option<PathBuf>,
    },
    /// Files this export would land on are already there: it hasn't
    /// started; ask, then send it again with `on_clash` set.
    ExportClashes {
        job: Box<ExportJob>,
        files: Vec<PathBuf>,
    },
    /// An export finished: the files written, and what failed.
    ExportDone {
        written: Vec<Exported>,
        errors: Vec<String>,
        /// Metadata couldn't be copied (exiftool isn't installed).
        no_metadata: bool,
        /// Files for the library, in the cache: for the library worker to
        /// copy in (`LibraryCommand::AddExports`).
        into_library: Vec<LibraryExport>,
    },
    /// The export presets (saved, or the starters).
    Presets(Vec<Preset>),
    /// The edit presets.
    Looks(Vec<shoebox_develop::preset::Preset>),
    /// The colour files were read again.
    Colours,
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
    /// AI raw denoise: how far (0–1); done (at the proxy's size, for these
    /// raw settings); or why not.
    AiProgress(PhotoId, f32),
    Ai(PhotoId, Raw, Arc<cpu::Rgb>),
    AiFailed(PhotoId, String),
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

    /// Render part of the photo at full size (the newest request wins).
    pub fn region(&self, job: RegionJob) {
        let _ = self.requests.send(Message::Region(Box::new(job)));
    }

    /// Load the export presets (answered by `Presets`).
    pub fn load_presets(&self) {
        let _ = self.requests.send(Message::LoadPresets);
    }

    pub fn save_presets(&self, presets: Vec<Preset>) {
        let _ = self.requests.send(Message::SavePresets(presets));
    }

    /// Load the edit presets (answered by `Looks`).
    pub fn load_looks(&self) {
        let _ = self.requests.send(Message::LoadLooks);
    }

    pub fn save_looks(&self, looks: Vec<shoebox_develop::preset::Preset>) {
        let _ = self.requests.send(Message::SaveLooks(looks));
    }

    /// Read the colour files again (answered by `Colours`); with a folder,
    /// show it in Finder too.
    pub fn colours(&self, show: Option<ColourFolder>) {
        let _ = self.requests.send(Message::Colours(show));
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

    /// AI raw denoise for the photo open (answered by `Ai` or `AiFailed`).
    pub fn ai(&self, r: Request) {
        let _ = self.requests.send(Message::Ai(r));
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

    /// Convert these NEFs' DNGs in the background, in order, one at a time
    /// (the converter's other slot stays free for the photo being opened).
    /// Replaces the last list; one converting carries on. Files that don't
    /// need a DNG, or have one, are passed over.
    pub fn convert_ahead(&self, nefs: Vec<PathBuf>) {
        let _ = self.requests.send(Message::ConvertAhead(nefs));
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
    // One cache for every thread, so at most two conversions run at once.
    let dngs = Arc::new(DngCache::new(
        cache_dir,
        shoebox_develop::dng::DEFAULT_LIMIT,
    ));
    let mut converter: Option<PathBuf> = None;
    // Exports run on a thread of their own, so Develop never waits on one.
    let (exporter, jobs) = crossbeam_channel::unbounded::<(ExportJob, Option<PathBuf>)>();
    {
        let (dngs, events, repaint) = (dngs.clone(), events.clone(), repaint.clone());
        let staging = cache_dir.join("export-staging");
        let spawned = std::thread::Builder::new()
            .name("export".into())
            .spawn(move || {
                while let Ok((job, conv)) = jobs.recv() {
                    run_export(&job, conv.as_deref(), &dngs, &staging, &|e| {
                        let _ = events.send(e);
                        repaint();
                    });
                }
            });
        if let Err(e) = spawned {
            tracing::error!("could not start the export thread: {e}");
        }
    }
    // Conversions ahead of need, on another.
    let (ahead, ahead_rx) = crossbeam_channel::unbounded::<(Option<PathBuf>, Vec<PathBuf>)>();
    {
        let dngs = dngs.clone();
        let spawned = std::thread::Builder::new()
            .name("convert-ahead".into())
            .spawn(move || convert_ahead(&dngs, &ahead_rx));
        if let Err(e) = spawned {
            tracing::error!("could not start the convert-ahead thread: {e}");
        }
    }
    let _ = AI_CACHE.set(shoebox_develop::denoise::Cache::new(
        &cache_dir.join("ai-denoise"),
        shoebox_develop::denoise::DEFAULT_LIMIT,
    ));
    // AI raw denoise for Develop, on a thread of its own: the newest
    // request, the last one stopped.
    let (ai_tx, ai_rx) =
        crossbeam_channel::unbounded::<(Request, Option<PathBuf>, Arc<AtomicBool>)>();
    let mut ai_cancel = Arc::new(AtomicBool::new(false));
    {
        let (dngs, events, repaint) = (dngs.clone(), events.clone(), repaint.clone());
        let spawned = std::thread::Builder::new()
            .name("ai-denoise".into())
            .spawn(move || {
                let emit = |e: Event| {
                    let _ = events.send(e);
                    repaint();
                };
                while let Ok(mut job) = ai_rx.recv() {
                    // Only the newest.
                    while let Ok(newer) = ai_rx.try_recv() {
                        job = newer;
                    }
                    let (r, converter, cancel) = job;
                    match ai_for_view(&r, converter.as_deref(), &dngs, &emit, &cancel) {
                        Ok(img) => emit(Event::Ai(r.id, r.raw.clone(), Arc::new(img))),
                        Err(e) if cancel.load(Ordering::Relaxed) => {
                            tracing::debug!("AI denoise stopped: {e}")
                        }
                        Err(e) => emit(Event::AiFailed(r.id, e)),
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::error!("could not start the AI denoise thread: {e}");
        }
    }
    let previews = cache_dir.join("edited");
    // What Vision and SAM find, kept on disk by the picture they saw.
    shoebox_develop::mask::store::set(Some(shoebox_develop::mask::store::Store::new(
        &cache_dir.join("ai-masks"),
        shoebox_develop::mask::store::DEFAULT_LIMIT,
    )));
    // Object removal's fills, likewise by what the model saw.
    shoebox_develop::heal::remove::set_store(Some(shoebox_develop::mask::store::Store::new(
        &cache_dir.join("ai-fills"),
        shoebox_develop::heal::remove::DEFAULT_LIMIT,
    )));
    // Subject and person masks come from Apple's Vision (macOS only; a
    // portable model is a later phase). Kept on disk by the OS build.
    let vision = shoebox_ml::vision::identity().unwrap_or_default();
    shoebox_develop::mask::register_finder(
        &vision,
        Box::new(|rgb, w, h, what| {
            use shoebox_develop::edit::FacePart as P;
            use shoebox_develop::mask::Find;
            use shoebox_ml::vision::FacePart as V;
            let find = match what {
                Find::Subject => shoebox_ml::vision::Find::Subject,
                Find::Person => shoebox_ml::vision::Find::Person,
                Find::Face(part) => {
                    let part = match part {
                        P::Skin => V::Skin,
                        P::Eyes => V::Eyes,
                        P::Eyebrows => V::Eyebrows,
                        P::Lips => V::Lips,
                        P::Teeth => V::Teeth,
                    };
                    return shoebox_ml::vision::face_parts(rgb, w, h, part)
                        .map(|c| c.data)
                        .map_err(|e| e.to_string());
                }
                // Objects go to the segmenter (`objects`), landscape to the
                // scene model (`models::register_landscape`), not here.
                Find::Object(_) | Find::Landscape(_) => {
                    return Err("not a Vision search".into());
                }
            };
            shoebox_ml::vision::segment(rgb, w, h, find)
                .map(|c| c.data)
                .map_err(|e| e.to_string())
        }),
    );
    // Faces' boxes for Smart Lighting's spot (Vision's are x, y, w, h).
    shoebox_develop::mask::register_face_finder(Box::new(|rgb, w, h| {
        shoebox_ml::vision::faces(rgb, w, h)
            .map(|faces| {
                faces
                    .iter()
                    .map(|f| {
                        let b = f.bounds;
                        [b[0], b[1], b[0] + b[2], b[1] + b[3]]
                    })
                    .collect()
            })
            .map_err(|e| e.to_string())
    }));
    // Camera match profiles (`<data>/profiles/*.json`).
    let n = shoebox_develop::profile::load_dir(&presets_file.with_file_name("profiles"));
    if n > 0 {
        tracing::info!("{n} camera match profiles");
    }
    let data = presets_file
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    load_colours(&data);
    // Load Lensfun's database now, so turning lens corrections on doesn't
    // wait for it.
    let _ = std::thread::Builder::new()
        .name("lensfun".into())
        .spawn(|| {
            shoebox_develop::lens::lensfun();
        });
    let mut kept: Vec<Ready> = Vec::new();
    let mut open: Option<Request> = None;
    let mut queue: std::collections::VecDeque<PreviewJob> = Default::default();
    let mut region: Option<Box<RegionJob>> = None;
    // The photo being zoomed into, demosaiced at full size.
    let mut full: Option<Ready> = None;
    loop {
        // Take everything waiting; with nothing to do, wait.
        let first = if open.is_none() && queue.is_empty() && region.is_none() {
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
                Message::Region(j) => region = Some(j),
                Message::Preview(p) => {
                    queue.retain(|q| q.id != p.id);
                    queue.push_back(*p);
                }
                Message::Configure(path, limit) => {
                    converter = path;
                    dngs.set_limit(limit);
                    if let Err(e) = dngs.evict(None) {
                        tracing::warn!("DNG cache: {e}");
                    }
                }
                Message::Check => emit(Event::Status(status(converter.as_deref(), &dngs))),
                Message::Export(job) => {
                    if exporter.send((*job, converter.clone())).is_err() {
                        tracing::warn!("the export thread has stopped");
                    }
                }
                Message::ConvertAhead(list) => {
                    let _ = ahead.send((converter.clone(), list));
                }
                Message::Ai(r) => {
                    ai_cancel.store(true, Ordering::Relaxed);
                    ai_cancel = Arc::new(AtomicBool::new(false));
                    let _ = ai_tx.send((r, converter.clone(), ai_cancel.clone()));
                }
                Message::Colours(show) => {
                    if let Some(f) = show {
                        let dir = f.path(&data);
                        match std::fs::create_dir_all(&dir) {
                            Ok(()) => crate::app::open_file(&dir),
                            Err(e) => tracing::warn!("{}: {e}", dir.display()),
                        }
                    }
                    load_colours(&data);
                    emit(Event::Colours);
                }
                Message::LoadPresets => emit(Event::Presets(load_presets(presets_file))),
                Message::LoadLooks => emit(Event::Looks(shoebox_develop::preset::load(
                    &looks_file(presets_file),
                ))),
                Message::SaveLooks(list) => {
                    if let Err(e) = shoebox_develop::preset::save(&looks_file(presets_file), &list)
                    {
                        tracing::warn!("saving presets: {e}");
                    }
                }
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
                None => prepare(&r, Some(PROXY), converter.as_deref(), &dngs, &emit, false),
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
        if let Some(job) = region.take() {
            let r = &job.request;
            let ai = cpu::layers::wants_ai(&job.settings);
            if full.as_ref().is_none_or(|f| {
                f.id != r.id || f.raw != r.raw || (ai && f.src.cache.ai().is_none())
            }) {
                full = None;
                match prepare(r, None, converter.as_deref(), &dngs, &|_| {}, ai) {
                    Ok(Some(k)) => full = Some(k),
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!("full-size render of {}: {e}", r.path.display());
                        continue;
                    }
                }
            }
            if let Some(f) = &full {
                let o = cpu::Options {
                    orientation: if f.upright {
                        1
                    } else {
                        job.options.orientation
                    },
                    ..job.options
                };
                let mut frame = cpu::frame(&f.meta, &f.src, &job.settings, &o);
                job.overlays.apply(&mut frame);
                let size = (frame.width, frame.height);
                let frame = frame.region(job.region);
                let x = job.region[0].min(size.0.saturating_sub(1));
                let y = job.region[1].min(size.1.saturating_sub(1));
                let (w, h) = (frame.width, frame.height);
                let rgb = cpu::apply_from(&f.src, &job.settings, &frame, true);
                emit(Event::Region {
                    id: r.id,
                    key: job.key,
                    region: [x, y, w, h],
                    full: size,
                    rgb,
                });
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

/// The convert-ahead thread: the newest list, one file at a time.
fn convert_ahead(dngs: &DngCache, lists: &Receiver<(Option<PathBuf>, Vec<PathBuf>)>) {
    let mut list = std::collections::VecDeque::new();
    let mut converter = None;
    loop {
        if list.is_empty() {
            match lists.recv() {
                Ok((c, l)) => (converter, list) = (c, l.into()),
                Err(_) => return,
            }
        }
        for (c, l) in lists.try_iter() {
            (converter, list) = (c, l.into());
        }
        let Some(nef) = list.pop_front() else {
            continue;
        };
        let Some(conv) = Converter::find(converter.as_deref()) else {
            list.clear();
            continue;
        };
        let Ok(bytes) = std::fs::read(&nef) else {
            continue;
        };
        if !shoebox_develop::dng::needs_dng(&bytes) || dngs.has(&DngCache::key(&bytes)) {
            continue;
        }
        if let Err(e) = dngs.get_or_convert(&conv, &nef, &bytes) {
            tracing::warn!("converting {} ahead: {e}", nef.display());
        }
    }
}

/// The converter and the cache, for Settings → Raw.
fn status(converter: Option<&std::path::Path>, dngs: &DngCache) -> RawStatus {
    RawStatus {
        converter: Converter::find(converter).map(|c| (c.program().to_path_buf(), c.version())),
        cache_bytes: dngs.size().unwrap_or(0),
    }
}

/// Render an edit's preview JPEG unless it exists. False when it can't be
/// made yet (a High efficiency NEF without the converter).
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
    let ai = cpu::layers::wants_ai(&p.settings);
    let ready = match kept.iter().find(|k| k.id == p.id && k.raw == raw) {
        Some(k) => k.clone(),
        None => {
            let r = Request {
                id: p.id,
                path: p.path.clone(),
                format: p.format,
                raw,
                lens: p.lens.clone(),
            };
            match prepare(&r, Some(PROXY), converter, dngs, &|_| {}, ai)? {
                Some(k) => k,
                None => return Ok(false),
            }
        }
    };
    if ai && ready.meta.is_mosaic() && ready.src.cache.ai().is_none() {
        // Kept from Develop without the AI's result: make it now.
        let r = Request {
            id: p.id,
            path: p.path.clone(),
            format: p.format,
            raw: p.settings.modules.raw.clone(),
            lens: p.lens.clone(),
        };
        if let Some(k) = prepare(&r, Some(PROXY), converter, dngs, &|_| {}, true)? {
            return finish_preview(p, &k, dir, &out);
        }
    }
    finish_preview(p, &ready, dir, &out)
}

/// Render and write the preview from a prepared photo.
fn finish_preview(
    p: &PreviewJob,
    ready: &Ready,
    dir: &std::path::Path,
    out: &std::path::Path,
) -> Result<bool, String> {
    let o = cpu::Options {
        long_edge: None,
        orientation: if ready.upright { 1 } else { p.orientation },
        target: p.target,
    };
    let src = if ready.src.rgb.width.max(ready.src.rgb.height) > PREVIEW as usize {
        let small = cpu::downscale(&ready.src.rgb, PREVIEW as usize);
        let scale = ready.src.scale * small.width as f32 / ready.src.rgb.width as f32;
        let d = Demosaiced::new(small, ready.src.as_shot, ready.src.referred)
            .with_scale(scale)
            .with_lens(ready.src.lens.clone());
        if let Some(ai) = ready.src.cache.ai() {
            d.cache
                .set_ai(Arc::new(cpu::downscale(&ai, PREVIEW as usize)));
        }
        Arc::new(d)
    } else {
        ready.src.clone()
    };
    let frame = cpu::frame(&ready.meta, &src, &p.settings, &o);
    let img = cpu::apply_from(&src, &p.settings, &frame, true);
    let bytes = shoebox_develop::encode::jpeg(&img, p.target, Default::default())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // The app's own cache: written whole, then renamed into place.
    let tmp = dir.join(format!(".{}.tmp", p.key));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(true)
}

/// Decode and demosaic one photo; None if it needs the converter and it
/// isn't installed.
/// With `ai`, the AI-denoised source (kept on disk) is attached too.
fn prepare(
    r: &Request,
    long_edge: Option<u32>,
    converter: Option<&std::path::Path>,
    dngs: &DngCache,
    emit: &dyn Fn(Event),
    ai: bool,
) -> Result<Option<Ready>, String> {
    if !r.format.is_raw() {
        return finished(r, long_edge).map(Some);
    }
    let bytes = std::fs::read(&r.path).map_err(|e| format!("{}: {e}", r.path.display()))?;
    let Some(source) = raw_source(r, &bytes, converter, dngs, emit)? else {
        return Ok(None);
    };
    let file_hash = blake3::hash(&bytes).to_hex().to_string();
    // The camera's own lens data, from the NEF itself.
    let lens = r.lens.as_ref().map(|shot| {
        Arc::new(Shot {
            camera_data: shoebox_decode::nef::lens_data(&bytes),
            ..(**shot).clone()
        })
    });
    // For Auto (when preparing for Develop): the camera's own JPEG (a
    // DNG's preview).
    let camera_jpeg = long_edge.and_then(|_| {
        shoebox_decode::render(r.format, &bytes, Some(512), 1, shoebox_decode::Target::Srgb).ok()
    });
    drop(bytes);
    let mut raw = shoebox_decode::raw::decode_path(&source).map_err(|e| e.to_string())?;
    let src = cpu::demosaic(&raw, &r.raw, long_edge).with_lens(lens);
    // A linear DNG's AI denoise is in it already.
    if ai && raw.is_mosaic() && shoebox_develop::denoise::model().is_some() {
        match ai_denoised(&raw, &r.raw, &file_hash, &|_| {}, &AtomicBool::new(false)) {
            Ok(full) => {
                let img = match long_edge {
                    Some(l) => cpu::downscale(&full, l as usize),
                    None => full,
                };
                if (img.width, img.height) == (src.rgb.width, src.rgb.height) {
                    src.cache.set_ai(Arc::new(img));
                }
            }
            Err(e) => tracing::warn!("AI denoise of {}: {e}", r.path.display()),
        }
    }
    raw.data = Vec::new();
    let auto = long_edge.map(|_| shoebox_develop::auto::measure(&raw, &src, camera_jpeg.as_ref()));
    Ok(Some(Ready {
        id: r.id,
        raw: r.raw.clone(),
        meta: Arc::new(raw),
        src: Arc::new(src),
        upright: false,
        auto,
    }))
}

/// The file to decode a NEF's raw data from: itself, or its DNG (converted
/// now if need be). None if it needs the converter and there isn't one.
fn raw_source(
    r: &Request,
    bytes: &[u8],
    converter: Option<&std::path::Path>,
    dngs: &DngCache,
    emit: &dyn Fn(Event),
) -> Result<Option<PathBuf>, String> {
    if !shoebox_develop::dng::needs_dng(bytes) {
        return Ok(Some(r.path.clone()));
    }
    let Some(conv) = Converter::find(converter) else {
        return Ok(None);
    };
    let key = DngCache::key(bytes);
    Ok(Some(match dngs.get(&key) {
        Some(p) => p,
        None => {
            emit(Event::Converting(r.id));
            dngs.get_or_convert(&conv, &r.path, bytes)
                .map_err(|e| e.to_string())?
        }
    }))
}

/// AI raw denoise results on disk (set when the develop thread starts).
static AI_CACHE: std::sync::OnceLock<shoebox_develop::denoise::Cache> = std::sync::OnceLock::new();

/// The AI-denoised source at full size: kept on disk, or made (seconds).
fn ai_denoised(
    raw: &RawImage,
    settings: &Raw,
    file_hash: &str,
    progress: &dyn Fn(f32),
    cancel: &AtomicBool,
) -> Result<cpu::Rgb, String> {
    let key = shoebox_develop::denoise::key(file_hash, settings);
    if let Some(img) = AI_CACHE.get().and_then(|c| c.get(&key)) {
        return Ok(img);
    }
    let img = shoebox_develop::denoise::run(raw, settings, progress, cancel)?;
    if let Some(c) = AI_CACHE.get()
        && let Err(e) = c.put(&key, &img)
    {
        tracing::warn!("keeping the AI denoise result: {e}");
    }
    Ok(img)
}

/// The AI-denoised source of the photo open in Develop, at the proxy's
/// size.
fn ai_for_view(
    r: &Request,
    converter: Option<&std::path::Path>,
    dngs: &DngCache,
    emit: &dyn Fn(Event),
    cancel: &AtomicBool,
) -> Result<cpu::Rgb, String> {
    if !r.format.is_raw() {
        return Err("AI denoise works on raw files".into());
    }
    let bytes = std::fs::read(&r.path).map_err(|e| format!("{}: {e}", r.path.display()))?;
    let Some(source) = raw_source(r, &bytes, converter, dngs, &|_| {})? else {
        return Err("needs Adobe DNG Converter".into());
    };
    let file_hash = blake3::hash(&bytes).to_hex().to_string();
    drop(bytes);
    let raw = shoebox_decode::raw::decode_path(&source).map_err(|e| e.to_string())?;
    if !raw.is_mosaic() {
        return Err("this linear DNG is demosaiced and denoised already".into());
    }
    let full = ai_denoised(
        &raw,
        &r.raw,
        &file_hash,
        &|f| emit(Event::AiProgress(r.id, f)),
        cancel,
    )?;
    Ok(cpu::downscale(&full, PROXY as usize))
}

/// A finished image: decoded (colour-managed to Display P3, the widest the
/// decoder offers) at the proxy size, as a display-referred source.
fn finished(r: &Request, long_edge: Option<u32>) -> Result<Ready, String> {
    finished_image(r, long_edge).map(|mut ready| {
        ready.src = Arc::new((*ready.src).clone().with_lens(r.lens.clone()));
        ready
    })
}

fn finished_image(r: &Request, long_edge: Option<u32>) -> Result<Ready, String> {
    use shoebox_decode::Target;
    let data = std::fs::read(&r.path).map_err(|e| format!("{}: {e}", r.path.display()))?;
    // 16-bit TIFFs and PNGs keep their precision.
    let deep = shoebox_decode::rgb16::render16(r.format, &data, long_edge, 1, Target::DisplayP3)
        .map_err(|e| e.to_string())?;
    let src = match deep {
        Some(img) => cpu::from_display16(
            img.width as usize,
            img.height as usize,
            &img.px,
            cpu::Target::DisplayP3,
        ),
        None => {
            let img = shoebox_decode::render(r.format, &data, long_edge, 1, Target::DisplayP3)
                .map_err(|e| e.to_string())?;
            cpu::from_display(
                img.width as usize,
                img.height as usize,
                &img.px,
                cpu::Target::DisplayP3,
            )
        }
    };
    // Prepared smaller than the file (the proxy): detail radii and grain
    // are in the full image's pixels.
    let long = src.rgb.width.max(src.rgb.height) as f32;
    let src = match shoebox_decode::dimensions(r.format, &data) {
        Ok((w, h)) if w.max(h) > 0 => {
            let scale = (long / w.max(h) as f32).min(1.0);
            src.with_scale(scale)
        }
        _ => src,
    };
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
            linear: None,
        }),
        src: Arc::new(src),
        // ImageIO turned it upright.
        upright: r.format.is_platform(),
        auto: None,
    })
}

/// The export presets file, or the starters if there's none (or it can't
/// be read).
/// The edit presets' file, beside the export presets'.
fn looks_file(export_presets: &std::path::Path) -> PathBuf {
    export_presets.with_file_name("presets").join("user.json")
}

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
/// Where an item goes with a preset: the folder and file name (without
/// `-1`, `-2`), or why it can't go there.
/// Whether `folder` is inside one of the library's folders.
fn in_library(job: &ExportJob, folder: &std::path::Path) -> bool {
    let real = real_path(folder);
    job.roots.iter().any(|r| real.starts_with(real_path(r)))
}

fn destination(
    job: &ExportJob,
    item: &ExportItem,
    preset: &Preset,
    seq: usize,
) -> Result<(PathBuf, String), String> {
    use shoebox_develop::export::Destination;
    let folder = match preset.destination {
        // Beside the original.
        Destination::Library => item
            .path
            .parent()
            .map(PathBuf::from)
            .ok_or("the photo has no folder")?,
        Destination::Folder => match &preset.folder {
            Some(f) => f.clone(),
            None => default_folder(&preset.name).ok_or("no home folder")?,
        },
    };
    // Compared as real paths: /var is /private/var on macOS, and a symlink
    // mustn't sneak an export into a reference folder.
    let real = real_path(&folder);
    if job.reference.iter().any(|r| real.starts_with(real_path(r))) {
        return Err(format!(
            "{} is in a reference folder, which Shoebox never writes to",
            folder.display()
        ));
    }
    if preset.destination == Destination::Folder && in_library(job, &folder) {
        return Err(format!(
            "{} is in the library: to export into it, set the preset to go beside the original",
            folder.display()
        ));
    }
    let mut name = item
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(copy) = &item.copy_name {
        name = format!("{name}_{copy}");
    }
    let stem = shoebox_develop::export::file_stem(
        &preset.naming,
        &name,
        &preset.name,
        item.date.as_deref(),
        seq + 1,
    );
    Ok((folder, format!("{stem}.{}", preset.format.extension())))
}

fn run_export(
    job: &ExportJob,
    converter: Option<&std::path::Path>,
    dngs: &DngCache,
    staging: &std::path::Path,
    emit: &(dyn Fn(Event) + Sync),
) {
    // Before anything is written: files already there? Ask first.
    if job.on_clash.is_none() {
        let mut clashes = Vec::new();
        for (seq, item) in job.items.iter().enumerate() {
            for preset in &job.presets {
                if let Ok((folder, file)) = destination(job, item, preset, seq) {
                    // In the library nothing is replaced: no question.
                    let p = folder.join(file);
                    if p.exists() && !clashes.contains(&p) && !in_library(job, &folder) {
                        clashes.push(p);
                    }
                }
            }
        }
        if !clashes.is_empty() {
            emit(Event::ExportClashes {
                job: Box::new(job.clone()),
                files: clashes,
            });
            return;
        }
    }
    let total = job.items.len() * job.presets.len();
    let exiftool = exiftool();
    let names = plan_names(job);
    let progress = Mutex::new(ExportProgress::default());
    // Two photos at a time (the spec's two concurrent exports).
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..EXPORTS_AT_ONCE.min(job.items.len()) {
            scope.spawn(|| {
                loop {
                    let seq = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = job.items.get(seq) else {
                        return;
                    };
                    export_item(
                        job,
                        seq,
                        item,
                        &names,
                        staging,
                        converter,
                        dngs,
                        exiftool.as_deref(),
                        &progress,
                        total,
                        emit,
                    );
                }
            });
        }
    });
    let p = progress.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut written = p.written;
    written.sort_by_key(|(seq, _)| *seq);
    let mut into_library = p.into_library;
    into_library.sort_by_key(|(seq, _)| *seq);
    emit(Event::ExportDone {
        written: written.into_iter().map(|(_, w)| w).collect(),
        errors: p.errors,
        no_metadata: exiftool.is_none(),
        into_library: into_library.into_iter().map(|(_, c)| c).collect(),
    });
}

/// How many photos an export works on at once.
const EXPORTS_AT_ONCE: usize = 2;

/// An export so far, shared by its threads.
#[derive(Default)]
struct ExportProgress {
    done: usize,
    /// Files written, by the photo's place in the export.
    written: Vec<(usize, Exported)>,
    errors: Vec<String>,
    /// Files rendered for the library, waiting in the cache to be copied
    /// in, by the photo's place in the export.
    into_library: Vec<(usize, LibraryExport)>,
}

/// Where each photo goes with each preset, by (photo, preset): the
/// preset's name, or with `-1`, `-2` when an earlier photo in this export
/// has it (in the photos' order, however the threads finish), and whether
/// it's the first to claim it (Replace replaces only what was there before
/// this export). Errors (a folder inside the library) are kept for each.
type Names = std::collections::HashMap<(usize, usize), Result<(PathBuf, String, bool), String>>;

fn plan_names(job: &ExportJob) -> Names {
    let mut claimed: std::collections::HashSet<PathBuf> = Default::default();
    let mut out = Names::new();
    for (seq, item) in job.items.iter().enumerate() {
        for (k, preset) in job.presets.iter().enumerate() {
            let planned = destination(job, item, preset, seq).map(|(folder, file)| {
                let target = folder.join(&file);
                // A file there before this export is replaced only when
                // the person chose Replace. In the library, only the export
                // being redone (its old file goes to the Trash); anything
                // else gets a new name.
                let free = (job.on_clash == Some(Clash::Replace) && !in_library(job, &folder))
                    || item.replaces.as_ref() == Some(&target)
                    || !target.exists();
                if claimed.insert(target) && free {
                    return (folder, file, true);
                }
                let ext = preset.format.extension();
                let stem = file.strip_suffix(&format!(".{ext}")).unwrap_or(&file);
                let mut n = 1;
                loop {
                    let alt = format!("{stem}-{n}.{ext}");
                    let p = folder.join(&alt);
                    if !p.exists() && claimed.insert(p) {
                        return (folder, alt, false);
                    }
                    n += 1;
                }
            });
            out.insert((seq, k), planned);
        }
    }
    out
}

/// The font a text watermark uses when its preset names none: the app's
/// own text font.
fn watermark_font() -> &'static [u8] {
    static FONT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        eframe::egui::FontDefinitions::default()
            .font_data
            .get("Ubuntu-Light")
            .map(|f| (*f.font).as_ref().to_vec())
            .unwrap_or_default()
    })
}

/// Export one photo with each preset.
#[allow(clippy::too_many_arguments)]
fn export_item(
    job: &ExportJob,
    seq: usize,
    item: &ExportItem,
    names: &Names,
    staging: &std::path::Path,
    converter: Option<&std::path::Path>,
    dngs: &DngCache,
    exiftool: Option<&std::path::Path>,
    progress: &Mutex<ExportProgress>,
    total: usize,
    emit: &(dyn Fn(Event) + Sync),
) {
    let lock = || progress.lock().unwrap_or_else(|e| e.into_inner());
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
        lens: item.lens.clone(),
    };
    let ai = cpu::layers::wants_ai(&item.settings);
    let failed = match prepare(&r, None, converter, dngs, &|_| {}, ai) {
        Ok(Some(ready)) => {
            for (k, preset) in job.presets.iter().enumerate() {
                let result = (|| -> Result<PathBuf, String> {
                    let (folder, file, first) = names
                        .get(&(seq, k))
                        .cloned()
                        .unwrap_or_else(|| Err("not planned".into()))?;
                    let o = cpu::Options {
                        long_edge: None,
                        orientation: if ready.upright { 1 } else { item.orientation },
                        target: preset.space.target(),
                    };
                    let bytes = if preset.format == shoebox_develop::export::FileFormat::LinearDng {
                        // The source itself, not a render of the edit.
                        shoebox_develop::export::linear_dng(
                            &ready.meta,
                            &ready.src,
                            &item.settings,
                            o.orientation,
                        )?
                    } else {
                        let frame = cpu::frame(&ready.meta, &ready.src, &item.settings, &o);
                        let img = cpu::apply_from(&ready.src, &item.settings, &frame, true);
                        let img = shoebox_develop::export::finish(&img, preset, watermark_font())?;
                        shoebox_develop::export::encode(&img, preset)?
                    };
                    let ext = preset.format.extension();
                    let stem = file.strip_suffix(&format!(".{ext}")).unwrap_or(&file);
                    if in_library(job, &folder) {
                        // Written in the cache; the library worker copies it
                        // in as a journaled batch that never replaces.
                        std::fs::create_dir_all(staging)
                            .map_err(|e| format!("{}: {e}", staging.display()))?;
                        let staged =
                            shoebox_develop::export::write_new(staging, stem, ext, &bytes)?;
                        if let Some(tool) = exiftool {
                            copy_metadata(tool, item, preset, &staged);
                        }
                        let hash = std::fs::read(&staged)
                            .map(|b| *blake3::hash(&b).as_bytes())
                            .map_err(|e| format!("{}: {e}", staged.display()))?;
                        let dst = folder.join(&file);
                        lock().into_library.push((
                            seq,
                            LibraryExport {
                                copy: shoebox_core::fileops::PlannedCopy {
                                    src: staged,
                                    dst: dst.clone(),
                                    hash,
                                },
                                photo: item.path.clone(),
                                replaces: item.replaces.clone().filter(|r| *r == dst),
                            },
                        ));
                        return Ok(dst);
                    }
                    // Replace only what was there before this export: two
                    // photos landing on one name keep both.
                    let out = if job.on_clash == Some(Clash::Replace) && first {
                        shoebox_develop::export::write_replacing(&folder, stem, ext, &bytes)?
                    } else {
                        shoebox_develop::export::write_new(&folder, stem, ext, &bytes)?
                    };
                    if let Some(tool) = exiftool {
                        copy_metadata(tool, item, preset, &out);
                    }
                    Ok(out)
                })();
                let mut p = lock();
                p.done += 1;
                let last = match result {
                    Ok(file) => {
                        p.written.push((
                            seq,
                            Exported {
                                photo: item.path.clone(),
                                copy: item.copy.clone(),
                                file: file.clone(),
                                preset: preset.clone(),
                                edit_key: item.edit_key.clone(),
                                tracked: preset.destination
                                    == shoebox_develop::export::Destination::Library,
                            },
                        ));
                        Some(file)
                    }
                    Err(e) => {
                        p.errors.push(format!("{name} ({}): {e}", preset.name));
                        None
                    }
                };
                emit(Event::ExportProgress {
                    done: p.done,
                    total,
                    last,
                });
            }
            return;
        }
        Ok(None) => format!("{name}: needs Adobe DNG Converter"),
        Err(e) => format!("{name}: {e}"),
    };
    let mut p = lock();
    p.errors.push(failed);
    p.done += job.presets.len();
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

/// What "all but location" copies: an allowlist (camera, exposure, dates,
/// author, copyright, caption, keywords, rating, label), so no location
/// field of any format, known or not, can come through.
const NOT_LOCATION: [&str; 37] = [
    "-EXIF:Make",
    "-EXIF:Model",
    "-EXIF:LensMake",
    "-EXIF:LensModel",
    "-EXIF:LensInfo",
    "-EXIF:ExposureTime",
    "-EXIF:FNumber",
    "-EXIF:ISO",
    "-EXIF:ExposureProgram",
    "-EXIF:ExposureCompensation",
    "-EXIF:MeteringMode",
    "-EXIF:Flash",
    "-EXIF:WhiteBalance",
    "-EXIF:FocalLength",
    "-EXIF:FocalLengthIn35mmFormat",
    "-EXIF:DateTimeOriginal",
    "-EXIF:CreateDate",
    "-EXIF:OffsetTimeOriginal",
    "-EXIF:SubSecTimeOriginal",
    "-EXIF:Artist",
    "-EXIF:Copyright",
    "-EXIF:ImageDescription",
    "-XMP-dc:Title",
    "-XMP-dc:Description",
    "-XMP-dc:Subject",
    "-XMP-dc:Creator",
    "-XMP-dc:Rights",
    "-XMP-xmp:Rating",
    "-XMP-xmp:Label",
    "-XMP-lr:HierarchicalSubject",
    "-IPTC:Keywords",
    "-IPTC:ObjectName",
    "-IPTC:Caption-Abstract",
    "-IPTC:By-line",
    "-IPTC:CopyrightNotice",
    "-IPTC:Headline",
    "-XMP-photoshop:Headline",
];

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
        Metadata::AllButLocation => {
            cmd.arg("-TagsFromFile").arg(&item.path).args(NOT_LOCATION);
            if let Some(s) = &item.sidecar {
                cmd.arg("-TagsFromFile").arg(s).args(NOT_LOCATION);
            }
        }
        Metadata::All => {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Five photos, two at a time: every file written once, two with the
    /// same name kept apart, listed in the photos' order, and progress
    /// counts up to the total.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn exports_two_photos_at_a_time() {
        let t = tempfile::tempdir().unwrap();
        let mut items = Vec::new();
        for (i, rel) in [
            "a/one.jpg",
            "b/two.jpg",
            "c/three.jpg",
            "d/one.jpg",
            "e/five.jpg",
        ]
        .iter()
        .enumerate()
        {
            let path = t.path().join("lib").join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let img = shoebox_fixtures::scene(i as u64 + 1, 96, 64);
            std::fs::write(&path, shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
            items.push(ExportItem {
                id: i as u64,
                path,
                format: shoebox_decode::Format::Jpeg,
                orientation: 1,
                settings: Default::default(),
                edit_key: None,
                date: None,
                sidecar: None,
                lens: None,
                copy_name: None,
                copy: shoebox_core::edits::MAIN.to_owned(),
                replaces: None,
            });
        }
        let out = t.path().join("out");
        let preset = Preset {
            folder: Some(out.clone()),
            naming: "{name}".into(),
            metadata: shoebox_develop::export::Metadata::CopyrightOnly,
            ..shoebox_develop::export::starters()[0].clone()
        };
        let job = ExportJob {
            items,
            presets: vec![preset],
            roots: vec![t.path().join("lib")],
            reference: Vec::new(),
            on_clash: Some(Clash::NewFile),
        };
        let events = Mutex::new(Vec::new());
        let dngs = DngCache::new(&t.path().join("cache"), 1 << 30);
        run_export(&job, None, &dngs, &t.path().join("staging"), &|e| {
            events.lock().unwrap().push(e)
        });
        let events = events.into_inner().unwrap();
        let mut done: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                Event::ExportProgress { done, total: 5, .. } => Some(*done),
                _ => None,
            })
            .collect();
        done.sort_unstable();
        assert_eq!(done, [1, 2, 3, 4, 5]);
        let Some(Event::ExportDone {
            written, errors, ..
        }) = events.last()
        else {
            panic!("no ExportDone");
        };
        assert!(errors.is_empty(), "{errors:?}");
        let photos: Vec<_> = written.iter().map(|w| w.photo.clone()).collect();
        let want: Vec<_> = job.items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(photos, want);
        let names: Vec<_> = written
            .iter()
            .map(|w| w.file.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["one.jpg", "two.jpg", "three.jpg", "one-1.jpg", "five.jpg"]
        );
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 5);

        // Again, as new files: each takes the next free name.
        let names = plan_names(&job);
        assert_eq!(names[&(0, 0)].as_ref().unwrap().1, "one-2.jpg");
        assert_eq!(names[&(3, 0)].as_ref().unwrap().1, "one-3.jpg");
        run_export(&job, None, &dngs, &t.path().join("staging"), &|_| {});
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 10);
        assert!(out.join("one-3.jpg").exists() && out.join("two-1.jpg").exists());

        // Into a reference folder: refused, for every photo.
        let refused = ExportJob {
            reference: vec![t.path().to_path_buf()],
            ..job.clone()
        };
        assert!(
            plan_names(&refused)
                .values()
                .all(|r| r.as_ref().is_err_and(|e| e.contains("reference folder")))
        );
    }

    /// AVIF and linear DNG exports: written outside the library under
    /// names that never replace a file, or beside the original (staged for
    /// the library worker, which adds them to the photo's stack), a DNG
    /// only from a raw photo, and a linear DNG in the library exported again
    /// beside itself.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn avif_and_dng_exports() {
        use shoebox_develop::export::{Destination, FileFormat};
        let t = tempfile::tempdir().unwrap();
        let item = |id: u64, path: PathBuf, format| ExportItem {
            id,
            path,
            format,
            orientation: 1,
            settings: Default::default(),
            edit_key: None,
            date: None,
            sidecar: None,
            lens: None,
            copy_name: None,
            copy: shoebox_core::edits::MAIN.to_owned(),
            replaces: None,
        };
        let lib = t.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let jpeg = lib.join("one.jpg");
        let img = shoebox_fixtures::scene(1, 96, 64);
        std::fs::write(&jpeg, shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
        // A raw photo (a DNG under a NEF's name goes the same way).
        let raw = lib.join("two.nef");
        let dng = shoebox_fixtures::Dng::new(96, 64);
        std::fs::write(&raw, dng.write(|x, y| 2000 + ((x + y) % 7) as u16 * 300)).unwrap();
        let out = t.path().join("out");
        let preset = |format| Preset {
            name: "p".into(),
            format,
            folder: Some(out.clone()),
            naming: "{name}".into(),
            metadata: shoebox_develop::export::Metadata::CopyrightOnly,
            ..shoebox_develop::export::starters()[0].clone()
        };
        let avif = preset(FileFormat::Avif {
            quality: 80,
            ten_bit: true,
            speed: 10,
        });
        let job = ExportJob {
            items: vec![
                item(0, jpeg.clone(), shoebox_decode::Format::Jpeg),
                item(1, raw.clone(), shoebox_decode::Format::Nef),
            ],
            presets: vec![avif.clone(), preset(FileFormat::LinearDng)],
            roots: vec![lib.clone()],
            reference: Vec::new(),
            on_clash: Some(Clash::NewFile),
        };
        let dngs = DngCache::new(&t.path().join("cache"), 1 << 30);
        let run = |job: &ExportJob| {
            let events = Mutex::new(Vec::new());
            run_export(job, None, &dngs, &t.path().join("staging"), &|e| {
                events.lock().unwrap().push(e)
            });
            match events.into_inner().unwrap().pop() {
                Some(Event::ExportDone {
                    written,
                    errors,
                    into_library,
                    ..
                }) => (written, errors, into_library),
                _ => panic!("no ExportDone"),
            }
        };
        let (written, errors, _) = run(&job);
        let names: Vec<_> = written
            .iter()
            .map(|w| w.file.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["one.avif", "two.avif", "two.dng"]);
        // A JPEG has no raw data to archive.
        assert!(
            errors.len() == 1 && errors[0].contains("raw photo"),
            "{errors:?}"
        );
        assert_eq!(
            &std::fs::read(out.join("one.avif")).unwrap()[4..12],
            b"ftypavif"
        );
        assert_eq!(&std::fs::read(out.join("two.dng")).unwrap()[..4], b"II*\0");
        // Again: new names, nothing replaced.
        let before = std::fs::read(out.join("two.dng")).unwrap();
        let (written, _, _) = run(&job);
        let names: Vec<_> = written
            .iter()
            .map(|w| w.file.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["one-1.avif", "two-1.avif", "two-1.dng"]);
        assert_eq!(std::fs::read(out.join("two.dng")).unwrap(), before);
        // A folder in the library: refused, as for every format.
        let refused = ExportJob {
            presets: vec![Preset {
                folder: Some(lib.join("exports")),
                ..preset(FileFormat::LinearDng)
            }],
            ..job.clone()
        };
        assert!(
            plan_names(&refused)
                .values()
                .all(|r| r.as_ref().is_err_and(|e| e.contains("library"))),
            "{:?}",
            plan_names(&refused)
        );
        // Beside the original: the NEF, and a linear DNG already in the
        // library (opened past demosaic), whose name is taken by itself.
        let three = lib.join("three.dng");
        std::fs::write(&three, &before).unwrap();
        let beside = |p: Preset| Preset {
            destination: Destination::Library,
            ..p
        };
        let job = ExportJob {
            items: vec![
                item(1, raw.clone(), shoebox_decode::Format::Nef),
                item(2, three.clone(), shoebox_decode::Format::Dng),
            ],
            presets: vec![beside(avif.clone()), beside(preset(FileFormat::LinearDng))],
            ..job.clone()
        };
        let (written, errors, staged) = run(&job);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(written.iter().all(|w| w.tracked));
        let mut dsts: Vec<_> = staged
            .iter()
            .map(|e| {
                e.copy
                    .dst
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        dsts.sort();
        assert_eq!(dsts, ["three-1.dng", "three.avif", "two.avif", "two.dng"]);
        for e in &staged {
            // In the cache until the library worker copies it in; nothing
            // in the library yet, and nothing replaced.
            assert!(e.copy.src.is_file() && !e.copy.dst.exists() && e.replaces.is_none());
            assert_eq!(e.copy.dst.parent(), Some(lib.as_path()));
            let bytes = std::fs::read(&e.copy.src).unwrap();
            assert_eq!(*blake3::hash(&bytes).as_bytes(), e.copy.hash);
            if e.copy.dst.extension().is_some_and(|x| x == "dng") {
                let back = shoebox_decode::raw::decode(&bytes).unwrap();
                assert!(back.linear.is_some());
            } else {
                assert_eq!(&bytes[4..12], b"ftypavif");
            }
        }
        assert_eq!(std::fs::read(&three).unwrap(), before);
    }

    /// "All but location" leaves nothing that says where: coordinates or
    /// place names, in EXIF, IPTC or XMP. Needs exiftool (skipped without).
    #[test]
    #[allow(clippy::unwrap_used)]
    fn all_but_location_strips_every_location_tag() {
        let Some(tool) = exiftool() else {
            eprintln!("skipping: no exiftool");
            return;
        };
        let t = tempfile::tempdir().unwrap();
        let img = shoebox_fixtures::scene(1, 64, 48);
        let src = t.path().join("src.jpg");
        std::fs::write(&src, shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
        let tag = |args: &[&str], file: &std::path::Path| {
            let o = std::process::Command::new(&tool)
                .args(["-q", "-overwrite_original"])
                .args(args)
                .arg(file)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        tag(
            &[
                "-GPSLatitude=35.0",
                "-GPSLatitudeRef=N",
                "-GPSLongitude=135.7",
                "-GPSLongitudeRef=E",
                "-IPTC:City=Kyoto",
                "-IPTC:Country-PrimaryLocationName=Japan",
                "-XMP-photoshop:City=Kyoto",
                "-XMP-photoshop:Country=Japan",
                "-XMP-iptcCore:Location=Gion",
                "-XMP-iptcExt:LocationShownCity=Kyoto",
                "-IPTC:ContentLocationName=Kyoto",
                "-XMP-dc:Coverage=Japan",
                "-XMP-iptcExt:LocationCreatedCity=Kyoto",
                "-XMP-exif:GPSLatitude=35.0",
                "-Make=NIKON CORPORATION",
                "-ExposureTime=1/30",
                "-Artist=Someone",
                "-Copyright=Pink Octopus",
            ],
            &src,
        );
        let out = t.path().join("out.jpg");
        std::fs::write(&out, shoebox_fixtures::jpeg(&img, 90).unwrap()).unwrap();
        let item = ExportItem {
            id: 1,
            path: src,
            format: shoebox_decode::Format::Jpeg,
            orientation: 1,
            settings: Default::default(),
            edit_key: None,
            date: None,
            sidecar: None,
            lens: None,
            copy_name: None,
            copy: shoebox_core::edits::MAIN.to_owned(),
            replaces: None,
        };
        let preset = Preset {
            metadata: shoebox_develop::export::Metadata::AllButLocation,
            ..shoebox_develop::export::starters()[0].clone()
        };
        copy_metadata(&tool, &item, &preset, &out);
        let dump = std::process::Command::new(&tool)
            .args(["-a", "-G1", "-s"])
            .arg(&out)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&dump.stdout);
        for word in ["Kyoto", "Japan", "Gion", "GPS"] {
            assert!(!text.contains(word), "{word} survived:\n{text}");
        }
        // The rest came across.
        assert!(
            text.contains("Pink Octopus") && text.contains("Someone"),
            "{text}"
        );
        assert!(
            text.contains("NIKON CORPORATION") && text.contains("1/30"),
            "{text}"
        );
    }
}
