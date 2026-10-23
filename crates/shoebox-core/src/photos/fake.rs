//! A Photos library in memory, for tests (core's and the app's UI tests):
//! no PhotoKit, nothing on the Mac's real library.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::model::{
    Access, Asset, AssetId, EditedBy, Library, Listing, Media, PushRequest, Resource, ResourceKind,
};
use super::source::PhotoSource;
use crate::error::{CoreError, Result, io};

#[derive(Default)]
pub struct FakeSource {
    /// List assets without their files, as PhotoKit's source does.
    lazy: AtomicBool,
    state: Mutex<State>,
    downloads: AtomicUsize,
    pushes: AtomicUsize,
}

#[derive(Default)]
struct State {
    access: Option<Access>,
    /// Access once asked (default Full).
    answer: Option<Access>,
    library: Library,
    files: HashMap<(String, ResourceKind), Vec<u8>>,
    failing: HashMap<(String, ResourceKind), String>,
    /// Assets whose download panics (as a binding did on a nil name).
    panicking: Vec<String>,
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

    /// Rename an album (its folders stay).
    pub fn rename_album(&self, id: &str, name: &str) {
        if let Ok(mut s) = self.state.lock()
            && let Some(a) = s.library.albums.iter_mut().find(|a| a.id == id)
        {
            a.name = name.to_owned();
        }
    }

    /// Delete an album (its photos stay in Photos).
    pub fn remove_album(&self, id: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.library.albums.retain(|a| a.id != id);
            for a in &mut s.library.assets {
                a.albums.retain(|x| x != id);
            }
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

    /// Make an asset's download panic.
    pub fn panic_on(&self, local: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.panicking.push(local.to_owned());
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

    /// List assets without their files (they're looked up at download), as
    /// PhotoKit's source does.
    pub fn list_without_files(&self) {
        self.lazy.store(true, Ordering::Relaxed);
    }

    /// Pushes so far (new assets and edits).
    pub fn pushes(&self) -> usize {
        self.pushes.load(Ordering::Relaxed)
    }

    /// The asset with this local id, as it is now.
    pub fn asset(&self, local: &str) -> Option<Asset> {
        let s = self.state.lock().ok()?;
        s.library
            .assets
            .iter()
            .find(|a| a.id.local == local)
            .cloned()
    }

    /// The bytes of one of an asset's files.
    pub fn file(&self, local: &str, kind: ResourceKind) -> Option<Vec<u8>> {
        let s = self.state.lock().ok()?;
        s.files.get(&(local.to_owned(), kind)).cloned()
    }

    /// Albums by name, with their assets' local ids.
    pub fn album_members(&self, name: &str) -> Vec<String> {
        let Ok(s) = self.state.lock() else {
            return Vec::new();
        };
        let Some(al) = s.library.albums.iter().find(|a| a.name == name) else {
            return Vec::new();
        };
        s.library
            .assets
            .iter()
            .filter(|a| a.albums.contains(&al.id))
            .map(|a| a.id.local.clone())
            .collect()
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

    fn fetch_with(
        &self,
        progress: &(dyn Fn(Listing) + Sync),
        stop: &AtomicBool,
    ) -> Result<Library> {
        if stop.load(Ordering::Relaxed) {
            return Err(CoreError::Cancelled);
        }
        let s = self
            .state
            .lock()
            .map_err(|_| CoreError::Photos("fake library poisoned".into()))?;
        match s.access {
            Some(Access::Full | Access::Limited) => {
                let (n, a) = (s.library.assets.len(), s.library.albums.len());
                progress(Listing::Photos { done: n, total: n });
                progress(Listing::Albums { done: a, total: a });
                progress(Listing::CloudIds { done: n, total: n });
                let mut lib = s.library.clone();
                if self.lazy.load(Ordering::Relaxed) {
                    for a in &mut lib.assets {
                        a.live = a.resource(ResourceKind::PairedVideo).is_some();
                        a.resources.clear();
                    }
                }
                Ok(lib)
            }
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
            #[allow(clippy::panic)]
            if s.panicking.contains(&asset.local) {
                drop(s);
                panic!("fake: {} panics", asset.local);
            }
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

    fn lookup(&self, ids: &[AssetId]) -> Result<Vec<Asset>> {
        let s = self
            .state
            .lock()
            .map_err(|_| CoreError::Photos("fake library poisoned".into()))?;
        Ok(s.library
            .assets
            .iter()
            .filter(|a| ids.iter().any(|i| i.local == a.id.local))
            .cloned()
            .collect())
    }

    fn push(&self, req: &PushRequest) -> Result<AssetId> {
        let bytes = std::fs::read(&req.file).map_err(io(&req.file))?;
        let mut s = self
            .state
            .lock()
            .map_err(|_| CoreError::Photos("fake library poisoned".into()))?;
        let name = req
            .file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let now = jiff::Timestamp::now();
        let id = match &req.edit_of {
            Some(of) => {
                let a = s
                    .library
                    .assets
                    .iter_mut()
                    .find(|a| a.id.local == of.local)
                    .ok_or_else(|| CoreError::Photos(format!("{} isn't in Photos", of.local)))?;
                a.edited = Some(EditedBy::Shoebox);
                a.modified = Some(now);
                a.resources.retain(|r| r.kind != ResourceKind::Edited);
                a.resources.push(Resource {
                    kind: ResourceKind::Edited,
                    file_name: "FullSizeRender.jpg".into(),
                    size: Some(bytes.len() as u64),
                });
                let id = a.id.clone();
                s.files
                    .insert((id.local.clone(), ResourceKind::Edited), bytes);
                id
            }
            None => {
                let n = s.library.assets.len();
                let id = AssetId {
                    cloud: None,
                    local: format!("pushed-{n}"),
                };
                s.library.assets.push(Asset {
                    id: id.clone(),
                    media: Media::Photo,
                    created: Some(now),
                    modified: Some(now),
                    location: None,
                    favourite: false,
                    hidden: false,
                    burst: None,
                    live: false,
                    width: 0,
                    height: 0,
                    resources: vec![Resource {
                        kind: ResourceKind::Original,
                        file_name: name,
                        size: Some(bytes.len() as u64),
                    }],
                    albums: Vec::new(),
                    edited: None,
                });
                s.files
                    .insert((id.local.clone(), ResourceKind::Original), bytes);
                id
            }
        };
        if let Some(album) = &req.album {
            let existing = s
                .library
                .albums
                .iter()
                .find(|a| &a.name == album)
                .map(|a| a.id.clone());
            let album_id = match existing {
                Some(a) => a,
                None => {
                    let a = format!("album-{}", s.library.albums.len());
                    s.library.albums.push(super::model::Album {
                        id: a.clone(),
                        name: album.clone(),
                        folders: Vec::new(),
                    });
                    a
                }
            };
            if let Some(a) = s.library.assets.iter_mut().find(|a| a.id.local == id.local)
                && !a.albums.contains(&album_id)
            {
                a.albums.push(album_id);
            }
        }
        self.pushes.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    fn undo_push(&self, id: &AssetId, created: bool) -> Result<()> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| CoreError::Photos("fake library poisoned".into()))?;
        if created {
            s.library.assets.retain(|a| a.id.local != id.local);
            s.files.retain(|(l, _), _| l != &id.local);
            return Ok(());
        }
        let a = s
            .library
            .assets
            .iter_mut()
            .find(|a| a.id.local == id.local)
            .ok_or_else(|| CoreError::Photos(format!("{} isn't in Photos", id.local)))?;
        a.edited = None;
        a.modified = Some(jiff::Timestamp::now());
        a.resources.retain(|r| r.kind != ResourceKind::Edited);
        s.files.remove(&(id.local.clone(), ResourceKind::Edited));
        Ok(())
    }
}
