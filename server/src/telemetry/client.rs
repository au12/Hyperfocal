//! What the browsers report in debug mode (web/src/lib/telemetry.ts sends a report about
//! once a minute, and when the page is hidden): errors, how long the viewer and the grid
//! take, every request as the browser saw it, the event stream, long tasks and slow
//! interactions, dropped frames while scrolling or panning, memory, battery and network.
//! Each report goes to the client file as it came; per device (a browser, by an id it
//! keeps), the totals are kept for the debug panel and the report.

use super::hist::{BUCKETS, Hist};
use super::iso;
use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};

const MAX_DEVICES: usize = 16;
const MAX_KEYS: usize = 300;

#[derive(Default)]
struct Res {
    n: u64,
    bytes: u64,
    cached: u64,
    sw: u64,
    dur: Hist,
    srv: Hist,
}

struct ErrAgg {
    kind: String,
    msg: String,
    n: u64,
    first: i64,
    last: i64,
    sample: Value,
}

#[derive(Default)]
pub struct Device {
    env: Value,
    nav: Value,
    first: i64,
    last: i64,
    reports: u64,
    pages: VecDeque<String>,
    ctx: Value,
    timings: HashMap<String, Hist>,
    counts: HashMap<String, u64>,
    res: HashMap<String, Res>,
    sse: HashMap<String, [u64; 3]>,
    long: (u64, f64, f64),
    loaf: HashMap<String, (u64, f64)>,
    events: VecDeque<Value>,
    /// Per view: frames, frames dropped, the worst frame (ms), the frame budget (ms).
    frames: HashMap<String, (u64, u64, f64, f64)>,
    mem: Value,
    mem_peak: Map<String, Value>,
    battery: VecDeque<(i64, f64, bool)>,
    visible_s: f64,
    hidden_s: f64,
    errors: HashMap<String, ErrAgg>,
}

#[derive(Default)]
pub struct Clients {
    devices: Mutex<HashMap<String, Device>>,
}

fn merge_hist(h: &Hist, v: &Value) {
    let n = v["n"].as_u64().unwrap_or(0);
    if n == 0 {
        return;
    }
    let buckets = v["b"].as_object().map(|b| b.iter().filter_map(|(k, c)| Some((k.parse::<usize>().ok()?, c.as_u64()?))).filter(|(i, _)| *i < BUCKETS).collect::<Vec<_>>()).unwrap_or_default();
    h.merge(n, v["err"].as_u64().unwrap_or(0), v["sum_us"].as_u64().unwrap_or(0), v["max_us"].as_u64().unwrap_or(0), buckets.into_iter());
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { s.chars().take(max).collect::<String>() + "…" }
}

impl Clients {
    /// Takes one report (already parsed); `now` is the server's time.
    pub fn ingest(&self, r: &Value, now: i64) -> Result<String, String> {
        let dev = r["dev"].as_str().filter(|d| !d.is_empty() && d.len() <= 64).ok_or("no device id")?.to_string();
        let mut all = self.devices.lock();
        if !all.contains_key(&dev) && all.len() >= MAX_DEVICES {
            // The device not heard from the longest makes room.
            if let Some(old) = all.iter().min_by_key(|(_, d)| d.last).map(|(k, _)| k.clone()) {
                all.remove(&old);
            }
        }
        let d = all.entry(dev.clone()).or_insert_with(|| Device { first: now, ..Default::default() });
        d.last = now;
        d.reports += 1;
        if let Some(p) = r["page"].as_str()
            && !d.pages.iter().any(|x| x == p)
        {
            if d.pages.len() >= 20 {
                d.pages.pop_front();
            }
            d.pages.push_back(short(p, 40));
        }
        if r["env"].is_object() {
            d.env = r["env"].clone();
        }
        if r["nav"].is_object() {
            d.nav = r["nav"].clone();
        }
        if r["ctx"].is_object() {
            d.ctx = r["ctx"].clone();
        }
        if let Some(v) = r["vis"].as_object() {
            d.visible_s += v.get("visible_s").and_then(Value::as_f64).unwrap_or(0.0);
            d.hidden_s += v.get("hidden_s").and_then(Value::as_f64).unwrap_or(0.0);
        }
        if let Some(t) = r["timings"].as_object() {
            for (k, v) in t {
                if d.timings.len() < MAX_KEYS || d.timings.contains_key(k) {
                    merge_hist(d.timings.entry(short(k, 80)).or_default(), v);
                }
            }
        }
        if let Some(c) = r["counts"].as_object() {
            for (k, v) in c {
                if d.counts.len() < MAX_KEYS || d.counts.contains_key(k) {
                    *d.counts.entry(short(k, 80)).or_default() += v.as_u64().unwrap_or(0);
                }
            }
        }
        if let Some(res) = r["res"].as_object() {
            for (k, v) in res {
                if d.res.len() >= MAX_KEYS && !d.res.contains_key(k) {
                    continue;
                }
                let e = d.res.entry(short(k, 100)).or_default();
                e.n += v["n"].as_u64().unwrap_or(0);
                e.bytes += v["bytes"].as_u64().unwrap_or(0);
                e.cached += v["cached"].as_u64().unwrap_or(0);
                e.sw += v["sw"].as_u64().unwrap_or(0);
                merge_hist(&e.dur, &v["dur"]);
                merge_hist(&e.srv, &v["srv"]);
            }
        }
        if let Some(s) = r["sse"].as_object() {
            for (k, v) in s {
                if d.sse.len() < MAX_KEYS || d.sse.contains_key(k) {
                    let e = d.sse.entry(short(k, 40)).or_default();
                    e[0] += v["n"].as_u64().unwrap_or(0);
                    e[1] += v["bytes"].as_u64().unwrap_or(0);
                    e[2] += v["hidden"].as_u64().unwrap_or(0);
                }
            }
        }
        if r["long"].is_object() {
            d.long.0 += r["long"]["n"].as_u64().unwrap_or(0);
            d.long.1 += r["long"]["ms"].as_f64().unwrap_or(0.0);
            d.long.2 = d.long.2.max(r["long"]["max"].as_f64().unwrap_or(0.0));
        }
        if let Some(l) = r["loaf"].as_array() {
            for x in l {
                let Some(k) = x["key"].as_str() else { continue };
                if d.loaf.len() < MAX_KEYS || d.loaf.contains_key(k) {
                    let e = d.loaf.entry(short(k, 200)).or_default();
                    e.0 += x["n"].as_u64().unwrap_or(0);
                    e.1 += x["ms"].as_f64().unwrap_or(0.0);
                }
            }
        }
        if let Some(ev) = r["events"].as_array() {
            for e in ev.iter().take(50) {
                if d.events.len() >= 50 {
                    d.events.pop_front();
                }
                let mut e = e.clone();
                e["t"] = now.into();
                d.events.push_back(e);
            }
        }
        if let Some(f) = r["frames"].as_object() {
            for (k, v) in f {
                if d.frames.len() >= 32 && !d.frames.contains_key(k) {
                    continue;
                }
                let e = d.frames.entry(short(k, 40)).or_default();
                e.0 += v["n"].as_u64().unwrap_or(0);
                e.1 += v["dropped"].as_u64().unwrap_or(0);
                e.2 = e.2.max(v["worst"].as_f64().unwrap_or(0.0));
                if let Some(b) = v["budget"].as_f64() {
                    e.3 = b;
                }
            }
        }
        if let Some(m) = r["mem"].as_object() {
            d.mem = r["mem"].clone();
            for (k, v) in m {
                if let Some(x) = v.as_f64() {
                    let peak = d.mem_peak.get(k).and_then(Value::as_f64).unwrap_or(f64::MIN);
                    if x > peak {
                        d.mem_peak.insert(k.clone(), x.into());
                    }
                }
            }
        }
        if let Some(level) = r["bat"]["level"].as_f64() {
            if d.battery.len() >= 1000 {
                d.battery.pop_front();
            }
            d.battery.push_back((now, level, r["bat"]["charging"].as_bool().unwrap_or(false)));
        }
        if let Some(errs) = r["errors"].as_array() {
            for e in errs.iter().take(100) {
                let kind = short(e["kind"].as_str().unwrap_or("error"), 30);
                let msg = short(e["msg"].as_str().unwrap_or(""), 400);
                let key = format!("{kind}|{msg}");
                let n = e["n"].as_u64().unwrap_or(1).max(1);
                if let Some(a) = d.errors.get_mut(&key) {
                    a.n += n;
                    a.last = now;
                    a.sample = e.clone();
                } else if d.errors.len() < 200 {
                    d.errors.insert(key, ErrAgg { kind, msg, n, first: now, last: now, sample: e.clone() });
                }
            }
        }
        Ok(dev)
    }

    /// Every device's totals, the most recently heard from first.
    pub fn summaries(&self) -> Vec<Value> {
        let all = self.devices.lock();
        let mut v: Vec<(&String, &Device)> = all.iter().collect();
        v.sort_by_key(|(_, d)| std::cmp::Reverse(d.last));
        v.into_iter().map(|(id, d)| d.json(id)).collect()
    }

    /// Error counts of every device (for the summary line).
    pub fn error_count(&self) -> u64 {
        self.devices.lock().values().map(|d| d.errors.values().map(|e| e.n).sum::<u64>()).sum()
    }
}

impl Device {
    /// %/hour the battery went down over the latest stretch off the charger (at least 10
    /// minutes and a drop of 1% are needed to say).
    fn drain(&self) -> Option<f64> {
        let last = self.battery.back()?;
        if last.2 {
            return None;
        }
        let first = self.battery.iter().rev().take_while(|b| !b.2).last()?;
        let hours = (last.0 - first.0) as f64 / 3_600_000.0;
        let drop = first.1 - last.1;
        (hours >= 1.0 / 6.0 && drop >= 0.01).then(|| ((drop * 100.0 / hours) * 10.0).round() / 10.0)
    }

    fn json(&self, id: &str) -> Value {
        let mut errors: Vec<&ErrAgg> = self.errors.values().collect();
        errors.sort_by_key(|e| std::cmp::Reverse(e.n));
        let mut res: Vec<(&String, &Res)> = self.res.iter().collect();
        res.sort_by_key(|(_, r)| std::cmp::Reverse(r.dur.snapshot().sum));
        let mut loaf: Vec<(&String, &(u64, f64))> = self.loaf.iter().collect();
        loaf.sort_by(|a, b| b.1.1.total_cmp(&a.1.1));
        let ms = |us: u64| (us as f64 / 100.0).round() / 10.0;
        json!({
            "dev": id,
            "first": iso(self.first),
            "last": iso(self.last),
            "reports": self.reports,
            "pages": self.pages.len(),
            "env": self.env,
            "nav": self.nav,
            "ctx": self.ctx,
            "visible_min": (self.visible_s / 6.0).round() / 10.0,
            "hidden_min": (self.hidden_s / 6.0).round() / 10.0,
            "errors": errors.iter().take(20).map(|e| json!({"kind": e.kind, "msg": e.msg, "n": e.n, "first": iso(e.first), "last": iso(e.last), "sample": e.sample})).collect::<Vec<_>>(),
            "timings": self.timings.iter().map(|(k, h)| (k.clone(), h.snapshot().json())).collect::<Map<_, _>>(),
            "counts": self.counts.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<_, _>>(),
            "res": res.iter().take(40).map(|(k, r)| {
                let (d, s) = (r.dur.snapshot(), r.srv.snapshot());
                (k.to_string(), json!({
                    "n": r.n,
                    "mb": (r.bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0,
                    "cached": r.cached,
                    "sw": r.sw,
                    "p50": ms(d.pct(0.5)), "p95": ms(d.pct(0.95)), "max": ms(d.max),
                    "srv_p50": ms(s.pct(0.5)), "srv_p95": ms(s.pct(0.95)),
                }))
            }).collect::<Map<_, _>>(),
            "sse": self.sse.iter().map(|(k, v)| (k.clone(), json!({"n": v[0], "kb": v[1] / 1024, "hidden": v[2]}))).collect::<Map<_, _>>(),
            "long": {"n": self.long.0, "s": (self.long.1 / 100.0).round() / 10.0, "max_ms": self.long.2},
            "loaf": loaf.iter().take(12).map(|(k, v)| json!({"key": k, "n": v.0, "ms": v.1.round()})).collect::<Vec<_>>(),
            "slow_events": self.events.iter().rev().take(20).cloned().collect::<Vec<_>>(),
            "frames": self.frames.iter().map(|(k, f)| (k.clone(), json!({
                "n": f.0,
                "dropped": f.1,
                "dropped_pct": if f.0 + f.1 > 0 { (f.1 as f64 * 1000.0 / (f.0 + f.1) as f64).round() / 10.0 } else { 0.0 },
                "worst_ms": f.2.round(),
                "budget_ms": (f.3 * 10.0).round() / 10.0,
            }))).collect::<Map<_, _>>(),
            "mem": self.mem,
            "mem_peak": self.mem_peak,
            "battery": self.battery.back().map(|b| json!({"level": b.1, "charging": b.2, "drain_pct_h": self.drain(), "samples": self.battery.len()})),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_add_up_per_device() {
        let c = Clients::default();
        let hist = json!({"n": 2, "sum_us": 300_000, "max_us": 200_000, "b": {"70": 1, "71": 1}});
        let r = json!({
            "dev": "d1", "page": "p1",
            "env": {"ua": "Safari", "mobile": true},
            "vis": {"visible_s": 60.0, "hidden_s": 0.0},
            "timings": {"viewer.show.raw": hist},
            "counts": {"thumb.ok": 10},
            "res": {"GET /api/photo/{id}/thumb": {"n": 10, "bytes": 400_000, "cached": 2, "dur": hist, "srv": hist}},
            "sse": {"progress": {"n": 30, "bytes": 6000, "hidden": 5}},
            "frames": {"grid": {"n": 100, "dropped": 25, "worst": 80.0, "budget": 16.7}},
            "mem": {"js_used_mb": 50.0},
            "bat": {"level": 0.9, "charging": false},
            "errors": [{"kind": "js", "msg": "x is undefined", "n": 1}],
        });
        assert_eq!(c.ingest(&r, 1_000_000).unwrap(), "d1");
        let mut r2 = r.clone();
        r2["bat"] = json!({"level": 0.85, "charging": false});
        r2["mem"] = json!({"js_used_mb": 40.0});
        c.ingest(&r2, 1_000_000 + 30 * 60_000).unwrap();
        assert!(c.ingest(&json!({"page": "x"}), 0).is_err());
        let s = &c.summaries()[0];
        assert_eq!(s["reports"], 2);
        assert_eq!(s["counts"]["thumb.ok"], 20);
        assert_eq!(s["timings"]["viewer.show.raw"]["n"], 4);
        assert_eq!(s["res"]["GET /api/photo/{id}/thumb"]["n"], 20);
        assert_eq!(s["sse"]["progress"]["hidden"], 10);
        assert_eq!(s["frames"]["grid"]["dropped_pct"], 20.0);
        assert_eq!(s["mem_peak"]["js_used_mb"], 50.0);
        assert_eq!(s["errors"][0]["n"], 2);
        // 5% in half an hour.
        assert_eq!(s["battery"]["drain_pct_h"], 10.0);
        assert_eq!(s["visible_min"], 2.0);
        assert_eq!(c.error_count(), 2);
    }
}
