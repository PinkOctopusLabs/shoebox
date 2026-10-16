//! The journal: every file operation is recorded here before it runs.
//!
//! Stored in its own database (not the rebuildable cache), with
//! `synchronous=FULL` so a row is on disk before the operation it describes.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use crate::error::{CoreError, Result, io};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS batches (
    id      INTEGER PRIMARY KEY,
    ts      INTEGER NOT NULL,
    kind    TEXT NOT NULL,      -- move | undo
    label   TEXT NOT NULL,
    status  TEXT NOT NULL,      -- running | done | failed | undone
    undo_of INTEGER REFERENCES batches(id)
);
CREATE TABLE IF NOT EXISTS journal (
    id       INTEGER PRIMARY KEY,
    batch_id INTEGER NOT NULL REFERENCES batches(id),
    seq      INTEGER NOT NULL,
    ts       INTEGER NOT NULL,
    op       TEXT NOT NULL,     -- move | mkdir
    src      TEXT NOT NULL,
    dst      TEXT NOT NULL,
    size     INTEGER,
    status   TEXT NOT NULL,     -- pending | done | failed
    undone   INTEGER NOT NULL DEFAULT 0,
    error    TEXT
);
CREATE INDEX IF NOT EXISTS journal_batch ON journal(batch_id, seq);
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Move,
    Mkdir,
}

impl Op {
    fn as_str(self) -> &'static str {
        match self {
            Op::Move => "move",
            Op::Mkdir => "mkdir",
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "move" => Ok(Op::Move),
            "mkdir" => Ok(Op::Mkdir),
            other => Err(CoreError::Invalid(format!("unknown journal op {other:?}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpStatus {
    Pending,
    Done,
    Failed,
}

impl OpStatus {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(OpStatus::Pending),
            "done" => Ok(OpStatus::Done),
            "failed" => Ok(OpStatus::Failed),
            other => Err(CoreError::Invalid(format!("unknown op status {other:?}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpRecord {
    pub id: i64,
    pub seq: i64,
    pub op: Op,
    pub src: PathBuf,
    pub dst: PathBuf,
    pub size: Option<u64>,
    pub status: OpStatus,
    pub undone: bool,
}

#[derive(Debug, Clone)]
pub struct BatchSummary {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    pub label: String,
    pub status: String,
    pub undo_of: Option<i64>,
    pub ops: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| CoreError::Invalid(format!("non-UTF-8 path {}", p.display())))
}

pub struct Journal {
    conn: Connection,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn begin_batch(&self, kind: &str, label: &str, undo_of: Option<i64>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO batches(ts, kind, label, status, undo_of) VALUES (?1, ?2, ?3, 'running', ?4)",
            params![now_ms(), kind, label, undo_of],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_batch_status(&self, batch: i64, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE batches SET status = ?2 WHERE id = ?1",
            params![batch, status],
        )?;
        Ok(())
    }

    /// Record an operation as pending. Call before touching the disk.
    pub fn record(
        &self,
        batch: i64,
        seq: usize,
        op: Op,
        src: &Path,
        dst: &Path,
        size: Option<u64>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO journal(batch_id, seq, ts, op, src, dst, size, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending')",
            params![
                batch,
                seq as i64,
                now_ms(),
                op.as_str(),
                path_str(src)?,
                path_str(dst)?,
                size.map(|s| s as i64)
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn complete(&self, op_id: i64) -> Result<()> {
        self.conn
            .execute("UPDATE journal SET status = 'done' WHERE id = ?1", [op_id])?;
        Ok(())
    }

    pub fn fail(&self, op_id: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE journal SET status = 'failed', error = ?2 WHERE id = ?1",
            params![op_id, error],
        )?;
        Ok(())
    }

    pub fn mark_undone(&self, op_id: i64) -> Result<()> {
        self.conn
            .execute("UPDATE journal SET undone = 1 WHERE id = ?1", [op_id])?;
        Ok(())
    }

    pub fn ops(&self, batch: i64) -> Result<Vec<OpRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, seq, op, src, dst, size, status, undone FROM journal
             WHERE batch_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([batch], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, bool>(7)?,
            ))
        })?;
        rows.map(|row| {
            let (id, seq, op, src, dst, size, status, undone) = row?;
            Ok(OpRecord {
                id,
                seq,
                op: Op::parse(&op)?,
                src: src.into(),
                dst: dst.into(),
                size: size.map(|s| s as u64),
                status: OpStatus::parse(&status)?,
                undone,
            })
        })
        .collect()
    }

    fn summaries(&self, filter: &str) -> Result<Vec<BatchSummary>> {
        let sql = format!(
            "SELECT b.id, b.ts, b.kind, b.label, b.status, b.undo_of,
                    (SELECT COUNT(*) FROM journal j WHERE j.batch_id = b.id AND j.op = 'move')
             FROM batches b {filter} ORDER BY b.id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(BatchSummary {
                id: r.get(0)?,
                ts: r.get(1)?,
                kind: r.get(2)?,
                label: r.get(3)?,
                status: r.get(4)?,
                undo_of: r.get(5)?,
                ops: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn batches(&self) -> Result<Vec<BatchSummary>> {
        self.summaries("")
    }

    /// Batches that were started but never finished (the process died).
    pub fn incomplete(&self) -> Result<Vec<BatchSummary>> {
        self.summaries("WHERE b.status = 'running'")
    }

    pub fn batch(&self, id: i64) -> Result<Option<BatchSummary>> {
        Ok(self.batches()?.into_iter().find(|b| b.id == id))
    }
}
