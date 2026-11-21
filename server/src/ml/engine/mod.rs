//! The AI engine: face, dog and cat detection and recognition, search by content and eye
//! checks, on ONNX Runtime. It runs in its own process, `hyperfocal ml-engine`, started
//! and watched by the server (see `super::supervisor`), and answers on 127.0.0.1 with the
//! same HTTP API the separate `hyperfocal-ml` container had (`serve`), so a crash in ONNX
//! Runtime or the GPU driver costs the analysis a restart, never the server.
//!
//! Faces:  YuNet (OpenCV Zoo, MIT) at three scales, then alignment on its five landmarks
//!         and an AuraFace (Apache-2.0) embedding from the full-resolution pixels.
//! Pets:   RF-DETR (Roboflow, Apache-2.0) for dogs and cats, then a pet re-identification
//!         embedding (open-noodle pet-recognition, Apache-2.0) of each animal.
//! Search: SigLIP 2 (Google, Apache-2.0) image and text embeddings in one space.
//! Eyes:   MediaPipe Face Landmarker (Google, Apache-2.0) on close-ups of known faces.
//!
//! This is a port of the Python service it replaced, with the same preprocessing
//! (`imgproc` reproduces OpenCV's arithmetic) and the same ONNX Runtime version, so the
//! embeddings, scores and calibrated thresholds carry over: photos analysed by the old
//! container are not analysed again.

pub mod eyes;
pub mod faces;
pub mod imgproc;
pub mod pets;
pub mod probe;
pub mod search;
pub mod serve;
#[cfg(test)]
mod tests;

use crate::images::jpeg::Rgb;
use anyhow::{Context, Result, anyhow, bail};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub const ENGINE_VERSION: &str = "2.0.0";

/// What the server tells the engine process (JSON in `HYPERFOCAL_ML_ENGINE`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// ONNX Runtime's shared library.
    pub ort_lib: PathBuf,
    pub device: Device,
    pub gpu: i32,
    /// Cap on the GPU memory ONNX Runtime takes (MB, 0 = none).
    pub gpu_mem_mb: u64,
    /// Threads per model on the CPU (0 = ONNX Runtime's default: the physical cores).
    pub threads: usize,
    /// Requests that run models at once.
    pub workers: usize,
    /// Required on every /v1/ request: "Authorization: Bearer <token>".
    pub token: String,
    pub faces: Option<FaceModels>,
    pub search: Option<SearchModels>,
    pub eyes: Option<EyeModels>,
    pub face_scales: Vec<usize>,
    pub face_threshold: f32,
    pub animal_threshold: f32,
    pub face_norm_low: f64,
    pub face_norm_high: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Cpu,
    Cuda,
}

impl Device {
    pub fn as_str(self) -> &'static str {
        match self {
            Device::Cpu => "cpu",
            Device::Cuda => "cuda",
        }
    }
}

/// A model file and its content id (the first 12 hex digits of its sha256: the id the
/// stored embeddings and analyses are tagged with).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelFile {
    pub path: PathBuf,
    pub id: String,
}

impl ModelFile {
    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceModels {
    pub detector: ModelFile,
    pub embedder: ModelFile,
    pub animal_detector: ModelFile,
    pub pet_embedder: ModelFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchModels {
    pub vision: ModelFile,
    pub text: ModelFile,
    pub tokenizer: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EyeModels {
    pub landmarks: ModelFile,
    pub blendshapes: ModelFile,
}

/// Makes ONNX Runtime sessions the same way for every model.
pub struct Runtime {
    env: &'static ort::environment::Environment,
    pub device: Device,
    cfg: EngineConfig,
}

/// ONNX Runtime's environment: one per process (the first library given is the one).
fn environment(lib: &Path) -> Result<&'static ort::environment::Environment> {
    static ENV: std::sync::OnceLock<ort::environment::Environment> = std::sync::OnceLock::new();
    static INIT: Mutex<()> = Mutex::new(());
    let _g = INIT.lock();
    if let Some(e) = ENV.get() {
        return Ok(e);
    }
    let builder = ort::init_from(lib).map_err(|e| anyhow!("loading ONNX Runtime ({}): {e}", lib.display()))?;
    let env = builder.with_name("hyperfocal").with_telemetry(false).build().map_err(oe)?;
    Ok(ENV.get_or_init(|| env))
}

/// An ort error as an anyhow error (ort's carry raw pointers, so they are not `Send`).
pub(crate) fn oe<R>(e: ort::Error<R>) -> anyhow::Error {
    anyhow!("{e}")
}

impl Runtime {
    pub fn new(cfg: &EngineConfig) -> Result<Runtime> {
        Ok(Runtime { env: environment(&cfg.ort_lib)?, device: cfg.device, cfg: cfg.clone() })
    }

    pub fn session(&self, path: &Path) -> Result<Session> {
        if !path.exists() {
            bail!("model not found: {}", path.display());
        }
        let mut b = Session::builder(self.env)
            .map_err(oe)?
            .with_optimization_level(GraphOptimizationLevel::All)
            .map_err(oe)?
            .with_inter_threads(1)
            .map_err(oe)?
            .with_log_level(ort::logging::LogLevel::Error)
            .map_err(oe)?;
        if self.cfg.threads > 0 {
            b = b.with_intra_threads(self.cfg.threads).map_err(oe)?;
        }
        if self.device == Device::Cuda {
            let mut cuda = ort::ep::CUDA::default()
                .with_device_id(self.cfg.gpu)
                // Grow the arena by what a run needs, not by doubling: a 4 GB card is
                // often shared with other apps.
                .with_arena_extend_strategy(ort::ep::ArenaExtendStrategy::SameAsRequested)
                // The face detector sees a few image shapes: a heuristic algorithm choice
                // avoids a slow cuDNN benchmark for each new one.
                .with_conv_algorithm_search(ort::ep::cuda::ConvAlgorithmSearch::Heuristic);
            if self.cfg.gpu_mem_mb > 0 {
                cuda = cuda.with_memory_limit((self.cfg.gpu_mem_mb as usize) << 20);
            }
            b = b.with_execution_providers([cuda.build().error_on_failure()]).map_err(oe)?;
        }
        b.commit_from_file(path).map_err(|e| anyhow!("loading {}: {e}", path.display()))
    }
}

/// At most this many requests run models at once.
pub struct Gate {
    free: Mutex<usize>,
    cv: Condvar,
}

impl Gate {
    pub fn new(n: usize) -> Gate {
        Gate { free: Mutex::new(n.max(1)), cv: Condvar::new() }
    }

    pub fn enter(&self) -> GateGuard<'_> {
        let mut f = self.free.lock();
        while *f == 0 {
            self.cv.wait(&mut f);
        }
        *f -= 1;
        GateGuard(self)
    }
}

pub struct GateGuard<'a>(&'a Gate);

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        *self.0.free.lock() += 1;
        self.0.cv.notify_one();
    }
}

pub struct Engine {
    pub cfg: EngineConfig,
    pub device: Device,
    /// Kept for the life of the sessions (and for the text model, loaded on demand).
    pub rt: Runtime,
    pub faces: Option<faces::Faces>,
    pub pets: Option<pets::Pets>,
    pub search: Option<search::Search>,
    pub eyes: Option<eyes::Eyes>,
    pub gate: Gate,
    pub started: Instant,
    pub analyzed: AtomicU64,
    pub busy_us: AtomicU64,
}

impl Engine {
    pub fn load(cfg: EngineConfig) -> Result<Engine> {
        let t = Instant::now();
        let rt = Runtime::new(&cfg)?;
        let (faces, pets) = match &cfg.faces {
            Some(m) => (
                Some(faces::Faces::new(&rt, m, &cfg).context("face models")?),
                Some(pets::Pets::new(&rt, m, cfg.animal_threshold).context("pet models")?),
            ),
            None => (None, None),
        };
        let search = match &cfg.search {
            Some(m) => Some(search::Search::new(rt.session(&m.vision.path)?, m).context("search models")?),
            None => None,
        };
        let eyes = match &cfg.eyes {
            Some(m) => Some(eyes::Eyes::new(&rt, m).context("eye models")?),
            None => None,
        };
        let workers = if cfg.workers > 0 { cfg.workers } else if cfg.device == Device::Cuda { 2 } else { 1 };
        let e = Engine {
            device: cfg.device,
            rt,
            cfg,
            faces,
            pets,
            search,
            eyes,
            gate: Gate::new(workers),
            started: Instant::now(),
            analyzed: AtomicU64::new(0),
            busy_us: AtomicU64::new(0),
        };
        e.warm()?;
        tracing::info!("AI engine: models ready on the {} in {:.1}s", if e.device == Device::Cuda { "GPU" } else { "CPU" }, t.elapsed().as_secs_f64());
        Ok(e)
    }

    pub fn workers(&self) -> usize {
        if self.cfg.workers > 0 { self.cfg.workers } else if self.device == Device::Cuda { 2 } else { 1 }
    }

    /// Runs every model once, so the first photo is not slow (and a broken model or GPU
    /// fails at start-up, not on the first request).
    fn warm(&self) -> Result<()> {
        let mut img = Rgb { w: 640, h: 480, px: vec![127u8; 640 * 480 * 3] };
        for y in 0..480usize {
            for x in 0..640usize {
                let (dx, dy) = (x as f64 - 320.0, y as f64 - 240.0);
                if dx * dx + dy * dy <= 120.0 * 120.0 {
                    img.px[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3].copy_from_slice(&[150, 160, 180]);
                }
            }
        }
        if self.faces.is_some() {
            self.analyze_rgb(&img, true, true, None, None)?;
            if let Some(f) = &self.faces {
                f.embed(&[vec![0u8; 112 * 112 * 3]])?;
            }
            if let Some(p) = &self.pets {
                p.embed(&[(vec![0u8; 224 * 224 * 3], 224, 224)])?;
            }
        }
        if let Some(s) = &self.search {
            s.embed_images(&[&img])?;
        }
        if let Some(e) = &self.eyes {
            let small = imgproc::resize(&img.px, 640, 480, 320, 320, imgproc::Interp::Linear);
            e.check(&Rgb { w: 320, h: 320, px: small })?;
        }
        Ok(())
    }

    pub fn info(&self) -> serde_json::Value {
        let mut models = serde_json::Map::new();
        if let (Some(m), Some(p)) = (&self.cfg.faces, &self.pets) {
            models.insert("face_detector".into(), serde_json::json!({ "file": m.detector.name(), "id": m.detector.id }));
            models.insert("face_embedder".into(), serde_json::json!({ "file": m.embedder.name(), "id": m.embedder.id, "dim": 512 }));
            models.insert("animal_detector".into(), serde_json::json!({ "file": m.animal_detector.name(), "id": m.animal_detector.id, "size": p.size }));
            models.insert("pet_embedder".into(), serde_json::json!({ "file": m.pet_embedder.name(), "id": m.pet_embedder.id, "dim": 512 }));
        }
        if let (Some(m), Some(_)) = (&self.cfg.search, &self.search) {
            models.insert("image_embedder".into(), serde_json::json!({ "file": m.vision.name(), "id": m.vision.id, "dim": search::DIM }));
            models.insert("text_embedder".into(), serde_json::json!({ "file": m.text.name(), "id": m.text.id, "dim": search::DIM }));
        }
        if let (Some(m), Some(_)) = (&self.cfg.eyes, &self.eyes) {
            let id = format!("{}{}", m.landmarks.id, &m.blendshapes.id[..m.blendshapes.id.len().min(4)]);
            models.insert("eye_checker".into(), serde_json::json!({ "file": m.landmarks.name(), "id": id, "crop": eyes::CROP_SIDE }));
        }
        let analyzed = self.analyzed.load(Ordering::Relaxed);
        let busy = self.busy_us.load(Ordering::Relaxed) as f64 / 1000.0;
        serde_json::json!({
            "service": "hyperfocal-ml",
            "version": ENGINE_VERSION,
            "device": self.device.as_str(),
            "workers": self.workers(),
            "models": models,
            "face_scales": self.cfg.face_scales,
            "thresholds": { "face": self.cfg.face_threshold, "animal": self.cfg.animal_threshold },
            "faces": self.faces.is_some(),
            "search": self.search.is_some(),
            "eyes": self.eyes.is_some(),
            "analyzed": analyzed,
            "uptime_s": self.started.elapsed().as_secs(),
            "avg_ms": if analyzed > 0 { Some((busy / analyzed as f64 * 10.0).round() / 10.0) } else { None },
        })
    }

    /// Faces and animals in an encoded, upright image.
    pub fn analyze(&self, data: &[u8], faces: bool, animals: bool, face_thr: Option<f32>, animal_thr: Option<f32>) -> Result<serde_json::Value, AnalyzeError> {
        let t0 = Instant::now();
        let img = decode(data).map_err(|e| AnalyzeError::Image(format!("{e:#}")))?;
        let decode_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let mut v = self.analyze_rgb(&img, faces, animals, face_thr, animal_thr).map_err(AnalyzeError::Model)?;
        if let Some(ms) = v["ms"].as_object_mut() {
            ms.insert("decode".into(), round_to(decode_ms, 1).into());
            ms.insert("total".into(), round_to(t0.elapsed().as_secs_f64() * 1000.0, 1).into());
        }
        self.analyzed.fetch_add(1, Ordering::Relaxed);
        self.busy_us.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
        Ok(v)
    }

    pub fn analyze_rgb(&self, img: &Rgb, want_faces: bool, want_animals: bool, face_thr: Option<f32>, animal_thr: Option<f32>) -> Result<serde_json::Value> {
        let (Some(fm), Some(pm)) = (&self.faces, &self.pets) else {
            bail!("face and pet recognition is off");
        };
        let (w, h) = (img.w, img.h);
        let mut ms = serde_json::Map::new();
        let mut face_out = Vec::new();
        let mut animal_out: Vec<serde_json::Value> = Vec::new();
        let _g = self.gate.enter();
        let mut found_pets = Vec::new();
        if want_animals {
            let t = Instant::now();
            found_pets = pm.detect(img, animal_thr.unwrap_or(self.cfg.animal_threshold))?;
            ms.insert("animal_detect".into(), round_to(t.elapsed().as_secs_f64() * 1000.0, 1).into());
        }
        if want_faces {
            let t = Instant::now();
            let mut found = fm.detect(img, face_thr.unwrap_or(self.cfg.face_threshold))?;
            // YuNet takes some dog and cat faces for human ones (with a lower score): a
            // doubtful face that lies mostly inside an animal is the animal's.
            if !found_pets.is_empty() && !found.is_empty() {
                found.retain(|f| f.score as f64 >= 0.92 || !pets::inside_animal(&f.b, w, h, &found_pets));
            }
            ms.insert("face_detect".into(), round_to(t.elapsed().as_secs_f64() * 1000.0, 1).into());
            if !found.is_empty() {
                let t = Instant::now();
                let aligned: Vec<Vec<u8>> = found.iter().map(|f| fm.align(img, f)).collect();
                let embs = fm.embed(&aligned)?;
                ms.insert("face_embed".into(), round_to(t.elapsed().as_secs_f64() * 1000.0, 1).into());
                for (f, (emb, norm)) in found.iter().zip(embs) {
                    let (x, y, bw, bh) = (f.b[0] as f64, f.b[1] as f64, f.b[2] as f64, f.b[3] as f64);
                    let (x0, y0) = (x.max(0.0), y.max(0.0));
                    let (x1, y1) = ((x + bw).min(w as f64), (y + bh).min(h as f64));
                    if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
                        continue;
                    }
                    let size = f.b[2].min(f.b[3]) as f64;
                    let q = ((norm as f64 - self.cfg.face_norm_low) / (self.cfg.face_norm_high - self.cfg.face_norm_low).max(1e-6)).clamp(0.0, 1.0) * (size / 32.0).min(1.0);
                    let lm: Vec<f64> = f.lm.iter().flat_map(|p| [round_to((p[0] / w as f32) as f64, 5), round_to((p[1] / h as f32) as f64, 5)]).collect();
                    face_out.push(serde_json::json!({
                        "box": [round_to(x0 / w as f64, 5), round_to(y0 / h as f64, 5), round_to((x1 - x0) / w as f64, 5), round_to((y1 - y0) / h as f64, 5)],
                        "score": round_to(f.score as f64, 4),
                        "lm": lm,
                        "norm": round_to(norm as f64, 3),
                        "q": round_to(q, 3),
                        "px": round_to(size, 1),
                        "emb": b64_f16(&emb),
                    }));
                }
            }
        }
        if want_animals {
            let mut crops = Vec::new();
            let mut idx = Vec::new();
            for (i, a) in found_pets.iter().enumerate() {
                let b = a.b;
                // As numpy does it: single precision.
                let x0 = (b[0] * w as f32) as usize;
                let y0 = (b[1] * h as f32) as usize;
                let x1 = ((b[0] + b[2]) * w as f32).ceil() as usize;
                let y1 = ((b[1] + b[3]) * h as f32).ceil() as usize;
                let px = x1 as i64 - x0 as i64;
                let px = px.min(y1 as i64 - y0 as i64);
                animal_out.push(serde_json::json!({
                    "box": b.iter().map(|v| round_to(*v as f64, 5)).collect::<Vec<_>>(),
                    "score": round_to(a.score as f64, 4),
                    "species": a.species,
                    "px": px,
                }));
                // Too small to tell one animal from another.
                if px >= 32 {
                    let (c, cw, ch) = imgproc::crop(&img.px, w, h, x0, y0, x1, y1);
                    if cw > 0 && ch > 0 {
                        crops.push((c, cw, ch));
                        idx.push(i);
                    }
                }
            }
            if !crops.is_empty() {
                let t = Instant::now();
                for (i, e) in idx.into_iter().zip(pm.embed(&crops)?) {
                    animal_out[i]["emb"] = b64_f16(&e).into();
                }
                ms.insert("pet_embed".into(), round_to(t.elapsed().as_secs_f64() * 1000.0, 1).into());
            }
        }
        Ok(serde_json::json!({ "w": w, "h": h, "faces": face_out, "animals": animal_out, "ms": ms }))
    }
}

#[derive(Debug)]
pub enum AnalyzeError {
    /// The image could not be read: this request's fault.
    Image(String),
    /// A model failed: the engine's.
    Model(anyhow::Error),
}

/// An encoded image (JPEG, or PNG), upright as it is stored: orientation tags are ignored.
pub fn decode(data: &[u8]) -> Result<Rgb> {
    if data.starts_with(&[0xFF, 0xD8]) {
        return crate::images::jpeg::decode_scaled(data, 0);
    }
    let img = image::load_from_memory(data).map_err(|_| anyhow!("not a decodable image (send a JPEG or PNG)"))?;
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    Ok(Rgb { w, h, px: rgb.into_raw() })
}

/// A normalized vector as the API sends it: base64 of little-endian float16.
pub fn b64_f16(v: &[f32]) -> String {
    use base64::Engine;
    let mut bytes = Vec::with_capacity(v.len() * 2);
    for x in v {
        bytes.extend_from_slice(&half::f16::from_f32(*x).to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Unit length (a zero vector stays zero).
pub fn normalize(v: &mut [f32]) -> f32 {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let d = n.max(1e-9);
    for x in v.iter_mut() {
        *x /= d;
    }
    n
}

/// Rounded to `digits` decimals, halves to even (as Python's round).
pub fn round_to(v: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (v * k).round_ties_even() / k
}

/// The rows of an NCHW float tensor from interleaved RGB, mapped by `f`.
pub fn chw(img: &[u8], w: usize, h: usize, out: &mut Vec<f32>, f: impl Fn(usize, u8) -> f32) {
    let plane = w * h;
    let base = out.len();
    out.resize(base + plane * 3, 0.0);
    for i in 0..plane {
        for c in 0..3 {
            out[base + c * plane + i] = f(c, img[i * 3 + c]);
        }
    }
}

/// Splits `v` into rows of `dim` and makes each unit length.
pub fn rows(data: &[f32], dim: usize) -> Vec<Vec<f32>> {
    data.chunks_exact(dim)
        .map(|r| {
            let mut v = r.to_vec();
            normalize(&mut v);
            v
        })
        .collect()
}
