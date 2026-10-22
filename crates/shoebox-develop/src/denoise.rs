//! AI raw denoise (EDITING-SPEC, Noise reduction, tier 2): a model that
//! denoises and demosaics the raw mosaic in one, in place of RCD. The app
//! registers it (RawNIND's UtNet2 in `shoebox-ml`); this module feeds it
//! and keeps its results.
//!
//! - [`run`]: the mosaic (levels applied, not white-balanced) packed into
//!   RGGB planes at half resolution, denoised, unpacked to full resolution
//!   and balanced at as shot, as RCD's output is. Where RCD's highlight
//!   reconstruction has rebuilt clipped photosites, its result is kept.
//! - [`Cache`]: results on disk (half floats), by the file's content, the
//!   raw settings and the model, the oldest removed past a limit.
//! - The live pipe blends it with RCD's by the Strength slider
//!   (`Demosaiced::base`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use rayon::prelude::*;
use shoebox_decode::raw::RawImage;

use crate::cpu::{self, Rgb};
use crate::edit::Raw;
use crate::wb;

/// Towards clipping (this fraction of it and up), RCD's result takes
/// over: the model's channels clip unevenly, which colours bright lights.
const BLEND: f32 = 0.75;

/// Denoises packed RGGB planes (four `h`×`w`, 0–1): camera RGB planes at
/// `2h`×`2w`. Hears progress (0–1); stops when the flag is set.
pub type Denoiser = dyn Fn(&[f32], usize, usize, &dyn Fn(f32), &AtomicBool) -> Result<Vec<f32>, String>
    + Send
    + Sync;

struct Registered {
    model: String,
    run: Box<Denoiser>,
}

static DENOISER: OnceLock<Registered> = OnceLock::new();

/// Give Develop a raw denoiser, named `model` (results are kept by it).
/// Once.
pub fn register(model: &str, run: Box<Denoiser>) {
    let _ = DENOISER.set(Registered {
        model: model.to_owned(),
        run,
    });
}

/// The registered model's name.
pub fn model() -> Option<&'static str> {
    DENOISER.get().map(|d| d.model.as_str())
}

/// The photo's mosaic denoised and demosaiced by the model, full size,
/// balanced at as shot (RCD's form). Takes seconds.
pub fn run(
    raw: &RawImage,
    r: &Raw,
    progress: &dyn Fn(f32),
    cancel: &AtomicBool,
) -> Result<Rgb, String> {
    let Some(d) = DENOISER.get() else {
        return Err("no AI denoiser in this build".into());
    };
    let (w, h) = (raw.width, raw.height);
    if w < 32 || h < 32 {
        return Err("too small".into());
    }
    if !raw.is_mosaic() {
        return Err("a linear DNG is demosaiced and denoised already".into());
    }
    // Where red is: packing from there makes any 2×2 Bayer layout RGGB.
    let Some((dy, dx)) = (0..2)
        .flat_map(|y| (0..2).map(move |x| (y, x)))
        .find(|&(y, x)| raw.cfa[y][x] == 0)
    else {
        return Err("not a Bayer sensor".into());
    };
    if raw.cfa[dy][1 - dx] != 1 || raw.cfa[1 - dy][dx] != 1 || raw.cfa[1 - dy][1 - dx] != 2 {
        return Err("not a Bayer sensor".into());
    }
    let mosaic = cpu::levelled(raw, r);
    let (ph, pw) = ((h - dy) / 2, (w - dx) / 2);
    let mut packed = vec![0f32; 4 * ph * pw];
    packed
        .par_chunks_mut(ph * pw)
        .enumerate()
        .for_each(|(c, plane)| {
            let (oy, ox) = [(0, 0), (0, 1), (1, 0), (1, 1)][c];
            for y in 0..ph {
                for x in 0..pw {
                    let (sy, sx) = (dy + 2 * y + oy, dx + 2 * x + ox);
                    plane[y * pw + x] = mosaic[sy * w + sx].clamp(0.0, 1.0);
                }
            }
        });
    let planes = (d.run)(&packed, ph, pw, progress, cancel)?;
    if planes.len() != 3 * 4 * ph * pw {
        return Err("the denoiser's result is the wrong size".into());
    }
    // Back to full size (the rows and columns the packing skipped copy
    // their neighbours), balanced at as shot.
    let mult = wb::resolve(raw, &Default::default()).multipliers;
    let (oh, ow) = (2 * ph, 2 * pw);
    let mut out = Rgb::new(w, h);
    out.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, line)| {
            let sy = (y.saturating_sub(dy)).min(oh - 1);
            for x in 0..w {
                let sx = (x.saturating_sub(dx)).min(ow - 1);
                for c in 0..3 {
                    line[x * 3 + c] = planes[(c * oh + sy) * ow + sx].max(0.0) * mult[c];
                }
            }
        });
    // The model's gain was matched to its packed input; RCD's demosaic
    // of the same mosaic is the better measure of how bright it should
    // be (the input's mean counts green twice).
    // Only where nothing is near clipping: there RCD rebuilds what the
    // model can't, and would pull the match.
    let rcd = cpu::demosaic(raw, r, None);
    let clip = cpu::raw::OPPOSED_CLIP;
    let level = |b: &[f32]| (0..3).map(|c| b[c] / mult[c]).fold(0f32, f32::max);
    let (want, got) = out
        .data
        .par_chunks(3)
        .zip(rcd.rgb.data.par_chunks(3))
        .filter(|(_, b)| level(b) < BLEND * clip)
        .map(|(a, b)| {
            let sum = |p: &[f32]| p.iter().map(|&v| f64::from(v)).sum::<f64>();
            (sum(b), sum(a))
        })
        .reduce(|| (0.0, 0.0), |x, y| (x.0 + y.0, x.1 + y.1));
    if got > 1e-9 {
        let k = (want / got) as f32;
        out.data.par_iter_mut().for_each(|v| *v *= k);
    }
    // Clipped highlights: RCD's reconstruction (the model never learned
    // to rebuild them).
    out.data
        .par_chunks_mut(3)
        .zip(rcd.rgb.data.par_chunks(3))
        .for_each(|(a, b)| {
            let t = ((level(b) - BLEND * clip) / ((1.0 - BLEND) * clip)).clamp(0.0, 1.0);
            let k = t * t * (3.0 - 2.0 * t);
            for c in 0..3 {
                a[c] += (b[c] - a[c]) * k;
            }
        });
    Ok(out)
}

/// The key results are kept by: the file's content hash, the raw settings
/// and the model.
pub fn key(file_hash: &str, r: &Raw) -> String {
    let settings = serde_json::to_string(r).unwrap_or_default();
    let model = model().unwrap_or("none");
    let k = blake3::hash(format!("{file_hash}\n{settings}\n{model}").as_bytes());
    k.to_hex().to_string()
}

/// Results on disk.
pub struct Cache {
    dir: PathBuf,
    limit: u64,
}

/// The default limit: about twenty full-size results.
pub const DEFAULT_LIMIT: u64 = 3 << 30;

impl Cache {
    pub fn new(dir: &Path, limit: u64) -> Self {
        Self {
            dir: dir.to_path_buf(),
            limit,
        }
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.f16"))
    }

    /// A kept result (its time touched, so it's kept longest).
    pub fn get(&self, key: &str) -> Option<Rgb> {
        let p = self.path(key);
        let bytes = std::fs::read(&p).ok()?;
        let _ = std::fs::File::options()
            .append(true)
            .open(&p)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
        decode(&bytes)
    }

    /// Keep a result (written beside, then renamed into place), removing
    /// the oldest past the limit.
    pub fn put(&self, key: &str, img: &Rgb) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let p = self.path(key);
        let tmp = p.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, encode(img))?;
        std::fs::rename(&tmp, &p)?;
        self.evict()
    }

    fn evict(&self) -> std::io::Result<()> {
        let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "f16"))
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                Some((m.modified().ok()?, m.len(), e.path()))
            })
            .collect();
        files.sort();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        for (_, len, path) in files {
            if total <= self.limit {
                break;
            }
            // Only the app's own cache files (`*.f16` here) are removed.
            std::fs::remove_file(&path)?;
            total -= len;
        }
        Ok(())
    }
}

const MAGIC: &[u8; 8] = b"SBAIDN01";

fn encode(img: &Rgb) -> Vec<u8> {
    let mut out = Vec::with_capacity(24 + img.data.len() * 2);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(img.width as u64).to_le_bytes());
    out.extend_from_slice(&(img.height as u64).to_le_bytes());
    for v in &img.data {
        out.extend_from_slice(&half::f16::from_f32(*v).to_le_bytes());
    }
    out
}

fn decode(b: &[u8]) -> Option<Rgb> {
    if b.len() < 24 || &b[..8] != MAGIC {
        return None;
    }
    let w = usize::try_from(u64::from_le_bytes(b[8..16].try_into().ok()?)).ok()?;
    let h = usize::try_from(u64::from_le_bytes(b[16..24].try_into().ok()?)).ok()?;
    let n = w.checked_mul(h)?.checked_mul(3)?;
    if b.len() != 24 + n * 2 {
        return None;
    }
    let mut img = Rgb::new(w, h);
    img.data
        .par_iter_mut()
        .zip(b[24..].par_chunks(2))
        .for_each(|(o, c)| *o = half::f16::from_le_bytes([c[0], c[1]]).to_f32());
    Some(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_round_trip() {
        let mut img = Rgb::new(3, 2);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = i as f32 * 0.37;
        }
        let back = decode(&encode(&img)).unwrap_or_else(|| Rgb::new(0, 0));
        assert_eq!((back.width, back.height), (3, 2));
        for (a, b) in img.data.iter().zip(&back.data) {
            assert!((a - b).abs() <= a.abs() * 1e-3);
        }
        assert!(decode(b"not a file").is_none());
    }
}
