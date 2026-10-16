# Shoebox

A Rust + egui photo library app for macOS. **`docs/SPEC.md` is the source of truth**: read the relevant section before changing behaviour, and flag any conflict rather than silently diverging.

## Layout
- `crates/shoebox-core`: library model, scanning, hashing, duplicate index, SQLite cache, and `fileops` (the only module that moves, renames or copies files; it journals everything). No egui.
- `crates/shoebox-decode`: JPEG/TIFF/PNG decoding, NEF embedded-preview extraction, grayscale thumbnails.
- `crates/shoebox-app`: the eframe/egui app (`cargo run -p shoebox-app`).
- `crates/shoebox-cli`: the `shoebox` binary: `dupes` (report only, never moves), `apply` (moves rows a person confirmed in the report), `history`, `undo`.
- `crates/shoebox-fixtures`: synthetic test library generator (dev-only).
- `tests/fixtures/`: synthetic fixtures (built per test by `shoebox-fixtures`; `cargo run -p shoebox-fixtures -- <empty dir>` writes them to disk). Real camera files go in the gitignored `tests/fixtures/real/`; tests that need them skip when it's absent.

## Working rules
- One milestone at a time, in order, each as a reviewable chunk.
- Before finishing any task: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- `thiserror` in library crates, `anyhow` in binaries. No `unwrap()`/`expect()` outside tests (enforced by workspace clippy lints).
- No `unsafe` except in thin FFI wrappers (`shoebox-core/src/fileops/sys.rs`, `shoebox-decode/src/imageio.rs`).
- `shoebox-core` must not depend on egui. Every file operation goes through `shoebox_core::fileops`.
- Target `aarch64-apple-darwin`, toolchain pinned in `rust-toolchain.toml`, minimum macOS 27, release `lto = "thin"`.
- Prefer pure-Rust crates.

## Safety rules (hard requirements)
1. Never delete files outside the app's own cache directories. "Remove" means move to `_rejected/`/`_duplicates/`, or the macOS Trash on explicit command.
2. Journal every move, rename and copy, with a batch id, *before* it runs.
3. Never write to image files. Marks go to XMP sidecars only.
4. Never overwrite: collisions get `-1`, `-2` suffixes.
5. Reference roots are read-only.
6. Multi-file operations show a dry-run table and need confirmation.
7. **Never run anything against the real photo library during development.** Use fixtures or an APFS clone (`cp -cR Photos Photos-test`).
