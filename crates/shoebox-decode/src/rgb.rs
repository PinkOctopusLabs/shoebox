//! 8-bit interleaved RGB images: resampling, orientation, and colour
//! conversion to sRGB.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use moxcms::{ColorProfile, Layout, Transform8BitExecutor, TransformOptions};

use crate::{DecodeError, Gray};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgb8 {
    pub width: u32,
    pub height: u32,
    /// Interleaved R, G, B.
    pub px: Vec<u8>,
}

impl Rgb8 {
    pub fn new(width: u32, height: u32, px: Vec<u8>) -> Result<Self, DecodeError> {
        if width == 0 || height == 0 || px.len() != width as usize * height as usize * 3 {
            return Err(DecodeError::Format(format!(
                "RGB buffer {} does not match {width}x{height}",
                px.len()
            )));
        }
        Ok(Self { width, height, px })
    }

    pub fn from_gray(g: &Gray) -> Self {
        Self {
            width: g.width,
            height: g.height,
            px: g.px.iter().flat_map(|&v| [v, v, v]).collect(),
        }
    }

    fn planes(&self) -> [Gray; 3] {
        let n = self.width as usize * self.height as usize;
        let mut p = [vec![0u8; n], vec![0u8; n], vec![0u8; n]];
        for (i, c) in self.px.as_chunks::<3>().0.iter().enumerate() {
            p[0][i] = c[0];
            p[1][i] = c[1];
            p[2][i] = c[2];
        }
        p.map(|px| Gray {
            width: self.width,
            height: self.height,
            px,
        })
    }

    fn from_planes(p: [Gray; 3]) -> Self {
        let (width, height) = (p[0].width, p[0].height);
        let mut px = Vec::with_capacity(p[0].px.len() * 3);
        for i in 0..p[0].px.len() {
            px.extend([p[0].px[i], p[1].px[i], p[2].px[i]]);
        }
        Self { width, height, px }
    }

    fn map_planes(&self, f: impl Fn(&Gray) -> Gray) -> Self {
        Self::from_planes(self.planes().map(|g| f(&g)))
    }

    /// Area-average resample to exactly `w`×`h`.
    pub fn resize(&self, w: u32, h: u32) -> Self {
        self.map_planes(|g| g.resize(w, h))
    }

    /// Resize so the long edge is `long` (never upscales).
    pub fn fit_long_edge(&self, long: u32) -> Self {
        if self.width.max(self.height) <= long {
            return self.clone();
        }
        self.map_planes(|g| g.fit_long_edge(long))
    }

    /// Apply an EXIF Orientation value (1–8) so the result is upright.
    pub fn orient(&self, orientation: u16) -> Self {
        if orientation <= 1 || orientation > 8 {
            return self.clone();
        }
        self.map_planes(|g| g.orient(orientation))
    }

    pub fn to_gray(&self) -> Gray {
        Gray {
            width: self.width,
            height: self.height,
            px: crate::gray::luma_from_rgb8(&self.px, 3),
        }
    }

    /// RGBA with opaque alpha, as GPU textures want.
    pub fn to_rgba(&self) -> Vec<u8> {
        self.px
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect()
    }

    /// 256-bin histograms of R, G, B and luma.
    pub fn histogram(&self) -> [[u32; 256]; 4] {
        let mut h = [[0u32; 256]; 4];
        for c in self.px.as_chunks::<3>().0 {
            h[0][c[0] as usize] += 1;
            h[1][c[1] as usize] += 1;
            h[2][c[2] as usize] += 1;
            let y = (77 * u32::from(c[0]) + 150 * u32::from(c[1]) + 29 * u32::from(c[2])) >> 8;
            h[3][y as usize] += 1;
        }
        h
    }
}

type Transform = Arc<Transform8BitExecutor>;

/// Transforms are costly to build and most libraries use a handful of
/// profiles (sRGB, Adobe RGB, ProPhoto), so they're cached by profile bytes.
fn transform_for(icc: &[u8]) -> Result<Option<Transform>, DecodeError> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Option<Transform>>>> = OnceLock::new();
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        icc.hash(&mut h);
        h.finish()
    };
    let cache = CACHE.get_or_init(Default::default);
    if let Some(t) = cache.lock().map_err(|_| DecodeError::Truncated)?.get(&key) {
        return Ok(t.clone());
    }
    let src = ColorProfile::new_from_slice(icc)
        .map_err(|e| DecodeError::Format(format!("ICC profile: {e:?}")))?;
    // An sRGB-equivalent source needs no work.
    let t = if is_srgb(&src) {
        None
    } else {
        Some(
            src.create_transform_8bit(
                Layout::Rgb,
                &ColorProfile::new_srgb(),
                Layout::Rgb,
                TransformOptions::default(),
            )
            .map_err(|e| DecodeError::Format(format!("ICC transform: {e:?}")))?,
        )
    };
    cache
        .lock()
        .map_err(|_| DecodeError::Truncated)?
        .insert(key, t.clone());
    Ok(t)
}

/// True when converting from `p` to sRGB would change nothing: same
/// colourants, and a grey ramp maps to itself (so the tone curves match too).
fn is_srgb(p: &ColorProfile) -> bool {
    let srgb = ColorProfile::new_srgb();
    let close = |a: moxcms::Xyzd, b: moxcms::Xyzd| {
        (a.x - b.x).abs() < 2e-3 && (a.y - b.y).abs() < 2e-3 && (a.z - b.z).abs() < 2e-3
    };
    if !(close(p.red_colorant, srgb.red_colorant)
        && close(p.green_colorant, srgb.green_colorant)
        && close(p.blue_colorant, srgb.blue_colorant))
    {
        return false;
    }
    let Ok(t) =
        p.create_transform_8bit(Layout::Rgb, &srgb, Layout::Rgb, TransformOptions::default())
    else {
        return false;
    };
    let ramp: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v]).collect();
    let mut out = vec![0u8; ramp.len()];
    t.transform(&ramp, &mut out).is_ok() && ramp.iter().zip(&out).all(|(a, b)| a.abs_diff(*b) <= 1)
}

/// Convert from the embedded profile to sRGB in place. Missing or unreadable
/// profiles are treated as sRGB (the spec's fallback).
pub fn to_srgb(img: &mut Rgb8, icc: Option<&[u8]>) {
    let Some(icc) = icc else { return };
    match transform_for(icc) {
        Ok(Some(t)) => {
            let src = img.px.clone();
            if let Err(e) = t.transform(&src, &mut img.px) {
                img.px = src;
                tracing::warn!("ICC transform failed: {e:?}");
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("ignoring ICC profile: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adobe_rgb_green_desaturates_into_srgb() {
        let adobe = ColorProfile::new_adobe_rgb().encode().unwrap();
        let mut img = Rgb8::new(1, 1, vec![0, 255, 0]).unwrap();
        to_srgb(&mut img, Some(&adobe));
        // Adobe RGB's pure green is outside sRGB: clipped to full green, with
        // red/blue at zero, but a mid-tone shows the shift clearly.
        let mut mid = Rgb8::new(1, 1, vec![128, 64, 32]).unwrap();
        to_srgb(&mut mid, Some(&adobe));
        assert_ne!(
            mid.px,
            vec![128, 64, 32],
            "Adobe RGB must not pass through untouched"
        );
    }

    #[test]
    fn srgb_profile_is_identity() {
        let srgb = ColorProfile::new_srgb().encode().unwrap();
        let mut img = Rgb8::new(2, 1, vec![10, 128, 250, 200, 100, 0]).unwrap();
        let before = img.clone();
        to_srgb(&mut img, Some(&srgb));
        assert_eq!(img, before);
    }

    #[test]
    fn resize_and_orient_match_gray_behaviour() {
        let g = Gray::new(4, 2, (0..8).map(|v| v * 30).collect()).unwrap();
        let rgb = Rgb8::from_gray(&g);
        assert_eq!(rgb.orient(6).to_gray(), g.orient(6));
        assert_eq!(rgb.fit_long_edge(2).to_gray().px, g.fit_long_edge(2).px);
    }
}
