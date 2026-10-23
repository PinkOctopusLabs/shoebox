//! What a Photos library holds, as Shoebox sees it: assets, their files
//! (resources) and albums. A [`super::PhotoSource`] fills these in.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// The format id Shoebox's own edits carry in Photos (`PHAdjustmentData`),
/// so they're recognised and never imported back.
pub const ADJUSTMENT_FORMAT: &str = crate::BUNDLE_ID;

/// An asset's identity in Photos.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssetId {
    /// `PHCloudIdentifier`: the same on every Mac and after Photos rebuilds
    /// its library. None for an asset that isn't in iCloud (yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<String>,
    /// `PHAsset.localIdentifier`: this Mac's.
    pub local: String,
}

impl AssetId {
    /// The key the sync record uses: the cloud id, else the local one.
    pub fn key(&self) -> String {
        match &self.cloud {
            Some(c) => c.clone(),
            None => format!("local:{}", self.local),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Media {
    Photo,
    Video,
}

/// One file of an asset (`PHAssetResourceType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// The photo or video as it was added (`.photo`, `.video`).
    Original,
    /// The other half of a RAW+JPEG pair (`.alternatePhoto`).
    Alternate,
    /// A Live Photo's movie (`.pairedVideo`).
    PairedVideo,
    /// The current edit, rendered (`.fullSizePhoto`, `.fullSizeVideo`).
    Edited,
}

impl ResourceKind {
    /// Files that are the asset as it was taken (not an edit).
    pub fn is_original(self) -> bool {
        !matches!(self, ResourceKind::Edited)
    }
}

/// A file of an asset, as Photos describes it (nothing downloaded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub kind: ResourceKind,
    /// The name it was added with (`IMG_1234.HEIC`), or Photos' name for
    /// an edit (`FullSizeRender.jpg`).
    pub file_name: String,
    /// Bytes, when Photos says.
    pub size: Option<u64>,
}

/// Who made an asset's current edit (from its adjustment data's format id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditedBy {
    /// Photos itself, or another app: a change made outside Shoebox.
    Other(String),
    /// Shoebox ([`ADJUSTMENT_FORMAT`]).
    Shoebox,
}

impl EditedBy {
    pub fn from_format(format: &str) -> Self {
        if format == ADJUSTMENT_FORMAT {
            EditedBy::Shoebox
        } else {
            EditedBy::Other(format.to_owned())
        }
    }
}

/// An asset's place in a burst.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Burst {
    pub id: String,
    /// The frame Photos shows for the burst.
    pub representative: bool,
    /// A frame the person kept.
    pub picked: bool,
}

/// A photo or video in Photos.
#[derive(Debug, Clone, PartialEq)]
pub struct Asset {
    pub id: AssetId,
    pub media: Media,
    /// When it was taken, as Photos has it (the person may have changed it).
    pub created: Option<Timestamp>,
    /// Last change in Photos (an edit, a favourite).
    pub modified: Option<Timestamp>,
    /// Where it was taken, as Photos has it (degrees, north and east positive).
    pub location: Option<(f64, f64)>,
    pub favourite: bool,
    pub hidden: bool,
    pub burst: Option<Burst>,
    /// A Live Photo (it has a movie).
    pub live: bool,
    pub width: u32,
    pub height: u32,
    /// Its files. Empty when the listing left them out (PhotoKit: asking
    /// for 30,000 assets' files one by one takes minutes): they're looked
    /// up just before the asset downloads ([`super::PhotoSource::resources`]).
    pub resources: Vec<Resource>,
    /// Ids of the albums it's in.
    pub albums: Vec<String>,
    /// Who made its current edit; None if it has none.
    pub edited: Option<EditedBy>,
}

impl Asset {
    pub fn resource(&self, kind: ResourceKind) -> Option<&Resource> {
        self.resources.iter().find(|r| r.kind == kind)
    }

    /// Has an edit made outside Shoebox.
    pub fn edited_elsewhere(&self) -> bool {
        matches!(self.edited, Some(EditedBy::Other(_)))
            && (self.resources.is_empty() || self.resource(ResourceKind::Edited).is_some())
    }
}

/// An album the person made (smart albums aren't listed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub id: String,
    pub name: String,
    /// The folders it's in, outermost first.
    pub folders: Vec<String>,
}

impl Album {
    /// Folders and name, outermost first (`["Trips", "Japan"]`).
    pub fn path(&self) -> Vec<String> {
        let mut p = self.folders.clone();
        p.push(self.name.clone());
        p
    }
}

/// A rendered file to put into Photos (PHOTOS-SPEC, Push).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRequest {
    /// The rendered file, in the cache (JPEG for an edit of an asset).
    pub file: std::path::PathBuf,
    /// Edit this asset (its original stays; Photos can revert); None: a
    /// new asset from the file.
    pub edit_of: Option<AssetId>,
    /// Also put it in the album of this name (made if needed).
    pub album: Option<String>,
    /// Kept with an edit as its adjustment data (format [`ADJUSTMENT_FORMAT`]).
    pub adjustment: Vec<u8>,
}

/// What Shoebox may do with the Photos library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Not asked yet.
    NotAsked,
    /// Every asset.
    Full,
    /// Only the assets the person chose.
    Limited,
    /// Refused (or restricted by the Mac's settings).
    Denied,
}

/// How far listing Photos has got: each stage's done and total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    /// Reading each photo's details.
    Photos { done: usize, total: usize },
    /// Reading the albums and what's in them.
    Albums { done: usize, total: usize },
    /// Matching photos to their iCloud ids.
    CloudIds { done: usize, total: usize },
}

/// Everything Photos lists (metadata only).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Library {
    pub assets: Vec<Asset>,
    pub albums: Vec<Album>,
}
