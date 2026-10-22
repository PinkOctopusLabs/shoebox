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
    Byte(Vec<u8>),
    Undefined(Vec<u8>),
    Short(Vec<u16>),
    Long(Vec<u32>),
    Ascii(String),
    Rational(Vec<(u32, u32)>),
    SRational(Vec<(i32, i32)>),
}

impl Val {
    fn encode(&self) -> (u16, u32, Vec<u8>) {
        match self {
            Val::Byte(v) => (1, v.len() as u32, v.clone()),
            Val::Undefined(v) => (7, v.len() as u32, v.clone()),
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
            Val::SRational(v) => (
                10,
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

/// A synthetic Bayer DNG, as rawler reads one: 16-bit uncompressed CFA data.
pub struct Dng {
    pub width: u32,
    pub height: u32,
    /// CFA colours (0 R, 1 G, 2 B) at `(row % 2, col % 2)` of the full image.
    pub cfa: [[u8; 2]; 2],
    pub black: u16,
    pub white: u16,
    /// `(left, top, right, bottom)` of the active area, if any.
    pub active: Option<(u32, u32, u32, u32)>,
    /// Default crop origin and size within the active area, if any.
    pub crop: Option<((u32, u32), (u32, u32))>,
    /// AsShotNeutral (camera RGB of a neutral, green 1.0).
    pub neutral: [f32; 3],
    /// ColorMatrix1 (XYZ → camera, row by row) and its illuminant (17 = A).
    pub matrix1: ([f32; 9], u16),
    /// ColorMatrix2 and its illuminant (21 = D65).
    pub matrix2: Option<([f32; 9], u16)>,
    pub make: String,
    pub model: String,
    /// ISO, in an Exif IFD, if any.
    pub iso: Option<u16>,
    /// A JPEG preview (NewSubFileType 1, in a SubIFD, as Adobe DNG
    /// Converter writes them), if any.
    pub preview: Option<Vec<u8>>,
    /// Capture metadata (an Exif IFD, and IFD 0's orientation), if any;
    /// it replaces `iso`.
    pub shot: Option<Shot>,
    /// The exposure time `shot` records, as a fraction (1/250 if None).
    pub exposure: Option<(u32, u32)>,
}

impl Dng {
    /// A 2×2-RGGB test DNG with Z f-like levels and matrices.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            cfa: [[0, 1], [1, 2]],
            black: 1008,
            white: 15892,
            active: None,
            crop: None,
            neutral: [0.5, 1.0, 0.7],
            matrix1: (
                [
                    1.3904, -0.7947, 0.0654, -0.432, 1.2105, 0.2497, -0.0235, 0.083, 0.9243,
                ],
                17,
            ),
            matrix2: Some((
                [
                    1.1607, -0.4491, -0.0977, -0.4522, 1.246, 0.2304, -0.0458, 0.1519, 0.7616,
                ],
                21,
            )),
            make: "Shoebox".into(),
            model: "Test Sensor".into(),
            iso: None,
            preview: None,
            shot: None,
            exposure: None,
        }
    }

    /// The file, with photosite values from `value(x, y)` (raw counts).
    pub fn write(&self, value: impl Fn(u32, u32) -> u16) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((self.width * self.height * 2) as usize);
        for y in 0..self.height {
            for x in 0..self.width {
                pixels.extend(value(x, y).to_le_bytes());
            }
        }
        let srat = |m: &[f32; 9]| {
            Val::SRational(
                m.iter()
                    .map(|v| ((v * 10000.0).round() as i32, 10000))
                    .collect(),
            )
        };
        let mut w = TiffWriter::new();
        let strip = w.blob(&pixels);
        let mut entries = vec![
            (0x00FE, Val::Long(vec![0])),
            (0x0100, Val::Long(vec![self.width])),
            (0x0101, Val::Long(vec![self.height])),
            (0x0102, Val::Short(vec![16])),
            (0x0103, Val::Short(vec![1])),
            (0x0106, Val::Short(vec![32803])),
            (0x010F, Val::Ascii(self.make.clone())),
            (0x0110, Val::Ascii(self.model.clone())),
            (0x0111, Val::Long(vec![strip])),
            (
                0x0112,
                Val::Short(vec![self.shot.as_ref().map_or(1, |s| s.orientation)]),
            ),
            (0x0115, Val::Short(vec![1])),
            (0x0116, Val::Long(vec![self.height])),
            (0x0117, Val::Long(vec![pixels.len() as u32])),
            (0x011C, Val::Short(vec![1])),
            (0x828D, Val::Short(vec![2, 2])),
            (
                0x828E,
                Val::Byte(vec![
                    self.cfa[0][0],
                    self.cfa[0][1],
                    self.cfa[1][0],
                    self.cfa[1][1],
                ]),
            ),
            (0xC612, Val::Byte(vec![1, 4, 0, 0])),
            (0xC613, Val::Byte(vec![1, 1, 0, 0])),
            (0xC614, Val::Ascii(format!("{} {}", self.make, self.model))),
            (0xC61A, Val::Short(vec![self.black])),
            (0xC61D, Val::Short(vec![self.white])),
            (0xC621, srat(&self.matrix1.0)),
            (
                0xC628,
                Val::Rational(
                    self.neutral
                        .iter()
                        .map(|v| ((v * 10000.0).round() as u32, 10000))
                        .collect(),
                ),
            ),
            (0xC65A, Val::Short(vec![self.matrix1.1])),
        ];
        if let Some((m, ill)) = &self.matrix2 {
            entries.push((0xC622, srat(m)));
            entries.push((0xC65B, Val::Short(vec![*ill])));
        }
        if let Some((l, t, r, b)) = self.active {
            entries.push((0xC68D, Val::Long(vec![t, l, b, r])));
        }
        if let Some(((ox, oy), (cw, ch))) = self.crop {
            entries.push((0xC61F, Val::Long(vec![ox, oy])));
            entries.push((0xC620, Val::Long(vec![cw, ch])));
        }
        if let Some(shot) = &self.shot {
            let exif = exif_entries_at(&mut w, shot, self.exposure.unwrap_or((1, 250)));
            entries.push((0x8769, Val::Long(vec![exif])));
        } else if let Some(iso) = self.iso {
            let exif = w.ifd(vec![(0x8827, Val::Short(vec![iso]))], 0);
            entries.push((0x8769, Val::Long(vec![exif])));
        }
        if let Some(jpeg) = &self.preview {
            let at = w.blob(jpeg);
            let (pw, ph) =
                image::load_from_memory(jpeg).map_or((0, 0), |i| (i.width(), i.height()));
            let sub = w.ifd(
                vec![
                    (0x00FE, Val::Long(vec![1])),
                    (0x0100, Val::Long(vec![pw])),
                    (0x0101, Val::Long(vec![ph])),
                    (0x0102, Val::Short(vec![8, 8, 8])),
                    (0x0103, Val::Short(vec![7])),
                    (0x0106, Val::Short(vec![6])),
                    (0x0111, Val::Long(vec![at])),
                    (0x0115, Val::Short(vec![3])),
                    (0x0116, Val::Long(vec![ph])),
                    (0x0117, Val::Long(vec![jpeg.len() as u32])),
                ],
                0,
            );
            entries.push((0x014A, Val::Long(vec![sub])));
        }
        let ifd0 = w.ifd(entries, 0);
        w.finish(ifd0)
    }
}

/// A TIFF carrying just a Nikon maker note that records `compression` the
/// way a Z body does (tag 0x51) or an older body does (tag 0x93).
pub fn nikon_compression_stub(compression: u16, z_body: bool) -> Vec<u8> {
    let mut inner = TiffWriter::new();
    let entry = if z_body {
        let mut block = vec![0u8; 24];
        block[10..12].copy_from_slice(&compression.to_le_bytes());
        (0x0051, Val::Undefined(block))
    } else {
        (0x0093, Val::Short(vec![compression]))
    };
    let ifd = inner.ifd(vec![(0x0001, Val::Undefined(b"0211".to_vec())), entry], 0);
    let mut note = b"Nikon\0\x02\x11\0\0".to_vec();
    note.extend(inner.finish(ifd));

    let mut w = TiffWriter::new();
    let exif = w.ifd(vec![(0x927C, Val::Undefined(note))], 0);
    let ifd0 = w.ifd(
        vec![
            (0x010F, Val::Ascii("NIKON CORPORATION".into())),
            (0x8769, Val::Long(vec![exif])),
        ],
        0,
    );
    w.finish(ifd0)
}

/// An uncompressed 8-bit RGB TIFF (one strip) with an embedded ICC profile,
/// like a Lightroom export in Adobe RGB or ProPhoto.
pub fn tiff_with_icc(img: &RgbImage, icc: &[u8]) -> Vec<u8> {
    let mut w = TiffWriter::new();
    let strip = w.blob(img.as_raw());
    let (width, height) = img.dimensions();
    let ifd0 = w.ifd(
        vec![
            (0x0100, Val::Long(vec![width])),
            (0x0101, Val::Long(vec![height])),
            (0x0102, Val::Short(vec![8, 8, 8])),
            (0x0103, Val::Short(vec![1])),
            (0x0106, Val::Short(vec![2])),
            (0x0111, Val::Long(vec![strip])),
            (0x0115, Val::Short(vec![3])),
            (0x0116, Val::Long(vec![height])),
            (0x0117, Val::Long(vec![img.as_raw().len() as u32])),
            (0x011C, Val::Short(vec![1])),
            (0x8773, Val::Undefined(icc.to_vec())),
        ],
        0,
    );
    w.finish(ifd0)
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
    exif_entries_at(w, shot, (1, 250))
}

fn exif_entries_at(w: &mut TiffWriter, shot: &Shot, exposure: (u32, u32)) -> u32 {
    w.ifd(
        vec![
            (0x9003, Val::Ascii(shot.datetime.into())),
            (0x9291, Val::Ascii(shot.subsec.into())),
            (0x9011, Val::Ascii("+09:00".into())),
            (0x829A, Val::Rational(vec![exposure])),
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

/// Smooth value noise in 0–1: random values on a grid of `cell` pixels,
/// blended with smoothstep (corners where cells meet, for alignment).
fn value_noise(x: f64, y: f64, cell: f64, seed: u64) -> f64 {
    let (gx, gy) = (x / cell, y / cell);
    let (ix, iy) = (gx.floor(), gy.floor());
    let s = |t: f64| t * t * (3.0 - 2.0 * t);
    let (fx, fy) = (s(gx - ix), s(gy - iy));
    let at = |i: f64, j: f64| {
        let mut r = Rng((i as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (j as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ seed
            | 1);
        r.next();
        f64::from(r.f())
    };
    let top = at(ix, iy) * (1.0 - fx) + at(ix + 1.0, iy) * fx;
    let bottom = at(ix, iy + 1.0) * (1.0 - fx) + at(ix + 1.0, iy + 1.0) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// The HDR test scene's light at `(x, y)` of a `w`×`h` view, relative to
/// the sensor's white at the 0 EV frame's exposure: a textured room (0.02–
/// 0.17), a dark corner (about 0.005), and a window of sky 1.4–3.2 (clipped
/// at 0 EV, held by −2 EV) with dark glazing bars.
pub fn hdr_light(x: f64, y: f64, w: f64, h: f64) -> f64 {
    let (u, v) = (x / w, y / h);
    if (0.56..0.86).contains(&u) && (0.14..0.56).contains(&v) {
        if (u - 0.71).abs() < 0.012 || (v - 0.35).abs() < 0.015 {
            return 0.06;
        }
        let down = (v - 0.14) / 0.42;
        return 1.4 + 1.5 * (1.0 - down) + 0.3 * value_noise(x, y, 9.0, 3);
    }
    if u < 0.22 && v > 0.7 {
        return 0.003 + 0.004 * value_noise(x, y, 11.0, 5);
    }
    0.02 + 0.15 * (0.6 * value_noise(x, y, 14.0, 1) + 0.4 * value_noise(x, y, 5.0, 2))
}

/// One frame of a synthetic HDR bracket: [`hdr_light`] seen `ev` stops
/// brighter than 0 EV, moved and turned (hand-held), with sensor noise and
/// clipping, as a raw DNG with EXIF (ISO 400, the exposure time).
#[derive(Debug, Clone)]
pub struct Bracket {
    pub width: u32,
    pub height: u32,
    pub ev: f64,
    /// Where the scene sits in this frame: frame pixel `p` sees the scene
    /// at `R(rotate)·(p − centre) + centre + shift`.
    pub shift: (f64, f64),
    /// Degrees.
    pub rotate: f64,
    /// Something in the room only this frame saw: `[x0, y0, x1, y1]` of the
    /// scene, and its light.
    pub mover: Option<([f64; 4], f64)>,
    pub seed: u64,
    pub datetime: &'static str,
    pub subsec: &'static str,
}

impl Bracket {
    pub fn new(width: u32, height: u32, ev: f64) -> Self {
        Self {
            width,
            height,
            ev,
            shift: (0.0, 0.0),
            rotate: 0.0,
            mover: None,
            seed: 1,
            datetime: "2026:10:22 14:30:00",
            subsec: "10",
        }
    }

    fn centre(&self) -> (f64, f64) {
        (
            (f64::from(self.width) - 1.0) / 2.0,
            (f64::from(self.height) - 1.0) / 2.0,
        )
    }

    /// Frame pixel → scene (the 0 EV frame's pixels, when it doesn't move).
    pub fn to_scene(&self, x: f64, y: f64) -> (f64, f64) {
        let (cx, cy) = self.centre();
        let (s, c) = self.rotate.to_radians().sin_cos();
        let (dx, dy) = (x - cx, y - cy);
        (
            c * dx - s * dy + cx + self.shift.0,
            s * dx + c * dy + cy + self.shift.1,
        )
    }

    /// Scene → frame pixel.
    pub fn to_frame(&self, x: f64, y: f64) -> (f64, f64) {
        let (cx, cy) = self.centre();
        let (s, c) = self.rotate.to_radians().sin_cos();
        let (dx, dy) = (x - cx - self.shift.0, y - cy - self.shift.1);
        (c * dx + s * dy + cx, -s * dx + c * dy + cy)
    }

    /// The light at frame pixel `(x, y)`.
    pub fn light(&self, x: f64, y: f64) -> f64 {
        let (sx, sy) = self.to_scene(x, y);
        if let Some(([x0, y0, x1, y1], m)) = self.mover
            && (x0..x1).contains(&sx)
            && (y0..y1).contains(&sy)
        {
            return m;
        }
        hdr_light(sx, sy, f64::from(self.width), f64::from(self.height))
    }

    pub fn write(&self) -> Vec<u8> {
        let dng = Dng {
            shot: Some(Shot {
                datetime: self.datetime,
                subsec: self.subsec,
                orientation: 1,
                serial: "7654321",
            }),
            exposure: Some(((2f64.powf(self.ev) * 4000.0).round() as u32, 1_000_000)),
            ..Dng::new(self.width, self.height)
        };
        let gain = 2f64.powf(self.ev);
        let range = f64::from(dng.white - dng.black);
        // ISO 400's noise, as the merge models it: shot and read.
        let (a, b) = (8e-5, 1.6e-8);
        let mut rng = Rng(self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
        let mut noise = move || {
            let u1 = (f64::from(rng.f()) + 1e-4).min(1.0);
            let u2 = f64::from(rng.f());
            (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
        };
        let mut px = vec![0u16; (self.width * self.height) as usize];
        for y in 0..self.height {
            for x in 0..self.width {
                let c = usize::from(dng.cfa[(y % 2) as usize][(x % 2) as usize]);
                let v = self.light(f64::from(x), f64::from(y)) * f64::from(dng.neutral[c]) * gain;
                let v = v + noise() * (a * v.max(0.0) + b).sqrt();
                let counts = f64::from(dng.black) + v.clamp(0.0, 1.0) * range;
                px[(y * self.width + x) as usize] = counts.round() as u16;
            }
        }
        let w = self.width;
        dng.write(|x, y| px[(y * w + x) as usize])
    }
}

/// The depth of the focus test scene's disc (0 near … 1 far).
pub const FOCUS_DISC_DEPTH: f64 = 0.1;

/// The focus test scene's wall at `(x, y)` of a `w`×`h` view: its light
/// (camera RGB, the sensor's white 1) and depth, a textured wall slanting
/// away from near (left, depth 0) to far (right, depth 1). Fine texture
/// (cells of 2.5 and 6 pixels), so a pixel of blur shows.
pub fn focus_wall(x: f64, y: f64, w: f64, _h: f64) -> ([f64; 3], f64) {
    let t = 0.55 * value_noise(x, y, 2.5, 21) + 0.45 * value_noise(x, y, 6.0, 22);
    let v = 0.04 + 0.16 * t;
    ([0.55 * v, v, 0.75 * v], (x / w).clamp(0.0, 1.0))
}

/// The focus test scene's disc at `(x, y)`: its light, and how much of the
/// pixel it covers (a pixel's worth of soft edge). Brighter than the wall
/// and near the camera ([`FOCUS_DISC_DEPTH`]), in front of the wall's far
/// side: a depth edge.
pub fn focus_disc(x: f64, y: f64, w: f64, h: f64) -> ([f64; 3], f64) {
    let (cx, cy, r) = (0.72 * w, 0.5 * h, 0.2 * h);
    let a = (r - (x - cx).hypot(y - cy) + 0.5).clamp(0.0, 1.0);
    let t = 0.5 * value_noise(x, y, 2.5, 23) + 0.5 * value_noise(x, y, 5.0, 24);
    let v = 0.3 + 0.3 * t;
    ([0.6 * v, v, 0.5 * v], a)
}

/// A separable Gaussian blur of `c`-channel pixels, edges clamped.
fn gauss(img: &[f64], w: usize, h: usize, c: usize, sigma: f64) -> Vec<f64> {
    if sigma < 0.05 {
        return img.to_vec();
    }
    let r = (3.0 * sigma).ceil() as isize;
    let k: Vec<f64> = (-r..=r)
        .map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f64 = k.iter().sum();
    let k: Vec<f64> = k.iter().map(|v| v / sum).collect();
    let (wi, hi) = (w as isize, h as isize);
    let mut tmp = vec![0.0; img.len()];
    for y in 0..h {
        for x in 0..w {
            for ch in 0..c {
                let mut s = 0.0;
                for (j, kv) in k.iter().enumerate() {
                    let xx = (x as isize + j as isize - r).clamp(0, wi - 1) as usize;
                    s += kv * img[(y * w + xx) * c + ch];
                }
                tmp[(y * w + x) * c + ch] = s;
            }
        }
    }
    let mut out = vec![0.0; img.len()];
    for y in 0..h {
        for x in 0..w {
            for ch in 0..c {
                let mut s = 0.0;
                for (j, kv) in k.iter().enumerate() {
                    let yy = (y as isize + j as isize - r).clamp(0, hi - 1) as usize;
                    s += kv * tmp[(yy * w + x) * c + ch];
                }
                out[(y * w + x) * c + ch] = s;
            }
        }
    }
    out
}

/// One frame of a synthetic focus bracket of [`focus_wall`] and
/// [`focus_disc`]: focused at depth `focus`, everything else blurred by
/// how far it is from it (layered: the disc's blur spreads over the wall,
/// as a lens's does), magnified about the centre (focus breathing) and
/// shifted, with ISO 400's noise, as a raw DNG with EXIF.
#[derive(Debug, Clone)]
pub struct FocusFrame {
    pub width: u32,
    pub height: u32,
    /// The depth in focus (0 near … 1 far).
    pub focus: f64,
    /// Blur (a Gaussian's σ, pixels) per unit of depth from the focus.
    pub blur: f64,
    /// Frame pixel `p` sees the scene at `(p − centre) / scale + centre +
    /// shift`.
    pub scale: f64,
    pub shift: (f64, f64),
    /// Everything in focus: the truth a stack aims at.
    pub sharp: bool,
    pub noise: bool,
    pub seed: u64,
    pub subsec: &'static str,
}

impl FocusFrame {
    pub fn new(width: u32, height: u32, focus: f64) -> Self {
        Self {
            width,
            height,
            focus,
            blur: 5.0,
            scale: 1.0,
            shift: (0.0, 0.0),
            sharp: false,
            noise: true,
            seed: 1,
            subsec: "10",
        }
    }

    fn centre(&self) -> (f64, f64) {
        (
            (f64::from(self.width) - 1.0) / 2.0,
            (f64::from(self.height) - 1.0) / 2.0,
        )
    }

    /// Frame pixel → scene.
    pub fn to_scene(&self, x: f64, y: f64) -> (f64, f64) {
        let (cx, cy) = self.centre();
        (
            (x - cx) / self.scale + cx + self.shift.0,
            (y - cy) / self.scale + cy + self.shift.1,
        )
    }

    /// Scene → frame pixel.
    pub fn to_frame(&self, x: f64, y: f64) -> (f64, f64) {
        let (cx, cy) = self.centre();
        (
            (x - cx - self.shift.0) * self.scale + cx,
            (y - cy - self.shift.1) * self.scale + cy,
        )
    }

    /// The light each pixel sees (camera RGB, three a pixel, row by row),
    /// without noise.
    pub fn light(&self) -> Vec<f64> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (fw, fh) = (f64::from(self.width), f64::from(self.height));
        let mut wall = vec![0.0; w * h * 3];
        let mut depth = vec![0.0; w * h];
        let mut disc = vec![0.0; w * h * 3];
        let mut alpha = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = self.to_scene(x as f64, y as f64);
                let i = y * w + x;
                let (c, d) = focus_wall(sx, sy, fw, fh);
                let (f, a) = focus_disc(sx, sy, fw, fh);
                wall[i * 3..i * 3 + 3].copy_from_slice(&c);
                depth[i] = d;
                for k in 0..3 {
                    disc[i * 3 + k] = f[k] * a;
                }
                alpha[i] = a;
            }
        }
        if self.sharp {
            return (0..w * h * 3)
                .map(|j| disc[j] + wall[j] * (1.0 - alpha[j / 3]))
                .collect();
        }
        let sd = self.blur * (FOCUS_DISC_DEPTH - self.focus).abs();
        let disc = gauss(&disc, w, h, 3, sd);
        let alpha = gauss(&alpha, w, h, 1, sd);
        // The wall's blur varies with its depth: blurs every half pixel of
        // σ (each from the last), mixed per pixel between the two nearest.
        let most = depth
            .iter()
            .map(|d| self.blur * (d - self.focus).abs())
            .fold(0.0, f64::max);
        let n = (most / 0.5).ceil() as usize + 1;
        let mut out = vec![0.0; w * h * 3];
        let mut cur = wall;
        for j in 0..n {
            let step = 0.5 * ((2 * j + 1) as f64).sqrt();
            let next = gauss(&cur, w, h, 3, step);
            for i in 0..w * h {
                let t = self.blur * (depth[i] - self.focus).abs() / 0.5;
                if (t.floor() as usize).min(n - 1) != j {
                    continue;
                }
                let f = (t - j as f64).clamp(0.0, 1.0);
                for k in 0..3 {
                    let v = cur[i * 3 + k] * (1.0 - f) + next[i * 3 + k] * f;
                    out[i * 3 + k] = disc[i * 3 + k] + v * (1.0 - alpha[i]);
                }
            }
            cur = next;
        }
        out
    }

    pub fn write(&self) -> Vec<u8> {
        let dng = Dng {
            shot: Some(Shot {
                datetime: "2026:10:22 15:10:00",
                subsec: self.subsec,
                orientation: 1,
                serial: "7654321",
            }),
            exposure: Some((4000, 1_000_000)),
            ..Dng::new(self.width, self.height)
        };
        let light = self.light();
        let range = f64::from(dng.white - dng.black);
        // ISO 400's noise, as Bracket's.
        let (a, b) = if self.noise {
            (8e-5, 1.6e-8)
        } else {
            (0.0, 0.0)
        };
        let mut rng = Rng(self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
        let mut noise = move || {
            let u1 = (f64::from(rng.f()) + 1e-4).min(1.0);
            let u2 = f64::from(rng.f());
            (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
        };
        let w = self.width as usize;
        let mut px = vec![0u16; w * self.height as usize];
        for y in 0..self.height as usize {
            for x in 0..w {
                let c = usize::from(dng.cfa[y % 2][x % 2]);
                let v = light[(y * w + x) * 3 + c];
                let v = v + noise() * (a * v.max(0.0) + b).sqrt();
                let counts = f64::from(dng.black) + v.clamp(0.0, 1.0) * range;
                px[y * w + x] = counts.round() as u16;
            }
        }
        dng.write(|x, y| px[y as usize * w + x as usize])
    }
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
    with_exif_gps(jpeg, shot, None)
}

/// [`with_exif`] plus a GPS position (degrees; south and west negative).
pub fn with_exif_gps(jpeg: &[u8], shot: &Shot, gps: Option<(f64, f64)>) -> Vec<u8> {
    let mut w = TiffWriter::new();
    let exif = exif_entries(&mut w, shot);
    let mut tags = ifd0_camera(shot, exif);
    if let Some((lat, lon)) = gps {
        // Degrees, minutes and seconds (hundredths) as rationals.
        let dms = |v: f64| {
            let v = v.abs();
            let d = v.floor();
            let m = ((v - d) * 60.0).floor();
            let s = ((v - d) * 60.0 - m) * 60.0;
            Val::Rational(vec![
                (d as u32, 1),
                (m as u32, 1),
                ((s * 100.0).round() as u32, 100),
            ])
        };
        let gps_ifd = w.ifd(
            vec![
                (0x0001, Val::Ascii(if lat < 0.0 { "S" } else { "N" }.into())),
                (0x0002, dms(lat)),
                (0x0003, Val::Ascii(if lon < 0.0 { "W" } else { "E" }.into())),
                (0x0004, dms(lon)),
            ],
            0,
        );
        tags.push((0x8825, Val::Long(vec![gps_ifd])));
    }
    let ifd0 = w.ifd(tags, 0);
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
