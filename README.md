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
- **Stacks:** a frame's NEF, JPG and TIFF exports show as one photo, and a
  Live Photo's picture and video as one (movies show a poster frame).
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
- **Semantic search** (optional): describe a photo, in any language, and
  find it; find photos similar to one; browse suggested tags (Things). Runs Google's SigLIP 2 model on
  your Mac (downloaded once, 1.5 GB, when you turn it on); your photos
  never leave it.
- **People** (optional): faces found and grouped by likeness; name a group
  to see all their photos and add a `People|Name` keyword. Runs OpenCV's
  YuNet and SFace models on your Mac (39 MB, downloaded when you turn it
  on).
- **CLI:** `shoebox scan | dupes | apply | import | rename | history | undo |
  resume` for scripting the same operations.

## Safety

These are hard rules in the code, with tests:

1. Nothing in your library is ever deleted. "Remove" means move to
   `_rejected/` or `_duplicates/`, or the Trash when you ask.
2. Every move, rename and copy is journaled before it runs, and can be undone
   (or, if the app quit part-way, resumed or rolled back).
3. Image files are never written. Marks go to XMP sidecars; the only other
   files Shoebox writes in a library are its own `<root>/.shoebox/**/*.json`
   (photo edits live in `.shoebox/edits/`).
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

Released builds (from `scripts/release-macos.sh`, on the GitHub Releases
page) are signed, notarized and update themselves with
[Sparkle](https://sparkle-project.org).

`cargo test` runs everything, including headless UI tests that render the
real app (they need a GPU, so CI skips them). Optional real-file tests use
files you put in the gitignored `tests/fixtures/real/`.

## Docs

- [`docs/SPEC.md`](docs/SPEC.md): what Shoebox is meant to be (the source of
  truth).
- [`docs/EDITING-SPEC.md`](docs/EDITING-SPEC.md): the plan for photo editing
  (Develop view, raw decoding, export), in phases.
- [`docs/DECISIONS.md`](docs/DECISIONS.md): where and why the code differs
  from the spec, with measurements.
- [`CLAUDE.md`](CLAUDE.md): working notes for AI-assisted development.

## Licence

Licensed under the GNU General Public License v3.0 or later; see
[LICENSE](LICENSE).

Map tiles © [OpenStreetMap](https://www.openstreetmap.org/copyright)
contributors. Updates use [Sparkle](https://github.com/sparkle-project/Sparkle)
(MIT; its licence ships in the app), and icons are
[Phosphor](https://phosphoricons.com) (MIT; likewise).
Semantic search uses Google's
[SigLIP 2](https://huggingface.co/google/siglip2-base-patch16-224) model
(Apache-2.0; downloaded, not distributed), and faces OpenCV Zoo's
[YuNet](https://huggingface.co/opencv/face_detection_yunet) (MIT) and
[SFace](https://huggingface.co/opencv/face_recognition_sface) (Apache-2.0),
both with
[candle](https://github.com/huggingface/candle) (MIT or Apache-2.0; one crate
vendored with a fix, see `vendor/README.md`).
Raw editing ports two of [darktable](https://www.darktable.org)'s algorithms
(GPL-3.0): Luis Sanz Rodríguez's RCD demosaicing and the "inpaint opposed"
highlight reconstruction (each module names its source file and commit), and
decodes raw files with [rawler](https://github.com/dnglab/dnglab)
(LGPL-2.1).
