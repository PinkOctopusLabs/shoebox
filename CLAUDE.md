# Shoebox

A Rust + egui photo library app for macOS. **`docs/SPEC.md` is the source of truth**: read the relevant section before changing behaviour, and flag any conflict rather than silently diverging.

## Layout
- `crates/shoebox-core`: library model, scanning, hashing, duplicate index, SQLite cache, and `fileops` (the only module that moves, renames, copies or writes files; it journals everything). No egui.
  M3: `xmp` (read any form, edit by splicing original bytes), `marks`, `sidecar` (precedence + write planning), `stacks`, `query` (filters, sort, smart filters), `events`, `changes` (unseen-change tracking).
  M4: `review` (duplicate groups for the app, auto-select, decisions → one batch).
  M5: `template` (naming), `import` (plan verified copies), `organise` (stack moves, renames, reject, reference rewrites), `collections`.
  M6: `timeline` (days, calendar maths, heat levels).
- `crates/shoebox-decode`: JPEG/TIFF/PNG decoding, NEF embedded-preview extraction, grayscale thumbnails.
- `crates/shoebox-app`: the eframe/egui app (`cargo run --release -p shoebox-app`).
  `jobs.rs` (background loader + library worker; the UI thread never reads
  files), `watch.rs` (file watcher feeding the library worker), `textures.rs` (LRU budget), `views/` (grid, masonry, loupe,
  filmstrip, compare, survey, duplicates, timeline, map, files = import/history/dialogs), `panels.rs`. Headless UI tests in `tests/ui.rs` render PNGs
  (`SHOEBOX_UI_SNAPSHOTS=<dir>`); `--ignored` runs the 10k scroll test and,
  with `SHOEBOX_UI_LIBRARY=<APFS clone>`, a read-only real-library run; with `SHOEBOX_UI_REF`/`SHOEBOX_UI_UNDATED`
  (APFS clones), the `_undated` clean-up on real files. Tests never fetch map
  tiles (`AppPaths.tile_cache: None`); `SHOEBOX_UI_TILES=<dir>` opts in.
- `crates/shoebox-cli`: the `shoebox` binary: `scan` (metadata), `dupes` (report only, never moves), `apply` (moves rows a person confirmed in the report), `import`, `rename`, `history`, `undo`. `SHOEBOX_TRASH_DIR` sends trashed files to a folder (tests).
- `crates/shoebox-fixtures`: synthetic test library generator (dev-only).
- `tests/fixtures/`: synthetic fixtures (built per test by `shoebox-fixtures`; `cargo run -p shoebox-fixtures -- <empty dir>` writes them to disk). Real camera files go in the gitignored `tests/fixtures/real/`; tests that need them skip when it's absent.

## Working rules
- One milestone at a time, in order, each as a reviewable chunk.
- Before finishing any task: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- `thiserror` in library crates, `anyhow` in binaries. No `unwrap()`/`expect()` outside tests (enforced by workspace clippy lints).
- No `unsafe` except in thin FFI wrappers (`shoebox-core/src/fileops/sys.rs`, `shoebox-decode/src/imageio.rs`).
- `shoebox-core` must not depend on egui. Every file operation goes through `shoebox_core::fileops`.
- Target `aarch64-apple-darwin`, toolchain pinned in `rust-toolchain.toml`, minimum macOS 27, release `lto = "thin"`.
- Prefer pure-Rust crates. Keep code portable to Linux: platform-specific code
  goes behind `cfg` with a macOS and a Linux branch (see `docs/DECISIONS.md`).
  Ship and routinely test on macOS only; for an occasional Linux check:
  `docker run --rm -v "$PWD":/src:ro -v shoebox-linux-target:/target -e CARGO_TARGET_DIR=/target -w /src rust:1.99-bookworm cargo test --workspace`

## Safety rules (hard requirements)
1. Never delete files outside the app's own cache directories. "Remove" means move to `_rejected/`/`_duplicates/`, or the macOS Trash on explicit command.
2. Journal every move, rename and copy, with a batch id, *before* it runs.
3. Never write to image files. Marks go to XMP sidecars only; the only other files Shoebox writes in a library are `<root>/.shoebox/*.json`.
4. Never overwrite: collisions get `-1`, `-2` suffixes.
5. Reference roots are read-only.
6. Multi-file operations show a dry-run table and need confirmation.
7. **Never run anything against the real photo library during development.** Use fixtures or an APFS clone (`cp -cR Photos Photos-test`).
