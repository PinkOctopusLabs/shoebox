# Decisions and deviations from SPEC.md

Newest first. SPEC.md stays the source of truth; this records where we
deliberately differ and why.

## 2026-10-16 — M2 browsing choices
- **Library roots live in `settings.json`** in the data folder, not only in
  the cache's `roots` table: they're user choices, and the cache must stay
  safe to delete.
- **Header-only metadata.** Browsing reads the first 256 KB of each file
  (EXIF, orientation, size); real NEFs and Lightroom TIFFs keep all of it in
  the first ~40 KB. Full read only as a fallback. 4,460 files in 0.9 s.
- **Thumbnails from embedded previews**: Z f JPG MPF previews (640×424,
  1620×1080) and NEF embedded JPEGs; 6.7 ms vs 202 ms for a full decode.
  Thumbnails are cached as sRGB, upright JPEGs keyed by path, size, mtime
  and orientation (an edited file gets a new one).
- **Display colour is Display P3 on macOS** (sRGB elsewhere). Images are
  converted from their ICC profile (untagged = sRGB) to Display P3 with
  moxcms, and the window's Metal layer is tagged Display P3, so macOS
  colour-matches to the actual screen, as Preview.app does; wide-gamut
  Adobe RGB/ProPhoto colours are no longer clipped to sRGB. egui-wgpu has no
  surface colour-space option and wgpu resets the layer's colour space when
  it reconfigures (on resize), so `display::DisplayTagger` re-applies the tag
  each frame. Thumbnails are cached per target colour space.
  Checks: Adobe RGB → P3 within 0.76 levels of ImageIO; a deep Adobe RGB
  green that sRGB clips keeps its blue component in P3. egui's own UI
  colours are sRGB values drawn into a P3 surface, so accent colours are a
  touch more saturated than egui intends; greys are identical.
- **Headless UI verification** with egui_kittest: the real app renders to
  PNGs in tests, and a 10,000-photo scroll test measures UI frame time
  (p99 0.88 ms CPU; GPU time isn't measured headlessly).
- HEIC on Linux is still unsupported (libheif backend not built).

## 2026-10-16 — One portable decoder; Linux kept buildable
The owner also uses Linux. Linux stays a non-goal for v1 releases (as the spec
says), but the code must stay portable, so ImageIO is **not** the main
decoder. This supersedes the "ImageIO for display" part of the entry below.
- One pure-Rust decode path per format on every OS, for hashing and display:
  zune-jpeg, tiff, png, and our NEF `JpgFromRaw` extractor.
- Display thumbnails (M2) come from embedded previews where present: Z f
  JPGs carry MPF previews at 640×424 (163 KB) and full HD (606 KB); NEFs a
  640×424 `PreviewImage`. Files without them (carved previews, TIFFs) are
  decoded in full; TIFFs take ~6.5 ms.
- Colour management (M2): `moxcms` (pure Rust) rather than `lcms2` (C).
- HEIC (M2) is the only platform-specific decoder: ImageIO on macOS (wrapper
  in `shoebox_decode::imageio`), libheif on Linux, behind one interface.
- Platform code: no-overwrite rename uses `renamex_np(RENAME_EXCL)` on macOS
  and `renameat2(RENAME_NOREPLACE)` on Linux (other platforms: error, never
  an overwriting rename). Data dir: Application Support on macOS,
  `$XDG_DATA_HOME/shoebox` elsewhere.
- Verified once in a Linux container (rust:1.99-bookworm, aarch64): clippy
  `-D warnings` clean, 48/48 tests pass. Routine testing stays on macOS.

## 2026-10-16 — ImageIO benchmark (for M2 thumbnails)
`shoebox_decode::imageio` is a thin FFI wrapper (ImageIO + CoreGraphics),
needed for HEIC in M2 anyway. RAW files need a UTI type hint, since a NEF
looks like a TIFF from its bytes. Real Z f files, median per image:

| Input | ours | ImageIO |
| --- | --- | --- |
| camera JPG 6048×4032 → 256 px | 110 ms | 58 ms |
| camera JPG → 1024 px | 132 ms | 33 ms |
| 48 JPGs in parallel → 256 px | 12.2 ms/img | 6.6 ms/img |
| Lightroom 16-bit TIFF 2560 px → 256 px | 6.5 ms | 15 ms |
| NEF → 256 px | 28 ms (JpgFromRaw + zune) | 78 ms |
| turbojpeg 1/8 scaled (reference) | 65 ms | |

Hashes from ImageIO differ from ours by 0–3 bits (different resampling and
gray conversion), so the two paths must never be mixed in one cache.

Decision: keep our decoders for **hashing** (deterministic, pure Rust, not
tied to macOS version changes; cached hashes stay valid). For **display**
thumbnails in M2, use ImageIO for JPEG and HEIC (2–4× faster, and it does
ICC colour management, which M2 requires), and our own path for NEF
(embedded preview) and TIFF.

## 2026-10-16 — Multi-index hash instead of a BK-tree
The spec names a BK-tree, but at radius 10 over 64-bit hashes a BK-tree visits
nearly every node, so grouping was quadratic (10k items: 2.19 s; 200k
infeasible). `dupes::mih` splits pHashes into four 16-bit chunks and probes
every bucket within 2 bits per chunk (pigeonhole), then verifies; radii above
15 fall back to a linear scan. Matching runs in parallel, assignment
sequentially. Benchmarks (random hashes, every 10th a near-copy):
10k 7.8 ms, 25k 25 ms, 100k 330 ms, 200k 1.47 s. Reports on the real-library
copy are byte-identical to the BK-tree version.

## 2026-10-16 — Fewer, better duplicate candidates
Measured on a copy of the real library (4,460 files) after the change above.
- **Siblings aren't duplicates.** Files in the same folder with the same base
  name (ignoring case, extension and `-N` suffixes, stripped only while the
  shorter name exists) are renditions of one frame. They match each other
  only exactly or by image data, never perceptually.
- **Blank and low-detail frames.** Luma stddev of the 256 px thumbnail is
  cached. ≤ 3 (blank, blown-out): no perceptual matching. ≤ 8 (mostly dark):
  only near-identical perceptual matches. Real library: blank = 0.0,
  p1 = 2.4, median = 45.
- **Keeper-centred groups replace union-find.** Every member matches its
  keeper directly; matches of matches no longer chain (largest group 991 → 52).
- **Different exposures are never suggested as duplicates.** If both files
  have a capture time and they differ, the row is `review` with a note and
  `same_exposure = no`. On the real library this covers 1,759 of 1,759
  remaining candidates (bursts and blank frames); filtering it out leaves 0.
- Real carved previews (40 `JpgFromRaw` extracted with exiftool) against the
  library as reference: 40/40 suggested `duplicate` at image-data level; 39
  matched their own NEF, 1 a byte-identical blank frame.

## 2026-10-16 — Duplicates are flagged, never moved automatically
A dry run on a copy of the real library showed that perceptual matches at the
spec's thresholds (≤ 4 / ≤ 10) also catch burst frames seconds apart and blank
frames across days: 64% of files had a *different* frame within distance 10.
So `shoebox dupes` only writes a report (with `suggestion` = duplicate/review
and an empty `confirm` column); `--move-matched` just fills the proposed
`dest`. `shoebox apply <report>` moves only rows a person marked `y`, after
re-checking each file's BLAKE3 against the report, refusing reference files,
and refusing to empty a group. The M4 review UI will follow the same rule.

## 2026-10-16 — M1 implementation choices
- Journal in its own `journal.sqlite`, not the rebuildable cache.
- Perceptual hashes stored for 0/90/180/270° for every file (carved previews
  may lack or misstate Orientation).
- Moves across volumes are refused (would need copy + delete).
- Sidecars move with their file; a shared `name.xmp` stays.
- walkdir instead of jwalk; own dHash/pHash instead of `image_hasher`.
- zune-jpeg kept over turbojpeg (1.84× gap on a 24 MP decode to 256 px,
  below the 2× bar; avoids a C dependency).
- Decodes capped at 200 MP to bound memory on hostile/corrupt headers.
