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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

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
    /// `HYPERFOCAL_ML_IDLE_MIN`: minutes without AI work before the engine stops and
    /// frees its memory (0: never). None: the default for the device (`idle_limits`).
    pub idle_min: Option<f64>,
    /// `HYPERFOCAL_ML_IDLE_NO_CLIENTS_MIN`: the same while no browser has Hyperfocal open.
    pub idle_no_clients_min: Option<f64>,
}

/// Idle limits when unset: on a GPU, which other apps (Plex, Immich) often share, the
/// engine gives its memory back; on the CPU it stays loaded, as before.
pub const IDLE_GPU_MIN: f64 = 15.0;
pub const IDLE_NO_CLIENTS_GPU_MIN: f64 = 2.0;

impl MlConfig {
    pub fn from_env(config_dir: &Path) -> MlConfig {
        let s = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let b = |k: &str, d: bool| match s(k).map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("1" | "true" | "yes" | "on") => true,
            Some("0" | "false" | "no" | "off") => false,
            _ => d,
        };
        let n = |k: &str| s(k).and_then(|v| v.parse::<u64>().ok());
        // Minutes; 0, off, never or false: never.
        let minutes = |k: &str| {
            s(k).and_then(|v| match v.to_ascii_lowercase().as_str() {
                "off" | "never" | "false" | "no" => Some(0.0),
                v => v.parse::<f64>().ok().filter(|m| m.is_finite() && *m >= 0.0),
            })
        };
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
            idle_min: minutes("HYPERFOCAL_ML_IDLE_MIN"),
            idle_no_clients_min: minutes("HYPERFOCAL_ML_IDLE_NO_CLIENTS_MIN"),
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
            idle_min: None,
            idle_no_clients_min: None,
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

    /// How long the engine may go without work before it stops: always, and while no
    /// browser is connected (None: no limit).
    pub fn idle_limits(&self, device: engine::Device) -> (Option<Duration>, Option<Duration>) {
        let gpu = device == engine::Device::Cuda;
        let limit = |v: Option<f64>, gpu_default: f64| {
            let m = v.unwrap_or(if gpu { gpu_default } else { 0.0 });
            (m > 0.0).then(|| Duration::from_secs_f64(m * 60.0))
        };
        (limit(self.idle_min, IDLE_GPU_MIN), limit(self.idle_no_clients_min, IDLE_NO_CLIENTS_GPU_MIN))
    }
}

/// What the UI shows about the AI engine.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MlStatus {
    pub enabled: bool,
    /// off, waiting (to retry), downloading, starting, ready, asleep (stopped for want of
    /// work: starts again when needed), error
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
    /// Minutes without work before the engine stops (None: it stays loaded), and the
    /// same while no browser is connected.
    pub idle_min: Option<f64>,
    pub idle_no_clients_min: Option<f64>,
    /// Browsers with Hyperfocal open (their event streams).
    pub clients: usize,
}

/// The running engine.
#[derive(Clone)]
pub struct Endpoint {
    pub base: String,
    pub token: String,
}

/// What the server and its clients share about the running engine.
pub struct Link {
    pub status: RwLock<MlStatus>,
    endpoint: RwLock<Option<Endpoint>>,
    /// Counts engine starts (and failed wakes), for waiters.
    ready: Mutex<u64>,
    ready_cv: Condvar,
    /// The engine was stopped for want of work: the next request starts it.
    asleep: AtomicBool,
    want: Mutex<bool>,
    want_cv: Condvar,
    /// Requests to the engine in progress.
    inflight: AtomicUsize,
    /// Milliseconds since `epoch`: the last work for the engine, the last browser gone.
    last_work: AtomicU64,
    last_client: AtomicU64,
    clients: AtomicUsize,
    epoch: Instant,
    /// The engine's `/v1/info`, answered while it sleeps.
    pub(crate) info: RwLock<Option<serde_json::Value>>,
}

/// How long a request waits for a sleeping engine to start.
const WAKE_WAIT: Duration = Duration::from_secs(120);

impl Link {
    fn new(status: MlStatus) -> Link {
        Link {
            status: RwLock::new(status),
            endpoint: RwLock::new(None),
            ready: Mutex::new(0),
            ready_cv: Condvar::new(),
            asleep: AtomicBool::new(false),
            want: Mutex::new(false),
            want_cv: Condvar::new(),
            inflight: AtomicUsize::new(0),
            last_work: AtomicU64::new(0),
            last_client: AtomicU64::new(0),
            clients: AtomicUsize::new(0),
            epoch: Instant::now(),
            info: RwLock::new(None),
        }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    pub(crate) fn touch(&self) {
        self.last_work.store(self.now_ms(), SeqCst);
    }

    pub fn asleep(&self) -> bool {
        self.asleep.load(SeqCst)
    }

    pub fn clients(&self) -> usize {
        self.clients.load(SeqCst)
    }

    /// Why there is no engine to talk to.
    fn down(&self) -> MlError {
        let s = self.status.read().clone();
        match s.phase.as_str() {
            "off" => MlError::Unreachable("the AI features are turned off (HYPERFOCAL_ML=false)".into()),
            "error" | "waiting" => MlError::Unreachable(s.error.unwrap_or_else(|| "the AI engine is not running".into())),
            _ => MlError::Preparing(describe(&s)),
        }
    }

    /// The engine for one request, started first if it sleeps. `work`: the request is
    /// work (it keeps the engine awake), not a look at its info.
    pub(crate) fn lease(self: &Arc<Self>, work: bool) -> Result<(Lease, Endpoint), MlError> {
        // Counted before the endpoint is read: the supervisor puts the engine to sleep
        // only under the endpoint's lock, and only with nothing in flight.
        self.inflight.fetch_add(1, SeqCst);
        let lease = Lease { link: self.clone(), work };
        if work {
            self.touch();
        }
        let deadline = Instant::now() + WAKE_WAIT;
        loop {
            let seen = *self.ready.lock();
            if let Some(ep) = self.endpoint.read().clone() {
                return Ok((lease, ep));
            }
            if !self.asleep() {
                return Err(self.down());
            }
            self.wake();
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(MlError::Preparing("starting the AI engine".into()));
            }
            let mut g = self.ready.lock();
            if *g == seen {
                self.ready_cv.wait_for(&mut g, left.min(Duration::from_secs(1)));
            }
        }
    }

    /// Asks the supervisor to start the sleeping engine.
    pub fn wake(&self) {
        let mut w = self.want.lock();
        if !*w {
            *w = true;
            self.want_cv.notify_all();
        }
    }

    /// The supervisor: waits until something needs the engine.
    pub(crate) fn wait_for_want(&self) {
        let mut w = self.want.lock();
        while !*w {
            self.want_cv.wait_for(&mut w, Duration::from_secs(60));
        }
        *w = false;
    }

    /// The supervisor: stops serving if the engine has been idle long enough. True when
    /// it was put to sleep (no request holds it, and none can get it any more).
    pub(crate) fn try_sleep(&self, idle: Option<Duration>, no_clients: Option<Duration>) -> bool {
        let now = self.now_ms();
        let since_work = now.saturating_sub(self.last_work.load(SeqCst));
        let limit_ms = |d: Duration| d.as_millis() as u64;
        let mut due = idle.is_some_and(|d| since_work >= limit_ms(d));
        if !due && self.clients() == 0 {
            // From the later of the last work and the last browser leaving (a reload is
            // not a goodbye).
            let quiet = since_work.min(now.saturating_sub(self.last_client.load(SeqCst)));
            due = no_clients.is_some_and(|d| quiet >= limit_ms(d));
        }
        if !due {
            return false;
        }
        let mut ep = self.endpoint.write();
        if self.inflight.load(SeqCst) > 0 || ep.is_none() {
            return false;
        }
        self.asleep.store(true, SeqCst);
        *ep = None;
        true
    }

    /// The supervisor: a wake could not start the engine; waiting requests get its error.
    pub(crate) fn wake_failed(&self) {
        self.asleep.store(false, SeqCst);
        *self.want.lock() = false;
        *self.ready.lock() += 1;
        self.ready_cv.notify_all();
    }

    pub(crate) fn set_endpoint(&self, ep: Option<Endpoint>) {
        let up = ep.is_some();
        if up {
            // Idle time counts from the start.
            self.touch();
        }
        {
            let mut e = self.endpoint.write();
            *e = ep;
            if up {
                self.asleep.store(false, SeqCst);
            }
        }
        if up {
            *self.want.lock() = false;
        }
        if up {
            *self.ready.lock() += 1;
            self.ready_cv.notify_all();
        }
    }

    /// A browser opened the event stream; the guard counts it until it closes.
    pub fn client_connected(self: &Arc<Self>) -> ClientGuard {
        self.clients.fetch_add(1, SeqCst);
        ClientGuard(self.clone())
    }
}

/// One request to the engine in progress.
pub(crate) struct Lease {
    link: Arc<Link>,
    work: bool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.work {
            // A long request counts until it ends.
            self.link.touch();
        }
        self.link.inflight.fetch_sub(1, SeqCst);
    }
}

/// A connected browser (see `Link::client_connected`).
pub struct ClientGuard(Arc<Link>);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.last_client.store(self.0.now_ms(), SeqCst);
        self.0.clients.fetch_sub(1, SeqCst);
    }
}

pub struct Ml {
    pub cfg: MlConfig,
    pub link: Arc<Link>,
    agent: ureq::Agent,
    pub progress: fetch::Progress,
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
        Ml { cfg, link: Arc::new(Link::new(status)), agent: client::agent(), progress: Default::default() }
    }

    /// A client for the engine, or why there is none yet. While the engine sleeps the
    /// client is handed out all the same: its first piece of work starts the engine.
    pub fn client(&self) -> Result<Client, MlError> {
        if self.link.endpoint.read().is_some() || self.link.asleep() {
            return Ok(Client::managed(self.link.clone(), self.agent.clone()));
        }
        Err(self.link.down())
    }

    /// The status, with the download progress and connected browsers filled in.
    pub fn status(&self) -> MlStatus {
        let mut s = self.link.status.read().clone();
        if s.phase == "downloading" {
            s.done = self.progress.done.load(std::sync::atomic::Ordering::Relaxed);
            s.total = self.progress.total.load(std::sync::atomic::Ordering::Relaxed);
            s.current = self.progress.current.lock().clone();
        }
        s.clients = self.link.clients();
        s
    }

    pub(crate) fn set_endpoint(&self, ep: Option<Endpoint>) {
        self.link.set_endpoint(ep);
    }

    /// Waits until the engine is up or asleep, ready to be woken (or `d` passes).
    pub fn wait_ready(&self, d: Duration) -> bool {
        let ok = || self.link.endpoint.read().is_some() || self.link.asleep();
        if ok() {
            return true;
        }
        let mut g = self.link.ready.lock();
        self.link.ready_cv.wait_for(&mut g, d);
        drop(g);
        ok()
    }
}

/// "Downloading the AI models: 412 MB of 1.5 GB" and the like.
pub fn describe(s: &MlStatus) -> String {
    match s.phase.as_str() {
        "downloading" if s.total > 0 => format!("downloading the AI models: {} of {}", mb(s.done), mb(s.total)),
        "downloading" => "downloading the AI models".into(),
        "starting" => "starting the AI engine".into(),
        "ready" => "ready".into(),
        "asleep" => "asleep: starts again when needed".into(),
        "off" => "off".into(),
        _ => s.error.clone().unwrap_or_else(|| "the AI engine is not running".into()),
    }
}

fn mb(b: u64) -> String {
    if b >= 1_000_000_000 { format!("{:.1} GB", b as f64 / 1e9) } else { format!("{} MB", b / 1_000_000) }
}

#[cfg(test)]
mod idle_tests {
    use super::*;

    fn link() -> Arc<Link> {
        let l = Arc::new(Link::new(MlStatus { phase: "ready".into(), ..Default::default() }));
        l.set_endpoint(Some(Endpoint { base: "http://127.0.0.1:1".into(), token: "t".into() }));
        l
    }
    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn sleeps_after_idle_not_before() {
        let l = link();
        assert!(!l.try_sleep(Some(MS(200)), None), "just started");
        std::thread::sleep(MS(250));
        assert!(l.try_sleep(Some(MS(200)), None));
        assert!(l.asleep());
        assert!(l.endpoint.read().is_none());
    }

    #[test]
    fn work_in_flight_keeps_it_awake() {
        let l = link();
        let held = l.lease(true).unwrap();
        std::thread::sleep(MS(250));
        assert!(!l.try_sleep(Some(MS(200)), Some(MS(1))), "a request is running");
        drop(held);
        assert!(!l.try_sleep(Some(MS(200)), None), "the end of a request counts as work");
        // A look at the info is not work.
        std::thread::sleep(MS(150));
        drop(l.lease(false).unwrap());
        std::thread::sleep(MS(100));
        assert!(l.try_sleep(Some(MS(200)), None));
    }

    #[test]
    fn no_clients_limit_only_without_browsers() {
        let l = link();
        let browser = l.client_connected();
        std::thread::sleep(MS(150));
        assert!(!l.try_sleep(None, Some(MS(100))), "a browser is connected");
        drop(browser);
        // A reload is not a goodbye: the grace counts from the browser leaving.
        assert!(!l.try_sleep(None, Some(MS(100))));
        std::thread::sleep(MS(150));
        assert_eq!(l.clients(), 0);
        assert!(l.try_sleep(None, Some(MS(100))));
    }

    #[test]
    fn work_wakes_a_sleeping_engine() {
        let l = link();
        std::thread::sleep(MS(20));
        assert!(l.try_sleep(Some(MS(10)), None));
        // The supervisor: waits for work, then starts the engine on another port.
        let sup = {
            let l = l.clone();
            std::thread::spawn(move || {
                l.wait_for_want();
                std::thread::sleep(MS(100));
                l.set_endpoint(Some(Endpoint { base: "http://127.0.0.1:2".into(), token: "u".into() }));
            })
        };
        let (_lease, ep) = l.lease(true).unwrap();
        assert_eq!(ep.base, "http://127.0.0.1:2");
        assert!(!l.asleep());
        sup.join().unwrap();
        // The wake was consumed: a later sleep does not wake at once.
        assert!(!*l.want.lock());
    }

    #[test]
    fn failed_wake_reaches_the_waiter() {
        let l = link();
        std::thread::sleep(MS(20));
        assert!(l.try_sleep(Some(MS(10)), None));
        let sup = {
            let l = l.clone();
            std::thread::spawn(move || {
                l.wait_for_want();
                l.status.write().phase = "error".into();
                l.status.write().error = Some("no GPU".into());
                l.wake_failed();
            })
        };
        match l.lease(true) {
            Err(MlError::Unreachable(m)) => assert_eq!(m, "no GPU"),
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("no engine"),
        }
        sup.join().unwrap();
    }

    #[test]
    fn idle_defaults_by_device() {
        let mut c = MlConfig::off(Path::new("/tmp"));
        assert_eq!(c.idle_limits(engine::Device::Cpu), (None, None));
        assert_eq!(c.idle_limits(engine::Device::Cuda), (Some(Duration::from_secs(900)), Some(Duration::from_secs(120))));
        c.idle_min = Some(0.0);
        c.idle_no_clients_min = Some(0.5);
        assert_eq!(c.idle_limits(engine::Device::Cuda), (None, Some(Duration::from_secs(30))));
        assert_eq!(c.idle_limits(engine::Device::Cpu), (None, Some(Duration::from_secs(30))));
    }
}
