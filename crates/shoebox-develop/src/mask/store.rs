//! AI masks kept on disk (EDITING-SPEC, Rendering masks: "computed once
//! on the proxy and cached as 8-bit PNG"): what a finder or segmenter
//! returned, as an 8-bit grayscale PNG named by a key over everything it
//! was given, so opening a photo again, or restarting, reads the mask
//! instead of running Vision or SAM. Object removal keeps its fills in a
//! store of its own the same way, as 16-bit RGB PNGs
//! (`crate::heal::remove`).
//!
//! The key is a BLAKE3 over the picture the finder saw (its exact bytes
//! and size, so the file's content, its raw settings, the source's size,
//! the colour and display it was shown with and its orientation all
//! count), what was asked for (an object's clicks in that picture's
//! pixels), the model's identity and this format's version. A finder that
//! can't name itself (an empty id) keeps nothing.
//!
//! Files live in the app's own cache folder (`<cache>/ai-masks`), written
//! beside and renamed into place; past the limit the least recently used
//! are removed. Only `*.png` and our own stale temporary files there are
//! ever removed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// The default limit. A 2560 px mask is typically 30–300 KB as a PNG, so
/// this keeps thousands.
pub const DEFAULT_LIMIT: u64 = 512 << 20;

/// Bumped whenever what's stored, or how the finder's picture is made,
/// changes in a way the key wouldn't see.
const VERSION: &str = "shoebox-ai-mask/1";

/// Masks on disk.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    limit: u64,
}

static STORE: RwLock<Option<Arc<Store>>> = RwLock::new(None);
static TMP: AtomicU64 = AtomicU64::new(0);

/// Keep AI masks in `store` (None: nowhere). The app sets it to
/// `<cache>/ai-masks`.
pub fn set(store: Option<Store>) {
    let mut s = STORE.write().unwrap_or_else(|e| e.into_inner());
    *s = store.map(Arc::new);
}

/// Where AI masks are kept, if anywhere.
pub fn current() -> Option<Arc<Store>> {
    STORE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The key for a search: the model (`model`), what was asked (`what`, and
/// an object's `clicks` in the picture's pixels, true to add), and the
/// picture itself (`rgb`, `w`×`h`).
pub fn key(
    model: &str,
    what: &str,
    clicks: &[(f32, f32, bool)],
    rgb: &[u8],
    w: usize,
    h: usize,
) -> String {
    let mut k = blake3::Hasher::new();
    for s in [VERSION, model, what] {
        k.update(&(s.len() as u64).to_le_bytes());
        k.update(s.as_bytes());
    }
    k.update(&(clicks.len() as u64).to_le_bytes());
    for &(x, y, add) in clicks {
        k.update(&x.to_bits().to_le_bytes());
        k.update(&y.to_bits().to_le_bytes());
        k.update(&[u8::from(add)]);
    }
    k.update(&(w as u64).to_le_bytes());
    k.update(&(h as u64).to_le_bytes());
    k.update(blake3::hash(rgb).as_bytes());
    k.finalize().to_hex().to_string()
}

/// `run` (a search of `rgb`, `w`×`h`), or its result kept on disk under
/// the same key. Failures aren't kept.
pub fn through(
    model: &str,
    what: &str,
    clicks: &[(f32, f32, bool)],
    rgb: &[u8],
    w: usize,
    h: usize,
    run: impl FnOnce() -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    let store = current().filter(|_| !model.is_empty());
    let Some(store) = store else {
        return run();
    };
    let k = key(model, what, clicks, rgb, w, h);
    if let Some(m) = store.get(&k, w, h) {
        return Ok(m);
    }
    let m = run()?;
    if m.len() == w * h
        && let Err(e) = store.put(&k, &m, w, h)
    {
        tracing::warn!("keeping an AI mask in {}: {e}", store.dir.display());
    }
    Ok(m)
}

/// [`through`] for a map of values 0–1 on the finder's own grid (a depth
/// map), kept as a 16-bit PNG.
pub fn through_values(
    model: &str,
    what: &str,
    rgb: &[u8],
    w: usize,
    h: usize,
    run: impl FnOnce() -> Result<super::Coarse, String>,
) -> Result<super::Coarse, String> {
    let store = current().filter(|_| !model.is_empty());
    let Some(store) = store else {
        return run();
    };
    let k = key(model, what, &[], rgb, w, h);
    if let Some(m) = store.get_values(&k) {
        return Ok(m);
    }
    let m = run()?;
    if let Err(e) = store.put_values(&k, &m) {
        tracing::warn!("keeping a map in {}: {e}", store.dir.display());
    }
    Ok(m)
}

impl Store {
    pub fn new(dir: &Path, limit: u64) -> Self {
        Self {
            dir: dir.to_path_buf(),
            limit,
        }
    }

    pub fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.png"))
    }

    /// A kept mask of `w`×`h` (its time touched, so it's kept longest).
    /// None if there's none, or it can't be read or is the wrong size.
    pub fn get(&self, key: &str, w: usize, h: usize) -> Option<Vec<u8>> {
        self.read(key, w, h, Pixels::Gray8)
    }

    /// Keep a mask (written beside, then renamed into place), removing the
    /// least recently used past the limit.
    pub fn put(&self, key: &str, mask: &[u8], w: usize, h: usize) -> std::io::Result<()> {
        self.write(key, mask, w, h, Pixels::Gray8)
    }

    /// A kept 16-bit RGB picture of `w`×`h` (object removal's fills).
    pub fn get_rgb16(&self, key: &str, w: usize, h: usize) -> Option<Vec<u16>> {
        let b = self.read(key, w, h, Pixels::Rgb16)?;
        Some(
            b.as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect(),
        )
    }

    /// Keep a 16-bit RGB picture, as [`Store::put`].
    pub fn put_rgb16(&self, key: &str, rgb: &[u16], w: usize, h: usize) -> std::io::Result<()> {
        let b: Vec<u8> = rgb.iter().flat_map(|v| v.to_be_bytes()).collect();
        self.write(key, &b, w, h, Pixels::Rgb16)
    }

    fn read(&self, key: &str, w: usize, h: usize, px: Pixels) -> Option<Vec<u8>> {
        let p = self.path(key);
        let bytes = std::fs::read(&p).ok()?;
        let m = decode(&bytes, w, h, px)?;
        let _ = std::fs::File::options()
            .append(true)
            .open(&p)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        Some(m)
    }

    /// A kept map of values (any size), touched as [`Self::get`] does.
    pub fn get_values(&self, key: &str) -> Option<super::Coarse> {
        let p = self.path(key);
        let bytes = std::fs::read(&p).ok()?;
        let m = decode16(&bytes)?;
        let _ = std::fs::File::options()
            .append(true)
            .open(&p)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        Some(m)
    }

    /// Keep a map of values 0–1, 16 bits each.
    pub fn put_values(&self, key: &str, m: &super::Coarse) -> std::io::Result<()> {
        let bytes = encode16(m).map_err(std::io::Error::other)?;
        self.write_bytes(key, &bytes)
    }

    fn write(&self, key: &str, data: &[u8], w: usize, h: usize, px: Pixels) -> std::io::Result<()> {
        let bytes = encode(data, w, h, px).map_err(std::io::Error::other)?;
        self.write_bytes(key, &bytes)
    }

    /// Write `bytes` beside, then rename them into place, removing the
    /// least recently used past the limit.
    fn write_bytes(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let p = self.path(key);
        let n = TMP.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .dir
            .join(format!(".{key}.{}.{n}.tmp", std::process::id()));
        if let Err(e) = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &p)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        self.evict()
    }

    /// The bytes kept.
    pub fn size(&self) -> u64 {
        self.files().iter().map(|f| f.1).sum()
    }

    /// Our PNGs, oldest first, with their sizes.
    fn files(&self) -> Vec<(std::time::SystemTime, u64, PathBuf)> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<_> = dir
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                m.is_file().then_some(())?;
                Some((m.modified().ok()?, m.len(), e.path()))
            })
            .collect();
        files.sort();
        files
    }

    fn evict(&self) -> std::io::Result<()> {
        let files = self.files();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        for (_, len, path) in files {
            if total <= self.limit {
                break;
            }
            // Only the app's own cache files (`*.png` here) are removed.
            std::fs::remove_file(&path)?;
            total -= len;
        }
        // Temporary files left by a crash, a day on.
        if let Ok(dir) = std::fs::read_dir(&self.dir) {
            let day = std::time::Duration::from_secs(24 * 3600);
            for e in dir.filter_map(|e| e.ok()) {
                let name = e.file_name();
                let name = name.to_string_lossy();
                let old = e
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > day);
                if old && name.starts_with('.') && name.ends_with(".tmp") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Ok(())
    }
}

/// What a PNG here holds.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pixels {
    /// Masks.
    Gray8,
    /// Object removal's fills (samples big-endian, as PNG holds them).
    Rgb16,
}

impl Pixels {
    fn bytes(self) -> usize {
        match self {
            Pixels::Gray8 => 1,
            Pixels::Rgb16 => 6,
        }
    }

    fn png(self) -> (png::ColorType, png::BitDepth) {
        match self {
            Pixels::Gray8 => (png::ColorType::Grayscale, png::BitDepth::Eight),
            Pixels::Rgb16 => (png::ColorType::Rgb, png::BitDepth::Sixteen),
        }
    }
}

/// A PNG of `px`.
fn encode(mask: &[u8], w: usize, h: usize, px: Pixels) -> Result<Vec<u8>, String> {
    if mask.len() != w * h * px.bytes() || w == 0 || h == 0 {
        return Err("the mask is the wrong size".into());
    }
    let err = |e: png::EncodingError| e.to_string();
    let (pw, ph) = (
        u32::try_from(w).map_err(|e| e.to_string())?,
        u32::try_from(h).map_err(|e| e.to_string())?,
    );
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, pw, ph);
        let (colour, depth) = px.png();
        enc.set_color(colour);
        enc.set_depth(depth);
        enc.set_compression(png::Compression::Fast);
        let mut wr = enc.write_header().map_err(err)?;
        wr.write_image_data(mask).map_err(err)?;
    }
    Ok(out)
}

/// A PNG written by [`encode`], if it's whole, `w`×`h` and holds `px`.
fn decode(bytes: &[u8], w: usize, h: usize, px: Pixels) -> Option<Vec<u8>> {
    let mut r = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .ok()?;
    let info = r.info();
    if (info.width as usize, info.height as usize) != (w, h)
        || (info.color_type, info.bit_depth) != px.png()
    {
        return None;
    }
    let mut buf = vec![0; r.output_buffer_size()?];
    let frame = r.next_frame(&mut buf).ok()?;
    buf.truncate(frame.buffer_size());
    (buf.len() == w * h * px.bytes()).then_some(buf)
}

/// A 16-bit grayscale PNG of values 0–1.
fn encode16(m: &super::Coarse) -> Result<Vec<u8>, String> {
    let (w, h) = (m.width, m.height);
    if m.data.len() != w * h || w == 0 || h == 0 {
        return Err("the map is the wrong size".into());
    }
    let err = |e: png::EncodingError| e.to_string();
    let (pw, ph) = (
        u32::try_from(w).map_err(|e| e.to_string())?,
        u32::try_from(h).map_err(|e| e.to_string())?,
    );
    let data: Vec<u8> = m
        .data
        .iter()
        .flat_map(|v| ((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes())
        .collect();
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, pw, ph);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Sixteen);
        enc.set_compression(png::Compression::Fast);
        let mut wr = enc.write_header().map_err(err)?;
        wr.write_image_data(&data).map_err(err)?;
    }
    Ok(out)
}

/// A PNG written by [`encode16`], if it's whole.
fn decode16(bytes: &[u8]) -> Option<super::Coarse> {
    let mut r = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .ok()?;
    let info = r.info();
    let (w, h) = (info.width as usize, info.height as usize);
    if w == 0
        || h == 0
        || info.color_type != png::ColorType::Grayscale
        || info.bit_depth != png::BitDepth::Sixteen
    {
        return None;
    }
    let mut buf = vec![0; r.output_buffer_size()?];
    let frame = r.next_frame(&mut buf).ok()?;
    buf.truncate(frame.buffer_size());
    (buf.len() == w * h * 2).then(|| super::Coarse {
        width: w,
        height: h,
        data: buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f32::from(u16::from_be_bytes(*b)) / 65535.0)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_round_trip_in_sixteen_bits() {
        let m = super::super::Coarse {
            width: 5,
            height: 3,
            data: (0..15).map(|i| i as f32 / 14.0).collect(),
        };
        let b = encode16(&m).unwrap_or_default();
        let back = decode16(&b).unwrap_or_else(|| m.clone());
        assert_eq!((back.width, back.height), (5, 3));
        for (a, b) in m.data.iter().zip(&back.data) {
            assert!((a - b).abs() < 1e-5);
        }
        assert!(decode16(&b[..b.len() / 2]).is_none(), "cut short");
        // An 8-bit mask isn't a map.
        let m8 = encode(&[1, 2, 3, 4], 2, 2, Pixels::Gray8).unwrap_or_default();
        assert!(decode16(&m8).is_none());
    }

    #[test]
    fn pngs_round_trip() {
        let m: Vec<u8> = (0..35).map(|i| (i * 7) as u8).collect();
        let g = Pixels::Gray8;
        let b = encode(&m, 7, 5, g).unwrap_or_default();
        assert_eq!(decode(&b, 7, 5, g), Some(m));
        assert_eq!(decode(&b, 5, 7, g), None, "the wrong size");
        assert_eq!(decode(&b[..b.len() / 2], 7, 5, g), None, "cut short");
        assert_eq!(decode(b"not a png", 7, 5, g), None);
        assert_eq!(decode(&b, 7, 5, Pixels::Rgb16), None, "not a fill");
        let rgb: Vec<u8> = (0..7 * 5 * 6).map(|i| (i * 13) as u8).collect();
        let b = encode(&rgb, 7, 5, Pixels::Rgb16).unwrap_or_default();
        assert_eq!(decode(&b, 7, 5, Pixels::Rgb16), Some(rgb));
        assert_eq!(decode(&b, 7, 5, g), None, "not a mask");
    }

    #[test]
    fn keys_cover_everything_asked() {
        let rgb = vec![10u8; 4 * 3 * 3];
        let k = key("m", "subject", &[], &rgb, 4, 3);
        assert_eq!(k, key("m", "subject", &[], &rgb, 4, 3));
        assert_ne!(k, key("m2", "subject", &[], &rgb, 4, 3));
        assert_ne!(k, key("m", "person", &[], &rgb, 4, 3));
        assert_ne!(k, key("m", "subject", &[], &rgb, 3, 4));
        assert_ne!(k, key("m", "subject", &[(1.0, 1.0, true)], &rgb, 4, 3));
        let mut other = rgb.clone();
        other[5] = 11;
        assert_ne!(k, key("m", "subject", &[], &other, 4, 3));
        let a = key("m", "object", &[(1.0, 1.0, true)], &rgb, 4, 3);
        assert_ne!(a, key("m", "object", &[(1.0, 1.0, false)], &rgb, 4, 3));
        assert_ne!(a, key("m", "object", &[(1.0, 1.5, true)], &rgb, 4, 3));
    }
}
