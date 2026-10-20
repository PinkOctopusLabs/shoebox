# Manual checks

Checks that need new photos or a person's eye, kept here until they're
done. Run them on an APFS clone of the library (`cp -cR Photos Photos-test`)
with a throwaway build (`SHOEBOX_DATA_DIR=<scratch>`), never on the real
library. Tick them off (with the date and the result) as they're done; when a
check fails, note the file and what's wrong, and copy the file into
`tests/fixtures/real/` (gitignored).

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

- [ ] **Nikon's distortion convention.** Take a frame with straight lines
  (a building or a door frame) near the edges, with the most distorting
  lens you have. With Lens corrections on and Lensfun overridden (choose
  a different lens in the menu, or a lens Lensfun doesn't have), the lines
  should come out straight using only the camera's data. If they bow
  more instead, the convention is the other way round (`lens::make`; see
  DECISIONS, Lens corrections).
- [ ] **Vignetting from the camera's data**, on an evenly lit wall or sky
  at the widest aperture: the corners match the centre with the correction
  on.

## Phase 2 (as features land)

- [ ] **Highlights, Shadows, Clarity, Dehaze** on a few real scenes: no
  halos at hard edges at 100%.
- [ ] **Noise reduction** on the 10 highest-ISO files: noise clearly lower,
  detail (hair, foliage, text) kept, no blotches.
- [ ] **Auto** on the 30: rarely far from the camera JPEG's brightness.
