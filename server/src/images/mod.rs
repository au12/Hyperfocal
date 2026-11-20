//! Image service: thumbnails, previews, embedded JPEG pass-through and RAW renders,
//! with disk and memory caches. Everything here only *reads* the library; caches
//! live in the cache directory.

pub mod canon;
#[cfg(any(feature = "heic", test))]
pub mod color;
pub mod demosaic;
pub mod jpeg;
pub mod raw;
pub mod simd;
pub mod tone;

use crate::config::Config;
use crate::model::{Kind, PhotoRec};
use crate::native;
use crate::repo::{self, Located};
use crate::scan;
use crate::state::AppState;
use anyhow::{Result, anyhow};
use jpeg::Rgb;
use parking_lot::{Condvar, Mutex};
use raw::{Developed, WbMode};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tone::ToneMode;

pub const THUMB_LONG: usize = 512;
/// Exposure added to every RAW render: none. The tone curves (`tone`) are calibrated on
/// FastRawViewer's output, and the developed data already renders Canon's NormalWhiteLevel
/// as white (see `raw::BayerFrame::render_white`). Kept for the API's `boost_ev`.
pub const RENDER_BOOST_EV: f32 = 0.0;

/// Byte-weighted LRU (small number of large entries).
pub struct Lru<K, V> {
    map: HashMap<K, (V, u64, usize)>,
    tick: u64,
    bytes: usize,
    cap: usize,
}

impl<K: Hash + Eq + Clone, V: Clone> Lru<K, V> {
    pub fn new(cap: usize) -> Self {
        Lru { map: HashMap::new(), tick: 0, bytes: 0, cap }
    }
    pub fn get(&mut self, k: &K) -> Option<V> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(k).map(|e| {
            e.1 = t;
            e.0.clone()
        })
    }
    pub fn put(&mut self, k: K, v: V, size: usize) {
        self.tick += 1;
        if let Some(old) = self.map.insert(k, (v, self.tick, size)) {
            self.bytes -= old.2;
        }
        self.bytes += size;
        while self.bytes > self.cap && self.map.len() > 1 {
            let oldest = self.map.iter().min_by_key(|(_, e)| e.1).map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    if let Some(e) = self.map.remove(&k) {
                        self.bytes -= e.2;
                    }
                }
                None => break,
            }
        }
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// Counts a viewer request in [`Images::interactive`] for as long as it lives. A guard,
/// not a pair of calls: a request the browser gives up on (a thumbnail scrolled past, a
/// tile panned away) is dropped at an `.await`, and the count must still go down, or the
/// background thumbnail builder and preview pre-generation wait forever.
struct Busy<'a>(&'a AtomicUsize);

impl<'a> Busy<'a> {
    fn new(n: &'a AtomicUsize) -> Busy<'a> {
        n.fetch_add(1, Ordering::SeqCst);
        Busy(n)
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Lets the photo on screen jump ahead of prefetching.
pub struct Gate {
    sem: tokio::sync::Semaphore,
    total: usize,
    high: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl Gate {
    fn new(permits: usize) -> Gate {
        Gate { sem: tokio::sync::Semaphore::new(permits.max(1)), total: permits.max(1), high: AtomicUsize::new(0), notify: tokio::sync::Notify::new() }
    }
    pub async fn acquire(&self, high: bool) -> tokio::sync::SemaphorePermit<'_> {
        if high {
            self.high.fetch_add(1, Ordering::SeqCst);
            let p = self.sem.acquire().await.expect("gate closed");
            self.high.fetch_sub(1, Ordering::SeqCst);
            self.notify.notify_waiters();
            return p;
        }
        loop {
            if self.high.load(Ordering::SeqCst) == 0 {
                if let Ok(p) = self.sem.try_acquire() {
                    return p;
                }
            }
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(15)) => {}
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct DevKey {
    id: i64,
    sig: u64,
    wb: WbMode,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RenderKey {
    dev: DevKey,
    mode: ToneMode,
    ev: i32,
    long: usize,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TileKey {
    dev: DevKey,
    /// None: the demosaic input for the viewer's GPU path; mode and ev do not apply.
    mode: Option<ToneMode>,
    ev: i32,
    tx: usize,
    ty: usize,
}

type Inflight = Arc<tokio::sync::Mutex<Option<std::result::Result<Arc<Developed>, String>>>>;
type BayerInflight = Arc<tokio::sync::Mutex<Option<std::result::Result<Arc<raw::Bayer>, String>>>>;

pub struct Images {
    pub index_pool: rayon::ThreadPool,
    thumbs_dir: PathBuf,
    previews_dir: PathBuf,
    preview_budget: u64,
    thumb_sem: tokio::sync::Semaphore,
    gate: Gate,
    developed: Mutex<Lru<DevKey, Arc<Developed>>>,
    inflight: Mutex<HashMap<DevKey, Inflight>>,
    rendered: Mutex<Lru<RenderKey, Arc<Vec<u8>>>>,
    luts: Mutex<HashMap<(ToneMode, i32), Arc<Vec<u8>>>>,
    queue: Mutex<(VecDeque<i64>, HashSet<i64>)>,
    queue_cv: Condvar,
    interactive: AtomicUsize,
    /// Preview requests per size, for pre-rendering at the size viewers use.
    preview_sizes: Mutex<[u64; PREVIEW_SIZES.len()]>,
    /// Sensor data of the photos being inspected, for full-resolution tiles.
    bayers: Mutex<Lru<(i64, u64), Arc<raw::Bayer>>>,
    bayer_inflight: Mutex<HashMap<(i64, u64), BayerInflight>>,
    tiles: Mutex<Lru<TileKey, Arc<Vec<u8>>>>,
    /// Tiles demosaic on one core each: about as many at once as there are cores.
    tile_sem: tokio::sync::Semaphore,
}

/// The preview sizes (long edge) the API serves.
pub const PREVIEW_SIZES: [usize; 6] = [1024, 1600, 2048, 2560, 3200, 4096];

/// Content signature of a photo for cache keys: changes when the file, its size or
/// its effective orientation changes.
pub fn sig(rec: &PhotoRec) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for v in [rec.size, rec.mtime, rec.orientation as i64, rec.id] {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

pub fn sig_str(rec: &PhotoRec) -> String {
    format!("{:x}", sig(rec) & 0xffff_ffff_ffff)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Decoded display source for a photo, in sensor orientation (HEIF: upright), with a
/// long edge of at least `min_long` where the file allows it.
pub fn load_display_rgb(path: &Path, kind: Kind, min_long: usize) -> Result<Rgb> {
    match kind {
        Kind::Raw => load_raw_display(path, min_long),
        Kind::Jpeg => {
            let data = std::fs::read(path)?;
            jpeg::decode_scaled(&data, min_long)
        }
        Kind::Tiff | Kind::Png => {
            let img = image::ImageReader::open(path)?.with_guessed_format()?.decode()?.to_rgb8();
            let (w, h) = (img.width() as usize, img.height() as usize);
            Ok(Rgb { w, h, px: img.into_raw() })
        }
        // An embedded thumbnail when it is big enough, else the primary image.
        Kind::Heic => Ok(native::heic_thumb(path, min_long, 0)?.0),
    }
}

/// HEIC drafts: an embedded thumbnail at least this big is shown at once, while the
/// full-quality thumbnail is made (iPhones embed 320x240).
pub const DRAFT_MIN: usize = 240;

/// An embedded preview at least this big (or `min_long`, if smaller) is good enough;
/// otherwise the RAW data is developed.
fn good_enough(long: usize, min_long: usize) -> bool {
    long >= min_long.min(1024)
}

/// RAW: the smallest embedded JPEG that is big enough (FlashRaw, else LibRaw), else a
/// development of the sensor data.
fn load_raw_display(path: &Path, min_long: usize) -> Result<Rgb> {
    let flash_ok = !native::prefer_libraw()
        && match raw::catch(|| flash_preview(path, min_long)) {
            Ok(Some(img)) => return Ok(img),
            Ok(None) => true,
            Err(_) => false,
        };
    // FlashRaw cannot read the file (or LibRaw is preferred): LibRaw's previews.
    if !flash_ok && native::LIBRAW {
        match native::libraw_preview(path, min_long) {
            Ok(p) if good_enough(p.long_edge(), min_long) => match p {
                native::Preview::Jpeg { data, .. } => return jpeg::decode_scaled(&data, min_long),
                native::Preview::Rgb(img) => return Ok(img),
            },
            Ok(_) => {}
            Err(e) => tracing::debug!("LibRaw preview of {}: {e:#}", path.display()),
        }
    }
    let dev = raw::develop_path(path, WbMode::Asshot, 10.0)?;
    let lut = tone::build_lut(ToneMode::Normal, RENDER_BOOST_EV);
    Ok(raw::render(&dev, &lut, min_long.max(1)))
}

/// FlashRaw's embedded JPEG: Ok(None) when the file has none that is big enough.
fn flash_preview(path: &Path, min_long: usize) -> Result<Option<Rgb>> {
    let raw = flashraw::FlashRaw::open_path(path).map_err(|e| anyhow!("{e}"))?;
    let mut jpegs: Vec<&flashraw::PreviewInfo> =
        raw.previews().iter().filter(|p| p.encoding == flashraw::PreviewEncoding::Jpeg && p.width > 0).collect();
    jpegs.sort_by_key(|p| p.width as u64 * p.height as u64);
    let pick = jpegs.iter().find(|p| (p.width.max(p.height) as usize) >= min_long).or(jpegs.last()).copied();
    if let Some(p) = pick
        && good_enough(p.width.max(p.height) as usize, min_long)
        && let Ok(bytes) = raw.preview_bytes(p)
        && let Ok(img) = jpeg::decode_scaled(bytes, min_long)
    {
        return Ok(Some(img));
    }
    Ok(None)
}

/// The largest embedded JPEG of a RAW file, if it is at least 1000 pixels wide.
fn full_embedded_jpeg(path: &Path) -> Option<Vec<u8>> {
    if !native::prefer_libraw() {
        match raw::catch(|| flashraw::FlashRaw::open_path(path)) {
            Ok(raw) => {
                let best = raw
                    .previews()
                    .iter()
                    .filter(|p| p.encoding == flashraw::PreviewEncoding::Jpeg)
                    .max_by_key(|p| p.width as u64 * p.height as u64)
                    .cloned()?;
                if best.width < 1000 {
                    return None;
                }
                return raw.preview_bytes(&best).ok().map(jpeg::strip_exif);
            }
            Err(_) if !native::LIBRAW => return None,
            Err(_) => {}
        }
    }
    match native::libraw_preview(path, usize::MAX) {
        Ok(native::Preview::Jpeg { data, w, h }) if w.max(h) >= 1000 => Some(jpeg::strip_exif(&data)),
        _ => None,
    }
}

impl Images {
    pub fn new(cfg: &Config) -> Result<Images> {
        let index_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.index_threads.max(1))
            .thread_name(|i| format!("index-{i}"))
            .build()?;
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        Ok(Images {
            index_pool,
            thumbs_dir: cfg.cache_dir.join("thumbs"),
            previews_dir: cfg.cache_dir.join("previews"),
            preview_budget: cfg.preview_cache_mb as u64 * 1024 * 1024,
            thumb_sem: tokio::sync::Semaphore::new(cores.clamp(2, 8)),
            gate: Gate::new(cfg.decode_concurrency),
            developed: Mutex::new(Lru::new(cfg.raw_cache_mb * 1024 * 1024)),
            inflight: Mutex::new(HashMap::new()),
            rendered: Mutex::new(Lru::new(256 * 1024 * 1024)),
            luts: Mutex::new(HashMap::new()),
            queue: Mutex::new((VecDeque::new(), HashSet::new())),
            queue_cv: Condvar::new(),
            interactive: AtomicUsize::new(0),
            preview_sizes: Mutex::new([0; PREVIEW_SIZES.len()]),
            bayers: Mutex::new(Lru::new(320 * 1024 * 1024)),
            bayer_inflight: Mutex::new(HashMap::new()),
            tiles: Mutex::new(Lru::new(96 * 1024 * 1024)),
            tile_sem: tokio::sync::Semaphore::new(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 16)),
        })
    }

    pub fn thumb_path(&self, rec: &PhotoRec) -> PathBuf {
        self.thumbs_dir.join(format!("{:02x}", rec.id % 256)).join(format!("{}_{}.jpg", rec.id, sig_str(rec)))
    }

    /// Where the thumbnail of photo `id` at version `v` (`sig_str`, as photo lists give
    /// it) is kept, without looking the photo up: for the grid's many requests. None
    /// for anything that is not a version string.
    pub fn thumb_path_for(&self, id: i64, v: &str) -> Option<PathBuf> {
        let ok = id > 0 && !v.is_empty() && v.len() <= 16 && v.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        ok.then(|| self.thumbs_dir.join(format!("{:02x}", id % 256)).join(format!("{id}_{v}.jpg")))
    }

    fn draft_path(&self, rec: &PhotoRec) -> PathBuf {
        self.thumbs_dir.join(format!("{:02x}", rec.id % 256)).join(format!("{}_{}.draft.jpg", rec.id, sig_str(rec)))
    }

    fn preview_path(&self, rec: &PhotoRec, long: usize) -> PathBuf {
        self.previews_dir.join(format!("{:02x}", rec.id % 256)).join(format!("{}_{}_{}.jpg", rec.id, sig_str(rec), long))
    }

    pub fn cache_stats(&self) -> serde_json::Value {
        let d = self.developed.lock();
        let r = self.rendered.lock();
        serde_json::json!({
            "developed": d.len(), "developed_mb": d.bytes() / 1048576,
            "rendered": r.len(), "rendered_mb": r.bytes() / 1048576,
            "thumb_queue": self.queue.lock().0.len(),
        })
    }

    /// Someone is waiting on a thumbnail, preview or RAW development right now:
    /// background readers (the integrity monitor) pause.
    pub fn busy(&self) -> bool {
        self.interactive.load(Ordering::SeqCst) > 0 || self.gate.sem.available_permits() < self.gate.total
    }

    /// Thumbnails waiting to be built in the background.
    pub fn thumbs_queued(&self) -> usize {
        self.queue.lock().0.len()
    }

    /// Thumbnail file for a photo (generated on first request), and whether it is a
    /// draft: for a HEIC file whose embedded thumbnail is smaller than a thumbnail,
    /// that is shown at once while the full-quality one is made in the background.
    pub async fn thumb(&self, state: &AppState, loc: &Located) -> Result<(PathBuf, bool)> {
        let path = self.thumb_path(&loc.rec);
        if path.exists() {
            return Ok((path, false));
        }
        let kind = Kind::from_i32(loc.rec.kind);
        let draft = self.draft_path(&loc.rec);
        if kind == Kind::Heic && draft.exists() {
            self.queue_first(loc.rec.id);
            return Ok((draft, true));
        }
        let busy = Busy::new(&self.interactive);
        let _p = self.thumb_sem.acquire().await?;
        let src = scan::photo_path(state, &loc.folder_path, &loc.rec.name);
        let (orient, out, dout) = (loc.rec.orientation as u8, path.clone(), draft.clone());
        let res = tokio::task::spawn_blocking(move || -> Result<bool> {
            if kind == Kind::Heic {
                let (img, info) = native::heic_thumb(&src, THUMB_LONG, DRAFT_MIN)?;
                let is_draft = info.source == "draft";
                let img = jpeg::orient(jpeg::fit_long(img, THUMB_LONG)?, orient);
                write_atomic(if is_draft { &dout } else { &out }, &jpeg::encode(&img, 78, false)?)?;
                return Ok(is_draft);
            }
            make_thumb(&src, kind, orient, &out)?;
            Ok(false)
        })
        .await;
        drop(busy);
        if res?? {
            self.queue_first(loc.rec.id);
            return Ok((draft, true));
        }
        Ok((path, false))
    }

    /// Put a photo at the front of the thumbnail queue.
    fn queue_first(&self, id: i64) {
        let mut q = self.queue.lock();
        if q.1.insert(id) {
            q.0.push_front(id);
        } else if let Some(i) = q.0.iter().position(|x| *x == id) {
            q.0.remove(i);
            q.0.push_front(id);
        }
        self.queue_cv.notify_one();
    }

    /// The preview size viewers ask for most (2048 until anyone has asked).
    pub fn preferred_preview_size(&self) -> usize {
        let n = self.preview_sizes.lock();
        let best = (0..n.len()).rev().max_by_key(|&i| n[i]).unwrap_or(2);
        if n[best] == 0 { 2048 } else { PREVIEW_SIZES[best] }
    }

    /// Preview counts, as kept in the index between runs.
    pub fn preview_size_counts(&self) -> Vec<u64> {
        self.preview_sizes.lock().to_vec()
    }

    pub fn restore_preview_sizes(&self, counts: &[u64]) {
        let mut n = self.preview_sizes.lock();
        for (d, s) in n.iter_mut().zip(counts) {
            *d = *s;
        }
    }

    /// Medium preview (sensor orientation, long edge `long`), disk cached.
    pub async fn preview(&self, state: &AppState, loc: &Located, long: usize) -> Result<PathBuf> {
        if let Some(i) = PREVIEW_SIZES.iter().position(|s| *s == long) {
            let mut n = self.preview_sizes.lock();
            n[i] += 1;
            if n.iter().sum::<u64>() % 25 == 0 {
                let counts = n.to_vec();
                drop(n);
                if let Ok(c) = state.db.get() {
                    let _ = crate::db::set_meta(&c, "preview_sizes", &serde_json::to_string(&counts).unwrap_or_default());
                }
            }
        }
        let path = self.preview_path(&loc.rec, long);
        if path.exists() {
            touch(&path);
            return Ok(path);
        }
        let busy = Busy::new(&self.interactive);
        let _p = self.thumb_sem.acquire().await?;
        let src = scan::photo_path(state, &loc.folder_path, &loc.rec.name);
        let kind = Kind::from_i32(loc.rec.kind);
        let out = path.clone();
        let res = tokio::task::spawn_blocking(move || make_preview(&src, kind, long, &out)).await;
        drop(busy);
        res??;
        Ok(path)
    }

    /// The full-size view as JPEG: the largest embedded JPEG of a RAW file (or the JPEG
    /// itself) without EXIF, in sensor orientation; other formats at up to 4096 pixels,
    /// disk cached.
    pub async fn full_jpeg(&self, state: &AppState, loc: &Located) -> Result<Vec<u8>> {
        let src = scan::photo_path(state, &loc.folder_path, &loc.rec.name);
        let kind = Kind::from_i32(loc.rec.kind);
        match kind {
            Kind::Raw => {
                tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                    if let Some(b) = full_embedded_jpeg(&src) {
                        return Ok(b);
                    }
                    let img = load_display_rgb(&src, kind, 4096)?;
                    jpeg::encode(&img, 88, false)
                })
                .await?
            }
            Kind::Jpeg => Ok(tokio::task::spawn_blocking(move || -> Result<Vec<u8>> { Ok(jpeg::strip_exif(&std::fs::read(&src)?)) }).await??),
            Kind::Heic | Kind::Tiff | Kind::Png => {
                let path = self.preview(state, loc, 4096).await?;
                Ok(tokio::fs::read(path).await?)
            }
        }
    }

    /// Developed RAW data (single-flight, memory cached).
    pub async fn developed(&self, state: &AppState, loc: &Located, wb: WbMode, high: bool) -> Result<Arc<Developed>> {
        if Kind::from_i32(loc.rec.kind) != Kind::Raw {
            return Err(anyhow!("not a RAW file"));
        }
        let key = DevKey { id: loc.rec.id, sig: sig(&loc.rec), wb };
        if let Some(d) = self.developed.lock().get(&key) {
            return Ok(d);
        }
        let cell = {
            let mut inflight = self.inflight.lock();
            inflight.entry(key.clone()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None))).clone()
        };
        let mut slot = cell.lock().await;
        if let Some(r) = slot.as_ref() {
            return r.clone().map_err(|e| anyhow!(e));
        }
        if let Some(d) = self.developed.lock().get(&key) {
            return Ok(d);
        }
        let _permit = self.gate.acquire(high).await;
        let src = scan::photo_path(state, &loc.folder_path, &loc.rec.name);
        // The photo being viewed keeps its sensor data, for 1:1 tiles without decoding again.
        let keep = high;
        let res = tokio::task::spawn_blocking(move || raw::develop_path_keep(&src, wb, raw::UE_EV, keep)).await.map_err(|e| anyhow!("{e}")).and_then(|r| r);
        let res = res
            .map(|(d, b)| {
                if let Some(b) = b {
                    let size = b.bytes();
                    self.bayers.lock().put((key.id, key.sig), Arc::new(b), size);
                }
                Arc::new(d)
            })
            .map_err(|e| format!("{e:#}"));
        if let Ok(d) = &res {
            let size = d.bytes();
            self.developed.lock().put(key.clone(), d.clone(), size);
        }
        *slot = Some(res.clone());
        drop(slot);
        self.inflight.lock().remove(&key);
        res.map_err(|e| anyhow!(e))
    }

    /// The sensor data of a RAW file (cached for the photos being inspected).
    pub async fn bayer(&self, state: &AppState, loc: &Located) -> Result<Arc<raw::Bayer>> {
        let key = (loc.rec.id, sig(&loc.rec));
        if let Some(b) = self.bayers.lock().get(&key) {
            return Ok(b);
        }
        let cell = self.bayer_inflight.lock().entry(key).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None))).clone();
        let mut slot = cell.lock().await;
        if let Some(r) = slot.as_ref() {
            return r.clone().map_err(|e| anyhow!(e));
        }
        if let Some(b) = self.bayers.lock().get(&key) {
            return Ok(b);
        }
        let _permit = self.gate.acquire(true).await;
        let src = scan::photo_path(state, &loc.folder_path, &loc.rec.name);
        let res = tokio::task::spawn_blocking(move || raw::bayer_path(&src)).await.map_err(|e| anyhow!("{e}")).and_then(|r| r);
        let res = res.map(Arc::new).map_err(|e| format!("{e:#}"));
        if let Ok(b) = &res {
            self.bayers.lock().put(key, b.clone(), b.bytes());
        }
        *slot = Some(res.clone());
        drop(slot);
        self.bayer_inflight.lock().remove(&key);
        res.map_err(|e| anyhow!(e))
    }

    /// A full-resolution tile of a RAW file as JPEG bytes (see `raw::render_tile`).
    #[allow(clippy::too_many_arguments)]
    pub async fn raw_tile(&self, state: &AppState, loc: &Located, mode: ToneMode, ev: f32, wb: WbMode, tx: usize, ty: usize) -> Result<Arc<Vec<u8>>> {
        let dkey = DevKey { id: loc.rec.id, sig: sig(&loc.rec), wb };
        let tkey = TileKey { dev: dkey, mode: Some(mode), ev: (ev * 100.0).round() as i32, tx, ty };
        if let Some(b) = self.tiles.lock().get(&tkey) {
            return Ok(b);
        }
        let busy = Busy::new(&self.interactive);
        let res = async {
            let dev = self.developed(state, loc, wb, true).await?;
            raw::tile_rect(&dev, tx, ty)?;
            let bayer = self.bayer(state, loc).await?;
            let lut = self.lut(mode, RENDER_BOOST_EV + ev);
            let _p = self.tile_sem.acquire().await?;
            tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                let img = raw::catch(|| raw::render_tile(&bayer, &dev, &lut, tx, ty)).map_err(|e| anyhow!(e))?;
                jpeg::encode(&img, 90, true)
            })
            .await?
        }
        .await;
        drop(busy);
        let bytes = Arc::new(res?);
        self.tiles.lock().put(tkey, bytes.clone(), bytes.len());
        Ok(bytes)
    }

    /// A tile's demosaic input for the viewer's GPU path (`raw::cfa_tile`).
    pub async fn cfa_tile(&self, state: &AppState, loc: &Located, wb: WbMode, tx: usize, ty: usize) -> Result<Arc<Vec<u8>>> {
        let dkey = DevKey { id: loc.rec.id, sig: sig(&loc.rec), wb };
        let tkey = TileKey { dev: dkey, mode: None, ev: 0, tx, ty };
        if let Some(b) = self.tiles.lock().get(&tkey) {
            return Ok(b);
        }
        let busy = Busy::new(&self.interactive);
        let res = async {
            let dev = self.developed(state, loc, wb, true).await?;
            raw::tile_rect(&dev, tx, ty)?;
            let bayer = self.bayer(state, loc).await?;
            let _p = self.tile_sem.acquire().await?;
            tokio::task::spawn_blocking(move || raw::catch(|| raw::cfa_tile(&bayer, &dev, tx, ty)).map_err(|e| anyhow!(e))).await?
        }
        .await;
        drop(busy);
        let bytes = Arc::new(res?);
        self.tiles.lock().put(tkey, bytes.clone(), bytes.len());
        Ok(bytes)
    }

    /// The tone curve of a view (f16 bits of linear sRGB to 8-bit display values), as the
    /// RAW renders and tiles use it.
    pub fn view_lut(&self, mode: ToneMode, ev: f32) -> Arc<Vec<u8>> {
        self.lut(mode, RENDER_BOOST_EV + ev)
    }

    fn lut(&self, mode: ToneMode, ev: f32) -> Arc<Vec<u8>> {
        let k = (mode, (ev * 100.0).round() as i32);
        if let Some(l) = self.luts.lock().get(&k) {
            return l.clone();
        }
        let l = Arc::new(tone::build_lut(mode, ev));
        let mut m = self.luts.lock();
        if m.len() > 64 {
            m.clear();
        }
        m.insert(k, l.clone());
        l
    }

    /// A RAW render as JPEG bytes.
    pub async fn render(
        &self,
        state: &AppState,
        loc: &Located,
        mode: ToneMode,
        ev: f32,
        wb: WbMode,
        long: usize,
        high: bool,
    ) -> Result<Arc<Vec<u8>>> {
        let dkey = DevKey { id: loc.rec.id, sig: sig(&loc.rec), wb };
        let rkey = RenderKey { dev: dkey, mode, ev: (ev * 100.0).round() as i32, long };
        if let Some(b) = self.rendered.lock().get(&rkey) {
            return Ok(b);
        }
        let dev = self.developed(state, loc, wb, high).await?;
        let lut = self.lut(mode, RENDER_BOOST_EV + ev);
        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let img = raw::render(&dev, &lut, long);
            let img = jpeg::fit_long(img, long)?;
            jpeg::encode(&img, 86, true)
        })
        .await??;
        let bytes = Arc::new(bytes);
        let n = bytes.len();
        self.rendered.lock().put(rkey, bytes.clone(), n);
        Ok(bytes)
    }

    pub fn queue_folder_thumbs(&self, state: &AppState, folder_id: i64) {
        let Ok(conn) = state.db.get() else { return };
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM photos WHERE folder_id = ?1 AND pair_of IS NULL ORDER BY taken")
            .and_then(|mut st| st.query_map([folder_id], |r| r.get(0))?.collect())
            .unwrap_or_default();
        let mut q = self.queue.lock();
        for id in ids {
            if q.1.insert(id) {
                q.0.push_back(id);
            }
        }
        self.queue_cv.notify_one();
    }

    /// Background thumbnail builder: one thread, yields to interactive requests.
    pub fn spawn_workers(state: Arc<AppState>) {
        let st = state.clone();
        std::thread::Builder::new()
            .name("thumbs".into())
            .spawn(move || {
                // Drafts replaced, told to viewers in batches.
                let mut replaced: Vec<i64> = Vec::new();
                let mut told = Instant::now();
                loop {
                    let flush = |replaced: &mut Vec<i64>, told: &mut Instant| {
                        if !replaced.is_empty() {
                            st.emit(crate::state::Event::Thumbs { ids: std::mem::take(replaced) });
                            *told = Instant::now();
                        }
                    };
                    if told.elapsed() > Duration::from_millis(500) {
                        flush(&mut replaced, &mut told);
                    }
                    let id = {
                        let mut q = st.images.queue.lock();
                        if q.0.is_empty() && !replaced.is_empty() {
                            drop(q);
                            flush(&mut replaced, &mut told);
                            q = st.images.queue.lock();
                        }
                        while q.0.is_empty() {
                            st.images.queue_cv.wait_for(&mut q, Duration::from_secs(30));
                        }
                        let id = q.0.pop_front();
                        if let Some(id) = id {
                            q.1.remove(&id);
                        }
                        st.progress.write().thumbs_pending = q.0.len();
                        id
                    };
                    let Some(id) = id else { continue };
                    while st.images.interactive.load(Ordering::SeqCst) > 0 {
                        std::thread::sleep(Duration::from_millis(40));
                    }
                    if build_thumb(&st, id) {
                        replaced.push(id);
                    }
                }
            })
            .expect("spawn thumbs");

        if state.cfg.pregen_previews {
            let st = state.clone();
            std::thread::Builder::new().name("previews".into()).spawn(move || pregen_previews(&st)).expect("spawn previews");
        }

        // Preview cache size limit.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(600)).await;
                let dir = state.images.previews_dir.clone();
                let budget = state.images.preview_budget;
                let _ = tokio::task::spawn_blocking(move || trim_dir(&dir, budget)).await;
            }
        });
    }
}

/// Make a photo's thumbnail (queued work). True when it replaced a draft.
pub fn build_thumb(st: &AppState, id: i64) -> bool {
    let Ok(conn) = st.db.get() else { return false };
    let Ok(Some(loc)) = repo::load(&conn, id) else { return false };
    drop(conn);
    let out = st.images.thumb_path(&loc.rec);
    if out.exists() {
        return false;
    }
    let draft = st.images.draft_path(&loc.rec);
    let src = scan::photo_path(st, &loc.folder_path, &loc.rec.name);
    match raw::catch(|| make_thumb(&src, Kind::from_i32(loc.rec.kind), loc.rec.orientation as u8, &out)) {
        Ok(()) => std::fs::remove_file(&draft).is_ok(),
        Err(e) => {
            tracing::debug!("thumbnail for {} failed: {e}", src.display());
            false
        }
    }
}

/// A photo's thumbnail as pixels (upright, long edge `THUMB_LONG`), for analysis: the
/// cached thumbnail when there is one, else made in memory and not written (the
/// thumbnail builder and the grid write thumbnails; two writers of one file would race).
pub fn thumb_pixels(st: &AppState, loc: &Located) -> Result<Rgb> {
    if let Ok(data) = std::fs::read(st.images.thumb_path(&loc.rec)) {
        return jpeg::decode_scaled(&data, 0);
    }
    let src = scan::photo_path(st, &loc.folder_path, &loc.rec.name);
    let (kind, orientation) = (Kind::from_i32(loc.rec.kind), loc.rec.orientation as u8);
    raw::catch(|| -> Result<Rgb> {
        let img = load_display_rgb(&src, kind, THUMB_LONG)?;
        Ok(jpeg::orient(jpeg::fit_long(img, THUMB_LONG)?, orientation))
    })
    .map_err(|e| anyhow!(e))
}

/// Previews of recent imports, made ahead of the first look while nothing else is going
/// on (no viewer waiting, no thumbnails queued, no scan), at the size viewers ask for
/// most, and only up to 70% of the preview cache, so they never push out what was
/// actually viewed. Photos indexed before this ran (the library as it was) are left.
fn pregen_previews(st: &Arc<AppState>) {
    let since = {
        let Ok(c) = st.db.get() else { return };
        if let Some(v) = crate::db::get_meta(&c, "preview_sizes").and_then(|v| serde_json::from_str::<Vec<u64>>(&v).ok()) {
            st.images.restore_preview_sizes(&v);
        }
        match crate::db::get_meta(&c, "pregen_since").and_then(|v| v.parse::<i64>().ok()) {
            Some(v) => v,
            None => {
                let now = crate::db::now_ms();
                let _ = crate::db::set_meta(&c, "pregen_since", &now.to_string());
                now
            }
        }
    };
    let busy = || st.images.busy() || !st.images.queue.lock().0.is_empty() || st.progress.read().phase != "idle";
    loop {
        std::thread::sleep(Duration::from_secs(120));
        if !busy() {
            pregen_pass(st, since, &busy);
        }
    }
}

/// One pass of [`pregen_previews`]: returns how many previews it made.
pub fn pregen_pass(st: &AppState, since: i64, busy: &dyn Fn() -> bool) -> usize {
    let limit = st.images.preview_budget / 10 * 7;
    let mut used = dir_size(&st.images.previews_dir);
    let mut made = 0;
    if used >= limit {
        return 0;
    }
    {
        let long = st.images.preferred_preview_size();
        let from = (crate::db::now_ms() - st.cfg.pregen_days as i64 * 86_400_000).max(since + 3_600_000);
        let ids: Vec<i64> = st
            .db
            .get()
            .ok()
            .and_then(|c| {
                c.prepare(
                    "SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id
                     WHERE p.added_at >= ?1 AND p.pair_of IS NULL AND f.is_reject = 0 ORDER BY p.added_at DESC LIMIT 2000",
                )
                .and_then(|mut q| q.query_map([from], |r| r.get(0))?.collect())
                .ok()
            })
            .unwrap_or_default();
        for id in ids {
            while busy() {
                std::thread::sleep(Duration::from_millis(250));
            }
            let Ok(conn) = st.db.get() else { break };
            let Ok(Some(loc)) = repo::load(&conn, id) else { continue };
            drop(conn);
            let out = st.images.preview_path(&loc.rec, long);
            if out.exists() {
                continue;
            }
            let src = scan::photo_path(st, &loc.folder_path, &loc.rec.name);
            match raw::catch(|| make_preview(&src, Kind::from_i32(loc.rec.kind), long, &out)) {
                Ok(()) => {
                    used += std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                    made += 1;
                }
                Err(e) => tracing::debug!("preview of {} not made ahead: {e}", src.display()),
            }
            if used >= limit {
                break;
            }
        }
    }
    made
}

/// Total size of the files in a cache directory (one level of sub folders).
fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .filter_map(|sub| std::fs::read_dir(sub.path()).ok())
        .flat_map(|rd2| rd2.flatten().filter_map(|f| f.metadata().ok().filter(|m| m.is_file()).map(|m| m.len())))
        .sum()
}

/// Mark a cached file as just used (the cache trims the least recently used first);
/// at most once an hour per file.
fn touch(path: &Path) {
    let old = std::fs::metadata(path).and_then(|m| m.modified()).map(|t| t.elapsed().unwrap_or_default() > Duration::from_secs(3600)).unwrap_or(false);
    if old && let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// Write a preview of `src` (sensor orientation, long edge `long`) to `out`.
fn make_preview(src: &Path, kind: Kind, long: usize, out: &Path) -> Result<()> {
    let img = load_display_rgb(src, kind, long)?;
    let img = jpeg::fit_long(img, long)?;
    write_atomic(out, &jpeg::encode(&img, 85, false)?)
}

fn make_thumb(src: &Path, kind: Kind, orientation: u8, out: &Path) -> Result<()> {
    let img = load_display_rgb(src, kind, THUMB_LONG)?;
    let img = jpeg::fit_long(img, THUMB_LONG)?;
    let img = jpeg::orient(img, orientation);
    write_atomic(out, &jpeg::encode(&img, 78, false)?)
}

/// Delete the oldest cached previews beyond `budget` bytes (cache directory only).
fn trim_dir(dir: &Path, budget: u64) {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0u64;
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for sub in rd.flatten() {
        let Ok(rd2) = std::fs::read_dir(sub.path()) else { continue };
        for f in rd2.flatten() {
            if let Ok(md) = f.metadata() {
                if md.is_file() {
                    total += md.len();
                    files.push((md.modified().unwrap_or(std::time::UNIX_EPOCH), md.len(), f.path()));
                }
            }
        }
    }
    if total <= budget {
        return;
    }
    files.sort_by_key(|f| f.0);
    let target = budget * 8 / 10;
    for (_, len, p) in files {
        if total <= target {
            break;
        }
        if p.starts_with(dir) && std::fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}
