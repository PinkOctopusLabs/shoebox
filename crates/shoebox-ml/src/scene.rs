//! Scene segmentation for Develop's landscape masks (EDITING-SPEC, Local
//! adjustments: AI sky): UperNet on ConvNeXt-B (ImageNet-22K), trained on
//! ADE20K's 150 classes by ConvNeXt's authors (Meta, MIT). Ported to
//! candle from their code (github.com/facebookresearch/ConvNeXt, commit
//! 048efcea, `semantic_segmentation/backbone/convnext.py` and
//! `configs/convnext/upernet_convnext_base_640_160k_ade20k_ms.py`) and
//! the UPerHead and PPM it trains with (open-mmlab/mmsegmentation v0.x,
//! `decode_heads/uper_head.py`, `psp_head.py`; Apache 2.0), reading the
//! authors' own checkpoint (`upernet_convnext_base_22k_640x640.pth`).
//!
//! Everything runs channels last (`[H, W, C]`, one picture): the 7×7
//! depthwise convolutions as 49 shifted multiply-adds, strided ones as
//! reshapes and a matmul, and the head's 3×3 ones as one matmul per tap
//! (batch norms folded in), so nothing is unrolled into columns.
//!
//! [`SceneModel::segment`] gives each [`Part`]'s probability (the sum of
//! its classes' softmax) on a grid a quarter of the size the network saw
//! (the short side [`SHORT`]), soft where the model is unsure.

// Channel loops read clearer indexed.
#![allow(clippy::needless_range_loop)]

use std::path::Path;

use candle_core::{D, DType, Device, Module, Tensor};
use candle_nn::{LayerNorm, Linear, VarBuilder};

use crate::{MlError, Result};

/// The short side the network sees (the checkpoint's training crops, and
/// its test scale).
pub const SHORT: usize = 640;
/// The long side at most (its test scale's).
const LONG: usize = 2560;
/// mmsegmentation's normalisation, on 0–255 values.
const MEAN: [f32; 3] = [123.675, 116.28, 103.53];
const STD: [f32; 3] = [58.395, 57.12, 57.375];

/// ConvNeXt-B: blocks and widths per stage.
const DEPTHS: [usize; 4] = [3, 3, 27, 3];
const DIMS: [usize; 4] = [128, 256, 512, 1024];
/// The head's width, its pyramid pooling scales, and ADE20K's classes.
const CHANNELS: usize = 512;
const POOLS: [usize; 4] = [1, 2, 3, 6];
const CLASSES: usize = 150;

/// The checkpoint's file name.
pub const FILE: &str = "upernet_convnext_base_22k_640x640.pth";
/// Identifies the model (and the class groups: cached masks from another
/// are made again).
pub const MODEL_ID: &str = "upernet-convnext-base-22k-ade20k/1";

/// What a landscape mask can take: groups of ADE20K classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    Sky,
    /// Trees, grass, plants, flowers, palms, fields.
    Vegetation,
    /// Water, sea, rivers, lakes, waterfalls, pools.
    Water,
    /// Mountains and hills.
    Mountains,
    /// Buildings, houses, skyscrapers, towers, huts, bridges, piers and
    /// grandstands, and their walls, windows, doors, columns and awnings.
    Buildings,
    /// Natural and made ground: earth, land, sand, rock, roads, paths,
    /// pavements, dirt tracks, runways, floors.
    Ground,
}

impl Part {
    pub const ALL: [Part; 6] = [
        Part::Sky,
        Part::Vegetation,
        Part::Water,
        Part::Mountains,
        Part::Buildings,
        Part::Ground,
    ];

    /// Its ADE20K classes (mmsegmentation's numbering, from 0).
    pub fn classes(self) -> &'static [usize] {
        match self {
            // sky
            Part::Sky => &[2],
            // tree, grass, plant, field, flower, palm
            Part::Vegetation => &[4, 9, 17, 29, 66, 72],
            // water, sea, river, swimming pool, waterfall, lake
            Part::Water => &[21, 26, 60, 109, 113, 128],
            // mountain, hill
            Part::Mountains => &[16, 68],
            // wall, building, windowpane, door, house, column, skyscraper,
            // grandstand, bridge, hovel, tower, awning, pier
            Part::Buildings => &[0, 1, 8, 14, 25, 42, 48, 51, 61, 79, 84, 86, 140],
            // floor, road, sidewalk, earth, rock, sand, path, runway,
            // dirt track, land
            Part::Ground => &[3, 6, 11, 13, 34, 46, 52, 54, 91, 94],
        }
    }
}

/// Each part's probability, 0–1, on a `width`×`height` grid over the
/// picture (pixel centres line up; row-major).
#[derive(Debug, Clone)]
pub struct Scene {
    pub width: usize,
    pub height: usize,
    parts: Vec<Vec<f32>>,
}

impl Scene {
    pub fn part(&self, p: Part) -> &[f32] {
        let i = Part::ALL.iter().position(|q| *q == p).unwrap_or(0);
        &self.parts[i]
    }
}

fn err(e: candle_core::Error) -> MlError {
    MlError::Model(e)
}

/// The 7×7 depthwise convolution, as shifted multiply-adds.
struct Depthwise {
    /// `[49, C]`: each tap's weights.
    taps: Vec<Tensor>,
    bias: Tensor,
}

impl Depthwise {
    fn new(c: usize, vb: VarBuilder) -> candle_core::Result<Self> {
        let w = vb.get((c, 1, 7, 7), "weight")?.reshape((c, 49))?.t()?;
        let taps = (0..49)
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
        let pw = w + 6;
        let p = x
            .pad_with_zeros(0, 3, 4)?
            .pad_with_zeros(1, 3, 3)?
            .reshape(((h + 7) * pw, c))?;
        let n = h * pw;
        let mut acc = p.narrow(0, 0, n)?.broadcast_mul(&self.taps[0])?;
        for (t, k) in self.taps.iter().enumerate().skip(1) {
            let s = p.narrow(0, (t / 7) * pw + t % 7, n)?;
            acc = (acc + s.broadcast_mul(k)?)?;
        }
        acc.broadcast_add(&self.bias)?
            .reshape((h, pw, c))?
            .narrow(1, 0, w)?
            .contiguous()
    }
}

/// A ConvNeXt block: depthwise 7×7, layer norm, the MLP, layer scale.
struct Block {
    dw: Depthwise,
    norm: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    gamma: Tensor,
}

impl Block {
    fn new(c: usize, vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            dw: Depthwise::new(c, vb.pp("dwconv"))?,
            norm: candle_nn::layer_norm(c, 1e-6, vb.pp("norm"))?,
            fc1: candle_nn::linear(c, 4 * c, vb.pp("pwconv1"))?,
            fc2: candle_nn::linear(4 * c, c, vb.pp("pwconv2"))?,
            gamma: vb.get(c, "gamma")?,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let y = self.dw.forward(x)?;
        let y = self.norm.forward(&y)?;
        let y = self.fc1.forward(&y)?.gelu_erf()?;
        let y = self.fc2.forward(&y)?.broadcast_mul(&self.gamma)?;
        x + y
    }
}

/// A `k`×`k` convolution with stride `k` (the stem, the downsamplings):
/// pixels' blocks as rows, one matmul.
struct Patchify {
    k: usize,
    /// `[k·k·Cin, Cout]`, rows in (y, x, channel) order.
    weight: Tensor,
    bias: Tensor,
}

impl Patchify {
    fn new(cin: usize, cout: usize, k: usize, vb: VarBuilder) -> candle_core::Result<Self> {
        let weight = vb
            .get((cout, cin, k, k), "weight")?
            .permute((2, 3, 1, 0))?
            .reshape((k * k * cin, cout))?
            .contiguous()?;
        Ok(Self {
            k,
            weight,
            bias: vb.get(cout, "bias")?,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let (h, w, c) = x.dims3()?;
        let (k, oh, ow) = (self.k, h / self.k, w / self.k);
        let rows = x
            .narrow(0, 0, oh * k)?
            .narrow(1, 0, ow * k)?
            .reshape((oh, k, ow, k, c))?
            .permute((0, 2, 1, 3, 4))?
            .contiguous()?
            .reshape((oh * ow, k * k * c))?;
        rows.matmul(&self.weight)?
            .broadcast_add(&self.bias)?
            .reshape((oh, ow, ()))
    }
}

/// mmcv's `ConvModule` (convolution, batch norm, ReLU), `k`×`k` with
/// stride 1 and padding `k/2`, the norm folded into the weights: one
/// matmul per tap. Its input may come in parts (channels concatenated),
/// so the widest concatenation is never made.
struct ConvBnRelu {
    k: usize,
    /// Per tap, per input part: `[Cin_part, Cout]`.
    taps: Vec<Vec<Tensor>>,
    bias: Tensor,
}

impl ConvBnRelu {
    fn new(parts: &[usize], cout: usize, k: usize, vb: VarBuilder) -> candle_core::Result<Self> {
        let cin: usize = parts.iter().sum();
        let w = vb.get((cout, cin, k, k), "conv.weight")?;
        let bn = vb.pp("bn");
        let gamma = bn.get(cout, "weight")?;
        let beta = bn.get(cout, "bias")?;
        let mean = bn.get(cout, "running_mean")?;
        let var = bn.get(cout, "running_var")?;
        let scale = gamma.broadcast_div(&(var + 1e-5)?.sqrt()?)?;
        let bias = (beta - mean.broadcast_mul(&scale)?)?;
        let w = w.broadcast_mul(&scale.reshape((cout, 1, 1, 1))?)?;
        let mut taps = Vec::with_capacity(k * k);
        for t in 0..k * k {
            let tap = w
                .narrow(2, t / k, 1)?
                .narrow(3, t % k, 1)?
                .squeeze(3)?
                .squeeze(2)?;
            let mut at = 0;
            let mut per = Vec::with_capacity(parts.len());
            for &n in parts {
                per.push(tap.narrow(1, at, n)?.t()?.contiguous()?);
                at += n;
            }
            taps.push(per);
        }
        Ok(Self { k, taps, bias })
    }

    /// `xs`: the input's parts, each `[H, W, Cin_part]`.
    fn forward(&self, xs: &[&Tensor]) -> candle_core::Result<Tensor> {
        let (h, w, _) = xs[0].dims3()?;
        let p = self.k / 2;
        let mut acc: Option<Tensor> = None;
        for (x, i) in xs.iter().zip(0..) {
            let c = x.dim(2)?;
            let padded = if p > 0 {
                x.pad_with_zeros(0, p, p)?.pad_with_zeros(1, p, p)?
            } else {
                (*x).clone()
            };
            for (t, per) in self.taps.iter().enumerate() {
                let s = if p > 0 {
                    padded
                        .narrow(0, t / self.k, h)?
                        .narrow(1, t % self.k, w)?
                        .contiguous()?
                } else {
                    padded.contiguous()?
                };
                let y = s.reshape((h * w, c))?.matmul(&per[i])?;
                acc = Some(match acc {
                    Some(a) => (a + y)?,
                    None => y,
                });
            }
        }
        let Some(acc) = acc else {
            candle_core::bail!("no input");
        };
        acc.broadcast_add(&self.bias)?.relu()?.reshape((h, w, ()))
    }
}

/// `[H, W, C]` resized to `oh`×`ow` (bilinear, align_corners false).
fn resize(x: &Tensor, oh: usize, ow: usize) -> candle_core::Result<Tensor> {
    let (h, w, _) = x.dims3()?;
    if (h, w) == (oh, ow) {
        return Ok(x.clone());
    }
    x.permute((2, 0, 1))?
        .unsqueeze(0)?
        .contiguous()?
        .upsample_bilinear2d(oh, ow, false)?
        .squeeze(0)?
        .permute((1, 2, 0))?
        .contiguous()
}

/// `AdaptiveAvgPool2d(s)` of `[H, W, C]`: PyTorch's bins (from
/// ⌊i·H/s⌋ to ⌈(i+1)·H/s⌉).
fn adaptive_pool(x: &Tensor, s: usize) -> candle_core::Result<Tensor> {
    let (h, w, _) = x.dims3()?;
    let bin = |i: usize, n: usize| (i * n / s, ((i + 1) * n).div_ceil(s));
    let mut rows = Vec::with_capacity(s);
    for i in 0..s {
        let (y0, y1) = bin(i, h);
        let mut cols = Vec::with_capacity(s);
        for j in 0..s {
            let (x0, x1) = bin(j, w);
            let m = x
                .narrow(0, y0, y1 - y0)?
                .narrow(1, x0, x1 - x0)?
                .mean_keepdim(0)?
                .mean_keepdim(1)?;
            cols.push(m);
        }
        rows.push(Tensor::cat(&cols, 1)?);
    }
    Tensor::cat(&rows, 0)
}

/// The scene model, loaded once.
pub struct SceneModel {
    device: Device,
    stem: Patchify,
    stem_norm: LayerNorm,
    down: Vec<(LayerNorm, Patchify)>,
    stages: Vec<Vec<Block>>,
    outs: Vec<LayerNorm>,
    ppm: Vec<ConvBnRelu>,
    bottleneck: ConvBnRelu,
    lateral: Vec<ConvBnRelu>,
    fpn: Vec<ConvBnRelu>,
    fpn_bottleneck: ConvBnRelu,
    cls: Linear,
    /// `[150, parts]`: which classes each part sums.
    groups: Tensor,
}

impl SceneModel {
    /// Load the authors' checkpoint.
    pub fn load(path: &Path, device: &Device) -> Result<Self> {
        Self::build(path, device).map_err(err)
    }

    fn build(path: &Path, device: &Device) -> candle_core::Result<Self> {
        let vb = VarBuilder::from_pth_with_state(path, DType::F32, "state_dict", device)?;
        let bb = vb.pp("backbone");
        let ds = bb.pp("downsample_layers");
        let stem = Patchify::new(3, DIMS[0], 4, ds.pp("0").pp("0"))?;
        let stem_norm = candle_nn::layer_norm(DIMS[0], 1e-6, ds.pp("0").pp("1"))?;
        let mut down = Vec::with_capacity(3);
        for i in 1..4 {
            let d = ds.pp(i.to_string());
            down.push((
                candle_nn::layer_norm(DIMS[i - 1], 1e-6, d.pp("0"))?,
                Patchify::new(DIMS[i - 1], DIMS[i], 2, d.pp("1"))?,
            ));
        }
        let mut stages = Vec::with_capacity(4);
        let mut outs = Vec::with_capacity(4);
        for i in 0..4 {
            let s = bb.pp("stages").pp(i.to_string());
            stages.push(
                (0..DEPTHS[i])
                    .map(|j| Block::new(DIMS[i], s.pp(j.to_string())))
                    .collect::<candle_core::Result<Vec<_>>>()?,
            );
            outs.push(candle_nn::layer_norm(
                DIMS[i],
                1e-6,
                bb.pp(format!("norm{i}")),
            )?);
        }
        let head = vb.pp("decode_head");
        let ppm = (0..POOLS.len())
            .map(|i| {
                ConvBnRelu::new(
                    &[DIMS[3]],
                    CHANNELS,
                    1,
                    head.pp("psp_modules").pp(i.to_string()).pp("1"),
                )
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        let bottleneck = ConvBnRelu::new(
            &[DIMS[3], CHANNELS, CHANNELS, CHANNELS, CHANNELS],
            CHANNELS,
            3,
            head.pp("bottleneck"),
        )?;
        let lateral = (0..3)
            .map(|i| {
                ConvBnRelu::new(
                    &[DIMS[i]],
                    CHANNELS,
                    1,
                    head.pp("lateral_convs").pp(i.to_string()),
                )
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        let fpn = (0..3)
            .map(|i| {
                ConvBnRelu::new(
                    &[CHANNELS],
                    CHANNELS,
                    3,
                    head.pp("fpn_convs").pp(i.to_string()),
                )
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        let fpn_bottleneck =
            ConvBnRelu::new(&[CHANNELS; 4], CHANNELS, 3, head.pp("fpn_bottleneck"))?;
        let cls = {
            let c = head.pp("conv_seg");
            let w = c.get((CLASSES, CHANNELS, 1, 1), "weight")?;
            Linear::new(
                w.reshape((CLASSES, CHANNELS))?,
                Some(c.get(CLASSES, "bias")?),
            )
        };
        let mut g = vec![0f32; CLASSES * Part::ALL.len()];
        for (j, p) in Part::ALL.iter().enumerate() {
            for &c in p.classes() {
                g[c * Part::ALL.len() + j] = 1.0;
            }
        }
        let groups = Tensor::from_vec(g, (CLASSES, Part::ALL.len()), device)?;
        Ok(Self {
            device: device.clone(),
            stem,
            stem_norm,
            down,
            stages,
            outs,
            ppm,
            bottleneck,
            lateral,
            fpn,
            fpn_bottleneck,
            cls,
            groups,
        })
    }

    /// Segment a picture (8-bit sRGB, `width`×`height`, three bytes a
    /// pixel): each part's probability on a grid a quarter of the size
    /// the network saw.
    pub fn segment(&self, rgb: &[u8], width: usize, height: usize) -> Result<Scene> {
        if width == 0 || height == 0 || rgb.len() < width * height * 3 {
            return Err(MlError::Format("no image".into()));
        }
        self.segment_inner(rgb, width, height).map_err(err)
    }

    fn segment_inner(&self, rgb: &[u8], w: usize, h: usize) -> candle_core::Result<Scene> {
        let (iw, ih) = input_size(w, h);
        let input = Tensor::from_vec(prepare(rgb, w, h, iw, ih), (ih, iw, 3), &self.device)?;
        // ConvNeXt: four stages, each output layer-normed.
        let mut x = self.stem_norm.forward(&self.stem.forward(&input)?)?;
        let mut feats = Vec::with_capacity(4);
        for i in 0..4 {
            if i > 0 {
                let (norm, conv) = &self.down[i - 1];
                x = conv.forward(&norm.forward(&x)?)?;
            }
            for b in &self.stages[i] {
                x = b.forward(&x)?;
            }
            feats.push(self.outs[i].forward(&x)?);
        }
        // UPerHead: the pyramid pooling module on the coarsest level.
        let top = &feats[3];
        let (th, tw, _) = top.dims3()?;
        let mut psp = vec![top.clone()];
        for (conv, &s) in self.ppm.iter().zip(&POOLS) {
            let pooled = conv.forward(&[&adaptive_pool(top, s)?])?;
            psp.push(resize(&pooled, th, tw)?);
        }
        let psp: Vec<&Tensor> = psp.iter().collect();
        let mut laterals: Vec<Tensor> = (0..3)
            .map(|i| self.lateral[i].forward(&[&feats[i]]))
            .collect::<candle_core::Result<_>>()?;
        laterals.push(self.bottleneck.forward(&psp)?);
        // Top-down.
        for i in (1..4).rev() {
            let (ph, pw, _) = laterals[i - 1].dims3()?;
            let up = resize(&laterals[i], ph, pw)?;
            laterals[i - 1] = (&laterals[i - 1] + up)?;
        }
        let (oh, ow, _) = laterals[0].dims3()?;
        let mut outs = Vec::with_capacity(4);
        for i in 0..3 {
            outs.push(resize(&self.fpn[i].forward(&[&laterals[i]])?, oh, ow)?);
        }
        outs.push(resize(&laterals[3], oh, ow)?);
        let outs: Vec<&Tensor> = outs.iter().collect();
        let f = self.fpn_bottleneck.forward(&outs)?;
        let logits = self.cls.forward(&f.reshape((oh * ow, CHANNELS))?)?;
        let probs = candle_nn::ops::softmax(&logits, D::Minus1)?;
        let parts = probs
            .matmul(&self.groups)?
            .t()?
            .contiguous()?
            .to_device(&Device::Cpu)?
            .to_vec2::<f32>()?;
        Ok(Scene {
            width: ow,
            height: oh,
            parts: parts
                .into_iter()
                .map(|p| p.into_iter().map(|v| v.clamp(0.0, 1.0)).collect())
                .collect(),
        })
    }
}

/// The size the network sees for a `w`×`h` picture: the short side
/// [`SHORT`] (the long at most [`LONG`]), both multiples of 32 (the
/// coarsest level's stride).
pub fn input_size(w: usize, h: usize) -> (usize, usize) {
    let (short, long) = (w.min(h).max(1) as f64, w.max(h).max(1) as f64);
    let k = (SHORT as f64 / short).min(LONG as f64 / long);
    let side = |v: usize| ((v as f64 * k / 32.0).round() as usize).max(1) * 32;
    (side(w), side(h))
}

/// The picture resized to `ow`×`oh` (area-averaged when shrinking,
/// bilinear when growing), normalised, channels last.
fn prepare(rgb: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<f32> {
    let mut out = vec![0f32; 3 * ow * oh];
    let (kx, ky) = (w as f32 / ow as f32, h as f32 / oh as f32);
    let at = |x: usize, y: usize, c: usize| f32::from(rgb[(y * w + x) * 3 + c]);
    // The source pixels an output one covers, along one axis.
    let span = |o: usize, k: f32, n: usize| {
        let a = ((o as f32 * k) as usize).min(n - 1);
        let b = (((o + 1) as f32 * k).ceil() as usize).clamp(a + 1, n);
        a..b
    };
    for oy in 0..oh {
        for ox in 0..ow {
            let mut acc = [0f32; 3];
            if kx > 1.0 || ky > 1.0 {
                let (xs, ys) = (span(ox, kx, w), span(oy, ky, h));
                let n = (xs.len() * ys.len()) as f32;
                for y in ys {
                    for x in xs.clone() {
                        for c in 0..3 {
                            acc[c] += at(x, y, c) / n;
                        }
                    }
                }
            } else {
                let fx = ((ox as f32 + 0.5) * kx - 0.5).clamp(0.0, (w - 1) as f32);
                let fy = ((oy as f32 + 0.5) * ky - 0.5).clamp(0.0, (h - 1) as f32);
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                for c in 0..3 {
                    let top = at(x0, y0, c) + (at(x1, y0, c) - at(x0, y0, c)) * tx;
                    let bot = at(x0, y1, c) + (at(x1, y1, c) - at(x0, y1, c)) * tx;
                    acc[c] = top + (bot - top) * ty;
                }
            }
            for c in 0..3 {
                out[(oy * ow + ox) * 3 + c] = (acc[c] - MEAN[c]) / STD[c];
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_multiples_of_32() {
        assert_eq!(input_size(6000, 4000), (960, 640));
        assert_eq!(input_size(4000, 6000), (640, 960));
        assert_eq!(input_size(640, 640), (640, 640));
        // Panoramas: the long side at most 2560.
        let (w, h) = input_size(12000, 2000);
        assert_eq!((w % 32, h % 32), (0, 0));
        assert!(w <= 2560 && h < 640, "{w}×{h}");
    }

    #[test]
    fn parts_dont_share_classes() {
        let mut seen = [false; CLASSES];
        for p in Part::ALL {
            for &c in p.classes() {
                assert!(c < CLASSES && !seen[c], "class {c}");
                seen[c] = true;
            }
        }
    }

    #[test]
    fn adaptive_pool_matches_pytorch_bins() -> candle_core::Result<()> {
        // 5 rows of 1 column: bins of 3 are rows 0–1, 1–3, 3–4 (and every
        // column bin is the one column).
        let x = Tensor::from_vec(vec![1f32, 2., 3., 4., 5.], (5, 1, 1), &Device::Cpu)?;
        let p = adaptive_pool(&x, 3)?.flatten_all()?.to_vec1::<f32>()?;
        assert_eq!(p, vec![1.5, 1.5, 1.5, 3.0, 3.0, 3.0, 4.5, 4.5, 4.5]);
        Ok(())
    }

    #[test]
    fn depthwise_and_folded_convs_match_direct_sums() -> candle_core::Result<()> {
        let d = Device::Cpu;
        let (h, w, c) = (9, 8, 2);
        let xs: Vec<f32> = (0..h * w * c)
            .map(|i| ((i * 37) % 11) as f32 - 5.0)
            .collect();
        let x = Tensor::from_vec(xs.clone(), (h, w, c), &d)?;
        let ws: Vec<f32> = (0..c * 49)
            .map(|i| ((i * 13) % 7) as f32 * 0.1 - 0.3)
            .collect();
        let mut map = std::collections::HashMap::new();
        map.insert(
            "weight".to_owned(),
            Tensor::from_vec(ws.clone(), (c, 1, 7, 7), &d)?,
        );
        map.insert(
            "bias".to_owned(),
            Tensor::from_vec(vec![0.5f32, -1.0], c, &d)?,
        );
        let vb = VarBuilder::from_tensors(map, DType::F32, &d);
        let got = Depthwise::new(c, vb)?
            .forward(&x)?
            .flatten_all()?
            .to_vec1::<f32>()?;
        let bias = [0.5f32, -1.0];
        for y in 0..h {
            for xx in 0..w {
                for ch in 0..c {
                    let mut s = bias[ch];
                    for ky in 0..7 {
                        for kx in 0..7 {
                            let (sy, sx) = (y as i32 + ky as i32 - 3, xx as i32 + kx as i32 - 3);
                            if sy < 0 || sx < 0 || sy >= h as i32 || sx >= w as i32 {
                                continue;
                            }
                            s += xs[(sy as usize * w + sx as usize) * c + ch]
                                * ws[ch * 49 + ky * 7 + kx];
                        }
                    }
                    let g = got[(y * w + xx) * c + ch];
                    assert!((g - s).abs() < 1e-4, "{g} {s}");
                }
            }
        }
        // A 3×3 ConvModule (identity norm) against the same sums, its
        // input in two parts.
        let (cout, k) = (3, 3);
        let cw: Vec<f32> = (0..cout * c * 9)
            .map(|i| ((i * 5) % 9) as f32 * 0.1 - 0.4)
            .collect();
        let mut map = std::collections::HashMap::new();
        map.insert(
            "conv.weight".to_owned(),
            Tensor::from_vec(cw.clone(), (cout, c, k, k), &d)?,
        );
        map.insert("bn.weight".to_owned(), Tensor::ones(cout, DType::F32, &d)?);
        map.insert("bn.bias".to_owned(), Tensor::zeros(cout, DType::F32, &d)?);
        map.insert(
            "bn.running_mean".to_owned(),
            Tensor::zeros(cout, DType::F32, &d)?,
        );
        map.insert(
            "bn.running_var".to_owned(),
            Tensor::full(1.0f32 - 1e-5, cout, &d)?,
        );
        let vb = VarBuilder::from_tensors(map, DType::F32, &d);
        let conv = ConvBnRelu::new(&[1, 1], cout, k, vb)?;
        let (a, b) = (x.narrow(2, 0, 1)?, x.narrow(2, 1, 1)?);
        let got = conv.forward(&[&a, &b])?.flatten_all()?.to_vec1::<f32>()?;
        for y in 0..h {
            for xx in 0..w {
                for o in 0..cout {
                    let mut s = 0.0f32;
                    for ch in 0..c {
                        for ky in 0..k {
                            for kx in 0..k {
                                let (sy, sx) =
                                    (y as i32 + ky as i32 - 1, xx as i32 + kx as i32 - 1);
                                if sy < 0 || sx < 0 || sy >= h as i32 || sx >= w as i32 {
                                    continue;
                                }
                                s += xs[(sy as usize * w + sx as usize) * c + ch]
                                    * cw[((o * c + ch) * k + ky) * k + kx];
                            }
                        }
                    }
                    let g = got[(y * w + xx) * cout + o];
                    assert!((g - s.max(0.0)).abs() < 1e-3, "{g} {s}");
                }
            }
        }
        Ok(())
    }
}
