//! The AI engine's GPU runtime: downloads ONNX Runtime's CUDA build and NVIDIA's CUDA 13
//! and cuDNN 9 libraries (about 1.3 GB) as Hyperfocal does on a server with an NVIDIA GPU,
//! and checks that every library they need is among them (all but the driver's
//! libcuda.so.1, which the NVIDIA container runtime brings). Run it after changing the
//! runtimes in `src/ml/manifest.json`:
//!
//!     cargo test --release --test ml_runtime -- --ignored --nocapture
//!
//! HYPERFOCAL_TEST_ML_DIR keeps the download (default: a temporary folder).

use hyperfocal::ml::fetch::{Progress, fetch_all};
use hyperfocal::ml::manifest::{Fetch, file_name, runtime, wheel_done};
use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore]
fn gpu_runtime_downloads_and_links() {
    let pack = runtime("cuda").expect("a CUDA runtime for this architecture");
    let tmp = tempfile::tempdir().unwrap();
    let root = std::env::var_os("HYPERFOCAL_TEST_ML_DIR").map(PathBuf::from).unwrap_or_else(|| tmp.path().to_path_buf());
    let dir = root.join("runtime").join(&pack.dir);
    let list: Vec<Fetch> = pack.wheels.iter().filter(|w| !wheel_done(w, &dir)).map(|w| Fetch::Wheel { wheel: w.clone(), dir: dir.clone() }).collect();
    let t = std::time::Instant::now();
    fetch_all(&list, &Progress::default()).expect("the downloads");
    println!("downloaded {} wheel(s) in {:.0}s", list.len(), t.elapsed().as_secs_f64());
    for w in &pack.wheels {
        assert!(wheel_done(w, &dir), "{} unpacked", w.name());
    }
    // Every library's dependencies resolve within the folder (or the system), except the
    // driver.
    let mut missing = Vec::new();
    for w in &pack.wheels {
        for f in &w.files {
            let lib = dir.join(file_name(f));
            let out = Command::new("ldd").arg(&lib).env("LD_LIBRARY_PATH", &dir).output().expect("ldd");
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.contains("not found") && !line.contains("libcuda.so.1") {
                    missing.push(format!("{}: {}", file_name(f), line.trim()));
                }
            }
        }
    }
    assert!(missing.is_empty(), "unresolved libraries:\n{}", missing.join("\n"));
    let size: u64 = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.metadata().map(|m| m.len()).unwrap_or(0)).sum();
    println!("{} MB in {}", size >> 20, dir.display());
}
