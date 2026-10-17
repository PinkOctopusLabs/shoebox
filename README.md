# Shoebox

A fast, local-first photo library for macOS, written in Rust with
[egui](https://github.com/emilk/egui). It browses big folders of NEF, JPEG,
TIFF, PNG and HEIC files instantly, keeps your marks in standard XMP
sidecars that digiKam, Lightroom and DxO also read, finds duplicates, and
moves files only when you say so, with undo.

Your folders stay the library: Shoebox never writes to an image file, never
deletes anything, and keeps a journal of every file operation it makes.

> **Status:** personal project, in active development. It runs on macOS
> (Apple silicon, macOS 27+). It also builds on Linux, but is only used and
> tested routinely on macOS.

## What it does

- **Browse:** grid, masonry, loupe (with the NEF's embedded full-size
  JPEG), compare, survey, timeline and a map of photos with GPS. Colour
  managed (Display P3), virtualised for libraries of 100,000+ photos.
- **Stacks:** a frame's NEF, JPG and TIFF exports show as one photo.
- **Organise:** ratings, pick/reject flags, colour labels, hierarchical
  keywords, events and collections, all written to `name.ext.xmp` sidecars
  (edited in place, so other tools' data in them survives byte for byte).
- **Duplicates:** exact copies, the same image data (a preview carved from a
  NEF) and resized or recompressed copies, reviewed side by side; nothing
  moves until you apply, as one undoable batch.
- **Files:** import from a card (verified copies, named by template), move,
  rename, reject to `_rejected/`, move to Trash; every operation shows a
  dry-run table first and can be undone from History.
- **Live sync:** changes made by other apps to your folders show up at once,
  marked so you can see what changed.
- **CLI:** `shoebox scan | dupes | apply | import | rename | history | undo |
  resume` for scripting the same operations.

## Safety

These are hard rules in the code, with tests:

1. Nothing in your library is ever deleted. "Remove" means move to
   `_rejected/` or `_duplicates/`, or the Trash when you ask.
2. Every move, rename and copy is journaled before it runs, and can be undone
   (or, if the app quit part-way, resumed or rolled back).
3. Image files are never written. Marks go to XMP sidecars; the only other
   files Shoebox writes in a library are `<root>/.shoebox/*.json`.
4. Nothing is overwritten: name collisions get `-1`, `-2` suffixes.
5. Folders marked **Reference** are read-only.

Please still point it at a copy first. On APFS that costs no space:
`cp -cR Photos Photos-test`.

## Build and run

Needs Rust (the version is pinned in `rust-toolchain.toml`; `rustup`
installs it).

```sh
cargo run --release -p shoebox-app        # the app
cargo run --release -p shoebox-cli -- --help
scripts/bundle-macos.sh                   # Shoebox.app in target/release/bundle
```

`cargo test` runs everything, including headless UI tests that render the
real app (they need a GPU, so CI skips them). Optional real-file tests use
files you put in the gitignored `tests/fixtures/real/`.

## Docs

- [`docs/SPEC.md`](docs/SPEC.md): what Shoebox is meant to be (the source of
  truth).
- [`docs/DECISIONS.md`](docs/DECISIONS.md): where and why the code differs
  from the spec, with measurements.
- [`CLAUDE.md`](CLAUDE.md): working notes for AI-assisted development.

## Licence

Dual-licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.

Map tiles © [OpenStreetMap](https://www.openstreetmap.org/copyright)
contributors.
