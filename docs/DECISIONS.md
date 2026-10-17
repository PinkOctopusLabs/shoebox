# Decisions and deviations from SPEC.md

Newest first. SPEC.md stays the source of truth; this records where we
deliberately differ and why.

## 2026-10-17 — Settings window; counts are photos
- **Settings** (⌘, from the app menu or the keyboard, or Tasks → Settings…)
  is an in-app window with tabs, applied and saved as you change them:
  Library (folders, Reference, Remove, Add Folder…), Sidecars (marks to
  every member; also `name.xmp` for RAWs), Naming (import and rename
  templates with a live example; invalid ones aren't saved), Map (OSM tiles
  on/off: new, so the map can be used without any network), Memory (the
  spec's configurable texture budget, 256 MB–4 GB, default 768 MB) and
  Updates (Sparkle's automatic check / install, Check Now; builds without
  Sparkle say so). Escape closes it; while open, keys don't reach the grid.
- Not in the window: the masonry "justified rows" setting (that layout
  isn't built yet) and duplicate thresholds (set in the Duplicates view).
- **Counts are photos (stacks), not files**: "All photos" and each folder
  count the stacks with a file in that folder (what selecting it shows);
  a NEF + JPG counts once. The status bar still gives files as "total".

## 2026-10-17 — 200,000-file performance check
A synthetic library shaped like a camera library
(`shoebox-fixtures --example big_library`: 200,000 files, 117,709 frames,
82,291 NEF + JPG pairs, 2,391 day folders over 2015–2026, GPS on ~70% of
shoot days, a rating sidecar on ~10% of frames; 3 GB). Measured by the
ignored UI test `two_hundred_thousand_files` (CPU time per frame; GPU time
isn't measured headlessly), on this Mac:

| Measure | Target | Before | After |
| --- | --- | --- | --- |
| Open from cache: grid lists all | < 1 s | 1,698 ms | 815 ms |
| … visible thumbnails in | | | 830 ms (rescan from disk ends 3.3 s, in the background) |
| Grid scroll, 600 frames | 60 fps, none > 16 ms | p99 1.6 ms | p99 1.4 ms, max 1.8 ms |
| Rate a photo (catalog republished) | none > 16 ms | ~200 ms stall | max 1.6 ms |
| Search keystroke (first frame) | | 24–30 ms | 8.7 ms |
| Switch to timeline | | 23 ms | 10 ms |
| Masonry / map / filters / select all | | ≤ 9.6 ms | ≤ 9.6 ms |
| Duplicates, hashes cached | (10k: < 5 s) | 6.6 s for 200k, UI frames ≤ 15 ms | |

First open with no cache (scan, metadata, sidecars) took 9.8 s; the first
duplicate search, which hashes everything, 25 s.

Fixes:
- `Catalog::sort` parsed capture times on every comparison; now once per
  photo (from_cache 1.5 s → 0.82 s).
- `stacks::build` hashed `PathBuf`s (component by component) several times
  per photo; folders are now numbered and keys borrow `&str`
  (384 → 168 ms). The `-N` suffix rule is shared with duplicate grouping
  (`pipeline::sibling_stem`).
- Everything the UI derives from a catalog (ids, index, folder tree,
  keyword/camera/lens lists, changed files) is built on the library worker
  (`jobs::Published`); the UI thread only swaps it in. This was the ~200 ms
  stall on every rating, flag or label change at 200k.
- Search compares ASCII case-insensitively without allocating.
- The timeline's per-stack lookups are Vecs, not HashMaps.

`examples/time_open.rs` (shoebox-core) times opening from a cache, stage by
stage. Loading the 200,000 rows from SQLite (~270 ms) is now the largest
single cost.

## 2026-10-17 — Updates with Sparkle
- Not in the spec. Installed copies update themselves with
  [Sparkle](https://sparkle-project.org) 2 (the person's choice over a
  home-made updater): a background check daily, the update downloads quietly
  and installs when Shoebox quits; Shoebox > Check for Updates… asks now.
- The feed is `releases/latest/download/appcast.xml` on the public GitHub
  repo, so every release replaces it; no server of our own. Each update is
  EdDSA-signed (private key in the release Mac's Keychain, public key in
  `assets/sparkle-public-key.txt` → Info.plist) as well as Developer ID
  signed and notarized. Tested locally on throwaway builds: 0.0.1 found,
  downloaded and installed 0.0.2 on quit; an update signed with another key
  was downloaded and refused.
- `scripts/fetch-sparkle.sh` downloads a pinned Sparkle release and checks
  its SHA-256 (nothing vendored in the repo). The app loads the framework at
  runtime (`updater.rs`), so `cargo run` and tests have no updater and
  nothing links against Sparkle; Linux has none.
- A relaunch to finish an update waits while the library worker is handling
  a command (`jobs::working()`), so it never cuts a file operation short.
- `scripts/release-macos.sh <version>` bumps the version, tests, builds,
  notarizes, writes and signs the appcast, then tags, pushes and publishes
  the GitHub release. Run by hand; nothing releases on push.
- Ad-hoc builds are now signed *without* the hardened runtime: its library
  validation refuses an ad-hoc signed Sparkle.framework. Developer ID builds
  keep it (notarization needs it).

## 2026-10-17 — Mac app bundle
- `scripts/bundle-macos.sh` builds `Shoebox.app` (release, Info.plist with
  the bundle id, minimum macOS 27, folder-access usage strings; icon from
  `assets/icon-1024.png`, drawn by `examples/make_icon.rs`) and a zip. The
  `shoebox` CLI ships in `Contents/Helpers/` (in `MacOS/` it would replace
  `Shoebox` on the case-insensitive file system).
- Signed ad-hoc by default (runs on this Mac), or `SIGN_IDENTITY` (a Developer ID, with the hardened runtime) plus
  `NOTARY_PROFILE` to notarize and staple for other Macs. Not sandboxed: Shoebox moves files anywhere inside
  the folders you add, which a sandbox would only allow with security-scoped
  bookmarks for every root; worth revisiting for the Mac App Store only.
- Launched from Finder there's no terminal, so logs (and panics) go to
  `~/Library/Logs/Shoebox/shoebox.log`. `--version` prints the version.
  `SHOEBOX_DATA_DIR` makes the app (like the CLI) keep everything in one
  folder, for trying a build without touching real settings.

## 2026-10-17 — M6 Map
- **Tiles**: OpenStreetMap through the `walkers` egui map widget (the
  person chose online, cached tiles). Tiles cache under the app's Caches
  folder (`…/Caches/<bundle id>/tiles`), honouring HTTP expiry as OSM's tile
  policy asks; the User-Agent names Shoebox and nothing else. The tile
  server sees which map areas are viewed, never photos or file data. The map
  shows the "© OpenStreetMap contributors" credit. Tests run with tiles off
  (no network); `SHOEBOX_UI_TILES` opts the real-library test in.
- `walkers` brings reqwest/tokio and rustls with aws-lc (C) for HTTPS: an
  exception to "prefer pure Rust", as any HTTPS client needs native crypto.
- **GPS** comes from EXIF (`GPSLatitude`/`Longitude` + refs); 0,0 counts as
  no fix. Cached in the `exif` table (cache schema 6: the first start after
  updating rescans once, and unseen-change badges reset). A stack uses its
  primary's position, else any member's. GPS in sidecars isn't read yet.
- **Markers** are photo thumbnails with a count, grouped by 60-point screen
  cells (regrouped as you zoom). Click selects the group's photos;
  double-click zooms into a group or opens a single photo in the loupe. The
  map follows the grid's filters, folder, event and collection, and opens
  fitted to all located photos. Info panel: Location row + Show on Map.

## 2026-10-17 — M6 Timeline
- Follows the grid: the same filters, folder, event, collection and sort
  (descending sort gives newest first), one entry per stack.
- Heat strip shows one year at a time (◀ year ▶; defaults to the current
  photo's year), weeks across and weekdays down, shaded by quartile of the
  shooting days' counts, with the busiest day always darkest. Click a day →
  Grid at that day's first stack (as the spec says); hover shows the count.
- Days list as month headers and day rows: date, count, chips for events
  covering the day, and a strip of thumbnails that fit the width, with a
  "+N" tile that also jumps to Grid. Undated stacks come last.
- Dates are the EXIF capture date (camera local time) with plain calendar
  arithmetic in core (`timeline.rs`): no time-zone conversion.

## 2026-10-17 — M5 file operations and import
- **Templates** add `{YY} {MM} {DD} {HH} {mm} {ss} {name} {camera} {width}
  {height} {seq} {EXT}` to the spec's tokens. `{ext}` is lower case (as the
  existing library is). Without a date and without dimensions the fallback
  is `_undated/unknown/{name}`. Token values can never add folders, `..`, or
  hidden names (property-tested).
- **Import copies** go to a hidden temporary file, are BLAKE3-checked
  against the source, get the source's modification time, then are renamed
  into place without replacing anything (`renamex_np` / `renameat2`). A frame's
  files share one `-N` suffix; card sidecars (`DSC_0001.NEF.xmp`,
  `DSC_0001.xmp`) come along under the new name. "Already in the library"
  hashes only same-size library files.
- **Undoing an import moves the copies to the Trash** (only if they still
  match what was copied); the card still has the originals. That's the only
  way to undo a copy without deleting. Move to Trash itself is an explicit
  command with its own dry-run table; the system Trash can't be undone from
  Shoebox (Finder's Put Back does it). macOS uses NSFileManager (no Finder
  scripting). Tests and `SHOEBOX_TRASH_DIR` use a folder instead.
- **Stacks move together**: one `-N` suffix for the whole stack on a
  collision; `name.ext.xmp` follows each file; a shared `name.xmp` follows
  only when every image sharing it moves to one new name. Batch rename
  renders from the primary, so a frame keeps one name, and `-N` copies keep
  their suffix. Undated stacks are left alone by rename.
- **References follow moves**: events (`include`/`exclude`), collections
  and stack links name files by path, so a move or rename rewrites those
  `.shoebox/*.json` files in the same batch. The cache rows move too (no
  re-read, no false "changed" badge).
- **Collections** live in `.shoebox/collections.json` in every writable
  root (like events), not in the cache.
- **History** lists every batch and can undo any still consistent with the
  disk. An interrupted batch (the app quit mid-way) is flagged in the status
  bar and History offers **Resume** or **Roll back**. The whole plan is
  journaled (every step `pending`, sidecar contents included) in one
  transaction before the first step runs, so an interrupted batch's
  remaining steps are known. Resume first checks which pending steps
  already happened (died between doing and recording), clears its own
  partial copy, and runs the rest with the same checks (never replace; a
  sidecar only if unchanged since planning; trash only the planned
  content). An interrupted undo can only be rolled back. `shoebox resume`
  does the same from the command line. After a step fails, the rest are
  marked "not run".
- New folders are journaled (undo leaves them: nothing is deleted). The
  folder tree now shows empty folders, so they can be drop targets.
- Not in M5: creating an event during import, and the optional event
  folder rename (moving directories isn't an operation yet).

## 2026-10-17 — M4 Compare, Survey and duplicate review
- **Compare** syncs zoom as a factor over each pane's fit and pan as a
  centre in image fractions, so a 6048 px original and a 2560 px export
  show the same part of the picture. Space/Z goes to 100% of the select.
  Arrows step the candidate, Up swaps; the filmstrip picks the candidate.
- **Survey** takes the selection (2–9, display order); its X removes a tile.
  In Compare and Survey marks go to the active photo and auto-advance is
  off (you're choosing between photos on screen).
- **Review decisions are applied together**: per-file Keep / Move /
  Variant / Not a duplicate, then Apply shows the dry-run table (every
  move, sidecars included) and runs one journaled batch, so Cmd+Z undoes
  it all. Moves go to `<the file's own root>/_duplicates/<relative path>`.
- **Auto-select** follows the spec's keeper rule but only proposes moving
  copies taken at the same moment as the keeper (or with no capture time,
  like carved previews). Other exposures (bursts) stay undecided, as in the
  M1 decision. Reference files never move; a group always keeps one file.
  Every file to move is re-hashed (BLAKE3) right before the batch.
- **"Add to stack as a variant"** is stored in `<root>/.shoebox/stacks.json`
  in every writable root (a new app file beside `events.json`), written in
  the same batch as the moves. A settled variant leaves duplicate review.
- **Ignored pairs** go in the cache's `dupe_ignores` (by BLAKE3, so they
  survive renames). Schema rebuilds keep that table; deleting the cache
  file forgets them.
- **Groups of reference files only** (a NEF and its Lightroom export, both
  in the library) are hidden by default: nothing in them can move.
- Duplicates is a task view (Tasks → Duplicates), not one of the G/M/E/C/N/T
  views. `[` and `]` step groups; Esc returns to the list.
- The glyph test now scans every source file (it missed new views before).

## 2026-10-17 — Live sync and change badges (pulled forward before M4)
- **Watcher**: `notify` (FSEvents on macOS, inotify on Linux) on every root,
  debounced (quiet 250 ms, at most 2 s). Changed sidecars map to their
  photos and only those are re-read; added/removed images or folders cause
  an incremental rescan; `.shoebox/events.json` reloads the events.
- **Who changed it**: Shoebox updates its catalog and cache as it writes, so
  a later re-read finds nothing new for its own writes; anything a re-read
  or rescan does find was made by another app. Undo/redo re-read the files
  of the batch and record them as Shoebox's (with the undo batch's label).
  "Another app" is as specific as we can be: macOS doesn't say which app.
- **What counts as a change**: only files Shoebox already knew. A first
  scan or a newly added folder isn't badged. Unseen changes live in the
  cache (`files.changed_at`, `change_source`), so a cache rebuild forgets
  them, which is acceptable for a "since you last looked" hint.
- **Badges**: orange for another app, blue for Shoebox, top right of the
  cell; a collapsed stack shows its members' changes. Hover and the Info
  panel say what changed, by whom and when (Shoebox's batch number stands
  in for the History link until M5).
- **Sort "Modified"** now uses the newest image or sidecar modification
  across the stack, newest first. "Recently changed" (last 7 days) is a
  built-in smart filter; the filter bar adds "Changed in N days" and
  "Unseen changes".

## 2026-10-16 — M3 stacks, organising and events
- **Stacks**: same folder + base name, or identical capture time with
  sub-seconds + camera serial (exports elsewhere join; bursts don't).
  Variants are `-N` names whose format already exists in the stack (a
  name rule; comparing pixels would need full decodes). Real copy: 4,460
  files → 1,537 stacks.
- **Marks on a stack** go to the primary's sidecar; the settings option
  "marks to all stack members" copies them to every member.
- **Writes are off the UI thread**: the app shows a change at once and the
  library worker writes it as one journaled batch per action, so Cmd+Z
  undoes a whole action (rating 20 photos = one undo).
- **Caps Lock** auto-advance is read via CoreGraphics on macOS (only with a
  real window); a toolbar toggle does the same everywhere.
- **Events** live in `<root>/.shoebox/events.json` in every writable root,
  merged on load (newest edit per event wins); manual additions and
  exclusions are stored by the primary's absolute path (M5's moves will
  need to update them). Members' sidecars get `Iptc4xmpExt:Event` (the
  innermost event) and an `Events|Parent|Child` keyword per event. The
  events file and sidecar changes are one batch; undoing the creation of
  `events.json` leaves `{}` (nothing is deleted). Renaming day folders after
  an event (spec: optional, off by default) is a file operation, left for M5.
- **Fonts**: egui 0.37 dropped symbols from its default fonts and panics
  on missing glyphs; `monochrome_emoji_fonts` is on, and `tests/glyphs.rs`
  fails if any UI string uses an unsupported glyph.
- **Acceptance stand-ins** (no digiKam or DxO here): ExifTool reads ratings
  Shoebox writes, Shoebox reads ratings ExifTool writes, a rescan picks up a
  sidecar changed by another app, and the golden test uses real LightCraft
  sidecars in place of DxO's.
- **Not in M3**: the file watcher (spec "Staying in sync") and change badges;
  a rescan picks up external changes for now.

## 2026-10-16 — M3 sidecars and marks
- **Edit by splicing.** `xmp::edit` never re-serialises: it copies the
  original bytes and only removes the owned properties being changed
  (attribute or element form, in any rdf:Description, under any prefix
  bound to the right namespace, e.g. the old `xap:`) and inserts the new
  values in the first Description. Golden tests on two real LightCraft
  sidecars: ExifTool reports all 14 foreign properties identical, and the
  original bytes survive as a subsequence of the output. ExifTool also
  serves as the stand-in for digiKam both ways (reads ours; we read its).
  No DxO or digiKam samples were available.
- **digiKam:TagsList uses `/`** (as digiKam writes it), not the spec's `|`
  example; `lr:hierarchicalSubject` uses `|`; `dc:subject` gets leaf names.
- **Precedence is per property:** EXIF → embedded XMP → `name.xmp` →
  `name.ext.xmp`. Shoebox writes only `name.ext.xmp` (optionally `name.xmp`
  for RAW) and only the marks that changed, so marks other tools keep
  elsewhere stay valid. Unrecognised `xmp:Label` text (custom Lightroom
  label sets) is left alone unless the label is changed.
- **Sidecar edits are journaled** (not required by the spec, which lists
  moves/renames/copies): each write records the previous bytes and the
  hash of what was written, lands atomically (temp file + rename), and can
  be undone only while the file is still what Shoebox wrote. Undoing a
  sidecar Shoebox created empties our properties rather than deleting the
  file. Only `.xmp` paths outside reference roots can be written.
- The journal (user history) is migrated in place; the cache (schema v4,
  now with a `marks` table) is rebuilt when its schema changes.
- Found in the real library: two LightCraft sidecars named
  `20260430-101010.xmp` beside `20260430-101010-46.tif`, i.e. orphaned by a
  rename; no tool will associate them with their images.

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
