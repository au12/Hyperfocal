//! HTTP API of the People & pets view.
//!
//! Every change to who is who happens here (under the engine's lock, in one
//! transaction), and the photos whose sidecars it touches are queued for the writer.

use super::engine::{Engine, subject_by_name};
use super::regions::Rect;
use super::{AUTO, CAT, CONFIRMED, DOG, FACE, IGNORED, PERSON, PET, SRC_DETECTED, keyword};
use crate::api::{ApiError, ApiResult, St, bad};
use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/people/status", get(status))
        .route("/people", get(list))
        .route("/people/names", get(names))
        .route("/people/merge", post(merge))
        .route("/people/assign", post(assign))
        .route("/people/confirm", post(confirm))
        .route("/people/reject", post(reject))
        .route("/people/ignore", post(ignore))
        .route("/people/analyze", post(analyze))
        .route("/people/control", post(control))
        .route("/people/{id}", get(subject).post(update).delete(remove))
        .route("/people/{id}/detections", get(subject_detections))
        .route("/photo/{id}/people", get(photo_people))
        .route("/detection/{id}/crop", get(crop))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> ApiResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| anyhow::anyhow!("{e}"))?.map_err(ApiError::from)
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

/// A version for a crop URL: changes when the photo or the box does.
pub(crate) fn crop_v(r: &Rect, photo_v: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for v in [r.x, r.y, r.w, r.h] {
        h ^= (v * 1e5).round() as i64 as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{photo_v}.{:x}", h & 0xffff_ffff)
}

fn photo_sig(c: &Connection, photo: i64) -> String {
    c.query_row("SELECT size, mtime, orientation FROM photos WHERE id = ?1", [photo], |r| {
        Ok(crate::images::sig_str(&crate::model::PhotoRec { id: photo, size: r.get(0)?, mtime: r.get(1)?, orientation: r.get(2)?, ..Default::default() }))
    })
    .unwrap_or_default()
}

fn kind_label(subject_kind: i64, species: i64) -> &'static str {
    if subject_kind == PERSON {
        "person"
    } else if species == CAT {
        "cat"
    } else {
        "dog"
    }
}

/// Detections as the UI sees them.
const DET_COLS: &str = "d.id, d.photo_id, d.kind, d.x, d.y, d.w, d.h, d.quality, d.state, d.sim, d.source, d.subject_id, d.score";

fn det_json(r: &rusqlite::Row, sig: &str) -> rusqlite::Result<Value> {
    let rect = Rect { x: r.get(3)?, y: r.get(4)?, w: r.get(5)?, h: r.get(6)? };
    Ok(json!({
        "id": r.get::<_, i64>(0)?, "photo": r.get::<_, i64>(1)?, "kind": super::kind_name(r.get(2)?),
        "box": [rect.x, rect.y, rect.w, rect.h], "q": r.get::<_, f64>(7)?, "state": r.get::<_, i64>(8)?,
        "sim": r.get::<_, Option<f64>>(9)?, "src": r.get::<_, i64>(10)?, "subject": r.get::<_, Option<i64>>(11)?,
        "score": r.get::<_, f64>(12)?, "v": crop_v(&rect, sig),
    }))
}

// ------------------------------------------------------------------ reading

async fn status(State(state): St) -> ApiResult<Json<Value>> {
    let st = state.clone();
    blocking(move || {
        let c = st.db.get()?;
        let count = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
        let s = st.people.status.read().clone();
        Ok(json!({
            "status": s,
            "people": count("SELECT count(*) FROM subjects s WHERE kind = 1 AND name IS NOT NULL AND EXISTS (SELECT 1 FROM detections d WHERE d.subject_id = s.id)"),
            "pets": count("SELECT count(*) FROM subjects s WHERE kind = 2 AND name IS NOT NULL AND EXISTS (SELECT 1 FROM detections d WHERE d.subject_id = s.id)"),
            "groups": count("SELECT count(*) FROM subjects s WHERE name IS NULL AND hidden = 0 AND EXISTS (SELECT 1 FROM detections d WHERE d.subject_id = s.id)"),
            "faces": count("SELECT count(*) FROM detections WHERE kind = 1 AND state != 3"),
            "animals": count("SELECT count(*) FROM detections WHERE kind IN (2, 3) AND state != 3"),
            "review": count("SELECT count(*) FROM detections d JOIN subjects s ON s.id = d.subject_id WHERE d.state = 0 AND s.name IS NOT NULL AND s.hidden = 0"),
            "paused": st.people.ctl.lock().paused,
            "xmp": st.cfg.people_xmp && !st.is_readonly(),
        }))
    })
    .await
    .map(Json)
}

#[derive(Deserialize, Default)]
struct ListQuery {
    /// person | pet
    kind: Option<String>,
    /// 1: hidden ones only
    hidden: Option<u8>,
    /// named | unnamed (default both)
    named: Option<String>,
    /// Least photos (default 1).
    min: Option<i64>,
    /// Only subjects in this folder (and below it).
    folder: Option<i64>,
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// People, pets and unnamed groups: named first, then by number of photos.
///
/// Counted from the index of detections by subject, less the detections in reject
/// folders (few, found from those folders): a join of every detection with its photo
/// and folder would take a second on a 300,000-photo library. A person is in a photo
/// at most once, so detections count photos.
async fn list(State(state): St, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    blocking(move || {
        let c = st.db.get()?;
        let kind = if q.kind.as_deref() == Some("pet") { PET } else { PERSON };
        let mut w = vec!["s.kind = ?".to_string(), "s.hidden = ?".to_string()];
        let mut a: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(kind), Box::new(q.hidden.unwrap_or(0) as i64)];
        match q.named.as_deref() {
            Some("named") => w.push("s.name IS NOT NULL".into()),
            Some("unnamed") => w.push("s.name IS NULL".into()),
            _ => {}
        }
        if let Some(text) = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            w.push("s.name LIKE ? ESCAPE '\\'".into());
            a.push(Box::new(format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))));
        }
        // Detections that count: not ignored, not in a reject folder, and in the folder
        // asked for (and below it).
        let (scope, scope_args): (String, Vec<String>) = match q.folder {
            Some(fid) => {
                let path: String = c.query_row("SELECT path FROM folders WHERE id = ?1", [fid], |r| r.get(0)).map_err(|_| anyhow::anyhow!("no such folder"))?;
                if path.is_empty() {
                    (String::new(), Vec::new())
                } else {
                    ("AND (f.path = ? OR (f.path >= ? AND f.path < ?))".into(), vec![path.clone(), format!("{path}/"), format!("{path}0")])
                }
            }
            None => (String::new(), Vec::new()),
        };
        let mut counts: HashMap<i64, (i64, i64, i64)> = HashMap::new();
        if scope.is_empty() {
            let mut st = c.prepare_cached(
                "SELECT subject_id, count(*), sum(state = 1), sum(state = 0) FROM detections WHERE subject_id IS NOT NULL AND state != 3 GROUP BY subject_id",
            )?;
            for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))? {
                let (s, n) = r?;
                counts.insert(s, n);
            }
            let mut st = c.prepare_cached(
                "SELECT d.subject_id, count(*), sum(d.state = 1), sum(d.state = 0) FROM folders f JOIN photos p ON p.folder_id = f.id
                 JOIN detections d ON d.photo_id = p.id WHERE f.is_reject = 1 AND d.subject_id IS NOT NULL AND d.state != 3 GROUP BY d.subject_id",
            )?;
            for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))))? {
                let (s, (n, cf, au)) = r?;
                if let Some(e) = counts.get_mut(&s) {
                    e.0 -= n;
                    e.1 -= cf;
                    e.2 -= au;
                }
            }
        } else {
            let mut st = c.prepare(&format!(
                "SELECT d.subject_id, count(*), sum(d.state = 1), sum(d.state = 0) FROM detections d JOIN photos p ON p.id = d.photo_id
                 JOIN folders f ON f.id = p.folder_id WHERE d.subject_id IS NOT NULL AND d.state != 3 AND f.is_reject = 0 {scope} GROUP BY d.subject_id"
            ))?;
            for r in st.query_map(params_from_iter(scope_args.iter()), |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))? {
                let (s, n) = r?;
                counts.insert(s, n);
            }
        }
        let min = q.min.unwrap_or(1).max(1);
        let limit = q.limit.unwrap_or(300).clamp(1, 5000) as usize;
        let offset = q.offset.unwrap_or(0).max(0) as usize;
        let mut stmt = c.prepare(&format!("SELECT s.id, s.kind, s.species, s.name, s.hidden FROM subjects s WHERE {}", w.join(" AND ")))?;
        #[allow(clippy::type_complexity)]
        let mut rows: Vec<(i64, i64, i64, Option<String>, i64, (i64, i64, i64))> = stmt
            .query_map(params_from_iter(a.iter().map(|b| b.as_ref())), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .filter_map(|r| r.ok())
            .filter_map(|(id, k, sp, name, hidden)| counts.get(&id).filter(|n| n.0 > 0).map(|n| (id, k, sp, name, hidden, *n)))
            .collect();
        let total = rows.len();
        rows.retain(|r| r.5.0 >= min);
        rows.sort_by(|x, y| {
            x.3.is_none()
                .cmp(&y.3.is_none())
                .then(y.5.0.cmp(&x.5.0))
                .then_with(|| x.3.as_deref().unwrap_or("").to_lowercase().cmp(&y.3.as_deref().unwrap_or("").to_lowercase()))
                .then(x.0.cmp(&y.0))
        });
        let mut out = Vec::new();
        for (id, k, species, name, hidden, (n, confirmed, auto)) in rows.into_iter().skip(offset).take(limit) {
            out.push(json!({
                "id": id, "kind": kind_label(k, species), "name": name, "hidden": hidden != 0,
                "photos": n, "confirmed": confirmed, "review": if name.is_some() { auto } else { 0 },
                "cover": cover(&c, id)?,
            }));
        }
        Ok(json!({ "subjects": out, "total": total }))
    })
    .await
    .map(Json)
}

/// The detection that best shows a subject: a confirmed one if any, the best view.
/// (Walks the subject's detections best first, `detections_best`: no sort.)
fn cover(c: &Connection, subject: i64) -> anyhow::Result<Value> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {DET_COLS} FROM detections d JOIN photos p ON p.id = d.photo_id JOIN folders f ON f.id = p.folder_id
         WHERE d.subject_id = ?1 AND d.state = ?2 AND d.source = ?3 AND f.is_reject = 0
         ORDER BY d.quality DESC LIMIT 1"
    ))?;
    for state in [CONFIRMED, AUTO] {
        let row = st
            .query_row(params![subject, state, SRC_DETECTED], |r| {
                let photo: i64 = r.get(1)?;
                det_json(r, &photo_sig(c, photo))
            })
            .optional()?;
        if let Some(v) = row {
            return Ok(v);
        }
    }
    Ok(Value::Null)
}

async fn subject(State(state): St, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = blocking(move || {
        let c = st.db.get()?;
        let row: Option<(i64, i64, Option<String>, i64)> =
            c.query_row("SELECT kind, species, name, hidden FROM subjects WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
        let Some((k, species, name, hidden)) = row else { return Ok(None) };
        let (photos, confirmed, auto): (i64, i64, i64) = c.query_row(
            "SELECT count(DISTINCT d.photo_id), COALESCE(sum(d.state = 1), 0), COALESCE(sum(d.state = 0), 0)
             FROM detections d JOIN photos p ON p.id = d.photo_id JOIN folders f ON f.id = p.folder_id
             WHERE d.subject_id = ?1 AND d.state != 3 AND f.is_reject = 0",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(Some(json!({
            "id": id, "kind": kind_label(k, species), "name": name, "hidden": hidden != 0,
            "photos": photos, "confirmed": confirmed, "review": auto, "cover": cover(&c, id)?,
            "keyword": name.as_deref().map(|n| keyword(k, n)),
        })))
    })
    .await?;
    v.map(Json).ok_or_else(not_found)
}

#[derive(Deserialize)]
struct DetQuery {
    /// auto | confirmed | all
    state: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// A subject's faces (or animals): suggestions least sure first, so the doubtful ones
/// are seen first; confirmed ones newest first.
async fn subject_detections(State(state): St, Path(id): Path<i64>, Query(q): Query<DetQuery>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    blocking(move || {
        let c = st.db.get()?;
        let (filter, order) = match q.state.as_deref() {
            Some("auto") => ("AND d.state = 0", "d.sim IS NULL DESC, d.sim ASC, d.id"),
            Some("confirmed") => ("AND d.state = 1", "COALESCE(p.taken, p.mtime / 1000) DESC, d.id"),
            _ => ("", "d.state ASC, COALESCE(p.taken, p.mtime / 1000) DESC, d.id"),
        };
        let limit = q.limit.unwrap_or(200).clamp(1, 2000);
        let offset = q.offset.unwrap_or(0).max(0);
        let mut stmt = c.prepare(&format!(
            "SELECT {DET_COLS}, p.size, p.mtime, p.orientation, p.name, p.taken FROM detections d JOIN photos p ON p.id = d.photo_id
             JOIN folders f ON f.id = p.folder_id
             WHERE d.subject_id = ?1 AND d.state != 3 AND f.is_reject = 0 {filter} ORDER BY {order} LIMIT {limit} OFFSET {offset}"
        ))?;
        let rows: Vec<Value> = stmt
            .query_map([id], |r| {
                let photo: i64 = r.get(1)?;
                let sig = crate::images::sig_str(&crate::model::PhotoRec { id: photo, size: r.get(13)?, mtime: r.get(14)?, orientation: r.get(15)?, ..Default::default() });
                let mut v = det_json(r, &sig)?;
                v["pv"] = json!(sig);
                v["file"] = json!(r.get::<_, String>(16)?);
                v["taken"] = json!(r.get::<_, Option<i64>>(17)?);
                Ok(v)
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(json!(rows))
    })
    .await
    .map(Json)
}

/// The faces and animals of a photo, with who they are (or might be).
async fn photo_people(State(state): St, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = blocking(move || {
        let c = st.db.get()?;
        let sig = photo_sig(&c, id);
        let analysis: Option<(i64, Option<String>)> = c.query_row("SELECT at, error FROM analysis WHERE photo_id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let mut stmt = c.prepare(&format!(
            "SELECT {DET_COLS}, s.name, s.kind, s.species, s.hidden FROM detections d LEFT JOIN subjects s ON s.id = d.subject_id
             WHERE d.photo_id = ?1 AND d.state != 3 ORDER BY d.x"
        ))?;
        let dets: Vec<Value> = stmt
            .query_map([id], |r| {
                let mut v = det_json(r, &sig)?;
                if let Some(sid) = r.get::<_, Option<i64>>(11)? {
                    v["who"] = json!({
                        "id": sid, "name": r.get::<_, Option<String>>(13)?,
                        "kind": kind_label(r.get(14)?, r.get(15)?), "hidden": r.get::<_, i64>(16)? != 0,
                    });
                }
                Ok(v)
            })?
            .collect::<rusqlite::Result<_>>()?;
        let ignored: i64 = c.query_row("SELECT count(*) FROM detections WHERE photo_id = ?1 AND state = 3", [id], |r| r.get(0))?;
        Ok(json!({
            "analyzed": analysis.as_ref().is_some_and(|a| a.1.is_none()), "error": analysis.and_then(|a| a.1),
            "detections": dets, "ignored": ignored,
        }))
    })
    .await?;
    // Not analysed yet: next in line.
    if !v["analyzed"].as_bool().unwrap_or(false) && v["error"].is_null() && state.cfg.ml.faces_on() {
        state.people.prioritize(&[id]);
    }
    Ok(Json(v))
}

#[derive(Deserialize)]
struct NamesQuery {
    q: Option<String>,
    /// face | dog | cat: names that can take this kind of detection.
    kind: Option<String>,
}

/// Names for autocompletion, most used first.
async fn names(State(state): St, Query(q): Query<NamesQuery>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    blocking(move || {
        let c = st.db.get()?;
        let kind = match q.kind.as_deref() {
            Some("dog" | "cat" | "pet") => PET,
            _ => PERSON,
        };
        let pat = q.q.unwrap_or_default().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let mut stmt = c.prepare(
            "SELECT s.id, s.name, s.kind, s.species, (SELECT count(DISTINCT photo_id) FROM detections WHERE subject_id = s.id AND state = 1) n
             FROM subjects s WHERE s.kind = ?1 AND s.name IS NOT NULL AND (s.name LIKE ?2 || '%' ESCAPE '\\' OR s.name LIKE '% ' || ?2 || '%' ESCAPE '\\')
               AND EXISTS (SELECT 1 FROM detections d WHERE d.subject_id = s.id)
             ORDER BY n DESC, s.name COLLATE NOCASE LIMIT 20",
        )?;
        let rows: Vec<Value> = stmt
            .query_map(params![kind, pat], |r| {
                Ok(json!({"id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "kind": kind_label(r.get(2)?, r.get(3)?), "photos": r.get::<_, i64>(4)?}))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(json!(rows))
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct CropQuery {
    /// 2: a close-up twice the size, the face filling it, from an image twice as large
    /// (culling; not kept).
    s: Option<u8>,
}

async fn crop(State(state): St, Path(id): Path<i64>, Query(q): Query<CropQuery>) -> ApiResult<Response> {
    let st = state.clone();
    let big = q.s == Some(2);
    let bytes = blocking(move || -> anyhow::Result<Option<Vec<u8>>> {
        if !big && let Some(b) = st.people.crops.get(id) {
            return Ok(Some(b));
        }
        // Not kept (a poor face, or the cache was cleared): made from the photo.
        let c = st.db.get()?;
        let row: Option<(i64, i64, Rect)> = c
            .query_row("SELECT photo_id, kind, x, y, w, h FROM detections WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, Rect { x: r.get(2)?, y: r.get(3)?, w: r.get(4)?, h: r.get(5)? }))
            })
            .optional()?;
        let Some((photo, kind, rect)) = row else { return Ok(None) };
        let Some(loc) = crate::repo::load(&c, photo)? else { return Ok(None) };
        drop(c);
        let src = crate::scan::photo_path(&st, &loc.folder_path, &loc.rec.name);
        let k = crate::model::Kind::from_i32(loc.rec.kind);
        // The size it was analysed at: the same source image, so the box fits (boxes are
        // relative, so a larger image of it fits too).
        let scale = if big { 2 } else { 1 };
        let long = st.cfg.ml_size * scale;
        let o = loc.rec.orientation as u8;
        let img = crate::images::raw::catch(|| -> anyhow::Result<crate::images::jpeg::Rgb> {
            let img = crate::images::load_display_rgb(&src, k, long)?;
            Ok(crate::images::jpeg::orient(crate::images::jpeg::fit_long(img, long)?, o))
        })
        .map_err(|e| anyhow::anyhow!(e))?;
        let b = if big { super::worker::closeup(&img, rect, kind, super::worker::CROP * scale)? } else { super::worker::crop(&img, rect, kind, super::worker::CROP)? };
        if !big {
            st.people.crops.put(&[(id, b.clone())]);
        }
        Ok(Some(b))
    })
    .await?
    .ok_or_else(not_found)?;
    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    Ok(resp)
}

// ------------------------------------------------------------------ changing

/// Run a change under the engine's lock in one transaction; queue the sidecar writes it
/// asks for and tell the viewers.
fn change<T>(
    state: &Arc<AppState>,
    f: impl FnOnce(&rusqlite::Transaction, &mut Engine, &mut Writes) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut g = state.people.engine.lock();
    let eng = g.get_or_insert_with(Engine::empty);
    let mut conn = state.db.get()?;
    let tx = conn.transaction()?;
    let mut w = Writes::default();
    let r = match f(&tx, eng, &mut w) {
        Ok(r) => r,
        Err(e) => {
            drop(tx);
            // The in-memory index may have moved on: back to what the database says.
            *eng = Engine::load(&conn)?;
            return Err(e);
        }
    };
    tx.commit()?;
    drop(g);
    let photos: Vec<i64> = w.photos.keys().copied().chain(w.seen.iter().copied()).collect::<HashSet<_>>().into_iter().collect();
    if !w.photos.is_empty() {
        state.people.write(w.photos.into_iter().map(|(p, d)| (p, d.into_iter().collect())));
    }
    super::emit_changed(state, photos);
    Ok(r)
}

/// Photos whose sidecars a change touches, with keywords that may have to go, and
/// photos that only changed in the index.
#[derive(Default)]
struct Writes {
    photos: HashMap<i64, HashSet<String>>,
    seen: HashSet<i64>,
}

impl Writes {
    fn photo(&mut self, p: i64) {
        self.photos.entry(p).or_default();
    }
    fn drop_kw(&mut self, p: i64, kw: String) {
        self.photos.entry(p).or_default().insert(kw);
    }
}

struct DetRow {
    id: i64,
    photo: i64,
    kind: i64,
    subject: Option<i64>,
    state: i64,
}

fn load_dets(c: &Connection, ids: &[i64]) -> anyhow::Result<Vec<DetRow>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(500) {
        let sql = format!("SELECT id, photo_id, kind, subject_id, state FROM detections WHERE id IN ({})", chunk.iter().map(|_| "?").collect::<Vec<_>>().join(","));
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(chunk.iter()), |r| Ok(DetRow { id: r.get(0)?, photo: r.get(1)?, kind: r.get(2)?, subject: r.get(3)?, state: r.get(4)? }))?;
        for r in rows {
            out.push(r?);
        }
    }
    Ok(out)
}

/// The keyword a subject has in the sidecars (None: no name).
fn subject_keyword(c: &Connection, s: i64) -> anyhow::Result<Option<String>> {
    Ok(c.query_row("SELECT kind, name FROM subjects WHERE id = ?1", [s], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))
        .optional()?
        .and_then(|(k, n)| n.map(|n| keyword(k, &n))))
}

fn clean_name(n: &str) -> Result<String, ApiError> {
    let n = n.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() || n.chars().count() > 80 {
        return Err(bad("a name of 1 to 80 characters"));
    }
    if n.contains('|') {
        return Err(bad("a name cannot contain |"));
    }
    Ok(n)
}

fn ids_ok(ids: &[i64]) -> ApiResult<()> {
    if ids.is_empty() || ids.len() > 50_000 {
        return Err(bad("between 1 and 50000 detections"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct AssignBody {
    ids: Vec<i64>,
    subject: Option<i64>,
    name: Option<String>,
}

/// "This is …": these detections belong to a subject (by id, or by name: an existing
/// person or pet, else a new one). Confirmed, and written to the sidecars.
async fn assign(State(state): St, Json(b): Json<AssignBody>) -> ApiResult<Json<Value>> {
    ids_ok(&b.ids)?;
    let name = b.name.as_deref().map(clean_name).transpose()?;
    if b.subject.is_none() && name.is_none() {
        return Err(bad("a subject or a name"));
    }
    let st = state.clone();
    let ids = b.ids.clone();
    let subject = b.subject;
    blocking(move || {
        change(&st, |tx, eng, w| {
            let dets = load_dets(tx, &ids)?;
            if dets.is_empty() {
                anyhow::bail!("no such detection");
            }
            let faces = dets.iter().any(|d| d.kind == FACE);
            if faces && dets.iter().any(|d| d.kind != FACE) {
                anyhow::bail!("faces and animals cannot be one subject");
            }
            let target = match (subject, &name) {
                (Some(s), _) => s,
                (None, Some(n)) => {
                    // A pet named for the first time takes the species most of its animals have.
                    let kind = if faces { FACE } else if dets.iter().filter(|d| d.kind == CAT).count() * 2 > dets.len() { CAT } else { DOG };
                    subject_by_name(tx, n, kind)?
                }
                _ => unreachable!(),
            };
            let (tkind, tspecies): (i64, i64) = tx
                .query_row("SELECT kind, species FROM subjects WHERE id = ?1", [target], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("no such person or pet"))?;
            if (tkind == PERSON) != faces {
                anyhow::bail!("{} cannot be given to a {}", if faces { "a face" } else { "an animal" }, if tkind == PERSON { "person" } else { "pet" });
            }
            let mut touched = Vec::new();
            let mut released = Vec::new();
            for d in &dets {
                // A name we wrote goes (a keyword someone typed for an unconfirmed
                // suggestion stays).
                if d.state == CONFIRMED
                    && let Some(old) = d.subject.filter(|s| *s != target)
                    && let Some(kw) = subject_keyword(tx, old)?
                {
                    w.drop_kw(d.photo, kw);
                }
                // A pet's species follows the pet (the detector can mistake one for the other).
                let kind = if tkind == PET { tspecies } else { FACE };
                tx.execute("UPDATE detections SET subject_id = ?2, state = ?3, sim = NULL, kind = ?4 WHERE id = ?1", params![d.id, target, CONFIRMED, kind])?;
                tx.execute("DELETE FROM detection_rejects WHERE detection_id = ?1 AND subject_id = ?2", params![d.id, target])?;
                // Once per photo: another detection here with this subject lets go.
                let others: Vec<i64> = tx
                    .prepare("SELECT id FROM detections WHERE photo_id = ?1 AND subject_id = ?2 AND id != ?3")?
                    .query_map(params![d.photo, target, d.id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for o in others {
                    if !ids.contains(&o) {
                        tx.execute("UPDATE detections SET subject_id = NULL, state = ?2, sim = NULL WHERE id = ?1", params![o, AUTO])?;
                        touched.push(o);
                        released.push(o);
                    }
                }
                touched.push(d.id);
                w.photo(d.photo);
            }
            tx.execute("UPDATE subjects SET updated = ?2 WHERE id = ?1", params![target, crate::db::now_ms()])?;
            eng.refresh(tx, &touched)?;
            // Faces nobody could place may now match this subject; a face that let go
            // may match someone else.
            let r = eng.propagate(tx, &ids)?;
            w.seen.extend(r.photos);
            let r = eng.assign(tx, &released)?;
            w.seen.extend(r.photos);
            Ok(json!({ "subject": target }))
        })
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct ConfirmBody {
    ids: Option<Vec<i64>>,
    /// Every suggestion of this subject.
    subject: Option<i64>,
}

/// Accept suggestions: they become confirmed and go to the sidecars.
async fn confirm(State(state): St, Json(b): Json<ConfirmBody>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    blocking(move || {
        change(&st, |tx, eng, w| {
            let ids: Vec<i64> = match (b.ids, b.subject) {
                (Some(ids), _) => ids,
                (None, Some(s)) => tx
                    .prepare("SELECT id FROM detections WHERE subject_id = ?1 AND state = ?2")?
                    .query_map(params![s, AUTO], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?,
                _ => anyhow::bail!("detections or a subject"),
            };
            let dets = load_dets(tx, &ids)?;
            let mut n = 0;
            for d in dets.iter().filter(|d| d.subject.is_some() && d.state == AUTO) {
                tx.execute("UPDATE detections SET state = ?2 WHERE id = ?1", params![d.id, CONFIRMED])?;
                w.photo(d.photo);
                n += 1;
            }
            eng.refresh(tx, &ids)?;
            let r = eng.propagate(tx, &ids)?;
            w.seen.extend(r.photos);
            Ok(json!({ "confirmed": n }))
        })
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct IdsBody {
    ids: Vec<i64>,
}

/// "Not this person (or pet)": the detection lets go of its subject, which is never
/// suggested for it again, and is matched once more.
async fn reject(State(state): St, Json(b): Json<IdsBody>) -> ApiResult<Json<Value>> {
    ids_ok(&b.ids)?;
    let st = state.clone();
    blocking(move || {
        change(&st, |tx, eng, w| {
            let dets = load_dets(tx, &b.ids)?;
            let mut freed = Vec::new();
            for d in &dets {
                let Some(s) = d.subject else { continue };
                tx.execute("INSERT OR IGNORE INTO detection_rejects(detection_id, subject_id) VALUES(?1, ?2)", params![d.id, s])?;
                tx.execute("UPDATE detections SET subject_id = NULL, state = ?2, sim = NULL WHERE id = ?1", params![d.id, AUTO])?;
                eng.add_reject(d.id, s);
                if d.state == CONFIRMED
                    && let Some(kw) = subject_keyword(tx, s)?
                {
                    w.drop_kw(d.photo, kw);
                }
                w.seen.insert(d.photo);
                freed.push(d.id);
            }
            eng.refresh(tx, &freed)?;
            let r = eng.assign(tx, &freed)?;
            w.seen.extend(r.photos);
            Ok(json!({ "rejected": freed.len() }))
        })
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct IgnoreBody {
    ids: Vec<i64>,
    /// false: take back an earlier "ignore".
    #[serde(default = "yes")]
    ignored: bool,
}

fn yes() -> bool {
    true
}

/// "Not a face" (or not of interest): left out of everything, sidecars included.
async fn ignore(State(state): St, Json(b): Json<IgnoreBody>) -> ApiResult<Json<Value>> {
    ids_ok(&b.ids)?;
    let st = state.clone();
    blocking(move || {
        change(&st, |tx, eng, w| {
            let dets = load_dets(tx, &b.ids)?;
            let mut ids = Vec::new();
            for d in &dets {
                if b.ignored {
                    if d.state == CONFIRMED
                        && let Some(s) = d.subject
                        && let Some(kw) = subject_keyword(tx, s)?
                    {
                        w.drop_kw(d.photo, kw);
                    }
                    tx.execute("UPDATE detections SET subject_id = NULL, state = ?2, sim = NULL WHERE id = ?1", params![d.id, IGNORED])?;
                } else if d.state == IGNORED {
                    tx.execute("UPDATE detections SET state = ?2 WHERE id = ?1", params![d.id, AUTO])?;
                }
                // Its region (ours, or one another app wrote) leaves the sidecar.
                w.photo(d.photo);
                ids.push(d.id);
            }
            eng.refresh(tx, &ids)?;
            if !b.ignored {
                eng.assign(tx, &ids)?;
            }
            Ok(json!({ "changed": ids.len() }))
        })
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct UpdateBody {
    /// A new name; "" or null takes the name away.
    #[serde(default, deserialize_with = "double")]
    name: Option<Option<String>>,
    hidden: Option<bool>,
}

fn double<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

/// Name, rename or un-name a subject; hide or show it. Naming an unnamed group
/// confirms its faces (you saw them when you named it). A name another subject already
/// has is refused with 409 and that subject's id, so the UI can offer to merge.
async fn update(State(state): St, Path(id): Path<i64>, Json(b): Json<UpdateBody>) -> ApiResult<Json<Value>> {
    let new_name = match &b.name {
        Some(Some(n)) if !n.trim().is_empty() => Some(Some(clean_name(n)?)),
        Some(_) => Some(None),
        None => None,
    };
    let st = state.clone();
    let res = blocking(move || {
        change(&st, |tx, eng, w| {
            let (kind, old): (i64, Option<String>) = tx
                .query_row("SELECT kind, name FROM subjects WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("no such person or pet"))?;
            if let Some(name) = &new_name {
                if let Some(n) = name
                    && let Some(other) = tx
                        .query_row("SELECT id FROM subjects WHERE kind = ?1 AND name = ?2 COLLATE NOCASE AND id != ?3", params![kind, n, id], |r| r.get::<_, i64>(0))
                        .optional()?
                {
                    return Ok(Err(other));
                }
                tx.execute("UPDATE subjects SET name = ?2, updated = ?3 WHERE id = ?1", params![id, name, crate::db::now_ms()])?;
                if old.is_none() && name.is_some() {
                    tx.execute("UPDATE detections SET state = ?2 WHERE subject_id = ?1 AND state = ?3", params![id, CONFIRMED, AUTO])?;
                }
                let rows: Vec<(i64, i64)> = tx
                    .prepare("SELECT id, photo_id FROM detections WHERE subject_id = ?1 AND state = ?2")?
                    .query_map(params![id, CONFIRMED], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                for (_, p) in &rows {
                    match &old {
                        Some(o) if name.as_deref() != Some(o.as_str()) => w.drop_kw(*p, keyword(kind, o)),
                        _ => w.photo(*p),
                    }
                }
                let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
                eng.refresh(tx, &ids)?;
                if old.is_none() && name.is_some() {
                    let r = eng.propagate(tx, &ids)?;
                    w.seen.extend(r.photos);
                }
            }
            if let Some(h) = b.hidden {
                tx.execute("UPDATE subjects SET hidden = ?2, updated = ?3 WHERE id = ?1", params![id, h as i64, crate::db::now_ms()])?;
                // A hidden group keeps its faces (and keeps taking new look-alikes) when
                // the groups are formed again.
                if h {
                    let ids: Vec<i64> = tx
                        .prepare("SELECT id FROM detections WHERE subject_id = ?1 AND state = ?2")?
                        .query_map(params![id, AUTO], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    let named: bool = tx.query_row("SELECT name IS NOT NULL FROM subjects WHERE id = ?1", [id], |r| r.get(0))?;
                    if !named {
                        tx.execute("UPDATE detections SET state = ?2 WHERE subject_id = ?1 AND state = ?3", params![id, CONFIRMED, AUTO])?;
                        eng.refresh(tx, &ids)?;
                    }
                }
            }
            Ok(Ok(()))
        })
    })
    .await?;
    match res {
        Ok(()) => Ok(Json(json!({ "ok": true }))),
        Err(other) => Err(ApiError(StatusCode::CONFLICT, format!("merge:{other}"))),
    }
}

/// Take a subject away: its faces become unknown again (and its name leaves the
/// sidecars).
async fn remove(State(state): St, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    blocking(move || {
        change(&st, |tx, eng, w| {
            let kw = subject_keyword(tx, id)?;
            let rows: Vec<(i64, i64, i64)> = tx
                .prepare("SELECT id, photo_id, state FROM detections WHERE subject_id = ?1")?
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (_, p, s) in &rows {
                if *s == CONFIRMED
                    && let Some(k) = &kw
                {
                    w.drop_kw(*p, k.clone());
                }
                w.seen.insert(*p);
            }
            tx.execute("UPDATE detections SET subject_id = NULL, state = ?2, sim = NULL WHERE subject_id = ?1 AND state != ?3", params![id, AUTO, IGNORED])?;
            tx.execute("DELETE FROM subjects WHERE id = ?1", [id])?;
            eng.relabel_subject(id, None);
            let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
            eng.refresh(tx, &ids)?;
            Ok(json!({ "freed": ids.len() }))
        })
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
struct MergeBody {
    from: Vec<i64>,
    into: i64,
}

/// One person (or pet) in several groups: everything moves into `into`. A named
/// subject takes in the others' faces as confirmed; `into` takes the name of the one
/// named subject among them if it has none.
async fn merge(State(state): St, Json(b): Json<MergeBody>) -> ApiResult<Json<Value>> {
    let from: Vec<i64> = b.from.iter().copied().filter(|f| *f != b.into).collect();
    if from.is_empty() || from.len() > 1000 {
        return Err(bad("between 1 and 1000 subjects to merge"));
    }
    let st = state.clone();
    let into = b.into;
    blocking(move || {
        change(&st, |tx, eng, w| {
            let (kind, into_name): (i64, Option<String>) = tx
                .query_row("SELECT kind, name FROM subjects WHERE id = ?1", [into], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("no such person or pet"))?;
            let mut names: Vec<String> = Vec::new();
            for &f in &from {
                let (k, n): (i64, Option<String>) = tx
                    .query_row("SELECT kind, name FROM subjects WHERE id = ?1", [f], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?
                    .ok_or_else(|| anyhow::anyhow!("no such person or pet"))?;
                if k != kind {
                    anyhow::bail!("people and pets cannot be merged");
                }
                if let Some(n) = n {
                    names.push(n);
                }
            }
            let mut name = into_name.clone();
            if name.is_none() && names.len() == 1 {
                name = Some(names[0].clone());
                tx.execute("UPDATE subjects SET name = NULL WHERE id IN (SELECT value FROM json_each(?1))", [serde_json::to_string(&from)?])?;
                tx.execute("UPDATE subjects SET name = ?2 WHERE id = ?1", params![into, name])?;
            }
            let mut moved = Vec::new();
            for &f in &from {
                let rows: Vec<(i64, i64, i64)> = tx
                    .prepare("SELECT id, photo_id, state FROM detections WHERE subject_id = ?1")?
                    .query_map([f], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                let fkw = subject_keyword(tx, f)?;
                for (d, p, s) in rows {
                    // A face said not to be `into` lets go instead.
                    let said_not: bool = tx
                        .query_row("SELECT 1 FROM detection_rejects WHERE detection_id = ?1 AND subject_id = ?2", params![d, into], |_| Ok(()))
                        .optional()?
                        .is_some();
                    if said_not {
                        tx.execute("UPDATE detections SET subject_id = NULL, state = ?2, sim = NULL WHERE id = ?1", params![d, AUTO])?;
                        moved.push(d);
                        if s == CONFIRMED
                            && let Some(k) = &fkw
                        {
                            w.drop_kw(p, k.clone());
                        }
                        w.seen.insert(p);
                        continue;
                    }
                    // Once per photo: when both were in it, the confirmed or better view stays.
                    let clash: Option<(i64, i64)> = tx
                        .query_row("SELECT id, state FROM detections WHERE photo_id = ?1 AND subject_id = ?2", params![p, into], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()?;
                    if let Some((_, ostate)) = clash
                        && ostate >= s
                    {
                        tx.execute("UPDATE detections SET subject_id = NULL, state = ?2, sim = NULL WHERE id = ?1", params![d, AUTO])?;
                        moved.push(d);
                    } else {
                        if let Some((other, _)) = clash {
                            tx.execute("UPDATE detections SET subject_id = NULL, state = ?2, sim = NULL WHERE id = ?1", params![other, AUTO])?;
                            moved.push(other);
                        }
                        let state = if name.is_some() { CONFIRMED } else { s };
                        tx.execute("UPDATE detections SET subject_id = ?2, state = ?3 WHERE id = ?1", params![d, into, state])?;
                        moved.push(d);
                    }
                    if let Some(k) = &fkw {
                        w.drop_kw(p, k.clone());
                    } else {
                        w.photo(p);
                    }
                }
                tx.execute("UPDATE OR IGNORE detection_rejects SET subject_id = ?2 WHERE subject_id = ?1", params![f, into])?;
                tx.execute("DELETE FROM subjects WHERE id = ?1", [f])?;
                eng.move_rejects(f, into);
            }
            if name != into_name {
                // `into` took its name from one of the others: its own photos get it too.
                let all: Vec<(i64, i64)> = tx
                    .prepare("SELECT id, photo_id FROM detections WHERE subject_id = ?1 AND state = ?2")?
                    .query_map(params![into, CONFIRMED], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                for (_, p) in &all {
                    w.photo(*p);
                }
            }
            tx.execute("UPDATE subjects SET updated = ?2 WHERE id = ?1", params![into, crate::db::now_ms()])?;
            eng.refresh(tx, &moved)?;
            Ok(json!({ "subject": into, "moved": moved.len() }))
        })
    })
    .await
    .map(Json)
}

/// Analyse these photos next (the viewer asks for the photos on screen).
async fn analyze(State(state): St, Json(b): Json<IdsBody>) -> ApiResult<Json<Value>> {
    if b.ids.len() > 2000 {
        return Err(bad("at most 2000 photos"));
    }
    state.people.prioritize(&b.ids);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ControlBody {
    pause: Option<bool>,
    regroup: Option<bool>,
    /// Analyse a folder (and the folders below it) again.
    reanalyze: Option<i64>,
}

async fn control(State(state): St, Json(b): Json<ControlBody>) -> ApiResult<Json<Value>> {
    if let Some(p) = b.pause {
        state.people.ctl.lock().paused = p;
    }
    if b.regroup == Some(true) {
        state.people.ctl.lock().regroup = true;
    }
    if let Some(fid) = b.reanalyze {
        let st = state.clone();
        blocking(move || {
            let c = st.db.get()?;
            let path: String = c.query_row("SELECT path FROM folders WHERE id = ?1", [fid], |r| r.get(0))?;
            let n = if path.is_empty() {
                c.execute("DELETE FROM analysis", [])?
            } else {
                c.execute(
                    "DELETE FROM analysis WHERE photo_id IN (SELECT p.id FROM photos p JOIN folders f ON f.id = p.folder_id WHERE f.path = ?1 OR (f.path >= ?2 AND f.path < ?3))",
                    params![path, format!("{path}/"), format!("{path}0")],
                )?
            };
            Ok(n)
        })
        .await?;
    }
    state.people.wake();
    Ok(Json(json!({ "ok": true, "paused": state.people.ctl.lock().paused })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
        let req = req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Naming, merging, rejecting and the photo filter, through the real router.
    #[tokio::test]
    async fn name_merge_reject_filter() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("photos")).unwrap();
        let mut cfg = crate::config::Config::for_tests(tmp.path());
        cfg.readonly = true;
        let state = crate::build_state(cfg).await.unwrap();
        let (g1, g2, faces) = {
            let c = state.db.get().unwrap();
            c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(1, NULL, '', 'Library', 1)", []).unwrap();
            for i in 1..=4 {
                let rec = crate::model::PhotoRec { folder_id: 1, name: format!("{i}.CR3"), stem: format!("{i}"), ext: "cr3".into(), kind: 1, ..Default::default() };
                crate::repo::upsert(&c, &rec).unwrap();
            }
            crate::repo::refresh_folder(&c, 1).unwrap();
            let g = |name: Option<&str>| -> i64 {
                c.query_row("INSERT INTO subjects(kind, species, name, hidden, created, updated) VALUES(1, 1, ?1, 0, 0, 0) RETURNING id", [name], |r| r.get(0)).unwrap()
            };
            let (g1, g2) = (g(None), g(None));
            let mut faces = Vec::new();
            for (photo, s) in [(1, g1), (2, g1), (3, g2), (4, g2)] {
                faces.push(
                    c.query_row(
                        "INSERT INTO detections(photo_id, kind, x, y, w, h, score, quality, subject_id, state, created) VALUES(?1, 1, 0.1, 0.1, 0.2, 0.2, 0.9, 0.9, ?2, 0, 0) RETURNING id",
                        params![photo, s],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap(),
                );
            }
            (g1, g2, faces)
        };
        let app = crate::api::router(state.clone());
        let (st, list) = call(&app, "GET", "/api/people", None).await;
        assert_eq!(st, StatusCode::OK, "{list}");
        assert_eq!(list["subjects"].as_array().unwrap().len(), 2);
        // Naming a group confirms its faces.
        let (st, _) = call(&app, "POST", &format!("/api/people/{g1}"), Some(json!({"name": "  Ann   Lee "}))).await;
        assert_eq!(st, StatusCode::OK);
        let (_, s) = call(&app, "GET", &format!("/api/people/{g1}"), None).await;
        assert_eq!(s["name"], "Ann Lee");
        assert_eq!(s["confirmed"], 2);
        assert_eq!(s["keyword"], "People|Ann Lee");
        // The same name for the other group: refused, with the subject to merge into.
        let (st, e) = call(&app, "POST", &format!("/api/people/{g2}"), Some(json!({"name": "ann lee"}))).await;
        assert_eq!(st, StatusCode::CONFLICT);
        assert_eq!(e["error"], format!("merge:{g1}"));
        let (st, m) = call(&app, "POST", "/api/people/merge", Some(json!({"from": [g2], "into": g1}))).await;
        assert_eq!(st, StatusCode::OK, "{m}");
        let (_, s) = call(&app, "GET", &format!("/api/people/{g1}"), None).await;
        assert_eq!(s["photos"], 4);
        assert_eq!(s["confirmed"], 4);
        // The photo filter.
        let (_, n) = call(&app, "GET", &format!("/api/photos?folder=1&rec=1&count=1&subj={g1}"), None).await;
        assert_eq!(n["count"], 4);
        // Not Ann: the face lets go and is never suggested for her again.
        let (st, _) = call(&app, "POST", "/api/people/reject", Some(json!({"ids": [faces[3]]}))).await;
        assert_eq!(st, StatusCode::OK);
        let (_, n) = call(&app, "GET", &format!("/api/photos?folder=1&rec=1&count=1&subj={g1}"), None).await;
        assert_eq!(n["count"], 3);
        let rej: i64 = state.db.get().unwrap().query_row("SELECT count(*) FROM detection_rejects WHERE subject_id = ?1", [g1], |r| r.get(0)).unwrap();
        assert_eq!(rej, 1);
        // Assign by a new name; the photo's people.
        let (st, a) = call(&app, "POST", "/api/people/assign", Some(json!({"ids": [faces[3]], "name": "Bo"}))).await;
        assert_eq!(st, StatusCode::OK, "{a}");
        let (_, p) = call(&app, "GET", "/api/photo/4/people", None).await;
        assert_eq!(p["detections"][0]["who"]["name"], "Bo");
        assert_eq!(p["detections"][0]["state"], 1);
        let (_, names) = call(&app, "GET", "/api/people/names?q=b", None).await;
        assert_eq!(names[0]["name"], "Bo");
        // Ignore, then delete a person.
        let (st, _) = call(&app, "POST", "/api/people/ignore", Some(json!({"ids": [faces[0]]}))).await;
        assert_eq!(st, StatusCode::OK);
        let (_, s) = call(&app, "GET", &format!("/api/people/{g1}"), None).await;
        assert_eq!(s["photos"], 2);
        let (st, _) = call(&app, "DELETE", &format!("/api/people/{g1}"), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(call(&app, "GET", &format!("/api/people/{g1}"), None).await.0, StatusCode::NOT_FOUND);
    }
}
