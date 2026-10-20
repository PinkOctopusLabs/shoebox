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

pub enum Event {
    /// Converting a High efficiency NEF (the first time it's opened).
    Converting(PhotoId),
    Ready(Ready),
    /// High efficiency, and Adobe DNG Converter isn't installed.
    NeedsConverter(PhotoId),
    Failed(PhotoId, String),
}

pub struct Develop {
    requests: Sender<Request>,
    pub events: Receiver<Event>,
}

impl Develop {
    pub fn new(cache_dir: PathBuf, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (rtx, rrx) = crossbeam_channel::unbounded::<Request>();
        let (etx, erx) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("develop".into())
            .spawn(move || worker(&cache_dir, &rrx, &etx, &repaint));
        if let Err(e) = spawned {
            tracing::error!("could not start the develop thread: {e}");
        }
        Self {
            requests: rtx,
            events: erx,
        }
    }

    pub fn open(&self, r: Request) {
        let _ = self.requests.send(r);
    }
}

fn worker(
    cache_dir: &std::path::Path,
    requests: &Receiver<Request>,
    events: &Sender<Event>,
    repaint: &(dyn Fn() + Send + Sync),
) {
    let emit = |e: Event| {
        let _ = events.send(e);
        repaint();
    };
    let dngs = DngCache::new(cache_dir, shoebox_develop::dng::DEFAULT_LIMIT);
    let mut kept: Vec<Ready> = Vec::new();
    while let Ok(mut r) = requests.recv() {
        // Only the newest request matters.
        while let Ok(newer) = requests.try_recv() {
            r = newer;
        }
        if let Some(hit) = kept.iter().position(|k| k.id == r.id && k.raw == r.raw) {
            let k = kept.remove(hit);
            kept.push(k.clone());
            emit(Event::Ready(k));
            continue;
        }
        match prepare(&r, &dngs, &emit) {
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
    }
}

/// Decode and demosaic one photo; None if it needs the converter and it
/// isn't installed.
fn prepare(r: &Request, dngs: &DngCache, emit: &dyn Fn(Event)) -> Result<Option<Ready>, String> {
    if r.format != shoebox_decode::Format::Nef {
        return finished(r).map(Some);
    }
    let bytes = std::fs::read(&r.path).map_err(|e| format!("{}: {e}", r.path.display()))?;
    let source = if shoebox_develop::dng::needs_dng(&bytes) {
        let Some(conv) = Converter::find(None) else {
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
    let src = cpu::demosaic(&raw, &r.raw, Some(PROXY));
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
fn finished(r: &Request) -> Result<Ready, String> {
    use shoebox_decode::{Format, Target};
    let img = shoebox_decode::render_path(r.format, &r.path, Some(PROXY), 1, Target::DisplayP3)
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
