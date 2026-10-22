//! The object model (SAM 2.1 Tiny, `shoebox_ml::sam`) behind Develop's
//! click masks: downloaded when first asked for (pinned and checked), loaded
//! once, and registered as the masks' segmenter. The last picture's
//! encoding is kept, so each further click takes milliseconds.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use shoebox_ml::sam::{Click, Embedding, Sam};

/// Where the model's download is.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch {
    Idle,
    /// Bytes so far of the whole.
    Downloading(u64, u64),
    Failed(String),
}

pub struct Objects {
    dir: PathBuf,
    staging: PathBuf,
    model: Mutex<Option<Sam>>,
    /// The last picture encoded: its hash and encoding.
    last: Mutex<Option<(blake3::Hash, Embedding)>>,
    fetch: Mutex<Fetch>,
    /// Set when a download finishes: Develop asks again for masks that
    /// failed for want of it.
    pub arrived: AtomicBool,
}

impl Objects {
    /// The model in `dir` (downloaded through `staging`), registered as
    /// the masks' segmenter.
    pub fn register(dir: PathBuf, staging: PathBuf) -> Arc<Self> {
        let o = Arc::new(Self {
            dir,
            staging,
            model: Mutex::new(None),
            last: Mutex::new(None),
            fetch: Mutex::new(Fetch::Idle),
            arrived: AtomicBool::new(false),
        });
        let seg = o.clone();
        shoebox_develop::mask::register_segmenter(Box::new(move |rgb, w, h, clicks| {
            seg.segment(rgb, w, h, clicks)
        }));
        o
    }

    /// Downloaded and ready to use.
    pub fn ready(&self) -> bool {
        shoebox_ml::download::SAM.is_ready(&self.dir)
    }

    pub fn fetch_state(&self) -> Fetch {
        self.fetch.lock().map_or(Fetch::Idle, |f| f.clone())
    }

    /// The download's size in bytes.
    pub fn bytes() -> u64 {
        shoebox_ml::download::SAM.bytes()
    }

    /// Start downloading the model (on its own thread).
    pub fn download(self: &Arc<Self>, ctx: eframe::egui::Context) {
        if matches!(self.fetch_state(), Fetch::Downloading(..)) || self.ready() {
            return;
        }
        let total = Self::bytes();
        if let Ok(mut f) = self.fetch.lock() {
            *f = Fetch::Downloading(0, total);
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("object-model".into())
            .spawn(move || {
                let cancel = AtomicBool::new(false);
                let r = shoebox_ml::download::fetch(
                    &shoebox_ml::download::SAM,
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

    fn segment(
        &self,
        rgb: &[u8],
        w: usize,
        h: usize,
        clicks: &[(f32, f32, bool)],
    ) -> Result<Vec<u8>, String> {
        if !self.ready() {
            return Err("the object model isn't downloaded yet".into());
        }
        let mut model = self.model.lock().map_err(|e| e.to_string())?;
        if model.is_none() {
            let path = self.dir.join(shoebox_ml::sam::FILE);
            *model = Some(Sam::load(&path, &shoebox_ml::device()).map_err(|e| e.to_string())?);
        }
        let Some(sam) = model.as_ref() else {
            return Err("no object model".into());
        };
        let key = blake3::hash(rgb);
        let mut last = self.last.lock().map_err(|e| e.to_string())?;
        if last.as_ref().is_none_or(|(k, _)| *k != key) {
            let e = sam.embed(rgb, w, h).map_err(|e| e.to_string())?;
            *last = Some((key, e));
        }
        let Some((_, e)) = last.as_ref() else {
            return Err("no encoding".into());
        };
        let clicks: Vec<Click> = clicks
            .iter()
            .map(|&(x, y, add)| Click { x, y, add })
            .collect();
        let cover = sam.predict(e, &clicks).map_err(|e| e.to_string())?;
        Ok(cover
            .iter()
            .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect())
    }
}
