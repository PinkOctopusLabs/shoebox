//! Events: a named group of photos (Japan Trip, a wedding afternoon)
//! that moves no files.
//!
//! Membership is marked on each photo, not worked out from dates or
//! folders, so two events can share a day: a member's sidecar has a keyword
//! `Events|Japan Trip` (`Events|Japan Trip|Kyoto` for a sub-event, which
//! makes it a member of both) and `Iptc4xmpExt:Event` with the innermost
//! event's name, so digiKam can show and search them. Events can nest and
//! overlap. The events themselves (names, nesting) are mirrored to
//! `<root>/.shoebox/events.json` in every writable root and merged on load
//! (newest edit of each event wins), so they survive a cache rebuild.
//!
//! Events files from before (version 1) also held a date range and paths
//! added or excluded by hand; those fields are ignored. Saving an event
//! always wrote its members' keywords, so they stay members.

use std::collections::{BTreeSet, HashMap, HashSet};
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
    /// Milliseconds since the epoch of the last edit (merge tiebreak).
    #[serde(default)]
    pub updated: i64,
}

/// Photos joining or leaving an event, by their paths (any file of a stack
/// stands for the stack).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventChange {
    pub event: String,
    pub photos: Vec<PathBuf>,
    pub add: bool,
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
    v.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    v
}

/// The bytes of `events.json` for every writable root.
pub fn plan_save(writable_roots: &[PathBuf], events: &[Event]) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let body = serde_json::to_vec_pretty(&EventsFile {
        version: 2,
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

/// Event ids by their keyword path (`Japan Trip|Kyoto`).
fn by_path(events: &[Event]) -> HashMap<String, &str> {
    events
        .iter()
        .map(|e| (path_of(events, &e.id), e.id.as_str()))
        .collect()
}

/// The events a photo is marked with (ids of known events; keywords under
/// `Events|` that name no event are left alone).
fn marked(marks: &Marks, paths: &HashMap<String, &str>) -> BTreeSet<String> {
    marks
        .keywords
        .iter()
        .filter_map(|k| k.strip_prefix(KEYWORD_ROOT)?.strip_prefix('|'))
        .filter_map(|p| paths.get(p).map(|id| (*id).to_owned()))
        .collect()
}

/// `id` and its ancestors (a sub-event's photos are in its parents too).
fn with_ancestors<'a>(events: &'a [Event], id: &'a str) -> Vec<&'a str> {
    let mut out = vec![id];
    let mut cur = id;
    while let Some(p) = events
        .iter()
        .find(|e| e.id == cur)
        .and_then(|e| e.parent.as_deref())
    {
        if out.contains(&p) || out.len() > 32 {
            break; // a parent cycle in a hand-edited file
        }
        out.push(p);
        cur = p;
    }
    out
}

/// Each event's stacks: those with a photo marked with it or one of its
/// sub-events.
pub fn memberships(c: &Catalog, events: &[Event]) -> HashMap<String, HashSet<usize>> {
    let paths = by_path(events);
    let mut out: HashMap<String, HashSet<usize>> = events
        .iter()
        .map(|e| (e.id.clone(), HashSet::new()))
        .collect();
    for (s, st) in c.stacks.iter().enumerate() {
        for i in st.all() {
            for id in marked(&c.photos[i].marks, &paths) {
                for a in with_ancestors(events, &id) {
                    if let Some(m) = out.get_mut(a) {
                        m.insert(s);
                    }
                }
            }
        }
    }
    out
}

/// Marks with these events: `Events|…` keywords of known events kept in
/// place (renamed if need be) or dropped, new ones added at the end, and
/// the innermost event's name.
fn apply(marks: &Marks, ids: &BTreeSet<String>, old: &[Event], new: &[Event]) -> Marks {
    let old_paths = by_path(old);
    let mut m = marks.clone();
    let mut done: HashSet<&str> = HashSet::new();
    m.keywords = marks
        .keywords
        .iter()
        .filter_map(|k| {
            let Some(id) = k
                .strip_prefix(KEYWORD_ROOT)
                .and_then(|r| r.strip_prefix('|'))
                .and_then(|p| old_paths.get(p))
            else {
                return Some(k.clone()); // not an event keyword we know
            };
            (ids.contains(*id) && done.insert(id))
                .then(|| format!("{KEYWORD_ROOT}|{}", path_of(new, id)))
        })
        .collect();
    let mut added: Vec<String> = ids
        .iter()
        .filter(|id| !done.contains(id.as_str()))
        .map(|id| format!("{KEYWORD_ROOT}|{}", path_of(new, id)))
        .collect();
    added.sort();
    m.keywords.extend(added);
    // Innermost: the deepest path (the last, alphabetically, among equals).
    let innermost = ids
        .iter()
        .map(|id| path_of(new, id))
        .max_by_key(|p| (p.matches('|').count(), p.clone()))
        .and_then(|p| p.rsplit('|').next().map(str::to_owned));
    if innermost.is_some() || !marked(marks, &old_paths).is_empty() {
        m.event = innermost;
    }
    m
}

/// `marks` with these events joined (as well as those it's in already).
pub fn join(marks: &Marks, add: &[String], events: &[Event]) -> Marks {
    let mut ids = marked(marks, &by_path(events));
    ids.extend(add.iter().cloned());
    apply(marks, &ids, events, events)
}

/// `marks` with these events left (others it's in are kept).
pub fn leave(marks: &Marks, remove: &[String], events: &[Event]) -> Marks {
    let mut ids = marked(marks, &by_path(events));
    ids.retain(|id| !remove.contains(id));
    apply(marks, &ids, events, events)
}

/// The id of the event at this path of names, if there is one (matched
/// case-insensitively; nothing is made).
pub fn find_path(events: &[Event], names: &[String]) -> Option<String> {
    let mut parent: Option<String> = None;
    for name in names {
        let id = events
            .iter()
            .find(|e| e.parent == parent && e.name.to_lowercase() == name.to_lowercase())?
            .id
            .clone();
        parent = Some(id);
    }
    parent
}

/// The id of the event at this path of names (`["Italy 2019", "Rome"]`),
/// matched case-insensitively, creating the missing ones in `events`.
pub fn ensure_path(events: &mut Vec<Event>, names: &[String]) -> Option<String> {
    let mut parent: Option<String> = None;
    for name in names {
        let found = events
            .iter()
            .find(|e| e.parent == parent && e.name.to_lowercase() == name.to_lowercase())
            .map(|e| e.id.clone());
        let id = match found {
            Some(id) => id,
            None => {
                let id = new_id();
                events.push(Event {
                    id: id.clone(),
                    name: name.clone(),
                    parent: parent.clone(),
                    updated: now_ms(),
                });
                id
            }
        };
        parent = Some(id);
    }
    parent
}

/// Mark changes for moving from the `old` events to `new` (renamed, moved
/// under another event, deleted) and for `change` (photos joining or
/// leaving one). Joining marks the primary, or every stack member when
/// `all_members`; leaving unmarks every member, and leaves the event's
/// sub-events too.
pub fn mark_updates(
    c: &Catalog,
    old: &[Event],
    new: &[Event],
    change: Option<&EventChange>,
    all_members: bool,
) -> Vec<(usize, Marks)> {
    let paths = by_path(old);
    let known: HashSet<&str> = new.iter().map(|e| e.id.as_str()).collect();
    // The stacks the change is about (by any of their files).
    let mut changed: HashSet<usize> = HashSet::new();
    if let Some(ch) = change {
        let wanted: HashSet<&Path> = ch.photos.iter().map(PathBuf::as_path).collect();
        for (i, p) in c.photos.iter().enumerate() {
            if wanted.contains(p.path.as_path()) {
                changed.insert(c.stack_of[i]);
            }
        }
    }
    // Leaving an event leaves its sub-events too.
    let leaving: HashSet<&str> = match change {
        Some(ch) if !ch.add => new
            .iter()
            .chain(old)
            .filter(|e| with_ancestors(new, &e.id).contains(&ch.event.as_str()))
            .map(|e| e.id.as_str())
            .chain([ch.event.as_str()])
            .collect(),
        _ => HashSet::new(),
    };
    let mut out = Vec::new();
    for (s, st) in c.stacks.iter().enumerate() {
        for i in st.all() {
            let marks = &c.photos[i].marks;
            let mut ids = marked(marks, &paths);
            if changed.contains(&s)
                && let Some(ch) = change
            {
                if !ch.add {
                    ids.retain(|id| !leaving.contains(id.as_str()));
                } else if all_members || i == st.primary {
                    ids.insert(ch.event.clone());
                }
            }
            ids.retain(|id| known.contains(id.as_str()));
            let updated = apply(marks, &ids, old, new);
            if updated != *marks {
                out.push((i, updated));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Photo;
    use shoebox_decode::Format;

    fn ev(id: &str, name: &str, parent: Option<&str>) -> Event {
        Event {
            id: id.into(),
            name: name.into(),
            parent: parent.map(Into::into),
            updated: 0,
        }
    }

    /// Photos taken the same day, in the same folder.
    fn catalog(keywords: &[&[&str]]) -> Catalog {
        let photos: Vec<Photo> = keywords
            .iter()
            .enumerate()
            .map(|(n, kw)| {
                let path = PathBuf::from(format!("/lib/2026-04-24/p{n}.jpg"));
                let mut p = Photo {
                    root: 0,
                    rel: format!("2026-04-24/p{n}.jpg"),
                    format: Format::Jpeg,
                    path,
                    size: 0,
                    mtime_ns: 0,
                    meta: Default::default(),
                    marks: Default::default(),
                    sidecar: None,
                    sidecar_sig: String::new(),
                    change: None,
                    cloud: false,
                };
                p.meta.exif.capture_time = Some(format!("2026:04:24 1{n}:00:00"));
                p.marks.keywords = kw.iter().map(|k| (*k).to_owned()).collect();
                // As Shoebox writes them: the innermost event's name too.
                p.marks.event = kw
                    .iter()
                    .find_map(|k| k.strip_prefix("Events|"))
                    .and_then(|e| e.rsplit('|').next())
                    .map(str::to_owned);
                p
            })
            .collect();
        let (stacks, stack_of) = crate::stacks::build(&photos, &[]);
        Catalog {
            photos,
            stacks,
            stack_of,
            ..Default::default()
        }
    }

    fn set<const N: usize>(v: [usize; N]) -> HashSet<usize> {
        v.into_iter().collect()
    }

    /// Membership is what each photo is marked with: two events on one day
    /// in one folder stay apart, and a sub-event's photos are in its parent.
    #[test]
    fn members_are_marked_on_photos() {
        let events = vec![
            ev("a", "Japan Trip", None),
            ev("b", "Kyoto", Some("a")),
            ev("w", "Wedding", None),
        ];
        let c = catalog(&[
            &["Events|Japan Trip|Kyoto", "temple"],
            &["Events|Wedding"],
            &["Events|Japan Trip"],
            &["Events|Gone"], // an event that no longer exists
        ]);
        let m = memberships(&c, &events);
        assert_eq!(m["a"], set([0, 2]));
        assert_eq!(m["b"], set([0]));
        assert_eq!(m["w"], set([1]));
    }

    #[test]
    fn joining_leaving_renaming_and_deleting() {
        let old = vec![ev("a", "Japan Trip", None), ev("b", "Kyoto", Some("a"))];
        let c = catalog(&[&["Events|Japan Trip|Kyoto", "temple"], &[], &["Events|Old"]]);
        let path = |i: usize| c.photos[i].path.clone();
        let updates = |new: &[Event], ch: Option<EventChange>| -> HashMap<usize, Marks> {
            mark_updates(&c, &old, new, ch.as_ref(), false)
                .into_iter()
                .collect()
        };

        // Joining Kyoto: the keyword and the IPTC event name.
        let u = updates(
            &old,
            Some(EventChange {
                event: "b".into(),
                photos: vec![path(1)],
                add: true,
            }),
        );
        assert_eq!(u.len(), 1);
        assert_eq!(u[&1].keywords, vec!["Events|Japan Trip|Kyoto"]);
        assert_eq!(u[&1].event.as_deref(), Some("Kyoto"));

        // Leaving Japan Trip leaves Kyoto too; other keywords stay, and so
        // do keywords naming no event.
        let u = updates(
            &old,
            Some(EventChange {
                event: "a".into(),
                photos: vec![path(0), path(2)],
                add: false,
            }),
        );
        assert_eq!(u.len(), 1);
        assert_eq!(u[&0].keywords, vec!["temple"]);
        assert_eq!(u[&0].event, None);

        // Renaming the parent rewrites the sub-event's keyword.
        let mut renamed = old.clone();
        renamed[0].name = "Japan 2026".into();
        let u = updates(&renamed, None);
        assert_eq!(u[&0].keywords, vec!["Events|Japan 2026|Kyoto", "temple"]);

        // Deleting Kyoto (nothing else changes) unmarks its photos.
        let u = updates(&old[..1], None);
        assert_eq!(u[&0].keywords, vec!["temple"]);
        assert_eq!(u[&0].event, None);
    }

    #[test]
    fn files_merge_newest_wins() {
        let t = tempfile::tempdir().unwrap();
        let (r1, r2) = (t.path().join("r1"), t.path().join("r2"));
        let mut a = ev("x", "Old name", None);
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

    /// Version 1 files (a date range, paths added or excluded) still load.
    #[test]
    fn old_files_load() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join(".shoebox")).unwrap();
        std::fs::write(
            file_for(t.path()),
            r#"{"version":1,"events":[{"id":"x","name":"Trip","parent":null,
                "start":"2026-04-24 00:00:00","end":"2026-04-25 23:59:59",
                "include":["/a.jpg"],"exclude":[],"updated":3}]}"#,
        )
        .unwrap();
        assert_eq!(
            load(&[t.path().to_path_buf()]),
            vec![Event {
                updated: 3,
                ..ev("x", "Trip", None)
            }]
        );
    }
}
