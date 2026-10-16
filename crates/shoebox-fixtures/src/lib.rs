//! Deterministic synthetic fixtures for tests.
//!
//! Real camera files are large and personal, so CI builds a small fake library
//! instead: NEF-structured TIFF containers with an embedded full-size JPEG in a
//! SubIFD (as Nikon writes `JpgFromRaw`), byte-identical carved previews,
//! Lightroom-style TIFF exports, recompressed copies and unrelated images.
//! Real files go in `tests/fixtures/real/` (gitignored).

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::{self, FilterType};
use image::{ExtendedColorType, ImageEncoder, RgbImage};

/// Small xorshift PRNG so fixtures don't depend on a `rand` version.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }
}

/// A photo-like procedural image: gradient sky, soft blobs, bands and grain.
pub fn scene(seed: u64, w: u32, h: u32) -> RgbImage {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let base = [rng.f(), rng.f(), rng.f()];
    let blobs: Vec<_> = (0..9)
        .map(|_| {
            (
                rng.f(),
                rng.f(),
                0.05 + 0.25 * rng.f(),
                [rng.f(), rng.f(), rng.f()],
                if rng.f() > 0.5 { 1.0 } else { -1.0 },
            )
        })
        .collect();
    let (fx, fy, phase) = (
        2.0 + 5.0 * rng.f(),
        1.0 + 4.0 * rng.f(),
        std::f32::consts::TAU * rng.f(),
    );
    let mut grain = Rng(seed ^ 0xABCD);
    RgbImage::from_fn(w, h, |x, y| {
        let u = x as f32 / w as f32;
        let v = y as f32 / h as f32;
        let mut c = [0f32; 3];
        for (k, ck) in c.iter_mut().enumerate() {
            *ck = base[k] * (1.0 - 0.6 * v) + 0.15 * (u * fx * 3.0 + v * fy * 3.0 + phase).sin();
        }
        for &(bx, by, r, col, sign) in &blobs {
            let d2 = ((u - bx).powi(2) + (v - by).powi(2)) / (r * r);
            let a = (-d2).exp() * sign * 0.7;
            for k in 0..3 {
                c[k] += a * col[k];
            }
        }
        let n = (grain.f() - 0.5) * 0.04;
        let px = |f: f32| ((f + n).clamp(0.0, 1.0) * 255.0) as u8;
        image::Rgb([px(c[0]), px(c[1]), px(c[2])])
    })
}

pub fn jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

pub fn tiff(img: &RgbImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    image::codecs::tiff::TiffEncoder::new(&mut out).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(out.into_inner())
}

pub fn png(img: &RgbImage) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

pub fn resize_long(img: &RgbImage, long: u32) -> RgbImage {
    let s = long as f32 / img.width().max(img.height()) as f32;
    let w = (img.width() as f32 * s).round() as u32;
    let h = (img.height() as f32 * s).round() as u32;
    imageops::resize(img, w, h, FilterType::Lanczos3)
}

/// A strong edit: 10% crop on each side plus a contrast curve, like a
/// Lightroom virtual-copy export.
pub fn edited_variant(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    let crop = imageops::crop_imm(img, w / 10, h / 10, w * 8 / 10, h * 8 / 10).to_image();
    let mut out = crop;
    for p in out.pixels_mut() {
        for c in p.0.iter_mut() {
            let f = *c as f32 / 255.0;
            let s = 1.0 / (1.0 + (-(f - 0.5) * 8.0).exp());
            *c = (s * 255.0) as u8;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Minimal little-endian TIFF writer for EXIF blocks and fake NEFs.

pub enum Val {
    Short(Vec<u16>),
    Long(Vec<u32>),
    Ascii(String),
    Rational(Vec<(u32, u32)>),
}

impl Val {
    fn encode(&self) -> (u16, u32, Vec<u8>) {
        match self {
            Val::Short(v) => (
                3,
                v.len() as u32,
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
            Val::Long(v) => (
                4,
                v.len() as u32,
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
            Val::Ascii(s) => {
                let mut b = s.as_bytes().to_vec();
                b.push(0);
                (2, b.len() as u32, b)
            }
            Val::Rational(v) => (
                5,
                v.len() as u32,
                v.iter()
                    .flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes()))
                    .collect(),
            ),
        }
    }
}

#[derive(Default)]
pub struct TiffWriter {
    buf: Vec<u8>,
}

impl TiffWriter {
    pub fn new() -> Self {
        let mut buf = b"II*\0".to_vec();
        buf.extend(0u32.to_le_bytes()); // first IFD, patched in finish()
        Self { buf }
    }

    fn align(&mut self) {
        if self.buf.len() % 2 == 1 {
            self.buf.push(0);
        }
    }

    pub fn blob(&mut self, data: &[u8]) -> u32 {
        self.align();
        let at = self.buf.len() as u32;
        self.buf.extend_from_slice(data);
        at
    }

    /// Write an IFD (entries sorted by tag) with the given next-IFD pointer.
    pub fn ifd(&mut self, mut entries: Vec<(u16, Val)>, next: u32) -> u32 {
        entries.sort_by_key(|(t, _)| *t);
        let encoded: Vec<_> = entries.iter().map(|(t, v)| (*t, v.encode())).collect();
        // External values first, so the IFD can point at them.
        let mut values = Vec::new();
        for (_, (_, _, bytes)) in &encoded {
            values.push(if bytes.len() > 4 {
                Some(self.blob(bytes))
            } else {
                None
            });
        }
        self.align();
        let at = self.buf.len() as u32;
        self.buf.extend((encoded.len() as u16).to_le_bytes());
        for ((tag, (typ, count, bytes)), ext) in encoded.iter().zip(values) {
            self.buf.extend(tag.to_le_bytes());
            self.buf.extend(typ.to_le_bytes());
            self.buf.extend(count.to_le_bytes());
            match ext {
                Some(off) => self.buf.extend(off.to_le_bytes()),
                None => {
                    let mut inline = bytes.clone();
                    inline.resize(4, 0);
                    self.buf.extend(inline);
                }
            }
        }
        self.buf.extend(next.to_le_bytes());
        at
    }

    pub fn finish(mut self, ifd0: u32) -> Vec<u8> {
        self.buf[4..8].copy_from_slice(&ifd0.to_le_bytes());
        self.buf
    }
}

/// Capture metadata for a synthetic frame.
#[derive(Clone)]
pub struct Shot {
    pub datetime: &'static str,
    pub subsec: &'static str,
    pub orientation: u16,
    pub serial: &'static str,
}

fn exif_entries(w: &mut TiffWriter, shot: &Shot) -> u32 {
    w.ifd(
        vec![
            (0x9003, Val::Ascii(shot.datetime.into())),
            (0x9291, Val::Ascii(shot.subsec.into())),
            (0x9011, Val::Ascii("+09:00".into())),
            (0x829A, Val::Rational(vec![(1, 250)])),
            (0x829D, Val::Rational(vec![(28, 10)])),
            (0x8827, Val::Short(vec![400])),
            (0x920A, Val::Rational(vec![(400, 10)])),
            (0xA431, Val::Ascii(shot.serial.into())),
            (0xA434, Val::Ascii("NIKKOR Z 40mm f/2".into())),
        ],
        0,
    )
}

fn ifd0_camera(shot: &Shot, exif: u32) -> Vec<(u16, Val)> {
    vec![
        (0x010F, Val::Ascii("NIKON CORPORATION".into())),
        (0x0110, Val::Ascii("NIKON Z f".into())),
        (0x0112, Val::Short(vec![shot.orientation])),
        (0x0132, Val::Ascii(shot.datetime.into())),
        (0x8769, Val::Long(vec![exif])),
    ]
}

/// A NEF-shaped TIFF: IFD0 (thumbnail descriptor + camera tags), IFD1 with a
/// small JPEG preview, SubIFD0 with the full-size `JpgFromRaw`, SubIFD1 with
/// fake raw strip data.
pub fn nef(embedded_jpeg: &[u8], dims: (u32, u32), small_jpeg: &[u8], shot: &Shot) -> Vec<u8> {
    let mut w = TiffWriter::new();
    let full = w.blob(embedded_jpeg);
    let small = w.blob(small_jpeg);
    let mut raw = Rng(7);
    let raw_bytes: Vec<u8> = (0..4096).map(|_| raw.next() as u8).collect();
    let raw_at = w.blob(&raw_bytes);
    let sub0 = w.ifd(
        vec![
            (0x00FE, Val::Long(vec![0])),
            (0x0100, Val::Long(vec![dims.0])),
            (0x0101, Val::Long(vec![dims.1])),
            (0x0103, Val::Short(vec![6])),
            (0x0201, Val::Long(vec![full])),
            (0x0202, Val::Long(vec![embedded_jpeg.len() as u32])),
        ],
        0,
    );
    let sub1 = w.ifd(
        vec![
            (0x00FE, Val::Long(vec![0])),
            (0x0100, Val::Long(vec![dims.0])),
            (0x0101, Val::Long(vec![dims.1])),
            (0x0102, Val::Short(vec![14])),
            (0x0103, Val::Short(vec![34713])),
            (0x0111, Val::Long(vec![raw_at])),
            (0x0117, Val::Long(vec![raw_bytes.len() as u32])),
        ],
        0,
    );
    let ifd1 = w.ifd(
        vec![
            (0x0103, Val::Short(vec![6])),
            (0x0201, Val::Long(vec![small])),
            (0x0202, Val::Long(vec![small_jpeg.len() as u32])),
        ],
        0,
    );
    let exif = exif_entries(&mut w, shot);
    let mut e0 = ifd0_camera(shot, exif);
    e0.extend([
        (0x00FE, Val::Long(vec![1])),
        (0x0100, Val::Long(vec![160])),
        (0x0101, Val::Long(vec![120])),
        (0x014A, Val::Long(vec![sub0, sub1])),
    ]);
    let ifd0 = w.ifd(e0, ifd1);
    w.finish(ifd0)
}

/// Insert an EXIF APP1 segment straight after SOI.
pub fn with_exif(jpeg: &[u8], shot: &Shot) -> Vec<u8> {
    let mut w = TiffWriter::new();
    let exif = exif_entries(&mut w, shot);
    let ifd0 = w.ifd(ifd0_camera(shot, exif), 0);
    let tiff = w.finish(ifd0);
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend([0xFF, 0xE1]);
    out.extend(((app1.len() + 2) as u16).to_be_bytes());
    out.extend(app1);
    out.extend(&jpeg[2..]);
    out
}

pub const SIDECAR: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="3"/>
 </rdf:RDF>
</x:xmpmeta>
"#;

/// Paths of the generated library, relative to its root.
pub struct Library {
    pub root: PathBuf,
    pub reference: PathBuf,
    pub scan: PathBuf,
}

impl Library {
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

fn write(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, bytes).with_context(|| format!("writing {}", p.display()))
}

/// Build the synthetic library under `root` (which should be empty).
///
/// ```text
/// ref/2026/2026-04-24/DSC_0001.NEF   A, landscape, Orientation 1
/// ref/2026/2026-04-24/DSC_0001.JPG   A re-encoded q85 with EXIF
/// ref/2026/2026-04-24/DSC_0002.NEF   B, portrait, stored on-sensor with Orientation 6
/// ref/lightroom/DSC_0001.tif         A at 1280 px
/// ref/lightroom/DSC_0001-2.tif       A cropped + contrast (virtual copy)
/// ref/lightroom/DSC_0002.tif         B upright at 1280 px
/// scan/recovered/f0001.jpg (+.xmp)   byte-identical carve of DSC_0001.NEF JpgFromRaw
/// scan/recovered/f0002.jpg           byte-identical carve of DSC_0002.NEF JpgFromRaw
/// scan/recovered/f0003.jpg           A at 900 px, q70, no EXIF
/// scan/recovered/f0004.jpg           exact copy of DSC_0001.JPG
/// scan/recovered/f0005.jpg           C, unrelated
/// scan/recovered/f0006.jpg           exact copy of f0005 (scan-only duplicate)
/// scan/misc/f0007.png                A at 600 px
/// scan/rotated/f0008.jpg             B on-sensor (unrotated), 1000 px, q75, no EXIF
/// scan/recovered/f0009.jpg           blank white frame, 900 px
/// scan/recovered/f0010.jpg           A with EXIF 1 s after DSC_0001 (a burst neighbour)
/// ref/blank/DSC_0099.JPG             blank white frame, full size
/// ```
pub fn build_library(root: &Path) -> Result<Library> {
    let a = scene(1, 1800, 1200);
    let b_upright = scene(2, 1200, 1800);
    let b_sensor = imageops::rotate270(&b_upright); // Orientation 6 rotates it back
    let c = scene(3, 1800, 1200);

    let shot1 = Shot {
        datetime: "2026:04:24 14:03:11",
        subsec: "42",
        orientation: 1,
        serial: "6012345",
    };
    let shot2 = Shot {
        datetime: "2026:04:24 14:05:02",
        subsec: "07",
        orientation: 6,
        serial: "6012345",
    };

    let a_full = jpeg(&a, 92)?;
    let a_small = jpeg(&resize_long(&a, 320), 80)?;
    let b_full = jpeg(&b_sensor, 92)?;
    let b_small = jpeg(&resize_long(&b_sensor, 320), 80)?;
    let camera_jpg = with_exif(&jpeg(&a, 85)?, &shot1);
    let c_jpg = jpeg(&c, 88)?;

    write(
        root,
        "ref/2026/2026-04-24/DSC_0001.NEF",
        &nef(&a_full, a.dimensions(), &a_small, &shot1),
    )?;
    write(root, "ref/2026/2026-04-24/DSC_0001.JPG", &camera_jpg)?;
    write(
        root,
        "ref/2026/2026-04-24/DSC_0002.NEF",
        &nef(&b_full, b_sensor.dimensions(), &b_small, &shot2),
    )?;
    write(
        root,
        "ref/lightroom/DSC_0001.tif",
        &tiff(&resize_long(&a, 1280))?,
    )?;
    write(
        root,
        "ref/lightroom/DSC_0001-2.tif",
        &tiff(&resize_long(&edited_variant(&a), 1280))?,
    )?;
    write(
        root,
        "ref/lightroom/DSC_0002.tif",
        &tiff(&resize_long(&b_upright, 1280))?,
    )?;

    write(root, "scan/recovered/f0001.jpg", &a_full)?;
    write(root, "scan/recovered/f0001.jpg.xmp", SIDECAR.as_bytes())?;
    write(root, "scan/recovered/f0002.jpg", &b_full)?;
    write(
        root,
        "scan/recovered/f0003.jpg",
        &jpeg(&resize_long(&a, 900), 70)?,
    )?;
    write(root, "scan/recovered/f0004.jpg", &camera_jpg)?;
    write(root, "scan/recovered/f0005.jpg", &c_jpg)?;
    write(root, "scan/recovered/f0006.jpg", &c_jpg)?;
    write(root, "scan/misc/f0007.png", &png(&resize_long(&a, 600))?)?;
    write(
        root,
        "scan/rotated/f0008.jpg",
        &jpeg(&resize_long(&b_sensor, 1000), 75)?,
    )?;
    let white = RgbImage::from_pixel(1800, 1200, image::Rgb([255, 255, 255]));
    write(root, "ref/blank/DSC_0099.JPG", &jpeg(&white, 90)?)?;
    write(
        root,
        "scan/recovered/f0009.jpg",
        &jpeg(&resize_long(&white, 900), 70)?,
    )?;
    let burst = Shot {
        datetime: "2026:04:24 14:03:12",
        ..shot1.clone()
    };
    write(
        root,
        "scan/recovered/f0010.jpg",
        &with_exif(&jpeg(&a, 80)?, &burst),
    )?;

    Ok(Library {
        root: root.to_path_buf(),
        reference: root.join("ref"),
        scan: root.join("scan"),
    })
}
