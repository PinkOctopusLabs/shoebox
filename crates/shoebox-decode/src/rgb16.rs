//! 16-bit RGB for editing: 16-bit TIFFs and PNGs keep their precision on
//! the way to Develop (the display path reduces everything to 8 bits).

use moxcms::{ColorProfile, Layout, TransformOptions};

use crate::{DecodeError, Format, Target};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgb16 {
    pub width: u32,
    pub height: u32,
    /// Interleaved R, G, B, 0–65535.
    pub px: Vec<u16>,
}

impl Rgb16 {
    pub fn new(width: u32, height: u32, px: Vec<u16>) -> Result<Self, DecodeError> {
        if width == 0 || height == 0 || px.len() != width as usize * height as usize * 3 {
            return Err(DecodeError::Format(format!(
                "RGB buffer {} does not match {width}x{height}",
                px.len()
            )));
        }
        Ok(Self { width, height, px })
    }

    /// Area-average resample so the long edge is `long` (never upscales).
    pub fn fit_long_edge(&self, long: u32) -> Self {
        let cur = self.width.max(self.height);
        if cur <= long {
            return self.clone();
        }
        let s = f64::from(long) / f64::from(cur);
        let w = (f64::from(self.width) * s).round().max(1.0) as u32;
        let h = (f64::from(self.height) * s).round().max(1.0) as u32;
        let (sw, sh) = (self.width as usize, self.height as usize);
        // Rows first, then columns, in f32.
        let xs = crate::gray::area_weights(self.width, w);
        let mut rows = vec![0f32; w as usize * sh * 3];
        for y in 0..sh {
            let src = &self.px[y * sw * 3..(y + 1) * sw * 3];
            for (x, ws) in xs.iter().enumerate() {
                let o = &mut rows[(y * w as usize + x) * 3..][..3];
                for &(i, wt) in ws {
                    for c in 0..3 {
                        o[c] += f32::from(src[i * 3 + c]) * wt;
                    }
                }
            }
        }
        let ys = crate::gray::area_weights(self.height, h);
        let wu = w as usize;
        let mut px = vec![0u16; wu * h as usize * 3];
        for (y, ws) in ys.iter().enumerate() {
            for x in 0..wu {
                for c in 0..3 {
                    let v: f32 = ws
                        .iter()
                        .map(|&(i, wt)| rows[(i * wu + x) * 3 + c] * wt)
                        .sum();
                    px[(y * wu + x) * 3 + c] = v.round().clamp(0.0, 65535.0) as u16;
                }
            }
        }
        Self {
            width: w,
            height: h,
            px,
        }
    }

    /// Apply an EXIF Orientation value (1–8) so the result is upright.
    pub fn orient(&self, orientation: u16) -> Self {
        if orientation <= 1 || orientation > 8 {
            return self.clone();
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let turned = orientation >= 5;
        let (ow, oh) = if turned { (h, w) } else { (w, h) };
        let mut px = vec![0u16; self.px.len()];
        for y in 0..oh {
            for x in 0..ow {
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    _ => (w - 1 - y, x),
                };
                let s = (sy * w + sx) * 3;
                px[(y * ow + x) * 3..][..3].copy_from_slice(&self.px[s..s + 3]);
            }
        }
        Self {
            width: ow as u32,
            height: oh as u32,
            px,
        }
    }
}

/// A 16-bit TIFF or PNG in `target`'s colour space, upright, long edge at
/// most `long_edge`. None for any other file (8-bit ones included): those
/// go through [`crate::render`].
pub fn render16(
    format: Format,
    data: &[u8],
    long_edge: Option<u32>,
    orientation: u16,
    target: Target,
) -> Result<Option<Rgb16>, DecodeError> {
    let found = match format {
        Format::Tiff => crate::tiff::decode_rgb16(data)?,
        Format::Png => crate::png::decode_rgb16(data)?,
        _ => None,
    };
    let Some((mut img, icc)) = found else {
        return Ok(None);
    };
    if let Some(l) = long_edge {
        img = img.fit_long_edge(l);
    }
    convert(&mut img, icc.as_deref(), target)?;
    Ok(Some(img.orient(orientation)))
}

/// From the embedded profile (sRGB if none, or if it can't be read) to
/// `target`, in place.
fn convert(img: &mut Rgb16, icc: Option<&[u8]>, target: Target) -> Result<(), DecodeError> {
    let src = icc
        .and_then(|b| ColorProfile::new_from_slice(b).ok())
        .unwrap_or_else(ColorProfile::new_srgb);
    let dst = match target {
        Target::Srgb => ColorProfile::new_srgb(),
        Target::DisplayP3 => ColorProfile::new_display_p3(),
    };
    let t = src
        .create_transform_16bit(Layout::Rgb, &dst, Layout::Rgb, TransformOptions::default())
        .map_err(|e| DecodeError::Format(format!("ICC transform: {e:?}")))?;
    let mut out = vec![0u16; img.px.len()];
    t.transform(&img.px, &mut out)
        .map_err(|e| DecodeError::Format(format!("ICC transform: {e:?}")))?;
    img.px = out;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::Rgb16;
    use crate::Rgb8;

    #[test]
    fn orients_as_the_8_bit_path_does() {
        let (w, h) = (5u32, 3u32);
        let px8: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let a = Rgb8::new(w, h, px8.clone()).unwrap();
        let b = Rgb16::new(w, h, px8.iter().map(|&v| u16::from(v) * 257).collect()).unwrap();
        for o in 1..=8 {
            let (a, b) = (a.orient(o), b.orient(o));
            assert_eq!((a.width, a.height), (b.width, b.height), "{o}");
            let back: Vec<u8> = b.px.iter().map(|&v| (v / 257) as u8).collect();
            assert_eq!(a.px, back, "orientation {o}");
        }
    }

    /// A ramp finer than 8 bits can hold: 16-bit TIFFs and PNGs keep it;
    /// an 8-bit file is left to the 8-bit path.
    #[test]
    fn deep_files_keep_their_precision() {
        use crate::{Format, Target};
        let (w, h) = (1024u32, 2u32);
        let ramp: Vec<u16> = (0..w * h)
            .flat_map(|i| {
                let v = (i % w) as u16 * 16;
                [v, v, v]
            })
            .collect();
        let distinct = |img: &Rgb16| {
            let mut v: Vec<u16> = img.px.iter().step_by(3).copied().collect();
            v.sort_unstable();
            v.dedup();
            v.len()
        };

        let mut tif = std::io::Cursor::new(Vec::new());
        tiff::encoder::TiffEncoder::new(&mut tif)
            .unwrap()
            .write_image::<tiff::encoder::colortype::RGB16>(w, h, &ramp)
            .unwrap();
        let img = super::render16(Format::Tiff, tif.get_ref(), None, 1, Target::Srgb)
            .unwrap()
            .unwrap();
        assert!(distinct(&img) > 900, "{}", distinct(&img));

        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Sixteen);
            let bytes: Vec<u8> = ramp.iter().flat_map(|v| v.to_be_bytes()).collect();
            e.write_header().unwrap().write_image_data(&bytes).unwrap();
        }
        let img = super::render16(Format::Png, &out, None, 6, Target::DisplayP3)
            .unwrap()
            .unwrap();
        assert_eq!((img.width, img.height), (h, w), "turned upright");
        assert!(distinct(&img) > 900, "{}", distinct(&img));

        let mut tif8 = std::io::Cursor::new(Vec::new());
        let ramp8: Vec<u8> = ramp.iter().map(|v| (v >> 8) as u8).collect();
        tiff::encoder::TiffEncoder::new(&mut tif8)
            .unwrap()
            .write_image::<tiff::encoder::colortype::RGB8>(w, h, &ramp8)
            .unwrap();
        assert!(
            super::render16(Format::Tiff, tif8.get_ref(), None, 1, Target::Srgb)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn shrinks_by_area() {
        let img = Rgb16::new(4, 2, vec![1000; 24]).unwrap();
        let s = img.fit_long_edge(2);
        assert_eq!((s.width, s.height), (2, 1));
        assert!(s.px.iter().all(|&v| v == 1000));
        assert_eq!(img.fit_long_edge(8), img);
    }
}
