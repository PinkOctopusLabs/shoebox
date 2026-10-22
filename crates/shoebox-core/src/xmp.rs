//! XMP sidecars: read marks in any serialisation, and edit by splicing.
//!
//! Editing never re-serialises the document. The original bytes are copied
//! through unchanged except for the byte ranges of the properties Shoebox
//! owns (removed) and one insertion point (the new values), so properties
//! written by DxO, digiKam, Lightroom, LightCraft or anything else survive
//! byte for byte, including ones we don't understand.
//!
//! Owned properties (spec, Organising section):
//! - `xmp:Rating`
//! - `digiKam:PickLabel` and `shoebox:Pick`
//! - `xmp:Label` and `digiKam:ColorLabel`
//! - `dc:subject`, `lr:hierarchicalSubject`, `digiKam:TagsList`
//! - `Iptc4xmpExt:Event` (events)

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use crate::error::{CoreError, Result};

pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
pub const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
pub const NS_LR: &str = "http://ns.adobe.com/lightroom/1.0/";
pub const NS_DIGIKAM: &str = "http://www.digikam.org/ns/1.0/";
pub const NS_IPTC_EXT: &str = "http://iptc.org/std/Iptc4xmpExt/2008-02-29/";
pub const NS_SHOEBOX: &str = "http://ns.crankyunicorn.com/shoebox/1.0/";
pub const NS_EXIF: &str = "http://ns.adobe.com/exif/1.0/";
pub const NS_PHOTOSHOP: &str = "http://ns.adobe.com/photoshop/1.0/";

/// Preferred prefixes when we have to declare a namespace ourselves.
fn default_prefix(ns: &str) -> &'static str {
    match ns {
        NS_XMP => "xmp",
        NS_DC => "dc",
        NS_LR => "lr",
        NS_DIGIKAM => "digiKam",
        NS_IPTC_EXT => "Iptc4xmpExt",
        NS_EXIF => "exif",
        NS_PHOTOSHOP => "photoshop",
        NS_SHOEBOX => "shoebox",
        _ => "ns",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Prop {
    Rating,
    PickLabel,
    Pick,
    Label,
    ColorLabel,
    Subject,
    HierarchicalSubject,
    TagsList,
    Event,
    /// A location added in another app, or by an iCloud Photos import
    /// (written only into a copy's new sidecar).
    GpsLatitude,
    GpsLongitude,
    /// When it was taken, set in another app or by an iCloud Photos import
    /// (ISO 8601; written only into a copy's new sidecar). Read from
    /// sidecars only; it wins over the file's EXIF.
    DateTimeOriginal,
    DateCreated,
}

impl Prop {
    pub const ALL: [Prop; 13] = [
        Prop::Rating,
        Prop::PickLabel,
        Prop::Pick,
        Prop::Label,
        Prop::ColorLabel,
        Prop::Subject,
        Prop::HierarchicalSubject,
        Prop::TagsList,
        Prop::Event,
        Prop::GpsLatitude,
        Prop::GpsLongitude,
        Prop::DateTimeOriginal,
        Prop::DateCreated,
    ];

    fn ns(self) -> &'static str {
        match self {
            Prop::Rating | Prop::Label => NS_XMP,
            Prop::PickLabel | Prop::ColorLabel | Prop::TagsList => NS_DIGIKAM,
            Prop::Pick => NS_SHOEBOX,
            Prop::Subject => NS_DC,
            Prop::HierarchicalSubject => NS_LR,
            Prop::Event => NS_IPTC_EXT,
            Prop::GpsLatitude | Prop::GpsLongitude | Prop::DateTimeOriginal => NS_EXIF,
            Prop::DateCreated => NS_PHOTOSHOP,
        }
    }

    fn local(self) -> &'static str {
        match self {
            Prop::Rating => "Rating",
            Prop::PickLabel => "PickLabel",
            Prop::Pick => "Pick",
            Prop::Label => "Label",
            Prop::ColorLabel => "ColorLabel",
            Prop::Subject => "subject",
            Prop::HierarchicalSubject => "hierarchicalSubject",
            Prop::TagsList => "TagsList",
            Prop::Event => "Event",
            Prop::GpsLatitude => "GPSLatitude",
            Prop::GpsLongitude => "GPSLongitude",
            Prop::DateTimeOriginal => "DateTimeOriginal",
            Prop::DateCreated => "DateCreated",
        }
    }

    fn find(ns: &str, local: &str) -> Option<Prop> {
        Prop::ALL
            .into_iter()
            .find(|p| p.ns() == ns && p.local() == local)
    }
}

/// A property value in one of the XMP forms we write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Bag(Vec<String>),
    Seq(Vec<String>),
    /// Language alternative; we write `x-default` only.
    LangAlt(String),
}

impl Value {
    /// All strings in the value, regardless of form.
    pub fn items(&self) -> Vec<String> {
        match self {
            Value::Text(t) | Value::LangAlt(t) => vec![t.clone()],
            Value::Bag(v) | Value::Seq(v) => v.clone(),
        }
    }

    pub fn first(&self) -> Option<String> {
        self.items().into_iter().next()
    }
}

// ---------------------------------------------------------------------------
// Parsing: one pass that records owned property locations and values.

#[derive(Debug, Default)]
struct Scan {
    /// Values found, by property (first occurrence wins).
    values: HashMap<Prop, Value>,
    /// Every owned occurrence and the byte range that removes it.
    removals: Vec<(Prop, Range<usize>)>,
    /// The first rdf:Description: its start-tag range, whether it's
    /// self-closing, the byte offset of its end tag, and its qualified name.
    first_desc: Option<Desc>,
    /// Offset of `</rdf:RDF>` (to add a Description if there's none).
    rdf_end: Option<usize>,
    rdf_qname: Option<String>,
}

#[derive(Debug, Clone)]
struct Desc {
    start_tag: Range<usize>,
    empty: bool,
    end_tag_at: Option<usize>,
    qname: String,
    /// Namespace bindings in scope at this element (prefix → uri).
    scope: HashMap<String, String>,
    /// Indentation of the start tag's line.
    indent: String,
}

fn xml_err(e: impl std::fmt::Display) -> CoreError {
    CoreError::Invalid(format!("XMP: {e}"))
}

fn bound(r: &ResolveResult<'_>) -> Option<String> {
    match r {
        ResolveResult::Bound(ns) => std::str::from_utf8(ns.as_ref().as_bytes())
            .ok()
            .map(str::to_owned),
        _ => None,
    }
}

fn decl_bindings(start: &BytesStart<'_>) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for a in start.attributes().with_checks(false).flatten() {
        let key: String = a.key.as_ref().to_owned();
        let val = a
            .normalized_value(quick_xml::XmlVersion::default())
            .map(|v| v.into_owned())
            .unwrap_or_default();
        if let Some(p) = key.strip_prefix("xmlns:") {
            m.insert(p.to_owned(), val);
        } else if key == "xmlns" {
            m.insert(String::new(), val);
        }
    }
    m
}

/// Byte range of attribute `qname="…"` (with its leading whitespace) inside
/// the start tag `tag` that begins at absolute offset `base`.
fn attr_range(tag: &[u8], base: usize, qname: &[u8]) -> Option<Range<usize>> {
    let mut i = 0;
    while let Some(off) = find(&tag[i..], qname) {
        let at = i + off;
        let before_ws = at > 0 && tag[at - 1].is_ascii_whitespace();
        let mut j = at + qname.len();
        while j < tag.len() && tag[j].is_ascii_whitespace() {
            j += 1;
        }
        if before_ws && tag.get(j) == Some(&b'=') {
            j += 1;
            while j < tag.len() && tag[j].is_ascii_whitespace() {
                j += 1;
            }
            let quote = *tag.get(j)?;
            if quote == b'"' || quote == b'\'' {
                let close = j + 1 + tag[j + 1..].iter().position(|&c| c == quote)?;
                let mut s = at;
                while s > 0 && tag[s - 1].is_ascii_whitespace() {
                    s -= 1;
                }
                return Some(base + s..base + close + 1);
            }
        }
        i = at + 1;
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Extend a removal backwards over the whitespace that precedes it on its
/// line (and the newline), so removed elements don't leave blank lines.
fn widen_to_line(doc: &[u8], r: Range<usize>) -> Range<usize> {
    let mut s = r.start;
    while s > 0 && (doc[s - 1] == b' ' || doc[s - 1] == b'\t') {
        s -= 1;
    }
    if s > 0 && doc[s - 1] == b'\n' {
        s -= 1;
        if s > 0 && doc[s - 1] == b'\r' {
            s -= 1;
        }
    } else {
        s = r.start;
    }
    s..r.end
}

fn line_indent(doc: &[u8], at: usize) -> String {
    let line_start = doc[..at]
        .iter()
        .rposition(|&c| c == b'\n')
        .map_or(0, |p| p + 1);
    doc[line_start..at]
        .iter()
        .take_while(|c| **c == b' ' || **c == b'\t')
        .map(|&c| c as char)
        .collect()
}

fn scan(doc: &[u8]) -> Result<Scan> {
    let mut r = NsReader::from_reader(doc);
    r.config_mut().trim_text(false);
    let mut out = Scan::default();
    // Element stack: (resolved ns, local, qname, start offset, bindings in scope).
    let mut stack: Vec<(Option<String>, String, String, usize)> = Vec::new();
    let mut scopes: Vec<HashMap<String, String>> = vec![HashMap::new()];
    // Owned property being read: (prop, start offset, depth, collected items, container kind).
    struct Open {
        prop: Prop,
        start: usize,
        depth: usize,
        text: String,
        items: Vec<String>,
        kind: Option<&'static str>,
        in_li: bool,
    }
    let mut open: Option<Open> = None;
    let mut desc_depth: Option<usize> = None;
    let mut first_desc_open = false;

    loop {
        let pos = r.buffer_position() as usize;
        let (res, ev) = r.read_resolved_event().map_err(xml_err)?;
        let ns = bound(&res);
        let end = r.buffer_position() as usize;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                let local: String = e.local_name().as_ref().to_owned();
                let qname: String = e.name().as_ref().to_owned();
                let mut scope = scopes.last().cloned().unwrap_or_default();
                scope.extend(decl_bindings(e));
                let depth = stack.len();

                if ns.as_deref() == Some(NS_RDF) && local == "RDF" {
                    out.rdf_qname = Some(qname.clone());
                }
                if ns.as_deref() == Some(NS_RDF) && local == "Description" {
                    // Owned properties written as attributes on the Description.
                    for a in e.attributes().with_checks(false).flatten() {
                        let (ares, alocal) = r.resolver().resolve_attribute(a.key);
                        let Some(ans) = bound(&ares) else { continue };
                        let Some(prop) = Prop::find(&ans, alocal.as_ref()) else {
                            continue;
                        };
                        let v = a
                            .normalized_value(quick_xml::XmlVersion::default())
                            .map(|v| v.into_owned())
                            .unwrap_or_default();
                        out.values.entry(prop).or_insert(Value::Text(v));
                        if let Some(rg) = attr_range(&doc[pos..end], pos, a.key.as_ref().as_bytes())
                        {
                            out.removals.push((prop, rg));
                        }
                    }
                    if out.first_desc.is_none() {
                        out.first_desc = Some(Desc {
                            start_tag: pos..end,
                            empty,
                            end_tag_at: None,
                            qname: qname.clone(),
                            scope: scope.clone(),
                            indent: line_indent(doc, pos),
                        });
                        first_desc_open = !empty;
                    }
                    if !empty {
                        desc_depth = Some(depth);
                    }
                } else if open.is_none()
                    && desc_depth == Some(depth.wrapping_sub(1))
                    && let Some(prop) = ns.as_deref().and_then(|n| Prop::find(n, &local))
                {
                    // An owned property element directly inside a Description.
                    let mut text = String::new();
                    if empty {
                        // e.g. <xmp:Rating rdf:value="3"/> — rare; take any attribute value.
                        if let Some(a) = e.attributes().with_checks(false).flatten().next() {
                            text = a
                                .normalized_value(quick_xml::XmlVersion::default())
                                .map(|v| v.into_owned())
                                .unwrap_or_default();
                        }
                        out.values.entry(prop).or_insert(Value::Text(text));
                        out.removals.push((prop, widen_to_line(doc, pos..end)));
                    } else {
                        open = Some(Open {
                            prop,
                            start: pos,
                            depth,
                            text,
                            items: Vec::new(),
                            kind: None,
                            in_li: false,
                        });
                    }
                } else if let Some(o) = open.as_mut()
                    && ns.as_deref() == Some(NS_RDF)
                {
                    match local.as_str() {
                        "Bag" => o.kind = Some("Bag"),
                        "Seq" => o.kind = Some("Seq"),
                        "Alt" => o.kind = Some("Alt"),
                        "li" if !empty => {
                            o.in_li = true;
                            o.text.clear();
                        }
                        _ => {}
                    }
                }
                if !empty {
                    stack.push((ns, local, qname, pos));
                    scopes.push(scope);
                }
            }
            Event::Text(ref t) => {
                if let Some(o) = open.as_mut() {
                    // Entities arrive as separate GeneralRef events.
                    o.text.push_str(t);
                }
            }
            Event::GeneralRef(ref g) => {
                if let Some(o) = open.as_mut() {
                    let name: String = (**g).to_owned();
                    let ch = match name.as_str() {
                        "amp" => "&".to_owned(),
                        "lt" => "<".to_owned(),
                        "gt" => ">".to_owned(),
                        "quot" => "\"".to_owned(),
                        "apos" => "'".to_owned(),
                        n => n
                            .strip_prefix("#x")
                            .and_then(|h| u32::from_str_radix(h, 16).ok())
                            .or_else(|| n.strip_prefix('#').and_then(|d| d.parse().ok()))
                            .and_then(char::from_u32)
                            .map(String::from)
                            .unwrap_or_default(),
                    };
                    o.text.push_str(&ch);
                }
            }
            Event::CData(ref c) => {
                if let Some(o) = open.as_mut() {
                    o.text.push_str(c);
                }
            }
            Event::End(_) => {
                let depth = stack.len().saturating_sub(1);
                let (ens, elocal, _, _) = stack.pop().unwrap_or_default();
                scopes.pop();
                if let Some(o) = open.as_mut() {
                    if ens.as_deref() == Some(NS_RDF) && elocal == "li" && o.in_li {
                        o.items.push(std::mem::take(&mut o.text));
                        o.in_li = false;
                    } else if depth == o.depth {
                        let value = match o.kind {
                            Some("Bag") => Value::Bag(std::mem::take(&mut o.items)),
                            Some("Seq") => Value::Seq(std::mem::take(&mut o.items)),
                            Some(_) => Value::LangAlt(o.items.first().cloned().unwrap_or_default()),
                            None => Value::Text(o.text.trim().to_owned()),
                        };
                        out.values.entry(o.prop).or_insert(value);
                        out.removals
                            .push((o.prop, widen_to_line(doc, o.start..end)));
                        open = None;
                    }
                }
                if ens.as_deref() == Some(NS_RDF) && elocal == "Description" {
                    if first_desc_open
                        && let Some(d) = out.first_desc.as_mut()
                        && d.end_tag_at.is_none()
                    {
                        d.end_tag_at = Some(pos);
                        first_desc_open = false;
                    }
                    desc_depth = None;
                }
                if ens.as_deref() == Some(NS_RDF) && elocal == "RDF" {
                    out.rdf_end = Some(pos);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Every owned property present, in whatever form it was written.
pub fn read(doc: &[u8]) -> Result<HashMap<Prop, Value>> {
    Ok(scan(doc)?.values)
}

// ---------------------------------------------------------------------------
// Writing

fn esc(s: &str) -> String {
    quick_xml::escape::escape(s).into_owned()
}

fn render_value(prop_q: &str, v: &Value, indent: &str, rdf: &str) -> String {
    let inner = format!("{indent} ");
    match v {
        Value::Text(t) => format!("{indent}<{prop_q}>{}</{prop_q}>\n", esc(t)),
        Value::LangAlt(t) => format!(
            "{indent}<{prop_q}>\n{inner}<{rdf}:Alt>\n{inner} <{rdf}:li xml:lang=\"x-default\">{}</{rdf}:li>\n{inner}</{rdf}:Alt>\n{indent}</{prop_q}>\n",
            esc(t)
        ),
        Value::Bag(items) | Value::Seq(items) => {
            let kind = if matches!(v, Value::Bag(_)) {
                "Bag"
            } else {
                "Seq"
            };
            let lis: String = items
                .iter()
                .map(|i| format!("{inner} <{rdf}:li>{}</{rdf}:li>\n", esc(i)))
                .collect();
            format!(
                "{indent}<{prop_q}>\n{inner}<{rdf}:{kind}>\n{lis}{inner}</{rdf}:{kind}>\n{indent}</{prop_q}>\n"
            )
        }
    }
}

const TEMPLATE: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Shoebox\">\n \
<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
<rdf:Description rdf:about=\"\">\n  \
</rdf:Description>\n \
</rdf:RDF>\n\
</x:xmpmeta>\n\
<?xpacket end=\"w\"?>\n";

/// Apply changes to a sidecar (or create one). `changes` maps each owned
/// property to its new value, or None to remove it; properties not listed
/// are left exactly as they are. Everything that isn't an owned property
/// being changed is copied byte for byte.
pub fn edit(existing: Option<&[u8]>, changes: &[(Prop, Option<Value>)]) -> Result<Vec<u8>> {
    let doc: &[u8] = existing.unwrap_or(TEMPLATE.as_bytes());
    let s = scan(doc)?;
    let changing: BTreeSet<Prop> = changes.iter().map(|(p, _)| *p).collect();

    // Remove every occurrence of each property being changed.
    let mut removals: Vec<Range<usize>> = s
        .removals
        .iter()
        .filter(|(p, _)| changing.contains(p))
        .map(|(_, r)| r.clone())
        .collect();
    removals.sort_by_key(|r| r.start);

    let desc = match &s.first_desc {
        Some(d) => d.clone(),
        None => return edit_without_description(doc, &s, changes),
    };
    let rdf = desc
        .qname
        .split(':')
        .next()
        .filter(|_| desc.qname.contains(':'))
        .unwrap_or("rdf")
        .to_owned();

    // Namespaces: reuse an in-scope prefix, else declare one on the Description.
    let mut decls = String::new();
    let mut prefix_for: HashMap<&'static str, String> = HashMap::new();
    let mut taken: HashMap<String, String> = desc.scope.clone();
    for (p, v) in changes {
        if v.is_none() || prefix_for.contains_key(p.ns()) {
            continue;
        }
        let ns = p.ns();
        let prefix = match taken.iter().find(|(_, u)| u.as_str() == ns) {
            Some((pre, _)) => pre.clone(),
            None => {
                let base = default_prefix(ns).to_owned();
                let mut cand = base.clone();
                let mut n = 1;
                while taken.contains_key(&cand) {
                    n += 1;
                    cand = format!("{base}{n}");
                }
                decls.push_str(&format!("\n{}    xmlns:{cand}=\"{ns}\"", desc.indent));
                taken.insert(cand.clone(), ns.to_owned());
                cand
            }
        };
        prefix_for.insert(ns, prefix);
    }

    let child_indent = format!("{} ", desc.indent);
    let mut body = String::new();
    for (p, v) in changes {
        if let Some(v) = v {
            let q = format!("{}:{}", prefix_for[p.ns()], p.local());
            body.push_str(&render_value(&q, v, &child_indent, &rdf));
        }
    }

    // Splice: copy doc, skipping removals, inserting decls before the start
    // tag's '>' and the body before the Description's end tag.
    let close_len = if desc.empty { 2 } else { 1 }; // "/>" or ">"
    let decl_at = desc.start_tag.end - close_len;
    let mut inserts: Vec<(usize, String)> = Vec::new();
    if !decls.is_empty() {
        inserts.push((decl_at, decls));
    }
    if desc.empty {
        // `<rdf:Description …/>` → `<rdf:Description …>` body `</rdf:Description>`.
        inserts.push((
            decl_at,
            format!(">\n{body}{}</{}>", desc.indent, desc.qname),
        ));
        removals.push(decl_at..desc.start_tag.end);
    } else if !body.is_empty() {
        let at = desc
            .end_tag_at
            .ok_or_else(|| CoreError::Invalid("XMP: unterminated rdf:Description".into()))?;
        // Insert on its own line(s) just before the end tag's line.
        let line_start = doc[..at]
            .iter()
            .rposition(|&c| c == b'\n')
            .map_or(at, |p| p + 1);
        let only_ws = doc[line_start..at]
            .iter()
            .all(|c| *c == b' ' || *c == b'\t');
        if only_ws {
            inserts.push((line_start, body));
        } else {
            inserts.push((at, format!("\n{body}{}", desc.indent)));
        }
    }
    Ok(splice(doc, &removals, &inserts))
}

fn edit_without_description(
    doc: &[u8],
    s: &Scan,
    changes: &[(Prop, Option<Value>)],
) -> Result<Vec<u8>> {
    let at = s
        .rdf_end
        .ok_or_else(|| CoreError::Invalid("XMP: no rdf:RDF element; refusing to edit".into()))?;
    let rdf = s
        .rdf_qname
        .as_deref()
        .and_then(|q| q.split_once(':').map(|(p, _)| p))
        .unwrap_or("rdf")
        .to_owned();
    let mut decls = String::new();
    let mut seen = BTreeSet::new();
    for (p, v) in changes {
        if v.is_some() && seen.insert(p.ns()) {
            decls.push_str(&format!(
                "\n    xmlns:{}=\"{}\"",
                default_prefix(p.ns()),
                p.ns()
            ));
        }
    }
    let mut body = String::new();
    for (p, v) in changes {
        if let Some(v) = v {
            body.push_str(&render_value(
                &format!("{}:{}", default_prefix(p.ns()), p.local()),
                v,
                "   ",
                &rdf,
            ));
        }
    }
    let block =
        format!("  <{rdf}:Description {rdf}:about=\"\"{decls}>\n{body}  </{rdf}:Description>\n ");
    Ok(splice(doc, &[], &[(at, block)]))
}

fn splice(doc: &[u8], removals: &[Range<usize>], inserts: &[(usize, String)]) -> Vec<u8> {
    let mut points: Vec<usize> = removals
        .iter()
        .flat_map(|r| [r.start, r.end])
        .chain(inserts.iter().map(|(a, _)| *a))
        .collect();
    points.push(0);
    points.push(doc.len());
    points.sort_unstable();
    points.dedup();
    let mut out = Vec::with_capacity(doc.len() + 512);
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        for (at, text) in inserts {
            if *at == a {
                out.extend_from_slice(text.as_bytes());
            }
        }
        if !removals.iter().any(|r| r.start <= a && b <= r.end) {
            out.extend_from_slice(&doc[a..b]);
        }
    }
    for (at, text) in inserts {
        if *at == doc.len() {
            out.extend_from_slice(text.as_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELEMENTS: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="LightCraft">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lc="http://ns.lightcraft.app/lc/1.0/">
   <xmp:CreatorTool>LightCraft</xmp:CreatorTool>
   <xmp:Rating>2</xmp:Rating>
   <dc:subject>
    <rdf:Bag>
     <rdf:li>beach</rdf:li>
     <rdf:li>fish &amp; chips</rdf:li>
    </rdf:Bag>
   </dc:subject>
   <lc:settings>{"version":1,"wb":{"temp":6500.0}}</lc:settings>
   <lc:flag>none</lc:flag>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    /// Lightroom/Exiv2 style: simple properties as attributes, old `xap` prefix.
    const ATTRIBUTES: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xap="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:digiKam="http://www.digikam.org/ns/1.0/" xap:Rating="5" xap:Label="Blue" crs:Exposure2012="+0.35" digiKam:PickLabel="3"/>
</rdf:RDF>
</x:xmpmeta>"#;

    /// Properties split across two Descriptions, as some tools write.
    const SPLIT: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
 <rdf:Description rdf:about="" xmlns:tiff="http://ns.adobe.com/tiff/1.0/">
  <tiff:Make>NIKON CORPORATION</tiff:Make>
 </rdf:Description>
 <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
  <xmp:Rating>1</xmp:Rating>
  <lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Japan|Kyoto</rdf:li></rdf:Bag></lr:hierarchicalSubject>
 </rdf:Description>
</rdf:RDF></x:xmpmeta>"#;

    fn text(v: &str) -> Option<Value> {
        Some(Value::Text(v.into()))
    }

    #[test]
    fn reads_every_form() {
        let e = read(ELEMENTS.as_bytes()).unwrap();
        assert_eq!(e[&Prop::Rating], Value::Text("2".into()));
        assert_eq!(
            e[&Prop::Subject],
            Value::Bag(vec!["beach".into(), "fish & chips".into()])
        );
        let a = read(ATTRIBUTES.as_bytes()).unwrap();
        assert_eq!(
            a[&Prop::Rating],
            Value::Text("5".into()),
            "xap: prefix is the xmp namespace"
        );
        assert_eq!(a[&Prop::Label], Value::Text("Blue".into()));
        assert_eq!(a[&Prop::PickLabel], Value::Text("3".into()));
        let s = read(SPLIT.as_bytes()).unwrap();
        assert_eq!(
            s[&Prop::HierarchicalSubject],
            Value::Bag(vec!["Places|Japan|Kyoto".into()])
        );
    }

    #[test]
    fn element_edit_keeps_everything_else() {
        let out = edit(Some(ELEMENTS.as_bytes()), &[(Prop::Rating, text("4"))]).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert_eq!(
            read(out.as_bytes()).unwrap()[&Prop::Rating],
            Value::Text("4".into())
        );
        assert_eq!(
            out.matches("Rating").count(),
            2,
            "one element (open+close), no duplicate:\n{out}"
        );
        // Every other line survives, in order.
        let kept: Vec<&str> = ELEMENTS
            .lines()
            .filter(|l| !l.contains("xmp:Rating"))
            .collect();
        let mut it = out.lines();
        for line in kept {
            assert!(it.any(|o| o == line), "lost or reordered: {line}\n{out}");
        }
    }

    #[test]
    fn attribute_form_is_replaced_not_duplicated() {
        let out = edit(
            Some(ATTRIBUTES.as_bytes()),
            &[(Prop::Rating, text("1")), (Prop::Label, None)],
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        let v = read(out.as_bytes()).unwrap();
        assert_eq!(v[&Prop::Rating], Value::Text("1".into()));
        assert!(!v.contains_key(&Prop::Label), "label removed");
        assert!(
            out.contains(r#"crs:Exposure2012="+0.35""#),
            "foreign attribute kept:\n{out}"
        );
        assert!(
            out.contains(r#"digiKam:PickLabel="3""#),
            "untouched owned attribute kept"
        );
        assert!(
            out.contains("<xap:Rating>1</xap:Rating>"),
            "reuses the existing prefix:\n{out}"
        );
        assert!(!out.contains(r#"xap:Rating="5""#));
    }

    #[test]
    fn split_descriptions() {
        let out = edit(Some(SPLIT.as_bytes()), &[(Prop::Rating, text("3"))]).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert_eq!(out.matches(">3<").count(), 1);
        assert!(
            !out.contains(">1<"),
            "old value in the second Description removed:\n{out}"
        );
        assert!(out.contains("<tiff:Make>NIKON CORPORATION</tiff:Make>"));
        assert!(out.contains("Places|Japan|Kyoto"));
        assert_eq!(
            read(out.as_bytes()).unwrap()[&Prop::Rating],
            Value::Text("3".into())
        );
    }

    #[test]
    fn new_sidecar_and_all_value_kinds() {
        let changes = vec![
            (Prop::Rating, text("5")),
            (
                Prop::Subject,
                Some(Value::Bag(vec!["a<b".into(), "Kyoto".into()])),
            ),
            (
                Prop::TagsList,
                Some(Value::Seq(vec!["Places/Japan/Kyoto".into()])),
            ),
            (Prop::Event, Some(Value::LangAlt("Japan Trip".into()))),
            (Prop::Pick, text("pick")),
        ];
        let out = edit(None, &changes).unwrap();
        let v = read(&out).unwrap();
        for (p, val) in &changes {
            assert_eq!(
                v.get(p),
                val.as_ref(),
                "{p:?}\n{}",
                String::from_utf8_lossy(&out)
            );
        }
        // Editing again with the same values is stable.
        assert_eq!(edit(Some(&out), &changes).unwrap(), out);
    }

    #[test]
    fn refuses_non_xmp() {
        assert!(
            edit(
                Some(b"<html><body>not xmp</body></html>"),
                &[(Prop::Rating, text("1"))]
            )
            .is_err()
        );
    }
}
