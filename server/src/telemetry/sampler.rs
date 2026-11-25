//! Every `HYPERFOCAL_DEBUG_INTERVAL` seconds, one sample of everything worth watching over
//! time: the process (CPU, memory split into allocator, page cache and mapped files, I/O,
//! wake-ups), its threads by pool, its child processes, the container's cgroup and pressure,
//! the GPU, the caches, every background worker's progress, and what the HTTP routes, the
//! timed operations and the SQL statements did since the sample before. Written to the
//! metrics file and kept in memory for the debug panel.

use super::hist::Snap;
use super::store::Kind;
use super::system;
use super::{Telemetry, stamp};
use crate::state::AppState;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Samples kept in memory (an hour at 30 s).
const KEEP: usize = 120;

/// The raw counters of the sample before, to turn into rates.
#[derive(Default)]
pub struct Prev {
    at: Option<Instant>,
    cpu_ticks: (u64, u64),
    io: HashMap<String, u64>,
    ctx: (u64, u64),
    flt: (u64, u64),
    threads: HashMap<u64, (String, u64, u64, u64)>,
    children: HashMap<u64, u64>,
    cg_cpu: (u64, u64, u64),
    cg_io: (u64, u64),
    oom_kill: Option<u64>,
    tokio_busy: Vec<Duration>,
    http: HashMap<String, (Snap, u64, [u64; 5], u64, u64)>,
    ops: HashMap<&'static str, Snap>,
    counters: HashMap<&'static str, u64>,
    sql: HashMap<String, (u64, u64)>,
    logs: [u64; 5],
    streams: (u64, u64),
    gpu_at: Option<Instant>,
    previews_at: Option<Instant>,
    previews_mb: u64,
}

pub fn spawn(state: Arc<AppState>, rt: Option<tokio::runtime::Handle>) {
    let _ = std::thread::Builder::new().name("telemetry".into()).spawn(move || {
        let Some(t) = super::get() else { return };
        let mut prev = Prev::default();
        // A first reading, so the first sample has rates.
        let _ = sample(t, &state, rt.as_ref(), &mut prev);
        loop {
            std::thread::sleep(t.cfg.interval);
            let v = Arc::new(sample(t, &state, rt.as_ref(), &mut prev));
            t.store.line(Kind::Metrics, &v);
            let mut s = t.samples.lock();
            if s.len() >= KEEP {
                s.pop_front();
            }
            s.push_back(v);
            drop(s);
            t.logs.sweep();
        }
    });
}

fn mb(b: u64) -> f64 {
    (b as f64 / 1_048_576.0 * 10.0).round() / 10.0
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Takes one sample (and keeps what the next one needs in `prev`).
pub fn sample(t: &Telemetry, state: &AppState, rt: Option<&tokio::runtime::Handle>, prev: &mut Prev) -> Value {
    let now = Instant::now();
    let dt = prev.at.map(|p| now.duration_since(p).as_secs_f64()).unwrap_or(0.0).max(1e-3);
    let first = prev.at.is_none();
    prev.at = Some(now);
    let rate = |cur: u64, old: u64| if first { 0.0 } else { cur.saturating_sub(old) as f64 / dt };
    let ticks = system::clock_ticks() as f64;
    let mut v = json!({"k": "sample", "up": t.uptime().as_secs(), "dt": r1(dt)});

    // ---- the process
    let st = system::self_stat().unwrap_or_default();
    let status = system::self_status();
    let io = system::self_io();
    let kb = |k: &str| status.get(k).copied().unwrap_or(0) * 1024;
    let (ut, stt) = (st.utime, st.stime);
    let cpu_user = rate(ut, prev.cpu_ticks.0) / ticks * 100.0;
    let cpu_sys = rate(stt, prev.cpu_ticks.1) / ticks * 100.0;
    prev.cpu_ticks = (ut, stt);
    let io_rate = |k: &str| mb(rate(io.get(k).copied().unwrap_or(0), prev.io.get(k).copied().unwrap_or(0)) as u64);
    let ctx = (status.get("voluntary_ctxt_switches").copied().unwrap_or(0), status.get("nonvoluntary_ctxt_switches").copied().unwrap_or(0));
    v["proc"] = json!({
        "cpu": r1(cpu_user + cpu_sys),
        "user": r1(cpu_user),
        "sys": r1(cpu_sys),
        "rss_mb": mb(kb("VmRSS")),
        // Anonymous memory is the app's own; file pages are mapped files (SQLite's mmap,
        // RAW files read through memory maps), which the kernel can drop.
        "anon_mb": mb(kb("RssAnon")),
        "file_mb": mb(kb("RssFile")),
        "shmem_mb": mb(kb("RssShmem")),
        "hwm_mb": mb(kb("VmHWM")),
        "vsz_mb": mb(st.vsize),
        "swap_mb": mb(kb("VmSwap")),
        "threads": st.threads,
        "fds": system::fd_count(),
        // MB/s from storage (not the page cache), and through read()/write() in all.
        "read_mbs": io_rate("read_bytes"),
        "write_mbs": io_rate("write_bytes"),
        "rchar_mbs": io_rate("rchar"),
        "wchar_mbs": io_rate("wchar"),
        "majflt_s": r1(rate(st.majflt, prev.flt.1)),
        "minflt_s": r1(rate(st.minflt, prev.flt.0)),
        "ctx_s": r1(rate(ctx.0, prev.ctx.0)),
        "ctx_invol_s": r1(rate(ctx.1, prev.ctx.1)),
    });
    prev.io = io;
    prev.ctx = ctx;
    prev.flt = (st.minflt, st.majflt);
    v["malloc"] = system::malloc().unwrap_or(Value::Null);

    // ---- threads, by pool: CPU, time waiting for a CPU, wake-ups
    let mut groups: HashMap<String, (u64, f64, f64, f64)> = HashMap::new();
    let mut seen = HashMap::new();
    for th in system::threads() {
        let g = groups.entry(system::thread_group(&th.name)).or_default();
        g.0 += 1;
        // The same thread as before (a thread id can be reused by a new thread).
        if let Some((_, run, wait, slices)) = prev.threads.get(&th.tid).filter(|p| p.0 == th.name) {
            g.1 += th.run_ns.saturating_sub(*run) as f64;
            g.2 += th.wait_ns.saturating_sub(*wait) as f64;
            g.3 += th.slices.saturating_sub(*slices) as f64;
        }
        seen.insert(th.tid, (th.name, th.run_ns, th.wait_ns, th.slices));
    }
    prev.threads = seen;
    let mut tg: Vec<(String, (u64, f64, f64, f64))> = groups.into_iter().collect();
    tg.sort_by(|a, b| b.1.1.total_cmp(&a.1.1).then(a.0.cmp(&b.0)));
    let ns = dt * 1e9;
    v["threads"] = tg
        .iter()
        .map(|(name, (n, run, wait, slices))| json!({"name": name, "n": n, "cpu": r1(run / ns * 100.0), "wait": r1(wait / ns * 100.0), "wake_s": r1(slices / dt)}))
        .collect();

    // ---- child processes (the AI engine, ExifTool, the LibRaw helper)
    let page_ticks = |c: u64, old: Option<&u64>| if first { 0.0 } else { old.map(|o| c.saturating_sub(*o) as f64 / dt / ticks * 100.0).unwrap_or(0.0) };
    let kids = system::children();
    v["children"] = kids.iter().map(|c| json!({"pid": c.pid, "name": c.name, "rss_mb": mb(c.rss), "cpu": r1(page_ticks(c.ticks, prev.children.get(&c.pid))), "threads": c.threads})).collect();
    prev.children = kids.iter().map(|c| (c.pid, c.ticks)).collect();

    // ---- the container
    if let Some(cg) = system::cgroup() {
        let s = |k: &str| cg.stat.get(k).copied();
        let mut c = json!({
            "mem_mb": cg.mem.map(mb),
            "max_mb": cg.mem_max.map(mb),
            "peak_mb": cg.mem_peak.map(mb),
            // v2 names, then v1's.
            "anon_mb": s("anon").or(s("rss")).map(mb),
            "file_mb": s("file").or(s("cache")).map(mb),
            "kernel_mb": s("kernel").map(mb),
            "dirty_mb": s("file_dirty").or(s("dirty")).map(mb),
            "writeback_mb": s("file_writeback").or(s("writeback")).map(mb),
            "oom": cg.events.get("oom").copied(),
            "oom_kill": cg.events.get("oom_kill").copied(),
        });
        if let Some(k) = cg.events.get("oom_kill").copied() {
            if let Some(p) = prev.oom_kill
                && k > p
            {
                tracing::error!("debug: the kernel killed {} process(es) in this container for want of memory", k - p);
                t.event(json!({"k": "oom_kill", "count": k - p, "mem_mb": cg.mem.map(mb), "max_mb": cg.mem_max.map(mb)}));
            }
            prev.oom_kill = Some(k);
        }
        if cg.v2 {
            let u = cg.cpu.get("usage_usec").copied().unwrap_or(0);
            let th = cg.cpu.get("throttled_usec").copied().unwrap_or(0);
            let nth = cg.cpu.get("nr_throttled").copied().unwrap_or(0);
            c["cpu"] = r1(rate(u, prev.cg_cpu.0) / 1e6 * 100.0).into();
            c["throttled"] = r1(rate(th, prev.cg_cpu.1) / 1e6 * 100.0).into();
            c["throttled_n"] = r1(rate(nth, prev.cg_cpu.2) * dt).into();
            prev.cg_cpu = (u, th, nth);
            if let Some((r, w, devs)) = &cg.io {
                c["read_mbs"] = mb(rate(*r, prev.cg_io.0) as u64).into();
                c["write_mbs"] = mb(rate(*w, prev.cg_io.1) as u64).into();
                prev.cg_io = (*r, *w);
                c["io_devs"] = devs.iter().map(|(d, r, w)| json!({"dev": d, "read_gb": r >> 30, "write_gb": w >> 30})).collect();
            }
            c["psi"] = Value::Object(cg.psi.clone());
        }
        v["cg"] = c;
    }
    let mi = system::meminfo();
    let mk = |k: &str| mi.get(k).map(|v| v / 1024);
    v["host"] = json!({"load": system::loadavg(), "mem_total_mb": mk("MemTotal"), "mem_avail_mb": mk("MemAvailable"), "cached_mb": mk("Cached"), "dirty_mb": mk("Dirty"), "swap_free_mb": mk("SwapFree")});

    // ---- the GPU, every 2 minutes while the AI engine is loaded (a query can wake it)
    let ml = state.ml.status();
    let gpu_due = prev.gpu_at.is_none_or(|a| a.elapsed() >= Duration::from_secs(120));
    if ml.device.as_deref() == Some("cuda") && ml.phase == "ready" && gpu_due && system::has_nvidia_smi() {
        prev.gpu_at = Some(Instant::now());
        v["gpu"] = system::gpu().unwrap_or(Value::Null);
    }

    // ---- the async runtime
    if let Some(rt) = rt {
        let m = rt.metrics();
        let n = m.num_workers();
        let busy: Vec<Duration> = (0..n).map(|i| m.worker_total_busy_duration(i)).collect();
        let total: f64 = busy.iter().zip(prev.tokio_busy.iter().chain(std::iter::repeat(&Duration::ZERO))).map(|(b, p)| b.saturating_sub(*p).as_secs_f64()).sum();
        v["tokio"] = json!({"workers": n, "tasks": m.num_alive_tasks(), "queue": m.global_queue_depth(), "busy": if first { 0.0 } else { r1(total / dt / n.max(1) as f64 * 100.0) }});
        prev.tokio_busy = busy;
    }

    // ---- the index database, disks, caches, indexes in memory
    let pool = state.db.state();
    let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let db = state.cfg.config_dir.join("hyperfocal.db");
    v["db"] = json!({
        "conns": pool.connections, "idle": pool.idle_connections,
        "db_mb": mb(size(&db)),
        "wal_mb": mb(size(&state.cfg.config_dir.join("hyperfocal.db-wal"))),
    });
    let disk = |p: &std::path::Path| system::disk(p).map(|(free, total)| json!({"free_gb": r1(free as f64 / 1e9), "total_gb": r1(total as f64 / 1e9)}));
    if prev.previews_at.is_none_or(|a| a.elapsed() >= Duration::from_secs(600)) {
        prev.previews_at = Some(Instant::now());
        prev.previews_mb = state.images.previews_bytes() >> 20;
    }
    v["disk"] = json!({"config": disk(&state.cfg.config_dir), "photos": disk(&state.cfg.photos_root), "previews_mb": prev.previews_mb, "debug_mb": t.store.dir_bytes() >> 20});
    v["caches"] = state.images.cache_report();
    v["mem"] = memory_indexes(state);

    // ---- background work
    let progress = state.progress.read().clone();
    let people = {
        let p = state.people.status.read();
        json!({"phase": p.phase, "analyzed": p.analyzed, "pending": p.pending, "failed": p.failed, "rate": r1(p.rate)})
    };
    let integrity = {
        let s = state.integrity.status.read();
        json!({"phase": s.phase, "session": s.session})
    };
    v["work"] = json!({
        "scan": {"phase": progress.phase, "folders": progress.folders_done, "of": progress.folders_total, "indexed": progress.photos_indexed, "thumbs_queue": progress.thumbs_pending},
        "cull": crate::cull::status_json(state),
        "people": people,
        "visual": crate::visual::status_json(state),
        "dupes": serde_json::to_value(state.dupes.status.read().clone()).unwrap_or_default(),
        "integrity": integrity,
        "ml": {"phase": ml.phase, "device": ml.device, "clients": state.ml.link.clients()},
        "jobs": state.jobs.lock().values().filter(|j| !j.finished).count(),
        // Browsers with the event stream open (each gets every message).
        "browsers": t.http.open_streams(),
    });

    // ---- what happened since the sample before
    v["http"] = http_window(t, prev, first);
    let (opened, bytes) = t.http.stream_totals();
    v["streams"] = json!({"open": t.http.open_streams(), "opened": opened.saturating_sub(prev.streams.0), "kb": (bytes.saturating_sub(prev.streams.1)) / 1024});
    prev.streams = (opened, bytes);
    let mut ops = Map::new();
    for op in t.ops() {
        let s = op.h.snapshot();
        let w = s.since(prev.ops.get(op.name).unwrap_or(&Snap::default()), op.h.take_window_max());
        if w.n > 0 && !first {
            ops.insert(op.name.into(), w.json());
        }
        prev.ops.insert(op.name, s);
    }
    v["ops"] = Value::Object(ops);
    let mut counts = Map::new();
    for (name, n) in t.counters() {
        let d = n.saturating_sub(prev.counters.get(name).copied().unwrap_or(0));
        if d > 0 && !first {
            counts.insert(name.into(), d.into());
        }
        prev.counters.insert(name, n);
    }
    v["count"] = Value::Object(counts);
    v["sql"] = sql_window(t, prev, first);
    let logs: Vec<u64> = (0..5).map(|i| t.logs.count(i)).collect();
    let mut lw = Map::new();
    for (i, name) in super::logs::LEVELS.iter().enumerate() {
        let d = logs[i].saturating_sub(prev.logs[i]);
        if d > 0 && !first {
            lw.insert((*name).into(), d.into());
        }
        prev.logs[i] = logs[i];
    }
    v["logs"] = Value::Object(lw);
    v["store"] = json!({"written": t.store.written.load(std::sync::atomic::Ordering::Relaxed), "dropped": t.store.dropped.load(std::sync::atomic::Ordering::Relaxed)});
    stamp(&mut v);
    v
}

/// Memory held by the in-memory indexes (faces and pets, search by content).
fn memory_indexes(state: &AppState) -> Value {
    let people = state.people.engine.try_lock().map(|e| e.as_ref().map(|e| (e.faces.len(), e.pets.len(), e.mem_bytes())));
    let visual = state.visual.index.try_read().map(|i| (i.len(), i.mem_bytes()));
    json!({
        "faces": people.and_then(|p| p.map(|p| p.0)),
        "pets": people.and_then(|p| p.map(|p| p.1)),
        "people_mb": people.and_then(|p| p.map(|p| mb(p.2 as u64))),
        "visual": visual.map(|v| v.0),
        "visual_mb": visual.map(|v| mb(v.1 as u64)),
    })
}

fn http_window(t: &Telemetry, prev: &mut Prev, first: bool) -> Value {
    let mut out: Vec<(String, f64, Value)> = Vec::new();
    for (route, r) in t.http.routes() {
        let s = r.h.snapshot();
        let bytes = r.bytes();
        let status = r.statuses();
        let (aborted, cancelled) = (r.aborted(), r.cancelled());
        let wmax = r.h.take_window_max();
        let empty = (Snap::default(), 0, [0; 5], 0, 0);
        let p = prev.http.get(&route).unwrap_or(&empty);
        let w = s.since(&p.0, wmax);
        let c = cancelled.saturating_sub(p.4);
        if (w.n > 0 || c > 0) && !first {
            let mut j = w.json();
            j["kb"] = ((bytes.saturating_sub(p.1)) / 1024).into();
            let errs: u64 = (3..5).map(|i| status[i].saturating_sub(p.2[i])).sum();
            if errs > 0 {
                j["4xx"] = status[3].saturating_sub(p.2[3]).into();
                j["5xx"] = status[4].saturating_sub(p.2[4]).into();
            }
            let a = aborted.saturating_sub(p.3);
            if a > 0 {
                j["aborted"] = a.into();
            }
            if c > 0 {
                j["cancelled"] = c.into();
            }
            out.push((route.clone(), w.sum as f64, j));
        }
        prev.http.insert(route, (s, bytes, status, aborted, cancelled));
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    Value::Object(out.into_iter().map(|(k, _, v)| (k, v)).collect())
}

/// The statements that took the most time since the sample before (top 10).
fn sql_window(t: &Telemetry, prev: &mut Prev, first: bool) -> Value {
    let mut out: Vec<(u64, Value)> = Vec::new();
    for (sql, s) in t.sql.all() {
        let (pn, pus) = prev.sql.get(&sql).copied().unwrap_or((0, 0));
        let (n, us) = (s.n.saturating_sub(pn), s.us.saturating_sub(pus));
        if n > 0 && !first {
            out.push((us, json!({"sql": super::sql::short(&sql, 160), "n": n, "ms": us / 1000})));
        }
        prev.sql.insert(sql, (s.n, s.us));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    Value::Array(out.into_iter().take(10).map(|x| x.1).collect())
}
