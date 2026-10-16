# Test fixtures

## Synthetic (generated, used by CI)

Tests build a small fake library in a temp folder with
`shoebox_fixtures::build_library` (see `crates/shoebox-fixtures/src/lib.rs` for
the layout): NEF-structured TIFFs with a full-size `JpgFromRaw` in a SubIFD,
byte-identical carved previews, Lightroom-style TIFF exports and a `-2`
variant, recompressed and rotated copies, an unrelated image, and a sidecar.

To write it to disk for manual CLI runs:

    cargo run -p shoebox-fixtures -- /tmp/shoebox-fixtures

## Real files (gitignored, optional)

Put real camera files in `tests/fixtures/real/`. Tests in
`crates/shoebox-core/tests/real_fixtures.rs` run over whatever is there and
skip when the folder is absent. Suggested set from the spec:

- a Z f NEF and its in-camera JPG
- a Lightroom TIFF export and its `-2` virtual-copy export
- a carved preview JPEG with no EXIF
- a DxO PhotoLab sidecar and a digiKam sidecar (for M3 golden tests)

These are copies; never point tests at the working library.
