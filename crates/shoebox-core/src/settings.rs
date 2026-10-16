//! User settings, stored as JSON in the data directory. Unlike the cache,
//! this is user state (which folders make up the library), so it's never
//! rebuilt or deleted by the app.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result, io};
use crate::scan::{Role, Root};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootSetting {
    pub path: PathBuf,
    /// Read-only, never moved or written to.
    #[serde(default)]
    pub reference: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub roots: Vec<RootSetting>,
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| CoreError::Invalid(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io(path)(e)),
        }
    }

    /// Atomic write: temporary file in the same folder, then rename over.
    /// (The data directory is the app's own; this never touches the library.)
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(io(dir))?;
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| CoreError::Invalid(format!("settings: {e}")))?;
        std::fs::write(&tmp, json).map_err(io(&tmp))?;
        std::fs::rename(&tmp, path).map_err(io(path))?;
        Ok(())
    }

    /// Add a root (canonicalised). Refuses duplicates and nested roots.
    pub fn add_root(&mut self, path: &Path) -> Result<PathBuf> {
        let p = path.canonicalize().map_err(io(path))?;
        if !p.is_dir() {
            return Err(CoreError::Invalid(format!(
                "{} is not a folder",
                p.display()
            )));
        }
        if let Some(r) = self
            .roots
            .iter()
            .find(|r| p.starts_with(&r.path) || r.path.starts_with(&p))
        {
            return Err(CoreError::Invalid(format!(
                "{} overlaps library folder {}",
                p.display(),
                r.path.display()
            )));
        }
        self.roots.push(RootSetting {
            path: p.clone(),
            reference: false,
        });
        Ok(p)
    }

    /// Remove a root from the library. Files on disk are untouched.
    pub fn remove_root(&mut self, path: &Path) {
        self.roots.retain(|r| r.path != path);
    }

    pub fn set_reference(&mut self, path: &Path, reference: bool) {
        for r in self.roots.iter_mut().filter(|r| r.path == path) {
            r.reference = reference;
        }
    }

    /// Roots that still exist on disk, as scan roots.
    pub fn scan_roots(&self) -> Vec<Root> {
        self.roots
            .iter()
            .filter_map(|r| {
                let role = if r.reference {
                    Role::Reference
                } else {
                    Role::Scan
                };
                Root::new(&r.path, role).ok()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rules() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("a/b"));
        std::fs::create_dir_all(&b).unwrap();
        let mut s = Settings::default();
        let ca = s.add_root(&a).unwrap();
        assert!(s.add_root(&b).is_err(), "nested roots refused");
        assert!(s.add_root(&a).is_err(), "duplicates refused");
        s.set_reference(&ca, true);
        let f = t.path().join("data/settings.json");
        s.save(&f).unwrap();
        assert_eq!(Settings::load(&f).unwrap(), s);
        assert_eq!(s.scan_roots()[0].role, Role::Reference);
        s.remove_root(&ca);
        assert!(s.roots.is_empty());
        assert!(a.exists(), "removing a root leaves files alone");
        assert_eq!(
            Settings::load(&t.path().join("missing.json")).unwrap(),
            Settings::default()
        );
    }
}
