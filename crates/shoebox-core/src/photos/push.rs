//! Sending photos to Photos (PHOTOS-SPEC, Push): what each becomes there,
//! running the sends with their records in one batch, and undoing them.
//!
//! Photos always gets a rendered file (an export), never a raw or an
//! original. A photo that came from Photos goes back as an edit of the
//! asset it came from (Photos keeps the original and can revert); any
//! other becomes a new asset; sending again updates the same asset. The
//! records (`.shoebox/photos/pushed-<run>.json`) keep a sync from importing
//! them back.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::ledger::{self, Ledgers, Pushed};
use super::model::{Asset, AssetId, PushRequest, ResourceKind};
use super::source::PhotoSource;
use super::sync::Batches;
use crate::error::{CoreError, Result};

/// A library photo the person asked to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub photo: PathBuf,
    /// Which edit: `main`, or a virtual copy's id.
    pub copy: String,
    /// The edit's preview key (None: unedited).
    pub render_hash: Option<String>,
    pub video: bool,
}

/// What a send does with one photo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A new asset.
    New,
    /// An edit of the asset the photo came from. `replaces`: that asset
    /// has an edit made in Photos, which this replaces there; `kept`: the
    /// library's copy of that edit (an import's `-photos` file).
    Edit {
        asset: AssetId,
        replaces: bool,
        kept: Option<PathBuf>,
    },
    /// A new edit on the asset an earlier send made or edited.
    Update { asset: AssetId, created: bool },
    /// Not sent: unchanged since it was sent.
    UpToDate,
    /// Not sent: it came from Photos and has no edit here.
    InPhotos,
    /// Not sent: Shoebox doesn't edit videos.
    Video,
}

impl Action {
    pub fn sends(&self) -> bool {
        matches!(
            self,
            Action::New | Action::Edit { .. } | Action::Update { .. }
        )
    }

    /// Whether its file must be a JPEG (PhotoKit's rule for edits).
    pub fn edits(&self) -> bool {
        matches!(self, Action::Edit { .. } | Action::Update { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub item: Item,
    pub action: Action,
}

/// The assets the plan needs to look at in Photos (to see their edits).
pub fn assets_to_look_up(items: &[Item], ledgers: &Ledgers, roots: &[PathBuf]) -> Vec<AssetId> {
    items
        .iter()
        .filter_map(|i| ledgers.asset_of(roots, &i.photo).map(|(id, _)| id))
        .collect()
}

/// What sending `items` would do. `assets`: those of
/// [`assets_to_look_up`], as Photos has them now.
pub fn plan(items: &[Item], ledgers: &Ledgers, roots: &[PathBuf], assets: &[Asset]) -> Vec<Row> {
    items
        .iter()
        .map(|item| Row {
            item: item.clone(),
            action: action_for(item, ledgers, roots, assets),
        })
        .collect()
}

fn action_for(item: &Item, ledgers: &Ledgers, roots: &[PathBuf], assets: &[Asset]) -> Action {
    if item.video {
        return Action::Video;
    }
    if let Some((key, p)) = ledgers.pushed_from(roots, &item.photo, &item.copy) {
        if p.render_hash == item.render_hash {
            return Action::UpToDate;
        }
        let cloud = (!key.starts_with("local:")).then(|| key.to_owned());
        return Action::Update {
            asset: AssetId {
                cloud,
                local: p.local_id.clone(),
            },
            created: p.created,
        };
    }
    if let Some((asset, _)) = ledgers.asset_of(roots, &item.photo) {
        if item.render_hash.is_none() {
            return Action::InPhotos;
        }
        let now = assets.iter().find(|a| a.id.local == asset.local);
        let replaces = now.is_some_and(Asset::edited_elsewhere);
        let kept = ledgers.find(&asset).and_then(|(r, e)| {
            let f = e.file(ResourceKind::Edited)?;
            Some(roots.get(r)?.join(&f.rel))
        });
        return Action::Edit {
            asset,
            replaces,
            kept,
        };
    }
    Action::New
}

/// What an edit's adjustment data says (for Shoebox to read back; Photos
/// only keeps it).
#[derive(Serialize)]
struct Adjustment<'a> {
    render_hash: Option<&'a str>,
    copy: &'a str,
}

/// A rendered file for a row, in the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub row: Row,
    pub file: PathBuf,
}

/// What a send did.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub batch: Option<i64>,
    pub created: usize,
    pub edited: usize,
    pub failed: Vec<(PathBuf, String)>,
}

/// The root a library photo is in (its index in `roots`).
fn root_of(roots: &[PathBuf], photo: &Path) -> Option<usize> {
    roots
        .iter()
        .enumerate()
        .filter(|(_, r)| photo.starts_with(r))
        .max_by_key(|(_, r)| r.components().count())
        .map(|(i, _)| i)
}

/// Send each rendered file to Photos, then record the sends in one batch
/// (`run`: when, for the record's name). A failed send is listed and the
/// rest go on.
#[allow(clippy::too_many_arguments)]
pub fn run(
    source: &dyn PhotoSource,
    batches: &dyn Batches,
    ledgers: &mut Ledgers,
    roots: &[PathBuf],
    rendered: &[Rendered],
    album: Option<&str>,
    run: &str,
    label: &str,
) -> Result<Report> {
    let mut report = Report::default();
    let mut sent: Vec<(usize, AssetId, Pushed)> = Vec::new();
    for r in rendered {
        let Some(root) = root_of(roots, &r.row.item.photo) else {
            report
                .failed
                .push((r.row.item.photo.clone(), "not in a library folder".into()));
            continue;
        };
        let (edit_of, created) = match &r.row.action {
            Action::New => (None, true),
            Action::Edit { asset, .. } => (Some(asset.clone()), false),
            Action::Update { asset, created } => (Some(asset.clone()), *created),
            _ => continue,
        };
        let adjustment = serde_json::to_vec(&Adjustment {
            render_hash: r.row.item.render_hash.as_deref(),
            copy: &r.row.item.copy,
        })
        .map_err(|e| CoreError::Invalid(format!("adjustment data: {e}")))?;
        let req = PushRequest {
            file: r.file.clone(),
            edit_of,
            album: album.map(str::to_owned),
            adjustment,
        };
        match source.push(&req) {
            Ok(id) => {
                if req.edit_of.is_some() {
                    report.edited += 1;
                } else {
                    report.created += 1;
                }
                let from = r
                    .row
                    .item
                    .photo
                    .strip_prefix(&roots[root])
                    .unwrap_or(&r.row.item.photo)
                    .to_string_lossy()
                    .into_owned();
                sent.push((
                    root,
                    id.clone(),
                    Pushed {
                        local_id: id.local.clone(),
                        from,
                        copy: r.row.item.copy.clone(),
                        created,
                        render_hash: r.row.item.render_hash.clone(),
                        pushed_at: crate::events::now_ms(),
                        batch: 0,
                    },
                ));
            }
            Err(e) => report
                .failed
                .push((r.row.item.photo.clone(), e.to_string())),
        }
    }
    if sent.is_empty() {
        return Ok(report);
    }
    let batch = batches.begin(label)?;
    report.batch = Some(batch);
    let mut writes = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        let here: Vec<(AssetId, Pushed)> = sent
            .iter()
            .filter(|(r, _, _)| *r == i)
            .map(|(_, id, p)| (id.clone(), Pushed { batch, ..p.clone() }))
            .collect();
        if here.is_empty() {
            continue;
        }
        writes.push(ledger::plan_pushed(root, run, &here)?);
        for (id, p) in here {
            ledgers.record_push(i, &id, p);
        }
    }
    batches.step(batch, Vec::new(), writes)?;
    batches.end(batch)?;
    Ok(report)
}

/// Undo the pushes `batch` recorded, in Photos: refused (nothing changed)
/// if an asset was changed in Photos since. The batch itself (the records)
/// is undone by the caller after this succeeds.
pub fn undo(source: &dyn PhotoSource, ledgers: &Ledgers, batch: i64) -> Result<usize> {
    let pushes = ledgers.pushes_of(batch);
    let ids: Vec<AssetId> = pushes
        .iter()
        .map(|(key, _, p)| AssetId {
            cloud: (!key.starts_with("local:")).then(|| key.clone()),
            local: p.local_id.clone(),
        })
        .collect();
    let now = source.lookup(&ids)?;
    for (id, (_, _, p)) in ids.iter().zip(&pushes) {
        let Some(a) = now.iter().find(|a| a.id.local == id.local) else {
            continue; // gone from Photos already
        };
        // Changed since (edited on a phone): a minute's grace for Photos'
        // own bookkeeping after the push.
        let changed = a
            .modified
            .is_some_and(|m| m.as_millisecond() > p.pushed_at + 60_000)
            && a.edited_elsewhere();
        if changed {
            return Err(CoreError::Refused(format!(
                "{} was changed in Photos after it was sent, so it's left as it is",
                p.from
            )));
        }
    }
    let mut n = 0;
    for (id, (_, _, p)) in ids.iter().zip(&pushes) {
        if now.iter().any(|a| a.id.local == id.local) {
            source.undo_push(id, p.created)?;
            n += 1;
        }
    }
    Ok(n)
}
