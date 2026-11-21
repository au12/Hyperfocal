//! Keeps the AI engine running: decides between the CPU and an NVIDIA GPU, downloads what
//! the enabled features need, starts `hyperfocal ml-engine`, and starts it again when it
//! stops (on the CPU, if the GPU keeps failing). The workers find it through `Ml::client`.

use super::engine::Device;
use super::engine::probe::Gpus;
use super::engine::serve::{FAILED, READY};
use super::manifest;
use super::{DeviceChoice, Endpoint, MlStatus};
use crate::state::{AppState, Event};
use anyhow::{Context, Result, anyhow, bail};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Loading the models can take a while the first time on a GPU (cuDNN warms up).
const START_TIMEOUT: Duration = Duration::from_secs(900);

pub fn spawn(state: Arc<AppState>) {
    let cfg = &state.ml.cfg;
    if let Some(url) = &cfg.legacy_url {
        tracing::warn!("HYPERFOCAL_ML_URL ({url}) is no longer used: the AI features run inside Hyperfocal now, and the hyperfocal-ml container can be removed");
    }
    if !cfg.enabled {
        tracing::info!("AI features: off (HYPERFOCAL_ML=false)");
        return;
    }
    if !cfg.any_on() {
        tracing::info!("AI features: off (HYPERFOCAL_ML_FACES and HYPERFOCAL_ML_SEARCH are false)");
        set(&state, |s| s.phase = "off".into());
        return;
    }
    std::thread::Builder::new()
        .name("ml".into())
        .spawn(move || run(&state))
        .expect("spawn ml");
}

fn set(state: &AppState, f: impl FnOnce(&mut MlStatus)) {
    f(&mut state.ml.status.write());
    state.emit(Event::Ml(state.ml.status()));
}

fn exe() -> PathBuf {
    if let Some(p) = std::env::var_os("HYPERFOCAL_HELPER_EXE") {
        return PathBuf::from(p);
    }
    // Keeps working when the binary on disk is replaced while the server runs.
    let proc = Path::new("/proc/self/exe");
    if proc.exists() { proc.to_path_buf() } else { std::env::current_exe().unwrap_or_else(|_| PathBuf::from("hyperfocal")) }
}

fn run(state: &Arc<AppState>) {
    let cfg = state.ml.cfg.clone();
    let mut failures = 0u32;
    let mut fetch_failures = 0u32;
    let mut cpu_only: Option<String> = None;
    let mut cleaned = false;
    loop {
        // CPU or GPU.
        let (device, gpu, note) = choose_device(&cfg, cpu_only.as_deref());
        set(state, |s| {
            s.device = Some(device.as_str().into());
            s.gpu = gpu.clone();
            s.note = note.clone();
        });
        let token = {
            let b: [u8; 24] = rand::random();
            b.iter().map(|x| format!("{x:02x}")).collect::<String>()
        };
        let plan = match manifest::plan(&cfg, device, &token) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("AI features: {e:#}");
                set(state, |s| {
                    s.phase = "error".into();
                    s.error = Some(format!("{e:#}"));
                });
                std::thread::sleep(Duration::from_secs(3600));
                continue;
            }
        };
        // Downloads.
        if !plan.fetch.is_empty() {
            let total: u64 = plan.fetch.iter().map(|f| f.size()).sum();
            tracing::info!("AI features: downloading {} file(s), {} MB, into {}", plan.fetch.len(), total / 1_000_000, cfg.dir.display());
            set(state, |s| {
                s.phase = "downloading".into();
                s.error = None;
            });
            let done = std::sync::atomic::AtomicBool::new(false);
            let res = std::thread::scope(|sc| {
                // Live progress for the UI.
                sc.spawn(|| {
                    while !done.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(1000));
                        state.emit(Event::Ml(state.ml.status()));
                    }
                });
                let r = super::fetch::fetch_all(&plan.fetch, &state.ml.progress);
                done.store(true, std::sync::atomic::Ordering::Relaxed);
                r
            });
            if let Err(e) = res {
                fetch_failures += 1;
                let wait = Duration::from_secs((60u64 << (fetch_failures - 1).min(5)).min(1800));
                tracing::warn!("AI features: downloading the models failed: {e:#}; trying again in {} min", wait.as_secs() / 60);
                set(state, |s| {
                    s.phase = "waiting".into();
                    s.error = Some(format!("Could not download the AI models ({e:#}). Trying again in {} min.", wait.as_secs() / 60));
                });
                std::thread::sleep(wait);
                continue;
            }
            fetch_failures = 0;
            tracing::info!("AI features: downloads complete");
        }
        if !cleaned {
            cleaned = true;
            cleanup(&cfg.dir);
        }
        set(state, |s| s.disk_mb = super::fetch::disk_usage(&cfg.dir) / 1_000_000);
        // The engine.
        set(state, |s| {
            s.phase = "starting".into();
            s.error = None;
        });
        let started = Instant::now();
        match start(&plan) {
            Ok((mut child, port)) => {
                tracing::info!("AI engine ready on the {}{}: {}", if device == Device::Cuda { "GPU" } else { "CPU" }, gpu.as_ref().map(|g| format!(" ({g})")).unwrap_or_default(), features(&cfg));
                state.ml.set_endpoint(Some(Endpoint { base: format!("http://127.0.0.1:{port}"), token: token.clone() }));
                set(state, |s| {
                    s.phase = "ready".into();
                    s.error = None;
                });
                wake(state);
                // Child::wait closes the child's stdin, which tells the engine to stop:
                // keep our end open while it runs.
                let stdin = child.stdin.take();
                let status = child.wait();
                drop(stdin);
                state.ml.set_endpoint(None);
                let why = match status {
                    Ok(s) => describe_exit(s),
                    Err(e) => e.to_string(),
                };
                if started.elapsed() > Duration::from_secs(600) {
                    failures = 0;
                }
                failures += 1;
                tracing::error!("AI engine stopped ({why}); starting it again");
                if device == Device::Cuda && failures >= 2 && cpu_only.is_none() {
                    cpu_only = Some(format!("The AI engine stopped twice on the GPU ({why}): it runs on the CPU until Hyperfocal restarts."));
                }
                set(state, |s| {
                    s.phase = "waiting".into();
                    s.error = Some(format!("The AI engine stopped ({why}). Starting it again…"));
                });
            }
            Err(e) => {
                failures += 1;
                tracing::error!("AI engine did not start: {e:#}");
                if device == Device::Cuda && cpu_only.is_none() {
                    // Usually the GPU (memory, driver): the CPU works.
                    cpu_only = Some(format!("The AI engine could not start on the GPU ({e:#}): it runs on the CPU."));
                    continue;
                }
                set(state, |s| {
                    s.phase = "error".into();
                    s.error = Some(format!("The AI engine did not start: {e:#}"));
                });
            }
        }
        std::thread::sleep(Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300)));
    }
}

fn features(cfg: &super::MlConfig) -> String {
    let mut v = Vec::new();
    if cfg.faces_on() {
        v.push("people and pets");
    }
    if cfg.search_on() {
        v.push("search");
    }
    if cfg.eyes_on() {
        v.push("eyes");
    }
    v.join(", ")
}

/// Lets the workers waiting for the engine go.
fn wake(state: &AppState) {
    state.people.wake();
    state.visual.wake();
    state.cull.wake();
}

fn describe_exit(s: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (s.code(), s.signal()) {
        (_, Some(9)) => "killed: out of memory?".into(),
        (_, Some(sig)) => format!("crashed, signal {sig}"),
        (Some(c), _) => format!("exit code {c}"),
        _ => "stopped".into(),
    }
}

/// CPU or CUDA, the GPU's name, and a note when the configured choice could not be had.
fn choose_device(cfg: &super::MlConfig, cpu_only: Option<&str>) -> (Device, Option<String>, Option<String>) {
    if let Some(why) = cpu_only {
        return (Device::Cpu, None, Some(why.to_string()));
    }
    if cfg.device == DeviceChoice::Cpu {
        return (Device::Cpu, None, None);
    }
    let explicit = cfg.device == DeviceChoice::Cuda;
    let cpu = |why: String| -> (Device, Option<String>, Option<String>) {
        if explicit {
            tracing::warn!("AI features: HYPERFOCAL_ML_DEVICE=cuda, but {why}: running on the CPU");
            (Device::Cpu, None, Some(format!("HYPERFOCAL_ML_DEVICE is cuda, but {why}: the AI features run on the CPU.")))
        } else {
            (Device::Cpu, None, None)
        }
    };
    let Some(pack) = manifest::runtime("cuda") else {
        return cpu(format!("there is no GPU runtime for {}", manifest::arch()));
    };
    let g = match probe_gpu() {
        Ok(g) => g,
        Err(e) => return cpu(format!("{e:#}")),
    };
    if let Some(e) = g.error {
        return cpu(e);
    }
    if g.driver < pack.min_driver {
        let v = |d: i32| format!("{}.{}", d / 1000, d % 1000 / 10);
        let why = format!("the NVIDIA driver supports CUDA {} and the GPU runtime needs {} (driver 580 or newer)", v(g.driver), v(pack.min_driver));
        // A GPU that is there but cannot be used is worth a note even on auto.
        tracing::warn!("AI features: {why}: running on the CPU");
        return (Device::Cpu, None, Some(format!("An NVIDIA GPU was found, but {why}: the AI features run on the CPU.")));
    }
    let Some(dev) = g.devices.get(cfg.gpu as usize) else {
        return cpu(format!("there is no GPU number {} (the container sees {})", cfg.gpu, g.devices.len()));
    };
    (Device::Cuda, Some(format!("{}, {} GB", dev.name, (dev.memory_mb + 512) / 1024)), None)
}

/// Asks a short-lived child what GPU the driver sees (never in the server's process).
fn probe_gpu() -> Result<Gpus> {
    let mut cmd = Command::new(exe());
    cmd.arg("ml-engine").arg("probe").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().context("starting the GPU check")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the GPU check did not answer");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().expect("piped"), &mut out)?;
    serde_json::from_str(out.trim()).map_err(|e| anyhow!("the GPU check answered nonsense ({e})"))
}

/// Starts the engine and waits until it answers: the child and its port.
fn start(plan: &manifest::Plan) -> Result<(Child, u16)> {
    let mut cmd = Command::new(exe());
    cmd.arg("ml-engine")
        .env("HYPERFOCAL_ML_ENGINE", serde_json::to_string(&plan.engine)?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(dir) = &plan.lib_dir {
        // CUDA, cuDNN and ONNX Runtime's CUDA provider find each other here.
        let mut v = dir.as_os_str().to_owned();
        if let Some(old) = std::env::var_os("LD_LIBRARY_PATH").filter(|o| !o.is_empty()) {
            v.push(":");
            v.push(old);
        }
        cmd.env("LD_LIBRARY_PATH", v);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe. The engine dies with the server.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().context("starting the AI engine")?;
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("ml-out".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    // Nobody listens any more: keep draining so the engine never blocks.
                    continue;
                }
            }
        })
        .expect("spawn ml-out");
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.min(Duration::from_secs(1))) {
            Ok(line) => {
                if let Some(p) = line.strip_prefix(READY) {
                    let port: u16 = p.trim().parse().map_err(|_| anyhow!("the engine announced no port"))?;
                    return Ok((child, port));
                }
                if let Some(msg) = line.strip_prefix(FAILED) {
                    let _ = child.wait();
                    bail!("{}", msg.trim());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Some(s) = child.try_wait()? {
                    bail!("the engine {}", describe_exit(s));
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("the engine took longer than {} minutes to load the models", START_TIMEOUT.as_secs() / 60);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let s = child.wait()?;
                bail!("the engine {}", describe_exit(s));
            }
        }
    }
}

/// Removes what an older version downloaded that this one no longer uses (models, and
/// runtime folders, not in the manifest).
fn cleanup(dir: &Path) {
    let (models, runtimes) = manifest::known();
    let mut freed = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir.join("models")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let base = name.strip_suffix(".part").unwrap_or(&name);
            if !models.contains(&base) && e.file_type().is_ok_and(|t| t.is_file()) {
                freed += e.metadata().map(|m| m.len()).unwrap_or(0);
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(dir.join("runtime")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !runtimes.contains(&name.as_str()) && e.file_type().is_ok_and(|t| t.is_dir()) {
                freed += super::fetch::disk_usage(&e.path());
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    if freed > 0 {
        tracing::info!("AI features: removed {} MB of models and libraries no longer used", freed / 1_000_000);
    }
}
