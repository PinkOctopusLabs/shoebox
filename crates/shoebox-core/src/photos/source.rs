//! The Photos library behind a trait: PhotoKit in the app
//! (`shoebox-photos`), [`super::fake::FakeSource`] in tests.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::model::{Access, Asset, AssetId, Library, Listing, PushRequest, Resource, ResourceKind};
use crate::error::Result;

pub trait PhotoSource: Send + Sync {
    /// What Shoebox may do now (asks nothing).
    fn access(&self) -> Access;

    /// Ask the person for access if they haven't been asked; the answer.
    fn request_access(&self) -> Access;

    /// Every asset and album (metadata only; downloads nothing, but a large
    /// library takes a while). `progress` hears how far it's got; setting
    /// `stop` ends it with [`crate::CoreError::Cancelled`].
    fn fetch_with(&self, progress: &(dyn Fn(Listing) + Sync), stop: &AtomicBool)
    -> Result<Library>;

    /// [`PhotoSource::fetch_with`], unwatched.
    fn fetch(&self) -> Result<Library> {
        self.fetch_with(&|_| {}, &AtomicBool::new(false))
    }

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

    /// An asset's files (for one listed without them).
    fn resources(&self, asset: &AssetId) -> Result<Vec<Resource>> {
        Ok(self
            .lookup(std::slice::from_ref(asset))?
            .into_iter()
            .next()
            .map(|a| a.resources)
            .unwrap_or_default())
    }

    /// These assets as Photos has them now (those still there), with
    /// their files.
    fn lookup(&self, ids: &[AssetId]) -> Result<Vec<Asset>>;

    /// Put a rendered file into Photos: a new asset, or an edit of one
    /// (macOS asks the person to allow changing it). The asset's id.
    fn push(&self, req: &PushRequest) -> Result<AssetId>;

    /// Undo a push: delete an asset Shoebox made (`created`; it goes to
    /// Recently Deleted, macOS asks first), or revert an edited asset to
    /// its original.
    fn undo_push(&self, id: &AssetId, created: bool) -> Result<()>;
}
