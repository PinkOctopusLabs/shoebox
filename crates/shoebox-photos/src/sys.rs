//! Thin FFI wrapper over PhotoKit: plain Rust values out, nothing kept.
//! The crate's only `unsafe`. Callers block on PhotoKit's completion
//! handlers here (never on the UI thread).
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{Retained, autoreleasepool};
use objc2_foundation::{NSArray, NSBundle, NSData, NSDate, NSError, NSString};
use objc2_photos::{
    PHAccessLevel, PHAsset, PHAssetCollection, PHAssetCollectionSubtype, PHAssetCollectionType,
    PHAssetResource, PHAssetResourceManager, PHAssetResourceRequestOptions, PHAuthorizationStatus,
    PHCollection, PHCollectionList, PHFetchOptions, PHPhotoLibrary,
};

/// `PHAuthorizationStatus` for read and write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
    Limited,
}

fn status_of(s: PHAuthorizationStatus) -> Status {
    match s {
        PHAuthorizationStatus::NotDetermined => Status::NotDetermined,
        PHAuthorizationStatus::Restricted => Status::Restricted,
        PHAuthorizationStatus::Authorized => Status::Authorized,
        PHAuthorizationStatus::Limited => Status::Limited,
        _ => Status::Denied,
    }
}

/// What Shoebox may do now (asks nothing).
pub fn status() -> Status {
    // SAFETY: a class method with no preconditions.
    status_of(unsafe {
        PHPhotoLibrary::authorizationStatusForAccessLevel(PHAccessLevel::ReadWrite)
    })
}

/// Whether the running app's Info.plist explains why it wants Photos.
/// Without it macOS ends the process when access is asked for (an
/// unbundled `cargo run` binary has none).
pub fn has_usage_description() -> bool {
    autoreleasepool(|_| {
        NSBundle::mainBundle()
            .objectForInfoDictionaryKey(&NSString::from_str("NSPhotoLibraryUsageDescription"))
            .is_some()
    })
}

/// Ask for read and write access (macOS shows its prompt once); the answer.
pub fn request() -> Status {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let block = RcBlock::new(move |s: PHAuthorizationStatus| {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(status_of(s));
        }
    });
    // SAFETY: the handler is a valid block PhotoKit copies and calls once.
    unsafe {
        PHPhotoLibrary::requestAuthorizationForAccessLevel_handler(PHAccessLevel::ReadWrite, &block)
    };
    rx.recv().unwrap_or(Status::Denied)
}

/// A resource as PhotoKit lists it.
#[derive(Debug, Clone)]
pub struct RawResource {
    /// `PHAssetResourceType`.
    pub kind: isize,
    pub name: String,
}

/// An asset as PhotoKit lists it.
#[derive(Debug, Clone)]
pub struct RawAsset {
    pub local: String,
    /// `PHAssetMediaType`: 1 image, 2 video.
    pub media: isize,
    /// Seconds since 1970.
    pub created: Option<f64>,
    pub modified: Option<f64>,
    pub location: Option<(f64, f64)>,
    pub favourite: bool,
    pub hidden: bool,
    pub burst: Option<String>,
    pub represents_burst: bool,
    /// `PHAssetBurstSelectionType` bits: 1 auto pick, 2 user pick.
    pub burst_selection: usize,
    pub width: usize,
    pub height: usize,
    pub has_adjustments: bool,
    pub adjustment_format: Option<String>,
    pub resources: Vec<RawResource>,
}

fn date(d: Option<Retained<NSDate>>) -> Option<f64> {
    d.map(|d| d.timeIntervalSince1970())
}

fn read_asset(a: &PHAsset) -> RawAsset {
    // SAFETY: plain property reads on a valid asset.
    unsafe {
        let location = a.location().map(|l| {
            let c = l.coordinate();
            (c.latitude, c.longitude)
        });
        let resources = PHAssetResource::assetResourcesForAsset(a)
            .iter()
            .map(|r| RawResource {
                kind: r.r#type().0,
                name: r.originalFilename().to_string(),
            })
            .collect();
        RawAsset {
            local: a.localIdentifier().to_string(),
            media: a.mediaType().0,
            created: date(a.creationDate()),
            modified: date(a.modificationDate()),
            location,
            favourite: a.isFavorite(),
            hidden: a.isHidden(),
            burst: a.burstIdentifier().map(|b| b.to_string()),
            represents_burst: a.representsBurst(),
            burst_selection: a.burstSelectionTypes().0,
            width: a.pixelWidth(),
            height: a.pixelHeight(),
            has_adjustments: a.hasAdjustments(),
            adjustment_format: a.adjustmentFormatIdentifier().map(|f| f.to_string()),
            resources,
        }
    }
}

/// Every asset, hidden ones and every burst frame included, so Shoebox's
/// own options decide what's left out. `progress(done, total)`.
pub fn assets(progress: &dyn Fn(usize, usize)) -> Vec<RawAsset> {
    // SAFETY: fetch options are plain objects; the result is read in place.
    unsafe {
        let opts = PHFetchOptions::new();
        opts.setIncludeHiddenAssets(true);
        opts.setIncludeAllBurstAssets(true);
        let found = PHAsset::fetchAssetsWithOptions(Some(&opts));
        let n = found.count();
        let mut out = Vec::with_capacity(n);
        // Autorelease pools in chunks keep 30,000 assets' objects from
        // piling up.
        for start in (0..n).step_by(500) {
            autoreleasepool(|_| {
                for i in start..(start + 500).min(n) {
                    out.push(read_asset(&found.objectAtIndex(i)));
                }
            });
            progress(out.len(), n);
        }
        out
    }
}

/// An album the person made.
#[derive(Debug, Clone)]
pub struct RawAlbum {
    pub id: String,
    pub name: String,
    /// The folders it's in, outermost first.
    pub folders: Vec<String>,
    /// Local ids of its assets.
    pub assets: Vec<String>,
}

/// The folders around `c`, outermost first.
fn folders_of(c: &PHCollection) -> Vec<String> {
    let mut out = Vec::new();
    // SAFETY: fetches over valid collections; at most 32 levels.
    unsafe {
        let mut current: Retained<PHCollection> = Retained::from(c);
        for _ in 0..32 {
            let Some(parent) =
                PHCollectionList::fetchCollectionListsContainingCollection_options(&current, None)
                    .firstObject()
            else {
                break;
            };
            out.push(
                parent
                    .localizedTitle()
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
            );
            current = Retained::into_super(parent);
        }
    }
    out.reverse();
    out
}

/// The titles of the person's own albums, and nothing else.
pub fn album_titles() -> Vec<String> {
    // SAFETY: a fetch with no options; titles read in place.
    unsafe {
        let found = PHAssetCollection::fetchAssetCollectionsWithType_subtype_options(
            PHAssetCollectionType::Album,
            PHAssetCollectionSubtype::AlbumRegular,
            None,
        );
        (0..found.count())
            .filter_map(|i| {
                found
                    .objectAtIndex(i)
                    .localizedTitle()
                    .map(|t| t.to_string())
            })
            .collect()
    }
}

/// The person's own albums (not smart albums or shared ones).
pub fn albums() -> Vec<RawAlbum> {
    let mut out = Vec::new();
    // SAFETY: fetches with valid options; results read in place.
    unsafe {
        let found = PHAssetCollection::fetchAssetCollectionsWithType_subtype_options(
            PHAssetCollectionType::Album,
            PHAssetCollectionSubtype::AlbumRegular,
            None,
        );
        let opts = PHFetchOptions::new();
        opts.setIncludeHiddenAssets(true);
        opts.setIncludeAllBurstAssets(true);
        for i in 0..found.count() {
            autoreleasepool(|_| {
                let album = found.objectAtIndex(i);
                let in_it = PHAsset::fetchAssetsInAssetCollection_options(&album, Some(&opts));
                let assets = (0..in_it.count())
                    .map(|j| in_it.objectAtIndex(j).localIdentifier().to_string())
                    .collect();
                out.push(RawAlbum {
                    id: album.localIdentifier().to_string(),
                    name: album
                        .localizedTitle()
                        .map(|t| t.to_string())
                        .unwrap_or_default(),
                    folders: folders_of(&album),
                    assets,
                });
            });
        }
    }
    out
}

/// Cloud ids for local ids (those Photos has one for).
pub fn cloud_ids(locals: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    // SAFETY: arrays of valid strings; the shared library always exists.
    unsafe {
        let lib = PHPhotoLibrary::sharedPhotoLibrary();
        for chunk in locals.chunks(1000) {
            autoreleasepool(|_| {
                let ids: Vec<Retained<NSString>> =
                    chunk.iter().map(|s| NSString::from_str(s)).collect();
                let arr = NSArray::from_retained_slice(&ids);
                let map = lib.cloudIdentifierMappingsForLocalIdentifiers(&arr);
                for (local, s) in ids.iter().zip(chunk) {
                    let Some(m) = map.objectForKey(local) else {
                        continue;
                    };
                    if m.error().is_none()
                        && let Some(c) = m.cloudIdentifier()
                    {
                        out.insert(s.clone(), c.stringValue().to_string());
                    }
                }
            });
        }
    }
    out
}

fn error_text(e: &NSError) -> String {
    e.localizedDescription().to_string()
}

/// Write one resource of an asset (by local id and `PHAssetResourceType`)
/// to `to`, a new file, downloading it from iCloud if needed. On an error
/// or a stop the partial file is removed. `Err(None)`: stopped.
pub fn download(
    local: &str,
    kind: isize,
    to: &Path,
    progress: &(dyn Fn(f64) + Sync),
    stop: &AtomicBool,
) -> Result<(), Option<String>> {
    let file = std::fs::File::create_new(to).map_err(|e| Some(format!("{}: {e}", to.display())))?;
    let file = Arc::new(Mutex::new(Some(std::io::BufWriter::new(file))));
    let write_failed = Arc::new(Mutex::new(None::<String>));
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let (ptx, prx) = mpsc::channel::<f64>();
    let result = autoreleasepool(|_| {
        // SAFETY: fetches by valid ids; blocks are valid and outlive the
        // request (PhotoKit copies them); the data pointer is valid for the
        // duration of the data handler.
        unsafe {
            let ids = NSArray::from_retained_slice(&[NSString::from_str(local)]);
            let Some(asset) =
                PHAsset::fetchAssetsWithLocalIdentifiers_options(&ids, None).firstObject()
            else {
                return Err(Some(format!("{local} isn't in Photos any more")));
            };
            let Some(resource) = PHAssetResource::assetResourcesForAsset(&asset)
                .iter()
                .find(|r| r.r#type().0 == kind)
            else {
                return Err(Some(format!("{local} has no file of kind {kind}")));
            };
            let opts = PHAssetResourceRequestOptions::new();
            opts.setNetworkAccessAllowed(true);
            let ptx = Mutex::new(ptx);
            let on_progress = RcBlock::new(move |f: f64| {
                if let Ok(p) = ptx.lock() {
                    let _ = p.send(f);
                }
            });
            opts.setProgressHandler(RcBlock::as_ptr(&on_progress));
            let (f2, w2) = (file.clone(), write_failed.clone());
            let on_data = RcBlock::new(move |data: NonNull<NSData>| {
                let bytes = data.as_ref().to_vec();
                if let Ok(mut g) = f2.lock()
                    && let Some(w) = g.as_mut()
                    && let Err(e) = w.write_all(&bytes)
                    && let Ok(mut fail) = w2.lock()
                {
                    *fail = Some(e.to_string());
                }
            });
            let tx = Mutex::new(tx);
            let on_done = RcBlock::new(move |err: *mut NSError| {
                let r = err.as_ref().map(error_text);
                if let Ok(t) = tx.lock() {
                    let _ = t.send(r);
                }
            });
            let manager = PHAssetResourceManager::defaultManager();
            let id = manager
                .requestDataForAssetResource_options_dataReceivedHandler_completionHandler(
                    &resource,
                    Some(&opts),
                    &on_data,
                    &on_done,
                );
            let mut cancelled = false;
            loop {
                while let Ok(f) = prx.try_recv() {
                    progress(f);
                }
                match rx.recv_timeout(Duration::from_millis(200)) {
                    // Stopped: whatever PhotoKit says about the cancelled request.
                    Ok(_) if cancelled => break Err(None),
                    Ok(Some(e)) => break Err(Some(e)),
                    Ok(None) => break Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if stop.load(Ordering::Relaxed) && !cancelled {
                            manager.cancelDataRequest(id);
                            cancelled = true;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        break Err(Some("Photos stopped answering".into()));
                    }
                }
            }
        }
    });
    let flushed = file
        .lock()
        .ok()
        .and_then(|mut g| g.take())
        .map(|mut w| w.flush().map_err(|e| e.to_string()));
    let failed = write_failed.lock().ok().and_then(|mut g| g.take());
    let result = match (result, failed, flushed) {
        (Err(e), _, _) => Err(e),
        (Ok(()), Some(e), _) | (Ok(()), None, Some(Err(e))) => Err(Some(e)),
        (Ok(()), None, _) => Ok(()),
    };
    if result.is_err() {
        // Our own half-written file in the cache.
        let _ = std::fs::remove_file(to);
    }
    result
}
