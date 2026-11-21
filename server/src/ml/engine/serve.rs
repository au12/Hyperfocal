//! `hyperfocal ml-engine`: the engine process. Loads the models, then answers on
//! 127.0.0.1 (a port of its choosing, announced on stdout) until the server that started
//! it goes away (its stdin closes).
//!
//! ```text
//!     GET  /health        200 "ok"
//!     GET  /v1/info       device, models (with content ids) and limits
//!     POST /v1/analyze    body: an upright JPEG or PNG; query: faces=0, animals=0 to skip
//!                         either. Answers the faces and animals found.
//!     POST /v1/embed      body: an image, or several as application/x-hfml-batch (each one
//!                         a 4-byte little-endian length, then its bytes). Answers
//!                         {"embs": [...]}: a SigLIP 2 embedding (base64 float16) per image.
//!     POST /v1/text       body: {"texts": [...]}. Answers {"embs": [...]} in the same space.
//!     POST /v1/eyes       body: close-ups of faces (one image, or a batch as for /v1/embed).
//!                         Answers {"eyes": [...]}: per close-up {"closed": 0..1, ...} or null.
//! ```
//!
//! Every /v1/ request carries "Authorization: Bearer <token>" (the server's, from the
//! engine configuration).
//!
//! Also: `hyperfocal ml-engine probe` (what GPU the driver sees, as JSON) and
//! `hyperfocal ml-engine selftest <dir>` (the image build's check that ONNX Runtime loads
//! and the bundled eye models give the right answers on two test portraits).

use super::{AnalyzeError, Engine, EngineConfig, decode};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

pub const READY: &str = "HYPERFOCAL-ML-READY";
pub const FAILED: &str = "HYPERFOCAL-ML-FAILED";
const MAX_BODY: usize = 64 << 20;

pub fn main(args: &[std::ffi::OsString]) -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(false)
        // ONNX Runtime's own warnings are about its telemetry and the like; real
        // problems come back as errors.
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("HYPERFOCAL_ML_LOG").unwrap_or_else(|_| "info,ort=error".into()))
        .init();
    match args.first().and_then(|a| a.to_str()) {
        Some("probe") => {
            println!("{}", serde_json::to_string(&super::probe::probe()).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Some("selftest") => selftest(args.get(1).map(std::path::PathBuf::from)),
        _ => serve(),
    }
}

fn fail(msg: &str) -> ExitCode {
    tracing::error!("AI engine: {msg}");
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{FAILED} {}", msg.replace('\n', " "));
    let _ = out.flush();
    ExitCode::from(2)
}

fn serve() -> ExitCode {
    let cfg: EngineConfig = match std::env::var("HYPERFOCAL_ML_ENGINE").map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
        Ok(c) => c,
        Err(e) => return fail(&format!("no engine configuration (HYPERFOCAL_ML_ENGINE): {e}")),
    };
    let engine = match Engine::load(cfg) {
        Ok(e) => Arc::new(e),
        Err(e) => return fail(&format!("{e:#}")),
    };
    // The server keeps our stdin open: when it closes (the server stopped or crashed),
    // the engine stops too.
    std::thread::spawn(|| {
        let mut sink = [0u8; 64];
        let mut stdin = std::io::stdin();
        while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
        std::process::exit(0);
    });
    let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(r) => r,
        Err(e) => return fail(&format!("starting the runtime: {e}")),
    };
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(l) => l,
            Err(e) => return fail(&format!("binding 127.0.0.1: {e}")),
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let app = router(engine);
        {
            let mut out = std::io::stdout();
            let _ = writeln!(out, "{READY} {port}");
            let _ = out.flush();
        }
        match axum::serve(listener, app).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&format!("serving: {e}")),
        }
    })
}

pub fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/v1/info", get(info))
        .route("/v1/analyze", post(analyze))
        .route("/v1/embed", post(embed))
        .route("/v1/text", post(text))
        .route("/v1/eyes", post(eyes))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(engine)
}

type St = State<Arc<Engine>>;

fn answer(code: StatusCode, v: Value) -> Response {
    (code, [(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

fn error(code: StatusCode, msg: impl Into<String>) -> Response {
    answer(code, json!({ "error": msg.into() }))
}

fn authorized(engine: &Engine, h: &HeaderMap) -> bool {
    use hmac::Mac;
    let got = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    let want = format!("Bearer {}", engine.cfg.token);
    // Constant time: HMAC both with a fixed key and compare the tags.
    let tag = |s: &str| {
        let mut m = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"hyperfocal-ml").expect("hmac key");
        m.update(s.as_bytes());
        m.finalize().into_bytes()
    };
    !engine.cfg.token.is_empty() && tag(got) == tag(&want)
}

async fn info(State(e): St, h: HeaderMap) -> Response {
    if !authorized(&e, &h) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    answer(StatusCode::OK, e.info())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, Response> {
    tokio::task::spawn_blocking(f).await.map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, format!("the engine failed: {e}")))
}

async fn analyze(State(e): St, h: HeaderMap, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    if !authorized(&e, &h) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if e.faces.is_none() {
        return error(StatusCode::NOT_IMPLEMENTED, "face and pet recognition is off (HYPERFOCAL_ML_FACES=false)");
    }
    if body.is_empty() {
        return error(StatusCode::BAD_REQUEST, "send the image as the request body (at most 64 MB)");
    }
    let flag = |k: &str| q.get(k).is_none_or(|v| !matches!(v.as_str(), "0" | "false" | "no"));
    let num = |k: &str| q.get(k).and_then(|v| v.parse::<f32>().ok());
    let (faces, animals, ft, at) = (flag("faces"), flag("animals"), num("face_threshold"), num("animal_threshold"));
    let eng = e.clone();
    match blocking(move || eng.analyze(&body, faces, animals, ft, at)).await {
        Ok(Ok(v)) => answer(StatusCode::OK, v),
        Ok(Err(AnalyzeError::Image(m))) => error(StatusCode::UNPROCESSABLE_ENTITY, m),
        Ok(Err(AnalyzeError::Model(err))) => {
            tracing::error!("AI engine: analysis failed: {err:#}");
            error(StatusCode::INTERNAL_SERVER_ERROR, format!("analysis failed: {err:#}"))
        }
        Err(r) => r,
    }
}

/// The images of an application/x-hfml-batch body.
pub fn split_batch(data: &[u8]) -> Result<Vec<&[u8]>, String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        if i + 4 > data.len() {
            return Err("a truncated batch".into());
        }
        let n = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if n == 0 || i + n > data.len() {
            return Err("a truncated batch".into());
        }
        out.push(&data[i..i + n]);
        i += n;
    }
    Ok(out)
}

/// The images of a request (one, or a batch), or what is wrong with it.
fn images<'a>(h: &HeaderMap, body: &'a [u8]) -> Result<Vec<&'a [u8]>, String> {
    let batch = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|c| c.starts_with("application/x-hfml-batch"));
    let list = if batch { split_batch(body)? } else { vec![body] };
    if list.is_empty() || list.len() > 64 || list.iter().any(|d| d.is_empty()) {
        return Err("1 to 64 images a request".into());
    }
    Ok(list)
}

async fn embed(State(e): St, h: HeaderMap, body: Bytes) -> Response {
    if !authorized(&e, &h) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if e.search.is_none() {
        return error(StatusCode::NOT_IMPLEMENTED, "search by content is off (HYPERFOCAL_ML_SEARCH=false)");
    }
    if let Err(m) = images(&h, &body) {
        return error(StatusCode::BAD_REQUEST, m);
    }
    let eng = e.clone();
    let t = Instant::now();
    let res = blocking(move || -> anyhow::Result<Vec<Option<String>>> {
        let list = images(&h, &body).map_err(|m| anyhow::anyhow!(m))?;
        let decoded: Vec<Option<crate::images::jpeg::Rgb>> = list.iter().map(|d| decode(d).ok()).collect();
        let ok: Vec<&crate::images::jpeg::Rgb> = decoded.iter().flatten().collect();
        let mut out: Vec<Option<String>> = vec![None; decoded.len()];
        if !ok.is_empty() {
            let _g = eng.gate.enter();
            let embs = eng.search.as_ref().expect("search on").embed_images(&ok)?;
            let mut it = embs.into_iter();
            for (slot, d) in out.iter_mut().zip(&decoded) {
                if d.is_some() {
                    *slot = it.next().map(|v| super::b64_f16(&v));
                }
            }
        }
        Ok(out)
    })
    .await;
    match res {
        Ok(Ok(embs)) => answer(StatusCode::OK, json!({ "embs": embs, "ms": super::round_to(t.elapsed().as_secs_f64() * 1000.0, 1) })),
        Ok(Err(err)) => {
            tracing::error!("AI engine: embedding failed: {err:#}");
            error(StatusCode::INTERNAL_SERVER_ERROR, format!("embedding failed: {err:#}"))
        }
        Err(r) => r,
    }
}

async fn text(State(e): St, h: HeaderMap, body: Bytes) -> Response {
    if !authorized(&e, &h) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if e.search.is_none() {
        return error(StatusCode::NOT_IMPLEMENTED, "search by content is off (HYPERFOCAL_ML_SEARCH=false)");
    }
    let texts: Vec<String> = match serde_json::from_slice::<Value>(&body).ok().and_then(|v| v.get("texts").cloned()).and_then(|t| serde_json::from_value::<Vec<String>>(t).ok()) {
        Some(t) if !t.is_empty() && t.len() <= 64 => t,
        _ => return error(StatusCode::BAD_REQUEST, "texts: 1 to 64 strings"),
    };
    let eng = e.clone();
    let t = Instant::now();
    match blocking(move || {
        let _g = eng.gate.enter();
        eng.search.as_ref().expect("search on").embed_texts(&eng.rt, &texts)
    })
    .await
    {
        Ok(Ok(embs)) => {
            let embs: Vec<String> = embs.iter().map(|v| super::b64_f16(v)).collect();
            answer(StatusCode::OK, json!({ "embs": embs, "ms": super::round_to(t.elapsed().as_secs_f64() * 1000.0, 1) }))
        }
        Ok(Err(err)) => {
            tracing::error!("AI engine: text embedding failed: {err:#}");
            error(StatusCode::INTERNAL_SERVER_ERROR, format!("embedding failed: {err:#}"))
        }
        Err(r) => r,
    }
}

async fn eyes(State(e): St, h: HeaderMap, body: Bytes) -> Response {
    if !authorized(&e, &h) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if e.eyes.is_none() {
        return error(StatusCode::NOT_IMPLEMENTED, "eye checks are off (HYPERFOCAL_ML_EYES=false)");
    }
    if let Err(m) = images(&h, &body) {
        return error(StatusCode::BAD_REQUEST, m);
    }
    let eng = e.clone();
    let t = Instant::now();
    let res = blocking(move || -> anyhow::Result<Vec<Value>> {
        let list = images(&h, &body).map_err(|m| anyhow::anyhow!(m))?;
        let decoded: Vec<Option<crate::images::jpeg::Rgb>> = list.iter().map(|d| decode(d).ok().filter(|i| i.w.min(i.h) >= 32)).collect();
        let _g = eng.gate.enter();
        let checker = eng.eyes.as_ref().expect("eyes on");
        decoded.iter().map(|d| Ok(match d { Some(img) => checker.check(img)?.json(), None => Value::Null })).collect()
    })
    .await;
    match res {
        Ok(Ok(list)) => answer(StatusCode::OK, json!({ "eyes": list, "ms": super::round_to(t.elapsed().as_secs_f64() * 1000.0, 1) })),
        Ok(Err(err)) => {
            tracing::error!("AI engine: eye check failed: {err:#}");
            error(StatusCode::INTERNAL_SERVER_ERROR, format!("eye check failed: {err:#}"))
        }
        Err(r) => r,
    }
}

/// The image build's check: ONNX Runtime loads in the runtime image and the bundled eye
/// models tell an open pair of eyes from a closed one.
fn selftest(dir: Option<std::path::PathBuf>) -> ExitCode {
    let run = || -> anyhow::Result<()> {
        let dir = dir.ok_or_else(|| anyhow::anyhow!("usage: hyperfocal ml-engine selftest <folder with face-open.jpg and face-closed.jpg>"))?;
        let ort_lib = super::super::ort_library().ok_or_else(|| anyhow::anyhow!("ONNX Runtime is not in the image (HYPERFOCAL_ORT_LIB)"))?;
        let vendored = super::super::vendored_dir();
        let file = |name: &str| super::ModelFile { path: vendored.join(name), id: String::new() };
        let cfg = EngineConfig {
            ort_lib,
            device: super::Device::Cpu,
            gpu: 0,
            gpu_mem_mb: 0,
            threads: 1,
            workers: 1,
            token: String::new(),
            faces: None,
            search: None,
            eyes: Some(super::EyeModels { landmarks: file("face-landmarks.onnx"), blendshapes: file("face-blendshapes.onnx") }),
            face_scales: vec![],
            face_threshold: 0.7,
            animal_threshold: 0.35,
            face_norm_low: 12.0,
            face_norm_high: 24.0,
        };
        let e = Engine::load(cfg)?;
        let check = |name: &str| -> anyhow::Result<super::eyes::EyeCheck> {
            let data = std::fs::read(dir.join(name))?;
            e.eyes.as_ref().expect("eyes").check(&decode(&data)?)
        };
        let open = check("face-open.jpg")?;
        let shut = check("face-closed.jpg")?;
        println!("eyes open: closed {:.3} (p {:.3}); eyes shut: closed {:.3} (p {:.3})", open.closed, open.p, shut.closed, shut.p);
        if !(open.p > 0.9 && open.closed < 0.2 && shut.p > 0.9 && shut.closed > 0.6) {
            anyhow::bail!("the eye check gives wrong answers");
        }
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("selftest: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_batch;

    #[test]
    fn batches_split_into_their_images() {
        let (a, b) = (b"\xff\xd8one".as_slice(), b"\xff\xd8second".as_slice());
        let mut body = Vec::new();
        for x in [a, b] {
            body.extend_from_slice(&(x.len() as u32).to_le_bytes());
            body.extend_from_slice(x);
        }
        assert_eq!(split_batch(&body).unwrap(), vec![a, b]);
        assert!(split_batch(&body[..body.len() - 2]).is_err());
        assert!(split_batch(&[5, 0]).is_err());
    }
}
