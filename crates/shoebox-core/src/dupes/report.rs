//! CSV report of duplicate groups.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// Left empty by the tool; a person writes `y` to approve moving this file.
    pub confirm: &'static str,
    pub group: usize,
    /// `keep` for the keeper; otherwise the level of the direct match.
    pub level: String,
    pub distance_to_keeper: Option<u32>,
    pub rotation: Option<u16>,
    pub role: &'static str,
    /// `keep`, `duplicate` (directly matches a reference file), `review`
    /// (weaker or reference-less match) or `none` (reference files).
    pub suggestion: &'static str,
    pub path: String,
    /// The library root the file was scanned under.
    pub root: String,
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

/// The columns `shoebox apply` needs from a reviewed report.
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewRow {
    pub confirm: Option<String>,
    pub group: usize,
    pub role: String,
    pub path: String,
    pub root: String,
    pub blake3: String,
    pub dest: Option<String>,
}

impl ReviewRow {
    /// `y`, `yes`, `x`, `1` or `true`, in any case.
    pub fn is_confirmed(&self) -> bool {
        self.confirm.as_deref().is_some_and(|c| {
            matches!(
                c.trim().to_ascii_lowercase().as_str(),
                "y" | "yes" | "x" | "1" | "true"
            )
        })
    }
}

pub fn read_csv<R: Read>(r: R) -> Result<Vec<ReviewRow>> {
    let mut csv = csv::Reader::from_reader(r);
    Ok(csv.deserialize().collect::<Result<_, _>>()?)
}
