//! CSV report of duplicate groups.

use std::io::Write;

use serde::Serialize;

use crate::error::Result;

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub group: usize,
    /// `keep` for the keeper; otherwise the level of the direct match.
    pub level: String,
    pub distance_to_keeper: Option<u32>,
    pub rotation: Option<u16>,
    pub role: &'static str,
    /// `keep`, `move` or `none`.
    pub action: &'static str,
    pub path: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: u64,
    pub blake3: String,
    pub matched_ref: Option<String>,
    pub matched_ref_level: Option<&'static str>,
    pub matched_ref_distance: Option<u32>,
    pub dest: Option<String>,
    pub note: Option<String>,
}

pub fn write_csv<W: Write>(w: W, rows: &[Row]) -> Result<()> {
    let mut csv = csv::Writer::from_writer(w);
    for r in rows {
        csv.serialize(r)?;
    }
    csv.flush().map_err(csv::Error::from)?;
    Ok(())
}
