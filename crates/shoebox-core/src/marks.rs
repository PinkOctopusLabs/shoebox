//! Culling marks (rating, flag, colour label, keywords) and how they map to
//! the XMP properties other tools read.
//!
//! - Rating 0–5 → `xmp:Rating`.
//! - Flag → `digiKam:PickLabel` (3 accepted, 1 rejected, 0 none) and
//!   `shoebox:Pick`. On read, Bridge's `xmp:Rating = -1` also means reject.
//! - Colour label → `xmp:Label` (Lightroom's English names) and
//!   `digiKam:ColorLabel` (1 red, 3 yellow, 4 green, 5 blue, 6 magenta).
//! - Keywords are hierarchical paths (`Places|Japan|Kyoto`) →
//!   `lr:hierarchicalSubject` (`|`), `digiKam:TagsList` (`/`, as digiKam
//!   writes it) and `dc:subject` (leaf names).

use std::collections::HashMap;

use crate::xmp::{Prop, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pick {
    Pick,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Label {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl Label {
    pub const ALL: [Label; 5] = [
        Label::Red,
        Label::Yellow,
        Label::Green,
        Label::Blue,
        Label::Purple,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Label::Red => "Red",
            Label::Yellow => "Yellow",
            Label::Green => "Green",
            Label::Blue => "Blue",
            Label::Purple => "Purple",
        }
    }

    fn digikam(self) -> u8 {
        match self {
            Label::Red => 1,
            Label::Yellow => 3,
            Label::Green => 4,
            Label::Blue => 5,
            Label::Purple => 6,
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        Label::ALL
            .into_iter()
            .find(|l| l.name().eq_ignore_ascii_case(s.trim()))
    }

    fn from_digikam(n: u8) -> Option<Self> {
        Label::ALL.into_iter().find(|l| l.digikam() == n)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    /// 0–5 (0 = unrated).
    pub rating: u8,
    pub pick: Option<Pick>,
    pub label: Option<Label>,
    /// Hierarchical keywords, `|`-separated, in the order read or added.
    pub keywords: Vec<String>,
    /// `Iptc4xmpExt:Event`: the innermost event the photo belongs to.
    pub event: Option<String>,
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        *self == Marks::default()
    }

    /// Interpret the owned properties of a sidecar (or embedded XMP).
    pub fn from_props(props: &HashMap<Prop, Value>) -> Self {
        let text = |p: Prop| props.get(&p).and_then(Value::first);
        let rating_raw: Option<i32> = text(Prop::Rating)
            .and_then(|r| r.trim().parse::<f64>().ok())
            .map(|r| r.round() as i32);
        let pick = match text(Prop::Pick).as_deref().map(str::trim) {
            Some("pick") => Some(Pick::Pick),
            Some("reject") => Some(Pick::Reject),
            Some(_) => None,
            None => match text(Prop::PickLabel).and_then(|v| v.trim().parse::<u8>().ok()) {
                Some(3) => Some(Pick::Pick),
                Some(1) => Some(Pick::Reject),
                Some(_) => None,
                None => (rating_raw == Some(-1)).then_some(Pick::Reject),
            },
        };
        let label = text(Prop::Label)
            .as_deref()
            .and_then(Label::from_name)
            .or_else(|| {
                text(Prop::ColorLabel)
                    .and_then(|v| v.trim().parse::<u8>().ok())
                    .and_then(Label::from_digikam)
            });

        let mut keywords: Vec<String> = Vec::new();
        let push = |k: String, keywords: &mut Vec<String>| {
            let k = k.trim().trim_matches('|').to_owned();
            if !k.is_empty() && !keywords.contains(&k) {
                keywords.push(k);
            }
        };
        if let Some(v) = props.get(&Prop::HierarchicalSubject) {
            for k in v.items() {
                push(k, &mut keywords);
            }
        }
        if let Some(v) = props.get(&Prop::TagsList) {
            for k in v.items() {
                push(k.replace('/', "|"), &mut keywords);
            }
        }
        // Flat keywords count only if they aren't already part of a path
        // (Lightroom lists a path's parents in dc:subject too).
        if let Some(v) = props.get(&Prop::Subject) {
            for k in v.items() {
                let covered = keywords
                    .iter()
                    .any(|path| path.split('|').any(|part| part == k.trim()));
                if !covered {
                    push(k, &mut keywords);
                }
            }
        }
        Self {
            rating: rating_raw.unwrap_or(0).clamp(0, 5) as u8,
            pick,
            label,
            keywords,
            event: props
                .get(&Prop::Event)
                .and_then(Value::first)
                .filter(|e| !e.trim().is_empty()),
        }
    }

    /// The property changes that turn `old` into `self`. Only properties
    /// whose meaning changed are listed, so untouched marks keep whatever
    /// form another tool wrote them in.
    pub fn changes_from(&self, old: &Marks) -> Vec<(Prop, Option<Value>)> {
        let mut out = Vec::new();
        let text = |s: String| Some(Value::Text(s));
        if self.rating != old.rating {
            out.push((Prop::Rating, text(self.rating.to_string())));
        }
        if self.pick != old.pick {
            let (dk, ours) = match self.pick {
                Some(Pick::Pick) => ("3", "pick"),
                Some(Pick::Reject) => ("1", "reject"),
                None => ("0", "none"),
            };
            out.push((Prop::PickLabel, text(dk.into())));
            out.push((Prop::Pick, text(ours.into())));
        }
        if self.label != old.label {
            match self.label {
                Some(l) => {
                    out.push((Prop::Label, text(l.name().into())));
                    out.push((Prop::ColorLabel, text(l.digikam().to_string())));
                }
                None => {
                    out.push((Prop::Label, None));
                    out.push((Prop::ColorLabel, None));
                }
            }
        }
        if self.event != old.event {
            out.push((Prop::Event, self.event.clone().map(Value::LangAlt)));
        }
        if self.keywords != old.keywords {
            if self.keywords.is_empty() {
                out.push((Prop::HierarchicalSubject, None));
                out.push((Prop::TagsList, None));
                out.push((Prop::Subject, None));
            } else {
                let mut leaves: Vec<String> = Vec::new();
                for k in &self.keywords {
                    let leaf = k.rsplit('|').next().unwrap_or(k).to_owned();
                    if !leaves.contains(&leaf) {
                        leaves.push(leaf);
                    }
                }
                out.push((
                    Prop::HierarchicalSubject,
                    Some(Value::Bag(self.keywords.clone())),
                ));
                out.push((
                    Prop::TagsList,
                    Some(Value::Seq(
                        self.keywords.iter().map(|k| k.replace('|', "/")).collect(),
                    )),
                ));
                out.push((Prop::Subject, Some(Value::Bag(leaves))));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmp;

    fn round_trip(m: &Marks) -> Marks {
        let doc = xmp::edit(None, &m.changes_from(&Marks::default())).unwrap();
        Marks::from_props(&xmp::read(&doc).unwrap())
    }

    #[test]
    fn round_trips() {
        let m = Marks {
            rating: 4,
            pick: Some(Pick::Reject),
            label: Some(Label::Purple),
            keywords: vec![
                "Places|Japan|Kyoto".into(),
                "temple".into(),
                "Events|Japan Trip".into(),
            ],
            event: Some("Japan Trip".into()),
        };
        assert_eq!(round_trip(&m), m);
        assert_eq!(round_trip(&Marks::default()), Marks::default());
    }

    #[test]
    fn interprets_other_tools() {
        let mut p = HashMap::new();
        p.insert(Prop::Rating, Value::Text("-1".into())); // Bridge reject
        p.insert(Prop::ColorLabel, Value::Text("4".into())); // digiKam green
        // Lightroom lists parents in dc:subject; they mustn't become keywords.
        p.insert(
            Prop::Subject,
            Value::Bag(vec![
                "Places".into(),
                "Japan".into(),
                "Kyoto".into(),
                "food".into(),
            ]),
        );
        p.insert(
            Prop::HierarchicalSubject,
            Value::Bag(vec!["Places|Japan|Kyoto".into()]),
        );
        let m = Marks::from_props(&p);
        assert_eq!(m.pick, Some(Pick::Reject));
        assert_eq!(m.rating, 0);
        assert_eq!(m.label, Some(Label::Green));
        assert_eq!(m.keywords, vec!["Places|Japan|Kyoto", "food"]);
    }

    #[test]
    fn only_changed_marks_are_written() {
        let old = Marks {
            rating: 2,
            keywords: vec!["a".into()],
            ..Default::default()
        };
        let new = Marks {
            rating: 3,
            ..old.clone()
        };
        let c = new.changes_from(&old);
        assert_eq!(c, vec![(Prop::Rating, Some(Value::Text("3".into())))]);
    }

    #[test]
    fn unknown_label_text_is_left_alone() {
        // A custom Lightroom label set ("Approved") isn't ours to interpret;
        // it reads as no label and is untouched unless the label changes.
        let mut p = HashMap::new();
        p.insert(Prop::Label, Value::Text("Approved".into()));
        let m = Marks::from_props(&p);
        assert_eq!(m.label, None);
        let with_rating = Marks {
            rating: 1,
            ..m.clone()
        };
        assert!(
            with_rating
                .changes_from(&m)
                .iter()
                .all(|(p, _)| *p != Prop::Label)
        );
    }
}
