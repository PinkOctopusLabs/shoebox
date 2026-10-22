//! Object removal's inpainting model (EDITING-SPEC, Healing and object
//! removal): MI-GAN (Sargsyan, Navasardyan, Xu and Shi, "MI-GAN: A Simple
//! Baseline for Image Inpainting on Mobile Devices", ICCV 2023; Picsart AI
//! Research; code and weights MIT), the 512 px model trained on Places2.
//! Ported to candle from the authors' inference network
//! (github.com/Picsart-AI-Research/MI-GAN, commit 2b793c5e,
//! `lib/model_zoo/migan_inference.py`, fed as `scripts/demo.py` and
//! `scripts/create_onnx_pipeline.py` do), reading their own checkpoint
//! (`migan_512_places2.pt`, a state dict).
//!
//! Convolutions only (no FFT, so candle runs it as it is): an encoder and
//! a StyleGAN-like synthesis network of depthwise-separable 3×3 blocks,
//! with fixed `[1, 3, 3, 1]` filters to halve and double, skips from each
//! encoder level, and fixed noise (stored in the checkpoint). Everything
//! runs channels last (`[H, W, C]`, one picture): the depthwise
//! convolutions as shifted multiply-adds, the resampling filters as
//! strided taps (halving) or four 2×2 phases (doubling), the 1×1s as
//! matmuls. The noise fixes the size: [`SIZE`] square.
//!
//! [`Inpainter::inpaint`] takes the picture (0–1, sRGB-like) and which
//! pixels are known, and returns the whole picture as the network draws
//! it; callers keep the known pixels and blend.

// Channel loops read clearer indexed.
#![allow(clippy::needless_range_loop)]

use std::path::Path;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;

use crate::{MlError, Result};

/// The square the network sees.
pub const SIZE: usize = 512;
/// The checkpoint's file name.
pub const FILE: &str = "migan_512_places2.pt";
/// Identifies the model (fills kept from another are made again).
pub const MODEL_ID: &str = "migan-512-places2/1";

/// Channels at a resolution: `min(32768 / res, 512)`.
fn channels(res: usize) -> usize {
    (32768 / res).min(512)
}

fn err(e: candle_core::Error) -> MlError {
    MlError::Model(e)
}

/// `lrelu_agc`: leaky ReLU (0.2), gain √2, clamped to ±256.
fn act(x: &Tensor) -> candle_core::Result<Tensor> {
    (x.maximum(&(x * 0.2)?)? * std::f64::consts::SQRT_2)?.clamp(-256f32, 256f32)
}

/// A depthwise 3×3 convolution (padding 1) with bias, as shifted
/// multiply-adds.
struct Depthwise {
    /// Each tap's weights, `[C]`.
    taps: Vec<Tensor>,
    bias: Tensor,
}

impl Depthwise {
    fn new(c: usize, vb: VarBuilder) -> candle_core::Result<Self> {
        let w = vb.get((c, 1, 3, 3), "weight")?.reshape((c, 9))?.t()?;
        let taps = (0..9)
            .map(|t| w.get(t)?.contiguous())
            .collect::<candle_core::Result<_>>()?;
        Ok(Self {
            taps,
            bias: vb.get(c, "bias")?,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let (h, w, c) = x.dims3()?;
        // Padded and flattened, each tap is one contiguous run of rows the
        // padded width wide (an extra row covers the last taps' overrun);
        // the padding's columns are dropped at the end.
        let pw = w + 2;
        let p = x
            .pad_with_zeros(0, 1, 2)?
            .pad_with_zeros(1, 1, 1)?
            .reshape(((h + 3) * pw, c))?;
        let n = h * pw;
        let mut acc = p.narrow(0, 0, n)?.broadcast_mul(&self.taps[0])?;
        for (t, k) in self.taps.iter().enumerate().skip(1) {
            let s = p.narrow(0, (t / 3) * pw + t % 3, n)?;
            acc = (acc + s.broadcast_mul(k)?)?;
        }
        acc.broadcast_add(&self.bias)?
            .reshape((h, pw, c))?
            .narrow(1, 0, w)?
            .contiguous()
    }
}

/// A 4×4 depthwise filter's taps, `[C]` each (row-major).
fn filter_taps(c: usize, vb: VarBuilder) -> candle_core::Result<Vec<Tensor>> {
    let w = vb
        .get((c, 1, 4, 4), "filter.weight")?
        .reshape((c, 16))?
        .t()?;
    (0..16).map(|t| w.get(t)?.contiguous()).collect()
}

/// `Downsample2d`: the 4×4 filter with stride 2 and padding 1.
struct Down {
    taps: Vec<Tensor>,
}

impl Down {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let (h, w, c) = x.dims3()?;
        let (oh, ow) = (h / 2, w / 2);
        // Padded 1 before and 2 after: the last taps' pairs run one past
        // the padding (into a zero that's never read).
        let p = x.pad_with_zeros(0, 1, 2)?.pad_with_zeros(1, 1, 2)?;
        let mut acc: Option<Tensor> = None;
        for (t, k) in self.taps.iter().enumerate() {
            let (i, j) = (t / 4, t % 4);
            // Rows i, i + 2, …; columns j, j + 2, ….
            let s = p
                .narrow(0, i, 2 * oh)?
                .reshape((oh, 2, w + 3, c))?
                .narrow(1, 0, 1)?
                .narrow(2, j, 2 * ow)?
                .reshape((oh, 2 * ow, c))?
                .reshape((oh, ow, 2, c))?
                .narrow(2, 0, 1)?
                .squeeze(2)?;
            let y = s.broadcast_mul(k)?;
            acc = Some(match acc {
                Some(a) => (a + y)?,
                None => y,
            });
        }
        match acc {
            Some(a) => a.contiguous(),
            None => candle_core::bail!("no taps"),
        }
    }
}

/// `Upsample2d`: nearest ×2 keeping one pixel of each 2×2 (the rest zero),
/// padded 2 before and 1 after, then the 4×4 filter. Worked as four
/// phases of a 2×2 filter on the input: output row `2y` reads input rows
/// `y − 1` and `y` (taps 0 and 2), row `2y + 1` rows `y` and `y + 1` (taps
/// 1 and 3); columns likewise.
struct Up {
    taps: Vec<Tensor>,
}

impl Up {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let (h, w, c) = x.dims3()?;
        let p = x.pad_with_zeros(0, 1, 1)?.pad_with_zeros(1, 1, 1)?;
        // (tap, offset into the padded input) per output phase.
        const PHASE: [[(usize, usize); 2]; 2] = [[(0, 0), (2, 1)], [(1, 1), (3, 2)]];
        let mut rows = Vec::with_capacity(2);
        for py in 0..2 {
            let mut cols = Vec::with_capacity(2);
            for px in 0..2 {
                let mut acc: Option<Tensor> = None;
                for &(i, dy) in &PHASE[py] {
                    for &(j, dx) in &PHASE[px] {
                        let s = p.narrow(0, dy, h)?.narrow(1, dx, w)?;
                        let y = s.broadcast_mul(&self.taps[i * 4 + j])?;
                        acc = Some(match acc {
                            Some(a) => (a + y)?,
                            None => y,
                        });
                    }
                }
                match acc {
                    Some(a) => cols.push(a),
                    None => candle_core::bail!("no taps"),
                }
            }
            // Columns interleaved: [H, W, 2, C] → [H, 2W, C].
            rows.push(Tensor::stack(&cols, 2)?.reshape((h, 2 * w, c))?);
        }
        Tensor::stack(&rows, 1)?.reshape((2 * h, 2 * w, c))
    }
}

/// A 1×1 convolution as a matmul: `[Cin, Cout]`, optional bias.
struct Pointwise {
    weight: Tensor,
    bias: Option<Tensor>,
}

impl Pointwise {
    fn new(cin: usize, cout: usize, bias: bool, vb: VarBuilder) -> candle_core::Result<Self> {
        let weight = vb
            .get((cout, cin, 1, 1), "weight")?
            .reshape((cout, cin))?
            .t()?
            .contiguous()?;
        let bias = if bias {
            Some(vb.get(cout, "bias")?)
        } else {
            None
        };
        Ok(Self { weight, bias })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let (h, w, c) = x.dims3()?;
        let y = x.reshape((h * w, c))?.matmul(&self.weight)?;
        let y = match &self.bias {
            Some(b) => y.broadcast_add(b)?,
            None => y,
        };
        y.reshape((h, w, ()))
    }
}

/// `SeparableConv2d`: depthwise 3×3 (activated), optionally halved, 1×1,
/// optionally doubled, the fixed noise added, activated.
struct Separable {
    dw: Depthwise,
    down: Option<Down>,
    pw: Pointwise,
    up: Option<Up>,
    /// `[H, W, 1]`: the stored noise times its strength.
    noise: Option<Tensor>,
}

impl Separable {
    fn new(
        cin: usize,
        cout: usize,
        down: bool,
        up: bool,
        noise: bool,
        vb: VarBuilder,
    ) -> candle_core::Result<Self> {
        let noise = if noise {
            let strength = vb.get((), "noise_strength")?;
            let n = vb.get_unchecked("noise_const")?;
            let (h, w) = n.dims2()?;
            Some(n.broadcast_mul(&strength)?.reshape((h, w, 1))?)
        } else {
            None
        };
        Ok(Self {
            dw: Depthwise::new(cin, vb.pp("conv1"))?,
            down: if down {
                Some(Down {
                    taps: filter_taps(cin, vb.pp("downsample"))?,
                })
            } else {
                None
            },
            pw: Pointwise::new(cin, cout, false, vb.pp("conv2"))?,
            up: if up {
                Some(Up {
                    taps: filter_taps(cout, vb.pp("upsample"))?,
                })
            } else {
                None
            },
            noise,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let mut x = act(&self.dw.forward(x)?)?;
        if let Some(d) = &self.down {
            x = d.forward(&x)?;
        }
        x = self.pw.forward(&x)?;
        if let Some(u) = &self.up {
            x = u.forward(&x)?;
        }
        if let Some(n) = &self.noise {
            x = x.broadcast_add(n)?;
        }
        act(&x)
    }
}

/// An encoder level: `conv1` (its result a skip), `conv2` halving.
struct EncoderBlock {
    fromrgb: Option<Pointwise>,
    conv1: Separable,
    conv2: Separable,
}

/// A synthesis level: `conv1` (doubling, except the first), the skip added,
/// `conv2`; its picture added to the previous one doubled.
struct SynthesisBlock {
    conv1: Separable,
    conv2: Separable,
    torgb: Pointwise,
    /// Doubles the previous level's picture (not the first level).
    up: Option<Up>,
}

/// MI-GAN, loaded once.
pub struct Inpainter {
    device: Device,
    /// From [`SIZE`] down to 8, then the 4×4 level.
    encoder: Vec<(usize, EncoderBlock)>,
    /// From 4 up to [`SIZE`].
    synthesis: Vec<(usize, SynthesisBlock)>,
}

impl Inpainter {
    /// Load the authors' checkpoint (a state dict).
    pub fn load(path: &Path, device: &Device) -> Result<Self> {
        Self::build(path, device).map_err(err)
    }

    fn build(path: &Path, device: &Device) -> candle_core::Result<Self> {
        let vb = VarBuilder::from_pth(path, DType::F32, device)?;
        let enc = vb.pp("encoder");
        let mut encoder = Vec::new();
        let mut res = SIZE;
        while res >= 4 {
            let b = enc.pp(format!("b{res}"));
            let (ci, co) = (channels(res), channels((res / 2).max(4)));
            let last = res == 4;
            encoder.push((
                res,
                EncoderBlock {
                    fromrgb: if res == SIZE {
                        Some(Pointwise::new(4, ci, true, b.pp("fromrgb"))?)
                    } else {
                        None
                    },
                    conv1: Separable::new(ci, ci, false, false, false, b.pp("conv1"))?,
                    conv2: Separable::new(
                        ci,
                        if last { ci } else { co },
                        !last,
                        false,
                        false,
                        b.pp("conv2"),
                    )?,
                },
            ));
            res /= 2;
        }
        let syn = vb.pp("synthesis");
        let mut synthesis = Vec::new();
        let mut res = 4;
        while res <= SIZE {
            let b = syn.pp(format!("b{res}"));
            let first = res == 4;
            let (ci, co) = (channels(if first { 4 } else { res / 2 }), channels(res));
            synthesis.push((
                res,
                SynthesisBlock {
                    conv1: Separable::new(ci, co, false, !first, !first, b.pp("conv1"))?,
                    conv2: Separable::new(co, co, false, false, !first, b.pp("conv2"))?,
                    torgb: Pointwise::new(co, 3, true, b.pp("torgb"))?,
                    up: if first {
                        None
                    } else {
                        Some(Up {
                            taps: filter_taps(3, b.pp("upsample"))?,
                        })
                    },
                },
            ));
            res *= 2;
        }
        Ok(Self {
            device: device.clone(),
            encoder,
            synthesis,
        })
    }

    /// Fill the unknown pixels of `rgb` ([`SIZE`]², three 0–1 values a
    /// pixel, sRGB-like as the network was trained on; `known` true where
    /// the picture is kept). The whole picture as the network draws it,
    /// 0–1: callers keep what was known.
    pub fn inpaint(&self, rgb: &[f32], known: &[bool]) -> Result<Vec<f32>> {
        let n = SIZE * SIZE;
        if rgb.len() != 3 * n || known.len() != n {
            return Err(MlError::Format(format!(
                "inpainting takes a {SIZE}×{SIZE} picture"
            )));
        }
        self.inpaint_inner(rgb, known).map_err(err)
    }

    fn inpaint_inner(&self, rgb: &[f32], known: &[bool]) -> candle_core::Result<Vec<f32>> {
        // The mask less a half, then the picture (−1 to 1) where known.
        let mut input = vec![0f32; 4 * SIZE * SIZE];
        for (i, &k) in known.iter().enumerate() {
            let m = if k { 1.0 } else { 0.0 };
            input[i * 4] = m - 0.5;
            for c in 0..3 {
                input[i * 4 + 1 + c] = (rgb[i * 3 + c].clamp(0.0, 1.0) * 2.0 - 1.0) * m;
            }
        }
        let img = Tensor::from_vec(input, (SIZE, SIZE, 4), &self.device)?;
        let mut x: Option<Tensor> = None;
        let mut skips = Vec::with_capacity(self.encoder.len());
        for (_, b) in &self.encoder {
            if let Some(f) = &b.fromrgb {
                let y = act(&f.forward(&img)?)?;
                x = Some(match x {
                    Some(x) => (x + y)?,
                    None => y,
                });
            }
            let Some(xi) = &x else {
                candle_core::bail!("no input");
            };
            let feat = b.conv1.forward(xi)?;
            x = Some(b.conv2.forward(&feat)?);
            skips.push(feat);
        }
        let Some(mut x) = x else {
            candle_core::bail!("no encoder");
        };
        let mut picture: Option<Tensor> = None;
        // Skips from the coarsest up.
        for ((_, b), skip) in self.synthesis.iter().zip(skips.iter().rev()) {
            x = b.conv1.forward(&x)?;
            x = (x + skip)?;
            x = b.conv2.forward(&x)?;
            let y = b.torgb.forward(&x)?;
            picture = Some(match (picture, &b.up) {
                (Some(p), Some(up)) => (up.forward(&p)? + y)?,
                _ => y,
            });
        }
        let Some(p) = picture else {
            candle_core::bail!("no synthesis");
        };
        let p = ((p + 1.0)? * 0.5)?.clamp(0f32, 1f32)?;
        p.to_device(&Device::Cpu)?.flatten_all()?.to_vec1::<f32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vb(map: Vec<(&str, Tensor)>) -> VarBuilder<'static> {
        let map = map.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        VarBuilder::from_tensors(map, DType::F32, &Device::Cpu)
    }

    /// A picture `[h, w, c]` of small integers, and its values.
    fn picture(h: usize, w: usize, c: usize) -> candle_core::Result<(Tensor, Vec<f32>)> {
        let xs: Vec<f32> = (0..h * w * c)
            .map(|i| ((i * 37) % 11) as f32 - 5.0)
            .collect();
        Ok((Tensor::from_vec(xs.clone(), (h, w, c), &Device::Cpu)?, xs))
    }

    /// Per-channel 4×4 filters `[c, 1, 4, 4]` and their values.
    fn filters(c: usize) -> candle_core::Result<(Tensor, Vec<f32>)> {
        let ws: Vec<f32> = (0..c * 16)
            .map(|i| ((i * 13) % 7) as f32 * 0.1 - 0.3)
            .collect();
        Ok((
            Tensor::from_vec(ws.clone(), (c, 1, 4, 4), &Device::Cpu)?,
            ws,
        ))
    }

    #[test]
    fn channels_per_level() {
        assert_eq!(channels(512), 64);
        assert_eq!(channels(256), 128);
        assert_eq!(channels(128), 256);
        assert_eq!(channels(64), 512);
        assert_eq!(channels(4), 512);
    }

    #[test]
    fn depthwise_matches_direct_sums() -> candle_core::Result<()> {
        let (h, w, c) = (7, 6, 2);
        let (x, xs) = picture(h, w, c)?;
        let ws: Vec<f32> = (0..c * 9)
            .map(|i| ((i * 5) % 9) as f32 * 0.1 - 0.4)
            .collect();
        let dw = Depthwise::new(
            c,
            vb(vec![
                (
                    "weight",
                    Tensor::from_vec(ws.clone(), (c, 1, 3, 3), &Device::Cpu)?,
                ),
                (
                    "bias",
                    Tensor::from_vec(vec![0.5f32, -1.0], c, &Device::Cpu)?,
                ),
            ]),
        )?;
        let got = dw.forward(&x)?.flatten_all()?.to_vec1::<f32>()?;
        let bias = [0.5f32, -1.0];
        for y in 0..h {
            for xx in 0..w {
                for ch in 0..c {
                    let mut s = bias[ch];
                    for ky in 0..3 {
                        for kx in 0..3 {
                            let (sy, sx) = (y as i32 + ky as i32 - 1, xx as i32 + kx as i32 - 1);
                            if sy < 0 || sx < 0 || sy >= h as i32 || sx >= w as i32 {
                                continue;
                            }
                            s += xs[(sy as usize * w + sx as usize) * c + ch]
                                * ws[ch * 9 + ky * 3 + kx];
                        }
                    }
                    let g = got[(y * w + xx) * c + ch];
                    assert!((g - s).abs() < 1e-4, "{g} {s}");
                }
            }
        }
        Ok(())
    }

    /// PyTorch's `Conv2d(k=4, stride=2, padding=1, groups=C)`, summed
    /// directly.
    #[test]
    fn halving_matches_a_strided_convolution() -> candle_core::Result<()> {
        let (h, w, c) = (8, 6, 2);
        let (x, xs) = picture(h, w, c)?;
        let (f, ws) = filters(c)?;
        let down = Down {
            taps: filter_taps(c, vb(vec![("filter.weight", f)]))?,
        };
        let got = down.forward(&x)?;
        assert_eq!(got.dims3()?, (h / 2, w / 2, c));
        let got = got.flatten_all()?.to_vec1::<f32>()?;
        for y in 0..h / 2 {
            for xx in 0..w / 2 {
                for ch in 0..c {
                    let mut s = 0.0;
                    for i in 0..4 {
                        for j in 0..4 {
                            let (sy, sx) =
                                (2 * y as i32 + i as i32 - 1, 2 * xx as i32 + j as i32 - 1);
                            if sy < 0 || sx < 0 || sy >= h as i32 || sx >= w as i32 {
                                continue;
                            }
                            s += xs[(sy as usize * w + sx as usize) * c + ch]
                                * ws[ch * 16 + i * 4 + j];
                        }
                    }
                    let g = got[(y * (w / 2) + xx) * c + ch];
                    assert!((g - s).abs() < 1e-4, "{g} {s}");
                }
            }
        }
        Ok(())
    }

    /// `Upsample2d` as the authors wrote it: nearest ×2, times the
    /// `[[1, 0], [0, 0]]` pattern, padded (2, 1), the 4×4 filter.
    #[test]
    fn doubling_matches_the_reference() -> candle_core::Result<()> {
        let (h, w, c) = (5, 4, 2);
        let (x, xs) = picture(h, w, c)?;
        let (f, ws) = filters(c)?;
        let up = Up {
            taps: filter_taps(c, vb(vec![("filter.weight", f)]))?,
        };
        let got = up.forward(&x)?;
        let (oh, ow) = (2 * h, 2 * w);
        assert_eq!(got.dims3()?, (oh, ow, c));
        let got = got.flatten_all()?.to_vec1::<f32>()?;
        // Zero-stuffed and padded 2 before, 1 after.
        let (ph, pw) = (oh + 3, ow + 3);
        let mut z = vec![0f32; ph * pw * c];
        for y in 0..h {
            for xx in 0..w {
                for ch in 0..c {
                    z[((2 * y + 2) * pw + 2 * xx + 2) * c + ch] = xs[(y * w + xx) * c + ch];
                }
            }
        }
        for y in 0..oh {
            for xx in 0..ow {
                for ch in 0..c {
                    let mut s = 0.0;
                    for i in 0..4 {
                        for j in 0..4 {
                            s += z[((y + i) * pw + xx + j) * c + ch] * ws[ch * 16 + i * 4 + j];
                        }
                    }
                    let g = got[(y * ow + xx) * c + ch];
                    assert!((g - s).abs() < 1e-4, "({y}, {xx}, {ch}): {g} {s}");
                }
            }
        }
        Ok(())
    }

    #[test]
    fn activation_is_scaled_and_clamped() -> candle_core::Result<()> {
        let x = Tensor::from_vec(vec![-10f32, 0.0, 1.0, 1000.0], 4, &Device::Cpu)?;
        let y = act(&x)?.to_vec1::<f32>()?;
        let r2 = std::f32::consts::SQRT_2;
        assert!((y[0] + 2.0 * r2).abs() < 1e-5);
        assert_eq!(y[1], 0.0);
        assert!((y[2] - r2).abs() < 1e-5);
        assert_eq!(y[3], 256.0);
        Ok(())
    }
}
