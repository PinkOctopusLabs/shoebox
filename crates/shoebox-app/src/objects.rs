//! The object model (SAM 2.1 Tiny, `shoebox_ml::sam`) behind Develop's
//! click masks: downloaded when first asked for (`models`), loaded once,
//! and registered as the masks' segmenter. The last picture's
//! encoding is kept, so each further click takes milliseconds.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shoebox_ml::sam::{Click, Embedding, Sam};

use crate::models::Model;

pub struct Objects {
    pub model: Arc<Model>,
    sam: Mutex<Option<Sam>>,
    /// The last picture encoded: its hash and encoding.
    last: Mutex<Option<(blake3::Hash, Embedding)>>,
}

impl Objects {
    /// The model in `dir` (downloaded through `staging`), registered as
    /// the masks' segmenter.
    pub fn register(dir: PathBuf, staging: PathBuf) -> Arc<Self> {
        let o = Arc::new(Self {
            model: Model::new(&shoebox_ml::download::SAM, dir, staging),
            sam: Mutex::new(None),
            last: Mutex::new(None),
        });
        let seg = o.clone();
        shoebox_develop::mask::register_segmenter(Box::new(move |rgb, w, h, clicks| {
            seg.segment(rgb, w, h, clicks)
        }));
        o
    }

    fn segment(
        &self,
        rgb: &[u8],
        w: usize,
        h: usize,
        clicks: &[(f32, f32, bool)],
    ) -> Result<Vec<u8>, String> {
        if !self.model.ready() {
            return Err("the object model isn't downloaded yet".into());
        }
        let mut model = self.sam.lock().map_err(|e| e.to_string())?;
        if model.is_none() {
            let path = self.model.dir.join(shoebox_ml::sam::FILE);
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
