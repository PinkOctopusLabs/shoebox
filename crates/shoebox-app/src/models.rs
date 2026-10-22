//! Models Develop downloads when first asked for (the object model, the
//! landscape model, AI raw denoise, object removal): pinned and checked by
//! `shoebox_ml::download`, fetched on a thread of their own, their
//! progress for the panel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use shoebox_ml::download::Spec;

/// Where a model's download is.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch {
    Idle,
    /// Bytes so far of the whole.
    Downloading(u64, u64),
    Failed(String),
}

pub struct Model {
    spec: &'static Spec,
    pub dir: PathBuf,
    staging: PathBuf,
    fetch: Mutex<Fetch>,
    /// Set when a download finishes (taken by [`Model::take_arrived`]).
    arrived: AtomicBool,
}

impl Model {
    pub fn new(spec: &'static Spec, dir: PathBuf, staging: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            spec,
            dir,
            staging,
            fetch: Mutex::new(Fetch::Idle),
            arrived: AtomicBool::new(false),
        })
    }

    /// Downloaded and ready to use.
    pub fn ready(&self) -> bool {
        self.spec.is_ready(&self.dir)
    }

    pub fn fetch_state(&self) -> Fetch {
        self.fetch.lock().map_or(Fetch::Idle, |f| f.clone())
    }

    /// The download's size in bytes.
    pub fn bytes(&self) -> u64 {
        self.spec.bytes()
    }

    /// A download finished since last asked.
    pub fn take_arrived(&self) -> bool {
        self.arrived.swap(false, Ordering::SeqCst)
    }

    /// Start downloading (on its own thread).
    pub fn download(self: &Arc<Self>, ctx: eframe::egui::Context) {
        if matches!(self.fetch_state(), Fetch::Downloading(..)) || self.ready() {
            return;
        }
        let total = self.bytes();
        if let Ok(mut f) = self.fetch.lock() {
            *f = Fetch::Downloading(0, total);
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("model-download".into())
            .spawn(move || {
                let cancel = AtomicBool::new(false);
                let r = shoebox_ml::download::fetch(
                    me.spec,
                    &me.dir,
                    &me.staging,
                    &|s| {
                        if let shoebox_ml::download::Stage::Downloading { done } = s
                            && let Ok(mut f) = me.fetch.lock()
                        {
                            *f = Fetch::Downloading(done, total);
                            ctx.request_repaint();
                        }
                    },
                    &cancel,
                );
                if let Ok(mut f) = me.fetch.lock() {
                    *f = match r {
                        Ok(()) => Fetch::Idle,
                        Err(e) => Fetch::Failed(e.to_string()),
                    };
                }
                me.arrived.store(true, Ordering::SeqCst);
                ctx.request_repaint();
            });
        if let Err(e) = spawned
            && let Ok(mut f) = self.fetch.lock()
        {
            *f = Fetch::Failed(e.to_string());
        }
    }
}

/// A download button, or its progress, for `model` (`what`: "the object
/// model"); nothing once it's there.
pub fn download_ui(ui: &mut eframe::egui::Ui, model: &Arc<Model>, what: &str, hint: &str) {
    use eframe::egui;
    if model.ready() {
        return;
    }
    match model.fetch_state() {
        Fetch::Downloading(done, total) => {
            ui.add(
                egui::ProgressBar::new(done as f32 / total.max(1) as f32)
                    .text(format!("Downloading {what}…")),
            );
        }
        state => {
            if let Fetch::Failed(e) = state {
                ui.weak(format!("Download failed: {e}"));
                ui.end_row();
            }
            let mb = (model.bytes() / 1_000_000).max(1);
            if ui
                .button(format!("Download {what} ({mb} MB)"))
                .on_hover_text(hint)
                .clicked()
            {
                model.download(ui.ctx().clone());
            }
        }
    }
    ui.end_row();
}

/// Sky and landscape masks (`shoebox_ml::scene`): the scene model in `dir`
/// (downloaded through `staging`), registered as the masks' scene finder
/// and loaded on first use. One picture's segmentation is kept (by its
/// hash), so each further part of it is immediate.
pub fn register_landscape(dir: PathBuf, staging: PathBuf) -> Arc<Model> {
    use shoebox_develop::edit::LandscapePart as L;
    use shoebox_ml::scene::{Part, Scene, SceneModel};
    let model = Model::new(&shoebox_ml::download::SCENE, dir, staging);
    let net: Mutex<Option<SceneModel>> = Mutex::new(None);
    let last: Mutex<Option<(blake3::Hash, Scene)>> = Mutex::new(None);
    let m = model.clone();
    shoebox_develop::mask::register_scene_finder(
        shoebox_ml::scene::MODEL_ID,
        Box::new(move |rgb, w, h, part| {
            if !m.ready() {
                return Err("the landscape model isn't downloaded yet".into());
            }
            let part = match part {
                L::Sky => Part::Sky,
                L::Vegetation => Part::Vegetation,
                L::Water => Part::Water,
                L::Mountains => Part::Mountains,
                L::Buildings => Part::Buildings,
                L::Ground => Part::Ground,
            };
            let key = blake3::hash(rgb);
            // One search at a time (the model's lock), so a picture's
            // parts asked for together are segmented once.
            let mut net = net.lock().map_err(|e| e.to_string())?;
            let mut last = last.lock().map_err(|e| e.to_string())?;
            if last.as_ref().is_none_or(|(k, _)| *k != key) {
                if net.is_none() {
                    let path = m.dir.join(shoebox_ml::scene::FILE);
                    *net = Some(
                        SceneModel::load(&path, &shoebox_ml::device())
                            .map_err(|e| e.to_string())?,
                    );
                }
                let Some(n) = net.as_ref() else {
                    return Err("no landscape model".into());
                };
                *last = Some((key, n.segment(rgb, w, h).map_err(|e| e.to_string())?));
            }
            let Some((_, s)) = last.as_ref() else {
                return Err("no segmentation".into());
            };
            Ok(shoebox_develop::mask::Coarse {
                width: s.width,
                height: s.height,
                data: s.part(part).to_vec(),
            })
        }),
    );
    model
}

/// Object removal (MI-GAN, `shoebox_ml::inpaint`): its model in `dir`
/// (downloaded through `staging`), registered as Develop's inpainter and
/// loaded on first use.
pub fn register_inpainter(dir: PathBuf, staging: PathBuf) -> Arc<Model> {
    use shoebox_ml::inpaint::{FILE, Inpainter, MODEL_ID, SIZE};
    let model = Model::new(&shoebox_ml::download::INPAINT, dir, staging);
    let net: Mutex<Option<Inpainter>> = Mutex::new(None);
    let m = model.clone();
    shoebox_develop::heal::remove::register(
        MODEL_ID,
        SIZE,
        Box::new(move |rgb, known| {
            if !m.ready() {
                return Err("the object removal model isn't downloaded yet".into());
            }
            let mut net = net.lock().map_err(|e| e.to_string())?;
            if net.is_none() {
                *net = Some(
                    Inpainter::load(&m.dir.join(FILE), &shoebox_ml::device())
                        .map_err(|e| e.to_string())?,
                );
            }
            let Some(n) = net.as_ref() else {
                return Err("no object removal model".into());
            };
            n.inpaint(rgb, known).map_err(|e| e.to_string())
        }),
    );
    model
}

/// Lens Blur's depth maps (Depth Anything V2 Small, `shoebox_ml::depth`):
/// the model in `dir` (downloaded through `staging`), registered as
/// Develop's depth model and loaded on first use.
pub fn register_depth(dir: PathBuf, staging: PathBuf) -> Arc<Model> {
    let model = Model::new(&shoebox_ml::download::DEPTH, dir, staging);
    let net: Mutex<Option<shoebox_ml::depth::DepthModel>> = Mutex::new(None);
    let m = model.clone();
    shoebox_develop::depth::register(
        shoebox_ml::depth::MODEL_ID,
        Box::new(move |rgb, w, h| {
            if !m.ready() {
                return Err("the depth model isn't downloaded yet".into());
            }
            let mut net = net.lock().map_err(|e| e.to_string())?;
            if net.is_none() {
                let path = m.dir.join(shoebox_ml::depth::FILE);
                *net = Some(
                    shoebox_ml::depth::DepthModel::load(&path, &shoebox_ml::device())
                        .map_err(|e| e.to_string())?,
                );
            }
            let Some(n) = net.as_ref() else {
                return Err("no depth model".into());
            };
            let d = n.estimate(rgb, w, h).map_err(|e| e.to_string())?;
            Ok(shoebox_develop::mask::Coarse {
                width: d.width,
                height: d.height,
                data: d.data,
            })
        }),
    );
    model
}

/// AI raw denoise (RawNIND, `shoebox_ml::rawnind`): its model in `dir`
/// (downloaded through `staging`), registered as Develop's denoiser and
/// loaded on first use.
pub fn register_denoiser(dir: PathBuf, staging: PathBuf) -> Arc<Model> {
    let model = Model::new(&shoebox_ml::download::RAWNIND, dir, staging);
    let net: Mutex<Option<shoebox_ml::rawnind::RawDenoiser>> = Mutex::new(None);
    let m = model.clone();
    shoebox_develop::denoise::register(
        shoebox_ml::rawnind::MODEL_ID,
        Box::new(move |packed, h, w, progress, cancel| {
            if !m.ready() {
                return Err("the AI denoise model isn't downloaded yet".into());
            }
            let mut net = net.lock().map_err(|e| e.to_string())?;
            if net.is_none() {
                let path = m.dir.join(shoebox_ml::rawnind::FILE);
                *net = Some(
                    shoebox_ml::rawnind::RawDenoiser::load(&path, &shoebox_ml::device())
                        .map_err(|e| e.to_string())?,
                );
            }
            let Some(n) = net.as_ref() else {
                return Err("no AI denoise model".into());
            };
            n.denoise(packed, h, w, progress, cancel)
                .map_err(|e| e.to_string())
        }),
    );
    model
}
