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

## Develop, Phase 1: the edit document (2026-10-20)

- `shoebox-develop::edit`, as EDITING-SPEC's Edit document, with these
  choices:
  - `source.blake3` instead of `source.xxh3`: Shoebox already hashes files
    with BLAKE3 (duplicates, imports), so one hash serves both.
  - Settings are typed structs whose defaults live in code; on save they're
    serialised and everything equal to the default is pruned, so an
    untouched photo stores `{}`. Modules this build doesn't know are kept
    verbatim (`Modules::other`).
  - History: each step is a JSON Patch from the previous (pruned) settings.
    Undo, redo and jump replay patches from the defaults (cheap: a few
    dozen small patches). `history_at` records an undone position; a new
    change drops the undone steps and any snapshot past them. A slider drag
    is one step (`amend`), and a drag back to the start removes it.
    History that doesn't replay to the stored settings (a hand-edited file)
    is dropped and the settings win.
  - Values are `f64` so files read `0.7`, not `0.699999988`.
  - `render_hash`: BLAKE3 of the process version and pruned settings, not
    history or names; it keys rendered previews and stale exports.
  - Phase 1 modules: `raw` (demosaic RCD, highlights inpaint-opposed),
    `white_balance` (as shot, presets, custom temp/tint), `input_colour`
    (matrix), `exposure`, `display_transform` (contrast, white and black
    EV offsets, preserve hue), and `geometry` (rotate, crop as fractions of
    the upright rotated frame, aspect).

## Develop, Phase 1: where edits are stored (2026-10-20)

- Safety rule 3 is widened (as EDITING-SPEC asks) from `.shoebox/*.json`
  to `.shoebox/**/*.json`: `fileops` takes JSON anywhere inside a root's
  `.shoebox`, makes (and journals) missing folders there only, and
  `write_edits` takes only `.shoebox/edits/…`.
- Edit files: `<root>/.shoebox/edits/<rel>.json` (master) and
  `<rel>.<copy id>.json` (virtual copies; ids have no dots). The file's
  location says which photo it belongs to; `source.path` inside is a note
  brought up to date on the next save, so a move never has to rewrite it.
- Saves are journaled like every other write, as batches of kind `edit`
  (previous contents kept). Library ⌘Z and the History list skip them:
  edits are undone in Develop's own history. Undoing a first save leaves
  `{}`, which reads as "no edit".
- `plan_moves` and `plan_group_moves` find a photo's edit files the way
  they find its sidecars, so every move, rename, organise and duplicate
  clean-up carries them in the same batch, under the same `-N` suffix.
  A photo moved out of every library root leaves its edit where it is.
- Photos moved in Finder: an edit whose photo is gone is matched by
  `source.size` (added to the document for this), then BLAKE3, and moved
  to its photo's edit path. Unmatched edits are kept, never deleted.
- Catalog: `edits(root, rel_path, copy_id, doc_json, doc_hash, updated_at,
  saved_at)` in `cache.sqlite`, keyed by paths rather than file ids and
  outside the rebuildable set, so a cache rebuild can't lose an unsaved
  edit. `save_pending` writes every unsaved row as one batch (the app
  calls it after 2 s idle and on leaving a photo); `load` takes the file
  when it was written after the row (another Mac, a restore).

## Develop, Phase 1: the CPU reference pipeline (2026-10-20)

- `shoebox-develop::cpu` runs the stages in order on the CPU: white
  balance on the mosaic → highlights → demosaic → proxy downscale (box, by
  a whole factor) → camera → linear Rec.2020 → exposure → EXIF orientation
  → rotate and crop (one bilinear resample) → display transform → output
  space and sRGB transfer curve. Exports use it until the GPU pipe exists;
  then it's what the shaders are tested against. 24 MP renders in 1.2 s
  (M2 Max, release, partly single-threaded).
- **Ported from darktable** (commit 2747ed2a, downloaded with permission):
  RCD (`src/iop/demosaicing/rcd.c`) and inpaint opposed
  (`src/iop/hlreconstruct/opposed.c`, `_calc_refavg` from `segbased.c`).
  Whole-image buffers instead of tiles; RCD's 9-pixel border comes from
  the bilinear (box3) demosaic instead of PPG. Highlights clip at darktable's
  0.987 of each channel's white-balanced white (the spec's 0.98 is close).
  RCD beats bilinear by over 1 dB on all four Bayer layouts of a synthetic
  scene; on the Z f fixture it shows no mazes or fringes at 100%.
- **White balance** follows the DNG SDK: Robertson temperature and tint,
  dual matrices blended by inverse temperature, the as-shot white found by
  iteration, Bradford to D65. White-balanced raw values keep their physical
  meaning (the matrix undoes the multipliers relative to green), so moving
  white balance doesn't change brightness. Presets use Lightroom's values.
- **Display transform**, Shoebox's own (the spec's "AgX-style" left the
  details open): in stops from mid grey, grey stays at display grey (0.18
  linear) with a slope of 0.17 display units per stop (the contrast slider
  scales it by 2^(c/100)); above, a shoulder `t/(1+t^p)^(1/p)` reaches 99%
  at the white point (4 stops up by default: the sensor clips ~2.5 up);
  below, an exponential fall-off like a gamma encode's, pulled to zero at
  the black point (10 stops down). A first attempt (one logistic with a
  power to pin grey) crushed the midtones whenever the range changed. Before
  the curve, primaries are pulled 15% towards grey and pushed 10% back
  after (AgX's idea), so bright saturated colours roll off to white. With
  "preserve hue", the brightest channel is tone-mapped and the others
  scaled with it, blending to per-channel near white.
- **Baseline exposure** +0.75 stops before the slider: on the HE★ fixture
  that puts luminance percentiles (5/25/50/75/95/99%: 1 4 10 24 123 248)
  close to the Z f's own JPEG (3 5 9 19 137 252). To be tuned on the
  30-NEF set; Auto exposure (Phase 2) refines it per photo.
- `examples/develop_probe.rs` renders files (HE NEFs through the
  converter) with slider values from the environment, for tuning.

## Develop, Phase 1: the GPU live pipe (2026-10-20)

- `shoebox-develop::gpu`: one wgpu compute pass (`develop.wgsl`) that runs
  the live pipe per output pixel from the demosaiced image in an
  `rgba32float` texture: geometry map → bilinear sample → colour matrix →
  display transform → output. It takes any `wgpu::Device`, so the app
  shares eframe's (wgpu 30, the same features). Output `rgba32float` for
  read-back (tests, export) or `rgba16float` for display.
- **White balance moved after demosaic** (darktable's "late correction"):
  the mosaic is balanced at as shot for highlights and demosaic, cached,
  and a different white balance is a per-channel ratio folded into the
  colour matrix. Without it every white balance move re-ran highlights and
  RCD (0.34 s); now it's instant. The CPU reference was restructured the
  same way (`cpu::demosaic` once, then `cpu::frame` + `cpu::apply`), so
  both pipes run the same numbers.
- Orientation, rotation and crop are one affine map (`geometry_map`),
  worked out in f64 and shared; one bilinear sample per pixel.
- **Checked against the CPU** within 1e-4 on gradients, colour ramps, a
  checkerboard and values up to 8, across settings, display and scene
  output. Rotated cases are allowed 1e-3: the GPU fuses the map's
  multiply-adds, moving a sample position by ~1e-6 px, which a
  checkerboard edge amplifies (a quarter of an 8-bit step). On the real
  HE★ fixture the largest difference is 5.4e-7. Tests skip without an
  adapter (Linux CI).
- **Speed** (M2 Max): a 2560×1707 proxy renders in 1.4 ms per frame (the
  spec allows 33). Opening a photo: decode 0.11 s + highlights 0.14 s +
  RCD 0.24 s (now parallel by rows; was 0.59 s single-threaded), so about
  0.5 s to a sharp proxy for a Lossless NEF or a converted DNG (0.5–1.4 s
  more for a first conversion).
- **Stage cache**: with one cheap pass, a slider re-renders the whole live
  pipe; the expensive part (demosaic) is the cached `cpu::Demosaiced`.
  Stages that cost more (dehaze, local tone, Phase 2) will get their own
  passes and cached textures.
- `Pipe::new` returns validation errors (a driver rejecting the shader)
  instead of wgpu's default panic.
- RCD is still on the CPU; the spec's GPU RCD waits until the open-time
  target needs it.

## Develop, Phase 1: the Develop view (2026-10-20)

- `View::Develop` (D; G or Escape back; arrows and the filmstrip step).
  Right panel: white balance (as shot, presets, custom temperature and
  tint), exposure, tone (contrast, whites, blacks, preserve hue), rotate
  (setting the largest crop for the angle), raw (highlights, demosaic).
  Each module has Reset; double-click resets a slider. Left panel: the
  edit's history (click to jump). ⌘Z/⌘⇧Z walk that history in Develop,
  not the library's. `\` shows before/after. A slider drag is one step.
- The develop thread (`develop.rs`) prepares a photo off the UI thread:
  High efficiency NEFs through Adobe DNG Converter (cached), decode,
  demosaic to a 2560 px proxy; the newest request wins and the last four
  are kept. The embedded preview shows meanwhile, with "Preparing raw…" or
  "Converting to DNG…"; without the converter, the spec's message and a
  Download Adobe DNG Converter button.
- The live pipe renders on eframe's own wgpu device into an
  `rgba16float` texture registered with egui-wgpu (egui 0.37 treats
  texture values as gamma-encoded, which is what the shader writes), so
  nothing goes through the CPU; without a GPU the same pipe runs on the CPU
  into an egui texture. Output is Display P3 on macOS (the window's layer
  is already tagged P3), sRGB elsewhere.
- Edits: loaded from the catalog or `.shoebox/edits` through the library
  worker; a change is handed to the catalog 0.3 s after sliders are
  released, and written to its file after 2 s idle, on leaving the photo
  or Develop, and at the next launch if the app quit first. The worker
  fills in the source's BLAKE3 and size on first save.
- Reference-root photos open read-only. Only NEFs for now (JPEG and TIFF
  editing to come).
- Still to come in Phase 1: the histogram and clipping warnings, a crop
  tool drawn on the image (crop is set by rotation only so far), JPEG/TIFF
  export, edited previews in the other views, and Settings → Raw.

- **Histogram and clipping** (Develop): a second compute pass counts the
  render (R, G, B and luma, 256 bins of 8-bit values, luma weighted as the
  library's thumbnails are) into atomic bins, read back without stalling
  a frame (mapped, then polled on later frames). It sits at the top of the
  right panel, as Lightroom's does, rather than in the top bar the spec
  names, with the share of clipped highlights and shadows. J (or the
  Clipping toggle) paints blown highlights red and crushed shadows blue,
  in the shader and the CPU pipe alike. Checked against `cpu::histogram`.
- **Before and after**: Edited / Original / Split over the photo, `\`
  flicking to the original and back, Y for the split (original left of a
  divider to drag, the edit right of it). The original keeps the edit's
  crop, rotation and raw settings so the halves line up; both renders
  share the GPU pipe and the uploaded image. Editing while the original
  shows goes back to the edit; a split stays, to watch the change.
- **Every still image is editable**, not only raw files. A finished image
  (JPEG, TIFF, PNG, HEIC) is decoded with its colour profile to Display P3
  at the proxy size (8 bits a channel for now), linearised into Rec.2020,
  and marked display-referred: the defaults reproduce it exactly (within
  one 8-bit step), with no scene curve, AgX inset or baseline exposure.
  Exposure is a linear gain; contrast, whites and blacks are a levels and
  S-curve on 2.2-encoded values that's the identity at its defaults (same
  directions as for raws); white balance is relative, as Lightroom's is
  for JPEGs (as shot is no change, shown as 6500 K and tint 0; a preset or
  custom white is adapted to D65, so Tungsten neutralises warm light). No
  raw module. HEIC comes from ImageIO already upright. GPU and CPU agree
  within 1e-4 here too. Movies can't be edited.
- **Crop tool** (R, or Crop… under Crop & rotate): the whole rotated frame
  shows with the crop over it (outside darkened, thirds inside); drag a
  corner, an edge or the middle; an aspect (Free, Original, 1:1, 3:2, 4:3,
  16:9, 5:4, either way round) holds while dragging; a step that would
  take the crop outside the rotated photo is refused. Enter, R or Done
  applies it as one history step ("Crop"); Escape or Cancel leaves it.
  Rotating still sets the largest crop for the angle.
- **Edited previews everywhere else**: the library worker lists photos'
  edits (when the library opens, then each change), keyed by the photo's
  BLAKE3 and the edit's render hash (an edit that changes nothing has no
  key). The develop thread renders each edit at 2560 px into
  `<cache>/edited/<key>.jpg` (Display P3 on macOS, with its ICC profile),
  behind any photo being opened, and skips ones already there. The loader
  then shows that JPEG instead of the original for every size in every
  view (falling back to the original if it can't be read), and the tile's
  badge reads "· EDITED". Duplicates and semantic search still use the
  original. JPEGs are written by the pure-Rust `jpeg-encoder` (the spec
  names mozjpeg, a C library) with profiles from `moxcms`; a round trip
  through the library's decoder keeps colours within 3/255.
- Not yet: the loupe's `\` to show the original (the loupe keys textures
  by photo and size only), and 100% zoom on an edited photo shows the
  2560 px preview, not a full-resolution render.
- **Settings → Raw**: Adobe DNG Converter's status (found, with its
  version, or not), Download and Check again, a path for a converter
  installed elsewhere (a text field: macOS's folder picker treats `.app`
  bundles as files), how many photos in the library are High efficiency
  (from the catalog), and the DNG cache: its limit (2–200 GB, default
  20), the space in use, and Clear. Finding the converter and measuring
  or clearing the cache run on the develop thread, so the UI thread never
  touches the disk. The spec's "pre-convert likely edits when idle" option
  isn't built yet.

## Develop, Phase 1: export (2026-10-20)

- `shoebox-develop::export`: presets (format and options, colour space,
  size, metadata, folder, naming) as JSON in `export-presets.json` in the
  data folder (one file rather than a folder of them), starting with
  "JPEG · Full size", "JPEG · Web 2048" and "TIFF · 16-bit" (ZIP, Adobe
  RGB). JPEG through the pure-Rust `jpeg-encoder` (not mozjpeg, a C
  library); 16-bit TIFF (LZW or ZIP) through `tiff`; each tagged with its
  profile (sRGB, Display P3, Adobe RGB, ProPhoto; ProPhoto adapted to
  D50, with the 1.8 curve, Adobe RGB 2.2). Long edge or megapixels with a
  separable Lanczos-3 (Shoebox's own, not `fast_image_resize`), never
  upscaling. Names from `{name}`, `{preset}`, `{date}`, `{seq}`; a taken
  name gets `-1`, `-2` (files are only ever created, `create_new`).
- Exports run on their own thread (so Develop never waits on one), at full
  resolution through the CPU pipe (High efficiency NEFs through the DNG
  cache, whose temporary folders are now per thread). One at a time rather
  than the spec's two concurrent.
- Metadata: with exiftool installed, the original's tags and its
  sidecar's XMP are copied in ("all"); "all but location" copies an
  allowlist instead (camera, lens, exposure, dates, author, copyright,
  caption, keywords, rating, label), so no location field of any format
  can come through, tested against a file carrying GPS, IPTC, XMP and IPTC
  Extension locations; "copyright only" copies just that, Orientation set to 1 (the pixels are upright) and the
  export's own ICC profile kept. Without exiftool, files go out without
  metadata and the notice says so.
- Exports never go inside a library folder (compared as real paths: a
  first version compared `/var/…` with the root's `/private/var/…` and
  let one through; the test now covers it). Exporting into the library,
  which the spec allows as an explicit choice, waits for safety rule 3 to
  be amended.
- The dialog (Shift-⌘E, or Export… in the photo menu) ticks presets and
  edits them; ⌥-Shift-⌘E repeats the last export on the selection (the
  spec's ⌘E is already "new event from selection"). Progress is in the
  status bar; a notice says where files went. Each export is recorded in
  the catalog (`exports`, outside the rebuildable set, following photo
  moves) with the preset and the edit it came from; the Info panel lists a
  photo's exports, flagged stale (the edit changed) or missing.
- **Files already there** (decided: ask). Before writing anything, the
  export thread works out every destination; if any exist, the export
  waits and the app asks once for the batch: Replace, Save as new files
  (`-1`, `-2`), or Cancel. Replacing writes the new file beside the old and
  renames it over, so a failed export leaves the old file whole; two photos
  in one batch landing on one name keep both. Safety rule 4 now says so
  (exports only, never in the library). Re-export (Info panel, per export)
  renders again with the recorded preset to the same name, so the same
  question comes up; the catalog keeps one record per file. Output
  sharpening and the PNG, AVIF and JPEG XL formats are later phases'
  items.

## Develop: rotate and flip, the white balance eyedropper (2026-10-20)

- Geometry gains `quarter` (turns clockwise) and `flip`. EXIF orientations
  and the edit's turns and mirror form one group of eight, so they compose
  into a single orientation code (`compose_orientation`) inside the
  geometry map: no extra resample, the GPU unchanged. A turn turns the
  crop rectangle and swaps a `w:h` aspect with it; a turn after a mirror
  goes the right way on screen. Rotate left / right (⌘[ / ⌘]) and Flip
  under Crop & rotate.
- Eyedropper (W, or Pick neutral): a 5×5 patch of the source under the
  click, mapped back through the render's own geometry; for a raw, its
  as-shot balance undone gives the camera neutral (through the matrices,
  to temperature and tint); for a finished image, its chromaticity.
  Applied as a custom white balance, clamped to the sliders' ranges.
  Relative white balance for finished images now adapts to D65 itself
  (not a Planckian 6500 K, which isn't quite D65), so a picked grey comes
  out grey.

## Develop: zoom to 100% (2026-10-20)

- Z, Space or a double-click toggle fit and 100% (one full-size pixel per
  screen pixel); drag or scroll pans. The proxy shows enlarged at once (the
  draft); the develop thread then renders just the visible part, plus a
  third again on each side, at full resolution on the CPU
  (`Frame::region`: the same map from the region's corner, tested equal to
  that part of a whole render) and it's drawn over the draft. A new region
  is asked for when the view moves past it or the edit changes; the
  newest request wins. The full-size demosaic of the photo being zoomed is
  kept in the develop thread (about 290 MB for 24 MP) rather than uploaded
  as a 390 MB float texture. Zoomed views show the edit (not the
  original or a split); crop and the eyedropper work at fit (the
  eyedropper at 100% too).

## Edits after photos move in Finder (2026-10-20)

- After every full scan (start-up, Rescan, roots changed), the library
  worker lists each writable root's edit files; one whose photo isn't on
  disk is matched by size, then BLAKE3, to a photo without an edit of its
  own and moved to that photo's edit path. All the moves are one journaled
  batch, "Reattach edits" (undoable from History like any move); the
  catalog rows follow, and a notice says how many were found. Watcher
  updates don't reattach; the next full scan does.
- Edits that match nothing stay where they are and are listed under
  "Edits without a photo" in Settings → Raw, with Show in Finder. They're
  matched again on every scan, so a photo that comes back gets its edit.
  Empty edits (`{}`, an undone first save) aren't listed.

## Snapshots (2026-10-20)

EDITING-SPEC calls snapshots "named pointers into history". A pointer alone
is lost when a change after an undo discards the steps it points to, and a
slider drag folded into the step a snapshot points at would change the
snapshot. So:

- A snapshot points into history while those steps exist. When they're about
  to be discarded, it takes a copy of its settings (`settings`, defaults left
  out) and keeps that from then on. A drag right after a snapshot starts a
  new step instead of amending the snapshotted one.
- Going back to a snapshot is a new history step ("Snapshot: name"), as in
  Lightroom, rather than a jump: nothing is discarded and ⌘Z undoes it.
- Snapshots live in the left panel above History: + names the edit as it is
  now (the name is editable straight away); right-click renames or deletes.

## Importing a reference photo to edit it (2026-10-20)

Develop opens a photo in a reference folder read-only and offers "Import
this folder…" (EDITING-SPEC: "offers to import them"). Import works on
folders, so it imports the photo's folder; the Import view shows the plan and
nothing is copied until Import is clicked, as for any import. Import used to
leave every such file out as "already in the library", because each matched
itself. Now a source file in a reference root doesn't count as its own match.
A copy elsewhere in the library still does, and so does a card's photo that a
reference folder already has.

## Converting ahead (2026-10-20)

EDITING-SPEC's "hiding the wait" for High efficiency NEFs:

- **Neighbours.** Opening a photo in Develop queues the photos either side
  of it (in the order shown) for conversion. They run on a thread of their
  own, one at a time, so the converter's second slot stays free for the
  photo being opened.
- **Idle pre-convert.** This is a setting in Settings → Raw, off by default.
  "When the Mac is idle" is taken to mean a minute with no input to Shoebox;
  watching system-wide idle would need an IOKit FFI wrapper for little
  gain. The list is picks and photos rated 3★ or more, outside reference
  folders, newest first. It is capped at half the DNG cache (about 30 MB
  per DNG) so it can't evict everything else. Any input stops it once the
  file being converted finishes.
- **One cache.** The develop, export and convert-ahead threads now share one
  `DngCache`, so "at most two conversions at once" holds across all of them.
  Before this, the export thread made its own cache and could run two more.
  The cache limit became settable in place for this.

## Two exports at once (2026-10-20)

EDITING-SPEC's export queue runs "2 concurrent". An export works on two
photos at a time. Export jobs still run one after another, because the
status bar and the clash question each follow a single job. Output names
are planned before anything is written, in the photos' order, so the
`-1`, `-2` suffixes don't depend on which thread finishes first. Writes still
never overwrite (rule 4); Replace replaces only files that were there before
the export.

## Golden images and benchmarks (2026-10-20)

- **Goldens are 16-bit TIFFs, not EXR.** They are display-encoded renders,
  which is what the ΔE2000 comparison measures, and 16 bits is far finer
  than the 0.5 mean limit. `tiff` is already a dependency; an EXR writer
  would add one for nothing. The comparison treats both sRGB and Display P3
  with the sRGB curve.
- **Two sets.** A synthetic set (three edits of a generated DNG) is
  committed under `crates/shoebox-develop/tests/golden/` and runs in CI. The
  real set (the 30 definition-of-done NEFs and their edit documents) stays
  outside git and runs with `SHOEBOX_GOLDEN_DIR`. Real files are rendered on
  the CPU at a 2048 px long edge, to keep the run short; the GPU pipe is
  already held to the CPU one within 1e-4. Each set's `manifest.json`
  records the process version, and a mismatch fails until the goldens are
  blessed again (`SHOEBOX_GOLDEN_BLESS=1`). That makes a process-version
  bump a deliberate regeneration, as the spec asks.
- **Benchmarks aren't a CI gate yet.** `benches/pipeline.rs` covers decode,
  demosaic (proxy and full), the proxy render and export. The spec's "fail
  CI on a >20% regression" needs a stable machine and a stored baseline;
  GitHub's shared runners vary by more than 20% from run to run. For now
  the check is local: save a baseline on main, run the branch against it,
  then `examples/bench_check.rs` fails on any mean more than 20% slower.
  First numbers on this Mac (synthetic 24 MP): decode 28 ms, RCD 187 ms
  (proxy and full alike: it demosaics at full size, then downscales), CPU
  proxy render 55 ms (the live GPU pipe is the one held to 33 ms), Lanczos
  to 2048 px 23 ms, JPEG encode 184 ms.

## Exports into the library; rule 3 amended (2026-10-20)

Decision: Shoebox may write new files into the library, but never modify
originals. Rule 3 now reads that files already in a library are never
modified or replaced. New files may be added (imports, and exports the
person sends there), always through `fileops`. Rule 4's Replace exception
applies only outside the library.

An export whose folder is inside a writable root is rendered into
`<cache>/export-staging/`, with metadata copied there. The library worker
then copies it in with `fileops::import`: journaled, BLAKE3-verified, never
replacing. The batch is called "export into the library", ⌘Z moves its files
to the Trash, and the cache copies are removed afterwards. Inside the
library a clash never asks: the export just takes the next free `-1`, `-2`
name. Reference folders are refused. Exported files are ordinary library
files from then on, so they show up in the catalog.

## Phase 1 gate deferred; no other editors in testing (2026-10-20)

- The Phase 1 gate (30 NEFs clean, ColorChecker ΔE2000 ≤ 3) needs new
  photos, a ColorChecker shot among them, so it's deferred until those are
  taken. Phase 2 starts now. When those photos are taken, add a frame of
  straight lines (a building or door frame) near the edges, shot with the
  most distorting lens available. It confirms the convention used for the
  NEF's own distortion coefficients (see Lens corrections). The gate still has to pass before a release
  calls Phase 1 done. The 0.3.0 release isn't held for it.
- Testing never depends on licensed editors (PhotoLab, DeepPRIME, PureRAW,
  Lightroom): none is available. EDITING-SPEC's definition of done, the
  AI-denoise quality gate, the Phase 3 gate and the risks now judge Shoebox
  on its own, with the camera's JPEG as the only outside reference, plus
  measurements (ColorChecker, golden images). Other editors are still named,
  but only as sources of features to build.

## Phase 2, first chunk: colour and tone curve (2026-10-20)

- **Order.** These per-pixel modules come first because they fit the single
  GPU pass as it is: calibration, colour (vibrance, saturation, HSL), black
  and white, tone curve. Neighbourhood modules (highlights and shadows,
  clarity, dehaze, sharpening, denoise) come next, together with the
  multi-pass pipe they need.
- **Where they run.** Calibration is a 3×3 matrix on the working primaries.
  It's folded into the frame's colour matrix, so it costs nothing per pixel,
  and white stays white. Colour and black and white run per pixel in Oklab,
  reached from linear Rec.2020 through a composed LMS matrix, before the
  display transform. Scene-linear values above 1 are fine, because Oklab's
  cube root is homogeneous. HSL luminance scales linear light, ±1 stop at
  ±100, so it keeps hue. All the colour sliders fade out on greys, by chroma
  relative to lightness.
- **The tone curve deviates from the spec.** EDITING-SPEC places it before
  the display transform, as "a monotone cubic in log space". It runs instead
  on the display transform's own display values (2.2-encoded, before the
  output space). A curve the user draws then matches what the screen and
  histogram show, and doesn't fight the sigmoid. It's still independent of
  the export space, because it runs before the output matrix. Point curves
  are monotone cubics (Fritsch–Carlson); the parametric regions are smooth
  humps. Both are baked into a 1024-entry table per channel, kept
  non-decreasing.
- **Whites and Blacks.** These stay the display transform's white and black
  points (Phase 1). Module 7 adds Highlights and Shadows (guided filter)
  next chunk, not a second Whites/Blacks pair.
- Module sections now open and close (the arrow, or a click on the title).
  A point on the curve is removed by double-clicking or right-clicking it.

## Phase 2, second chunk: dehaze, highlights and shadows, clarity (2026-10-20)

- **A fast guided filter instead of extra GPU passes.** The coefficients
  `(a, b)` come from a small copy of the source (512 px long edge, made
  once and cached on the source). The live pass samples them (bilinear) at
  each pixel's source position and evaluates `a·I + b` with the pixel's own
  guide, so edges stay sharp at full size (He and Sun, 2015). Working in
  source space means crop, rotation, the proxy, 100% regions and export
  all use the same maps. The GPU needs one storage buffer, re-uploaded only
  when the maps change.
- **The maps depend on the colour matrix and the dehaze amount**, not on
  the slider amounts. Dragging highlights, shadows or clarity reuses them.
  Dragging exposure or white balance recomputes them: 9 ms with all three
  on (benchmark `local maps`), after replacing the airlight sort with a
  selection and making the box filter's vertical pass cache-friendly. A
  768 px copy took 35 ms.
- **Highlights and shadows:** the base layer is log luminance, guided by
  itself (radius 2.5% of the long edge, ε 0.25 stops²). Each slider is up
  to ±1.5 stops, weighted by the base's tone. Detail keeps its contrast and
  there's no halo (both tested).
- **Clarity:** the same at 6% (ε 1). The detail above the base is scaled
  in the midtones.
- **Dehaze:** the dark channel prior (airlight from the haziest 0.1%,
  ω 0.95, t ≥ 0.1). The transmission is refined by a guided filter on
  luminance. A negative amount mixes towards the airlight.
- Texture, sharpening and the simpler denoiser need fine neighbourhoods at
  the source's own resolution. They come next, as neighbour taps in the
  live pass.

## Phase 2, third chunk: noise reduction, sharpening, texture (2026-10-20)

- **One wavelet stage does all three.** A generalised Anscombe transform
  per channel stabilises the noise; its gain and read variance are
  estimated per source from the flattest 8×8 blocks, with a robust line
  fit. An orthonormal luma/chroma basis keeps the noise at unit variance.
  Then come five à-trous (B3) scales:
  - **Noise reduction:** soft thresholds, separate for luma and colour.
    Detail keeps part of what the luma threshold removed.
  - **Sharpening:** gain on the luma bands that match its radius, masked
    by edge strength (Masking), with halos soft-limited (Detail).
  - **Texture:** gain on the middle bands.

  This follows EDITING-SPEC's tier 1, but runs on the demosaiced source
  rather than the CFA data, so finished images get it too. Sharpening is an
  enhancement of the wavelet bands, not Richardson–Lucy deconvolution: that
  deconvolution is iterative and lens-specific. It can come with lens
  corrections.
- **Radii are in the original's pixels.** `Demosaiced::scale` moves the
  bands, so a proxy shows roughly what the full render will. A radius that
  falls below a pixel fades out ("best judged at 100%", as in Lightroom). A
  finished image's proxy currently has scale 1, so its preview sharpening
  is a little strong; the full size is right.
- **It runs on the CPU for now, live view included.** 29 ms at proxy size
  (benchmark `detail stage at proxy`), inside the spec's 50 ms. The result
  is cached on the source and uploaded as the GPU pass's source, so only
  dragging a detail slider re-runs it; the live pass doesn't change. The
  unprocessed image stays uploaded for Original and Split. A GPU version of
  the stage can come later if drags feel slow.
- Defaults are zero. Auto (later in Phase 2) will set noise reduction by
  ISO, as the spec says.

## Lens corrections (2026-10-20)

- **The NEF's own data.** It isn't in the MakerNote. It's in NikonNEFInfo
  (tag 0xC7D5 in the raw image's SubIFD): tag 5 is DistortionInfo, tag 6
  VignetteInfo. Both store their normalising radius (88474/4096 = 21.6 mm,
  the half-diagonal of a full-frame sensor), so r = 1 at the frame's
  half-diagonal.
  - **Vignetting** is a correction gain 1 + v1·r² + v2·r⁴ + v3·r⁶. On the
    test NEF (Z 40mm f/2 at f/4) it gives 2.32× at the corner, against
    Lensfun's 2.38× for the same lens and aperture.
  - **Distortion:** a corrected pixel at radius r comes from radius
    r·(1 + k1·r² + k2·r⁴ + k3·r⁶) in the raw. Block-matching the raw against
    the camera's own corrected JPEG (`examples/lens_probe.rs`) fits that
    convention best (0.43 px RMS, against 0.90 px the other way round). The
    40mm's distortion is too small for the match to prove it, though, so a
    shot of straight lines with a more distorting lens should confirm it
    (added to the deferred gate's checks).
- **Order of sources.** As the spec says: the camera's data first for
  distortion and vignetting, then Lensfun for chromatic aberration, and for
  distortion and vignetting where the camera gives none. Lensfun's bundled
  database has the Z f and 33 Z-mount lenses.
- **A grid instead of ported formulas.** Lensfun's own scalar code computes
  a 65-column grid of per-channel source offsets and gains, as fractions
  of the frame, so one grid serves the proxy, 100% regions and export. The
  live pass interpolates it (about 0.04 px of error at full size for strong
  distortion). There are no models to port and keep in sync. Distortion
  and vignetting have amounts (0–200%). A fill zoom (binary search over the
  frame's edge points) keeps the corrected frame without blank corners and
  is folded into the geometry map.
- Lensfun's colour pass with `reverse` set simulates vignetting (unlike its
  geometry passes), so the correction is its reciprocal.
- Lens corrections are off by default for now. Auto will turn them on.

## Manual transform (2026-10-20)

The geometry map is now a 3×3 projective matrix: output → crop →
un-rotate → un-transform → orientation → source. The shader divides by its
third row; `Frame::source_at` does the same on the CPU. The transform is a
camera tilted (Vertical) or panned (Horizontal) by up to 30° about the
centre, with the focal length set to the frame's diagonal. The tilt is
re-centred, because a real tilt also moves the picture. Then come Aspect
(±0.5 stop of width against height), Scale (50–150%) and offsets (up to a
quarter of the frame). Constrain (on by default) zooms in after the
perspective just enough that the frame's corners come from inside the
original, so the user's Scale is kept. The transform turns and mirrors
with the frame. Auto-upright (line detection) is Phase 3.

## Defringe, hot pixels and raw levels (2026-10-20)

- **Defringe** runs in the live pass, after the lens grid (so after
  chromatic aberration correction, as the spec says) and the colour matrix.
  Its edge strength is the relative luminance gradient from four source
  samples. Purple (Oklab hue 285–345°) and green (105–165°) lose
  saturation in proportion to the amount and the edge, so flat areas of
  the same colour keep theirs. The hue windows are fixed for now: adjustable
  ranges can follow if photos need them. It has its own section so it
  works without a lens profile.
- **Hot and dead pixels** are fixed on the CFA data before demosaicing. A
  photosite more than twice its brightest same-colour neighbour (+0.02), or
  under 30% of its darkest lit one, takes their median. It's off by
  default, because an isolated star can trip it.
- **Raw black and white points** move the levels the decoder normalised:
  ±5% of the range for black, 0.5–1.2× for white. Like the other Raw
  settings, they re-prepare the photo, and the develop thread keeps only
  the newest request while a slider drags.

## Presets, copy and paste (2026-10-20)

- **What a preset is.** It carries whole *parts*: White balance, Exposure,
  Tone, Presence, Tone curve, Colour, Black & white, Calibration, Detail,
  Lens, Transform, Crop & rotate and Raw. A part is replaced whole, so a
  "Colour" preset sets vibrance back to 0 too, as Lightroom does with the
  panels a preset includes.
- **Amount** (0–200%) scales every number from its default. Anything else
  (switches, modes, curve points) takes the preset's value from 50% up,
  and the default below. The Amount slider shows under the list after
  applying a preset, and hides once anything else changes: re-applying
  from the earlier settings would undo that change.
- **Where they're saved.** User presets go in one file,
  `<data>/presets/user.json`, rather than a file each. Deleting one is then
  a rewrite of Shoebox's own file, never a file deletion (safety rule 1).
  Packs will have folders of their own beside it. Hovering a preset shows
  the photo with it applied; nothing changes until a click.
- **Copy and paste.** Shift-⌘C (in Develop) opens a checklist of parts.
  Transform, Crop & rotate and Raw start unticked, as the spec says
  ("rarely transfer between frames"). Shift-⌘V pastes onto the selection
  (or the current photo):
  - the photo open in Develop changes as an ordinary step;
  - the others go to the library worker in one `ApplyPreset` command:
    each edit gets a "Paste settings" step in its own history, and the
    edits are written as one batch;
  - reference photos are skipped, with a note saying how many.

  egui may deliver ⌘C and ⌘V as Copy and Paste events, so both are
  accepted. "Sync" (copy, then paste onto the selection) and auto-sync can
  follow on top of this.
- The two "+" buttons in Develop's left panel now have their own names for
  VoiceOver ("New snapshot", "New preset").

## Auto (2026-10-20)

- **Measured as the photo is prepared, applied as numbers.** The develop
  thread measures each raw photo as it prepares it, using the proxy and,
  for a NEF, its embedded camera JPEG decoded from bytes it has already
  read. `auto::settings` turns those measurements into ordinary slider
  values. A photo with no edit gets them as its first history step
  ("Auto"), and the Auto button runs it again. A preset chosen in Settings
  → Raw replaces Auto for new photos, and Auto can be turned off there.
  Finished images (JPEG, TIFF, PNG) get no Auto: they're already rendered.
- **The rules,** tuned by eye so far (the 30-NEF review in MANUAL-CHECKS
  will refine them):
  - **Exposure:** brings the 99.5th percentile to where the default curve
    shows 0.95 (clamped to ±2 stops), then halfway towards the exposure
    that matches the camera JPEG's median.
  - **Noise reduction by ISO:** luminance 10 per stop above ISO 400 (up to
    60); colour 20 + 6 per stop above ISO 100 (up to 50).
  - **Sharpening:** 30, masking 20.
  - **Dehaze:** only when the darkest channel's median is over a quarter
    of the highlights.
  - **Shadows +15:** when over 30% of the frame is four stops under mid
    grey, standing in for Smart Lighting until Phase 3.
  - **Lens corrections:** on when there's a profile.
- **Two fixes found on the real NEF.**
  - The noise estimate fitted its line with bins spaced linearly up to the
    brightest block, so a night scene's blocks all fell in one bin. Bright
    bins then set the read noise to σ ≈ 3% of white, and Auto's noise
    reduction turned the shadows into a grey veil with no colour. Bins are
    now in stops, and the fit is weighted by relative error: σ ≈ 0.03%
    there.
  - A slider holding a value finer than its decimals (Auto's 0.35 is
    0.35000000000000003) "changed" on display, adding a spurious history
    step. Changes below half a displayed step are now ignored unless the
    slider is in use. And a press that moves a slider is now part of the
    drag that follows, so they make one step, not two.

## Virtual copies (2026-10-20)

EDITING-SPEC shows virtual copies "as a stack in the library". That would
give each copy its own identity in the catalog, where everything (selection,
textures, marks, previews) is keyed by file path. So copies start at the
Develop level, on the storage that already supports them
(`<rel>.<copy>.json`, and `copy_id` in the catalog's edits table):

- Develop's left panel lists the photo's copies (Master first), from its edit
  files and any catalog rows not yet written (`edits::copies_of`). "+" makes
  a copy of the edit as it is now: same settings, its own name and an empty
  history (`EditDoc::copy_of`). A click switches copies, keeping the one
  left. Right-click renames a copy (opening it first) or deletes it.
- Deleting moves the copy's edit file to the Trash through `fileops`, as a
  journaled, undoable batch, and forgets its catalog row.
- Only the master gets Auto and drives the library's preview. The grid
  badge reads "EDITED +n" for n copies.
- Exporting the photo open in Develop uses the edit as it is now (any
  copy), with a copy's name added to the file name ("DSC_1234_Copy 1").
  An export from elsewhere uses the master.
- Still to come: stale-export badges per copy (export rows are recorded
  against the master's hash for now). Copies as tiles: below.

## Camera match profiles (2026-10-20)

- **The look.** It's a 17³ 3D table on the display transform's output
  (display-linear Rec.2020, 2.2-encoded), applied before the user's tone
  curve, on the CPU and the GPU (checked against each other). It learns
  both the camera's tone and its colour, as the spec asks ("a tone curve
  plus a 3D LUT"). One table carries both, because they're fitted from the
  same pairs.
- **Fitting** (`examples/camera_match.rs`, run on an APFS clone; see
  MANUAL-CHECKS):
  - each raw is rendered with the matrix at the exposure that matches its
    JPEG's median, so the table learns shape, not each photo's exposure;
  - both images are shrunk to about 320 px, and only the middle 70% is
    used, because the camera's distortion correction moves the JPEG's
    edges;
  - clipped JPEG pixels are skipped;
  - each pixel's offset is splatted into the eight cells around it. Cells
    with little data are pulled towards no change, and empty ones take
    their neighbours' offsets.
  - Every tenth pair is held back to report mean ΔE2000 with and without
    the look. On the one real pair in `tests/fixtures/real` it goes from
    2.32 to 1.63 (on the pair fitted, so only a smoke test).
- Profiles live in `<data>/profiles/*.json`, keyed by camera model, and
  load when Shoebox starts. Develop offers "Camera match" beside Auto when
  the camera has one, and Auto picks it then. It's part of a preset's
  Colour part.
- A profile per Picture Control (the spec's "fit one per Picture Control")
  is a matter of fitting from pairs shot with each and choosing between
  them. It can follow once the first one has been fitted.

## Lens corrections: the camera's distortion, confirmed (2026-10-21)

On a real NEF from a NIKKOR Z 24-70mm f/2.8 S at 51 mm, Auto's lens
correction made the edit look larger than the Original side of the split.
Setting Distortion to 0 put it back. The NEF's coefficients (0.01025,
−0.00956, 0.02218) ask for about 2.3% more image at the corners, and the
fill zoom then crops in by about that much. Lensfun's profile for the same
lens and focal length barely moves the frame (fill zoom 0.998).

Block-matching the uncorrected raw against the camera's own corrected JPEG
(the NEF's JpgFromRaw, `examples/lens_probe.rs`) settles the convention:

| Model | RMS |
|---|---|
| r·(1 + k1·r² + k2·r⁴ + k3·r⁶), r = 1 at the half-diagonal | 2.5 px |
| A uniform scale only | 7.1 px |
| The inverse convention | 12.2 px |

The camera's own scale (0.980) also matches Shoebox's fill zoom (0.978).
So the larger frame is the correction doing what the camera does: the
edit is framed like the Z f's JPEG, and the Original shows the raw
uncorrected. Chosen with the user: the camera's distortion stays first.
Lensfun covers chromatic aberration, and anything the camera doesn't give.

## History: one line per setting (2026-10-21)

Every release of a slider, and every click of a checkbox, used to add a
history line, so toggling lens corrections a few times filled the list.
Now a change to the same control as the last step folds into that step
(`EditDoc::amend`), whether or not the mouse was let go in between. A step
that folds back to where it began is removed, so switching something off
and on again leaves no line at all. The run ends when anything else
changes, on undo or redo, on a jump in the history list, and when another
photo or copy opens. Presets, Auto and Paste settings are always new steps,
because each is an action of its own; a preset's Amount slider folds into
the preset it scales. The tone curve counts as one setting, so dragging
several points is one line.

## Theme and text size (2026-10-21)

egui's defaults (12.5 pt text, 18 px rows, 3 px between rows) felt cramped
on a desktop. `theme.rs` sets text sizes and spacing in one place: body
and buttons 13.5 pt, small text 11.5 pt, 22 px controls, 6 px between rows,
roomier padding and margins. A Text size setting (Small, Medium, Large,
Larger) zooms all of it together, stored in the settings and changed with
⌘+ / ⌘− / ⌘0 (egui's own keyboard zoom is off, so the choice persists).
Labels painted on photos (badges, timeline axes) keep fixed sizes and
scale only with the zoom.

## Export: 16-bit PNG, borders and watermarks (2026-10-21)

PNG joins JPEG and TIFF as an export format: 16 bits a channel, with the
space's ICC profile in an iCCP chunk (the `png` crate, already used by
the tests).

Borders and watermarks are export preset options (`overlay.rs`), applied
after the resize in the preset's colour space. A border is a width (% of
the long edge) and an sRGB colour. The file keeps the preset's long edge
and the photo shrinks to make room, keeping its shape; the border is even
all round, so the short side comes out a little longer than a plain
resize. A watermark is one line of text or a PNG (its transparency kept),
with a size (% of the photo's long edge), opacity, one of nine positions
and a margin, placed on the photo inside the border. Colours are given in
sRGB and converted to the export's space.

Text is drawn from the font's outlines with `skrifa` and filled
anti-aliased with `tiny-skia` (MIT/Apache and BSD-3; both were already in
the build through egui, so no new code was fetched). The default font is
the app's own text font; a preset can name a .ttf or .otf file instead.
A watermark image or font that can't be read fails that preset's export
with a message, rather than quietly leaving the watermark out.

## Soft proofing (2026-10-21)

S in Develop (or the Soft proof toggle under Clipping) shows the edit as
an export in sRGB, Display P3, Adobe RGB or ProPhoto would hold it: the
display-linear Rec.2020 values go into that space, are clipped to it,
and come back before the screen's own output space (`cpu::Proof`,
mirrored in the shader and held to the CPU within 1e-4). Gamut paints
magenta where proofing changed a colour: a channel below zero in the
proof space, or above one where the screen wouldn't clip it anyway, so
blown highlights aren't flagged twice. Only the edit is proofed, not the
Original side of a comparison. The histogram counts the proofed render,
not the warning.

In the library S still folds and unfolds stacks; in Develop, where stacks
don't apply, it's soft proofing, as in Lightroom. Proofing against
printer ICC profiles (moxcms can read them) is left for later; the four
export spaces cover what Shoebox exports.

## Virtual copies as tiles (2026-10-21)

A virtual copy is a tile of its own, stacked with its file: an open stack
(S) lists its files, then their copies, each showing its own edit's
preview with a "NEF · Copy 1" badge; a folded stack says "EDITED +n" (or
"+n COPIES" when the master is unedited). The catalog stays one entry per
file, so a copy's tile has an id of its own (`jobs::copy_id`, from the
file's id and the copy's) that resolves to its file, the way the loupe's
original already had one (`original_id`):

- Its textures are its preview; selection, current photo, Compare, Survey
  and the loupe work on it like any tile.
- Marks, moves, renames, rejects and the Trash act on the file (marks live
  in the file's sidecar, so a copy shares its file's rating, flag and
  label); a file and its copy both selected count once.
- D on a copy's tile opens that copy in Develop, and switching copies in
  Develop moves the current tile to match (opening the stack). Export and
  Paste settings use the copy's edit; its menu has Delete Copy (its edit
  file to the Trash, undoable), never touching the file.
- Photos Shoebox moves (organise, undo, redo) take their edit and copies
  along: the worker reports them at the new path (before, only edits
  reattached after a move outside Shoebox were reported again).

## Exports: outside the library, or beside the original (2026-10-21)

Chosen with the user. A preset's Destination is either a folder outside
the library or beside the original:

- **Outside:** files for elsewhere, not kept track of (no rows, no stale
  badge, no Info panel listing). Exporting onto a file asks first (Replace
  or a new name), as before. A folder inside the library is refused: that
  is what "beside the original" is for. Rows recorded for outside exports
  before this stay listed.
- **Beside the original:** the export lands in the photo's folder, joins
  its stack as a variant (a link in `.shoebox/stacks.json`, the mechanism
  duplicate review uses), and is kept track of per edit: a virtual copy's
  exports are recorded with its copy id and go stale when that copy's edit
  changes, not the master's.
- **Re-export in the library replaces in place.** The new file is written
  and hashed in the cache, then one batch (`FileOps::add_exports`) sends the
  old file to the Trash, only if it still has the content recorded, and
  copies the new one in under its name, verified. ⌘Z puts the old one back;
  undo's preflight now lets a file move back to a place a step before it
  freed. CLAUDE.md's rule 4 says so.

A new virtual copy's history now starts with one step, "Copied from
Master", from the defaults to the settings copied. History replays from
the defaults, so without it a reopened copy dropped its history.

## Phase 3, first chunk: local adjustments (2026-10-22)

Phase 3 starts with the mask machinery, since AI masks, Point Color and
healing build on it. Order from here: local adjustments; Point Color;
spot heal and clone; red-eye; auto-upright; reference view and match
total exposures; Apple Vision subject and person masks; SAM 2.1 click
masks; AI raw denoise (RawNIND); Smart-light tone; finer AI masks.

- **The recipe, not pixels** (EDITING-SPEC): `Settings.locals` is a list
  of layers (id, name, on, amount, mask, sliders). A mask is parts in
  order, each with its own op (add takes the maximum, subtract multiplies
  by 1 − v, intersect multiplies) and invert: PhotoLab 9's model. A part
  this build doesn't know is kept as JSON and leaves the mask alone.
- **Coordinates:** the stored image (before EXIF orientation, turns and
  crop), in units of its long edge, so masks stay on what they cover when
  the photo is turned or cropped and mean the same on the proxy and the
  full image. Masks are read at the geometry map's source position before
  lens corrections (where they were drawn); the global maps at the
  lens-corrected one, as before.
- **Rasterised on the CPU** to one 8-bit plane per layer on a grid of at
  most 2560 px (the proxy's own size), cached on the source by a hash of
  the masks; the GPU gets them packed four to a word and samples them
  bilinearly, the CPU reference the same bytes, so the pipes agree within
  1e-4. A proxy-sized mask with a gradient, an ellipse and a 400-point
  stroke rasterises in 18 ms (release), so dragging a handle stays live.
  Exports read the same 2560 px grid; the guided-filter upsampling the
  spec asks for comes with the AI masks.
- **Brush:** within one stroke the strongest dab counts (a stroke doesn't
  build up over itself); strokes build on each other by flow; ⌥ erases.
  Each stroke is one history step. The edge-aware auto-mask waits for the
  AI masks chunk.
- **Ranges** read the picture as the frame shows it: brightness is the
  display transform of the pixel's luminance (0–1), hue is Oklab's, and
  greys (relative chroma under 0.02–0.08) are never in a colour range.
  They're computed on the same grid from the source.
- **Where in the pipe:** after the global dehaze, highlights, shadows and
  clarity, before the colour stage, in linear Rec.2020 (the spec's diagram
  places locals after the tone curve, but the tone curve here runs on
  display values; local exposure should behave like global exposure).
- **Sliders:** exposure, contrast (about mid grey, 0.35 stop per stop at
  100), highlights, shadows, clarity and dehaze (through the global maps,
  computed whenever any layer needs them), temperature and tint
  (Bradford from a light ±60 mired from daylight, tint at half the global
  scale) and saturation (towards luminance). Sharpening and AI-denoise
  strength per layer come with AI denoise (they act on the source, before
  geometry).
- **Presets and copy/paste:** "Local adjustments" is a part, unticked by
  default when copying (EDITING-SPEC, Batch editing); carried whole, not
  scaled by a preset's Amount.
- **UI:** a Local adjustments section with New mask (linear, radial,
  brush, luminance range, colour range), the masks (on/off, select,
  delete), the selected mask's parts (op, invert, add/subtract/intersect
  another), its own settings and the sliders. On the photo: handles for
  gradients and ellipses (the long axis's handle sizes and turns it),
  the brush with its circle, a click to pick a range. O shows the mask in
  red, "Mask only" alone; Escape puts the mask down before leaving
  Develop. New gradients start in place rather than being drawn; drag
  their handles.

## Point Color (2026-10-22)

- `modules.point_colour.points`: up to eight colours picked on the photo,
  each stored as Oklab lightness, chroma and hue with its hue shift (up to
  30° either way), saturation and luminance (±1 stop at ±100), and hue,
  saturation and luminance ranges (0–100).
- **In the colour stage**, after HSL and before vibrance. Each point is
  weighed on the colour as it came in (before HSL), full within its range
  and fading to nothing at twice as far, in hue, chroma and lightness
  multiplied together; hue counts less where either colour is nearly
  grey. The shifts of all points add up, so their order doesn't matter.
- **Picking** reads what the colour stage sees there (after local
  adjustments), averaged over 3×3 output pixels.
- **Show affected** greys whatever the selected point doesn't reach, in
  both pipes, so the GPU check covers it. Point Color travels with the
  Colour part of presets and copied settings.

## Spot removal: heal and clone (2026-10-22)

- `modules.heal.spots`: each spot a circle (`dest`, radius, feather,
  opacity) filled from the same circle at `source`, heal or clone, in
  the stored image's long-edge units like masks. Spots apply in order.
- **On the source, after the detail stage**, so after noise reduction and
  in linear light, as the spec asks. The spec also says after lens
  correction; here lens corrections are part of sampling in the live
  pipe, so spots sit before them, in the coordinates they're drawn in.
  The result is cached on the source (keyed by the detail stage's result
  and the spots) and is what the GPU uploads (`Demosaiced::prepared`),
  so the shader needs nothing new.
- **Heal** is the Poisson blend: the copied texture plus the harmonic
  interpolation of the edge's difference, solved on the CPU by successive
  over-relaxation (ω from the circle's size, stopping when the largest
  change is 1e-5 of the values). A blot healed from a part of a gradient
  0.125 brighter matches the gradient within 0.01. The spec suggested
  Jacobi on the GPU; a spot is small, so the CPU solves it in
  milliseconds and the pipes can't differ.
- **The source** is chosen when the spot is made: of 72 circles 2.5, 3.5
  and 5 radii away that fit in the photo, the one whose ring (1.05–1.6
  radii, compared as square roots of the values) is most like the spot's,
  slightly favouring nearer ones; found on the photo as healed so far.
  Dragging the source changes it.
- **Tool:** Q (or Spot tool) turns it on; a click adds a spot (Heal or
  Clone, with the panel's size, feather and opacity), the panel's
  sliders change the selected spot, the spot and its source drag,
  Backspace removes the selected spot, Escape puts the tool down. Spot
  removal is its own part in presets and copied settings, unticked by
  default.

## Red-eye (2026-10-22)

- Two more spot kinds, so the spot tool does both (Q, then Red eye or Pet
  eye): a click puts a circle on the eye; there's no source.
- **Red eye** finds the pupil inside the circle by redness, red above the
  larger of green and blue relative to brightness (full at 0.4, none
  below 0.2, so skin, about 0.15, is left alone), softened by a 3×3 blur;
  red takes green and blue's mean and the pupil darkens (Darken, 0–100).
- **Pet eye** turns the whole circle (feathered) into a dark grey of its
  own brightness: animals' reflections are green, yellow or white, not
  red, so redness can't find them.
- Like spots, on the source after the detail stage, in camera RGB
  balanced at as shot for raws (near enough neutral for redness).
- **Bounds on edit files** (from an automated security review): an edit
  is JSON anyone could write, so spots are read with radius at most a
  quarter of the long edge, feather, opacity and darken in range, and
  positions finite (else skipped); at most 2000 spots and 4000
  relaxation sweeps; brush radii at most a long edge.
- `browse_views_render` now allows 120 s for its library (the biggest
  fixture, NEFs included) to settle: with three more GPU-heavy Develop
  tests beside it, 60 s ran out twice in a full run; alone it settles in
  under 30.

## Auto-upright (2026-10-22)

- **Lines:** in a copy of the photo at 1024 px, upright (orientation,
  quarter turns, mirror) and lens-corrected, before rotation and
  perspective. Log luminance, a 3×3 blur, Sobel; the top fifth of
  gradients (at least 0.6 stops) grown into regions of one direction
  (within 22.5°), each fitted with its principal axis and kept if at
  least 4% of the long edge and thin (LSD's idea, much simplified; no
  false-alarm test).
- **Solving:** lines within 30° of vertical or level count; the cost is
  each line's lean once corrected, squared, capped at 6° (so a stray line
  can't pull the result), weighted by length. A grid (rotation ±10° in
  1° steps; perspective ±100 in 10s), then a pattern search halving the
  steps eight times. The correction is the rotation (with the largest
  crop that fits, as the Angle slider does) and the Transform's vertical
  and horizontal perspective, with Constrain on, as one history step.
- **Modes:** Level turns only; Vertical adds vertical perspective (levels
  count a quarter: a facade's horizontals converge); Full adds horizontal
  perspective; Auto is Full limited to ±60 and charged a little for
  strong corrections.
- Tested on lines distorted by a known correction (recovered within 0.5°
  and 4 slider units) and on a drawn grid turned 3°, found, levelled
  (3.0°), and level again after the real render (mean lean under 0.5°).
- Guided upright (drawing the lines) followed the same day: see
  "Guided upright (2026-10-22)".
- **The crop's frame** (2026-10-22): Auto-upright and the Angle slider
  sized the largest crop from the file's orientation alone, leaving out
  the edit's quarter turns (Guided already had them). `max_crop` is
  symmetric in width and height (its two limits swap places), so the
  crops came out the same and nothing showed; all three, and
  `geometry_map`, now take the frame from `cpu::frame_size` (file
  orientation, then quarter turns and mirror), so a crop that one day
  depends on the aspect stays right.

## Reference view and Match Total Exposures (2026-10-22)

- **Reference view** (Lightroom's): a fourth choice beside Edited,
  Original and Split, and Shift-R. The reference photo shows in the left
  half as the library shows it (its edited preview if it has one), the
  edit in the right. The first time, with no reference yet, the current
  photo becomes it; Use as Reference in any photo's menu picks another.
  The reference stays while moving between photos.
- **Match Total Exposures** (the photo menu, with several selected):
  Lightroom's way, from metadata, not from rendering. Each photo's
  captured light is log2(exposure time × ISO ÷ f-number²) from its EXIF;
  its Exposure becomes the current photo's Exposure plus the difference
  in captured light, so a bracketed or changing series comes out equally
  bright. One step in each photo's history, written as one batch
  (`ApplyPreset` now takes a preset per photo; Paste uses the same).
  Photos without the EXIF, reference photos and movies are left alone and
  counted in the notice. For a virtual copy's tile, the current photo's
  Exposure is its master's.

## Subject and person masks (Apple Vision) (2026-10-22)

- Two mask parts, Subject (`VNGenerateForegroundInstanceMaskRequest`, all
  instances) and People (`VNGeneratePersonSegmentationRequest`, accurate,
  8-bit), through `objc2-vision` 0.3.2 with `objc2-core-video` and
  `objc2-core-graphics` (Zlib/Apache/MIT, the objc2 project's bindings the
  app already uses). The FFI is `shoebox-ml::vision`.
- `shoebox-develop` doesn't depend on `shoebox-ml` for it: the app
  registers a finder (`mask::register_finder`), as camera match profiles
  are registered. Without one, the parts cover nothing.
- **Found once per source**, on the masks' grid (at most 2560 px), on the
  picture as the frame shows it, turned upright (the file's orientation,
  then the edit's turns and mirror) so Vision sees it the right way up;
  the mask is turned back to the stored image. Cached on the source by
  what, orientation and size; colour edits don't search again.
- **The live view never waits:** its source is marked for the background,
  so a search runs on its own thread and the view redraws when it lands
  (the render keys on a generation counter; repaints are asked for while
  a search runs). Exports, 100% regions and previews search on their own
  threads as they render.
- The spec's 8-bit PNG cache on disk and the guided-filter upsampling
  for full-size exports came later: see "AI masks on disk, and finer for
  export (2026-10-22)".

## Linux deferred (2026-10-22)

The user's call: Shoebox is built for macOS only for now. New work needs
no Linux branch, there's no local Docker check before commits, and the
CI Linux job was left running but wasn't a blocker (later disabled;
below). Existing Linux branches stay. To come back to with Linux:

- **Portable subject and person masks.** Vision is Apple's. Open models
  that could run on candle like SAM: BiRefNet (MIT) or U²-Net / IS-Net
  (Apache-2.0) for subjects, MODNet (Apache-2.0) or a SegFormer /
  Mask2Former person class for people. SAM 2.1 (next) is portable and can
  make a subject mask from a click meanwhile.
- The local Docker check (in CLAUDE.md) and CI's Linux job as a gate.
- 2026-10-22: CI's Linux job is disabled (it kept failing): `if: false`
  on the `linux` job in `.github/workflows/ci.yml`. Delete that line to
  re-enable it.

## Object masks: SAM 2.1 on candle (2026-10-22)

- **The port:** `shoebox-ml::sam`, from Meta's reference code
  (facebookresearch/sam2 at 2b90b9f5; Apache 2.0): Hiera-T (stages 1, 2,
  7, 2; windows 8, 4, 14, 7; global attention at blocks 5, 7, 9; queries
  pooled where stages begin), the FPN neck (top-down into the two
  coarsest levels, the coarsest dropped), the decoder's high-resolution
  projections, `no_mem_embed` added as the image predictor does, the
  prompt encoder (random-frequency position encoding) and the two-way
  mask decoder with its high-resolution upscaling. The windowed position
  embedding is interpolated bicubically (PyTorch's A = −0.75) once at
  load. f32, on Metal. No memory attention or tracking: stills only.
- **Weights:** Meta's own `sam2.1_hiera_tiny.pt` (156 MB, Apache 2.0)
  from facebook/sam2.1-hiera-tiny at revision de431c40, SHA-256 pinned,
  read directly by candle's PyTorch reader (the parameter names match
  Meta's code). Downloaded when someone first adds an Object part (the
  panel offers it, with progress), into the models folder.
- **Checked** without PyTorch to compare against: on a drawn picture, a
  click on a ball gives the ball (IoU 0.998) and on a square the square
  (0.996), each untouched by the other; a negative click takes away.
  Encoding takes about a second, a click a fraction of one (first runs
  include Metal's shader compilation).
- **In masks:** an Object part holds its clicks (stored-image units; ⌥
  takes away). The app registers SAM as the masks' segmenter (as Vision
  is their finder); the picture is the masks' grid as shown, upright, and
  the clicks are turned with it. One click picks the best of SAM's three
  masks by predicted quality; several use the single mask unless it's
  unstable (Meta's dynamic multimask). The last picture's encoding is
  kept, so each further click only runs the decoder. Two objects need two
  parts (one prompt finds one object).
- The spec's on-disk mask cache and guided-filter upsampling to full
  resolution: see "AI masks on disk, and finer for export (2026-10-22)".

## AI raw denoise: RawNIND on candle (2026-10-22)

- **The model:** RawNIND's UtNet2 Bayer denoiser (Brummer and De
  Vleeschouwer, 2025; code GPL-3.0, weights GPL-3.0 and CC BY 4.0), the
  canonical 2024-02-21 checkpoint darktable-ai packages, ported to candle
  from the authors' `raw_denoiser.py` (rawnind_jddc at e80fb7b1): a
  four-level U-Net, LeakyReLU 0.2, a 1×1 head and pixel shuffle. Packed
  RGGB at half resolution in, camera RGB at full resolution out, at an
  arbitrary gain; f32 (it overflows f16).
- **Where the weights come from** (changing the 2026-10-19 plan): candle
  reads the authors' PyTorch checkpoint directly, so there's nothing to
  convert, and Google Drive's direct download endpoint
  (`drive.usercontent.google.com/download?id=…&confirm=t`) serves it
  fine through the app's downloader, pinned by SHA-256 (31 MB). That
  replaces publishing a converted copy on a `models-rawnind-1` release.
  If Drive stops serving it, mirroring it on a release (with the user's
  go-ahead, as it's public) is the fallback.
- **Feeding it** (darktable-ai's notes, checked on the Z f's files): the
  mosaic with levels and hot pixels as RCD gets it, not white-balanced,
  packed from wherever red is (any 2×2 Bayer becomes RGGB; rows and
  columns skipped copy their neighbours), tiles of 512 with 32 of
  context each side, mirrored at the edges. The output is camera RGB:
  balanced at as shot it matches RCD's colour channel for channel. Its
  gain is matched to RCD's demosaic of the same mosaic, away from
  clipping (matching overall left it 5% bright, as highlights pulled the
  match). From 75% of clipping up, RCD's highlight reconstruction takes
  over (the model's channels clip unevenly).
- **Measured:** a 24 MP Z f frame takes 12–18 s on an M2 Max (the spec
  allows 30); colour within 1% of RCD; on a synthetic noisy mosaic,
  noise falls about fourteen-fold and tile seams don't show. Bright thin
  lights in a night shot pick up coloured edges: a model limitation, for
  the manual checks.
- **In the app:** Detail → AI denoise (raw photos) with Strength (a blend
  with RCD's result, live). Results are kept on disk by the file's
  content, raw settings and model (`<cache>/ai-denoise`, half floats, 3
  GB, oldest removed). The view gets it on a thread of its own and
  redraws when it lands (RCD's until then, with progress shown);
  regions at 100%, previews and exports make or load it inline. The
  model downloads from the panel when first wanted.
- Per-layer AI denoise strength (local adjustments; the spec's "AI
  denoise per mask") came later: "Local AI denoise strength and
  sharpening (2026-10-22)".
- UI tests now run at most half the cores' worth of apps at once: as the
  suite grew, whichever was slowest missed its settle timeout.

## Smart Lighting (2026-10-22)

- **Module 6, local tone:** a local Laplacian filter on log luminance
  (Paris, Hasinoff and Kautz; Aubry et al.'s fast form: twelve reference
  levels, pyramids down to a pixel or two so whole regions are
  compressed, not only edges). Differences up to a stop are detail and
  kept; larger ones keep 35% at full strength.
- **How it runs:** on the maps' 512 px copy, once per source and colour
  matrix, at full strength; the gain it makes (stops) is fitted to log
  luminance by a guided filter and stored as two more map slots (now
  eight), so the live pipe evaluates it per pixel at any size like the
  other maps. Strength scales the gain (dragging it doesn't redo the
  analysis); Bias weights lifts by 1 + b and cuts by 1 − b. About 15 ms
  on a 1200 px proxy of the Z f's frame; no halos round the roses at
  full strength.
- **Auto** now sets it as the spec asks (slight, uniform): 25, or 40 when
  over 30% of the frame is four stops under mid grey. That replaces the
  Shadows +15 it stood in with.
- The spot-weighted mode came later ("Smart Lighting, spot weighted").

## Colour grading (2026-10-22)

- **Module 10:** wheels for shadows, midtones, highlights and the whole
  photo (hue in Oklab's degrees, saturation up to an Oklab chroma of 0.12,
  luminance ±50% of lightness), Blending and Balance, as Lightroom's
  Color Grading has them.
- **In Oklab, after black and white**, so it can split-tone a monochrome
  photo. Ranges from lightness: x = L^pivot, with the pivot putting
  Balance's split (lightness 0.3–0.7) at 0.5; shadows (1 − x)^k,
  highlights x^k, midtones what's left; k from 3 (blending 0) to 1
  (blending 100). Tints are offsets in a and b scaled by lightness, so
  black stays black.
- **Panel:** one large wheel at a time (Shadows, Midtones, Highlights,
  Global; a range with something set shows in bold), drawn in Oklab's
  hues as the grading uses them; drag for hue and saturation,
  double-click to clear; sliders for exact values.

## Effects: vignette and grain (2026-10-22)

- **Module 15, on display values** after the display transform, before
  the clipping and gamut overlays (so they warn about the final picture).
- **Vignette:** post-crop, placed by the whole frame (a 100% region
  carries its offset, so it gets its own part). Distance from the middle,
  1 at the corners; Roundness from the frame's shape (0) to a circle
  (+100); Midpoint where it's half; Feather its width. Darkening
  multiplies (Highlights keeps bright edges bright); lightening goes
  towards white.
- **Grain:** monochrome value noise from an integer hash (the same u32
  arithmetic in Rust and WGSL), a second finer octave by Roughness,
  strongest in the midtones (none on black or white). Sized in the
  full-size photo's pixels (1–6); where a proxy's pixel spans several
  grains it's weakened by as much, so the proxy looks like the export.
- Finished images (JPEG, TIFF, PNG) prepared at the proxy size now carry
  their scale, as raws did: grain and the detail stage's radii were in
  the proxy's pixels for them.
- "Effects" is its own part in presets and copied settings.

## Finer AI masks: background and faces' parts (2026-10-22)

- **Background:** the Subject part inverted (no search of its own).
- **Faces' parts** from Vision's face landmarks (`VNDetectFaceLandmarksRequest`,
  every face): Face skin (an oval over the face's box up to the forehead,
  less the eyes, eyebrows and lips), Eyes, Eyebrows (strokes along their
  lines), Lips (outer less inner) and Teeth (inside the lips), filled and
  softened in proportion to the face's size. Found once per source, like
  Subject and People. Mask menus list them under Face.
- **Waiting:** hair, clothes and body skin (Lightroom's people masks) and
  landscape categories need segmentation models of their own; Sky is
  Phase 4 in the spec (SegFormer), the rest with it.

## Local sharpness (2026-10-22)

- Local adjustments gain Sharpness (−100 to +100): each pixel's fine
  detail (itself less the mean of its four neighbours one source pixel
  away, through the colour matrix, before the local tone maps) added at
  up to 1.5× or taken away at up to half (averaging it with its
  neighbours). Worked out per pixel in both pipes only when a layer uses
  it; no extra images kept.
- Its reach is a source pixel, so on the proxy it's coarser than at
  100%. Replaced the same day by sharpening on the detail stage itself
  (and softening sized in the original's pixels), with per-layer AI
  denoise strength: "Local AI denoise strength and sharpening
  (2026-10-22)".

## Auto: AI denoise from an ISO (2026-10-22)

- **The setting:** Settings → Raw, "AI denoise in Auto from ISO", off by
  default; ticked, it starts at ISO 3200 (100 to 102400). Stored as
  `ai_denoise_from_iso` (absent: off).
- **The rule:** when the photo's ISO is at or above it, Auto turns AI
  denoise on at strength 100 and caps the classic luminance noise
  reduction at 10 (the model does the work; the classic tier only tidies
  up). Colour noise reduction stays by ISO. Below the ISO, Auto leaves AI
  denoise as it was, so running Auto again doesn't undo a choice made by
  hand.
- **Raw photos only** (scene-referred sources; the app passes no
  threshold for others). A photo without an ISO in its EXIF never
  qualifies.
- **Without the model** the step is still made: the photo shows the
  classic result (luminance at most 10) until the model is downloaded and
  the AI's result arrives. The setting offers the download when ticked,
  as Detail does.
- The test DNG fixture can now carry an ISO (in an Exif IFD), for the UI
  test.

## Guided upright (2026-10-22)

- **What it does:** the person draws two to four lines along things that
  should be vertical or level; each is made exactly vertical or level,
  whichever it's nearer (within 45°). One guide does nothing yet (the
  spec's two to four, as Lightroom waits for the second).
- **Solving:** `upright::solve_guided`, Levenberg–Marquardt on the
  guides' leans (degrees), over the rotation and the camera's tilt and pan
  (in degrees, ±30° for the sliders' ±100), with a numerical Jacobian.
  Verticals alone turn and tilt; levels alone turn and pan; both together
  may use all three. A small cost on the correction itself (10⁻³ per
  degree) picks the smallest correction when the guides leave a choice
  (one vertical and one level); four guides are a least-squares fit.
  Results are rounded to 0.01° and 0.1 slider units, not Auto's 0.1° and
  whole units, so the guides stay within 0.3° of straight. The person's
  Aspect is taken as 0 (as Auto does): with Aspect and a rotation, guides
  lean slightly.
- **Applied** as Auto-upright is: the rotation with the largest crop that
  fits, the Transform's vertical and horizontal perspective, Constrain on.
  Fewer than two guides after a change: the correction goes back to none
  if the guides had given one, else the photo is left as it was (drawing
  the first guide doesn't undo an earlier rotation).
- **Stored** in the edit as `geometry.guides` (ends as fractions of the
  upright frame before rotation and perspective), so turning Guided on
  again shows them to move. They're left out of the render hash (only
  what they did is rendered), and out of presets and copied settings (no
  part names them). Auto, Level, Vertical, Full and the Transform's Reset
  clear them.
- **Drawing:** while Guided is on, the photo shows without rotation,
  crop or Transform (as cropping shows the whole frame), so the screen
  maps linearly onto the frame the guides live in, the lens corrections
  included as Auto's lines are. The corrected photo shows on leaving
  (Done, Enter or Escape); the sliders and history change as each guide
  does. A drag draws a guide; an end drags; a click selects one and
  Backspace (or Remove guide) removes it; Clear removes all. Each guide
  drawn, moved or removed is one history step (a drag folds into it); a
  guide under 8 points long when let go is dropped and its step with it.
- Tested on guides distorted by known corrections (two verticals, two
  levels, one of each, three, four: all within 0.3° once corrected; four
  recover the camera within 0.3° and 2 units), and in the app on a grid
  turned 3°: two guides level it (3.0°), a moved end is one more step,
  removing one takes the correction away.

## Smart Lighting, spot weighted (2026-10-22)

- **`SmartLighting.spot`:** a box `[x0, y0, x1, y1]` in the stored image,
  in units of its long edge, as other positions are (None: uniform).
  Presets and copied settings carry Tone's Strength and Bias but not the
  box: a box on one photo's face means nothing on another's, so the photo
  pasted onto keeps its own (or stays uniform).
- **How it weights:** the uniform filter runs as before; then the spot's
  mean tone (after it, stops from mid grey) is brought into −0.5 to +1
  stop (a light face sits about a stop over mid grey), by at most 2
  stops. The shift is whole for every tone up to a stop past the spot's
  (on its side) and fades out by three stops, so a dark face's lift
  reaches the shadows but not the bright background, and a bright spot's
  cut holds the shadows. It's a tone weighting, not a spatial one: no
  gradient across a plain sky, and no halo round the box. Strength and
  Bias scale it with the rest of the gain.
- **Still a map change:** the shifted gain is fitted by the same guided
  filter into map slots 6–7, so the shader is unchanged (a GPU case
  checks it). The spot is in the maps' key; the uniform gain is cached on
  the source by colour matrix and dehaze, so dragging the box redoes only
  the shift and the fit, not the pyramid.
- **Face:** `mask::register_face_finder` (the app registers Vision's face
  rectangles, from the landmarks request the face masks use);
  `cpu::layers::faces` shows it the picture as the frame shows it,
  upright on the masks' grid, and turns the boxes back to the stored
  image, largest first. The app runs it on a thread of its own when Face
  is clicked and sets the box to the largest; without a face (or on
  another system) it says so.
- **Panel:** under the Smart Lighting sliders, Uniform / Spot weighted;
  Spot weighted starts with a box in the middle (30% of the long edge)
  and shows it on the photo (Edit box): corners resize, inside moves,
  a drag outside draws a new one (fitted only: zoomed, a drag pans).

## AI masks on disk, and finer for export (2026-10-22)

The two leftovers of the AI mask parts (Subject, People, Background,
Face parts, Object), from EDITING-SPEC's "Rendering masks".

- **On disk** (`mask::store`): what Vision or SAM returned, as an 8-bit
  grayscale PNG in `<cache>/ai-masks/<key>.png`, written beside and
  renamed into place; 512 MB, least recently used removed first (a read
  touches the file), and only our own `*.png` (and our temporary files
  a day old) are ever removed there. A damaged file, or one of the wrong
  size, is ignored and found again. `mask::find` and `mask::segment` go
  through it, so the in-memory cache (per source) is unchanged and the
  live view's background search reads the disk on its thread; opening a
  photo again or restarting reads the PNG instead of searching.
- **The key** is a BLAKE3 over: the exact picture the finder is given
  (its bytes and size), the kind (subject, person, face part; object
  with its clicks in that picture's pixels), the finder's identity and a
  format version. The spec says "keyed by file hash and prompt points";
  the picture's own hash is stricter and needs nothing from the app: it
  changes with the file's content, its raw settings, the source's size
  (proxy or full), the colour and display it's shown with and its
  orientation, which are what the finder's result depends on. (The
  in-memory key is what, orientation and grid size per source, and
  deliberately ignores colour edits; on disk a colour edit then reopening
  searches once more, never shows a mask found on another picture.) An
  object's own key in the edit (`object_key`, `DefaultHasher`) isn't
  used: it isn't stable across Rust versions; the clicks themselves are.
- **Identities:** `register_finder` and `register_segmenter` now take one.
  Vision's is `vision/<REVISION>/<macOS build>` (`shoebox_ml::vision::
  identity`, from SystemVersion.plist: Vision's models come with the OS;
  `REVISION` is bumped when our requests or `draw_parts` change); SAM's
  is `sam::MODEL_ID`. An empty identity (no readable build) keeps
  nothing.
- **Finer for export** (`mask::fine`): when the source is larger than
  the 2560 px grid (exports and 100% regions read the full-size source)
  and a layer has an AI part, every layer is rasterised on the source's
  own pixels instead, within 128 MB for all layers (a 45 MP export: full
  size with two masks, a little smaller with more; 128 MB is also wgpu's
  default storage binding, so the GPU could take it). AI parts are found
  on the grid as before and upsampled by He and Sun's fast guided filter:
  coefficients on the grid (radius 4 grid pixels, ε 10⁻³), read
  bilinearly and applied to the full-size guide per pixel. Both pipes
  read the result like any masks (the shader is unchanged; a GPU case on
  a source wider than the grid agrees within 1e-4). Previews and the live
  view (proxies at most 2560 px) are as before.
- **The guide is colour, not luminance** (differing from the spec's "on
  full-resolution luminance"): each channel of the source, relative to
  its mean luminance, compressed `v/(1+v)` and gamma encoded, through He's
  colour guided filter (a 3×3 solve per grid pixel). On a luminance guide
  an edge between two colours equally bright is invisible, and there the
  filter blurs the mask (wider than bilinear); with colour it's as sharp
  as any. It reads the source before the edit, so colour edits don't
  redo it.
- **Geometric parts** (gradients, ellipses, brushes) are rasterised at
  the fine size: they're formulas, as cheap per pixel there, and a hard
  brush edge stays sharp. **Ranges** stay on the grid, read bilinearly:
  they need tones per pixel (three floats, 540 MB at 45 MP) and are soft
  by construction. Masks without AI parts keep the 2560 px grid, so
  nothing else changes.
- **Measured:** a disc's edge in a 256 px picture with its mask on a 64
  px grid: mean error within 8 px of the edge 0.002 against bilinear's
  0.084 (blocky mask), 0.115 against 0.287 (also blurred over 5 grid
  pixels), and no pixel on the wrong side of a half against 355 and 174;
  the same between two colours equally bright. A 45 MP frame with three
  masks (two AI parts) takes 2.3 s in all (release, M2 Max) and holds
  127 MB of masks; working the coefficients out briefly takes about 0.3
  GB more.
- Done in bands of 32 rows, so beyond the result only a few rows of
  floats are held per thread.

## Local AI denoise strength and sharpening (2026-10-22)

- **AI denoise strength per layer** (`LocalAdjust.denoise_ai`, −100 to
  +100; its slider shows for raw photos only): added to the global
  Strength inside the mask, the sum kept to 0–100. Layers apply in order,
  each blending towards its target by its mask, so −100 cancels a global
  100 where its mask is full. The module off counts as 0, so a layer
  alone brings AI denoise in where it's drawn: the app runs the model when
  the module is on or a layer adds strength (`cpu::layers::wants_ai`).
  Without the AI's result (the model still running, or not downloaded) a
  layer's strength does nothing, as the global Strength doesn't; the
  panel says the model's missing.
- **Images beside the source** (`cpu::Aux`, made by `Demosaiced::aux`):
  the source the pipe reads stays what it was (the global Strength's
  blend through the detail stage and spots). Where a layer raises the
  strength a pixel mixes towards the source at full strength, where one
  lowers it towards the source at none (RCD's), each made the same way
  (detail stage, then heal), by (k − g)/(1 − g) or (g − k)/g, g being the
  global strength and k the pixel's. That's exact where a mask is fully
  on or off; in a feather it's a blend of the two finished images rather
  than the detail stage of the blended input (the stage isn't linear),
  which only differs with noise reduction on and the global strength
  between 0 and 100.
- **Only what's needed:** the full-strength image when a layer raises it
  (and the global isn't already 100), RCD's when one lowers it (and the
  global isn't 0), none when no layer touches it, so other photos keep
  one source in memory and spots are healed once. They're cached on the
  source, keyed by what they're made from (the detail stage, the spots,
  the source the pipe reads and the AI's result), so dragging a mask or
  a slider doesn't remake them; they go when no layer needs them.
- **GPU:** one more binding, a `texture_2d_array` (binding 10) with the
  images as its layers, or a 1×1 placeholder. A texture rather than a
  buffer: the pipe binds seven of the eight storage buffers wgpu's
  default limits allow a stage (the app's device uses those limits), and
  sampled textures (16 allowed) leave room. The shader's `texel` mixes
  them in on every read, as the CPU's `Source` does, so lens corrections
  and defringe's neighbours see the same mixed source. Each layer is an
  RGBA f32 texture the source's size (70 MB at the 2560 px proxy).
- **Everywhere:** the live view uploads them with the source
  (`Pipe::upload_with`); the CPU fallback, 100% regions, previews and
  exports use `cpu::apply_from`, which makes them at their size.
- **Sharpening per layer, on the detail stage** (`LocalAdjust.sharpness`
  keeps its field and range; +100 is now the global Sharpening module's
  amount 100, with its radius, detail and masking): inside the mask, the
  amount so far plus the layer's (up to the module's 150), on top of the
  global amount. It replaces the one-source-pixel neighbour difference,
  so a layer's sharpening looks like the global module's, sized in the
  original's pixels: the same on the proxy, at 100% and in exports.
- **How, without running the stage per pixel:** the stage's sharpening
  isn't linear in its amount (the halo limit, `push / (1 + |push| /
  limit)`, saturates), so one delta image scaled by the amount was up to
  30–50% out (rms of the change) at small amounts on a test of texture,
  edges and noise. Instead the stage runs twice more beside the source,
  with ¼ and 1 more amount (then spots, as the source has), and each
  pixel's change is fitted as `alpha·e / (1 + beta·e)` for an extra
  amount `e` (alpha per channel, beta shared, as the limit acts on luma):
  one RGBA image (`detail::Sharpen`, a layer of the GPU's array). Against
  the global module at the same amount it's within 0.7–1.5% (rms of the
  change; at most 0.0012 in linear values) at full size and at a proxy's
  scale, alone or on top of a global 40, and exact at +100
  (`tests/layers.rs`). Both runs take about 110 ms on a 2560 px proxy,
  once per change to the detail stage, spots or AI (not per drag of a
  layer's slider or mask).
- **Softening** (negative values) the same way, with a blur:
  `detail::soften`, the source less its finest à-trous bands on linear
  values, everything up to about two of the original's pixels and fading
  out by eight (the bands placed by `scale`, so the proxy matches), as
  one more image at full strength scaled by the layer (linear, so no
  fit; about 17 ms on the proxy). At −100 a one-pixel checker loses over
  80% of its contrast, as before, but now the same at 100% as on the
  proxy. Softening isn't held back by the halo limit, as negative
  sharpening through the stage would be.
- **Process version:** not bumped. Local Sharpness came earlier the same
  day and no release has had it (Develop isn't released: 0.2.3 predates
  it), so the only edits using it were made with today's builds, and they
  still look sensible (more or less of the same kind of sharpening). As
  with the proxy scale for finished images (Effects), the render hash
  stays, so a preview made before shows the old look until the edit
  changes.


## Phase 4 scope (2026-10-22)

- Phase 4 starts with Phase 3's gate (the 30-NEF review) still deferred to
  `docs/MANUAL-CHECKS.md`, as the spec's rule for photo gates allows; the
  spec's "nothing from Phase 4 before the Phase 3 gate" line now says so.
  Every phase's person-eye checks are done together once the phases are
  built.
- **In**, in three waves (agents on separate parts of the code at once):
  1. Sky and landscape masks (one scene-segmentation model with an open
     licence: SegFormer's own weights are checked first), control points
     (U Point), DCP profiles with colour calibration (CAT16) and `.cube`
     looks, AVIF and linear DNG export.
  2. Object removal (a convolution-only inpainting model), Lens Blur
     (Depth Anything V2 Small), the wide-angle face fix (volume
     deformation).
  3. Photo merges: HDR, focus stacking, panorama. These need safety rule
     3 to cover merge results (new files beside their sources, through
     `fileops`); the rule is amended when wave 3 starts.
- **Out:** JPEG XL export (`jpegxl-rs` builds Google's C++ libjxl; AVIF
  covers the modern-format need), learning your taste (needs a few
  hundred Shoebox edits first), body masks (hair, clothes, skin: no
  openly licensed human-parsing model chosen yet; revisit after the
  landscape model).


## Control points (2026-10-22)

- **What one is:** PhotoLab's U Point as the spec has it, built from
  parts that were already there; no new part kind, so edit files and the
  GPU pipe are unchanged. `mask::control_point` makes the recipe: a
  Radial part (Add) at the click, then a Colour range (Intersect) and a
  Luminance range (Intersect) around what's there. Parts combine in
  order from nothing (Add takes the max, Intersect multiplies), so the
  circle comes first and the ranges only narrow it: circle ∩ colour ∩
  brightness, the same in any order of the two ranges.
- **Sampling:** `mask::tone_at`, the range pickers' own 5×5 patch of the
  source read under the frame's colour and display (what `Tones` holds),
  now shared with them; `mask::picked_luminance` is the pickers' band too
  (±0.1 at full effect, smoothness 0.1). The colour range is 20° either
  side, the Colour part's default.
- **Greys:** a colour range passes no greys (chroma below 0.02–0.08), so a
  control point on something near-grey (relative chroma under 0.05) has
  no colour part: it's the circle ∩ brightness. Without that a click on a
  white wall or a grey sky would select nothing.
- **Size:** a circle of 0.12 long edges, feather 0.75: soft from about a
  quarter of the radius out, as a U Point's falloff from its centre
  rather than a flat disc. Sized afterwards with the ellipse's own
  handles (the right-hand one sizes and turns it, the lower one makes it
  an ellipse); the ranges are ordinary parts with their sliders and Pick.
- **Interaction:** the Local adjustments panel's **Control point** button
  or **K**; the next click on the photo makes the layer ("Control point
  n", history "New control point", one step), selects it with its circle
  in hand, and puts the tool down. Escape puts it down without placing
  one; it's one tool on the photo at a time with spots, guides, Point
  Color's and white balance's pickers. One click, one point (not
  PhotoLab's stay-on tool), so a click meant for a handle can't make a
  stray point.
- **K:** free in Develop (⌘K is keywords). Lightroom has no control
  points; its K is the adjustment brush, which Shoebox has no key for. If
  the brush gets a key it should be something else, or K moves.

## Sky and landscape masks (2026-10-22)

- **Not SegFormer.** NVIDIA's SegFormer weights (nvidia/segformer-b*-
  finetuned-ade-*) say `license: other` on Hugging Face and point to
  NVlabs/SegFormer, whose LICENSE is the NVIDIA Source Code License:
  §3.3 "may be used or intended for use non-commercially … for research
  or evaluation purposes only" (the README says the same). GPL-3.0
  forbids such restrictions, so neither the weights nor `candle-
  transformers`' `segformer` with them can be used.
- **Also looked at:** DINOv2's ADE20K heads (Meta, Apache 2.0, in
  `candle-transformers` as `dinov2`): ViT-S linear is 44.3 mIoU, and its
  14-pixel patches give a coarse map (37 cells across a 512 px short
  side); candle's `dinov2` also interpolates position embeddings by
  nearest neighbour with height and width swapped for non-square
  pictures and wants a classifier head the checkpoint lacks, so it would
  have been a port anyway. Hugging Face's openmmlab/upernet-convnext-*
  (MIT) are conversions of mmsegmentation's checkpoints; the authors'
  own came first.
- **Chosen:** UperNet on ConvNeXt-B (ImageNet-22K), 640 px crops,
  trained on ADE20K by ConvNeXt's authors: facebookresearch/ConvNeXt
  (MIT, "This project is released under the MIT license"), its semantic
  segmentation table's `upernet_convnext_base_22k_640x640.pth` (489 MB,
  f32, 122 M parameters; 52.6 mIoU single scale, against 46.0 for
  ConvNeXt-T and SegFormer-B0's 37.4) from Meta's file server
  (dl.fbaipublicfiles.com), SHA-256 pinned (`download::SCENE`). ADE20K's
  images have their own (research) terms, which bind the dataset, not
  the weights, as with SAM's SA-1B. The head dominates the work and is
  the same for T, S and B, so B costs little more time than T.
- **The port** (`shoebox-ml::scene`, from the repo at 048efcea and
  mmsegmentation v0.x's UPerHead and PPM): channels last throughout;
  the 7×7 depthwise convolutions as 49 shifted multiply-adds, each tap
  one contiguous run of the padded, flattened picture (twice as fast as
  strided views; candle's grouped convolution would run a convolution
  per channel); the stem and downsamplings as reshapes and a matmul;
  the head's 3×3 convolutions as one matmul per tap with batch norm
  folded in, their concatenated inputs taken in parts (an im2col of the
  2048-channel bottleneck would be 2.8 GB). f32 on Metal. The checkpoint
  is read directly (`state_dict`). Tested against direct sums for the
  convolutions and PyTorch's adaptive pooling bins.
- **Input:** the picture as shown, upright, on the masks' grid, resized
  so its short side is 640 (the long at most 2560) and both are
  multiples of 32, normalised as mmsegmentation does; the whole picture
  at once (the config's `whole` test mode).
- **Parts:** each a sum of ADE20K classes' softmax, so edges and unsure
  places are soft. Sky (sky); Vegetation (tree, grass, plant, field,
  flower, palm); Water (water, sea, river, swimming pool, waterfall,
  lake); Mountains (mountain, hill); Buildings (building, house,
  skyscraper, hovel, tower, bridge, pier, grandstand, wall, windowpane,
  door, column, awning); Ground (earth, land, sand, rock, road, path,
  sidewalk, dirt track, runway, floor). Lightroom's split of ground into
  natural and artificial is left as one; walls and floors count indoors
  too. ADE20K has no snow class (snow lands in whatever it lies on).
  In edits, `{"kind": "landscape", "part": "sky"}` (the spec's example
  said `ai_sky`; it now matches); a part a later build adds is kept by
  this one as an unknown part.
- **Soft and sharp:** the model's map is a quarter of what it saw
  (240×160 for a 3:2 photo). `mask::landscape` upsamples it to the
  picture it was found in with the guided filter `mask::fine` uses
  (coefficients on the coarse grid along the picture area-averaged
  there, applied with the picture's own colour), so edges land where the
  picture has them and stay soft where it has none. Exports and 100%
  regions then upsample again to the source's pixels, as for the other
  AI parts. A test edge one 8-pixel cell from a coarse cell's: the rows
  either side are on and off within two pixels.
- **Found once per picture:** the app registers the model as the masks'
  scene finder (`register_scene_finder`, identity `scene::MODEL_ID`), so
  each part goes through `mask::store` like Subject and People; the app
  keeps the last picture's segmentation, so a second part of the same
  photo is immediate. The model is downloaded when the panel's button is
  pressed (as SAM), into `<models>/upernet-convnext-base-ade20k`.
- **Timings** (M2 Max, Metal, release): load 1.2–1.7 s; a 2560×1707
  picture (seen at 960×640) 1.2 s, of which the backbone is 0.9 s (the
  depthwise convolutions most of it); the first run in a process takes
  about 10 s more while Metal compiles candle's kernels (as SAM's does);
  the guided upsampling to the 2560 px grid 0.3 s per part. On the three
  real fixtures (fire dancers on a night beach, a road through a van's
  windscreen, a still life against cardboard) the coarse parts looked
  right: the night sky as Sky (less where smoke is), the sand as Ground;
  the sky through the windscreen as Sky and the roadside buildings as
  Buildings; the cardboard walls as Buildings.
- **Checks:** unit tests with a fake finder (`tests/landscape.rs`:
  edges follow the picture, upright and put back, once per part and on
  disk, fine for big sources, edit JSON); a headless UI test (the
  Landscape menu, the download offered; with `SHOEBOX_SKY_MODEL` the
  sky of a drawn landscape found, 40% of the frame); the real model on a
  drawn landscape (`SHOEBOX_SKY_MODEL=<folder> cargo test -p shoebox-ml
  --test scene -- --ignored`): sky 1.00 above, 0.00 below, the grass all
  Vegetation. `examples/scene_probe.rs` writes each part of real photos
  as PNGs. Real skies: `docs/MANUAL-CHECKS.md`.

## Colour: DCP profiles, calibration and looks (2026-10-22)

### DNG camera profiles

- **What's read** (`dcp`, written from the DNG specification 1.6,
  chapter 6, with the SDK's reference code's behaviour; nothing ported):
  ColorMatrix1/2 and ForwardMatrix1/2 by CalibrationIlluminant1/2,
  ProfileHueSatMapDims/Data1/2, ProfileLookTableDims/Data, the two
  encoding tags, ProfileToneCurve, BaselineExposureOffset,
  DefaultBlackRender, ProfileName and UniqueCameraModel. "IIRC"/"MMCR"
  files (plain TIFF headers too), bounds-checked; a file that doesn't
  parse is skipped with a warning.
- **Matrices.** A profile's colour matrices (and forward matrices, when
  it has a pair) stand in for the raw file's everywhere: white balance,
  the as-shot white's temperature and tint, the eyedropper and the camera
  → Rec.2020 matrix (`wb::camera_for`, `wb::resolve_with`). They're
  normalised as the SDK does (a colour matrix's white peaks at 1; a
  forward matrix takes camera white to D50 exactly), and the brightness
  rule stays: the white's neutral, normalised to Y = 1 by the colour
  matrix, comes out at Y = 1, so moving between Matrix and a profile with
  the same matrices changes nothing.
- **Where the tables run.** Per pixel, right after the colour matrix and
  before defringe and the local tone maps: linear Rec.2020 → linear
  ProPhoto (Bradford to D50), HueSatMap, LookTable, back. HueSatMap is
  looked up at the scene's value before exposure (the colour matrix's
  gain undone), as the DNG pipe has it before its exposure ramp;
  LookTable after all of it (Shoebox's +0.75 baseline, the profile's
  offset and the slider). Two HueSatMaps are blended by inverse
  temperature of the white, as the matrices are. The local tone maps are
  worked out from the colour matrix alone (their guide is a small copy);
  with a profile that moves value a lot their gains are a little off, as
  with any stage before them.
- **Deviations from the SDK**, for a scene-referred pipe: values aren't
  pinned to 1 (it pins), and saturation is kept from going past the
  gamut's edge (`min(s·scale, max(s, 1))`) rather than pinned at 1, so a
  colour already outside isn't dragged in. An sRGB-encoded table scales
  the encoded value for 2.5D tables too (the SDK encodes only for 3D
  lookups). ProfileToneCurve is linear between its points (profiles'
  curves are dense; the SDK fits a spline). An unknown illuminant counts
  as D50.
- **The tone curve** is off by default ("Profile's tone curve" under the
  profile menu, `input_colour.dcp_tone_curve`): Shoebox's display
  transform stays in charge as it does for every other profile, and Auto
  and the baseline exposure are tuned to it. On, it replaces the scene
  curve and the AgX inset (Adobe's hue-keeping RGB tone on ProPhoto,
  values clipped to 0–1 as the profile's pipe clips them); Contrast,
  Whites and Blacks then do nothing. DefaultBlackRender is read, but
  Shoebox never adds an automatic black, so both values render alike.
- **The edit** stores `input_colour.profile: "dcp"` and
  `input_colour.dcp: {name, blake3}`. Profiles are found by contents, so
  one that's gone or changed renders as the matrix and Develop says so,
  offering a profile of the same name "as it is now" (a step). The
  render hash covers the reference, so previews follow. They travel in
  presets' Colour part with the rest of `input_colour`; Auto leaves a
  chosen profile alone.
- **The folder** is `<data>/profiles/` (beside the camera match
  profiles, `*.dcp`), read when the develop thread starts and again from
  the menu ("Read profiles again"); "Show profiles folder" makes it if
  it isn't there and opens it. Shoebox never writes into it. Only
  profiles whose UniqueCameraModel is the photo's model (or make and
  model) are offered.
- **GPU.** A stage may bind eight storage buffers and seven were used,
  so all colour tables share binding 6 (camera match look, HueSatMaps,
  LookTable, tone curve; offsets in the params), uploaded once and kept
  while the tables are the same ones. Checked against the CPU within
  1e-4 with two blended 2.5D tables, a 3D sRGB LookTable, forward
  matrices and the tone curve. The test tables keep greys' entries alike
  across hues, as real profiles do: a grey's hue is noise, and a table
  that changed greys by hue would turn rounding into visible steps.
- **Process version untouched:** the profile choice is new and off by
  default, so old edits render as before (the synthetic goldens are
  unchanged).

### Colour calibration (CAT16)

- **What it is:** the spec's Phase 4 option, darktable's "modern" scheme
  (`src/iop/channelmixerrgb.c`; no code ported, the CAT16 matrix is
  CAM16's): the camera renders as for daylight (its D65 multipliers and
  D65 calibration, a DNG profile's too), then CAT16, fully adapted,
  takes the scene's illuminant to D65 in the working space.
- **Where it lives: white balance.** It's
  `white_balance.adaptation: "cat16"` (default `"camera"`: the matrices
  interpolated for the white, then Bradford, as before), behind a
  "Colour calibration (CAT16)" switch in the White balance section. The
  same controls then name the illuminant: As shot (the as-shot white as
  the daylight rendering shows it), the presets, Temperature and Tint, and
  the eyedropper (the patch's colour in the daylight rendering), so
  there's one place for the light, not two modules fighting over it. It
  travels in presets' White balance part.
- **The Calibration section** (primaries' hue and saturation, shadows
  tint) is darktable's mixer part of the same module in Lightroom's
  terms; it stays as it is and applies after the adaptation, as before.
- **Mixed light:** under colour calibration, local adjustments'
  Temperature and Tint adapt by CAT16 as well, so a masked layer takes a
  second light out the same way (darktable does this with a second,
  masked instance). Without it they stay Bradford.
- **Cost:** none per pixel. It's a 3×3 matrix folded into the colour
  matrix (and each layer's), so the GPU needs nothing new; the parity
  test covers it (custom illuminant with layers, and as shot).
  darktable's gamut compression and clipping of negatives aren't
  ported: Shoebox's display transform already rolls saturated colours
  off; revisit if deep-blue LED shots show artefacts.
- **Brightness:** the white still comes out at the same Y as without it
  (von Kries keeps a white's luminance), and the as-shot temperature
  reads close to the plain one (within a few hundred kelvin on the Z f's
  matrices).
- **Old edits:** the field is absent unless set, so they read and render
  as before; no process version change. A DNG profile's tables blend for
  the temperature the camera's calibration is blended at
  (`Resolved::calibrated_at`: the white's, or D65 under calibration).

### Looks (`.cube`)

- **What's read** (`cube`): Adobe's and Resolve's `.cube` text: TITLE,
  LUT_3D_SIZE (2–65; larger is refused), LUT_1D_SIZE, DOMAIN_MIN/MAX,
  LUT_1D/3D_INPUT_RANGE, comments (whole lines and after data), other
  keywords skipped; wrong counts, bad numbers and empty ranges refused
  with the line. A 1D look becomes a 33³ table (exact between its
  points, since trilinear is linear per channel on a per-channel curve;
  a dense 1D curve loses a little between them); a 1D shaper before a 3D
  table is folded into the table. Hald CLUT PNGs aren't read yet.
- **Where:** on the display transform's output, after the camera match
  look and before the tone curve (the spec's "just after the display
  transform"; the tone curve can still adjust a look, as Lightroom's
  come before its curve). The files don't say their space; nearly all
  are made for sRGB-encoded Rec.709, so that's assumed: display-linear
  Rec.2020 (after the AgX outset) → linear sRGB, clipped to 0–1,
  encoded, the table (trilinear, `Lut3::sample`, as the camera match
  look), decoded, back. So a look keeps wide-gamut colours only at
  Amount 0. Amount (0–200%, default 100) mixes the result with what went
  in, extrapolating past 100%.
- **The edit** stores `look_lut: {lut: {name, blake3}, amount}`; a look
  that's gone or changed is skipped (the section says so and offers one
  of the same name as it is now). It's part of presets' Colour part with
  the profile, so a "film" preset carries its look; a preset's Amount
  scales the look's from 100% (below 50% a preset leaves the look out,
  as for anything that isn't a number).
- **The folder** is `<data>/looks/*.cube` (files over 64 MB skipped),
  read at start and from the section's menu ("Read looks again"; "Show
  looks folder" makes and opens it). Shoebox never writes into it:
  "importing" a look is putting it there, which keeps the person's files
  as they are and needs no copy through Shoebox.
- **GPU:** the table goes in the shared colour-tables buffer (a 65³
  table is 3.3 MB, uploaded once and kept while it's the same look);
  checked within 1e-4 at 70, 100 and 150% on a raw and a finished image
  with an unusual domain.

## AVIF and linear DNG export (2026-10-22)

**AVIF.**

- Encoded with `rav1e` and `avif-serialize`, the two crates `ravif` is
  made from, not with `ravif` itself: `ravif` 0.13 writes every file as
  sRGB (BT.709 primaries in both the AV1 header and the `colr` box) and
  takes 8-bit pixels, so a Display P3 export would be mislabelled and
  "10-bit" would be 8 bits padded out. `encode::avif` does its part:
  4:4:4 YCbCr (BT.601 coefficients, full range) from the render's float
  values at 10 or 8 bits; CICP in the sequence header and the `colr` box
  (sRGB 1/13/6, which is AVIF's default, so no box is written; Display
  P3 12/13/6); `ravif`'s quality scale and still-picture speed settings,
  credited in the code. Licences: `rav1e` BSD-2-Clause (with the
  royalty-free AOMedia Patent License 1.0), `avif-serialize`
  BSD-3-Clause, and what they bring in MIT, Apache-2.0 or BSD-2-Clause
  (`av1-grain`, `v_frame`, `av-scenechange`, `maybe-rayon`, …). `ravif`
  (BSD-3-Clause) isn't a dependency.
- sRGB or Display P3 only: AVIF describes colour by CICP, which has no
  code for Adobe RGB or ProPhoto (an ICC `colr` box would need a writer
  `avif-serialize` hasn't got). The dialog offers just those two.
- rav1e's NEON assembly is switched on for aarch64 only (a
  target-specific feature). The C compiler assembles it (the app already
  builds C, for `aws-lc-sys`); NASM is only needed on x86, where it stays
  off. Pure Rust, AVIF took 2.5–3.5× as long.
- Timings: a 24 MP Z f photo (a night scene), M2 Max with 12 cores. They
  were taken while other builds loaded the machine, so they're CPU seconds
  for the encode alone; on an idle machine wall time is roughly a tenth.

  | Quality | Speed 6 | Speed 8 | Speed 10 | Size | PSNR |
  | --- | --- | --- | --- | --- | --- |
  | 80 | 14 s (33 s pure Rust) | 13 s (32 s) | 3 s | 0.7 MB | 37.7 dB |
  | 85 | 17 s | 15 s (38 s) | 3 s (11.5 s) | 1.5 MB | 38.7 dB |
  | 90 | 21.5 s (48 s) | 23 s (45 s) | 5 s | 3.4 MB | 40.7 dB |

  For comparison, JPEG 92 (4:4:4) is 7.0 MB at 40.7 dB and takes about
  0.3 s. PSNR is measured against the render, as macOS reads the file
  back. Speed 10 gave the same PSNR with files about 10% larger. With
  `ravif`'s settings, speeds 6 and 8 cost about the same and make the
  same size.
- **Defaults: quality 85, 10-bit, speed 6** (libavif's default speed
  too). That's about 1.5–2 s for 24 MP on an idle machine, plus the
  render: inside the spec's 3 s for a full-size export. Speed (1–10) is a
  preset setting.
- Checked by: a test of the boxes (`ftyp`, `ispe`, `pixi`, `colr`), and
  by macOS's own decoder, ImageIO (what Preview and Photos use). It reads
  both spaces back within 4/255, and reads the P3 file as P3: its
  saturated red, seen in sRGB, loses its green. On the real exports,
  `sips` reports 10 bits and Display P3.
- Metadata: exiftool writes EXIF and XMP into AVIF, so the preset's
  metadata choice works as it does for the other formats. That was
  checked on a real file (camera, lens, exposure, date). `avif-serialize`
  could embed EXIF itself, but it isn't used: exiftool copies metadata
  for every format.

**Linear DNG.**

- **What's in it:** the source as the pipe starts from it. That is the
  edit's raw settings (black and white points, hot pixels, highlight
  handling, demosaic) and the AI denoise at the edit's Strength. It's in
  camera-native linear RGB (the as-shot balance undone), with the raw
  file's two ColorMatrix tags (each with its CalibrationIlluminant) and
  AsShotNeutral, so another raw editor renders it from scratch, as it
  would the NEF. NoiseReductionApplied records how much AI denoise is in
  it (0 for none, 1 at full Strength); Adobe's readers use it to ease off
  their own noise reduction.
- **Left out, decided:**
  - The detail stage (classic noise reduction, sharpening, texture),
    spot removal, geometry (crop, rotation, upright) and the whole look:
    any raw editor redoes these.
  - Lens corrections: baking them in would see them applied twice by
    editors that apply a profile from the lens in EXIF. Lightroom's own
    Enhance DNGs leave them out too.

  The file is there for the AI denoise, which nothing else reproduces.
  The only thing applied is the EXIF orientation: the pixels are upright
  with Orientation 1, as in every export. The edit's own quarter turns
  aren't applied.
- **No ForwardMatrix:** the NEF has none (rawler gives the two
  ColorMatrix tags). Without one, the DNG spec inverts ColorMatrix and
  adapts to D50 with Bradford, which is what a ForwardMatrix worked out
  from them would say anyway.
- **Storage: 16-bit unsigned LinearRaw**, lossless JPEG in 256 px
  tiles, in a sub-IFD, through rawler's writer. That's how Adobe DNG
  Converter writes linear DNGs, and every raw reader takes it.
  - Half floats with deflate were considered. They would keep
    reconstructed highlights above white and finer shadow steps, but
    rawler's writer doesn't write them and readers support them less
    well. 16 bits is already finer than the 14-bit sensor everywhere.
  - Values clip at the sensor's white, so Shoebox's reconstructed
    highlights aren't carried over: the other editor does its own
    highlight recovery, as with Adobe's linear DNGs.
  - BaselineExposure isn't written (it isn't known for the camera), so
    another editor's default brightness may differ slightly from the
    NEF's.
- **Previews and tags:** IFD 0 holds a preview of at most 256 px in sRGB
  at the code defaults, for Finder and Quick Look. Make, Model and
  UniqueCameraModel come from the raw decoder. exiftool then copies EXIF
  (camera, lens, exposure, dates) from the NEF, as for the other formats,
  and other editors need that to find lens profiles. Without exiftool,
  the DNG has no lens data.
- **Size and time:** a 24 MP Z f photo makes 67–78 MB, written in
  0.2–0.35 s once the source is ready (no render is needed).
  - With AI denoise off in the edit, the DNG is just the RCD demosaic.
  - With AI denoise on but its result missing (the model isn't
    downloaded), the export fails rather than archive a file that isn't
    denoised.
  - A finished image (JPEG, TIFF and so on) is refused: it has no raw
    data.
- **Checked by:**
  - rawler reads the file back (size, three samples, LinearRaw, white
    level, matrices, neutral, values, NoiseReductionApplied, orientation).
  - macOS ImageIO renders it from the raw data (at full size, not from
    the preview), with a grey card coming out grey.
  - For the two real Z f files (one turned 90°), Preview's rendering
    matches Shoebox's colours (`sips`), and exiftool's metadata copy
    leaves the DNG readable by both.
  - Opening it in a third-party raw editor is a manual check.
- **Should Shoebox treat its own DNG exports as editable sources?** Yes,
  in time. rawler decodes them (LinearRaw, three samples), and Develop
  would open one as a raw without demosaic or AI denoise, since those are
  done and NoiseReductionApplied says so. It would start from default
  settings, like a merge result. But the library doesn't read DNG or AVIF
  files yet: `shoebox_decode::Format` has neither, so such a file in the
  library would be one Shoebox doesn't show. So for now:
  - Both formats export only to folders outside the library.
  - "Beside the original" is disabled for them in the dialog, and the
    export thread refuses it.
  - A folder inside the library is refused, as it is for every format.

  The follow-up that lifts this adds both formats to the library. DNG
  would go through rawler, both CFA DNGs and linear ones (skipping
  demosaic), with ImageIO for thumbnails. AVIF would go through ImageIO,
  as HEIC does.

  **Lifted the same day** ("DNG and AVIF in the library", below): the
  library reads both, and both export beside the original like every
  other format. A folder inside the library is still refused.

**Presets.** `FileFormat::Avif { quality, ten_bit, speed }` and
`FileFormat::LinearDng` are stored as `{"kind":"avif",…}` and
`{"kind":"linear_dng"}`. `ten_bit` and `speed` take their defaults when
missing, and older presets load unchanged. There are no new starter
presets.

## Wave 1 follow-ups, decided (2026-10-22)

- **AVIF:** rav1e's NEON assembly stays on (aarch64 only; the C compiler
  builds it, no NASM), for the 2.5–3.5× speed.
- **Landscape model:** ConvNeXt-B in f32 (489 MB) stays: accuracy over
  download size. No half-precision variant.
- **DNG and AVIF in the library:** yes. The library reads both (Shoebox's
  own linear DNGs open in Develop as raws past demosaic and AI denoise),
  and both formats can then be exported beside the original. Lifts the
  folder-only rule in "AVIF and linear DNG export".

## Volume deformation (2026-10-22)

ViewPoint's wide-angle fix (EDITING-SPEC, Optics and geometry), in
`shoebox_develop::volume`.

- **Settings:** `geometry.volume`: `mode` (off, horizontal, vertical,
  spherical), `amount` (0–100, default 100), `faces` (Fix faces),
  `face_amount` (0–100, default 100) and `boxes` (faces found, in the
  stored image in long-edge units, as Smart Lighting's spot). Off by
  default and stored as differences, so old edits are unchanged and
  render exactly as before (no grid). Everything, boxes included, is in
  the render hash: unlike guided upright's guides, the boxes decide the
  render. At most 64 boxes count, and non-finite or inside-out ones are
  skipped (an edit is JSON anyone could write).
- **How wide:** the focal length in pixels from EXIF focal length × the
  camera's crop factor (Lensfun's) over full frame's 43.27 mm diagonal. A
  camera Lensfun doesn't know is full frame from 10 mm up; under 10 mm
  (a phone or compact, sensor unknown) or no focal length, a 24 mm
  equivalent (84° diagonal). Never wider than 150° diagonal. The panel
  shows the field of view used.
- **Global maps** (normalised `x = (p − c)/f`, so `|x| = tan θ`; the
  centre keeps its place and scale, lines through it stay straight):
  - Spherical: radius `(1 − a)·tan θ + a·2 tan(θ/2)`: stereographic at
    100%, conformal, so a disc anywhere comes out round.
  - Cylindrical: across the upright frame `u' = (1 − a)u + a·atan u`,
    down `v' = v·((1 − a) + a·cos atan u)`: verticals stay vertical, what's
    at the sides keeps its shape on the axis. Vertical is the same turned.
    "Horizontal" follows the upright frame (file orientation and the
    edit's turns: 5–8 swap the stored axes).
  - Inverted by bisection on the radius (or the bending coordinate):
    both are monotonic, bounded by the stereographic and `(1 − a)` limits.
- **Faces:** Shih, Lai and Liang (SIGGRAPH 2019) simplified. Per face, the
  stereographic projection moved and scaled by a similarity so the face
  keeps its centre (where the global map puts it) and its area: the
  similarity nearest the Jacobian of global ∘ inverse-stereographic at the
  face, scaled to its determinant. Its weight is whole within 0.75 × the
  box's longer side (the head, not just eyes to chin) and smoothsteps to
  nothing by 2.5 times that; overlapping weights are normalised to sum
  at most 1. Outside the feather nothing moves, so straight lines away
  from faces stay straight; lines crossing a feather bend a little (Shih
  et al.'s line and smoothness terms, solved over one mesh, would
  spread that; not done). The forward map is inverted per node by Newton
  from the global inverse. Boxes are found in the source (before lens
  corrections), so each centre is taken back through the lens model by
  fixed-point iteration before use.
- **Where:** after lens corrections, before the Transform's perspective
  (the projections are about the optical centre of a rectilinear picture,
  which the perspective moves), folded into the lens grid: each node holds
  where the lens put the point the deformation shows there, so the live
  pipe still makes one resample and the shader is unchanged. The grid is
  65 nodes on the long edge as before, 129 with faces (a face's feather
  spans a few cells). A photo with no lens data gets a grid with no lens
  part. CPU/GPU agree within 1e-4 in scene values (2e-6 measured); on
  display values the gpu test allows 1e-3, as for rotation (2e-4 measured,
  the display toe magnifying position rounding on a dark saturated ramp).
- **Filled:** the lens grid's fill zoom covers the deformation too
  (searched up to 4× instead of 2×), so the frame stays filled; Spherical
  at 100% on a 24 mm equivalent zooms 1.17× (a face near a corner,
  1.01×). No option to keep
  the blank corners.
- **Masks and spots:** read at the geometry map's position before the lens
  grid, as before, which is now also before the deformation: they stay
  where they were drawn on screen, and content moves under them when the
  deformation changes (as with lens corrections). AI masks and spots were
  already in source coordinates read at that position; near the corners of
  a strongly deformed wide photo they can sit off what they covered, so
  draw them after setting the deformation. Faces' boxes are drawn where
  they show (`Frame::unlensed` undoes the grid).
- **Auto-upright** analyses the picture with the deformation (the
  perspective acts on it); Cylindrical keeps the verticals it needs.
- **Presets and copied settings:** the Transform part carries the mode and
  Amount (they suit a lens and a scene); Fix faces, its amount and boxes
  stay with their photo, as Smart Lighting's spot does. Crop & rotate's
  Reset leaves the deformation alone (it has its own section and Reset);
  the Original view shows it undone, to compare.
- **Panel:** a "Volume deformation" section after Transform: field of
  view, Off / Cylindrical (horizontal) / Cylindrical (vertical) /
  Spherical, Amount, Fix faces (looks for faces on a thread when first
  ticked, Find again, Show faces draws their boxes, Face fix). History:
  "Volume deformation: Spherical", "Volume amount 60%", "Fix faces on",
  "Faces found: 2", "Face fix 80%", "Reset volume deformation".

## DNG and AVIF in the library (2026-10-22)

Decided in "Wave 1 follow-ups": the library reads both formats, Shoebox's
own linear DNGs open in Develop as raws past demosaic and AI denoise, and
both formats export beside the original.

**File kinds.** `Format::Dng` (`.dng`) and `Format::Avif` (`.avif`).
`Format::is_raw` (NEF, DNG) is what Develop decodes as raw data;
`Format::is_platform` (HEIC, AVIF) is what ImageIO decodes. The file-type
filter offers both. The scanner, the watcher, import and the CLI take
every `Format`, so a camera's DNGs on a card are imported like NEFs.

**DNG in the library.**

- **Shown from its previews.** `dng::previews` lists every
  reduced-resolution image (NewSubFileType 1, in IFD 0's chain and the
  SubIFDs) stored as baseline JPEG (Adobe DNG Converter's) or uncompressed
  8-bit RGB (IFD 0 thumbnails; Shoebox's own is at most 256 px). `render`
  takes the smallest that's big enough, as for NEFs: the size asked for,
  or the image's own for full size, within 5%. Previews are taken as sRGB
  unless they embed a profile (PreviewColorSpace isn't read; Adobe's and
  Shoebox's are sRGB).
- **Else rendered from the raw data**, by ImageIO (Apple's raw engine,
  what Preview and Finder show), and by rawler's own developer (PPG, the
  D65 matrix, the sRGB curve; flatter) where ImageIO fails, and off macOS.
  The brief said rawler; ImageIO comes first because its rendering is the
  one people see elsewhere on the Mac, it reads more DNGs (JPEG XL DNGs
  from recent phones), and it was already checked on Shoebox's linear
  DNGs. This happens for a linear DNG past 256 px, an Adobe "medium"
  preview in the loupe, or a DNG with no preview. Thumbnails are cached as
  for any file, so it's once per size.
- **Metadata** as for NEFs: EXIF from the TIFF container. The size is the
  main image's DefaultCropSize (or its whole size), turned by IFD 0's
  Orientation, which also turns its previews and renders. rawler's writer
  puts a linear DNG's IFDs after the data, so its first scan reads the
  whole file for them (the hash reads it anyway).
- **Image-data hash**, adding to M1's rules (JPEG: SOS..EOI; TIFF: IFD 0's
  strips; NEF: its JpgFromRaw's scan data): BLAKE3 of the main image's
  strips or tiles (NewSubFileType 0, the largest), in file order. Two DNGs
  of the same raw data with other metadata or previews match exactly; a
  NEF and the DNG converted from it hold different encodings, so they
  match perceptually only. Perceptual hashes come from the 256 px render.
- **Stacks:** primary order NEF, DNG, TIFF, JPG, PNG, HEIC, AVIF, movies.
  A camera DNG beside a NEF of the same name (one converted from it)
  stacks with it by name, the NEF primary; a phone's DNG and JPG make a
  `DNG+JPG` stack, the DNG primary. Duplicate keepers: NEF > DNG > TIFF >
  JPG and HEIC > PNG and AVIF.
- **"NEF missing"** counts any raw file in the stack: a DNG is the raw.
  The label stays as SPEC names it.
- **Sidecars:** `x.dng.xmp`. The per-stem `x.xmp` that Settings adds for
  raw files stays NEF only: Lightroom reads a DNG's marks from inside it,
  which Shoebox never writes, so a stem sidecar would be one more file
  nothing reads, shared with a NEF of the same name.

**AVIF in the library.** As HEIC: ImageIO decodes it (thumbnails, loupe,
perceptual hashes), from its nclx colour or ICC profile into the display's
space, and applies its rotation. Its size comes from the HEIF boxes
(`heif`: the primary item's `ispe`, turned by its `irot`), not a decode.
EXIF (kamadak reads AVIF as HEIF) gives the date and camera; its
Orientation is set aside, since `irot` is what turns an AVIF. No
image-data hash, as for HEIC. Develop opens it as a finished image.

**Develop.**

- **Camera DNGs are raws.** rawler decodes CFA DNGs, the path High
  efficiency NEFs already take through their converted DNGs; nothing to
  convert. The camera's own lens data is read from NEFs only; Lensfun works
  from EXIF as for any raw.
- **Linear DNGs** (LinearRaw, three samples): `raw::decode` keeps the
  samples (`RawImage::linear`, with NoiseReductionApplied) and
  `cpu::demosaic` skips hot pixels, highlight reconstruction and demosaic:
  the pixels balanced at as shot, with the edit's raw black and white
  points. Matrices and as-shot white come from the file. The source comes
  back within 16-bit steps of what was exported, and a render at the
  defaults matches the original's (test).
- **No AI denoise.** Detail says what the DNG records ("AI denoise: in
  this linear DNG (40%)") instead of offering it, and Raw offers only the
  levels. Auto's AI from ISO, local AI strength and the AI thread leave it
  alone. A linear DNG exported from one carries its NoiseReductionApplied
  over.
- It starts from default settings, like any photo.

**Exports beside the original.** The folder-only rule in "AVIF and linear
DNG export" is lifted: the dialog offers Beside the original for every
format, and the export thread no longer refuses it. AVIF and DNG exports
there go as the others do: written in the cache, copied in as one
journaled batch that never replaces (`-1` when the name is taken), linked
into the photo's stack as variants, listed in the Info panel, flagged
stale when the edit changes, and replaced only by re-exporting them (the
old file to the Trash, ⌘Z brings it back). A folder inside the library is
still refused. A linear DNG goes stale on any change to the edit, though
only its raw settings and AI denoise are in it: the same rule as every
export, at the cost of a flag after a look-only change (open choice).

**Caches.** No schema or version bump: earlier scans skipped `.dng` and
`.avif` files altogether, so a rescan adds them as new files and no cached
row changes meaning.

**SPEC.** Its file types, the decode crate's line and the stack's primary
order now include DNG and AVIF. Nothing conflicts.

**Checked by:** decode tests (preview choice, the raw rendered without
one, the hash's ranges, orientation, AVIF made by `sips` through ImageIO),
core (scan, metadata, stacks, an export linked as a variant, hashes, the
cache reused), develop (a linear DNG reopened without demosaic, AI denoise
carried over; Shoebox's P3 AVIFs read in P3), app (both formats exported
beside a NEF and beside a linear DNG, never replacing; a UI test choosing
AVIF and Beside the original in the dialog, both joining the stack and
opening in Develop). Real files: MANUAL-CHECKS, "DNG and AVIF in the
library".

## Object removal (2026-10-22)

- **The model: MI-GAN** (Sargsyan, Navasardyan, Xu and Shi, "MI-GAN: A
  Simple Baseline for Image Inpainting on Mobile Devices", ICCV 2023;
  Picsart AI Research), the 512 px model trained on Places2. Convolutions
  only (depthwise-separable 3×3s, fixed `[1, 3, 3, 1]` resampling
  filters, skips, fixed noise; no FFT), 7.4 M parameters, f32, 29.6 MB.
  It was made for this: the authors' own pipeline crops around the hole,
  resizes to 512 and blends back.
- **Licence, code and weights:** github.com/Picsart-AI-Research/MI-GAN
  (commit 2b793c5e) has `LICENSE`, MIT ("Copyright (c) 2024 Picsart AI
  Research (PAIR)"), and a separate `LICENSE-WEIGHTS`, also MIT, which
  the README names as the weights' licence ("see LICENSE-WEIGHTS … for
  the license"). The authors' Hugging Face copy (andraniksargsyan/migan,
  ONNX only) is tagged `license: mit`. MIT is GPL-3.0-compatible.
  Places2's research terms bind the dataset, not the weights (as with
  ADE20K and SA-1B).
- **Also looked at:** Big-LaMa (Apache 2.0) needs FFT (its fast Fourier
  convolutions), which candle lacks; MAT's weights are CC BY-NC 4.0
  (non-commercial, which GPL-3.0 can't carry); Stable Diffusion
  inpainting is under OpenRAIL-M's use restrictions (incompatible) and
  far too big; AOT-GAN (Apache 2.0, convolutions only, a Places2 512
  checkpoint) is the fallback, twice MI-GAN's size and older.
- **Weights:** the authors' own `migan_512_places2.pt` (a state dict
  candle reads directly) from their Google Drive, linked from the
  README, through the same `drive.usercontent.google.com` endpoint as
  RawNIND, SHA-256 pinned (`download::INPAINT`); downloaded from the
  Spot removal panel when Remove is first used, into
  `<models>/migan-512-places2`. If Drive stops serving it, mirroring it
  on a release (with the user's go-ahead) is the fallback.
- **The port** (`shoebox-ml::inpaint`, from `migan_inference.py`):
  channels last; the depthwise 3×3s as shifted multiply-adds (as the
  scene model's 7×7s), halving as 16 strided taps, doubling (nearest ×2
  keeping one pixel of four, padded 2/1, the 4×4 filter) as four 2×2
  phases on the input, the 1×1s as matmuls; LeakyReLU 0.2 × √2 clamped
  to ±256; the stored noise per level. The noise fixes the size at 512².
  Tested against direct sums of PyTorch's definitions (no PyTorch here to
  compare whole outputs); on the real model a 60 px disc cut from a
  gradient comes back within 0.004 of it, and a 20 px post across 32 px
  stripes within 0.019 (a flat fill: 0.167). A 40 px post across the same
  stripes is filled with plausible but not continued stripes: regular
  structure through a wide hole is the model's weak spot.
- **In the pipeline:** a removal is a spot (`kind: remove`) holding
  brush strokes (`strokes`, the mask brush's `Stroke`, in long-edge units
  of the stored image; `dest` is their middle, for grabbing). Spots run
  in order on the source after the detail stage, as heal and clone do,
  so removals are in linear light after denoise and before tone, and
  the result is what the GPU uploads (`Demosaiced::prepared`); the shader
  is unchanged.
- **Feeding it:** a square crop around the strokes, twice their extent
  and at least 0.2 long edges (a 2560 px proxy's 512), clamped to the
  photo (the whole photo, stretched, when the strokes are that big, as
  the authors do), averaged down (or bilinear up) to 512. The hole is
  everything the strokes reach, feather included, plus half a model
  pixel. The model was trained on sRGB JPEGs, so the crop is divided by
  its own bright end (the 99th percentile of the kept pixels' largest
  channel) and sRGB-encoded; its output is decoded the same way. Raws
  are fed in camera RGB balanced at as shot (near enough neutral; the
  model fills from what's around, so the primaries matter little).
  Clipping at the bright end only touches the fill's neighbours read
  bilinearly at the hole's edge.
- **Blending:** the fill (the model's picture in the hole, the input
  elsewhere) read bilinearly at the source's pixels, mixed in by the
  strokes' coverage: full within the brush radius, fading over Feather ×
  radius beyond it (at least a pixel), times Opacity. Erasing strokes
  take away what earlier strokes painted.
- **Grain:** an upsampled fill is smoother than the photo (a noisy night
  shot showed a soft band at 100%). The photo's fine detail around the
  hole (each pixel less its 3×3 mean) is fitted per channel as `a + b ×
  value`; what the fill lacks of it is added as noise from an integer
  hash of the pixel, correlated between channels as the photo's is. A
  smooth fill in σ 0.02 noise comes out with 0.0189 of detail against
  the surroundings' 0.0190; on clean photos it adds nothing.
- **Exports and 100% regions** make their own crop of the full-size
  source and ask the model again (0.6 s) rather than upsampling the
  proxy's fill: the crop is in the stored image's units, so the model
  sees the same part of the photo at the same 512² (only resampling
  differs), and it fills alike (a test: proxy and full size within 0.02).
  The fill is then upsampled to the crop's pixels and grained.
- **Kept:** in memory per source (by a BLAKE3 of exactly what the model
  sees, its identity and a version), so other sliders, opacity and the
  images beside the source for local adjustments reuse it; on disk as
  16-bit RGB PNGs in `<cache>/ai-fills` (256 MB, least recently used
  removed, through `mask::store`'s store, which now takes 16-bit RGB
  besides 8-bit masks), so reopening a photo doesn't run the model.
- **The live view never waits:** fills are made on a thread of their
  own (`maps::Cache::fill`), the newest first, skipping any the last
  healing no longer wants (a feather slider dragged doesn't queue one per
  frame); until one arrives the object stays and the panel says
  "Removing…". Exports, previews and 100% regions fill inline.
- **Tool:** Spot removal → Remove (with Q's tool on): a drag paints a
  stroke (shown red as it goes, from where the button went down), and
  letting go makes it a removal ("Remove object", one step) and selects
  it; a click is one dab. Its own Brush size (default 5, i.e. 0.025 long
  edges: a spot's 2 is too small for objects); Feather and Opacity as
  spots'. The selected removal shows its strokes; dragging its ring moves
  them (one step); Option-drag erases from it ("Erase from removal").
  Size on a selected removal resizes its strokes. No SAM "click an
  object to remove it" yet: a removal's hole is strokes only.
- **Bounds:** at most 500 strokes and 20,000 points a removal, finite
  points, radii at most a quarter of the long edge (as spots). Points
  are spaced at least a quarter radius apart (as the brush spaces them),
  and a removal's segments may reach over 16 long edges² in all (each
  segment's box, which is what drawing it costs; a hand-painted removal
  is well under one), so a hand-written edit can't make a removal take
  hours. At most 100 removals run in one edit (each asks the model).
  Found by a security review of the commit.
- **Measured** (M2 Max, release, Metal): the model 0.6 s a fill (CPU 2.7
  s), the first in a process about 1 s with Metal's compilation; load
  0.1 s; making the job 16–35 ms, blending 2–10 ms on a 24 MP frame. On a
  real night shot (fire dancers on a beach) a speaker on a tripod went,
  sand and dark sea continued, no seam at fit; at 100% the grained fill
  is close to the sand around it.
- **Checks:** unit tests with a stand-in model (`tests/remove.rs`: the
  object goes and nothing else changes, feather, kept in memory and on
  disk, the live view's thread, proxy against full size, grain, edit
  JSON); a headless UI test (paint, the download offered, move, erase;
  with `SHOEBOX_INPAINT_MODEL` a post filled as the sky beside it); the
  real model (`SHOEBOX_INPAINT_MODEL=<folder> cargo test -p shoebox-ml
  --test inpaint -- --ignored`); `examples/remove_probe.rs` times it on a
  real raw and writes the crop before and after. Real photos:
  `docs/MANUAL-CHECKS.md`.

## Lens Blur (2026-10-22)

**The depth model.**

- **Depth Anything V2 Small** (Yang et al., 2024). Its Hugging Face page
  (depth-anything/Depth-Anything-V2-Small, revision 03876f86) says
  `license: apache-2.0`, and the authors' README (DepthAnything/
  Depth-Anything-V2) says "Depth-Anything-V2-Small model is under the
  Apache-2.0 license. Depth-Anything-V2-Base/Large/Giant models are under
  the CC-BY-NC-4.0 license": only Small is ever fetched. The authors' own
  `depth_anything_v2_vits.pth` (99 MB, f32, 25 M parameters), SHA-256
  pinned (`download::DEPTH`), read directly by candle's PyTorch reader;
  downloaded from the Lens Blur section when first wanted, into
  `<models>/depth-anything-v2-small`.
- **A port, not `candle-transformers`' `depth_anything_v2`.** That one
  differs from the authors' model where it shows: every resize in the
  DPT head is nearest-neighbour (the authors' are bilinear, corners
  aligned), the head only takes square pictures (a 3:2 photo would be
  squashed), its DINOv2 layer norms use ε 1e-5 (1e-6), and its DINOv2
  wants an ImageNet classifier head the checkpoint hasn't got.
  `shoebox-ml::depth` ports the authors' code (at a561b849: `dpt.py`,
  `dinov2.py`, `util/blocks.py`, `util/transform.py`) in about 400
  lines: ViT-S/14 with LayerScale and exact GELU, the position embedding
  interpolated bicubically as DINOv2 does (its scale factor, with the
  0.1 it adds), the DPT head. No new crates.
- **Input:** the picture as the frame shows it on the masks' grid,
  upright (as the AI masks get it), resized so the short side is 518 and
  the long one in proportion, both multiples of 14 (the authors' "lower
  bound" resize), area-averaged (the authors use OpenCV's bicubic, which
  aliases when shrinking a large picture); ImageNet's normalisation. Out
  comes relative inverse depth at that size.
- **Checked** without PyTorch: on a drawn scene (a ball on a chequered
  floor under a sky) the ball and the near floor come out nearer than
  the far floor, the sky farthest (`SHOEBOX_DEPTH_MODEL=<folder> cargo
  test -p shoebox-ml --test depth -- --ignored`); on two real fixtures
  (the night beach's crowd, the van's seats and pillars) the maps are
  right, with crisp edges round people and seats.
- **Timings** (M2 Max, Metal, release): load 0.17 s; a picture 0.22–0.34
  s (0.77 s the first time in a process, while Metal compiles candle's
  kernels); then the guided upsampling to the grid, about 0.15 s at
  2560 px.
- **Apple's depth data:** Z f NEFs have none; HEIC portrait depth
  (AVDepthData) is a later item, to stand in for the model where a photo
  has it.

**The depth map** (`shoebox_develop::depth`).

- The model's output normalised (its 0.5th percentile to 0, its 99.5th
  to 1, so a stray pixel doesn't set the range), kept on disk by
  `mask::store` as a 16-bit PNG at the model's size (keyed, as the AI
  masks are, by a BLAKE3 of the exact picture and the model's identity),
  then upsampled to the grid by the colour guided filter `mask::fine`
  uses (`mask::guided`, now shared with the landscape parts), so the
  blur's edges land on the picture's, and turned back to the stored
  image's orientation. In memory per source and orientation, as subjects
  are; found on a thread of its own for the live view, inline for 100%
  regions, previews and exports.
- **Refining:** local adjustments gain a **Depth** slider (−100 to +100,
  shown while Lens Blur is on): inside the mask the depth map is pushed
  nearer or farther (+100 a whole map nearer), layer by layer, kept to
  0–1. The simplest thing that fixes a missed subject: a Subject or brush
  mask with Depth up brings it into the focus range, and a push keeps the
  model's detail inside the mask (painting depth values would flatten
  it). Refined on the CPU (a few ms on the grid), kept while the masks and
  pushes are the same.

**Rendering** (`cpu::lens_blur`).

- **Where:** in the source stage, after the detail stage, spots and the
  layers' source mixing, as each source pixel is read: in scene-referred
  linear light (the colour matrix, tone and display transform come
  after, so highlights bloom as through a lens), in the source's pixels
  (crop, rotation and lens corrections apply to the blurred scene).
- **Circle of confusion:** a lens's blur circle grows with the
  difference in inverse depth from the focus, which is what the map
  holds: `c = amount · max(|d − focus| − range/2, 0)`, signed (positive
  nearer), 4% of the long edge at Amount 100 a whole map from the focus.
  The focus is the depth at a point clicked on the photo (stored as mask
  positions are, so it follows a changed or refined map), or with none,
  the nearest tenth's (the map's 90th percentile).
- **A working copy:** the blur is gathered on a copy whose long edge
  puts the largest circle at 16 of its pixels (256–1024 px: 400 at
  Amount 100, 800 at 50), so a pixel gathers at most about 850 taps
  whatever the photo's size, on the proxy and in a 45 MP export alike.
  Each working pixel takes its nearest source pixel's circle (the
  largest signed circle in its box) and averages the source, highlights
  boosted, each source pixel weighted by how like that its own circle
  is: on a depth edge it is one side, not a mix at a depth neither has.
  (The circle at its centre, then the box's most typical one, came
  first: they flip between the sides as the depth moves a hair, which
  spread the subject's colour onto the background, and the GPU and CPU
  disagreed where they flipped.)
- **Gathered as if scattered, in two layers** (after Jimenez's and
  Sousa's game renderers): each neighbour within its own circle covers
  the pixel, weighted by one over its circle's area (an area table per
  shape, from the taps themselves). Neighbours nearer and blurrier by
  more than half the pixel's circle (at least a pixel) go, over a
  one-pixel ramp, to a front layer whose coverage is its alpha; in the
  back, a neighbour farther than the pixel reaches it no further than the
  pixel's own circle. So a sharp subject never takes the background's
  blur, the background never takes the subject's colour, and a blurred
  foreground spreads over what's behind it.
- **Back to the source's pixels:** each source pixel mixes the sharp
  source with the back layer by its own circle (in working pixels: none
  under a quarter, all from one; a circle smaller than a working pixel is
  a mix of sharp and the copy's softness), the four working pixels read
  bilaterally (weighted by how like the pixel's circle theirs are, so
  across a depth edge the other side doesn't bleed in), then the front
  layer over it. A pixel in focus is the source exactly.
- **Bokeh:** a circle, or six straight blades (a hexagon with a corner
  up): the taps' distances are measured in the aperture's shape, the
  edge antialiased over a working pixel. Cat's eyes and onion rings
  aren't modelled.
- **Highlight boost:** source pixels from 40% of clipping up gain up to
  ×5 at full boost (a smoothstep) before averaging into the working
  copy, so clipped lights carry more light into their discs, as their
  unclipped brightness would. Pixels in focus never use the copy, so
  they aren't brightened.
- **Show depth:** the map (near light) with the focus range tinted
  amber, instead of the photo; the focus point is drawn while choosing
  it or showing the map. **Set focus** then a click (Escape stops; one
  tool on the photo at a time), **Nearest** forgets the point.
- **Measured** on synthetic scenes (`tests/lens_blur.rs`, a fake depth
  model): the subject unchanged within 1e-6 a few pixels in from its
  edge; a background checker's contrast 0.427 → 0.003; no red from a red
  disc on the background 2–40 px beside it (0.0500 against 0.0500 far
  away, also with a working copy a third of the source's size); the proxy
  and a render twice its size agree within 0.0004 (mean, after
  downscaling); boost 100 brings 1.7× the light round a clipped point;
  the hexagon reaches 9 px up and 8 across.

**GPU** (`gpu/lens_blur.wgsl`).

- Two compute passes of their own before the develop pass (`prepare`:
  the working copy and circles; `gather`: the two layers into a
  two-layer array), each with its own bind group, so the develop pass
  gains only two sampled textures (binding 11 the refined depth map,
  R32Float; 12 the layers' array) and no storage buffers (seven of its
  eight are used). Its `texel` mixes them in as `cpu::Source` does, after
  the layers' aux images. The depth map is uploaded once per map (copied
  in by the encoder, so `Pipe::encode` keeps its signature), the working
  textures are kept by size, and the taps and area table are one storage
  buffer of the gather pass's own.
- Within 1e-4 of the CPU (display and linear output) for the circle and
  the blades with boost, a refined map, a working copy a third of the
  source's size and the depth shown; 1e-3 on a turned frame, as for the
  develop pipe. The ramps (the front share; the largest circle rather
  than a choice) keep float rounding from flipping a decision.
- **Timings** (M2 Max, a 2560×1707 proxy, release, all passes; other
  builds were loading the machine, so the higher figures are under
  load): 7–10.5 ms a frame at Amount 100 (working copy 400 px, 861
  taps), 10.5–21 ms at 50 (800 px), 7 ms at 10 (1024 px, 69 taps); the
  develop pass alone is 1.4 ms. The CPU's blur of the same proxy takes
  60–510 ms (100% regions, the CPU fallback), kept on the source between
  renders. A 24 MP export's whole CPU render goes from 0.8 s to 1.1–2.0 s
  (`a_full_size_render_on_the_cpu`, ignored; the working copy's
  averaging reads every source pixel's depth).

**The edit.** `modules.lens_blur: {enabled, amount, focus, range, bokeh,
boost}` (off, 50, none, 10, circle, 0) and `locals[].adjust.depth`,
stored as differences from the defaults, so old edits read and render as
before (no process version change); the render hash covers them.
History: "Lens Blur on", "Lens Blur amount 60", "Focus range 20",
"Highlight boost 40", "Bokeh Six blades", "Focus set", "Focus on the
nearest", "Mask depth +30". Presets, copy and paste: a part of its own,
"Lens Blur" (on, Amount, range, bokeh, boost; not the focus point, as
Smart Lighting's spot isn't carried), not ticked when copying by default
(a depth effect is the photo's own).

**Open:** HEIC portrait depth; refining on the GPU (the refined map is
re-uploaded per Depth drag: 17 MB at 2560 px, fine so far); focusing on
Vision's subject by default instead of the nearest tenth.

## Photo merges: rule 3 and the result (2026-10-22)

- **Rule 3 amended** (CLAUDE.md, approved): Shoebox may add photo merges
  to a library (an HDR, panorama or focus stack the person makes from
  photos they chose), written beside its sources as a new DNG through
  `fileops`: journaled, verified, undoable (⌘Z sends it to the Trash),
  never replacing a file. A merge reads its sources only: it never
  changes, moves or hides them, and it's refused in reference folders.
- **The result is a photo of its own**, beside its first source (not
  stacked with the sources, not a variant of one); it records what it
  was made from, and the Info panel lists those sources.
- **Floating-point linear DNG** (half or single floats, DNG 1.4's
  floating-point sample format), so HDR keeps values past the sensor's
  white; Shoebox writes it itself rather than through rawler's integer
  writer. Its edit starts from defaults.

## Photo merges: groundwork (2026-10-22)

What HDR, panorama and focus stacking share, in `shoebox_develop::merge`
(the pixels), `shoebox_core::merges` and `FileOps::add_merge` (the
library), and the app's `merge` and `views::merge`.

**Sources** (`merge::load`).

- Raw files only: a NEF (a High efficiency one through its converted DNG,
  as Develop does), a camera DNG, or a linear DNG (Shoebox's exports and
  merges, already linear). A JPEG or TIFF is refused: its tone curve
  makes it non-linear, and HDR needs the sensor's range anyway.
- Each is decoded and demosaiced at the **code defaults** (RCD, inpaint
  opposed, no hot pixels, the camera's black and white), not at the
  edit's raw settings, and **without AI denoise**: a merge is a new raw,
  averaging frames already lowers the noise, and AI denoise on each
  source would add minutes. The as-shot balance is undone, so pixels are
  camera-native linear with the sensor's white at 1.0, as in a linear
  DNG export; then turned upright.
- Pixels the sensor clipped are marked: any photosite at or above 0.98
  of white within two pixels (demosaic spreads them).
- EXIF comes from rawler's reading of the file (exposure time, f-number,
  ISO; and everything copied into the result).
- **Lens corrections aren't applied** before merging, nor baked into the
  result: Develop applies Lensfun to the DNG from its copied EXIF, as
  for any raw. A NEF's own lens data (Nikon's, read from NEFs only) isn't
  carried over. Fine for HDR (frames barely move); panorama should
  correct before warping.

**Alignment** (`merge::align`), pure Rust, no new crates.

- Images are one channel (`Gray`), prepared by the caller; NaN leaves a
  pixel out (clipped, lost in noise). Pyramids halve to 64 px.
- **Features:** Harris corners (structure tensor over 5×5) on the level
  no bigger than 1600 px, the strongest few in each cell of a 16×12 grid
  (so a bright window's edges don't crowd out a dim room), sub-pixel
  peaks. Each is described by 8×8 samples three pixels apart from a
  blurred copy, zero mean and unit length (MOPS without its
  orientation: rotations of a few degrees are fine). Matches are mutual
  nearest neighbours passing Lowe's ratio (0.8); with `max_shift`, only
  within that fraction of the long edge.
- **RANSAC** (deterministic, 99.9% confidence, at most 4000 tries) fits a
  translation, similarity, affine map or homography (normalised linear
  least squares, Hartley's normalisation with one scale for both sides),
  1.5 px at the feature level; refitted to its inliers twice.
- **Direct refinement:** Gauss–Newton on the pixels, coarse to fine from
  the feature level down to full size, on the 40% of the reference's
  pixels with the strongest gradient (thinned to 160 000 a level), with
  a gain and offset for what exposure normalising left, Huber weights
  (scale from the residuals' MAD) for what moved, in normalised
  coordinates; it stops when no corner moves by a thousandth of a pixel.
  It takes the features' answer (about a pixel) to hundredths. If it
  wanders more than 4 feature-level pixels from it, the features' answer
  stands. Without enough matches, frames expected to overlap (`max_shift`)
  are refined from no movement; others fail.
- **Accuracy** (tests, 480×320 synthetic frames with noise): shifts and
  turns recovered to 0.04 px at the worst of a 9×9 grid, rotation to
  0.005°; a 1.5% scale (focus breathing) to under 0.1 px; a panorama pair
  overlapping 40% and turned 0.8° matched by features alone to under
  0.5 px.
- **Warping:** Catmull–Rom, held within the four nearest values so
  clipped edges don't ring; masks take the maximum of the 4×4 they're
  sampled from.

**The result** (`merge::dng`, `encode::float_dng`).

- **Layout:** IFD 0 a 256 px uncompressed sRGB thumbnail (Finder, Quick
  Look), the DNG and colour tags, EXIF and XMP; SubIFD 0 the image:
  LinearRaw, three samples, **16-bit half floats** (SampleFormat 3), in
  256 px tiles, Adobe Deflate (zlib, level 5) with the floating-point
  predictor (TIFF predictor 3: each row's bytes in planes, most
  significant first, then differenced a pixel apart), WhiteLevel 1;
  SubIFD 1 a JPEG preview of at most 1024 px (sRGB, PreviewColorSpace
  2), which the library shows up to that size. DNGBackwardVersion is
  1.4 (DNGVersion reads 1.6: rawler's writer sets it).
- Half rather than single floats: 11 significant bits (finer than any
  sensor's noise at any level), a range to 65504 times white, and
  subnormals reaching 24 stops under it; half the size. Predictor 3
  rather than DNG 1.5's 34894: the older form every float reader takes
  (rawler, ImageIO; tests).
- **Units:** the reference frame's: its sensor white is 1.0, brighter is
  kept. Size, matrices and as-shot white are the reference's.
- **EXIF:** the reference's, copied by rawler's writer (camera, lens,
  exposure, dates, GPS, serial), **without sub-seconds**: a merge is no one
  exposure, and the stacks' capture-time rule needs them, so it never
  joins its sources by time (the catalog keeps merges apart too). The
  orientation is IFD 0's: 1.
- **The record** of what it was made from: in the DNG's XMP
  (`shoebox:Merge`, the kind, and `shoebox:Sources`, a sequence of file
  names and BLAKE3s, in namespace
  `https://github.com/PinkOctopusLabs/shoebox/ns/merge/1.0/`; nothing
  the library reads as marks), and in `<root>/.shoebox/merges.json` in
  every writable root (the file, its kind, each source's path and
  BLAKE3, when). The XMP travels with the file and other tools read it;
  the JSON is what the library reads (Info panel, stacks, duplicates)
  without opening DNGs (rawler's writer puts IFDs at the end, beyond what
  the scan reads for marks). Shoebox's moves rewrite it in the move's
  batch, as for stack links and collections.
- 3 × 24 MP: 87 MB (synthetic noise compresses worse than most photos).

**The library side.**

- `merges::plan`: refuses sources outside the library or in a reference
  folder; hashes each source; the file goes beside the first source as
  `<stem>-HDR.dng` (`-Pano`, `-Focus` reserved), `-1`, `-2` while taken.
  Two merges of the same first photo (`-HDR`, `-HDR-1`) stack together
  as variants by the name rule; left so (open choice).
- `FileOps::add_merge`, one batch of kind `merge`: refused if the file's
  place is taken, it or a source is in a reference root, or a source no
  longer has the content the merge read; journaled, then the DNG copied
  from the cache and verified against its BLAKE3, then `merges.json`.
  Undo sends the DNG to the Trash and restores `merges.json` (emptied if
  it was new).
- **A photo of its own:** `stacks::build_apart` never joins a merge by
  capture time; duplicate detection matches a merge only exactly or by
  image data, never perceptually (it looks like its reference by
  design).
- **Dry run (SPEC safety rule 4):** a merge moves nothing and adds one
  file; the dialog is its table: the sources, then the one file it will
  write and where. No conflict with SPEC; noted here.

**The app.** Merge to HDR… in the photo menu and Ctrl+H (Lightroom's
key; ⌃ isn't in the bundled fonts, so the menu says Ctrl-H). The dialog
checks from the catalog alone (the UI thread reads no files): 2–9
photos, raw, not in a reference folder, not cloud-only, one size and
camera. Merges run one at a time on the develop thread's merge thread
(beside exports, so Develop never waits), with progress and Stop in the
status bar; the DNG is written in `<cache>/merge-staging` and copied in
by the library worker (`LibraryCommand::AddMerge`), which then rescans,
and the new photo is selected (shown in All Photos if the view had it
filtered out). Develop has no merge command of its own: the selection
works there too (Ctrl+H, or the filmstrip's menu).

**For panorama and focus stacking:** `merge::load`, `Merged`, `Record`
and `dng` as they are (any size to 65535 a side); `align::features`,
`matches` (with `near: None`) and `ransac` (`Model::Homography`) for
the pairwise homographies of overlapping frames, and `refine` on each
overlap (frames of different sizes, outside left out as NaN); `warp`
takes a homography only, so a cylindrical or spherical projection needs
its own map (or `warp` generalised to a closure). Focus stacking aligns
with `align::align` and `Model::Similarity` (tested on a 1.5% scale),
the gain and offset absorbing brightness changes. `merges::Kind`,
`plan`, `add_merge`, the record, `merge::Job.kind` and
`LibraryCommand::AddMerge` take the other kinds as they are; the dialog
is HDR's.

## HDR merge (2026-10-22)

`shoebox_develop::merge::hdr`.

- **2 to 9 raw frames** of one camera and size (otherwise refused).
- **Reference: the middle exposure** by EXIF (the darker of the middle
  two for an even count), or one the person picks. The brief said the
  first (reference) source gives size, matrices and as-shot white; the
  file still goes beside the first source and is named after it, but the
  reference is the middle exposure, because the result is in its units:
  a default render then looks like the frame the photographer metered,
  and deghosting compares against the frame best exposed overall.
  Without EXIF exposures the first is the reference.
- **Exposure:** each frame's relative to the reference **measured** from
  the pixels: the median ratio of luminance where both are exposed well
  (not clipped, under 0.85 of white, over 1%), after alignment, from at
  least 2000 pixels. EXIF (time × ISO ÷ f-number²) is the starting point
  (for alignment's normalising) and the fallback; without either, the
  merge is refused. The brief had EXIF first; measured is better because
  EXIF's times are nominal (1/60 for 1/62.5), and a few percent off shows
  as banding where frames hand over. A measured value more than ⅓ stop
  from EXIF is logged.
- **Weights:** each frame's estimate (its value ÷ its exposure) weighted
  by inverse variance under a noise model of shot noise (a sensor holding
  about 50 000 electrons at white at ISO 100) and read noise, scaled by
  ISO: `e² / (a·v + b)`, so longer exposures count for more, by about
  their exposure where shot noise rules and its square in deep shadow.
  One weight per pixel for all three channels (no colour shifts),
  tapering smoothly to nothing between 0.85 and 0.96 of white in the
  brightest channel, and nothing where the sensor clipped. Every frame
  also has a tiny weight growing as the fourth power of its shortness,
  so where every frame clipped the shortest one's (highlight-
  reconstructed) value stands.
- **Alignment:** on by default (hand-held), a homography; off for a
  tripod. The frames are encoded for it as the square root of their
  exposure-normalised luminance, clipped and near-black pixels left
  out, so a −2 and a +2 EV frame match on their mid-tones.
- **Deghosting** (Off, Low, Medium (default), High): each frame against
  the reference in 4×4 blocks both expose well; a block is movement when
  the difference passes `n·σ + t·value` (σ from the noise model; n, t =
  8, 0.25 / 5, 0.12 / 3.5, 0.06), the relative part allowing for
  interpolation and demosaic. Flagged blocks are grown by the feather's
  reach and feathered outward (a box blur of about 1/250 of the long
  edge, at least 2 blocks), so inside a ghost the frame counts for
  nothing and its edge blends. Basic, as decided: where the reference
  clipped there's nothing to compare with, so a branch moving against a
  bright sky can ghost there (MANUAL-CHECKS).
- **Streaming:** the reference and running sums stay in memory; each
  other frame is loaded, aligned, warped, compared and added, then
  dropped, so memory doesn't grow with the frame count.
- **Auto and the display transform:** the edit starts from the defaults
  (as decided, no Auto-tone after merging). The display transform's
  shoulder rolls the extra range off (a window four times white renders
  below white and brighter than the room: test). Auto measures the 99.5th
  percentile and brings exposure down for it, within its ±2 stops (test);
  Highlights does the rest. Linear DNGs keep values above 1.0 through
  the raw stage unless the person moves the raw white point (its levels
  clip at 1.0 then; open choice).
- **Timings,** 3 × 24 MP (6048×4032, a synthetic hand-held bracket from
  `examples/merge_probe.rs`), M2 Max, 12 cores, release: loading a frame
  (decode, demosaic, upright) 0.35–0.5 s; pyramids 0.07 s; alignment
  0.3 s (1278 of 1280 matches agreeing, refined to an rms of 0.006); warp
  0.12–0.16 s; the whole merge 2.25–2.4 s; the DNG 0.34–0.38 s (87 MB);
  so about 3 s from Merge to a DNG in the cache. Peak memory 4.4 GB
  resident (macOS keeps freed pages; it doesn't grow with more frames).
- **Checked by:** `tests/merge.rs` (develop, 9): alignment (three models,
  focus breathing, a panorama pair), a −2/0/+2 bracket of a window-lit
  room (frames moved and turned, noise, clipping, a passer-by in the +2
  frame) merged to within 0.1% of the true light in the room and the
  window and 1% in a dark corner (the tests allow 3% and 6%), the window
  past white recovered from
  −2 EV, exposures measured within 0.1%, the passer-by gone with
  deghosting and there without; a tripod bracket unaligned; Stop; the
  refusals; the float DNG read back by rawler (values past white, colour,
  previews, EXIF without sub-seconds, the XMP record), rendered by
  Develop and Auto, and by ImageIO. Core (`tests/merges.rs`, 4): planned
  beside the first source, journaled, sources byte-identical, a photo of
  its own with its record, never overwriting, a failed verification
  journaled and leaving nothing, refused in and from reference folders
  and for a changed source, undo to the Trash, moves carrying the record,
  no perceptual duplicates. UI (`hdr_merge_from_the_photo_menu`): the
  dialog, the merge selected when done, Info's sources, Develop, ⌘Z.
  Real brackets: MANUAL-CHECKS, "HDR merge".
- **Open choices:** AI denoise of the sources (a dialog option, at the
  cost of minutes); deghosting where the reference clipped (against the
  next shorter frame); a single-float option.

## Focus stacking (2026-10-22)

`shoebox_develop::merge::focus`; Merge to Focus Stack… in the photo menu
(no key: Lightroom has none), the merge dialog generalised by kind.

- **Two or more raw frames** of one camera and size, no most: they're
  streamed. Same exposure assumed; small differences (a flickering light,
  aperture rounding) are divided out by the gain measured against the
  reference (the median ratio of block means, about 96 blocks across, of
  blocks that are neither near black nor near white: blur keeps a block's
  mean). Logged past 0.1 EV.
- **Reference: the middle frame** of those chosen (capture order; the
  earlier of the middle two for an even count). Its colour, EXIF and
  pixel grid become the result's; the chains to it are half as long as
  from the first; the crop is about even on both sides. The file still
  goes beside the first frame and is named after it (`-Focus`). The
  dialog shows which; it can't be changed (unlike HDR's: nothing depends
  on it but the grid).
- **Alignment** (on by default; off for frames that neither move nor
  change size): each frame to its **neighbour towards the reference**,
  `align::align` with `Model::Similarity` (focus breathing scales; a
  hand-held frame shifts and turns), composed along the chain
  (`H_ref→k = H_neighbour→k · H_ref→neighbour`). Neighbours differ least
  in focus, so share the most sharp detail; frames from the two ends of a
  macro stack may share none. Chaining's error grows as a random walk
  (hundredths of a pixel a link). If a link fails the frame takes its
  neighbour's transform and the notice names it. Frames are aligned on
  **linear** luminance, not HDR's square root: with one exposure there's
  nothing to equalise, and a curve makes blurred texture darker on
  average than sharp (Jensen's inequality), which refinement partly
  explains by moving: on the synthetic stack the far frames' error
  halved (0.43 → 0.19 px at the worst corner). What's left there is the
  scene itself: a blurred convex shape looks smaller, by about σ²/2r
  (0.2 px for a 50 px disc blurred by 4.5 px), so frames with different
  blur don't match exactly. Pre-blurring both frames made it worse
  (tried, 1–3 px).
- **Combining,** streamed: each frame is loaded, aligned, warped, added
  and dropped; the sums are fixed in size. Both methods weight each
  frame by the energy of its luminance detail against the **noise
  there**: the HDR noise model (shot and read, by ISO) at the local
  brightness, scaled per level by the band's response to white noise or,
  if smaller, the median the reference shows (demosaic smooths noise; the
  median of a mostly out-of-focus frame is noise). Weights are
  `(1 + max(E/noise − 2.5, 0))^8`, kept as a streamed soft maximum (a
  running mean with weights relative to the largest so far, so no
  overflow and no second pass): where one frame is clearly sharper it
  alone counts; where none has more than 2.5 times the noise's energy
  they **average evenly**, so noise falls by the frame count (8 frames of
  flat grey: 0.36 of one frame's noise; PMax's maximum would keep the
  noisiest). Colour follows luminance's weights (no channel mixing).
  - **Pyramid (default):** Burt and Adelson's Laplacian pyramid (5-tap
    binomial, down to 64 px), each level's coefficients by the weights
    of their energy smoothed over 5×5 at that level, the top level a plain
    mean; collapsed. This is enfuse's contrast weighting with a hard mask
    and Zerene's PMax, between them: per level and per pixel, so fine
    hairs crossing a blurred background keep their own frame at the fine
    levels while the coarse levels blend. **Halo control:** each pixel is
    then held within the least and most its channels were in any frame
    covering it (a coefficient mix can overshoot every frame at a hard
    edge; nothing a frame saw is cut). Costs six floats a pixel.
  - **Depth map:** whole pixels by the weights of a fine band's energy
    (luminance less its pyramid's next level) smoothed by three box
    passes of radius the larger of 2 and 1/1000 of the long edge (σ 2.5
    to 6.5 px), times a wider look (four times the radius, exponent 4)
    so flat areas follow
    the edges near them. Where one frame is clearly sharpest the result
    is its pixels exactly (truest colour and contrast); boundaries blend
    over a few pixels. No depth index is kept: the soft maximum is the
    smoothed choice.
  - Both, against the synthetic truth (below): detail at least 0.95
    (Pyramid) and 0.97 (Depth map) of the sharpest frame's in every block
    away from the depth edge, error to the all-in-focus truth within 9%
    (Pyramid) and 0.5% (Depth map) of the best single frame's per block;
    at the depth edge the mean error in rings 2–12 px either side is under
    0.004 of a 0.25 contrast (the far frames' spill there is ±0.04), so
    no halos. Depth map is the stronger here because the scene's depth
    edge is one clean disc; Pyramid stays the default for what real
    stacks have more of (overlapping fine structure), as in Zerene and
    Helicon; MANUAL-CHECKS compares them on real ones.
- **Edges:** after scale alignment a more magnified frame doesn't cover
  the reference's edges. The result is **cut to the part of the
  reference every frame covers** (each frame's corners mapped back, the
  box inside them all), so every pixel saw every focus distance; a frame
  that falls short is otherwise filled out to its edges by its nearest
  pixels and left out there (weight zero, coverage shrunk per pyramid
  level). With ±1.35% breathing over 10 frames the cut is 53 and 26 px a
  side of 6048×4032 (0.9%). Keeping the full frame, edges from fewer
  frames, is the open choice.
- **The result:** a half-float linear DNG as HDR's (`merge::dng`), the
  cut size, the reference's colour and EXIF (no sub-seconds), record kind
  `focus`, `<first>-Focus.dng` beside the first frame; `merges::Kind`
  gains a default (HDR) for the dialog's state. The app's job carries
  `focus::Options` beside HDR's; the status bar says "Stacking focus".
- **Timings,** 10 × 24 MP (6048×4032, a synthetic stack from
  `examples/focus_probe.rs`: near to far, 0.3% breathing a frame, blur up
  to 3 px), M2 Max, 12 cores, release, the machine otherwise busy (load
  ~25): load 0.34–0.44 s a frame, alignment 0.25–0.3 s (about 1300 of
  1350 matches agreeing, refined to an rms of 0.004), warp 0.12 s, the
  combining about 0.1 s; the whole stack 7.7–8.6 s (Pyramid), 7.4–9.6 s
  (Depth map), so about 0.8 s a frame; 20 frames 15.8 s; the DNG 0.4 s
  (93 MB). **Memory** doesn't grow with the frame count: at most two
  full-size frames at once (one loaded, one warped; `Focus.peak_frames`,
  tested), the sums about 1.2 GB (Pyramid: 5 floats a pixel over the
  levels, 6 for the range) or 0.5 GB (Depth map), three luminance
  pyramids; peak resident 7.4–8.2 GB for 10 frames and 8.2 GB for 20
  (peak footprint 8.2 and 7.9 GB), 5.5 GB for 3; most of it loading
  (decode, demosaic) and freed pages macOS keeps.
- **Checked by:** `tests/focus.rs` (develop, 3) on synthetic stacks
  (`shoebox_fixtures::FocusFrame`: a textured wall slanting from near to
  far and a brighter disc near the camera in front of its far side; each
  frame Gaussian-blurred by its distance from focus, layered so the
  disc's blur spills over the wall as a lens's does; magnified 0.3% a
  frame and shifted; ISO 400 noise; the truth rendered all in focus):
  six frames, both methods: breathing recovered to 0.1% in scale and
  0.25 px at the worst corner, gains within 0.5%, the cut, detail and
  truth as above, halos, two frames held; a rail stack without alignment
  (full size, one frame held, the DNG read back with its record); the
  refusals and Stop. Unit tests (4): one frame back exactly, flat noise
  averaged, smoothing, coverage. Core (`tests/merges.rs`): a focus stack
  planned as `-Focus`, beside an HDR of the same photos, journaled, its
  record's kind, never overwriting, refused from a reference folder,
  undone to the Trash. UI (`focus_stack_from_the_photo_menu`): the menu
  item, the dialog (reference, Method, Align), the stack selected, Info's
  sources, its cut size, ⌘Z. Real stacks: MANUAL-CHECKS, "Focus
  stacking".
- **Open choices:** keeping the full frame (edges from fewer frames)
  instead of cutting; choosing the reference; a depth index map kept for
  retouching from one frame (Zerene's), which needs a second pass; the
  noise floor (2.5) and steepness (8) on real stacks; half-float range
  bounds (a third less of the Pyramid's memory) if 45 MP stacks press on
  16 GB Macs.

## Panorama (2026-10-22)

`shoebox_develop::merge::pano`, after Brown and Lowe, "Automatic
Panoramic Image Stitching using Invariant Features" (2007), with Hugin's
and OpenCV's stitching pipelines as the reference for each stage. No code
is ported (the methods are the papers'); no new crates.

- **2 to 60 raw frames** of one camera and sensor size (portrait and
  landscape frames may mix); otherwise refused. The file goes beside the
  first as `<stem>-Pano.dng`.
- **Lens corrections first** (option, on): each frame's `lens` grid (the
  NEF's own data, then Lensfun: distortion, vignetting, lateral CA), read
  in the frame's upright pixels (`pano::optics`). Not a separate pre-warp:
  the grid is folded into the one resample that projects the frame (output
  pixel → direction → ideal frame pixel → the grid → where red, green and
  blue are), so frames are resampled once; the previews matched and
  measured on are corrected the same way. Without a profile the frame is
  used as it is (a notice says so when no frame had one).
- **The result's EXIF** is the reference frame's without lens or focal
  length (a panorama has neither), so Develop never corrects its lens a
  second time.
- **Matching:** Harris features (`align::features`, 2000 a frame on a
  ≤1600 px corrected copy, square-root encoded so a gain stays a gain) of
  **every pair**, ratio-tested matches, a RANSAC homography, kept when
  more than 8 + 0.3 × the features in the overlap agree (Brown and Lowe's
  test) and at least 12. All pairs rather than neighbours in capture order:
  multi-row and 360° sets aren't shot in a neat order, and it costs about
  20 ms a pair (6 frames 0.35 s; 30 frames, 435 pairs, about 9 s). Each
  kept pair's homography is then **refined on the pixels** (`align::refine`
  on the ≤1024 px previews, from their quarter-size level) and its matches
  taken from it on a 25 × 17 grid over the overlap: with the features'
  matches alone frames came out up to 0.054° off; refined, at most 0.031° (0.011° for two rows without a lens).
- **A frame that overlaps none** (not reached from the best-connected
  frame) is refused by name ("DSC_0102.dng doesn't overlap the other
  photos"); nothing is written. Brown and Lowe would make several
  panoramas; one per merge is clearer here.
- **A camera turning about one point:** a rotation per frame and **one
  focal length** (one lens and zoom; per-frame focal lengths would let a
  zoom change between frames, at the cost of conditioning), the principal
  point the frame's centre (lens corrections put it there). Started from
  EXIF and the crop factor (`volume::focal_px`), or without a focal length
  in EXIF from the homographies (OpenCV's `focalsFromHomography`, median);
  rotations from a maximum spanning tree of the links by agreeing points
  (each relative rotation the nearest to `K⁻¹·H·K`). Then **bundle
  adjustment**: Levenberg–Marquardt on every match's transfer error both
  ways, Huber weights (scale from the residuals' MAD), numerical
  derivatives per match (it touches two frames and the focal length), the
  reference frame held still; then matches beyond 4 robust σ dropped and
  adjusted again.
- **The reference** (the result's units, colour, EXIF): the frame with the
  most links, the middle of the capture order breaking ties, so a row's
  middle frame. The brief's first-source rule is kept for where the file
  goes and its name.
- **Laid flat:** when the turning camera leaves more than about the
  features' own error, the frames are also fitted as homographies onto
  the reference's plane (the same adjustment, eight parameters a frame in
  normalised coordinates); if that fits much better (median error under
  0.6 of the camera's) the panorama is drawn flat, in the reference's
  pixels (Perspective; a notice says so). Scans, murals, a poster walked
  along. Rows of a few frames fit both alike and stay turning.
- **Straightening** (Brown and Lowe's wave correction): up is the
  eigenvector of the frames' x axes' scatter with the smallest eigenvalue
  (a level row's x axes lie in the horizontal plane), down agreeing with
  the frames' own; for a single column (x axes all alike) the frames' mean
  up. The panorama's centre is the middle of the longitudes covered (the
  largest gap behind); all round (no gap over 10°), the frames' mean view.
- **Exposure:** a gain per frame (the reference's 1), from the median
  ratio of corrected preview luminance over each link's overlap (well
  exposed, not clipped, both sides), solved together by least squares on
  their logarithms, held very lightly to EXIF's differences (so a frame
  the overlaps can't measure takes EXIF's). Global per frame, as asked;
  vignetting is the lens profile's job.
- **Projections** (`pano::project`): Spherical (equirectangular, to
  360° × 180°, a pole inside a frame reached), Cylindrical (heights beyond
  ±75° left out), Perspective (refused beyond 120° across or all round:
  "choose Cylindrical or Spherical"). One focal length of pixels per
  radian (or unit) keeps the frames' own resolution at the centre. **Auto:**
  Perspective up to 90° each way (within ±60° of the horizon),
  Cylindrical up to 120° high (within ±75°), else Spherical. Each output
  pixel's inverse map is evaluated directly per projection (`align::warp`
  is left as it is).
- **Seams** (`pano::seam`), on a canvas of about a megapixel: first
  centre-weighted (each pixel to the frame it's furthest inside, by the
  product of its distances from the frame's edges: a Voronoi diagram of
  the centres, as Enblend starts from), preferring pixels a frame covers
  well (one canvas pixel in from its edge, so a seam never sits where a
  frame runs out); then for each overlapping pair a **graph cut** (Kwatra
  et al. 2003, as OpenCV's `GraphCutSeamFinder`) over the pixels both
  cover well: edge costs where the two differ (square-root, exposure
  compensated, relative to the scene's typical level) plus a little for
  length, a smaller bias to step one's choice; Dinic's max-flow,
  iterative. A seam runs where frames agree, so it goes round a person who
  moved rather than through them (test: without the cut 36% of a person
  astride the seam shows; with it, all of them).
- **Blending** (`pano::blend`): Burt and Adelson's multi-band blend in
  linear camera space: each frame's Laplacian pyramid weighted by the
  Gaussian pyramid of its seam mask (from the canvas labels, bilinear,
  cut at ½), levels normalised by their summed weights, collapsed; bands
  for a blend about 5% of the result's size (OpenCV's default), at most 7.
  Before its pyramid each frame's box is filled beyond its edge
  (push–pull), so no frame's edge rings into a seam.
- **Streaming:** frames are read twice: once for previews and features
  (then dropped), once to blend, each warped into its own box of the
  result (aligned to the coarsest level), added to the accumulators and
  dropped. Not tiled further: the accumulators hold the whole result
  (about 21 bytes a pixel: three channels and a weight, a third more for
  the coarser levels; the collapse expands in place, row by row), and the
  float DNG writer takes the whole image. Hence **a cap of 150 MP** (and
  65 000 px a side): a larger panorama is drawn smaller to fit, and the
  notice says at what scale. Tiling the blend (and the writer) would lift
  it; open choice.
- **Edges:** cropped by default to the largest rectangle inside what's
  covered (the histogram method on the canvas, one canvas pixel in); off
  keeps the whole boundary, empty pixels 0 (black): LinearRaw has no
  alpha. DNG's DefaultCrop could carry the inner rectangle over the full
  boundary, and "Fill edges" could reuse object removal's MI-GAN later
  (open choices). A 360° panorama's left and right edges continue each
  other but aren't blended across.
- **The dialog** is HDR's, generalised by kind (`views::merge`: the
  title, the refusal's counts, a kind's options in a function of its own),
  so focus stacking adds its options the same way; `merge::Job` gained a
  `pano` field beside HDR's `options`.
- **Timings,** 6 × 24 MP (6048×4032, a synthetic hand-held row through
  the Nikkor Z 40 mm's distortion and vignetting, exposures ±15%,
  `examples/pano_probe.rs`), M2 Max, 12 cores, release, files cached:
  reading the frames (decode, demosaic, previews, features) 1.7 s;
  matching and refining all 15 pairs 0.35 s; adjustment, straightening,
  gains under 0.05 s; seams 0.6 s; blending (each frame read again,
  warped, its pyramid added) 0.75 s a frame; collapse 0.25 s: **about
  7.4 s** for an 81 MP result (23407×3465, Cylindrical; 13.8 s with the
  files read cold); the DNG 1.0 s (238 MB). Focal length found within
  0.02% of the truth (EXIF was 2% off), gains exact to 4 digits, transfer
  error median 0.01 px. **Memory:** peak resident 5.2 GB while blending,
  5.6 GB writing the DNG (the accumulators 1.7 GB, one frame's decode and
  box about 1.5 GB at once; macOS keeps freed pages, so resident climbs
  over the frames).
- **Checked by:** `tests/panorama.rs` (develop, 7): a five-frame hand-held
  row (600×400 frames, 47° each, turned by up to ±2° pitch and roll, the
  Nikkor Z 40 mm's distortion and strong vignetting, gains 0.85–1.25,
  noise): rotations within 0.05° (found at most 0.031°), focal length within 0.5%
  (0.06%; EXIF 3% off), gains within 1%, the scene re-projected (32-pixel
  blocks: median within 1%, 95% within 1%; full-height strips, where a
  seam, an exposure or vignetting would show, within 0.6% and no step over
  0.6% between neighbours; found 0.2–0.25%), no empty pixel after the
  crop, the DNG read back (linear, its size, no lens or focal length, the
  XMP record); a frame overlapping nothing named; too few; Perspective
  refused at about 137° and drawn at 77°; Spherical uncropped keeps black
  corners; a wall shot from three places laid flat and matching the wall;
  a person astride a seam whole; two rows of three registered within
  0.05° and straightened to within 0.1°; Stop. Units in the modules
  (geometry, optics' orientations, the cut and the rectangle, the blend
  giving one frame back exactly and two without a step, the fill). Core
  (`tests/merges.rs`): a panorama planned as `-Pano` beside its first
  source, journaled, its record's kind, a photo of its own, `-Pano-1`
  next, undone to the Trash. UI (`panorama_from_the_photo_menu`): Ctrl+M,
  the dialog's options, the result selected and wider than a frame,
  Info's sources, Develop, ⌘Z. Real sets: MANUAL-CHECKS, "Panorama".
- **Open choices:** a layout preview in the dialog (it needs every frame
  decoded and matched, about 0.3 s a frame on the merge thread, so not
  "cheap"; the embedded JPEGs could do it faster, without lens
  corrections); a shared radial distortion term in the adjustment for
  lenses no profile knows (Hugin's a, b, c), and vignetting estimated
  from the overlaps (Goldman); per-frame focal lengths; candidate pairs
  by a coarse match first for sets of 30 or more; seams at more than a
  megapixel for very large panoramas; tiling the blend and the DNG writer
  to lift the 150 MP cap; DNG DefaultCrop for the uncropped boundary; Fill
  edges by MI-GAN; blending across a 360°'s back edge; several panoramas
  from one selection (Brown and Lowe's connected components).

## iCloud Photos: the core (2026-10-22)
`docs/PHOTOS-SPEC.md` accepted (its open questions answered 2026-10-20):
originals at full quality, favourites ★5, Photos' edits as `-photos`
variants, albums as events, hidden left out, the card import's Into root
and Names template, pushes as rendered files only, videos copied. Phases 1
and 2 built in `shoebox_core::photos`, against a fake library; PhotoKit
(`shoebox-photos`, Phase 3) and the UI are next.
- **The sync record is a folder, not one file.** `.shoebox/photos/` gets a
  new `synced-<run>-<n>.json` per sync step, never rewritten; the newest
  record of an asset wins; `choices.json` (album choices, change token) is
  rewritten each run. One `photos.json` rewritten after each step would have
  put a growing multi-megabyte file into the journal twice per step (a
  30k-asset first sync: gigabytes). Undo empties a sync's own files
  (fileops leaves `{}`), so earlier records come back by themselves.
- **One batch per sync, in steps.** `FileOps::begin_import`,
  `import_step` (checked and run like `import_until`, appended to the
  batch), `end_import`. A sync quit half-way is a running batch, which crash
  recovery offers to resume or roll back like any other. Each step's copies
  and its records go together, so a stop loses at most a step; its files
  are then found by hash.
- **Keyed by `PHCloudIdentifier`**, else `local:<localIdentifier>`; also
  looked up by local id, so an asset recorded before it reached iCloud
  isn't copied again when it gets a cloud id.
- **Dates.** The file's EXIF date stands when it's the same moment as
  Photos' creation date (within a second; without an EXIF offset, any whole
  number of quarter hours up to 14 hours, as the camera's clock may have
  been in another zone). Otherwise, or with no EXIF date, Photos' date names
  the file (in the EXIF offset's zone if there is one, else the Mac's) and
  goes into the copy's new sidecar as `exif:DateTimeOriginal` and
  `photoshop:DateCreated` (ISO 8601). Sidecar dates (never the image's own
  XMP) now win over EXIF when the catalog reads a photo
  (`sidecar::SidecarDate`); both properties joined `xmp::Prop`, so undoing
  a sidecar write strips them too. Photos' location goes into the sidecar
  (as `exif:GPSLatitude`/`GPSLongitude`, which the map already reads) when
  the file has none.
- **Names.** One frame per asset: the original, a RAW+JPEG pair's other
  half and a Live Photo's movie share the stem and one suffix, named from
  the original (as card imports name a Live Photo's movie). Photos' edit
  is `<stem>-photos.<ext>` beside it; a newer edit later is
  `-photos-1`, `-photos-2`, beside the original wherever an earlier sync
  put it (in that root). Files whose bytes the library has are recorded as
  `in_library` with the library path and not copied; an asset with none
  copied is `in_library`.
- **Albums** default to events (nested by Photos' folders, joining existing
  events of the same name), except names that say nothing (`import::
  says_nothing`: dates, `Photos`). Assets synced before that join albums, or
  are favourited, get the marks in their existing sidecars (never a
  reference root's; ★5 only when unrated) without downloading; leaving an
  album is only counted.
- **Updated Photos edits** are offered when the asset's modified time is
  later than the record's `edit_modified`; a favourite also changes it, so
  the render is downloaded and dropped if its hash is already recorded.
- **Downloads** go to a cache folder (`staging`), are hashed and read
  there, then copied in through `fileops`; the staged files are removed
  after each step. Sequential for now; four at once is the app's job
  (Phase 3).
- **Tests** (`tests/photos.rs`): the first sync's names, ★5, the album's
  event, the screenshot dated and placed by Photos and read back so, one
  batch; syncing again (also after a move in Finder, also into another
  root) downloads and copies nothing; a joined album and a newer Photos
  edit; a failed download tried again next time; undo; the record deleted
  (everything found in the library, nothing copied). Units: the ledger's
  merge across roots and undo, the date rules, GPS text read back.

## iCloud Photos: PhotoKit (2026-10-23)
PHOTOS-SPEC Phase 3: `shoebox-photos` (`PhotoKitSource` on `objc2-photos`,
Zlib/Apache-2.0/MIT), the app's Photos thread, the bundle's permission.
Nothing here has run against a Photos library yet: the probe waits for a
test account (MANUAL-CHECKS 11); until then this is the code's design.
- **Never asked without a bundle.** macOS ends a process that asks for
  Photos without `NSPhotoLibraryUsageDescription`, so `PhotoKitSource`
  asks only when the main bundle has it (`can_ask`); `cargo run` and the
  tests never ask, and the tests only read their own Info.plist. The
  bundle script adds the usage string, and for Developer ID (hardened
  runtime) builds `assets/Shoebox.entitlements`
  (`com.apple.security.personal-information.photos-library`).
- **The probe is in the app**, `Shoebox.app/Contents/MacOS/Shoebox
  --photos-probe <empty folder> [--keep]`, not an example binary: TCC
  grants Photos to a signed bundle, and the probe should see what the app
  sees. It refuses a library without an album named `Shoebox test library`,
  checked by listing album titles only before anything else is read. Besides
  the facts it syncs into `<folder>/library` (its own journal), plans again
  and undoes the sync (kept with `--keep`).
- **Listing.** Every asset (`includeHiddenAssets`, `includeAllBurstAssets`,
  so Shoebox's options decide), in autorelease pools of 500; resources by
  `PHAssetResourceType` (photo and video → original, alternatePhoto,
  pairedVideo, fullSizePhoto and fullSizeVideo → edited; adjustment data
  and the rest aren't copied); regular albums only (not smart or shared),
  with their folders from `fetchCollectionListsContainingCollection`; cloud
  ids from `cloudIdentifierMappingsForLocalIdentifiers` in batches of 1000.
- **Who edited** comes from `PHAsset.adjustmentFormatIdentifier` (public),
  so Shoebox's own pushed edits (format `com.crankyunicorn.shoebox`, the
  bundle id) are known without reading adjustment data.
- **No file sizes.** PhotoKit has no public size (only an undocumented KVC
  key that raises if missing), so the plan's sizes are unknown and the sync
  checks free space instead: before each step, at least 10 GB on the
  library's and the cache's volumes (`fileops::free_bytes`, statvfs), else
  it stops cleanly (`Report::low_space`).
- **Downloads** stream with `requestDataForAssetResource` (network allowed)
  into a new file in the staging folder, so a stop cancels the request
  (`cancelDataRequest`) and the partial file is removed; four at once
  (`sync::Job::parallel`, scoped threads per step, results placed in
  order); steps of 20 assets.
- **The library stays usable.** The sync runs on the Photos thread; each
  step goes to the library worker (`LibraryCommand::PhotosBatch`: begin,
  step, end, each answered), which owns `FileOps`, so other commands
  interleave with a sync's steps. `sync::Batches` abstracts it (tests use
  `FileOps` directly). The undo stack gets the batch when it ends (or a
  step fails).
- **Keep awake** with `NSProcessInfo` user-initiated activity
  (`platform::keep_awake`) for the run.
- **Not yet:** the change token (every sync lists everything; metadata
  only, but per-asset resource lists for 30k assets may take a minute:
  timed by the probe), and the Import screen (Phase 4).
- **Tests:** core `a_stop_or_a_full_disk_copies_nothing_more` (stopped
  from the start, and a free-space floor no disk meets: nothing copied,
  both batches closed); app `photos::syncs_through_the_library_worker`
  (FakeSource through the real library worker: one batch, done, nothing
  new on a second plan); `shoebox-photos` units (kinds, dates, never
  asking unbundled).

## iCloud Photos: the Import screen (2026-10-23)
PHOTOS-SPEC Phase 4, against the fake library (`views/icloud.rs`).
- **Import has two sources**, "A card or folder" and "iCloud Photos", above
  the form; switching is off while either is busy. iCloud Photos shares
  the card import's Into root and Names template (as decided 2026-10-20).
  Find photos asks for access (a note for Denied, pointing at System
  Settings, and for Limited), lists Photos and shows: photos to copy, then
  copied before, edited again in Photos, added to albums or favourited
  since, hidden and untouched burst frames left out, and how many left
  albums (nothing changes for those). The album table edits each album's
  name and Event / Keyword / Nothing; the choices go with the run and are
  remembered for next time. The button says "Bring photos up to date" when
  only marks or newer edits are left.
- **No previews before copying:** nothing is downloaded until the person
  confirms, so the plan is counts and albums, not thumbnails.
- **Progress** on the screen (assets done of all, bytes downloaded, speed,
  time left, the file) and in the status bar; Stop; quitting asks first,
  as for a card import, and Stop and Quit waits for the step in flight.
  The report lists copies, files already in the library, Photos' dates,
  marks given, why it stopped, and failures with Try again.
- **Settings › iCloud Photos** holds the sync options (`Settings.photos`,
  `plan::Options` now serde): Live Photos' movies, Photos' edits, every
  burst frame, hidden photos, and what a favourite becomes (★5 by default,
  ★4, ★3, a keyword, a pick, nothing).
- **Info panel:** "From iCloud Photos" (or "Photos' edit, from iCloud
  Photos") with when it was synced; the library worker answers from the
  records, read once and dropped when a command could change them.
- **Not built:** Re-import missing (needs every library file hashed to tell
  moved from gone) and Rebuild sync record (a sync after the records are
  lost already finds every file by its bytes and records it, copying
  nothing that's there; a button that downloads without copying would only
  save the copy step). Both in OPEN-ITEMS.
- **Test** (`import_from_icloud_photos`): plan with the hidden one left
  out, an album made a keyword, nothing downloaded before confirming, the
  copies named by the template with ★5 and the album keyword, the Info
  row, nothing new the second time, Settings' tab, undo from History.

## iCloud Photos: Send to Photos (2026-10-23)
PHOTOS-SPEC Phase 5 (`photos::push`, `views/push.rs`), against the fake
library; PhotoKit's writes (`shoebox-photos/src/sys.rs`: create, edit,
revert, delete, albums) wait for a test account.
- **Send to Photos…** in the photo menu, for the selection (each tile's
  edit, a virtual copy's too), and **Send Again** in the Info panel. The
  dialog's table says what each photo becomes: a new photo; an edit of its
  photo in Photos (warning when that replaces an edit made in Photos, and
  naming the library's `-photos` copy of it, or saying it'll be lost from
  Photos); an update of an earlier send; or not sent (up to date, already
  in Photos and not edited here, a movie). Not a destination in the export
  dialog: one dialog with its own table reads more clearly (OPEN-ITEMS).
- **Files are exports.** New photos use a chosen JPEG, PNG or TIFF preset
  or Shoebox's own "Photos (JPEG, full size)" (quality 92, 4:4:4, Display
  P3, all metadata); edits of an asset in Photos are always that JPEG
  (PhotoKit's rule). They render as an export job with a `tag`, into
  `<cache>/photos-staging/push/<tag>/`, named as the photo; the export's
  ExportDone goes to the send, not the export notices; the Photos thread
  sends them, then removes them.
- **Records:** `.shoebox/photos/pushed-<run>.json` in the photo's root
  (asset key → local id, the photo relative to the root, copy, created or
  edited, the edit's preview key, when, batch), written in a batch of kind
  `photos-push` (`FileOps::begin_push`). Newest record per asset wins. A
  sync leaves out assets Shoebox created (`Plan::sent`); edits Shoebox made
  are Shoebox's by their adjustment format, so they're never "edited again
  in Photos".
- **Undo** (⌘Z or History) of a `photos-push` batch goes to the Photos
  thread first (`LibraryEvent::UndoPush`): it deletes the assets Shoebox
  made (to Recently Deleted) and reverts the edits to the original, then
  the library worker undoes the batch (`UndoPushed`; never redone: sending
  again does that). Refused, changing nothing, if an asset was edited in
  Photos more than a minute after the send. Reverting drops every edit, so
  undoing an update goes back to the original, not to the earlier send;
  album membership stays.
- **Development guard:** debug builds' `PhotoKitSource` writes only to a
  library with an album named `Shoebox test library`; release builds write
  as the person asks.
- **Not built:** the `shoebox:PushedFrom` marker in sent files (the
  records are the guard; the marker would cover lost records), the grid
  badge for photos edited since they were sent (the Info row says "edited
  since"), and an edit made in Photos to a photo Shoebox created is never
  imported back. OPEN-ITEMS.
- **Tests:** core `sending_to_photos` (each action, the send, up to date
  and update, a sync afterwards bringing nothing back, undo in Photos then
  the records) and `undoing_a_send_is_refused_after_a_change_in_photos`;
  UI `send_to_photos` (the dialog's rows, a rendered JPEG becoming a new
  asset, the Info row, up to date, ⌘Z taking it back out of Photos).

## iCloud Photos: listing progress (2026-10-23)
First test (a 25–30k photo library): Find photos sat on a spinner for
minutes. Listing reads every asset's details and files, its albums, then
its iCloud ids, and said nothing until done. `PhotoSource::fetch_with`
now reports each stage (`photos::Listing`: photos, albums, iCloud ids,
done of total; photos in chunks of 100) and stops when asked
(`CoreError::Cancelled`); `fetch` stays for callers that don't watch. The
Import screen shows the stage with a progress bar ("Reading your photos:
12,000 of 28,431") and Stop; the log times each stage
(`photos: listed N assets in S s`), to find which one to make faster.

## iCloud Photos: listing without files (2026-10-23)
The first real try sat on "Listing your Photos library…" for over 30
minutes (25k photos). The listing asked PhotoKit for every asset's files
(`assetResourcesForAsset`, one query an asset). Now it reads only each
asset's own properties; whether it's a Live Photo comes from its media
subtypes (`PhotoLive`), whether it has an edit from `hasAdjustments` and
its adjustment format. Files are looked up per asset just before it
downloads (`PhotoSource::resources`); a RAW+JPEG pair's other half is asked
for and skipped if it isn't there. A sync after the first never looks up
files for assets it has already. The probe times 200 lookups.
`FakeSource::list_without_files` lists the same way;
`listing_without_files_syncs_the_same` checks a sync gives the same
library either way. The iCloud ids stage (`cloudIdentifierMappings`) is
next if it's slow: the stage timings in the log will say.

## iCloud Photos: events follow their albums (2026-10-23)
Photos' albums hold references, as Shoebox's events do (a keyword on each
photo; a photo can be in several). Asked for: keep album events in step
with Photos, one way (Photos → Shoebox).
- Each sync compares a synced photo's albums with those recorded last
  time: albums joined add the event (as before), albums left take the
  photo out of the event (`Plan::departed`, `events::leave`); keyword
  albums lose the keyword. Only photos that came from Photos are touched,
  so photos added to an event in Shoebox stay, and only Photos' changes
  since the last sync apply, so a photo taken out of an event in Shoebox
  isn't put back.
- `choices.json` remembers each album's name in Photos and the event it
  made (by id). An album renamed or moved in Photos renames or moves its
  event and the members' keywords (`events::mark_updates`), unless the
  event was renamed in Shoebox since (provisional: see OPEN-ITEMS); joins
  follow the link, so they go to that event, not a new one of the album's
  name. An album deleted in Photos leaves its event and members alone and
  is forgotten.
- All in the sync's first step (one write a photo), in its batch: ⌘Z takes
  it back. The Import screen says what moves before Import ("3 photos
  taken out of Trips › Japan in Photos: they leave its event here", the
  renames, the deletions).
- Test: `events_follow_their_albums` (leave, join, rename, a library
  photo of the person's own kept and renamed, a rename in Shoebox kept and
  joined, an album deleted); `a_photo_in_several_albums_is_one_copy_in_several_events`.

## Cloud-only photos: waiting out the drive (2026-10-23)
Opening NEFs in a Google Drive (streaming) library failed with
"Operation timed out (os error 60)": reading a placeholder makes the
drive fetch it, and Google Drive answers a read it can't serve in time
with `ETIMEDOUT` while it goes on fetching. The loupe and Develop decoded
straight from the placeholder, so the decoder's read failed. Now anything
about to read a photo that's in the cloud only (the loader for a full
view or a thumbnail, Develop's prepare, so exports too) downloads it first
with `cloud::download`, which reads through and, on `ETIMEDOUT` (or an
interrupted read), waits 2 s and reads on from where it got to, for up to
10 minutes (`DOWNLOAD_PATIENCE`). The real drive's behaviour isn't
reproducible in tests (no placeholder there); MANUAL-CHECKS 1 covers it.

## iCloud Photos: a nil file name (2026-10-23)
The first real import stopped at 20,180 of 29,649: `-[PHAssetResource
originalFilename]` returned nil for some assets, though its header says it
never does, and objc2-photos' generated binding panics on that. The panic
in a download thread ended the Photos thread, leaving the screen waiting
and the batch running (crash recovery's Resume closes it; everything
copied had its records written with it). Now the name and type are read
with `msg_send!` as optional, a missing name is made from the type
(`unnamed.heic`, …; files are named by the template anyway); a panic in one
asset's download fails that asset (Try again) and the sync goes on
(`a_panic_fails_one_asset_not_the_sync`); and a panic anywhere in a run
ends that run with a message, not the thread.
- Then: the next sync failed 10 assets with "File exists": the crashed
  run's half-downloaded files were still in the staging folder under the
  same names (`<asset index>-<kind>.<ext>`). Each sync now downloads into
  `photos-staging/sync-<run>/`, removed when it ends, and starts by
  clearing loose files and other `sync-*` folders there (the cache's own;
  Send to Photos' `push` folder is left alone)
  (`leftovers_in_the_cache_dont_block_the_next_sync`).

## Cloud-only photos download when clicked (2026-10-23)
After an iCloud Photos import into a Google Drive (streaming) library,
Drive moved copies off the Mac before Shoebox made their thumbnails, so
they showed as grey cloud tiles. Asked for: clicking one downloads it.
Now the current photo, if it's in the cloud only, is downloaded once
(`ShoeboxApp::fetch_current`: that file, through `LibraryCommand::Download`,
progress in the status bar), then read like any other, thumbnail and
details included. Only the clicked file: a selection (⌘A) downloads
nothing; Download from Cloud in the menu still does a selection's whole
stacks. SPEC "Cloud drives" updated. Test: `cloud_only_photos`.

## All Photos opens at the newest (2026-10-23)
Asked for. Grid and Masonry jump to the latest end of All Photos (the
bottom, sorted by capture time oldest first; the top newest first; not
for other sorts or a narrowed view) on start and when All photos is
chosen (`scroll_to_latest`, `ShoeboxApp::latest_jump`). Show in All
Photos asks for its photo after clearing the scope, so it wins. Test:
`all_photos_opens_at_the_newest`; `show_in_all_photos_scrolls_to_it` still
passes.

## Interrupted batches with nothing left to run close themselves (2026-10-23)
The crashed iCloud Photos import left its batch "running" with every step
done (the Photos thread died between steps), so History kept asking to
Resume or Roll back, and Roll back would have sent ~28,000 copies to the
Trash. At start the library worker now closes interrupted batches with no
step pending (`FileOps::close_finished_interrupted`; not interrupted
undos); ones with a step still to run are asked about as before (SPEC
safety rule 8). Test: `interrupted_batches_with_nothing_left_are_closed`.

## iCloud Photos: the sync record as logs (2026-10-23)
The record's one file per step (20 assets) had made 1,485 files (16.5 MB)
in a real library's `.shoebox/photos` after two syncs. On a cloud drive
each is an item to upload, any of them can be moved off the Mac, and the
next sync's plan reads them all; and they only ever grew. Supersedes "the
sync record is a folder, not one file" in "iCloud Photos: the core".
- **Logs, appended to.** Each step appends its records (a compact JSON line
  an asset: `key` plus the entry's fields) to `synced-<run>-<n>.jsonl`;
  a step that would take a log past 1 MB (`ledger::ROLL_AT`) starts the
  next (`ledger::Segment`, kept per root by the sync). The two syncs above
  would be about 17 files; a 30k first sync, about 16. During a sync a
  cloud drive uploads the newest log again as it grows, up to 1 MB at a
  time; rolling bounds that.
- **fileops: append and truncate.** A write to a `.jsonl` in `.shoebox`
  is an append (`planned_write` → `Planned::Append`), never a rewrite. The
  journal row (`append`) holds where it starts (`size`), the bytes
  (`payload`) and their BLAKE3; nothing of what was there. It runs only if
  the file is still that long, and syncs before returning. Undo is a
  `truncate` row (`Planned::Truncate`: back to that length, only if the
  file ends with exactly those bytes; it keeps them, so redo, an undo of
  the undo, appends them again). Undo checks each log's length step by
  step, so a batch's many appends to one log undo newest first. Resume
  after a crash: an append fully there is marked done; a part of one (ours:
  a prefix of its bytes) is cut off and written again. Nothing is deleted:
  undoing a log's first append leaves it empty, as an undone `.json` is
  left `{}`.
- **Old files.** `synced-*.json` step files are read as before, with the
  logs; the newest record wins across both. Nothing is converted.
- Push records (`pushed-<run>.json`, one per send) and `choices.json` are
  unchanged.
- Tests: `app_logs_are_appended_undone_and_redone`,
  `an_app_log_changed_since_is_not_undone`, `an_interrupted_append_resumes`
  (fileops); `old_step_files_are_read_with_the_logs`,
  `logs_roll_over_and_skip_a_cut_line` (ledger);
  `first_sync_names_marks_and_records` checks one log for a three-step sync.
