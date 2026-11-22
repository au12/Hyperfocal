# Roadmap

Ranked by value to a large personal RAW archive, weighed against effort. Effort: **S** small, **M** medium, **L** large.

Every item keeps the rules in the README's [Safety](README.md#safety) section:

- originals are never written or deleted;
- changes go to XMP sidecars, and only when you approve them;
- the core stays a small, fast single binary.

Heavy features are optional add-ons.

**If only three things get built next:**

1. Instant change detection, so edits made in Excire show up in seconds.
2. Auto keywords from the search embeddings that already exist, matched against your own keyword tree.
3. Excire round-trip helpers: a "changed outside Hyperfocal" view, and sidecars re-read when a photo is opened.

## AI features

### How they fit

AI runs **inside the Hyperfocal image, in a process of its own** (the AI engine, `hyperfocal ml-engine`: ONNX Runtime through Rust, on the CPU or an NVIDIA GPU), which the server starts, watches and restarts. The image stays small: the models, and for a GPU ONNX Runtime's CUDA build with NVIDIA's libraries, are downloaded into `/config/ml` the first time a feature needs them, and every AI feature can be switched off. The server hands the engine an upright JPEG of each photo over a local connection and keeps everything it answers; the engine never decodes a RAW file or opens the photo share. New models are added the same way: pinned by sha256 in `server/src/ml/manifest.json`, licensed for commercial use, downloaded on demand.

Results live in Hyperfocal's database, never in the photos:

- embeddings (int8: 512 bytes a face; about 1 KB a photo for search);
- face and pet boxes, groups and names;
- scores.

Anything worth keeping beyond the app (keywords, person names, face regions) goes to sidecars only after you accept it, journaled and undoable like every other edit. Analysis runs at idle, photos on screen first, and resumes after restarts.

| # | Feature | Value | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Auto keywords and captions** | Medium–high | S–M | The SigLIP 2 embeddings every photo already has make zero-shot tags cheap: score your own keyword tree's terms against each photo (text embeddings, computed once), suggest the clear winners, accept or reject in bulk, then write them to the sidecars. Captions would need a small vision-language model in the AI engine. |
| 2 | **Aesthetic ranking** | Medium | M | A learned "would you keep this" score as one more tie-breaker for the best frame of a series and a sort. No head with a commercial licence exists for SigLIP 2 yet; training one on your own picks and rejects is the honest version. |
| 3 | **More from People & pets** | Medium | S–M | Birthdays and "who was there" for a period; a person's photos by year; manual face boxes for faces the detector misses; more animals (horses, birds) when someone asks. |
| 4 | **Group shots** | Medium | M | For a series of a group: who has their eyes closed in which frame, and the frame where the most people look best (the face strip already lines them up; this would rank them). |

## Functionality

| # | Item | Value | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Capture-time shift** | Medium | S | Fix a camera clock or time zone for a selection. The corrected time goes to the sidecars as `exif:DateTimeOriginal`, which Lightroom reads. |
| 2 | **Video files** | Medium | M | Thumbnails and playback for MOV/MP4 from phones and cameras, and Live Photo pairs (HEIC + MOV) grouped like RAW+JPEG. ffmpeg would run in the helper process, like libheif. |
| 3 | **Excire round-trip helpers** | Medium | S | Writes already re-read the sidecar and change only the fields you edited. Add a "changed outside Hyperfocal" view (what Excire or another app changed since the last scan), and re-check a photo's sidecar when you open it, so the panel is never up to 15 minutes out of date. |
| 4 | **Offline place names** | Medium | S | Bundle GeoNames cities for place search and reverse geocoding, so they work without OpenStreetMap's Nominatim service. Map tiles would still come from OpenStreetMap unless a local tile server is added. |
| 5 | **Export** | Medium | S–M | Resized JPEGs of a selection, with metadata and an optional watermark, as a download or into an export folder outside the library. |
| 6 | **Users and single sign-on** | Low–medium | M | OIDC through Authelia or Authentik, or trusted headers from the reverse proxy, plus read-only accounts for family. |
| 7 | **Slideshow** | Low | S | Full-screen slideshow of a selection, a collection or an album. |

## Performance

| # | Item | Gain | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Instant change detection** | Edits made in Excire appear in seconds instead of up to 15 minutes | S–M | inotify watches on the library, with the periodic rescan kept as a safety net: changes made outside the share path (for example straight to a disk or pool path) raise no events. |
| 2 | **Search index** | Search by content stays in tens of milliseconds past a million photos | S | A search compares the query with every photo's int8 embedding (AVX2), about 50 ms for 300,000 photos; an HNSW index (usearch) is the next step if libraries grow far past that. |

## Platform and quality

| # | Item | Effort | Notes |
| --- | --- | --- | --- |
| 1 | **LibRaw 0.22** | S–M | Debian stable ships LibRaw 0.21, which lacks colour data for some cameras released since 2022. LibRaw 0.22 is out: take it when Debian stable ships it, or build it from source in the image and follow upstream security releases yourself. Low urgency, because FlashRaw decodes those cameras first and LibRaw is only the fallback. |
| 2 | **arm64 image** | S | Multi-arch build for ARM NAS boxes and the Raspberry Pi 5. |
| 3 | **Community Applications listing** | S | Publish the Unraid template to Community Applications, so installing is a search away. |

## Done

- **People & pets, set right and tidied up.** Every face and animal has a menu (⋯ or right-click) in the person's sheet, the info panel and on the photo. *Not a face* (*Not a dog or cat*) takes a false detection out of everything, culling included, for good; *It's a dog* / *It's a cat* turns a pet's face the face detector took for a person's into that animal, embedded by the pet model from its head so it finds its pet, and kept when the photo is analysed again. Whole unnamed groups too. The People view, the person sheet, the info panel and the boxes on the photo were reworked: a review banner, *Next to review*, faces in the name suggestions, a find field, cross-highlighting between the panel and the photo.
- **One image for everything.** The AI features run in the Hyperfocal image (a Rust port of the Python `hyperfocal-ml` container, with the same results: OpenCV's preprocessing reproduced, the same ONNX Runtime), configured with `HYPERFOCAL_ML_*` variables, with models and the GPU runtime downloaded into `/config/ml` on demand and checked against their sha256s.
- **Culling help.** Series as stacks in the grid, topped by their best frame (your rating or pick, then open eyes, then sharpness), unfolded in place; *Keep best* rejects the rest in one undoable move. Every photo is measured once in the background: sharpness of the subject (faces, else the sharpest part) on the RAW's half-size development, with the sensor noise taken out first; clipping from the RAW data; closed eyes from MediaPipe's face landmarks and blendshapes, run in the AI engine. "Possible rejects" filters (blurry, eyes closed, clipped, too dark, weaker frames of a series), a sharpness sort, and a strip of every face across the series in the single view. Nothing is rejected automatically.
- **People filters.** How many people are in a photo, portraits or groups, eyes open or closed, from the faces People & pets found (doubtful detections and passers-by left out).
- **Search by content and Find similar.** SigLIP 2 image and text embeddings from the AI engine, kept as int8 in the index; any description, in many languages, ranked and combined with the filters; the photos most like one; and look-alikes (crops, edits, mirror images) in the Duplicates report through an inverted-file index. Near-duplicates also catch turned and mirrored copies.
- **Albums.** Hand-picked photos in an order of their own, with a cover, a note and drag-to-arrange; a photo that moved is found again.
- **Send to Immich.** A selection or an album into a new or existing Immich album; photos Immich already has, by path or by content, are never uploaded twice; RAW files go as full-size JPEGs with their metadata, or as originals.
- **Packed thumbnail store.** Append-only pack files with checksums and a hint file per pack: a thumbnail is one positioned read, start-up skips 300,000 directory entries, and appdata backups copy a few large files.
- **Tiled zoom for every image.** JPEG, HEIC, TIFF, PNG and embedded JPEGs zoom through a pyramid of tiles, like RAW files.
- **Smaller previews.** Thumbnails and previews made in the background are encoded with mozjpeg's trellis quantisation: 7–12% smaller at the same SSIMULACRA2 score. WebP and jpegli were measured too: WebP saved less and is slow to encode, and jpegli only pays off as a C++ build.
- **Faster regrouping.** 64-byte sign codes are compared before the int8 vectors: the same matches, a 600,000-face regroup in 11 minutes instead of 70.
- **Overexposure overlay follows the exposure correction**, as FastRawViewer's does.
- **Duplicates: Reject all only for exact copies;** other kinds of group are rejected one at a time, after a look.
- **UI refresh.** A graphite-and-glass design set in Inter, with small animations for actions and a reduced-motion fallback.
- **Duplicates report.** Exact copies from the integrity hashes (image data only, so copies with other metadata still match), near-duplicates from a 64-bit DCT perceptual hash of each thumbnail matched by multi-index hashing (every pair within the distance: the whole report for 300,000 photos takes under half a second on a 2-vCPU VM, about a second at the loose level), and series; for the library, a folder, or a folder against the library. Each group suggests a keeper and why, opens in the compare view, can be dismissed as "not duplicates", and rejects the others in one journaled move.
- **People & pets.** The AI engine (CPU or NVIDIA) finds faces (YuNet at three scales), dogs and cats (RF-DETR Small) and embeds them (AuraFace, pet re-identification), all models licensed for commercial use. Hyperfocal matches them in memory (int8, AVX2), forms unnamed groups, and lets you name, merge, review suggestions, reject, hide and ignore; confirmed names go to the sidecars as `People|`/`Pets|` keywords, MWG regions and IPTC person shown, and names other apps wrote come in. Undo is read back from the sidecars.
- **Compare view.** Two to four photos side by side, zoom and pan synced (1:1) or separate, rating and rejecting from the panes, and stepping one pane through the folder against the others.
- **Archive integrity monitor.** Content hashes of every original (image data only for JPEG, HEIC and TIFF/DNG), verified again in a paced nightly window, with moves recognised by hash, and alerts through ntfy, Gotify or a webhook.
- **Full-resolution RAW view.** RCD demosaicing of the visible tiles at 1:1, on the device's GPU (WebGL2) or on the server.
- **Timeline and saved collections.** Capture-date browsing across folders; any search saved as a collection.
- **Keyword manager.** Rename, move, merge and remove keywords library-wide as one undoable change.
- **Geotag from a track.** GPX, KML, TCX, NMEA and phone location history, with time-zone detection, clock drift and a map preview.
- **Previews of recent imports at idle,** within the preview cache budget.
- **Faster HEIC thumbnails** from the thumbnail image phones embed.
- **Database tuning at 300,000 photos:** covering indexes, `mmap_size`, a trigram index for substring search.
- **Offline copies** of thumbnails, previews and folder lists through a service worker.
- **SIMD and GPU development:** a vectorised develop pass (2.9× faster, AVX2 picked at run time) and the GPU demosaic.
- **Rebindable keyboard shortcuts,** with reset to the defaults.
- **Fuzzing:** cargo-fuzz targets for the HEIF, XMP, JPEG and TIFF readers, GPS tracks and the helper's output, run weekly, plus a smoke test on every push. It found two NMEA inputs that crashed the track parser.
- **Large-library test:** a generated 300,000-photo index through the real router, in CI.
