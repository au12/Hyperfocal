#![recursion_limit = "256"]
//! Hyperfocal: fast, self-hosted RAW photo culling and library management.

mod api;
mod config;
mod db;
mod files;
mod images;
mod meta;
mod model;
mod ops;
mod repo;
mod scan;
mod state;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::{HeaderValue, Request, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use parking_lot::{Condvar, Mutex, RwLock};
use state::{AppState, Progress};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};

fn library_writable(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: valid C string.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

fn load_session_key(dir: &std::path::Path) -> [u8; 32] {
    let p = dir.join("session.key");
    if let Ok(b) = std::fs::read(&p) {
        if b.len() == 32 {
            let mut k = [0u8; 32];
            k.copy_from_slice(&b);
            return k;
        }
    }
    let k: [u8; 32] = rand::random();
    let _ = std::fs::write(&p, k);
    k
}

async fn static_cache(req: Request<axum::body::Body>, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let mut resp = next.run(req).await;
    let v = if path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    if !resp.headers().contains_key(header::CACHE_CONTROL) {
        resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(v));
    }
    resp
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "hyperfocal=info,tower_http=warn".into()))
        .init();
    let cfg = config::Config::from_env();
    std::fs::create_dir_all(&cfg.config_dir).with_context(|| format!("creating {}", cfg.config_dir.display()))?;
    std::fs::create_dir_all(&cfg.cache_dir).with_context(|| format!("creating {}", cfg.cache_dir.display()))?;
    tracing::info!("Hyperfocal {} — library {} (config {})", env!("CARGO_PKG_VERSION"), cfg.photos_root.display(), cfg.config_dir.display());

    let readonly = cfg.readonly || !library_writable(&cfg.photos_root);
    if readonly {
        tracing::warn!("read-only mode: the photo library will not be written to");
    }
    let pool = db::open(&cfg.config_dir.join("hyperfocal.db"))?;
    let exiftool = meta::exiftool::ExifTool::new(&cfg.exiftool, &cfg.config_dir)?;
    let images = images::Images::new(&cfg)?;
    let (events, _) = tokio::sync::broadcast::channel(512);
    let session_key = load_session_key(&cfg.config_dir);
    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        db: pool,
        events,
        exiftool: tokio::sync::Mutex::new(exiftool),
        write_lock: tokio::sync::Mutex::new(()),
        progress: RwLock::new(Progress { phase: "starting".into(), library_ok: true, ..Default::default() }),
        scan_tx: Mutex::new(None),
        busy: Mutex::new(HashSet::new()),
        busy_cv: Condvar::new(),
        images,
        readonly: AtomicBool::new(readonly),
        session_key,
    });

    if !readonly {
        match state.exiftool.lock().await.version().await {
            Ok(v) => tracing::info!("exiftool {v}"),
            Err(e) => {
                tracing::error!("exiftool is not available ({e:#}); switching to read-only mode");
                state.readonly.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    scan::spawn(state.clone());
    images::Images::spawn_workers(state.clone());

    let web = cfg.web_dir.clone();
    let spa = ServeDir::new(&web)
        .precompressed_br()
        .precompressed_gzip()
        .fallback(ServeFile::new(web.join("index.html")));
    let app = Router::new()
        .merge(api::router(state.clone()))
        .fallback_service(spa)
        .layer(middleware::from_fn(static_cache))
        .layer(CompressionLayer::new());

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], cfg.port));
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}
