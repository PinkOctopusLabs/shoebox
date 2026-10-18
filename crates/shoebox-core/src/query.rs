//! Filtering, search and sort over stacks. A smart filter is a saved
//! [`Filter`].
//!
//! Marks and capture details come from the stack's primary; file type and
//! search look at every member (so "TIF" finds stacks with a TIFF export).

use serde::{Deserialize, Serialize};
use shoebox_decode::Format;

use crate::catalog::Catalog;
use crate::marks::{Label, Pick};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlagFilter {
    #[default]
    Any,
    Picked,
    Rejected,
    Unflagged,
    /// Picked or unflagged (hide rejects).
    NotRejected,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelFilter {
    #[default]
    Any,
    None,
    Is(LabelName),
}

/// Serialisable mirror of [`Label`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelName {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl From<Label> for LabelName {
    fn from(l: Label) -> Self {
        match l {
            Label::Red => LabelName::Red,
            Label::Yellow => LabelName::Yellow,
            Label::Green => LabelName::Green,
            Label::Blue => LabelName::Blue,
            Label::Purple => LabelName::Purple,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    /// Minimum rating (0 = any).
    #[serde(default)]
    pub min_rating: u8,
    #[serde(default)]
    pub flag: FlagFilter,
    #[serde(default)]
    pub label: LabelFilter,
    /// File types; empty = any. A stack matches if any member has one.
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub camera: Option<String>,
    #[serde(default)]
    pub lens: Option<String>,
    /// Stacks without a NEF.
    #[serde(default)]
    pub nef_missing: bool,
    /// Stacks whose primary has no capture time.
    #[serde(default)]
    pub undated: bool,
    /// Case-insensitive substring of a member's file name or of a keyword.
    #[serde(default)]
    pub search: String,
    /// Only within this folder (and below).
    #[serde(default)]
    pub folder: Option<std::path::PathBuf>,
    /// Stacks with an image or sidecar modified within this many days.
    #[serde(default)]
    pub changed_within_days: Option<u32>,
    /// Stacks with a change you haven't viewed yet.
    #[serde(default)]
    pub unseen_changes: bool,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        Filter {
            folder: None,
            ..self.clone()
        } == Filter::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartFilter {
    pub name: String,
    pub filter: Filter,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    #[default]
    CaptureTime,
    FileName,
    Rating,
    Modified,
}

impl Sort {
    pub const ALL: [(Sort, &'static str); 4] = [
        (Sort::CaptureTime, "Capture time"),
        (Sort::FileName, "File name"),
        (Sort::Rating, "Rating"),
        (Sort::Modified, "Modified"),
    ];
}

/// Does stack `s` pass the filter?
pub fn matches(c: &Catalog, s: usize, f: &Filter) -> bool {
    let st = &c.stacks[s];
    let p = &c.photos[st.primary];
    let m = &p.marks;
    if m.rating < f.min_rating {
        return false;
    }
    let flag_ok = match f.flag {
        FlagFilter::Any => true,
        FlagFilter::Picked => m.pick == Some(Pick::Pick),
        FlagFilter::Rejected => m.pick == Some(Pick::Reject),
        FlagFilter::Unflagged => m.pick.is_none(),
        FlagFilter::NotRejected => m.pick != Some(Pick::Reject),
    };
    if !flag_ok {
        return false;
    }
    let label_ok = match f.label {
        LabelFilter::Any => true,
        LabelFilter::None => m.label.is_none(),
        LabelFilter::Is(l) => m.label.map(LabelName::from) == Some(l),
    };
    if !label_ok {
        return false;
    }
    if !f.types.is_empty()
        && !st
            .all()
            .any(|i| f.types.iter().any(|t| t == c.photos[i].format.label()))
    {
        return false;
    }
    if f.camera
        .as_ref()
        .is_some_and(|cam| p.meta.exif.model.as_ref() != Some(cam))
    {
        return false;
    }
    if f.lens
        .as_ref()
        .is_some_and(|l| p.meta.exif.lens.as_ref() != Some(l))
    {
        return false;
    }
    if f.nef_missing && st.all().any(|i| c.photos[i].format == Format::Nef) {
        return false;
    }
    if f.undated && p.meta.capture_seconds().is_some() {
        return false;
    }
    if let Some(folder) = &f.folder
        && !st.all().any(|i| c.photos[i].path.starts_with(folder))
    {
        return false;
    }
    if let Some(days) = f.changed_within_days {
        let since = crate::events::now_ms() * 1_000_000 - i64::from(days) * 86_400 * 1_000_000_000;
        if stack_modified_ns(c, s) < since {
            return false;
        }
    }
    if f.unseen_changes && !st.all().any(|i| c.photos[i].change.is_some()) {
        return false;
    }
    let q = f.search.trim();
    if !q.is_empty() {
        let in_names = st.all().any(|i| {
            c.photos[i]
                .path
                .file_name()
                .is_some_and(|n| contains_ci(&n.to_string_lossy(), q))
        });
        let in_keywords = m.keywords.iter().any(|k| contains_ci(k, q));
        if !in_names && !in_keywords {
            return false;
        }
    }
    true
}

/// Case-insensitive `haystack.contains(needle)`, without allocating when
/// both are ASCII (file names nearly always are; this runs per keystroke
/// over every stack).
fn contains_ci(haystack: &str, needle: &str) -> bool {
    if haystack.is_ascii() && needle.is_ascii() {
        let (h, n) = (haystack.as_bytes(), needle.as_bytes());
        n.is_empty()
            || (n.len() <= h.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n)))
    } else {
        haystack.to_lowercase().contains(&needle.to_lowercase())
    }
}

/// When any file of stack `s` (image or sidecar) last changed on disk (ns).
pub fn stack_modified_ns(c: &Catalog, s: usize) -> i64 {
    c.stacks[s]
        .all()
        .map(|i| c.photos[i].modified_ns())
        .max()
        .unwrap_or(0)
}

/// The built-in "Recently changed" smart filter: the last 7 days, newest first.
pub fn recently_changed() -> Filter {
    Filter {
        changed_within_days: Some(7),
        ..Filter::default()
    }
}

/// Stack indices passing `f`, in `sort` order (ties by capture time).
pub fn run(c: &Catalog, f: &Filter, sort: Sort, descending: bool) -> Vec<usize> {
    let mut out: Vec<usize> = (0..c.stacks.len()).filter(|&s| matches(c, s, f)).collect();
    let primary = |s: usize| &c.photos[c.stacks[s].primary];
    // Stacks are already in capture-time order of their primaries.
    match sort {
        Sort::CaptureTime => {}
        Sort::FileName => {
            out.sort_by_key(|&s| primary(s).path.file_name().map(|n| n.to_ascii_lowercase()))
        }
        Sort::Rating => out.sort_by_key(|&s| std::cmp::Reverse(primary(s).marks.rating)),
        Sort::Modified => out.sort_by_key(|&s| std::cmp::Reverse(stack_modified_ns(c, s))),
    }
    if descending {
        out.reverse();
    }
    out
}

/// Every keyword in use, for autocomplete, most used first.
pub fn keywords(c: &Catalog) -> Vec<(String, usize)> {
    let mut counts: std::collections::HashMap<&str, usize> = Default::default();
    for p in &c.photos {
        for k in &p.marks.keywords {
            *counts.entry(k.as_str()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().map(|(k, n)| (k.to_owned(), n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Distinct values of a metadata field among primaries, for filter menus.
pub fn distinct(
    c: &Catalog,
    field: impl Fn(&crate::exif::ExifInfo) -> Option<&String>,
) -> Vec<String> {
    let mut v: Vec<String> = c
        .stacks
        .iter()
        .filter_map(|s| field(&c.photos[s.primary].meta.exif).cloned())
        .collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_contains() {
        assert!(super::contains_ci("DSC_1234.NEF", "dsc_12"));
        assert!(super::contains_ci("dsc_1234.nef", "SC_1"));
        assert!(!super::contains_ci("DSC_1234.NEF", "dsc_13"));
        assert!(!super::contains_ci("a", "abc"));
        assert!(super::contains_ci("Ärger-Übung.jpg", "äRGER"));
        assert!(super::contains_ci("Kyoto", ""));
    }
    use crate::catalog::Photo;
    use crate::marks::Marks;
    use crate::meta::Meta;

    fn photo(path: &str, rating: u8, pick: Option<Pick>, kw: &[&str], dated: bool) -> Photo {
        let p = std::path::PathBuf::from(path);
        let mut meta = Meta::default();
        if dated {
            meta.exif.capture_time = Some("2026:05:01 10:00:00".into());
            meta.exif.model = Some("NIKON Z f".into());
        }
        Photo {
            root: 0,
            rel: path.into(),
            format: Format::from_path(&p).unwrap(),
            path: p,
            size: 0,
            mtime_ns: 0,
            meta,
            marks: Marks {
                rating,
                pick,
                label: None,
                keywords: kw.iter().map(|s| (*s).to_owned()).collect(),
                event: None,
                gps_e7: None,
            },
            sidecar: None,
            sidecar_sig: String::new(),
            change: None,
            cloud: false,
        }
    }

    fn catalog(photos: Vec<Photo>) -> Catalog {
        let (stacks, stack_of) = crate::stacks::build(&photos, &[]);
        Catalog {
            roots: Vec::new(),
            photos,
            stacks,
            stack_of,
            links: Vec::new(),
            folders: Vec::new(),
        }
    }

    #[test]
    fn filters() {
        let c = catalog(vec![
            photo(
                "/a/x.nef",
                4,
                Some(Pick::Pick),
                &["Places|Japan|Kyoto"],
                true,
            ),
            photo("/a/x.jpg", 0, None, &[], true),
            photo("/a/y.tif", 2, Some(Pick::Reject), &[], true),
            photo("/a/z.jpg", 0, None, &["beach"], false),
        ]);
        let names = |f: &Filter| -> Vec<String> {
            run(&c, f, Sort::FileName, false)
                .into_iter()
                .map(|s| {
                    c.photos[c.stacks[s].primary]
                        .path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        };
        assert_eq!(
            names(&Filter::default()),
            vec!["x.nef", "y.tif", "z.jpg"],
            "x.jpg is in x.nef's stack"
        );
        assert_eq!(
            names(&Filter {
                min_rating: 3,
                ..Default::default()
            }),
            vec!["x.nef"]
        );
        assert_eq!(
            names(&Filter {
                flag: FlagFilter::NotRejected,
                ..Default::default()
            }),
            vec!["x.nef", "z.jpg"]
        );
        assert_eq!(
            names(&Filter {
                nef_missing: true,
                ..Default::default()
            }),
            vec!["y.tif", "z.jpg"]
        );
        assert_eq!(
            names(&Filter {
                undated: true,
                ..Default::default()
            }),
            vec!["z.jpg"]
        );
        assert_eq!(
            names(&Filter {
                types: vec!["JPG".into()],
                ..Default::default()
            }),
            vec!["x.nef", "z.jpg"]
        );
        assert_eq!(
            names(&Filter {
                search: "kyoto".into(),
                ..Default::default()
            }),
            vec!["x.nef"]
        );
        assert_eq!(
            names(&Filter {
                search: "Y.T".into(),
                ..Default::default()
            }),
            vec!["y.tif"]
        );
        assert_eq!(
            names(&Filter {
                camera: Some("NIKON Z f".into()),
                ..Default::default()
            }),
            vec!["x.nef", "y.tif"]
        );
    }

    #[test]
    fn sorts_and_keywords() {
        let c = catalog(vec![
            photo("/a/b.jpg", 1, None, &["x", "y"], true),
            photo("/a/a.jpg", 5, None, &["y"], true),
        ]);
        let order = |s: Sort, d: bool| -> Vec<u8> {
            run(&c, &Filter::default(), s, d)
                .into_iter()
                .map(|i| c.photos[c.stacks[i].primary].marks.rating)
                .collect()
        };
        assert_eq!(order(Sort::Rating, false), vec![5, 1]);
        assert_eq!(order(Sort::FileName, true), vec![1, 5]);
        assert_eq!(keywords(&c)[0], ("y".into(), 2));
    }

    #[test]
    fn smart_filters_serialise() {
        let s = SmartFilter {
            name: "Picks ★3+".into(),
            filter: Filter {
                min_rating: 3,
                flag: FlagFilter::Picked,
                label: LabelFilter::Is(LabelName::Red),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<SmartFilter>(&json).unwrap(), s);
    }
}
