# Roadmap

Ranked by value to a large personal RAW archive, weighed against effort. Effort: **S** small, **M** medium, **L** large.

Every item keeps the rules in the README's [Safety](README.md#safety) section:

- originals are never written or deleted;
- changes go to XMP sidecars, and only when you approve them;
- the core stays a small, fast single binary.

Heavy features are optional add-ons.

**If only three things get built next:**

1. A compare view.
2. One embedding pipeline. It powers both text search and "similar photos".
3. An archive integrity monitor.

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
| 3 | **Duplicates and near-duplicates** | High | S–M | Exact copies (the same file imported into two folders) come free with the integrity monitor's content hashes (Functionality 2). Near-duplicates (edits, exports, re-saves) by perceptual hash plus embedding distance. Report-only, with a jump to each copy. Hyperfocal still never deletes: you reject the copy you don't want. |
| 4 | **Bursts and best-of-burst** | High | M | Group frames shot seconds apart that look alike into one stack. Rank them by sharpness, open eyes and exposure, and suggest the keeper. Nothing is rejected automatically. |
| 5 | **Faces** | High | L | Detection (SCRFD), recognition embeddings (ArcFace) and clustering (HDBSCAN) into "unnamed people" you can name, merge and split. Names go to sidecars as person keywords, which Excire reads like any other keyword, and as MWG face regions (`mwg-rs:Regions`), the format Lightroom Classic and digiKam use. Regions those apps wrote are read too. Nothing leaves the server. Check model licences: InsightFace's pretrained weights are for non-commercial research use only. |
| 6 | **Cull assist** | Medium–high | M | Per-photo scores: sharpness on the subject or face (from the RAW, not the JPEG), motion blur, closed eyes, and clipping (measured on the RAW data, as the view tools already do). Optionally an aesthetic score from the CLIP embedding. Shown as "possible rejects" filters, never as automatic rejects. |
| 7 | **Auto keywords and captions** | Medium | M | Zero-shot tags matched against your own keyword tree first; optionally captions from a small vision-language model. Bulk accept/reject UI, then written to sidecars. |

## Functionality

| # | Item | Value | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Compare view** | Very high | S–M | Two to four photos side by side with synced zoom and pan, like Lightroom's Survey view. Pick and reject from the compare grid. |
| 2 | **Archive integrity monitor** | Very high | M | Record a fast content hash (xxh3) of every original when it is first indexed, then re-verify slowly in the background (for example a full pass every 90 days, paced so array disks aren't kept spinning). Report any original that changed or vanished, in the UI and optionally through a webhook such as ntfy or Gotify. Report-only. Disk-level checks catch bit-rot; this also catches an app or script that rewrites, truncates or removes an original, which to the disk is an ordinary write. For JPEG and HEIC files, which some apps write metadata into, the hash covers only the image data, so a metadata edit isn't reported as damage. Early warning matters because backups eventually rotate out the good copy. |
| 3 | **Full-resolution RAW view** | High | M | The RAW view is half resolution today (one pixel per 2x2 sensor block). On demand, demosaic only the visible area at 1:1 (RCD- or AMaZE-class) for focus checks without switching to the embedded JPEG. |
| 4 | **Timeline and smart collections** | High | S | Browse by capture date across folders (year, month, day), and save searches as collections, such as "5★ picks from 2019 with no location". |
| 5 | **Keyword manager** | Medium–high | M | Rename, merge and re-parent keywords across the library as one journaled, undoable bulk sidecar write. |
| 6 | **Geotag from a track** | Medium–high | M | Match capture times against GPX files or a phone's location-history export, preview the result on the map, then write GPS to the sidecars. |
| 7 | **Capture-time shift** | Medium | S | Fix a camera clock or time zone for a selection. The corrected time goes to the sidecars as `exif:DateTimeOriginal`, which Lightroom reads. |
| 8 | **Video files** | Medium | M | Thumbnails and playback for MOV/MP4 from phones and cameras, and Live Photo pairs (HEIC + MOV) grouped like RAW+JPEG. ffmpeg would run in the helper process, like libheif. |
| 9 | **Excire round-trip helpers** | Medium | S | Writes already re-read the sidecar and change only the fields you edited. Add a "changed outside Hyperfocal" view (what Excire or another app changed since the last scan), and re-check a photo's sidecar when you open it, so the panel is never up to 15 minutes out of date. |
| 10 | **Offline place names** | Medium | S | Bundle GeoNames cities for place search and reverse geocoding, so they work without OpenStreetMap's Nominatim service. Map tiles would still come from OpenStreetMap unless a local tile server is added. |
| 11 | **Export** | Medium | S–M | Resized JPEGs of a selection, with metadata and an optional watermark, as a download or into an export folder outside the library. |
| 12 | **Immich hand-off** | Medium | S | Send picks or a selection to an Immich album through Immich's API, for libraries Immich already reads as an external library. |
| 13 | **Users and single sign-on** | Low–medium | M | OIDC through Authelia or Authentik, or trusted headers from the reverse proxy, plus read-only accounts for family. |
| 14 | **Slideshow** | Low | S | Full-screen slideshow of a selection or a collection. |

## Performance

| # | Item | Gain | Effort | Notes |
| --- | --- | --- | --- | --- |
| 1 | **Instant change detection** | Edits made in Excire appear in seconds instead of up to 15 minutes | S–M | inotify watches on the library, with the periodic rescan kept as a safety net: changes made outside the share path (for example straight to a disk or pool path) raise no events. |
| 2 | **Pre-render previews at idle** | The first open of a new import is instant | S | Build screen-size previews for recently imported folders in the background, as thumbnails already are, within the preview cache budget. |
| 3 | **Faster HEIC thumbnails** | Grid thumbnails without a full HEVC decode | S | Use the thumbnail image most HEIC files embed when it is large enough for the grid, and decode the full image only when it isn't. Check the sizes real phones embed first. |
| 4 | **Packed thumbnail store** | Faster cold starts and much faster appdata backups | S–M | 300,000 thumbnails as 300,000 files make every appdata backup crawl. Pack them into a few large append-only files with an index in SQLite. Until then, `cache` inside AppData can be left out of backups: it is rebuilt automatically, and the database and sidecar backups live outside it. |
| 5 | **Tiled pyramid for zoomed views** | Smooth 1:1 panning of 45–100 MP images | M | Serve only the visible tiles at each zoom level instead of one large JPEG. Pairs with Functionality 3. |
| 6 | **Database tuning at 300,000 photos** | Faster filters and search | S | Covering indexes for the common filters, `mmap_size`, and an FTS5 trigram index for substring search. Measure on a generated large library first (Platform 2). |
| 7 | **Offline cache on phones** | Instant re-visits; recent folders browsable offline | S | A service worker caching thumbnails and previews. It needs HTTPS, which the reverse proxy provides. |
| 8 | **Smaller previews** | Less data on phones and slow Wi-Fi | S | Encode cached previews with jpegli (still ordinary JPEGs, so browsers are unaffected) or serve WebP. Measure size, quality and encode time on real photos first. |
| 9 | **SIMD and GPU development** | Faster RAW views on large sensors | M | Portable SIMD for the binning and tone pass; a GPU path for the full-resolution demosaic. |

## Platform and quality

| # | Item | Effort | Notes |
| --- | --- | --- | --- |
| 1 | **Fuzzing** | S | cargo-fuzz targets for the HEIF metadata reader, the XMP parser, the JPEG header reader and the helper protocol. They are pure Rust, so the risk is panics and runaway allocations rather than memory corruption; fuzzing finds those inputs before a damaged file in the archive does. |
| 2 | **Large-library test** | S | A generated 300,000-photo library in CI, to catch slow queries and memory growth before they reach a real archive. |
| 3 | **LibRaw 0.22** | S–M | Debian stable ships LibRaw 0.21, which lacks colour data for some cameras released since 2022. LibRaw 0.22 is out: take it when Debian stable ships it, or build it from source in the image and follow upstream security releases yourself. Low urgency, because FlashRaw decodes those cameras first and LibRaw is only the fallback. |
| 4 | **arm64 image** | S | Multi-arch build for ARM NAS boxes and the Raspberry Pi 5. |
| 5 | **Community Applications listing** | S | Publish the Unraid template to Community Applications, so installing is a search away. |
