//! The analysis worker: sends photos to the AI engine and keeps the detections, the
//! groups and the crops up to date.
//!
//! Order: photos on screen first (the viewer asks), then new imports, then the rest of
//! the library in index order (the first scan indexes the newest years first). A photo
//! is analysed again when its file, its orientation or the models change; what was
//! confirmed carries over to the new detections. Background work pauses while you are
//! viewing photos, like the other background jobs.

use super::engine::{Engine, pet_quality};
use super::index::{cosine, quantize, to_blob};
use crate::ml::client::{Analysis, Client, MlError, decode_embedding, embedders, models_key};
use super::regions::{Rect, iou};
use super::{AUTO, CAT, DOG, FACE, SRC_DETECTED, SRC_XMP};
use crate::images::jpeg::{self, Rgb};
use crate::model::Kind;
use crate::repo::{self, Located};
use crate::state::AppState;
use anyhow::{Result, anyhow};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{OptionalExtension, params};
use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ crops

/// Small JPEG crops of faces and animals for the People view, in one cache database
/// (hundreds of thousands of tiny files would slow down every appdata backup).
pub struct Crops {
    pool: r2d2::Pool<SqliteConnectionManager>,
}

pub const CROP: usize = 160;

impl Crops {
    pub fn open(path: &Path) -> Result<Crops> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let m = SqliteConnectionManager::file(path).with_init(|c| c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=10000;"));
        let pool = r2d2::Pool::builder().max_size(4).build(m)?;
        pool.get()?.execute_batch("CREATE TABLE IF NOT EXISTS crops(id INTEGER PRIMARY KEY, data BLOB NOT NULL);")?;
        Ok(Crops { pool })
    }

    pub fn get(&self, id: i64) -> Option<Vec<u8>> {
        self.pool.get().ok()?.query_row("SELECT data FROM crops WHERE id = ?1", [id], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn put(&self, items: &[(i64, Vec<u8>)]) {
        let Ok(mut c) = self.pool.get() else { return };
        let Ok(tx) = c.transaction() else { return };
        for (id, data) in items {
            let _ = tx.execute("INSERT OR REPLACE INTO crops(id, data) VALUES(?1, ?2)", params![id, data]);
        }
        let _ = tx.commit();
    }

    pub fn delete(&self, ids: &[i64]) {
        let Ok(c) = self.pool.get() else { return };
        for id in ids {
            let _ = c.execute("DELETE FROM crops WHERE id = ?1", [id]);
        }
    }
}

/// A square crop around a box of an upright image: a face with room for hair and chin,
/// an animal as it is.
pub fn crop(img: &Rgb, r: Rect, kind: i64, size: usize) -> Result<Vec<u8>> {
    let face = kind == FACE;
    crop_around(img, r, size, if face { 1.7 } else { 1.06 }, if face { 0.08 } else { 0.0 })
}

/// A close-up of a face for culling: the face filling the square, its eyes and mouth
/// large enough to judge.
pub fn closeup(img: &Rgb, r: Rect, kind: i64, size: usize) -> Result<Vec<u8>> {
    if kind != FACE {
        return crop(img, r, kind, size);
    }
    crop_around(img, r, size, 1.22, 0.04)
}

/// A square of `factor` times the box's long side around it (raised by `up` of its
/// height), at most `size` pixels.
fn crop_around(img: &Rgb, r: Rect, size: usize, factor: f64, up: f64) -> Result<Vec<u8>> {
    let (w, h) = (img.w as f64, img.h as f64);
    let (bw, bh) = (r.w * w, r.h * h);
    let mut side = bw.max(bh) * factor;
    side = side.max(8.0).min(w.min(h));
    let cx = (r.x + r.w / 2.0) * w;
    let cy = (r.y + r.h / 2.0) * h - up * bh;
    let x0 = (cx - side / 2.0).clamp(0.0, (w - side).max(0.0)).round() as usize;
    let y0 = (cy - side / 2.0).clamp(0.0, (h - side).max(0.0)).round() as usize;
    let s = (side.round() as usize).min(img.w - x0).min(img.h - y0).max(1);
    let mut px = Vec::with_capacity(s * s * 3);
    for y in y0..y0 + s {
        let row = &img.px[(y * img.w + x0) * 3..(y * img.w + x0 + s) * 3];
        px.extend_from_slice(row);
    }
    let sub = Rgb { w: s, h: s, px };
    let out = if s > size { jpeg::resize(sub, size, size)? } else { sub };
    jpeg::encode(&out, 84, false)
}

// ------------------------------------------------------------------ the worker

pub fn spawn(state: Arc<AppState>) {
    super::xmp::spawn_writer(state.clone());
    std::thread::Builder::new()
        .name("people".into())
        .spawn(move || {
            // Give the first scan a head start.
            std::thread::sleep(Duration::from_secs(3));
            run(&state)
        })
        .expect("spawn people");
}

fn set_phase(state: &AppState, phase: &str, error: Option<String>) {
    let changed = {
        let mut s = state.people.status.write();
        let changed = s.phase != phase || s.error != error;
        s.phase = phase.to_string();
        s.error = error;
        changed
    };
    if changed {
        emit_status(state);
    }
}

pub fn emit_status(state: &AppState) {
    let s = state.people.status.read().clone();
    state.emit(crate::state::Event::PeopleStatus(s));
}

/// Wait until something happens or `d` passes.
fn wait(state: &AppState, d: Duration) {
    let mut c = state.people.ctl.lock();
    if !c.wake {
        state.people.cv.wait_for(&mut c, d);
    }
    c.wake = false;
}

fn load_engine(state: &AppState) -> Result<()> {
    if state.people.engine.lock().is_some() {
        return Ok(());
    }
    let t = Instant::now();
    let c = state.db.get()?;
    let e = Engine::load(&c)?;
    let (f, p) = e.counts();
    tracing::info!("people: {f} faces and {p} animals indexed in {:.1}s", t.elapsed().as_secs_f64());
    *state.people.engine.lock() = Some(e);
    Ok(())
}

fn run(state: &Arc<AppState>) {
    if let Err(e) = load_engine(state) {
        tracing::error!("people: loading the index failed: {e:#}");
    }
    if !state.cfg.ml.faces_on() {
        // Face and pet recognition is off: names from other apps' regions are still read
        // (resync), and every edit works on what was analysed before.
        loop {
            set_phase(state, "off", None);
            resyncs(state);
            wait(state, Duration::from_secs(60));
        }
    }
    let mut backoff = Duration::from_secs(5);
    let mut cursor = 0i64;
    let mut last_count = Instant::now() - Duration::from_secs(3600);
    let mut rate: VecDeque<Instant> = VecDeque::new();
    loop {
        resyncs(state);
        let client = match state.ml.client() {
            Ok(c) => c,
            Err(MlError::Preparing(msg)) => {
                // The models are downloading or loading: the supervisor wakes us when the
                // engine is up.
                set_phase(state, "preparing", Some(msg));
                state.people.status.write().service = None;
                wait(state, Duration::from_secs(15));
                continue;
            }
            Err(e) => {
                set_phase(state, "offline", Some(e.to_string()));
                state.people.status.write().service = None;
                wait(state, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(120));
                continue;
            }
        };
        let info = match client.info() {
            Ok(i) => i,
            Err(e) => {
                set_phase(state, "offline", Some(format!("{e:#}")));
                state.people.status.write().service = None;
                wait(state, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(120));
                continue;
            }
        };
        backoff = Duration::from_secs(5);
        if let Err(e) = check_models(state, &info) {
            tracing::warn!("people: {e:#}");
        }
        let key = models_key(&info);
        state.people.status.write().service = Some(info);
        // Work until the engine goes away.
        loop {
            resyncs(state);
            let (paused, regroup) = {
                let mut c = state.people.ctl.lock();
                (c.paused, std::mem::take(&mut c.regroup))
            };
            if regroup {
                run_regroup(state);
                continue;
            }
            if paused {
                set_phase(state, "paused", None);
                wait(state, Duration::from_secs(30));
                continue;
            }
            if last_count.elapsed() > Duration::from_secs(15) {
                update_counts(state, &key);
                last_count = Instant::now();
            }
            let (batch, priority) = next_batch(state, &key, &mut cursor);
            if batch.is_empty() {
                update_counts(state, &key);
                set_phase(state, "idle", None);
                maybe_regroup(state);
                state.people.status.write().current = None;
                emit_status(state);
                wait(state, Duration::from_secs(120));
                // The engine may have restarted: check again.
                break;
            }
            set_phase(state, "analysing", None);
            match process(state, &client, &key, batch, priority, &mut rate) {
                Ok(()) => {}
                Err(e) => {
                    set_phase(state, "offline", Some(format!("{e:#}")));
                    wait(state, backoff);
                    break;
                }
            }
            let now = Instant::now();
            while rate.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(300)) {
                rate.pop_front();
            }
            state.people.status.write().rate = if rate.len() > 1 {
                rate.len() as f64 / now.duration_since(rate[0]).as_secs_f64().max(1.0) * 60.0
            } else {
                0.0
            };
        }
    }
}

/// When the engine brings different embedding models, the stored embeddings mean
/// nothing to the new ones: drop them (boxes and names stay, and carry over when each
/// photo is analysed again).
fn check_models(state: &AppState, info: &serde_json::Value) -> Result<()> {
    let (face, pet) = embedders(info);
    let c = state.db.get()?;
    let old = crate::db::get_meta(&c, "people_embedders");
    let now = format!("{face}/{pet}");
    if old.as_deref() == Some(now.as_str()) {
        return Ok(());
    }
    if let Some(old) = old {
        let (of, op) = old.split_once('/').unwrap_or(("", ""));
        if of != face {
            let n = c.execute("UPDATE detections SET emb = NULL WHERE kind = ?1 AND emb IS NOT NULL", [FACE])?;
            tracing::warn!("people: the face recognition model changed ({of} -> {face}): {n} faces will be analysed again");
        }
        if op != pet {
            let n = c.execute("UPDATE detections SET emb = NULL WHERE kind IN (?1, ?2) AND emb IS NOT NULL", [DOG, CAT])?;
            tracing::warn!("people: the pet recognition model changed ({op} -> {pet}): {n} animals will be analysed again");
        }
        *state.people.engine.lock() = Some(Engine::load(&c)?);
    }
    crate::db::set_meta(&c, "people_embedders", &now)?;
    Ok(())
}

const NEEDS: &str = "p.pair_of IS NULL AND f.is_reject = 0
    AND (a.photo_id IS NULL OR a.size != p.size OR a.mtime != p.mtime OR a.orientation != p.orientation OR a.models != ?1
         OR (a.error IS NOT NULL AND a.at < ?2))";

/// Retry a photo that failed after this long.
const RETRY_MS: i64 = 7 * 86_400_000;

fn update_counts(state: &AppState, key: &str) {
    let Ok(c) = state.db.get() else { return };
    let retry = crate::db::now_ms() - RETRY_MS;
    let pending: i64 = c
        .query_row(
            &format!("SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN analysis a ON a.photo_id = p.id WHERE {NEEDS}"),
            params![key, retry],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let analyzed: i64 = c.query_row("SELECT count(*) FROM analysis WHERE error IS NULL", [], |r| r.get(0)).unwrap_or(0);
    let failed: i64 = c.query_row("SELECT count(*) FROM analysis WHERE error IS NOT NULL", [], |r| r.get(0)).unwrap_or(0);
    let mut s = state.people.status.write();
    s.pending = pending;
    s.analyzed = analyzed;
    s.failed = failed;
}

/// The next photos to analyse, and whether they are ones on screen.
fn next_batch(state: &AppState, key: &str, cursor: &mut i64) -> (Vec<i64>, bool) {
    let Ok(c) = state.db.get() else { return (Vec::new(), false) };
    let retry = crate::db::now_ms() - RETRY_MS;
    let needs = |id: i64| -> bool {
        c.query_row(
            &format!("SELECT 1 FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN analysis a ON a.photo_id = p.id WHERE p.id = ?3 AND {NEEDS}"),
            params![key, retry, id],
            |_| Ok(()),
        )
        .is_ok()
    };
    // On screen first.
    let wanted: Vec<i64> = {
        let mut ctl = state.people.ctl.lock();
        let mut v = Vec::new();
        while v.len() < 8 {
            let Some(id) = ctl.priority.pop_front() else { break };
            ctl.priority_set.remove(&id);
            v.push(id);
        }
        v
    };
    let wanted: Vec<i64> = wanted.into_iter().filter(|&id| needs(id)).collect();
    if !wanted.is_empty() {
        return (wanted, true);
    }
    let sweep = |from: i64| -> Vec<i64> {
        c.prepare_cached(&format!(
            "SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id LEFT JOIN analysis a ON a.photo_id = p.id
             WHERE p.id > ?3 AND {NEEDS} ORDER BY p.id LIMIT 48"
        ))
        .and_then(|mut st| st.query_map(params![key, retry, from], |r| r.get(0))?.collect())
        .unwrap_or_default()
    };
    let mut ids = sweep(*cursor);
    if ids.is_empty() && *cursor > 0 {
        *cursor = 0;
        ids = sweep(0);
    }
    if let Some(&last) = ids.last() {
        *cursor = last;
    }
    (ids, false)
}

struct Prepared {
    loc: Located,
    img: Rgb,
    jpeg: Vec<u8>,
}

fn prepare(state: &AppState, id: i64) -> Result<Option<Prepared>, String> {
    let c = state.db.get().map_err(|e| e.to_string())?;
    let Some(loc) = repo::load(&c, id).map_err(|e| e.to_string())? else { return Ok(None) };
    let pair: Option<i64> = c.query_row("SELECT pair_of FROM photos WHERE id = ?1", [id], |r| r.get(0)).map_err(|e| e.to_string())?;
    drop(c);
    if loc.folder_is_reject || pair.is_some() {
        return Ok(None);
    }
    let src = crate::scan::photo_path(state, &loc.folder_path, &loc.rec.name);
    let kind = Kind::from_i32(loc.rec.kind);
    let long = state.cfg.ml_size;
    let o = loc.rec.orientation as u8;
    let (img, data) = crate::images::raw::catch(|| -> Result<(Rgb, Vec<u8>)> {
        let img = crate::images::load_display_rgb(&src, kind, long)?;
        let img = jpeg::orient(jpeg::fit_long(img, long)?, o);
        let data = jpeg::encode(&img, 90, false)?;
        Ok((img, data))
    })?;
    Ok(Some(Prepared { loc, img, jpeg: data }))
}

/// Analyse a batch with a few photos in flight at once (decoding here and inference in
/// the engine overlap). An unreachable engine ends the batch with an error.
fn process(state: &Arc<AppState>, client: &Client, key: &str, batch: Vec<i64>, priority: bool, rate: &mut VecDeque<Instant>) -> Result<()> {
    let queue = parking_lot::Mutex::new(VecDeque::from(batch));
    let down: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);
    let stop = AtomicBool::new(false);
    let done = parking_lot::Mutex::new(Vec::<(i64, Instant)>::new());
    let workers = state.cfg.ml_concurrency.clamp(1, 16);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let Some(id) = queue.lock().pop_front() else { return };
                    // Background work waits while you look at photos.
                    if !priority {
                        while state.images.busy() && !stop.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    match one(state, client, key, id) {
                        Ok(true) => done.lock().push((id, Instant::now())),
                        Ok(false) => {}
                        Err(e) => {
                            *down.lock() = Some(e);
                            stop.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            });
        }
    });
    let done = done.into_inner();
    rate.extend(done.iter().map(|d| d.1));
    if !done.is_empty() {
        super::emit_changed(state, done.iter().map(|d| d.0).collect());
        emit_status(state);
    }
    match down.into_inner() {
        Some(e) => Err(anyhow!(e)),
        None => Ok(()),
    }
}

/// Analyse one photo. Ok(true): analysed; Ok(false): skipped or failed on its own (an
/// unreadable file); Err: the engine is unreachable.
fn one(state: &Arc<AppState>, client: &Client, key: &str, id: i64) -> Result<bool, String> {
    let prep = match prepare(state, id) {
        Ok(Some(p)) => p,
        Ok(None) => return Ok(false),
        Err(e) => {
            record_error(state, id, key, &e);
            return Ok(false);
        }
    };
    state.people.status.write().current = Some(format!("{}/{}", prep.loc.folder_path, prep.loc.rec.name));
    let a = match client.analyze(&prep.jpeg) {
        Ok(a) => a,
        Err(MlError::Unreachable(e) | MlError::Preparing(e)) => return Err(e),
        Err(MlError::Rejected(e)) => {
            record_error(state, id, key, &e);
            return Ok(false);
        }
    };
    match store(state, &prep, &a, key) {
        Ok(()) => Ok(true),
        Err(e) => {
            tracing::warn!("people: storing the analysis of {}: {e:#}", prep.loc.rec.name);
            Ok(false)
        }
    }
}

fn record_error(state: &AppState, id: i64, key: &str, err: &str) {
    let Ok(c) = state.db.get() else { return };
    tracing::debug!("people: photo {id} not analysed: {err}");
    let _ = c.execute(
        "INSERT INTO analysis(photo_id, size, mtime, orientation, models, at, error, w, h)
         SELECT id, size, mtime, orientation, ?2, ?3, ?4, 0, 0 FROM photos WHERE id = ?1
         ON CONFLICT(photo_id) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, orientation = excluded.orientation,
            models = excluded.models, at = excluded.at, error = excluded.error",
        params![id, key, crate::db::now_ms(), err.chars().take(500).collect::<String>()],
    );
}

struct NewDet {
    kind: i64,
    rect: Rect,
    score: f64,
    quality: f64,
    emb: Option<Vec<i8>>,
}

fn rect_of(b: &[f32; 4]) -> Rect {
    let x = (b[0] as f64).clamp(0.0, 1.0);
    let y = (b[1] as f64).clamp(0.0, 1.0);
    Rect { x, y, w: (b[2] as f64).clamp(0.0, 1.0 - x), h: (b[3] as f64).clamp(0.0, 1.0 - y) }
}

/// Keep an analysis: detections (what was confirmed carries over to the same faces),
/// names from the photo's metadata, matches and groups, crops.
fn store(state: &Arc<AppState>, prep: &Prepared, a: &Analysis, key: &str) -> Result<()> {
    let photo = prep.loc.rec.id;
    let mut new: Vec<NewDet> = Vec::new();
    for f in &a.faces {
        let emb = f.emb.as_deref().and_then(|e| decode_embedding(e).ok()).map(|v| quantize(&v));
        new.push(NewDet { kind: FACE, rect: rect_of(&f.bbox), score: f.score as f64, quality: f.q as f64, emb });
    }
    for x in &a.animals {
        let kind = if x.species == "cat" { CAT } else { DOG };
        let emb = x.emb.as_deref().and_then(|e| decode_embedding(e).ok()).map(|v| quantize(&v));
        // Too small to tell which pet it is, and perhaps no pet at all: left out.
        if emb.is_none() && x.score < 0.6 {
            continue;
        }
        new.push(NewDet { kind, rect: rect_of(&x.bbox), score: x.score as f64, quality: pet_quality(x.px, x.score) as f64, emb });
    }
    // Names in the photo's metadata, read before the engine is locked.
    let meta = super::xmp::fresh_people(state, prep.loc.clone());

    let mut eng_guard = state.people.engine.lock();
    let eng = eng_guard.get_or_insert_with(Engine::empty);
    let mut conn = state.db.get()?;
    let tx = conn.transaction()?;
    // The photo may have changed or gone while it was analysed.
    let now_rec: Option<(i64, i64, i32, i64)> = tx
        .query_row("SELECT size, mtime, orientation, folder_id FROM photos WHERE id = ?1", [photo], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let Some((size, mtime, orient, _)) = now_rec else { return Ok(()) };
    if size != prep.loc.rec.size || mtime != prep.loc.rec.mtime || orient != prep.loc.rec.orientation {
        return Ok(());
    }
    struct Old {
        id: i64,
        kind: i64,
        rect: Rect,
        emb: Option<Vec<i8>>,
        source: i64,
    }
    let old: Vec<Old> = {
        let mut st = tx.prepare("SELECT id, kind, x, y, w, h, emb, source FROM detections WHERE photo_id = ?1")?;
        st.query_map([photo], |r| {
            Ok(Old {
                id: r.get(0)?,
                kind: r.get(1)?,
                rect: Rect { x: r.get(2)?, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? },
                emb: r.get_ref(6)?.as_blob_or_null()?.and_then(super::index::from_blob),
                source: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    // Carry over: the same face (or animal) is where it was, or looks the same (after a
    // rotation, or with a new detector).
    let mut order: Vec<usize> = (0..new.len()).collect();
    order.sort_by(|&i, &j| new[j].quality.total_cmp(&new[i].quality));
    let mut taken: HashSet<i64> = HashSet::new();
    let mut touched: Vec<i64> = Vec::new();
    let mut fresh: Vec<i64> = Vec::new();
    let now = crate::db::now_ms();
    for i in order {
        let n = &new[i];
        let best = old
            .iter()
            .filter(|o| !taken.contains(&o.id) && (o.kind == FACE) == (n.kind == FACE))
            .map(|o| {
                let by_box = iou(o.rect, n.rect);
                let by_look = match (&o.emb, &n.emb) {
                    (Some(a), Some(b)) if o.kind == n.kind => cosine(a, b) as f64,
                    _ => 0.0,
                };
                (o, by_box.max(if by_look >= 0.8 { by_look } else { 0.0 }))
            })
            .filter(|(_, s)| *s >= 0.5)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let emb = n.emb.as_ref().map(|e| to_blob(e));
        match best {
            Some((o, _)) => {
                taken.insert(o.id);
                tx.execute(
                    "UPDATE detections SET kind = ?2, x = ?3, y = ?4, w = ?5, h = ?6, score = ?7, quality = ?8, emb = ?9, source = ?10 WHERE id = ?1",
                    params![o.id, n.kind, n.rect.x, n.rect.y, n.rect.w, n.rect.h, n.score, n.quality, emb, SRC_DETECTED],
                )?;
                touched.push(o.id);
            }
            None => {
                let id: i64 = tx.query_row(
                    "INSERT INTO detections(photo_id, kind, x, y, w, h, score, quality, emb, subject_id, state, source, created)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11, ?12) RETURNING id",
                    params![photo, n.kind, n.rect.x, n.rect.y, n.rect.w, n.rect.h, n.score, n.quality, emb, AUTO, SRC_DETECTED, now],
                    |r| r.get(0),
                )?;
                touched.push(id);
                fresh.push(id);
            }
        }
    }
    let mut gone: Vec<i64> = Vec::new();
    for o in &old {
        // Regions from the sidecar stay until the sidecar loses them.
        if !taken.contains(&o.id) && o.source != SRC_XMP {
            tx.execute("DELETE FROM detections WHERE id = ?1", [o.id])?;
            gone.push(o.id);
        }
    }
    tx.execute(
        "INSERT INTO analysis(photo_id, size, mtime, orientation, models, at, error, w, h) VALUES(?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)
         ON CONFLICT(photo_id) DO UPDATE SET size = excluded.size, mtime = excluded.mtime, orientation = excluded.orientation,
            models = excluded.models, at = excluded.at, error = NULL, w = excluded.w, h = excluded.h",
        params![photo, size, mtime, orient, key, now, a.w, a.h],
    )?;
    let mut all = touched.clone();
    all.extend(&gone);
    let res: Result<()> = (|| {
        eng.refresh(&tx, &all)?;
        super::xmp::sync(&tx, eng, &meta, false)?;
        // Faces carried over keep their person; the new and the unassigned ones are matched.
        let todo: Vec<i64> = {
            let mut st = tx.prepare("SELECT id FROM detections WHERE photo_id = ?1 AND subject_id IS NULL AND state = ?2 AND emb IS NOT NULL")?;
            st.query_map(params![photo, AUTO], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let assigned = eng.assign(&tx, &todo)?;
        if assigned.photos.len() > 1 {
            super::emit_changed(state, assigned.photos.into_iter().filter(|p| *p != photo).collect());
        }
        Ok(())
    })();
    if let Err(e) = res {
        // The in-memory index may now differ from the database: reload it.
        drop(tx);
        *eng = Engine::load(&conn)?;
        return Err(e);
    }
    tx.commit()?;
    drop(eng_guard);
    // Crops of the faces and animals worth showing.
    let mut crops: Vec<(i64, Vec<u8>)> = Vec::new();
    {
        let mut st = conn.prepare("SELECT id, kind, x, y, w, h, quality, subject_id FROM detections WHERE photo_id = ?1 AND source = ?2")?;
        let rows: Vec<(i64, i64, Rect, f64, Option<i64>)> = st
            .query_map(params![photo, SRC_DETECTED], |r| Ok((r.get(0)?, r.get(1)?, Rect { x: r.get(2)?, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? }, r.get(6)?, r.get(7)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, kind, rect, q, subject) in rows {
            if (kind != FACE || q >= 0.1 || subject.is_some())
                && let Ok(b) = crop(&prep.img, rect, kind, CROP)
            {
                crops.push((id, b));
            }
        }
    }
    state.people.crops.put(&crops);
    state.people.crops.delete(&gone);
    Ok(())
}

/// Detections deleted since the last look (with their photo or folder, or by a new
/// analysis): out of the in-memory index and the crops.
fn drain_gone(state: &AppState) {
    loop {
        let Ok(c) = state.db.get() else { return };
        let ids: Vec<i64> = c
            .prepare_cached("SELECT id FROM detections_gone LIMIT 5000")
            .and_then(|mut st| st.query_map([], |r| r.get(0))?.collect())
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        if let Some(e) = state.people.engine.lock().as_mut() {
            e.forget(&ids);
        }
        state.people.crops.delete(&ids);
        let json = serde_json::to_string(&ids).unwrap_or_default();
        if c.execute("DELETE FROM detections_gone WHERE id IN (SELECT value FROM json_each(?1))", [json]).is_err() || ids.len() < 5000 {
            return;
        }
    }
}

/// Person and pet keywords someone took off photos (in the keyword editor): the faces
/// they named are not them after all. They become unknown again (never suggested as
/// that name again) and their regions lose the name.
fn unnames(state: &Arc<AppState>) {
    let items: Vec<(i64, Vec<String>)> = std::mem::take(&mut state.people.ctl.lock().unname).into_iter().collect();
    if items.is_empty() {
        return;
    }
    let mut photos = Vec::new();
    for (photo, keywords) in items {
        let mut g = state.people.engine.lock();
        let eng = g.get_or_insert_with(Engine::empty);
        let Ok(mut c) = state.db.get() else { return };
        let r = (|| -> Result<bool> {
            let tx = c.transaction()?;
            let mut freed = Vec::new();
            for k in &keywords {
                let Some((kind, name)) = super::parse_keyword(k) else { continue };
                let Some(s) = tx
                    .query_row("SELECT id FROM subjects WHERE kind = ?1 AND name = ?2 COLLATE NOCASE", params![kind, name], |r| r.get::<_, i64>(0))
                    .optional()?
                else {
                    continue;
                };
                let ids: Vec<i64> = tx
                    .prepare("SELECT id FROM detections WHERE photo_id = ?1 AND subject_id = ?2")?
                    .query_map(params![photo, s], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for d in ids {
                    tx.execute("UPDATE detections SET subject_id = NULL, state = ?2, sim = NULL WHERE id = ?1", params![d, AUTO])?;
                    tx.execute("INSERT OR IGNORE INTO detection_rejects(detection_id, subject_id) VALUES(?1, ?2)", params![d, s])?;
                    eng.add_reject(d, s);
                    freed.push(d);
                }
            }
            eng.refresh(&tx, &freed)?;
            tx.commit()?;
            Ok(!freed.is_empty())
        })();
        match r {
            Ok(true) => photos.push(photo),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!("people: taking names off photo {photo}: {e:#}");
                // The in-memory index may have moved on: back to what the database says.
                if let Ok(c) = state.db.get()
                    && let Ok(e) = Engine::load(&c)
                {
                    *eng = e;
                }
            }
        }
    }
    if !photos.is_empty() {
        state.people.write(photos.iter().map(|&p| (p, Vec::new())));
        super::emit_changed(state, photos);
    }
}

/// Names from sidecars that changed (or were restored by an undo), without the AI
/// engine; names taken off by hand; and detections deleted elsewhere.
fn resyncs(state: &Arc<AppState>) {
    drain_gone(state);
    unnames(state);
    loop {
        let (ids, exact): (Vec<i64>, bool) = {
            let mut c = state.people.ctl.lock();
            if !c.exact.is_empty() {
                let n = c.exact.len().min(200);
                (c.exact.drain(..n).collect(), true)
            } else if !c.resync.is_empty() {
                let n = c.resync.len().min(200);
                (c.resync.drain(..n).collect(), false)
            } else {
                return;
            }
        };
        let mut changed = Vec::new();
        for id in ids {
            let Ok(c) = state.db.get() else { return };
            // Only analysed photos: the others are read when they are analysed.
            let analysed = c.query_row("SELECT 1 FROM analysis WHERE photo_id = ?1 AND error IS NULL", [id], |_| Ok(())).is_ok();
            let regions: bool = c.query_row("SELECT 1 FROM detections WHERE photo_id = ?1 LIMIT 1", [id], |_| Ok(())).is_ok();
            if !analysed && !regions {
                continue;
            }
            let Ok(Some(loc)) = repo::load(&c, id) else { continue };
            drop(c);
            let loc = super::xmp::fresh_people(state, loc);
            let mut g = state.people.engine.lock();
            let eng = g.get_or_insert_with(Engine::empty);
            let Ok(mut c) = state.db.get() else { return };
            let r = (|| -> Result<bool> {
                let tx = c.transaction()?;
                let ch = super::xmp::sync(&tx, eng, &loc, exact)?;
                tx.commit()?;
                Ok(ch)
            })();
            match r {
                Ok(true) => changed.push(id),
                Ok(false) => {}
                Err(e) => tracing::warn!("people: reading the names of {}: {e:#}", loc.rec.name),
            }
        }
        if !changed.is_empty() {
            super::emit_changed(state, changed);
        }
    }
}

fn run_regroup(state: &Arc<AppState>) {
    set_phase(state, "grouping", None);
    let t = Instant::now();
    let st = state.clone();
    let progress = move |f: f64| {
        let mut s = st.people.status.write();
        if s.regroup.is_none_or(|p| f - p > 0.02 || f >= 1.0) {
            s.regroup = Some(f);
            drop(s);
            emit_status(&st);
        }
    };
    let r = (|| -> Result<()> {
        let mut c = state.db.get()?;
        let out = super::engine::regroup(&state.people.engine, &mut c, &progress)?;
        let n = state.people.engine.lock().as_ref().map(|e| e.faces.len() + e.pets.len()).unwrap_or(0);
        crate::db::set_meta(&c, "people_regrouped", &format!("{n}/{}", crate::db::now_ms()))?;
        tracing::info!("people: regrouped {n} faces and animals into {} new groups in {:.0}s", out.new_groups, t.elapsed().as_secs_f64());
        Ok(())
    })();
    if let Err(e) = r {
        tracing::warn!("people: regrouping failed: {e:#}");
        // The in-memory index may have moved on: back to what the database says.
        if let Ok(c) = state.db.get()
            && let Ok(e) = Engine::load(&c)
        {
            *state.people.engine.lock() = Some(e);
        }
    }
    state.people.status.write().regroup = None;
    super::emit_changed(state, Vec::new());
    set_phase(state, "idle", None);
}

/// After a large first analysis, group once more from scratch (best faces first): the
/// groups come out cleaner than in the order photos happened to be analysed. At most
/// once a day, when the library has 30% more faces than at the last grouping.
fn maybe_regroup(state: &Arc<AppState>) {
    let faces = state.people.engine.lock().as_ref().map(|e| e.faces.len() + e.pets.len()).unwrap_or(0);
    if faces < 50 {
        return;
    }
    let Ok(c) = state.db.get() else { return };
    let (last_n, last_t) = crate::db::get_meta(&c, "people_regrouped")
        .and_then(|v| v.split_once('/').and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<i64>().ok()?))))
        .unwrap_or((0, 0));
    if (faces as f64) < last_n as f64 * 1.3 || crate::db::now_ms() - last_t < 86_400_000 {
        return;
    }
    run_regroup(state);
}
