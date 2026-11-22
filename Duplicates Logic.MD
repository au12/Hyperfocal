# Duplicates report: design, status and what's next

How Hyperfocal finds exact copies, near-duplicates, look-alikes and series, how it compares with Excire Foto, and what still needs to be done for it to be better than Excire everywhere.

State as of 22 Nov 2026 (`main` at `d8a2c97`).

## Where it stands against Excire Foto 2027

Excire's duplicate finder, from its own documentation:
- **Scope:** the whole database or the current filmstrip.
- **Exact duplicates:** identical binary hashes, computed while you wait.
- **Near duplicates:** "very strict" to "very loose".
- **Bursts:** with a maximum time interval.
- **Results:** saved as collections.
- **Flagging assistant:** sets keep or reject flags from criteria you choose (file size, image size, date, resolution, file format, rating).

| | Excire Foto | Hyperfocal | Verdict |
|---|---|---|---|
| Exact copies | Whole-file binary hash, computed at search time (slow) | Integrity monitor's hashes, already on file. JPEG/HEIC/TIFF/DNG hash only the image data, so a copy that another app wrote metadata into still matches | **Ahead** (Excire's binary hash misses those) |
| Near-duplicates | Visual similarity, very strict to very loose | 64-bit perceptual hash, strict / normal / loose. Turned and mirrored copies are found too | **Ahead** on turned and mirrored copies; even otherwise |
| Crops, edits, other renderings | Only as loose near-duplicates | Look-alikes: SigLIP 2 image embeddings, each match shown as a % alike | **Ahead** |
| Bursts / series | Max time interval | Gap 0–15 s, plus cuts at scene changes, by camera and folder | Even, slightly ahead |
| RAW+JPEG pairs | Separate files | One photo; a copy of either file makes the pair a copy | **Ahead** |
| Scope | Database or current filmstrip | Library, folder (with sub-folders), folder against the rest of the library | Ahead on "against"; **behind** on "current filmstrip" (any search, filter or album) |
| Choosing what to keep | Flagging assistant, criteria you pick | Fixed keeper rule with its reason shown; click to override; persistent "Not duplicates" | **Behind** on configurability, ahead on explaining itself |
| Action | Sets keep/reject flags | Moves the others to `_Rejected` (journaled, undoable). Reject all for exact copies | **Behind**: no "flag only" option |
| Results | Saved as collections | Live report, always current; a group opens as a temporary collection and in Compare | Ahead on freshness, **behind** on saving |
| Speed | Exact search waits on hashing | 0.25–1 s for 300,000 photos (pixel modes), a few seconds for library-wide look-alikes | **Ahead** |
| Where it runs | Desktop app | Web and phone, from the server | **Ahead** |
| Videos | Managed (2025+) | Not indexed at all | Behind (out of scope for now) |

**Net:** ahead on what it finds and on speed. Behind on four workflow points, all fixable: a keeper rule you configure, flagging instead of moving, the current view as the scope, and saving results. See [What still needs doing](#what-still-needs-doing).

## What has been worked on

| When | Commit | Change |
|---|---|---|
| 20 Nov | `b73ec5c` (PR #12) | First version: exact copies, near-duplicates (pHash with multi-index hashing), series, the three scopes, keeper with reason, Compare, "Not duplicates", reject the others. Schema 4. The `ids` list filter. Compare panes show the folder when names collide. |
| 20 Nov | `74a0bb9` | "Reject all but the keepers" is limited to exact copies. Near-duplicates and series are different files, so each of those groups gets a look first. |
| 21 Nov | `8d9b4e9` | Thumbnails moved into pack files. The worker now reads them from the pack, and a thumbnail it has to make is stored there too (the old "made in memory, never written" rule is gone). |
| 21 Nov | `fb35305` | **Look-alikes** (mode `visual`), through SigLIP 2 embeddings with an inverted-file index. **Turned and mirrored copies** in near-duplicates, through the seven dihedral hashes of each photo (`phashes.dih`). |
| 21 Nov | `e83e8df` (PR #13) | The AI engine is built into the Hyperfocal image (models downloaded on demand), so look-alikes no longer need a second container. `HYPERFOCAL_ML_SEARCH` turns them on or off. |
| 21 Nov | `2cc7fbf` | This document. |

## Architecture (server/src/dupes/)

| File | Role |
|---|---|
| `phash.rs` | 64-bit DCT pHash: 32×32 area-averaged Rec.601 luma, coefficients 1..8 × 1..8, median split (always 32 bits set). Flat images (std < 2) hash to 0 and are never compared. `dihedral()`: the hashes of the seven turned and mirrored versions. `sharpness()`: Laplacian energy, best 3 of a 4×4 grid, divided by the image variance (exposure-invariant). |
| `mih.rs` | All pairs within distance r: multi-index hashing, four 16-bit tables, joined bucket by bucket (each bucket against itself and buckets within s = r/4 bits). A pair is reported only by the first table that can see it. Tested equal to brute force. Chosen over a BK-tree, which degrades at r = 7–10. Also answers queries for the turned and mirrored hashes. |
| `bursts.rs` | Series: sort by (folder, camera, taken, name); cut at a new folder or camera, a gap > max_gap, or consecutive pHashes > 22 bits apart. Also used by culling's stacks (`server/src/cull/series.rs`). |
| `report.rs` | Grouping, scopes, keeper, the report. |
| `worker.rs` | Background thread `dupes`: fills `phashes`. |
| `api.rs` | `GET /api/dupes?mode=exact\|near\|visual\|series`, `GET /api/dupes/status`, `POST /api/dupes/dismiss {ids, undo}`, `DELETE /api/dupes/dismissed`. |
| `../ann.rs` | Inverted file for look-alikes over the whole library: spherical k-means, √n cells, int8 centroids, >98% recall against brute force. |

**Schema 4.**
- `phashes(photo_id, size, mtime, orientation, hash, sharp, at, error, dih)`, keyed on the file state like `analysis`.
- `dupes_ignored(a, b)` pairs.
- Partial index `originals_hash_ok ON originals(hash) WHERE status = 0`.

**Exact copies.** Integrity `originals` rows with `status = 0` that match the photo's current size and mtime. Each is mapped to the shown photo with `COALESCE(pair_of, id)`, so either file of a RAW+JPEG pair makes the pair a copy, then joined with union-find. For a folder scope, only that folder's hashes are looked up (1 ms). The README demo library has a real case: the same Flickr photo uploaded twice with different metadata, found as one group.

**Near-duplicates.** Identical hashes are merged first, so there is no quadratic blow-up. Then leader clustering: the hash with the most neighbours leads and takes every unassigned neighbour within r, and so on. Members are within r of the leader (2r of each other), so a drifting timelapse does not chain into one group. A member that is far from the keeper upright but near one of its turned or mirrored hashes is labelled "Turned 90°", "Mirrored" and so on.

**Look-alikes (mode `visual`, needs search by content).**
- Cosine of int8 SigLIP 2 embeddings, at least 0.95 / 0.92 / 0.88 for strict / normal / loose. Shown as "97% alike".
- Grouped the same way as near-duplicates.
- Search method: exact pairs for up to 12,000 photos; above that, the inverted file (nprobe 4), which takes a few seconds for 300,000 photos.

**Levels (near-duplicates):** strict 4, normal 7, loose 10 bits. Random-pair false positives at 100,000 photos: about 0.2 pairs at 7 bits, about 50 at 10.

**Scopes.**
- *Library.*
- *Folder:* the folder and its sub-folders; groups form only among the photos inside it.
- *Against:* library-wide groups with members both inside and outside the folder. Each member is labelled "This folder" or "Elsewhere".
- Series ignore "against", because a series never spans folders.

**Dismissals ("Not duplicates").** All pairs of the group are stored. Exact mode skips ignored pairs in union-find. The other modes drop members that were dismissed together with the leader. A new copy brings the group back with it.

**Reject all:** exact copies only.

## Keeper rules

**Copies (exact, near-duplicates, look-alikes):**
1. not flagged rejected
2. RAW > TIFF/PNG > HEIC/JPEG
3. most pixels
4. most metadata (rating, label, title, caption, keywords, GPS, creator)
5. outside the folder (against scope)
6. RAW+JPEG pair
7. rating
8. not in a backup or export folder, and not named as a copy (whole-word match: backup, copy, export, old, temp, …; `(2)` suffix)
9. oldest mtime
10. lowest id ("Found first")

**Series:**
1. not flagged rejected
2. rating
3. picked
4. sharpness (ties within 10%)
5. format, pixels, metadata, pair
6. first frame

Culling's stacks use their own best-frame rule: rating, pick, open eyes, then the RAW sharpness measurement. The report's series keeper still uses the thumbnail estimate.

**Reason shown:** the first criterion where the keeper beats the runner-up.

## Calibration

- **pHash on real images:** a q30–q50 re-encode, a 40% resize, +18 brightness and a 1/4-scale decode moved 0–2 bits (tested on the README screenshots).
- **Smooth synthetic gradients are unstable** (6–12 bits after q50), because their coefficients cluster at the median. Test scenes need hard edges.
- **Demo burst:** frames shifted by 1–2.5% of the width are 6–14 bits apart, so the normal level can join near-identical frames.
- **Sharpness on the demo burst:** unblurred frames 0.376–0.392; a Gaussian(4) blurred frame 0.129.
- **Not yet calibrated on a real archive.** Every threshold (bits, cosine floors, the 22-bit scene cut) comes from synthetic data, the CC0 demo library and theory.

## Worker behaviour

- Passes run newest id first. A folder the report was asked about goes first. (Culling and search-by-content passes go newest folder path first instead; the dupes worker is fast enough that it was left as is.)
- It reads the thumbnail from the pack store. If that thumbnail is queued (pregen on), the photo is deferred and the pass comes back in 120 s. A missing thumbnail that isn't queued is made and stored in the pack.
- It pauses while `images.busy()` or while the scanner is not idle.
- Change-triggered passes run at most once a minute, plus one every 30 min. It emits `Event::Dupes` when it has hashed something.
- `HYPERFOCAL_DUPES=false` turns it off. `hash_now()` exists for tests.

## Performance (300,000-photo generated index, 2-vCPU VM)

| Report | Time | Notes |
|---|---|---|
| Exact | 250 ms | was 860 ms; the partial index made the duplicate-hash GROUP BY 460 → 50 ms |
| Near, normal | 440 ms | was 1.8 s; full scan 120 ms + MIH 110 ms |
| Near, loose | 960 ms | MIH 620 ms; scales with cores |
| Series | 330 ms | |
| Against | 370 ms | |
| One folder, exact | 1 ms | |
| Look-alikes, library | a few seconds | the inverted file is rebuilt on every request |

- Member details are loaded only for the groups returned (default limit 2000, newest first).
- The pixel-mode timings are checked in `tests/large_library.rs`; look-alikes are not.

## UI

- **Dialog:** `DuplicatesDialog.svelte`, with state in `lib/dupes.svelte.ts`. The scroll position and keeper picks survive a trip to the compare view.
- **Modes:** Exact copies, Near-duplicates, Look-alikes (shown once search by content is ready), Series.
- **Groups:** a group opens as a temporary collection (`id: 'dupes'`) through the `ids` list filter. Desktop shows a "Duplicates ›" breadcrumb; phones get a back-link in the header.
- **Rejects:** `store.afterMove` drops rejected photos from the report's groups, wherever they were rejected.
- **Compare:** panes show the folder when two compared photos share a name.
- **Phones:** the dialog opens from Settings → Duplicates.

## What still needs doing

Ranked by how much each closes the gap with Excire or pulls ahead of it.

| # | Item | Why | Effort | How |
|---|---|---|---|---|
| 1 | **Keeper rules you set** | Excire's flagging assistant lets you pick the criteria; ours is fixed. | M | Settings: drag to order the criteria. Add newest/oldest, largest/smallest file, "prefer folder …" and "never keep in folder …". Save as the default; a per-search override in the dialog; the reason text follows. |
| 2 | **Flag instead of move** | Excire only flags; Hyperfocal only moves. You cull alongside Excire, so flags that sync matter. | S | A per-group and a Reject-all choice: "Flag the others as rejected" (pick −1 in the XMP, which Excire reads) and optionally "pick the keeper". Same journal and undo. |
| 3 | **Merge metadata into the keeper** | Rejecting a copy that has the rating, keywords or GPS loses them from the kept photo. Excire doesn't help here. | M | Before rejecting, offer to copy missing ratings, labels, keywords, title/caption, GPS and people onto the keeper (union of keywords, highest rating); written to its sidecar and journaled. Show "+3 keywords, ★4 from the copies" on the group. |
| 4 | **Scope = the current view** | Excire's "current filmstrip". Ours is folder-only, so a search, filter, album, date range or person can't be checked. | S–M | Pass the list's `viewParams` as the scope (and "this view against the library"), reusing `build_where` to get the id set. |
| 5 | **Check every import automatically** | Beyond Excire: copies are caught as they arrive, not when you remember to look. | M | After a scan adds photos, run "new photos against the library" in exact and near modes. Badge the folder and the Duplicates entry with "N already in the library"; one click opens the against report for that folder. |
| 6 | **Exact copies without waiting for the night** | Excire hashes when asked. We wait for the integrity monitor, so an existing library shows partial results until its first nightly pass. | S | For a folder scope, hash the folder's unhashed originals right away (the integrity code already does the work), with progress in the dialog. |
| 7 | **Calibrate on the real archive** | The thresholds are untested on real data, and false positives at loose and the look-alike floors decide whether people trust the report. | S | Run all modes on Adam's library and review a sample of groups at each level. Tune the bit radii, cosine floors and scene cut, and record the numbers here. |
| 8 | **Keyboard flow** | A strength of Hyperfocal elsewhere (FRV-style culling); the dialog is mouse-only. | S | ↑/↓ or J/K between groups; ←/→ or 1–9 to choose the keeper; R rejects the others, N marks "Not duplicates", C compares, F flags. Keys shown in the help. |
| 9 | **Series keeper from culling's measurements** | The report still guesses sharpness from the thumbnail; culling measures the RAW and checks eyes. | S | Use culling's stored sharpness and eye scores when present, and fall back to the thumbnail estimate. |
| 10 | **Save results** | Excire keeps its results as collections. | S | "Save as album" for a group, and for all groups of a search (an album per group, or one album of the non-keepers to review later). |
| 11 | **Faster look-alikes over the library** | Seconds per request, because the inverted file is rebuilt every time. | S–M | Keep the index in memory and update it as embeddings arrive. Add a timing to `large_library.rs`. |
| 12 | **Finer levels** | Excire offers a range from very strict to very loose; we have three steps. | S | A five-step slider, or a slider over the bit radius and cosine floor, with the group count updating live. |
| 13 | **Grouped by folder** | Excire shows groups within groups; a long report is easier to work through folder by folder. | S | A "group by folder" toggle that sorts and heads groups by the keeper's folder. |
| — | Videos | Excire manages videos; Hyperfocal does not index them at all. | — | Only once video support exists. |

Doing 1–6 puts Hyperfocal ahead of Excire on every row of the table above except videos.
