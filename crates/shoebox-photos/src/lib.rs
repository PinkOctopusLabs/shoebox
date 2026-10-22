//! iCloud Photos through PhotoKit (`docs/PHOTOS-SPEC.md`): the
//! [`PhotoSource`] the app syncs from. Only translation lives here; what to
//! copy and how is decided in `shoebox_core::photos`, against its fake.
//!
//! PhotoKit only ever opens the Mac's system Photos library, so during
//! development this runs only in a separate macOS user account with a test
//! library (CLAUDE.md, safety rule 9).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use shoebox_core::photos::model::{Burst, EditedBy};
use shoebox_core::photos::{
    Access, Album, Asset, AssetId, Library, Media, PhotoSource, Resource, ResourceKind,
};
use shoebox_core::{CoreError, Result};

pub mod probe;
#[cfg(target_os = "macos")]
mod sys;

/// `PHAssetResourceType`s and what they are to Shoebox.
const KINDS: [(isize, ResourceKind); 6] = [
    (1, ResourceKind::Original),    // photo
    (2, ResourceKind::Original),    // video
    (4, ResourceKind::Alternate),   // alternatePhoto
    (9, ResourceKind::PairedVideo), // pairedVideo
    (5, ResourceKind::Edited),      // fullSizePhoto
    (6, ResourceKind::Edited),      // fullSizeVideo
];

fn kind_of(raw: isize) -> Option<ResourceKind> {
    KINDS.iter().find(|(r, _)| *r == raw).map(|(_, k)| *k)
}

fn timestamp(secs: Option<f64>) -> Option<jiff::Timestamp> {
    let s = secs?;
    let ms = (s * 1000.0).round();
    (ms.is_finite() && ms.abs() < 1e15)
        .then(|| jiff::Timestamp::from_millisecond(ms as i64).ok())
        .flatten()
}

/// The Mac's Photos library.
#[derive(Debug, Default)]
pub struct PhotoKitSource;

impl PhotoKitSource {
    pub fn new() -> Self {
        Self
    }

    /// The titles of the person's albums (reads nothing else), for the
    /// probe's and push's test-library guard.
    pub fn album_titles(&self) -> Vec<String> {
        #[cfg(target_os = "macos")]
        {
            if matches!(self.access(), Access::Full | Access::Limited) {
                return sys::album_titles();
            }
        }
        Vec::new()
    }

    /// Whether this build can ask for Photos access at all: a bundled app
    /// with its usage description (an unbundled binary would be ended by
    /// macOS for asking).
    pub fn can_ask() -> bool {
        #[cfg(target_os = "macos")]
        {
            sys::has_usage_description()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn access_of(s: sys::Status) -> Access {
    match s {
        sys::Status::NotDetermined => Access::NotAsked,
        sys::Status::Authorized => Access::Full,
        sys::Status::Limited => Access::Limited,
        sys::Status::Denied | sys::Status::Restricted => Access::Denied,
    }
}

/// Turn PhotoKit's listing into the core's model.
#[cfg(target_os = "macos")]
fn library(
    raw: Vec<sys::RawAsset>,
    albums: Vec<sys::RawAlbum>,
    cloud: &HashMap<String, String>,
) -> Library {
    let mut in_albums: HashMap<&str, Vec<String>> = HashMap::new();
    for al in &albums {
        for a in &al.assets {
            in_albums.entry(a.as_str()).or_default().push(al.id.clone());
        }
    }
    let assets = raw
        .iter()
        .map(|a| {
            let mut resources: Vec<Resource> = Vec::new();
            for r in &a.resources {
                let Some(kind) = kind_of(r.kind) else {
                    continue;
                };
                if resources.iter().any(|x| x.kind == kind) {
                    continue;
                }
                resources.push(Resource {
                    kind,
                    file_name: r.name.clone(),
                    // PhotoKit has no public file size.
                    size: None,
                });
            }
            Asset {
                id: AssetId {
                    cloud: cloud.get(&a.local).cloned(),
                    local: a.local.clone(),
                },
                media: if a.media == 2 {
                    Media::Video
                } else {
                    Media::Photo
                },
                created: timestamp(a.created),
                modified: timestamp(a.modified),
                location: a.location.filter(|(lat, lon)| {
                    lat.is_finite() && lon.is_finite() && (*lat != 0.0 || *lon != 0.0)
                }),
                favourite: a.favourite,
                hidden: a.hidden,
                burst: a.burst.clone().map(|id| Burst {
                    id,
                    representative: a.represents_burst,
                    picked: a.burst_selection & 2 != 0,
                }),
                width: u32::try_from(a.width).unwrap_or(0),
                height: u32::try_from(a.height).unwrap_or(0),
                resources,
                albums: in_albums.get(a.local.as_str()).cloned().unwrap_or_default(),
                edited: a
                    .has_adjustments
                    .then(|| EditedBy::from_format(a.adjustment_format.as_deref().unwrap_or(""))),
            }
        })
        .collect();
    Library {
        assets,
        albums: albums
            .into_iter()
            .map(|a| Album {
                id: a.id,
                name: a.name,
                folders: a.folders,
            })
            .collect(),
    }
}

impl PhotoSource for PhotoKitSource {
    fn access(&self) -> Access {
        #[cfg(target_os = "macos")]
        {
            access_of(sys::status())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Access::Denied
        }
    }

    fn request_access(&self) -> Access {
        #[cfg(target_os = "macos")]
        {
            let now = self.access();
            if now != Access::NotAsked {
                return now;
            }
            if !Self::can_ask() {
                tracing::warn!(
                    "photos: this build has no NSPhotoLibraryUsageDescription; not asking"
                );
                return Access::Denied;
            }
            access_of(sys::request())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Access::Denied
        }
    }

    fn fetch(&self) -> Result<Library> {
        #[cfg(target_os = "macos")]
        {
            if !matches!(self.access(), Access::Full | Access::Limited) {
                return Err(CoreError::Photos("no access to Photos".into()));
            }
            let raw = sys::assets(&|_, _| {});
            let albums = sys::albums();
            let locals: Vec<String> = raw.iter().map(|a| a.local.clone()).collect();
            let cloud = sys::cloud_ids(&locals);
            Ok(library(raw, albums, &cloud))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(CoreError::Photos("Photos needs macOS".into()))
        }
    }

    fn download(
        &self,
        asset: &AssetId,
        kind: ResourceKind,
        to: &Path,
        progress: &(dyn Fn(f64) + Sync),
        stop: &AtomicBool,
    ) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            // The first PhotoKit type of this kind the asset has.
            let ids = sys_kinds(kind);
            let mut last = None;
            for raw in ids {
                match sys::download(&asset.local, raw, to, progress, stop) {
                    Ok(()) => return Ok(()),
                    Err(None) => return Err(CoreError::Cancelled),
                    Err(Some(e)) => last = Some(e),
                }
            }
            Err(CoreError::Photos(
                last.unwrap_or_else(|| "nothing to download".into()),
            ))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (asset, kind, to, progress, stop);
            Err(CoreError::Photos("Photos needs macOS".into()))
        }
    }
}

/// PhotoKit's types for one of ours, most likely first.
#[cfg(target_os = "macos")]
fn sys_kinds(kind: ResourceKind) -> Vec<isize> {
    KINDS
        .iter()
        .filter(|(_, k)| *k == kind)
        .map(|(r, _)| *r)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_dates() {
        assert_eq!(kind_of(1), Some(ResourceKind::Original));
        assert_eq!(kind_of(9), Some(ResourceKind::PairedVideo));
        assert_eq!(kind_of(7), None, "adjustment data isn't copied");
        let t = timestamp(Some(1_717_420_502.12)).unwrap();
        assert_eq!(t.to_string(), "2024-06-03T13:15:02.12Z");
        assert!(timestamp(Some(f64::NAN)).is_none());
    }

    /// Never asks during tests: `cargo test` has no usage description.
    #[test]
    fn an_unbundled_binary_never_asks() {
        assert!(!PhotoKitSource::can_ask());
    }
}
