# Roadmap

Ranked by value to a large personal RAW archive, weighed against effort. Effort: **S** small, **M** medium, **L** large.

Every item keeps the rules in the README's [Safety](README.md#safety) section:

- originals are never written or deleted;
- changes go to XMP sidecars, and only when you approve them;
- the core stays a small, fast single binary.

Heavy features are optional add-ons.

**If only three things get built next:**

1. One embedding pipeline. It powers both text search and "similar photos".
2. A duplicates report: exact copies come free with the integrity monitor's content hashes.
3. Instant change detection, so edits made in Excire show up in seconds.

## AI features

### How they fit

AI runs in an **optional companion container** (`hyperfocal-ml`), so the main image stays small. It talks to Hyperfocal over a small HTTP API and uses ONNX Runtime, on an NVIDIA GPU when one is passed through (a 4 GB card is plenty) and on the CPU otherwise. It works from the 512 px thumbnails Hyperfocal already makes, plus larger previews for faces, all fetched through Hyperfocal's API. It never decodes a RAW file and doesn't need the photo share mounted at all.

Results live in Hyperfocal's database, never in the photos:

- an embedding of about 1 KB per photo (300,000 photos ≈ 300 MB);
- face boxes and clusters;
- scores.

Anything worth keeping beyond the app (keywords, person names, face regions) goes to sidecars only after you accept it, journaled and undoable like every other edit. Indexing runs at idle, newest folders first, and resumes after restarts.

| # | Feature | Value | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Text search** ("kids on a beach at sunset", "red car at night") | Very high | M | SigLIP 2 or CLIP image and text embeddings, ranked by cosine similarity and combined with the existing filters and full-text search. Brute force over 300,000 vectors takes tens of milliseconds on a NAS CPU; an HNSW index (usearch) brings that to a few milliseconds if needed. |
| 2 | **Similar photos** ("more like this") | High | S | Free once #1 exists: the nearest neighbours of a photo's embedding, across the whole library or within a date range. |
| 3 | **Duplicates and near-duplicates** | High | S–M | Exact copies (the same file imported into two folders) come free with the integrity monitor's content hashes, which every original now has. Near-duplicates (edits, exports, re-saves) by perceptual hash plus embedding distance. Report-only, with a jump to each copy. Hyperfocal still never deletes: you reject the copy you don't want. |
| 4 | **Bursts and best-of-burst** | High | M | Group frames shot seconds apart that look alike into one stack. Rank them by sharpness, open eyes and exposure, and suggest the keeper. Nothing is rejected automatically. |
| 5 | **Faces** | High | L | Detection (SCRFD), recognition embeddings (ArcFace) and clustering (HDBSCAN) into "unnamed people" you can name, merge and split. Names go to sidecars as person keywords, which Excire reads like any other keyword, and as MWG face regions (`mwg-rs:Regions`), the format Lightroom Classic and digiKam use. Regions those apps wrote are read too. Nothing leaves the server. Check model licences: InsightFace's pretrained weights are for non-commercial research use only. |
| 6 | **Cull assist** | Medium–high | M | Per-photo scores: sharpness on the subject or face (from the RAW, not the JPEG), motion blur, closed eyes, and clipping (measured on the RAW data, as the view tools already do). Optionally an aesthetic score from the CLIP embedding. Shown as "possible rejects" filters, never as automatic rejects. |
| 7 | **Auto keywords and captions** | Medium | M | Zero-shot tags matched against your own keyword tree first; optionally captions from a small vision-language model. Bulk accept/reject UI, then written to sidecars. |

## Functionality

| # | Item | Value | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Capture-time shift** | Medium | S | Fix a camera clock or time zone for a selection. The corrected time goes to the sidecars as `exif:DateTimeOriginal`, which Lightroom reads. |
| 2 | **Video files** | Medium | M | Thumbnails and playback for MOV/MP4 from phones and cameras, and Live Photo pairs (HEIC + MOV) grouped like RAW+JPEG. ffmpeg would run in the helper process, like libheif. |
| 3 | **Excire round-trip helpers** | Medium | S | Writes already re-read the sidecar and change only the fields you edited. Add a "changed outside Hyperfocal" view (what Excire or another app changed since the last scan), and re-check a photo's sidecar when you open it, so the panel is never up to 15 minutes out of date. |
| 4 | **Offline place names** | Medium | S | Bundle GeoNames cities for place search and reverse geocoding, so they work without OpenStreetMap's Nominatim service. Map tiles would still come from OpenStreetMap unless a local tile server is added. |
| 5 | **Export** | Medium | S–M | Resized JPEGs of a selection, with metadata and an optional watermark, as a download or into an export folder outside the library. |
| 6 | **Immich hand-off** | Medium | S | Send picks or a selection to an Immich album through Immich's API, for libraries Immich already reads as an external library. |
| 7 | **Users and single sign-on** | Low–medium | M | OIDC through Authelia or Authentik, or trusted headers from the reverse proxy, plus read-only accounts for family. |
| 8 | **Slideshow** | Low | S | Full-screen slideshow of a selection or a collection. |

## Performance

| # | Item | Gain | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Instant change detection** | Edits made in Excire appear in seconds instead of up to 15 minutes | S–M | inotify watches on the library, with the periodic rescan kept as a safety net: changes made outside the share path (for example straight to a disk or pool path) raise no events. |
| 2 | **Packed thumbnail store** | Faster cold starts and much faster appdata backups | S–M | 300,000 thumbnails as 300,000 files make every appdata backup crawl. Pack them into a few large append-only files with an index in SQLite. Until then, `cache` inside AppData can be left out of backups: it is rebuilt automatically, and the database and sidecar backups live outside it. |
| 3 | **Tiled pyramid for zoomed views** | Smooth 1:1 panning of 45–100 MP images | M | RAW files already zoom in through full-resolution tiles. JPEG and HEIC files still load one large image: serve them as tiles at each zoom level too. |
| 4 | **Smaller previews** | Less data on phones and slow Wi-Fi | S | Encode cached previews with jpegli (still ordinary JPEGs, so browsers are unaffected) or serve WebP. Measure size, quality and encode time on real photos first. |

## Platform and quality

| # | Item | Effort | Notes |
| --- | --- | --- | --- |
| 1 | **LibRaw 0.22** | S–M | Debian stable ships LibRaw 0.21, which lacks colour data for some cameras released since 2022. LibRaw 0.22 is out: take it when Debian stable ships it, or build it from source in the image and follow upstream security releases yourself. Low urgency, because FlashRaw decodes those cameras first and LibRaw is only the fallback. |
| 2 | **arm64 image** | S | Multi-arch build for ARM NAS boxes and the Raspberry Pi 5. |
| 3 | **Community Applications listing** | S | Publish the Unraid template to Community Applications, so installing is a search away. |

## Done

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
