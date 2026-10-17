//! The SigLIP 2 image and text towers, loaded separately.

use std::path::Path;

use candle_core::{DType, Device, Module, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::siglip;
use shoebox_decode::Rgb8;

use crate::convert::{TEXT_FILE, VISION_FILE};
use crate::{DIM, IMAGE_SIZE, MlError, Result};

/// The text tower reads at most this many tokens (padded to exactly this).
const MAX_TOKENS: usize = 64;

fn config() -> siglip::Config {
    let mut c = siglip::Config::base_patch16_224();
    // SigLIP 2 uses Gemma's multilingual vocabulary.
    c.text_config.vocab_size = 256_000;
    c.text_config.pad_token_id = 0;
    c.text_config.eos_token_id = 1;
    c
}

/// Half precision on the GPU; the CPU is faster in single precision.
fn dtype(device: &Device) -> DType {
    if device.is_cpu() {
        DType::F32
    } else {
        DType::F16
    }
}

fn weights<'a>(path: &Path, device: &'a Device) -> Result<VarBuilder<'a>> {
    if !path.exists() {
        return Err(MlError::Format(format!("{} is missing", path.display())));
    }
    let tensors = candle_core::safetensors::load(path, &Device::Cpu)?;
    Ok(VarBuilder::from_tensors(tensors, dtype(device), device))
}

/// Unit-length rows of a (batch, DIM) tensor.
fn rows(t: &Tensor) -> Result<Vec<Vec<f32>>> {
    let mut out = t.to_dtype(DType::F32)?.to_vec2::<f32>()?;
    for v in &mut out {
        if v.len() != DIM {
            return Err(MlError::Format(format!("embedding has {} values", v.len())));
        }
        crate::normalize(v);
    }
    Ok(out)
}

/// The model's input for one image: squashed to 224 × 224 (as SigLIP's own
/// preprocessing does, without cropping), as CHW floats in [-1, 1].
pub fn preprocess(img: &Rgb8) -> Vec<f32> {
    let s = IMAGE_SIZE as usize;
    let small = if (img.width, img.height) == (IMAGE_SIZE, IMAGE_SIZE) {
        img.clone()
    } else {
        img.resize(IMAGE_SIZE, IMAGE_SIZE)
    };
    let mut out = vec![0f32; 3 * s * s];
    for (i, px) in small.px.as_chunks::<3>().0.iter().enumerate() {
        for c in 0..3 {
            out[c * s * s + i] = f32::from(px[c]) / 127.5 - 1.0;
        }
    }
    out
}

pub struct ImageEncoder {
    model: siglip::VisionModel,
    device: Device,
}

impl ImageEncoder {
    /// Load the image tower from a model folder (see [`crate::download`]).
    pub fn load(dir: &Path, device: &Device) -> Result<Self> {
        let vb = weights(&dir.join(VISION_FILE), device)?;
        let model = siglip::VisionModel::new(&config().vision_config, true, vb.pp("vision_model"))?;
        Ok(Self {
            model,
            device: device.clone(),
        })
    }

    /// Embeddings (unit length) for preprocessed images ([`preprocess`]).
    pub fn embed(&self, images: &[Vec<f32>]) -> Result<Vec<Vec<f32>>> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let s = IMAGE_SIZE as usize;
        let flat: Vec<f32> = images.iter().flatten().copied().collect();
        if flat.len() != images.len() * 3 * s * s {
            return Err(MlError::Format(
                "preprocessed image has the wrong size".into(),
            ));
        }
        let x = Tensor::from_vec(flat, (images.len(), 3, s, s), &self.device)?
            .to_dtype(dtype(&self.device))?;
        rows(&self.model.forward(&x)?)
    }
}

pub struct TextEncoder {
    model: siglip::TextModel,
    tokenizer: tokenizers::Tokenizer,
    device: Device,
    /// SigLIP's learned sigmoid scale and bias: P(match) = σ(scale·cos + bias).
    scale: f32,
    bias: f32,
}

impl TextEncoder {
    /// Load the text tower and tokenizer from a model folder.
    pub fn load(dir: &Path, device: &Device) -> Result<Self> {
        let path = dir.join(TEXT_FILE);
        if !path.exists() {
            return Err(MlError::Format(format!("{} is missing", path.display())));
        }
        let tensors = candle_core::safetensors::load(&path, &Device::Cpu)?;
        let scalar = |name: &str| -> Result<f32> {
            let t = tensors
                .get(name)
                .ok_or_else(|| MlError::Format(format!("{name} is missing")))?;
            Ok(t.to_dtype(DType::F32)?.flatten_all()?.to_vec1::<f32>()?[..]
                .first()
                .copied()
                .unwrap_or(0.0))
        };
        let (scale, bias) = (scalar("logit_scale")?.exp(), scalar("logit_bias")?);
        let vb = VarBuilder::from_tensors(tensors, dtype(device), device);
        let model = siglip::TextModel::new(&config().text_config, vb.pp("text_model"))?;
        let tok_path = dir.join(crate::download::TOKENIZER_FILE);
        let mut tokenizer = tokenizers::Tokenizer::from_file(&tok_path)
            .map_err(|e| MlError::Tokenizer(format!("{}: {e}", tok_path.display())))?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| MlError::Tokenizer(e.to_string()))?;
        tokenizer.with_padding(Some(tokenizers::PaddingParams {
            strategy: tokenizers::PaddingStrategy::Fixed(MAX_TOKENS),
            pad_id: 0,
            pad_token: "<pad>".into(),
            ..Default::default()
        }));
        Ok(Self {
            model,
            tokenizer,
            device: device.clone(),
            scale,
            bias,
        })
    }

    /// Token ids for a query: lower-cased (as the model was trained), ending
    /// in `<eos>`, padded to 64.
    pub fn tokens(&self, text: &str) -> Result<Vec<u32>> {
        let enc = self
            .tokenizer
            .encode(text.trim().to_lowercase(), true)
            .map_err(|e| MlError::Tokenizer(e.to_string()))?;
        Ok(enc.get_ids().to_vec())
    }

    /// Embedding (unit length) of a text query.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let ids = self.tokens(text)?;
        let n = ids.len();
        let x = Tensor::from_vec(ids, (1, n), &self.device)?;
        rows(&self.model.forward(&x)?)?
            .pop()
            .ok_or_else(|| MlError::Format("no embedding".into()))
    }

    /// The model's probability that an image matches the text, from the
    /// cosine similarity of their embeddings.
    pub fn probability(&self, cosine: f32) -> f32 {
        1.0 / (1.0 + (-(self.scale * cosine + self.bias)).exp())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_layout_and_range() {
        // Left half black, right half white, 448 × 100: squashed, not cropped.
        let (w, h) = (448u32, 100u32);
        let px = (0..w * h)
            .flat_map(|i| {
                if i % w < w / 2 {
                    [0, 0, 0]
                } else {
                    [255, 255, 255]
                }
            })
            .collect();
        let v = preprocess(&Rgb8::new(w, h, px).unwrap());
        let s = IMAGE_SIZE as usize;
        assert_eq!(v.len(), 3 * s * s);
        for c in 0..3 {
            let row = &v[c * s * s..c * s * s + s];
            assert!((row[0] + 1.0).abs() < 1e-3, "black is -1");
            assert!((row[s - 1] - 1.0).abs() < 1e-3, "white is +1");
        }
    }
}
