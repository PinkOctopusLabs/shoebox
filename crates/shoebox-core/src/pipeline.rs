//! Scan → analyse (through the cache) → duplicate items.

use rayon::prelude::*;

use crate::cache::{self, Cache};
use crate::dupes::Item;
use crate::error::Result;
use crate::hash::{self, Analysis};
use crate::scan::{Role, Root, ScannedFile};

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub files: usize,
    pub cached: usize,
    pub analyzed: usize,
    pub unreadable: usize,
    pub undecodable: usize,
}

/// A scanned file with its analysis.
pub struct Analyzed {
    pub file: ScannedFile,
    pub analysis: Analysis,
}

const STORE_CHUNK: usize = 256;

/// Analyse every file, reusing cached results for unchanged files. Files that
/// can't be read are skipped with a warning.
pub fn analyze_all(
    cache: &mut Cache,
    roots: &[Root],
    files: Vec<ScannedFile>,
) -> Result<(Vec<Analyzed>, Stats)> {
    let root_ids = roots
        .iter()
        .map(|r| cache.root_id(&r.path, r.role == Role::Reference))
        .collect::<Result<Vec<_>>>()?;
    let mut stats = Stats {
        files: files.len(),
        ..Stats::default()
    };
    let mut done = Vec::with_capacity(files.len());
    let mut todo = Vec::new();
    for f in files {
        match cache.lookup(root_ids[f.root], &f.rel, f.size, f.mtime_ns)? {
            Some(a) => {
                stats.cached += 1;
                done.push(Analyzed {
                    file: f,
                    analysis: a,
                });
            }
            None => todo.push(f),
        }
    }
    tracing::info!("{} cached, {} to analyse", stats.cached, todo.len());

    for chunk in todo.chunks(STORE_CHUNK) {
        let results: Vec<_> = chunk
            .par_iter()
            .filter_map(|f| match hash::analyze_path(&f.path) {
                Ok(a) => Some(Analyzed {
                    file: f.clone(),
                    analysis: a,
                }),
                Err(e) => {
                    tracing::warn!("skipping {e}");
                    None
                }
            })
            .collect();
        stats.unreadable += chunk.len() - results.len();
        stats.analyzed += results.len();
        let exts: Vec<String> = results
            .iter()
            .map(|r| r.file.format.label().to_ascii_lowercase())
            .collect();
        let entries: Vec<_> = results
            .iter()
            .zip(&exts)
            .map(|(r, ext)| cache::Entry {
                root_id: root_ids[r.file.root],
                rel: &r.file.rel,
                ext,
                size: r.file.size,
                mtime_ns: r.file.mtime_ns,
                analysis: &r.analysis,
            })
            .collect();
        cache.store_all(&entries)?;
        done.extend(results);
        tracing::info!(
            "analysed {}/{}",
            stats.analyzed + stats.unreadable,
            todo.len()
        );
    }
    stats.undecodable = done.iter().filter(|a| a.analysis.hashes.is_empty()).count();
    done.sort_by(|a, b| a.file.path.cmp(&b.file.path));
    Ok((done, stats))
}

pub fn items(roots: &[Root], analyzed: &[Analyzed]) -> Vec<Item> {
    analyzed
        .iter()
        .map(|a| Item {
            role: roots[a.file.root].role,
            format: a.file.format,
            blake3: a.analysis.blake3,
            image_data_hash: a.analysis.image_data_hash,
            hashes: a.analysis.hashes.clone(),
            pixels: u64::from(a.analysis.width.unwrap_or(0))
                * u64::from(a.analysis.height.unwrap_or(0)),
        })
        .collect()
}
