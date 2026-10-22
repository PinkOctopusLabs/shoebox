//! Writing rendered images: JPEG (pure Rust, `jpeg-encoder`) with an
//! embedded ICC profile (`moxcms`), for previews and export; AVIF (`rav1e`
//! and `avif-serialize`, pure Rust) tagged by CICP; and linear DNGs
//! (`rawler`'s DNG writer): 16-bit for exports, floating point for photo
//! merges.

use shoebox_decode::raw::ColorMatrix;

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

/// AVIF options.
#[derive(Debug, Clone, Copy)]
pub struct Avif {
    /// 1–100.
    pub quality: u8,
    /// 10 bits a channel rather than 8.
    pub ten_bit: bool,
    /// rav1e's speed preset, 1 (slow, smaller) to 10.
    pub speed: u8,
}

/// The speed exports use unless a preset says otherwise (DECISIONS, "AVIF
/// and linear DNG export").
pub const AVIF_SPEED: u8 = 6;

impl Default for Avif {
    fn default() -> Self {
        Self {
            quality: 85,
            ten_bit: true,
            speed: AVIF_SPEED,
        }
    }
}

/// Encode display values (0–1, `target`'s space) as an AVIF: 4:4:4 YCbCr
/// (BT.601 coefficients, full range) with the space given by CICP in both
/// the AV1 sequence header and the `colr` box (sRGB: BT.709 primaries;
/// Display P3: SMPTE EG 432-1; both with the sRGB curve). AVIF describes
/// colour by CICP, which has no code for Adobe RGB or ProPhoto, so those
/// are refused.
///
/// rav1e and avif-serialize are what `ravif` is built from; `ravif` itself
/// fixes the description at sRGB and takes 8-bit input, so this does its
/// part here (the quality scale and the still-image speed settings follow
/// `ravif` 0.13, BSD-3-Clause, Kornel Lesiński).
pub fn avif(img: &Rgb, target: Target, o: Avif) -> Result<Vec<u8>, String> {
    use avif_serialize::constants as c;
    use rav1e::prelude::*;
    let (primaries, cicp) = match target {
        Target::Srgb => (ColorPrimaries::BT709, c::ColorPrimaries::Bt709),
        Target::DisplayP3 => (ColorPrimaries::SMPTE432, c::ColorPrimaries::DisplayP3),
        Target::AdobeRgb | Target::ProPhoto => {
            return Err("AVIF holds sRGB or Display P3, not this colour space".into());
        }
    };
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || w > 1 << 16 || h > 1 << 16 {
        return Err(format!("{w}×{h} is too large for AVIF"));
    }
    let depth: usize = if o.ten_bit { 10 } else { 8 };
    let quantizer = avif_quantizer(o.quality);
    let speed = o.speed.clamp(1, 10);
    let mut enc = EncoderConfig::with_speed_preset(speed);
    enc.width = w;
    enc.height = h;
    enc.bit_depth = depth;
    enc.chroma_sampling = ChromaSampling::Cs444;
    enc.chroma_sample_position = ChromaSamplePosition::Unknown;
    enc.pixel_range = PixelRange::Full;
    enc.color_description = Some(ColorDescription {
        color_primaries: primaries,
        transfer_characteristics: TransferCharacteristics::SRGB,
        matrix_coefficients: MatrixCoefficients::BT601,
    });
    enc.still_picture = true;
    enc.quantizer = usize::from(quantizer);
    enc.min_quantizer = quantizer;
    enc.tune = Tune::Psychovisual;
    let min_tile = still_speed(&mut enc.speed_settings, speed, quantizer);
    // Tiles are how rav1e spreads one frame over threads.
    enc.tiles = rayon::current_num_threads().clamp(1, ((w * h) / (min_tile * min_tile)).max(1));
    let cfg = Config::new().with_encoder_config(enc);
    let ycc = to_ycbcr(img, depth);
    // rav1e asserts on what it doesn't expect: an export fails with a
    // message rather than taking the app down.
    let color = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if o.ten_bit {
            av1_frame::<u16>(&cfg, w, h, &ycc)
        } else {
            av1_frame::<u8>(&cfg, w, h, &ycc)
        }
    }))
    .map_err(|_| "the AVIF encoder failed".to_owned())??;
    let mut out = Vec::new();
    avif_serialize::Aviffy::new()
        .set_color_primaries(cicp)
        .set_transfer_characteristics(c::TransferCharacteristics::Srgb)
        .set_matrix_coefficients(c::MatrixCoefficients::Bt601)
        .set_full_color_range(true)
        .write(&mut out, &color, None, w as u32, h as u32, depth as u8)
        .map_err(|e| format!("AVIF: {e}"))?;
    Ok(out)
}

/// rav1e's settings at `speed` tuned for one still picture, as `ravif`
/// does it; returns the smallest tile side worth splitting into.
fn still_speed(s: &mut rav1e::config::SpeedSettings, speed: u8, quantizer: u8) -> usize {
    use rav1e::prelude::*;
    let low_quality = quantizer < avif_quantizer(55);
    let high_quality = quantizer > avif_quantizer(80);
    let max_block = if high_quality { 16 } else { 64 };
    // One frame: no references, look-ahead or scene detection.
    s.multiref = false;
    s.rdo_lookahead_frames = 1;
    s.scene_detection_mode = SceneDetectionSpeed::None;
    s.motion.include_near_mvs = false;
    s.motion.use_satd_subpel = false;
    s.fast_deblock = speed >= 7 && !high_quality;
    s.transform.reduced_tx_set = speed == 4 || speed >= 9;
    s.transform.tx_domain_rate = speed >= 10;
    s.transform.rdo_tx_decision = speed <= 4 && !high_quality;
    s.transform.enable_inter_tx_split = speed >= 9;
    s.partition.encode_bottomup = speed <= 2;
    s.cdef = low_quality && speed <= 9;
    s.lrf = low_quality && speed <= 8;
    s.sgr_complexity = if speed <= 2 {
        SGRComplexityLevel::Full
    } else {
        SGRComplexityLevel::Reduced
    };
    s.prediction.fine_directional_intra = speed <= 6;
    s.prediction.prediction_modes = if speed <= 1 {
        PredictionModesSetting::ComplexAll
    } else {
        PredictionModesSetting::Simple
    };
    let (min, max) = match speed {
        1 if low_quality => (4, 64.min(max_block)),
        2 if low_quality => (4, 32.min(max_block)),
        1..=4 => (4, 16),
        5..=8 => (8, 16),
        _ => (16, 16),
    };
    let size = |s: usize| match s {
        4 => BlockSize::BLOCK_4X4,
        8 => BlockSize::BLOCK_8X8,
        16 => BlockSize::BLOCK_16X16,
        32 => BlockSize::BLOCK_32X32,
        _ => BlockSize::BLOCK_64X64,
    };
    s.partition.partition_range = PartitionRange::new(size(min), size(max));
    let tile = match speed {
        1 => 2048,
        2 => 1024,
        3 => 512,
        4 => 256,
        _ => 128,
    };
    tile * if high_quality { 2 } else { 1 }
}

/// `ravif`'s quality scale (1–100) as an AV1 quantizer (0–255).
fn avif_quantizer(quality: u8) -> u8 {
    let q = f32::from(quality.clamp(1, 100)) / 100.0;
    let x = if q >= 0.82 {
        (1.0 - q) * 2.6
    } else if q > 0.25 {
        q.mul_add(-0.5, 1.0 - 0.125)
    } else {
        1.0 - q
    };
    (x * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Display values as Y, Cb and Cr planes (BT.601 coefficients, full range)
/// at `depth` bits.
fn to_ycbcr(img: &Rgb, depth: usize) -> [Vec<u16>; 3] {
    use rayon::prelude::*;
    const KR: f32 = 0.299;
    const KB: f32 = 0.114;
    let max = ((1u32 << depth) - 1) as f32;
    let n = img.width * img.height;
    let mut planes = [vec![0u16; n], vec![0u16; n], vec![0u16; n]];
    let [ys, cbs, crs] = &mut planes;
    let w = img.width.max(1);
    (
        ys.par_chunks_mut(w),
        cbs.par_chunks_mut(w),
        crs.par_chunks_mut(w),
        img.data.par_chunks(w * 3),
    )
        .into_par_iter()
        .for_each(|(ys, cbs, crs, line)| {
            for (i, p) in line.as_chunks::<3>().0.iter().enumerate() {
                let [r, g, b] = [0, 1, 2].map(|c| p[c].clamp(0.0, 1.0));
                let y = KR * r + (1.0 - KR - KB) * g + KB * b;
                let cb = (b - y) / (2.0 * (1.0 - KB)) + 0.5;
                let cr = (r - y) / (2.0 * (1.0 - KR)) + 0.5;
                let q = |v: f32| (v.clamp(0.0, 1.0) * max).round() as u16;
                (ys[i], cbs[i], crs[i]) = (q(y), q(cb), q(cr));
            }
        });
    planes
}

/// One frame through rav1e: its AV1 bitstream.
fn av1_frame<P: rav1e::Pixel + TryFrom<u16>>(
    cfg: &rav1e::Config,
    w: usize,
    h: usize,
    planes: &[Vec<u16>; 3],
) -> Result<Vec<u8>, String> {
    use rav1e::prelude::*;
    let err = |e: &dyn std::fmt::Display| format!("AVIF: {e}");
    let mut ctx: Context<P> = cfg.new_context().map_err(|e| err(&e))?;
    let mut frame = ctx.new_frame();
    for (plane, src) in frame.planes.iter_mut().zip(planes) {
        let mut slice = plane.mut_slice(Default::default());
        for (row, line) in slice.rows_iter_mut().take(h).zip(src.chunks_exact(w)) {
            for (d, &s) in row[..w].iter_mut().zip(line) {
                *d = P::try_from(s).map_err(|_| "AVIF: sample out of range".to_owned())?;
            }
        }
    }
    ctx.send_frame(frame).map_err(|e| err(&e))?;
    ctx.flush();
    let mut out = Vec::new();
    loop {
        match ctx.receive_packet() {
            Ok(mut packet) => {
                if packet.frame_type == FrameType::KEY {
                    out.append(&mut packet.data);
                }
            }
            Err(EncoderStatus::Encoded) => {}
            Err(EncoderStatus::LimitReached) => break,
            Err(e) => return Err(err(&e)),
        }
    }
    Ok(out)
}

/// What goes into a linear DNG (see [`linear_dng`]).
#[derive(Debug, Clone, Copy)]
pub struct LinearDng<'a> {
    pub width: usize,
    pub height: usize,
    /// Camera-native linear RGB, black 0 and white 65535, row by row,
    /// upright.
    pub data: &'a [u16],
    pub make: &'a str,
    pub model: &'a str,
    /// The camera RGB of a neutral as shot (AsShotNeutral), green 1.
    pub neutral: [f32; 3],
    /// XYZ → camera matrices by calibration temperature, lowest first: the
    /// first and last are written.
    pub matrices: &'a [ColorMatrix],
    /// How much noise reduction the data has had, 0–1
    /// (NoiseReductionApplied).
    pub noise_reduction: f32,
    /// An upright 8-bit sRGB preview (width, height, pixels) for IFD 0.
    pub thumbnail: Option<(usize, usize, &'a [u8])>,
}

/// Write a linear DNG with `rawler`'s writer: the image is a LinearRaw
/// sub-IFD, 16-bit, lossless JPEG in tiles (the form Adobe DNG Converter
/// writes linear DNGs in, so every raw reader takes it), with the colour
/// matrices and as-shot neutral other raw editors render it from, and the
/// preview, if any, in IFD 0.
pub fn linear_dng(d: &LinearDng) -> Result<Vec<u8>, String> {
    if d.data.len() != d.width * d.height * 3 || d.width == 0 || d.height == 0 {
        return Err("linear DNG: the image doesn't match its size".into());
    }
    if d.width > usize::from(u16::MAX) || d.height > usize::from(u16::MAX) {
        return Err(format!("{}×{} is too large for DNG", d.width, d.height));
    }
    let (Some(low), Some(high)) = (d.matrices.first(), d.matrices.last()) else {
        return Err("linear DNG: no colour matrix".into());
    };
    // rawler's writer asserts on input it doesn't expect.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        write_linear_dng(d, low, (high.temperature > low.temperature).then_some(high))
    }))
    .map_err(|_| "the DNG writer failed".to_owned())?
    .map_err(|e| format!("DNG: {e}"))
}

fn write_linear_dng(
    d: &LinearDng,
    low: &ColorMatrix,
    high: Option<&ColorMatrix>,
) -> Result<Vec<u8>, rawler::dng::writer::DngError> {
    use rawler::dng::writer::DngWriter;
    use rawler::dng::{DNG_VERSION_V1_1, DngCompression};
    use rawler::formats::tiff::{Rational, SRational};
    use rawler::tags::{DngTag, TiffCommonTag};
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut dng = DngWriter::new(&mut buf, DNG_VERSION_V1_1)?;
    {
        let mut raw = dng.subframe(0);
        // Predictor 1 (left), as Adobe's linear DNGs.
        raw.rgb_image_u16(d.data, d.width, d.height, DngCompression::Lossless, 1)?;
        let nr = d.noise_reduction.clamp(0.0, 1.0);
        raw.ifd_mut().add_tag(
            DngTag::NoiseReductionApplied,
            Rational::new((nr * 100.0).round() as u32, 100),
        );
        raw.finalize()?;
    }
    let neutral: Vec<Rational> = d
        .neutral
        .iter()
        .map(|v| Rational::new((v.clamp(0.0, 64.0) * 1e6).round() as u32, 1_000_000))
        .collect();
    dng.as_shot_neutral(&neutral);
    let matrix = |m: &ColorMatrix| -> Vec<SRational> {
        m.xyz_to_cam
            .iter()
            .flatten()
            .map(|v| SRational::new((v * 10_000.0).round() as i32, 10_000))
            .collect()
    };
    dng.color_matrix(1, illuminant(low.temperature), matrix(low));
    if let Some(high) = high {
        dng.color_matrix(2, illuminant(high.temperature), matrix(high));
    }
    let root = dng.root_ifd_mut();
    root.add_tag(TiffCommonTag::Make, d.make);
    root.add_tag(TiffCommonTag::Model, d.model);
    let unique = if d.model.starts_with(d.make) {
        d.model.to_owned()
    } else {
        format!("{} {}", d.make, d.model)
    };
    root.add_tag(DngTag::UniqueCameraModel, unique.as_str());
    root.add_tag(TiffCommonTag::Software, "Shoebox");
    root.add_tag(TiffCommonTag::Orientation, 1u16);
    if let Some((w, h, px)) = d.thumbnail
        && px.len() == w * h * 3
    {
        let offset = dng.dng.write_data(px)?;
        let root = dng.root_ifd_mut();
        root.add_tag(TiffCommonTag::NewSubFileType, 1u32);
        root.add_tag(TiffCommonTag::ImageWidth, w as u32);
        root.add_tag(TiffCommonTag::ImageLength, h as u32);
        root.add_tag(TiffCommonTag::BitsPerSample, &[8u16, 8, 8][..]);
        root.add_tag(TiffCommonTag::Compression, 1u16);
        root.add_tag(TiffCommonTag::PhotometricInt, 2u16);
        root.add_tag(TiffCommonTag::SamplesPerPixel, 3u16);
        root.add_tag(TiffCommonTag::StripOffsets, offset);
        root.add_tag(TiffCommonTag::StripByteCounts, px.len() as u32);
        root.add_tag(TiffCommonTag::RowsPerStrip, h as u32);
    }
    dng.close()?;
    Ok(buf.into_inner())
}

/// An EXIF light source with this colour temperature (the table Adobe's
/// DNG SDK reads them back with).
fn illuminant(kelvin: f32) -> rawler::imgop::xyz::Illuminant {
    use rawler::imgop::xyz::Illuminant as I;
    [
        (I::A, 2856.0),
        (I::IsoStudioTungsten, 3200.0),
        (I::WhiteFluorescent, 3525.0),
        (I::CoolWhiteFluorescent, 4150.0),
        (I::B, 4874.0),
        (I::DaylightWhiteFluorescent, 5000.0),
        (I::D50, 5003.0),
        (I::Daylight, 5500.0),
        (I::D55, 5503.0),
        (I::DaylightFluorescent, 6430.0),
        (I::CloudyWeather, 6500.0),
        (I::D65, 6504.0),
        (I::C, 6774.0),
        (I::Shade, 7500.0),
        (I::D75, 7504.0),
    ]
    .into_iter()
    .min_by(|a, b| (a.1 - kelvin).abs().total_cmp(&(b.1 - kelvin).abs()))
    .map_or(I::D65, |(i, _)| i)
}

/// What goes into a floating-point linear DNG (see [`float_dng`]).
#[derive(Clone, Copy)]
pub struct FloatDng<'a> {
    pub width: usize,
    pub height: usize,
    /// Camera-native linear RGB, row by row, upright: black 0, the sensor's
    /// white 1.0, values above it kept (an HDR merge's highlights).
    pub data: &'a [f32],
    pub make: &'a str,
    pub model: &'a str,
    /// The camera RGB of a neutral as shot (AsShotNeutral), green 1.
    pub neutral: [f32; 3],
    /// XYZ → camera matrices by calibration temperature, lowest first: the
    /// first and last are written.
    pub matrices: &'a [ColorMatrix],
    /// An upright 8-bit sRGB thumbnail (width, height, pixels) for IFD 0.
    pub thumbnail: Option<(usize, usize, &'a [u8])>,
    /// A larger preview, a baseline JPEG in sRGB, in a SubIFD.
    pub preview: Option<&'a [u8]>,
    /// EXIF to copy (camera, lens, exposure, dates), as rawler read it from
    /// a source.
    pub exif: Option<&'a rawler::decoders::RawMetadata>,
    /// An XMP packet for IFD 0.
    pub xmp: Option<&'a [u8]>,
}

/// Tiles of the floating-point image: this many pixels square.
const FLOAT_TILE: usize = 256;

/// Write a floating-point linear DNG (DNG 1.4): the image a LinearRaw
/// SubIFD of 16-bit (half) floats, three samples a pixel, in 256-pixel
/// tiles, deflated with the floating-point predictor (TIFF predictor 3:
/// each row's bytes split into planes, most significant first, then
/// differenced), as Adobe's HDR merges are stored. WhiteLevel is 1.0, so
/// values above it (to 65504) are highlights beyond the sensor's white.
/// The colour tags are as [`linear_dng`]'s.
pub fn float_dng(d: &FloatDng) -> Result<Vec<u8>, String> {
    if d.data.len() != d.width * d.height * 3 || d.width == 0 || d.height == 0 {
        return Err("float DNG: the image doesn't match its size".into());
    }
    if d.width > usize::from(u16::MAX) || d.height > usize::from(u16::MAX) {
        return Err(format!("{}×{} is too large for DNG", d.width, d.height));
    }
    let (Some(low), Some(high)) = (d.matrices.first(), d.matrices.last()) else {
        return Err("float DNG: no colour matrix".into());
    };
    let tiles = float_tiles(d.data, d.width, d.height)?;
    // rawler's writer asserts on input it doesn't expect.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        write_float_dng(
            d,
            &tiles,
            low,
            (high.temperature > low.temperature).then_some(high),
        )
    }))
    .map_err(|_| "the DNG writer failed".to_owned())?
    .map_err(|e| format!("DNG: {e}"))
}

/// Each tile half floats, byte planes differenced, deflated; row-major,
/// edge tiles padded by repeating the last row and column (readers crop).
fn float_tiles(data: &[f32], width: usize, height: usize) -> Result<Vec<Vec<u8>>, String> {
    use rayon::prelude::*;
    use std::io::Write;
    let across = width.div_ceil(FLOAT_TILE);
    let down = height.div_ceil(FLOAT_TILE);
    (0..across * down)
        .into_par_iter()
        .map(|t| {
            let (tx, ty) = (t % across * FLOAT_TILE, t / across * FLOAT_TILE);
            let row_bytes = FLOAT_TILE * 3 * 2;
            let mut raw = vec![0u8; row_bytes * FLOAT_TILE];
            for (r, row) in raw.chunks_exact_mut(row_bytes).enumerate() {
                let y = (ty + r).min(height - 1);
                let (hi, lo) = row.split_at_mut(FLOAT_TILE * 3);
                for col in 0..FLOAT_TILE {
                    let x = (tx + col).min(width - 1);
                    for c in 0..3 {
                        let v = data[(y * width + x) * 3 + c];
                        let v = if v.is_finite() {
                            v.clamp(0.0, 65504.0)
                        } else {
                            0.0
                        };
                        let [h, l] = half::f16::from_f32(v).to_bits().to_be_bytes();
                        hi[col * 3 + c] = h;
                        lo[col * 3 + c] = l;
                    }
                }
                // Difference each byte from the one a pixel before it.
                for i in (3..row.len()).rev() {
                    row[i] = row[i].wrapping_sub(row[i - 3]);
                }
            }
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(5));
            z.write_all(&raw).map_err(|e| e.to_string())?;
            z.finish().map_err(|e| e.to_string())
        })
        .collect()
}

fn write_float_dng(
    d: &FloatDng,
    tiles: &[Vec<u8>],
    low: &ColorMatrix,
    high: Option<&ColorMatrix>,
) -> Result<Vec<u8>, rawler::dng::writer::DngError> {
    use rawler::dng::DNG_VERSION_V1_4;
    use rawler::dng::writer::DngWriter;
    use rawler::formats::tiff::{Rational, SRational};
    use rawler::tags::{DngTag, TiffCommonTag};
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut dng = DngWriter::new(&mut buf, DNG_VERSION_V1_4)?;
    if let Some(meta) = d.exif {
        dng.load_metadata(meta)?;
    }
    let mut offsets = Vec::with_capacity(tiles.len());
    let mut counts = Vec::with_capacity(tiles.len());
    for t in tiles {
        offsets.push(dng.dng.write_data(t)?);
        counts.push(t.len() as u32);
    }
    {
        let mut raw = dng.subframe(0);
        let ifd = raw.ifd_mut();
        ifd.add_tag(TiffCommonTag::ImageWidth, d.width as u32);
        ifd.add_tag(TiffCommonTag::ImageLength, d.height as u32);
        ifd.add_tag(TiffCommonTag::BitsPerSample, &[16u16, 16, 16][..]);
        ifd.add_tag(TiffCommonTag::SampleFormat, &[3u16, 3, 3][..]);
        // Adobe's deflate; the floating-point predictor.
        ifd.add_tag(TiffCommonTag::Compression, 8u16);
        ifd.add_tag(TiffCommonTag::Predictor, 3u16);
        ifd.add_tag(TiffCommonTag::PhotometricInt, 34892u16);
        ifd.add_tag(TiffCommonTag::SamplesPerPixel, 3u16);
        // PlanarConfiguration: chunky.
        ifd.add_untyped_tag(0x011C, 1u16);
        ifd.add_tag(TiffCommonTag::TileWidth, FLOAT_TILE as u32);
        ifd.add_tag(TiffCommonTag::TileLength, FLOAT_TILE as u32);
        ifd.add_tag(TiffCommonTag::TileOffsets, &offsets[..]);
        ifd.add_tag(TiffCommonTag::TileByteCounts, &counts[..]);
        ifd.add_tag(TiffCommonTag::WhiteLevel, &[1u32, 1, 1][..]);
        raw.finalize()?;
    }
    if let Some(jpeg) = d.preview
        && let Ok((w, h)) = shoebox_decode::jpeg::dimensions(jpeg)
    {
        let at = dng.dng.write_data(jpeg)?;
        let mut p = dng.subframe(1);
        let ifd = p.ifd_mut();
        ifd.add_tag(TiffCommonTag::ImageWidth, w);
        ifd.add_tag(TiffCommonTag::ImageLength, h);
        ifd.add_tag(TiffCommonTag::BitsPerSample, &[8u16, 8, 8][..]);
        ifd.add_tag(TiffCommonTag::Compression, 7u16);
        ifd.add_tag(TiffCommonTag::PhotometricInt, 6u16);
        ifd.add_tag(TiffCommonTag::SamplesPerPixel, 3u16);
        ifd.add_tag(TiffCommonTag::StripOffsets, at);
        ifd.add_tag(TiffCommonTag::StripByteCounts, jpeg.len() as u32);
        ifd.add_tag(TiffCommonTag::RowsPerStrip, h);
        // sRGB.
        ifd.add_tag(DngTag::PreviewColorSpace, 2u32);
        p.finalize()?;
    }
    let neutral: Vec<Rational> = d
        .neutral
        .iter()
        .map(|v| Rational::new((v.clamp(0.0, 64.0) * 1e6).round() as u32, 1_000_000))
        .collect();
    dng.as_shot_neutral(&neutral);
    let matrix = |m: &ColorMatrix| -> Vec<SRational> {
        m.xyz_to_cam
            .iter()
            .flatten()
            .map(|v| SRational::new((v * 10_000.0).round() as i32, 10_000))
            .collect()
    };
    dng.color_matrix(1, illuminant(low.temperature), matrix(low));
    if let Some(high) = high {
        dng.color_matrix(2, illuminant(high.temperature), matrix(high));
    }
    if let Some(xmp) = d.xmp {
        dng.xpacket(xmp)?;
    }
    let root = dng.root_ifd_mut();
    root.add_tag(TiffCommonTag::Make, d.make);
    root.add_tag(TiffCommonTag::Model, d.model);
    let unique = if d.model.starts_with(d.make) {
        d.model.to_owned()
    } else {
        format!("{} {}", d.make, d.model)
    };
    root.add_tag(DngTag::UniqueCameraModel, unique.as_str());
    root.add_tag(TiffCommonTag::Software, "Shoebox");
    root.add_tag(TiffCommonTag::Orientation, 1u16);
    if let Some((w, h, px)) = d.thumbnail
        && px.len() == w * h * 3
    {
        let offset = dng.dng.write_data(px)?;
        let root = dng.root_ifd_mut();
        root.add_tag(TiffCommonTag::NewSubFileType, 1u32);
        root.add_tag(TiffCommonTag::ImageWidth, w as u32);
        root.add_tag(TiffCommonTag::ImageLength, h as u32);
        root.add_tag(TiffCommonTag::BitsPerSample, &[8u16, 8, 8][..]);
        root.add_tag(TiffCommonTag::Compression, 1u16);
        root.add_tag(TiffCommonTag::PhotometricInt, 2u16);
        root.add_tag(TiffCommonTag::SamplesPerPixel, 3u16);
        root.add_tag(TiffCommonTag::StripOffsets, offset);
        root.add_tag(TiffCommonTag::StripByteCounts, px.len() as u32);
        root.add_tag(TiffCommonTag::RowsPerStrip, h as u32);
    }
    dng.close()?;
    Ok(buf.into_inner())
}
