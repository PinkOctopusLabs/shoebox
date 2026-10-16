//! Where the app keeps its own state, following Apple's layout.

use std::path::{Path, PathBuf};

use crate::BUNDLE_ID;
use crate::error::{CoreError, Result};

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| CoreError::Invalid("HOME is not set".into()))
}

/// Where the cache database and journal live:
/// `~/Library/Application Support/<bundle id>` on macOS, and
/// `$XDG_DATA_HOME/shoebox` (default `~/.local/share/shoebox`) elsewhere.
pub fn default_data_dir() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Ok(home()?.join("Library/Application Support").join(BUNDLE_ID))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let base = match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => home()?.join(".local/share"),
        };
        let _ = BUNDLE_ID;
        Ok(base.join("shoebox"))
    }
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
