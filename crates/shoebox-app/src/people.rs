//! People: a background thread that downloads the face models (once, when the
//! setting is turned on), finds and describes the faces in each stack's
//! primary image, groups them, and matches groups to the people you named.
//!
//! It only reads library files. Its state: face rows in `embeddings.sqlite`
//! (rebuildable) and your names in `people.json` (user state, in the data
//! directory). Keywords go to sidecars only when you ask, through the usual
//! marks path.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select};
use rayon::prelude::*;
use shoebox_core::catalog::Catalog;
use shoebox_core::embeddings::{FileKey, Store, Vector};
use shoebox_core::events::now_ms;
use shoebox_core::people::{self, Face, FaceRef, People};
use shoebox_decode::{Format, Rgb8};

use crate::jobs::PhotoId;
use crate::semantic::{Download, Item, ModelState, Status, primaries, start_download};

/// Faces this alike (cosine) join a group. SFace's same-person threshold is
/// 0.363, but grouping chains, so it's stricter (sisters scored 0.40).
pub const GROUP_MIN: f32 = 0.45;
/// A group is a named person if its faces resemble their examples this much.
pub const NAME_MIN: f32 = 0.45;
/// Faces the detector is less sure of aren't grouped (they're kept).
pub const MIN_SCORE: f32 = 0.85;
/// Faces smaller than this (fraction of the image's long edge) aren't
/// grouped: too few pixels to tell people apart.
pub const MIN_SIZE: f32 = 0.025;
/// Unnamed groups need this many faces to be listed.
pub const MIN_GROUP: usize = 3;
/// Regroup after this many more photos (and when indexing ends).
const REGROUP_EVERY: usize = 2000;
/// Photos per batch.
const BATCH: usize = 8;

/// A face found: rectangle (x, y, width, height as fractions of the
/// image), detector score, unit-length description.
pub type FoundFace = ([f32; 4], f32, Vec<f32>);

/// Finds faces and describes them.
pub trait FaceModel: Send {
    /// Faces in `img` (upright).
    fn faces(&mut self, img: &Rgb8) -> Result<Vec<FoundFace>, String>;
}

/// Where the face models come from.
#[derive(Clone)]
pub enum FaceSource {
    /// YuNet + SFace, downloaded into `dir` when first turned on.
    Download { dir: PathBuf, staging: PathBuf },
    /// A ready model (tests).
    Fixed(fn() -> Box<dyn FaceModel>),
}

struct OpenCvFaces {
    detector: shoebox_ml::faces::FaceDetector,
    embedder: shoebox_ml::faces::FaceEmbedder,
}

impl FaceModel for OpenCvFaces {
    fn faces(&mut self, img: &Rgb8) -> Result<Vec<FoundFace>, String> {
        use shoebox_ml::faces::{DETECT_SIZE, align};
        let small = img.fit_long_edge(DETECT_SIZE);
        let found = self
            .detector
            .detect(&small, 0.8)
            .map_err(|e| e.to_string())?;
        if found.is_empty() {
            return Ok(Vec::new());
        }
        // Align from the bigger image: more pixels per face.
        let k = img.width as f32 / small.width as f32;
        let crops: Vec<Rgb8> = found.iter().map(|f| align(img, &f.scaled(k))).collect();
        let vecs = self.embedder.embed(&crops).map_err(|e| e.to_string())?;
        let (w, h) = (small.width as f32, small.height as f32);
        Ok(found
            .iter()
            .zip(vecs)
            .map(|(f, v)| {
                let [x, y, fw, fh] = f.rect;
                ([x / w, y / h, fw / w, fh / h], f.score, v)
            })
            .collect())
    }
}

fn load_opencv(dir: &std::path::Path) -> Result<Box<dyn FaceModel>, String> {
    use shoebox_ml::download::{SFACE_FILE, YUNET_FILE};
    let read = |f: &str| std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"));
    // These small models are faster on the CPU than through Metal.
    let cpu = shoebox_ml::Device::Cpu;
    Ok(Box::new(OpenCvFaces {
        detector: shoebox_ml::faces::FaceDetector::load(&read(YUNET_FILE)?, &cpu)
            .map_err(|e| e.to_string())?,
        embedder: shoebox_ml::faces::FaceEmbedder::load(&read(SFACE_FILE)?, &cpu)
            .map_err(|e| e.to_string())?,
    }))
}

/// Which group: a named person (by id) or an unnamed group (by one of its
/// faces), so a choice survives regrouping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupKey {
    Person(String),
    Unnamed(FaceRef),
}

/// A person (or unnamed group) for the UI.
#[derive(Debug, Clone)]
pub struct Group {
    pub key: GroupKey,
    pub name: Option<String>,
    /// Primary photos they're in.
    pub photos: Vec<PhotoId>,
    /// Their faces.
    pub faces: Vec<FaceRef>,
}

/// The library's people, for the UI.
#[derive(Debug, Default)]
pub struct PeopleView {
    /// Named people (by name), then unnamed groups (biggest first).
    pub groups: Vec<Group>,
    /// Groups in each primary photo (indices into `groups`).
    pub by_photo: HashMap<PhotoId, Vec<usize>>,
    /// Where each grouped face is in its photo: (x, y, width, height as
    /// fractions of the upright image, group index).
    pub boxes: HashMap<PhotoId, Vec<([f32; 4], usize)>>,
    /// Faces found.
    pub faces: usize,
}

impl PeopleView {
    pub fn find(&self, key: &GroupKey) -> Option<usize> {
        self.groups.iter().position(|g| &g.key == key)
    }
}

pub enum Command {
    Enable(bool),
    Catalog(Arc<Catalog>, bool),
    CancelDownload,
    /// Name (or rename) these groups as one person: several groups are
    /// merged, their faces becoming that person's examples. A name someone
    /// already has merges into them.
    Name {
        groups: Vec<GroupKey>,
        name: String,
    },
    /// These faces aren't this person.
    NotPerson {
        person: String,
        faces: Vec<FaceRef>,
    },
    /// Forget a named person (their faces go back to unnamed groups).
    Forget {
        person: String,
    },
    /// Undo or redo the last change to names (naming, merging, "not",
    /// forgetting).
    Undo,
    Redo,
    /// A newer action elsewhere (the library's): nothing to redo here.
    ClearRedo,
}

pub enum Event {
    Status(Status),
    People(Arc<PeopleView>),
    Error(String),
    /// When the change ⌘Z would undo was made, and when the one ⌘⇧Z would
    /// redo was undone (ms since the epoch).
    History {
        undo_at: Option<i64>,
        redo_at: Option<i64>,
    },
}

pub struct PeopleWorker {
    tx: Sender<Command>,
    pub events: Receiver<Event>,
}

impl PeopleWorker {
    pub fn new(
        source: FaceSource,
        db: PathBuf,
        names: PathBuf,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (etx, events) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("people".into())
            .spawn(move || {
                let notify = move |e: Event| {
                    let _ = etx.send(e);
                    repaint();
                };
                Worker::new(source, db, names, notify).run(&rx);
            });
        if let Err(e) = spawned {
            tracing::error!("could not start the people thread: {e}");
        }
        Self { tx, events }
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

struct Worker<F: Fn(Event)> {
    source: FaceSource,
    db_path: PathBuf,
    names_path: PathBuf,
    notify: F,
    enabled: bool,
    status: Status,
    store: Option<Store>,
    model: Option<Box<dyn FaceModel>>,
    download: Option<Download>,
    /// Faces by file.
    faces: HashMap<PathBuf, Vec<Face>>,
    keys: Vec<FileKey>,
    pending: VecDeque<Item>,
    catalog: Option<(Arc<Catalog>, bool)>,
    people: People,
    /// Names before each change, and when it was made (undo); names before
    /// each undo, and when it was undone (redo).
    undo: Vec<(People, i64)>,
    redo: Vec<(People, i64)>,
    /// The last grouping: each group's faces (for naming).
    groups: Vec<(GroupKey, Vec<FaceRef>)>,
    /// Files scanned when last grouped (None: needs grouping).
    grouped: Option<usize>,
    pool: Option<rayon::ThreadPool>,
}

impl<F: Fn(Event)> Worker<F> {
    fn new(source: FaceSource, db_path: PathBuf, names_path: PathBuf, notify: F) -> Self {
        Self {
            source,
            db_path,
            names_path,
            notify,
            enabled: false,
            status: Status::default(),
            store: None,
            model: None,
            download: None,
            faces: HashMap::new(),
            keys: Vec::new(),
            pending: VecDeque::new(),
            catalog: None,
            people: People::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            groups: Vec::new(),
            grouped: None,
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(3)
                .thread_name(|i| format!("people-{i}"))
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
                    Err(_) => return,
                },
                recv(progress) -> s => if let Ok(s) = s {
                    self.status.model = Download::state(&shoebox_ml::download::FACES, s);
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
                default(wait) => if busy { self.index_batch() },
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
            Command::Name { groups, name } => {
                let name = name.trim().to_owned();
                if name.is_empty() || groups.is_empty() {
                    return;
                }
                let mut people: Vec<String> = Vec::new();
                let mut faces: Vec<Vec<&Vector>> = Vec::new();
                for g in &groups {
                    match g {
                        GroupKey::Person(id) => people.push(id.clone()),
                        GroupKey::Unnamed(_) => {
                            if let Some((_, refs)) = self.groups.iter().find(|(k, _)| k == g) {
                                faces.push(
                                    refs.iter()
                                        .filter_map(|r| self.faces.get(&r.path)?.get(r.index))
                                        .map(|f| &f.vec)
                                        .collect(),
                                );
                            }
                        }
                    }
                }
                let before = self.people.clone();
                let mut named = std::mem::take(&mut self.people);
                named.merge(&name, &people, &faces);
                self.people = named;
                self.changed(before);
            }
            Command::NotPerson { person, faces } => {
                let before = self.people.clone();
                if let Some(p) = self.people.people.iter_mut().find(|p| p.id == person) {
                    for f in faces {
                        if !p.not.contains(&f) {
                            p.not.push(f);
                        }
                    }
                }
                self.changed(before);
            }
            Command::Forget { person } => {
                let before = self.people.clone();
                self.people.people.retain(|p| p.id != person);
                self.changed(before);
            }
            Command::Undo | Command::Redo => {
                let undo = matches!(c, Command::Undo);
                let (from, to) = if undo {
                    (&mut self.undo, &mut self.redo)
                } else {
                    (&mut self.redo, &mut self.undo)
                };
                let Some((names, _)) = from.pop() else {
                    return;
                };
                to.push((std::mem::replace(&mut self.people, names), now_ms()));
                self.save_names();
                self.history();
                self.regroup();
            }
            Command::ClearRedo => {
                if !self.redo.is_empty() {
                    self.redo.clear();
                    self.history();
                }
            }
        }
    }

    /// After a change to names: undoable, saved, and regrouped.
    fn changed(&mut self, before: People) {
        if before == self.people {
            return;
        }
        self.undo.push((before, now_ms()));
        self.redo.clear();
        self.save_names();
        self.history();
        self.regroup();
    }

    fn history(&self) {
        (self.notify)(Event::History {
            undo_at: self.undo.last().map(|u| u.1),
            redo_at: self.redo.last().map(|r| r.1),
        });
    }

    fn save_names(&self) {
        if let Err(e) = self.people.save(&self.names_path) {
            (self.notify)(Event::Error(format!("Couldn't save names: {e}")));
        }
    }

    fn start(&mut self) {
        match People::load(&self.names_path) {
            Ok(p) => self.people = p,
            Err(e) => (self.notify)(Event::Error(format!("Couldn't read names: {e}"))),
        }
        if self.store.is_none() {
            match Store::open(&self.db_path, "faces", shoebox_ml::faces::MODEL_ID) {
                Ok(s) => self.store = Some(s),
                Err(e) => {
                    self.status.model = ModelState::Failed(e.to_string());
                    self.publish();
                    return;
                }
            }
        }
        match self.source.clone() {
            FaceSource::Fixed(make) => {
                self.model = Some(make());
                self.model_ready();
            }
            FaceSource::Download { dir, staging } => {
                if shoebox_ml::download::FACES.is_ready(&dir) {
                    self.model_ready();
                } else if self.download.is_none() {
                    self.status.model = ModelState::Downloading {
                        done: 0,
                        total: shoebox_ml::download::FACES.bytes(),
                    };
                    self.publish();
                    self.download =
                        Some(start_download(&shoebox_ml::download::FACES, dir, staging));
                }
            }
        }
    }

    fn model_ready(&mut self) {
        if !self.enabled {
            return;
        }
        if self.model.is_none()
            && let FaceSource::Download { dir, .. } = &self.source
        {
            match load_opencv(dir) {
                Ok(m) => self.model = Some(m),
                Err(e) => {
                    self.status.model = ModelState::Failed(e);
                    self.publish();
                    return;
                }
            }
        }
        self.status.model = ModelState::Ready;
        self.keys.clear();
        self.sync();
        self.publish();
    }

    fn sync(&mut self) {
        let Some((catalog, complete)) = self.catalog.clone() else {
            return;
        };
        let (keys, formats) = primaries(&catalog);
        self.status.total = keys.len();
        if keys == self.keys {
            return;
        }
        let Some(store) = &mut self.store else {
            return;
        };
        match store.sync(&keys, complete) {
            Ok(r) => {
                tracing::info!(
                    "faces: {} photos scanned, {} to scan, {} followed, {} removed",
                    r.found.len(),
                    r.missing.len(),
                    r.followed,
                    r.removed
                );
                self.faces = r
                    .found
                    .iter()
                    .map(|(p, row)| (p.clone(), people::decode(row)))
                    .collect();
                self.pending = r
                    .missing
                    .into_iter()
                    .filter_map(|key| {
                        let &(format, orientation): &(Format, u16) = formats.get(&key.path)?;
                        Some(Item {
                            key,
                            format,
                            orientation,
                        })
                    })
                    .collect();
                self.pending.make_contiguous().reverse();
                self.status.indexed = self.faces.len();
                self.status.skipped = 0;
                self.keys = keys;
                self.grouped = None;
            }
            Err(e) => self.status.model = ModelState::Failed(e.to_string()),
        }
        self.publish();
        self.maybe_regroup();
    }

    fn index_batch(&mut self) {
        let n = self.pending.len().min(BATCH);
        let batch: Vec<Item> = self.pending.drain(..n).collect();
        // Big enough to align faces from; the detector runs on 640 px.
        let decode = |it: &Item| it.decode(1600);
        let decoded: Vec<Option<Rgb8>> = match &self.pool {
            Some(pool) => pool.install(|| batch.par_iter().map(decode).collect()),
            None => batch.iter().map(decode).collect(),
        };
        let Some(model) = &mut self.model else {
            return;
        };
        let mut rows = Vec::new();
        for (it, img) in batch.iter().zip(decoded) {
            let Some(img) = img else {
                self.status.skipped += 1;
                continue;
            };
            match model.faces(&img) {
                Ok(found) => {
                    let faces: Vec<Face> = found
                        .into_iter()
                        .map(|(rect, score, v)| Face {
                            rect,
                            score,
                            vec: Vector::quantise(&v),
                        })
                        .collect();
                    rows.push((it.key.clone(), people::encode(&faces)));
                    self.faces.insert(it.key.path.clone(), faces);
                }
                Err(e) => {
                    tracing::warn!("faces in {}: {e}", it.key.path.display());
                    self.status.skipped += 1;
                }
            }
        }
        if let Some(store) = &mut self.store
            && let Err(e) = store.put(&rows)
        {
            tracing::warn!("faces: {e}");
        }
        self.status.indexed = self.faces.len();
        self.publish();
        self.maybe_regroup();
    }

    fn maybe_regroup(&mut self) {
        let n = self.faces.len();
        let due = match self.grouped {
            None => self.pending.is_empty() || n >= REGROUP_EVERY,
            Some(g) => (self.pending.is_empty() && g != n) || n >= g + REGROUP_EVERY,
        };
        if due {
            self.regroup();
        }
    }

    /// Group the faces, match groups to named people, and publish.
    fn regroup(&mut self) {
        let t = Instant::now();
        self.grouped = Some(self.faces.len());
        // Faces worth grouping, in a fixed order (by path, then position).
        let mut paths: Vec<&PathBuf> = self.faces.keys().collect();
        paths.sort();
        let mut refs: Vec<FaceRef> = Vec::new();
        let mut vecs: Vec<&Vector> = Vec::new();
        let mut scores: Vec<f32> = Vec::new();
        let mut total = 0;
        for p in paths {
            for (i, f) in self.faces[p].iter().enumerate() {
                total += 1;
                if f.score >= MIN_SCORE && f.rect[2].max(f.rect[3]) >= MIN_SIZE {
                    refs.push(FaceRef {
                        path: p.clone(),
                        index: i,
                    });
                    vecs.push(&f.vec);
                    scores.push(f.score);
                }
            }
        }
        let mut order: Vec<usize> = (0..vecs.len()).collect();
        order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
        let of = people::group(&vecs, &order, GROUP_MIN);
        let n_groups = of.iter().copied().max().map_or(0, |m| m + 1);
        let mut members: Vec<Vec<usize>> = vec![Vec::new(); n_groups];
        for (f, &g) in of.iter().enumerate() {
            members[g].push(f);
        }
        let group_vecs: Vec<Vec<&Vector>> = members
            .iter()
            .map(|m| m.iter().map(|&f| vecs[f]).collect())
            .collect();
        let named = people::assign(&group_vecs, &self.people, NAME_MIN);

        // Named people (several groups may be one person), then unnamed.
        let mut by_person: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut unnamed: Vec<usize> = Vec::new();
        for (g, p) in named.iter().enumerate() {
            match p {
                Some(p) => by_person.entry(*p).or_default().push(g),
                None => unnamed.push(g),
            }
        }
        let mut out: Vec<(GroupKey, Option<String>, Vec<usize>)> = Vec::new();
        let mut person_ids: Vec<usize> = by_person.keys().copied().collect();
        person_ids.sort_by(|a, b| {
            self.people.people[*a]
                .name
                .to_lowercase()
                .cmp(&self.people.people[*b].name.to_lowercase())
        });
        for p in person_ids {
            let person = &self.people.people[p];
            let faces: Vec<usize> = by_person[&p]
                .iter()
                .flat_map(|&g| members[g].iter().copied())
                .filter(|&f| !person.not.contains(&refs[f]))
                .collect();
            out.push((
                GroupKey::Person(person.id.clone()),
                Some(person.name.clone()),
                faces,
            ));
        }
        for g in unnamed {
            if members[g].len() >= MIN_GROUP {
                let first = refs[members[g][0]].clone();
                out.push((GroupKey::Unnamed(first), None, members[g].clone()));
            }
        }

        self.groups = out
            .iter()
            .map(|(k, _, f)| (k.clone(), f.iter().map(|&i| refs[i].clone()).collect()))
            .collect();
        let mut view = PeopleView {
            faces: total,
            ..PeopleView::default()
        };
        for (gi, (key, name, faces)) in out.into_iter().enumerate() {
            let mut photos: Vec<PhotoId> = Vec::new();
            let face_refs: Vec<FaceRef> = faces.iter().map(|&i| refs[i].clone()).collect();
            for r in &face_refs {
                let id = crate::jobs::photo_id(&r.path);
                if let Some(f) = self.faces.get(&r.path).and_then(|v| v.get(r.index)) {
                    view.boxes.entry(id).or_default().push((f.rect, gi));
                }
                if !photos.contains(&id) {
                    photos.push(id);
                    view.by_photo.entry(id).or_default().push(gi);
                }
            }
            view.groups.push(Group {
                key,
                name,
                photos,
                faces: face_refs,
            });
        }
        tracing::info!(
            "people: {} faces in {} groups ({} listed) in {:?}",
            vecs.len(),
            n_groups,
            view.groups.len(),
            t.elapsed()
        );
        (self.notify)(Event::People(Arc::new(view)));
    }
}

/// Download size, for the settings window.
pub fn download_bytes() -> u64 {
    shoebox_ml::download::FACES.bytes()
}

pub fn model_source() -> String {
    shoebox_ml::download::FACES.source()
}

/// The UI's side of People.
#[derive(Default)]
pub struct PeopleState {
    pub status: Status,
    pub view: Option<Arc<PeopleView>>,
    /// Showing this group's photos.
    pub showing: Option<GroupKey>,
    /// …and these groups' too (⌘-click), say to check they're one person.
    pub also: Vec<GroupKey>,
    /// Each group's photos among those shown (before the people chosen
    /// narrow them), by index into the view's groups.
    pub counts: Vec<usize>,
    /// The name being typed for the group shown.
    pub name_input: String,
    /// People lists every group, not just the first few.
    pub all: bool,
    /// The person whose name the pointer is on (info panel), this frame:
    /// their face is outlined in the loupe.
    pub hovered: Option<GroupKey>,
    enabled_sent: Option<bool>,
}

impl PeopleState {
    pub fn set_enabled(&mut self, worker: &PeopleWorker, on: bool) {
        if self.enabled_sent != Some(on) {
            self.enabled_sent = Some(on);
            worker.send(Command::Enable(on));
            if !on {
                self.showing = None;
            }
        }
    }

    pub fn retry(&mut self, worker: &PeopleWorker) {
        worker.send(Command::Enable(false));
        worker.send(Command::Enable(true));
        self.enabled_sent = Some(true);
    }

    /// The group shown, if it still exists and is the only one.
    pub fn shown_group(&self) -> Option<&Group> {
        if !self.also.is_empty() {
            return None;
        }
        let v = self.view.as_ref()?;
        v.find(self.showing.as_ref()?).map(|i| &v.groups[i])
    }

    /// Every group shown (one, or several with ⌘-click), by index.
    pub fn shown_indices(&self) -> Vec<usize> {
        let Some(v) = self.view.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<usize> = self
            .showing
            .iter()
            .chain(&self.also)
            .filter_map(|k| v.find(k))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Every group shown.
    pub fn shown_groups(&self) -> Vec<&Group> {
        let Some(v) = self.view.as_ref() else {
            return Vec::new();
        };
        self.shown_indices()
            .into_iter()
            .map(|i| &v.groups[i])
            .collect()
    }

    /// A group's display name.
    pub fn label(&self, g: &Group) -> String {
        match &g.name {
            Some(n) => n.clone(),
            None => {
                let n = self
                    .view
                    .as_ref()
                    .map(|v| {
                        v.groups
                            .iter()
                            .filter(|x| x.name.is_none())
                            .position(|x| x.key == g.key)
                            .map_or(0, |p| p + 1)
                    })
                    .unwrap_or(0);
                format!("Unnamed {n}")
            }
        }
    }
}
