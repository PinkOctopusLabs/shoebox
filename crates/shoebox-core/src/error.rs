use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: shoebox_decode::DecodeError,
    },
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("report: {0}")]
    Csv(#[from] csv::Error),
    #[error("{0}")]
    Invalid(String),
    /// A safety rule stopped the operation before anything was touched.
    #[error("refused: {0}")]
    Refused(String),
    /// A batch stopped part-way; the journal records exactly what ran.
    #[error("batch {batch} stopped after {done} of {total} operations: {reason}")]
    BatchFailed {
        batch: i64,
        done: usize,
        total: usize,
        reason: String,
    },
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;

/// `map_err` helper that attaches the path to an I/O error.
pub fn io(path: &Path) -> impl FnOnce(std::io::Error) -> CoreError + '_ {
    move |source| CoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}
