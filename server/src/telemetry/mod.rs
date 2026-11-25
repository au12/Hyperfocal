//! Debug mode (`HYPERFOCAL_DEBUG`): detailed telemetry for finding what fails, what is
//! slow, and where memory, CPU time and a phone's battery go.
//!
//! Off (the default), every hook in the code is one atomic load and nothing else runs. On:
//!
//! - log events at debug level and above, grouped by the line of code they come from, with
//!   panics and an unclean end of the previous run (`logs`);
//! - every HTTP request: counts, latency percentiles, bytes, aborted transfers and errors
//!   with their message, per route (`http`); every SQL statement: time, full-scan steps,
//!   sorts and automatic indexes (`sql`);
//! - timings of the work behind them (developing RAW files, thumbnails, culling, the AI
//!   engine, sidecar writes, hashing): the `tel_op!` timers and `tel_count!` counters in
//!   the code;
//! - every `HYPERFOCAL_DEBUG_INTERVAL` seconds, a sample of the process, its threads and
//!   child processes, the allocator, the container's cgroup, the GPU, the caches and every
//!   background worker (`sampler`);
//! - what the browsers report: errors, slow interactions, long tasks, dropped frames,
//!   network use, memory and battery (`client`, sent by web/src/lib/telemetry.ts).
//!
//! It all goes to NDJSON files in `<config>/debug`, capped in size and age (`store`), and
//! Settings → Debug downloads everything as one report (`report`).

pub mod api;
pub mod client;
pub mod hist;
pub mod http;
pub mod logs;
pub mod report;
pub mod sampler;
pub mod sql;
pub mod store;
pub mod system;

use crate::config::{env_num, env_str};
use hist::Hist;
use parking_lot::{Mutex, RwLock};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Debug mode's settings (from the environment).
#[derive(Debug, Clone)]
pub struct DebugConfig {
    pub enabled: bool,
    /// `HYPERFOCAL_DEBUG=trace`: trace-level log events too.
    pub trace: bool,
    /// Where the telemetry files go (`<config>/debug`).
    pub dir: PathBuf,
    /// Disk budget for those files (`HYPERFOCAL_DEBUG_MAX_MB`).
    pub max_bytes: u64,
    /// Files older than this many days are deleted (`HYPERFOCAL_DEBUG_DAYS`).
    pub days: u32,
    /// Time between samples (`HYPERFOCAL_DEBUG_INTERVAL`, seconds).
    pub interval: Duration,
}

impl DebugConfig {
    pub fn off(config_dir: &Path) -> DebugConfig {
        DebugConfig { enabled: false, trace: false, dir: config_dir.join("debug"), max_bytes: 1 << 30, days: 30, interval: Duration::from_secs(30) }
    }

    pub fn from_env(config_dir: &Path) -> DebugConfig {
        let v = env_str("HYPERFOCAL_DEBUG").unwrap_or_default().to_ascii_lowercase();
        let trace = v == "trace";
        DebugConfig {
            enabled: trace || matches!(v.as_str(), "1" | "true" | "yes" | "on" | "debug"),
            trace,
            dir: config_dir.join("debug"),
            max_bytes: env_num("HYPERFOCAL_DEBUG_MAX_MB", 1024u64).clamp(16, 1 << 20) << 20,
            days: env_num("HYPERFOCAL_DEBUG_DAYS", 30u32).clamp(1, 3650),
            interval: Duration::from_secs(env_num("HYPERFOCAL_DEBUG_INTERVAL", 30u64).clamp(5, 3600)),
        }
    }
}

static ON: AtomicBool = AtomicBool::new(false);
static TEL: OnceLock<Telemetry> = OnceLock::new();

/// Whether debug mode is on: what every hook checks first.
#[inline]
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// The telemetry, when debug mode is on.
#[inline]
pub fn get() -> Option<&'static Telemetry> {
    if on() { TEL.get() } else { None }
}

/// At most this many distinct names for operations and counters made up at run time (a
/// bug that puts an id in a name must not grow memory without end).
const MAX_NAMES: usize = 1000;

pub struct Telemetry {
    pub cfg: DebugConfig,
    /// When debug mode started (ms since the epoch).
    pub started_ms: i64,
    pub started: Instant,
    ops: RwLock<HashMap<&'static str, &'static OpStats>>,
    counters: RwLock<HashMap<&'static str, &'static AtomicU64>>,
    pub http: http::Http,
    pub sql: sql::Sql,
    pub logs: logs::Logs,
    pub clients: client::Clients,
    pub store: Arc<store::Store>,
    /// The most recent samples, newest last (an hour at the default interval).
    pub samples: Mutex<VecDeque<Arc<Value>>>,
    /// Moments marked from the UI ("this was slow"), newest last.
    pub marks: Mutex<VecDeque<Value>>,
    /// The previous run, when it ended without shutting down (a crash, an out-of-memory
    /// kill, `docker kill`): what was last known about it.
    pub previous: Mutex<Option<Value>>,
}

impl Telemetry {
    fn new(cfg: DebugConfig, store: Arc<store::Store>) -> Telemetry {
        Telemetry {
            started_ms: crate::db::now_ms(),
            started: Instant::now(),
            ops: RwLock::new(HashMap::new()),
            counters: RwLock::new(HashMap::new()),
            http: http::Http::default(),
            sql: sql::Sql::default(),
            logs: logs::Logs::new(Some(store.clone())),
            clients: client::Clients::default(),
            store,
            samples: Mutex::new(VecDeque::new()),
            marks: Mutex::new(VecDeque::new()),
            previous: Mutex::new(None),
            cfg,
        }
    }

    /// The statistics of an operation (made on first use, kept for good).
    pub fn op(&self, name: &str, slow_ms: u64) -> &'static OpStats {
        if let Some(o) = self.ops.read().get(name) {
            return o;
        }
        let mut m = self.ops.write();
        if let Some(o) = m.get(name) {
            return o;
        }
        let key: &'static str = if m.len() >= MAX_NAMES { "(other)" } else { Box::leak(name.to_string().into_boxed_str()) };
        *m.entry(key).or_insert_with(|| Box::leak(Box::new(OpStats::new(key, slow_ms))))
    }

    fn counter(&self, name: &str) -> &'static AtomicU64 {
        if let Some(c) = self.counters.read().get(name) {
            return c;
        }
        let mut m = self.counters.write();
        if let Some(c) = m.get(name) {
            return c;
        }
        let key: &'static str = if m.len() >= MAX_NAMES { "(other)" } else { Box::leak(name.to_string().into_boxed_str()) };
        *m.entry(key).or_insert_with(|| Box::leak(Box::new(AtomicU64::new(0))))
    }

    /// Every operation, by name.
    pub fn ops(&self) -> Vec<&'static OpStats> {
        let mut v: Vec<_> = self.ops.read().values().copied().collect();
        v.sort_by_key(|o| o.name);
        v
    }

    /// Every counter and its value.
    pub fn counters(&self) -> Vec<(&'static str, u64)> {
        let mut v: Vec<_> = self.counters.read().iter().map(|(k, c)| (*k, c.load(Ordering::Relaxed))).collect();
        v.sort();
        v
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    /// The newest sample.
    pub fn latest(&self) -> Option<Arc<Value>> {
        self.samples.lock().back().cloned()
    }

    /// Appends an event (a log line, a mark, a slow request...) to the events file.
    pub fn event(&self, mut v: Value) {
        stamp(&mut v);
        self.store.line(store::Kind::Events, &v);
    }
}

/// How long an operation takes: counts, errors, a latency histogram and the latest slow
/// calls with what they were working on.
pub struct OpStats {
    pub name: &'static str,
    slow_us: u64,
    pub h: Hist,
    slow: Mutex<VecDeque<Value>>,
}

impl OpStats {
    fn new(name: &'static str, slow_ms: u64) -> OpStats {
        OpStats { name, slow_us: slow_ms.saturating_mul(1000), h: Hist::new(), slow: Mutex::new(VecDeque::new()) }
    }

    pub fn record(&self, us: u64, err: bool, detail: Option<&str>) {
        self.h.add(us, err);
        if us >= self.slow_us {
            let mut s = self.slow.lock();
            if s.len() >= 12 {
                s.pop_front();
            }
            let mut v = json!({"t": crate::db::now_ms(), "ms": us / 1000, "th": thread_name()});
            if let Some(d) = detail {
                v["detail"] = d.into();
            }
            if err {
                v["err"] = true.into();
            }
            s.push_back(v);
        }
    }

    /// The latest slow calls (slower than this operation's threshold).
    pub fn slow(&self) -> Vec<Value> {
        self.slow.lock().iter().cloned().collect()
    }

    pub fn slow_ms(&self) -> u64 {
        self.slow_us / 1000
    }
}

/// Times an operation: records its duration when dropped. Inert (no clock read, nothing
/// recorded) when debug mode is off. Made by [`tel_op!`](crate::tel_op).
#[must_use = "a timer records when it is dropped"]
pub struct Timer {
    op: Option<&'static OpStats>,
    t0: Option<Instant>,
    err: bool,
    detail: Option<String>,
}

impl Timer {
    pub const OFF: Timer = Timer { op: None, t0: None, err: false, detail: None };

    #[doc(hidden)]
    #[inline]
    pub fn start(slot: &'static OnceLock<&'static OpStats>, name: &'static str, slow_ms: u64) -> Timer {
        if !on() {
            return Timer::OFF;
        }
        let Some(t) = TEL.get() else { return Timer::OFF };
        let op = *slot.get_or_init(|| t.op(name, slow_ms));
        Timer { op: Some(op), t0: Some(Instant::now()), err: false, detail: None }
    }

    /// A timer for an operation whose name is only known at run time.
    pub fn named(name: &str, slow_ms: u64) -> Timer {
        match get() {
            Some(t) => Timer { op: Some(t.op(name, slow_ms)), t0: Some(Instant::now()), err: false, detail: None },
            None => Timer::OFF,
        }
    }

    pub fn is_on(&self) -> bool {
        self.op.is_some()
    }

    /// Counts this call as failed.
    pub fn fail(&mut self) {
        self.err = true;
    }

    /// Counts this call as failed if `r` is an error.
    pub fn check<T, E>(&mut self, r: &Result<T, E>) {
        if r.is_err() {
            self.err = true;
        }
    }

    /// What the call worked on, kept with it if it turns out slow (computed only when
    /// debug mode is on).
    pub fn detail(&mut self, f: impl FnOnce() -> String) {
        if self.op.is_some() {
            self.detail = Some(f());
        }
    }

    /// Forgets this call (it did nothing worth timing).
    pub fn discard(mut self) {
        self.op = None;
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let (Some(op), Some(t0)) = (self.op, self.t0) {
            op.record(t0.elapsed().as_micros() as u64, self.err, self.detail.as_deref());
        }
    }
}

/// `tel_op!("name")` or `tel_op!("name", slow_ms)`: a [`Timer`] for an operation, which
/// records how long it took when dropped. Calls slower than `slow_ms` (1000 by default) are
/// kept with their detail. One atomic load when debug mode is off.
#[macro_export]
macro_rules! tel_op {
    ($name:literal) => {
        $crate::tel_op!($name, 1000)
    };
    ($name:literal, $slow_ms:expr) => {{
        static SLOT: ::std::sync::OnceLock<&'static $crate::telemetry::OpStats> = ::std::sync::OnceLock::new();
        $crate::telemetry::Timer::start(&SLOT, $name, $slow_ms)
    }};
}

/// `tel_count!("name")` or `tel_count!("name", n)`: adds to a counter (debug mode only).
#[macro_export]
macro_rules! tel_count {
    ($name:literal) => {
        $crate::tel_count!($name, 1)
    };
    ($name:literal, $n:expr) => {{
        if $crate::telemetry::on() {
            static SLOT: ::std::sync::OnceLock<&'static ::std::sync::atomic::AtomicU64> = ::std::sync::OnceLock::new();
            $crate::telemetry::bump(&SLOT, $name, ($n) as u64);
        }
    }};
}

#[doc(hidden)]
pub fn bump(slot: &'static OnceLock<&'static AtomicU64>, name: &'static str, n: u64) {
    if let Some(t) = TEL.get() {
        slot.get_or_init(|| t.counter(name)).fetch_add(n, Ordering::Relaxed);
    }
}

/// Adds to a counter whose name is only known at run time.
pub fn count(name: &str, n: u64) {
    if let Some(t) = get() {
        t.counter(name).fetch_add(n, Ordering::Relaxed);
    }
}

/// Records a duration measured elsewhere (by the AI engine, for instance).
pub fn record(name: &str, d: Duration, err: bool) {
    if let Some(t) = get() {
        t.op(name, 1000).record(d.as_micros() as u64, err, None);
    }
}

/// The current thread's name ("?" for an unnamed one).
pub fn thread_name() -> String {
    std::thread::current().name().unwrap_or("?").to_string()
}

/// Adds the time to an event: `t` (ms since the epoch) and `ts` (UTC, readable).
pub fn stamp(v: &mut Value) {
    if v.get("t").is_none() {
        v["t"] = crate::db::now_ms().into();
    }
    if v.get("ts").is_none() {
        let t = v["t"].as_i64().unwrap_or(0);
        v["ts"] = iso(t).into();
    }
}

/// `2026-11-25T03:14:15.926Z`.
pub fn iso(ms: i64) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).ok().and_then(|t| t.format(&fmt).ok()).unwrap_or_default()
}

/// Debug mode's state for `/api/status` (null when off).
pub fn status_json() -> Value {
    match get() {
        None => Value::Null,
        Some(t) => json!({
            "enabled": true,
            "since": t.started_ms,
            "interval": t.cfg.interval.as_secs(),
            "days": t.cfg.days,
            "max_mb": t.cfg.max_bytes >> 20,
            "dir_mb": t.store.dir_bytes() >> 20,
        }),
    }
}

/// Starts debug mode when `cfg` says so (before anything else, so the log layer and the
/// SQL hook see everything). Returns the layer that sends log events here, to install
/// with the subscriber.
pub fn init(cfg: &DebugConfig) -> Option<logs::Layer> {
    if !cfg.enabled {
        return None;
    }
    let store = match store::Store::start(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hyperfocal: debug mode could not use {}: {e}", cfg.dir.display());
            return None;
        }
    };
    if TEL.set(Telemetry::new(cfg.clone(), store)).is_err() {
        // Already started (tests): keep the first.
    }
    let t = TEL.get()?;
    ON.store(true, Ordering::Relaxed);
    *t.previous.lock() = t.store.begin_session();
    logs::install_panic_hook();
    Some(logs::Layer::new(&t.logs))
}

/// The server is up: starts the sampler and writes what this run is (versions, settings,
/// the machine) as its first event.
pub fn attach(state: &Arc<crate::state::AppState>) {
    let Some(t) = get() else { return };
    let prev = t.previous.lock().clone();
    if let Some(p) = &prev {
        tracing::warn!(
            "debug: the previous run (started {}) ended without shutting down: crashed, killed for memory, or stopped with docker kill",
            p["started"].as_i64().map(iso).unwrap_or_default()
        );
        t.event(json!({"k": "unclean_exit", "previous": p}));
    }
    t.event(json!({
        "k": "start",
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "config": report::config_json(&state.cfg),
        "env": report::env_json(),
        "system": system::machine(),
        "decoders": state.decoders,
        "simd": crate::images::simd::level(),
        "readonly": state.is_readonly(),
    }));
    sampler::spawn(state.clone(), tokio::runtime::Handle::try_current().ok());
    tracing::info!(
        "debug mode: telemetry is recorded to {} (at most {} MB, {} days; a sample every {} s)",
        t.cfg.dir.display(),
        t.cfg.max_bytes >> 20,
        t.cfg.days,
        t.cfg.interval.as_secs()
    );
}

/// A clean shutdown: the last event, files flushed, and the session marked as ended well.
/// Called as soon as the server is told to stop (an open event stream can keep it waiting
/// until the container is killed); a second call does nothing.
pub fn shutdown() {
    static DONE: AtomicBool = AtomicBool::new(false);
    let Some(t) = get() else { return };
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    t.event(json!({"k": "stop", "uptime_s": t.uptime().as_secs()}));
    t.store.end_session();
    t.store.flush(Duration::from_secs(3));
}

/// Debug mode for tests: started once per test process, in a directory of its own.
#[cfg(test)]
pub fn test_init() -> &'static Telemetry {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("tempdir").keep());
    if let Some(t) = TEL.get() {
        ON.store(true, Ordering::Relaxed);
        return t;
    }
    let mut cfg = DebugConfig::off(dir);
    cfg.enabled = true;
    let _ = init(&cfg);
    TEL.get().expect("telemetry")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timers_and_counters_record() {
        let t = test_init();
        {
            let mut a = crate::tel_op!("test.op", 0);
            a.detail(|| "photo 7".into());
        }
        {
            let mut b = crate::tel_op!("test.op", 0);
            b.fail();
        }
        crate::tel_count!("test.count", 3);
        count("test.count", 2);
        let op = t.op("test.op", 0);
        let s = op.h.snapshot();
        assert_eq!(s.n, 2);
        assert_eq!(s.err, 1);
        assert_eq!(op.slow().len(), 2);
        assert_eq!(op.slow()[0]["detail"], "photo 7");
        assert_eq!(t.counters().iter().find(|c| c.0 == "test.count").map(|c| c.1), Some(5));
        let st = status_json();
        assert_eq!(st["enabled"], true);
    }

    #[test]
    fn iso_times() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_764_040_455_926), "2025-11-25T03:14:15.926Z");
    }
}
