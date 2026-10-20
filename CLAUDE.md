# Shoebox

A Rust + egui photo library app for macOS. **`docs/SPEC.md` is the source of truth**: read the relevant section before changing behaviour, and flag any conflict rather than silently diverging. For photo editing (the Develop view, raw decoding, export), **`docs/EDITING-SPEC.md`** is the source of truth in the same way; its *Before Phase 1* tasks come first. Checks that need new photos or a person's eye (deferred gates, real-file verifications) are listed in **`docs/MANUAL-CHECKS.md`**: add to it rather than leaving them only in DECISIONS.

## Layout
- `crates/shoebox-core`: library model, scanning, hashing, duplicate index, SQLite cache, and `fileops` (the only module that moves, renames, copies or writes files; it journals everything). No egui.
  M3: `xmp` (read any form, edit by splicing original bytes), `marks`, `sidecar` (precedence + write planning), `stacks`, `query` (filters, sort, smart filters), `events`, `changes` (unseen-change tracking).
  M4: `review` (duplicate groups for the app, auto-select, decisions → one batch).
  M5: `template` (naming), `import` (plan verified copies), `organise` (stack moves, renames, reject, reference rewrites), `collections`, `cloud` (cloud-only placeholders: detect, download).
  Develop: `edits` (`.shoebox/edits/<rel>[.<copy>].json`: paths, moves carry them, reattach by size + BLAKE3, catalog working copy vs file, `save_pending` as one `edit` batch).
  M6: `timeline` (days, calendar maths, heat levels), `embeddings` (semantic search store + 8-bit exhaustive search + tag rule; own `embeddings.sqlite`, rows by kind), `people` (face rows, grouping, `people.json`).
- `crates/shoebox-decode`: JPEG/TIFF/PNG decoding (`rgb16`: 16-bit TIFFs and PNGs at full precision for Develop), NEF embedded-preview extraction, grayscale thumbnails, `mov` (MOV/MP4 movie headers), movie poster frames through AVFoundation (`imageio.rs`; `render_path`).
  NEFs are shown without decoding raw data: `nef.rs` finds the embedded JPEGs and `render()` decodes the smallest big enough (`JpgFromRaw` in the loupe) with zune-jpeg, converts with moxcms and orients. That path (it reads High efficiency NEFs too) stays: it is Develop's instant first paint and the library's thumbnail source. The raw decoder sits beside it in `raw` (rawler, inside `catch_unwind`: Lossless/uncompressed NEFs and DNGs, cropped, black-subtracted, normalised); `nef::compression` reads `NEFCompression` (High efficiency needs a DNG). `examples/raw_probe.rs` shows what it makes of files.
- `crates/shoebox-develop` (`docs/EDITING-SPEC.md`, being built): the edit pipeline (settings documents, stage cache, wgpu kernels, export). `shoebox-app` calls it; it uses `shoebox-decode::raw` and, for AI stages, `shoebox-ml`. `gpu`: the live pipe as one wgpu compute pass (`develop.wgsl`) on the app's device, tested against `cpu` within 1e-4 (skips without an adapter). `cpu`: the CPU reference pipe (`demosaic` once per file at as-shot WB, then `frame` + `apply`; `cpu::look`: calibration, colour/HSL in Oklab, black and white, tone-curve tables; `cpu::maps`: dehaze, highlights/shadows and clarity as a fast guided filter, coefficients from a 512 px copy cached on the source, evaluated per pixel; `cpu::detail`: noise reduction, sharpening and texture as one à-trous wavelet stage on Anscombe-stabilised source pixels, noise estimated per image, cached on the source and uploaded as the GPU's source) (raw stages ported from darktable: RCD, inpaint opposed; then matrix colour, exposure, geometry, display transform, output); `wb`, `colour`: white balance and colour maths; `examples/develop_probe.rs` renders files. `golden`: ΔE2000 comparison with stored renders (`tests/golden.rs`: synthetic goldens in `tests/golden/`; `SHOEBOX_GOLDEN_DIR` runs the real set; `SHOEBOX_GOLDEN_BLESS=1` rewrites them). `benches/pipeline.rs` (criterion) with `examples/bench_check.rs` (fails on a >20% regression against a saved baseline). `lens`: lens corrections as a grid (per-channel source offsets + vignetting gain) from the NEF's own data (`shoebox_decode::nef::lens_data`, IFD 0xC7D5) and Lensfun (`lensfun` crate, bundled DB, loaded once), with a fill zoom folded into the geometry map; `examples/lens_probe.rs` block-matches a raw against its camera JPEG. `export`: presets, Lanczos resize, JPEG/16-bit TIFF, naming, never-overwrite writes. `encode`: JPEG with ICC (jpeg-encoder, moxcms). `edit`: the edit document (serde; settings stored as differences from code defaults, unknown modules kept; history as JSON Patches replayed from the defaults, `amend` folds a drag into one step; `render_hash` keys previews and staleness). `dng`: High efficiency NEFs → Adobe DNG Converter (`-c -p0`, two at once, 180 s timeout) → `<cache>/dng/<blake3 of NEF>.dng`, LRU-evicted; tests use a shell-script converter, and `--ignored` runs the real one on the real fixture.
- `crates/shoebox-ml`: semantic search model (SigLIP 2 on candle; Metal on macOS): pinned, checksummed download (`download`), f16 split (`convert`), image/text encoders, `labels.txt` (tag list), `faces` (YuNet/SFace: detect, align, embed) on `onnx` (a small ONNX interpreter, no protoc). Never touches the library. `examples/faces_probe.rs` checks faces on a folder. `examples/probe.rs` times and ranks a folder; `examples/fetch.rs` downloads. Develop's models (RawNIND, SAM 2.1, SegFormer) go here too, native on candle from the authors' weights: never `ort`, and `onnx` doesn't grow for them (`docs/DECISIONS.md`). `vendor/candle-metal-kernels` is candle's with one fix (Metal 3.2 shaders; see `vendor/README.md`).
- `crates/shoebox-app`: the eframe/egui app (`cargo run --release -p shoebox-app`).
  `develop.rs` (develop thread: HE conversion, raw decode and proxy demosaic for Develop; conversions ahead on their own thread: the photos either side in Develop, and likely edits when idle), `jobs.rs` (background loader + library worker; the UI thread never reads
  files), `watch.rs` (file watcher feeding the library worker), `semantic.rs`
  (semantic search thread: model download, indexing, queries, tags; the `Model`
  trait lets tests use a fake model), `people.rs` (faces thread: grouping, names in
  `people.json`; `FaceModel` trait for tests), `updater.rs`
  (Sparkle, loaded at runtime from bundled builds only), `textures.rs` (LRU budget), `picker.rs` (folder pickers as sheets, polled each frame), `panels.rs` (top bar with search and undo/redo, filter strip over the photos, icon view bar under them, side panels, status bar), `icons.rs` (bundled Phosphor icon font, icon buttons), `views/` (export: the export dialog, presets, the Info panel's exports; crop: Develop's crop tool; look: Develop's tone curve, colour, black and white and calibration sections; develop: the Develop view, GPU render into an egui native texture, edit kept and saved through the library worker; grid, masonry, loupe,
  filmstrip, compare, survey, duplicates, timeline, map, settings, files = import/history/dialogs), `menu.rs` (Settings… in the macOS app menu). Headless UI tests in `tests/ui.rs` render PNGs
  (`SHOEBOX_UI_SNAPSHOTS=<dir>`); `--ignored` runs the 10k scroll test,
  with `SHOEBOX_UI_BIG=<dir>` the 200k performance test (library from
  `cargo run --release -p shoebox-fixtures --example big_library -- <dir>`;
  numbers in `docs/DECISIONS.md`) and,
  with `SHOEBOX_UI_LIBRARY=<APFS clone>`, a read-only real-library run; with `SHOEBOX_UI_REF`/`SHOEBOX_UI_UNDATED`
  (APFS clones), the `_undated` clean-up on real files. Tests never fetch map
  tiles (`AppPaths.tile_cache: None`); `SHOEBOX_UI_TILES=<dir>` opts in. Semantic search tests use a fake model;
  `SHOEBOX_ML_MODEL=<model dir> SHOEBOX_UI_PHOTOS=<folder>` runs the ignored real-model test;
  `SHOEBOX_FACES_MODEL=<dir>` the People one.
- `crates/shoebox-cli`: the `shoebox` binary: `scan` (metadata), `dupes` (report only, never moves), `apply` (moves rows a person confirmed in the report), `import` (`--folder-events`), `rename`, `history`, `undo`. `SHOEBOX_TRASH_DIR` sends trashed files to a folder (tests).
- `crates/shoebox-fixtures`: synthetic test library generator (dev-only).
- `tests/fixtures/`: synthetic fixtures (built per test by `shoebox-fixtures`; `cargo run -p shoebox-fixtures -- <empty dir>` writes them to disk). Real camera files go in the gitignored `tests/fixtures/real/`; tests that need them skip when it's absent.

## Building the app
- `scripts/bundle-macos.sh` → `target/release/bundle/Shoebox.app` (+ zip). Ad-hoc
  signed by default; `SIGN_IDENTITY="Developer ID Application: …"` and
  `NOTARY_PROFILE=<notarytool profile>` for a notarized build.
- To try a build without touching real settings: `SHOEBOX_DATA_DIR=<scratch dir>`.
- Updates: `scripts/fetch-sparkle.sh` (pinned, checksummed Sparkle) is embedded
  when `assets/sparkle-public-key.txt` exists. `scripts/release-macos.sh <version>`
  publishes a signed, notarized release + appcast to GitHub Releases.
  `SHOEBOX_UPDATE_FEED=<url>` points a build at a test feed and checks at once;
  test with throwaway builds (`BUNDLE_ID=…updatetest`, a test key via
  `SPARKLE_PUBLIC_KEY`), never the real app.

## Working rules
- One milestone at a time, in order, each as a reviewable chunk.
- Before finishing any task: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- `thiserror` in library crates, `anyhow` in binaries. No `unwrap()`/`expect()` outside tests (enforced by workspace clippy lints).
- No `unsafe` except in thin FFI wrappers (`shoebox-core/src/fileops/sys.rs`, `shoebox-decode/src/imageio.rs`, and the app's `display.rs`, `platform.rs`, `updater.rs`, `menu.rs`).
- `shoebox-core` must not depend on egui. Every file operation goes through `shoebox_core::fileops`.
- Target `aarch64-apple-darwin`, toolchain pinned in `rust-toolchain.toml`, minimum macOS 27, release `lto = "thin"`.
- Prefer pure-Rust crates. Keep code portable to Linux: platform-specific code
  goes behind `cfg` with a macOS and a Linux branch (see `docs/DECISIONS.md`).
  Ship and routinely test on macOS only. CI (GitHub Actions, public repo
  PinkOctopusLabs/shoebox) runs macOS and Linux on every push, without the
  GPU-only UI tests; locally, for a Linux check:
  `docker run --rm -v "$PWD":/src:ro -v shoebox-linux-target:/target -e CARGO_TARGET_DIR=/target -w /src rust:1.99-bookworm cargo test --workspace`
- Licence is GPL-3.0-or-later. Code ported from darktable, RawTherapee or
  other GPL projects is allowed; note the source file and commit in a comment
  at the top of the module. Never add a dependency whose licence is
  incompatible with GPL-3.0 (for example, proprietary or GPL-2.0-only).
- The repo is public: never commit real photos, real GPS positions or other
  personal data (real files go in the gitignored `tests/fixtures/real/`).

## Safety rules (hard requirements)
1. Never delete files outside the app's own cache directories. "Remove" means move to `_rejected/`/`_duplicates/`, or the macOS Trash on explicit command.
2. Journal every move, rename and copy, with a batch id, *before* it runs.
3. Never write to image files that are already there: originals are never modified or replaced. Marks go to XMP sidecars; Shoebox's own files and photo edits go in `<root>/.shoebox/**/*.json`. Shoebox may add *new* files to a library (imports, and exports the person sends there), always through `fileops`: journaled, verified, undoable, never replacing a file.
4. Never overwrite: collisions get `-1`, `-2` suffixes. The one exception: an export outside the library may replace an earlier file when the person chooses Replace when asked; it's written beside it first, then renamed over it. Inside the library an export never replaces anything.
5. Reference roots are read-only.
6. Multi-file operations show a dry-run table and need confirmation.
7. **Never run anything against the real photo library during development.** Use fixtures or an APFS clone (`cp -cR Photos Photos-test`).
