# Shoebox — Photo Library App Spec

Oct 16, 2026

## Overview

Shoebox is a fast, local-first macOS app, written in Rust with egui, for browsing, culling and organising a folder-based photo library, with dupeGuru-style duplicate detection built in. Files on disk stay the source of truth: the app reads and writes standard XMP sidecars, so DxO PhotoLab and digiKam see the same ratings and keywords.

**Goals**

- Browse large folder trees of NEF, ARW, DNG, JPEG, TIFF, PNG, HEIC, AVIF, WebP and PSD files (and MOV, MP4 and MPG movies) instantly, with Lightroom-style grid, loupe, compare and survey views.
- Cull and organise with ratings, pick/reject flags, colour labels and keywords, written to XMP sidecars.
- Treat a NEF and its JPEG and TIFF siblings as one stack.
- Find exact and visually similar duplicates across folders, including against the previews embedded in NEFs, and review them side by side.
- Import and rename files into a dated folder structure.
- Never lose a file: no deletes, and every file operation is journaled and undoable.

**Non-goals for v1**

- RAW development or pixel editing. DxO PhotoLab does this.
- A proprietary catalog. The SQLite database is a cache that can be rebuilt from files and sidecars at any time.
- Cloud sync, sharing, multi-user access, or library roots on network shares. (Copying from iCloud Photos and sending photos back to it, by hand, is covered by `docs/PHOTOS-SPEC.md`: an import plus an explicit push, not two-way sync.) (A folder a cloud drive keeps on the Mac, such as Google Drive's, can be a root; files that are in the cloud only are handled as below.)
- Video playback beyond a poster frame.
- Windows and Linux builds. Keep the code portable, but only ship and test on macOS.

## Context

The app is built for one photographer's library first, and its defaults come from that library.

| Item | Detail |
| --- | --- |
| Camera | Nikon Z f, shooting RAW + JPEG, so most frames have a NEF and a JPG with the same capture time |
| File types | NEF masters; camera JPGs; TIFF exports from Lightroom (mostly 2560 px long edge, including virtual-copy variants); JPEG previews carved from NEFs during card recovery (no EXIF); PNG. Also read: HEIC, AVIF, WebP and Photoshop documents (PSD, their flattened image; through macOS ImageIO, never written), DNG (a camera's or a converter's, and Shoebox's own linear DNG exports; shown from its previews or rendered from the raw data), Sony ARW (shown from an embedded JPEG only if it has the whole frame: older bodies' are a 16:9 crop of a 3:2 photo, so those are rendered from the raw data like a DNG; Develop decodes it like a NEF), and movies: MOV, MP4 and MPEG-1/2 program streams (`.mpg`, an older camera's clips: size and length from the stream, no recording date, so dated by the file's modified time) |
| Duplicates today | NEF, JPG and TIFF of one frame share a base name; extra copies carry `-1`, `-2` suffixes |
| Scale | About 10,000 files today; design for 200,000 |
| Other tools | DxO PhotoLab 9 for RAW development, digiKam as the catalog; XMP sidecars are the contract between all three |
| Storage | Working library on the Mac's local APFS disk; backup is the user's choice (a NAS, Google Drive or similar), handled outside the app |

## Core concepts

Everything in the app is one of these eight things, and all of them except collections and events can be rebuilt from the files on disk.

| Concept | Definition |
| --- | --- |
| Library root | A folder the user adds to the app; any number of roots can be added, and stacks, events, search and duplicate checks work across all of them. Scanned recursively and watched for changes. A root can be marked **Reference**: read-only, never moved, renamed or written to, and used as the "keep" side in duplicate detection. |
| File | One file on disk: image, sidecar or other. |
| Stack | All files of one frame. Grouped by capture time (DateTimeOriginal + SubSecTimeOriginal) plus camera serial, or by matching base name in the same folder. One **primary** member is shown in the grid (default order NEF or ARW, DNG, TIFF or PSD, HEIC, JPG, PNG, AVIF or WebP; the same order Duplicates keeps) with a badge listing the members, such as `NEF+JPG+TIF`. |
| Variant | A stack member with different pixels from the primary, such as a Lightroom virtual-copy export (`-2`). Shown when the stack is expanded. |
| Sidecar | `<filename>.<ext>.xmp` next to a file, holding ratings, flags, labels and keywords. |
| Collection | A named virtual set of stacks. No files move. Stored in the cache and mirrored to `<root>/.shoebox/collections.json`, so collections survive a cache rebuild. |
| Event | A named group of photos, such as Japan Trip, made without moving any files. Membership is marked on each photo (in its sidecar), never taken from dates or folders, so two events can share a day (an import can mark photos with their source folders' names; see Import). Events can nest (Japan Trip › Kyoto) and overlap. The events themselves (names, nesting) are mirrored to \<root>/.shoebox/events.json so they survive a cache rebuild. |
| Smart filter | A saved query, such as rating ≥ 3, flag = pick, a date range, a camera, "NEF missing", or "undated". |

Ratings, flags and labels set on a stack are written to the primary's sidecar. A setting copies them to every member's sidecar instead, so tools that look at the JPEG or TIFF also see them.

## UI layout

One window with a library browser on the left, the active view in the centre and an info panel on the right, following Lightroom's Library module.

&#91;embedded content: window layout · 6 regions\]

The centre view changes with G, M, E, C, N and T; Tab folds the side panels away.

- **Toolbar (top):** view switcher (Grid, Masonry, Loupe, Compare, Survey, Timeline); sort (capture time, filename, rating, modified); filter bar (rating, flag, label, file type, camera, lens, NEF missing, undated; file type lists the types in the library with how many photos each finds, and several can be ticked: a stack shows if any of its files is one of them); search (filename, keyword); thumbnail size slider; a background-task indicator showing scan and hashing progress.
- **Left panel (resizable, collapsible, default 260 px):**
  - **Library**: each root as a folder tree with file counts, and a **+** button that opens a native folder picker. Right-click a folder to rescan, reveal in Finder, create a subfolder, mark as Reference, or remove from the library (files untouched).
  - Events, **Collections** and **Smart filters**.
  - **Tasks**: Duplicates review, Import, Find dates (undated photos in `_undated/`), History (undo log), Activity (what's running now).
  - Dragging grid items onto a folder moves them (with a confirm).
- **Centre:** the active view. All Photos (Grid and Masonry, sorted by capture time) opens at its newest photos, as Photos does: on start, and when All photos is chosen in the left panel (Show in All Photos goes to its photo instead). When the photos shown are narrowed (a folder, event, collection, person, tag, Find Similar or a filter) or the view isn't the grid, a photo's menu has **Show in All Photos**: it clears all of those, switches to the grid sorted by capture time, and selects that photo (its stack, if it's a folded stack's file), so the photos taken around it are beside it.
- **Right panel (collapsible):**
  - **Info**: histogram, capture time, camera, lens, shutter, aperture, ISO, focal length, dimensions, file size, full path, stack members, shutter count when available.
  - **Organise**: rating, flag, colour label, keyword editor with autocomplete.
- **Filmstrip (bottom):** shown in Loupe, Compare and Survey; the current image is highlighted, click to jump.
- **Status bar:** selected / filtered / total counts and the current file's path. On the right, the background work under way (the most important one, and "+N" when more is running); clicking it opens Activity.
- **Activity** (Tasks → Activity): History's counterpart for what's running now, so slow work can be seen rather than guessed at. One row for each thing under way, with its progress and how long it has been going: the library's current job (a move, undo, scan, import step…), each file being downloaded from a cloud drive and how many wait their turn, thumbnails loading, an iCloud Photos sync or import, an export, a merge, search indexing and faces. A download that has waited 10 seconds says it is waiting for its drive ("Waiting for Google Drive · 3 min"): a cloud drive serves files in its own time, and nothing Shoebox does makes it faster. Finding duplicates shows how many files it has hashed of how many, and when that count hasn't moved for 5 minutes says so ("no file finished for 6 min: waiting for a cloud drive?"). Rows for work that can be stopped (import, iCloud Photos, merge) have Stop; a download already waiting on its drive can't be interrupted, and the row doesn't pretend otherwise. With nothing running it says so.

Tab hides the side panels and Shift+Tab hides every panel, as in Lightroom.

## Views

Six views share one selection, so switching view never loses your place.

| View | Key | Behaviour |
| --- | --- | --- |
| Grid | G | Virtualised grid (only visible rows are laid out). Thumbnails 96–512 px. Badges for rating, flag, label colour, file type and stack count. Click, Shift-click range, Cmd-click toggle. Optional day headers. |
| Masonry | M | Thumbnails at their true aspect ratio, uncropped, in columns of equal width; each stack goes into the shortest column in date order. A setting switches to justified rows (equal row height), which keeps strict left-to-right date order. Layout comes from EXIF width, height and orientation, so it is ready before any thumbnail decodes. Virtualised like Grid, with the same badges and selection. |
| Loupe | E | One image, fit to window. Space or Z toggles 100%; scroll or pinch zooms; drag pans; arrows move. Shows the embedded preview immediately, then the full decode. NEFs display their embedded full-size JPEG (no demosaic in v1). |
| Compare | C | Two images side by side with synced zoom and pan. One is the "select", the other the "candidate"; arrows step the candidate. Used by duplicate review. |
| Survey | N | 2–9 selected images tiled to fit, for picking the best of a burst. Remove a tile with its X; flag or rate in place. |
| Timeline | T | Stacks grouped by month and day, with a heat strip of shooting days; click a day to jump to it in Grid. Can follow after v1. |
| Develop | D | Edit a photo, raw or finished (see `docs/EDITING-SPEC.md`): rendered live from its raw data, modules on the right, the edit's history on the left. G or Escape goes back. |

F toggles fullscreen on a black background in any view.

**Colour:** convert each image's embedded ICC profile to the display's colour space (sRGB fallback). Lightroom TIFFs are often Adobe RGB or ProPhoto, which look flat and dull if shown unconverted.

## Organising

Culling marks go into XMP; file changes always move whole stacks, sidecars included.

**Marks**

- **Rating** 0–5 → `xmp:Rating`.
- **Flag** pick / reject / none → `digiKam:PickLabel` (3 accepted, 1 rejected, 0 none) plus an app property, so digiKam shows it.
- **Colour label** red, yellow, green, blue, purple → `xmp:Label` (Lightroom-style text) and `digiKam:ColorLabel`.
- **Keywords** → `dc:subject` (flat), `lr:hierarchicalSubject` and `digiKam:TagsList` (`Places|Japan|Kyoto`).
- **Auto-advance** (Caps Lock, as in Lightroom): setting a mark moves to the next image.

**Events**

- An event is the photos marked with it; folders and dates have no bearing on it, so two events can share a day (a wedding and the party after it). In the left panel, each event sits under the year of its photos and expands to the folders they're in, for browsing only.
- Create an event from the selected photos (Cmd+E) or empty, then add photos from the photo menu (Add to Event), a folder's menu, or by dropping them on it; Remove from Event takes them out. A sub-event's photos are in its parent too.
- Written to sidecars as `Iptc4xmpExt:Event` (the innermost event) and as a keyword under `Events|<name>` (`Events|Japan Trip|Kyoto`), so digiKam can show and search it; that keyword is the membership.
- Optional, off by default: rename the day folders to carry the event (`2026-04-24 Japan Trip`), as one journaled, undoable batch.

**File operations**

- Move (drag to a folder or a menu command), rename, create folder, reveal in Finder.
- **Reject → move** sends rejected stacks to `_rejected/` inside the same root, keeping their relative path.
- **Move to Trash** only on an explicit command, using the macOS Trash so it can be recovered.
- Every operation on a stack applies to all members and their sidecars together.

**Import**

- From a card or any folder, **copy** (never move) into a library root using a template. Default: `{YYYY}/{YYYY-MM-DD}/{YYYYMMDD}-{HHMMSS}-{subsec}.{ext}`, with a `-N` suffix on collisions.
- Fallbacks: no sub-seconds → leave them out of the name; no capture date → `_undated/{width}x{height}/{original name}`. A movie with no recording date in its header (an editor's export often has none) is dated by its modified time instead; photos aren't (their modified time is often when they were copied).
- Verify every copy by BLAKE3 before reporting it imported.
- Skip files whose exact hash is already in the library, and list them. A file repeated within the source is copied once; the repeats are listed apart ("repeated in the source"), not as already in the library. Each left-out file is shown beside the file it matched (thumbnails of both) with **Import anyway**, which plans again without reading the source again (and **Leave out** to undo that).
- A folder's photo count shows just the photos its name goes on.
- While copying, the Import screen shows files and bytes done, the speed and time left, and the file being copied; the status bar shows it from any view.
- While copying from a card or disk, the Import screen says to keep it connected; once the import is over it offers to eject it. Quitting while an import copies asks first: keep importing, or stop it cleanly (between files) and quit.
- Rolling back an interrupted import (the app quit or crashed mid-copy) also moves the half-written copy to the Trash.
- Finding photos and copying them can be cancelled. A cancelled copy stops before its next file: what's copied stays as the batch (undoable from History); the copies' sidecars (folder names) come last, so they aren't written. Finding photos again finishes the job: the copies are then already in the library and get their folder names that way.
- Show a dry-run table (source → destination) before anything runs, or the photos to copy as thumbnails (the default; source and new name on hover). Previews are rendered from the source and not kept in the thumbnail cache.
- **Folder names.** The source's folders are listed with what each name becomes on the photos in it: an **event** (nested as the folders are: `Italy 2019/Rome` → Italy 2019 › Rome, joining an existing event of that name), a **keyword**, or **nothing**. Names that say nothing start as nothing: dates (`2019`, `2019-07-14`, `July 2019`), camera folders (`100NIKON`, `DCIM`) and generic names (`Photos`, `iCloud Photos`); others start as events. Each name can be edited. A picture found in two folders gets both; one the library already has gets them too (in its sidecar, unless it's in a reference root). The names go into the copies' own sidecars (made from the source's sidecar, if any) and `events.json`, in the import's batch: undo moves those sidecars to the Trash with the copies. The source is never written.

**Batch rename** uses the same template engine and dry-run table, inside or across folders.

**Dates**

A photo's date is its capture time: the file's EXIF date, unless a sidecar gives one (set in another app, by an iCloud Photos sync, or here), which wins. A photo with neither is undated: it sorts last, shows under Undated and goes to `_undated/` on import.

- **Set date…** in the photo menu (and on the Info panel's date row of an undated photo) sets the date of the selection: a date and time, or just the day, the month or the year when that's all that's known. The selection's photos are listed with their dates now and after (a dry run); Set writes the date to each photo's sidecar (`exif:DateTimeOriginal` and `photoshop:DateCreated`) as one journaled batch, so ⌘Z takes it back. The image files are never changed. Photos in reference folders are left out, and the dry run says so.
- **Approximate dates.** A day, month or year is written as XMP allows (`photoshop:DateCreated` `2019-03-12`, `2019-03` or `2019`; no `exif:DateTimeOriginal`, which needs a time). Such a photo sorts at the middle of its period (noon on the day, the 15th of the month, 1 July), and shows as "March 2019 (approximate)". Other apps may read only the year, or nothing.
- **Find dates** (Tasks → Find dates) helps date the undated photos in `_undated/` (undated photos filed by folder elsewhere are left as they are; Find Date… takes them one selection at a time). Look for clues starts it (nothing runs until then; Look at N more takes photos new since, Look again starts afresh); it gathers clues for each in the background, with Stop, and a working panel says what it's doing: the stage (reading the sync record, the duplicate finder's hashes with how many, each photo, the pictures), the photo it's on and the step (reading its dates, hashing it, comparing with the library, reading its text), and the photos just looked at with what was found. **Find Date…** in the photo menu does the same for the selected photos only, dated or not (to check a date), at once; All undated photos goes back. iCloud Photos sync record files in a cloud drive only are left out (reading one waits for the drive), and the panel says how many. For each photo it suggests a date with how precise it is (a moment, day, month, year or decade), how sure it is, and the evidence (“File name: 12 Mar 2019 10:15”). Clues, strongest first:
  - *In the file*: dates other than the capture date (EXIF's digitized and modified dates, the GPS date and time stamp, dates in its own XMP or IPTC, a PNG's creation time), dates in the file name (`IMG_20190312_101530`, `PXL_…`, `Screenshot 2019-03-12 at 10.15.30`, `IMG-20190312-WA0001` for the day, Unix timestamps) and in its folders' names (`2014/03 Japan`, `March 2014`).
  - *In the library* (Phase 2): the same picture dated elsewhere (the same pixels or a perceptual match, such as a carved preview and its NEF, against the photos the duplicate finder has hashed; the undated ones are hashed as they're looked at. While some dated photos aren't hashed, the view says how many are, with Open Duplicates, and Look again finds the clues afresh), the same frame (its stack) or iCloud Photos asset (the sync record), the camera's sequence (a file number between two dated frames in its folder, or a shutter count between two of the same camera's), and the dated photos beside it in its folder (not in `_` folders such as `_undated`).
  - *In the picture* (Phase 3), looked at in a second pass for the photos without a sure date yet (it's slower): dates in its text, read on the Mac by Apple Vision (a film camera's imprint such as `'98 3 12`, a ticket, a receipt or a newspaper: `25/12/2003`, `12 March 2019`; a day and month that could be either way round give both, as guesses). Hints, shown as suggestions to pick but never taken by Accept and never more than a guess: the dated photos it looks most like (semantic search's embeddings, when at least three of them share a day, month or year) and dated photos of the same two or more people (from People). Scene hints, from search's tags, are words beside the group, not dates: a Christmas tree (December), snow, autumn leaves, fireworks, a birthday, a wedding, a graduation, black and white. Look-alikes, people and scene hints need Search and People turned on.
  - The view groups photos that share a suggestion (a folder, a burst, near-identical pictures), each group with its thumbnails, the best suggestion and the others, the evidence, and a date field with its precision. Accept, a different suggestion, or a date typed in, applies to the group (or the photos picked in it); Skip leaves it. *Show beside…* opens Grid (all photos, by capture time) at the dated photo nearest the suggestion, to compare. *Accept all sure ones* (Phase 4) takes every high-confidence suggestion at once. Every write goes through the same dry run and batch as Set date…; nothing is dated without a click.
  - Photos dated from `_undated/` (Phase 4): Find dates then offers to move them where the import template puts them (by their new date), as a batch of its own with its own dry run, like a rename: an exact date by the whole template; a day into the template's folders, keeping the file's name (the template's time would be made up); a month or year stays, and the offer says how many. Their sidecars, with the dates, go with them.
  - Cloud-only photos aren't read, so only their names and folders give clues until they're here; the view says how many.
  - Phases: 1, Set date…, approximate dates and Find dates with the clues in the file (built 2026-10-23); 2, the library's clues and Show beside… (built 2026-10-24); 3, the picture's (built 2026-10-24); 4, Accept all sure ones and the move out of `_undated/` (built 2026-10-24). Shifting the dates of photos already dated (a camera whose clock was wrong) is for later (OPEN-ITEMS).

**Photo merges** (EDITING-SPEC, Phase 4 additions)

- **Merge to HDR…** in the photo menu (Ctrl+H) merges 2–9 bracketed raw photos into one new raw photo, a floating-point DNG.
- **Merge to Focus Stack…** in the photo menu merges two or more raw photos of one scene focused at different distances into one raw photo sharp throughout (`<first photo's name>-Focus.dng`), the same way. Its dialog offers Method (Pyramid, the default, or Depth map) and Align (on: focus breathing changes each photo's view a little); the middle photo is the reference, and the result is cut a little to what every photo covers.
- **Merge to Panorama…** beside it (Ctrl+M) stitches 2–60 overlapping raw photos of one camera into one, `<first>-Pano.dng`: Projection (Auto, Spherical, Cylindrical, Perspective), crop to the largest rectangle (on by default; off keeps the whole boundary with black corners), and each photo's lens corrected first (on by default). A photo that overlaps none of the others is named and nothing is written.
- The dialog is the dry run: it lists the photos (with each one's exposure), the options, why they can't be merged if they can't (not raw, a reference folder, cloud-only, different sizes or cameras), and the one file it will write: `<first photo's name>-HDR.dng` beside the first photo, `-1`, `-2` if that's taken.
- The merge runs in the background (progress and Stop in the status bar). The file is added as one journaled batch, verified, never replacing anything; ⌘Z sends it to the Trash. The photos merged are only read, never changed or moved, and merges aren't made from or into reference folders.
- The result is a photo of its own: it isn't stacked with its sources and is never suggested as their duplicate. Its Info panel lists what it was made from, and a source's lists the merges made from it. The record is in the DNG's XMP and in `<root>/.shoebox/merges.json`, which Shoebox's moves keep up to date.
- Its edit starts from the defaults; it opens in Develop as a raw past demosaic.

**Cloud drives**

Google Drive (streaming), iCloud Drive (Optimise Mac Storage), Dropbox and OneDrive (online-only) leave placeholders for files that are in the cloud only; macOS marks them `SF_DATALESS`, and reading one downloads it. Shoebox never reads one by itself:

- The scan lists it from its name, size and sidecars (which are small, so they're read), with a cloud badge, and caches nothing for it; the status bar counts them.
- Thumbnails show only if they were cached before; semantic search, faces and duplicates leave it out (Duplicates says how many) until it's here The duplicate finder looks again just before reading each file, so one its drive sent back to the cloud since the scan (Google Drive does when the disk is full) is left out too rather than waited for.
- Clicking it (making it the current photo, which opening it in the loupe does too), or **Download from Cloud** in a photo's menu (for the selection, whole stacks), downloads it by reading it through (nothing is written); then it's read like any other photo, thumbnail and details included. A click downloads that one file only, so selecting many (⌘A) downloads nothing. A download isn't recorded as a change.
- Import reads every source file, so importing from a cloud folder downloads it; the plan says how many.

## Duplicate detection

Three levels of match, all cached per file so a second scan only hashes new or changed files.

| Level | How | Catches |
| --- | --- | --- |
| Exact | BLAKE3 of the whole file | The same file recovered twice |
| Same image data | Hash of the image data without metadata: JPEG scan data (SOS to EOI), TIFF strip or tile data | Identical re-exports that differ only in export-time metadata |
| Similar | 64-bit dHash and pHash from a 256 px decode, compared by Hamming distance | Resized, recompressed or lightly edited versions, such as a 6048×4032 preview against a 2560 px TIFF |

- **NEFs** are compared through their embedded full-size JPEG (`JpgFromRaw`). A carved preview that is byte-identical to a NEF's embedded JPEG matches at the "same image data" level.
- **Thresholds**: one slider, **Show matches ≥ N%** (75% to 99%, default 84%, ≤ 10 bits), in the percentages the matches show; identical files and the same image data (100%) are always found. Matches at 93% or more (≤ 4 bits) are labelled near-identical, the rest similar (fixed; low-detail pictures match only when near-identical). Moving it groups the last search's files again at once (nothing is read again): the list follows it. These are starting points to tune against real files.
- **Index:** a BK-tree over hashes; matches merged into groups with union-find.
- **Reference roots** are never proposed for moving.
- **Ignored pairs** are remembered, so a dismissed match never comes back.

**Review (in the app)**

1. Tasks → Duplicates lists groups as thumbnail strips, most alike first (each group by its least alike match; a menu turns it to least alike first, to check the doubtful ones), with how alike each match is, dimensions and paths (a percentage: 100% only for an identical file or the same image data; a perceptual match from its distance over the 64-bit hashes, at most 99%: a likeness, not a probability). The search runs beside the library's other work, so moving, renaming or trashing photos doesn't wait for it; the size slider in the bar under the list makes the thumbnails larger (sharper ones load as they grow).
   - **In** chooses where to look: All photos (the default), a folder, an event or a collection. Within a folder, event or collection, only its own photos are compared with each other (every file of their stacks), so both files of every match are in it and nothing outside is listed or moved. A folder's, event's or collection's menu has **Find Duplicates in …**, which opens Duplicates and searches there. After applying or undoing, the search runs again in the same place.
2. Opening a group shows it in Compare (2 files) or Survey (3+).
3. Per file: keep, move to `_duplicates/`, add to stack as a variant, or ignore.
4. **Auto-select** rules mark the rest of each group in one go: keep the reference copy, then NEF over TIFF over HEIC over JPG (a phone's HEIC is the original; a JPG of it is usually a conversion), then the highest resolution; mark the other copies to move to `_duplicates/` when they're at least as alike as **Auto-select copies ≥ N%** (default 100%: identical files and the same image data only, which are always marked; down to 75%). Below 100%, **taken within** (the same moment by default; half a second up to a day, or any time) marks only copies taken that close in time to the copy they match, or with no capture time to tell (a resized export, a carved preview); other exposures (burst frames, dated at another moment than the copy they match, or than the kept copy when that copy is undated) stay undecided for a person, since they look alike without being copies. A decision stays with a file the slider hides and comes back with it; applying leaves hidden files out. A copy taken at another moment says how far apart ("taken 2 s apart"), and after Auto-select the view says how many it marked and how many it left undecided and why (taken further apart than taken within allows; less alike than the slider).
5. Move marked files, as one undoable batch. Apply shows the dry run first, sized to the window with its buttons always in view: what it does in numbers (how many identical files, same image data, near-identical and similar, the least alike, the sidecars that go with them, an example of where they go), then every file to move beside how alike it is and the copy that stays. While it runs, the view (and Activity) shows the stage (checking each file is unchanged since it was found, read in parallel; then moving files and sidecars), a bar, how many of how many and the time left; then reading the library again and finding again.

**Headless (CLI)**

```
shoebox dupes --ref <folder to keep> --scan <folder to check> \
  --threshold 10 --report dupes.csv --move-matched <folder for matches> --dry-run
```

This is exactly the job dupeGuru was going to do for the recovered previews, so it is the first thing to build.

## Metadata and XMP sidecars

The app reads EXIF from the images and writes only to sidecars; original image files are never modified in v1.

**Reading**

- EXIF from JPEG, TIFF and NEF (NEF is TIFF-based, so standard tags and SubIFDs read the same way): DateTimeOriginal, SubSecTimeOriginal, OffsetTimeOriginal, Make, Model, BodySerialNumber, LensModel, FocalLength, FNumber, ExposureTime, ISO, Orientation, width and height.
- Nikon shutter count from the MakerNote where it can be read; otherwise left blank. Shelling out to ExifTool, when installed, is an acceptable optional fallback.
- EXIF Orientation is applied to every thumbnail and view.
- Precedence for marks: sidecar, then embedded XMP, then EXIF.

**Writing**

- Sidecar name: `<filename>.<ext>.xmp`, so a NEF, JPG and TIFF of the same frame never share one sidecar.

  On scan, both conventions are read: `name.ext.xmp` and Lightroom-style `name.xmp`. A setting also writes `name.xmp` for RAW files, for tools that only look there.
- Merge, never replace: parse the existing sidecar, change only the properties the app owns (Organising section), and keep everything else byte-for-byte, including DxO, digiKam and Lightroom `crs:` data.
- Atomic writes: write a temporary file, then rename it over the original.
- Sidecars in Reference roots are never written.

**Staying in sync**

- A file watcher picks up sidecar changes made by DxO or digiKam and refreshes the UI.
- On startup, files whose size or modified time changed since the last scan are re-read.

**Change indicator**

- A small badge on grid and masonry thumbnails when the image or its sidecar has changed since you last viewed that stack. Hovering shows what changed and when.
- Changes made by other apps (DxO, digiKam, Finder) get a different badge colour from Shoebox's own.
- The Info panel shows the last-modified date and time for the image and the sidecar, and which app changed them; Shoebox's own changes link to their History entry.
- A "Recently changed" smart filter, and filtering or sorting by change date.
- The badge clears when the stack is opened in Loupe, or with Mark as Seen.

## Architecture

A Cargo workspace of four crates, where the UI is a thin layer over a core library that the CLI shares.

&#91;embedded content: crates and storage · 4 crates, 3 stores\]

Only the core touches the library on disk; both caches can be deleted and rebuilt from it.

| Crate | Responsibility |
| --- | --- |
| `shoebox-core` | Library model, scanner, metadata, XMP, stacks, hashing, duplicate index, file operations and journal, SQLite cache. No UI dependencies. |
| `shoebox-decode` | Decoding and thumbnails: JPEG (DCT-scaled decode for thumbnails), TIFF including 16-bit, PNG, HEIC, AVIF, WebP and PSD through macOS ImageIO, NEF and ARW embedded-preview extraction by walking the TIFF IFDs (an ARW without a whole-frame preview rendered from the raw data), MPEG stream headers, DNG previews (or the raw data rendered, when they're too small), ICC conversion. |
| `shoebox-app` | The eframe/egui application. |
| `shoebox-cli` | Headless `scan`, `dupes`, `import` and `rename` commands. |

**Suggested crates:** `eframe`/`egui` (wgpu backend), `rfd` (native dialogs), `rusqlite` (bundled SQLite), `rayon`, `crossbeam-channel`, `notify`, `jwalk`, `blake3`, `image_hasher`, `kamadak-exif`, `quick-xml`, `zune-jpeg` or `turbojpeg`, `image` and `tiff`, `lcms2`, `trash`, `serde`, `clap`, `tracing`, `thiserror`/`anyhow`.

**Threading**

- The UI thread never touches the disk or decodes an image.
- A job pool runs work by priority: visible thumbnails, then neighbours of the current image, then background scanning and hashing. Jobs for thumbnails scrolled out of view are cancelled.
- Results come back over a channel; textures are uploaded on the UI thread under an LRU memory budget (configurable).

**Cache (SQLite, rebuildable)** in `~/Library/Application Support/<bundle id>/`, thumbnails in `~/Library/Caches/<bundle id>/` at 256 px and 1024 px.

| Table | Key columns |
| --- | --- |
| `roots` | id, path, is\_reference |
| `files` | id, root\_id, rel\_path, ext, size, mtime, blake3, image\_data\_hash, changed\_at, change\_source, seen\_at |
| `exif` | file\_id, capture\_time, subsec, offset, make, model, serial, lens, focal, fnumber, exposure, iso, width, height, orientation, shutter\_count |
| `stacks`, `stack_members` | stack\_id, file\_id, role (primary / member / variant) |
| `marks` | file\_id, rating, pick, label, keywords, sidecar\_mtime |
| `phash` | file\_id, dhash, phash |
| `collections`, `collection_items` | collection\_id, stack\_id |
| events | event\_id, name, parent\_id (membership is read from each photo's sidecar keywords) |
| `journal` | id, batch\_id, ts, op, src, dst, undone |
| `dupe_ignores` | hash\_a, hash\_b |

**Performance targets** (measured on an Apple silicon MacBook Pro; targets to tune, not promises):

| Measure | Target |
| --- | --- |
| Open a 200,000-file library from cache | Grid usable in under 1 s |
| Grid scrolling | 60 fps; no UI frame over 16 ms |
| Loupe | Embedded preview under 100 ms; full 24 MP image under 1 s |
| Duplicate scan, 10,000 images | Under 2 min cold; under 5 s with hashes cached |

## Safety rules

The app must be safe to point at the only copy of a photo library, so these rules are hard requirements, not preferences.

1. **No deletes.** Nothing outside the app's own cache directories is ever unlinked. "Remove" means move to `_rejected/` or `_duplicates/`, or the macOS Trash on an explicit command.
2. **Journal first.** Every move, rename and copy is written to the journal, with a batch id, before it runs.
3. **Undo.** Cmd+Z reverses the last batch; the History panel lists every batch and can undo any of them that is still consistent with the disk.
4. **Dry run.** Any operation on more than one file shows a source → destination table and needs confirmation.
5. **No overwrites.** A name collision gets a `-1`, `-2` suffix; an existing file is never replaced.
6. **Verified copies.** An import copy counts only after its BLAKE3 matches the source.
7. **Reference roots are read-only**: no moves, renames or sidecar writes.
8. **Crash recovery.** On startup, the journal is checked for batches that were started but not finished, and the user is shown what to resume or roll back.
9. **Photos is written only by an explicit push** (`docs/PHOTOS-SPEC.md`), with a dry-run table and confirmation, and only to add a new asset, add or replace an edit Shoebox made (Photos can always revert it), or undo a push. No other asset, album or metadata in Photos is ever changed.
10. **No Photos access during development except a test library.** PhotoKit only opens the system Photo Library, so an APFS clone can't stand in for it: real PhotoKit code runs only in a separate macOS user account whose library is a test library made from fixtures.

## Keyboard shortcuts

Shortcuts follow Lightroom wherever one exists, so existing muscle memory carries over.

| Key | Action |
| --- | --- |
| G / M / E / C / N / T | Grid / Masonry / Loupe / Compare / Survey / Timeline |
| F | Fullscreen |
| Tab / Shift+Tab | Hide side panels / hide all panels |
| Arrow keys | Previous / next image |
| Space or Z | Toggle fit and 100% zoom |
| 0–5 | Set rating |
| P / X / U | Pick / reject / unflag |
| 6 / 7 / 8 / 9 | Red / yellow / green / blue label |
| Caps Lock | Auto-advance after marking |
| S | Expand or collapse stack (Cmd+E: new event from selection) |
| Cmd+K | Add keywords |
| Cmd+F or / | Search |
| Cmd+A / Cmd+D | Select all / deselect |
| Cmd+R | Reveal in Finder |
| Cmd+Backspace | Move selection to `_rejected/` |
| Cmd+Z / Cmd+Shift+Z | Undo / redo the last batch (in Develop: the edit's own history) |
| D | Develop the current photo |
| \\ / Y | In Develop: original / edited; split view (drag the divider) |
| R | In Develop: crop (Enter applies, Escape cancels) |
| J | In Develop: show clipping |
| W | In Develop: pick a neutral for white balance |
| K | In Develop: place a control point (a click on the photo) |
| Z or Space | In Develop: fit / 100% (drag or scroll to pan; double-click zooms there) |
| Cmd+[ / Cmd+] | In Develop: rotate left / right |
| Shift+Cmd+E / Opt+Shift+Cmd+E | Export… / export again with the last presets |
| Ctrl+H | Merge to HDR… (the selection) |

## Milestones

Build the core and the duplicate CLI first: it solves the recovered-preview problem straight away, and every later milestone reuses it.

1. **M0 — Skeleton.** Workspace, four crates, CI (fmt, clippy, tests), an empty three-panel egui window, logging.
   - Done when `cargo run -p shoebox-app` opens the window and CI is green.
2. **M1 — Core and duplicate CLI.** Scanner, EXIF, NEF preview extraction, BLAKE3, image-data hash, dHash/pHash, BK-tree, groups, CSV report, `--move-matched` with dry run and journal.
   - Done when the CLI command in the Duplicate detection section runs against a real library, carved previews that are byte-identical to a NEF's embedded JPEG match at distance 0, and a move can be undone.
3. **M2 — Browse.** Library roots and folder tree, virtualised grid and masonry view, thumbnail cache, loupe with zoom and pan, filmstrip, read-only info panel, colour management, HEIC.
   - Done when a 10,000-file library scrolls smoothly, NEFs show via their embedded preview, orientation is right, and Adobe RGB TIFFs look the same as in Preview.app.
4. **M3 — Organise.** Ratings, flags, labels and keywords written to sidecars; filters, sort and search; stacks; events.
   - Done when a rating set in the app shows in digiKam after a refresh and vice versa, and a golden test proves a DxO sidecar keeps every property the app doesn't own.
5. **M4 — Compare, Survey and duplicate review.** Duplicate groups in the UI, per-file actions, auto-select rules, ignored pairs.
   - Done when the `_undated` clean-up can be finished entirely in the app.
6. **M5 — File operations and import.** Move and rename with templates, card import, History panel with undo, collections.
   - Done when importing a card produces exactly the names the default template predicts and every batch can be undone.
7. **M6 — Later.** Timeline view, GPS map, and AI enrichment through ONNX Runtime (the `ort` crate): faces with InsightFace, objects with YOLO, and CLIP embeddings in sqlite-vec for semantic search.
8. **M7 — iCloud Photos.** Copy the Photos library into a root, sync again without copying anything twice, and send edited photos back (`docs/PHOTOS-SPEC.md`, its Plan).

## Notes for Claude Code

Export this doc as Markdown to `docs/SPEC.md` in the repo, and add a short `CLAUDE.md` that points to it and carries the conventions below.

**Working rules**

- Build one milestone at a time, in order, each as a reviewable chunk.
- Before finishing any task: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- `thiserror` in library crates, `anyhow` in binaries; no `unwrap()` or `expect()` outside tests.
- No `unsafe` except inside thin FFI wrappers.
- `shoebox-core` must not depend on egui. Every file operation goes through one module in core that journals it.
- Target `aarch64-apple-darwin` on current stable Rust, minimum macOS 27; release profile with `lto = "thin"`.
- Prefer pure-Rust crates. In M1, benchmark `zune-jpeg` against `turbojpeg`'s scaled decode for thumbnails and keep the faster one if the gap is large.

**Testing**

- Fixtures in `tests/fixtures/`: a Z f NEF and its JPG, a Lightroom TIFF export and its `-2` variant, a carved preview JPEG with no EXIF, a DxO sidecar, a digiKam sidecar. Large binaries go in Git LFS or a gitignored folder with a fetch script.
- Golden tests for XMP merge round-trips; property tests for the rename template engine.
- `criterion` benchmarks for thumbnailing, hashing and the duplicate index.

**Never against the real library during development.** Test on an APFS clone (`cp -cR Photos Photos-test`), which costs no disk space.

## Open questions

- [x] **Name.** Shoebox (decided).
- [x] **Sidecar naming.** `name.ext.xmp` per file is the default, both conventions are read, and name.xmp for RAWs is optional. Checking which one DxO PhotoLab 9 writes is deferred (2026-10-18); the default stays `name.ext.xmp`.
- [x] **Rejects.** Store only in `digiKam:PickLabel`, or also as an XMP rating of −1 (Adobe Bridge's reject)? Decided: PickLabel only for now; may become a per-library setting later.
- [x] **Embedded XMP.** Sidecars only, or also write marks into JPEG and TIFF files later? Decided: sidecars only, so image files are never modified.
- [x] **Cache location.** One cache per user in Application Support, or one per root, so a library on an external disk carries its own? Decided: follow Apple's layout, keyed by the app's bundle ID: the database in Application Support, thumbnails in Caches (which macOS may purge, as they rebuild). Only collections and events live in each root.
- [x] **NAS roots.** Allow folders on a NAS or network share as roots? Scanning many small files over SMB on Wi-Fi is slow, so they may need to be read-mostly with thumbnails cached locally. Decided: not for v1; roots are local folders only.
- [x] **Mac.** Confirm the Mac model and macOS version, to set the minimum OS and check the performance targets. Decided: an Apple silicon MacBook Pro on macOS 27, which is also the minimum OS.
- [x] **HEIC.** Needed at all, given phone photos live in iCloud Photos? Decided: yes, for iCloud Photos exports kept as a backup. Decode through macOS ImageIO (no extra library) in M2; a Live Photo's HEIC and MOV share a base name, so they form one stack.
