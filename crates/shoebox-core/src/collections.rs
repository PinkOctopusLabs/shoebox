//! Collections: named virtual sets of stacks. No files move.
//!
//! Stored in `<root>/.shoebox/collections.json` in every writable root and
//! merged on load (newest edit of each collection wins), like events, so
//! they survive a cache rebuild. A stack is remembered by its files' paths,
//! so it stays in the collection whichever member becomes primary; moves
//! made by Shoebox rewrite the paths in the same batch.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::error::{CoreError, Result, io};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub name: String,
    /// Paths of member stacks' primaries (when added).
    #[serde(default)]
    pub members: Vec<PathBuf>,
    /// Milliseconds since the epoch of the last edit (merge tiebreak).
    #[serde(default)]
    pub updated: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CollectionsFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    collections: Vec<Collection>,
}

pub fn file_for(root: &Path) -> PathBuf {
    root.join(".shoebox").join("collections.json")
}

pub fn new_id() -> String {
    crate::events::new_id().replacen("ev-", "col-", 1)
}

/// Every root's collections, merged (newest `updated` per id wins).
pub fn load(roots: &[PathBuf]) -> Vec<Collection> {
    let mut by_id: HashMap<String, Collection> = HashMap::new();
    for root in roots {
        let Ok(bytes) = std::fs::read(file_for(root)) else {
            continue;
        };
        match serde_json::from_slice::<CollectionsFile>(&bytes) {
            Ok(f) => {
                for c in f.collections {
                    if by_id.get(&c.id).is_none_or(|old| old.updated < c.updated) {
                        by_id.insert(c.id.clone(), c);
                    }
                }
            }
            Err(e) => tracing::warn!("ignoring unreadable {}: {e}", file_for(root).display()),
        }
    }
    let mut v: Vec<Collection> = by_id.into_values().collect();
    v.sort_by_key(|c| c.name.to_lowercase());
    v
}

/// The bytes of `collections.json` for every writable root.
pub fn plan_save(
    writable_roots: &[PathBuf],
    list: &[Collection],
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let body = serde_json::to_vec_pretty(&CollectionsFile {
        version: 1,
        collections: list.to_vec(),
    })
    .map_err(|e| CoreError::Invalid(format!("collections: {e}")))?;
    writable_roots
        .iter()
        .map(|r| {
            let dir = r.join(".shoebox");
            std::fs::create_dir_all(&dir).map_err(io(&dir))?;
            Ok((file_for(r), body.clone()))
        })
        .collect()
}

/// Stack indices in a collection (a stack counts if any of its files is
/// listed).
pub fn members(c: &Catalog, col: &Collection) -> HashSet<usize> {
    let listed: HashSet<&Path> = col.members.iter().map(PathBuf::as_path).collect();
    c.photos
        .iter()
        .enumerate()
        .filter(|(_, p)| listed.contains(p.path.as_path()))
        .map(|(i, _)| c.stack_of[i])
        .collect()
}
