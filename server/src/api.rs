//! HTTP API.

use crate::images::{self, raw::WbMode, tone::ToneMode};
use crate::meta::lcolor;
use crate::model::Kind;
use crate::ops::{self, Patch};
use crate::repo::{self, Located};
use crate::scan;
use crate::state::{AppState, Event, ScanMsg};
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use rusqlite::{Connection, ToSql, params_from_iter};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;

type St = State<Arc<AppState>>;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

impl From<r2d2::Error> for ApiError {
    fn from(e: r2d2::Error) -> Self {
        ApiError(StatusCode::SERVICE_UNAVAILABLE, e.to_string())
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

type ApiResult<T> = Result<T, ApiError>;

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/events", get(events))
        .route("/tree", get(tree))
        .route("/photos", get(list_photos))
        .route("/facets", get(facets))
        .route("/keywords", get(keywords))
        .route("/geo/clusters", get(geo_clusters))
        .route("/photo/{id}", get(photo_detail))
        .route("/photo/{id}/thumb", get(thumb))
        .route("/photo/{id}/preview", get(preview))
        .route("/photo/{id}/jpeg", get(full_jpeg))
        .route("/photo/{id}/raw", get(raw_render))
        .route("/photo/{id}/rawmask", get(raw_mask))
        .route("/photo/{id}/rawinfo", get(raw_info))
        .route("/meta", post(write_meta))
        .route("/reject", post(reject))
        .route("/restore", post(restore))
        .route("/undo", post(undo))
        .route("/journal", get(journal))
        .route("/rescan", post(rescan))
        .route("/settings", get(get_settings).post(set_settings))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state);
    Router::new().nest("/api", api)
}

// ---------------------------------------------------------------- auth

fn session_token(state: &AppState) -> String {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    let mut m = <Hmac<sha2::Sha256> as Mac>::new_from_slice(&state.session_key).expect("hmac");
    m.update(b"hyperfocal-session-v1");
    m.update(state.cfg.password.as_deref().unwrap_or("").as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(m.finalize().into_bytes())
}

fn has_session(state: &AppState, headers: &HeaderMap) -> bool {
    let want = format!("hf_session={}", session_token(state));
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|c| c.trim() == want)
}

async fn auth(State(state): St, req: Request, next: Next) -> Response {
    if state.cfg.password.is_none() {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if path.ends_with("/login") || path.ends_with("/status") || has_session(&state, req.headers()) {
        return next.run(req).await;
    }
    ApiError(StatusCode::UNAUTHORIZED, "login required".into()).into_response()
}

#[derive(Deserialize)]
struct LoginBody {
    password: String,
}

async fn login(State(state): St, Json(b): Json<LoginBody>) -> ApiResult<Response> {
    let Some(pw) = &state.cfg.password else { return Ok(Json(json!({"ok": true})).into_response()) };
    // Constant-time comparison.
    let ok = pw.len() == b.password.len() && pw.bytes().zip(b.password.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
    if !ok {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        return Err(ApiError(StatusCode::UNAUTHORIZED, "wrong password".into()));
    }
    let cookie = format!("hf_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000", session_token(&state));
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({"ok": true}))).into_response())
}

async fn logout() -> Response {
    ([(header::SET_COOKIE, "hf_session=; Path=/; Max-Age=0")], Json(json!({"ok": true}))).into_response()
}

// ---------------------------------------------------------------- status & events

async fn status(State(state): St, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let counts = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        let photos: i64 = c.query_row("SELECT count(*) FROM photos WHERE pair_of IS NULL", [], |r| r.get(0))?;
        let folders: i64 = c.query_row("SELECT count(*) FROM folders", [], |r| r.get(0))?;
        let geo: i64 = c.query_row("SELECT count(*) FROM photos WHERE lat IS NOT NULL AND pair_of IS NULL", [], |r| r.get(0))?;
        Ok(json!({"photos": photos, "folders": folders, "geotagged": geo}))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "readonly": state.is_readonly(),
        "reject_dir": state.cfg.reject_dir,
        "progress": *state.progress.read(),
        "counts": counts,
        "cache": state.images.cache_stats(),
        "auth": state.cfg.password.is_some(),
        "logged_in": state.cfg.password.is_none() || has_session(&state, &headers),
    })))
}

async fn events(State(state): St) -> impl IntoResponse {
    let rx = state.events.subscribe();
    let first = futures_util::stream::once(async { Event::Progress(Default::default()) }).filter(|_| async { false });
    let stream = first.chain(BroadcastStream::new(rx).filter_map(|e| async move { e.ok() })).map(|e| {
        Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(serde_json::to_string(&e).unwrap_or_default()))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------- library

async fn tree(State(state): St) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        let mut s = c.prepare("SELECT id, parent_id, name, photo_count, is_reject, scanned_at > 0 FROM folders ORDER BY path")?;
        let rows: Vec<Value> = s
            .query_map([], |r| {
                Ok(json!([r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, bool>(5)?]))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(json!({ "folders": rows }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

#[derive(Deserialize, Default, Clone)]
pub struct ListQuery {
    folder: Option<i64>,
    rec: Option<u8>,
    rej: Option<u8>,
    rmin: Option<i32>,
    rmax: Option<i32>,
    lc: Option<String>,
    pick: Option<i32>,
    kind: Option<String>,
    gps: Option<u8>,
    from: Option<i64>,
    to: Option<i64>,
    cam: Option<String>,
    lens: Option<String>,
    kw: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    desc: Option<u8>,
    limit: Option<usize>,
    west: Option<f64>,
    south: Option<f64>,
    east: Option<f64>,
    north: Option<f64>,
    zoom: Option<u32>,
}

fn build_where(c: &Connection, q: &ListQuery) -> anyhow::Result<(String, Vec<Box<dyn ToSql>>)> {
    let mut w: Vec<String> = vec!["p.pair_of IS NULL".into()];
    let mut a: Vec<Box<dyn ToSql>> = Vec::new();
    let mut folder_is_reject = false;
    if let Some(fid) = q.folder {
        let (path, is_rej): (String, i64) = c.query_row("SELECT path, is_reject FROM folders WHERE id = ?1", [fid], |r| Ok((r.get(0)?, r.get(1)?)))?;
        folder_is_reject = is_rej != 0;
        if q.rec.unwrap_or(0) == 1 {
            if !path.is_empty() {
                w.push("(f.path = ? OR substr(f.path, 1, length(?) + 1) = ? || '/')".into());
                a.push(Box::new(path.clone()));
                a.push(Box::new(path.clone()));
                a.push(Box::new(path));
            }
        } else {
            w.push("p.folder_id = ?".into());
            a.push(Box::new(fid));
        }
    }
    match q.rej.unwrap_or(0) {
        _ if folder_is_reject && q.rec.unwrap_or(0) == 0 => {}
        1 => {}
        2 => w.push("f.is_reject = 1".into()),
        _ => w.push("f.is_reject = 0".into()),
    }
    if let Some(v) = q.rmin {
        w.push("p.rating >= ?".into());
        a.push(Box::new(v));
    }
    if let Some(v) = q.rmax {
        w.push("p.rating <= ?".into());
        a.push(Box::new(v));
    }
    if let Some(lc) = q.lc.as_deref().filter(|s| !s.is_empty()) {
        let mut parts = Vec::new();
        for c in lc.split(',') {
            if c == "none" {
                parts.push("p.lcolor IS NULL".to_string());
            } else {
                parts.push("p.lcolor = ?".to_string());
                a.push(Box::new(c.to_string()));
            }
        }
        w.push(format!("({})", parts.join(" OR ")));
    }
    if let Some(v) = q.pick {
        w.push("p.pick = ?".into());
        a.push(Box::new(v));
    }
    if let Some(k) = q.kind.as_deref().filter(|s| !s.is_empty()) {
        let ks: Vec<i64> = k.split(',').filter_map(|x| x.parse().ok()).collect();
        if !ks.is_empty() {
            w.push(format!("p.kind IN ({})", ks.iter().map(|_| "?").collect::<Vec<_>>().join(",")));
            for x in ks {
                a.push(Box::new(x));
            }
        }
    }
    match q.gps {
        Some(1) => w.push("p.lat IS NOT NULL".into()),
        Some(0) => w.push("p.lat IS NULL".into()),
        _ => {}
    }
    if let Some(v) = q.from {
        w.push("p.taken >= ?".into());
        a.push(Box::new(v));
    }
    if let Some(v) = q.to {
        w.push("p.taken < ?".into());
        a.push(Box::new(v));
    }
    if let Some(v) = q.cam.as_deref().filter(|s| !s.is_empty()) {
        w.push("p.model = ?".into());
        a.push(Box::new(v.to_string()));
    }
    if let Some(v) = q.lens.as_deref().filter(|s| !s.is_empty()) {
        w.push("p.lens = ?".into());
        a.push(Box::new(v.to_string()));
    }
    if let Some(v) = q.kw.as_deref().filter(|s| !s.is_empty()) {
        w.push("p.id IN (SELECT photo_id FROM photo_keywords WHERE kw = ?)".into());
        a.push(Box::new(v.to_string()));
    }
    if let Some(fq) = q.q.as_deref().and_then(crate::db::fts_query) {
        w.push("p.id IN (SELECT rowid FROM photos_fts WHERE photos_fts MATCH ?)".into());
        a.push(Box::new(fq));
    }
    if let (Some(we), Some(s), Some(e), Some(n)) = (q.west, q.south, q.east, q.north) {
        w.push("p.lat BETWEEN ? AND ? AND p.lon BETWEEN ? AND ?".into());
        a.push(Box::new(s));
        a.push(Box::new(n));
        a.push(Box::new(we.max(-180.0)));
        a.push(Box::new(e.min(180.0)));
    }
    Ok((w.join(" AND "), a))
}

fn order_by(q: &ListQuery) -> String {
    let desc = q.desc.unwrap_or(0) == 1;
    let d = if desc { "DESC" } else { "ASC" };
    match q.sort.as_deref() {
        Some("name") => format!("p.name COLLATE NOCASE {d}"),
        Some("rating") => format!("p.rating {}, COALESCE(p.taken, p.mtime / 1000) ASC, p.name", if desc { "ASC" } else { "DESC" }),
        Some("modified") => format!("p.mtime {d}, p.name"),
        Some("size") => format!("p.size {d}, p.name"),
        _ => format!("COALESCE(p.taken, p.mtime / 1000) {d}, p.name COLLATE NOCASE {d}"),
    }
}

async fn list_photos(State(state): St, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    // First visit to a folder: index it right away; later visits re-check in the background.
    if let Some(fid) = q.folder {
        let st = state.clone();
        let scanned: i64 = {
            let c = state.db.get()?;
            c.query_row("SELECT scanned_at FROM folders WHERE id = ?1", [fid], |r| r.get(0)).map_err(|_| not_found())?
        };
        if scanned == 0 {
            let _ = tokio::task::spawn_blocking(move || scan::scan_folder(&st, fid, false)).await;
        } else if crate::db::now_ms() - scanned > 5_000 {
            state.request_scan(ScanMsg::Folder(fid));
        }
    }
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        let (wh, args) = build_where(&c, &q)?;
        let limit = q.limit.unwrap_or(100_000).min(250_000);
        let sql = format!(
            "SELECT p.id, p.name, p.width, p.height, p.orientation, p.taken, p.rating, p.lcolor, p.pick, p.kind, p.folder_id,
                    p.lat IS NOT NULL, p.size, p.mtime, p.companion IS NOT NULL, p.ext
             FROM photos p JOIN folders f ON f.id = p.folder_id WHERE {wh} ORDER BY {} LIMIT {limit}",
            order_by(&q)
        );
        let mut s = c.prepare(&sql)?;
        let mut cols: [Vec<Value>; 14] = Default::default();
        let mut rows = s.query(params_from_iter(args.iter().map(|b| b.as_ref())))?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let (w, h): (Option<i64>, Option<i64>) = (r.get(2)?, r.get(3)?);
            let o: i32 = r.get(4)?;
            let rec = crate::model::PhotoRec { id, size: r.get(12)?, mtime: r.get(13)?, orientation: o, ..Default::default() };
            cols[0].push(json!(id));
            cols[1].push(json!(r.get::<_, String>(1)?));
            cols[2].push(json!(w.unwrap_or(3)));
            cols[3].push(json!(h.unwrap_or(2)));
            cols[4].push(json!(o));
            cols[5].push(json!(r.get::<_, Option<i64>>(5)?));
            cols[6].push(json!(r.get::<_, i32>(6)?));
            cols[7].push(json!(r.get::<_, Option<String>>(7)?));
            cols[8].push(json!(r.get::<_, i32>(8)?));
            cols[9].push(json!(r.get::<_, i32>(9)?));
            cols[10].push(json!(r.get::<_, i64>(10)?));
            cols[11].push(json!(r.get::<_, bool>(11)? as u8));
            cols[12].push(json!(images::sig_str(&rec)));
            cols[13].push(json!(if r.get::<_, bool>(14)? { format!("{}+", r.get::<_, String>(15)?) } else { r.get::<_, String>(15)? }));
        }
        let [id, n, w, h, o, t, r, lc, p, k, f, g, v, x] = cols;
        Ok(json!({ "id": id, "n": n, "w": w, "h": h, "o": o, "t": t, "r": r, "lc": lc, "p": p, "k": k, "f": f, "g": g, "v": v, "x": x }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

async fn facets(State(state): St) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        let group = |sql: &str| -> anyhow::Result<Vec<Value>> {
            let mut s = c.prepare(sql)?;
            let rows = s.query_map([], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, i64>(1)?])))?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        };
        Ok(json!({
            "cameras": group("SELECT model, count(*) n FROM photos WHERE model IS NOT NULL AND pair_of IS NULL GROUP BY model ORDER BY n DESC LIMIT 200")?,
            "lenses": group("SELECT lens, count(*) n FROM photos WHERE lens IS NOT NULL AND pair_of IS NULL GROUP BY lens ORDER BY n DESC LIMIT 300")?,
            "years": group("SELECT strftime('%Y', taken, 'unixepoch') y, count(*) n FROM photos WHERE taken IS NOT NULL AND pair_of IS NULL GROUP BY y ORDER BY y DESC")?,
            "labels": group("SELECT lcolor, count(*) n FROM photos WHERE lcolor IS NOT NULL AND pair_of IS NULL GROUP BY lcolor")?,
        }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct KwQuery {
    q: Option<String>,
}

async fn keywords(State(state): St, Query(q): Query<KwQuery>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        let pat = q.q.unwrap_or_default().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let mut s = c.prepare(
            "SELECT kw, count(*) n FROM photo_keywords WHERE kw LIKE ?1 || '%' ESCAPE '\\' OR kw LIKE '%|' || ?1 || '%' ESCAPE '\\'
             GROUP BY kw ORDER BY n DESC, kw LIMIT 40",
        )?;
        let rows: Vec<Value> = s.query_map([pat], |r| Ok(json!([r.get::<_, String>(0)?, r.get::<_, i64>(1)?])))?.filter_map(|r| r.ok()).collect();
        Ok(json!(rows))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

async fn geo_clusters(State(state): St, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        let zoom = q.zoom.unwrap_or(2).min(22);
        let cell = 90.0 / 2f64.powi(zoom as i32);
        let mut q2 = q.clone();
        if q2.west.is_none() {
            q2.west = Some(-180.0);
            q2.east = Some(180.0);
            q2.south = Some(-90.0);
            q2.north = Some(90.0);
        }
        let (wh, mut args) = build_where(&c, &q2)?;
        let sql = format!(
            "SELECT CAST((p.lat + 90.0) / ? AS INT) gy, CAST((p.lon + 180.0) / ? AS INT) gx, count(*), avg(p.lat), avg(p.lon),
                    max(p.rating * 4294967296 + p.id), min(p.lat), min(p.lon), max(p.lat), max(p.lon)
             FROM photos p JOIN folders f ON f.id = p.folder_id WHERE {wh} AND p.lat IS NOT NULL GROUP BY gy, gx LIMIT 3000"
        );
        let mut all: Vec<Box<dyn ToSql>> = vec![Box::new(cell), Box::new(cell)];
        all.append(&mut args);
        let mut s = c.prepare(&sql)?;
        let mut out = Vec::new();
        let mut rows = s.query(params_from_iter(all.iter().map(|b| b.as_ref())))?;
        let mut reps = Vec::new();
        while let Some(r) = rows.next()? {
            let rep = r.get::<_, i64>(5)?.rem_euclid(4294967296);
            reps.push(rep);
            out.push(json!({
                "n": r.get::<_, i64>(2)?, "lat": r.get::<_, f64>(3)?, "lon": r.get::<_, f64>(4)?, "id": rep,
                "bounds": [r.get::<_, f64>(6)?, r.get::<_, f64>(7)?, r.get::<_, f64>(8)?, r.get::<_, f64>(9)?],
            }));
        }
        // Thumbnail versions of the representatives.
        let mut sigs = std::collections::HashMap::new();
        for chunk in reps.chunks(500) {
            let sql = format!("SELECT id, size, mtime, orientation FROM photos WHERE id IN ({})", chunk.iter().map(|_| "?").collect::<Vec<_>>().join(","));
            let mut s = c.prepare(&sql)?;
            let rows = s.query_map(params_from_iter(chunk.iter()), |r| {
                Ok(crate::model::PhotoRec { id: r.get(0)?, size: r.get(1)?, mtime: r.get(2)?, orientation: r.get(3)?, ..Default::default() })
            })?;
            for rec in rows.flatten() {
                sigs.insert(rec.id, images::sig_str(&rec));
            }
        }
        for o in out.iter_mut() {
            let id = o["id"].as_i64().unwrap_or(0);
            o["v"] = json!(sigs.get(&id));
        }
        Ok(json!({ "cell": cell, "clusters": out }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

// ---------------------------------------------------------------- photo

async fn load(state: &Arc<AppState>, id: i64) -> ApiResult<Located> {
    let st = state.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Located>> {
        let c = st.db.get()?;
        repo::load(&c, id)
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??
    .ok_or_else(not_found)
}

async fn photo_detail(State(state): St, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let loc = load(&state, id).await?;
    let r = &loc.rec;
    let dir = scan::abs_dir(&state, &loc.folder_path);
    let companions = crate::files::companions(&dir, &r.name, &r.stem).unwrap_or_default();
    let sidecar_info = r.sidecar.as_ref().and_then(|s| std::fs::metadata(dir.join(s)).ok().map(|m| (s.clone(), m.len())));
    Ok(Json(json!({
        "id": r.id, "folder_id": r.folder_id, "folder": loc.folder_path, "in_reject": loc.folder_is_reject,
        "name": r.name, "ext": r.ext, "kind": r.kind, "size": r.size, "mtime": r.mtime,
        "width": r.width, "height": r.height, "orientation": r.orientation,
        "taken": r.taken, "taken_str": r.taken_str,
        "make": r.make, "model": r.model, "lens": r.lens, "focal": r.focal, "fnum": r.fnum,
        "exposure": r.exposure, "iso": r.iso, "bias": r.bias,
        "rating": r.rating, "label": r.label, "lcolor": lcolor(r.label.as_deref()), "pick": r.pick,
        "title": r.title, "description": r.description, "creator": r.creator, "rights": r.rights,
        "keywords": r.keywords, "hkeywords": r.hkeywords,
        "lat": r.lat, "lon": r.lon, "alt": r.alt,
        "city": r.city, "state": r.state, "country": r.country, "country_code": r.country_code, "sublocation": r.sublocation,
        "sidecar": sidecar_info.as_ref().map(|s| &s.0), "sidecar_size": sidecar_info.as_ref().map(|s| s.1),
        "companion": r.companion, "files": companions, "error": r.meta_err,
        "v": images::sig_str(r),
    })))
}

fn image_response(bytes: Vec<u8>, mime: &'static str, versioned: bool) -> Response {
    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if versioned { "private, max-age=31536000, immutable" } else { "no-cache" }),
    );
    resp
}

#[derive(Deserialize, Default)]
struct ImgQuery {
    v: Option<String>,
    w: Option<usize>,
    mode: Option<ToneMode>,
    ev: Option<f32>,
    wb: Option<WbMode>,
    prio: Option<String>,
}

async fn thumb(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let path = state.images.thumb(&state, &loc).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    let bytes = tokio::fs::read(path).await.map_err(|e| anyhow::anyhow!(e))?;
    Ok(image_response(bytes, "image/jpeg", q.v.is_some()))
}

async fn preview(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let long = q.w.unwrap_or(2048).clamp(256, 4096);
    let long = [1024, 1600, 2048, 2560, 3200, 4096].into_iter().find(|&s| s >= long).unwrap_or(4096);
    let path = state.images.preview(&state, &loc, long).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    let bytes = tokio::fs::read(path).await.map_err(|e| anyhow::anyhow!(e))?;
    Ok(image_response(bytes, "image/jpeg", q.v.is_some()))
}

async fn full_jpeg(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let bytes = state.images.full_jpeg(&state, &loc).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    Ok(image_response(bytes, "image/jpeg", q.v.is_some()))
}

async fn raw_render(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    if Kind::from_i32(loc.rec.kind) != Kind::Raw {
        return Err(bad("not a RAW file"));
    }
    let long = q.w.unwrap_or(0).min(16384);
    let high = q.prio.as_deref() != Some("low");
    let bytes = state
        .images
        .render(&state, &loc, q.mode.unwrap_or_default(), q.ev.unwrap_or(0.0).clamp(-6.0, 6.0), q.wb.unwrap_or_default(), long, high)
        .await
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    Ok(image_response(bytes.as_ref().clone(), "image/jpeg", q.v.is_some()))
}

async fn raw_mask(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let dev = state.images.developed(&state, &loc, q.wb.unwrap_or_default(), true).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    let d2 = dev.clone();
    let png = tokio::task::spawn_blocking(move || d2.mask_png().to_vec()).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(image_response(png, "image/png", q.v.is_some()))
}

async fn raw_info(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let dev = state.images.developed(&state, &loc, q.wb.unwrap_or_default(), true).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    let v = json!({
        "w": dev.w, "h": dev.h, "hist": dev.hist, "hist_min_ev": images::raw::HIST_MIN_EV, "hist_per_ev": images::raw::HIST_PER_EV,
        "over": dev.over, "under": dev.under, "wb": dev.wb, "baseline_ev": dev.baseline_ev, "boost_ev": images::RENDER_BOOST_EV,
        "decode_ms": dev.decode_ms, "develop_ms": dev.develop_ms, "approximate": dev.approximate,
    });
    let mut resp = Json(v).into_response();
    if q.v.is_some() {
        resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    }
    Ok(resp)
}

// ---------------------------------------------------------------- edits

#[derive(Deserialize)]
struct MetaBody {
    ids: Vec<i64>,
    patch: Patch,
}

fn check_writable(state: &AppState) -> ApiResult<()> {
    if state.is_readonly() {
        return Err(ApiError(StatusCode::FORBIDDEN, "Hyperfocal is in read-only mode".into()));
    }
    Ok(())
}

async fn write_meta(State(state): St, Json(b): Json<MetaBody>) -> ApiResult<Json<Value>> {
    check_writable(&state)?;
    if b.ids.is_empty() || b.ids.len() > 20_000 {
        return Err(bad("between 1 and 20000 photos per change"));
    }
    let r = ops::apply_meta(&state, &b.ids, &b.patch, None).await?;
    Ok(Json(json!(r)))
}

#[derive(Deserialize)]
struct IdsBody {
    ids: Vec<i64>,
}

async fn reject(State(state): St, Json(b): Json<IdsBody>) -> ApiResult<Json<Value>> {
    check_writable(&state)?;
    Ok(Json(json!(ops::reject(&state, &b.ids).await?)))
}

async fn restore(State(state): St, Json(b): Json<IdsBody>) -> ApiResult<Json<Value>> {
    check_writable(&state)?;
    Ok(Json(json!(ops::restore(&state, &b.ids).await?)))
}

#[derive(Deserialize)]
struct UndoBody {
    batch: Option<String>,
}

async fn undo(State(state): St, Json(b): Json<UndoBody>) -> ApiResult<Json<Value>> {
    check_writable(&state)?;
    let batch = match b.batch {
        Some(b) => b,
        None => {
            let c = state.db.get()?;
            c.query_row(
                "SELECT batch FROM journal WHERE undone = 0 AND json_extract(data, '$.undo_of') IS NULL ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
                .map_err(|_| bad("nothing to undo"))?
        }
    };
    Ok(Json(json!(ops::undo(&state, &batch).await?)))
}

async fn journal(State(state): St) -> ApiResult<Json<Value>> {
    Ok(Json(json!(ops::recent_journal(&state, 100)?)))
}

#[derive(Deserialize)]
struct RescanBody {
    folder: Option<i64>,
}

async fn rescan(State(state): St, Json(b): Json<RescanBody>) -> ApiResult<Json<Value>> {
    match b.folder {
        Some(f) => {
            let st = state.clone();
            tokio::task::spawn_blocking(move || scan::scan_folder(&st, f, true)).await.map_err(|e| anyhow::anyhow!("{e}"))??;
        }
        None => state.request_scan(ScanMsg::Full),
    }
    Ok(Json(json!({"ok": true})))
}

async fn get_settings(State(state): St) -> ApiResult<Json<Value>> {
    let c = state.db.get()?;
    let v = crate::db::get_setting(&c, "ui").and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(json!({}));
    Ok(Json(v))
}

async fn set_settings(State(state): St, Json(v): Json<Value>) -> ApiResult<Json<Value>> {
    if !v.is_object() {
        return Err(bad("settings must be an object"));
    }
    let c = state.db.get()?;
    crate::db::set_setting(&c, "ui", &v.to_string())?;
    Ok(Json(v))
}
