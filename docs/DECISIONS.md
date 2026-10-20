# Decisions and deviations from SPEC.md

Newest first. SPEC.md stays the source of truth; this records where we
deliberately differ and why.

## 2026-10-18 — Things: A to Z, or by count
- The Things heading has a sort button: A to Z (the default) or most
  photos first, remembered in settings. Folded, the list keeps the eight
  tags with the most photos (A to Z would show only the a's and b's).

## 2026-10-18 — One undo for files and names; People counts what's shown
- Names (naming, merging, "Not …", Forget) are undoable. They live in
  `people.json`, not the journal, so the People worker keeps snapshots of
  its own. The two stacks report when their next undo was made and next
  redo was undone; ⌘Z (and the Undo button) undoes the newer of the two,
  ⇧⌘Z redoes the one undone last, and a new action of either kind clears
  both redo stacks.
- The People list counts each person's photos among those shown (folders,
  event, collection, filters, search), before the people chosen narrow the
  grid; people with none there are left out (unless chosen), and the hover
  text gives their total.

## 2026-10-18 — Same person: merging groups, and learning from it
- ⌘-click people in the sidebar to show several groups' photos at once;
  the bar then names them as one person ("Same person: name them") or
  merges them into someone named among them ("All Ann"). A person's menu
  has Same Person As. Typing a name someone already has merges into them.
- The face model isn't retrained (SFace embeddings are fixed); Shoebox
  learns through each person's example faces, which every regroup matches
  against. A merge makes the groups' faces that person's examples, each
  source an even share of 48 (was 24, and `spread` under-filled its share).
- Matching compares a face with a person's 3 closest examples, not the
  average of all: averaging diluted people with several looks (ages,
  glasses, angles), so the more groups you merged, the worse each matched.

## 2026-10-18 — Face boxes
- The view bar's Face boxes toggle (remembered in settings, on by default)
  alone decides whether faces are outlined: every recognised face, on
  tiles and in the loupe (named there), whatever People shows. The person
  chosen in People is amber (the selection blue is near white on a dark
  theme), everyone else faint white; faces too small on a tile to box are
  skipped. With the toggle off, pointing at a name in the info panel's
  People row still outlines that face.
- Boxes come from the face rows already stored (fractions of the upright
  image), sent to the UI with the groups; padded 12% as detector boxes hug
  the face.
- ⌘Z/⇧⌘Z note what they sent at once (the next redo is what was just
  undone, and vice versa), so a quick ⇧⌘Z after ⌘Z goes to the right stack
  before the workers report back.

## 2026-10-18 — Events and collections both stay; reference photos skipped
- Kept apart on purpose: events are written to the photos (shared with
  other apps; things that happened); collections live only in Shoebox
  (working sets such as picks or a print order, reference photos included).
  The photo menu now has Add to / Remove from Collection and New Collection
  from Selection, and a folder's menu Add to Collection, as for events.
- Marks and events on a selection that includes reference photos used to
  fail as a whole (a sidecar write anywhere in a reference root refuses the
  batch). `Catalog::set_marks` now skips reference photos and the status bar
  says how many were left as they are.
- Taking a photo out of the collection being shown from its own menu
  shrank the grid mid-frame and crashed (index out of bounds); cells and
  clicks now ignore positions that are gone.

## 2026-10-18 — Events are marked on photos, not date ranges
- Changed at the user's request (SPEC updated): an event is the photos
  marked with it. A date range took in other photos from the same days and
  folders, and couldn't hold two events in one day.
- The membership is the sidecar keyword `Events|Japan Trip|Kyoto` (with
  `Iptc4xmpExt:Event`), which Shoebox already wrote for every member, so
  existing events keep their photos; version-1 fields (start, end, include,
  exclude) in `events.json` are ignored. A sub-event's photos count for its
  parent. Renaming, nesting or deleting an event rewrites its members'
  sidecars in the same batch; leaving an event leaves its sub-events too.
- Sidecars move with their files, so moves and renames need no rewrite of
  `events.json` (collections still do).
- No more typed date ranges: an event's dates (the panel's year, hover
  text, order) come from its photos; one without dated photos sorts by when
  it was made.

## 2026-10-18 — Top bar and icon view bar (after Lightcraft)
- **Top bar** across the window: library-panel toggle, Undo, Redo at the
  left; search centred (up to 560 pt) with the Content switch inside it and
  a funnel that opens the rating, flag and label filters; Settings and the
  info-panel toggle at the right. On macOS it is the title bar (full-size
  content view, transparent title bar): it leaves room for the window
  buttons, drags the window, and double-click zooms (or minimises, per the
  system setting). AppKit would treat the whole strip as title bar, so a
  quick double-click on a button zoomed the window: winit's view now says
  `mouseDownCanMoveWindow` NO, and only the bar's empty space moves or
  zooms (checked with synthetic clicks on a throwaway build).
- **Filter strip** over the photos only when it has something: the scope
  chip and photo count, the filters (funnel open or any set), and Grid's
  "Group by day" / Masonry's "Justified rows".
- **View bar** under the photos: icons for the seven views and a sort menu
  at the left; stars, pick, reject and labels for the photo or selection in
  the middle (when there's room); thumbnail size, or the loupe's zoom, at
  the right.
- Icons are Phosphor (regular, MIT), bundled as a font
  (`crates/shoebox-app/assets`): egui-phosphor targets an older egui. Icon
  buttons carry names for VoiceOver (and the UI tests), not just glyphs.

## 2026-10-18 — Escape leaves the loupe; a new source starts afresh
- Escape in the loupe goes back to the view it was opened from (Grid,
  Masonry, Timeline…); an open menu or full screen takes the first Escape.
- Choosing a folder, event, collection or All photos in the sidebar clears
  the selection and the current photo and goes back to browsing (the view
  the loupe came from, else Grid): the old photo isn't in the new source.

## 2026-10-18 — One source at a time; several folders; adding to events
- The grid shows **one source**: folders, an event or a collection.
  Choosing one clears the others (a folder and an event together read as
  "both", but filtered as "the overlap"). Smart filters, people, tags and
  search still narrow whichever source is shown.
- **Several folders:** ⌘- or Shift-click adds a folder or takes it out; a
  plain click shows just that one (or everything, if it was the only one).
  The chip reads "📁 3 folders".
- **Adding to an event:** the photo menu has Add to Event (every event,
  sub-events indented) and New Event from Selection (⌘E); a folder's menu
  does the same for every photo in the folders shown; and photos can be
  dropped on an event. A new event spans its photos' dates (events are date
  ranges plus photos added by hand), so it can take in other photos from
  those days too.
- **Group by day** (Grid) and **Justified rows** (Masonry) are checkboxes at
  the right of the filter bar, not toggles among the views.

## 2026-10-18 — Filters over the photos; loupe fit
- The **filter bar** (scope chips, search, rating/flag/label, Filters) sits
  over the middle panel, like Lightroom's Library Filter, since it narrows
  what's there. It's hidden in Duplicates, Import and History. There's no
  top toolbar now: **Undo/Redo** and background work are at the right of
  the status bar, **Auto-advance** in the info panel's Organise section.
- **Loupe:** each photo opens at Fit (also when picked from the filmstrip).
  A plain scroll pans a zoomed image and never zooms (the grid's scroll
  momentum used to zoom a photo as it opened); pinch or ⌘-scroll zooms.

## 2026-10-18 — UI polish
- **Side panels** fold from ◀/▶ buttons at the ends of the view bar (Tab
  still folds both). Tab no longer also moves keyboard focus, which had put
  it in the search box so the next Tab did nothing.
- The left and right panels' scroll bars sit beside the rows instead of
  floating over the counts and + buttons.
- **Info panel:** empty rows are left out; with nothing selected, a short
  message. Paths show the home folder as `~` (full path on hover), with
  Reveal in Finder beside them.
- **Grid:** a colour label is a dot at the top left (the strip along the top
  looked like part of the selection). Format badges stay on every tile.
- **View bar:** Size shows only for Grid, Masonry and Timeline; Sort not in
  Duplicates, Import or History.
- **Auto-advance** is a checkbox, so its state is visible.
- egui's small text is 11 pt (was 9), for the duplicate captions, import
  and history tables.

## 2026-10-18 — DxO sidecar check deferred
- The check of which sidecar name DxO PhotoLab 9 writes, and the golden
  test on a real DxO sidecar, are off the list for now. The default stays
  `name.ext.xmp` (`name.xmp` for RAWs is a setting); both are read, and the
  synthetic golden test still proves unknown properties survive.

## 2026-10-18 — MP4
- `.mp4` and `.m4v` are movies too (`Format::Mp4`, badge **▶ MP4**, its own
  file-type filter). MPEG-4 is QuickTime's box structure, so the same header
  reader, poster frames, exact-copy duplicates and import naming apply.
  Without Apple's creation-date key (most non-Apple phones and cameras),
  the time is the `mvhd` UTC time in this Mac's time zone.

## 2026-10-17 — Live Photos and movies (MOV)
- `.mov` files are part of the library. A Live Photo (HEIC or JPG + MOV
  with one base name) stacks as one photo with a **LIVE** badge; the video
  is never the primary. A lone clip shows **▶ MOV**.
- **Poster frames** come from AVFoundation (`AVAssetImageGenerator`, in
  `imageio.rs`, the decoder's FFI file), a second in (or the middle of a
  shorter clip; the first frame is often black), exact rather than the
  nearest keyframe, upright (track transform applied), colour-managed like
  other thumbnails. Movies decode from their path (`render_path`), so a
  poster never reads the whole file. Linux: no poster (a placeholder).
- **Metadata** from a small, portable reader of the movie's boxes: display
  size (rotation applied), duration, and the creation time: Apple's
  `com.apple.quicktime.creationdate` (local time with offset) when present,
  else the `mvhd` UTC time in this Mac's time zone. Only the `moov` box is
  read, wherever it is in the file.
- **Duplicates:** exact copies only (BLAKE3, streamed); no perceptual
  hashes for video. **Import** names a Live Photo's video from its photo's
  capture time, so the pair keeps one name.
- Playback is a non-goal (spec): the loupe shows the poster frame and an
  "Open in QuickTime Player" button.

## 2026-10-17 — Shutter count, GPS from sidecars
- **Shutter count:** ShutterCount (tag 0xA7) from a Nikon type-3
  MakerNote ("Nikon\0", version, then a TIFF header its offsets count
  from). It's a plain LONG (Nikon uses it as a key to encrypt other tags).
  Read from the first 256 KB like other metadata; the real Z f NEF and JPG
  both carry it. A TIFF export has no MakerNote, so the info panel shows the
  stack's NEF or JPG count. No ExifTool fallback (other makes stay blank).
- **GPS from sidecars:** `exif:GPSLatitude` / `GPSLongitude` in a sidecar
  ("DDD,MM.mmmmN", "DDD,MM,SSN" or decimal), as digiKam and Lightroom write
  a location you add, are read with the marks (re-read when the sidecar
  changes) and win over the camera's EXIF: they're usually a correction.
  Never written. The map and info panel use the result. Cache schema 7.

## 2026-10-17 — Lightroom-style layout
Following Lightroom's Library module:
- **Top bar, one row:** what's shown (scope chips, search, rating, flag,
  label, Filters ▾, Clear), with background work, Auto-advance and
  Undo/Redo at the right.
- **A bar under the photos** (between the side panels): view buttons,
  Days/Rows, then sort and thumbnail size at the right.
- **Info panel** in folding sections like the left panel's: Histogram,
  Info, Organise.
- **Tiles** a step lighter than the background, the selected one lighter
  still.

## 2026-10-17 — Day headers, justified rows
- **Grid day headers** (the toolbar's **Days**, saved in settings): each day
  starts a new row under "Friday 24 April 2026 · 86 photos". Only when
  sorted by capture time and not showing a best-match search. The grid now
  lays out lines (headings and rows) with their offsets and draws those in
  view; Up and Down follow the laid-out rows.
- **Justified rows** (Masonry's **Rows**): photos at the thumbnail height
  fill a row, which is then scaled to the width (at most 1.5× taller, so a
  lone panorama doesn't make a giant row); the last row isn't stretched.
  Strict left-to-right date order, as the spec says.
- Fixed: two settings changed in quick succession could lose the second,
  because the library worker's echo of the first save overwrote it. After
  the first load the app now takes only the library folders from the
  worker; every other setting is the UI's.

## 2026-10-17 — Polish: toolbar, left panel, grid cells
- **Toolbar** is two fixed rows: views, size, auto-advance, undo/redo; then
  what's narrowing the grid (folder, event, collection or smart filter, each
  a chip with ✖), search, rating, flag, label, **Filters ▾** (file type,
  camera, lens, NEF missing, undated, changed, unseen; the button counts the
  ones set) and Clear, with sort at the right. It no longer wraps to a third.
- **Left panel** scrolls as one, with folding section headings (small
  capitals, "+" at the right) and rows with counts right-aligned in muted
  text. Every row toggles: clicking a selected folder, event, collection,
  smart filter (including Recently changed), person or tag shows everything
  again, and a selected row shows ✖ on hover. People and Things list 8, then
  "Show all".
- **Grid cells:** badges, flag, rating, change dot and label strip sit on
  the image, not the cell around it (the type badge used to sit in a strip
  below landscape photos); 4 px gaps.
- Checked on a clone of the real library (1,537 photos): suggested tags were
  right in every contact sheet looked at (monkey, cat, cliff, sunset,
  swimming pool, glasses); People found the two travellers and a boatman,
  with one person split over three groups (hat, swimming; naming them alike
  merges them) and three groups of the cat (YuNet finds cat faces).

## 2026-10-17 — M6 faces: People
The spec says faces with InsightFace. InsightFace's models are licensed
"for non-commercial research purposes only", so Shoebox uses OpenCV Zoo's
**YuNet** detector (MIT) and **SFace** recognizer (Apache 2.0) instead:
39 MB, downloaded (pinned Hugging Face revisions, SHA-256 checked) when
Settings → People is turned on. Separate from semantic search, since faces
are more personal; both are off by default.

- **Running them:** a small ONNX interpreter on candle (`shoebox-ml::onnx`:
  the dozen operators these two models use; protobuf messages declared by
  hand, so no `protoc` at build time). Checked against OpenCV 5's own
  `FaceDetectorYN` / `FaceRecognizerSF` on the same pixels: identical
  detections and scores, and the same face similarities to 3 decimals.
- **Speed (M2 Max, CPU):** 32 ms to find faces in a photo (at 640 px),
  24 ms per face to describe it. candle runs grouped convolutions one
  group at a time, so depthwise ones are done directly (8× faster for
  SFace). These small models are 2–18× *slower* on Metal, so they stay on
  the CPU. Faces are aligned from a 1600 px decode (the detector sees 640).
- **Grouping:** each face joins the group whose average face it resembles
  most, if cosine ≥ 0.45 (SFace's own same-person threshold, 0.363, would
  merge sisters, who scored 0.40), then a second pass. Faces scored < 0.85
  or smaller than 2.5% of the image aren't grouped. Unnamed groups need 3
  faces to be listed. On 11 public portraits: Biden (4 photos), Michelle
  Obama (3, including a family photo) and Barack Obama (3, including the
  same family photo) grouped exactly; same-person pairs 0.68–0.84,
  different people ≤ 0.40.
- **Names are user state:** `people.json` in the data folder (not the
  cache, not the library), each person with up to 24 example faces and the
  faces you said aren't them. After any regrouping or rebuild, a group is
  the named person its faces resemble (average ≥ 0.45). Faces themselves
  are rebuildable rows in `embeddings.sqlite` (kind "faces"), following
  moved files like image embeddings.
- **UI:** People in the left panel (named, then unnamed groups, with photo
  counts); showing one puts a name field in the search bar, and for a named
  person "Add keyword to N photos" (`People|Name`, through the usual
  undoable marks path to sidecars) and Forget; "Not Name" in the photo
  menu; people in the current photo in the info panel.
- Not done: face rectangles drawn on photos, face thumbnails in the list,
  and writing face regions (MWG) to sidecars. Faces and image embeddings
  each read the file, so with both on a photo is read twice.

## 2026-10-17 — M6 objects: suggested tags from SigLIP
The spec says objects with YOLO. Ultralytics' YOLO is AGPL-3.0, which
doesn't fit an MIT/Apache app, and a detector's ~80 classes cover less of
what's in photos than tags do. Instead the SigLIP embeddings semantic search
already computes are scored against a built-in list of ~290 labels
(`shoebox-ml/src/labels.txt`: animals, places, weather, food, things,
activities, kinds of photo), with no extra download. No boxes.

- **The rule** (`embeddings::TagRule`): raw scores aren't comparable across
  labels or photos (SigLIP scores "stars" or "tea" well on many things, and
  some photos score well on every label), so a tag needs a *lift*: its
  score minus the label's 25th percentile over the library minus the
  photo's median over all labels. Tag if score ≥ 0.09, lift ≥ 0.045, lift
  within 0.03 of the photo's best; at most 5. A per-label z-score was tried
  first and dropped: it penalises common subjects (a library of mountains
  would rarely tag "mountain"); the percentile baseline only does that for
  subjects in over 3/4 of the library.
- On 11 sample photos: street/road/truck/city, fireworks, bicycle/city
  lights/alley/street, beach, cat (+ tiger), burger, mountain/snowy
  mountain/hill, apple, sunflower, city skyline/skyscraper/city/tower — and
  one dog photo that scores high on unrelated labels (dog + dancing,
  chocolate, stars, blossom). Needs checking on a real library.
- **Suggestions, not keywords:** the info panel shows them with a + that
  adds the keyword (to the sidecar, as any keyword). **Things** in the left
  panel lists tags by photo count; clicking one shows exactly those photos,
  best first. Label vectors are computed once (2.5 s) and stored; tagging
  re-runs when indexing ends and every 2,000 photos while it runs.

## 2026-10-17 — M6 semantic search
The spec says CLIP embeddings through ONNX Runtime (`ort`), stored in
sqlite-vec. What's built instead, and why:

- **candle, not ONNX Runtime.** Hugging Face's pure-Rust ML library, with
  Metal on Apple silicon and the CPU elsewhere (Accelerate on macOS). No
  C++ runtime to bundle and sign; follows "prefer pure-Rust crates". It's in
  its own crate, `shoebox-ml`, so the core and CLI don't build it.
- **SigLIP 2 base (patch 16, 224 px; Apache 2.0), not CLIP.** Better
  text-to-image retrieval, and multilingual. On sample photos every query
  put the right photo first, including "un chat" and "Hund".
- **Downloaded, not shipped.** It's off by default; turning it on (Settings →
  Search) downloads a pinned revision from Hugging Face (1.5 GB, once),
  checked against SHA-256 digests, resumable. It's converted to half
  precision and split into image and text halves (785 MB on disk, in
  Application Support). The download and conversion happen in the Caches
  folder; the float32 checkpoint is deleted there once converted (the
  safety rules allow deletes only in the app's cache directories). This is
  the only network use besides map tiles and update checks.
- **No sqlite-vec.** Embeddings are stored as 8-bit integers with a scale
  per vector, in their own `embeddings.sqlite` (not the cache: indexing
  takes minutes to hours, and a cache rebuild shouldn't lose it), and
  searched exhaustively in memory: 1.9 ms for 200,000 photos. sqlite-vec is
  an alpha C extension whose default table also compares every row.
- **Moves and renames don't recompute.** Rows are keyed by path and checked
  against size and mtime; a row whose file left the library is given to a
  new file with the same size and mtime (a move keeps both).
- **One embedding per photo:** the stack primary's image, decoded like a
  thumbnail (a NEF's embedded preview), newest photos first.
- **Thresholds, measured** on sample photos: a text query matches at cosine
  ≥ 0.07, where SigLIP's own match probability reaches about 1 in 10,000
  (true matches scored 0.08–0.13, unrelated photos below 0.06; "landscape"
  finds the mountain only, "portrait of a person" finds nothing in a set
  without people). Find Similar matches at ≥ 0.75 (unrelated photos
  0.46–0.73, the JPG and TIFF of one frame 0.99). Tune on a real library.
- **Speed** (M2 Max): indexing 13.5 ms a photo on the GPU in batches of 16
  (a 10k library in about 2½ minutes; CPU 38 ms), plus decoding on 4
  threads. A query: about 10 ms, plus 0.6 s to load the text model the
  first time. Memory: the image model (190 MB) only while indexing; the
  text model (565 MB, mostly its 256k-word vocabulary) from the first search
  until 10 minutes without one.
- **candle's Metal kernels are patched** (vendored in `vendor/`, one line):
  they don't compile as Metal 4, macOS 27's default, so the patch compiles
  them as Metal 3.2. Without it every GPU matrix multiply fails. See
  `vendor/README.md`; drop it when candle fixes this.
- UI: a **Content** switch by the search box (the box then takes a
  description; results are best match first, other filters still apply),
  **Find Similar Photos** in the photo menu, progress in the toolbar and the
  Search tab. Not done yet from M6's AI list: faces and objects.

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
  M1 decision. (Until 0.2.3 undated look-alikes were left undecided too,
  against this rule; a burst frame whose closest copy is undated is told
  apart by the kept copy's time.) Reference files never move; a group
  always keeps one file.
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

## Things counts follow the photos in view (2026-10-18)
Things counted every photo the search index last tagged, so a removed
folder's photos stayed in it until the next tagging pass (never, if no
photos were left). Like People, it now counts the tagged photos in view
(folder, events, collections, filters, search), before a chosen tag
narrows them; tags with none are hidden unless chosen.

## Import keeps folder names as events or keywords (2026-10-18)
An unorganised library's folder names (`Italy 2019`, `2025-photoshoot`)
are its only organisation, and filing by date throws them away. Import now
lists the source's folders with what each name becomes on its photos:
event (default), keyword or nothing (default for dates, camera folders and
generic names; the new dated folders already say the date). Events nest as
the folders do and reuse an existing event with the same path. A picture
in two folders gets both; a library photo found again gets them too.
- The copies' sidecars are written, not copied, when there are names to
  add: made from the source's sidecar plus the names, so the copy is still
  never an image write and the source is never touched.
- All of it is one batch (`FileOps::import` takes writes after the
  copies). Undo trashes a sidecar the import created (as it does the
  copies) instead of emptying it; library sidecars get their old bytes.
- A duplicate inside the source keeps the first file by path; a later
  copy's own sidecar isn't merged (as before).
- CLI: `shoebox import --folder-events` uses the defaults.

## Cloud-only files (2026-10-18)
Google Drive's streaming mode (and iCloud/Dropbox/OneDrive equivalents)
leaves dataless placeholders; any read downloads the file. A first scan of a
streamed folder would have downloaded everything (metadata, thumbnails,
hashes, embeddings, faces).
- Detection: `st_flags & SF_DATALESS` from `lstat`, which never downloads
  (`shoebox_core::cloud`). Linux has no equivalent: always false. Checked on
  a real Google Drive mount: the flag is set on streamed files.
- Not read means not cached: a cloud-only photo has no metadata row, so the
  scan after it's downloaded reads it. A photo read earlier and evicted
  later keeps its cached metadata, thumbnails and embeddings.
- Marks come from sidecars alone (the image's embedded XMP/EXIF rating
  would need the file).
- Downloads happen only when asked: opening in the loupe, or Download from
  Cloud (reading the file through; no write). Grid thumbnails of
  cloud-only photos come from the thumbnail cache or show a cloud.
- Semantic search and faces keep cloud-only photos' existing rows but don't
  compute new ones; their totals leave them out so indexing finishes.
- Moves and renames on a File Provider volume (`~/Library/CloudStorage`)
  aren't verified yet: `renamex_np(RENAME_EXCL)` there may be refused, in
  which case the batch fails cleanly (journaled, nothing overwritten).
- Tests can't make a real placeholder, so `cloud::pretend_cloud_only`
  marks a path as one (process-wide, cleared by `download`).

## Folder pickers are sheets, not blocking dialogs (2026-10-18)

- rfd's blocking `pick_folder` runs `NSOpenPanel.runModal` inside the frame,
  a nested event loop inside winit's handler. A repaint asked for by a
  background thread (loader, faces, search) while it's open aborts the app:
  "tried to handle event while another event is currently being handled".
- Every picker now goes through `picker::Picker`: rfd's `AsyncFileDialog`
  (a sheet on the window, no nested loop) started in one frame and polled
  in the next ones, with a waker that asks egui for a repaint. What the
  folder is for (`FolderFor`) is acted on when it's picked.
- Headless tests can't open a panel; checked by hand on a throwaway build.

## Undated movies take their modified time (2026-10-19)

- Movies exported from an editor often have no date in their header (`mvhd`
  creation time 0, no Apple creation date), so they went to `_undated/`.
  Their modified time is usually when they were made, so a movie with no
  header date is now dated by it: its capture time everywhere (grid,
  timeline, import and rename names). Photos aren't: a photo's modified time
  is often when it was downloaded or copied. Chosen by the user.
- `META_VERSION` 3, so cached movie rows are read again (every file's
  metadata is re-read once).

## Import: check left-out files, filter by folder, cancel (2026-10-19)

- Planning is `import::scan` (read and hash the source, and the same-size
  library files; slow) then `import::decide` (destinations; quick). The
  worker keeps the scan, so **Import anyway** on a left-out file
  (`ReplanImport(force)`) plans again in a moment, even for a big card.
  Forced files stay listed (`Match::forced`) so they can be left out again.
- Left-out files show their thumbnail beside the matched file's: a library
  photo's from the thumbnail cache, a source file's rendered uncached.
- The folder filter uses `Plan::origins` (the folders each picture turns up
  in), so a photo copied from one folder but repeated in another shows
  under both: it gets both names.
- Cancel is a flag shared with the library worker (`Library::cancel_import`;
  the worker is busy, so it can't be a command). Scanning stops with
  `CoreError::Cancelled`; copying stops before the next step with
  `BatchCancelled`: the rest marked not run, the batch `cancelled`, what ran
  kept and undoable. Sidecar writes come after the copies in an import
  batch, so a cancelled import's copies have no folder names yet; importing
  again adds them (they're then library photos found again).

## Import copy progress (2026-10-19)

- A copy across volumes (a card, another disk) goes 1 MB at a time so
  progress moves within a big file; on one volume it stays the system copy
  (an APFS clone: instant, no extra space). `FileOps::import_until` reports
  `CopyProgress` (files, bytes, the file being copied) as copies start, go
  and finish; the worker sends at most ~10 updates a second.
- The Import screen shows a bar by bytes, "3.0 GB of 12.0 GB · 51 MB/s ·
  about 3 min left", the file being copied and Cancel; the status bar shows
  "Importing 1/4 · 51 MB/s · …" from any view. Verification (reading the copy
  back for its BLAKE3) isn't in the bytes, so a file's bar can sit at its end
  for a moment.

## Import safety: quitting, cards, interrupted copies (2026-10-19)

- After an import from a card was cut off by quitting (and the card was
  then pulled without ejecting, and macOS's exFAT repair left it empty):
  - Quitting while an import copies asks first. The window's close button
    is held with `ViewportCommand::CancelClose`; the menu's Quit (⌘Q),
    which terminated at once, is pointed at `menu.rs`'s target and raises a
    flag the app reads. "Stop and Quit" cancels between files and quits
    when `ImportEnded` arrives (sent however the import ends).
  - While copying from `/Volumes/…` (macOS) or `/media`, `/run/media`
    (Linux), the screen says to keep it connected, and offers Eject after:
    `diskutil eject` / `gio mount --eject` on the library worker (it moves
    no files, so it isn't a `fileops` operation).
  - Rolling back an interrupted import trashes each pending copy's
    `.shoebox-tmp` (journaled in the undo batch). Resume already removed it
    before copying again.
- Shoebox only ever opens a card's files for reading; the journal shows no
  operation on the card in that import.

## Licence: GPL-3.0-or-later (2026-10-19)

- Shoebox moves from MIT OR Apache-2.0 to GPL-3.0-or-later, before any
  editing code lands (`docs/EDITING-SPEC.md`, Task 0.1). The Develop work
  ports raw-processing code from GPL projects (darktable, RawTherapee) and
  ships the RawNIND denoise models, whose code and weights are GPL-3.0;
  neither can go out under MIT/Apache.
- Versions up to 0.2.3 were released as MIT OR Apache-2.0 and stay so for
  anyone who took them. 0.3.0 is the first GPL release.
- The full licence text is `LICENSE`, and the app bundle carries it as
  `Contents/Resources/LICENSE.txt`. The public repo and GitHub Releases make
  the source available. Third-party parts (Sparkle, Phosphor, SigLIP 2,
  YuNet, SFace, candle) are MIT or Apache-2.0, which GPL-3.0 accepts;
  `vendor/candle-metal-kernels` keeps its own licence.
- Shoebox will not be sold.

## Editing models: original weights, native on candle (2026-10-19)

- Every Develop model (`docs/EDITING-SPEC.md`, Task 0.3) runs on candle in
  `shoebox-ml`, written natively in Rust as SigLIP 2 is, with weights from
  the original authors: RawNIND UtNet2 (Bayer; joint denoise and demosaic)
  and SAM 2.1 Hiera (image predictor) in Phase 3, SegFormer for skies in
  Phase 4. darktable-ai's notes guide how each is fed; its ONNX packages
  aren't used.
- Still no `ort` (as M6 decided), and `shoebox-ml::onnx` stays the small
  interpreter for YuNet and SFace; it doesn't grow for these models.
  `shoebox-develop` reaches them through `shoebox-ml`.
- RawNIND's weights exist only in the author's Google Drive, which can't be
  a download source. They are converted once to safetensors
  (`scripts/convert-rawnind.py`, written with the port in Phase 3) and
  published on a `models-rawnind-1` release; `shoebox-ml::download` pins
  the URL and SHA-256. Weights never go in git. It runs in f32: its
  activations overflow f16.
- If candle takes over 30 s a frame for RawNIND, the fallback is Core ML
  through `objc2-core-ml`, not `ort`.

## Develop, Phase 1: raw decode (2026-10-19)

- `shoebox-decode::raw` decodes with `rawler` 0.8.0 (pure Rust, LGPL-2.1)
  and prepares the data the pipe starts from: cropped to the image area
  (DNG crop, else active area), black subtracted per CFA position (the
  pattern counted from the active area, DNG's rule), normalised so white is
  1.0, never negative, values above white kept. White balance, highlights
  and demosaic stay in the pipe. `CLIP` (0.98) marks clipped photosites.
  rawler has `todo!()`s and asserts for unexpected inputs, so it runs inside
  `catch_unwind`: a bad file is an error, not a crash. Only 2×2 RGB Bayer
  data is accepted.
- The real Z f fixture is **High efficiency★**, and rawler refuses it.
  Assumed typical of the archive, so the DNG route (EDITING-SPEC, RAW
  decode) is how existing photos get edited, not an edge case. New photos
  will be Lossless (the camera setting is changing) and go to rawler
  directly.
- Tested on the same file, macOS 27's `CIRAWFilter` decodes HE★ properly
  (decoder versions 7 and 8; 6048×4032 in 1.8 s; at −3 EV the sparklers
  keep detail the camera JPEG has blown). It gives demosaiced linear RGB,
  not the CFA, so it isn't used: the decision was the spec's DNG route.
- `NEFCompression` is read from Nikon's maker note at scan time (Z bodies:
  tag 0x51, u16 at byte 10, little-endian as rawler reads it; older bodies:
  tag 0x93), stored as `exif.nef_compression`, and shown in the Info panel
  with a note on High efficiency files. The maker note is within the first
  256 KB, so scans read nothing more.
- Cache schema 8 adds that column with `ALTER TABLE` instead of the usual
  rebuild, which would have discarded every duplicate hash;
  `META_VERSION` 4 re-reads metadata (the cheap part) to fill it.
- **DNG conversion** (`shoebox-develop::dng`), measured on the HE★
  fixture with Adobe DNG Converter 18.7: `-c -p0` (lossless-JPEG DNG
  1.4, no preview) takes 0.5–1.4 s and makes a 22 MB DNG; rawler decodes it
  in 0.11 s (uncompressed `-u`: 49 MB, 0.03 s). A rough half-size render
  from it (binning, as-shot WB, D65 matrix) matches the camera JPEG's
  colours. No command-line switch was found that makes JPEG XL DNGs
  (`-jxl`, `-dng1.7.1` changed nothing), and lossless JPEG is fast
  enough, so the spec's JXL option is dropped. Never run the converter
  with `-h`: it opens its window instead of printing help.
- The DNG cache is keyed by the NEF's BLAKE3, computed when converting (the
  catalog's hash is only there after duplicate analysis). The spec says the
  converter is proven by converting a small bundled test file; the repo is
  public and can't carry a real NEF, so instead each DNG is decoded by
  rawler before it's kept, and a failed conversion is reported as such.
  Conversions run two at a time; a file asked for twice converts once.
