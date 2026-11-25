//! Log events in debug mode: counted per line of code that logs them (so a warning raised
//! for 30,000 files is one group with a count and a few examples, not 30,000 lines to read),
//! written to the events file at a bounded rate per line of code, and kept in memory for the
//! debug panel. Panics are recorded with where they happened.

use super::store::{Kind, Store};
use super::{stamp, thread_name};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::callsite::Identifier;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;

/// Lines written to the events file per line of code and minute; the rest are counted.
const PER_MINUTE_PROBLEMS: u32 = 120;
const PER_MINUTE_OTHER: u32 = 30;
/// Examples kept per line of code (different messages).
const SAMPLES: usize = 6;
const RING: usize = 300;
const MSG_MAX: usize = 2000;

pub const LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

fn level_index(l: Level) -> usize {
    match l {
        Level::ERROR => 0,
        Level::WARN => 1,
        Level::INFO => 2,
        Level::DEBUG => 3,
        _ => 4,
    }
}

/// One line of code that logs.
struct Site {
    level: Level,
    target: &'static str,
    at: String,
    n: u64,
    first: i64,
    last: i64,
    samples: VecDeque<String>,
    /// The minute (ms / 60,000) the rate limit counts in, lines written in it, and lines
    /// not written.
    minute: i64,
    written: u32,
    suppressed: u64,
}

pub struct Logs {
    sites: Mutex<HashMap<Identifier, Site>>,
    /// The latest events of every level, and of warnings and errors only.
    recent: Mutex<VecDeque<Value>>,
    problems: Mutex<VecDeque<Value>>,
    /// Events per level since the start (see [`LEVELS`]).
    pub counts: [AtomicU64; 5],
    pub panics: Mutex<Vec<Value>>,
    store: Option<Arc<Store>>,
}

impl Logs {
    pub fn new(store: Option<Arc<Store>>) -> Logs {
        Logs {
            sites: Mutex::new(HashMap::new()),
            recent: Mutex::new(VecDeque::new()),
            problems: Mutex::new(VecDeque::new()),
            counts: [const { AtomicU64::new(0) }; 5],
            panics: Mutex::new(Vec::new()),
            store,
        }
    }

    fn write(&self, v: &Value) {
        if let Some(s) = &self.store {
            s.line(Kind::Events, v);
        }
    }

    pub fn on_event(&self, event: &Event<'_>) {
        let meta = event.metadata();
        let mut vis = Message::default();
        event.record(&mut vis);
        let msg = vis.finish();
        let level = *meta.level();
        self.counts[level_index(level)].fetch_add(1, Ordering::Relaxed);
        let now = crate::db::now_ms();
        let at = format!("{}:{}", meta.file().unwrap_or("?"), meta.line().unwrap_or(0));
        let mut v = json!({"t": now, "k": "log", "lvl": level.as_str(), "tgt": meta.target(), "at": at, "th": thread_name(), "msg": msg});
        let problem = level <= Level::WARN;
        let (write, flushed) = {
            let mut sites = self.sites.lock();
            let s = sites.entry(meta.callsite()).or_insert_with(|| Site {
                level,
                target: meta.target(),
                at: at.clone(),
                n: 0,
                first: now,
                last: now,
                samples: VecDeque::new(),
                minute: now / 60_000,
                written: 0,
                suppressed: 0,
            });
            s.n += 1;
            s.last = now;
            if !s.samples.iter().any(|m| *m == msg) {
                if s.samples.len() >= SAMPLES {
                    s.samples.pop_front();
                }
                s.samples.push_back(msg.clone());
            }
            let mut flushed = None;
            if s.minute != now / 60_000 {
                if s.suppressed > 0 {
                    flushed = Some(json!({"t": now, "k": "suppressed", "lvl": s.level.as_str(), "at": s.at, "n": s.suppressed, "minute": s.minute * 60_000}));
                }
                s.minute = now / 60_000;
                s.written = 0;
                s.suppressed = 0;
            }
            let limit = if problem { PER_MINUTE_PROBLEMS } else { PER_MINUTE_OTHER };
            let over_cap = self.store.as_ref().is_some_and(|s| s.over_cap()) && !problem;
            let write = s.written < limit && !over_cap;
            if write {
                s.written += 1;
            } else {
                s.suppressed += 1;
            }
            (write, flushed)
        };
        if let Some(f) = flushed {
            self.write(&f);
        }
        stamp(&mut v);
        if write {
            self.write(&v);
        }
        push(&self.recent, v.clone());
        if problem {
            push(&self.problems, v);
        }
    }

    /// Writes the counts of lines held back in minutes that have ended (the sampler calls
    /// this, so a burst that stopped is still accounted for).
    pub fn sweep(&self) {
        let now = crate::db::now_ms();
        let mut out = Vec::new();
        {
            let mut sites = self.sites.lock();
            for s in sites.values_mut() {
                if s.minute != now / 60_000 && s.suppressed > 0 {
                    out.push(json!({"t": now, "k": "suppressed", "lvl": s.level.as_str(), "at": s.at, "n": s.suppressed, "minute": s.minute * 60_000}));
                    s.suppressed = 0;
                    s.written = 0;
                    s.minute = now / 60_000;
                }
            }
        }
        for mut v in out {
            stamp(&mut v);
            self.write(&v);
        }
    }

    pub fn count(&self, level: usize) -> u64 {
        self.counts[level].load(Ordering::Relaxed)
    }

    /// Every line of code that logged, most frequent first (`min_level`: 0 errors only, 1
    /// warnings and errors, ... 4 everything).
    pub fn sites(&self, min_level: usize) -> Vec<Value> {
        let sites = self.sites.lock();
        let mut v: Vec<&Site> = sites.values().filter(|s| level_index(s.level) <= min_level).collect();
        v.sort_by(|a, b| level_index(a.level).cmp(&level_index(b.level)).then(b.n.cmp(&a.n)));
        v.iter()
            .map(|s| {
                json!({
                    "lvl": s.level.as_str(), "at": s.at, "tgt": s.target, "n": s.n,
                    "first": super::iso(s.first), "last": super::iso(s.last), "samples": s.samples,
                })
            })
            .collect()
    }

    pub fn recent(&self, n: usize) -> Vec<Value> {
        let r = self.recent.lock();
        r.iter().skip(r.len().saturating_sub(n)).cloned().collect()
    }

    pub fn problems(&self, n: usize) -> Vec<Value> {
        let r = self.problems.lock();
        r.iter().skip(r.len().saturating_sub(n)).cloned().collect()
    }

    pub fn record_panic(&self, mut v: Value) {
        self.counts[0].fetch_add(1, Ordering::Relaxed);
        stamp(&mut v);
        self.write(&v);
        push(&self.problems, v.clone());
        let mut p = self.panics.lock();
        if p.len() < 100 {
            p.push(v);
        }
    }
}

fn push(ring: &Mutex<VecDeque<Value>>, v: Value) {
    let mut r = ring.lock();
    if r.len() >= RING {
        r.pop_front();
    }
    r.push_back(v);
}

/// An event's message, with any other fields after it as `name=value`.
#[derive(Default)]
struct Message {
    msg: String,
    fields: String,
}

impl Message {
    fn finish(mut self) -> String {
        if !self.fields.is_empty() {
            if !self.msg.is_empty() {
                self.msg.push(' ');
            }
            self.msg.push_str(self.fields.trim_start());
        }
        if self.msg.len() > MSG_MAX {
            let mut cut = MSG_MAX;
            while !self.msg.is_char_boundary(cut) {
                cut -= 1;
            }
            self.msg.truncate(cut);
            self.msg.push('…');
        }
        self.msg
    }
}

impl Visit for Message {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.msg.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.msg, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// The tracing layer that feeds [`Logs`] (installed with a filter of its own: see `lib.rs`).
pub struct Layer {
    logs: &'static Logs,
}

impl Layer {
    pub fn new(logs: &'static Logs) -> Layer {
        Layer { logs }
    }
}

impl<S: Subscriber> tracing_subscriber::Layer<S> for Layer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        self.logs.on_event(event);
    }
}

/// Records panics (with the thread and the place) before the usual message is printed.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(t) = super::get() {
            let payload = info.payload();
            let msg = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "(no message)".into());
            let at = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
            // Symbols are stripped from release builds: a backtrace only with RUST_BACKTRACE.
            let bt = std::backtrace::Backtrace::capture();
            let bt = (bt.status() == std::backtrace::BacktraceStatus::Captured).then(|| bt.to_string().chars().take(8000).collect::<String>());
            t.logs.record_panic(json!({"k": "panic", "th": thread_name(), "at": at, "msg": msg, "bt": bt}));
        }
        prev(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn events_are_grouped_by_line_of_code() {
        let logs: &'static Logs = Box::leak(Box::new(Logs::new(None)));
        let sub = tracing_subscriber::registry().with(Layer::new(logs));
        tracing::subscriber::with_default(sub, || {
            for i in 0..5 {
                tracing::warn!("cannot read /photos/{i}.CR3: denied");
            }
            tracing::debug!(photo = 7, "made a thumbnail");
            tracing::error!("x");
        });
        assert_eq!(logs.count(1), 5);
        assert_eq!(logs.count(0), 1);
        assert_eq!(logs.count(3), 1);
        let sites = logs.sites(4);
        assert_eq!(sites.len(), 3);
        // Errors first, then by count.
        assert_eq!(sites[0]["lvl"], "ERROR");
        assert_eq!(sites[1]["n"], 5);
        assert_eq!(sites[1]["samples"].as_array().unwrap().len(), 5);
        assert!(sites[1]["at"].as_str().unwrap().contains("logs.rs:"));
        let rec = logs.recent(10);
        assert_eq!(rec.len(), 7);
        assert_eq!(rec[5]["msg"], "made a thumbnail photo=7");
        assert_eq!(logs.problems(10).len(), 6);
        // Warnings and errors only.
        assert_eq!(logs.sites(1).len(), 2);
    }
}
