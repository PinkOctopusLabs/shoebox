//! SQLite cache of per-file analysis, keyed by (root, relative path, size, mtime).

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{CoreError, Result, io};
use crate::exif::ExifInfo;
use crate::hash::{ANALYZER_VERSION, Analysis, Digest, PHashes};
use crate::meta::{META_VERSION, Meta};

const SCHEMA_VERSION: i64 = 3;

/// Rebuildable tables, dropped when the schema version changes.
/// `dupe_ignores` holds user decisions and is never dropped.
const REBUILDABLE: &[&str] = &["phash", "exif", "files", "roots"];

pub struct Cache {
    conn: Connection,
}

/// Browsing metadata for one file, ready to be written to the cache.
pub struct MetaEntry<'a> {
    pub root_id: i64,
    pub rel: &'a str,
    pub ext: &'a str,
    pub size: u64,
    pub mtime_ns: i64,
    pub meta: &'a Meta,
}

/// One analysed file ready to be written to the cache.
pub struct Entry<'a> {
    pub root_id: i64,
    pub rel: &'a str,
    pub ext: &'a str,
    pub size: u64,
    pub mtime_ns: i64,
    pub analysis: &'a Analysis,
}

fn digest(v: Vec<u8>) -> Result<Digest> {
    v.try_into()
        .map_err(|_| CoreError::Invalid("cache holds a malformed digest".into()))
}

impl Cache {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(CoreError::Invalid(format!(
                "cache schema {version} is newer than this build ({SCHEMA_VERSION})"
            )));
        }
        if version != 0 && version < SCHEMA_VERSION {
            tracing::info!("cache schema {version} is out of date; rebuilding");
            for t in REBUILDABLE {
                conn.execute_batch(&format!("DROP TABLE IF EXISTS {t}"))?;
            }
        }
        conn.execute_batch(include_str!("schema.sql"))?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
    }

    pub fn root_id(&self, path: &Path, is_reference: bool) -> Result<i64> {
        let p = path
            .to_str()
            .ok_or_else(|| CoreError::Invalid(format!("non-UTF-8 root {}", path.display())))?;
        self.conn.execute(
            "INSERT INTO roots(path, is_reference) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET is_reference = excluded.is_reference",
            params![p, is_reference],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM roots WHERE path = ?1", [p], |r| r.get(0))?)
    }

    /// Cached analysis if the file is unchanged since it was analysed.
    pub fn lookup(
        &self,
        root_id: i64,
        rel: &str,
        size: u64,
        mtime_ns: i64,
    ) -> Result<Option<Analysis>> {
        let row = self
            .conn
            .query_row(
                "SELECT f.id, f.blake3, f.image_data_hash, f.width, f.height, f.analyze_error,
                        f.luma_stddev,
                        e.capture_time, e.subsec, e.offset, e.make, e.model, e.serial, e.lens,
                        e.focal, e.fnumber, e.exposure, e.iso, e.orientation
                 FROM files f LEFT JOIN exif e ON e.file_id = f.id
                 WHERE f.root_id = ?1 AND f.rel_path = ?2 AND f.size = ?3 AND f.mtime = ?4
                   AND f.analyzer_version = ?5",
                params![root_id, rel, size as i64, mtime_ns, ANALYZER_VERSION],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, Option<Vec<u8>>>(2)?,
                        r.get::<_, Option<u32>>(3)?,
                        r.get::<_, Option<u32>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<f32>>(6)?,
                        ExifInfo {
                            capture_time: r.get(7)?,
                            subsec: r.get(8)?,
                            offset: r.get(9)?,
                            make: r.get(10)?,
                            model: r.get(11)?,
                            serial: r.get(12)?,
                            lens: r.get(13)?,
                            focal: r.get(14)?,
                            fnumber: r.get(15)?,
                            exposure: r.get(16)?,
                            iso: r.get(17)?,
                            orientation: r.get(18)?,
                        },
                    ))
                },
            )
            .optional()?;
        let Some((id, b3, idh, width, height, error, luma_stddev, exif)) = row else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT dhash, phash FROM phash WHERE file_id = ?1 ORDER BY rotation",
        )?;
        let hashes = stmt
            .query_map([id], |r| {
                Ok(PHashes {
                    dhash: r.get::<_, i64>(0)? as u64,
                    phash: r.get::<_, i64>(1)? as u64,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(Analysis {
            blake3: digest(b3)?,
            image_data_hash: idh.map(digest).transpose()?,
            width,
            height,
            exif,
            hashes,
            luma_stddev,
            error,
        }))
    }

    /// Store browsing metadata. If a file's size or mtime changed, its hashes
    /// are cleared (they describe the old contents); otherwise they're kept.
    pub fn store_meta(&mut self, entries: &[MetaEntry<'_>]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut file = tx.prepare_cached(
                "INSERT INTO files(root_id, rel_path, ext, size, mtime, width, height, meta_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(root_id, rel_path) DO UPDATE SET
                   ext = excluded.ext, width = excluded.width, height = excluded.height,
                   meta_version = excluded.meta_version,
                   blake3 = CASE WHEN size = excluded.size AND mtime = excluded.mtime
                                 THEN blake3 ELSE NULL END,
                   image_data_hash = CASE WHEN size = excluded.size AND mtime = excluded.mtime
                                 THEN image_data_hash ELSE NULL END,
                   analyzer_version = CASE WHEN size = excluded.size AND mtime = excluded.mtime
                                 THEN analyzer_version ELSE NULL END,
                   size = excluded.size, mtime = excluded.mtime
                 RETURNING id",
            )?;
            let mut exif = tx.prepare_cached(
                "INSERT OR REPLACE INTO exif(file_id, capture_time, subsec, offset, make, model,
                    serial, lens, focal, fnumber, exposure, iso, width, height, orientation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            for e in entries {
                let m = e.meta;
                let id: i64 = file.query_row(
                    params![
                        e.root_id,
                        e.rel,
                        e.ext,
                        e.size as i64,
                        e.mtime_ns,
                        m.width,
                        m.height,
                        META_VERSION
                    ],
                    |r| r.get(0),
                )?;
                let x = &m.exif;
                exif.execute(params![
                    id,
                    x.capture_time,
                    x.subsec,
                    x.offset,
                    x.make,
                    x.model,
                    x.serial,
                    x.lens,
                    x.focal,
                    x.fnumber,
                    x.exposure,
                    x.iso,
                    m.width,
                    m.height,
                    x.orientation,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every file of a root with current metadata: `rel → (size, mtime, meta)`.
    pub fn load_meta(&self, root_id: i64) -> Result<HashMap<String, (u64, i64, Meta)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.rel_path, f.size, f.mtime, f.width, f.height,
                    e.capture_time, e.subsec, e.offset, e.make, e.model, e.serial, e.lens,
                    e.focal, e.fnumber, e.exposure, e.iso, e.orientation
             FROM files f LEFT JOIN exif e ON e.file_id = f.id
             WHERE f.root_id = ?1 AND f.meta_version = ?2",
        )?;
        let rows = stmt.query_map(params![root_id, META_VERSION], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)?,
                    Meta {
                        width: r.get(3)?,
                        height: r.get(4)?,
                        exif: ExifInfo {
                            capture_time: r.get(5)?,
                            subsec: r.get(6)?,
                            offset: r.get(7)?,
                            make: r.get(8)?,
                            model: r.get(9)?,
                            serial: r.get(10)?,
                            lens: r.get(11)?,
                            focal: r.get(12)?,
                            fnumber: r.get(13)?,
                            exposure: r.get(14)?,
                            iso: r.get(15)?,
                            orientation: r.get(16)?,
                        },
                    },
                ),
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Drop cache rows for files of `root_id` that are no longer on disk.
    /// (Cache rows only; nothing in the library is touched.)
    pub fn forget_missing(
        &mut self,
        root_id: i64,
        present: &std::collections::HashSet<String>,
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let gone: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id, rel_path FROM files WHERE root_id = ?1")?;
            let rows = stmt.query_map([root_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            rows.filter_map(|r| r.ok())
                .filter(|(_, rel)| !present.contains(rel))
                .map(|(id, _)| id)
                .collect()
        };
        {
            let mut del = tx.prepare("DELETE FROM files WHERE id = ?1")?;
            for id in &gone {
                del.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(gone.len())
    }

    /// Write many entries in one transaction.
    pub fn store_all(&mut self, entries: &[Entry<'_>]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut file = tx.prepare_cached(
                "INSERT INTO files(root_id, rel_path, ext, size, mtime, blake3, image_data_hash,
                                   width, height, analyzer_version, analyze_error, luma_stddev,
                                   meta_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(root_id, rel_path) DO UPDATE SET
                   ext = excluded.ext, size = excluded.size, mtime = excluded.mtime,
                   blake3 = excluded.blake3, image_data_hash = excluded.image_data_hash,
                   width = excluded.width, height = excluded.height,
                   analyzer_version = excluded.analyzer_version,
                   analyze_error = excluded.analyze_error,
                   luma_stddev = excluded.luma_stddev,
                   meta_version = excluded.meta_version
                 RETURNING id",
            )?;
            let mut exif = tx.prepare_cached(
                "INSERT OR REPLACE INTO exif(file_id, capture_time, subsec, offset, make, model,
                    serial, lens, focal, fnumber, exposure, iso, width, height, orientation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?;
            let mut clear = tx.prepare_cached("DELETE FROM phash WHERE file_id = ?1")?;
            let mut ph = tx.prepare_cached(
                "INSERT INTO phash(file_id, rotation, dhash, phash) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for e in entries {
                let a = e.analysis;
                let id: i64 = file.query_row(
                    params![
                        e.root_id,
                        e.rel,
                        e.ext,
                        e.size as i64,
                        e.mtime_ns,
                        &a.blake3[..],
                        a.image_data_hash.as_ref().map(|d| &d[..]),
                        a.width,
                        a.height,
                        ANALYZER_VERSION,
                        a.error,
                        a.luma_stddev,
                        META_VERSION,
                    ],
                    |r| r.get(0),
                )?;
                let x = &a.exif;
                exif.execute(params![
                    id,
                    x.capture_time,
                    x.subsec,
                    x.offset,
                    x.make,
                    x.model,
                    x.serial,
                    x.lens,
                    x.focal,
                    x.fnumber,
                    x.exposure,
                    x.iso,
                    a.width,
                    a.height,
                    x.orientation,
                ])?;
                clear.execute([id])?;
                for (rot, h) in a.hashes.iter().enumerate() {
                    ph.execute(params![id, rot as i64 * 90, h.dhash as i64, h.phash as i64])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}
