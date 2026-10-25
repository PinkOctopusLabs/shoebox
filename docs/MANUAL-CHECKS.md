# Manual checks

Checks that need real photos or a person's eye: everything the tests can't
judge. Every editing phase and library milestone is built (2026-10-22), so
they're run now, together, before the release. This file is the runbook.

**How to use it**

- Gather the photos and tools in [Before you start](#before-you-start)
  first, then work down the sections in order: later ones lean on what
  earlier ones set up (models downloaded, the 30 edited, exports made).
- Tick each box (`[x]`) as it's done, with the date and the result on the
  line after it, indented. Numbers asked for ("note the time") go there too.
- When a check fails, note the file and what's wrong, copy the file into
  `tests/fixtures/real/` (gitignored), and add the failure to
  `docs/OPEN-ITEMS.md` (a bug or missing piece) or `docs/DECISIONS.md` (a
  choice the result forces). Leave the box empty until it passes.
- **Never on the real library.** Work on an APFS clone (`cp -cR Photos
  Photos-test`) or copies, with a throwaway build that doesn't touch real
  settings: `SHOEBOX_DATA_DIR=<scratch dir>` (CLAUDE.md, "Building the
  app"). Real files never go into the repo.
- **Never judge against Lightroom or PhotoLab** (or any licensed editor):
  the camera's own JPEG and measurements (ΔE2000, timings) are the
  references. darktable, RawTherapee, Preview, Photos and Finder are fine
  where a check asks whether a file opens elsewhere.

**66 checks** (58 carried over, 2 gate verdicts added, 3 for iCloud Photos, 1 for cloud-only raws, 1 for Activity, 1 for Find dates), plus one already
done:

| Area | Checks |
| --- | --- |
| [1. Library and files](#1-library-and-files) | 7 |
| [2. Raw decode and colour](#2-raw-decode-and-colour) | 6 |
| [3. Develop: tone, colour and effects](#3-develop-tone-colour-and-effects) | 8 |
| [4. Noise and detail](#4-noise-and-detail) | 3 |
| [5. Local adjustments and masks](#5-local-adjustments-and-masks) | 11 |
| [6. Retouching and Lens Blur](#6-retouching-and-lens-blur) | 4 |
| [7. Lens and geometry](#7-lens-and-geometry) | 4 (+1 done) |
| [8. Merges](#8-merges) | 10 |
| [9. Export](#9-export) | 7 |
| [10. Last: the gate verdicts and goldens](#10-last-the-gate-verdicts-and-goldens) | 3 |
| [11. iCloud Photos (a test macOS account)](#11-icloud-photos-a-test-macos-account) | 3 |

## The gates

Each editing phase ends at a gate (EDITING-SPEC, "Performance, testing and
roadmap"). A photo gate may be deferred while the next phase starts, but no
release claims a phase is done until its gate passes. These decide that;
the other checks feed them.

| Gate | Passes when | Checks |
| --- | --- | --- |
| Phase 1: foundations | 30 NEFs clean; ColorChecker mean ΔE2000 ≤ 3 with the matrix profile | [30 NEFs render cleanly](#2-raw-decode-and-colour), [ColorChecker](#2-raw-decode-and-colour) |
| Phase 2: everyday editor | A daily driver for low-ISO shots | [Daily driver](#10-last-the-gate-verdicts-and-goldens), fed by sections 3, 4, 7 and 9 |
| Phase 3: replace PhotoLab | 27 of 30 pass the 30-NEF review, in each pass (EDITING-SPEC's definition of done) | [The 30-NEF review](#10-last-the-gate-verdicts-and-goldens), fed by sections 4–7 |

Phase 4 has no gate of its own: its checks just have to pass.

## Before you start

### What to shoot or gather

Shoot on the Z f in RAW + JPEG wherever a check compares with the camera
JPEG, with the Picture Control you use. Pick the 30 so they also cover as
many of the subjects below as they can (a backlit portrait, an interior
with a window, a sky): that saves shooting twice. Put everything in the
APFS clone, in folders by shoot.

| Shoot or gather | Notes | Used by |
| --- | --- | --- |
| **The 30-NEF set** | Low light, high ISO, backlit, landscape with sky, portrait, architecture, blown highlights; some High efficiency, some Lossless; each with its camera JPEG | 30 NEFs render cleanly, Auto on the 30, the 30-NEF review, Golden images |
| **The 10 highest-ISO NEFs** (of the 30, or more) | Including night shots with bright lights; an ISO series of one scene helps | Noise reduction, AI denoise, AI denoise per mask, AI denoise in Auto, Linear DNG exports, A Shoebox linear DNG reopened |
| **A ColorChecker** (Classic, Passport or Nano: say which), in daylight and under tungsten | Lossless, base ISO, open shade or overcast, square on, white patch not clipped | ColorChecker, A ColorChecker DCP |
| **A few hundred varied NEF+JPEG pairs** (same names), from the clone | Your Picture Control; Active D-Lighting off if you can | Fit the camera match profile |
| **An evenly lit wall or sky** at the widest aperture | | Vignetting |
| **Portraits**: a flash portrait (and a pet photo), backlit portraits, a group, a profile view, a portrait with flyaway hair against a sky, skin with a blemish | | Red-eye, Smart Lighting (both), Face masks, Subject and People masks, AI mask edges, Spot removal, Sharpness per mask, Control points, Lens Blur |
| **Landscapes and skies**: a clear sky over a city, a cloudy sky behind bare trees, a sunset over the sea, a mountain lake, a forest path, a night sky, a bare tree against a bright sky, a red flower on green leaves, foliage | | Sky and landscape masks, AI mask edges, Smart Lighting, Point Color, Vignette and grain, Control points |
| **Very saturated photos**: flowers, neon, sunsets | | Soft proofing, AVIF exports |
| **Architecture**: a tall building shot looking up, an interior, a horizon over water; also photos without strong lines (a portrait, foliage) | | Auto-upright, Guided upright |
| **Wide-angle**: a 14–20 mm group with faces at the edges and corners, a 24 mm street scene with buildings at the sides, a portrait-orientation wide shot | | Volume deformation |
| **Mixed light**: an interior lit by tungsten lamps with daylight from a window; an LED-lit stage or street at night (deep blue or violet LEDs) | | Colour calibration (CAT16) |
| **Things to remove**: sensor dust in a sky, a small sign on a wall, a person walking across a beach or square, a power line across a sky, a sign or bin against a wall or hedge, a dust bunny in a clear sky, something on a pattern (tiles, a fence, a brick wall) | | Spot removal, Object removal |
| **Things to click**: a person, a car, a tree, a cup; a busy street; a landscape with no subject | | Object masks, Subject and People masks |
| **Depth**: a portrait, a street scene with depth, flowers close up, a night shot with point lights, ideally a foreground branch | | Lens Blur |
| **A bracketed set**, or a series shot in manual with changing light | The HDR brackets below do | Match Total Exposures |
| **HDR**: a hand-held 3-frame bracket (−2, 0, +2 EV, the Z f's AE bracketing) of a room with a bright window and a dark corner; a tripod bracket of five frames ±2 EV in 1 EV steps with branches moving in wind (a person walking through one frame if possible) | | Section 8, HDR |
| **Focus**: a macro stack of 10–30 frames (a flower or an insect, tripod, focus shift from nearest to farthest, wide open or near it); a landscape with near and far, 3–5 frames at f/8–f/11, one set hand-held | | Section 8, focus stacking |
| **Panorama**: a hand-held 5-frame row (the 40 mm, portrait orientation, about a third of a frame per turn, AE on, a horizon and sky); a two-row 360° (about ±20°, 16–24 frames, along one row and back along the other); a city street with people walking through the overlaps (3–4 frames); a mural or long poster shot walking along it (3 frames); a document scanned in overlapping parts; a row through an adapted manual lens (no EXIF lens) | | Section 8, panorama |
| **Other files**: a DNG from Adobe DNG Converter of a Z f NEF (default medium preview); a DNG from a phone or another camera (Pixel or iPhone ProRAW, Leica, Ricoh) with its JPEG if it has one; AVIFs from an iPhone (or Photoshop, Squoosh, a browser), one in Display P3 and one portrait | | A camera DNG from another app, An AVIF from an iPhone |
| **NEF+JPG stacks** among real photos | | Virtual copies as tiles |
| **A PNG logo** for the watermark | | Watermark and border |
| **`.cube` looks**: film-emulation packs with a 33³ and a 65³ Adobe cube, a Resolve cube with a shaper, a 1D curve | Keep the app each came from to compare | Looks |
| **Adobe Standard and Camera Standard** for the Z f | From Adobe DNG Converter's profiles folder | An Adobe profile |
| **An external display** beside the laptop's | | Text size and spacing |

### Tools

- **Adobe DNG Converter** (free): High efficiency NEFs, the DNG from
  another app, the ColorChecker DCP and the Adobe profiles.
- **dcamprof**: the ColorChecker DCP.
- **darktable and RawTherapee**: opening Shoebox's linear and merged DNGs
  elsewhere.
- **exiftool**: the merged DNG's XMP record.
- Built in: Preview, Photos, Finder's Get Info, Activity Monitor.
- **Not built yet: the ColorChecker measuring tool**
  (`examples/colorchecker.rs`). The two ColorChecker checks wait for it;
  shoot the chart anyway.
- **Models**, each downloaded once from its panel in the app: SAM 2.1
  (Object masks), AI denoise, the landscape model (Sky and landscape),
  object removal, the depth model (Lens Blur). Fetch them on a good
  connection before starting.

## 1. Library and files

- [ ] **Text size and spacing** (library and export, from 2026-10-21). At
  each Text size (Settings → Appearance, ⌘+ / ⌘−), look at the grid,
  Develop, the export dialog and Settings, on the laptop screen and an
  external one. Passes: nothing clipped or overlapping.
- [ ] **Virtual copies as tiles** (library and export, from 2026-10-21), on
  a few real photos, NEF+JPG stacks too. Passes: S opens the stack with
  each copy showing its own edit; D on a copy opens that copy; Delete Copy
  from its menu leaves the file alone.
- [ ] **Sony ARW, PSD, WebP and MPG** (from 2026-10-25), on the
  library's own: the NEX-3 ARWs of 30 Jun 2018, the Photoshop documents
  of 23 Oct 2004 and 14 Aug 2010 (a panorama beside its frames), the WebPs
  of 2023 and the MPG clips of 28 Jul 2018. Passes:
  - each ARW shows the whole 3:2 frame (not the camera's 16:9 preview) and
    leads an `ARW+JPG` stack; Info shows the date, camera and lens; the
    loupe at 100% is sharp; Develop opens it as a raw and a default render
    looks like its JPEG; Lens corrections find the lens (or say they
    don't);
  - each PSD shows its flattened image at its full size and leads its
    stack over the JPEG of the same name; WebPs show like PNGs;
  - each MPG shows a poster frame, dated by its sidecar (28 Jul 2018, not
    when Drive wrote it), and opens in QuickTime Player;
  - Find duplicates includes them (an ARW and its JPEG are a stack, not
    duplicates).
- [ ] **Duplicates beside other work** (from 2026-10-25), on the whole
  library with hashes not yet cached (a first run takes hours): while
  Finding duplicates runs, Move to Trash a photo (and ⌘Z it back), rename
  one and rate some; each happens at once, and Activity shows the search
  and the move as separate rows. When it finishes: matches show a
  percentage (100% only for identical files and the same image data);
  dragging Show matches ≥ narrows and widens the list at once, with no
  Find again; Auto-select at 100% marks only 100% copies to move, and
  lower with Only taken at the same moment leaves burst frames
  undecided. Note what Show matches ≥ needs to be to find the real
  copies without too many look-alikes (DECISIONS: the default).
- [ ] **A camera DNG from another app** (from 2026-10-22): a DNG from Adobe
  DNG Converter (of a Z f NEF, with its default medium preview) and one
  from a phone or another camera (a Pixel or iPhone ProRAW, a Leica or
  Ricoh), each in a folder with its JPEG if it has one. Passes:
  - the grid shows each at once (from its preview), upright; the loupe at
    100% is sharp (rendered from the raw data where the preview is
    smaller), with colours like Preview's;
  - Info shows the date, camera, lens and the full size;
  - the converted DNG stacks under its NEF (NEF primary, `NEF+DNG`); the
    phone's makes a `DNG+JPG` stack;
  - Develop opens each as a raw (Raw section with Highlights and Demosaic,
    AI denoise offered); a default render looks like the NEF's. Note
    anything rawler refuses (the message in Develop);
  - duplicates: copy the DNG to another folder and change its rating in
    another app: found as the same image data.
- [ ] **An AVIF from an iPhone or elsewhere** (from 2026-10-22): an AVIF
  from an iPhone (or one saved by Photoshop, Squoosh or a browser), one in
  Display P3 and one turned (portrait), and Shoebox's own AVIF export
  beside the original (made in [AVIF exports](#9-export)). Passes:
  - grid and loupe show them upright, with Preview's colours (the P3 one
    keeps its saturated reds and greens);
  - Info shows the date, camera and size;
  - Develop opens each as a finished image; an export from it matches.

- [ ] **Large raws in the cloud only** (from 2026-10-23): in a Google
  Drive streaming library, open a few NEFs that are in the cloud only
  (cloud badge) in the loupe and in Develop. Passes: each opens once it has
  downloaded (a large one may take a while; the log says "still
  downloading"), no "Operation timed out".

- [ ] **Activity while a cloud drive is slow** (from 2026-10-23): in the
  Google Drive library, open a photo that's in the cloud only while Drive
  is busy (just after a large import, or with a big upload under way).
  Passes: Tasks → Activity lists it, and after 10 s it reads "Waiting for
  Google Drive" with the time going up (the status bar says the same);
  while it waits, marking, rejecting and undo still work, and grid
  thumbnails of other cloud-only photos show as in the cloud rather than
  waiting; the row goes once the photo opens.

- [ ] **Find dates on real undated photos** (from 2026-10-23), on an APFS
  clone of the library (`cp -cR`), not the library itself. Open Tasks →
  Find dates. Passes: nothing runs until Look for clues; it counts the
  undated photos as Grid's Undated does; the working panel shows each
  stage and photo as it goes (no long silent wait at the start), and Stop
  stops it; Find Date… on one photo looks at it alone;
  the clues arrive in Activity's "Finding dates" and the groups make sense
  (WhatsApp and screenshot names, `-photos` edits by their names, scans by
  their folders); spot-check ten suggestions against what you know; Accept
  on a group and a typed month on another write only `.xmp` files (the
  dry run lists them), the photos move to their place in Grid, the Info
  panel shows "March 2019 (approximate)" for the month, and ⌘Z puts both
  back. Run Tasks → Duplicates first once, so the same-picture clue has
  hashes to compare with; carved previews should then find their NEFs,
  `-photos` edits their originals, and frames between dated ones their
  day. *Show beside…* opens Grid at photos of that day. With Search and People
  on, scans with a date printed on them (a film camera's orange imprint)
  are read; look-alike and people hints look sensible and are never what
  Accept takes; scene hints (a Christmas tree) show beside their groups.
  Accept all sure ones lists only sure suggestions (spot-check a few) and
  writes them as one step; Move them… then lists the `_undated/` photos
  with their new places (exact dates named by the template, days in their
  day folder with their own names), and ⌘Z puts them back. Note clues that
  were wrong or missing, and how long the picture pass takes for the
  library's undated photos.

## 2. Raw decode and colour

- [ ] **30 NEFs render cleanly** (Phase 1 gate, deferred 2026-10-20). The
  30-NEF set: a mix of low light, high ISO, backlit, landscape with sky,
  portrait, architecture and blown highlights, some High efficiency and
  some Lossless. Each one passes:
  - it opens in about 1.5 s;
  - there are no demosaic artefacts at 100%;
  - blown highlights go neutral white;
  - "As shot" white balance matches the camera JPEG, and the eyedropper
    makes grey neutral;
  - colour and tone look plausible beside the camera JPEG;
  - the basic edits work and are still there on reopening;
  - JPEG and 16-bit TIFF exports match Develop.

  The same 30 serve [Auto on the 30](#3-develop-tone-colour-and-effects),
  [the 30-NEF review](#10-last-the-gate-verdicts-and-goldens) and the
  goldens.
- [ ] **ColorChecker, mean ΔE2000 ≤ 3** with the matrix profile (Phase 1
  gate, deferred 2026-10-20). Shoot it with RAW set to Lossless
  compression, at base ISO, in daylight (open shade or overcast), square
  on, with the white patch not clipped. Also take one shot under tungsten,
  for a custom profile later ([the DCP check](#2-raw-decode-and-colour)
  below uses both). Measuring needs `examples/colorchecker.rs`, which
  isn't built yet: say which chart you have (Classic, Passport or Nano).
  Passes: mean ΔE2000 ≤ 3.
- [ ] **A ColorChecker DCP with dcamprof, mean ΔE2000 ≤ 3** (Phase 4; the
  spec's target with a custom DCP is ≤ 1.5). Use the chart shots from the
  check above (daylight and tungsten, Lossless, base ISO, white patch not
  clipped).
  1. Convert both NEFs to DNG (Adobe DNG Converter).
  2. Follow dcamprof's documentation: `make-target` from each shot's
     measured patches, `make-profile` for each, and `make-dcp` with both
     (daylight and tungsten, so the tables blend by temperature), naming
     the camera as its raw files do (`-n "Nikon Z f"`) and without a tone
     curve, so Shoebox's tone stays.
  3. Put the `.dcp` in the folder Develop's profile menu → "Show profiles
     folder" opens, choose "Read profiles again", and pick it on the chart
     photo.
  4. Measure ΔE2000 (the measuring tool isn't built yet).

  Passes: mean ΔE2000 ≤ 3 (aim for ≤ 1.5); the tungsten shot with As shot
  comes out neutral too.
- [ ] **An Adobe profile** (Phase 4): Adobe Standard or Camera Standard for
  the Z f, from Adobe DNG Converter's profiles folder, copied into the
  profiles folder. Try it with and without "Profile's tone curve", beside
  the camera JPEG. Passes: plausible, no hue jumps in skies or skin, no
  banding in smooth gradients; a missing profile (move the file out, read
  again) shows the warning and renders as the matrix.
- [ ] **Fit the camera match profile** (Phase 2). On an APFS clone, collect
  a few hundred varied NEF+JPEG pairs (same names) in one folder, shot with
  the Picture Control you use, and with Active D-Lighting off if you can.
  Run
  `cargo run --release -p shoebox-develop --example camera_match -- <folder> NIKON_Z_f.json`.
  It prints the held-back pairs' mean ΔE2000 with the matrix and with the
  look. Copy the file into Shoebox's `profiles/` folder (beside
  `export-presets.json` in its Application Support folder), and "Camera
  match" appears beside Auto. Passes: the look's ΔE2000 is clearly lower
  than the matrix's, and by eye it matches the camera JPEGs on a few photos
  it wasn't fitted on.
- [ ] **Colour calibration (CAT16) in mixed light** (Phase 4): an interior
  lit by tungsten lamps with daylight from a window, and an LED-lit stage
  or street at night. Turn on Colour calibration and pick the main light
  with the eyedropper; then add a radial or brush layer over the window
  with Temperature/Tint to take the second light out. Passes:
  - neutrals neutral, skin and saturated colours natural (compare with the
    same pick without it);
  - no hue shift at the mask's edge beyond the light's own;
  - deep blue or violet LEDs don't go to flat or negative-looking blotches.

## 3. Develop: tone, colour and effects

- [ ] **Highlights, Shadows, Clarity, Dehaze** (Phase 2) on a few real
  scenes. Passes: no halos at hard edges at 100%.
- [ ] **Auto on the 30** (Phase 2). Passes: rarely far from the camera
  JPEG's brightness. (Pass 1 of [the 30-NEF
  review](#10-last-the-gate-verdicts-and-goldens) judges the same
  renders more fully.)
- [ ] **Smart Lighting** (Phase 3) on backlit portraits, interiors with
  windows and landscapes with bright skies. Passes: shadows open up and
  skies hold, without halos at the horizon or round people; Auto's slight
  setting doesn't look processed.
- [ ] **Smart Lighting, spot weighted** (Phase 3), on backlit portraits.
  Passes: Face puts the box on the largest face (also on a photo turned
  90°) and the face comes up towards a natural brightness while the bright
  background holds; a box dragged over a dark subject does the same; no
  halos round the subject, and the export matches Develop.
- [ ] **Match Total Exposures** (Phase 3) on a bracketed set or a series
  shot in manual with changing light (the [HDR brackets](#8-merges) do).
  Passes: matched photos look equally bright beside the one matched to.
  **Reference view** (same check): edit one photo beside another from the
  same shoot. Passes: the two halves don't flicker or lag.
- [ ] **Colour grading** (Phase 3): a classic teal-shadows/orange-highlights
  grade and a split-toned black and white. Passes: smooth, with no banding
  at the ranges' joins, and black stays black.
- [ ] **Looks (`.cube`)** (Phase 4): a few film-emulation packs (a 33³ and
  a 65³ Adobe cube, a Resolve cube with a shaper, a 1D curve) in the looks
  folder. Passes: each reads (nothing in the log), looks as it does in the
  app it came from on the same JPEG, Amount at 50% and 150% behave, a
  smooth sky shows no banding, and a full-size export matches Develop.
- [ ] **Vignette and grain** (Phase 3): a darkening vignette on a cropped
  photo, and grain at 100% against a full-size export, on a sky. Passes:
  the vignette follows the crop; the grain is the same size and strength
  in both, with no pattern visible.

## 4. Noise and detail

Downloads the AI denoise model once. Use the 10 highest-ISO NEFs for all
three.

- [ ] **Noise reduction** (Phase 2), the classic one, on the 10
  highest-ISO files. Passes: noise clearly lower, detail (hair, foliage,
  text) kept, no blotches.
- [ ] **AI denoise** (Phase 3) on the 10 highest-ISO NEFs (Detail → AI
  denoise, download the model once). Look at bright lights in night shots
  for coloured edges; note the time per photo; export one. Passes: noise
  clearly lower than the classic noise reduction (check above), detail
  (hair, foliage, text) kept, colours unchanged, no tile seams, no
  coloured edges round lights; the export matches Develop.
- [ ] **AI denoise in Auto** (Phase 3; Settings → Raw, "AI denoise in Auto
  from ISO"). Pick the ISO where the classic noise reduction stops being
  enough on the Z f's files (the two checks above show where). On an APFS
  clone, open a NEF above it for the first time. Passes: AI denoise turns
  on, and the result isn't smeared by the classic luminance noise
  reduction left under it (10).

Per-mask AI denoise and sharpening are in
[section 5](#5-local-adjustments-and-masks).

## 5. Local adjustments and masks

Downloads SAM 2.1 and the landscape model once each.

- [ ] **Local adjustments** (Phase 3) on a few real photos: a graduated
  sky (linear, exposure −1), a vignette-like ellipse on a face, a
  brushed-in dodge on a NEF, at 100% and in a full-size export. Passes:
  the masks stay put after rotating 90°, cropping and with lens corrections
  on; the exported JPEG matches Develop; brush strokes feel responsive with
  a large brush; O shows the mask where expected.
- [ ] **Point Color** (Phase 3) on a few real photos (a sky, skin,
  foliage). Passes: a picked colour shifts smoothly with no hard edges or
  speckle where it fades out; Show affected matches what changes; skin
  picked with a narrow range leaves similar backgrounds alone.
- [ ] **Control points** (Phase 4; K) on landscapes and portraits. Passes:
  - a point on a blue sky darkens the sky round it without reaching into
    trees, roofs or a face of a similar brightness;
  - a point on a face warms the skin without the background;
  - a point on a white wall or grey cloud (no colour part) still takes it;
  - the default size and softness feel like PhotoLab's U Point as you
    remember it (don't open PhotoLab; or say which way they should go);
  - the edge of the effect isn't visible as a circle.
- [ ] **Subject and People masks** (Phase 3) on real photos (a portrait, a
  pet, a group, a busy street, a landscape with no subject). Passes: the
  mask follows the subject's outline (hair is the hard part), arrives
  within a second or so, stays right after rotating 90°, and the export
  matches Develop. (Edges at full size: [AI mask edges](#5-local-adjustments-and-masks) below.)
- [ ] **Face masks** (Phase 3; Face skin, Eyes, Eyebrows, Lips, Teeth) on
  portraits, a group and a profile view. Passes: each lands on its part,
  follows a turned photo, and skin smoothing through Face skin leaves the
  eyes sharp. **Background** is the subject's inverse.
- [ ] **Object masks (SAM 2.1)** (Phase 3). Download the model from the
  panel once; on real photos click a person, a car, a tree, a cup. Note how
  long the first click and later clicks take. Passes: the mask takes the
  object, ⌥-clicks trim it, and a photo turned 90° still works.
- [ ] **Sky and landscape masks** (from 2026-10-22; download the landscape
  model from the panel once), on real photos: a clear sky over a city, a
  cloudy sky behind bare trees, a sunset over the sea, a mountain lake, a
  forest path, a night sky, an interior with a window, a portrait with no
  sky. Note the time to the first part on a new photo. Passes:
  - Sky takes the whole sky (clouds and the sun's glow included) and stops
    at the horizon, roofs and branches without a halo when darkened by 2
    stops, at fit, at 100% and in a full-size export;
  - Vegetation, Water, Mountains, Buildings and Ground each take what they
    name and little else (note where a lake's reflection of the sky goes,
    whether snow lands in Mountains or Ground, and whether Buildings taking
    indoor walls is a nuisance);
  - a photo with no sky gets an empty Sky mask; a photo turned 90° works;
  - a second part of the same photo is immediate.
- [ ] **AI mask edges at full size** (guided upsampling, from 2026-10-22): a
  portrait with flyaway hair against a sky, a bare tree against a bright
  sky, and a red flower on green leaves (an edge between two colours
  equally bright), each with a Subject (or People) mask and a strong
  adjustment (exposure −2 on the background). Note the export's time
  against the same photo without the mask. Passes: at 100% and in a
  full-size export, the edge follows the hair, twigs and petals rather than
  going blocky or haloed, and fine strands aren't lost; the export doesn't
  differ visibly from Develop at fit.
- [ ] **AI masks kept on disk** (Phase 3): open a photo with Subject and
  Object masks, close the app, open it again. Passes: the masks are there
  at once ("Finding it…" no more than flickers), and `<cache>/ai-masks`
  holds a PNG per mask. After a macOS update, Subject and People are found
  again (once): tick this part when the next update comes.
- [ ] **AI denoise per mask** (Phase 3) on high-ISO NEFs. Passes:
  - with the global AI denoise off, a brushed mask at +100 denoises only
    where brushed, with no seam along a hard or feathered edge;
  - with the global at 100, a mask at −100 over a face brings back RCD's
    grain there and nowhere else;
  - with a spot healed inside the mask, the spot still looks healed;
  - the 100% view and a full-size export match Develop.
- [ ] **Sharpness per mask** (Phase 3) on a few real photos (eyes in a
  portrait, foliage, a building). Passes:
  - a mask at +60 looks like the global Sharpening at 60 inside it
    (compare with the mask off and the global at 60), at the fitted view,
    at 100% and in a full-size export alike, with no halo or step along a
    feathered edge;
  - −100 over skin softens it about as much in all three without smearing
    larger features;
  - the global Sharpening's radius, detail and masking change the mask's
    sharpening too.

## 6. Retouching and Lens Blur

Downloads the object removal and depth models once each.

- [ ] **Spot removal** (Phase 3) on real blemishes (sensor dust in a sky, a
  spot on skin, a small sign on a wall). Passes: heal leaves no visible
  edge or colour shift at 100% and in a full-size export; the automatic
  source picks a sensible place; clone copies texture exactly.
- [ ] **Red-eye** (Phase 3) on a flash portrait (and a pet photo if there's
  one). Passes: the pupil loses its red without a ring, the iris and skin
  around it stay as they were, and pet eye covers the glow.
- [ ] **Object removal** (from 2026-10-22; Spot removal → Remove, download
  the model from the panel once). On real photos, paint over: a person
  walking across a beach or a square, a power line across a sky (one long
  stroke, and in several), a sign or bin against a wall or hedge, a dust
  bunny in a clear sky, and something on a pattern (tiles, a fence, a
  brick wall). Passes:
  - the fill continues what's around it without a visible patch at fit,
    and at 100% the grain matches (a high-ISO raw especially: no smooth
    band, no speckle that's finer or more colourful than the photo's);
  - a full-size export matches what the proxy showed (same fill, sharper
    or not softer than the surroundings);
  - Feather and Opacity behave; Option-drag erases part of a removal and
    the rest is filled again; moving a removal moves its strokes;
  - reopening the photo shows the fills at once (from disk), and dragging
    other sliders never re-runs the model;
  - the panel's "Removing…" clears within about a second per removal.

  Note where it fails (regular patterns through wide holes are its weak
  spot) and how big an object can go before the fill looks smeared.
- [ ] **Lens Blur on real photos** (from 2026-10-22). Download the depth
  model from the Lens Blur section, then use a portrait, a street scene
  with depth, a close-up of flowers and a night shot with point lights.
  Passes:
  - Show depth: near things light, far dark, edges of people, hair and
    branches following the photo; the focus range tinted where clicked;
  - Set focus on the subject: it stays exactly as sharp as with Lens Blur
    off; the background blurs more the farther it is; no bright or
    coloured outline round the subject, and no sharp "cut-out" edge
    against the blur (look at hair and shoulders at 100%);
  - something nearer than the focus (a foreground branch) blurs over
    what's behind it, softly;
  - Highlight boost makes the night lights' bokeh discs brighter and
    rounder; Six blades makes them hexagons;
  - a mask over a wrongly placed part (a hand the model put too far, say)
    with Depth +30 brings it into focus;
  - drag Amount and Focus range: the view keeps up (expected about 10 ms a
    frame on the GPU, 10–20 under load); note the time to the first blur
    after opening a photo;
  - a full-size export compared with Develop at 100%: the same blur.

## 7. Lens and geometry

- [x] **Nikon's distortion convention** (lens corrections, 2026-10-21):
  confirmed on a NIKKOR Z 24-70mm f/2.8 S frame at 51 mm against the
  camera's own corrected JPEG: 2.5 px RMS with Shoebox's reading, against
  12.2 px for the reverse (DECISIONS, 2026-10-21).
- [ ] **Vignetting from the camera's data** (lens corrections, from
  2026-10-20), on an evenly lit wall or sky at the widest aperture.
  Passes: the corners match the centre with the correction on.
- [ ] **Auto-upright** (Phase 3) on real architecture (a tall building shot
  looking up, an interior, a horizon over water) and on photos without
  strong lines (a portrait, foliage). Passes: Auto straightens the first
  kind without looking stretched and leaves the second nearly alone; Level
  levels the horizon.
- [ ] **Guided upright** (Phase 3) on real architecture. Passes: two guides
  along a tall building's edges make them vertical; a third along a
  roofline or horizon levels it too; the guides land where drawn on a NEF
  turned 90° and on one with lens corrections on; Escape shows the
  corrected photo and turning Guided on again shows the guides to move.
- [ ] **Volume deformation** (wide-angle face fix, from 2026-10-22) on real
  wide photos: a 14–20 mm group photo with faces at the edges and corners,
  a 24 mm street scene with buildings at the sides, and a
  portrait-orientation wide shot. Compare with the camera's or ViewPoint's
  output if there is one (no licensed editors needed: judge against the
  subject). Passes:
  - Spherical at 100% makes corner faces and round things round without
    the centre changing;
  - Cylindrical (horizontal) keeps the buildings' verticals straight;
  - Fix faces finds every face (note any missed or false ones), makes edge
    faces look natural, and leaves door frames, horizons and walls away
    from faces straight (note any bend where a line passes near a face);
  - the zoom it takes to stay filled is acceptable, a 100% region and a
    full-size export match the view, and the field of view shown matches
    the lens.

## 8. Merges

The library has no brackets, focus stacks or panoramas yet: shoot them (see
[Before you start](#before-you-start)). Work in an APFS clone of the
library, never the library itself. The merged DNGs are checked in other
raw editors here too.

### HDR merge

- [ ] **A hand-held 3-frame bracket of a window-lit interior** (from
  2026-10-22): −2, 0, +2 EV (the Z f's AE bracketing), hand-held, a room
  with a bright window and something dark in a corner. Select the three,
  Merge to HDR… (Ctrl+H), defaults. Passes:
  - the dialog lists the three with their exposures, the 0 EV one as the
    reference, and names `<first>-HDR.dng` beside the first;
  - note the time from Merge to the new photo selected (expected about 3 s
    for 3 × 24 MP on an idle M2 Max);
  - at 100%: no double edges anywhere, corners included (alignment); the
    window's view outside holds detail where the 0 EV NEF clipped; the
    dark corner is cleaner than the 0 EV NEF's (compare at the same
    exposure); no banding or seams where the frames hand over (the window
    frame, a lamp's falloff);
  - at the defaults it renders like the 0 EV NEF, the window rolled off;
    Auto brings the window down (exposure no lower than −2) and Highlights
    −100 shows the view outside;
  - the three NEFs are unchanged (Finder dates, and duplicate review
    doesn't pair the DNG with them); the DNG isn't stacked with them; Info
    lists them; ⌘Z sends it to the Trash.
- [ ] **A tripod bracket with a moving tree** (from 2026-10-22): five frames
  ±2 EV in steps of 1 EV on a tripod, branches moving in wind, a person
  walking through one frame if possible. Merge with Align off, then again
  with Align on (both Deghost Medium), and once with Deghost Off. Passes:
  - Deghost Medium: the branches and the person come out once, from the
    reference, without semi-transparent copies; the edge of the deghosted
    area isn't visible (feathered). Off shows the ghosts (the check
    works). Note any ghost left where the reference was clipped (the sky
    behind the branches): the basic deghosting doesn't look there;
  - Align on doesn't move a tripod bracket (no softening against Align off
    at 100%).
- [ ] **The DNG in another raw editor** (from 2026-10-22): open a merged DNG
  in darktable and in RawTherapee (never Lightroom or PhotoLab). Passes:
  - it opens as a linear raw, upright, with the reference's camera and lens
    in its metadata and colours like Shoebox's at the defaults;
  - the window's highlights are there (lower exposure by 2–3 EV): note
    whether either editor clips values above 1.0 before its own highlight
    tools (darktable's highlight reconstruction should be off for it);
  - `exiftool -XMP-shoebox:all` lists the sources and their BLAKE3s.

  (Shoebox's linear DNG export gets the same treatment in
  [Linear DNG exports](#9-export).)

### Focus stacking

- [ ] **A macro stack of 10–30 frames** (from 2026-10-22): a flower or an
  insect at close range on a tripod, focus shift shooting (or focusing by
  hand) from the nearest point to the farthest, wide open or near it.
  Select them all, Merge to Focus Stack…, Pyramid, Align on; then again
  with Depth map. Passes:
  - the dialog lists them with the middle one as the reference and names
    `<first>-Focus.dng` beside the first;
  - note the time from Merge to the new photo selected (expected about
    0.8 s a frame for 24 MP on an idle M2 Max, so 10–25 s) and the app's
    memory in Activity Monitor (expected to level off near 8 GB, not to
    grow with the frame count);
  - at 100%: sharp from the nearest petal to the farthest, no double edges
    anywhere (alignment through the breathing; check the corners); no
    bright or dark rims along edges between near and far (petal against
    background: the halos focus stacks are known for); no flicker of noise
    in the out-of-focus background (it should be as clean as a single
    frame's, or cleaner); fine hairs or bristles crossing something
    farther (Pyramid should keep them, Depth map may lose or ghost them:
    note which);
  - Pyramid against Depth map: colour and contrast of large smooth areas
    (Depth map should match a frame's exactly; note any shift with
    Pyramid), and which looks better where hairs overlap;
  - the edges: the result is a little smaller than a frame (cut to what
    every frame covers): note how much against the breathing between the
    first and last frames;
  - the frames are unchanged and not stacked with the DNG; Info lists them;
    ⌘Z sends it to the Trash.
- [ ] **A landscape with near and far** (from 2026-10-22): 3–5 frames
  hand-held or on a tripod, focused on a foreground rock or flowers, the
  middle distance and the horizon, at f/8–f/11. Passes:
  - merged with defaults: the foreground and the horizon both sharp; the
    sky smooth (no noise added, no blotches where frames hand over); the
    edge of the foreground against the distance without a halo;
  - hand-held: no doubled detail at the corners; a frame that couldn't be
    aligned is named in the notice;
  - a frame that's mostly sharp (the reference sharp nearly everywhere)
    doesn't make the stack softer than it: compare the stack with the
    sharpest frame at 100% in the middle distance (the noise estimate
    comes from the reference; note if fine texture is lost).

### Panorama

- [ ] **A hand-held 5-frame landscape row** (from 2026-10-22): the Z f and a
  lens Lensfun or the NEF knows (the 40 mm), portrait orientation, turning
  about a third of a frame each time, AE on so exposures differ a little, a
  horizon and some sky. Select the five, Merge to Panorama… (Ctrl+M),
  defaults. Passes:
  - the dialog lists the five and names `<first>-Pano.dng` beside the
    first. Note the time from Merge to the new photo selected (expected
    about 7–10 s for 6 × 24 MP on an idle M2 Max) and the size;
  - at 100% along every seam: no double edges, no step in brightness or
    colour (the sky especially: vignetting and exposure), no soft band;
  - the horizon is level and straight (Auto chose Cylindrical); the crop
    leaves no black corner. Try Spherical and Perspective (refused if it's
    wider than 120°) and Crop off (black outside the boundary);
  - the NEFs are unchanged, the DNG isn't stacked with them, Info lists
    them, Develop opens it without lens corrections offered for a lens, ⌘Z
    sends it to the Trash.
- [ ] **A two-row 360°** (from 2026-10-22): hand-held or on a tripod head,
  two rows (about +20° and −20°) all the way round, 16–24 frames, in the
  order a person shoots them (along one row, back along the other).
  Passes:
  - it joins as one (no frame named as overlapping nothing), Auto chooses
    Spherical, the two ends meet at the back (the left and right edges
    continue each other, though they're not blended across), the horizon
    is level all round, no frame upside down or tilted;
  - note the time and the size (scaled to 150 MP if it would be larger:
    the notice says so) and the memory the app reaches (Activity Monitor).
- [ ] **A city scene with people moving** (from 2026-10-22): 3–4 frames of a
  street with people walking through the overlaps. Passes: no half people,
  no ghosts: each person comes out whole from one frame or not at all (the
  seams go round them). Note any cut through someone (the seams are found
  on a canvas of about a megapixel, so small figures in a huge panorama
  may still be cut).
- [ ] **A flat thing from several places** (from 2026-10-22): a mural or a
  long poster shot walking along it (3 frames), and a document scanned in
  overlapping parts. Passes: the notice says it was laid flat; straight
  edges stay straight; no seams.
- [ ] **A lens Lensfun doesn't know** (from 2026-10-22): a row through an
  adapted manual lens (no EXIF lens). Passes: it still stitches (the
  notice says no lens profile was found). Note any vignetting banding at
  the seams and how far edges miss (no distortion correction).

## 9. Export

Most checks above already compare an export with Develop; these cover
export itself.

- [ ] **Exports beside the original** (library and export, from
  2026-10-21). Export a NEF as JPEG beside the original; change the edit;
  Re-export; ⌘Z. Passes: the export lands in the NEF's folder, joins its
  stack (S), and shows in the Info panel; changing the edit makes it
  stale; Re-export puts the new one in place and the old one in the Trash;
  ⌘Z brings the old one back; the export opens in Preview with the right
  colours and metadata.
- [ ] **Exports outside the library** (library and export, from
  2026-10-21). Passes: they aren't listed in the Info panel, and a preset
  pointed at a folder inside the library is refused with a clear message.
- [ ] **Watermark and border** exports (Phase 2): a text and a PNG-logo
  watermark, in a full-size JPEG and a Web 2048 export. Passes: both look
  crisp at 100%.
- [ ] **Soft proofing** (Phase 2) on a few very saturated photos (flowers,
  neon, sunsets), with S on and sRGB chosen. Passes: the preview matches an
  sRGB JPEG export of the same edit opened in Preview, and Gamut marks the
  colours that changed.
- [ ] **AVIF exports** (from 2026-10-22). Export a saturated photo (flowers,
  a sunset) as Display P3 and as sRGB AVIF at the defaults (quality 85,
  10-bit, speed 6). Passes:
  - both open in Preview and Photos with Develop's colours, matching a P3
    JPEG of the same edit;
  - at 100%, skies show no banding and detail holds up close to JPEG 92;
  - note the time per 24 MP photo on an idle Mac (expected 2–3 s with the
    render);
  - with "All" metadata, Finder's Get Info shows the camera, lens and date.

  Export one beside the original too, for
  [An AVIF from an iPhone](#1-library-and-files).
- [ ] **Linear DNG exports** (from 2026-10-22). Export a high-ISO NEF twice,
  once with AI denoise at Strength 100 and once without it. Open both in
  Preview and in darktable or RawTherapee (never Lightroom or PhotoLab).
  Passes:
  - white balance and colours look like that editor's rendering of the NEF
    itself;
  - the AI one is clean at 100%, with no smearing or colour blotches;
  - the editor finds the lens from EXIF (darktable's lens correction
    module);
  - a photo shot in portrait orientation comes out upright;
  - Finder shows a thumbnail.

  Note the file size, and how bright it looks next to the NEF in each
  editor (no BaselineExposure is written).
- [ ] **A Shoebox linear DNG reopened** (from 2026-10-22). Export a
  high-ISO NEF as a linear DNG beside the original, once with AI denoise at
  100 and once at 40. Passes:
  - each joins the NEF's stack as a variant, listed in the Info panel;
    changing the edit flags it stale, and Re-export replaces it (⌘Z brings
    the old one back), as in [Exports beside the original](#9-export);
  - its thumbnail and loupe look like the NEF's default rendering;
  - in Develop it renders like the NEF at the same settings, the noise as
    clean as the NEF's with AI denoise; Detail says "AI denoise: in this
    linear DNG (100%)" (or 40%), and Raw offers only the levels;
  - exported again (JPEG, and linear DNG), the files match a JPEG exported
    from the NEF with AI denoise at the same strength.

## 10. Last: the gate verdicts and goldens

Once everything above is done and the 30 are edited.

- [ ] **Daily driver for low-ISO shots** (Phase 2 gate; added 2026-10-22
  from EDITING-SPEC's roadmap). Take a real low-ISO shoot through Shoebox
  start to finish: cull, edit, export. Passes: nothing missing or broken
  sends you to another editor. The spec gives no number: note what you
  judged on and anything that got in the way.
- [ ] **The 30-NEF review** (Phase 3 gate, EDITING-SPEC's definition of
  done; added 2026-10-22). Review the 30 at 100% and at fit-to-screen, in
  two passes: Pass 1 judges Shoebox's Auto rendering with no manual edits;
  Pass 2 judges each photo edited normally. The camera's own JPEG of each
  frame is the only outside reference. Passes: acceptable on at least 27
  of 30 in each pass. Note the ones that fail and why.
- [ ] **Golden images of the 30** (Phase 1, deferred 2026-10-20). Once
  they're edited, put the NEFs and their edit files
  (`.shoebox/edits/<name>.NEF.json`) in one folder, then store the
  goldens:
  `SHOEBOX_GOLDEN_DIR=<folder> SHOEBOX_GOLDEN_BLESS=1 cargo test -p shoebox-develop --test golden real_goldens`.
  From then on, the same command without `SHOEBOX_GOLDEN_BLESS` catches
  any change to the renders.

## 11. iCloud Photos (a test macOS account)

Independent of the others: PhotoKit only opens the Mac's own Photos
library, so these run in a separate macOS user account whose Photos
library is a test library (CLAUDE.md, safety rule 9), never in yours.

- [ ] **The Photos probe** (PHOTOS-SPEC Phases 0 and 3; added 2026-10-23).
  Set up once:
  1. System Settings → Users & Groups: add a standard user, "Shoebox Test".
     Log in to it. For the iCloud facts, sign it in to a throwaway Apple ID
     with iCloud Photos on; without one the probe still runs, but no asset
     has a cloud id (fact 2) and nothing downloads from iCloud.
  2. In its Photos: import `cargo run -p shoebox-fixtures -- <empty
     folder>`'s photos, a few screenshots (no EXIF date) and a short movie;
     make an album **Shoebox test library** and one inside a folder
     (`Trips › Japan`); favourite two photos, hide one, and edit one in
     Photos (Adjust, then Done).
  3. Build the bundle in your own account (`scripts/bundle-macos.sh`), copy
     `target/release/bundle/Shoebox.app` to `/Users/Shared`, and in the test
     account run
     `/Users/Shared/Shoebox.app/Contents/MacOS/Shoebox --photos-probe /Users/Shared/probe`
     (a new folder). Allow Photos access when asked.

  Passes: `report.txt` says fact 1 "same bytes" for every sample, fact 2
  names how many assets have cloud ids (all of them with iCloud on), the
  edit shows under "Edits by who made them" as Photos', the albums are
  listed with their folders; the sync copies every planned asset (hidden and
  untouched burst frames left out, the screenshots "dated by Photos"), "Sync
  again" has 0 new, and "Undone" leaves 0 records. Then run it again into
  another new folder with `--keep` after the folder, and look at
  `<folder>/library`: day folders named as card imports are, each Live
  Photo's movie beside its photo, the edited photo's `-photos` copy beside
  it and dated as it is (in its day, not under Undated), the favourites rated ★5 and the album photos in its event (open the
  folder in Shoebox in the test account to see). Copy `report.txt` into
  DECISIONS ("iCloud Photos: PhotoKit").
- [ ] **Import from iCloud Photos in the app** (PHOTOS-SPEC Phase 4; added
  2026-10-23), in the test account, after the probe. Open
  `/Users/Shared/Shoebox.app` with `SHOEBOX_DATA_DIR` pointing at a scratch
  folder, add an empty library folder, then Tasks › Import › iCloud
  Photos › Find photos. Passes: the counts match the probe's plan; the
  album table shows the albums with their folders; Import copies them with
  progress and a speed; Stop part-way keeps what's copied and the next
  Find photos offers only the rest; quitting mid-copy asks first; the Info
  panel says "From iCloud Photos" on a copy; favourites have ★5, the
  album's photos are in its event; History's Undo takes the copies back.
- [ ] **Send to Photos** (PHOTOS-SPEC Phase 5; added 2026-10-23), in the
  test account, after the import. Edit a photo that came from Photos and
  one of the library's own (a fixture NEF) in Develop; select both, Send to
  Photos…. Passes: the table says "An edit of its photo in Photos" and "A
  new photo in Photos"; macOS asks to allow the change; in Photos the
  edited one shows the edit and Image › Revert to Original brings the
  original back; the NEF is a new JPEG with its date; with "Add to the
  album" it's in that album. Sending again: "up to date"; change the edit
  and send: "Updates what was sent before", and Photos has no second copy.
  Find photos in Import: nothing new. ⌘Z: macOS asks to delete the new one
  (Recently Deleted) and the edit is reverted. Also edit the photo edited
  earlier in Photos and send it: the table warns its Photos edit is
  replaced and names the `-photos` copy.

