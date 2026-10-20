# Shoebox — iCloud Photos Spec

Oct 20, 2026 · Draft for review

Copy photos out of the Mac's Photos library (and so iCloud Photos) into a Shoebox library root, sync again later without copying anything twice, and send a photo, usually one edited in Develop, back into Photos. Once this spec is accepted, it is the source of truth for the feature the way `docs/EDITING-SPEC.md` is for editing.

## Goals and non-goals

**Goals**

- **Import:** copy every photo and video in Photos (originals, at full quality, downloaded from iCloud when the Mac only keeps optimised versions) into a library root. The copy goes through the same pipeline as a card import: template naming, BLAKE3 verification, one journaled batch, undoable.
- **Sync again:** later runs copy only the assets that no earlier run copied. This still holds after the copies are renamed, moved, rejected or trashed in Shoebox, and after the catalog is rebuilt.
- **Push:** send a Shoebox photo to Photos. A photo that came from Photos goes back as an *edit* of the asset it came from, so Photos keeps the original and can Revert to Original. Any other photo (a NEF from the camera, a scan) becomes a new asset. Pushing again replaces the earlier push and doesn't add another copy.
- Scale: 30,000 assets on the first run, 200,000 by design, as for the rest of the library.

**Non-goals**

- **Two-way sync.** Nothing is deleted or changed in Shoebox because it changed in Photos, and nothing changes in Photos unless a person pushes it. "Sync" here means "import what's new".
- Translating Photos' own adjustments into a Shoebox edit, or a Shoebox edit into Photos adjustments. Photos gets rendered pixels.
- Shared Albums (iCloud Shared Albums hold reduced copies, not originals). The iCloud Shared Photo Library *is* in scope: its assets are in the system library.
- Reading `Photos Library.photoslibrary` directly (see Decisions).
- Linux. The code stays portable (see Architecture), but the feature needs PhotoKit.

**Conflicts with `docs/SPEC.md` to resolve when this is accepted**

1. *Non-goals for v1* lists "Cloud sync". This feature is import plus explicit push, not sync, but the line should be reworded: "Cloud sync (iCloud Photos import and push are covered by `docs/PHOTOS-SPEC.md`)".
2. *Safety rules* say nothing about writing outside the library. Push writes to the Photos library, so it needs a new rule (rule 9 below), and CLAUDE.md's safety list needs the same line.
3. *Milestones*: this becomes **M7 — iCloud Photos**, after Develop Phase 1. The working rule is one milestone at a time, in order.

## How it works

PhotoKit (`Photos.framework`) is Apple's supported API for the system Photo Library. It's the only way to get originals from iCloud when the Mac stores optimised copies, and the only supported way to write back.

| Need | PhotoKit |
| --- | --- |
| Permission | `PHPhotoLibrary.requestAuthorization(for: .readWrite)`; `NSPhotoLibraryUsageDescription` in Info.plist; with the hardened runtime, the `com.apple.security.personal-information.photos-library` entitlement |
| List assets | `PHAsset.fetchAssets(with:)` over images and videos, sorted by creation date; metadata only, which is quick even for 30k assets |
| Stable identity | `PHCloudIdentifier` (from `localIdentifier` via `cloudIdentifierMappings`): stays the same across Macs and a rebuilt Photos library. `localIdentifier` is kept too, as a fallback for assets that aren't in iCloud |
| Files of an asset | `PHAssetResource.assetResources(for:)`: `.photo` (the original), `.alternatePhoto` (the JPEG of a RAW+JPEG pair), `.pairedVideo` (a Live Photo's MOV), `.fullSizePhoto` / `.fullSizeVideo` (the current Photos edit), `.video` |
| Copy a file out | `PHAssetResourceManager.writeData(for:toFile:options:)` with `isNetworkAccessAllowed = true` (downloads from iCloud as needed, with a progress callback) |
| What changed since last time | `PHPhotoLibrary.fetchPersistentChanges(since:)` with a stored `PHPersistentChangeToken` |
| Albums | `PHAssetCollection.fetchAssetCollections(with: .album …)` and the folders above them (`PHCollectionList`) |
| New asset | `PHAssetCreationRequest.forAsset().addResource(with: .photo, fileURL:)` inside `performChanges` |
| Edit an asset | `PHContentEditingOutput` with `PHAdjustmentData` (our format id) and a rendered JPEG, set on `PHAssetChangeRequest.contentEditingOutput` |
| Undo a push | `PHAssetChangeRequest.revertAssetContentToOriginal()`; deleting an asset Shoebox created (`PHAssetChangeRequest.deleteAssets`: macOS asks the person, and the asset goes to Recently Deleted for 30 days) |

macOS shows its own confirmation alert when an app modifies or deletes existing assets. That's expected and kept: one `performChanges` per push batch, so one alert per batch.

## Import

The Import screen gets a second source next to "a card or any folder": **iCloud Photos**. The rest of the form is the same as a card import: **Into** picks any library folder that isn't a reference folder (the first one by default), and **Names** is the same template, shared with card imports and saved in settings, so with the default template phone and camera photos of one day land in the same day folder. Everything after "find the photos" is the existing import (`shoebox_core::import`): the plan, the thumbnails or dry-run table, left-out files with **Import anyway**, progress, cancel, undo.

**Finding photos (planning)**

1. Ask for Photos access if Shoebox doesn't have it. If it's refused or limited, say what that means and link to System Settings › Privacy › Photos. *Limited* access imports only the assets the person chose, and says so.
2. Fetch every asset's metadata: identifiers, media type and subtypes, creation date, location, favourite, hidden, burst, resource list with names, types and sizes, whether it has a Photos edit, and album membership.
3. Leave out what earlier runs copied (see Sync ledger) and list it as **already synced** with the count, not one row per asset.
4. The rest go into the import plan as planned photos, one frame per asset, so a Live Photo's HEIC and MOV, and a RAW+JPEG pair, share one base name and one `-N` suffix. They then stack by base name, as card imports do.
5. Exact-hash skipping still applies, but the hash isn't known until a file is downloaded, so it's checked *after* download and *before* the copy into the library. A skipped asset is recorded in the ledger as "already in the library: `<path>`" so it isn't downloaded again next time.

**What's copied per asset**

Always the originals as Photos holds them, at full quality: never the optimised copies an "Optimise Mac Storage" Mac keeps, and never a preview or a re-encoded JPEG. An iPhone HEIC comes as that HEIC, a ProRAW photo as its DNG, a camera RAW+JPEG imported into Photos as both the raw and the JPEG, and a video as the original MOV or MP4. The one exception is a photo that was only ever a JPEG in Photos: then the JPEG *is* the original.

| Resource | Copied | Notes |
| --- | --- | --- |
| Original (`.photo` / `.video`) | Always | Byte-for-byte what was uploaded: HEIC, JPEG, ProRAW DNG, PNG screenshots, MOV |
| RAW+JPEG pair (`.alternatePhoto`) | Always | Same base name, other extension |
| Live Photo video (`.pairedVideo`) | Setting, default on | Same base name, `.MOV` |
| Photos edit (`.fullSizePhoto` / `.fullSizeVideo`) | Setting, default on | Imported as a variant of the original in its stack, named `<base>-photos.<ext>` (`-photos` marks it as Photos' render, like Lightroom's `-2`). Shoebox can't re-edit Photos' adjustments, so this keeps how the photo looks today |
| Burst frames | Only the picked frames by default; setting for all | Photos' "representative" frame plus any the person kept |
| Hidden assets | Setting, default off | Off because Hidden is usually deliberately private. When on, they get the keyword `Photos|Hidden` |
| Recently Deleted | Never | Not returned by PhotoKit |

**Names and dates.** The import template names files as usual. Many Photos assets have no EXIF capture date (screenshots, saved images, messaging apps), and the person may have changed a date in Photos, which leaves the original file unchanged. So:

- The file's EXIF date wins when there is one and it agrees with Photos (within the time-zone offset).
- No EXIF date: name the file by Photos' creation date (not `_undated/`) and write that date to the copy's sidecar as `photoshop:DateCreated` and `exif:DateTimeOriginal` (XMP), which the scanner must then read as capture time (a small change in `exif`/`sidecar` precedence: sidecar date, then EXIF).
- EXIF and Photos disagree (the person corrected the date in Photos): Photos wins for the name and the sidecar date, and the Info panel shows both dates.
- The same rule applies to location: a location the person set in Photos and that isn't in the file goes to the sidecar as `exif:GPSLatitude`/`GPSLongitude`, read by the map.

**Albums and marks**

| Photos | Shoebox (default) | Other choices |
| --- | --- | --- |
| Album | **Event** of the same name, nested as Photos' album folders are (`Trips › Japan` → Trips › Japan), joining an existing event of that name, as card-import folder names do | Collection, keyword `Albums|…`, nothing |
| Favourite | **★★★★★** (`xmp:Rating` 5) | Another rating, keyword `Photos|Favourite`, pick flag, nothing |
| Hidden (when imported) | Keyword `Photos|Hidden` | — |
| Title, caption, keywords, people | Not available | PhotoKit doesn't expose them publicly. If a later macOS does, they map to `dc:title`, `dc:description`, `dc:subject` |
| Smart albums, Memories | Not imported | They're queries over Photos' own data |

These follow the folder-names feature: the plan lists each album with what it becomes, editable (an album's name can be edited too), and the choices are remembered for the next run. Events and ratings are written into the copies' sidecars, and new events into `events.json`, in the import's batch. On later runs, a new album, or an already-synced asset added to an album, adds it to the event without copying anything (album membership is re-read each run). An asset favourited in Photos since it was synced gets ★5 only if it has no rating in Shoebox; a rating set in Shoebox is never replaced. A photo removed from an album, or unfavourited, in Photos is **not** changed in Shoebox: Shoebox never changes things because Photos changed, it reports them instead ("12 photos left *Japan* in Photos since the last sync"; one click takes them out of the event).

**Downloading**

- Originals of an "Optimise Mac Storage" library are mostly in iCloud. The plan shows how many assets need downloading and the total size (from the resources' recorded sizes, where PhotoKit reports them; otherwise "about N GB", estimated from the ones known).
- Before starting, check free space on the destination volume and the cache volume: the import needs the planned size plus 10 GB headroom. If it doesn't fit, say so and offer to import a date range.
- Each resource downloads with `writeData` into a staging folder in the cache (`<cache>/photos-staging/<batch>/`), is hashed, and then is copied into the library through `fileops` as any import file is: journaled first, verified by BLAKE3, never overwriting. The staged file is then removed (it's in a cache directory, so rule 1 allows it). Staging keeps `fileops` the only thing that writes into the library and keeps a half-downloaded file out of it.
- Four downloads at once, with the copy into the library running behind them. Progress shows assets and bytes downloaded, the speed and time left, separately from the copy's progress.
- **Resumable.** Each asset is recorded in the ledger as soon as its files are copied and verified, so a cancelled, quit or crashed import loses at most the assets in flight. Running it again continues where it stopped. The batch for the run stays one History entry and is undoable as a whole.
- An asset that fails (download error, iCloud says it's unavailable, decode error on a file that's then found to be corrupt) is skipped with its reason and listed at the end with **Try again**. It's not recorded as synced.
- The first run of 30k assets will take hours and may use a few hundred GB. The Import screen says that up front and recommends an AC adapter; Shoebox asks the system to stay awake while it runs (`NSProcessInfo.beginActivity`; nothing in the app does this yet, so this feature adds it, and card imports can use it too).

## Sync ledger

What's been copied is recorded per library root in `<root>/.shoebox/photos.json`, beside `collections.json` and `events.json`, so it survives a catalog rebuild and travels with the library. It's written atomically through `fileops` in the import's batch (undoing the import removes its entries).

```json
{
  "schema": 1,
  "assets": {
    "<PHCloudIdentifier>": {
      "local_id": "9F2C…/L0/001",
      "status": "copied",
      "files": [
        { "kind": "original", "rel": "2024/2024-06-03/20240603-141502-120.HEIC", "blake3": "…", "size": 2840193 },
        { "kind": "paired_video", "rel": "2024/2024-06-03/20240603-141502-120.MOV", "blake3": "…", "size": 3120044 },
        { "kind": "photos_edit", "rel": "2024/2024-06-03/20240603-141502-120-photos.JPG", "blake3": "…", "size": 1932001, "photos_modified": "2024-06-04T09:12:00Z" }
      ],
      "batch": "b-…",
      "synced_at": "2026-10-21T10:02:11Z"
    }
  },
  "pushed": {
    "<PHCloudIdentifier>": {
      "from": { "rel": "2026/2026-10-12/DSC_1234.NEF", "blake3": "…", "copy": "main" },
      "kind": "edit_of_original",
      "render_hash": "…",
      "pushed_at": "2026-10-22T18:30:00Z",
      "batch": "b-…"
    }
  },
  "change_token": "<base64 PHPersistentChangeToken>",
  "albums": { "<album local id>": { "name": "Trips › Japan", "use": "event", "event": "Trips › Japan" } }
}
```

- **Status** is `copied`, `in_library` (skipped: the same bytes were already there, with the path), or `left_out` (the person chose not to import it; it's offered again only with **Show left-out**).
- **"Synced before" means a ledger entry**, not "a file exists". An asset whose copy was later moved to `_rejected/` or the Trash stays synced and isn't copied again. Settings › iCloud Photos has **Re-import missing**, which lists entries whose files are no longer anywhere in the library (found by BLAKE3, so moves don't count as missing) and copies them again on request.
- **A changed Photos edit.** If the asset's `modificationDate` and its `.fullSizePhoto` changed since `photos_modified` (the person edited it again in Photos), the next run offers the new render as **updated in Photos** and imports it as another variant (`-photos-1`); the earlier one is never replaced. Edits Shoebox itself pushed are recognised by our adjustment format id and never come back as imports.
- **No round trips.** Nothing Shoebox sends to Photos is ever imported back as a new photo:
  - *A photo that came from Photos* goes back as an edit of the same asset, which is already in the ledger as synced, so it isn't copied again. Its edit changes the asset's `modificationDate`, which would normally offer the render as **updated in Photos**, but the edit carries our adjustment format id, so it's recognised as Shoebox's own and skipped.
  - *A photo from elsewhere* becomes a new asset, recorded under `pushed` with its cloud id the moment PhotoKit returns it, and skipped by later imports.
  - *Backstop for a lost ledger:* every file Shoebox renders for Photos carries a Shoebox marker in its XMP (`shoebox:PushedFrom` = the source photo's BLAKE3 and copy id). Photos keeps an asset's original bytes, so a later import, or **Rebuild sync record**, reads the marker and links the asset back to the Shoebox photo it came from instead of importing it.
  - *Edited again in Photos* (on the phone, after a push): that's a genuine change made outside Shoebox, so it's offered as **updated in Photos** and imported as a `-photos` variant in the source photo's stack, never as a separate photo.
- **Several roots.** The ledger is per root, and import checks the ledgers of *every* root, so an asset copied into one root isn't copied again into another.
- **Lost ledger.** If `photos.json` is gone, the exact-hash check still prevents duplicate copies (Photos returns originals byte-for-byte), at the cost of downloading again. Settings offers **Rebuild sync record**: fetch, download and hash, match against the library (and Shoebox's push markers), write the ledger, copy nothing.
- **Quick re-sync.** The change token lets a later run ask Photos for only the inserted and updated assets since last time, so "Sync again" on a 30k library takes seconds when little changed. Without a usable token (first run, token expired, other Mac) it falls back to the full fetch and diff, which is still metadata only.

## Push to Photos

Pushing is exporting with Photos as the destination: what goes to Photos is always a rendered file (JPEG, PNG or TIFF, from an export preset), never a raw or an original. **Send to Photos** is in a photo's menu, for the selection (one per stack: the primary, or the edited copy if there is one), and the export dialog has **Photos** as a destination beside a folder. Videos can't be sent (Shoebox doesn't edit them).

**What gets sent**

| Shoebox photo | Has a Develop edit | Sent as |
| --- | --- | --- |
| Came from Photos (in the ledger) | Yes | An **edit of the original asset**: the rendered JPEG plus adjustment data `{format: "com.pinkoctopuslabs.shoebox", version: "1", data: {render_hash, edit path}}`. Photos shows the edit, keeps its original, and offers Revert to Original |
| Came from Photos | No | Nothing to send; the menu item is disabled and says "already in Photos" |
| Anything else (NEF, scan, card JPEG) | Yes or no | A **new asset** whose original is the rendered file, dated and located like the source (an unedited NEF renders with the default settings, as an export does) |
| Already pushed before | Edited since | The same asset is updated with a new edit (for an asset Shoebox created, that's an edit on top of the first push, so its first push is the "original"). Never a second asset |
| Already pushed before | Not changed since | Disabled: "up to date in Photos" |

- **Rendering** uses the export pipeline and its presets. A built-in, non-deletable "Photos" preset is the default: JPEG, full size, quality 92, 4:4:4, Display P3, all metadata (capture date, GPS, keywords as XMP). Any JPEG, PNG or TIFF preset can be chosen instead, and the preset used is recorded with the push, as exports record theirs. It renders into the cache, never into the library, and the render carries the `shoebox:PushedFrom` marker (see No round trips). An edit of a Photos asset must be a JPEG (PhotoKit's rule for rendered edits), so those always use a JPEG preset.
- **Option per push:** add to an album (default: none; the album is remembered).
- **Dry-run table** (rule 6): photo → what it becomes in Photos (new asset / edit of *IMG_1234* / update of an earlier push), size, album. Confirm, then the system alert, then the writes.
- **Stale pushes** work like stale exports: if a pushed photo's edit `render_hash` changes, the grid shows a small badge, and the Info panel's "In Photos" row says "edited since sent to Photos" with **Send again**. Nothing is pushed automatically.
- **History and undo.** A push is a History batch. Undo reverts an edit Shoebox applied (`revertAssetContentToOriginal`, or re-applies the previous Shoebox edit if there were several) and deletes an asset Shoebox created, which macOS confirms and moves to Recently Deleted. Undo is refused if the asset has changed in Photos since the push (another edit, made on the phone), and says why.

## Safety rules

The existing rules all apply to the library side unchanged: imports are copies, journaled, verified, never overwriting, and the Photos library is never touched by an import. Two additions:

9. **Photos is written only by an explicit push**, with a dry-run table and confirmation, and only in these ways: add a new asset; add or replace an edit that Shoebox made, which Photos can always revert; and, to undo a push, revert that edit or delete an asset Shoebox created (to Recently Deleted). Shoebox never deletes, hides or changes any other asset, album or metadata in Photos.
10. **No Photos access during development, except a test library.** PhotoKit only ever opens the *system* Photo Library, so rule 7 ("never run against the real library") can't be met with an APFS clone. Real PhotoKit code runs only in a separate macOS user account (or a VM) whose system library is a test library made from fixtures, and not signed in to the person's Apple ID. The PhotoKit backend refuses to push unless the library has an album named `Shoebox test library` (a guard against mistakes, not a security boundary). The person's own 30k library is first used after the feature is finished, starting with a read-only import into a fresh root.

## Architecture

```
shoebox-core::photos          (pure Rust, no PhotoKit)
  source.rs    trait PhotoSource: authorise, assets(since token), albums,
               download(resource, to path, progress, cancel), push(…)
  model.rs     Asset, Resource, ResourceKind, Album, PushRequest, PushResult
  ledger.rs    photos.json: load across roots, merge, plan_save (fileops)
  plan.rs      assets + ledgers → import::Plan (reuses import::decide),
               album/mark mapping, sidecar dates and GPS
  push.rs      selection → push plan (new / edit / update / up to date)
  fake.rs      FakeSource from fixtures (#[cfg(any(test, feature = "fake"))])

shoebox-photos                (new crate, macOS only; Linux: empty stub)
  sys.rs       thin FFI over objc2-photos (unsafe allowed here)
  kit.rs       PhotoKitSource: impl PhotoSource
  examples/photos_probe.rs

shoebox-app
  photos.rs    Photos thread: authorisation, fetch, downloads into staging,
               hands staged files to the library worker; push renders through
               the export queue, then calls PhotoSource::push
  views/files  Import source picker gains "iCloud Photos"; Settings › iCloud Photos
```

- **Why a trait in core.** All the logic that decides what to copy, what's synced, and what a push does lives in core and is tested against `FakeSource`, including the UI tests, so the rules are exercised on every CI run (macOS and Linux) without PhotoKit. `shoebox-photos` is only translation. It's the same shape as `semantic.rs`'s `Model` and `people.rs`'s `FaceModel`.
- **Bindings:** `objc2-photos` (0.3, MIT/Apache-2.0/Zlib, compatible with GPL-3.0) alongside the `objc2`, `objc2-foundation` and `block2` the app already uses. Completion-handler APIs (`requestAuthorization`, `writeData`, `performChanges`) are wrapped to block the Photos thread on a channel, never the UI thread.
- **Unsafe:** add `crates/shoebox-photos/src/sys.rs` to CLAUDE.md's list of FFI wrappers.
- **Bundle:** `scripts/bundle-macos.sh` adds `NSPhotoLibraryUsageDescription` ("Shoebox copies photos from your Photos library into your Shoebox library, and adds photos you send to Photos.") and the photos-library entitlement to the signing step. An unbundled `cargo run` binary can't get Photos access reliably (TCC attributes it to the terminal), so PhotoKit work is tested from a bundled build (`SHOEBOX_DATA_DIR=<scratch>`, `BUNDLE_ID=…photostest`).
- **Linux:** `shoebox-photos` builds as a stub whose `PhotoKitSource::new()` returns `Unavailable`; the Import screen doesn't show the source.
- **CLI:** `shoebox photos status --root <root>` (reads the ledger; no Photos access needed) and `shoebox photos import --root <root> --dry-run`. Real imports from the CLI need the terminal to have Photos access, so they're documented as a convenience, not the main path.
- **Catalog:** a cached table `photos_assets(cloud_id, root_id, rel, kind, blake3)` rebuilt from the ledgers, for the Info panel's "From Photos" / "In Photos" rows and the stale-push badge.

## Plan

Each phase is one reviewable chunk, finished with `cargo fmt`, `clippy -D warnings` and `cargo test`.

**Phase 0 — Spike (in a test macOS account)**
- Create the test account and its test Photos library: about 200 synthetic assets from `shoebox-fixtures` (HEIC, JPEG with and without EXIF, a Live Photo pair, a RAW+JPEG pair, a video, a few edited in Photos, two albums in a folder, favourites, one hidden), with the `Shoebox test library` album.
- `shoebox-photos/examples/photos_probe.rs`: authorise, count, list assets with cloud ids and resources, write one original and one paired video to a scratch folder, time it, read a change token, make a test push of each kind (new asset, edit, revert).
- Confirm the four facts this spec depends on, and record them in `docs/DECISIONS.md`: originals from `writeData` are byte-identical across runs (so BLAKE3 skipping works); `PHCloudIdentifier` is available for the test library; the adjustment-data round trip identifies our own edits; resource sizes are or aren't available.
- *Done when* the probe runs from a bundled build in the test account and the four facts are written down. If a fact fails, revise this spec before Phase 1.

**Phase 1 — Core model and ledger, with the fake source**
- `photos::{model, source, fake, ledger}`; ledger load across roots, merge, atomic write through `fileops`; `Re-import missing` and lost-ledger logic.
- *Done when* tests cover: first import, re-run copies nothing, moved/rejected copies stay synced, two roots, lost ledger falls back to hashes, undo removes ledger entries.

**Phase 2 — Import planning**
- `photos::plan`: one frame per asset, resource choices and settings, `-photos` variants, dates and GPS to sidecars (plus the scanner reading sidecar capture dates), albums to events (or collections, keywords), favourites as ★5 (never replacing a Shoebox rating), hidden left out.
- *Done when* property tests over random fake libraries show every asset is either planned, already synced or listed with a reason, and golden tests show the sidecars written.

**Phase 3 — Staged copy and the PhotoKit backend**
- App `photos.rs`: downloads to staging (four at a time), hash, hand over to the library worker's import, ledger after each verified asset, cancel and resume, free-space check, keep-awake, failures with Try again.
- `shoebox-photos`: `PhotoKitSource` for authorisation, fetch, change tokens, `writeData`; Linux stub; bundle script changes.
- *Done when* an import of the test library in the test account produces exactly the names and stacks the fake predicts for the same assets, a second run copies nothing, a run killed mid-way resumes, and undo puts the root back.

**Phase 4 — Import UI**
- "iCloud Photos" source in Import: permission states, plan with already-synced and left-out counts, the album table, download and copy progress, failure list; Settings › iCloud Photos (last sync, counts, Rebuild sync record, Re-import missing, mapping defaults); the Info panel's "From Photos" row. Headless UI snapshot tests with `FakeSource`.
- *Done when* the UI tests cover each state and a person can do the test-library import entirely in the app.

**Phase 5 — Push**
- `photos::push` planning; the built-in Photos export preset; `PhotoSource::push` for new asset, edit, update and undo; dry-run table, ledger `pushed` entries, History batch; stale-push badge and Info panel row; the album option.
- *Done when* in the test account each row of the "What gets sent" table works, a pushed edit shows on the asset in Photos with Revert to Original, a second push updates rather than duplicates, the next import ignores everything pushed (also with `photos.json` deleted, through the marker), and undo reverses each kind.

**Phase 6 — CLI, docs and first real run**
- `shoebox photos status` and `import --dry-run`; SPEC.md and CLAUDE.md updates (non-goal wording, rules 9 and 10, layout, M7).
- First real run, by the person, on their own Mac: a dry run against their library, then an import into a new, empty root, checked with `shoebox photos status` and a re-run that copies nothing. Push is tried on a single photo first.

## Open questions

All answered (2026-10-20):

- [x] **Raw originals.** Always the full-quality originals Photos holds (see What's copied per asset); never optimised copies or previews.
- [x] **Favourites.** ★5.
- [x] **Photos edits.** Imported too, as `-photos` variants beside the originals.
- [x] **Albums.** Events, nested as album folders are.
- [x] **Hidden.** Left out by default.
- [x] **Destination.** As card import: the Into root picker and the shared Names template.
- [x] **Push.** Rendered files only (JPEG, PNG, TIFF through export presets), like Export; never raws or originals.
- [x] **Videos.** Copied on every run, as backups; never edited or pushed.

## Decisions

- **PhotoKit, not the Photos database.** `Photos.sqlite` (what osxphotos reads) has an undocumented schema that changes with macOS releases, can't download originals that are only in iCloud, and offers no way to write back. AppleScript export from Photos is slow, can't be resumed reliably and loses resource pairing. PhotoKit is supported, handles iCloud downloads and has proper write APIs, at the cost of FFI and the test-account requirement.
- **Ledger in the root, not only the catalog.** "Synced before" must survive a catalog rebuild (a cache by SPEC.md's rule), so it lives in `.shoebox/` like collections and events.
- **Staging in the cache.** PhotoKit writes files itself; letting it write into the library would bypass `fileops`' journal, no-overwrite and verification.
- **Copies, never links.** Each import is an independent copy: deleting a photo in Photos changes nothing in Shoebox, and the other way round.
