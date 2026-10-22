//! Monocular depth for Develop's Lens Blur (EDITING-SPEC, Phase 4
//! additions): Depth Anything V2 Small (Yang et al., 2024; code and the
//! Small checkpoint Apache 2.0; Base, Large and Giant are CC BY-NC 4.0 and
//! never used). Ported to candle from the authors' code
//! (DepthAnything/Depth-Anything-V2 at a561b849: `depth_anything_v2/
//! dpt.py`, `dinov2.py`, `util/blocks.py`, `util/transform.py`), reading
//! their checkpoint (`depth_anything_v2_vits.pth`) directly.
//!
//! `candle-transformers` has `depth_anything_v2` and `dinov2`, but they
//! differ from the authors' model where it shows: every resize in the head
//! is nearest-neighbour (the authors' are bilinear, corners aligned), the
//! head only takes square pictures (so a 3:2 photo would be squashed),
//! DINOv2's layer norms use ε 1e-5 (1e-6 here) and its classifier head is
//! required though the checkpoint has none. So this is a port of its own:
//! DINOv2 ViT-S/14 (LayerScale, exact GELU, the position embedding
//! interpolated bicubically for the picture's shape, as DINOv2 does) and
//! the DPT head.
//!
//! [`DepthModel::estimate`] gives relative inverse depth (larger is
//! nearer, affine-invariant: only its order and proportions mean
//! anything) on the grid the network saw: the short side [`SHORT`],
//! both sides multiples of 14.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use candle_core::{DType, Device, IndexOp, Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, ConvTranspose2d, LayerNorm, Linear, VarBuilder};

use crate::{MlError, Result};

/// The checkpoint's file name.
pub const FILE: &str = "depth_anything_v2_vits.pth";
/// Identifies the model and how its picture is made (depth maps kept from
/// another are made again).
pub const MODEL_ID: &str = "depth-anything-v2-small/1";
/// The short side the network sees (its training size).
pub const SHORT: usize = 518;
/// The long side at most (very wide panoramas are squeezed to it).
const LONG: usize = 1792;
const PATCH: usize = 14;
/// ViT-S: width, blocks, heads; the blocks the head reads.
const DIM: usize = 384;
const DEPTH: usize = 12;
const HEADS: usize = 6;
const TAKE: [usize; 4] = [2, 5, 8, 11];
/// The DPT head: its width and each level's channels.
const FEATURES: usize = 64;
const OUT: [usize; 4] = [48, 96, 192, 384];
/// ImageNet's normalisation, on 0–1 values.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// The checkpoint's position embedding grid (518 / 14).
const GRID: usize = 37;

fn err(e: candle_core::Error) -> MlError {
    MlError::Model(e)
}

/// A relative inverse depth map, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Depth {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
}

fn conv(
    vb: VarBuilder,
    cin: usize,
    cout: usize,
    k: usize,
    stride: usize,
) -> candle_core::Result<Conv2d> {
    let cfg = Conv2dConfig {
        padding: k / 2,
        stride,
        ..Default::default()
    };
    candle_nn::conv2d(cin, cout, k, cfg, vb)
}

fn conv_no_bias(vb: VarBuilder, cin: usize, cout: usize) -> candle_core::Result<Conv2d> {
    let cfg = Conv2dConfig {
        padding: 1,
        ..Default::default()
    };
    candle_nn::conv2d_no_bias(cin, cout, 3, cfg, vb)
}

/// One transformer block: attention and MLP, each scaled (LayerScale).
struct Block {
    norm1: LayerNorm,
    qkv: Linear,
    proj: Linear,
    ls1: Tensor,
    norm2: LayerNorm,
    fc1: Linear,
    fc2: Linear,
    ls2: Tensor,
}

impl Block {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        let a = vb.pp("attn");
        let m = vb.pp("mlp");
        Ok(Self {
            norm1: candle_nn::layer_norm(DIM, 1e-6, vb.pp("norm1"))?,
            qkv: candle_nn::linear(DIM, 3 * DIM, a.pp("qkv"))?,
            proj: candle_nn::linear(DIM, DIM, a.pp("proj"))?,
            ls1: vb.pp("ls1").get(DIM, "gamma")?,
            norm2: candle_nn::layer_norm(DIM, 1e-6, vb.pp("norm2"))?,
            fc1: candle_nn::linear(DIM, 4 * DIM, m.pp("fc1"))?,
            fc2: candle_nn::linear(4 * DIM, DIM, m.pp("fc2"))?,
            ls2: vb.pp("ls2").get(DIM, "gamma")?,
        })
    }

    /// `x`: `[N, DIM]` tokens.
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let n = x.dim(0)?;
        let hd = DIM / HEADS;
        let qkv = self
            .qkv
            .forward(&self.norm1.forward(x)?)?
            .reshape((n, 3, HEADS, hd))?
            .permute((1, 2, 0, 3))?;
        let q = (qkv.i(0)?.contiguous()? * (1.0 / (hd as f64).sqrt()))?;
        let k = qkv.i(1)?.contiguous()?;
        let v = qkv.i(2)?.contiguous()?;
        let attn = candle_nn::ops::softmax_last_dim(&q.matmul(&k.t()?)?)?;
        let o = attn
            .matmul(&v)?
            .transpose(0, 1)?
            .contiguous()?
            .reshape((n, DIM))?;
        let x = (x + self.proj.forward(&o)?.broadcast_mul(&self.ls1)?)?;
        let m = self
            .fc2
            .forward(&self.fc1.forward(&self.norm2.forward(&x)?)?.gelu_erf()?)?;
        x + m.broadcast_mul(&self.ls2)?
    }
}

/// The head's residual unit: ReLU, 3×3, ReLU, 3×3, plus what came in.
struct Residual {
    conv1: Conv2d,
    conv2: Conv2d,
}

impl Residual {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            conv1: conv(vb.pp("conv1"), FEATURES, FEATURES, 3, 1)?,
            conv2: conv(vb.pp("conv2"), FEATURES, FEATURES, 3, 1)?,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let y = self.conv1.forward(&x.relu()?)?;
        let y = self.conv2.forward(&y.relu()?)?;
        y + x
    }
}

/// A fusion block: the coarser path plus this level's (through a residual
/// unit), another residual unit, a bilinear resize (corners aligned) and a
/// 1×1 convolution.
struct Fusion {
    res1: Residual,
    res2: Residual,
    out: Conv2d,
}

impl Fusion {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            res1: Residual::new(vb.pp("resConfUnit1"))?,
            res2: Residual::new(vb.pp("resConfUnit2"))?,
            out: conv(vb.pp("out_conv"), FEATURES, FEATURES, 1, 1)?,
        })
    }

    fn forward(
        &self,
        x: &Tensor,
        skip: Option<&Tensor>,
        size: (usize, usize),
    ) -> candle_core::Result<Tensor> {
        let x = match skip {
            Some(s) => (x + self.res1.forward(s)?)?,
            None => x.clone(),
        };
        let x = self.res2.forward(&x)?;
        let x = x.upsample_bilinear2d(size.0, size.1, true)?;
        self.out.forward(&x)
    }
}

/// The depth model, loaded once.
pub struct DepthModel {
    device: Device,
    patch: Conv2d,
    cls: Tensor,
    /// The checkpoint's position embedding, `[1 + 37², DIM]`, on the CPU
    /// (interpolated for each picture shape and kept).
    pos: Vec<f32>,
    pos_for: Mutex<HashMap<(usize, usize), Tensor>>,
    blocks: Vec<Block>,
    norm: LayerNorm,
    projects: Vec<Conv2d>,
    up4: ConvTranspose2d,
    up2: ConvTranspose2d,
    down2: Conv2d,
    rn: Vec<Conv2d>,
    fusion: Vec<Fusion>,
    out1: Conv2d,
    out2: Conv2d,
    out3: Conv2d,
}

impl DepthModel {
    /// Load the authors' checkpoint.
    pub fn load(path: &Path, device: &Device) -> Result<Self> {
        Self::build(path, device).map_err(err)
    }

    fn build(path: &Path, device: &Device) -> candle_core::Result<Self> {
        let vb = VarBuilder::from_pth(path, DType::F32, device)?;
        let p = vb.pp("pretrained");
        let patch = candle_nn::conv2d(
            3,
            DIM,
            PATCH,
            Conv2dConfig {
                stride: PATCH,
                ..Default::default()
            },
            p.pp("patch_embed").pp("proj"),
        )?;
        let cls = p.get((1, 1, DIM), "cls_token")?.reshape((1, DIM))?;
        let pos = p
            .get((1, 1 + GRID * GRID, DIM), "pos_embed")?
            .flatten_all()?
            .to_device(&Device::Cpu)?
            .to_vec1::<f32>()?;
        let blocks = (0..DEPTH)
            .map(|i| Block::new(p.pp("blocks").pp(i.to_string())))
            .collect::<candle_core::Result<Vec<_>>>()?;
        let norm = candle_nn::layer_norm(DIM, 1e-6, p.pp("norm"))?;
        let h = vb.pp("depth_head");
        let projects = (0..4)
            .map(|i| conv(h.pp("projects").pp(i.to_string()), DIM, OUT[i], 1, 1))
            .collect::<candle_core::Result<Vec<_>>>()?;
        let rl = h.pp("resize_layers");
        let tcfg = |k: usize| candle_nn::ConvTranspose2dConfig {
            padding: 0,
            output_padding: 0,
            stride: k,
            dilation: 1,
        };
        let up4 = candle_nn::conv_transpose2d(OUT[0], OUT[0], 4, tcfg(4), rl.pp("0"))?;
        let up2 = candle_nn::conv_transpose2d(OUT[1], OUT[1], 2, tcfg(2), rl.pp("1"))?;
        let down2 = conv(rl.pp("3"), OUT[3], OUT[3], 3, 2)?;
        let s = h.pp("scratch");
        let rn = (0..4)
            .map(|i| conv_no_bias(s.pp(format!("layer{}_rn", i + 1)), OUT[i], FEATURES))
            .collect::<candle_core::Result<Vec<_>>>()?;
        let fusion = (1..=4)
            .map(|i| Fusion::new(s.pp(format!("refinenet{i}"))))
            .collect::<candle_core::Result<Vec<_>>>()?;
        let out1 = conv(s.pp("output_conv1"), FEATURES, FEATURES / 2, 3, 1)?;
        let o2 = s.pp("output_conv2");
        let out2 = conv(o2.pp("0"), FEATURES / 2, 32, 3, 1)?;
        let out3 = conv(o2.pp("2"), 32, 1, 1, 1)?;
        Ok(Self {
            device: device.clone(),
            patch,
            cls,
            pos,
            pos_for: Mutex::new(HashMap::new()),
            blocks,
            norm,
            projects,
            up4,
            up2,
            down2,
            rn,
            fusion,
            out1,
            out2,
            out3,
        })
    }

    /// The position embedding for `ph`×`pw` patches: the checkpoint's 37×37
    /// grid interpolated as DINOv2 does (bicubic, its scale factor with
    /// the 0.1 it adds), the class token's first. `[1 + ph·pw, DIM]`.
    fn position(&self, ph: usize, pw: usize) -> candle_core::Result<Tensor> {
        let mut kept = self.pos_for.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = kept.get(&(ph, pw)) {
            return Ok(t.clone());
        }
        let mut out = self.pos[..DIM].to_vec();
        if (ph, pw) == (GRID, GRID) {
            out.extend_from_slice(&self.pos[DIM..]);
        } else {
            // `[37, 37, DIM]` → channels first, interpolated, back.
            let grid = &self.pos[DIM..];
            let mut planes = vec![0f32; DIM * GRID * GRID];
            for (i, p) in grid.as_chunks::<DIM>().0.iter().enumerate() {
                for (c, v) in p.iter().enumerate() {
                    planes[c * GRID * GRID + i] = *v;
                }
            }
            let sy = (ph as f64 + 0.1) / GRID as f64;
            let sx = (pw as f64 + 0.1) / GRID as f64;
            let up = bicubic_scaled(&planes, DIM, GRID, GRID, (ph, sy), (pw, sx));
            let n = ph * pw;
            for i in 0..n {
                for c in 0..DIM {
                    out.push(up[c * n + i]);
                }
            }
        }
        let t = Tensor::from_vec(out, (1 + ph * pw, DIM), &self.device)?;
        kept.insert((ph, pw), t.clone());
        Ok(t)
    }

    /// Relative inverse depth of a picture (8-bit sRGB, `width`×`height`,
    /// three bytes a pixel), on the grid the network saw (see
    /// [`input_size`]); larger is nearer.
    pub fn estimate(&self, rgb: &[u8], width: usize, height: usize) -> Result<Depth> {
        if width == 0 || height == 0 || rgb.len() < width * height * 3 {
            return Err(MlError::Format("no image".into()));
        }
        self.estimate_inner(rgb, width, height).map_err(err)
    }

    fn estimate_inner(&self, rgb: &[u8], w: usize, h: usize) -> candle_core::Result<Depth> {
        let (iw, ih) = input_size(w, h);
        let (pw, ph) = (iw / PATCH, ih / PATCH);
        let input = Tensor::from_vec(prepare(rgb, w, h, iw, ih), (1, 3, ih, iw), &self.device)?;
        // DINOv2: patches, the class token, positions, blocks.
        let x = self.patch.forward(&input)?.reshape((DIM, ph * pw))?.t()?;
        let mut x = (Tensor::cat(&[&self.cls, &x], 0)? + self.position(ph, pw)?)?;
        let mut taken = Vec::with_capacity(4);
        for (i, b) in self.blocks.iter().enumerate() {
            x = b.forward(&x)?;
            if TAKE.contains(&i) {
                // Normed, without the class token, as a picture.
                let t = self.norm.forward(&x)?.i(1..)?;
                taken.push(t.t()?.contiguous()?.reshape((1, DIM, ph, pw))?);
            }
        }
        // The DPT head: four levels at 4×, 2×, 1× and ½× the patch grid.
        let mut levels = Vec::with_capacity(4);
        for (i, t) in taken.iter().enumerate() {
            let p = self.projects[i].forward(t)?;
            let p = match i {
                0 => self.up4.forward(&p)?,
                1 => self.up2.forward(&p)?,
                2 => p,
                _ => self.down2.forward(&p)?,
            };
            levels.push(self.rn[i].forward(&p)?);
        }
        let size = |t: &Tensor| -> candle_core::Result<(usize, usize)> {
            let (_, _, h, w) = t.dims4()?;
            Ok((h, w))
        };
        let path = self.fusion[3].forward(&levels[3], None, size(&levels[2])?)?;
        let path = self.fusion[2].forward(&path, Some(&levels[2]), size(&levels[1])?)?;
        let path = self.fusion[1].forward(&path, Some(&levels[1]), size(&levels[0])?)?;
        let (h0, w0) = size(&levels[0])?;
        let path = self.fusion[0].forward(&path, Some(&levels[0]), (2 * h0, 2 * w0))?;
        let out = self.out1.forward(&path)?;
        let out = out.upsample_bilinear2d(ih, iw, true)?;
        let out = self.out2.forward(&out)?.relu()?;
        let out = self.out3.forward(&out)?.relu()?;
        let data = out
            .flatten_all()?
            .to_device(&Device::Cpu)?
            .to_vec1::<f32>()?;
        Ok(Depth {
            width: iw,
            height: ih,
            data,
        })
    }
}

/// The size the network sees for a `w`×`h` picture, as the authors'
/// `Resize` (aspect kept, "lower bound", multiples of 14): the short side
/// [`SHORT`] and the long one in proportion (rounded to 14, at most
/// [`LONG`]).
pub fn input_size(w: usize, h: usize) -> (usize, usize) {
    let k = SHORT as f64 / w.min(h).max(1) as f64;
    let side = |v: usize| {
        let x = v as f64 * k;
        let mut y = (x / PATCH as f64).round() as usize * PATCH;
        if y < SHORT {
            y = (x / PATCH as f64).ceil() as usize * PATCH;
        }
        y.clamp(SHORT, LONG)
    };
    (side(w), side(h))
}

/// The picture resized to `ow`×`oh` (area-averaged when shrinking,
/// bilinear when growing), normalised, channels first. (The authors resize
/// with OpenCV's bicubic; averaging doesn't alias when shrinking a large
/// picture.)
fn prepare(rgb: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<f32> {
    let mut out = vec![0f32; 3 * ow * oh];
    let (kx, ky) = (w as f32 / ow as f32, h as f32 / oh as f32);
    let at = |x: usize, y: usize, c: usize| f32::from(rgb[(y * w + x) * 3 + c]) / 255.0;
    let span = |o: usize, k: f32, n: usize| {
        let a = ((o as f32 * k) as usize).min(n - 1);
        let b = (((o + 1) as f32 * k).ceil() as usize).clamp(a + 1, n);
        a..b
    };
    let plane = ow * oh;
    for oy in 0..oh {
        for ox in 0..ow {
            let mut acc = [0f32; 3];
            if kx > 1.0 || ky > 1.0 {
                let (xs, ys) = (span(ox, kx, w), span(oy, ky, h));
                let n = (xs.len() * ys.len()) as f32;
                for y in ys {
                    for x in xs.clone() {
                        for (c, a) in acc.iter_mut().enumerate() {
                            *a += at(x, y, c) / n;
                        }
                    }
                }
            } else {
                let fx = ((ox as f32 + 0.5) * kx - 0.5).clamp(0.0, (w - 1) as f32);
                let fy = ((oy as f32 + 0.5) * ky - 0.5).clamp(0.0, (h - 1) as f32);
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
                let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
                for (c, a) in acc.iter_mut().enumerate() {
                    let top = at(x0, y0, c) + (at(x1, y0, c) - at(x0, y0, c)) * tx;
                    let bot = at(x0, y1, c) + (at(x1, y1, c) - at(x0, y1, c)) * tx;
                    *a = top + (bot - top) * ty;
                }
            }
            for (c, a) in acc.iter().enumerate() {
                out[c * plane + oy * ow + ox] = (a - MEAN[c]) / STD[c];
            }
        }
    }
    out
}

/// PyTorch's bicubic weights (A = −0.75) for a fraction `t`, for the
/// samples at −1, 0, 1 and 2.
fn cubic(t: f64) -> [f64; 4] {
    let a = -0.75;
    let near = |x: f64| ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0;
    let far = |x: f64| ((a * x - 5.0 * a) * x + 8.0 * a) * x - 4.0 * a;
    [far(t + 1.0), near(t), near(1.0 - t), far(2.0 - t)]
}

/// `F.interpolate(mode="bicubic", scale_factor=…, align_corners=False)` of
/// `[C, h, w]` to `oh`×`ow`, sampling at `(o + 0.5) / scale − 0.5` with the
/// scale given (as PyTorch does when it's passed one).
fn bicubic_scaled(
    src: &[f32],
    c: usize,
    h: usize,
    w: usize,
    (oh, sy): (usize, f64),
    (ow, sx): (usize, f64),
) -> Vec<f32> {
    let taps = |n: usize, on: usize, scale: f64| -> Vec<([usize; 4], [f64; 4])> {
        (0..on)
            .map(|i| {
                let s = (i as f64 + 0.5) / scale - 0.5;
                let f = s.floor();
                let idx = [-1i64, 0, 1, 2].map(|d| (f as i64 + d).clamp(0, n as i64 - 1) as usize);
                (idx, cubic(s - f))
            })
            .collect()
    };
    let (ys, xs) = (taps(h, oh, sy), taps(w, ow, sx));
    let mut out = vec![0f32; c * oh * ow];
    for ch in 0..c {
        let plane = &src[ch * h * w..(ch + 1) * h * w];
        for (oy, (yi, yw)) in ys.iter().enumerate() {
            for (ox, (xi, xw)) in xs.iter().enumerate() {
                let mut v = 0.0;
                for a in 0..4 {
                    let mut row = 0.0;
                    for b in 0..4 {
                        row += f64::from(plane[yi[a] * w + xi[b]]) * xw[b];
                    }
                    v += row * yw[a];
                }
                out[ch * oh * ow + oy * ow + ox] = v as f32;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_follow_the_authors_resize() {
        // A 3:2 landscape: 518 tall, 777 → 784 wide (rounded to 14).
        assert_eq!(input_size(6000, 4000), (784, 518));
        assert_eq!(input_size(4000, 6000), (518, 784));
        assert_eq!(input_size(518, 518), (518, 518));
        // Never below the short side, never past the long limit.
        let (w, h) = input_size(20000, 2000);
        assert_eq!((w % 14, h % 14), (0, 0));
        assert_eq!((w, h), (LONG, SHORT));
    }

    #[test]
    fn bicubic_keeps_constants_and_the_grid() {
        let src: Vec<f32> = (0..2 * 5 * 5)
            .map(|i| (i / 25) as f32 * 3.0 + 1.0)
            .collect();
        let up = bicubic_scaled(&src, 2, 5, 5, (9, 9.1 / 5.0), (7, 7.1 / 5.0));
        assert!(up[..63].iter().all(|v| (v - 1.0).abs() < 1e-5));
        assert!(up[63..].iter().all(|v| (v - 4.0).abs() < 1e-5));
        // At scale 1 it's the identity.
        let ramp: Vec<f32> = (0..16).map(|i| i as f32).collect();
        let same = bicubic_scaled(&ramp, 1, 4, 4, (4, 1.0), (4, 1.0));
        for (a, b) in ramp.iter().zip(&same) {
            assert!((a - b).abs() < 1e-5);
        }
    }
}
