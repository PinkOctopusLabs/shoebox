//! Planning a sync, before anything is downloaded: which assets are new,
//! which were synced before (and what changed on them in Photos since),
//! which files of each are wanted, and what each album becomes.

use std::collections::{BTreeMap, HashSet};

use super::ledger::{AlbumChoice, AlbumUse, Ledgers};
use super::model::{Asset, Library, ResourceKind};

/// What a favourite in Photos becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Favourite {
    /// This rating (1–5).
    Rating(u8),
    /// The keyword `Photos|Favourite`.
    Keyword,
    /// A pick flag.
    Pick,
    Nothing,
}

/// The person's choices for a sync (Settings › iCloud Photos).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Options {
    /// Copy Live Photos' movies.
    pub live_videos: bool,
    /// Copy Photos' edits too, as `-photos` variants.
    pub photos_edits: bool,
    /// Copy every frame of a burst, not just the ones kept.
    pub all_burst_frames: bool,
    /// Copy hidden assets (keyword `Photos|Hidden`).
    pub hidden: bool,
    pub favourite: Favourite,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            live_videos: true,
            photos_edits: true,
            all_burst_frames: false,
            hidden: false,
            favourite: Favourite::Rating(5),
        }
    }
}

/// An asset to download, and which of its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// Index into [`Library::assets`].
    pub asset: usize,
    pub resources: Vec<ResourceKind>,
}

/// An album as the plan lists it: what its name becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRow {
    pub id: String,
    /// Photos' folders and name, outermost first.
    pub path: Vec<String>,
    /// The choice (remembered from the last sync, or the default).
    pub choice: AlbumChoice,
    /// Assets in it that this sync copies or has copied before.
    pub photos: usize,
}

/// An album renamed in Photos since the last sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRename {
    pub id: String,
    /// Its name in Photos then and now (folders and name, `›`-joined).
    pub from: String,
    pub to: String,
}

/// What a sync will do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    /// New assets to copy, oldest first.
    pub new: Vec<Planned>,
    /// Assets synced before with a newer edit in Photos: its render is
    /// downloaded and, if it's new, copied as another `-photos` variant.
    pub updated: Vec<Planned>,
    /// Assets synced before that joined albums since: (asset, album ids).
    pub joined: Vec<(usize, Vec<String>)>,
    /// Assets synced before and favourited in Photos since.
    pub favourited: Vec<usize>,
    /// Assets synced before that left albums since: (asset, album ids).
    /// They leave those albums' events (or lose their keywords).
    pub departed: Vec<(usize, Vec<String>)>,
    /// Albums assets left in Photos since the last sync: (album id, how
    /// many), for the summary.
    pub left: Vec<(String, usize)>,
    /// Albums renamed (or moved) in Photos since the last sync.
    pub renamed: Vec<AlbumRename>,
    /// Albums gone from Photos since the last sync: (id, name). Their
    /// events stay, no longer linked.
    pub gone: Vec<(String, String)>,
    /// What each album became last time (all of them, also those without
    /// photos now), for the sync.
    pub remembered: BTreeMap<String, AlbumChoice>,
    /// Synced before, nothing new.
    pub synced: usize,
    /// Left out: hidden in Photos.
    pub hidden: usize,
    /// Left out: burst frames nobody kept.
    pub burst_frames: usize,
    /// Left out: made in Photos by Shoebox (sent there).
    pub sent: usize,
    /// Albums with assets this sync copies or copied before.
    pub albums: Vec<AlbumRow>,
    /// Bytes to download, as far as Photos says, and how many files it
    /// gave no size for.
    pub bytes: u64,
    pub unknown_sizes: usize,
}

impl Plan {
    /// Assets with something to download.
    pub fn downloads(&self) -> impl Iterator<Item = &Planned> {
        self.new.iter().chain(&self.updated)
    }
}

/// The files of `a` a sync wants.
fn wanted(a: &Asset, o: &Options) -> Vec<ResourceKind> {
    // Listed without its files: what it may have, by its flags (an
    // Alternate it turns out not to have is skipped at download).
    if a.resources.is_empty() {
        let mut v = vec![ResourceKind::Original, ResourceKind::Alternate];
        if a.live && o.live_videos {
            v.push(ResourceKind::PairedVideo);
        }
        if o.photos_edits && a.edited_elsewhere() {
            v.push(ResourceKind::Edited);
        }
        return v;
    }
    let mut v = Vec::new();
    for r in &a.resources {
        let want = match r.kind {
            ResourceKind::Original | ResourceKind::Alternate => true,
            ResourceKind::PairedVideo => o.live_videos,
            // An edit made by Shoebox goes back to Shoebox's own original.
            ResourceKind::Edited => o.photos_edits && a.edited_elsewhere(),
        };
        if want && !v.contains(&r.kind) {
            v.push(r.kind);
        }
    }
    v
}

/// What an album becomes when nothing was chosen before: an event, unless
/// its name says nothing (a date, `Photos`).
pub fn default_choice(path: &[String]) -> AlbumChoice {
    let last = path.last().map(String::as_str).unwrap_or("");
    AlbumChoice {
        name: path.join(" › "),
        kind: if crate::import::says_nothing(last) {
            AlbumUse::Nothing
        } else {
            AlbumUse::Event
        },
        photos_name: Some(path.join(" › ")),
        event: None,
    }
}

/// Plan a sync of `library` against the records in `ledgers`. `choices`:
/// what each album became last time (from the destination root's record).
pub fn plan(
    library: &Library,
    ledgers: &Ledgers,
    choices: &BTreeMap<String, AlbumChoice>,
    o: &Options,
) -> Plan {
    let mut out = Plan::default();
    let mut order: Vec<usize> = (0..library.assets.len()).collect();
    order.sort_by_key(|&i| library.assets[i].created);
    let mut in_album: BTreeMap<&str, usize> = BTreeMap::new();
    let mut left: BTreeMap<String, usize> = BTreeMap::new();
    for i in order {
        let a = &library.assets[i];
        if a.hidden && !o.hidden {
            out.hidden += 1;
            continue;
        }
        if let Some(b) = &a.burst
            && !o.all_burst_frames
            && !b.representative
            && !b.picked
        {
            out.burst_frames += 1;
            continue;
        }
        if ledgers.pushed(&a.id).is_some_and(|(_, p)| p.created) {
            out.sent += 1;
            continue;
        }
        match ledgers.find(&a.id) {
            None => {
                let resources = wanted(a, o);
                if !resources.contains(&ResourceKind::Original) {
                    // Nothing to copy (Photos lists no original).
                    continue;
                }
                add_sizes(&mut out, a, &resources);
                for id in &a.albums {
                    *in_album.entry(id).or_default() += 1;
                }
                out.new.push(Planned {
                    asset: i,
                    resources,
                });
            }
            Some((_, e)) => {
                for id in &a.albums {
                    *in_album.entry(id).or_default() += 1;
                }
                let had: HashSet<&str> = e.albums.iter().map(String::as_str).collect();
                let now: HashSet<&str> = a.albums.iter().map(String::as_str).collect();
                let joined: Vec<String> = a
                    .albums
                    .iter()
                    .filter(|x| !had.contains(x.as_str()))
                    .cloned()
                    .collect();
                let mut departed: Vec<String> = e
                    .albums
                    .iter()
                    .filter(|x| !now.contains(x.as_str()))
                    .cloned()
                    .collect();
                departed.dedup();
                for x in &departed {
                    *left.entry(x.clone()).or_default() += 1;
                }
                let mut changed = false;
                if !joined.is_empty() {
                    out.joined.push((i, joined));
                    changed = true;
                }
                if !departed.is_empty() {
                    out.departed.push((i, departed));
                    changed = true;
                }
                if a.favourite && !e.favourite {
                    out.favourited.push(i);
                    changed = true;
                }
                // A newer edit made in Photos (a favourite changes the
                // modified time too; the render's hash tells them apart).
                let newer = o.photos_edits
                    && a.edited_elsewhere()
                    && match (&e.edit_modified, a.modified) {
                        (Some(at), Some(m)) => {
                            at.parse::<jiff::Timestamp>().map_or(true, |at| m > at)
                        }
                        (None, _) => true,
                        (Some(_), None) => false,
                    };
                if newer {
                    let resources = vec![ResourceKind::Edited];
                    add_sizes(&mut out, a, &resources);
                    out.updated.push(Planned {
                        asset: i,
                        resources,
                    });
                    changed = true;
                }
                if !changed {
                    out.synced += 1;
                }
            }
        }
    }
    out.left = left.into_iter().collect();
    out.remembered = choices.clone();
    let present: HashSet<&str> = library.albums.iter().map(|a| a.id.as_str()).collect();
    out.gone = choices
        .iter()
        .filter(|(id, _)| !present.contains(id.as_str()))
        .map(|(id, c)| {
            (
                id.clone(),
                c.photos_name.clone().unwrap_or_else(|| c.name.clone()),
            )
        })
        .collect();
    for al in &library.albums {
        let now = al.path().join(" › ");
        if let Some(c) = choices.get(&al.id)
            && let Some(was) = &c.photos_name
            && *was != now
        {
            out.renamed.push(AlbumRename {
                id: al.id.clone(),
                from: was.clone(),
                to: now,
            });
        }
    }
    out.albums = library
        .albums
        .iter()
        .filter_map(|al| {
            let photos = *in_album.get(al.id.as_str())?;
            let path = al.path();
            let now = path.join(" › ");
            let mut choice = choices
                .get(&al.id)
                .cloned()
                .unwrap_or_else(|| default_choice(&path));
            // Renamed in Photos, and not in Shoebox: the new name.
            if choice
                .photos_name
                .as_deref()
                .is_some_and(|was| was != now && choice.name == was)
            {
                choice.name = now.clone();
            }
            choice.photos_name = Some(now);
            Some(AlbumRow {
                id: al.id.clone(),
                path,
                choice,
                photos,
            })
        })
        .collect();
    out.albums.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn add_sizes(out: &mut Plan, a: &Asset, kinds: &[ResourceKind]) {
    for k in kinds {
        match a.resource(*k).and_then(|r| r.size) {
            Some(s) => out.bytes += s,
            None => out.unknown_sizes += 1,
        }
    }
}
