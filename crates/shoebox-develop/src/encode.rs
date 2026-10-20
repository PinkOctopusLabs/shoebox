//! Writing rendered images: JPEG (pure Rust, `jpeg-encoder`) with an
//! embedded ICC profile (`moxcms`), for previews and export.

use crate::cpu::{Rgb, Target, to_u8};

/// The ICC profile of a target space.
pub fn icc(target: Target) -> Result<Vec<u8>, String> {
    let p = match target {
        Target::Srgb => moxcms::ColorProfile::new_srgb(),
        Target::DisplayP3 => moxcms::ColorProfile::new_display_p3(),
        Target::AdobeRgb => moxcms::ColorProfile::new_adobe_rgb(),
        Target::ProPhoto => moxcms::ColorProfile::new_pro_photo_rgb(),
    };
    p.encode().map_err(|e| format!("ICC profile: {e}"))
}

/// JPEG options.
#[derive(Debug, Clone, Copy)]
pub struct Jpeg {
    /// 1–100.
    pub quality: u8,
    /// Full-resolution colour (4:4:4) rather than halved (4:2:0).
    pub full_chroma: bool,
    pub progressive: bool,
}

impl Default for Jpeg {
    fn default() -> Self {
        Self {
            quality: 90,
            full_chroma: false,
            progressive: true,
        }
    }
}

/// Encode display values (0–1, `target`'s space) as a JPEG tagged with
/// `target`'s profile.
pub fn jpeg(img: &Rgb, target: Target, o: Jpeg) -> Result<Vec<u8>, String> {
    let (w, h) = (
        u16::try_from(img.width).map_err(|_| "too wide for JPEG".to_owned())?,
        u16::try_from(img.height).map_err(|_| "too tall for JPEG".to_owned())?,
    );
    let mut out = Vec::new();
    let mut e = jpeg_encoder::Encoder::new(&mut out, o.quality.clamp(1, 100));
    e.set_sampling_factor(if o.full_chroma {
        jpeg_encoder::SamplingFactor::R_4_4_4
    } else {
        jpeg_encoder::SamplingFactor::R_4_2_0
    });
    e.set_progressive(o.progressive);
    e.add_icc_profile(&icc(target)?)
        .map_err(|e| e.to_string())?;
    e.encode(&to_u8(img), w, h, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| e.to_string())?;
    Ok(out)
}
