//! Photo editing for Shoebox's Develop view (`docs/EDITING-SPEC.md`).
//!
//! - [`dng`]: High efficiency NEFs, which no open decoder reads yet, become
//!   DNGs through Adobe's DNG Converter, cached by content.
//! - [`edit`]: the edit document: settings that differ from the defaults,
//!   with undoable history.

pub mod auto;
pub mod colour;
pub mod cpu;
pub mod dng;
pub mod edit;
pub mod encode;
pub mod export;
pub mod golden;
pub mod gpu;
pub mod lens;
pub mod preset;
pub mod wb;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum DevelopError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Decode(#[from] shoebox_decode::DecodeError),
    #[error("Adobe DNG Converter: {0}")]
    Converter(String),
}

pub type Result<T> = std::result::Result<T, DevelopError>;

pub(crate) fn io(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> DevelopError + '_ {
    move |source| DevelopError::Io {
        path: path.to_path_buf(),
        source,
    }
}
