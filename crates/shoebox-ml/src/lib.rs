//! Image and text embeddings for semantic search: Google's SigLIP 2 (base,
//! patch 16, 224 px; Apache 2.0) run with candle, on the GPU through Metal on
//! macOS and on the CPU elsewhere.
//!
//! The model isn't shipped with the app. [`download`] fetches a pinned
//! revision from Hugging Face, checks it against SHA-256 digests, and
//! [`convert`] splits it into half-precision vision and text halves, so
//! indexing only loads the image encoder and searching only the text one.
//! Nothing here reads or writes the photo library: callers decode images and
//! hand over pixels.

pub mod convert;
pub mod download;
mod encoder;

use std::path::PathBuf;

pub use candle_core::Device;
pub use encoder::{ImageEncoder, TextEncoder, preprocess};

/// Embedding length.
pub const DIM: usize = 768;
/// Side of the square image the model sees.
pub const IMAGE_SIZE: u32 = 224;

/// Identifies the embeddings this crate produces: stored vectors from a
/// different model (or preprocessing) are recomputed, never compared.
pub const MODEL_ID: &str = "siglip2-base-patch16-224/1";

#[derive(Debug, thiserror::Error)]
pub enum MlError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("download: {0}")]
    Http(String),
    #[error("{file} failed its checksum (the download was damaged or changed)")]
    Checksum { file: String },
    #[error("cancelled")]
    Cancelled,
    #[error("model: {0}")]
    Model(#[from] candle_core::Error),
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    #[error("{0}")]
    Format(String),
}

pub type Result<T, E = MlError> = std::result::Result<T, E>;

pub(crate) fn io(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> MlError + '_ {
    move |source| MlError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The compute device: Metal on macOS when available, else the CPU.
/// `SHOEBOX_ML_CPU=1` forces the CPU.
pub fn device() -> Device {
    #[cfg(target_os = "macos")]
    if std::env::var_os("SHOEBOX_ML_CPU").is_none_or(|v| v.is_empty() || v == "0") {
        match candle_core::Device::new_metal(0) {
            Ok(d) => return d,
            Err(e) => tracing::warn!("Metal unavailable, using the CPU: {e}"),
        }
    }
    candle_core::Device::Cpu
}

/// L2-normalise in place (a zero vector stays zero).
pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}
