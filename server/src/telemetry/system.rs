//! What the operating system knows about the process: CPU time, memory (and how much of it
//! is the allocator's, the page cache's or files mapped into memory), threads, child
//! processes (the AI engine, ExifTool, the LibRaw helper), the container's cgroup limits,
//! pressure stall information and the GPU. Linux only; elsewhere the readings are empty.

use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

fn read(p: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

pub fn clock_ticks() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if t > 0 { t as u64 } else { 100 }
}

pub fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if p > 0 { p as u64 } else { 4096 }
}

/// `/proc/<pid>/stat`.
#[derive(Debug, Default, Clone)]
pub struct Stat {
    pub comm: String,
    pub ppid: i64,
    pub minflt: u64,
    pub majflt: u64,
    /// CPU time in clock ticks.
    pub utime: u64,
    pub stime: u64,
    pub threads: u64,
    pub vsize: u64,
    pub rss_pages: u64,
}

pub fn parse_stat(s: &str) -> Option<Stat> {
    // The name is in parentheses and may itself contain spaces and parentheses.
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let comm = s.get(open + 1..close)?.to_string();
    let f: Vec<&str> = s.get(close + 1..)?.split_whitespace().collect();
    // f[0] is field 3 (state): field n is f[n - 3].
    let n = |i: usize| f.get(i - 3).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    Some(Stat {
        comm,
        ppid: n(4),
        minflt: n(10) as u64,
        majflt: n(12) as u64,
        utime: n(14) as u64,
        stime: n(15) as u64,
        threads: n(20) as u64,
        vsize: n(23) as u64,
        rss_pages: n(24).max(0) as u64,
    })
}

/// `Key: value kB` lines (`/proc/self/status`, `/proc/meminfo`) as numbers.
pub fn parse_kv(s: &str) -> HashMap<String, u64> {
    s.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            let num = v.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((k.trim().to_string(), num))
        })
        .collect()
}

/// `key value` lines (`memory.stat`, `cpu.stat`, `/proc/self/io` with a colon).
pub fn parse_pairs(s: &str) -> HashMap<String, u64> {
    s.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let k = it.next()?.trim_end_matches(':').to_string();
            let v = it.next()?.parse::<u64>().ok()?;
            Some((k, v))
        })
        .collect()
}

pub fn self_stat() -> Option<Stat> {
    read("/proc/self/stat").and_then(|s| parse_stat(&s))
}

pub fn self_status() -> HashMap<String, u64> {
    read("/proc/self/status").map(|s| parse_kv(&s)).unwrap_or_default()
}

pub fn self_io() -> HashMap<String, u64> {
    read("/proc/self/io").map(|s| parse_pairs(&s)).unwrap_or_default()
}

pub fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd").map(|d| d.count()).unwrap_or(0)
}

/// Resident memory now (bytes), cheaply.
pub fn rss_bytes() -> u64 {
    read("/proc/self/statm").and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok())).unwrap_or(0) * page_size()
}

/// One thread of this process: its name and scheduler figures.
#[derive(Debug, Clone)]
pub struct ThreadTimes {
    pub tid: u64,
    pub name: String,
    /// On the CPU, and runnable but waiting for one (ns).
    pub run_ns: u64,
    pub wait_ns: u64,
    /// Times it was given the CPU (about how often it woke up).
    pub slices: u64,
}

/// Every thread of this process.
pub fn threads() -> Vec<ThreadTimes> {
    let tick_ns = 1_000_000_000 / clock_ticks();
    let Ok(rd) = std::fs::read_dir("/proc/self/task") else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let tid: u64 = e.file_name().to_str()?.parse().ok()?;
            let dir = e.path();
            let name = read(dir.join("comm")).map(|s| s.trim_end().to_string()).unwrap_or_default();
            // schedstat: run time, wait time (ns), time slices. Without it (a kernel built
            // without schedstats), CPU ticks from stat.
            if let Some(ss) = read(dir.join("schedstat")) {
                let v: Vec<u64> = ss.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if v.len() >= 3 {
                    return Some(ThreadTimes { tid, name, run_ns: v[0], wait_ns: v[1], slices: v[2] });
                }
            }
            let st = read(dir.join("stat")).and_then(|s| parse_stat(&s))?;
            Some(ThreadTimes { tid, name, run_ns: (st.utime + st.stime) * tick_ns, wait_ns: 0, slices: 0 })
        })
        .collect()
}

/// A thread's name without its number ("index-3" → "index"), so threads of a pool count
/// together. Names are at most 15 characters ("tokio-runtime-w").
pub fn thread_group(name: &str) -> String {
    let t = name.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end_matches(['-', '_', ' ', '#']);
    if t.is_empty() { name.to_string() } else { t.to_string() }
}

/// A child process (or grandchild) of this one.
#[derive(Debug, Clone)]
pub struct Child {
    pub pid: u64,
    pub name: String,
    pub rss: u64,
    pub ticks: u64,
    pub threads: u64,
}

/// What a child process is: for `hyperfocal ml-engine` the sub-command, else its name.
fn child_name(pid: u64, comm: &str) -> String {
    let args: Vec<String> = read(format!("/proc/{pid}/cmdline"))
        .map(|s| s.split('\0').filter(|a| !a.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
    match args.first().map(|a| base(a)) {
        Some(b) if b == "hyperfocal" || comm == "hyperfocal" => args.get(1).cloned().unwrap_or(b),
        // ExifTool is a Perl script: its name comes after perl's own options.
        Some(b) if b.starts_with("perl") => args.iter().skip(1).find(|a| !a.starts_with('-')).map(|a| base(a)).unwrap_or(b),
        Some(b) => b,
        None => comm.to_string(),
    }
}

/// The processes this one started, and theirs.
pub fn children() -> Vec<Child> {
    let me = std::process::id() as i64;
    let page = page_size();
    let Ok(rd) = std::fs::read_dir("/proc") else { return Vec::new() };
    let all: Vec<(u64, Stat)> = rd
        .flatten()
        .filter_map(|e| {
            let pid: u64 = e.file_name().to_str()?.parse().ok()?;
            Some((pid, read(e.path().join("stat")).and_then(|s| parse_stat(&s))?))
        })
        .collect();
    let mut mine: Vec<i64> = vec![me];
    let mut out = Vec::new();
    // Two levels: the engine and the helpers are children; anything they start, grandchildren.
    for _ in 0..2 {
        let found: Vec<&(u64, Stat)> = all.iter().filter(|(pid, s)| mine.contains(&s.ppid) && !out.iter().any(|c: &Child| c.pid == *pid)).collect();
        for (pid, s) in &found {
            out.push(Child { pid: *pid, name: child_name(*pid, &s.comm), rss: s.rss_pages * page, ticks: s.utime + s.stime, threads: s.threads });
        }
        mine = found.iter().map(|(pid, _)| *pid as i64).collect();
    }
    out
}

/// The allocator (glibc malloc): bytes in use, free but held, in large mappings.
pub fn malloc() -> Option<Value> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let t = std::time::Instant::now();
        // SAFETY: mallinfo2 has no preconditions (it locks each arena while it reads it).
        let m = unsafe { libc::mallinfo2() };
        let mb = |b: usize| (b as f64 / 1_048_576.0 * 10.0).round() / 10.0;
        Some(json!({
            // In use: small allocations plus large ones (each its own mapping).
            "in_use_mb": mb(m.uordblks + m.hblkhd),
            // Freed but still held by the allocator (fragmentation, or kept for reuse).
            "free_mb": mb(m.fordblks),
            "mmap_mb": mb(m.hblkhd),
            "arena_mb": mb(m.arena),
            "releasable_mb": mb(m.keepcost),
            "ms": (t.elapsed().as_secs_f64() * 10_000.0).round() / 10.0,
        }))
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    None
}

/// Hands free memory held by the allocator back to the system (`malloc_trim`).
pub fn trim() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: malloc_trim has no preconditions.
        unsafe { libc::malloc_trim(0) == 1 }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    false
}

/// Pressure stall information: the share of time some (or all) tasks waited for the CPU,
/// memory or I/O, over the last 10 and 60 seconds (%).
pub fn parse_psi(s: &str) -> Value {
    let mut out = Map::new();
    for l in s.lines() {
        let mut it = l.split_whitespace();
        let Some(kind) = it.next() else { continue };
        for kv in it {
            if let Some((k, v)) = kv.split_once('=')
                && (k == "avg10" || k == "avg60")
                && let Ok(x) = v.parse::<f64>()
            {
                out.insert(format!("{kind}{}", &k[3..]), x.into());
            }
        }
    }
    Value::Object(out)
}

/// `io.stat`: bytes and operations per device (major:minor), and in all.
pub fn parse_io_stat(s: &str) -> (u64, u64, Vec<(String, u64, u64)>) {
    let (mut r, mut w) = (0, 0);
    let mut devs = Vec::new();
    for l in s.lines() {
        let mut it = l.split_whitespace();
        let Some(dev) = it.next() else { continue };
        let kv: HashMap<&str, u64> = it.filter_map(|x| x.split_once('=')).filter_map(|(k, v)| Some((k, v.parse().ok()?))).collect();
        let (dr, dw) = (kv.get("rbytes").copied().unwrap_or(0), kv.get("wbytes").copied().unwrap_or(0));
        r += dr;
        w += dw;
        devs.push((dev.to_string(), dr, dw));
    }
    (r, w, devs)
}

/// The container's cgroup (v2, or the memory figures of v1): raw counters, for the sampler
/// to turn into rates.
#[derive(Debug, Default, Clone)]
pub struct Cgroup {
    pub v2: bool,
    pub mem: Option<u64>,
    pub mem_max: Option<u64>,
    pub mem_peak: Option<u64>,
    pub stat: HashMap<String, u64>,
    pub events: HashMap<String, u64>,
    pub cpu: HashMap<String, u64>,
    pub cpu_max: Option<String>,
    pub io: Option<(u64, u64, Vec<(String, u64, u64)>)>,
    pub psi: Map<String, Value>,
}

pub fn cgroup() -> Option<Cgroup> {
    let root = Path::new("/sys/fs/cgroup");
    let num = |p: &Path| read(p).and_then(|s| s.trim().parse::<u64>().ok());
    if root.join("cgroup.controllers").exists() {
        let mut c = Cgroup { v2: true, ..Default::default() };
        c.mem = num(&root.join("memory.current"));
        c.mem_max = num(&root.join("memory.max")); // "max": no limit
        c.mem_peak = num(&root.join("memory.peak"));
        c.stat = read(root.join("memory.stat")).map(|s| parse_pairs(&s)).unwrap_or_default();
        c.events = read(root.join("memory.events")).map(|s| parse_pairs(&s)).unwrap_or_default();
        c.cpu = read(root.join("cpu.stat")).map(|s| parse_pairs(&s)).unwrap_or_default();
        c.cpu_max = read(root.join("cpu.max")).map(|s| s.trim().to_string());
        c.io = read(root.join("io.stat")).map(|s| parse_io_stat(&s));
        for k in ["cpu", "memory", "io"] {
            if let Some(s) = read(root.join(format!("{k}.pressure"))) {
                c.psi.insert(k.into(), parse_psi(&s));
            }
        }
        if c.psi.is_empty() {
            for k in ["cpu", "memory", "io"] {
                if let Some(s) = read(format!("/proc/pressure/{k}")) {
                    c.psi.insert(k.into(), parse_psi(&s));
                }
            }
        }
        return Some(c);
    }
    let m = root.join("memory");
    if m.exists() {
        let mut c = Cgroup { v2: false, ..Default::default() };
        c.mem = num(&m.join("memory.usage_in_bytes"));
        c.mem_max = num(&m.join("memory.limit_in_bytes")).filter(|v| *v < (1 << 60));
        c.mem_peak = num(&m.join("memory.max_usage_in_bytes"));
        c.stat = read(m.join("memory.stat")).map(|s| parse_pairs(&s)).unwrap_or_default();
        c.events = read(m.join("memory.oom_control")).map(|s| parse_pairs(&s)).unwrap_or_default();
        return Some(c);
    }
    None
}

/// Host memory (`/proc/meminfo`, which shows the host's figures inside a container).
pub fn meminfo() -> HashMap<String, u64> {
    read("/proc/meminfo").map(|s| parse_kv(&s)).unwrap_or_default()
}

pub fn loadavg() -> Vec<f64> {
    read("/proc/loadavg").map(|s| s.split_whitespace().take(3).filter_map(|v| v.parse().ok()).collect()).unwrap_or_default()
}

/// Free and total bytes of the file system holding `path`.
pub fn disk(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a statvfs to fill.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let f = s.f_frsize as u64;
    Some((s.f_bavail as u64 * f, s.f_blocks as u64 * f))
}

/// Runs a command for its output, killed after `timeout`.
pub fn run_for(cmd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(cmd).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

/// Whether `nvidia-smi` is there (the NVIDIA container runtime brings it with the GPU).
pub fn has_nvidia_smi() -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("nvidia-smi").is_file()))
}

/// The GPUs' load, memory, temperature and power (`nvidia-smi`).
pub fn gpu() -> Option<Value> {
    let out = run_for(
        "nvidia-smi",
        &["--query-gpu=index,name,utilization.gpu,utilization.memory,memory.used,memory.total,temperature.gpu,power.draw,pstate,clocks.sm", "--format=csv,noheader,nounits"],
        Duration::from_secs(5),
    )?;
    let num = |s: &str| s.trim().parse::<f64>().ok();
    let v: Vec<Value> = out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split(',').map(str::trim).collect();
            let g = |i: usize| f.get(i).copied().unwrap_or("");
            json!({
                "i": num(g(0)), "name": g(1), "util": num(g(2)), "mem_util": num(g(3)),
                "mem_used_mb": num(g(4)), "mem_total_mb": num(g(5)), "temp": num(g(6)),
                "power_w": num(g(7)), "pstate": g(8), "sm_mhz": num(g(9)),
            })
        })
        .collect();
    (!v.is_empty()).then(|| Value::Array(v))
}

/// The machine: CPU, cores, memory, kernel, the container's limits.
pub fn machine() -> Value {
    let cpuinfo = read("/proc/cpuinfo").unwrap_or_default();
    let model = cpuinfo.lines().find(|l| l.starts_with("model name")).and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string());
    let flags = cpuinfo.lines().find(|l| l.starts_with("flags")).map(|l| ["avx2", "avx512f", "sse4_2"].iter().filter(|f| l.split_whitespace().any(|x| x == **f)).map(|s| s.to_string()).collect::<Vec<_>>());
    let mi = meminfo();
    let cg = cgroup();
    json!({
        "cpu": model,
        "cpu_flags": flags,
        "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "cores_online": cpuinfo.lines().filter(|l| l.starts_with("processor")).count(),
        "mem_total_mb": mi.get("MemTotal").map(|k| k / 1024),
        "swap_total_mb": mi.get("SwapTotal").map(|k| k / 1024),
        "kernel": read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_string()),
        "os": read("/etc/os-release").and_then(|s| s.lines().find(|l| l.starts_with("PRETTY_NAME=")).map(|l| l[12..].trim_matches('"').to_string())),
        "cgroup": cg.as_ref().map(|c| json!({
            "v2": c.v2,
            "mem_max_mb": c.mem_max.map(|b| b >> 20),
            "cpu_max": c.cpu_max,
        })),
        "page_size": page_size(),
        "clock_ticks": clock_ticks(),
        "nvidia_smi": has_nvidia_smi(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_files() {
        let s = parse_stat("123 (tokio (x) w) S 1 2 3 4 5 6 70 8 90 10 1500 300 0 0 20 0 17 0 900 123456789 4321 18446744073709551615").unwrap();
        assert_eq!(s.comm, "tokio (x) w");
        assert_eq!(s.ppid, 1);
        assert_eq!(s.minflt, 70);
        assert_eq!(s.majflt, 90);
        assert_eq!(s.utime, 1500);
        assert_eq!(s.stime, 300);
        assert_eq!(s.threads, 17);
        assert_eq!(s.vsize, 123456789);
        assert_eq!(s.rss_pages, 4321);
        let kv = parse_kv("Name:\thyperfocal\nVmRSS:\t  123456 kB\nThreads:\t42\n");
        assert_eq!(kv["VmRSS"], 123456);
        assert_eq!(kv["Threads"], 42);
        assert!(!kv.contains_key("Name"));
        let p = parse_pairs("rchar: 100\nread_bytes: 4096\n");
        assert_eq!(p["read_bytes"], 4096);
        let psi = parse_psi("some avg10=1.50 avg60=0.25 avg300=0.00 total=123\nfull avg10=0.00 avg60=0.10 avg300=0.00 total=5\n");
        assert_eq!(psi["some10"], 1.5);
        assert_eq!(psi["full60"], 0.1);
        let (r, w, devs) = parse_io_stat("8:0 rbytes=1000 wbytes=20 rios=3 wios=1\n8:16 rbytes=500 wbytes=0 rios=1 wios=0\n");
        assert_eq!((r, w, devs.len()), (1500, 20, 2));
        assert_eq!(thread_group("index-3"), "index");
        assert_eq!(thread_group("tokio-runtime-w"), "tokio-runtime-w");
        assert_eq!(thread_group("12"), "12");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn reads_this_process() {
        let s = self_stat().expect("stat");
        assert!(s.threads >= 1);
        assert!(self_status().contains_key("VmRSS"));
        assert!(rss_bytes() > 0);
        assert!(!threads().is_empty());
        assert!(fd_count() > 0);
        let _ = children();
        let _ = cgroup();
        let m = machine();
        assert!(m["cores"].as_u64().unwrap() >= 1);
        #[cfg(target_env = "gnu")]
        assert!(malloc().unwrap()["in_use_mb"].as_f64().unwrap() > 0.0);
    }
}
