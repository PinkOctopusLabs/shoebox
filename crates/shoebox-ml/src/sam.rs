//! SAM 2.1 (Meta, Apache 2.0), the Tiny model's image predictor: click on a
//! photo, get the object there as a mask (EDITING-SPEC, Local adjustments:
//! AI object). Ported to candle from Meta's reference code
//! (github.com/facebookresearch/sam2, commit 2b90b9f5, `sam2/modeling`:
//! `backbones/hieradet.py`, `backbones/image_encoder.py`,
//! `sam/prompt_encoder.py`, `sam/mask_decoder.py`, `sam/transformer.py`,
//! `sam2_base.py`, and `sam2_image_predictor.py`), reading Meta's own
//! checkpoint (`sam2.1_hiera_tiny.pt`).
//!
//! - [`Sam::embed`] runs the image encoder once per picture (Hiera-T, the
//!   FPN neck, the decoder's high-resolution projections): about a second.
//! - [`Sam::predict`] runs the prompt encoder and mask decoder for clicks
//!   (positive and negative): milliseconds, so every click redraws.
//!
//! Only what still images need: no memory attention or video tracking.

// Channel loops read clearer indexed.
#![allow(clippy::needless_range_loop)]

use std::path::Path;

use candle_core::{D, DType, Device, Module, Tensor};
use candle_nn::{Conv2d, ConvTranspose2d, LayerNorm, Linear, VarBuilder};

use crate::{MlError, Result};

/// The square the encoder sees.
const SIZE: usize = 1024;
/// ImageNet's mean and standard deviation, as the predictor normalises.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Hiera-T: blocks per stage, windows per stage, the blocks with global
/// attention.
const STAGES: [usize; 4] = [1, 2, 7, 2];
const WINDOWS: [usize; 4] = [8, 4, 14, 7];
const GLOBAL: [usize; 3] = [5, 7, 9];
const EMBED: usize = 96;
/// The decoder's width.
const DIM: usize = 256;

/// The checkpoint's file name.
pub const FILE: &str = "sam2.1_hiera_tiny.pt";
/// Identifies the model (cached masks from another are made again).
pub const MODEL_ID: &str = "sam2.1-hiera-tiny/1";

fn err(e: candle_core::Error) -> MlError {
    MlError::Model(e)
}

/// `LayerNorm2d`: normalised over channels (dim 1) of NCHW.
struct LayerNorm2d {
    weight: Tensor,
    bias: Tensor,
}

impl LayerNorm2d {
    fn new(c: usize, vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            weight: vb.get(c, "weight")?.reshape((1, c, 1, 1))?,
            bias: vb.get(c, "bias")?.reshape((1, c, 1, 1))?,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let u = x.mean_keepdim(1)?;
        let d = x.broadcast_sub(&u)?;
        let s = d.sqr()?.mean_keepdim(1)?;
        let x = d.broadcast_div(&(s + 1e-6)?.sqrt()?)?;
        x.broadcast_mul(&self.weight)?.broadcast_add(&self.bias)
    }
}

/// `MLP`: linear layers, the activation between them.
struct Mlp {
    layers: Vec<Linear>,
    gelu: bool,
}

impl Mlp {
    fn new(
        input: usize,
        hidden: usize,
        output: usize,
        n: usize,
        gelu: bool,
        vb: VarBuilder,
    ) -> candle_core::Result<Self> {
        let mut layers = Vec::with_capacity(n);
        for i in 0..n {
            let a = if i == 0 { input } else { hidden };
            let b = if i + 1 == n { output } else { hidden };
            layers.push(candle_nn::linear(a, b, vb.pp(format!("layers.{i}")))?);
        }
        Ok(Self { layers, gelu })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let mut x = x.clone();
        for (i, l) in self.layers.iter().enumerate() {
            x = l.forward(&x)?;
            if i + 1 < self.layers.len() {
                x = if self.gelu { x.gelu_erf()? } else { x.relu()? };
            }
        }
        Ok(x)
    }
}

/// Scaled dot-product attention on `[B, heads, N, d]`.
fn attention(q: &Tensor, k: &Tensor, v: &Tensor) -> candle_core::Result<Tensor> {
    let d = q.dim(D::Minus1)? as f64;
    let w = (q.contiguous()?.matmul(&k.t()?.contiguous()?)? / d.sqrt())?;
    let w = candle_nn::ops::softmax_last_dim(&w)?;
    w.matmul(&v.contiguous()?)
}

/// `[B, H, W, C]` → `[B, C, H, W]`, 2×2 max pool, and back.
fn pool(x: &Tensor) -> candle_core::Result<Tensor> {
    x.permute((0, 3, 1, 2))?
        .max_pool2d(2)?
        .permute((0, 2, 3, 1))?
        .contiguous()
}

/// Windows of `ws`×`ws` (padding with zeros as needed): `[B·n, ws, ws, C]`
/// and the padded size.
fn partition(x: &Tensor, ws: usize) -> candle_core::Result<(Tensor, (usize, usize))> {
    let (b, h, w, c) = x.dims4()?;
    let (ph, pw) = ((ws - h % ws) % ws, (ws - w % ws) % ws);
    let x = if ph > 0 || pw > 0 {
        x.pad_with_zeros(1, 0, ph)?.pad_with_zeros(2, 0, pw)?
    } else {
        x.clone()
    };
    let (hp, wp) = (h + ph, w + pw);
    let x = x
        .reshape((b, hp / ws, ws, wp / ws, ws, c))?
        .permute((0, 1, 3, 2, 4, 5))?
        .contiguous()?
        .reshape((b * (hp / ws) * (wp / ws), ws, ws, c))?;
    Ok((x, (hp, wp)))
}

/// The windows back into `[B, H, W, C]`, padding removed.
fn unpartition(
    x: &Tensor,
    ws: usize,
    (hp, wp): (usize, usize),
    (h, w): (usize, usize),
) -> candle_core::Result<Tensor> {
    let n = x.dim(0)?;
    let c = x.dim(3)?;
    let b = n / (hp * wp / ws / ws);
    let x = x
        .reshape((b, hp / ws, wp / ws, ws, ws, c))?
        .permute((0, 1, 3, 2, 4, 5))?
        .contiguous()?
        .reshape((b, hp, wp, c))?;
    if hp > h || wp > w {
        x.narrow(1, 0, h)?.narrow(2, 0, w)?.contiguous()
    } else {
        Ok(x)
    }
}

/// `MultiScaleBlock`: (windowed) attention, pooling queries where a stage
/// begins, then an MLP.
struct Block {
    norm1: LayerNorm,
    qkv: Linear,
    attn_proj: Linear,
    norm2: LayerNorm,
    mlp: Mlp,
    /// Where the width changes.
    proj: Option<Linear>,
    heads: usize,
    window: usize,
    q_pool: bool,
    dim_out: usize,
}

impl Block {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let mut shortcut = x.clone();
        let x = self.norm1.forward(x)?;
        if let Some(p) = &self.proj {
            shortcut = p.forward(&x)?;
            if self.q_pool {
                shortcut = pool(&shortcut)?;
            }
        }
        let (_, h, w, _) = x.dims4()?;
        let (x, mut pad) = if self.window > 0 {
            partition(&x, self.window)?
        } else {
            (x.clone(), (h, w))
        };
        // Attention, pooling the queries.
        let (b, wh, ww, _) = x.dims4()?;
        let qkv = self.qkv.forward(&x)?.reshape((
            b,
            wh * ww,
            3,
            self.heads,
            self.dim_out / self.heads,
        ))?;
        let q = qkv.narrow(2, 0, 1)?.squeeze(2)?.contiguous()?;
        let k = qkv.narrow(2, 1, 1)?.squeeze(2)?.contiguous()?;
        let v = qkv.narrow(2, 2, 1)?.squeeze(2)?.contiguous()?;
        let (q, qh, qw) = if self.q_pool {
            let q = pool(&q.reshape((b, wh, ww, self.dim_out))?)?;
            let (_, qh, qw, _) = q.dims4()?;
            (
                q.reshape((b, qh * qw, self.heads, self.dim_out / self.heads))?,
                qh,
                qw,
            )
        } else {
            (q, wh, ww)
        };
        let out = attention(
            &q.transpose(1, 2)?,
            &k.transpose(1, 2)?,
            &v.transpose(1, 2)?,
        )?
        .transpose(1, 2)?
        .reshape((b, qh, qw, self.dim_out))?;
        let out = self.attn_proj.forward(&out)?;
        let mut window = self.window;
        let (sh, sw) = (shortcut.dim(1)?, shortcut.dim(2)?);
        if self.q_pool {
            window /= 2;
            pad = (
                sh + (window - sh % window) % window,
                sw + (window - sw % window) % window,
            );
        }
        let out = if self.window > 0 {
            unpartition(&out, window, pad, (sh, sw))?
        } else {
            out
        };
        let x = (shortcut + out)?;
        let m = self.mlp.forward(&self.norm2.forward(&x)?)?;
        x + m
    }
}

/// PyTorch's bicubic weights (A = −0.75) for a fraction `t`, for the
/// samples at −1, 0, 1 and 2.
fn cubic(t: f32) -> [f32; 4] {
    let a = -0.75f32;
    let near = |x: f32| ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0;
    let far = |x: f32| ((a * x - 5.0 * a) * x + 8.0 * a) * x - 4.0 * a;
    [far(t + 1.0), near(t), near(1.0 - t), far(2.0 - t)]
}

/// `F.interpolate(mode="bicubic", align_corners=False)` of `[C, h, w]`
/// (row-major) to `oh`×`ow`.
fn bicubic(src: &[f32], c: usize, h: usize, w: usize, oh: usize, ow: usize) -> Vec<f32> {
    let taps = |n: usize, on: usize| -> Vec<([usize; 4], [f32; 4])> {
        (0..on)
            .map(|i| {
                let s = (i as f32 + 0.5) * n as f32 / on as f32 - 0.5;
                let f = s.floor();
                let idx = [-1i64, 0, 1, 2].map(|d| (f as i64 + d).clamp(0, n as i64 - 1) as usize);
                (idx, cubic(s - f))
            })
            .collect()
    };
    let (ys, xs) = (taps(h, oh), taps(w, ow));
    let mut out = vec![0f32; c * oh * ow];
    for ch in 0..c {
        let plane = &src[ch * h * w..(ch + 1) * h * w];
        for (oy, (yi, yw)) in ys.iter().enumerate() {
            for (ox, (xi, xw)) in xs.iter().enumerate() {
                let mut v = 0.0;
                for a in 0..4 {
                    let mut row = 0.0;
                    for b in 0..4 {
                        row += plane[yi[a] * w + xi[b]] * xw[b];
                    }
                    v += row * yw[a];
                }
                out[ch * oh * ow + oy * ow + ox] = v;
            }
        }
    }
    out
}

/// One cross or self attention of the decoder's transformer.
struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
    heads: usize,
}

impl Attention {
    fn new(dim: usize, heads: usize, down: usize, vb: VarBuilder) -> candle_core::Result<Self> {
        let inner = dim / down;
        Ok(Self {
            q: candle_nn::linear(dim, inner, vb.pp("q_proj"))?,
            k: candle_nn::linear(dim, inner, vb.pp("k_proj"))?,
            v: candle_nn::linear(dim, inner, vb.pp("v_proj"))?,
            out: candle_nn::linear(inner, dim, vb.pp("out_proj"))?,
            heads,
        })
    }

    fn forward(&self, q: &Tensor, k: &Tensor, v: &Tensor) -> candle_core::Result<Tensor> {
        let split = |x: Tensor| -> candle_core::Result<Tensor> {
            let (b, n, c) = x.dims3()?;
            x.reshape((b, n, self.heads, c / self.heads))?
                .transpose(1, 2)
        };
        let (q, k, v) = (
            split(self.q.forward(q)?)?,
            split(self.k.forward(k)?)?,
            split(self.v.forward(v)?)?,
        );
        let o = attention(&q, &k, &v)?;
        let (b, h, n, c) = o.dims4()?;
        self.out
            .forward(&o.transpose(1, 2)?.reshape((b, n, h * c))?)
    }
}

/// `TwoWayAttentionBlock`.
struct TwoWay {
    self_attn: Attention,
    norm1: LayerNorm,
    to_image: Attention,
    norm2: LayerNorm,
    mlp: Mlp,
    norm3: LayerNorm,
    norm4: LayerNorm,
    to_token: Attention,
    skip_pe: bool,
}

impl TwoWay {
    fn forward(
        &self,
        queries: &Tensor,
        keys: &Tensor,
        query_pe: &Tensor,
        key_pe: &Tensor,
    ) -> candle_core::Result<(Tensor, Tensor)> {
        let queries = if self.skip_pe {
            self.self_attn.forward(queries, queries, queries)?
        } else {
            let q = (queries + query_pe)?;
            (queries + self.self_attn.forward(&q, &q, queries)?)?
        };
        let queries = self.norm1.forward(&queries)?;
        let q = (&queries + query_pe)?;
        let k = (keys + key_pe)?;
        let queries = (&queries + self.to_image.forward(&q, &k, keys)?)?;
        let queries = self.norm2.forward(&queries)?;
        let queries = (&queries + self.mlp.forward(&queries)?)?;
        let queries = self.norm3.forward(&queries)?;
        let q = (&queries + query_pe)?;
        let k = (keys + key_pe)?;
        let keys = (keys + self.to_token.forward(&k, &q, &queries)?)?;
        let keys = self.norm4.forward(&keys)?;
        Ok((queries, keys))
    }
}

/// The encoder's output for one picture.
pub struct Embedding {
    image: Tensor,
    s0: Tensor,
    s1: Tensor,
    width: usize,
    height: usize,
}

/// A click: where (pixels of the picture embedded) and whether it adds
/// (or, false, takes away).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Click {
    pub x: f32,
    pub y: f32,
    pub add: bool,
}

/// The loaded model.
pub struct Sam {
    device: Device,
    patch: Conv2d,
    /// Hiera's position embedding at the patch grid, `[1, 256, 256, 96]`.
    pos: Tensor,
    blocks: Vec<Block>,
    /// Stage ends (outputs taken after these blocks).
    ends: [usize; 4],
    neck: Vec<Conv2d>,
    conv_s0: Conv2d,
    conv_s1: Conv2d,
    no_mem: Tensor,
    gaussian: Tensor,
    point_embed: Vec<Tensor>,
    not_a_point: Tensor,
    no_mask: Tensor,
    layers: Vec<TwoWay>,
    final_attn: Attention,
    norm_final: LayerNorm,
    tokens: Tensor,
    up1: ConvTranspose2d,
    up_norm: LayerNorm2d,
    up2: ConvTranspose2d,
    hyper: Vec<Mlp>,
    iou: Mlp,
}

impl Sam {
    /// Load Meta's checkpoint (`sam2.1_hiera_tiny.pt`).
    pub fn load(path: &Path, device: &Device) -> Result<Self> {
        Self::build(path, device).map_err(err)
    }

    fn build(path: &Path, device: &Device) -> candle_core::Result<Self> {
        let vb = VarBuilder::from_pth_with_state(path, DType::F32, "model", device)?;
        let enc = vb.pp("image_encoder.trunk");
        let patch = candle_nn::conv2d(
            3,
            EMBED,
            7,
            candle_nn::Conv2dConfig {
                padding: 3,
                stride: 4,
                ..Default::default()
            },
            enc.pp("patch_embed.proj"),
        )?;
        // The windowed position embedding: the background interpolated to
        // the patch grid, the window's tiled over it.
        let grid = SIZE / 4;
        let bg = enc
            .get((1, EMBED, 7, 7), "pos_embed")?
            .to_device(&Device::Cpu)?
            .flatten_all()?
            .to_vec1::<f32>()?;
        let win = enc
            .get((1, EMBED, WINDOWS[0], WINDOWS[0]), "pos_embed_window")?
            .to_device(&Device::Cpu)?
            .flatten_all()?
            .to_vec1::<f32>()?;
        let mut pos = bicubic(&bg, EMBED, 7, 7, grid, grid);
        for c in 0..EMBED {
            for y in 0..grid {
                for x in 0..grid {
                    pos[(c * grid + y) * grid + x] +=
                        win[(c * WINDOWS[0] + y % WINDOWS[0]) * WINDOWS[0] + x % WINDOWS[0]];
                }
            }
        }
        let pos = Tensor::from_vec(pos, (1, EMBED, grid, grid), &Device::Cpu)?
            .permute((0, 2, 3, 1))?
            .contiguous()?
            .to_device(device)?;
        // The blocks.
        let depth: usize = STAGES.iter().sum();
        let ends = {
            let mut e = [0; 4];
            let mut acc = 0;
            for (i, n) in STAGES.iter().enumerate() {
                acc += n;
                e[i] = acc - 1;
            }
            e
        };
        let pooled: Vec<usize> = ends[..3].iter().map(|e| e + 1).collect();
        let (mut dim, mut heads, mut stage) = (EMBED, 1usize, 1usize);
        let mut blocks = Vec::with_capacity(depth);
        for i in 0..depth {
            let mut window = WINDOWS[stage - 1];
            if GLOBAL.contains(&i) {
                window = 0;
            }
            let mut dim_out = dim;
            if i > 0 && ends.contains(&(i - 1)) {
                dim_out = dim * 2;
                heads *= 2;
                stage += 1;
            }
            let b = enc.pp(format!("blocks.{i}"));
            blocks.push(Block {
                norm1: candle_nn::layer_norm(dim, 1e-6, b.pp("norm1"))?,
                qkv: candle_nn::linear(dim, dim_out * 3, b.pp("attn.qkv"))?,
                attn_proj: candle_nn::linear(dim_out, dim_out, b.pp("attn.proj"))?,
                norm2: candle_nn::layer_norm(dim_out, 1e-6, b.pp("norm2"))?,
                mlp: Mlp::new(dim_out, dim_out * 4, dim_out, 2, true, b.pp("mlp"))?,
                proj: if dim != dim_out {
                    Some(candle_nn::linear(dim, dim_out, b.pp("proj"))?)
                } else {
                    None
                },
                heads,
                window,
                q_pool: pooled.contains(&i),
                dim_out,
            });
            dim = dim_out;
        }
        let neck_vb = vb.pp("image_encoder.neck.convs");
        let channels = [768, 384, 192, 96];
        let neck = channels
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                candle_nn::conv2d(
                    c,
                    DIM,
                    1,
                    Default::default(),
                    neck_vb.pp(format!("{i}.conv")),
                )
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        let dec = vb.pp("sam_mask_decoder");
        let conv_s0 = candle_nn::conv2d(DIM, DIM / 8, 1, Default::default(), dec.pp("conv_s0"))?;
        let conv_s1 = candle_nn::conv2d(DIM, DIM / 4, 1, Default::default(), dec.pp("conv_s1"))?;
        let no_mem = vb
            .get((1, 1, DIM), "no_mem_embed")?
            .reshape((1, DIM, 1, 1))?;
        let pe = vb.pp("sam_prompt_encoder");
        let gaussian = pe.get((2, DIM / 2), "pe_layer.positional_encoding_gaussian_matrix")?;
        let point_embed = (0..4)
            .map(|i| pe.get((1, DIM), &format!("point_embeddings.{i}.weight")))
            .collect::<candle_core::Result<Vec<_>>>()?;
        let not_a_point = pe.get((1, DIM), "not_a_point_embed.weight")?;
        let no_mask = pe.get((1, DIM), "no_mask_embed.weight")?;
        let t = dec.pp("transformer");
        let layers = (0..2)
            .map(|i| {
                let l = t.pp(format!("layers.{i}"));
                Ok(TwoWay {
                    self_attn: Attention::new(DIM, 8, 1, l.pp("self_attn"))?,
                    norm1: candle_nn::layer_norm(DIM, 1e-5, l.pp("norm1"))?,
                    to_image: Attention::new(DIM, 8, 2, l.pp("cross_attn_token_to_image"))?,
                    norm2: candle_nn::layer_norm(DIM, 1e-5, l.pp("norm2"))?,
                    mlp: Mlp::new(DIM, 2048, DIM, 2, false, l.pp("mlp"))?,
                    norm3: candle_nn::layer_norm(DIM, 1e-5, l.pp("norm3"))?,
                    norm4: candle_nn::layer_norm(DIM, 1e-5, l.pp("norm4"))?,
                    to_token: Attention::new(DIM, 8, 2, l.pp("cross_attn_image_to_token"))?,
                    skip_pe: i == 0,
                })
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        let final_attn = Attention::new(DIM, 8, 2, t.pp("final_attn_token_to_image"))?;
        let norm_final = candle_nn::layer_norm(DIM, 1e-5, t.pp("norm_final_attn"))?;
        // Output tokens: object score, IoU, four masks.
        let tokens = Tensor::cat(
            &[
                dec.get((1, DIM), "obj_score_token.weight")?,
                dec.get((1, DIM), "iou_token.weight")?,
                dec.get((4, DIM), "mask_tokens.weight")?,
            ],
            0,
        )?;
        let up = candle_nn::ConvTranspose2dConfig {
            stride: 2,
            ..Default::default()
        };
        let up1 = candle_nn::conv_transpose2d(DIM, DIM / 4, 2, up, dec.pp("output_upscaling.0"))?;
        let up_norm = LayerNorm2d::new(DIM / 4, dec.pp("output_upscaling.1"))?;
        let up2 =
            candle_nn::conv_transpose2d(DIM / 4, DIM / 8, 2, up, dec.pp("output_upscaling.3"))?;
        let hyper = (0..4)
            .map(|i| {
                Mlp::new(
                    DIM,
                    DIM,
                    DIM / 8,
                    3,
                    false,
                    dec.pp(format!("output_hypernetworks_mlps.{i}")),
                )
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        let iou = Mlp::new(DIM, 256, 4, 3, false, dec.pp("iou_prediction_head"))?;
        Ok(Self {
            device: device.clone(),
            patch,
            pos,
            blocks,
            ends,
            neck,
            conv_s0,
            conv_s1,
            no_mem,
            gaussian,
            point_embed,
            not_a_point,
            no_mask,
            layers,
            final_attn,
            norm_final,
            tokens,
            up1,
            up_norm,
            up2,
            hyper,
            iou,
        })
    }

    /// Encode a picture (8-bit RGB, `width`×`height`, three bytes a pixel).
    pub fn embed(&self, rgb: &[u8], width: usize, height: usize) -> Result<Embedding> {
        if width == 0 || height == 0 || rgb.len() < width * height * 3 {
            return Err(MlError::Format("no image".into()));
        }
        self.embed_inner(rgb, width, height).map_err(err)
    }

    fn embed_inner(&self, rgb: &[u8], w: usize, h: usize) -> candle_core::Result<Embedding> {
        let input = Tensor::from_vec(square(rgb, w, h), (1, 3, SIZE, SIZE), &self.device)?;
        // Hiera.
        let mut x = self.patch.forward(&input)?.permute((0, 2, 3, 1))?;
        x = x.broadcast_add(&self.pos)?;
        let mut outs = Vec::with_capacity(4);
        for (i, b) in self.blocks.iter().enumerate() {
            x = b.forward(&x)?;
            if self.ends.contains(&i) {
                outs.push(x.permute((0, 3, 1, 2))?.contiguous()?);
            }
        }
        // The FPN neck: top-down into the two coarsest levels; the finest
        // two are the backbone's own (projected). The coarsest is dropped.
        let lateral = |i: usize| self.neck[3 - i].forward(&outs[i]);
        let top = lateral(3)?;
        let (th, tw) = (top.dim(2)?, top.dim(3)?);
        let l2 = (lateral(2)? + top.upsample_nearest2d(th * 2, tw * 2)?)?;
        let l1 = lateral(1)?;
        let l0 = lateral(0)?;
        Ok(Embedding {
            image: l2.broadcast_add(&self.no_mem)?,
            s0: self.conv_s0.forward(&l0)?,
            s1: self.conv_s1.forward(&l1)?,
            width: w,
            height: h,
        })
    }

    /// The random-frequency position encoding of points in 0–1 (`[N, 2]`).
    fn pe(&self, coords: &Tensor) -> candle_core::Result<Tensor> {
        let c = ((coords * 2.0)? - 1.0)?;
        let c = (c.matmul(&self.gaussian)? * (2.0 * std::f64::consts::PI))?;
        Tensor::cat(&[c.sin()?, c.cos()?], D::Minus1)
    }

    /// The object at these clicks: coverage 0–1 per pixel of the picture
    /// embedded (row-major), soft at its edge.
    pub fn predict(&self, e: &Embedding, clicks: &[Click]) -> Result<Vec<f32>> {
        if clicks.is_empty() {
            return Ok(vec![0.0; e.width * e.height]);
        }
        self.predict_inner(e, clicks).map_err(err)
    }

    fn predict_inner(&self, e: &Embedding, clicks: &[Click]) -> candle_core::Result<Vec<f32>> {
        let dev = &self.device;
        // Sparse prompts: the clicks (pixel centres in the 1024 square),
        // then the padding point.
        let (sx, sy) = (SIZE as f32 / e.width as f32, SIZE as f32 / e.height as f32);
        let mut coords = Vec::new();
        for c in clicks {
            coords.extend([
                (c.x * sx + 0.5) / SIZE as f32,
                (c.y * sy + 0.5) / SIZE as f32,
            ]);
        }
        coords.extend([0.5 / SIZE as f32, 0.5 / SIZE as f32]);
        let n = clicks.len() + 1;
        let pe = self.pe(&Tensor::from_vec(coords, (n, 2), dev)?)?;
        let rows = (0..n)
            .map(|i| {
                let row = pe.narrow(0, i, 1)?;
                match clicks.get(i) {
                    Some(c) => row + &self.point_embed[usize::from(c.add)],
                    None => Ok(self.not_a_point.clone()),
                }
            })
            .collect::<candle_core::Result<Vec<_>>>()?;
        let sparse = Tensor::cat(&rows, 0)?.unsqueeze(0)?;
        // The image side: no mask prompt; the grid's position encoding.
        let g = SIZE / 16;
        let mut grid = Vec::with_capacity(g * g * 2);
        for y in 0..g {
            for x in 0..g {
                grid.extend([(x as f32 + 0.5) / g as f32, (y as f32 + 0.5) / g as f32]);
            }
        }
        let image_pe = self
            .pe(&Tensor::from_vec(grid, (g * g, 2), dev)?)?
            .unsqueeze(0)?;
        let src = e
            .image
            .broadcast_add(&self.no_mask.reshape((1, DIM, 1, 1))?)?
            .flatten_from(2)?
            .transpose(1, 2)?
            .contiguous()?;
        // The two-way transformer.
        let tokens = Tensor::cat(&[self.tokens.unsqueeze(0)?, sparse], 1)?;
        let (mut queries, mut keys) = (tokens.clone(), src);
        for l in &self.layers {
            (queries, keys) = l.forward(&queries, &keys, &tokens, &image_pe)?;
        }
        let q = (&queries + &tokens)?;
        let k = (&keys + &image_pe)?;
        let queries = (&queries + self.final_attn.forward(&q, &k, &keys)?)?;
        let hs = self.norm_final.forward(&queries)?;
        // Upscale with the high-resolution features.
        let src = keys.transpose(1, 2)?.reshape((1, DIM, g, g))?;
        let up = self.up1.forward(&src)?.broadcast_add(&e.s1)?;
        let up = self.up_norm.forward(&up)?.gelu_erf()?;
        let up = self.up2.forward(&up)?.broadcast_add(&e.s0)?.gelu_erf()?;
        let (_, c, uh, uw) = up.dims4()?;
        let hyper = (0..4)
            .map(|i| self.hyper[i].forward(&hs.narrow(1, 2 + i, 1)?))
            .collect::<candle_core::Result<Vec<_>>>()?;
        let hyper = Tensor::cat(&hyper, 1)?;
        let masks = hyper
            .matmul(&up.reshape((1, c, uh * uw))?)?
            .reshape((4, uh, uw))?;
        let iou = candle_nn::ops::sigmoid(&self.iou.forward(&hs.narrow(1, 1, 1)?)?)?
            .flatten_all()?
            .to_vec1::<f32>()?;
        // One click is ambiguous: the best of the three masks by predicted
        // quality. Several: the single mask, unless it's unstable.
        let best = (1..4)
            .max_by(|&a, &b| iou[a].total_cmp(&iou[b]))
            .unwrap_or(1);
        let pick = if clicks.len() == 1 {
            best
        } else {
            let single = masks.get(0)?.flatten_all()?.to_vec1::<f32>()?;
            let inter = single.iter().filter(|&&v| v > 0.05).count() as f32;
            let union = single.iter().filter(|&&v| v > -0.05).count() as f32;
            let stable = union == 0.0 || inter / union >= 0.98;
            if stable { 0 } else { best }
        };
        let logits = masks
            .get(pick)?
            .to_device(&Device::Cpu)?
            .flatten_all()?
            .to_vec1::<f32>()?;
        Ok(upscale(&logits, uw, uh, e.width, e.height))
    }
}

/// The picture resized to the 1024 square (area-averaged when shrinking,
/// bilinear when growing), normalised, channels first.
fn square(rgb: &[u8], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; 3 * SIZE * SIZE];
    let (kx, ky) = (w as f32 / SIZE as f32, h as f32 / SIZE as f32);
    let at = |x: usize, y: usize, c: usize| f32::from(rgb[(y * w + x) * 3 + c]) / 255.0;
    for oy in 0..SIZE {
        for ox in 0..SIZE {
            let mut acc = [0f32; 3];
            if kx > 1.0 || ky > 1.0 {
                // The mean of the source pixels this one covers.
                let span = |o: usize, k: f32, n: usize| {
                    let a = ((o as f32 * k) as usize).min(n - 1);
                    let b = (((o + 1) as f32 * k).ceil() as usize).clamp(a + 1, n);
                    a..b
                };
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
                out[(c * SIZE + oy) * SIZE + ox] = (acc[c] - MEAN[c]) / STD[c];
            }
        }
    }
    out
}

/// Low-resolution logits (`lw`×`lh`) to coverage at `w`×`h`: bilinear
/// (align_corners false, as the predictor), then a sigmoid.
fn upscale(logits: &[f32], lw: usize, lh: usize, w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let fy = ((y as f32 + 0.5) * lh as f32 / h as f32 - 0.5).clamp(0.0, (lh - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy - fy.floor());
        let y1 = (y0 + 1).min(lh - 1);
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * lw as f32 / w as f32 - 0.5).clamp(0.0, (lw - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx - fx.floor());
            let x1 = (x0 + 1).min(lw - 1);
            let l = |xx: usize, yy: usize| logits[yy * lw + xx];
            let top = l(x0, y0) + (l(x1, y0) - l(x0, y0)) * tx;
            let bot = l(x0, y1) + (l(x1, y1) - l(x0, y1)) * tx;
            let v = top + (bot - top) * ty;
            out[y * w + x] = 1.0 / (1.0 + (-v).exp());
        }
    }
    out
}
