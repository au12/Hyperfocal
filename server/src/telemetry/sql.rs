//! SQL statements in debug mode: SQLite reports each one as it finishes, with its time
//! and its own counters (rows walked by a full scan, sorts, automatic indexes: the signs
//! of a missing index). Statements are grouped by their text with numbers taken out, so a
//! query built with ids in it still counts as one.

use super::{get, stamp, thread_name};
use parking_lot::Mutex;
use rusqlite::StatementStatus;
use rusqlite::trace::{TraceEvent, TraceEventCodes};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

/// Statements slower than this are written out one by one.
const SLOW_US: u64 = 100_000;
const MAX_STATEMENTS: usize = 1500;

#[derive(Debug, Default, Clone)]
pub struct Stmt {
    pub n: u64,
    /// Total and largest time (µs).
    pub us: u64,
    pub max_us: u64,
    /// The most rows a full scan walked in one run.
    pub scan: u64,
    pub sort: bool,
    pub autoindex: bool,
    /// Virtual machine steps in a run (the work SQLite did), at most.
    pub vm: u64,
}

#[derive(Default)]
pub struct Sql {
    stats: Mutex<HashMap<String, Stmt>>,
    slow: Mutex<VecDeque<Value>>,
    pub total_us: AtomicU64,
    pub count: AtomicU64,
    slow_written: Mutex<(i64, u32)>,
}

impl Sql {
    pub fn all(&self) -> Vec<(String, Stmt)> {
        self.stats.lock().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn slow(&self, n: usize) -> Vec<Value> {
        let s = self.slow.lock();
        s.iter().skip(s.len().saturating_sub(n)).cloned().collect()
    }

    /// The statements that took the most time in all.
    pub fn top(&self, n: usize) -> Vec<Value> {
        let mut v = self.all();
        v.sort_by(|a, b| b.1.us.cmp(&a.1.us));
        v.into_iter()
            .take(n)
            .map(|(sql, s)| {
                let mut j = json!({"sql": short(&sql, 400), "n": s.n, "ms": s.us / 1000, "avg_ms": (s.us / s.n.max(1)) as f64 / 1000.0, "max_ms": s.max_us / 1000});
                if s.scan > 0 {
                    j["scan_rows"] = s.scan.into();
                }
                if s.sort {
                    j["sort"] = true.into();
                }
                if s.autoindex {
                    j["autoindex"] = true.into();
                }
                j["vm"] = s.vm.into();
                j
            })
            .collect()
    }

    fn record(&self, sql: &str, us: u64, runs: u64, scan: u64, sort: bool, autoindex: bool, vm: u64) {
        self.total_us.fetch_add(us, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        let key = normalize(sql);
        let runs = runs.max(1);
        {
            let mut m = self.stats.lock();
            if !m.contains_key(key.as_ref()) {
                let k = if m.len() >= MAX_STATEMENTS { "(other statements)".to_string() } else { key.to_string() };
                m.entry(k).or_default();
            }
            let e = match m.get_mut(key.as_ref()) {
                Some(e) => e,
                None => m.get_mut("(other statements)").expect("inserted"),
            };
            e.n += 1;
            e.us += us;
            e.max_us = e.max_us.max(us);
            // The counters are the statement's since it was prepared (cached statements
            // run many times): per run on average.
            e.scan = e.scan.max(scan / runs);
            e.sort |= sort;
            e.autoindex |= autoindex;
            e.vm = e.vm.max(vm / runs);
        }
        if us >= SLOW_US {
            let mut v = json!({"k": "slow_sql", "ms": us / 1000, "sql": short(sql, 1500), "th": thread_name(), "scan_rows": scan / runs, "vm": vm / runs});
            if sort {
                v["sort"] = true.into();
            }
            if autoindex {
                v["autoindex"] = true.into();
            }
            stamp(&mut v);
            {
                let mut s = self.slow.lock();
                if s.len() >= 100 {
                    s.pop_front();
                }
                s.push_back(v.clone());
            }
            let minute = crate::db::now_ms() / 60_000;
            let write = {
                let mut w = self.slow_written.lock();
                if w.0 != minute {
                    *w = (minute, 0);
                }
                w.1 += 1;
                w.1 <= 60
            };
            if write && let Some(t) = get() {
                t.event(v);
            }
        }
    }
}

/// Reports a connection's statements here (debug mode only; called as connections open).
pub fn install(c: &rusqlite::Connection) {
    if super::on() {
        c.trace_v2(TraceEventCodes::SQLITE_TRACE_PROFILE, Some(on_profile));
    }
}

fn on_profile(e: TraceEvent<'_>) {
    let TraceEvent::Profile(stmt, d) = e else { return };
    let Some(t) = get() else { return };
    let st = |s: StatementStatus| stmt.get_status(s).max(0) as u64;
    t.sql.record(&stmt.sql(), d.as_micros() as u64, st(StatementStatus::Run), st(StatementStatus::FullscanStep), st(StatementStatus::Sort) > 0, st(StatementStatus::AutoIndex) > 0, st(StatementStatus::VmStep));
}

/// A statement's text as one line, at most `max` characters.
pub fn short(sql: &str, max: usize) -> String {
    let one: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max { one } else { one.chars().take(max).collect::<String>() + "…" }
}

/// The text a statement is grouped by: one line, numbers as `N` (parameters `?1` too), a
/// list of them as one.
pub fn normalize(sql: &str) -> Cow<'_, str> {
    let plain = !sql.bytes().any(|b| b.is_ascii_digit() || b == b'\n' || b == b'\t' || b == b'\r') && !sql.contains("  ");
    if plain && sql.len() <= 600 {
        return Cow::Borrowed(sql);
    }
    let mut out = String::with_capacity(sql.len().min(640));
    // In a name (photos_vis2: its digits stay), in a number (it becomes one N).
    let (mut name, mut num) = (false, false);
    for c in sql.chars() {
        if c.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            (name, num) = (false, false);
            continue;
        }
        if c.is_ascii_digit() {
            if name {
                out.push(c);
            } else if !num {
                out.push('N');
                num = true;
            }
            continue;
        }
        num = false;
        name = c.is_alphanumeric() || c == '_';
        out.push(c);
        if out.len() >= 600 {
            break;
        }
    }
    // "IN (N, N, N)" → "IN (N)".
    while out.contains("N,N") || out.contains("N, N") {
        out = out.replace("N, N", "N").replace("N,N", "N");
    }
    Cow::Owned(out.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_group_without_their_numbers() {
        assert_eq!(normalize("SELECT 1"), "SELECT N");
        assert_eq!(normalize("SELECT id FROM photos WHERE id IN (1, 2, 3) LIMIT 2000"), "SELECT id FROM photos WHERE id IN (N) LIMIT N");
        assert_eq!(normalize("SELECT * FROM t WHERE a = ?1 AND b = ?2"), "SELECT * FROM t WHERE a = ?N AND b = ?N");
        assert_eq!(normalize("CREATE INDEX photos_vis2 ON photos(x)"), "CREATE INDEX photos_vis2 ON photos(x)");
        assert_eq!(normalize("SELECT a\n   FROM b"), "SELECT a FROM b");
        assert!(matches!(normalize("SELECT a FROM b"), Cow::Borrowed(_)));
        assert_eq!(short("SELECT  a\nFROM b", 100), "SELECT a FROM b");
        assert_eq!(short("abcdef", 3), "abc…");
    }

    #[test]
    fn statements_are_profiled() {
        let t = super::super::test_init();
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&dir.path().join("t.db")).unwrap();
        let c = pool.get().unwrap();
        c.execute_batch("CREATE TABLE telemetry_probe(x INTEGER); INSERT INTO telemetry_probe VALUES (1), (2), (3);").unwrap();
        for _ in 0..3 {
            let n: i64 = c.query_row("SELECT count(*) FROM telemetry_probe WHERE x > 0", [], |r| r.get(0)).unwrap();
            assert_eq!(n, 3);
        }
        let all: HashMap<String, Stmt> = t.sql.all().into_iter().collect();
        let s = all.get("SELECT count(*) FROM telemetry_probe WHERE x > N").expect("profiled");
        assert_eq!(s.n, 3);
        // A full scan of the three rows.
        assert!(s.scan >= 2, "{s:?}");
        assert!(!t.sql.top(5).is_empty());
    }
}
