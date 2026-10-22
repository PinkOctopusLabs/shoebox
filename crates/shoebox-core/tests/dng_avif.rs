//! DNG and AVIF files in the library: scanned, read, hashed and stacked.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use shoebox_core::cache::Cache;
use shoebox_core::catalog::Catalog;
use shoebox_core::scan::{Role, Root};
use shoebox_decode::Format;
use shoebox_fixtures::{Dng, Shot};

fn put(p: &Path, b: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

fn shot(datetime: &'static str, orientation: u16) -> Shot {
    Shot {
        datetime,
        subsec: "40",
        orientation,
        serial: "7001234",
    }
}

/// A camera DNG: Z f-like raw data, a JPEG preview of `scene` and EXIF.
fn camera_dng(scene: u64, shot: Shot) -> Vec<u8> {
    let img = shoebox_fixtures::scene(scene, 300, 200);
    Dng {
        preview: Some(shoebox_fixtures::jpeg(&img, 90).unwrap()),
        shot: Some(shot),
        make: "NIKON CORPORATION".into(),
        model: "NIKON Z f".into(),
        ..Dng::new(300, 200)
    }
    .write(|x, y| 1500 + ((x * 7 + y * 13) % 600) as u16 * 15)
}

/// An AVIF made by macOS from a JPEG (None where `sips` can't).
fn avif(dir: &Path, scene: u64) -> Option<Vec<u8>> {
    let jpg = dir.join(format!("src{scene}.jpg"));
    let out = dir.join(format!("src{scene}.avif"));
    let img = shoebox_fixtures::scene(scene, 240, 160);
    std::fs::write(&jpg, shoebox_fixtures::jpeg(&img, 92).unwrap()).unwrap();
    let ok = std::process::Command::new("sips")
        .args(["-s", "format", "avif"])
        .arg(&jpg)
        .arg("--out")
        .arg(&out)
        .output()
        .is_ok_and(|o| o.status.success());
    ok.then(|| std::fs::read(&out).unwrap())
}

#[test]
fn dngs_and_avifs_are_photos() {
    let t = tempfile::tempdir().unwrap();
    let lib = t.path().join("lib");
    let day = lib.join("2026/2026-05-01");
    let nef_shot = shot("2026:05:01 09:00:00", 1);
    let preview = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(1, 300, 200), 90).unwrap();
    let small = shoebox_fixtures::jpeg(&shoebox_fixtures::scene(1, 160, 120), 80).unwrap();
    // A NEF and the DNG converted from it, by name.
    put(
        &day.join("DSC_0100.NEF"),
        &shoebox_fixtures::nef(&preview, (300, 200), &small, &nef_shot),
    );
    put(&day.join("DSC_0100.dng"), &camera_dng(1, nef_shot.clone()));
    // A camera's DNG and its JPEG, on its side.
    let turned = shot("2026:05:01 10:00:00", 6);
    put(&day.join("PXL_0200.dng"), &camera_dng(2, turned.clone()));
    put(
        &day.join("PXL_0200.jpg"),
        &shoebox_fixtures::with_exif(
            &shoebox_fixtures::jpeg(&shoebox_fixtures::scene(2, 300, 200), 90).unwrap(),
            &turned,
        ),
    );
    let avif = avif(t.path(), 3);
    if let Some(a) = &avif {
        put(&day.join("phone.avif"), a);
    }

    let roots = vec![Root::new(&lib, Role::Scan).unwrap()];
    let mut cache = Cache::open(&t.path().join("cache.sqlite")).unwrap();
    let (c, s) = Catalog::refresh(&mut cache, roots.clone(), &|_, _| {}).unwrap();
    assert_eq!(s.failed, 0);
    let find = |c: &Catalog, name: &str| -> usize {
        c.photos
            .iter()
            .position(|p| p.rel.ends_with(name))
            .unwrap_or_else(|| panic!("{name} wasn't scanned"))
    };
    let dng = &c.photos[find(&c, "DSC_0100.dng")];
    assert_eq!(dng.format, Format::Dng);
    assert_eq!(dng.meta.exif.model.as_deref(), Some("NIKON Z f"));
    assert_eq!(
        dng.meta.exif.capture_time.as_deref(),
        Some("2026:05:01 09:00:00")
    );
    assert_eq!((dng.meta.width, dng.meta.height), (Some(300), Some(200)));
    let pxl = &c.photos[find(&c, "PXL_0200.dng")];
    assert_eq!(pxl.meta.orientation(), 6);
    assert_eq!((pxl.meta.width, pxl.meta.height), (Some(200), Some(300)));

    // Stacks: the NEF leads its DNG; a DNG leads its JPEG.
    let stack = |c: &Catalog, name: &str| c.stacks[c.stack_of[find(c, name)]].clone();
    let st = stack(&c, "DSC_0100.dng");
    assert_eq!(c.photos[st.primary].format, Format::Nef);
    assert_eq!(st.members.len(), 2);
    let st = stack(&c, "PXL_0200.jpg");
    assert_eq!(c.photos[st.primary].format, Format::Dng);
    assert_eq!(st.badge(&c.photos), "DNG+JPG");

    if avif.is_some() {
        let a = &c.photos[find(&c, "phone.avif")];
        assert_eq!(a.format, Format::Avif);
        assert_eq!((a.meta.width, a.meta.height), (Some(240), Some(160)));
    } else {
        eprintln!("sips can't write AVIF here; skipped its part");
    }

    // A DNG exported beside the NEF joins its stack as a variant.
    let export = day.join("DSC_0100_Linear DNG.dng");
    put(&export, &camera_dng(1, shot("2026:05:01 11:00:00", 1)));
    let (mut c, _) = Catalog::refresh(&mut cache, roots.clone(), &|_, _| {}).unwrap();
    c.set_links(vec![shoebox_core::stacks::Link {
        file: export.canonicalize().unwrap(),
        of: day.join("DSC_0100.NEF").canonicalize().unwrap(),
    }]);
    let st = stack(&c, "DSC_0100.NEF");
    let variants: Vec<PathBuf> = st
        .variants
        .iter()
        .map(|&v| c.photos[v].path.clone())
        .collect();
    assert_eq!(variants, [export.canonicalize().unwrap()]);

    // Again from the cache: the same photos, nothing read.
    let (again, s) = Catalog::refresh(&mut cache, roots, &|_, _| {}).unwrap();
    assert_eq!((s.read, again.photos.len()), (0, c.photos.len()));
}

/// The image-data hash of a DNG is its raw data's: the same raw with other
/// metadata and preview matches; a perceptual hash comes from its preview.
#[test]
fn dng_hashes() {
    let a = camera_dng(4, shot("2026:05:01 09:00:00", 1));
    let b = camera_dng(5, shot("2027:01:01 12:00:00", 3));
    let ha = shoebox_core::hash::analyze_bytes(Format::Dng, &a);
    let hb = shoebox_core::hash::analyze_bytes(Format::Dng, &b);
    assert_eq!(ha.error, None);
    assert_ne!(ha.blake3, hb.blake3);
    assert!(ha.image_data_hash.is_some());
    assert_eq!(ha.image_data_hash, hb.image_data_hash);
    assert_eq!(ha.hashes.len(), 4);
    assert_eq!((ha.width, ha.height), (Some(300), Some(200)));
    // Other raw data: another hash.
    let other = Dng::new(300, 200).write(|x, _| 1500 + x as u16);
    let hc = shoebox_core::hash::analyze_bytes(Format::Dng, &other);
    assert_ne!(hc.image_data_hash, ha.image_data_hash);
}
