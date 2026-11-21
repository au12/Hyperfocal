# Duplicates report: design and calibration (Nov 2026)

Exact copies, near-duplicates, look-alikes and series for Hyperfocal, modelled on Excire Foto's duplicate search. Built on branch `duplicates-report` (PR #12 on au12/hyperfocal), merged into `main` (merge commit `b73ec5c`). Look-alikes and turned/mirrored copies came with the embedding pipeline (`fb35305`, Nov 21).

## Architecture (server/src/dupes/)

| File | Role |
|---|---|
| `phash.rs` | 64-bit DCT pHash: 32×32 area-averaged Rec.601 luma, coefficients 1..8 × 1..8, median split (always 32 bits set). Flat images (std < 2) hash to 0 and are never compared. Also `sharpness()`: Laplacian energy, best 3 of a 4×4 grid, divided by the image variance (exposure-invariant). |
| `mih.rs` | All pairs within distance r: multi-index hashing, four 16-bit tables, joined bucket-by-bucket (each bucket against itself and buckets within s = r/4 bits). A pair is reported only by the first table that can see it. Tested equal to brute force. Chosen over a BK-tree, which degrades at r = 7–10. |
| `bursts.rs` | Series: sort by (folder, camera, taken, name); cut at a new folder/camera, a gap > max_gap, or consecutive pHashes > 22 bits apart. Also used by culling's stacks (`server/src/cull/series.rs`). |
| `report.rs` | Grouping, scopes, keeper, the report. |
| `worker.rs` | Background thread `dupes`: fills `phashes`. |
| `api.rs` | `GET /api/dupes?mode=exact\|near\|visual\|series`, `GET /api/dupes/status`, `POST /api/dupes/dismiss {ids, undo}`, `DELETE /api/dupes/dismissed`. |

**Schema 4:** `phashes(photo_id, size, mtime, orientation, hash, sharp, at, error)` keyed on the file state like `analysis`; `dupes_ignored(a, b)` pairs; partial index `originals_hash_ok ON originals(hash) WHERE status = 0`.

**Exact copies:** integrity `originals` rows with `status = 0`, matching the photo's current size/mtime, mapped to the shown photo with `COALESCE(pair_of, id)` (either file of a RAW+JPEG pair makes the pair a copy), joined with union-find. JPEG/HEIC/TIFF/DNG hashes cover image data only, so a copy with metadata written into it still matches. (The README demo library has a real case: the same Flickr photo uploaded twice, different metadata, one group.)

**Near-duplicates:** identical hashes are merged first (no quadratic blow-up). Then leader clustering: the hash with the most look-alikes leads, takes every unassigned neighbour within r, and so on. Members are within r of the leader (2r of each other), so a drifting timelapse does not chain.
- **Turned and mirrored copies:** each photo's seven dihedral pHash variants are queried against everyone's upright hash (multi-index hashing queries); labelled "Turned 90°" or "Mirrored" in the UI.

**Look-alikes (mode `visual`, needs the ML container):** SigLIP 2 image embeddings (int8). Crops, edits, other renderings and mirror images that pixel fingerprints miss, shown with "97% alike". Exact search for a folder; for the library an inverted file over spherical k-means cells (√n cells, int8 centroids, `server/src/ann.rs`, >98% recall against brute force), a few seconds for 300,000 photos. Strict / normal / loose.

**Levels:** strict 4, normal 7, loose 10 bits. Random-pair false positives at 100k photos: about 0.2 pairs at 7 bits, about 50 at 10.

**Scopes:** library; folder = subtree, grouping only among photos inside it; against = library-wide groups with members both inside and outside ("This folder" / "Elsewhere" on each member). Series ignore "against" (a series never spans folders).

**Dismissals ("not duplicates"):** all pairs of the group are stored. Exact mode skips ignored pairs in union-find. Near and series modes drop members ignored with the leader. A new copy brings the group back with it.

**Reject all:** only for exact copies (commit `74a0bb9`). Near-duplicates, look-alikes and series are different files, so each of those groups is rejected on its own, after a look.

## Keeper rules

**Copies (exact, near, look-alikes):**
1. not flagged rejected
2. RAW > TIFF/PNG > HEIC/JPEG
3. most pixels
4. most metadata (rating, label, title, caption, keywords, GPS, creator)
5. outside the folder (against scope)
6. RAW+JPEG pair
7. rating
8. not in a backup/export folder, and not named as a copy (whole-word match: backup, copy, export, old, temp, …; `(2)` suffix)
9. oldest mtime
10. lowest id ("Found first")

**Series:**
1. not flagged rejected
2. rating
3. picked
4. sharpness (ties within 10%)
5. format, pixels, metadata, pair
6. first frame

Culling's stacks use their own best-frame rule (rating, pick, open eyes, then the RAW sharpness measurement); the report's series keeper still uses the thumbnail estimate.

**Reason shown:** the first criterion where the keeper beats the runner-up.

## Calibration

- pHash on real images: a q30–q50 re-encode, a 40% resize, +18 brightness and a 1/4-scale decode moved 0–2 bits (tested on the README screenshots).
- Smooth synthetic gradients are unstable (6–12 bits after q50), because their coefficients cluster at the median. Test scenes need hard edges.
- Demo burst: frames shifted by 1–2.5% of the width are 6–14 bits apart. Normal level can join near-identical frames.
- Sharpness on the demo burst: unblurred frames 0.376–0.392, a Gaussian(4) blurred frame 0.129.

## Worker behaviour

- Passes run newest id first. A folder the report was asked about is prioritised. (Culling and search-by-content passes now go newest folder path first instead; the dupes worker is fast enough that it was left as is.)
- It reads the cached thumbnail. If that thumbnail is queued (pregen on), the photo is deferred and the pass comes back in 120 s. A thumbnail that is missing and not queued is made in memory and never written, to avoid racing the thumbnail writer.
- It pauses while `images.busy()` or while the scanner is not idle.
- Change-triggered passes run at most once a minute, plus a pass every 30 min. It emits `Event::Dupes` when it hashed something.
- `HYPERFOCAL_DUPES=false` turns it off. `hash_now()` exists for tests.

## Performance (300k-photo generated index, 2-vCPU VM)

| Report | Time | Notes |
|---|---|---|
| Exact | 250 ms | was 860 ms; the partial index made the duplicate-hash GROUP BY 460 → 50 ms |
| Near, normal | 440 ms | was 1.8 s; full scan 120 ms + MIH 110 ms |
| Near, loose | 960 ms | MIH 620 ms; scales with cores |
| Series | 330 ms | |
| Against | 370 ms | |
| One folder, exact | 1 ms | |

- Member details are loaded only for the groups returned (default limit 2000, newest first).
- These timings are in `tests/large_library.rs`.

## UI

- `DuplicatesDialog.svelte`, with state kept in `lib/dupes.svelte.ts` (scroll position and keeper picks survive a trip to the compare view). Modes: Exact copies, Near-duplicates, Look-alikes (when search by content is ready), Series.
- A group opens as a temporary collection `id: 'dupes'` through the `ids` list filter. Breadcrumb "Duplicates ›" on desktop; header back-link on phones.
- `store.afterMove` drops rejected photos from the report's groups.
- Compare panes show the folder when two compared photos share a name.
- On phones the dialog opens from Settings → Duplicates.

## Open

- Bring the report's series keeper onto culling's RAW sharpness measurements (it still uses the thumbnail estimate).
