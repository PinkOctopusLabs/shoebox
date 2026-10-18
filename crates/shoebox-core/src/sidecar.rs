//! Where a photo's marks come from, and which file a change is written to.
//!
//! Reading merges, property by property, in increasing precedence:
//! EXIF → XMP embedded in the image → `name.xmp` (Lightroom style) →
//! `name.ext.xmp` (per file). Writing goes to `name.ext.xmp` (and, if the
//! setting is on, also to `name.xmp` for RAW files), and only the marks that
//! changed are written, so marks that live elsewhere keep working.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use shoebox_decode::Format;

use crate::error::{Result, io};
use crate::marks::Marks;
use crate::xmp::{self, Prop, Value};

/// `name.ext.xmp`: one per file, so a NEF, JPG and TIFF never share one.
pub fn per_file(image: &Path) -> PathBuf {
    let mut name = image.file_name().unwrap_or_default().to_os_string();
    name.push(".xmp");
    image.with_file_name(name)
}

/// `name.xmp` (Lightroom/DxO style), shared by a frame's files.
pub fn per_stem(image: &Path) -> PathBuf {
    image.with_extension("xmp")
}

/// Sidecars present on disk for `image`: (per-stem, per-file), found
/// case-insensitively (`.xmp` or `.XMP`) via `exists`.
pub fn present(image: &Path, exists: &dyn Fn(&Path) -> bool) -> (Option<PathBuf>, Option<PathBuf>) {
    let find = |p: PathBuf| -> Option<PathBuf> {
        if exists(&p) {
            return Some(p);
        }
        let upper = p.with_extension("XMP");
        exists(&upper).then_some(upper)
    };
    (find(per_stem(image)), find(per_file(image)))
}

/// EXIF Rating (tag 0x4746, written by cameras and Windows).
fn exif_rating(data: &[u8]) -> Option<String> {
    let e = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(data))
        .ok()?;
    let f = e.get_field(exif::Tag(exif::Context::Tiff, 0x4746), exif::In::PRIMARY)?;
    f.value.get_uint(0).map(|v| v.to_string())
}

/// Owned properties from the image itself: EXIF, then embedded XMP.
/// Reads only the start of the file (where both live in practice).
pub fn image_props(image: &Path, format: Format) -> HashMap<Prop, Value> {
    let mut props = HashMap::new();
    let Ok(f) = std::fs::File::open(image) else {
        return props;
    };
    let mut prefix = Vec::new();
    if f.take(512 * 1024).read_to_end(&mut prefix).is_err() {
        return props;
    }
    if let Some(r) = exif_rating(&prefix) {
        props.insert(Prop::Rating, Value::Text(r));
    }
    if let Some(r) = shoebox_decode::embedded_xmp(format, &prefix)
        && let Ok(p) = xmp::read(&prefix[r])
    {
        props.extend(p);
    }
    props
}

/// A photo's effective marks and the sidecars they came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Loaded {
    pub marks: Marks,
    /// The per-file sidecar, if it exists (where writes go).
    pub sidecar: Option<PathBuf>,
}

/// Merge the layers and interpret them as marks. `image`: None leaves out
/// what's in the image itself (a cloud-only file isn't read).
pub fn load(
    image: Option<&Path>,
    format: Format,
    stem: Option<&Path>,
    file: Option<&Path>,
) -> Result<Loaded> {
    let mut props = image.map(|i| image_props(i, format)).unwrap_or_default();
    for side in [stem, file].into_iter().flatten() {
        let bytes = std::fs::read(side).map_err(io(side))?;
        match xmp::read(&bytes) {
            Ok(p) => props.extend(p),
            Err(e) => tracing::warn!("ignoring unreadable sidecar {}: {e}", side.display()),
        }
    }
    Ok(Loaded {
        marks: Marks::from_props(&props),
        sidecar: file.map(Path::to_path_buf),
    })
}

/// The sidecar writes that change a photo's marks from `old` (as shown) to
/// `new`: edits to `name.ext.xmp` (created if needed) and, when
/// `also_stem` (the RAW setting), to `name.xmp` as well.
pub fn plan_write(
    image: &Path,
    old: &Marks,
    new: &Marks,
    also_stem: bool,
) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let changes = new.changes_from(old);
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    let (stem, file) = present(image, &|p| p.exists());
    let mut targets = vec![file.unwrap_or_else(|| per_file(image))];
    if also_stem {
        targets.push(stem.unwrap_or_else(|| per_stem(image)));
    }
    targets
        .into_iter()
        .map(|path| {
            let existing = match std::fs::read(&path) {
                Ok(b) => Some(b),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(io(&path)(e)),
            };
            let bytes = xmp::edit(existing.as_deref(), &changes)?;
            Ok((path, bytes))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marks::{Label, Pick};

    #[test]
    fn sidecar_names() {
        let p = Path::new("/a/DSC_0001.NEF");
        assert_eq!(per_file(p), Path::new("/a/DSC_0001.NEF.xmp"));
        assert_eq!(per_stem(p), Path::new("/a/DSC_0001.xmp"));
    }

    #[test]
    fn precedence_and_minimal_writes() {
        let t = tempfile::tempdir().unwrap();
        let img = t.path().join("a.jpg");
        std::fs::write(
            &img,
            shoebox_fixtures::jpeg(&shoebox_fixtures::scene(1, 64, 48), 80).unwrap(),
        )
        .unwrap();
        // Lightroom-style name.xmp says rating 2 and a keyword.
        let stem = per_stem(&img);
        let lr = xmp::edit(
            None,
            &[
                (Prop::Rating, Some(Value::Text("2".into()))),
                (
                    Prop::HierarchicalSubject,
                    Some(Value::Bag(vec!["Places|Rome".into()])),
                ),
            ],
        )
        .unwrap();
        std::fs::write(&stem, &lr).unwrap();

        let (s, f) = present(&img, &|p| p.exists());
        let before = load(Some(&img), Format::Jpeg, s.as_deref(), f.as_deref()).unwrap();
        assert_eq!(before.marks.rating, 2);
        assert_eq!(before.marks.keywords, vec!["Places|Rome"]);

        // Change only the label: the per-file sidecar gets just the label;
        // the rating and keyword still come from name.xmp.
        let new = Marks {
            label: Some(Label::Green),
            pick: Some(Pick::Pick),
            ..before.marks.clone()
        };
        let writes = plan_write(&img, &before.marks, &new, false).unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, per_file(&img));
        let written = xmp::read(&writes[0].1).unwrap();
        assert!(
            !written.contains_key(&Prop::Rating)
                && !written.contains_key(&Prop::HierarchicalSubject)
        );
        std::fs::write(&writes[0].0, &writes[0].1).unwrap();
        let (s, f) = present(&img, &|p| p.exists());
        let after = load(Some(&img), Format::Jpeg, s.as_deref(), f.as_deref()).unwrap();
        assert_eq!(after.marks, new);
        assert_eq!(std::fs::read(&stem).unwrap(), lr, "name.xmp untouched");
    }
}
