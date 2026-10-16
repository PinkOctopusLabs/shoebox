//! Crafted headers must be rejected before any large allocation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use shoebox_decode::{DecodeError, Format};
use shoebox_fixtures::{TiffWriter, Val};

#[test]
fn jpeg_claiming_huge_dimensions_is_refused() {
    let img = shoebox_fixtures::scene(5, 64, 48);
    let mut j = shoebox_fixtures::jpeg(&img, 80).unwrap();
    // Patch the SOF0 frame header to claim 30000×30000 (900 MP).
    let sof = j.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    j[sof + 5..sof + 7].copy_from_slice(&30000u16.to_be_bytes());
    j[sof + 7..sof + 9].copy_from_slice(&30000u16.to_be_bytes());
    let err = shoebox_decode::decode_luma(Format::Jpeg, &j).unwrap_err();
    assert!(matches!(err, DecodeError::Unsupported(_)), "{err}");
}

#[test]
fn tiff_claiming_huge_dimensions_is_refused() {
    let mut w = TiffWriter::new();
    let strip = w.blob(&[0u8; 16]);
    let ifd0 = w.ifd(
        vec![
            (0x0100, Val::Long(vec![40_000])),
            (0x0101, Val::Long(vec![40_000])),
            (0x0102, Val::Short(vec![8, 8, 8])),
            (0x0103, Val::Short(vec![1])),
            (0x0106, Val::Short(vec![2])),
            (0x0111, Val::Long(vec![strip])),
            (0x0115, Val::Short(vec![3])),
            (0x0116, Val::Long(vec![40_000])),
            (0x0117, Val::Long(vec![16])),
        ],
        0,
    );
    let t = w.finish(ifd0);
    assert!(shoebox_decode::decode_luma(Format::Tiff, &t).is_err());
}

#[test]
fn absurd_sub_ifd_list_is_refused() {
    let mut w = TiffWriter::new();
    let ifd0 = w.ifd(vec![(0x014A, Val::Long(vec![8; 2_000_000]))], 0);
    let t = w.finish(ifd0);
    assert!(shoebox_decode::nef::jpg_from_raw(&t).is_err());
}

#[test]
fn many_sub_ifds_are_capped_not_walked() {
    // 500k SubIFD pointers (under the value cap) all pointing at IFD0 itself.
    let mut w = TiffWriter::new();
    let placeholder = w.ifd(vec![(0x00FE, Val::Long(vec![0]))], 0);
    let ifd0 = w.ifd(vec![(0x014A, Val::Long(vec![placeholder; 500_000]))], 0);
    let t = w.finish(ifd0);
    let start = std::time::Instant::now();
    assert_eq!(shoebox_decode::nef::jpg_from_raw(&t).unwrap(), None);
    assert!(start.elapsed().as_secs() < 2);
}
