//! Image service: thumbnails, previews, embedded JPEG pass-through and RAW renders,
//! with disk and memory caches. Everything here only *reads* the library; caches
//! live in the cache directory.

#[cfg(any(feature = "heic", test))]
pub mod color;
pub mod jpeg;
pub mod raw;
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
use std::time::Duration;
use tone::ToneMode;

pub const THUMB_LONG: usize = 512;
/// Exposure added to every RAW render so mid-tones land close to the camera JPEG
/// (calibrated on Canon CR2/CR3 and Ricoh GR III DNG files; DNG BaselineExposure is
/// reported but not applied, since it is tuned for Adobe's own tone curve).
pub const RENDER_BOOST_EV: f32 = 0.9;

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

/// Lets the photo on screen jump ahead of prefetching.
pub struct Gate {
    sem: tokio::sync::Semaphore,
    high: AtomicUsize,
    notify: tokio::sync::Notify,
}

impl Gate {
    fn new(permits: usize) -> Gate {
        Gate { sem: tokio::sync::Semaphore::new(permits.max(1)), high: AtomicUsize::new(0), notify: tokio::sync::Notify::new() }
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

type Inflight = Arc<tokio::sync::Mutex<Option<std::result::Result<Arc<Developed>, String>>>>;

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
}

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
        Kind::Heic => Ok(native::heic_decode(path, min_long)?.0),
    }
}

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
        })
    }

    pub fn thumb_path(&self, rec: &PhotoRec) -> PathBuf {
        self.thumbs_dir.join(format!("{:02x}", rec.id % 256)).join(format!("{}_{}.jpg", rec.id, sig_str(rec)))
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

    /// Thumbnail file for a photo (generated on first request).
    pub async fn thumb(&self, state: &AppState, loc: &Located) -> Result<PathBuf> {
        let path = self.thumb_path(&loc.rec);
        if path.exists() {
            return Ok(path);
        }
        self.interactive.fetch_add(1, Ordering::SeqCst);
        let _p = self.thumb_sem.acquire().await?;
        let src = scan::photo_path(state, &loc.folder_path, &loc.rec.name);
        let (kind, orient, out) = (Kind::from_i32(loc.rec.kind), loc.rec.orientation as u8, path.clone());
        let res = tokio::task::spawn_blocking(move || make_thumb(&src, kind, orient, &out)).await;
        self.interactive.fetch_sub(1, Ordering::SeqCst);
        res??;
        Ok(path)
    }

    /// Medium preview (sensor orientation, long edge `long`), disk cached.
    pub async fn preview(&self, state: &AppState, loc: &Located, long: usize) -> Result<PathBuf> {
        let path = self.preview_path(&loc.rec, long);
        if path.exists() {
            return Ok(path);
        }
        self.interactive.fetch_add(1, Ordering::SeqCst);
        let _p = self.thumb_sem.acquire().await?;
        let src = scan::photo_path(state, &loc.folder_path, &loc.rec.name);
        let kind = Kind::from_i32(loc.rec.kind);
        let out = path.clone();
        let res = tokio::task::spawn_blocking(move || -> Result<()> {
            let img = load_display_rgb(&src, kind, long)?;
            let img = jpeg::fit_long(img, long)?;
            write_atomic(&out, &jpeg::encode(&img, 85, false)?)
        })
        .await;
        self.interactive.fetch_sub(1, Ordering::SeqCst);
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
        let res = tokio::task::spawn_blocking(move || -> Result<Developed> { raw::develop_path(&src, wb, 8.0) })
        .await
        .map_err(|e| anyhow!("{e}"))
        .and_then(|r| r);
        let res = res.map(Arc::new).map_err(|e| format!("{e:#}"));
        if let Ok(d) = &res {
            let size = d.bytes();
            self.developed.lock().put(key.clone(), d.clone(), size);
        }
        *slot = Some(res.clone());
        drop(slot);
        self.inflight.lock().remove(&key);
        res.map_err(|e| anyhow!(e))
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
                loop {
                    let id = {
                        let mut q = st.images.queue.lock();
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
                    let Ok(conn) = st.db.get() else { continue };
                    let Ok(Some(loc)) = repo::load(&conn, id) else { continue };
                    drop(conn);
                    let out = st.images.thumb_path(&loc.rec);
                    if out.exists() {
                        continue;
                    }
                    let src = scan::photo_path(&st, &loc.folder_path, &loc.rec.name);
                    if let Err(e) = raw::catch(|| make_thumb(&src, Kind::from_i32(loc.rec.kind), loc.rec.orientation as u8, &out)) {
                        tracing::debug!("thumbnail for {} failed: {e}", src.display());
                    }
                }
            })
            .expect("spawn thumbs");

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
