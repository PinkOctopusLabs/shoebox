# Open items

What's left open now that every library milestone (`docs/SPEC.md`) and all
four editing phases (`docs/EDITING-SPEC.md`) are built: choices made
provisionally, defaults that are guesses, known defects, possible
enhancements, and what's out of scope for now. Gathered on 2026-10-22 from
`docs/DECISIONS.md`, the specs, `docs/MANUAL-CHECKS.md` and the code (which
has no `TODO` or `FIXME` comments).

- **Adding:** anything left open goes here as well as in DECISIONS, under the
  right section: a bold name, a sentence or two, where it comes from
  (`DECISIONS "Section"` or a file) and, for enhancements, a rough size (S: an
  afternoon, M: a few days, L: a week or more).
- **Closing:** strike the item through (`- [x] ~~…~~`) with the commit that
  closed it, or remove it once that's in DECISIONS. A decision made goes to
  DECISIONS as usual; this file only points at it.
- **Manual checks:** many tuning questions here are answered by a check in
  `docs/MANUAL-CHECKS.md`; each names it ("check: …"). The checks themselves
  stay there. Where nothing answers a question yet, it says "no check yet":
  add one when the item is picked up.

## Needs a decision

Provisional choices the person should confirm or change.

- [ ] **iCloud Photos: an album's event renamed in Shoebox.** For now the
  Shoebox name sticks and Photos' later renames of the album don't follow,
  which can leave one name in Shoebox and another in Photos. Source:
  DECISIONS "iCloud Photos: events follow their albums".

- [ ] **iCloud Photos (M7): the probe in a test account.** Phases 1–3
  are built, but no PhotoKit code has run against a Photos library yet. The
  probe (`--photos-probe`, MANUAL-CHECKS 11) checks the facts the sync
  rests on: originals byte-identical, cloud ids, who edited, and how long
  listing 30k assets takes. Adjustment data round-tripping waits for push
  (Phase 5). Source: PHOTOS-SPEC "Plan"; DECISIONS "iCloud Photos: PhotoKit".
- [ ] **Spec items not built and not dropped.** Neither built nor recorded
  as out of scope: AMaZE demosaic ("max detail"), export output sharpening
  (screen, matte, glossy), preset packs (`.shoeboxpack`), Lightroom XMP
  preset import, Hald CLUT looks, a preset applied on import per folder,
  and SPEC's optional renaming of day folders after an event. Decide each:
  build (see Enhancements) or move to Out of scope. Source: EDITING-SPEC
  (Demosaic algorithms, Auto mode, presets and packs, Export), SPEC (Events).
- [ ] **Linear DNG export: 16-bit or float.** Now 16-bit unsigned LinearRaw
  through rawler's writer, clipped at the sensor's white, so Shoebox's
  reconstructed highlights are lost. Merges already write half-float DNGs
  (`encode::float_dng`), which keep them. Recommendation: reuse the float
  writer once "The DNG in another raw editor" shows darktable and
  RawTherapee read floats well. Source: DECISIONS "AVIF and linear DNG
  export".
- [ ] **Linear DNG goes stale on any edit change.** It holds only the raw
  settings and AI denoise, yet a look-only change flags it. Alternative: a
  staleness key from the raw part of the edit alone. Recommendation: narrow
  it. Source: DECISIONS "DNG and AVIF in the library".
- [ ] **AVIF default speed.** 6 (libavif's default) against 10: the same
  PSNR, files about 10% larger, 4–5× faster. Speed is a preset setting
  either way. Source: DECISIONS "AVIF and linear DNG export".
- [ ] **Stem sidecar (`x.xmp`) for DNGs.** Written for NEFs only: Lightroom
  reads a DNG's marks from inside it, so a stem sidecar would be read by
  nothing and shared with a NEF of the same name. Recommendation: keep.
  Source: DECISIONS "DNG and AVIF in the library".
- [ ] **DCP: profile tone curve off by default.** On, it replaces the
  display transform (Contrast, Whites, Blacks then do nothing). Recommendation:
  keep off; Auto and the baseline exposure are tuned to Shoebox's own curve.
  Source: DECISIONS "DNG camera profiles".
- [ ] **DCP: departures from the DNG SDK.** Values not pinned to 1;
  saturation only kept from passing the gamut's edge; sRGB encoding applied
  to 2.5D tables too; ProfileToneCurve linear between points, not a spline;
  DefaultBlackRender read but ignored. Confirm, or match the SDK where
  check "An Adobe profile" shows a difference. Source: DECISIONS "DNG camera
  profiles".
- [ ] **Looks: space and a preset's Amount.** `.cube` files are assumed
  sRGB-encoded Rec.709 (the spec said "the LUT's declared space (sRGB or
  P3)"), so a look keeps wide-gamut colours only at Amount 0; a per-look
  space choice is the alternative. A preset applied below 50% leaves its
  look out altogether (the rule for anything not a number);
  recommendation: always carry the look and scale its Amount from 0.
  Source: DECISIONS "Looks (`.cube`)".
- [ ] **Landscape mask classes.** Walls and floors count as Buildings and
  Ground indoors too; Ground is one part, not Lightroom's natural and
  artificial; ADE20K has no snow. Regrouping changes what's stored, so it
  needs a `scene::MODEL_ID` bump. Check: "Sky and landscape masks". Source:
  DECISIONS "Sky and landscape masks". (The model itself, ConvNeXt-B f32,
  489 MB, is settled: "Wave 1 follow-ups, decided".)
- [ ] **Control points: key and tool.** K is free in Develop, but it's
  Lightroom's adjustment brush key; the brush has no key, so give it another
  one (or move K). Placing ends after each point, where PhotoLab's tool stays
  on. Source: DECISIONS "Control points".
- [ ] **HDR reference and exposures.** The reference is the middle exposure
  (the brief said the first source) and exposure ratios are measured from
  the pixels, EXIF only the start and fallback (the brief had EXIF first).
  Confirm. Source: DECISIONS "HDR merge".
- [ ] **Raw white point on a linear DNG clips above 1.0.** Moving it on an
  HDR merge clips the range the merge kept. Alternative: scale without
  clipping for linear sources. Source: DECISIONS "HDR merge".
- [ ] **No lens corrections before merging.** Merges use Lensfun from the
  copied EXIF; the NEF's own lens data (Nikon's, read from NEFs only) is
  lost in the result. Alternatives: copy NikonNEFInfo into the DNG, or
  correct HDR and focus frames before merging as panorama does. Source:
  DECISIONS "Photo merges: groundwork".
- [ ] **Two merges of one first photo stack together** (`-HDR`, `-HDR-1`)
  as variants, by the name rule. Left so. Source: DECISIONS "Photo merges:
  groundwork".
- [ ] **Focus stack: cut or full frame; fixed reference.** The result is cut
  to what every frame covers (0.9% a side for ±1.35% breathing); keeping
  the full frame, edges from fewer frames, is the alternative. The middle
  frame is the reference and can't be changed. Source: DECISIONS "Focus
  stacking".
- [ ] **Panorama: one per merge.** A frame that overlaps none is refused by
  name; Brown and Lowe would make several panoramas. Source: DECISIONS
  "Panorama".
- [ ] **Lens Blur's default focus.** The nearest tenth of the depth map
  (its 90th percentile); Vision's subject is the alternative. Source:
  DECISIONS "Lens Blur".
- [ ] **Volume deformation always fills the frame** (zooming up to 1.17×
  at Spherical 100% on a 24 mm equivalent); no option keeps the blank
  corners. Source: DECISIONS "Volume deformation".
- [ ] **Rejects also as an XMP rating of −1** (Bridge's reject): PickLabel
  only for now, "may become a per-library setting later". Source: SPEC,
  Open questions.

## Bugs

- [ ] **UI tests fail under heavy load.** `virtual_copies` failed in most
  full runs from 2026-10-22, at its right-click on the copy (the copies
  row, then the menu's Delete, not on screen in 30 s); on 2026-10-23
  `develop_upright_levels_a_tilted_photo` and
  `develop_colour_grading_wheel` failed the same way, and on 2026-10-24
  `find_dates_reads_text_in_the_picture` (Vision's first load took about
  45 s alone). They pass alone. A
  bisect (pass at e22cdf8, fail at 3444a2b) wasn't conclusive: the Mac's
  load average was 18–25 (Photos' mediaanalysisd, several users logged
  in). The test now waits for the row and the menu item rather than a
  fixed number of frames. Rerun on a quiet machine; if it still fails,
  bisect again. S. Source: these runs.
- [ ] **A sidecar date stays in the metadata cache after it's removed.**
  A capture time from a sidecar (iCloud Photos' date, or one set in another
  app) is applied to the photo's metadata, which is cached; if the sidecar
  date is later deleted, the old one stays until the image itself changes
  or the cache is rebuilt. S. Source: DECISIONS "iCloud Photos: the core".

Small known defects.

- [ ] **Mask brush loses the start of each stroke.** `views/local.rs`
  starts a stroke where the drag is detected, a few points from where the
  button went down, and a click without a drag paints nothing. Object
  removal's tool (`views/spots.rs`) already starts from
  `pointer.press_origin()` and makes a dab on a click. Size: S.
- [ ] **Parts list overlap.** In Local adjustments, an intersected
  Luminance range row (op box, label, then Invert and ✖ laid out from the
  right) is wider than the panel: the label runs under the Invert toggle.
  Size: S. Source: `views/local.rs`, the parts rows.
- [ ] **Guided upright assumes Aspect 0.** With an Aspect set and a
  rotation, guides end up leaning slightly. Size: S–M (solve with Aspect, or
  hold it fixed in the model). Source: DECISIONS "Guided upright".
- [ ] **UI tests time out under heavy load.** With other builds running,
  whichever test is slowest misses its settle timeout; on a quiet machine
  they pass. Scale the timeouts with load, or run fewer apps at once than
  half the cores. Size: S. Source: DECISIONS "Red-eye", "AI raw denoise",
  `crates/shoebox-app/tests/ui.rs`.

## Tune on real photos

Defaults that are guesses so far.

- [ ] **Find dates: look-alike threshold and the picture pass.** A dated
  photo counts as a look-alike at an embedding cosine of 0.8 or more
  (`dating::LOOK_MIN`), the 8 closest are asked and 3 must share a period;
  text is read at a 2048 px long edge (`TEXT_EDGE`). All guesses. The
  first text read in a run loads Vision's model (about 45 s here). Check:
  "Find dates on real undated photos" in MANUAL-CHECKS.

- [ ] **Baseline exposure (+0.75) and the display transform** (white point
  4 stops up, slope 0.17 a stop). Check: "30 NEFs render cleanly". Source:
  DECISIONS "Develop, Phase 1: the CPU reference pipeline".
- [ ] **Auto's rules**, tuned by eye: exposure towards the camera JPEG,
  noise reduction by ISO, sharpening 30 / masking 20, the dehaze threshold,
  Smart Lighting 25 (40 for dark frames). Check: "Auto on the 30". Source:
  DECISIONS "Auto", "Smart Lighting".
- [ ] **AI denoise in Auto from ISO 3200**, with classic luminance capped
  at 10. Check: "AI denoise in Auto". Source: DECISIONS "Auto: AI denoise
  from an ISO".
- [ ] **RawNIND's coloured edges** round bright thin lights in night shots
  (a model limitation; how bad on real files). Check: "AI denoise". Source:
  DECISIONS "AI raw denoise: RawNIND on candle".
- [ ] **Highlights, shadows and clarity radii** (2.5% and 6% of the long
  edge). Check: "Highlights, Shadows, Clarity, Dehaze". Source: DECISIONS
  "Phase 2, second chunk".
- [ ] **Defringe's hue windows** (purple 285–345°, green 105–165°) and the
  hot-pixel thresholds. No check yet. Source: DECISIONS "Defringe, hot
  pixels and raw levels".
- [ ] **Control point defaults:** radius 0.12 long edges, feather 0.75,
  colour ±20°, luminance ±0.1. Check: "Control points". Source: DECISIONS
  "Control points".
- [ ] **Smart Lighting spot amounts:** the spot brought into −0.5 to +1
  stop, by at most 2 stops, fading by three stops. Check: "Smart Lighting,
  spot weighted". Source: DECISIONS "Smart Lighting, spot weighted".
- [ ] **Local softening's reach** (about 2 px of the original, fading by
  8). Check: "Sharpness per mask". Source: DECISIONS "Local AI denoise
  strength and sharpening".
- [ ] **Auto-upright's limits** (lean capped at 6°, Auto ±60, its charge
  for strong corrections). Check: "Auto-upright". Source: DECISIONS
  "Auto-upright".
- [ ] **Red-eye redness** (full at 0.4, none below 0.2). Check: "Red-eye".
  Source: DECISIONS "Red-eye".
- [ ] **Volume deformation's face weights** (whole within 0.75 × the box,
  gone by 2.5 × that). Check: "Volume deformation". Source: DECISIONS
  "Volume deformation".
- [ ] **Lens Blur's scale:** Amount 100 is 4% of the long edge a whole map
  from focus, default 50; highlight boost from 40% of clipping. Check: "Lens
  Blur on real photos". Source: DECISIONS "Lens Blur".
- [ ] **Object removal's grain match** at 100% on high-ISO raws. Check:
  "Object removal". Source: DECISIONS "Object removal".
- [ ] **HDR deghosting thresholds and noise model** (Low/Medium/High
  n, t; 50 000 electrons at white). Check: "A tripod bracket with a moving
  tree". Source: DECISIONS "HDR merge".
- [ ] **Focus stacking: noise floor 2.5, steepness 8**, and whether the
  reference-based noise estimate softens faint texture; Pyramid or Depth
  map as the default. Checks: "A macro stack of 10–30 frames", "A landscape
  with near and far". Source: DECISIONS "Focus stacking".
- [ ] **Panorama seams on a 1 MP canvas** (small figures in a huge panorama
  may still be cut). Check: "A city scene with people moving". Source:
  DECISIONS "Panorama".
- [ ] **Whether other editors keep values above 1.0** in Shoebox's float
  DNGs. Check: "The DNG in another raw editor". Source: DECISIONS "HDR
  merge".
- [ ] **Linear DNG brightness without BaselineExposure** in other editors.
  Check: "Linear DNG exports". Source: DECISIONS "AVIF and linear DNG
  export".
- [ ] **CAT16 without gamut compression** on deep blue and violet LEDs.
  Check: "Colour calibration (CAT16) in mixed light". Source: DECISIONS
  "Colour calibration (CAT16)".
- [ ] **Semantic search thresholds** (a match at cosine 0.07, Find Similar
  at 0.75) and the tag rule (score 0.09, lift 0.045). No check yet. Source:
  DECISIONS "M6 semantic search", "M6 objects".
- [ ] **Face grouping** (cosine 0.45, quality 0.85, 2.5% of the image). No
  check yet. Source: DECISIONS "M6 faces: People".
- [ ] **Moves and renames on File Provider volumes**
  (`~/Library/CloudStorage`): `renamex_np(RENAME_EXCL)` there is
  unverified (a refusal fails the batch cleanly). No check yet. Source:
  DECISIONS "Cloud-only files".

## Enhancements

### Library

- [ ] **Loupe: panoramas sharp at 100%.** A photo wider or taller than
  the GPU's largest texture (8,192 px here; about 140 panoramas in the
  library, up to 21,316 px) is decoded to fit it (from 2026-10-25; it
  used to end the app), so 100% and closer upscale it. Tile it instead:
  the region shown at full resolution, as Develop's zoom does. M.
- [ ] **Shift dates.** Move the dates of photos already dated by an offset
  (a camera whose clock was wrong, or set to another time zone), for a
  selection or a camera over a span, with the dry run and batch Set date…
  uses. Deferred when Find dates was planned (SPEC "Dates"). M.
- [ ] **iCloud Photos: "probably already in the library".** Hold back a
  Photos asset when the library has a photo taken at the same moment (to
  the sub-second) with the same proportions, before downloading it, with
  Import anyway: catches earlier exports in another format (JPEG, edited),
  which the byte check can't. Also match cloud-only placeholders by name,
  size and date when the sync record is lost, and a large-scale "import
  twice after partial exports, copy nothing" test. M. Deferred 2026-10-23.
- [ ] **iCloud Photos: Re-import missing.** PHOTOS-SPEC's Settings button
  to copy again photos whose copies are gone from the library (not just
  moved). Needs the library's files hashed to tell. M. Source: DECISIONS
  "iCloud Photos: the Import screen".
- [ ] **iCloud Photos: Rebuild sync record** as its own button (a sync
  already rebuilds lost records by hash, but downloads to do it). S–M.
  Source: same.
- [ ] **Send to Photos: the `shoebox:PushedFrom` marker** in sent files, so
  a sync with lost records still knows Shoebox's own photos. S–M (export's
  metadata writing). Source: DECISIONS "iCloud Photos: Send to Photos".
- [ ] **Send to Photos: grid badge** for photos edited since they were sent
  (only the Info row says so now). S. Source: same.
- [ ] **Send to Photos: Photos as an export destination** in the export
  dialog, beside the Send dialog. S. Source: same. (The person may rethink
  exports as a whole: deferred 2026-10-23.)
- [ ] **Send to Photos: undoing an update** goes back to the original, not
  to the earlier send; undo leaves album membership. S–M. Source: same.
- [ ] **iCloud Photos: the iCloud ids stage.** If the log shows
  `cloudIdentifierMappings` is slow on a real library, look up cloud ids
  only for assets the records don't know by their local id. S–M. Source:
  DECISIONS "iCloud Photos: listing without files".
- [ ] **iCloud Photos: quick re-sync by change token.** Every sync lists
  every asset now; `fetchPersistentChangesSinceToken` would list only
  what changed. M. Source: DECISIONS "iCloud Photos: PhotoKit".

- [ ] **Face thumbnails in the People list.** S. Source: DECISIONS "M6
  faces: People".
- [ ] **Face regions (MWG) written to sidecars.** M. Source: DECISIONS "M6
  faces: People".
- [ ] **One read for faces and image embeddings** (each reads the file now
  when both are on). S–M. Source: DECISIONS "M6 faces: People".
- [ ] **Day folders renamed after an event** (SPEC's optional, off by
  default); needs directory moves in `fileops`. M. Source: SPEC Events,
  DECISIONS "M5 file operations and import".
- [ ] **Shutter count for other makes** through ExifTool when installed, as
  SPEC allows. S. Source: DECISIONS "Shutter count, GPS from sidecars".
- [ ] **Play a Live Photo's video** from its photo in the loupe (the
  stack's MOV; held or with a button, as Photos does). S. Source: DECISIONS
  "Movies play in the loupe".
- [ ] **Perceptual duplicates for movies** (exact copies only now). M.
  Source: DECISIONS "Live Photos and movies (MOV)".
- [ ] **The loupe at 100% on an edited photo** shows the 2560 px preview;
  render the visible region at full size as Develop does. M. Source:
  DECISIONS "Develop, Phase 1: the Develop view".
- [ ] **Reattach moved edits on watcher updates**, not only on full scans.
  S. Source: DECISIONS "Edits after photos move in Finder".
- [ ] **Idle pre-convert on system idle** (an IOKit wrapper) rather than a
  minute without input to Shoebox. S. Source: DECISIONS "Converting ahead".

### Develop: colour

- [ ] **ColorChecker measuring tool** (`examples/colorchecker.rs`), needed
  by the Phase 1 gate and the DCP check; say which chart. M. Source:
  MANUAL-CHECKS "Phase 1 gate".
- [ ] **Camera match per Picture Control.** M. Source: DECISIONS "Camera
  match profiles".
- [ ] **Local tone maps that see the DCP tables** (worked out from the
  colour matrix alone, so a strong profile leaves their gains a little
  off). M. Source: DECISIONS "DNG camera profiles".
- [ ] **CAT16 gamut compression** (darktable's), if LED shots need it. M.
  Source: DECISIONS "Colour calibration (CAT16)".
- [ ] **Hald CLUT PNG looks.** S. Source: DECISIONS "Looks (`.cube`)".
- [ ] **Adjustable defringe hue ranges.** S. Source: DECISIONS "Defringe,
  hot pixels and raw levels".
- [ ] **Soft proofing against printer ICC profiles.** S–M. Source:
  DECISIONS "Soft proofing".
- [ ] **AMaZE demosaic** as a "max detail" option (port from RawTherapee).
  M. Source: EDITING-SPEC, Demosaic algorithms.
- [ ] **Lightroom XMP preset import** (`crs:` mapped onto modules). M.
  Source: EDITING-SPEC, Packs.
- [ ] **Preset packs** (`.shoeboxpack`, `packs/<name>/`). M. Source:
  EDITING-SPEC, Packs; DECISIONS "Presets, copy and paste".
- [ ] **Sync and auto-sync** on top of copy and paste; a preset applied on
  import per folder. S–M. Source: DECISIONS "Presets, copy and paste";
  EDITING-SPEC, Presets.

### Develop: masks

- [ ] **Edge-aware auto-mask for the brush** (EDITING-SPEC's optional). M.
  Source: DECISIONS "Phase 3, first chunk: local adjustments".
- [ ] **Geometric masks at full size in exports** without an AI part
  (masks without one keep the 2560 px grid; ranges stay on it). S–M.
  Source: DECISIONS "AI masks on disk, and finer for export".
- [ ] **Masks read after volume deformation**, so masks drawn before a
  strong deformation stay on what they covered near the corners. M. Source:
  DECISIONS "Volume deformation".

### Develop: retouching

- [ ] **Add strokes to the selected removal** (each drag makes a new
  removal now; Option-drag only erases). S. Source: DECISIONS "Object
  removal".
- [ ] **Click an object to remove it** (SAM's mask as the hole). M. Source:
  DECISIONS "Object removal".
- [ ] **Finer fills at 100%:** a big hole is filled at 512² and upsampled,
  soft beyond the matched grain; tiles or a second pass at full size. M–L.
  Source: DECISIONS "Object removal".
- [ ] **AOT-GAN for regular patterns** through wide holes (MI-GAN's weak
  spot), as a fallback or choice. M–L. Source: DECISIONS "Object removal".
- [ ] **A reach for local softening** (fixed at about 2 px). S. Source:
  DECISIONS "Local AI denoise strength and sharpening".

### Develop: geometry

- [ ] **Shih et al.'s full mesh solve** for Fix faces (lines crossing a
  face's feather bend slightly now). L. Source: DECISIONS "Volume
  deformation".
- [ ] **Deconvolution sharpening with lens profiles** (Richardson–Lucy). L.
  Source: DECISIONS "Phase 2, third chunk".
- [ ] **Tighter volume deformation parity test:** display values measure
  2e-4, the test allows 1e-3 (as rotation). S. Source: DECISIONS "Volume
  deformation".

### Develop: Lens Blur

- [ ] **Apple's depth data** (HEIC portraits' AVDepthData) in place of the
  model where a photo has it. M. Source: DECISIONS "Lens Blur".
- [ ] **Painted depth** besides the masks' Depth push (a push keeps the
  model's detail; painting would flatten it, but sometimes that's wanted). M.
  Source: DECISIONS "Lens Blur".
- [ ] **Refining the depth map on the GPU** (17 MB re-uploaded per Depth
  drag at 2560 px). S–M. Source: DECISIONS "Lens Blur".
- [ ] **Cat's-eye and onion-ring bokeh.** M. Source: DECISIONS "Lens Blur".

### Merges

- [ ] **HDR: AI denoise of the sources** as a dialog option (minutes). M.
  Source: DECISIONS "HDR merge".
- [ ] **HDR: deghosting where the reference clipped** (against the next
  shorter frame). M. Source: DECISIONS "HDR merge".
- [ ] **HDR: a single-float option.** S. Source: DECISIONS "HDR merge".
- [ ] **Focus: a depth-index map** kept for retouching from one frame
  (Zerene's; needs a second pass). M–L. Source: DECISIONS "Focus stacking".
- [ ] **Focus: half-float range bounds** (a third less of the Pyramid's
  memory; 45 MP stacks need about 15 GB now). S–M. Source: DECISIONS "Focus
  stacking".
- [ ] **Panorama: a layout preview** in the dialog (embedded JPEGs could
  make it fast, without lens corrections). M. Source: DECISIONS "Panorama".
- [ ] **Panorama: lenses without a profile** (a shared radial distortion
  term in the adjustment; vignetting from the overlaps, Goldman). M–L.
  Check: "A lens Lensfun doesn't know". Source: DECISIONS "Panorama".
- [ ] **Panorama: per-frame focal lengths.** M. Source: DECISIONS
  "Panorama".
- [ ] **Panorama: a coarse pre-match for 30+ frames** (all pairs now: 435
  pairs, about 9 s, for 30). S–M. Source: DECISIONS "Panorama".
- [ ] **Panorama: past 150 MP** by tiling the blend and the DNG writer; seams
  on a finer canvas for very large panoramas. L. Source: DECISIONS
  "Panorama".
- [ ] **Panorama: DefaultCrop** over the uncropped boundary instead of
  cutting. S–M. Source: DECISIONS "Panorama".
- [ ] **Panorama: Fill edges** with MI-GAN. M. Source: DECISIONS
  "Panorama".
- [ ] **Panorama: blending across a 360°'s back edge.** M. Check: "A
  two-row 360°". Source: DECISIONS "Panorama".
- [ ] **Panorama: several panoramas from one selection** (connected
  components). M. Source: DECISIONS "Panorama".

### Export

- [ ] **Output sharpening** (screen, matte, glossy) in presets. M. Source:
  EDITING-SPEC, Export presets.
- [ ] **AVIF in Adobe RGB or ProPhoto** (needs an ICC `colr` box, which
  `avif-serialize` can't write). M. Source: DECISIONS "AVIF and linear DNG
  export".
- [ ] **BaselineExposure in linear DNGs**, once known for the camera. S.
  Source: DECISIONS "AVIF and linear DNG export".
- [ ] **DNG previews' colour** (PreviewColorSpace isn't read; previews are
  taken as sRGB unless profiled). S. Source: DECISIONS "DNG and AVIF in the
  library".
- [ ] **Export jobs at once** (photos go two at a time, jobs one after
  another). S. Source: DECISIONS "Two exports at once".

### Models

- [ ] **Mirror the Google Drive models** (RawNIND, MI-GAN) on a release if
  Drive stops serving them (needs the person's go-ahead: the repo is
  public). S. Source: DECISIONS "AI raw denoise", "Object removal".
- [ ] **Model folders without a READY file:** a folder set by hand
  (`SHOEBOX_*_MODEL`, a copied folder) needs `READY` holding the model's id;
  checking the pinned SHA-256s would do instead. S. Source:
  `shoebox-ml/src/download.rs`.
- [ ] **Drop the candle Metal patch** when candle compiles as Metal 4. S.
  Source: DECISIONS "M6 semantic search", `vendor/README.md`.

### Performance

- [ ] **Local AI denoise on part of the frame:** a layer that adds AI
  strength runs the model over the whole frame (12–18 s) even with the
  global module off and the mask small. M. Source: DECISIONS "Local AI
  denoise strength and sharpening", `cpu::layers::wants_ai`.
- [ ] **RCD on the GPU** (0.24 s on the CPU) if opening needs it. M.
  Source: DECISIONS "Develop, Phase 1: the GPU live pipe".
- [ ] **The detail stage on the GPU** (29 ms at proxy size on the CPU) if
  drags feel slow. M. Source: DECISIONS "Phase 2, third chunk".
- [ ] **Benchmarks as a CI gate** (needs a stable machine and a stored
  baseline; local `bench_check` for now). S–M. Source: DECISIONS "Golden
  images and benchmarks".

### Platform and build

- [ ] **`target/` hygiene.** It grows without bound; `cargo clean` past
  about 30 GB (CLAUDE.md), as `cargo sweep --time` deletes current builds
  on macOS. A script that cleans by size, or per-worktree clean-up on
  merge, would save remembering. S. Source: CLAUDE.md, Working rules.

## Out of scope for now

- **JPEG XL export.** `jpegxl-rs` builds Google's C++ libjxl; AVIF covers
  the modern-format need. Back with a pure-Rust encoder or a reason to take
  the C++. Source: DECISIONS "Phase 4 scope". (JPEG XL DNGs from the
  converter were dropped too: no switch makes them; "Develop, Phase 1: raw
  decode".)
- **Linux.** macOS only (decided 2026-10-22); CI's Linux job is off (`if:
  false`). Back when the person wants it: re-enable the job and the Docker
  check, portable subject and person masks (BiRefNet, U²-Net, MODNet on
  candle), HEIC through libheif, movie posters. Source: DECISIONS "Linux
  deferred".
- **Learning your taste** (Auto from the person's own edits). Needs a few
  hundred Shoebox edits to learn from. Source: DECISIONS "Phase 4 scope".
- **Body masks** (hair, clothes, body skin). No openly licensed
  human-parsing model chosen; revisit after the landscape model, now done.
  Source: DECISIONS "Phase 4 scope", "Finer AI masks".
- **HDR/EDR display output.** Out of scope for v1. Source: EDITING-SPEC,
  Display.
- **The DxO sidecar-name check** and a golden test on a real DxO sidecar.
  No PhotoLab here; the default stays `name.ext.xmp`. Back with a DxO
  sidecar to test against. Source: DECISIONS "DxO sidecar check deferred".
- **Sandboxing and the Mac App Store.** Shoebox moves files anywhere in its
  roots, which a sandbox would allow only with security-scoped bookmarks
  for every root. Back only for the App Store. Source: DECISIONS "Mac app
  bundle".
