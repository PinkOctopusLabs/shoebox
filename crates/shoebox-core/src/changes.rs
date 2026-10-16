//! Change tracking: which photos changed on disk since you last viewed them,
//! what changed (image or sidecar), and who changed it (Shoebox, with its
//! journal batch, or another app such as DxO, digiKam or Finder).
//!
//! A change is recorded only for a file Shoebox already knew, so a first scan
//! or a newly added folder doesn't light up the whole library. Viewing the
//! stack in the loupe, or Mark as Seen, clears it.

use serde::{Deserialize, Serialize};

/// What changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct What {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub sidecar: bool,
}

impl What {
    pub const IMAGE: What = What {
        image: true,
        sidecar: false,
    };
    pub const SIDECAR: What = What {
        image: false,
        sidecar: true,
    };

    pub fn any(self) -> bool {
        self.image || self.sidecar
    }

    pub fn union(self, o: What) -> What {
        What {
            image: self.image || o.image,
            sidecar: self.sidecar || o.sidecar,
        }
    }

    pub fn describe(self) -> &'static str {
        match (self.image, self.sidecar) {
            (true, true) => "Image and sidecar",
            (true, false) => "Image",
            (false, true) => "Sidecar",
            (false, false) => "Nothing",
        }
    }
}

/// Who made a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum By {
    /// Shoebox, in this journal batch.
    Shoebox { batch: i64, label: String },
    /// Anything else: detected by the watcher or a rescan.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// Milliseconds since the epoch when Shoebox noticed (or made) it.
    pub at_ms: i64,
    pub what: What,
    pub by: By,
}

impl Change {
    pub fn now(what: What, by: By) -> Self {
        Self {
            at_ms: crate::events::now_ms(),
            what,
            by,
        }
    }

    pub fn by_other(&self) -> bool {
        self.by == By::Other
    }

    /// Fold a newer change into an unseen one: what changed accumulates, and
    /// another app's change outranks Shoebox's (it's the one to notice).
    pub fn merge(old: Option<&Change>, new: Change) -> Change {
        match old {
            None => new,
            Some(o) => Change {
                at_ms: new.at_ms.max(o.at_ms),
                what: o.what.union(new.what),
                by: if o.by_other() { By::Other } else { new.by },
            },
        }
    }

    /// The cache's `change_source` column.
    pub fn source(&self) -> String {
        serde_json::to_string(&(&self.what, &self.by)).unwrap_or_default()
    }

    pub fn from_source(at_ms: i64, source: &str) -> Option<Change> {
        let (what, by): (What, By) = serde_json::from_str(source).ok()?;
        Some(Change { at_ms, what, by })
    }
}

/// The latest modification time in a sidecar signature (`path@mtime;…`).
pub fn sig_mtime(sig: &str) -> Option<i64> {
    sig.split(';')
        .filter_map(|s| s.rsplit_once('@')?.1.parse::<i64>().ok())
        .filter(|&t| t != 0)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_and_round_trip() {
        let own = Change::now(
            What::SIDECAR,
            By::Shoebox {
                batch: 4,
                label: "rating 3".into(),
            },
        );
        let other = Change::now(What::IMAGE, By::Other);
        let m = Change::merge(Some(&other), own.clone());
        assert_eq!(m.by, By::Other, "another app's change wins");
        assert_eq!(
            m.what,
            What {
                image: true,
                sidecar: true
            }
        );
        let back = Change::from_source(own.at_ms, &own.source()).unwrap();
        assert_eq!(back, own);
        assert_eq!(sig_mtime("/a/x.xmp@5;/a/x.nef.xmp@9"), Some(9));
        assert_eq!(sig_mtime(""), None);
    }
}
