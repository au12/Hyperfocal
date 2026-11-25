//! `/api/debug/*` (debug mode only; behind the login like the rest of the API):
//!
//! ```text
//! GET  /api/debug/live              the debug panel's data
//! GET  /api/debug/summary           one page of text (markdown)
//! GET  /api/debug/bundle?days=7     the report: a .tar.gz to download
//! POST /api/debug/client            a browser's report (web/src/lib/telemetry.ts)
//! POST /api/debug/mark              {note, ctx}: marks this moment in the telemetry
//! POST /api/debug/trim              hands memory the allocator holds back to the system
//! ```

use super::{Telemetry, get, report, stamp, system};
use crate::api::{ApiError, ApiResult};
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get as get_route, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/debug/live", get_route(live))
        .route("/debug/summary", get_route(summary))
        .route("/debug/bundle", get_route(bundle))
        .route("/debug/client", post(client))
        .route("/debug/mark", post(mark))
        .route("/debug/trim", post(trim))
}

fn tel() -> ApiResult<&'static Telemetry> {
    get().ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "debug mode is off (set HYPERFOCAL_DEBUG=true on the container)".into()))
}

async fn live() -> ApiResult<Json<Value>> {
    let t = tel()?;
    Ok(Json(tokio::task::spawn_blocking(move || report::live(t)).await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?))
}

async fn summary(State(state): St) -> ApiResult<Response> {
    let t = tel()?;
    let text = tokio::task::spawn_blocking(move || report::summary(t, &state)).await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(([(header::CONTENT_TYPE, "text/markdown; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], text).into_response())
}

#[derive(Deserialize)]
struct BundleQuery {
    days: Option<u32>,
    max_mb: Option<u64>,
}

async fn bundle(State(state): St, Query(q): Query<BundleQuery>) -> ApiResult<Response> {
    let t = tel()?;
    let days = q.days.unwrap_or(7).clamp(1, 3650);
    let max = q.max_mb.unwrap_or(400).clamp(1, 4096) << 20;
    let bytes = tokio::task::spawn_blocking(move || report::bundle(t, &state, days, max))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("making the report: {e}")))?;
    let fmt = time::macros::format_description!("[year][month][day]-[hour][minute]");
    let name = format!("hyperfocal-debug-{}.tar.gz", time::OffsetDateTime::now_utc().format(&fmt).unwrap_or_default());
    Ok((
        [
            (header::CONTENT_TYPE, "application/gzip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        bytes,
    )
        .into_response())
}

/// A browser's report. Sent with `navigator.sendBeacon` when the page is hidden, so the
/// body is read as text whatever its content type says.
async fn client(body: Bytes) -> ApiResult<Json<Value>> {
    let t = tel()?;
    if body.len() > 1 << 20 {
        return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "report too large".into()));
    }
    let mut r: Value = serde_json::from_slice(&body).map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("not a report: {e}")))?;
    let now = crate::db::now_ms();
    t.clients.ingest(&r, now).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    // The browser's clock as `ct`; `t` is the server's.
    if let Some(ct) = r.get("t").cloned() {
        r["ct"] = ct;
    }
    r["t"] = now.into();
    r["k"] = "client".into();
    if let Some(o) = r.as_object_mut() {
        o.remove("ts");
    }
    stamp(&mut r);
    t.store.line(super::store::Kind::Client, &r);
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Mark {
    #[serde(default)]
    note: String,
    #[serde(default)]
    ctx: Value,
    #[serde(default)]
    dev: Option<String>,
}

async fn mark(Json(m): Json<Mark>) -> ApiResult<Json<Value>> {
    let t = tel()?;
    let latest = t.latest();
    let pick = |k: &str| latest.as_ref().map(|s| s[k].clone()).unwrap_or(Value::Null);
    let mut v = json!({
        "k": "mark",
        "note": m.note.chars().take(1000).collect::<String>(),
        "ctx": m.ctx,
        "dev": m.dev,
        // What the server was doing at the latest sample, for context.
        "proc": pick("proc"),
        "work": pick("work"),
        "open_streams": t.http.open_streams(),
    });
    stamp(&mut v);
    {
        let mut marks = t.marks.lock();
        if marks.len() >= 200 {
            marks.pop_front();
        }
        marks.push_back(v.clone());
    }
    tracing::info!("debug: moment marked: {}", v["note"].as_str().unwrap_or(""));
    t.store.line(super::store::Kind::Events, &v);
    Ok(Json(json!({"ok": true, "t": v["t"]})))
}

async fn trim() -> ApiResult<Json<Value>> {
    let t = tel()?;
    let res = tokio::task::spawn_blocking(|| {
        let before = (system::rss_bytes(), system::malloc());
        let started = std::time::Instant::now();
        let released = system::trim();
        let ms = started.elapsed().as_millis() as u64;
        let after = (system::rss_bytes(), system::malloc());
        (before, after, released, ms)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let ((rss0, m0), (rss1, m1), released, ms) = res;
    let mb = |b: u64| (b as f64 / 1_048_576.0 * 10.0).round() / 10.0;
    let v = json!({
        "k": "trim",
        "released": released,
        "ms": ms,
        "rss_before_mb": mb(rss0),
        "rss_after_mb": mb(rss1),
        "freed_mb": mb(rss0.saturating_sub(rss1)),
        "malloc_before": m0,
        "malloc_after": m1,
    });
    tracing::info!("debug: released memory: RSS {} → {} MB in {ms} ms", mb(rss0), mb(rss1));
    t.event(v.clone());
    Ok(Json(v))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
        let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
        let req = req.body(body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty)).unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        (status, axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec())
    }

    /// The files in a .tar.gz, by name.
    fn untar(gz: &[u8]) -> std::collections::HashMap<String, Vec<u8>> {
        use std::io::Read;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(gz).read_to_end(&mut raw).unwrap();
        let mut out = std::collections::HashMap::new();
        let mut at = 0;
        while at + 512 <= raw.len() && raw[at] != 0 {
            let h = &raw[at..at + 512];
            let name = String::from_utf8_lossy(&h[..100]).trim_end_matches('\0').to_string();
            let size = usize::from_str_radix(std::str::from_utf8(&h[124..135]).unwrap(), 8).unwrap();
            out.insert(name, raw[at + 512..at + 512 + size].to_vec());
            at += 512 + size.div_ceil(512) * 512;
        }
        out
    }

    #[tokio::test]
    async fn debug_mode_end_to_end() {
        let t = super::super::test_init();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("photos")).unwrap();
        let mut cfg = crate::config::Config::for_tests(tmp.path());
        cfg.readonly = true;
        let state = crate::build_state(cfg).await.unwrap();
        {
            let c = state.db.get().unwrap();
            c.execute("INSERT INTO folders(id, parent_id, path, name, scanned_at) VALUES(1, NULL, '', 'Library', 1), (2, 1, '2014/Trip', 'Trip', 1)", []).unwrap();
            for i in 1..=5 {
                let rec = crate::model::PhotoRec {
                    folder_id: 2,
                    name: format!("IMG_{i}.CR2"),
                    stem: format!("IMG_{i}"),
                    ext: "cr2".into(),
                    kind: 1,
                    model: Some("Canon EOS 5D Mark II".into()),
                    meta_err: (i <= 3).then(|| format!("cannot read /photos/2014/Trip/IMG_{i}.CR2: truncated at byte {}", i * 1000)),
                    ..Default::default()
                };
                crate::repo::upsert(&c, &rec).unwrap();
            }
            crate::repo::refresh_folder(&c, 2).unwrap();
        }
        let app = crate::api::router(state.clone());

        let (st, body) = call(&app, "GET", "/api/status", None).await;
        assert_eq!(st, StatusCode::OK);
        let status: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(status["debug"]["enabled"], true);

        let report = json!({
            "v": 1, "dev": "test-device", "page": "p1", "t": 1,
            "env": {"ua": "Test", "mobile": false, "cores": 8, "dpr": 2},
            "timings": {"viewer.show.raw": {"n": 1, "sum_us": 250_000, "max_us": 250_000, "b": {"80": 1}}},
            "errors": [{"kind": "js", "msg": "boom", "n": 1}],
        });
        let (st, body) = call(&app, "POST", "/api/debug/client", Some(report)).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let (st, _) = call(&app, "POST", "/api/debug/client", Some(json!({"no": "device"}))).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _) = call(&app, "POST", "/api/debug/mark", Some(json!({"note": "the loupe stuttered", "ctx": {"view": "loupe"}}))).await;
        assert_eq!(st, StatusCode::OK);
        let (st, body) = call(&app, "POST", "/api/debug/trim", None).await;
        assert_eq!(st, StatusCode::OK);
        let trim: Value = serde_json::from_slice(&body).unwrap();
        assert!(trim["rss_after_mb"].as_f64().unwrap() > 0.0);

        // Two samples: the second has rates.
        let mut prev = super::super::sampler::Prev::default();
        let rt = tokio::runtime::Handle::current();
        let _ = super::super::sampler::sample(t, &state, Some(&rt), &mut prev);
        let s = super::super::sampler::sample(t, &state, Some(&rt), &mut prev);
        for k in ["proc", "threads", "caches", "work", "db", "disk", "http", "ops", "tokio"] {
            assert!(!s[k].is_null(), "{k} missing: {s}");
        }
        assert!(s["proc"]["rss_mb"].as_f64().unwrap() > 0.0);
        assert!(s["caches"]["developed"]["cap_mb"].as_f64().unwrap() > 0.0);
        t.samples.lock().push_back(std::sync::Arc::new(s));

        let (st, body) = call(&app, "GET", "/api/debug/live", None).await;
        assert_eq!(st, StatusCode::OK);
        let live: Value = serde_json::from_slice(&body).unwrap();
        assert!(live["clients"].as_array().unwrap().iter().any(|c| c["dev"] == "test-device"));
        assert!(live["marks"].as_array().unwrap().iter().any(|m| m["note"] == "the loupe stuttered"));
        assert!(live["latest"]["proc"].is_object());

        let (st, body) = call(&app, "GET", "/api/debug/summary", None).await;
        assert_eq!(st, StatusCode::OK);
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("# Hyperfocal debug summary"), "{text}");
        assert!(text.contains("the loupe stuttered"));

        let (st, body) = call(&app, "GET", "/api/debug/bundle?days=1", None).await;
        assert_eq!(st, StatusCode::OK);
        let files = untar(&body);
        for f in ["hyperfocal-debug/README.md", "hyperfocal-debug/summary.md", "hyperfocal-debug/report.json"] {
            assert!(files.contains_key(f), "{f} not in {:?}", files.keys().collect::<Vec<_>>());
        }
        assert!(files.keys().any(|k| k.starts_with("hyperfocal-debug/debug/client-")), "{:?}", files.keys().collect::<Vec<_>>());
        let rep: Value = serde_json::from_slice(&files["hyperfocal-debug/report.json"]).unwrap();
        let census = &rep["census"];
        assert_eq!(census["library"]["photos"], 5);
        assert_eq!(census["library"]["by_camera"][0]["model"], "Canon EOS 5D Mark II");
        let meta = &census["errors"][0];
        assert_eq!(meta["source"], "metadata (scan)");
        assert_eq!(meta["total"], 3);
        // The same failure on three files: one group.
        assert_eq!(meta["groups"][0]["n"], 3);
        assert_eq!(meta["groups"][0]["error"], "cannot read <path>: truncated at byte #");
        assert!(rep["config"]["password"] == false);
    }
}
