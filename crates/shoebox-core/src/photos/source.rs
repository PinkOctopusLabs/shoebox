//! The Photos library behind a trait: PhotoKit in the app
//! (`shoebox-photos`), [`super::fake::FakeSource`] in tests.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::model::{Access, AssetId, Library, ResourceKind};
use crate::error::Result;

pub trait PhotoSource: Send + Sync {
    /// What Shoebox may do now (asks nothing).
    fn access(&self) -> Access;

    /// Ask the person for access if they haven't been asked; the answer.
    fn request_access(&self) -> Access;

    /// Every asset and album (metadata only: quick, downloads nothing).
    fn fetch(&self) -> Result<Library>;

    /// Write one of an asset's files to `to` (a new file in the cache),
    /// downloading it from iCloud if it isn't on this Mac. `progress` gets
    /// the fraction done (0–1); setting `stop` ends it early with
    /// [`crate::CoreError::Cancelled`] and leaves no file.
    fn download(
        &self,
        asset: &AssetId,
        kind: ResourceKind,
        to: &Path,
        progress: &(dyn Fn(f64) + Sync),
        stop: &AtomicBool,
    ) -> Result<()>;
}
