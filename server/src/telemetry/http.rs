//! HTTP requests in debug mode, per route ("GET /api/photo/{id}/raw"): how many, how long
//! (to the response head and to the last byte), bytes sent, status classes, transfers the
//! browser gave up part-way (a thumbnail scrolled past) and requests it gave up before the
//! answer was ready (the server's work wasted). Errors keep their message; slow requests
//! their full address. Responses carry `Server-Timing: app;dur=…` so the browser's own
//! timings can be split into server time and the rest.

use super::hist::Hist;
use super::{get, stamp};
use axum::body::{Body, Bytes, HttpBody as _};
use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

/// Requests slower than this (to the last byte) are written out one by one.
const SLOW_MS: u64 = 1000;
/// HTTP errors and slow requests written per minute, at most.
const PER_MINUTE: u32 = 60;
const ERROR_BODY: usize = 600;

#[derive(Default)]
pub struct Route {
    /// Time to the last byte (µs; errors are 5xx).
    pub h: Hist,
    /// Time to the response head.
    pub ttfb: Hist,
    bytes: AtomicU64,
    /// 1xx..5xx.
    status: [AtomicU64; 5],
    aborted: AtomicU64,
    cancelled: AtomicU64,
    slowest: Mutex<Vec<Value>>,
}

impl Route {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    pub fn statuses(&self) -> [u64; 5] {
        std::array::from_fn(|i| self.status[i].load(Ordering::Relaxed))
    }
    pub fn aborted(&self) -> u64 {
        self.aborted.load(Ordering::Relaxed)
    }
    pub fn cancelled(&self) -> u64 {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// The five slowest requests since the start.
    pub fn slowest(&self) -> Vec<Value> {
        self.slowest.lock().clone()
    }

    pub fn json(&self) -> Value {
        let mut j = self.h.snapshot().json();
        let ttfb = self.ttfb.snapshot();
        j["ttfb_p50"] = ((ttfb.pct(0.5) as f64 / 100.0).round() / 10.0).into();
        j["ttfb_p95"] = ((ttfb.pct(0.95) as f64 / 100.0).round() / 10.0).into();
        let n = self.h.count().max(1);
        j["mb"] = ((self.bytes() as f64 / 1_048_576.0 * 10.0).round() / 10.0).into();
        j["kb_avg"] = (self.bytes() / n / 1024).into();
        let s = self.statuses();
        if s[3] + s[4] > 0 {
            j["4xx"] = s[3].into();
            j["5xx"] = s[4].into();
        }
        if self.aborted() > 0 {
            j["aborted"] = self.aborted().into();
        }
        if self.cancelled() > 0 {
            j["cancelled"] = self.cancelled().into();
        }
        j
    }
}

#[derive(Default)]
pub struct Http {
    routes: RwLock<HashMap<String, Arc<Route>>>,
    open_streams: AtomicI64,
    streams_opened: AtomicU64,
    stream_bytes: AtomicU64,
    errors: Mutex<VecDeque<Value>>,
    slow: Mutex<VecDeque<Value>>,
    /// The minute the write limit counts in, and lines written in it.
    limit: Mutex<(i64, u32)>,
}

impl Http {
    fn route(&self, key: &str) -> Arc<Route> {
        if let Some(r) = self.routes.read().get(key) {
            return r.clone();
        }
        let mut m = self.routes.write();
        // Unknown paths are counted together (bots, typos): never one entry each.
        let key = if m.len() >= 400 { "(other)" } else { key };
        m.entry(key.to_string()).or_default().clone()
    }

    pub fn routes(&self) -> Vec<(String, Arc<Route>)> {
        let mut v: Vec<_> = self.routes.read().iter().map(|(k, r)| (k.clone(), r.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    pub fn open_streams(&self) -> i64 {
        self.open_streams.load(Ordering::Relaxed)
    }

    /// Event streams opened and bytes sent on them, since the start.
    pub fn stream_totals(&self) -> (u64, u64) {
        (self.streams_opened.load(Ordering::Relaxed), self.stream_bytes.load(Ordering::Relaxed))
    }

    pub fn errors(&self, n: usize) -> Vec<Value> {
        let e = self.errors.lock();
        e.iter().skip(e.len().saturating_sub(n)).cloned().collect()
    }

    pub fn slow(&self, n: usize) -> Vec<Value> {
        let e = self.slow.lock();
        e.iter().skip(e.len().saturating_sub(n)).cloned().collect()
    }

    fn may_write(&self) -> bool {
        let minute = crate::db::now_ms() / 60_000;
        let mut l = self.limit.lock();
        if l.0 != minute {
            *l = (minute, 0);
        }
        l.1 += 1;
        l.1 <= PER_MINUTE
    }
}

fn keep(ring: &Mutex<VecDeque<Value>>, v: Value) {
    let mut r = ring.lock();
    if r.len() >= 200 {
        r.pop_front();
    }
    r.push_back(v);
}

/// The route of a request: its pattern when the router matched one, else what kind of
/// file it is.
fn route_of(req: &Request) -> String {
    let m = req.method().as_str();
    if let Some(p) = req.extensions().get::<MatchedPath>() {
        return format!("{m} {}", p.as_str());
    }
    let path = req.uri().path();
    let kind = if path.starts_with("/assets/") {
        "/assets/*"
    } else if path.starts_with("/fonts/") {
        "/fonts/*"
    } else if path.starts_with("/api/") {
        "/api/* (no such route)"
    } else {
        "/ (app files)"
    };
    format!("{m} {kind}")
}

/// What a request was, kept until its body has gone out.
struct Done {
    route: Arc<Route>,
    key: String,
    uri: String,
    start: Instant,
    ttfb_us: u64,
    status: StatusCode,
    stream: bool,
    bytes: u64,
    /// The body's length, when known up front (hyper stops polling once it has sent it).
    expected: Option<u64>,
    /// The start of an error's body (its message).
    body: Option<Vec<u8>>,
}

impl Done {
    fn finish(self, complete: bool) {
        let Some(t) = get() else { return };
        if self.stream {
            t.http.open_streams.fetch_sub(1, Ordering::Relaxed);
            t.http.stream_bytes.fetch_add(self.bytes, Ordering::Relaxed);
            return;
        }
        let us = self.start.elapsed().as_micros() as u64;
        let code = self.status.as_u16();
        let r = &self.route;
        r.h.add(us, code >= 500);
        r.bytes.fetch_add(self.bytes, Ordering::Relaxed);
        r.status[((code / 100) as usize).clamp(1, 5) - 1].fetch_add(1, Ordering::Relaxed);
        if !complete {
            r.aborted.fetch_add(1, Ordering::Relaxed);
        }
        let ms = us / 1000;
        if ms >= SLOW_MS || code >= 400 {
            let mut v = json!({"route": self.key, "uri": self.uri, "status": code, "ms": ms, "ttfb_ms": self.ttfb_us / 1000, "bytes": self.bytes});
            if !complete {
                v["aborted"] = true.into();
            }
            if let Some(b) = &self.body {
                let text = String::from_utf8_lossy(b);
                let msg = serde_json::from_str::<Value>(&text).ok().and_then(|j| j["error"].as_str().map(str::to_string)).unwrap_or_else(|| text.to_string());
                v["error"] = msg.into();
            }
            stamp(&mut v);
            {
                let mut s = r.slowest.lock();
                if s.len() < 5 || s.iter().any(|x| x["ms"].as_u64().unwrap_or(0) < ms) {
                    s.push(v.clone());
                    s.sort_by_key(|x| std::cmp::Reverse(x["ms"].as_u64().unwrap_or(0)));
                    s.truncate(5);
                }
            }
            // Expected answers are not errors: no login yet, nothing new (304).
            let error = code >= 400 && code != 401;
            if error {
                keep(&t.http.errors, v.clone());
            }
            if ms >= SLOW_MS {
                keep(&t.http.slow, v.clone());
            }
            if (error || ms >= SLOW_MS) && t.http.may_write() {
                v["k"] = (if error { "http_error" } else { "slow_http" }).into();
                t.event(v);
            }
        }
    }
}

/// A response body that counts what goes out and notices when it ends, finished or not.
struct Counted {
    inner: Body,
    done: Option<Done>,
    ended: bool,
}

impl axum::body::HttpBody for Counted {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<http_body::Frame<Bytes>, axum::Error>>> {
        let r = Pin::new(&mut self.inner).poll_frame(cx);
        match &r {
            Poll::Ready(Some(Ok(frame))) => {
                if let (Some(data), Some(d)) = (frame.data_ref(), self.done.as_mut()) {
                    d.bytes += data.len() as u64;
                    if let Some(b) = d.body.as_mut()
                        && b.len() < ERROR_BODY
                    {
                        b.extend_from_slice(&data[..data.len().min(ERROR_BODY - b.len())]);
                    }
                }
            }
            Poll::Ready(None) => self.ended = true,
            _ => {}
        }
        r
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        if let Some(d) = self.done.take() {
            // A body that knows it has nothing left, or of a length announced up front, is
            // not polled to its end.
            let complete = self.ended || self.inner.is_end_stream() || d.expected.is_some_and(|e| d.bytes >= e);
            d.finish(complete);
        }
    }
}

/// A request still waiting for its answer: if the browser gives up first, its future is
/// dropped here, and the work it started (unless cancelled with it) was for nothing.
struct Waiting {
    route: Option<Arc<Route>>,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if let Some(r) = self.route.take() {
            r.cancelled.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The middleware (outermost, so it sees what goes over the wire).
pub async fn middleware(req: Request, next: Next) -> Response {
    let Some(t) = get() else { return next.run(req).await };
    let key = route_of(&req);
    let route = t.http.route(&key);
    let uri = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_default();
    let start = Instant::now();
    let mut waiting = Waiting { route: Some(route.clone()) };
    let mut resp = next.run(req).await;
    waiting.route = None;
    let ttfb = start.elapsed();
    route.ttfb.add(ttfb.as_micros() as u64, false);
    if let Ok(v) = HeaderValue::from_str(&format!("app;dur={:.1}", ttfb.as_secs_f64() * 1000.0)) {
        resp.headers_mut().insert(header::HeaderName::from_static("server-timing"), v);
    }
    let status = resp.status();
    let stream = resp.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|c| c.starts_with("text/event-stream"));
    if stream {
        t.http.open_streams.fetch_add(1, Ordering::Relaxed);
        t.http.streams_opened.fetch_add(1, Ordering::Relaxed);
    }
    let (parts, body) = resp.into_parts();
    let done = Done {
        route,
        key,
        uri,
        start,
        ttfb_us: ttfb.as_micros() as u64,
        status,
        stream,
        bytes: 0,
        expected: body.size_hint().exact(),
        body: (status.as_u16() >= 400).then(Vec::new),
    };
    Response::from_parts(parts, Body::new(Counted { inner: body, done: Some(done), ended: false }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get as route_get;
    use tower::ServiceExt;

    #[tokio::test]
    async fn routes_are_counted_by_pattern() {
        let t = super::super::test_init();
        let api = Router::new()
            .route("/telemetry-test/{id}", route_get(|| async { "hello" }))
            .route("/telemetry-test/{id}/fail", route_get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({"error": "it broke"}))) }));
        let app = Router::new().nest("/api", api).fallback(|| async { "app" }).layer(axum::middleware::from_fn(middleware));
        for id in [1, 2, 3] {
            let resp = app.clone().oneshot(Request::get(format!("/api/telemetry-test/{id}")).body(Body::empty()).unwrap()).await.unwrap();
            assert!(resp.headers().get("server-timing").unwrap().to_str().unwrap().starts_with("app;dur="));
            let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
            assert_eq!(&body[..], b"hello");
        }
        let resp = app.clone().oneshot(Request::get("/api/telemetry-test/9/fail").body(Body::empty()).unwrap()).await.unwrap();
        let _ = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let resp = app.clone().oneshot(Request::get("/somewhere").body(Body::empty()).unwrap()).await.unwrap();
        drop(resp); // never read: an aborted transfer
        let routes: HashMap<String, Arc<Route>> = t.http.routes().into_iter().collect();
        let ok = &routes["GET /api/telemetry-test/{id}"];
        assert_eq!(ok.h.count(), 3);
        assert_eq!(ok.bytes(), 15);
        assert_eq!(ok.statuses()[1], 3);
        let fail = &routes["GET /api/telemetry-test/{id}/fail"];
        assert_eq!(fail.statuses()[4], 1);
        assert!(t.http.errors(50).iter().any(|e| e["error"] == "it broke" && e["uri"] == "/api/telemetry-test/9/fail"));
        let spa = &routes["GET / (app files)"];
        assert!(spa.h.count() >= 1);
        assert!(spa.aborted() >= 1);
    }
}
