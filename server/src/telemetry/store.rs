//! The telemetry files: NDJSON (one JSON object per line), one file per kind and UTC day
//! in `<config>/debug`:
//!
//! - `events-YYYY-MM-DD.ndjson`: log events, panics, slow requests and statements, HTTP
//!   errors, marks, the start and stop of each run;
//! - `metrics-YYYY-MM-DD.ndjson`: the sampler's samples;
//! - `client-YYYY-MM-DD.ndjson`: the browsers' reports.
//!
//! Written by one thread from a bounded queue (a full queue drops lines and counts them:
//! telemetry never holds up the app). Files older than `HYPERFOCAL_DEBUG_DAYS` go, and
//! the oldest go first beyond `HYPERFOCAL_DEBUG_MAX_MB`; if today's files alone reach the
//! cap, debug-level events stop being written for the day (warnings, samples and browser
//! reports carry on). `session.json` tells whether the previous run shut down cleanly.

use super::DebugConfig;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Events,
    Metrics,
    Client,
}

pub const KINDS: [Kind; 3] = [Kind::Events, Kind::Metrics, Kind::Client];

impl Kind {
    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Events => "events",
            Kind::Metrics => "metrics",
            Kind::Client => "client",
        }
    }

    fn index(self) -> usize {
        match self {
            Kind::Events => 0,
            Kind::Metrics => 1,
            Kind::Client => 2,
        }
    }
}

enum Msg {
    Line(Kind, String),
    Flush(std::sync::mpsc::Sender<()>),
}

/// A telemetry file on disk.
#[derive(Debug, Clone)]
pub struct DayFile {
    pub path: PathBuf,
    pub kind: Kind,
    /// `YYYY-MM-DD` (UTC).
    pub day: String,
    pub size: u64,
}

pub struct Store {
    pub dir: PathBuf,
    tx: SyncSender<Msg>,
    rx: Mutex<Option<Receiver<Msg>>>,
    max_bytes: u64,
    days: u32,
    /// Lines dropped because the queue was full, lines written, write errors.
    pub dropped: AtomicU64,
    pub written: AtomicU64,
    pub errors: AtomicU64,
    dir_bytes: AtomicU64,
    over_cap: AtomicBool,
}

/// `YYYY-MM-DD` of a time (ms since the epoch, UTC).
pub fn day_of(ms: i64) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).ok().and_then(|t| t.format(&fmt).ok()).unwrap_or_default()
}

fn parse_name(name: &str) -> Option<(Kind, String)> {
    let stem = name.strip_suffix(".ndjson")?;
    let (prefix, day) = stem.split_once('-')?;
    let kind = KINDS.into_iter().find(|k| k.prefix() == prefix)?;
    let b = day.as_bytes();
    let ok = b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().filter(|c| c.is_ascii_digit()).count() == 8;
    ok.then(|| (kind, day.to_string()))
}

impl Store {
    pub fn start(cfg: &DebugConfig) -> std::io::Result<Arc<Store>> {
        std::fs::create_dir_all(&cfg.dir)?;
        // Fails now (not on the first line) if the directory is not writable.
        let probe = cfg.dir.join(".write-test");
        std::fs::write(&probe, b"")?;
        let _ = std::fs::remove_file(&probe);
        let (tx, rx) = std::sync::mpsc::sync_channel(16_384);
        let store = Arc::new(Store {
            dir: cfg.dir.clone(),
            tx,
            rx: Mutex::new(Some(rx)),
            max_bytes: cfg.max_bytes,
            days: cfg.days,
            dropped: AtomicU64::new(0),
            written: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            dir_bytes: AtomicU64::new(0),
            over_cap: AtomicBool::new(false),
        });
        store.retention();
        let s = store.clone();
        std::thread::Builder::new().name("telemetry-io".into()).spawn(move || s.run())?;
        Ok(store)
    }

    /// Queues a line for its file (dropped, and counted, if the queue is full).
    pub fn line(&self, kind: Kind, v: &Value) {
        let Ok(s) = serde_json::to_string(v) else { return };
        match self.tx.try_send(Msg::Line(kind, s)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Waits (at most `timeout`) until everything queued so far is on disk.
    pub fn flush(&self, timeout: Duration) {
        let (ack, done) = std::sync::mpsc::channel();
        if self.tx.send(Msg::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(timeout);
        }
    }

    /// Bytes in the telemetry files (as of the last clean-up).
    pub fn dir_bytes(&self) -> u64 {
        self.dir_bytes.load(Ordering::Relaxed)
    }

    /// Today's files have filled the budget: only what matters most is written.
    pub fn over_cap(&self) -> bool {
        self.over_cap.load(Ordering::Relaxed)
    }

    fn run(&self) {
        let Some(rx) = self.rx.lock().take() else { return };
        let mut open: [Option<(String, BufWriter<File>)>; 3] = [None, None, None];
        let mut last_flush = Instant::now();
        let mut last_clean = Instant::now();
        let flush_all = |open: &mut [Option<(String, BufWriter<File>)>; 3]| {
            for (_, w) in open.iter_mut().flatten() {
                let _ = w.flush();
            }
        };
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(Msg::Line(kind, s)) => {
                    let day = day_of(crate::db::now_ms());
                    let slot = &mut open[kind.index()];
                    if slot.as_ref().is_none_or(|(d, _)| *d != day) {
                        if let Some((_, mut w)) = slot.take() {
                            let _ = w.flush();
                        }
                        let path = self.dir.join(format!("{}-{day}.ndjson", kind.prefix()));
                        match OpenOptions::new().create(true).append(true).open(&path) {
                            Ok(f) => *slot = Some((day, BufWriter::with_capacity(64 * 1024, f))),
                            Err(e) => {
                                if self.errors.fetch_add(1, Ordering::Relaxed) == 0 {
                                    eprintln!("hyperfocal: debug mode cannot write {}: {e}", path.display());
                                }
                                continue;
                            }
                        }
                    }
                    if let Some((_, w)) = slot.as_mut() {
                        if w.write_all(s.as_bytes()).and_then(|_| w.write_all(b"\n")).is_ok() {
                            self.written.fetch_add(1, Ordering::Relaxed);
                            self.dir_bytes.fetch_add(s.len() as u64 + 1, Ordering::Relaxed);
                        } else {
                            self.errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                Ok(Msg::Flush(ack)) => {
                    flush_all(&mut open);
                    let _ = ack.send(());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    flush_all(&mut open);
                    return;
                }
            }
            if last_flush.elapsed() >= Duration::from_secs(1) {
                flush_all(&mut open);
                last_flush = Instant::now();
            }
            let due = if self.dir_bytes() > self.max_bytes { 60 } else { 600 };
            if last_clean.elapsed() >= Duration::from_secs(due) {
                flush_all(&mut open);
                self.retention();
                last_clean = Instant::now();
            }
        }
    }

    /// The telemetry files, oldest first.
    pub fn files(&self) -> Vec<DayFile> {
        list(&self.dir)
    }

    /// Deletes files past their age, then the oldest while over the budget (never today's).
    pub fn retention(&self) {
        let today = day_of(crate::db::now_ms());
        let cutoff = day_of(crate::db::now_ms() - self.days as i64 * 86_400_000);
        let mut kept = Vec::new();
        for f in list(&self.dir) {
            if f.day < cutoff && std::fs::remove_file(&f.path).is_ok() {
                continue;
            }
            kept.push(f);
        }
        let mut total: u64 = kept.iter().map(|f| f.size).sum();
        for f in &kept {
            if total <= self.max_bytes {
                break;
            }
            if f.day < today && std::fs::remove_file(&f.path).is_ok() {
                total -= f.size;
            }
        }
        self.over_cap.store(total > self.max_bytes, Ordering::Relaxed);
        self.dir_bytes.store(total, Ordering::Relaxed);
    }

    /// Marks this run as started; returns the previous run if it never shut down cleanly,
    /// with the last sample it wrote.
    pub fn begin_session(&self) -> Option<Value> {
        let p = self.dir.join("session.json");
        let prev = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let unclean = prev.filter(|v| v["clean"] != true).map(|mut v| {
            if let Some(last) = self.last_sample() {
                v["last_sample"] = last;
            }
            v
        });
        let now = json!({"pid": std::process::id(), "version": env!("CARGO_PKG_VERSION"), "started": crate::db::now_ms(), "clean": false});
        let _ = std::fs::write(&p, serde_json::to_vec_pretty(&now).unwrap_or_default());
        unclean
    }

    /// Marks this run as shut down cleanly.
    pub fn end_session(&self) {
        let p = self.dir.join("session.json");
        let mut v = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).unwrap_or_else(|| json!({}));
        v["clean"] = true.into();
        v["ended"] = crate::db::now_ms().into();
        let _ = std::fs::write(&p, serde_json::to_vec_pretty(&v).unwrap_or_default());
    }

    /// The last line of the newest metrics file.
    fn last_sample(&self) -> Option<Value> {
        let f = list(&self.dir).into_iter().rev().find(|f| f.kind == Kind::Metrics)?;
        last_line(&f.path).and_then(|l| serde_json::from_str(&l).ok())
    }
}

/// The telemetry files in `dir`, oldest day first.
pub fn list(dir: &Path) -> Vec<DayFile> {
    let mut v: Vec<DayFile> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    let (kind, day) = parse_name(&name)?;
                    let size = e.metadata().ok().filter(|m| m.is_file())?.len();
                    Some(DayFile { path: e.path(), kind, day, size })
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| a.day.cmp(&b.day).then(a.kind.index().cmp(&b.kind.index())));
    v
}

/// The last complete line of a file (read from its end).
pub fn last_line(path: &Path) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(256 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let s = String::from_utf8_lossy(&buf);
    s.lines().rev().find(|l| l.trim_start().starts_with('{') && l.trim_end().ends_with('}')).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse() {
        assert_eq!(parse_name("events-2026-11-25.ndjson"), Some((Kind::Events, "2026-11-25".into())));
        assert_eq!(parse_name("metrics-2026-01-02.ndjson"), Some((Kind::Metrics, "2026-01-02".into())));
        assert_eq!(parse_name("client-2026-01-02.ndjson").map(|p| p.0), Some(Kind::Client));
        assert_eq!(parse_name("session.json"), None);
        assert_eq!(parse_name("other-2026-01-02.ndjson"), None);
        assert_eq!(parse_name("events-today.ndjson"), None);
        assert_eq!(day_of(0), "1970-01-01");
    }

    #[test]
    fn old_and_oversized_files_go_first() {
        let t = tempfile::tempdir().unwrap();
        let mut cfg = DebugConfig::off(t.path());
        cfg.max_bytes = 3000;
        cfg.days = 30;
        let now = crate::db::now_ms();
        let day = |d: i64| day_of(now - d * 86_400_000);
        std::fs::create_dir_all(&cfg.dir).unwrap();
        let w = |name: String, n: usize| std::fs::write(cfg.dir.join(name), vec![b'x'; n]).unwrap();
        w(format!("events-{}.ndjson", day(40)), 10); // too old
        w(format!("metrics-{}.ndjson", day(3)), 1000);
        w(format!("events-{}.ndjson", day(2)), 1000);
        w(format!("client-{}.ndjson", day(1)), 1000);
        w(format!("metrics-{}.ndjson", day(0)), 1000);
        w("session.json".into(), 5);
        let store = Store::start(&cfg).unwrap();
        let names: Vec<String> = store.files().iter().map(|f| f.path.file_name().unwrap().to_string_lossy().to_string()).collect();
        // 4000 bytes over a 3000 budget: the oldest day's file goes; the expired one too.
        assert_eq!(names, vec![format!("events-{}.ndjson", day(2)), format!("client-{}.ndjson", day(1)), format!("metrics-{}.ndjson", day(0))]);
        assert_eq!(store.dir_bytes(), 3000);
        assert!(!store.over_cap());
        assert!(cfg.dir.join("session.json").exists());
        // Lines reach the files.
        store.line(Kind::Events, &json!({"k": "test"}));
        store.flush(Duration::from_secs(5));
        let today = cfg.dir.join(format!("events-{}.ndjson", day(0)));
        assert_eq!(last_line(&today).as_deref(), Some(r#"{"k":"test"}"#));
        // An unclean end is noticed by the next run (the first file is not a session).
        assert!(store.begin_session().is_none());
        let prev = store.begin_session().expect("the session before did not end cleanly");
        assert_eq!(prev["clean"], false);
        store.end_session();
        assert!(store.begin_session().is_none());
    }
}
