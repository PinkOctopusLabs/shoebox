//! Naming templates for import and batch rename.
//!
//! A template is literal text with `{token}`s, rendered to a path relative
//! to a library root. `/` in the template makes folders; `/` (and other
//! characters that can't appear in a name) inside a token's value never do.
//!
//! | Token | Value |
//! | --- | --- |
//! | `{YYYY}` `{YY}` `{MM}` `{DD}` | capture date |
//! | `{HH}` `{mm}` `{ss}` | capture time |
//! | `{YYYY-MM-DD}` `{YYYYMMDD}` `{HHMMSS}` | the usual combinations |
//! | `{subsec}` | sub-seconds (`SubSecTimeOriginal`) |
//! | `{ext}` / `{EXT}` | extension, lower / upper case |
//! | `{name}` | the original file name without extension |
//! | `{camera}` | camera model |
//! | `{width}` `{height}` | upright pixel size |
//! | `{seq}` | position in the batch, 4 digits |
//!
//! Fallbacks: without sub-seconds, `{subsec}` and the separator before it
//! are left out; without a capture date the file goes to
//! `_undated/{width}x{height}/{original name}`.

use std::fmt;
use std::path::PathBuf;

use crate::error::{CoreError, Result};

/// The spec's default import template.
pub const DEFAULT_IMPORT: &str = "{YYYY}/{YYYY-MM-DD}/{YYYYMMDD}-{HHMMSS}-{subsec}.{ext}";
/// Default batch-rename template (same names, kept in their folder).
pub const DEFAULT_RENAME: &str = "{YYYYMMDD}-{HHMMSS}-{subsec}.{ext}";

const TOKENS: &[&str] = &[
    "YYYY",
    "YY",
    "MM",
    "DD",
    "HH",
    "mm",
    "ss",
    "YYYY-MM-DD",
    "YYYYMMDD",
    "HHMMSS",
    "subsec",
    "ext",
    "EXT",
    "name",
    "camera",
    "width",
    "height",
    "seq",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit(String),
    Tok(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
}

/// What a template is rendered from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fields {
    /// EXIF `YYYY:MM:DD HH:MM:SS`.
    pub capture_time: Option<String>,
    pub subsec: Option<String>,
    /// The original file name, extension included.
    pub file_name: String,
    pub camera: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// 1-based position in the batch.
    pub seq: usize,
}

impl Fields {
    fn stem_ext(&self) -> (&str, &str) {
        match self.file_name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s, e),
            _ => (self.file_name.as_str(), ""),
        }
    }
}

/// Parsed capture time.
struct When {
    y: String,
    mo: String,
    d: String,
    h: String,
    mi: String,
    s: String,
}

fn parse_time(t: &str) -> Option<When> {
    let (date, time) = t.trim().split_once(' ')?;
    let d: Vec<&str> = date.split(':').collect();
    let tm: Vec<&str> = time.trim().split(':').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    if d.len() != 3 || tm.len() < 3 {
        return None;
    }
    let sec = &tm[2][..tm[2].len().min(2)];
    if !(digits(d[0], 4) && digits(d[1], 2) && digits(d[2], 2))
        || !(digits(tm[0], 2) && digits(tm[1], 2) && digits(sec, 2))
        || d[0] == "0000"
    {
        return None;
    }
    Some(When {
        y: d[0].into(),
        mo: d[1].into(),
        d: d[2].into(),
        h: tm[0].into(),
        mi: tm[1].into(),
        s: sec.into(),
    })
}

/// A token's value made safe as part of one file name.
fn clean(v: &str) -> String {
    let s: String = v
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Never a hidden name or a parent reference.
    s.trim_start_matches('.').to_owned()
}

impl Template {
    pub fn parse(s: &str) -> Result<Self> {
        let bad = |m: String| CoreError::Invalid(format!("template {s:?}: {m}"));
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut rest = s;
        while let Some(i) = rest.find(['{', '}']) {
            if rest.as_bytes()[i] == b'}' {
                return Err(bad("a } without {".into()));
            }
            lit.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            let end = after.find('}').ok_or_else(|| bad("a { without }".into()))?;
            let name = &after[..end];
            let tok = TOKENS
                .iter()
                .find(|t| **t == name)
                .ok_or_else(|| bad(format!("unknown token {{{name}}}")))?;
            if !lit.is_empty() {
                parts.push(Part::Lit(std::mem::take(&mut lit)));
            }
            parts.push(Part::Tok(tok));
            rest = &after[end + 1..];
        }
        lit.push_str(rest);
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        if s.starts_with('/') {
            return Err(bad("must be relative to the library root".into()));
        }
        if s.split('/').any(|c| c == ".." || c == ".") {
            return Err(bad("`.` and `..` folders aren't allowed".into()));
        }
        if s.ends_with('/') || s.contains("//") {
            return Err(bad("empty folder name".into()));
        }
        if !parts.iter().any(|p| matches!(p, Part::Tok(_))) {
            return Err(bad(
                "needs at least one {token}, or every file gets one name".into(),
            ));
        }
        Ok(Self { parts })
    }

    /// Render to a path relative to the library root.
    pub fn render(&self, f: &Fields) -> PathBuf {
        let (stem, ext) = f.stem_ext();
        let Some(w) = f.capture_time.as_deref().and_then(parse_time) else {
            let size = match (f.width, f.height) {
                (Some(w), Some(h)) => format!("{w}x{h}"),
                _ => "unknown".to_owned(),
            };
            let name = clean(&f.file_name);
            let name = if name.is_empty() {
                "unnamed".to_owned()
            } else {
                name
            };
            return PathBuf::from("_undated").join(size).join(name);
        };
        let subsec = f
            .subsec
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
        let mut out = String::new();
        for (i, p) in self.parts.iter().enumerate() {
            match p {
                Part::Lit(l) => out.push_str(l),
                Part::Tok(t) => {
                    let v: String = match *t {
                        "YYYY" => w.y.clone(),
                        "YY" => w.y[2..].to_owned(),
                        "MM" => w.mo.clone(),
                        "DD" => w.d.clone(),
                        "HH" => w.h.clone(),
                        "mm" => w.mi.clone(),
                        "ss" => w.s.clone(),
                        "YYYY-MM-DD" => format!("{}-{}-{}", w.y, w.mo, w.d),
                        "YYYYMMDD" => format!("{}{}{}", w.y, w.mo, w.d),
                        "HHMMSS" => format!("{}{}{}", w.h, w.mi, w.s),
                        "subsec" => match subsec {
                            Some(s) => s.to_owned(),
                            None => {
                                // Drop the separator before it, if any.
                                let before =
                                    matches!(self.parts.get(i.wrapping_sub(1)), Some(Part::Lit(_)));
                                if before && out.ends_with(['-', '_', '.', ' ']) {
                                    out.pop();
                                }
                                String::new()
                            }
                        },
                        "ext" => ext.to_ascii_lowercase(),
                        "EXT" => ext.to_ascii_uppercase(),
                        "name" => stem.to_owned(),
                        "camera" => f.camera.clone().unwrap_or_else(|| "unknown camera".into()),
                        "width" => f.width.map_or_else(|| "0".into(), |x| x.to_string()),
                        "height" => f.height.map_or_else(|| "0".into(), |x| x.to_string()),
                        "seq" => format!("{:04}", f.seq),
                        _ => String::new(),
                    };
                    out.push_str(&clean(&v));
                }
            }
        }
        // No empty or hidden components, and no trailing dot from an empty
        // extension.
        let comps: Vec<String> = out
            .split('/')
            .map(|c| {
                let c = c.trim().trim_end_matches('.').trim_start_matches('.');
                if c.is_empty() {
                    "_".to_owned()
                } else {
                    c.to_owned()
                }
            })
            .collect();
        comps.iter().collect()
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for p in &self.parts {
            match p {
                Part::Lit(l) => f.write_str(l)?,
                Part::Tok(t) => write!(f, "{{{t}}}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::Component;

    fn fields(t: Option<&str>, ss: Option<&str>, name: &str) -> Fields {
        Fields {
            capture_time: t.map(Into::into),
            subsec: ss.map(Into::into),
            file_name: name.into(),
            camera: Some("NIKON Z f".into()),
            width: Some(6048),
            height: Some(4032),
            seq: 7,
        }
    }

    #[test]
    fn default_template_and_fallbacks() {
        let t = Template::parse(DEFAULT_IMPORT).unwrap();
        assert_eq!(t.to_string(), DEFAULT_IMPORT);
        let f = fields(Some("2026:05:01 12:34:56"), Some("77"), "DSC_0042.NEF");
        assert_eq!(
            t.render(&f),
            PathBuf::from("2026/2026-05-01/20260501-123456-77.nef")
        );
        let f = fields(Some("2026:05:01 12:34:56"), None, "DSC_0042.JPG");
        assert_eq!(
            t.render(&f),
            PathBuf::from("2026/2026-05-01/20260501-123456.jpg"),
            "no sub-seconds: left out with their separator"
        );
        let f = fields(None, Some("77"), "f0001.jpg");
        assert_eq!(t.render(&f), PathBuf::from("_undated/6048x4032/f0001.jpg"));
        let f = fields(Some("0000:00:00 00:00:00"), None, "x.jpg");
        assert!(
            t.render(&f).starts_with("_undated"),
            "a zeroed date is no date"
        );
    }

    #[test]
    fn other_tokens_and_bad_templates() {
        let t = Template::parse("{camera}/{YY}{MM}{DD}_{HH}{mm}{ss}_{seq}_{name}.{EXT}").unwrap();
        let mut f = fields(Some("2026:05:01 12:34:56"), None, "DSC_0042.nef");
        f.camera = Some("Z f/Mk II".into());
        assert_eq!(
            t.render(&f),
            PathBuf::from("Z f_Mk II/260501_123456_0007_DSC_0042.NEF")
        );
        for bad in [
            "{nope}.{ext}",
            "{YYYY",
            "a}b",
            "/abs/{name}",
            "../{name}",
            "plain.jpg",
            "{YYYY}//x",
        ] {
            assert!(Template::parse(bad).is_err(), "{bad}");
        }
    }

    fn time() -> impl Strategy<Value = Option<String>> {
        prop_oneof![
            Just(None),
            (
                1990u32..2100,
                1u32..13,
                1u32..29,
                0u32..24,
                0u32..60,
                0u32..60
            )
                .prop_map(|(y, mo, d, h, mi, s)| Some(format!(
                    "{y:04}:{mo:02}:{d:02} {h:02}:{mi:02}:{s:02}"
                ))),
            "[ -~]{0,24}".prop_map(Some), // garbage
        ]
    }

    proptest! {
        /// Whatever the metadata, the default template gives a relative path
        /// of plain names: no `..`, no hidden or empty parts, the original
        /// extension kept, and the same result every time.
        #[test]
        fn renders_safe_relative_paths(
            t in time(),
            ss in proptest::option::of("[0-9]{0,3}|[ -~]{0,6}"),
            name in "[ -~]{0,20}(\\.[A-Za-z]{3})?",
            camera in proptest::option::of("[ -~]{0,16}"),
        ) {
            let tpl = Template::parse(DEFAULT_IMPORT).unwrap();
            let f = Fields { capture_time: t, subsec: ss, file_name: name.clone(), camera, width: Some(10), height: Some(20), seq: 1 };
            let p = tpl.render(&f);
            prop_assert_eq!(&p, &tpl.render(&f));
            prop_assert!(p.is_relative());
            for c in p.components() {
                match c {
                    Component::Normal(s) => {
                        let s = s.to_string_lossy();
                        prop_assert!(!s.is_empty() && !s.starts_with('.'), "{:?}", p);
                    }
                    other => prop_assert!(false, "{:?} in {:?}", other, p),
                }
            }
            let dated = p.components().count() == 3 && !p.starts_with("_undated");
            prop_assert!(dated || p.starts_with("_undated"));
            let (_, ext) = f.stem_ext();
            if dated && !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
                let got = p.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
                prop_assert_eq!(got, ext.to_ascii_lowercase());
                let file = p.file_name().unwrap().to_string_lossy().into_owned();
                prop_assert!(!file.contains("-."), "{}", file);
            }
        }

        /// Parsing what a template prints gives the same template.
        #[test]
        fn display_round_trips(lits in proptest::collection::vec("[a-z_ -]{0,4}", 1..4), toks in proptest::collection::vec(0usize..TOKENS.len(), 1..4)) {
            let mut s = String::new();
            for (l, t) in lits.iter().zip(&toks) {
                s.push_str(l);
                s.push('{');
                s.push_str(TOKENS[*t]);
                s.push('}');
            }
            let t = Template::parse(&s).unwrap();
            prop_assert_eq!(Template::parse(&t.to_string()).unwrap(), t);
        }
    }
}
