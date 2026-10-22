//! AI raw denoise (EDITING-SPEC, Noise reduction, tier 2): RawNIND's UtNet2
//! Bayer denoiser (Brummer and De Vleeschouwer, 2025; code GPL-3.0,
//! weights GPL-3.0 and CC BY 4.0), ported to candle from the authors'
//! `src/rawnind/models/raw_denoiser.py` (github.com/trougnouf/rawnind_jddc,
//! commit e80fb7b1). It denoises and demosaics in one: packed RGGB at half
//! resolution in, camera RGB at full resolution out, at an arbitrary gain
//! that's matched to the input's mean afterwards (darktable-ai's notes on
//! feeding it: `models/rawdenoise-nind`).
//!
//! The checkpoint is the canonical Bayer one darktable-ai uses
//! (`DenoiserTrainingBayerToProfiledRGB_4ch_2024-02-21-…-4`). It runs in
//! f32: its activations overflow f16.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{Conv2d, ConvTranspose2d, VarBuilder};

use crate::{MlError, Result};

/// The checkpoint's file name.
pub const FILE: &str = "denoiser_bayer2prgb_utnet2.pt";
/// Identifies the model (cached results from another are made again).
pub const MODEL_ID: &str = "rawnind-utnet2-bayer-2024-02-21/1";

const FUNIT: usize = 32;
/// Tiles of packed pixels: a multiple of 16 (four 2× pools), with this
/// much context each side that's thrown away.
const TILE: usize = 512;
const MARGIN: usize = 32;

/// Two 3×3 convolutions, each with LeakyReLU (0.2).
struct Pair {
    a: Conv2d,
    b: Conv2d,
}

impl Pair {
    fn new(cin: usize, cout: usize, vb: VarBuilder) -> candle_core::Result<Self> {
        let cfg = candle_nn::Conv2dConfig {
            padding: 1,
            ..Default::default()
        };
        Ok(Self {
            a: candle_nn::conv2d(cin, cout, 3, cfg, vb.pp("0"))?,
            b: candle_nn::conv2d(cout, cout, 3, cfg, vb.pp("2"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let leaky = |x: Tensor| -> candle_core::Result<Tensor> { x.maximum(&(&x * 0.2)?) };
        leaky(self.b.forward(&leaky(self.a.forward(x)?)?)?)
    }
}

/// The U-Net.
pub struct RawDenoiser {
    device: Device,
    down: [Pair; 4],
    bottom: Pair,
    ups: [ConvTranspose2d; 4],
    tconvs: [Pair; 4],
    out: Conv2d,
}

impl RawDenoiser {
    /// Load the authors' checkpoint (a state dict).
    pub fn load(path: &Path, device: &Device) -> Result<Self> {
        Ok(Self::build(path, device)?)
    }

    fn build(path: &Path, device: &Device) -> candle_core::Result<Self> {
        let vb = VarBuilder::from_pth(path, DType::F32, device)?;
        let f = FUNIT;
        let up = |cin: usize, cout: usize, name: &str| {
            candle_nn::conv_transpose2d(
                cin,
                cout,
                2,
                candle_nn::ConvTranspose2dConfig {
                    stride: 2,
                    ..Default::default()
                },
                vb.pp(name),
            )
        };
        Ok(Self {
            device: device.clone(),
            down: [
                Pair::new(4, f, vb.pp("convs1"))?,
                Pair::new(f, 2 * f, vb.pp("convs2"))?,
                Pair::new(2 * f, 4 * f, vb.pp("convs3"))?,
                Pair::new(4 * f, 8 * f, vb.pp("convs4"))?,
            ],
            bottom: Pair::new(8 * f, 16 * f, vb.pp("bottom"))?,
            ups: [
                up(16 * f, 8 * f, "up1")?,
                up(8 * f, 4 * f, "up2")?,
                up(4 * f, 2 * f, "up3")?,
                up(2 * f, f, "up4")?,
            ],
            tconvs: [
                Pair::new(16 * f, 8 * f, vb.pp("tconvs1"))?,
                Pair::new(8 * f, 4 * f, vb.pp("tconvs2"))?,
                Pair::new(4 * f, 2 * f, vb.pp("tconvs3"))?,
                Pair::new(2 * f, f, vb.pp("tconvs4"))?,
            ],
            out: candle_nn::conv2d(f, 12, 1, Default::default(), vb.pp("output_module.0"))?,
        })
    }

    /// The network on `[1, 4, h, w]` (h, w multiples of 16): `[1, 3, 2h, 2w]`.
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let l1 = self.down[0].forward(x)?;
        let l2 = self.down[1].forward(&l1.max_pool2d(2)?)?;
        let l3 = self.down[2].forward(&l2.max_pool2d(2)?)?;
        let l4 = self.down[3].forward(&l3.max_pool2d(2)?)?;
        let b = self.bottom.forward(&l4.max_pool2d(2)?)?;
        let l = Tensor::cat(&[self.ups[0].forward(&b)?, l4], 1)?;
        let l = Tensor::cat(&[self.ups[1].forward(&self.tconvs[0].forward(&l)?)?, l3], 1)?;
        let l = Tensor::cat(&[self.ups[2].forward(&self.tconvs[1].forward(&l)?)?, l2], 1)?;
        let l = Tensor::cat(&[self.ups[3].forward(&self.tconvs[2].forward(&l)?)?, l1], 1)?;
        let l = self.tconvs[3].forward(&l)?;
        pixel_shuffle(&self.out.forward(&l)?, 2)
    }

    /// Denoise and demosaic packed RGGB planes (`packed`: four `h`×`w`
    /// planes, 0–1, not white-balanced): camera RGB planes at `2h`×`2w`,
    /// gain-matched to the input's mean. Tile by tile; `progress` hears the
    /// fraction done; `cancel` stops it.
    pub fn denoise(
        &self,
        packed: &[f32],
        h: usize,
        w: usize,
        progress: &dyn Fn(f32),
        cancel: &AtomicBool,
    ) -> Result<Vec<f32>> {
        if h == 0 || w == 0 || packed.len() < 4 * h * w {
            return Err(MlError::Format("no image".into()));
        }
        let core = TILE - 2 * MARGIN;
        let (oh, ow) = (2 * h, 2 * w);
        let mut out = vec![0f32; 3 * oh * ow];
        let tiles_y = h.div_ceil(core);
        let tiles_x = w.div_ceil(core);
        let total = (tiles_x * tiles_y) as f32;
        let mut done = 0.0;
        // Mirror outside the image (darktable-ai's `mirror_cropped`).
        let mirror = |i: i64, n: usize| -> usize {
            let n = n as i64;
            let m = if n == 1 {
                0
            } else {
                (i.rem_euclid(2 * n - 2) - (n - 1)).abs()
            };
            (n - 1 - m) as usize
        };
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                if cancel.load(Ordering::Relaxed) {
                    return Err(MlError::Cancelled);
                }
                let (cy, cx) = (ty * core, tx * core);
                let (y0, x0) = (cy as i64 - MARGIN as i64, cx as i64 - MARGIN as i64);
                let mut tile = vec![0f32; 4 * TILE * TILE];
                for c in 0..4 {
                    for y in 0..TILE {
                        let sy = mirror(y0 + y as i64, h);
                        for x in 0..TILE {
                            let sx = mirror(x0 + x as i64, w);
                            tile[(c * TILE + y) * TILE + x] = packed[(c * h + sy) * w + sx];
                        }
                    }
                }
                let input = Tensor::from_vec(tile, (1, 4, TILE, TILE), &self.device)?;
                let result = self
                    .forward(&input)?
                    .to_device(&Device::Cpu)?
                    .flatten_all()?
                    .to_vec1::<f32>()?;
                // Keep the core (twice the size, out of twice the tile).
                let (ch, cw) = (core.min(h - cy), core.min(w - cx));
                let t2 = 2 * TILE;
                for c in 0..3 {
                    for y in 0..2 * ch {
                        let src = (c * t2 + 2 * MARGIN + y) * t2 + 2 * MARGIN;
                        let dst = (c * oh + 2 * cy + y) * ow + 2 * cx;
                        out[dst..dst + 2 * cw].copy_from_slice(&result[src..src + 2 * cw]);
                    }
                }
                done += 1.0;
                progress(done / total);
            }
        }
        // The network's gain is arbitrary: match the input's mean.
        let mean = |v: &[f32]| v.iter().map(|&x| f64::from(x)).sum::<f64>() / v.len().max(1) as f64;
        let (want, got) = (mean(&packed[..4 * h * w]), mean(&out));
        let gain = if got.abs() > 1e-12 {
            (want / got) as f32
        } else {
            1.0
        };
        for v in &mut out {
            *v *= gain;
        }
        Ok(out)
    }
}

/// `nn.PixelShuffle(r)`: `[B, C·r², H, W]` → `[B, C, H·r, W·r]`.
fn pixel_shuffle(x: &Tensor, r: usize) -> candle_core::Result<Tensor> {
    let (b, c, h, w) = x.dims4()?;
    let oc = c / (r * r);
    x.reshape((b, oc, r, r, h, w))?
        .permute((0, 1, 4, 2, 5, 3))?
        .contiguous()?
        .reshape((b, oc, h * r, w * r))
}
