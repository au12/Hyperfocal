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

pub(crate) type St = State<Arc<AppState>>;

pub struct ApiError(pub(crate) StatusCode, pub(crate) String);

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

pub(crate) fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not found".into())
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

pub fn router(state: Arc<AppState>) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/health", get(|| async { "ok" }))
        .route("/events", get(events))
        .route("/tree", get(tree))
        .route("/photos", get(list_photos))
        .route("/facets", get(facets))
        .route("/keywords", get(keywords))
        .route("/keywords/tree", get(keyword_tree))
        .route("/keywords/move", post(keyword_move))
        .route("/jobs", get(jobs))
        .route("/timeline", get(timeline))
        .route("/collections", get(list_collections).post(save_collection))
        .route("/collections/{id}", axum::routing::delete(delete_collection))
        .route("/geo/clusters", get(geo_clusters))
        .route("/photo/{id}", get(photo_detail))
        .route("/photo/{id}/thumb", get(thumb))
        .route("/photo/{id}/preview", get(preview))
        .route("/photo/{id}/jpeg", get(full_jpeg))
        .route("/photo/{id}/raw", get(raw_render))
        .route("/photo/{id}/rawmask", get(raw_mask))
        .route("/photo/{id}/rawinfo", get(raw_info))
        .route("/photo/{id}/rawtile", get(raw_tile))
        .route("/photo/{id}/cfatile", get(cfa_tile))
        .route("/photo/{id}/pyramid", get(pyramid_info))
        .route("/photo/{id}/ptile", get(pyramid_tile))
        .route("/lut", get(view_lut))
        .route("/meta", post(write_meta))
        .route("/reject", post(reject))
        .route("/restore", post(restore))
        .route("/undo", post(undo))
        .route("/journal", get(journal))
        .route("/rescan", post(rescan))
        .route("/settings", get(get_settings).post(set_settings))
        .merge(crate::geotag::routes())
        .merge(crate::integrity::routes())
        .merge(crate::people::api::routes())
        .merge(crate::dupes::api::routes())
        .merge(crate::albums::routes())
        .merge(crate::immich::routes())
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
    // Nested under /api; accept either form of the path.
    let path = req.uri().path();
    let path = path.strip_prefix("/api").unwrap_or(path);
    if matches!(path, "/login" | "/status" | "/health") || has_session(&state, req.headers()) {
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
    let logged_in = state.cfg.password.is_none() || has_session(&state, &headers);
    if !logged_in {
        // Before login, say only enough to show the login screen.
        return Ok(Json(json!({"version": env!("CARGO_PKG_VERSION"), "auth": true, "logged_in": false})));
    }
    let st = state.clone();
    let counts = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        // Every folder keeps the count of its photos (pairs folded), so this is cheap.
        let photos: i64 = c.query_row("SELECT COALESCE(sum(photo_count), 0) FROM folders", [], |r| r.get(0))?;
        let folders: i64 = c.query_row("SELECT count(*) FROM folders", [], |r| r.get(0))?;
        let geo: i64 = c.query_row("SELECT count(*) FROM photos WHERE lat IS NOT NULL AND pair_of IS NULL", [], |r| r.get(0))?;
        let (changed, missing, unreadable) = crate::integrity::counts(&c)?;
        Ok(json!({"photos": photos, "folders": folders, "geotagged": geo, "integrity": {"changed": changed, "missing": missing, "unreadable": unreadable}}))
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
        "decoders": state.decoders,
        "integrity": state.cfg.integrity,
        "immich": crate::immich::status(&state),
        "auth": state.cfg.password.is_some(),
        "logged_in": true,
    })))
}

async fn events(State(state): St) -> impl IntoResponse {
    let rx = state.events.subscribe();
    let first = futures_util::stream::once(async { Event::Progress(Default::default()) }).filter(|_| async { false });
    let stream = first.chain(BroadcastStream::new(rx).filter_map(|e| async move { e.ok() })).map(|e| {
        Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(serde_json::to_string(&e).unwrap_or_default()))
    });
    // Reverse proxies (nginx / Nginx Proxy Manager) must not buffer the event stream.
    ([(header::HeaderName::from_static("x-accel-buffering"), "no")], Sse::new(stream).keep_alive(KeepAlive::default()))
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
    /// 1: only count the matching photos.
    count: Option<u8>,
    /// 1: only photos without a capture time.
    nodate: Option<u8>,
    /// People, pets or groups (ids, comma-separated): photos with all of them.
    subj: Option<String>,
    /// Only these photos (ids, comma-separated): a group of the Duplicates report.
    ids: Option<String>,
    /// Only the photos of this album.
    album: Option<i64>,
}

/// SQL for "this folder and everything below it" on `f.path`, as an index range (paths
/// below `a` sort between `a/` and `a0`, since '0' follows '/').
fn subtree(path: &str, a: &mut Vec<Box<dyn ToSql>>) -> String {
    a.push(Box::new(path.to_string()));
    a.push(Box::new(format!("{path}/")));
    a.push(Box::new(format!("{path}0")));
    "(f.path = ? OR (f.path >= ? AND f.path < ?))".into()
}

fn build_where(c: &Connection, q: &ListQuery, trigram: bool) -> anyhow::Result<(String, Vec<Box<dyn ToSql>>)> {
    let mut w: Vec<String> = vec!["p.pair_of IS NULL".into()];
    let mut a: Vec<Box<dyn ToSql>> = Vec::new();
    let mut folder_is_reject = false;
    if let Some(fid) = q.folder {
        let (path, is_rej): (String, i64) = c.query_row("SELECT path, is_reject FROM folders WHERE id = ?1", [fid], |r| Ok((r.get(0)?, r.get(1)?)))?;
        folder_is_reject = is_rej != 0;
        if q.rec.unwrap_or(0) == 1 {
            if !path.is_empty() {
                w.push(subtree(&path, &mut a));
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
    if q.nodate == Some(1) {
        w.push("p.taken IS NULL".into());
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
    if let Some(v) = q.kw.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        // The keyword, and every path below it (`Places|Portugal` takes in
        // `Places|Portugal|Lisbon`): the range ends at the separator's successor.
        w.push("p.id IN (SELECT photo_id FROM photo_keywords WHERE kw = ? OR (kw > ? AND kw < ?))".into());
        a.push(Box::new(v.to_string()));
        a.push(Box::new(format!("{v}|")));
        a.push(Box::new(format!("{v}}}")));
    }
    if let Some(v) = q.subj.as_deref().filter(|s| !s.is_empty()) {
        for id in v.split(',').filter_map(|x| x.trim().parse::<i64>().ok()).take(8) {
            w.push("p.id IN (SELECT photo_id FROM detections WHERE subject_id = ?)".into());
            a.push(Box::new(id));
        }
    }
    if let Some(v) = q.ids.as_deref() {
        let ids: Vec<i64> = v.split(',').filter_map(|x| x.trim().parse::<i64>().ok()).take(5000).collect();
        if ids.is_empty() {
            w.push("0".into());
        } else {
            w.push(format!("p.id IN ({})", vec!["?"; ids.len()].join(",")));
            for id in ids {
                a.push(Box::new(id));
            }
        }
    }
    if let Some(album) = q.album {
        w.push("p.id IN (SELECT photo_id FROM album_photos WHERE album_id = ? AND photo_id IS NOT NULL)".into());
        a.push(Box::new(album));
    }
    if let Some(text) = q.q.as_deref() {
        let s = crate::db::search_query(text, trigram);
        if let Some(fq) = s.prefix {
            w.push("p.id IN (SELECT rowid FROM photos_fts WHERE photos_fts MATCH ?)".into());
            a.push(Box::new(fq));
        }
        if let Some(fq) = s.substring {
            w.push("p.id IN (SELECT rowid FROM photos_tri WHERE photos_tri MATCH ?)".into());
            a.push(Box::new(fq));
        }
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
        // An album's own order (the id is a number: safe to put in the SQL).
        Some("album") if q.album.is_some() => {
            format!("(SELECT ap.pos FROM album_photos ap WHERE ap.album_id = {} AND ap.photo_id = p.id) {d}, p.id", q.album.unwrap_or(0))
        }
        _ => format!("COALESCE(p.taken, p.mtime / 1000) {d}, p.name COLLATE NOCASE {d}"),
    }
}

/// The file-type badge of a grid tile: `cr3`, or `cr3+jpg` / `cr3+jpg+hif` for a RAW
/// with JPEG/HEIF files folded in (`companion` lists their names).
pub(crate) fn file_label_into(label: &mut String, ext: &str, companion: Option<&str>) {
    label.push_str(ext);
    for name in companion.unwrap_or_default().split(',').filter(|n| !n.is_empty()) {
        label.push('+');
        label.push_str(&crate::model::split_name(name).1);
    }
}

#[cfg(test)]
fn file_label(ext: &str, companion: Option<&str>) -> String {
    let mut label = String::new();
    file_label_into(&mut label, ext, companion);
    label
}

/// A JSON string literal.
fn json_str(out: &mut String, s: &str) {
    use std::fmt::Write;
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The columnar photo list, written straight into one string per column: a
/// 300,000-photo list is 20 MB of JSON, and building it as `serde_json` values would
/// allocate millions of small objects first.
struct Columns {
    cols: Vec<String>,
    rows: usize,
}

impl Columns {
    const NAMES: [&'static str; 14] = ["id", "n", "w", "h", "o", "t", "r", "lc", "p", "k", "f", "g", "v", "x"];

    fn new() -> Columns {
        Columns { cols: (0..Self::NAMES.len()).map(|_| String::with_capacity(4096)).collect(), rows: 0 }
    }
    fn row(&mut self) {
        if self.rows > 0 {
            for c in &mut self.cols {
                c.push(',');
            }
        }
        self.rows += 1;
    }
    fn int(&mut self, i: usize, v: i64) {
        use std::fmt::Write;
        let _ = write!(self.cols[i], "{v}");
    }
    fn opt_int(&mut self, i: usize, v: Option<i64>) {
        match v {
            Some(v) => self.int(i, v),
            None => self.cols[i].push_str("null"),
        }
    }
    fn str(&mut self, i: usize, v: &str) {
        json_str(&mut self.cols[i], v);
    }
    fn opt_str(&mut self, i: usize, v: Option<&str>) {
        match v {
            Some(v) => self.str(i, v),
            None => self.cols[i].push_str("null"),
        }
    }
    fn finish(self) -> String {
        let mut out = String::with_capacity(self.cols.iter().map(|c| c.len() + 8).sum::<usize>() + 2);
        out.push('{');
        for (i, (name, col)) in Self::NAMES.iter().zip(self.cols).enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(name);
            out.push_str("\":[");
            out.push_str(&col);
            out.push(']');
        }
        out.push('}');
        out
    }
}

fn json_response(body: String) -> Response {
    let mut resp = Response::new(Body::from(body));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

async fn list_photos(State(state): St, Query(q): Query<ListQuery>) -> ApiResult<Response> {
    let count_only = q.count.unwrap_or(0) == 1;
    // First visit to a folder: index it right away; later visits re-check in the background.
    if let Some(fid) = q.folder.filter(|_| !count_only) {
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
    let body = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let c = st.db.get()?;
        let (wh, args) = build_where(&c, &q, st.trigram_ready())?;
        if count_only {
            let n: i64 = c.query_row(
                &format!("SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id WHERE {wh}"),
                params_from_iter(args.iter().map(|b| b.as_ref())),
                |r| r.get(0),
            )?;
            return Ok(format!("{{\"count\":{n}}}"));
        }
        let limit = q.limit.unwrap_or(100_000).min(250_000);
        let sql = format!(
            "SELECT p.id, p.name, p.width, p.height, p.orientation, p.taken, p.rating, p.lcolor, p.pick, p.kind, p.folder_id,
                    p.lat IS NOT NULL, p.size, p.mtime, p.companion, p.ext
             FROM photos p JOIN folders f ON f.id = p.folder_id WHERE {wh} ORDER BY {} LIMIT {limit}",
            order_by(&q)
        );
        let mut s = c.prepare(&sql)?;
        let mut cols = Columns::new();
        let mut rows = s.query(params_from_iter(args.iter().map(|b| b.as_ref())))?;
        let mut label = String::new();
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let (w, h): (Option<i64>, Option<i64>) = (r.get(2)?, r.get(3)?);
            let o: i32 = r.get(4)?;
            let rec = crate::model::PhotoRec { id, size: r.get(12)?, mtime: r.get(13)?, orientation: o, ..Default::default() };
            cols.row();
            cols.int(0, id);
            cols.str(1, r.get_ref(1)?.as_str()?);
            cols.int(2, w.unwrap_or(3));
            cols.int(3, h.unwrap_or(2));
            cols.int(4, o as i64);
            cols.opt_int(5, r.get(5)?);
            cols.int(6, r.get::<_, i64>(6)?);
            cols.opt_str(7, r.get_ref(7)?.as_str_or_null()?);
            cols.int(8, r.get::<_, i64>(8)?);
            cols.int(9, r.get::<_, i64>(9)?);
            cols.int(10, r.get::<_, i64>(10)?);
            cols.int(11, r.get::<_, bool>(11)? as i64);
            cols.str(12, &images::sig_str(&rec));
            label.clear();
            file_label_into(&mut label, r.get_ref(15)?.as_str()?, r.get_ref(14)?.as_str_or_null()?);
            cols.str(13, &label);
        }
        Ok(cols.finish())
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(json_response(body))
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
        // Years from day buckets: integer division walks the covering index, where
        // strftime() on 300,000 rows costs more than the scan itself.
        let mut years: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
        for (day, n) in day_counts(&c, false)? {
            *years.entry(crate::model::civil_from_days(day).0).or_default() += n;
        }
        let years: Vec<Value> = years.into_iter().rev().map(|(y, n)| json!([y.to_string(), n])).collect();
        Ok(json!({
            "cameras": group("SELECT model, count(*) n FROM photos WHERE model IS NOT NULL AND pair_of IS NULL GROUP BY model ORDER BY n DESC LIMIT 200")?,
            "lenses": group("SELECT lens, count(*) n FROM photos WHERE lens IS NOT NULL AND pair_of IS NULL GROUP BY lens ORDER BY n DESC LIMIT 300")?,
            "years": years,
            "labels": group("SELECT lcolor, count(*) n FROM photos WHERE lcolor IS NOT NULL AND pair_of IS NULL GROUP BY lcolor")?,
        }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

/// Photos per capture day (days since 1970-01-01, camera clock), optionally without
/// the reject folders.
fn day_counts(c: &Connection, skip_rejects: bool) -> anyhow::Result<Vec<(i64, i64)>> {
    // Floor division, so photos from before 1970 land on the right day.
    let day = "(p.taken / 86400) - (p.taken % 86400 < 0)";
    let sql = if skip_rejects {
        format!("SELECT {day} d, count(*) FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.pair_of IS NULL AND p.taken IS NOT NULL AND f.is_reject = 0 GROUP BY d")
    } else {
        format!("SELECT {day} d, count(*) FROM photos p WHERE p.pair_of IS NULL AND p.taken IS NOT NULL GROUP BY d")
    };
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Photos per capture day across the library (reject folders left out), for the
/// timeline: `{"days": [[day, count], ...], "undated": n}`, days since 1970-01-01 on
/// the camera's clock.
async fn timeline(State(state): St) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        let days: Vec<Value> = day_counts(&c, true)?.into_iter().map(|(d, n)| json!([d, n])).collect();
        let undated: i64 = c.query_row(
            "SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.pair_of IS NULL AND p.taken IS NULL AND f.is_reject = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(json!({ "days": days, "undated": undated }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

/// Saved searches ("smart collections"). The spec is the UI's description of a view
/// (search text, filters, scope, dates, sort); the server only stores it.
async fn list_collections(State(state): St) -> ApiResult<Json<Value>> {
    let c = state.db.get()?;
    let mut st = c.prepare("SELECT id, name, spec, created, updated FROM collections ORDER BY position, name COLLATE NOCASE")?;
    let rows: Vec<Value> = st
        .query_map([], |r| {
            let spec: String = r.get(2)?;
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?,
                "spec": serde_json::from_str::<Value>(&spec).unwrap_or(json!({})),
                "created": r.get::<_, i64>(3)?, "updated": r.get::<_, i64>(4)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(Json(json!(rows)))
}

#[derive(Deserialize)]
struct CollectionBody {
    id: Option<i64>,
    name: String,
    spec: Value,
}

async fn save_collection(State(state): St, Json(b): Json<CollectionBody>) -> ApiResult<Json<Value>> {
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(bad("a collection needs a name of at most 120 characters"));
    }
    if !b.spec.is_object() {
        return Err(bad("the collection's spec must be an object"));
    }
    let spec = b.spec.to_string();
    if spec.len() > 16 * 1024 {
        return Err(bad("the collection's spec is too large"));
    }
    let c = state.db.get()?;
    let now = crate::db::now_ms();
    let id = match b.id {
        Some(id) => {
            let n = c.execute("UPDATE collections SET name = ?2, spec = ?3, updated = ?4 WHERE id = ?1", rusqlite::params![id, name, spec, now])?;
            if n == 0 {
                return Err(not_found());
            }
            id
        }
        None => c.query_row(
            "INSERT INTO collections(name, spec, position, created, updated) VALUES(?1, ?2, 0, ?3, ?3) RETURNING id",
            rusqlite::params![name, spec, now],
            |r| r.get(0),
        )?,
    };
    state.emit(Event::Collections);
    Ok(Json(json!({ "id": id, "name": name, "spec": b.spec, "updated": now })))
}

async fn delete_collection(State(state): St, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let c = state.db.get()?;
    if c.execute("DELETE FROM collections WHERE id = ?1", [id])? == 0 {
        return Err(not_found());
    }
    state.emit(Event::Collections);
    Ok(Json(json!({ "ok": true })))
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

/// A photo's id and keyword lists (plain, hierarchical).
type KeywordRow = (i64, Vec<String>, Vec<String>);

/// Every photo's keyword lists (pairs folded).
fn keyword_rows(c: &Connection) -> anyhow::Result<Vec<KeywordRow>> {
    let split = |s: Option<String>| -> Vec<String> { s.map(|s| s.split('\n').filter(|x| !x.is_empty()).map(str::to_string).collect()).unwrap_or_default() };
    let mut st = c.prepare("SELECT id, keywords, hkeywords FROM photos WHERE pair_of IS NULL AND (keywords IS NOT NULL OR hkeywords IS NOT NULL)")?;
    let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, split(r.get(1)?), split(r.get(2)?))))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The library's keyword tree: `{"nodes": [[path, photos], ...]}`, where a node's count
/// includes the photos of the nodes below it.
async fn keyword_tree(State(state): St) -> ApiResult<Json<Value>> {
    let st = state.clone();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let c = st.db.get()?;
        let rows = keyword_rows(&c)?;
        let tree = crate::keywords::tree(rows.iter().map(|(_, s, h)| (s.as_slice(), h.as_slice())));
        Ok(json!({ "nodes": tree }))
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct KwMoveBody {
    from: String,
    to: Option<String>,
}

/// Rename, merge, re-parent or remove a keyword across the library, as one undoable
/// batch written in the background.
async fn keyword_move(State(state): St, Json(b): Json<KwMoveBody>) -> ApiResult<Json<Value>> {
    check_writable(&state)?;
    let from = crate::keywords::split(&b.from);
    let to = b.to.as_deref().map(crate::keywords::split);
    crate::keywords::check_move(&from, to.as_deref()).map_err(bad)?;
    // Names of people and pets go with face regions and the index: they are renamed,
    // merged and removed in the People view.
    if from.first().is_some_and(|r| r.eq_ignore_ascii_case("People") || r.eq_ignore_ascii_case("Pets")) {
        return Err(bad("people and pets are renamed, merged and removed in the People & pets view"));
    }
    let st = state.clone();
    let from2 = from.clone();
    let ids = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<i64>> {
        let c = st.db.get()?;
        Ok(keyword_rows(&c)?.into_iter().filter(|(_, s, h)| crate::keywords::has_node(s, h, &from2)).map(|(id, _, _)| id).collect())
    })
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))??;
    if ids.is_empty() {
        return Err(bad("no photo has this keyword"));
    }
    let show = |p: &[String]| p.join(" › ");
    let label = match &to {
        Some(t) => format!("Keyword {} → {}", show(&from), show(t)),
        None => format!("Removing keyword {}", show(&from)),
    };
    let patch = Patch {
        keywords_move: Some(ops::KwMove { from: crate::keywords::join(&from), to: to.as_deref().map(crate::keywords::join) }),
        ..Default::default()
    };
    let job = ops::spawn_patch_job(&state, label, ids.into_iter().map(|id| (id, patch.clone())).collect())?;
    Ok(Json(json!({ "job": job })))
}

async fn jobs(State(state): St) -> Json<Value> {
    let mut v: Vec<ops::JobStatus> = state.jobs.lock().values().cloned().collect();
    v.sort_by_key(|j| -j.started);
    Json(json!(v))
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
        let (wh, mut args) = build_where(&c, &q2, st.trigram_ready())?;
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
    // The files that make up this photo (and move with it).
    let companions = {
        let (st, folder, is_reject, name) = (state.clone(), loc.folder_path.clone(), loc.folder_is_reject, r.name.clone());
        tokio::task::spawn_blocking(move || {
            let listing = scan::listing(&st, &folder, is_reject).ok()?;
            crate::sets::Plan::new(&listing).move_set(&name).ok()
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
    };
    let sidecar_info = r.sidecar.as_ref().and_then(|s| std::fs::metadata(dir.join(s)).ok().map(|m| (s.clone(), m.len())));
    let albums = crate::albums::of_photo(&*state.db.get()?, r.id)?;
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
        "v": images::sig_str(r), "albums": albums,
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
    /// Full-resolution tile column and row.
    tx: Option<usize>,
    ty: Option<usize>,
    /// Pyramid level of a zoom tile (0: full size).
    l: Option<usize>,
}

async fn thumb(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    // Made already (nearly always): straight from the pack, without an index lookup. The
    // version is part of the key, so this is always the image the URL names.
    if let Some(v) = q.v.clone() {
        let st = state.clone();
        if let Ok(Some(bytes)) = tokio::task::spawn_blocking(move || st.images.cached_thumb(id, &v)).await {
            return Ok(image_response(bytes, "image/jpeg", true));
        }
    }
    let loc = load(&state, id).await?;
    let (bytes, draft) = state.images.thumb(&state, &loc).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    let mut resp = image_response(bytes, "image/jpeg", q.v.is_some() && !draft);
    if draft {
        // Shown until the full-quality thumbnail is ready (an event says when): never cached.
        resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    Ok(resp)
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

/// A full-resolution (1:1) tile of a RAW file's visible area, demosaiced on the server,
/// in the colours and tone of the RAW view.
async fn raw_tile(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    if Kind::from_i32(loc.rec.kind) != Kind::Raw {
        return Err(bad("not a RAW file"));
    }
    let (Some(tx), Some(ty)) = (q.tx, q.ty) else { return Err(bad("tx and ty are required")) };
    let bytes = state
        .images
        .raw_tile(&state, &loc, q.mode.unwrap_or_default(), q.ev.unwrap_or(0.0).clamp(-6.0, 6.0), q.wb.unwrap_or_default(), tx, ty)
        .await
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    Ok(image_response(bytes.as_ref().clone(), "image/jpeg", q.v.is_some()))
}

/// A tile's demosaic input for the viewer's GPU path (see `images::raw::cfa_tile`).
async fn cfa_tile(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    if Kind::from_i32(loc.rec.kind) != Kind::Raw {
        return Err(bad("not a RAW file"));
    }
    let (Some(tx), Some(ty)) = (q.tx, q.ty) else { return Err(bad("tx and ty are required")) };
    let bytes = state.images.cfa_tile(&state, &loc, q.wb.unwrap_or_default(), tx, ty).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    Ok(image_response(bytes.as_ref().clone(), "application/octet-stream", q.v.is_some()))
}

/// The zoom pyramid of the non-RAW view (a JPEG, HEIC, TIFF or PNG, or a RAW file's
/// embedded JPEG): full size, levels, tile size and apron.
async fn pyramid_info(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let p = state.images.pyramid(&state, &loc).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    let mut resp = Json(json!(p)).into_response();
    if q.v.is_some() {
        resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    }
    Ok(resp)
}

/// A zoom tile of the non-RAW view: level `l`, column `tx`, row `ty` (JPEG).
async fn pyramid_tile(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let (Some(l), Some(tx), Some(ty)) = (q.l, q.tx, q.ty) else { return Err(bad("l, tx and ty are required")) };
    let bytes = state.images.pyramid_tile(&state, &loc, l, tx, ty).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    Ok(image_response(bytes.as_ref().clone(), "image/jpeg", q.v.is_some()))
}

/// The tone curve of a RAW view (65536 bytes: the display value of every f16 pattern
/// of linear sRGB), for the viewer's GPU path.
async fn view_lut(State(state): St, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let lut = state.images.view_lut(q.mode.unwrap_or_default(), q.ev.unwrap_or(0.0).clamp(-6.0, 6.0));
    Ok(image_response(lut.as_ref().clone(), "application/octet-stream", q.v.is_some()))
}

/// The RAW clipping mask (PNG; pixel value = MASK_* bits). `ev`: the exposure correction
/// the overexposure display follows (as FastRawViewer's does).
async fn raw_mask(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let ev = q.ev.unwrap_or(0.0).clamp(-6.0, 6.0);
    let png = state.images.mask_png(&state, &loc, q.wb.unwrap_or_default(), ev).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    Ok(image_response(png.as_ref().clone(), "image/png", q.v.is_some()))
}

async fn raw_info(State(state): St, Path(id): Path<i64>, Query(q): Query<ImgQuery>) -> ApiResult<Response> {
    let loc = load(&state, id).await?;
    let dev = state.images.developed(&state, &loc, q.wb.unwrap_or_default(), true).await.map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?;
    let v = json!({
        "w": dev.w, "h": dev.h, "hist": dev.hist, "hist_min_ev": images::raw::HIST_MIN_EV, "hist_per_ev": images::raw::HIST_PER_EV,
        "over": dev.over, "under": dev.under, "wb": dev.wb, "baseline_ev": dev.baseline_ev, "boost_ev": images::RENDER_BOOST_EV,
        "decode_ms": dev.decode_ms, "develop_ms": dev.develop_ms, "approximate": dev.approximate, "decoder": dev.decoder,
        "full": dev.full.map(|(w, h)| [w, h]), "tile": images::raw::TILE, "apron": images::raw::TILE_APRON,
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

pub(crate) fn check_writable(state: &AppState) -> ApiResult<()> {
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
    // Album changes only touch the index: they undo in read-only mode too.
    let albums_only = match b.batch.as_deref() {
        Some(batch) => crate::albums::batch_is_albums(&*state.db.get()?, batch)?,
        None => false,
    };
    if !albums_only {
        check_writable(&state)?;
    }
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
    // Large batches (a keyword moved across the library) undo in the background.
    if ops::undo_count(&state, &batch)? > 200 {
        let job = ops::spawn_undo(&state, &batch, "Undoing a large change".into())?;
        return Ok(Json(json!({ "batch": job.batch, "ok": [], "failed": [], "job": job })));
    }
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

#[cfg(test)]
mod tests {
    use super::{Columns, file_label};

    #[test]
    fn columns_are_valid_json() {
        let mut c = Columns::new();
        for (i, name) in ["IMG_1.CR3", "quote \" back\\slash\n\t\u{1}", "Zürich ✓"].iter().enumerate() {
            c.row();
            for col in 0..Columns::NAMES.len() {
                match col {
                    1 | 12 | 13 => c.str(col, name),
                    5 => c.opt_int(col, (i % 2 == 0).then_some(-5)),
                    7 => c.opt_str(col, (i == 1).then_some("red")),
                    _ => c.int(col, i as i64 * 1000 - 1),
                }
            }
        }
        let v: serde_json::Value = serde_json::from_str(&c.finish()).unwrap();
        assert_eq!(v["n"][1], "quote \" back\\slash\n\t\u{1}");
        assert_eq!(v["n"][2], "Zürich ✓");
        assert_eq!(v["t"], serde_json::json!([-5, null, -5]));
        assert_eq!(v["lc"], serde_json::json!([null, "red", null]));
        assert_eq!(v["id"], serde_json::json!([-1, 999, 1999]));
        let empty: serde_json::Value = serde_json::from_str(&Columns::new().finish()).unwrap();
        assert_eq!(empty["x"], serde_json::json!([]));
    }

    #[test]
    fn badges() {
        assert_eq!(file_label("cr3", None), "cr3");
        assert_eq!(file_label("cr3", Some("IMG_1.JPG")), "cr3+jpg");
        assert_eq!(file_label("cr3", Some("IMG_1.HIF")), "cr3+hif");
        assert_eq!(file_label("cr2", Some("IMG_1.JPG,IMG_1.HIF")), "cr2+jpg+hif");
        assert_eq!(file_label("dng", Some("")), "dng");
    }
}

#[cfg(test)]
mod collection_tests {
    use super::*;
    use axum::http::Request;
    use std::collections::HashMap;
    use tower::ServiceExt;

    async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
        let req = req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn collections_and_timeline() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("photos")).unwrap();
        let mut cfg = crate::config::Config::for_tests(tmp.path());
        cfg.readonly = true;
        let state = crate::build_state(cfg).await.unwrap();
        {
            let c = state.db.get().unwrap();
            c.execute_batch(
                "INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(1, NULL, '', 'Library', 1), (2, 1, '2019', '2019', 1), (3, 2, '2019/Trip', 'Trip', 1), (4, 3, '2019/Trip/_Rejected', '_Rejected', 1);
                 UPDATE folders SET is_reject = 1 WHERE id = 4;",
            )
            .unwrap();
            for (folder, name, taken, rating) in [(3, "a.CR3", Some(1_560_000_000i64), 5), (3, "b.CR3", Some(1_560_090_000), 0), (3, "c.JPG", None, 3), (4, "d.CR3", Some(1_560_000_100), 0)] {
                let rec = crate::model::PhotoRec { folder_id: folder, name: name.into(), stem: name.into(), ext: "cr3".into(), kind: 1, taken, rating, ..Default::default() };
                let id = crate::repo::upsert(&c, &rec).unwrap();
                crate::repo::index_text(&c, id, &rec, "2019/Trip").unwrap();
            }
            crate::repo::refresh_folder(&c, 3).unwrap();
            crate::repo::refresh_folder(&c, 4).unwrap();
        }
        let app = router(state.clone());
        // The timeline leaves the reject folder out and counts photos without a date.
        let (st, tl) = call(&app, "GET", "/api/timeline", None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(tl["undated"], 1);
        let days = tl["days"].as_array().unwrap();
        assert_eq!(days.len(), 2, "{tl}");
        assert_eq!(days[0], json!([1_560_000_000 / 86400, 1]));
        // Counts, a date range and "no date".
        let (_, n) = call(&app, "GET", "/api/photos?folder=1&rec=1&count=1", None).await;
        assert_eq!(n["count"], 3);
        let (_, n) = call(&app, "GET", "/api/photos?folder=1&rec=1&nodate=1&count=1", None).await;
        assert_eq!(n["count"], 1);
        let (_, n) = call(&app, "GET", "/api/photos?folder=1&rec=1&from=1559952000&to=1560038400&count=1", None).await;
        assert_eq!(n["count"], 1);
        // Subtree ranges never catch a sibling folder that only shares a prefix.
        {
            let c = state.db.get().unwrap();
            c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(5, 1, '20190', '20190', 1)", []).unwrap();
            let rec = crate::model::PhotoRec { folder_id: 5, name: "x.CR3".into(), stem: "x".into(), ext: "cr3".into(), kind: 1, ..Default::default() };
            crate::repo::upsert(&c, &rec).unwrap();
            crate::repo::refresh_folder(&c, 5).unwrap();
        }
        let (_, n) = call(&app, "GET", "/api/photos?folder=2&rec=1&count=1", None).await;
        assert_eq!(n["count"], 3);
        // Collections: create, list, rename, delete; works in read-only mode (index only).
        let spec = json!({"filters": {"rmin": 5}, "folder": "2019", "rec": true});
        let (st, c) = call(&app, "POST", "/api/collections", Some(json!({"name": " Best of 2019 ", "spec": spec}))).await;
        assert_eq!(st, StatusCode::OK, "{c}");
        let id = c["id"].as_i64().unwrap();
        assert_eq!(c["name"], "Best of 2019");
        let (_, list) = call(&app, "GET", "/api/collections", None).await;
        assert_eq!(list[0]["spec"], spec);
        let (st, _) = call(&app, "POST", "/api/collections", Some(json!({"id": id, "name": "Best", "spec": spec}))).await;
        assert_eq!(st, StatusCode::OK);
        let (_, list) = call(&app, "GET", "/api/collections", None).await;
        assert_eq!(list[0]["name"], "Best");
        assert_eq!(call(&app, "POST", "/api/collections", Some(json!({"name": "", "spec": spec}))).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(&app, "POST", "/api/collections", Some(json!({"name": "x", "spec": [1]}))).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(&app, "POST", "/api/collections", Some(json!({"id": 999, "name": "x", "spec": {}}))).await.0, StatusCode::NOT_FOUND);
        assert_eq!(call(&app, "DELETE", &format!("/api/collections/{id}"), None).await.0, StatusCode::OK);
        assert_eq!(call(&app, "DELETE", &format!("/api/collections/{id}"), None).await.0, StatusCode::NOT_FOUND);
        let (_, list) = call(&app, "GET", "/api/collections", None).await;
        assert_eq!(list, json!([]));
    }

    #[tokio::test]
    async fn keyword_filter_and_tree() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("photos")).unwrap();
        let mut cfg = crate::config::Config::for_tests(tmp.path());
        cfg.readonly = true;
        let state = crate::build_state(cfg).await.unwrap();
        {
            let c = state.db.get().unwrap();
            c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(1, NULL, '', 'Library', 1)", []).unwrap();
            let photos: [(&str, &[&str], &[&str]); 5] = [
                ("a.CR3", &["Lisbon", "Portugal", "Places"], &["Places|Portugal|Lisbon"]),
                ("b.CR3", &["Ärzte"], &["Places|Portugal|Ärzte"]),
                ("c.CR3", &["Lisbon"], &[]),
                ("d.CR3", &["dog"], &["Places|Portugalia"]),
                ("e.CR3", &[], &[]),
            ];
            for (name, subj, hier) in photos {
                let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
                let rec = crate::model::PhotoRec { folder_id: 1, name: name.into(), stem: name.into(), ext: "cr3".into(), kind: 1, keywords: v(subj), hkeywords: v(hier), ..Default::default() };
                let id = crate::repo::upsert(&c, &rec).unwrap();
                crate::repo::index_text(&c, id, &rec, "").unwrap();
            }
            crate::repo::refresh_folder(&c, 1).unwrap();
        }
        let app = router(state.clone());
        let count = |kw: &str| format!("/api/photos?folder=1&rec=1&count=1&kw={}", urlencode(kw));
        // A path takes in the paths below it (any letters), never a sibling sharing a prefix.
        assert_eq!(call(&app, "GET", &count("Places|Portugal"), None).await.1["count"], 2);
        assert_eq!(call(&app, "GET", &count("places|portugal|lisbon"), None).await.1["count"], 1);
        assert_eq!(call(&app, "GET", &count("Places"), None).await.1["count"], 3);
        assert_eq!(call(&app, "GET", &count("Lisbon"), None).await.1["count"], 2);
        assert_eq!(call(&app, "GET", &count(" dog "), None).await.1["count"], 1);
        // The tree counts each node once per photo; a covered plain keyword is no node.
        let (st, t) = call(&app, "GET", "/api/keywords/tree", None).await;
        assert_eq!(st, StatusCode::OK);
        let nodes: HashMap<String, i64> = t["nodes"].as_array().unwrap().iter().map(|n| (n[0].as_str().unwrap().to_string(), n[1].as_i64().unwrap())).collect();
        assert_eq!(nodes["Places"], 3);
        assert_eq!(nodes["Places|Portugal"], 2);
        assert_eq!(nodes["Places|Portugalia"], 1);
        assert_eq!(nodes["Lisbon"], 1, "only c.CR3 has Lisbon outside a path");
        assert!(!nodes.contains_key("Portugal") && !nodes.contains_key("Ärzte"));
        // Changes are refused in read-only mode.
        let (st, _) = call(&app, "POST", "/api/keywords/move", Some(json!({"from": "Lisbon", "to": "Places|Portugal|Lisbon"}))).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }

    fn urlencode(s: &str) -> String {
        s.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
    }
}
