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
