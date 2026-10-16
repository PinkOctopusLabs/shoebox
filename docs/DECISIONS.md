# Decisions and deviations from SPEC.md

Newest first. SPEC.md stays the source of truth; this records where we
deliberately differ and why.

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
