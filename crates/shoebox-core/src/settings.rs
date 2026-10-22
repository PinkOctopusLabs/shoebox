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
    /// iCloud Photos sync choices (PHOTOS-SPEC).
    #[serde(default)]
    pub photos: crate::photos::plan::Options,
    /// Marks set on a stack go to every member's sidecar, not just the
    /// primary's (so tools looking at the JPEG or TIFF see them too).
    #[serde(default)]
    pub marks_to_all_stack_members: bool,
    /// Also write Lightroom-style `name.xmp` for RAW files.
    #[serde(default)]
    pub write_stem_sidecar_for_raw: bool,
    /// Saved filters, shown under Smart filters.
    #[serde(default)]
    pub smart_filters: Vec<crate::query::SmartFilter>,
    /// Import naming template (empty = the default).
    #[serde(default)]
    pub import_template: String,
    /// Batch rename template (empty = the default).
    #[serde(default)]
    pub rename_template: String,
    /// Don't fetch map tiles (the map shows markers on a blank background).
    #[serde(default)]
    pub offline_map: bool,
    /// GPU memory for images, in MB (0 = the default).
    #[serde(default)]
    pub image_memory_mb: u32,
    /// Search photos by what's in them (downloads a model once).
    #[serde(default)]
    pub semantic_search: bool,
    /// Find and group the faces in photos (downloads models once).
    #[serde(default)]
    pub people: bool,
    /// Grid: a heading for each day (when sorted by capture time).
    #[serde(default)]
    pub day_headers: bool,
    /// Masonry: justified rows (equal height, strict date order) instead of
    /// columns.
    #[serde(default)]
    pub justified_rows: bool,
    /// Don't outline faces on photos (People; the view bar's toggle).
    #[serde(default)]
    pub hide_face_boxes: bool,
    /// Things listed most photos first (else A to Z).
    #[serde(default)]
    pub things_by_count: bool,
    /// Adobe DNG Converter, when it isn't in /Applications.
    #[serde(default)]
    pub dng_converter: Option<std::path::PathBuf>,
    /// The DNG cache's limit in GB (0: the default).
    #[serde(default)]
    pub dng_cache_gb: u32,
    /// Convert likely edits (picks, 3 stars and up) to DNGs while Shoebox
    /// is idle.
    #[serde(default)]
    pub preconvert: bool,
    /// Develop: don't apply Auto to a photo the first time it's opened.
    #[serde(default)]
    pub develop_auto_off: bool,
    /// Develop: this preset (by name) instead of Auto for new photos.
    #[serde(default)]
    pub new_photo_preset: Option<String>,
    /// Develop: Auto turns on AI denoise for raw photos of at least this
    /// ISO (None: never).
    #[serde(default)]
    pub ai_denoise_from_iso: Option<u32>,
    /// How big text and controls are (⌘+ / ⌘− / ⌘0).
    #[serde(default)]
    pub text_size: TextSize,
}

/// The app's text size: everything (text, controls, spacing) scales with it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSize {
    Small,
    #[default]
    Medium,
    Large,
    Larger,
}

impl TextSize {
    pub const ALL: [TextSize; 4] = [
        TextSize::Small,
        TextSize::Medium,
        TextSize::Large,
        TextSize::Larger,
    ];

    /// One size up (`up`) or down, stopping at the ends.
    pub fn step(self, up: bool) -> Self {
        let i = Self::ALL.iter().position(|&t| t == self).unwrap_or(1);
        let j = if up {
            (i + 1).min(Self::ALL.len() - 1)
        } else {
            i.saturating_sub(1)
        };
        Self::ALL[j]
    }
}

/// Default for [`Settings::dng_cache_gb`].
pub const DEFAULT_DNG_CACHE_GB: u32 = 20;

/// The ISO [`Settings::ai_denoise_from_iso`] starts at when turned on.
pub const DEFAULT_AI_DENOISE_ISO: u32 = 3200;

/// Default for [`Settings::image_memory_mb`].
pub const DEFAULT_IMAGE_MEMORY_MB: u32 = 768;

impl Settings {
    pub fn import_template(&self) -> &str {
        if self.import_template.trim().is_empty() {
            crate::template::DEFAULT_IMPORT
        } else {
            &self.import_template
        }
    }

    pub fn rename_template(&self) -> &str {
        if self.rename_template.trim().is_empty() {
            crate::template::DEFAULT_RENAME
        } else {
            &self.rename_template
        }
    }

    /// GPU memory for images, in bytes.
    pub fn image_memory_bytes(&self) -> usize {
        let mb = match self.image_memory_mb {
            0 => DEFAULT_IMAGE_MEMORY_MB,
            mb => mb.clamp(128, 16_384),
        };
        (mb as usize) << 20
    }

    /// Paths of the roots Shoebox may change.
    pub fn writable_roots(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .filter(|r| !r.reference)
            .map(|r| r.path.clone())
            .collect()
    }

    pub fn all_roots(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|r| r.path.clone()).collect()
    }
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

    #[test]
    fn text_size_steps_and_reads_old_files() {
        assert_eq!(TextSize::Medium.step(true), TextSize::Large);
        assert_eq!(TextSize::Larger.step(true), TextSize::Larger);
        assert_eq!(TextSize::Small.step(false), TextSize::Small);
        let old: Settings = serde_json::from_str(r#"{"roots": []}"#).unwrap();
        assert_eq!(old.text_size, TextSize::Medium);
    }

    #[test]
    fn ai_denoise_iso_defaults_off_and_round_trips() {
        let old: Settings = serde_json::from_str(r#"{"roots": []}"#).unwrap();
        assert_eq!(old.ai_denoise_from_iso, None);
        assert_eq!(Settings::default().ai_denoise_from_iso, None);
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("settings.json");
        let s = Settings {
            ai_denoise_from_iso: Some(6400),
            ..Settings::default()
        };
        s.save(&f).unwrap();
        assert_eq!(Settings::load(&f).unwrap().ai_denoise_from_iso, Some(6400));
    }
}
