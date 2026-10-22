//! A Photos library in memory, for tests (core's and the app's UI tests):
//! no PhotoKit, nothing on the Mac's real library.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::model::{Access, Asset, AssetId, Library, ResourceKind};
use super::source::PhotoSource;
use crate::error::{CoreError, Result, io};

#[derive(Default)]
pub struct FakeSource {
    state: Mutex<State>,
    downloads: AtomicUsize,
}

#[derive(Default)]
struct State {
    access: Option<Access>,
    /// Access once asked (default Full).
    answer: Option<Access>,
    library: Library,
    files: HashMap<(String, ResourceKind), Vec<u8>>,
    failing: HashMap<(String, ResourceKind), String>,
}

impl FakeSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an asset with the bytes of each of its resources (in the order
    /// of `asset.resources`; missing ones fail to download).
    pub fn add(&self, asset: Asset, files: Vec<Vec<u8>>) {
        if let Ok(mut s) = self.state.lock() {
            for (r, bytes) in asset.resources.iter().zip(files) {
                s.files.insert((asset.id.local.clone(), r.kind), bytes);
            }
            s.library.assets.push(asset);
        }
    }

    pub fn add_album(&self, album: super::model::Album) {
        if let Ok(mut s) = self.state.lock() {
            s.library.albums.push(album);
        }
    }

    /// Change an asset in place (an edit made in Photos, a favourite).
    pub fn update(
        &self,
        local: &str,
        f: impl FnOnce(&mut Asset, &mut dyn FnMut(ResourceKind, Vec<u8>)),
    ) {
        if let Ok(mut s) = self.state.lock() {
            let State { library, files, .. } = &mut *s;
            if let Some(a) = library.assets.iter_mut().find(|a| a.id.local == local) {
                let key = a.id.local.clone();
                f(a, &mut |kind, bytes| {
                    files.insert((key.clone(), kind), bytes);
                });
            }
        }
    }

    /// Make one file fail to download with `why`.
    pub fn fail(&self, local: &str, kind: ResourceKind, why: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.failing.insert((local.to_owned(), kind), why.to_owned());
        }
    }

    /// Start as already asked, with this answer.
    pub fn set_access(&self, access: Access) {
        if let Ok(mut s) = self.state.lock() {
            s.access = Some(access);
        }
    }

    /// What the person answers when asked.
    pub fn answer(&self, access: Access) {
        if let Ok(mut s) = self.state.lock() {
            s.answer = Some(access);
        }
    }

    /// Files downloaded so far.
    pub fn downloads(&self) -> usize {
        self.downloads.load(Ordering::Relaxed)
    }
}

impl PhotoSource for FakeSource {
    fn access(&self) -> Access {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.access)
            .unwrap_or(Access::NotAsked)
    }

    fn request_access(&self) -> Access {
        let Ok(mut s) = self.state.lock() else {
            return Access::Denied;
        };
        let a = match s.access {
            Some(a) if a != Access::NotAsked => a,
            _ => s.answer.unwrap_or(Access::Full),
        };
        s.access = Some(a);
        a
    }

    fn fetch(&self) -> Result<Library> {
        let s = self
            .state
            .lock()
            .map_err(|_| CoreError::Photos("fake library poisoned".into()))?;
        match s.access {
            Some(Access::Full | Access::Limited) => Ok(s.library.clone()),
            _ => Err(CoreError::Photos("no access".into())),
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
        if stop.load(Ordering::Relaxed) {
            return Err(CoreError::Cancelled);
        }
        let bytes = {
            let s = self
                .state
                .lock()
                .map_err(|_| CoreError::Photos("fake library poisoned".into()))?;
            let key = (asset.local.clone(), kind);
            if let Some(why) = s.failing.get(&key) {
                return Err(CoreError::Photos(why.clone()));
            }
            s.files
                .get(&key)
                .cloned()
                .ok_or_else(|| CoreError::Photos(format!("{} has no {kind:?} file", asset.local)))?
        };
        if to.exists() {
            return Err(CoreError::Refused(format!(
                "{} already exists",
                to.display()
            )));
        }
        std::fs::write(to, bytes).map_err(io(to))?;
        progress(1.0);
        self.downloads.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
