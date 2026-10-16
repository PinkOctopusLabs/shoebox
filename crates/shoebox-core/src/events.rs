//! Events: a named span of time (Japan Trip, a wedding afternoon) that
//! groups the photos it covers without moving any files.
//!
//! Membership is the start–end range of capture times, plus stacks added or
//! excluded by hand. Events can nest (Japan Trip › Kyoto) and overlap. Each
//! member's sidecar gets `Iptc4xmpExt:Event` (the innermost event's name) and
//! a keyword per event under `Events|…` (`Events|Japan Trip|Kyoto`), so
//! digiKam can show and search them. Events are mirrored to
//! `<root>/.shoebox/events.json` in every writable root and merged on load
//! (newest edit of each event wins), so they survive a cache rebuild.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::error::{CoreError, Result, io};
use crate::marks::Marks;

pub const KEYWORD_ROOT: &str = "Events";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    /// Inclusive range, `YYYY-MM-DD HH:MM:SS` (camera local time).
    pub start: String,
    pub end: String,
    /// Stacks added or excluded by hand, by their primary's absolute path.
    #[serde(default)]
    pub include: Vec<PathBuf>,
    #[serde(default)]
    pub exclude: Vec<PathBuf>,
    /// Milliseconds since the epoch of the last edit (merge tiebreak).
    #[serde(default)]
    pub updated: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct EventsFile {
    #[serde(default)]
    version: u32,
    /// Missing in `{}`, which undo leaves behind for a file it can't delete.
    #[serde(default)]
    events: Vec<Event>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// A fresh, unique-enough id (time + process + counter).
pub fn new_id() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("ev-{:x}-{:x}-{n:x}", now_ms(), std::process::id())
}

/// `<root>/.shoebox/events.json`.
pub fn file_for(root: &Path) -> PathBuf {
    root.join(".shoebox").join("events.json")
}

/// Merge the events files of these roots (newest `updated` per id wins).
pub fn load(roots: &[PathBuf]) -> Vec<Event> {
    let mut by_id: HashMap<String, Event> = HashMap::new();
    for root in roots {
        let Ok(bytes) = std::fs::read(file_for(root)) else {
            continue;
        };
        match serde_json::from_slice::<EventsFile>(&bytes) {
            Ok(f) => {
                for e in f.events {
                    match by_id.get(&e.id) {
                        Some(old) if old.updated >= e.updated => {}
                        _ => {
                            by_id.insert(e.id.clone(), e);
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("ignoring unreadable {}: {e}", file_for(root).display()),
        }
    }
    let mut v: Vec<Event> = by_id.into_values().collect();
    v.sort_by(|a, b| (&a.start, &a.name).cmp(&(&b.start, &b.name)));
    v
}

/// The bytes of `events.json` for every writable root.
pub fn plan_save(writable_roots: &[PathBuf], events: &[Event]) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let body = serde_json::to_vec_pretty(&EventsFile {
        version: 1,
        events: events.to_vec(),
    })
    .map_err(|e| CoreError::Invalid(format!("events: {e}")))?;
    writable_roots
        .iter()
        .map(|r| {
            let dir = r.join(".shoebox");
            std::fs::create_dir_all(&dir).map_err(io(&dir))?;
            Ok((file_for(r), body.clone()))
        })
        .collect()
}

/// `YYYY:MM:DD HH:MM:SS` (EXIF) → `YYYY-MM-DD HH:MM:SS`.
pub fn exif_to_event_time(t: &str) -> String {
    t.replacen(':', "-", 2)
}

/// The event path for keywords: `Japan Trip|Kyoto` for a nested event.
pub fn path_of(events: &[Event], id: &str) -> String {
    let by_id: HashMap<&str, &Event> = events.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut parts = Vec::new();
    let mut cur = by_id.get(id).copied();
    let mut guard = 0;
    while let Some(e) = cur {
        parts.push(e.name.clone());
        cur = e.parent.as_deref().and_then(|p| by_id.get(p).copied());
        guard += 1;
        if guard > 32 {
            break; // a parent cycle in a hand-edited file
        }
    }
    parts.reverse();
    parts.join("|")
}

/// Stack indices in the event.
pub fn members(c: &Catalog, e: &Event) -> HashSet<usize> {
    let include: HashSet<&Path> = e.include.iter().map(PathBuf::as_path).collect();
    let exclude: HashSet<&Path> = e.exclude.iter().map(PathBuf::as_path).collect();
    (0..c.stacks.len())
        .filter(|&s| {
            let p = &c.photos[c.stacks[s].primary];
            if exclude.contains(p.path.as_path()) {
                return false;
            }
            if include.contains(p.path.as_path()) {
                return true;
            }
            p.meta
                .exif
                .capture_time
                .as_deref()
                .map(exif_to_event_time)
                .is_some_and(|t| t.as_str() >= e.start.as_str() && t.as_str() <= e.end.as_str())
        })
        .collect()
}

/// Marks after applying the event set: `Events|…` keywords replaced by one
/// per containing event, and the innermost (latest-starting) event's name.
pub fn apply(marks: &Marks, containing: &[&Event], events: &[Event]) -> Marks {
    let mut m = marks.clone();
    m.keywords
        .retain(|k| k != KEYWORD_ROOT && !k.starts_with(&format!("{KEYWORD_ROOT}|")));
    let mut sorted: Vec<&&Event> = containing.iter().collect();
    sorted.sort_by(|a, b| (&a.start, &a.name).cmp(&(&b.start, &b.name)));
    for e in &sorted {
        m.keywords
            .push(format!("{KEYWORD_ROOT}|{}", path_of(events, &e.id)));
    }
    m.event = sorted.last().map(|e| e.name.clone());
    m
}

/// Mark changes for every photo whose event-derived marks differ, given the
/// new event list. `all_members` writes to every stack member, else to the
/// primary only (like other marks).
pub fn mark_updates(c: &Catalog, events: &[Event], all_members: bool) -> Vec<(usize, Marks)> {
    let mut containing: Vec<Vec<&Event>> = vec![Vec::new(); c.stacks.len()];
    for e in events {
        for s in members(c, e) {
            containing[s].push(e);
        }
    }
    let mut out = Vec::new();
    for (s, evs) in containing.iter().enumerate() {
        let st = &c.stacks[s];
        let targets: Vec<usize> = if all_members {
            st.all().collect()
        } else {
            vec![st.primary]
        };
        for i in targets {
            let new = apply(&c.photos[i].marks, evs, events);
            if new != c.photos[i].marks {
                out.push((i, new));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, name: &str, parent: Option<&str>, start: &str, end: &str) -> Event {
        Event {
            id: id.into(),
            name: name.into(),
            parent: parent.map(Into::into),
            start: start.into(),
            end: end.into(),
            include: Vec::new(),
            exclude: Vec::new(),
            updated: 0,
        }
    }

    #[test]
    fn nested_keywords_and_innermost_name() {
        let events = vec![
            ev(
                "a",
                "Japan Trip",
                None,
                "2026-04-20 00:00:00",
                "2026-05-10 23:59:59",
            ),
            ev(
                "b",
                "Kyoto",
                Some("a"),
                "2026-04-24 00:00:00",
                "2026-04-26 23:59:59",
            ),
        ];
        assert_eq!(path_of(&events, "b"), "Japan Trip|Kyoto");
        let base = Marks {
            keywords: vec!["temple".into(), "Events|Old".into()],
            ..Default::default()
        };
        let m = apply(&base, &[&events[1], &events[0]], &events);
        assert_eq!(
            m.keywords,
            vec!["temple", "Events|Japan Trip", "Events|Japan Trip|Kyoto"]
        );
        assert_eq!(m.event.as_deref(), Some("Kyoto"));
        let cleared = apply(&m, &[], &events);
        assert_eq!(cleared.keywords, vec!["temple"]);
        assert_eq!(cleared.event, None);
    }

    #[test]
    fn files_merge_newest_wins() {
        let t = tempfile::tempdir().unwrap();
        let (r1, r2) = (t.path().join("r1"), t.path().join("r2"));
        let mut a = ev(
            "x",
            "Old name",
            None,
            "2026-01-01 00:00:00",
            "2026-01-02 00:00:00",
        );
        for (path, bytes) in plan_save(std::slice::from_ref(&r1), &[a.clone()]).unwrap() {
            std::fs::write(path, bytes).unwrap();
        }
        a.name = "New name".into();
        a.updated = 5;
        for (path, bytes) in plan_save(std::slice::from_ref(&r2), &[a.clone()]).unwrap() {
            std::fs::write(path, bytes).unwrap();
        }
        let loaded = load(&[r1, r2]);
        assert_eq!(loaded, vec![a]);
    }
}
