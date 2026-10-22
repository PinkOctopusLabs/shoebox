# Manual checks

Checks that need new photos or a person's eye, kept here until they're
done. Run them on an APFS clone of the library (`cp -cR Photos Photos-test`)
with a throwaway build (`SHOEBOX_DATA_DIR=<scratch>`), never on the real
library. Tick them off (with the date and the result) as they're done; when a
check fails, note the file and what's wrong, and copy the file into
`tests/fixtures/real/` (gitignored).

**All of these wait until every editing phase is built** (decided
2026-10-22): new checks are added here as features land, and they're run
together at the end, before the release.

## Phase 1 gate (deferred 2026-10-20)

- [ ] **30 NEFs render cleanly.** A mix of low light, high ISO, backlit,
  landscape with sky, portrait, architecture and blown highlights, some
  High efficiency and some Lossless. Each one passes these checks:
  - it opens in about 1.5 s;
  - there are no demosaic artefacts at 100%;
  - blown highlights go neutral white;
  - "As shot" white balance matches the camera JPEG, and the eyedropper
    makes grey neutral;
  - colour and tone look plausible beside the camera JPEG;
  - the basic edits work and are still there on reopening;
  - JPEG and 16-bit TIFF exports match Develop.
- [ ] **ColorChecker, mean ΔE2000 ≤ 3** with the matrix profile. Shoot it
  with RAW set to Lossless compression, at base ISO, in daylight (open shade
  or overcast), square on, with the white patch not clipped. Also take one
  shot under tungsten, for a custom profile later. This needs the
  measuring tool (`examples/colorchecker.rs`), which isn't built yet: say
  which chart you have (Classic, Passport or Nano).
- [ ] **Golden images of the 30.** Once they're edited, put the NEFs and
  their edit files (`.shoebox/edits/<name>.NEF.json`) in one folder, then
  store the goldens:
  `SHOEBOX_GOLDEN_DIR=<folder> SHOEBOX_GOLDEN_BLESS=1 cargo test -p shoebox-develop --test golden real_goldens`.
  From then on, the same command without `SHOEBOX_GOLDEN_BLESS` catches
  any change to the renders.

## Lens corrections (from 2026-10-20)

- [x] **Nikon's distortion convention** (2026-10-21): confirmed on a NIKKOR
  Z 24-70mm f/2.8 S frame at 51 mm against the camera's own corrected
  JPEG: 2.5 px RMS with Shoebox's reading, against 12.2 px for the reverse
  (DECISIONS, 2026-10-21).
- [ ] **Vignetting from the camera's data**, on an evenly lit wall or sky
  at the widest aperture: the corners match the centre with the correction
  on.

## Phase 2 (as features land)

- [ ] **Highlights, Shadows, Clarity, Dehaze** on a few real scenes: no
  halos at hard edges at 100%.
- [ ] **Noise reduction** on the 10 highest-ISO files: noise clearly lower,
  detail (hair, foliage, text) kept, no blotches.
- [ ] **Auto** on the 30: rarely far from the camera JPEG's brightness.
- [ ] **Soft proofing** on a few very saturated photos (flowers, neon,
  sunsets): with S on and sRGB chosen, the preview matches an sRGB JPEG
  export of the same edit opened in Preview, and Gamut marks the colours
  that changed.
- [ ] **Watermark and border** exports: a text and a PNG-logo watermark
  look crisp at 100% in a full-size JPEG and a Web 2048 export.
- [ ] **Fit the camera match profile.** On an APFS clone, collect a few
  hundred varied NEF+JPEG pairs (same names) in one folder. Shoot them with
  the Picture Control you use, and with Active D-Lighting off if you can.
  Then run
  `cargo run --release -p shoebox-develop --example camera_match -- <folder> NIKON_Z_f.json`.
  It prints the held-back pairs' mean ΔE2000 with the matrix and with the
  look; the look should be clearly lower. Copy the file into Shoebox's
  `profiles/` folder (beside `export-presets.json` in its Application
  Support folder), and "Camera match" appears beside Auto. Then check it
  by eye against the camera JPEGs on a few photos it wasn't fitted on.

## Library and export (from 2026-10-21)

- [ ] **Text size and spacing** at each Text size (Settings → Appearance,
  ⌘+ / ⌘−): nothing clipped or overlapping in the grid, Develop, the
  export dialog and Settings, on the laptop screen and an external one.
- [ ] **Virtual copies as tiles** on a few real photos (NEF+JPG stacks
  too): S opens the stack with each copy showing its own edit; D on a copy
  opens that copy; Delete Copy from its menu leaves the file alone.
- [ ] **Exports beside the original**: a JPEG export of a NEF lands in its
  folder, joins its stack (S), and shows in the Info panel; change the
  edit, it goes stale; Re-export puts the new one in place and the old one
  in the Trash; ⌘Z brings the old one back. Check the export opens in
  Preview with the right colours and metadata.
- [ ] **Exports outside the library** aren't listed in the Info panel, and
  a preset pointed at a folder inside the library is refused with a clear
  message.

## Phase 3 (as features land)

- [ ] **Local adjustments** on a few real photos: a graduated sky
  (linear, exposure −1), a vignette-like ellipse on a face, a brushed-in
  dodge on a NEF at 100% and in a full-size export: the masks stay put
  after rotating 90°, cropping and with lens corrections on; the
  exported JPEG matches Develop; brush strokes feel responsive with a
  large brush; O shows the mask where expected.
- [ ] **Point Color** on a few real photos (a sky, skin, foliage): a
  picked colour shifts smoothly with no hard edges or speckle where it
  fades out, Show affected matches what changes, and skin picked with a
  narrow range leaves similar backgrounds alone.
- [ ] **Spot removal** on real blemishes (sensor dust in a sky, a spot
  on skin, a small sign on a wall): heal leaves no visible edge or colour
  shift at 100% and in a full-size export; the automatic source picks a
  sensible place; clone copies texture exactly.
- [ ] **Red-eye** on a flash portrait (and a pet photo if there's one):
  the pupil loses its red without a ring, the iris and skin around it
  stay as they were, and pet eye covers the glow.
- [ ] **Auto-upright** on real architecture (a tall building shot
  looking up, an interior, a horizon over water) and on photos without
  strong lines (a portrait, foliage): Auto straightens the first kind
  without looking stretched and leaves the second nearly alone; Level
  levels the horizon.
- [ ] **Guided upright** on real architecture: two guides along a tall
  building's edges make them vertical; a third along a roofline or
  horizon levels it too; the guides land where drawn on a NEF turned 90°
  and on one with lens corrections on; Escape shows the corrected photo
  and turning Guided on again shows the guides to move.
- [ ] **Match Total Exposures** on a bracketed set or a series shot in
  manual with changing light: matched photos look equally bright beside
  the one matched to. **Reference view**: edit one photo beside another
  from the same shoot and check the two halves don't flicker or lag.
- [ ] **Subject and People masks** on real photos (a portrait, a pet, a
  group, a busy street, a landscape with no subject): the mask follows
  the subject's outline (hair is the hard part), arrives within a second
  or so, stays right after rotating 90°, and the export matches Develop.
- [ ] **Object masks (SAM 2.1)**: download the model from the panel
  once; on real photos click a person, a car, a tree, a cup: the mask
  takes the object, ⌥-clicks trim it, and a photo turned 90° still
  works. Note how long the first click and later clicks take.
- [ ] **AI denoise** on the 10 highest-ISO NEFs (Detail → AI denoise,
  download the model once): noise clearly lower than the classic noise
  reduction, detail (hair, foliage, text) kept, colours unchanged, no
  tile seams; look at bright lights in night shots for coloured edges;
  note the time per photo. Export one and check it matches Develop.
- [ ] **AI denoise per mask** on high-ISO NEFs: with the global AI
  denoise off, a brushed mask at +100 denoises only where brushed, with
  no seam along a hard or feathered edge; with the global at 100, a mask
  at −100 over a face brings back RCD's grain there and nowhere else;
  with a spot healed inside the mask, the spot still looks healed; the
  100% view and a full-size export match Develop.
- [ ] **Sharpness per mask** on a few real photos (eyes in a portrait,
  foliage, a building): a mask at +60 looks like the global Sharpening
  at 60 inside it (compare with the mask off and the global at 60), at
  the fitted view, at 100% and in a full-size export alike, with no halo
  or step along a feathered edge; −100 over skin softens it about as
  much in all three without smearing larger features; the global
  Sharpening's radius, detail and masking change the mask's sharpening
  too.
- [ ] **AI denoise in Auto** (Settings → Raw, "AI denoise in Auto from
  ISO"): pick the ISO where the classic noise reduction stops being
  enough on the Z f's files; on an APFS clone, opening a NEF above it
  for the first time turns AI denoise on and the result isn't smeared by
  the classic luminance noise reduction left under it (10).
- [ ] **Smart Lighting** on backlit portraits, interiors with windows
  and landscapes with bright skies: shadows open up and skies hold,
  without halos at the horizon or round people; Auto's slight setting
  shouldn't look processed.
- [ ] **Colour grading**: a classic teal-shadows/orange-highlights grade
  and a split-toned black and white: smooth, with no banding at the
  ranges' joins, and black stays black.
- [ ] **Vignette and grain**: a darkening vignette on a cropped photo
  (it follows the crop), and grain at 100% against a full-size export
  (the same size and strength), on a sky (no pattern visible).
- [ ] **Face masks** (Face skin, Eyes, Eyebrows, Lips, Teeth) on
  portraits, a group and a profile view: each lands on its part, follows
  a turned photo, and skin smoothing through Face skin leaves the eyes
  sharp. **Background** is the subject's inverse.
- [ ] **Smart Lighting, spot weighted**: on backlit portraits, Face puts
  the box on the largest face (also on a photo turned 90°) and the face
  comes up towards a natural brightness while the bright background
  holds; a box dragged over a dark subject does the same; no halos round
  the subject, and the export matches Develop.
- [ ] **AI mask edges at full size** (guided upsampling, from
  2026-10-22): a portrait with flyaway hair against a sky, a bare tree
  against a bright sky, and a red flower on green leaves (an edge
  between two colours equally bright), each with a Subject (or People)
  mask and a strong adjustment (exposure −2 on the background). At 100%
  and in a full-size export, the edge follows the hair, twigs and petals
  rather than going blocky or haloed, and fine strands aren't lost; the
  export doesn't differ visibly from Develop at fit. Note the export's
  time against the same photo without the mask.
- [ ] **AI masks kept on disk**: open a photo with Subject and Object
  masks, close the app, open it again: the masks are there at once
  ("Finding it…" no more than flickers), and `<cache>/ai-masks` holds a PNG per mask. After a
  macOS update, Subject and People are found again (once).

## Phase 4 (as features land)

- [ ] **Control points** (K) on landscapes and portraits: a point on a
  blue sky darkens the sky round it without reaching into trees, roofs
  or a face of a similar brightness; a point on a face warms the skin
  without the background; a point on a white wall or grey cloud (no
  colour part) still takes it. The default size and softness feel like
  PhotoLab's U Point (or say which way they should go), and the edge of
  the effect isn't visible as a circle.
- [ ] **Sky and landscape masks** (from 2026-10-22; download the
  landscape model from the panel once): on real photos (a clear sky
  over a city, a cloudy sky behind bare trees, a sunset over the sea, a
  mountain lake, a forest path, a night sky, an interior with a window,
  a portrait with no sky): Sky takes the whole sky (clouds and the sun's
  glow included) and stops at the horizon, roofs and branches without a
  halo when darkened by 2 stops, at fit, at 100% and in a full-size
  export; Vegetation, Water, Mountains, Buildings and Ground each take
  what they name and little else (note where a lake's reflection of the
  sky goes, whether snow lands in Mountains or Ground, and whether
  Buildings taking indoor walls is a nuisance); a photo with no sky gets
  an empty Sky mask; a photo turned 90° works. Note the time to the
  first part on a new photo, and that a second part of it is immediate.
