//! Stored image embeddings for semantic search, and the search over them.
//!
//! Embeddings live in their own database (`embeddings.sqlite`), not the
//! cache: they take minutes to hours to compute, and a cache rebuild
//! shouldn't throw them away. Like the cache, everything here can be
//! recomputed from the files.
//!
//! Rows are keyed by path and checked against the file's size and mtime. A
//! file Shoebox (or anything else) moved or renamed keeps its size and mtime,
//! so [`Store::sync`] gives an orphaned row with the same size and mtime to
//! the new path instead of recomputing it.
//!
//! Vectors are stored and searched as 8-bit integers with one scale per
//! vector (a quarter of the size of floats, and indistinguishable for
//! ranking). A search compares the query with every vector: at 200,000
//! photos that's a few tens of milliseconds, so no approximate index.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{CoreError, Result, io};

/// 2: rows keyed by (kind, path), so image embeddings and faces share it.
const SCHEMA_VERSION: i64 = 2;

/// One embedding, quantised.
#[derive(Debug, Clone, PartialEq)]
pub struct Vector {
    pub scale: f32,
    pub q: Vec<i8>,
}

impl Vector {
    /// Quantise a (unit-length) float vector.
    pub fn quantise(v: &[f32]) -> Self {
        let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
        if max == 0.0 {
            return Self {
                scale: 0.0,
                q: vec![0; v.len()],
            };
        }
        let scale = max / 127.0;
        Self {
            scale,
            q: v.iter().map(|x| (x / scale).round() as i8).collect(),
        }
    }

    /// Dot product with another quantised vector (≈ cosine similarity for
    /// unit vectors).
    pub fn dot(&self, other: &Vector) -> f32 {
        let s: i32 = self
            .q
            .iter()
            .zip(&other.q)
            .map(|(&a, &b)| i32::from(a) * i32::from(b))
            .sum();
        s as f32 * self.scale * other.scale
    }

    fn to_blob(&self) -> Vec<u8> {
        self.q.iter().map(|&x| x as u8).collect()
    }

    fn from_blob(scale: f32, b: &[u8]) -> Self {
        Self {
            scale,
            q: b.iter().map(|&x| x as i8).collect(),
        }
    }

    /// As a stored row.
    pub fn payload(&self) -> Payload {
        Payload {
            scale: self.scale,
            data: self.to_blob(),
        }
    }

    pub fn from_payload(p: &Payload) -> Self {
        Self::from_blob(p.scale, &p.data)
    }
}

/// What a row holds: a vector's scale and bytes, or (faces) anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    pub scale: f32,
    pub data: Vec<u8>,
}

/// A file as the library sees it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileKey {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i64,
}

/// The embeddings of the files in the library, in memory.
#[derive(Debug, Default, Clone)]
pub struct Index {
    paths: Vec<PathBuf>,
    vectors: Vec<Vector>,
    by_path: HashMap<PathBuf, usize>,
}

impl Index {
    /// An index of stored vectors (rows of [`Store::sync`]).
    pub fn from_rows(rows: &[(PathBuf, Payload)]) -> Self {
        let mut ix = Index::default();
        for (p, r) in rows {
            ix.insert(p.clone(), Vector::from_payload(r));
        }
        ix
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.by_path.contains_key(path)
    }

    pub fn get(&self, path: &Path) -> Option<&Vector> {
        self.by_path.get(path).map(|&i| &self.vectors[i])
    }

    pub fn insert(&mut self, path: PathBuf, v: Vector) {
        match self.by_path.get(&path) {
            Some(&i) => self.vectors[i] = v,
            None => {
                self.by_path.insert(path.clone(), self.paths.len());
                self.paths.push(path);
                self.vectors.push(v);
            }
        }
    }

    /// Keep only these paths.
    pub fn retain(&mut self, keep: &HashSet<&Path>) {
        let mut paths = Vec::new();
        let mut vectors = Vec::new();
        for (p, v) in self.paths.drain(..).zip(self.vectors.drain(..)) {
            if keep.contains(p.as_path()) {
                paths.push(p);
                vectors.push(v);
            }
        }
        self.by_path = paths
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, p)| (p, i))
            .collect();
        self.paths = paths;
        self.vectors = vectors;
    }

    /// Every file scoring at least `min`, best first, with its score.
    pub fn search(&self, query: &Vector, min: f32) -> Vec<(PathBuf, f32)> {
        let mut hits: Vec<(usize, f32)> = self
            .vectors
            .par_iter()
            .enumerate()
            .map(|(i, v)| (i, v.dot(query)))
            .filter(|&(_, s)| s >= min)
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.into_iter()
            .map(|(i, s)| (self.paths[i].clone(), s))
            .collect()
    }

    /// Suggested tags for every file: for each, up to [`TagRule::max`]
    /// labels (index into `labels`, score), best first. See [`TagRule`].
    pub fn tag(&self, labels: &[Vector], rule: &TagRule) -> Vec<(PathBuf, Vec<(u16, f32)>)> {
        // Each label's score on photos without that subject: its 25th
        // percentile over the library (a sample of it).
        let baseline: Vec<f32> = if self.len() >= rule.min_library {
            let step = (self.len() / 20_000).max(1);
            let sample: Vec<&Vector> = self.vectors.iter().step_by(step).collect();
            labels
                .par_iter()
                .map(|l| {
                    let mut c: Vec<f32> = sample.iter().map(|v| v.dot(l)).collect();
                    c.sort_by(f32::total_cmp);
                    c[c.len() / 4]
                })
                .collect()
        } else {
            vec![0.0; labels.len()]
        };
        self.vectors
            .par_iter()
            .enumerate()
            .filter_map(|(i, v)| {
                let c: Vec<f32> = labels.iter().map(|l| v.dot(l)).collect();
                // This photo's score on labels in general.
                let mut sorted = c.clone();
                sorted.sort_by(f32::total_cmp);
                let typical = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
                let lift: Vec<f32> = c
                    .iter()
                    .zip(&baseline)
                    .map(|(c, b)| c - b - typical)
                    .collect();
                let best = lift.iter().copied().fold(f32::MIN, f32::max);
                let mut tags: Vec<(u16, f32, f32)> = (0..labels.len())
                    .filter(|&j| {
                        c[j] >= rule.min_score
                            && lift[j] >= rule.min_lift
                            && lift[j] >= best - rule.margin
                    })
                    .map(|j| (j as u16, c[j], lift[j]))
                    .collect();
                if tags.is_empty() {
                    return None;
                }
                tags.sort_by(|a, b| b.2.total_cmp(&a.2));
                tags.truncate(rule.max);
                Some((
                    self.paths[i].clone(),
                    tags.into_iter().map(|(j, c, _)| (j, c)).collect(),
                ))
            })
            .collect()
    }
}

/// When a photo gets a label as a suggested tag. Scores aren't comparable
/// across labels or across photos (with SigLIP, "stars" scores well on many
/// things, and some photos score well on every label), so a tag needs a
/// *lift*: its score minus the label's usual score on the library's photos
/// (25th percentile, so a subject in up to 3/4 of the library still
/// stands out) minus the photo's median score over all labels.
#[derive(Debug, Clone)]
pub struct TagRule {
    /// Score (cosine) a tag needs at least.
    pub min_score: f32,
    /// Lift a tag needs at least.
    pub min_lift: f32,
    /// Lift within this of the photo's best label.
    pub margin: f32,
    /// Tags per photo.
    pub max: usize,
    /// Below this many photos there's no per-label baseline (zero).
    pub min_library: usize,
}

/// Labels' vectors, kept so the text model isn't needed to tag.
impl Store {
    /// Stored vectors for these prompts, if all of them are stored.
    pub fn labels(&self, prompts: &[String]) -> Result<Option<Vec<Vector>>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT scale, vec FROM labels WHERE model = ?1 AND prompt = ?2")?;
        let mut out = Vec::with_capacity(prompts.len());
        for p in prompts {
            let row = st
                .query_row(params![self.model, p], |r| {
                    Ok((r.get::<_, f64>(0)?, r.get::<_, Vec<u8>>(1)?))
                })
                .optional()?;
            match row {
                Some((scale, vec)) => out.push(Vector::from_blob(scale as f32, &vec)),
                None => return Ok(None),
            }
        }
        Ok(Some(out))
    }

    pub fn put_labels(&mut self, labels: &[(String, Vector)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT OR REPLACE INTO labels(model, prompt, scale, vec) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (p, v) in labels {
                st.execute(params![self.model, p, f64::from(v.scale), v.to_blob()])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// Every row of `kind` from `model`, read through a read-only connection
/// beside the one the search or faces thread has (Find dates' look-alikes).
/// Nothing is created: a missing database reads as no rows.
pub fn read_rows(path: &Path, kind: &str, model: &str) -> Result<Vec<(PathBuf, Payload)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let mut st =
        conn.prepare("SELECT path, scale, vec FROM embeddings WHERE kind = ?1 AND model = ?2")?;
    let rows = st
        .query_map([kind, model], |r| {
            Ok((
                PathBuf::from(r.get::<_, String>(0)?),
                Payload {
                    scale: r.get::<_, f64>(1)? as f32,
                    data: r.get(2)?,
                },
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The embeddings database: one row per file and *kind* of analysis
/// ("image" embeddings, "faces"), each from a *model*.
pub struct Store {
    conn: Connection,
    kind: String,
    model: String,
}

/// What [`Store::sync`] found.
#[derive(Debug, Default)]
pub struct Sync {
    /// Files with an up-to-date row, and the row.
    pub found: Vec<(PathBuf, Payload)>,
    /// Files that need one, in the order given.
    pub missing: Vec<FileKey>,
    /// Rows given to a file's new path.
    pub followed: usize,
    /// Rows removed (their files are gone).
    pub removed: usize,
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| CoreError::Invalid(format!("non-UTF-8 path {}", p.display())))
}

impl Store {
    /// Open (or create) the database for rows of `kind` from `model`. Rows
    /// from other models of the kind are ignored, and replaced or removed
    /// by later syncs.
    pub fn open(path: &Path, kind: &str, model: &str) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        Self::init(Connection::open(path)?, kind, model)
    }

    pub fn open_in_memory(kind: &str, model: &str) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, kind, model)
    }

    fn init(conn: Connection, kind: &str, model: &str) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(CoreError::Invalid(format!(
                "embeddings schema {version} is newer than this build ({SCHEMA_VERSION})"
            )));
        }
        if version != 0 && version < SCHEMA_VERSION {
            // Rebuildable: an older layout is recomputed, not converted.
            conn.execute_batch("DROP TABLE IF EXISTS embeddings")?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS embeddings (
                 kind  TEXT NOT NULL,
                 path  TEXT NOT NULL,
                 size  INTEGER NOT NULL,
                 mtime INTEGER NOT NULL,
                 model TEXT NOT NULL,
                 scale REAL NOT NULL,
                 vec   BLOB NOT NULL,
                 PRIMARY KEY (kind, path)
             );
             CREATE INDEX IF NOT EXISTS embeddings_file ON embeddings(kind, size, mtime);
             CREATE TABLE IF NOT EXISTS labels (
                 model  TEXT NOT NULL,
                 prompt TEXT NOT NULL,
                 scale  REAL NOT NULL,
                 vec    BLOB NOT NULL,
                 PRIMARY KEY (model, prompt)
             );",
        )?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self {
            conn,
            kind: kind.to_owned(),
            model: model.to_owned(),
        })
    }

    /// Match the stored rows to the library's files. `complete`: `files` is
    /// the whole library, so rows matching none of them can go.
    pub fn sync(&mut self, files: &[FileKey], complete: bool) -> Result<Sync> {
        struct Row {
            path: String,
            size: u64,
            mtime: i64,
            model: String,
            scale: f32,
            vec: Vec<u8>,
        }
        let rows: Vec<Row> = {
            let mut st = self.conn.prepare(
                "SELECT path, size, mtime, model, scale, vec FROM embeddings WHERE kind = ?1",
            )?;
            st.query_map([&self.kind], |r| {
                Ok(Row {
                    path: r.get(0)?,
                    size: r.get::<_, i64>(1)? as u64,
                    mtime: r.get(2)?,
                    model: r.get(3)?,
                    scale: r.get::<_, f64>(4)? as f32,
                    vec: r.get(5)?,
                })
            })?
            .collect::<Result<_, _>>()?
        };
        let wanted: HashSet<&Path> = files.iter().map(|f| f.path.as_path()).collect();
        let by_path: HashMap<&str, &Row> = rows.iter().map(|r| (r.path.as_str(), r)).collect();
        // Rows of this model for paths no longer in the library, by (size, mtime).
        let mut orphans: HashMap<(u64, i64), Vec<&Row>> = HashMap::new();
        for r in &rows {
            if r.model == self.model && !wanted.contains(Path::new(&r.path)) {
                orphans.entry((r.size, r.mtime)).or_default().push(r);
            }
        }
        let mut out = Sync::default();
        let mut moves: Vec<(&str, &Path)> = Vec::new();
        let mut used: HashSet<&str> = HashSet::new();
        for f in files {
            let p = path_str(&f.path)?;
            if let Some(r) = by_path.get(p)
                && r.model == self.model
                && r.size == f.size
                && r.mtime == f.mtime_ns
            {
                used.insert(p);
                out.found.push((
                    f.path.clone(),
                    Payload {
                        scale: r.scale,
                        data: r.vec.clone(),
                    },
                ));
                continue;
            }
            let moved = orphans
                .get_mut(&(f.size, f.mtime_ns))
                .and_then(|v| v.pop())
                .filter(|_| !by_path.contains_key(p));
            match moved {
                Some(r) => {
                    used.insert(r.path.as_str());
                    moves.push((r.path.as_str(), &f.path));
                    out.found.push((
                        f.path.clone(),
                        Payload {
                            scale: r.scale,
                            data: r.vec.clone(),
                        },
                    ));
                }
                None => out.missing.push(f.clone()),
            }
        }
        let tx = self.conn.transaction()?;
        {
            let mut mv =
                tx.prepare_cached("UPDATE embeddings SET path = ?3 WHERE kind = ?1 AND path = ?2")?;
            for (from, to) in &moves {
                mv.execute(params![self.kind, from, path_str(to)?])?;
            }
            out.followed = moves.len();
            if complete {
                let mut del =
                    tx.prepare_cached("DELETE FROM embeddings WHERE kind = ?1 AND path = ?2")?;
                for r in &rows {
                    if !used.contains(r.path.as_str()) && !wanted.contains(Path::new(&r.path)) {
                        del.execute(params![self.kind, r.path])?;
                        out.removed += 1;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(out)
    }

    /// Store new rows (replacing any for the same path).
    pub fn put(&mut self, entries: &[(FileKey, Payload)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT OR REPLACE INTO embeddings(kind, path, size, mtime, model, scale, vec)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (f, v) in entries {
                st.execute(params![
                    self.kind,
                    path_str(&f.path)?,
                    f.size as i64,
                    f.mtime_ns,
                    self.model,
                    f64::from(v.scale),
                    v.data,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(p: &str, size: u64, mtime: i64) -> FileKey {
        FileKey {
            path: PathBuf::from(p),
            size,
            mtime_ns: mtime,
        }
    }

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn quantised_dot_is_close() {
        let a = unit(&[0.3, -0.2, 0.9, 0.1, -0.5]);
        let b = unit(&[0.1, -0.4, 0.7, 0.3, -0.2]);
        let exact: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        let q = Vector::quantise(&a).dot(&Vector::quantise(&b));
        assert!((exact - q).abs() < 0.01, "{exact} vs {q}");
        assert_eq!(
            Vector::quantise(&[0.0, 0.0]).dot(&Vector::quantise(&a[..2])),
            0.0
        );
    }

    #[test]
    fn search_ranks_and_thresholds() {
        let mut ix = Index::default();
        ix.insert("/a".into(), Vector::quantise(&unit(&[1.0, 0.0, 0.0])));
        ix.insert("/b".into(), Vector::quantise(&unit(&[0.8, 0.6, 0.0])));
        ix.insert("/c".into(), Vector::quantise(&unit(&[0.0, 0.0, 1.0])));
        let q = Vector::quantise(&unit(&[1.0, 0.1, 0.0]));
        let hits = ix.search(&q, 0.5);
        let names: Vec<_> = hits.iter().map(|(p, _)| p.to_str().unwrap()).collect();
        assert_eq!(names, ["/a", "/b"]);
        assert!(hits[0].1 > hits[1].1);
    }

    fn rule() -> TagRule {
        TagRule {
            min_score: 0.5,
            min_lift: 0.1,
            margin: 0.3,
            max: 2,
            min_library: 8,
        }
    }

    #[test]
    fn tags_need_lift_over_their_label_and_the_photo() {
        // Labels: x, y, and "z" which scores high on every photo.
        let labels: Vec<Vector> = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            .iter()
            .map(|v| Vector::quantise(v))
            .collect();
        let mut ix = Index::default();
        // Ten ordinary photos: mostly z, a little x or y.
        for i in 0..10 {
            let v = if i % 2 == 0 {
                [0.3, 0.0, 0.95]
            } else {
                [0.0, 0.3, 0.95]
            };
            ix.insert(format!("/p{i}").into(), Vector::quantise(&unit(&v)));
        }
        // One clearly x.
        ix.insert("/x".into(), Vector::quantise(&unit(&[0.8, 0.0, 0.6])));
        let tags: HashMap<PathBuf, Vec<(u16, f32)>> =
            ix.tag(&labels, &rule()).into_iter().collect();
        assert_eq!(
            tags[Path::new("/x")]
                .iter()
                .map(|t| t.0)
                .collect::<Vec<_>>(),
            [0],
            "x, not z: z scores 0.6 here, below its usual 0.95"
        );
        assert!(
            tags.values().flatten().all(|t| t.0 != 2),
            "z is never a tag: it's high everywhere"
        );

        // A tiny library: no per-label baseline, only the photo's own
        // (its median label score, 0.6: z isn't above it).
        let mut small = Index::default();
        small.insert("/x".into(), Vector::quantise(&unit(&[0.8, 0.0, 0.6])));
        let t = small.tag(&labels, &rule());
        assert_eq!(t[0].1.iter().map(|t| t.0).collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn label_vectors_are_stored() -> anyhow::Result<()> {
        let mut s = Store::open_in_memory("image", "m")?;
        let prompts = vec!["a dog".to_owned(), "a cat".to_owned()];
        assert!(s.labels(&prompts)?.is_none());
        let v = Vector::quantise(&[0.6, 0.8]);
        s.put_labels(&[(prompts[0].clone(), v.clone())])?;
        assert!(s.labels(&prompts)?.is_none(), "one missing");
        s.put_labels(&[(prompts[1].clone(), v.clone())])?;
        assert_eq!(s.labels(&prompts)?, Some(vec![v.clone(), v]));
        assert!(
            Store::open_in_memory("image", "other")?
                .labels(&prompts)?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn sync_keeps_follows_and_prunes() -> anyhow::Result<()> {
        let mut s = Store::open_in_memory("image", "m1")?;
        let v = |x: f32| Vector::quantise(&unit(&[x, 1.0]));
        s.put(&[
            (key("/lib/a.jpg", 10, 100), v(0.1).payload()),
            (key("/lib/b.jpg", 20, 200), v(0.2).payload()),
            (key("/lib/c.jpg", 30, 300), v(0.3).payload()),
        ])?;

        // a unchanged, b renamed (same size and mtime), c edited, d new.
        let files = [
            key("/lib/a.jpg", 10, 100),
            key("/lib/2024/b-renamed.jpg", 20, 200),
            key("/lib/c.jpg", 30, 999),
            key("/lib/d.jpg", 40, 400),
        ];
        let r = s.sync(&files, true)?;
        assert_eq!(r.followed, 1);
        let ix = Index::from_rows(&r.found);
        assert!(ix.contains(Path::new("/lib/a.jpg")));
        assert_eq!(ix.get(Path::new("/lib/2024/b-renamed.jpg")), Some(&v(0.2)));
        let missing: Vec<_> = r.missing.iter().map(|f| f.path.to_str().unwrap()).collect();
        assert_eq!(missing, ["/lib/c.jpg", "/lib/d.jpg"]);
        assert_eq!(
            r.removed, 0,
            "c's row is still wanted (it will be replaced)"
        );

        // The rename stuck; a later sync finds b at its new path directly.
        let r = s.sync(&files, true)?;
        assert_eq!(r.followed, 0);
        assert_eq!(r.found.len(), 2);

        // a leaves the library: an incomplete sync keeps its row, a complete one drops it.
        let rest = &files[1..];
        assert_eq!(s.sync(rest, false)?.removed, 0);
        assert_eq!(s.sync(&files, true)?.found.len(), 2, "a is back");
        assert_eq!(s.sync(rest, true)?.removed, 1);
        assert_eq!(s.sync(&files, true)?.found.len(), 1, "a must be recomputed");
        Ok(())
    }

    #[test]
    fn other_models_are_recomputed_and_pruned() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("embeddings.sqlite");
        let f = key("/lib/a.jpg", 10, 100);
        let one = Vector::quantise(&[1.0]).payload();
        Store::open(&db, "image", "old")?.put(&[(f.clone(), one.clone())])?;
        let mut s = Store::open(&db, "image", "new")?;
        let r = s.sync(std::slice::from_ref(&f), true)?;
        assert!(r.found.is_empty());
        assert_eq!(r.missing, std::slice::from_ref(&f));
        s.put(&[(f.clone(), one.clone())])?;
        assert_eq!(s.sync(std::slice::from_ref(&f), true)?.found.len(), 1);

        // Another kind of row for the same file is separate.
        let mut faces = Store::open(&db, "faces", "f1")?;
        assert_eq!(faces.sync(std::slice::from_ref(&f), true)?.missing.len(), 1);
        faces.put(&[(f.clone(), one)])?;
        assert_eq!(faces.sync(std::slice::from_ref(&f), true)?.found.len(), 1);
        assert_eq!(s.sync(&[f], true)?.found.len(), 1, "image row kept");
        Ok(())
    }

    /// Search time at the design scale (200,000 photos, 768 dimensions).
    /// `cargo test --release -p shoebox-core -- --ignored search_200k --nocapture`
    #[test]
    #[ignore]
    fn search_200k() {
        let mut ix = Index::default();
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut rand = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        for i in 0..200_000 {
            let v: Vec<f32> = (0..768).map(|_| rand()).collect();
            ix.insert(
                PathBuf::from(format!("/lib/{i}.jpg")),
                Vector::quantise(&unit(&v)),
            );
        }
        let q = Vector::quantise(&unit(&(0..768).map(|_| rand()).collect::<Vec<_>>()));
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let hits = ix.search(&q, 0.07);
            best = best.min(t.elapsed().as_secs_f64() * 1e3);
            assert!(hits.len() < 200_000);
        }
        eprintln!("200k search: {best:.1} ms");
    }

    #[test]
    fn rows_read_beside_the_store() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("embeddings.sqlite");
        assert!(
            read_rows(&db, "image", "m1").unwrap().is_empty(),
            "no database yet"
        );
        let mut store = Store::open(&db, "image", "m1").unwrap();
        let key = FileKey {
            path: "/lib/a.jpg".into(),
            size: 1,
            mtime_ns: 2,
        };
        let v = Vector::quantise(&[0.6, 0.8]);
        store.put(&[(key, v.payload())]).unwrap();
        let rows = read_rows(&db, "image", "m1").unwrap();
        assert_eq!(rows, vec![(PathBuf::from("/lib/a.jpg"), v.payload())]);
        assert!(read_rows(&db, "image", "other model").unwrap().is_empty());
        assert!(read_rows(&db, "faces", "m1").unwrap().is_empty());
    }
}
