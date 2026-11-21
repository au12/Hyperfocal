//! The built-in AI features: face, dog and cat recognition (People & pets), search by
//! content (and Find similar, look-alike duplicates) and the closed-eyes check of culling.
//!
//! Everything is inside the Hyperfocal image; nothing large is. The models (about 0.7 GB
//! for people and pets, 1 GB for search) and, for an NVIDIA GPU, ONNX Runtime's CUDA build
//! with NVIDIA's libraries (about 1.3 GB) are downloaded into /config/ml the first time a
//! feature needs them, each checked against the sha256 in `manifest.json`. ONNX Runtime
//! for the CPU (28 MB) and the two small eye models are in the image.
//!
//! - `supervisor`: decides the device, downloads what is missing, starts the engine
//!   process and starts it again if it stops.
//! - `engine`: the models, in their own process (`hyperfocal ml-engine`), on 127.0.0.1.
//! - `client`: what the workers (people, search, culling) call the engine with.
//!
//! `HYPERFOCAL_ML=false` turns all of it off: nothing is downloaded or started.

pub mod client;
pub mod engine;
pub mod fetch;
pub mod manifest;
pub mod supervisor;

pub use client::{Client, MlError};

use parking_lot::{Condvar, Mutex, RwLock};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the image keeps ONNX Runtime for the CPU (`HYPERFOCAL_ORT_LIB` overrides it).
const IMAGE_ORT: &str = "/usr/lib/hyperfocal/libonnxruntime.so.1.29.0";
/// Where the image keeps the bundled models (`HYPERFOCAL_ML_BUNDLED` overrides it).
const IMAGE_MODELS: &str = "/app/ml/models";

/// ONNX Runtime's library for the CPU, when there is one (the image has it).
pub fn ort_library() -> Option<PathBuf> {
    let p = std::env::var_os("HYPERFOCAL_ORT_LIB").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(IMAGE_ORT));
    p.exists().then_some(p)
}

/// The models that ship in the image.
pub fn vendored_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("HYPERFOCAL_ML_BUNDLED") {
        return PathBuf::from(p);
    }
    let image = PathBuf::from(IMAGE_MODELS);
    if image.exists() {
        return image;
    }
    // A development build: the repository's copy.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ml/models")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceChoice {
    /// The NVIDIA GPU when the container can reach one, else the CPU.
    Auto,
    Cpu,
    Cuda,
}

/// The AI settings, from the environment.
#[derive(Debug, Clone)]
pub struct MlConfig {
    /// `HYPERFOCAL_ML`: false turns every AI feature off.
    pub enabled: bool,
    pub device: DeviceChoice,
    pub gpu: i32,
    pub gpu_mem_mb: u64,
    pub threads: usize,
    pub workers: usize,
    /// People & pets (faces, dogs and cats).
    pub faces: bool,
    /// Search by content, Find similar, look-alikes.
    pub search: bool,
    /// Culling's closed-eyes check (needs People & pets: it checks the faces found).
    pub eyes: bool,
    /// small or nano.
    pub pet_detector: String,
    pub face_scales: Vec<usize>,
    /// Models and runtimes (`HYPERFOCAL_ML_DIR`, default /config/ml).
    pub dir: PathBuf,
    /// The old companion container's address, if still set (no longer used).
    pub legacy_url: Option<String>,
}

impl MlConfig {
    pub fn from_env(config_dir: &Path) -> MlConfig {
        let s = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let b = |k: &str, d: bool| match s(k).map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("1" | "true" | "yes" | "on") => true,
            Some("0" | "false" | "no" | "off") => false,
            _ => d,
        };
        let n = |k: &str| s(k).and_then(|v| v.parse::<u64>().ok());
        let device = match s("HYPERFOCAL_ML_DEVICE").map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("cpu") => DeviceChoice::Cpu,
            Some("cuda" | "gpu" | "nvidia") => DeviceChoice::Cuda,
            _ => DeviceChoice::Auto,
        };
        let pet = match s("HYPERFOCAL_ML_PET_DETECTOR").map(|v| v.to_ascii_lowercase()).as_deref() {
            Some(v) if v.contains("nano") => "nano",
            _ => "small",
        };
        let scales: Vec<usize> = s("HYPERFOCAL_ML_FACE_SCALES")
            .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).filter(|&x: &usize| (64..=8192).contains(&x)).collect())
            .filter(|v: &Vec<usize>| !v.is_empty())
            .unwrap_or_else(|| vec![1600, 640, 320]);
        MlConfig {
            enabled: b("HYPERFOCAL_ML", true),
            device,
            gpu: n("HYPERFOCAL_ML_GPU").unwrap_or(0).min(64) as i32,
            gpu_mem_mb: n("HYPERFOCAL_ML_GPU_MEM_MB").unwrap_or(0),
            threads: n("HYPERFOCAL_ML_THREADS").unwrap_or(0).min(256) as usize,
            workers: n("HYPERFOCAL_ML_WORKERS").unwrap_or(0).min(16) as usize,
            faces: b("HYPERFOCAL_ML_FACES", true),
            search: b("HYPERFOCAL_ML_SEARCH", true),
            eyes: b("HYPERFOCAL_ML_EYES", true),
            pet_detector: pet.into(),
            face_scales: scales,
            dir: s("HYPERFOCAL_ML_DIR").map(PathBuf::from).unwrap_or_else(|| config_dir.join("ml")),
            legacy_url: s("HYPERFOCAL_ML_URL"),
        }
    }

    /// Everything off: tests, and `HYPERFOCAL_ML=false`.
    pub fn off(config_dir: &Path) -> MlConfig {
        MlConfig {
            enabled: false,
            device: DeviceChoice::Cpu,
            gpu: 0,
            gpu_mem_mb: 0,
            threads: 0,
            workers: 0,
            faces: false,
            search: false,
            eyes: false,
            pet_detector: "small".into(),
            face_scales: vec![1600, 640, 320],
            dir: config_dir.join("ml"),
            legacy_url: None,
        }
    }

    pub fn faces_on(&self) -> bool {
        self.enabled && self.faces
    }

    pub fn search_on(&self) -> bool {
        self.enabled && self.search
    }

    pub fn eyes_on(&self) -> bool {
        self.enabled && self.faces && self.eyes
    }

    pub fn any_on(&self) -> bool {
        self.faces_on() || self.search_on()
    }
}

/// What the UI shows about the AI engine.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MlStatus {
    pub enabled: bool,
    /// off, waiting (to retry), downloading, starting, ready, error
    pub phase: String,
    /// cpu or cuda, once decided.
    pub device: Option<String>,
    /// The GPU's name, on cuda.
    pub gpu: Option<String>,
    pub faces: bool,
    pub search: bool,
    pub eyes: bool,
    /// Bytes downloaded and to download, while downloading.
    pub done: u64,
    pub total: u64,
    /// What is downloading.
    pub current: Option<String>,
    pub error: Option<String>,
    /// Something to know that is not an error (e.g. no GPU: running on the CPU).
    pub note: Option<String>,
    /// Size of the models and runtimes in /config/ml (MB, of 1,000,000 bytes).
    pub disk_mb: u64,
}

/// The running engine.
#[derive(Clone)]
pub struct Endpoint {
    pub base: String,
    pub token: String,
}

pub struct Ml {
    pub cfg: MlConfig,
    pub status: RwLock<MlStatus>,
    endpoint: RwLock<Option<Endpoint>>,
    agent: ureq::Agent,
    pub progress: fetch::Progress,
    ready: Mutex<u64>,
    ready_cv: Condvar,
}

impl Ml {
    pub fn new(cfg: MlConfig) -> Ml {
        let status = MlStatus {
            enabled: cfg.any_on(),
            phase: if cfg.any_on() { "starting".into() } else { "off".into() },
            faces: cfg.faces_on(),
            search: cfg.search_on(),
            eyes: cfg.eyes_on(),
            ..Default::default()
        };
        Ml { cfg, status: RwLock::new(status), endpoint: RwLock::new(None), agent: client::agent(), progress: Default::default(), ready: Mutex::new(0), ready_cv: Condvar::new() }
    }

    /// A client for the engine, or why there is none yet.
    pub fn client(&self) -> Result<Client, MlError> {
        if let Some(ep) = &*self.endpoint.read() {
            return Ok(Client::new(&ep.base, Some(ep.token.clone()), self.agent.clone()));
        }
        let s = self.status();
        Err(match s.phase.as_str() {
            "off" => MlError::Unreachable("the AI features are turned off (HYPERFOCAL_ML=false)".into()),
            "error" | "waiting" => MlError::Unreachable(s.error.unwrap_or_else(|| "the AI engine is not running".into())),
            _ => MlError::Preparing(describe(&s)),
        })
    }

    /// The status, with the download progress filled in.
    pub fn status(&self) -> MlStatus {
        let mut s = self.status.read().clone();
        if s.phase == "downloading" {
            s.done = self.progress.done.load(std::sync::atomic::Ordering::Relaxed);
            s.total = self.progress.total.load(std::sync::atomic::Ordering::Relaxed);
            s.current = self.progress.current.lock().clone();
        }
        s
    }

    pub(crate) fn set_endpoint(&self, ep: Option<Endpoint>) {
        let up = ep.is_some();
        *self.endpoint.write() = ep;
        if up {
            *self.ready.lock() += 1;
            self.ready_cv.notify_all();
        }
    }

    /// Waits until the engine is up (or `d` passes). True when it is.
    pub fn wait_ready(&self, d: Duration) -> bool {
        if self.endpoint.read().is_some() {
            return true;
        }
        let mut g = self.ready.lock();
        self.ready_cv.wait_for(&mut g, d);
        drop(g);
        self.endpoint.read().is_some()
    }
}

/// "Downloading the AI models: 412 MB of 1.5 GB" and the like.
pub fn describe(s: &MlStatus) -> String {
    match s.phase.as_str() {
        "downloading" if s.total > 0 => format!("downloading the AI models: {} of {}", mb(s.done), mb(s.total)),
        "downloading" => "downloading the AI models".into(),
        "starting" => "starting the AI engine".into(),
        "ready" => "ready".into(),
        "off" => "off".into(),
        _ => s.error.clone().unwrap_or_else(|| "the AI engine is not running".into()),
    }
}

fn mb(b: u64) -> String {
    if b >= 1_000_000_000 { format!("{:.1} GB", b as f64 / 1e9) } else { format!("{} MB", b / 1_000_000) }
}
