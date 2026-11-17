#![recursion_limit = "256"]
//! Hyperfocal: fast, self-hosted RAW photo culling and library management.

mod api;
mod config;
mod db;
mod files;
mod images;
mod inspect;
mod meta;
mod model;
mod native;
mod ops;
mod repo;
mod scan;
mod state;
#[cfg(test)]
mod safety_tests;

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
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
            let mut k = [0u8; 32];
            k.copy_from_slice(&b);
            return k;
        }
    }
    let k: [u8; 32] = rand::random();
    // Owner-only: anyone who can read this key can mint login cookies.
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&p) {
        Ok(mut f) => {
            let _ = f.write_all(&k);
        }
        Err(e) => tracing::warn!("could not persist {}: {e}", p.display()),
    }
    k
}

/// `hyperfocal healthcheck`: exit 0 when the local server answers. Used by the
/// Docker HEALTHCHECK so no curl is needed in the image.
fn healthcheck() -> std::process::ExitCode {
    use std::io::{Read, Write};
    let port: u16 = std::env::var("HYPERFOCAL_PORT").ok().and_then(|p| p.trim().parse().ok()).unwrap_or(8080);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let ok = (|| -> std::io::Result<bool> {
        let mut s = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        s.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut buf = [0u8; 32];
        let n = s.read(&mut buf)?;
        Ok(buf[..n].starts_with(b"HTTP/1.1 200") || buf[..n].starts_with(b"HTTP/1.0 200"))
    })()
    .unwrap_or(false);
    if ok { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE }
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

fn main() -> std::process::ExitCode {
    // args_os: the native helper receives file names, which need not be UTF-8.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match args.get(1).and_then(|a| a.to_str()) {
        Some("native") => return native::helper::main(&args[2..]),
        Some("inspect") => return inspect::main(&args[2..]),
        Some("healthcheck") => return healthcheck(),
        Some("--version" | "-V") => {
            println!("hyperfocal {}", env!("CARGO_PKG_VERSION"));
            return std::process::ExitCode::SUCCESS;
        }
        _ => {}
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    match rt.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hyperfocal: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Opens the index and prepares everything the server and the scanner share.
/// Falls back to read-only when the library is not writable or ExifTool is missing.
pub(crate) async fn build_state(cfg: config::Config) -> Result<Arc<AppState>> {
    std::fs::create_dir_all(&cfg.config_dir).with_context(|| format!("creating {}", cfg.config_dir.display()))?;
    std::fs::create_dir_all(&cfg.cache_dir).with_context(|| format!("creating {}", cfg.cache_dir.display()))?;
    let readonly = cfg.readonly || !library_writable(&cfg.photos_root);
    let pool = db::open(&cfg.config_dir.join("hyperfocal.db"))?;
    let exiftool = meta::exiftool::ExifTool::new(&cfg.exiftool, &cfg.config_dir)?;
    let images = images::Images::new(&cfg)?;
    native::set_prefer_libraw(cfg.raw_decoder_libraw);
    let decoders = tokio::task::spawn_blocking(native::Decoders::detect).await?;
    let (events, _) = tokio::sync::broadcast::channel(512);
    let session_key = load_session_key(&cfg.config_dir);
    let state = Arc::new(AppState {
        cfg,
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
        decoders,
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
    Ok(state)
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        // Plain text in `docker logs`; colours only on a terminal.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "hyperfocal=info,tower_http=warn".into()))
        .init();
    let cfg = config::Config::from_env();
    tracing::info!("Hyperfocal {} — library {} (config {})", env!("CARGO_PKG_VERSION"), cfg.photos_root.display(), cfg.config_dir.display());
    let state = build_state(cfg.clone()).await?;
    if state.is_readonly() {
        tracing::warn!("read-only mode: the photo library will not be written to");
    }
    let d = &state.decoders;
    tracing::info!(
        "decoders: FlashRaw {}{}, LibRaw {}, libheif {}",
        d.flashraw,
        if d.raw == "libraw" { " (not used: HYPERFOCAL_RAW_DECODER=libraw)" } else { "" },
        d.libraw.as_deref().unwrap_or("unavailable"),
        d.libheif.as_deref().unwrap_or("unavailable (no HEIC support)"),
    );

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
