//! Export (`docs/EDITING-SPEC.md`, Export): presets, resizing, encoding and
//! writing new files. Rendering is the CPU pipe at full resolution; this
//! module takes it from there. A linear DNG ([`linear_dng`]) is written
//! from the source instead, not a render.
//!
//! A name that's taken gets `-1`, `-2`, unless the person chose to
//! replace it ([`write_replacing`]): exports only, outside the library.

use std::io::Write;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use shoebox_decode::raw::RawImage;

use crate::cpu::{self, Demosaiced, Referred, Rgb, Target, to_u8, to_u16};
use crate::edit::Settings;
use crate::encode::{Avif, Jpeg, LinearDng, avif, icc, jpeg, linear_dng as linear_dng_bytes};
use crate::overlay::{self, Border, Watermark};

/// A named set of export settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub format: FileFormat,
    #[serde(default)]
    pub space: Space,
    #[serde(default)]
    pub size: OutputSize,
    #[serde(default)]
    pub metadata: Metadata,
    /// Outside the library, or beside the original in it.
    #[serde(default)]
    pub destination: Destination,
    /// With [`Destination::Folder`], where files go; None:
    /// `~/Pictures/Shoebox Exports/<name>`.
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// File name: `{name}` (the original's), `{preset}`, `{date}`
    /// (YYYY-MM-DD, capture), `{seq}` (1, 2, … in this export).
    #[serde(default = "default_naming")]
    pub naming: String,
    /// A border around the photo (inside the preset's size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<Border>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<Watermark>,
}

fn default_naming() -> String {
    "{name}_{preset}".into()
}

/// Where a preset's files go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    /// A folder outside the library. Shoebox doesn't keep track of these.
    #[default]
    Folder,
    /// Beside the original, joined to its stack: an edited version that
    /// lives in the library, flagged when the edit changes since, and
    /// re-exported in place.
    Library,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileFormat {
    Jpeg {
        quality: u8,
        #[serde(default)]
        full_chroma: bool,
        #[serde(default = "yes")]
        progressive: bool,
    },
    /// 16 bits a channel.
    Tiff { compression: TiffCompression },
    /// 16 bits a channel.
    Png,
    /// sRGB or Display P3 only (AVIF describes colour by CICP).
    Avif {
        quality: u8,
        #[serde(default = "yes")]
        ten_bit: bool,
        /// rav1e's speed, 1 (slow, smaller) to 10.
        #[serde(default = "avif_speed")]
        speed: u8,
    },
    /// The source, demosaiced and AI-denoised as the edit has it, as
    /// camera-native linear RGB (see [`linear_dng`]): nothing else of the
    /// edit, so no size, space, border or watermark. Raw photos only.
    LinearDng,
}

fn yes() -> bool {
    true
}

fn avif_speed() -> u8 {
    crate::encode::AVIF_SPEED
}

impl FileFormat {
    pub fn extension(self) -> &'static str {
        match self {
            FileFormat::Jpeg { .. } => "jpg",
            FileFormat::Tiff { .. } => "tif",
            FileFormat::Png => "png",
            FileFormat::Avif { .. } => "avif",
            FileFormat::LinearDng => "dng",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FileFormat::Jpeg { .. } => "JPEG",
            FileFormat::Tiff { .. } => "TIFF (16-bit)",
            FileFormat::Png => "PNG (16-bit)",
            FileFormat::Avif { .. } => "AVIF",
            FileFormat::LinearDng => "Linear DNG",
        }
    }

    /// The colour spaces the format holds.
    pub fn spaces(self) -> &'static [Space] {
        match self {
            FileFormat::Avif { .. } => &[Space::Srgb, Space::DisplayP3],
            // Camera-native: no output space.
            FileFormat::LinearDng => &[],
            _ => &[
                Space::Srgb,
                Space::DisplayP3,
                Space::AdobeRgb,
                Space::ProPhoto,
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TiffCompression {
    Lzw,
    Zip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Space {
    #[default]
    Srgb,
    DisplayP3,
    AdobeRgb,
    ProPhoto,
}

impl Space {
    pub fn target(self) -> Target {
        match self {
            Space::Srgb => Target::Srgb,
            Space::DisplayP3 => Target::DisplayP3,
            Space::AdobeRgb => Target::AdobeRgb,
            Space::ProPhoto => Target::ProPhoto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Space::Srgb => "sRGB",
            Space::DisplayP3 => "Display P3",
            Space::AdobeRgb => "Adobe RGB",
            Space::ProPhoto => "ProPhoto RGB",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputSize {
    #[default]
    Full,
    /// The long edge at most this many pixels.
    LongEdge { px: u32 },
    /// At most this many megapixels.
    Megapixels { mp: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metadata {
    #[default]
    All,
    AllButLocation,
    CopyrightOnly,
}

/// The presets Shoebox starts with.
pub fn starters() -> Vec<Preset> {
    vec![
        Preset {
            name: "JPEG · Full size".into(),
            format: FileFormat::Jpeg {
                quality: 92,
                full_chroma: true,
                progressive: true,
            },
            space: Space::Srgb,
            size: OutputSize::Full,
            metadata: Metadata::All,
            destination: Destination::Folder,
            folder: None,
            naming: default_naming(),
            border: None,
            watermark: None,
        },
        Preset {
            name: "JPEG · Web 2048".into(),
            format: FileFormat::Jpeg {
                quality: 85,
                full_chroma: false,
                progressive: true,
            },
            space: Space::Srgb,
            size: OutputSize::LongEdge { px: 2048 },
            metadata: Metadata::AllButLocation,
            destination: Destination::Folder,
            folder: None,
            naming: default_naming(),
            border: None,
            watermark: None,
        },
        Preset {
            name: "TIFF · 16-bit".into(),
            format: FileFormat::Tiff {
                compression: TiffCompression::Zip,
            },
            space: Space::AdobeRgb,
            size: OutputSize::Full,
            metadata: Metadata::All,
            destination: Destination::Folder,
            folder: None,
            naming: default_naming(),
            border: None,
            watermark: None,
        },
    ]
}

/// The output size for a `w`×`h` render (never larger than the render).
pub fn output_size(w: usize, h: usize, s: OutputSize) -> (usize, usize) {
    let scale = match s {
        OutputSize::Full => 1.0,
        OutputSize::LongEdge { px } => f64::from(px) / w.max(h) as f64,
        OutputSize::Megapixels { mp } => (f64::from(mp) * 1e6 / (w * h) as f64).sqrt(),
    }
    .min(1.0);
    (
        ((w as f64 * scale).round() as usize).max(1),
        ((h as f64 * scale).round() as usize).max(1),
    )
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let p = std::f64::consts::PI * x;
        p.sin() / p
    }
}

fn lanczos3(x: f64) -> f64 {
    if x.abs() < 3.0 {
        sinc(x) * sinc(x / 3.0)
    } else {
        0.0
    }
}

/// Weights for resampling `src` samples to `dst`: per output sample, the
/// first source index and its normalised weights.
fn weights(src: usize, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = src as f64 / dst as f64;
    let support = 3.0 * scale.max(1.0);
    (0..dst)
        .map(|i| {
            let centre = (i as f64 + 0.5) * scale;
            let first = (centre - support).floor().max(0.0) as usize;
            let last = ((centre + support).ceil() as usize).min(src);
            let mut w: Vec<f64> = (first..last)
                .map(|j| lanczos3((j as f64 + 0.5 - centre) / scale.max(1.0)))
                .collect();
            let sum: f64 = w.iter().sum();
            if sum.abs() > 1e-12 {
                w.iter_mut().for_each(|v| *v /= sum);
            }
            (first, w.into_iter().map(|v| v as f32).collect())
        })
        .collect()
}

/// Resize with Lanczos-3 (separable; horizontal, then vertical).
pub fn resize(img: &Rgb, w: usize, h: usize) -> Rgb {
    if (w, h) == (img.width, img.height) {
        return img.clone();
    }
    let wx = weights(img.width, w);
    let mut mid = Rgb::new(w, img.height);
    mid.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, line)| {
            for (x, (first, ws)) in wx.iter().enumerate() {
                let mut s = [0.0f32; 3];
                for (k, wt) in ws.iter().enumerate() {
                    let p = img.px(first + k, y);
                    for c in 0..3 {
                        s[c] += wt * p[c];
                    }
                }
                line[x * 3..x * 3 + 3].copy_from_slice(&s);
            }
        });
    let wy = weights(img.height, h);
    let mut out = Rgb::new(w, h);
    out.data
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, line)| {
            let (first, ws) = &wy[y];
            for x in 0..w {
                let mut s = [0.0f32; 3];
                for (k, wt) in ws.iter().enumerate() {
                    let p = mid.px(x, first + k);
                    for c in 0..3 {
                        s[c] += wt * p[c];
                    }
                }
                // Lanczos rings a little past the ends: keep display range.
                line[x * 3..x * 3 + 3].copy_from_slice(&s.map(|v| v.clamp(0.0, 1.0)));
            }
        });
    out
}

/// A full-size render (display values in the preset's space) as the file
/// will hold it: resized to the preset's size, then its border and
/// watermark. `default_font`: for a text watermark that names no font.
pub fn finish(img: &Rgb, preset: &Preset, default_font: &[u8]) -> Result<Rgb, String> {
    let target = preset.space.target();
    let (w, h) = output_size(img.width, img.height, preset.size);
    let ((iw, ih), px) = overlay::inside(w, h, preset.border.as_ref());
    let mut out = resize(img, iw, ih);
    if let Some(b) = &preset.border
        && px > 0
    {
        out = overlay::add_border(&out, px, overlay::from_srgb(b.colour, target));
    }
    if let Some(m) = &preset.watermark {
        overlay::draw(&mut out, (px, px, iw, ih), m, target, default_font)?;
    }
    Ok(out)
}

/// Encode display values (in `preset`'s space) as the preset's file.
pub fn encode(img: &Rgb, preset: &Preset) -> Result<Vec<u8>, String> {
    let target = preset.space.target();
    match preset.format {
        FileFormat::Jpeg {
            quality,
            full_chroma,
            progressive,
        } => jpeg(
            img,
            target,
            Jpeg {
                quality,
                full_chroma,
                progressive,
            },
        ),
        FileFormat::Tiff { compression } => tiff16(img, target, compression),
        FileFormat::Png => png16(img, target),
        FileFormat::Avif {
            quality,
            ten_bit,
            speed,
        } => avif(
            img,
            target,
            Avif {
                quality,
                ten_bit,
                speed,
            },
        ),
        FileFormat::LinearDng => {
            Err("a linear DNG is written from the source, not a render".into())
        }
    }
}

/// A linear DNG of a raw photo, for archiving it AI-denoised (EDITING-SPEC,
/// Export): the source as the pipe starts from it (the edit's raw settings:
/// black and white points, hot pixels, highlights and demosaic), blended
/// with the AI-denoised source at the edit's Strength, as camera-native
/// linear RGB with its matrices and as-shot white, so another raw editor
/// renders it from scratch. Nothing after that is baked in: no detail
/// stage, spots, lens corrections, geometry or look (DECISIONS, "AVIF and
/// linear DNG export"). Turned upright (Orientation 1), as every export is;
/// clipped at the sensor's white. From a linear DNG (an earlier export), the
/// AI denoise it records is carried over: there's none to add.
pub fn linear_dng(
    meta: &RawImage,
    src: &Demosaiced,
    s: &Settings,
    orientation: u16,
) -> Result<Vec<u8>, String> {
    if src.referred != Referred::Scene {
        return Err("a linear DNG is made from a raw photo".into());
    }
    let ai = &s.modules.denoise_ai;
    let carried = meta.linear.map(|l| l.noise_reduction.unwrap_or(0.0));
    if carried.is_none() && ai.enabled && src.ai_ready().is_none() {
        return Err("AI denoise hasn't run on this photo (is its model downloaded?)".into());
    }
    let base = src.base(s);
    let img = base.as_deref().unwrap_or(&src.rgb);
    let mult = src.as_shot;
    // As-shot balance undone, upright, as 16 bits: in one pass, without
    // another float copy of a 24 MP image.
    let (w, h) = cpu::upright(img.width, img.height, orientation);
    let data: Vec<u16> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            (0..w).flat_map(move |x| {
                let (sx, sy) = stored_at(img.width, img.height, orientation, x, y);
                let p = img.px(sx, sy);
                [0, 1, 2]
                    .map(|c| ((p[c] / mult[c].max(1e-6)).clamp(0.0, 1.0) * 65535.0 + 0.5) as u16)
            })
        })
        .collect();
    // A preview at the code defaults, as a raw file's would be.
    let small = Demosaiced::new(cpu::downscale(img, 256), mult, Referred::Scene);
    let o = cpu::Options {
        long_edge: None,
        orientation,
        target: Target::Srgb,
    };
    let preview = cpu::render_from(meta, &small, &Settings::default(), &o, true);
    let thumb = to_u8(&preview);
    linear_dng_bytes(&LinearDng {
        width: w,
        height: h,
        data: &data,
        make: &meta.make,
        model: &meta.model,
        neutral: mult.map(|m| 1.0 / m.max(1e-6)),
        matrices: &meta.matrices,
        noise_reduction: match (carried, src.ai_strength(s)) {
            (Some(v), _) => v,
            (None, Some(k)) if ai.enabled => k,
            _ => 0.0,
        },
        thumbnail: Some((preview.width, preview.height, &thumb)),
    })
}

/// The stored pixel (of a `w`×`h` image with EXIF `orientation`) shown at
/// `(x, y)` once it's upright.
pub fn stored_at(w: usize, h: usize, orientation: u16, x: usize, y: usize) -> (usize, usize) {
    match orientation {
        2 => (w - 1 - x, y),
        3 => (w - 1 - x, h - 1 - y),
        4 => (x, h - 1 - y),
        5 => (y, x),
        6 => (y, h - 1 - x),
        7 => (w - 1 - y, h - 1 - x),
        8 => (w - 1 - y, x),
        _ => (x, y),
    }
}

/// A 16-bit PNG with the space's ICC profile (iCCP).
fn png16(img: &Rgb, target: Target) -> Result<Vec<u8>, String> {
    let err = |e: png::EncodingError| e.to_string();
    let mut info = png::Info::with_size(img.width as u32, img.height as u32);
    info.color_type = png::ColorType::Rgb;
    info.bit_depth = png::BitDepth::Sixteen;
    info.icc_profile = Some(icc(target)?.into());
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::with_info(&mut out, info).map_err(err)?;
        enc.set_compression(png::Compression::Balanced);
        let mut w = enc.write_header().map_err(err)?;
        // PNG samples are big-endian.
        let data: Vec<u8> = to_u16(img).iter().flat_map(|v| v.to_be_bytes()).collect();
        w.write_image_data(&data).map_err(err)?;
    }
    Ok(out)
}

fn tiff16(img: &Rgb, target: Target, c: TiffCompression) -> Result<Vec<u8>, String> {
    use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};
    let err = |e: tiff::TiffError| e.to_string();
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let enc = TiffEncoder::new(&mut out)
            .map_err(err)?
            .with_compression(match c {
                TiffCompression::Lzw => Compression::Lzw,
                TiffCompression::Zip => Compression::Deflate(DeflateLevel::Balanced),
            });
        let mut enc = enc;
        let mut image = enc
            .new_image::<colortype::RGB16>(img.width as u32, img.height as u32)
            .map_err(err)?;
        let profile = icc(target)?;
        image
            .encoder()
            .write_tag(tiff::tags::Tag::IccProfile, &profile[..])
            .map_err(err)?;
        image.write_data(&to_u16(img)).map_err(err)?;
    }
    Ok(out.into_inner())
}

/// Fill in a naming template. Characters a file name can't hold become
/// `_`.
pub fn file_stem(
    template: &str,
    name: &str,
    preset: &str,
    date: Option<&str>,
    seq: usize,
) -> String {
    let s = template
        .replace("{name}", name)
        .replace("{preset}", preset)
        .replace("{date}", date.unwrap_or("undated"))
        .replace("{seq}", &seq.to_string());
    let s: String = s
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().trim_start_matches('.').to_owned();
    if s.is_empty() { name.to_owned() } else { s }
}

/// Write `bytes` as a new file `<dir>/<stem>.<ext>`, or `<stem>-1.<ext>`,
/// `-2`, … if taken: never replacing anything. Returns the path written.
pub fn write_new(dir: &Path, stem: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for n in 0..10_000u32 {
        let name = if n == 0 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(bytes)
                    .and_then(|()| f.sync_all())
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Err(format!("no free name for {stem} in {}", dir.display()))
}

/// Write `bytes` as `<dir>/<stem>.<ext>`, replacing a file already there
/// (the person chose to): written beside it first, then renamed over it, so
/// a failed write leaves the old file as it was. The temporary file has a
/// random name and is created exclusively, so nothing already in the folder
/// (a symlink, say) is ever opened or written through; the rename replaces
/// a link at the destination itself, not what it points to.
pub fn write_replacing(dir: &Path, stem: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{stem}.{ext}"));
    let mut tmp = tempfile::Builder::new()
        .prefix(".shoebox-export-")
        .tempfile_in(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    tmp.write_all(bytes)
        .and_then(|()| tmp.as_file().sync_all())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    tmp.persist(&path)
        .map_err(|e| format!("{}: {}", path.display(), e.error))?;
    Ok(path)
}
