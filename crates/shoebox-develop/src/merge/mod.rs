//! Photo merges (EDITING-SPEC, Phase 4 additions): photos a person chose
//! made into one new raw file, a floating-point linear DNG written beside
//! the first of them (DECISIONS, "Photo merges: rule 3 and the result" and
//! "Photo merges: groundwork"). Sources are only read.
//!
//! - [`load`]: a source as a merge reads it: the raw file decoded and
//!   demosaiced at the code defaults (no AI denoise), the as-shot balance
//!   undone, so camera-native linear RGB with the sensor's white at 1, as a
//!   linear DNG export holds it; turned upright; with the pixels the sensor
//!   clipped marked. A finished image (JPEG, TIFF) is refused: it isn't
//!   linear.
//! - [`align`]: global alignment (features and RANSAC, then a direct
//!   refinement on the pixels), for every kind of merge.
//! - [`hdr`]: bracketed exposures into one image of the scene's light.
//! - [`focus`]: frames focused at different distances into one sharp
//!   throughout.
//! - [`dng`]: the result as a float DNG: the reference source's size,
//!   matrices, as-shot white and EXIF, a preview at the code defaults, and
//!   an XMP record of what it was made from.

// Channel loops read clearer indexed than zipped.
#![allow(clippy::needless_range_loop)]

pub mod align;
pub mod focus;
pub mod hdr;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rawler::decoders::{RawDecodeParams, RawMetadata};
use rawler::rawsource::RawSource;
use rayon::prelude::*;
use shoebox_decode::raw::{CLIP, Linear, RawImage};

use crate::cpu::{self, Demosaiced, Options, Referred, Rgb, Target};
use crate::edit::{Raw, Settings};

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("stopped")]
    Cancelled,
    /// These photos can't be merged this way.
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Develop(#[from] crate::DevelopError),
}

pub type Result<T> = std::result::Result<T, MergeError>;

/// A photo to merge.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    /// The photo (named in the record and in errors).
    pub path: PathBuf,
    /// The file to decode: the photo, or a High efficiency NEF's DNG.
    pub raw_path: PathBuf,
    /// Its EXIF orientation (1–8), as the library has it.
    pub orientation: u16,
}

impl Input {
    pub fn new(path: impl Into<PathBuf>, orientation: u16) -> Self {
        let path = path.into();
        Self {
            raw_path: path.clone(),
            path,
            orientation,
        }
    }

    fn name(&self) -> String {
        file_name(&self.path)
    }
}

pub(crate) fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// How a photo was exposed, from its EXIF.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Exposure {
    pub seconds: Option<f64>,
    pub aperture: Option<f64>,
    pub iso: Option<f64>,
}

impl Exposure {
    fn from_metadata(m: &RawMetadata) -> Self {
        let ratio = |r: &rawler::formats::tiff::Rational| {
            (r.d != 0).then(|| f64::from(r.n) / f64::from(r.d))
        };
        let x = &m.exif;
        Self {
            seconds: x
                .exposure_time
                .as_ref()
                .and_then(ratio)
                .filter(|v| *v > 0.0),
            aperture: x.fnumber.as_ref().and_then(ratio).filter(|v| *v > 0.0),
            iso: x
                .iso_speed_ratings
                .map(f64::from)
                .or(x.iso_speed.map(f64::from))
                .or(x.recommended_exposure_index.map(f64::from))
                .filter(|v| *v > 0.0),
        }
    }

    /// How much light reached the sensor, in stops: log2 of time × ISO ÷
    /// f-number² (an f-number or ISO the file doesn't give counts as the
    /// same for every frame).
    pub fn stops(&self) -> Option<f64> {
        let t = self.seconds?;
        let n = self.aperture.unwrap_or(1.0);
        let iso = self.iso.unwrap_or(100.0);
        let v = (t * iso / (n * n)).log2();
        v.is_finite().then_some(v)
    }
}

/// A source as merges read it.
pub struct Source {
    pub path: PathBuf,
    /// Camera-native linear RGB (the as-shot balance undone), upright; the
    /// sensor's white is 1.0 in every channel.
    pub rgb: Rgb,
    /// 1 where the sensor clipped: any photosite within two pixels at or
    /// above [`CLIP`] (demosaic spreads them).
    pub clipped: Vec<u8>,
    /// The raw file's colour and camera (its data dropped).
    pub meta: RawImage,
    /// As-shot white balance multipliers (green 1).
    pub as_shot: [f32; 3],
    pub exposure: Exposure,
    /// EXIF as rawler reads it, to copy into the result.
    pub metadata: Option<RawMetadata>,
}

impl Source {
    pub fn width(&self) -> usize {
        self.rgb.width
    }

    pub fn height(&self) -> usize {
        self.rgb.height
    }
}

/// The EXIF rawler reads from a raw file (None if it can't).
pub fn metadata(src: &RawSource) -> Option<RawMetadata> {
    catch_unwind(AssertUnwindSafe(|| {
        let d = rawler::get_decoder(src).ok()?;
        d.raw_metadata(src, &RawDecodeParams::default()).ok()
    }))
    .ok()
    .flatten()
}

/// What a photo's EXIF says, without decoding it.
pub fn probe(input: &Input) -> Result<(Exposure, Option<RawMetadata>)> {
    let src = RawSource::new(&input.raw_path)
        .map_err(|e| MergeError::Failed(format!("{}: {e}", input.raw_path.display())))?;
    let m = metadata(&src);
    Ok((
        m.as_ref().map(Exposure::from_metadata).unwrap_or_default(),
        m,
    ))
}

/// Read a source (see the module's notes). `cancel` is looked at between
/// the slow steps.
pub fn load(input: &Input, cancel: &AtomicBool) -> Result<Source> {
    let format = shoebox_decode::Format::from_path(&input.path);
    if !format.is_some_and(shoebox_decode::Format::is_raw) {
        return Err(MergeError::Refused(format!(
            "{} isn't a raw photo: merges need raw files",
            input.name()
        )));
    }
    let bytes = Arc::new(
        std::fs::read(&input.raw_path)
            .map_err(|e| MergeError::Failed(format!("{}: {e}", input.raw_path.display())))?,
    );
    let src = RawSource::new_from_shared_vec(bytes.clone());
    let metadata = metadata(&src);
    drop(src);
    let mut raw = shoebox_decode::raw::decode(&bytes)
        .map_err(|e| MergeError::Failed(format!("{}: {e}", input.name())))?;
    drop(bytes);
    check(cancel)?;
    let (w, h) = (raw.width, raw.height);
    // Clipped photosites, spread over the pixels demosaic builds from them.
    let clipped_at: Vec<u8> = match raw.linear {
        Some(_) => raw
            .data
            .par_chunks(3)
            .map(|p| u8::from(p.iter().any(|&v| v >= CLIP)))
            .collect(),
        None => raw.data.par_iter().map(|&v| u8::from(v >= CLIP)).collect(),
    };
    let clipped_at = dilate(&clipped_at, w, h, 2);
    let d = cpu::demosaic(&raw, &Raw::default(), None);
    check(cancel)?;
    let as_shot = d.as_shot;
    let mut rgb = d.rgb;
    let inv = as_shot.map(|m| 1.0 / m.max(1e-6));
    rgb.data.par_chunks_mut(3).for_each(|p| {
        for c in 0..3 {
            p[c] *= inv[c];
        }
    });
    raw.data = Vec::new();
    let (rgb, clipped) = upright(rgb, clipped_at, input.orientation);
    Ok(Source {
        path: input.path.clone(),
        rgb,
        clipped,
        meta: raw,
        as_shot,
        exposure: metadata
            .as_ref()
            .map(Exposure::from_metadata)
            .unwrap_or_default(),
        metadata,
    })
}

pub(crate) fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(MergeError::Cancelled)
    } else {
        Ok(())
    }
}

/// A mask grown by `r` pixels (square).
pub(crate) fn dilate(m: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let mut rows = vec![0u8; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let src = &m[y * w..(y + 1) * w];
        for (x, o) in out.iter_mut().enumerate() {
            *o = src[x.saturating_sub(r)..(x + r + 1).min(w)]
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
        }
    });
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, o)| {
        for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
            for (v, s) in o.iter_mut().zip(&rows[yy * w..(yy + 1) * w]) {
                *v = (*v).max(*s);
            }
        }
    });
    out
}

/// An image and its mask turned upright (exact copies of pixels).
fn upright(rgb: Rgb, mask: Vec<u8>, orientation: u16) -> (Rgb, Vec<u8>) {
    if orientation <= 1 || orientation > 8 {
        return (rgb, mask);
    }
    let (sw, sh) = (rgb.width, rgb.height);
    let (w, h) = cpu::upright(sw, sh, orientation);
    let mut out = Rgb::new(w, h);
    let mut m = vec![0u8; w * h];
    out.data
        .par_chunks_mut(w * 3)
        .zip(m.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (row, mrow))| {
            for x in 0..w {
                let (sx, sy) = crate::export::stored_at(sw, sh, orientation, x, y);
                let i = sy * sw + sx;
                row[x * 3..x * 3 + 3].copy_from_slice(&rgb.data[i * 3..i * 3 + 3]);
                mrow[x] = mask[i];
            }
        });
    (out, m)
}

/// A merge's result, ready to write.
pub struct Merged {
    /// Camera-native linear RGB, upright, in the reference's units (its
    /// sensor's white is 1.0; brighter is kept).
    pub rgb: Rgb,
    /// Which input is the reference (size, colour, EXIF).
    pub reference: usize,
    /// The reference's colour and camera, sized as the result.
    pub meta: RawImage,
    pub as_shot: [f32; 3],
    pub metadata: Option<RawMetadata>,
}

/// What a merge was made from, recorded in its DNG.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// "hdr", "panorama", "focus".
    pub kind: String,
    /// Each source's file name and BLAKE3 (hex), in the order merged.
    pub sources: Vec<(String, String)>,
}

/// The Shoebox namespace for what merges record in XMP.
pub const XMP_NS: &str = "https://github.com/PinkOctopusLabs/shoebox/ns/merge/1.0/";

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

impl Record {
    /// An XMP packet: `shoebox:Merge` (the kind) and `shoebox:Sources`, a
    /// sequence of file names and BLAKE3s. Nothing the library reads as
    /// marks.
    pub fn xmp(&self) -> String {
        let items: String = self
            .sources
            .iter()
            .map(|(name, hash)| {
                format!(
                    "\n     <rdf:li rdf:parseType=\"Resource\">\
                     <shoebox:File>{}</shoebox:File><shoebox:Blake3>{}</shoebox:Blake3></rdf:li>",
                    escape(name),
                    escape(hash)
                )
            })
            .collect();
        format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Shoebox\">\n \
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
             <rdf:Description rdf:about=\"\"\n    \
             xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n    \
             xmlns:shoebox=\"{XMP_NS}\"\n    \
             xmp:CreatorTool=\"Shoebox\"\n    \
             shoebox:Merge=\"{}\">\n   \
             <shoebox:Sources>\n    <rdf:Seq>{items}\n    </rdf:Seq>\n   </shoebox:Sources>\n  \
             </rdf:Description>\n \
             </rdf:RDF>\n\
             </x:xmpmeta>\n\
             <?xpacket end=\"w\"?>",
            escape(&self.kind)
        )
    }
}

/// The DNG of a merge: [`crate::encode::float_dng`] with the reference's
/// colour, its EXIF (sub-seconds left out: the merge is no one exposure,
/// and doesn't stack with its sources by time), a 256-pixel thumbnail and
/// a 1024-pixel JPEG preview rendered at the code defaults, and `record`.
pub fn dng(m: &Merged, record: &Record) -> Result<Vec<u8>> {
    let (w, h) = (m.rgb.width, m.rgb.height);
    let mut meta = m.meta.clone();
    meta.width = w;
    meta.height = h;
    meta.data = Vec::new();
    meta.linear = Some(Linear::default());
    // Previews: the result balanced at as shot, as Develop opens it.
    let preview = |edge: usize| -> Rgb {
        let mut small = cpu::downscale(&m.rgb, edge);
        small.data.par_chunks_mut(3).for_each(|p| {
            for c in 0..3 {
                p[c] *= m.as_shot[c];
            }
        });
        let src = Demosaiced::new(small, m.as_shot, Referred::Scene);
        let o = Options {
            long_edge: None,
            orientation: 1,
            target: Target::Srgb,
        };
        cpu::render_from(&meta, &src, &Settings::default(), &o, true)
    };
    let thumb = preview(256);
    let thumb_px = cpu::to_u8(&thumb);
    let large = preview(1024);
    let jpeg = crate::encode::jpeg(
        &large,
        Target::Srgb,
        crate::encode::Jpeg {
            quality: 85,
            full_chroma: false,
            progressive: false,
        },
    )
    .map_err(MergeError::Failed)?;
    let metadata = m.metadata.clone().map(|mut md| {
        let x = &mut md.exif;
        x.sub_sec_time = None;
        x.sub_sec_time_original = None;
        x.sub_sec_time_digitized = None;
        // The pixels are upright; IFD 0 says so.
        x.orientation = None;
        md
    });
    let xmp = record.xmp();
    crate::encode::float_dng(&crate::encode::FloatDng {
        width: w,
        height: h,
        data: &m.rgb.data,
        make: &m.meta.make,
        model: &m.meta.model,
        neutral: m.as_shot.map(|v| 1.0 / v.max(1e-6)),
        matrices: &m.meta.matrices,
        thumbnail: Some((thumb.width, thumb.height, &thumb_px)),
        preview: Some(&jpeg),
        exif: metadata.as_ref(),
        xmp: Some(xmp.as_bytes()),
    })
    .map_err(MergeError::Failed)
}
