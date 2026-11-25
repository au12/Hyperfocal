//! What debug mode hands over: the live view for Settings → Debug, a one-page summary to
//! paste into a conversation, and the report bundle (a .tar.gz with the summary, a full
//! JSON report, a census of the library and its errors, and the telemetry files).

use super::{Telemetry, iso, store};
use crate::config::Config;
use crate::state::AppState;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom, Write};

// ---------------------------------------------------------------- settings and environment

/// Hides what must not leave the server: passwords, tokens, keys, and the path of an
/// alert URL (an ntfy topic is a secret of its own).
fn secret(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    ["PASSWORD", "TOKEN", "KEY", "SECRET"].iter().any(|s| n.contains(s))
}

fn redact_url(u: &str) -> String {
    match u.find("://").map(|i| i + 3) {
        Some(start) => {
            let host_end = u[start..].find('/').map(|i| start + i).unwrap_or(u.len());
            let host = &u[start..host_end];
            let host = host.rsplit('@').next().unwrap_or(host);
            format!("{}{host}{}", &u[..start], if host_end < u.len() { "/…" } else { "" })
        }
        None => "(set)".into(),
    }
}

/// The settings in effect (secrets only as set or not).
pub fn config_json(c: &Config) -> Value {
    let m = &c.ml;
    json!({
        "photos_root": c.photos_root, "config_dir": c.config_dir, "cache_dir": c.cache_dir, "web_dir": c.web_dir, "port": c.port,
        "readonly": c.readonly, "reject_dir": c.reject_dir, "rescan_minutes": c.rescan_minutes,
        "pregen_thumbs": c.pregen_thumbs, "pregen_previews": c.pregen_previews, "pregen_days": c.pregen_days,
        "raw_cache_mb": c.raw_cache_mb, "preview_cache_mb": c.preview_cache_mb, "index_threads": c.index_threads, "decode_concurrency": c.decode_concurrency,
        "password": c.password.is_some(), "exiftool": c.exiftool, "rename_sidecars": c.rename_sidecars, "raw_decoder": if c.raw_decoder_libraw { "libraw" } else { "auto" },
        "integrity": c.integrity, "cull": c.cull, "verify_days": c.verify_days, "verify_hour": c.verify_hour, "verify_minutes": c.verify_minutes, "hash_mbps": c.hash_mbps,
        "alert_url": c.alert_url.as_deref().map(redact_url), "alert_format": c.alert_format, "alert_token": c.alert_token.is_some(),
        "ml": {
            "enabled": m.enabled, "device": format!("{:?}", m.device), "gpu": m.gpu, "gpu_mem_mb": m.gpu_mem_mb, "threads": m.threads, "workers": m.workers,
            "faces": m.faces, "search": m.search, "eyes": m.eyes, "pet_detector": m.pet_detector, "face_scales": m.face_scales, "dir": m.dir,
            "idle_min": m.idle_min, "idle_no_clients_min": m.idle_no_clients_min, "legacy_url": m.legacy_url.is_some(),
        },
        "ml_size": c.ml_size, "ml_concurrency": c.ml_concurrency, "people_xmp": c.people_xmp, "dupes": c.dupes,
        "immich_url": c.immich_url.as_deref().map(redact_url), "immich_api_key": c.immich_api_key.is_some(),
        "immich_public_url": c.immich_public_url.as_deref().map(redact_url), "immich_library_path": c.immich_library_path,
        "debug": {"dir": c.debug.dir, "max_mb": c.debug.max_bytes >> 20, "days": c.debug.days, "interval_s": c.debug.interval.as_secs(), "trace": c.debug.trace},
    })
}

/// The container's environment that concerns Hyperfocal (secrets as "(set)").
pub fn env_json() -> Value {
    let mut out = Map::new();
    let mut vars: Vec<(String, String)> = std::env::vars().collect();
    vars.sort();
    for (k, v) in vars {
        let ours = k.starts_with("HYPERFOCAL_") || k.starts_with("NVIDIA_") || k.starts_with("CUDA_") || ["TZ", "PUID", "PGID", "UMASK", "RUST_LOG", "RUST_BACKTRACE", "LD_LIBRARY_PATH"].contains(&k.as_str());
        if !ours || k == "HYPERFOCAL_ML_ENGINE" {
            continue;
        }
        let shown = if secret(&k) {
            "(set)".to_string()
        } else if k.ends_with("_URL") {
            redact_url(&v)
        } else {
            v
        };
        out.insert(k, shown.into());
    }
    Value::Object(out)
}

// ---------------------------------------------------------------- live view

fn top_ops(t: &Telemetry, n: usize) -> Vec<Value> {
    let mut ops: Vec<_> = t.ops().into_iter().map(|o| (o, o.h.snapshot())).filter(|(_, s)| s.n > 0).collect();
    ops.sort_by(|a, b| b.1.sum.cmp(&a.1.sum));
    ops.into_iter()
        .take(n)
        .map(|(o, s)| {
            let mut j = s.json();
            j["name"] = o.name.into();
            j["slow_ms"] = o.slow_ms().into();
            let slow = o.slow();
            if !slow.is_empty() {
                j["slow"] = slow.into();
            }
            j
        })
        .collect()
}

fn top_routes(t: &Telemetry, n: usize) -> Vec<Value> {
    let mut routes: Vec<_> = t.http.routes().into_iter().map(|(k, r)| (k, r.h.snapshot().sum, r)).filter(|r| r.2.h.count() > 0 || r.2.cancelled() > 0).collect();
    routes.sort_by(|a, b| b.1.cmp(&a.1));
    routes
        .into_iter()
        .take(n)
        .map(|(k, _, r)| {
            let mut j = r.json();
            j["route"] = k.into();
            let slowest = r.slowest();
            if !slowest.is_empty() {
                j["slowest"] = slowest.into();
            }
            j
        })
        .collect()
}

/// Everything the debug panel shows, now.
pub fn live(t: &Telemetry) -> Value {
    let history: Vec<Value> = t
        .samples
        .lock()
        .iter()
        .map(|s| {
            json!({
                "t": s["t"], "cpu": s["proc"]["cpu"], "rss_mb": s["proc"]["rss_mb"], "anon_mb": s["proc"]["anon_mb"],
                "heap_mb": s["malloc"]["in_use_mb"], "free_mb": s["malloc"]["free_mb"], "cg_mb": s["cg"]["mem_mb"],
                "read_mbs": s["proc"]["read_mbs"], "children_mb": s["children"].as_array().map(|c| c.iter().filter_map(|x| x["rss_mb"].as_f64()).sum::<f64>()),
            })
        })
        .collect();
    let counts: Map<String, Value> = super::logs::LEVELS.iter().enumerate().map(|(i, l)| ((*l).to_string(), t.logs.count(i).into())).collect();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "since": t.started_ms,
        "uptime_s": t.uptime().as_secs(),
        "interval": t.cfg.interval.as_secs(),
        "store": {
            "dir": t.store.dir, "mb": t.store.dir_bytes() >> 20, "max_mb": t.cfg.max_bytes >> 20, "days": t.cfg.days, "over_cap": t.store.over_cap(),
            "written": t.store.written.load(std::sync::atomic::Ordering::Relaxed), "dropped": t.store.dropped.load(std::sync::atomic::Ordering::Relaxed),
        },
        "latest": t.latest().map(|s| (*s).clone()),
        "history": history,
        "logs": {"counts": counts, "sites": t.logs.sites(1).into_iter().take(15).collect::<Vec<_>>(), "problems": t.logs.problems(15)},
        "panics": t.logs.panics.lock().clone(),
        "http": {"routes": top_routes(t, 15), "errors": t.http.errors(10), "slow": t.http.slow(10), "open_streams": t.http.open_streams()},
        "ops": top_ops(t, 25),
        "sql": {
            "count": t.sql.count.load(std::sync::atomic::Ordering::Relaxed),
            "ms": t.sql.total_us.load(std::sync::atomic::Ordering::Relaxed) / 1000,
            "top": t.sql.top(10),
            "slow": t.sql.slow(10),
        },
        "clients": t.clients.summaries(),
        "marks": t.marks.lock().iter().rev().take(20).cloned().collect::<Vec<_>>(),
        "previous": t.previous.lock().clone(),
    })
}

// ---------------------------------------------------------------- summary

fn size(mb: f64) -> String {
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

fn dur(ms: f64) -> String {
    if ms >= 10_000.0 {
        format!("{:.0} s", ms / 1000.0)
    } else if ms >= 1000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        format!("{ms:.0} ms")
    }
}

fn hms(s: u64) -> String {
    match s {
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86_400 => format!("{} h {} min", s / 3600, s % 3600 / 60),
        s => format!("{} d {} h", s / 86_400, s % 86_400 / 3600),
    }
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

/// One page of plain text with what matters most, to read or paste into a conversation.
pub fn summary(t: &Telemetry, state: &AppState) -> String {
    let mut o = String::new();
    let now = crate::db::now_ms();
    let _ = writeln!(o, "# Hyperfocal debug summary\n");
    let _ = writeln!(o, "{} · Hyperfocal {} · up {} · debug mode since {}", iso(now), env!("CARGO_PKG_VERSION"), hms(t.uptime().as_secs()), iso(t.started_ms));
    if let Some(p) = &*t.previous.lock() {
        let _ = writeln!(
            o,
            "\n**The run before this one did not shut down cleanly** (started {}; last sample: RSS {}, container {}). A crash, an out-of-memory kill or docker kill.",
            p["started"].as_i64().map(iso).unwrap_or_default(),
            size(f(&p["last_sample"]["proc"]["rss_mb"])),
            size(f(&p["last_sample"]["cg"]["mem_mb"])),
        );
    }
    let latest = t.latest().map(|s| (*s).clone()).unwrap_or(Value::Null);
    let w = &latest["work"];
    let _ = writeln!(o, "\n## Library and background work\n");
    let pr = state.progress.read().clone();
    let _ = writeln!(o, "- Scan: {} ({} of {} folders, {} photos indexed this run); {} thumbnails queued", pr.phase, pr.folders_done, pr.folders_total, pr.photos_indexed, pr.thumbs_pending);
    let cull = crate::cull::status_json(state);
    let _ = writeln!(o, "- Culling: {} {} of {} ({:.0}/min)", cull["phase"].as_str().unwrap_or(""), cull["done"], cull["total"], f(&cull["rate"]));
    let p = state.people.status.read().clone();
    let _ = writeln!(o, "- People & pets: {} — {} analysed, {} to do, {} failed ({:.0}/min)", p.phase, p.analyzed, p.pending, p.failed, p.rate);
    let v = crate::visual::status_json(state);
    let _ = writeln!(o, "- Search by content: {} {} of {}", v["phase"].as_str().unwrap_or(""), v["done"], v["total"]);
    let d = state.dupes.status.read().clone();
    let _ = writeln!(o, "- Duplicates: {} ({} hashed this pass, {} to do)", d.phase, d.done, d.pending);
    let i = state.integrity.status.read().clone();
    let _ = writeln!(o, "- Integrity: {}", i.phase);
    let ml = state.ml.status();
    match ml.device.as_deref() {
        Some(dev) if ml.phase != "off" => {
            let _ = writeln!(o, "- AI engine: {} on {dev}{} · {} browser(s) connected", ml.phase, ml.gpu.as_deref().map(|g| format!(" ({g})")).unwrap_or_default(), state.ml.link.clients());
        }
        _ => {
            let _ = writeln!(o, "- AI engine: {}", ml.phase);
        }
    }

    if !latest.is_null() {
        let pc = &latest["proc"];
        let _ = writeln!(o, "\n## Resources (sample at {})\n", latest["ts"].as_str().unwrap_or(""));
        let _ = writeln!(
            o,
            "- Process: CPU {:.0}% (user {:.0}, sys {:.0}; 100% = one core) · RSS {} (own {}, mapped files {}; peak {}) · {} threads · {} files open · reads {:.1} MB/s from disk",
            f(&pc["cpu"]), f(&pc["user"]), f(&pc["sys"]), size(f(&pc["rss_mb"])), size(f(&pc["anon_mb"])), size(f(&pc["file_mb"])), size(f(&pc["hwm_mb"])), pc["threads"], pc["fds"], f(&pc["read_mbs"])
        );
        let m = &latest["malloc"];
        if m.is_object() {
            let _ = writeln!(o, "- Allocator: {} in use, {} freed but held (malloc_trim could return part of it)", size(f(&m["in_use_mb"])), size(f(&m["free_mb"])));
        }
        let cg = &latest["cg"];
        if cg.is_object() {
            let max = cg["max_mb"].as_f64().map(|m| format!(" of {}", size(m))).unwrap_or_else(|| " (no limit set)".into());
            let _ = writeln!(
                o,
                "- Container: {}{max} (page cache {}) · out-of-memory kills {} · CPU {:.0}%, throttled {:.0}%",
                size(f(&cg["mem_mb"])), size(f(&cg["file_mb"])), cg["oom_kill"].as_u64().unwrap_or(0), f(&cg["cpu"]), f(&cg["throttled"])
            );
            let psi = &cg["psi"];
            if psi.is_object() {
                let _ = writeln!(o, "- Pressure (some, 60 s avg): CPU {:.1}% · memory {:.1}% · I/O {:.1}%", f(&psi["cpu"]["some60"]), f(&psi["memory"]["some60"]), f(&psi["io"]["some60"]));
            }
        }
        if let Some(th) = latest["threads"].as_array() {
            let busy: Vec<String> = th.iter().filter(|x| f(&x["cpu"]) >= 1.0).take(8).map(|x| format!("{} {:.0}%", x["name"].as_str().unwrap_or("?"), f(&x["cpu"]))).collect();
            if !busy.is_empty() {
                let _ = writeln!(o, "- Busiest threads: {}", busy.join(" · "));
            }
            let wakes: f64 = th.iter().map(|x| f(&x["wake_s"])).sum();
            let _ = writeln!(o, "- Wake-ups: {wakes:.0}/s in all");
        }
        if let Some(ch) = latest["children"].as_array().filter(|c| !c.is_empty()) {
            let c: Vec<String> = ch.iter().map(|x| format!("{} {}, {:.0}% CPU", x["name"].as_str().unwrap_or("?"), size(f(&x["rss_mb"])), f(&x["cpu"]))).collect();
            let _ = writeln!(o, "- Child processes: {}", c.join(" · "));
        }
        if let Some(g) = latest["gpu"].as_array() {
            for x in g {
                let _ = writeln!(o, "- GPU: {} {:.0}% busy, {:.0} of {:.0} MB, {:.0} °C, {:.0} W", x["name"].as_str().unwrap_or("?"), f(&x["util"]), f(&x["mem_used_mb"]), f(&x["mem_total_mb"]), f(&x["temp"]), f(&x["power_w"]));
            }
        }
        let c = &latest["caches"];
        if let Some(obj) = c.as_object() {
            let parts: Vec<String> = obj.iter().filter_map(|(k, v)| v["mb"].as_f64().filter(|m| *m >= 1.0).map(|m| format!("{k} {}", size(m)))).collect();
            if !parts.is_empty() {
                let _ = writeln!(o, "- Memory caches: {}", parts.join(" · "));
            }
        }
        let mem = &latest["mem"];
        if mem["people_mb"].is_number() || mem["visual_mb"].is_number() {
            let _ = writeln!(o, "- Indexes in memory: faces and pets {} · search {}", size(f(&mem["people_mb"])), size(f(&mem["visual_mb"])));
        }
        let _ = writeln!(o, "- Work: {}", serde_json::to_string(&w).unwrap_or_default());
    }

    let _ = writeln!(o, "\n## Problems\n");
    let counts: Vec<u64> = (0..5).map(|i| t.logs.count(i)).collect();
    let _ = writeln!(
        o,
        "- Log: {} errors, {} warnings since debug mode started · panics {} · HTTP errors {} · browser errors {}",
        counts[0],
        counts[1],
        t.logs.panics.lock().len(),
        t.http.errors(1000).len(),
        t.clients.error_count()
    );
    for s in t.logs.sites(1).into_iter().take(12) {
        let sample = s["samples"].as_array().and_then(|a| a.last()).and_then(Value::as_str).unwrap_or("");
        let _ = writeln!(o, "- {} ×{} at {}: {}", s["lvl"].as_str().unwrap_or(""), s["n"], s["at"].as_str().unwrap_or(""), super::sql::short(sample, 220));
    }
    for p in t.logs.panics.lock().iter().take(5) {
        let _ = writeln!(o, "- PANIC in {} at {}: {}", p["th"].as_str().unwrap_or("?"), p["at"].as_str().unwrap_or("?"), p["msg"].as_str().unwrap_or(""));
    }
    for e in t.http.errors(5) {
        let _ = writeln!(o, "- HTTP {} {}: {}", e["status"], e["uri"].as_str().unwrap_or(""), super::sql::short(e["error"].as_str().unwrap_or(""), 200));
    }

    let routes = top_routes(t, 12);
    if !routes.is_empty() {
        let _ = writeln!(o, "\n## HTTP routes (by total time, since debug mode started)\n");
        let _ = writeln!(o, "| route | n | p50 | p95 | max | MB | aborted | cancelled |\n|---|---|---|---|---|---|---|---|");
        for r in routes {
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                r["route"].as_str().unwrap_or(""), r["n"], dur(f(&r["p50"])), dur(f(&r["p95"])), dur(f(&r["max"])), r["mb"], r["aborted"].as_u64().unwrap_or(0), r["cancelled"].as_u64().unwrap_or(0)
            );
        }
    }
    let ops = top_ops(t, 20);
    if !ops.is_empty() {
        let _ = writeln!(o, "\n## Timed operations (by total time)\n");
        let _ = writeln!(o, "| operation | n | total | p50 | p95 | max | errors |\n|---|---|---|---|---|---|---|");
        for x in ops {
            let _ = writeln!(o, "| {} | {} | {} | {} | {} | {} | {} |", x["name"].as_str().unwrap_or(""), x["n"], dur(f(&x["ms"])), dur(f(&x["p50"])), dur(f(&x["p95"])), dur(f(&x["max"])), x["err"].as_u64().unwrap_or(0));
        }
    }
    let sql = t.sql.top(10);
    if !sql.is_empty() {
        let _ = writeln!(o, "\n## SQL (by total time)\n");
        let _ = writeln!(o, "| total | n | avg | max | full scan rows | statement |\n|---|---|---|---|---|---|");
        for s in sql {
            let _ = writeln!(o, "| {} | {} | {:.1} ms | {} | {} | `{}` |", dur(f(&s["ms"])), s["n"], f(&s["avg_ms"]), dur(f(&s["max_ms"])), s["scan_rows"].as_u64().unwrap_or(0), super::sql::short(s["sql"].as_str().unwrap_or(""), 140).replace('|', "\\|"));
        }
    }
    let clients = t.clients.summaries();
    if !clients.is_empty() {
        let _ = writeln!(o, "\n## Browsers\n");
        for c in clients {
            let env = &c["env"];
            let _ = writeln!(
                o,
                "- {} ({}{}, {} cores, {}x) — last report {}",
                super::sql::short(env["ua"].as_str().unwrap_or("?"), 120),
                if env["mobile"] == true { "touch" } else { "desktop" },
                env["gpu"].as_str().map(|g| format!(", {}", super::sql::short(g, 60))).unwrap_or_default(),
                env["cores"],
                env["dpr"],
                c["last"].as_str().unwrap_or("")
            );
            let tm = &c["timings"];
            let mut bits = Vec::new();
            for k in ["viewer.show.raw", "viewer.show.preview", "viewer.show.full", "viewer.thumb", "thumb.load"] {
                if tm[k].is_object() {
                    bits.push(format!("{k} p50 {} p95 {} (n {})", dur(f(&tm[k]["p50"])), dur(f(&tm[k]["p95"])), tm[k]["n"]));
                }
            }
            if !bits.is_empty() {
                let _ = writeln!(o, "  - {}", bits.join(" · "));
            }
            let errs = c["errors"].as_array().map(|e| e.iter().map(|x| x["n"].as_u64().unwrap_or(0)).sum::<u64>()).unwrap_or(0);
            let frames: Vec<String> = c["frames"].as_object().map(|m| m.iter().map(|(k, v)| format!("{k} {}% dropped (worst {} ms)", v["dropped_pct"], v["worst_ms"])).collect()).unwrap_or_default();
            let net_mb: f64 = c["res"].as_object().map(|m| m.values().map(|v| f(&v["mb"])).sum()).unwrap_or(0.0);
            let _ = writeln!(
                o,
                "  - errors {errs} · long tasks {} ({} s) · {} · JS heap {} · received {} · battery {}",
                c["long"]["n"],
                c["long"]["s"],
                if frames.is_empty() { "no frame data".into() } else { frames.join(", ") },
                c["mem"]["js_used_mb"].as_f64().map(size).unwrap_or_else(|| "n/a".into()),
                size(net_mb),
                c["battery"]
                    .as_object()
                    .map(|b| format!("{:.0}%{}{}", f(&b["level"]) * 100.0, if b["charging"] == true { " charging" } else { "" }, b["drain_pct_h"].as_f64().map(|d| format!(", −{d}%/h")).unwrap_or_default()))
                    .unwrap_or_else(|| "n/a".into())
            );
            for e in c["errors"].as_array().into_iter().flatten().take(5) {
                let _ = writeln!(o, "  - {} ×{}: {}", e["kind"].as_str().unwrap_or(""), e["n"], super::sql::short(e["msg"].as_str().unwrap_or(""), 200));
            }
        }
    }
    let marks: Vec<Value> = t.marks.lock().iter().cloned().collect();
    if !marks.is_empty() {
        let _ = writeln!(o, "\n## Marked moments\n");
        for m in marks.iter().rev().take(15) {
            let _ = writeln!(o, "- {} “{}” {}", m["ts"].as_str().unwrap_or(""), m["note"].as_str().unwrap_or(""), serde_json::to_string(&m["ctx"]).unwrap_or_default());
        }
    }
    o
}

// ---------------------------------------------------------------- census

/// An error message with the parts that differ between files taken out (paths, numbers),
/// so the same failure on 3,000 files is one line.
pub fn normalize_error(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len().min(200));
    for word in msg.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let trimmed = word.trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ':' || c == '(' || c == ')');
        if trimmed.starts_with('/') && trimmed.len() > 1 {
            out.push_str("<path>");
            if word.ends_with(':') {
                out.push(':');
            }
            continue;
        }
        let mut in_num = false;
        for c in word.chars() {
            if c.is_ascii_digit() {
                if !in_num {
                    out.push('#');
                    in_num = true;
                }
            } else {
                in_num = false;
                out.push(c);
            }
        }
        if out.len() > 220 {
            break;
        }
    }
    out
}

fn query_rows(c: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let Ok(mut st) = c.prepare(sql) else { return Vec::new() };
    let n = st.column_count();
    let Ok(rows) = st.query_map([], |r| {
        Ok((0..n)
            .map(|i| match r.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Integer(x)) => json!(x),
                Ok(rusqlite::types::ValueRef::Real(x)) => json!(x),
                Ok(rusqlite::types::ValueRef::Text(b)) => json!(String::from_utf8_lossy(b)),
                Ok(rusqlite::types::ValueRef::Blob(b)) => json!(format!("<{} bytes>", b.len())),
                _ => Value::Null,
            })
            .collect::<Vec<_>>())
    }) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

fn one(c: &Connection, sql: &str) -> Value {
    query_rows(c, sql).into_iter().next().and_then(|r| r.into_iter().next()).unwrap_or(Value::Null)
}

/// Errors of one kind (cull, analysis...) grouped by message, extension and camera, with a
/// few example files each.
fn error_groups(c: &Connection, source: &str, sql: &str) -> Value {
    struct G {
        n: u64,
        msg: String,
        examples: Vec<String>,
    }
    let mut groups: HashMap<(String, String, String), G> = HashMap::new();
    let mut total = 0u64;
    for r in query_rows(c, sql) {
        let msg = r.first().and_then(Value::as_str).unwrap_or("").to_string();
        let ext = r.get(1).and_then(Value::as_str).unwrap_or("").to_lowercase();
        let model = r.get(2).and_then(Value::as_str).unwrap_or("").to_string();
        let path = r.get(3).and_then(Value::as_str).unwrap_or("").to_string();
        total += 1;
        let key = (normalize_error(&msg), ext, model);
        let g = groups.entry(key).or_insert_with(|| G { n: 0, msg: msg.chars().take(300).collect(), examples: Vec::new() });
        g.n += 1;
        if g.examples.len() < 3 {
            g.examples.push(path);
        }
    }
    let mut v: Vec<_> = groups.into_iter().collect();
    v.sort_by(|a, b| b.1.n.cmp(&a.1.n));
    json!({
        "source": source,
        "total": total,
        "groups": v.into_iter().take(80).map(|((norm, ext, model), g)| json!({"n": g.n, "error": norm, "example_message": g.msg, "ext": ext, "camera": model, "examples": g.examples})).collect::<Vec<_>>(),
    })
}

/// The library, how far each background pass has got, every recorded per-file error, and
/// the index database's size: from the index (a few seconds on a large library).
pub fn census(state: &AppState) -> Value {
    let Ok(c) = state.db.get() else { return json!({"error": "no database connection"}) };
    let t0 = std::time::Instant::now();
    let rows = |sql: &str| query_rows(&c, sql);
    let path = "f.path || '/' || p.name";
    let library = json!({
        "photos": one(&c, "SELECT count(*) FROM photos"),
        "shown": one(&c, "SELECT count(*) FROM photos WHERE pair_of IS NULL"),
        "pairs": one(&c, "SELECT count(*) FROM photos WHERE pair_of IS NOT NULL"),
        "with_sidecar": one(&c, "SELECT count(*) FROM photos WHERE sidecar IS NOT NULL"),
        "folders": one(&c, "SELECT count(*) FROM folders"),
        "reject_folders": one(&c, "SELECT count(*) FROM folders WHERE is_reject = 1"),
        "rejected_photos": one(&c, "SELECT count(*) FROM photos p JOIN folders f ON f.id = p.folder_id WHERE f.is_reject = 1"),
        "geotagged": one(&c, "SELECT count(*) FROM photos WHERE lat IS NOT NULL"),
        "no_date": one(&c, "SELECT count(*) FROM photos WHERE taken IS NULL"),
        "by_type": rows("SELECT lower(ext), kind, count(*), round(sum(size) / 1048576.0) AS mb FROM photos GROUP BY 1, 2 ORDER BY 3 DESC").into_iter()
            .map(|r| json!({"ext": r[0], "kind": r[1], "n": r[2], "mb": r[3]})).collect::<Vec<_>>(),
        // The camera audit: which bodies, in which formats (each one is a decoder path).
        "by_camera": rows("SELECT COALESCE(make, ''), COALESCE(model, ''), lower(ext), count(*), min(taken), max(taken) FROM photos GROUP BY 1, 2, 3 ORDER BY 4 DESC LIMIT 200").into_iter()
            .map(|r| json!({"make": r[0], "model": r[1], "ext": r[2], "n": r[3], "from": r[4].as_i64().map(|s| iso(s * 1000)), "to": r[5].as_i64().map(|s| iso(s * 1000))})).collect::<Vec<_>>(),
        "by_year": rows("SELECT COALESCE(strftime('%Y', taken, 'unixepoch'), 'none'), count(*) FROM photos GROUP BY 1 ORDER BY 1").into_iter()
            .map(|r| json!([r[0], r[1]])).collect::<Vec<_>>(),
        "largest_folders": rows("SELECT path, photo_count FROM folders ORDER BY photo_count DESC LIMIT 15").into_iter().map(|r| json!([r[0], r[1]])).collect::<Vec<_>>(),
    });
    let originals = rows("SELECT status, count(*) FROM originals GROUP BY status").into_iter().map(|r| json!({"status": r[0], "n": r[1]})).collect::<Vec<_>>();
    let coverage = json!({
        "thumbnails": state.images.thumbs.stats().entries,
        "cull": {"rows": one(&c, "SELECT count(*) FROM cull"), "errors": one(&c, "SELECT count(*) FROM cull WHERE error IS NOT NULL"), "with_issues": one(&c, "SELECT count(*) FROM cull WHERE issues != 0")},
        "people": {"analysed": one(&c, "SELECT count(*) FROM analysis"), "errors": one(&c, "SELECT count(*) FROM analysis WHERE error IS NOT NULL"),
            "detections": rows("SELECT kind, count(*) FROM detections GROUP BY kind").into_iter().map(|r| json!([r[0], r[1]])).collect::<Vec<_>>(),
            "subjects": one(&c, "SELECT count(*) FROM subjects"), "named": one(&c, "SELECT count(*) FROM subjects WHERE name IS NOT NULL")},
        "search": {"rows": one(&c, "SELECT count(*) FROM clip"), "errors": one(&c, "SELECT count(*) FROM clip WHERE emb IS NULL")},
        "dupes": {"rows": one(&c, "SELECT count(*) FROM phashes"), "errors": one(&c, "SELECT count(*) FROM phashes WHERE error IS NOT NULL")},
        "integrity": originals,
    });
    let limit = 200_000;
    let errors = json!([
        error_groups(&c, "metadata (scan)", &format!("SELECT p.meta_err, p.ext, p.model, {path} FROM photos p JOIN folders f ON f.id = p.folder_id WHERE p.meta_err IS NOT NULL LIMIT {limit}")),
        error_groups(&c, "culling", &format!("SELECT x.error, p.ext, p.model, {path} FROM cull x JOIN photos p ON p.id = x.photo_id JOIN folders f ON f.id = p.folder_id WHERE x.error IS NOT NULL LIMIT {limit}")),
        error_groups(&c, "people & pets", &format!("SELECT x.error, p.ext, p.model, {path} FROM analysis x JOIN photos p ON p.id = x.photo_id JOIN folders f ON f.id = p.folder_id WHERE x.error IS NOT NULL LIMIT {limit}")),
        error_groups(&c, "search by content", &format!("SELECT x.error, p.ext, p.model, {path} FROM clip x JOIN photos p ON p.id = x.photo_id JOIN folders f ON f.id = p.folder_id WHERE x.error IS NOT NULL LIMIT {limit}")),
        error_groups(&c, "duplicates", &format!("SELECT x.error, p.ext, p.model, {path} FROM phashes x JOIN photos p ON p.id = x.photo_id JOIN folders f ON f.id = p.folder_id WHERE x.error IS NOT NULL LIMIT {limit}")),
        error_groups(&c, "integrity", &format!("SELECT detail, lower(replace(name, rtrim(name, replace(name, '.', '')), '')), 'status ' || status, folder || '/' || name FROM originals WHERE status != 0 LIMIT {limit}")),
    ]);
    let tables: Vec<Value> = rows("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .into_iter()
        .filter_map(|r| r[0].as_str().map(str::to_string))
        .filter(|n| !n.contains('"'))
        .map(|n| json!([n, one(&c, &format!("SELECT count(*) FROM \"{n}\""))]))
        .collect();
    let db = json!({
        "sqlite": one(&c, "SELECT sqlite_version()"),
        "page_size": one(&c, "PRAGMA page_size"),
        "pages": one(&c, "PRAGMA page_count"),
        "free_pages": one(&c, "PRAGMA freelist_count"),
        "tables": tables,
        // Space per table and index (walks every page: a few seconds on a large index).
        "space_mb": rows("SELECT name, round(sum(pgsize) / 1048576.0, 1) FROM dbstat GROUP BY name ORDER BY 2 DESC LIMIT 40").into_iter().map(|r| json!([r[0], r[1]])).collect::<Vec<_>>(),
    });
    json!({"library": library, "coverage": coverage, "errors": errors, "db": db, "ms": t0.elapsed().as_millis() as u64})
}

// ---------------------------------------------------------------- the bundle

/// Writes one file into a tar archive (ustar: names under 100 bytes).
fn tar_file(out: &mut impl Write, name: &str, len: u64, mtime: u64, data: &mut impl Read) -> std::io::Result<()> {
    let mut h = [0u8; 512];
    let name = name.as_bytes();
    h[..name.len().min(99)].copy_from_slice(&name[..name.len().min(99)]);
    let octal = |h: &mut [u8; 512], at: usize, width: usize, v: u64| {
        let s = format!("{v:0w$o}", w = width - 1);
        h[at..at + width - 1].copy_from_slice(&s.as_bytes()[s.len() - (width - 1)..]);
    };
    octal(&mut h, 100, 8, 0o644);
    octal(&mut h, 108, 8, 0);
    octal(&mut h, 116, 8, 0);
    octal(&mut h, 124, 12, len);
    octal(&mut h, 136, 12, mtime);
    h[148..156].copy_from_slice(b"        ");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u64 = h.iter().map(|b| *b as u64).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
    out.write_all(&h)?;
    let n = std::io::copy(&mut data.take(len), out)?;
    if n != len {
        // The file shrank meanwhile: fill up to the size announced.
        out.write_all(&vec![0u8; (len - n) as usize])?;
    }
    let pad = (512 - (len % 512) as usize) % 512;
    out.write_all(&vec![0u8; pad])
}

fn tar_bytes(out: &mut impl Write, name: &str, data: &[u8], mtime: u64) -> std::io::Result<()> {
    tar_file(out, name, data.len() as u64, mtime, &mut &data[..])
}

const README: &str = r#"# Hyperfocal debug report

Made by Settings → Debug → Download report (or GET /api/debug/bundle) with HYPERFOCAL_DEBUG on.
Times are UTC; `t` fields are milliseconds since 1970. Paths are as the container sees them.
Secrets (password, tokens, API keys, alert topics) are not included.

- `summary.md`: one page of what matters most (also at GET /api/debug/summary).
- `report.json`: everything known now: `live` (the debug panel's data: latest sample, history,
  log groups, HTTP routes, timed operations, SQL, browsers, marks), `census` (the library by type
  and camera, how far each background pass is, every per-file error grouped by message, the
  index's size per table), `config`, `env`, `machine`, every log group, operation and statement.
- `debug/events-*.ndjson`: one JSON object per line, `k` says what it is:
  `start`/`stop`/`unclean_exit` (runs), `log` (lvl, at = file:line, th = thread, msg),
  `suppressed` (lines of one log site held back that minute), `panic`, `http_error`, `slow_http`
  (over 1 s), `slow_sql` (over 100 ms, with full-scan rows), `mark` (a moment marked in the UI,
  with what was on screen), `trim` (Release memory), `oom_kill`.
- `debug/metrics-*.ndjson`: a `sample` every interval. `proc` (cpu % of one core, rss/anon/file MB,
  disk MB/s, wake-ups), `malloc` (in use vs freed-but-held), `threads` (per pool: cpu %, wait % for a
  CPU, wake-ups/s), `children` (AI engine, ExifTool...), `cg` (container memory, OOM kills, CPU
  throttling, pressure), `gpu`, `tokio`, `db`, `disk`, `caches` (each memory cache: MB, cap, hits,
  misses, evictions), `mem` (indexes in memory), `work` (every background pass), and for the
  interval: `http` (per route n/p50/p95/max/kb/aborted/cancelled), `ops` (timed operations),
  `count` (counters), `sql` (top statements), `logs` (events per level).
- `debug/client-*.ndjson`: the browsers' reports (about one a minute while visible): `env`,
  `timings` (viewer.show.*, thumb.load, ... as histograms), `res` (requests per route, bytes,
  cache hits, server time from Server-Timing), `sse` (event stream messages), `long`/`loaf`
  (long tasks and the scripts behind them), `events` (slow interactions), `frames` (dropped frames
  while interacting), `mem`, `bat` (battery), `net`, `errors`.
- `debug/session.json`: whether the run before shut down cleanly.

A file whose name ends in `.tail.ndjson` holds only its newest lines (the report has a size cap).
"#;

/// The report bundle (gzip-compressed tar): the summary, the full report, and the telemetry
/// files of the last `days` days (newest first, at most `max_bytes` of them).
pub fn bundle(t: &Telemetry, state: &AppState, days: u32, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    t.store.flush(std::time::Duration::from_secs(3));
    let now = crate::db::now_ms();
    let mtime = (now / 1000) as u64;
    let census = census(state);
    let report = json!({
        "made": iso(now),
        "version": env!("CARGO_PKG_VERSION"),
        "live": live(t),
        "census": census,
        "config": config_json(&state.cfg),
        "env": env_json(),
        "machine": super::system::machine(),
        "decoders": state.decoders,
        "log_sites": t.logs.sites(4),
        "recent_events": t.logs.recent(300),
        "ops": top_ops(t, 1000),
        "counters": t.counters().into_iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<Map<_, _>>(),
        "http_routes": top_routes(t, 1000),
        "sql": t.sql.top(200),
        "sql_slow": t.sql.slow(100),
        "samples_in_memory": t.samples.lock().iter().map(|s| (**s).clone()).collect::<Vec<_>>(),
    });
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    tar_bytes(&mut gz, "hyperfocal-debug/README.md", README.as_bytes(), mtime)?;
    tar_bytes(&mut gz, "hyperfocal-debug/summary.md", summary(t, state).as_bytes(), mtime)?;
    tar_bytes(&mut gz, "hyperfocal-debug/report.json", &serde_json::to_vec_pretty(&report).unwrap_or_default(), mtime)?;
    if let Ok(b) = std::fs::read(t.store.dir.join("session.json")) {
        tar_bytes(&mut gz, "hyperfocal-debug/debug/session.json", &b, mtime)?;
    }
    let cutoff = store::day_of(now - (days.max(1) as i64 - 1) * 86_400_000);
    let mut budget = max_bytes;
    // Newest first, so a tight budget keeps what happened last.
    for f in t.store.files().into_iter().rev().filter(|f| f.day >= cutoff) {
        if budget == 0 {
            break;
        }
        let Ok(mut file) = std::fs::File::open(&f.path) else { continue };
        let len = file.metadata().map(|m| m.len()).unwrap_or(f.size);
        let name = f.path.file_name().and_then(|n| n.to_str()).unwrap_or("x.ndjson").to_string();
        if len <= budget {
            tar_file(&mut gz, &format!("hyperfocal-debug/debug/{name}"), len, mtime, &mut file)?;
            budget -= len;
        } else {
            // Its newest lines only, from the first line start in the last `budget` bytes.
            file.seek(SeekFrom::Start(len - budget))?;
            let mut buf = Vec::with_capacity(budget as usize);
            file.take(budget).read_to_end(&mut buf)?;
            let start = buf.iter().position(|b| *b == b'\n').map(|i| i + 1).unwrap_or(buf.len());
            let tail = name.replace(".ndjson", ".tail.ndjson");
            tar_bytes(&mut gz, &format!("hyperfocal-debug/debug/{tail}"), &buf[start..], mtime)?;
            budget = 0;
        }
    }
    gz.write_all(&[0u8; 1024])?;
    gz.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_group_across_files() {
        assert_eq!(normalize_error("cannot read /photos/2014/Trip/IMG_0001.CR2: Permission denied (os error 13)"), "cannot read <path>: Permission denied (os error #)");
        assert_eq!(normalize_error("unsupported compression 34892"), "unsupported compression #");
        assert_eq!(normalize_error("IMG_1234.CR3 is truncated"), "IMG_#.CR# is truncated");
    }

    #[test]
    fn secrets_stay_out() {
        assert!(secret("HYPERFOCAL_PASSWORD"));
        assert!(secret("HYPERFOCAL_IMMICH_API_KEY"));
        assert!(secret("HYPERFOCAL_ALERT_TOKEN"));
        assert!(!secret("HYPERFOCAL_PHOTOS"));
        assert_eq!(redact_url("https://ntfy.sh/my-secret-topic"), "https://ntfy.sh/…");
        assert_eq!(redact_url("http://user:pw@192.168.1.10:2283"), "http://192.168.1.10:2283");
        assert_eq!(redact_url("nonsense"), "(set)");
    }

    #[test]
    fn tar_entries_are_valid() {
        let mut out = Vec::new();
        tar_bytes(&mut out, "a/b.txt", b"hello", 1_700_000_000).unwrap();
        out.extend_from_slice(&[0u8; 1024]);
        assert_eq!(out.len(), 512 + 512 + 1024);
        assert_eq!(&out[..7], b"a/b.txt");
        assert_eq!(&out[257..262], b"ustar");
        assert_eq!(&out[512..517], b"hello");
        // The checksum is the sum of the header with its own field as spaces.
        let mut h = out[..512].to_vec();
        let stored = u64::from_str_radix(std::str::from_utf8(&h[148..154]).unwrap(), 8).unwrap();
        h[148..156].copy_from_slice(b"        ");
        assert_eq!(stored, h.iter().map(|b| *b as u64).sum::<u64>());
        assert_eq!(u64::from_str_radix(std::str::from_utf8(&out[124..135]).unwrap(), 8).unwrap(), 5);
    }
}
