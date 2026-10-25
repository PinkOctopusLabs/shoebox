//! Semantic search: a background thread that downloads the model (once, when
//! the setting is turned on), embeds each stack's primary image, and answers
//! text and "find similar" queries between batches.
//!
//! It only reads library files, through the same decoder as thumbnails; its
//! own state is `embeddings.sqlite` and the model folder.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use rayon::prelude::*;
use shoebox_core::catalog::Catalog;
use shoebox_core::embeddings::{FileKey, Index, Store, Vector};
use shoebox_decode::{Format, Rgb8, Target};

/// A text query matches at this cosine similarity and above: where SigLIP's
/// own match probability reaches about 1 in 10,000. Unrelated photos score
/// below 0.06 (measured; see docs/DECISIONS.md).
pub const TEXT_MIN: f32 = 0.07;
/// "Find similar": unrelated photos score 0.45–0.75, the same frame ~0.99.
pub const IMAGE_MIN: f32 = 0.75;
/// Images per model call.
const BATCH: usize = 16;
/// Unload the text model after this long without a query.
const TEXT_IDLE: Duration = Duration::from_secs(600);

/// Something that turns images and text into comparable unit vectors.
pub trait Model: Send {
    fn embed_images(&mut self, images: &[Rgb8]) -> Result<Vec<Vec<f32>>, String>;
    fn embed_text(&mut self, text: &str) -> Result<Vec<f32>, String>;
    /// Free what the image side holds (indexing is done for now).
    fn release_images(&mut self) {}
    /// Free what the text side holds (no query for a while).
    fn release_text(&mut self) {}
    /// The tags it can suggest.
    fn labels(&self) -> Vec<shoebox_ml::Label> {
        shoebox_ml::labels()
    }
}

/// When a photo gets a suggested tag (measured on sample photos; see
/// docs/DECISIONS.md and [`shoebox_core::embeddings::TagRule`]): a score of
/// 0.09 (where real matches start), a lift of 0.045, within 0.03 of the
/// photo's best label; at most 5.
fn tag_rule() -> shoebox_core::embeddings::TagRule {
    shoebox_core::embeddings::TagRule {
        min_score: 0.09,
        min_lift: 0.045,
        margin: 0.03,
        max: 5,
        min_library: 8,
    }
}

/// Re-tag after this many more photos are indexed (and when indexing ends).
const RETAG_EVERY: usize = 2000;

/// Suggested tags for the library.
#[derive(Debug, Default)]
pub struct Tags {
    /// Tag names (keywords they'd become), by label index.
    pub names: Vec<String>,
    /// Each photo's tags (label, score), best first, by primary photo.
    pub by_photo: std::collections::HashMap<crate::jobs::PhotoId, Vec<(u16, f32)>>,
    /// Labels with photos, most photos first.
    pub counts: Vec<(u16, usize)>,
}

/// Where the model comes from.
#[derive(Clone)]
pub enum ModelSource {
    /// SigLIP 2, downloaded into `dir` when first turned on (by way of
    /// `staging`, in the cache directory).
    Download { dir: PathBuf, staging: PathBuf },
    /// A ready model (tests).
    Fixed(fn() -> Box<dyn Model>),
}

/// SigLIP 2 through `shoebox-ml`, loading each half when it's needed.
struct Siglip {
    dir: PathBuf,
    device: shoebox_ml::Device,
    image: Option<shoebox_ml::ImageEncoder>,
    text: Option<shoebox_ml::TextEncoder>,
}

impl Model for Siglip {
    fn embed_images(&mut self, images: &[Rgb8]) -> Result<Vec<Vec<f32>>, String> {
        if self.image.is_none() {
            let t = Instant::now();
            self.image = Some(
                shoebox_ml::ImageEncoder::load(&self.dir, &self.device)
                    .map_err(|e| e.to_string())?,
            );
            tracing::info!("image model loaded in {:?}", t.elapsed());
        }
        let enc = self.image.as_ref().ok_or("image model not loaded")?;
        let inputs: Vec<Vec<f32>> = images.iter().map(shoebox_ml::preprocess).collect();
        enc.embed(&inputs).map_err(|e| e.to_string())
    }

    fn embed_text(&mut self, text: &str) -> Result<Vec<f32>, String> {
        if self.text.is_none() {
            let t = Instant::now();
            self.text = Some(
                shoebox_ml::TextEncoder::load(&self.dir, &self.device)
                    .map_err(|e| e.to_string())?,
            );
            tracing::info!("text model loaded in {:?}", t.elapsed());
        }
        let enc = self.text.as_ref().ok_or("text model not loaded")?;
        enc.embed(text).map_err(|e| e.to_string())
    }

    fn release_images(&mut self) {
        self.image = None;
    }

    fn release_text(&mut self) {
        self.text = None;
    }
}

/// The model's state, for the settings window and toolbar.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ModelState {
    /// Semantic search is off.
    #[default]
    Off,
    Downloading {
        done: u64,
        total: u64,
    },
    Verifying,
    Converting,
    Ready,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    pub model: ModelState,
    /// Photos with an embedding, of all photos (stacks) in the library.
    pub indexed: usize,
    pub total: usize,
    /// Photos that couldn't be read or embedded (skipped until the next
    /// library change).
    pub skipped: usize,
}

impl Status {
    pub fn indexing(&self) -> bool {
        self.model == ModelState::Ready && self.indexed + self.skipped < self.total
    }

    pub fn ready(&self) -> bool {
        self.model == ModelState::Ready
    }
}

pub enum Command {
    /// Semantic search on or off (from settings).
    Enable(bool),
    /// The library changed; `complete` once a scan has finished.
    Catalog(Arc<Catalog>, bool),
    /// Photos matching a description, best first.
    Text { id: u64, text: String },
    /// Photos that look like this one.
    Similar { id: u64, path: PathBuf },
    /// Stop a download in progress.
    CancelDownload,
}

pub enum Event {
    Status(Status),
    /// Results of query `id`: (primary path, score), best first.
    Results {
        id: u64,
        hits: Vec<(PathBuf, f32)>,
    },
    /// A query failed (e.g. the photo isn't indexed yet).
    Failed {
        id: u64,
        error: String,
    },
    /// Suggested tags for the library (after indexing).
    Tags(Arc<Tags>),
}

pub struct Semantic {
    tx: Sender<Command>,
    pub events: Receiver<Event>,
}

impl Semantic {
    pub fn new(
        source: ModelSource,
        db: PathBuf,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (etx, events) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("semantic".into())
            .spawn(move || {
                let notify = move |e: Event| {
                    let _ = etx.send(e);
                    repaint();
                };
                Worker::new(source, db, notify).run(&rx);
            });
        if let Err(e) = spawned {
            tracing::error!("could not start the semantic search thread: {e}");
        }
        Self { tx, events }
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

/// One photo to analyse.
pub(crate) struct Item {
    pub key: FileKey,
    pub format: Format,
    pub orientation: u16,
}

impl Item {
    /// The photo, upright, long edge at most `long` (a NEF's preview).
    pub fn decode(&self, long: u32) -> Option<Rgb8> {
        shoebox_decode::render_path(
            self.format,
            &self.key.path,
            Some(long),
            self.orientation,
            Target::Srgb,
        )
        .ok()
    }
}

/// Each stack's primary: what's analysed. Also the format and orientation
/// of those that can be read now (not in the cloud only: reading one would
/// download it; its row, if any, is kept).
pub(crate) fn primaries(
    catalog: &Catalog,
) -> (
    Vec<FileKey>,
    std::collections::HashMap<PathBuf, (Format, u16)>,
) {
    let mut keys = Vec::with_capacity(catalog.stacks.len());
    let mut formats = std::collections::HashMap::with_capacity(catalog.stacks.len());
    for st in &catalog.stacks {
        let p = &catalog.photos[st.primary];
        keys.push(FileKey {
            path: p.path.clone(),
            size: p.size,
            mtime_ns: p.mtime_ns,
        });
        if !p.cloud {
            formats.insert(p.path.clone(), (p.format, p.meta.orientation()));
        }
    }
    (keys, formats)
}

/// A model download running on its own thread.
pub(crate) struct Download {
    pub progress: Receiver<shoebox_ml::download::Stage>,
    pub done: Receiver<Result<(), String>>,
    pub cancel: Arc<AtomicBool>,
}

impl Download {
    /// The model state a progress message means.
    pub fn state(spec: &shoebox_ml::download::Spec, s: shoebox_ml::download::Stage) -> ModelState {
        use shoebox_ml::download::Stage;
        match s {
            Stage::Downloading { done } => ModelState::Downloading {
                done,
                total: spec.bytes(),
            },
            Stage::Verifying => ModelState::Verifying,
            Stage::Converting => ModelState::Converting,
        }
    }
}

struct Worker<F: Fn(Event)> {
    source: ModelSource,
    db_path: PathBuf,
    notify: F,
    enabled: bool,
    status: Status,
    store: Option<Store>,
    model: Option<Box<dyn Model>>,
    download: Option<Download>,
    index: Index,
    /// The library's primaries as last synced.
    keys: Vec<FileKey>,
    /// Of those, how many could be read (not in the cloud only).
    readable: usize,
    formats: std::collections::HashMap<PathBuf, (Format, u16)>,
    pending: std::collections::VecDeque<Item>,
    /// Latest catalog not yet synced (synced once the model is ready).
    catalog: Option<(Arc<Catalog>, bool)>,
    text_used: Option<Instant>,
    pool: Option<rayon::ThreadPool>,
    /// Label vectors (computed once per model, then stored).
    labels: Option<(Vec<String>, Vec<Vector>)>,
    /// Index size when last tagged (None: needs tagging).
    tagged: Option<usize>,
}

impl<F: Fn(Event)> Worker<F> {
    fn new(source: ModelSource, db_path: PathBuf, notify: F) -> Self {
        Self {
            source,
            db_path,
            notify,
            enabled: false,
            status: Status::default(),
            store: None,
            model: None,
            download: None,
            index: Index::default(),
            keys: Vec::new(),
            readable: 0,
            formats: Default::default(),
            pending: Default::default(),
            catalog: None,
            text_used: None,
            labels: None,
            tagged: None,
            // Decoding shares the machine with thumbnails: a few threads.
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(4)
                .thread_name(|i| format!("semantic-decode-{i}"))
                .build()
                .ok(),
        }
    }

    fn publish(&self) {
        (self.notify)(Event::Status(self.status.clone()));
    }

    fn run(mut self, rx: &Receiver<Command>) {
        loop {
            let busy = self.enabled && self.model.is_some() && !self.pending.is_empty();
            let wait = if busy {
                Duration::ZERO
            } else {
                Duration::from_secs(60)
            };
            let never = crossbeam_channel::never();
            let (progress, done) = match &self.download {
                Some(d) => (&d.progress, &d.done),
                None => (&never, &crossbeam_channel::never()),
            };
            select! {
                recv(rx) -> c => match c {
                    Ok(c) => self.command(c),
                    Err(_) => return, // the app is gone
                },
                recv(progress) -> s => if let Ok(s) = s {
                    self.status.model = Download::state(&shoebox_ml::download::SIGLIP, s);
                    self.publish();
                },
                recv(done) -> r => {
                    self.download = None;
                    match r {
                        Ok(Ok(())) => self.model_ready(),
                        Ok(Err(e)) => {
                            self.status.model = if self.enabled { ModelState::Failed(e) } else { ModelState::Off };
                            self.publish();
                        }
                        Err(_) => {}
                    }
                },
                default(wait) => {
                    if busy {
                        self.index_batch();
                    } else {
                        self.idle();
                    }
                }
            }
        }
    }

    fn idle(&mut self) {
        if let Some(m) = &mut self.model {
            m.release_images();
            if self.text_used.is_some_and(|t| t.elapsed() > TEXT_IDLE) {
                m.release_text();
                self.text_used = None;
            }
        }
    }

    fn command(&mut self, c: Command) {
        match c {
            Command::Enable(on) => {
                if on == self.enabled {
                    return;
                }
                self.enabled = on;
                if on {
                    self.start();
                } else {
                    if let Some(d) = &self.download {
                        d.cancel.store(true, Ordering::Relaxed);
                    }
                    self.model = None;
                    self.status.model = ModelState::Off;
                    self.publish();
                }
            }
            Command::Catalog(c, complete) => {
                self.catalog = Some((c, complete));
                if self.model.is_some() {
                    self.sync();
                }
            }
            Command::CancelDownload => {
                if let Some(d) = &self.download {
                    d.cancel.store(true, Ordering::Relaxed);
                }
            }
            Command::Text { id, text } => {
                let r = self.text_query(&text);
                self.answer(id, r);
            }
            Command::Similar { id, path } => {
                let r = match self.index.get(&path) {
                    Some(v) => Ok(self.index.search(v, IMAGE_MIN)),
                    None => Err("This photo isn't indexed yet.".to_owned()),
                };
                self.answer(id, r);
            }
        }
    }

    fn answer(&self, id: u64, r: Result<Vec<(PathBuf, f32)>, String>) {
        (self.notify)(match r {
            Ok(hits) => Event::Results { id, hits },
            Err(error) => Event::Failed { id, error },
        });
    }

    fn text_query(&mut self, text: &str) -> Result<Vec<(PathBuf, f32)>, String> {
        let model = self
            .model
            .as_mut()
            .ok_or("Semantic search isn't ready yet.")?;
        let t = Instant::now();
        let v = model.embed_text(text)?;
        self.text_used = Some(Instant::now());
        let hits = self.index.search(&Vector::quantise(&v), TEXT_MIN);
        tracing::debug!("\"{text}\": {} hits in {:?}", hits.len(), t.elapsed());
        Ok(hits)
    }

    /// Turned on: open the store, then get the model ready.
    fn start(&mut self) {
        if self.store.is_none() {
            match Store::open(&self.db_path, "image", shoebox_ml::MODEL_ID) {
                Ok(s) => self.store = Some(s),
                Err(e) => {
                    self.status.model = ModelState::Failed(e.to_string());
                    self.publish();
                    return;
                }
            }
        }
        match self.source.clone() {
            ModelSource::Fixed(make) => {
                self.model = Some(make());
                self.model_ready();
            }
            ModelSource::Download { dir, staging } => {
                if shoebox_ml::download::SIGLIP.is_ready(&dir) {
                    self.model = Some(siglip(dir));
                    self.model_ready();
                } else if self.download.is_none() {
                    self.status.model = ModelState::Downloading {
                        done: 0,
                        total: shoebox_ml::download::SIGLIP.bytes(),
                    };
                    self.publish();
                    self.download =
                        Some(start_download(&shoebox_ml::download::SIGLIP, dir, staging));
                }
            }
        }
    }

    fn model_ready(&mut self) {
        if !self.enabled {
            return;
        }
        if self.model.is_none()
            && let ModelSource::Download { dir, .. } = &self.source
        {
            self.model = Some(siglip(dir.clone()));
        }
        self.status.model = ModelState::Ready;
        self.keys.clear(); // force a sync
        self.sync();
        self.publish();
    }

    /// Match the store to the latest catalog and queue what's missing.
    fn sync(&mut self) {
        let Some((catalog, complete)) = self.catalog.clone() else {
            return;
        };
        let (keys, formats) = primaries(&catalog);
        // Marks and other changes republish the catalog; only the files
        // (and which are here, not in the cloud only) matter.
        if keys == self.keys && formats.len() == self.readable {
            return;
        }
        let Some(store) = &mut self.store else {
            return;
        };
        let t = Instant::now();
        match store.sync(&keys, complete) {
            Ok(r) => {
                tracing::info!(
                    "embeddings: {} current, {} to compute, {} followed, {} removed ({:?})",
                    r.found.len(),
                    r.missing.len(),
                    r.followed,
                    r.removed,
                    t.elapsed()
                );
                self.index = Index::from_rows(&r.found);
                self.pending = r
                    .missing
                    .into_iter()
                    .filter_map(|key| {
                        let &(format, orientation) = formats.get(&key.path)?;
                        Some(Item {
                            key,
                            format,
                            orientation,
                        })
                    })
                    .collect();
                // Newest first: most likely what you'll search for.
                self.pending.make_contiguous().reverse();
                self.status.indexed = self.index.len();
                // Cloud-only photos not indexed yet wait for their download.
                self.status.total = self.index.len() + self.pending.len();
                self.status.skipped = 0;
                self.readable = formats.len();
                self.keys = keys;
                self.formats = formats;
                // Tagging takes seconds on a big library, and the catalog
                // is republished every few seconds while a cloud drive sends
                // files: only moves and removals make it due at once, or it
                // would run again and again with no time left to index.
                if r.followed > 0 || r.removed > 0 {
                    self.tagged = None;
                }
            }
            Err(e) => self.status.model = ModelState::Failed(e.to_string()),
        }
        self.publish();
        self.maybe_tag();
    }

    /// Re-tag the library when indexing has finished, or every few thousand
    /// photos while it runs.
    fn maybe_tag(&mut self) {
        let n = self.index.len();
        let due = match self.tagged {
            None => self.pending.is_empty() || n >= RETAG_EVERY,
            Some(t) => (self.pending.is_empty() && t != n) || n >= t + RETAG_EVERY,
        };
        if !due || n == 0 {
            return;
        }
        let Some(model) = &mut self.model else { return };
        if self.labels.is_none() {
            let labels = model.labels();
            let prompts: Vec<String> = labels.iter().map(|l| l.prompt.clone()).collect();
            let stored = self
                .store
                .as_ref()
                .and_then(|s| s.labels(&prompts).ok().flatten());
            let vecs = match stored {
                Some(v) => v,
                None => {
                    let t = Instant::now();
                    let mut v = Vec::with_capacity(prompts.len());
                    for p in &prompts {
                        match model.embed_text(p) {
                            Ok(e) => v.push(Vector::quantise(&e)),
                            Err(e) => {
                                tracing::warn!("tag \"{p}\": {e}");
                                return;
                            }
                        }
                    }
                    self.text_used = Some(Instant::now());
                    tracing::info!("{} tag labels embedded in {:?}", v.len(), t.elapsed());
                    if let Some(s) = &mut self.store {
                        let rows: Vec<(String, Vector)> =
                            prompts.iter().cloned().zip(v.iter().cloned()).collect();
                        if let Err(e) = s.put_labels(&rows) {
                            tracing::warn!("tag labels: {e}");
                        }
                    }
                    v
                }
            };
            let names = labels.iter().map(|l| l.name.to_owned()).collect();
            self.labels = Some((names, vecs));
        }
        let Some((names, vecs)) = &self.labels else {
            return;
        };
        let t = Instant::now();
        let tagged = self.index.tag(vecs, &tag_rule());
        let mut counts = vec![0usize; names.len()];
        let by_photo: std::collections::HashMap<_, _> = tagged
            .into_iter()
            .map(|(p, tags)| {
                for &(l, _) in &tags {
                    counts[l as usize] += 1;
                }
                (crate::jobs::photo_id(&p), tags)
            })
            .collect();
        let mut counts: Vec<(u16, usize)> = counts
            .into_iter()
            .enumerate()
            .filter(|&(_, c)| c > 0)
            .map(|(l, c)| (l as u16, c))
            .collect();
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        tracing::info!("tagged {n} photos in {:?}", t.elapsed());
        self.tagged = Some(n);
        (self.notify)(Event::Tags(Arc::new(Tags {
            names: names.clone(),
            by_photo,
            counts,
        })));
    }

    fn index_batch(&mut self) {
        let n = self.pending.len().min(BATCH);
        let batch: Vec<Item> = self.pending.drain(..n).collect();
        // Big enough that the square input isn't upsampled.
        let decode = |it: &Item| it.decode(448);
        let decoded: Vec<Option<Rgb8>> = match &self.pool {
            Some(pool) => pool.install(|| batch.par_iter().map(decode).collect()),
            None => batch.iter().map(decode).collect(),
        };
        let (ok, images): (Vec<&Item>, Vec<Rgb8>) = batch
            .iter()
            .zip(decoded)
            .filter_map(|(it, img)| img.map(|i| (it, i)))
            .unzip();
        self.status.skipped += batch.len() - ok.len();
        let Some(model) = &mut self.model else {
            return;
        };
        match model.embed_images(&images) {
            Ok(vecs) => {
                let rows: Vec<(FileKey, Vector)> = ok
                    .iter()
                    .zip(vecs)
                    .map(|(it, v)| (it.key.clone(), Vector::quantise(&v)))
                    .collect();
                let stored: Vec<_> = rows.iter().map(|(k, v)| (k.clone(), v.payload())).collect();
                if let Some(store) = &mut self.store
                    && let Err(e) = store.put(&stored)
                {
                    tracing::warn!("embeddings: {e}");
                }
                for (k, v) in rows {
                    self.index.insert(k.path, v);
                }
                self.status.indexed = self.index.len();
            }
            Err(e) => {
                tracing::warn!("embedding failed: {e}");
                self.status.skipped += ok.len();
            }
        }
        self.publish();
        self.maybe_tag();
    }
}

fn siglip(dir: PathBuf) -> Box<dyn Model> {
    Box::new(Siglip {
        dir,
        device: shoebox_ml::device(),
        image: None,
        text: None,
    })
}

pub(crate) fn start_download(
    spec: &'static shoebox_ml::download::Spec,
    dir: PathBuf,
    staging: PathBuf,
) -> Download {
    let (ptx, progress) = crossbeam_channel::unbounded();
    let (dtx, done) = crossbeam_channel::bounded(1);
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let spawned = std::thread::Builder::new()
        .name("model-download".into())
        .spawn(move || {
            let r = shoebox_ml::download::fetch(
                spec,
                &dir,
                &staging,
                &|s| {
                    let _ = ptx.send(s);
                },
                &c,
            );
            let _ = dtx.send(r.map_err(|e| e.to_string()));
        });
    if let Err(e) = spawned {
        let (dtx, done) = crossbeam_channel::bounded(1);
        let _ = dtx.send(Err(e.to_string()));
        return Download {
            progress: crossbeam_channel::never(),
            done,
            cancel,
        };
    }
    Download {
        progress,
        done,
        cancel,
    }
}

/// Where the model was downloaded from (for the settings window).
pub fn model_source() -> String {
    shoebox_ml::download::SIGLIP.source()
}

/// Download size, for the settings window.
pub fn download_bytes() -> u64 {
    shoebox_ml::download::SIGLIP.bytes()
}

/// A semantic query.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    Text(String),
    /// Photos that look like this one (its primary's path).
    Similar(PathBuf),
    /// Photos with this suggested tag (answered from [`Tags`], no worker).
    Tag(u16),
}

/// The UI's side of semantic search.
#[derive(Default)]
pub struct SearchState {
    pub status: Status,
    /// The search box describes content, instead of matching names and keywords.
    pub by_content: bool,
    /// "Find similar" is showing photos like this one (id, path).
    pub similar: Option<(crate::jobs::PhotoId, PathBuf)>,
    /// Find Similar was chosen for this photo (applied next frame).
    pub similar_request: Option<crate::jobs::PhotoId>,
    /// Showing the photos with this suggested tag (Things in the left panel).
    pub tag: Option<u16>,
    /// The library's suggested tags, once computed.
    pub tags: Option<Arc<Tags>>,
    /// Photos per tag among those in view (before a tag narrows them), most
    /// first: the Things list's counts.
    pub counts: Vec<(u16, usize)>,
    /// Things lists every tag, not just the most common.
    pub things_all: bool,
    /// The latest results: score per primary photo.
    pub results: Option<(Query, std::collections::HashMap<crate::jobs::PhotoId, f32>)>,
    /// A query is out.
    pub searching: bool,
    /// Why the latest query failed.
    pub error: Option<String>,
    sent: Option<Sent>,
    next_id: u64,
    typed: String,
    typed_at: Option<Instant>,
    enabled_sent: Option<bool>,
}

struct Sent {
    id: u64,
    query: Query,
    at: Instant,
    /// Photos indexed when it was sent (more since: ask again).
    indexed: usize,
}

/// Wait this long after the last keystroke before searching.
const TYPING_PAUSE: Duration = Duration::from_millis(300);
/// While indexing, refresh the results this often.
const REFRESH: Duration = Duration::from_secs(3);

impl SearchState {
    /// Tell the worker about the setting when it changes.
    pub fn set_enabled(&mut self, semantic: &Semantic, on: bool) {
        if self.enabled_sent != Some(on) {
            self.enabled_sent = Some(on);
            semantic.send(Command::Enable(on));
            if !on {
                self.by_content = false;
                self.similar = None;
            }
        }
    }

    /// Turn it off and on again (after a failed download).
    pub fn retry(&mut self, semantic: &Semantic) {
        semantic.send(Command::Enable(false));
        semantic.send(Command::Enable(true));
        self.enabled_sent = Some(true);
    }

    /// Note what's in the search box (for the typing pause).
    pub fn typed(&mut self, text: &str) {
        if self.typed != text {
            self.typed = text.to_owned();
            self.typed_at = Some(Instant::now());
        }
    }

    /// Send `want` if it's new (or due a refresh). Returns how long until it
    /// should be checked again, if it's waiting for typing to pause.
    pub fn request(&mut self, semantic: &Semantic, want: Option<Query>) -> Option<Duration> {
        let Some(query) = want else {
            self.sent = None;
            self.searching = false;
            self.error = None;
            return None;
        };
        if !self.status.ready() {
            return None;
        }
        let refresh = self.sent.as_ref().is_some_and(|s| {
            s.query == query && s.indexed != self.status.indexed && s.at.elapsed() >= REFRESH
        });
        if self.sent.as_ref().is_some_and(|s| s.query == query) && !refresh {
            return None;
        }
        if matches!(query, Query::Text(_))
            && let Some(t) = self.typed_at
            && t.elapsed() < TYPING_PAUSE
        {
            return Some(TYPING_PAUSE - t.elapsed());
        }
        self.next_id += 1;
        let id = self.next_id;
        semantic.send(match &query {
            Query::Text(t) => Command::Text {
                id,
                text: t.clone(),
            },
            Query::Similar(p) => Command::Similar {
                id,
                path: p.clone(),
            },
            Query::Tag(_) => return None,
        });
        self.sent = Some(Sent {
            id,
            query,
            at: Instant::now(),
            indexed: self.status.indexed,
        });
        self.searching = !refresh;
        None
    }

    /// The query an answer belongs to, if it's the latest one.
    pub fn answered(&mut self, id: u64) -> Option<Query> {
        let s = self.sent.as_ref().filter(|s| s.id == id)?;
        self.searching = false;
        Some(s.query.clone())
    }
}
