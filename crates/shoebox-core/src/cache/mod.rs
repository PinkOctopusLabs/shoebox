//! SQLite cache of per-file analysis, keyed by (root, relative path, size, mtime).

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::changes::Change;
use crate::error::{CoreError, Result, io};
use crate::exif::ExifInfo;
use crate::hash::{ANALYZER_VERSION, Analysis, Digest, PHashes};
use crate::marks::{Label, Marks, Pick};
use crate::meta::{META_VERSION, Meta};
use crate::sidecar::SidecarDate;

/// A root's cached files by relative path.
pub type KnownFiles = HashMap<String, Known>;

/// One cached file: what was on disk when it was last read.
#[derive(Debug, Clone)]
pub struct Known {
    pub size: u64,
    pub mtime_ns: i64,
    pub meta: Meta,
    pub marks: Option<CachedMarks>,
    /// A change not yet seen.
    pub change: Option<Change>,
}

/// Marks as cached: the marks, the per-file sidecar, and a signature of the
/// sidecars present when they were read.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedMarks {
    pub marks: Marks,
    pub sidecar: Option<std::path::PathBuf>,
    pub sig: String,
    /// The sidecars' date, shown in place of the file's own (which `exif`
    /// keeps).
    pub date: Option<SidecarDate>,
}

fn pick_str(p: Option<Pick>) -> Option<&'static str> {
    p.map(|p| match p {
        Pick::Pick => "pick",
        Pick::Reject => "reject",
    })
}

fn pick_from(s: Option<String>) -> Option<Pick> {
    match s.as_deref() {
        Some("pick") => Some(Pick::Pick),
        Some("reject") => Some(Pick::Reject),
        _ => None,
    }
}

fn label_from(s: Option<String>) -> Option<Label> {
    s.and_then(|s| Label::ALL.into_iter().find(|l| l.name() == s))
}

const SCHEMA_VERSION: i64 = 10;

/// Rebuildable tables, dropped when the schema version changes.
/// `dupe_ignores` holds user decisions and is never dropped.
const REBUILDABLE: &[&str] = &["marks", "phash", "exif", "files", "roots"];

pub struct Cache {
    conn: Connection,
}

/// Which edit: a photo (its root's path and its path within it) and copy.
#[derive(Debug, Clone, Copy)]
pub struct EditKey<'a> {
    pub root: &'a str,
    pub rel: &'a str,
    pub copy: &'a str,
}

/// A file exported from a photo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRow {
    /// The photo's path.
    pub photo: String,
    pub copy: String,
    /// The exported file.
    pub path: String,
    pub preset_name: String,
    pub preset_json: String,
    /// The edit it was made from (None: unedited).
    pub edit_key: Option<String>,
    /// When (ms since the epoch).
    pub created_at: i64,
}

/// An edit's working copy in the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedEdit {
    pub json: String,
    pub hash: String,
    /// When it last changed, and when it was written to its file (ns).
    pub updated_ns: i64,
    pub saved_ns: Option<i64>,
}

/// Browsing metadata for one file, ready to be written to the cache.
pub struct MetaEntry<'a> {
    pub root_id: i64,
    pub rel: &'a str,
    pub ext: &'a str,
    pub size: u64,
    pub mtime_ns: i64,
    pub meta: &'a Meta,
    pub marks: &'a CachedMarks,
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

    /// Open for reading only, beside the library's own connection (Find
    /// dates' thread): nothing is created or upgraded, and a cache from
    /// another schema is refused.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != SCHEMA_VERSION {
            return Err(CoreError::Invalid(format!(
                "cache schema {version}, not {SCHEMA_VERSION}"
            )));
        }
        Ok(Self { conn })
    }

    /// A root's id, if the cache knows it (unlike [`Self::root_id`], never
    /// writes).
    pub fn find_root(&self, path: &Path) -> Result<Option<i64>> {
        let Some(p) = path.to_str() else {
            return Ok(None);
        };
        Ok(self
            .conn
            .query_row("SELECT id FROM roots WHERE path = ?1", [p], |r| r.get(0))
            .optional()?)
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
        // 8 and 9 add columns, 10 re-reads some rows; a rebuild would throw away every hash, and
        // what's known of cloud-only files. Rows from before 9 have the
        // sidecars' dates in `exif` (they stay until the file is read again).
        if version == 7 {
            conn.execute_batch("ALTER TABLE exif ADD COLUMN nef_compression INTEGER")?;
        }
        if version == 7 || version == 8 {
            conn.execute_batch("ALTER TABLE marks ADD COLUMN date TEXT")?;
        }
        if (7..=9).contains(&version) {
            // Before 10, hashing a movie for duplicates cached it without its
            // date: read those movies' metadata again.
            conn.execute_batch(
                "UPDATE files SET meta_version = NULL
                 WHERE lower(ext) IN ('mov', 'mp4', 'mpg')
                   AND id IN (SELECT file_id FROM exif WHERE capture_time IS NULL)",
            )?;
        } else if version != 0 && version < SCHEMA_VERSION {
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
                        e.focal, e.fnumber, e.exposure, e.iso, e.orientation, e.gps_lat, e.gps_lon,
                        e.shutter_count, e.nef_compression
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
                            gps: r
                                .get::<_, Option<f64>>(19)?
                                .zip(r.get::<_, Option<f64>>(20)?),
                            shutter_count: r.get(21)?,
                            nef_compression: r.get(22)?,
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
                    serial, lens, focal, fnumber, exposure, iso, width, height, orientation,
                    gps_lat, gps_lon, shutter_count, nef_compression)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                         ?18, ?19)",
            )?;
            let mut marks = tx.prepare_cached(
                "INSERT OR REPLACE INTO marks(file_id, rating, pick, label, keywords, sidecar, sidecar_sig, event,
                    gps_lat_e7, gps_lon_e7, date)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for e in entries {
                // The file's own date; a sidecar's goes with the marks.
                let own = e.meta.file_meta();
                let m = &own;
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
                    x.gps.map(|g| g.0),
                    x.gps.map(|g| g.1),
                    x.shutter_count,
                    x.nef_compression,
                ])?;
                let k = e.marks;
                marks.execute(params![
                    id,
                    k.marks.rating,
                    pick_str(k.marks.pick),
                    k.marks.label.map(Label::name),
                    k.marks.keywords.join("\n"),
                    k.sidecar.as_ref().and_then(|p| p.to_str()),
                    k.sig,
                    k.marks.event,
                    k.marks.gps_e7.map(|g| g.0),
                    k.marks.gps_e7.map(|g| g.1),
                    k.date.as_ref().map(SidecarDate::to_xmp),
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Update just the marks of one file (after Shoebox writes its sidecar).
    pub fn update_marks(&self, root_id: i64, rel: &str, m: &CachedMarks) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO marks(file_id, rating, pick, label, keywords, sidecar, sidecar_sig, event,
                gps_lat_e7, gps_lon_e7, date)
             SELECT id, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
             FROM files WHERE root_id = ?1 AND rel_path = ?2",
            params![
                root_id,
                rel,
                m.marks.rating,
                pick_str(m.marks.pick),
                m.marks.label.map(Label::name),
                m.marks.keywords.join("\n"),
                m.sidecar.as_ref().and_then(|p| p.to_str()),
                m.sig,
                m.marks.event,
                m.marks.gps_e7.map(|g| g.0),
                m.marks.gps_e7.map(|g| g.1),
                m.date.as_ref().map(SidecarDate::to_xmp),
            ],
        )?;
        Ok(())
    }

    /// Every file of a root with current metadata: `rel → (size, mtime, meta)`.
    pub fn load_meta(&self, root_id: i64) -> Result<KnownFiles> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.rel_path, f.size, f.mtime, f.width, f.height,
                    e.capture_time, e.subsec, e.offset, e.make, e.model, e.serial, e.lens,
                    e.focal, e.fnumber, e.exposure, e.iso, e.orientation,
                    k.rating, k.pick, k.label, k.keywords, k.sidecar, k.sidecar_sig, k.event,
                    f.changed_at, f.change_source, e.gps_lat, e.gps_lon, e.shutter_count,
                    k.gps_lat_e7, k.gps_lon_e7, e.nef_compression, k.date
             FROM files f LEFT JOIN exif e ON e.file_id = f.id
                          LEFT JOIN marks k ON k.file_id = f.id
             WHERE f.root_id = ?1 AND f.meta_version = ?2",
        )?;
        let rows = stmt.query_map(params![root_id, META_VERSION], |r| {
            let change = match (
                r.get::<_, Option<i64>>(24)?,
                r.get::<_, Option<String>>(25)?,
            ) {
                (Some(at), Some(src)) => Change::from_source(at, &src),
                _ => None,
            };
            Ok((
                r.get::<_, String>(0)?,
                Known {
                    size: r.get::<_, i64>(1)? as u64,
                    mtime_ns: r.get::<_, i64>(2)?,
                    meta: Meta {
                        dated: None,
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
                            gps: r
                                .get::<_, Option<f64>>(26)?
                                .zip(r.get::<_, Option<f64>>(27)?),
                            shutter_count: r.get(28)?,
                            nef_compression: r.get(31)?,
                        },
                    },
                    marks: match r.get::<_, Option<u8>>(17)? {
                        Some(rating) => Some(CachedMarks {
                            marks: Marks {
                                rating,
                                pick: pick_from(r.get(18)?),
                                label: label_from(r.get(19)?),
                                keywords: r
                                    .get::<_, String>(20)?
                                    .split('\n')
                                    .filter(|k| !k.is_empty())
                                    .map(str::to_owned)
                                    .collect(),
                                event: r.get(23)?,
                                gps_e7: r
                                    .get::<_, Option<i32>>(29)?
                                    .zip(r.get::<_, Option<i32>>(30)?),
                            },
                            sidecar: r.get::<_, Option<String>>(21)?.map(Into::into),
                            sig: r.get(22)?,
                            date: r
                                .get::<_, Option<String>>(32)?
                                .and_then(|d| SidecarDate::parse(&d)),
                        }),
                        None => None,
                    },
                    change,
                },
            ))
        })?;
        let mut out: KnownFiles = rows.collect::<Result<_, _>>()?;
        for k in out.values_mut() {
            let date = k.marks.as_ref().and_then(|m| m.date.as_ref());
            k.meta.set_sidecar_date(date);
        }
        Ok(out)
    }

    /// A file Shoebox moved: carry its row (metadata, hashes, marks, unseen
    /// change) to the new path instead of reading it again.
    pub fn move_file(&self, from: (i64, &str), to: (i64, &str)) -> Result<()> {
        self.conn.execute(
            "UPDATE OR REPLACE files SET root_id = ?3, rel_path = ?4
             WHERE root_id = ?1 AND rel_path = ?2",
            params![from.0, from.1, to.0, to.1],
        )?;
        Ok(())
    }

    /// Store the working copy of an edit, not yet saved to its file.
    pub fn put_edit(&self, key: &EditKey<'_>, json: &str, hash: &str, at_ns: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO edits(root, rel_path, copy_id, doc_json, doc_hash, updated_at, saved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
             ON CONFLICT(root, rel_path, copy_id) DO UPDATE SET
               doc_json = excluded.doc_json, doc_hash = excluded.doc_hash,
               updated_at = excluded.updated_at, saved_at = NULL",
            params![key.root, key.rel, key.copy, json, hash, at_ns],
        )?;
        Ok(())
    }

    pub fn edit(&self, key: &EditKey<'_>) -> Result<Option<CachedEdit>> {
        Ok(self
            .conn
            .query_row(
                "SELECT doc_json, doc_hash, updated_at, saved_at FROM edits
                 WHERE root = ?1 AND rel_path = ?2 AND copy_id = ?3",
                params![key.root, key.rel, key.copy],
                |r| {
                    Ok(CachedEdit {
                        json: r.get(0)?,
                        hash: r.get(1)?,
                        updated_ns: r.get(2)?,
                        saved_ns: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// The edit as written to its file at `at_ns` (only if it hasn't changed
    /// since `updated_ns`).
    pub fn mark_edit_saved(&self, key: &EditKey<'_>, updated_ns: i64, at_ns: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE edits SET saved_at = ?5
             WHERE root = ?1 AND rel_path = ?2 AND copy_id = ?3 AND updated_at = ?4",
            params![key.root, key.rel, key.copy, updated_ns, at_ns],
        )?;
        Ok(())
    }

    /// Edits changed since they were last written to their files:
    /// `(root, rel, copy)`.
    pub fn unsaved_edits(&self) -> Result<Vec<(String, String, String)>> {
        let mut st = self.conn.prepare_cached(
            "SELECT root, rel_path, copy_id FROM edits WHERE saved_at IS NULL ORDER BY updated_at",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Record a file exported from `photo` (replacing any record of the
    /// same file).
    pub fn add_export(&self, e: &ExportRow) -> Result<()> {
        // A file replaced by a new export is one export, not two.
        self.conn
            .execute("DELETE FROM exports WHERE path = ?1", params![e.path])?;
        self.conn.execute(
            "INSERT INTO exports(photo, copy_id, path, preset_name, preset_json, edit_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![e.photo, e.copy, e.path, e.preset_name, e.preset_json, e.edit_key, e.created_at],
        )?;
        Ok(())
    }

    /// A photo's exports, newest first.
    pub fn exports_of(&self, photo: &str) -> Result<Vec<ExportRow>> {
        let mut st = self.conn.prepare_cached(
            "SELECT photo, copy_id, path, preset_name, preset_json, edit_key, created_at
             FROM exports WHERE photo = ?1 ORDER BY created_at DESC",
        )?;
        let rows = st.query_map([photo], |r| {
            Ok(ExportRow {
                photo: r.get(0)?,
                copy: r.get(1)?,
                path: r.get(2)?,
                preset_name: r.get(3)?,
                preset_json: r.get(4)?,
                edit_key: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A photo Shoebox moved keeps its exports.
    pub fn move_exports(&self, from: &str, to: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE exports SET photo = ?2 WHERE photo = ?1",
            params![from, to],
        )?;
        Ok(())
    }

    /// Every edit's working copy: `(root, rel, copy, json)`.
    pub fn edits(&self) -> Result<Vec<(String, String, String, String)>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT root, rel_path, copy_id, doc_json FROM edits")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Forget an edit's row (a virtual copy moved to the Trash).
    pub fn remove_edit(&self, key: &EditKey<'_>) -> Result<()> {
        self.conn.execute(
            "DELETE FROM edits WHERE root = ?1 AND rel_path = ?2 AND copy_id = ?3",
            params![key.root, key.rel, key.copy],
        )?;
        Ok(())
    }

    /// The copy ids of a photo's edits in the catalog.
    pub fn edit_copies(&self, root: &str, rel: &str) -> Result<Vec<String>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT copy_id FROM edits WHERE root = ?1 AND rel_path = ?2")?;
        let rows = st.query_map(params![root, rel], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// A photo Shoebox moved takes its edits' rows along.
    pub fn move_edits(&self, from: (&str, &str), to: (&str, &str)) -> Result<()> {
        self.conn.execute(
            "UPDATE OR REPLACE edits SET root = ?3, rel_path = ?4
             WHERE root = ?1 AND rel_path = ?2",
            params![from.0, from.1, to.0, to.1],
        )?;
        Ok(())
    }

    /// Record a change to a file (merged into any unseen one).
    pub fn record_change(&self, root_id: i64, rel: &str, c: &Change) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET changed_at = ?3, change_source = ?4
             WHERE root_id = ?1 AND rel_path = ?2",
            params![root_id, rel, c.at_ms, c.source()],
        )?;
        Ok(())
    }

    /// Clear the unseen change of these files.
    pub fn mark_seen(&mut self, files: &[(i64, &str)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "UPDATE files SET changed_at = NULL, change_source = NULL, seen_at = ?3
                 WHERE root_id = ?1 AND rel_path = ?2",
            )?;
            let now = crate::events::now_ms();
            for (root, rel) in files {
                st.execute(params![root, rel, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remember that two files (by content) are not duplicates.
    pub fn ignore_pairs(&mut self, pairs: &[(Digest, Digest)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT OR IGNORE INTO dupe_ignores(hash_a, hash_b) VALUES (?1, ?2)",
            )?;
            for (a, b) in pairs {
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                st.execute(params![&a[..], &b[..]])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Every dismissed pair, smaller hash first.
    pub fn ignored_pairs(&self) -> Result<std::collections::HashSet<(Digest, Digest)>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT hash_a, hash_b FROM dupe_ignores")?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            let (a, b) = row?;
            out.insert((digest(a)?, digest(b)?));
        }
        Ok(out)
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
                    serial, lens, focal, fnumber, exposure, iso, width, height, orientation,
                    gps_lat, gps_lon, shutter_count, nef_compression)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                         ?18, ?19)",
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
                    x.gps.map(|g| g.0),
                    x.gps.map(|g| g.1),
                    x.shutter_count,
                    x.nef_compression,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Schema 7 → 8 adds a column in place: hashes survive the upgrade.
    #[test]
    fn schema_7_upgrades_without_a_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(include_str!("schema.sql")).unwrap();
            conn.execute_batch(
                "ALTER TABLE exif DROP COLUMN nef_compression;
                 ALTER TABLE marks DROP COLUMN date;
                 INSERT INTO roots(id, path) VALUES (1, '/photos');
                 INSERT INTO files(id, root_id, rel_path, ext, size, mtime, blake3)
                   VALUES (1, 1, 'a.nef', 'nef', 10, 20, x'00ff');
                 INSERT INTO exif(file_id, make) VALUES (1, 'NIKON');",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 7).unwrap();
        }
        let cache = Cache::open(&path).unwrap();
        let (blake3, compression): (Vec<u8>, Option<u16>) = cache
            .conn
            .query_row(
                "SELECT f.blake3, e.nef_compression FROM files f JOIN exif e ON e.file_id = f.id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((blake3, compression), (vec![0x00, 0xff], None));
        drop(cache);
        // And opening it again is a no-op.
        Cache::open(&path).unwrap();
    }

    /// Schema 8 → 9 adds the sidecars' date to the marks in place.
    #[test]
    fn schema_8_upgrades_without_a_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(include_str!("schema.sql")).unwrap();
            conn.execute_batch(
                "ALTER TABLE marks DROP COLUMN date;
                 INSERT INTO roots(id, path) VALUES (1, '/photos');
                 INSERT INTO files(id, root_id, rel_path, ext, size, mtime, blake3, meta_version)
                   VALUES (1, 1, 'a.jpg', 'jpg', 10, 20, x'00ff', 4);
                 INSERT INTO exif(file_id, capture_time) VALUES (1, '2019:03:12 10:15:30');
                 INSERT INTO marks(file_id, rating, keywords, sidecar_sig) VALUES (1, 3, '', 's');",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 8).unwrap();
        }
        let cache = Cache::open(&path).unwrap();
        let known = cache.load_meta(1).unwrap();
        let a = &known["a.jpg"];
        assert_eq!(
            a.meta.exif.capture_time.as_deref(),
            Some("2019:03:12 10:15:30")
        );
        let marks = a.marks.as_ref().unwrap();
        assert_eq!((marks.marks.rating, marks.date.as_ref()), (3, None));
        let blake3: Vec<u8> = cache
            .conn
            .query_row("SELECT blake3 FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(blake3, vec![0x00, 0xff]);
    }

    /// Schema 9 → 10: movies cached without a date (hashing them dropped
    /// it) are read again; their hashes, and every other row, stay.
    #[test]
    fn schema_9_reads_undated_movies_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(include_str!("schema.sql")).unwrap();
            conn.execute_batch(
                "INSERT INTO roots(id, path) VALUES (1, '/photos');
                 INSERT INTO files(id, root_id, rel_path, ext, size, mtime, blake3, meta_version)
                   VALUES (1, 1, 'a.mov', 'mov', 10, 20, x'00ff', 4),
                          (2, 1, 'b.mov', 'mov', 10, 20, x'00ff', 4),
                          (3, 1, 'c.png', 'png', 10, 20, x'00ff', 4);
                 INSERT INTO exif(file_id, capture_time) VALUES
                   (1, NULL), (2, '2019:03:12 10:15:30'), (3, NULL);",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 9).unwrap();
        }
        let cache = Cache::open(&path).unwrap();
        let mut known: Vec<String> = cache.load_meta(1).unwrap().into_keys().collect();
        known.sort();
        assert_eq!(known, ["b.mov", "c.png"]);
        let blake3: Vec<u8> = cache
            .conn
            .query_row("SELECT blake3 FROM files WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(blake3, vec![0x00, 0xff]);
    }
}
