//! Where the app keeps its own state, following Apple's layout.

use std::path::{Path, PathBuf};

use crate::BUNDLE_ID;
use crate::error::{CoreError, Result};

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| CoreError::Invalid("HOME is not set".into()))
}

/// `~/Library/Application Support/<bundle id>`: cache database and journal.
pub fn default_data_dir() -> Result<PathBuf> {
    Ok(home()?.join("Library/Application Support").join(BUNDLE_ID))
}

/// The rebuildable cache database.
pub fn cache_db(data_dir: &Path) -> PathBuf {
    data_dir.join("cache.sqlite")
}

/// The journal lives in its own database so deleting the cache never loses
/// undo history.
pub fn journal_db(data_dir: &Path) -> PathBuf {
    data_dir.join("journal.sqlite")
}

/// True if `path` is `root` or inside it. Both must already be canonical.
pub fn is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}
