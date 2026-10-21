//! Borders and watermarks on exports (EDITING-SPEC, Tools beyond PhotoLab:
//! "Watermark and borders"), as export preset options. They're applied to
//! the finished, resized image in the preset's colour space: a border
//! around it (the file's long edge is still the preset's size: the photo
//! shrinks to make room, keeping its shape), then
//! a text or image watermark placed on the photo inside the border.
//!
//! Text is drawn from a font's outlines (`skrifa`) filled anti-aliased
//! (`tiny-skia`); an image watermark is a PNG, its alpha kept. Colours are
//! given in sRGB and converted to the export's space.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use skrifa::MetadataProvider;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};

use crate::colour;
use crate::cpu::{Rgb, Target, srgb_eotf};

/// A border of one colour around the photo.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Border {
    /// Width, in % of the file's long edge.
    pub width: f32,
    /// sRGB.
    pub colour: [u8; 3],
}

impl Default for Border {
    fn default() -> Self {
        Self {
            width: 3.0,
            colour: [255, 255, 255],
        }
    }
}

/// What a watermark shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mark {
    Text {
        text: String,
        /// A TrueType or OpenType font file; None: Shoebox's own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font: Option<PathBuf>,
        /// sRGB.
        colour: [u8; 3],
    },
    /// A PNG (its transparency is kept), taken as sRGB.
    Image { path: PathBuf },
}

/// Where a watermark sits on the photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Centre,
    Right,
    BottomLeft,
    Bottom,
    #[default]
    BottomRight,
}

impl Anchor {
    /// Top to bottom, left to right (a 3×3 grid).
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Centre,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Anchor::TopLeft => "Top left",
            Anchor::Top => "Top",
            Anchor::TopRight => "Top right",
            Anchor::Left => "Left",
            Anchor::Centre => "Centre",
            Anchor::Right => "Right",
            Anchor::BottomLeft => "Bottom left",
            Anchor::Bottom => "Bottom",
            Anchor::BottomRight => "Bottom right",
        }
    }

    /// 0 (left/top), 1 (middle) or 2 (right/bottom) across and down.
    fn cell(self) -> (usize, usize) {
        let i = Self::ALL.iter().position(|&a| a == self).unwrap_or(8);
        (i % 3, i / 3)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Watermark {
    pub mark: Mark,
    /// Text: the font size; an image: its width; in % of the photo's long
    /// edge.
    pub size: f32,
    /// 0–100.
    pub opacity: f32,
    #[serde(default)]
    pub anchor: Anchor,
    /// From the photo's edges, in % of its long edge.
    #[serde(default = "default_margin")]
    pub margin: f32,
}

fn default_margin() -> f32 {
    2.0
}

impl Watermark {
    pub fn text(text: &str) -> Self {
        Self {
            mark: Mark::Text {
                text: text.to_owned(),
                font: None,
                colour: [255, 255, 255],
            },
            size: 3.0,
            opacity: 70.0,
            anchor: Anchor::BottomRight,
            margin: default_margin(),
        }
    }
}

/// The photo's size inside a border of `width` % (of the long edge) for a
/// `w`×`h` output: the photo scaled down so it and the border keep the long
/// edge. The border is even all round, so the short side comes out a
/// little longer than `h`. Returns the photo's size and the border in
/// pixels.
pub fn inside(w: usize, h: usize, border: Option<&Border>) -> ((usize, usize), usize) {
    let Some(b) = border else {
        return ((w, h), 0);
    };
    let long = w.max(h) as f32;
    let px = (b.width.clamp(0.0, 25.0) / 100.0 * long).round() as usize;
    let scale = ((long - 2.0 * px as f32) / long).max(0.01);
    let fit = |v: usize| ((v as f32 * scale).round() as usize).max(1);
    ((fit(w), fit(h)), px)
}

/// An sRGB colour as display values in `target`'s space.
pub fn from_srgb(c: [u8; 3], target: Target) -> [f32; 3] {
    let lin = c.map(|v| f64::from(srgb_eotf(f32::from(v) / 255.0)));
    let to_rec2020 = colour::inv(&Target::Srgb.from_rec2020()).unwrap_or(colour::IDENTITY);
    let t = colour::mulv(
        &colour::mul(&target.from_rec2020(), &to_rec2020),
        [lin[0], lin[1], lin[2]],
    );
    t.map(|v| target.encode((v as f32).clamp(0.0, 1.0)))
}

/// `img` framed by `border` (`px` wide).
pub fn add_border(img: &Rgb, px: usize, colour: [f32; 3]) -> Rgb {
    let (w, h) = (img.width + 2 * px, img.height + 2 * px);
    let mut out = Rgb::new(w, h);
    for p in out.data.as_chunks_mut::<3>().0 {
        *p = colour;
    }
    for y in 0..img.height {
        let from = &img.data[y * img.width * 3..(y + 1) * img.width * 3];
        let at = ((y + px) * w + px) * 3;
        out.data[at..at + img.width * 3].copy_from_slice(from);
    }
    out
}

/// Coverage (0–1) and a colour per pixel, to lay over the photo.
struct Layer {
    width: usize,
    height: usize,
    /// Display values in the export's space, and alpha.
    px: Vec<[f32; 4]>,
}

/// Lay `mark` over the photo, which fills `area` (x, y, w, h) of `img`.
/// `default_font`: the font text uses when the watermark names none.
pub fn draw(
    img: &mut Rgb,
    area: (usize, usize, usize, usize),
    mark: &Watermark,
    target: Target,
    default_font: &[u8],
) -> Result<(), String> {
    let (ax, ay, aw, ah) = area;
    let long = aw.max(ah) as f32;
    let size = (mark.size.clamp(0.1, 100.0) / 100.0 * long).max(1.0);
    let layer = match &mark.mark {
        Mark::Text { text, font, colour } => {
            let bytes;
            let data = match font {
                Some(path) => {
                    bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                    &bytes[..]
                }
                None => default_font,
            };
            text_layer(text, data, size, from_srgb(*colour, target))?
        }
        Mark::Image { path } => image_layer(path, size.round() as usize, target)?,
    };
    if layer.width == 0 || layer.height == 0 {
        return Ok(());
    }
    let margin = (mark.margin.clamp(0.0, 50.0) / 100.0 * long).round() as usize;
    let (cx, cy) = mark.anchor.cell();
    let place = |cell: usize, start: usize, span: usize, len: usize| -> isize {
        let start = start as isize;
        let (span, len, margin) = (span as isize, len as isize, margin as isize);
        match cell {
            0 => start + margin,
            1 => start + (span - len) / 2,
            _ => start + span - len - margin,
        }
    };
    let x0 = place(cx, ax, aw, layer.width);
    let y0 = place(cy, ay, ah, layer.height);
    let opacity = mark.opacity.clamp(0.0, 100.0) / 100.0;
    for ly in 0..layer.height {
        let y = y0 + ly as isize;
        if y < 0 || y as usize >= img.height {
            continue;
        }
        for lx in 0..layer.width {
            let x = x0 + lx as isize;
            if x < 0 || x as usize >= img.width {
                continue;
            }
            let [r, g, b, a] = layer.px[ly * layer.width + lx];
            let a = a * opacity;
            if a <= 0.0 {
                continue;
            }
            let at = (y as usize * img.width + x as usize) * 3;
            for (o, c) in img.data[at..at + 3].iter_mut().zip([r, g, b]) {
                *o += (c - *o) * a;
            }
        }
    }
    Ok(())
}

/// A font's outline as a tiny-skia path, from font units scaled to pixels
/// (skrifa's y points up; the image's down).
struct Pen {
    path: tiny_skia::PathBuilder,
    x: f32,
    baseline: f32,
}

impl Pen {
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x, self.baseline - y)
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.path.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.path.line_to(x, y);
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let ((cx0, cy0), (x, y)) = (self.at(cx0, cy0), self.at(x, y));
        self.path.quad_to(cx0, cy0, x, y);
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let ((cx0, cy0), (cx1, cy1), (x, y)) =
            (self.at(cx0, cy0), self.at(cx1, cy1), self.at(x, y));
        self.path.cubic_to(cx0, cy0, cx1, cy1, x, y);
    }
    fn close(&mut self) {
        self.path.close();
    }
}

/// One line of text at `px` (the font size), in one colour.
fn text_layer(text: &str, font: &[u8], px: f32, colour: [f32; 3]) -> Result<Layer, String> {
    let font = skrifa::FontRef::from_index(font, 0).map_err(|e| format!("font: {e}"))?;
    let size = Size::new(px);
    let loc = LocationRef::default();
    let metrics = font.metrics(size, loc);
    let advances = font.glyph_metrics(size, loc);
    let outlines = font.outline_glyphs();
    let charmap = font.charmap();
    let pad = 2.0;
    let mut pen = Pen {
        path: tiny_skia::PathBuilder::new(),
        x: pad,
        baseline: pad + metrics.ascent,
    };
    for ch in text.chars().filter(|c| !c.is_control()) {
        let Some(gid) = charmap.map(ch) else {
            continue;
        };
        if let Some(glyph) = outlines.get(gid) {
            glyph
                .draw(DrawSettings::unhinted(size, loc), &mut pen)
                .map_err(|e| format!("glyph: {e}"))?;
        }
        pen.x += advances.advance_width(gid).unwrap_or(0.0);
    }
    let width = (pen.x + pad).ceil() as usize;
    let height = (metrics.ascent - metrics.descent + 2.0 * pad).ceil() as usize;
    let Some(path) = pen.path.finish() else {
        // Nothing to draw (spaces, or no glyphs in this font).
        return Ok(Layer {
            width: 0,
            height: 0,
            px: Vec::new(),
        });
    };
    let mut mask = tiny_skia::Mask::new(width as u32, height as u32)
        .ok_or_else(|| "watermark too large".to_owned())?;
    mask.fill_path(
        &path,
        tiny_skia::FillRule::Winding,
        true,
        tiny_skia::Transform::identity(),
    );
    let [r, g, b] = colour;
    Ok(Layer {
        width,
        height,
        px: mask
            .data()
            .iter()
            .map(|&a| [r, g, b, f32::from(a) / 255.0])
            .collect(),
    })
}

/// A PNG `width` pixels wide (its aspect kept), area-averaged.
fn image_layer(path: &std::path::Path, width: usize, target: Target) -> Result<Layer, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut d = png::Decoder::new(std::io::BufReader::new(file));
    d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = d
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; r.output_buffer_size().ok_or("image too large")?];
    let info = r
        .next_frame(&mut buf)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let (sw, sh) = (info.width as usize, info.height as usize);
    // Straight sRGB RGBA, 8 bits.
    let buf = &buf[..info.buffer_size().min(buf.len())];
    let rgba: Vec<[u8; 4]> = match info.color_type {
        png::ColorType::Rgba => buf.as_chunks::<4>().0.to_vec(),
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::Indexed => return Err("an indexed PNG the decoder didn't expand".into()),
    };
    if sw == 0 || sh == 0 || width == 0 {
        return Ok(Layer {
            width: 0,
            height: 0,
            px: Vec::new(),
        });
    }
    let height = ((sh * width) as f32 / sw as f32).round().max(1.0) as usize;
    // Each colour once, in the export's space.
    let mut lut = std::collections::HashMap::new();
    let mut px = Vec::with_capacity(width * height);
    for y in 0..height {
        let (y0, y1) = (y * sh / height, ((y + 1) * sh).div_ceil(height).min(sh));
        for x in 0..width {
            let (x0, x1) = (x * sw / width, ((x + 1) * sw).div_ceil(width).min(sw));
            // Premultiplied average over the source pixels under this one.
            let mut acc = [0.0f32; 4];
            let mut n = 0.0;
            for sy in y0..y1.max(y0 + 1) {
                for sx in x0..x1.max(x0 + 1) {
                    let p = rgba[sy.min(sh - 1) * sw + sx.min(sw - 1)];
                    let a = f32::from(p[3]) / 255.0;
                    let c = *lut
                        .entry([p[0], p[1], p[2]])
                        .or_insert_with(|| from_srgb([p[0], p[1], p[2]], target));
                    for k in 0..3 {
                        acc[k] += c[k] * a;
                    }
                    acc[3] += a;
                    n += 1.0;
                }
            }
            let a = acc[3] / n;
            px.push(if acc[3] > 0.0 {
                [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], a]
            } else {
                [0.0; 4]
            });
        }
    }
    Ok(Layer { width, height, px })
}
