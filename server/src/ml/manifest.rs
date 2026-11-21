//! What the AI features need on disk (`manifest.json`, built into the binary): the models,
//! and ONNX Runtime for the CPU or an NVIDIA GPU. `plan` turns the configuration into the
//! files to have and the engine's configuration.

use super::engine::{Device, EngineConfig, EyeModels, FaceModels, ModelFile, SearchModels};
use super::MlConfig;
use anyhow::{Result, anyhow};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub models: Vec<Model>,
    pub runtimes: Vec<RuntimePack>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    pub file: String,
    /// faces (people and pets), search, eyes
    pub feature: String,
    /// The pet detector's size: small or nano.
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    pub sha256: String,
    pub size: u64,
    /// Shipped in the image (ml/models), not downloaded.
    #[serde(default)]
    pub bundled: bool,
    pub what: String,
    pub license: String,
    pub home: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimePack {
    /// cpu or cuda
    pub name: String,
    pub arch: String,
    /// Folder under ml/runtime/.
    pub dir: String,
    /// ONNX Runtime's library in it.
    pub lib: String,
    /// Oldest CUDA driver (cuDriverGetVersion) it runs on.
    #[serde(default)]
    pub min_driver: i32,
    pub wheels: Vec<Wheel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Wheel {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Members to take out of it (into the pack's folder, without their paths).
    pub files: Vec<String>,
}

impl Wheel {
    pub fn name(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or(&self.url)
    }
}

pub fn manifest() -> &'static Manifest {
    static M: OnceLock<Manifest> = OnceLock::new();
    M.get_or_init(|| serde_json::from_str(include_str!("manifest.json")).expect("ml/manifest.json is valid"))
}

/// Something to download: a model file, or a wheel to take files out of.
#[derive(Debug, Clone)]
pub enum Fetch {
    Model { url: String, sha256: String, size: u64, dest: PathBuf, what: String },
    Wheel { wheel: Wheel, dir: PathBuf },
}

impl Fetch {
    pub fn size(&self) -> u64 {
        match self {
            Fetch::Model { size, .. } => *size,
            Fetch::Wheel { wheel, .. } => wheel.size,
        }
    }
}

/// The files the configuration needs, and the engine's configuration once they are there.
pub struct Plan {
    pub fetch: Vec<Fetch>,
    pub engine: EngineConfig,
    /// Folder of the CUDA libraries (LD_LIBRARY_PATH for the engine), on the GPU.
    pub lib_dir: Option<PathBuf>,
}

pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

pub fn runtime(name: &str) -> Option<&'static RuntimePack> {
    manifest().runtimes.iter().find(|r| r.name == name && r.arch == arch())
}

/// The id of a model: the first 12 hex digits of its sha256 (as the stored analyses are
/// tagged).
fn id(m: &Model) -> String {
    m.sha256[..12].to_string()
}

/// The models of the enabled features.
pub fn models(cfg: &MlConfig) -> Vec<&'static Model> {
    manifest()
        .models
        .iter()
        .filter(|m| match m.feature.as_str() {
            "faces" => cfg.faces_on() && m.variant.as_deref().is_none_or(|v| v == cfg.pet_detector),
            "search" => cfg.search_on(),
            "eyes" => cfg.eyes_on(),
            _ => false,
        })
        .collect()
}

/// Where a model is: bundled ones in the image, the others in ml/models.
pub fn model_path(cfg: &MlConfig, m: &Model) -> PathBuf {
    if m.bundled { super::vendored_dir().join(&m.file) } else { cfg.dir.join("models").join(&m.file) }
}

/// Whether a wheel's files are all in `dir` (written there after the wheel checked out).
pub fn wheel_done(w: &Wheel, dir: &Path) -> bool {
    dir.join(format!(".{}", &w.sha256[..16])).exists() && w.files.iter().all(|f| dir.join(file_name(f)).exists())
}

pub fn file_name(member: &str) -> &str {
    member.rsplit('/').next().unwrap_or(member)
}

pub fn plan(cfg: &MlConfig, device: Device, token: &str) -> Result<Plan> {
    let mut fetch = Vec::new();
    let mut file = |name: &str| -> Result<ModelFile> {
        let m = manifest().models.iter().find(|m| m.file == name).ok_or_else(|| anyhow!("{name} is not in the manifest"))?;
        let path = model_path(cfg, m);
        if m.bundled {
            if !path.exists() {
                return Err(anyhow!("{} is missing from the image ({})", m.file, path.display()));
            }
        } else if std::fs::metadata(&path).map(|md| md.len()).ok() != Some(m.size) {
            fetch.push(Fetch::Model { url: m.url.clone().unwrap_or_default(), sha256: m.sha256.clone(), size: m.size, dest: path.clone(), what: m.what.clone() });
        }
        Ok(ModelFile { path, id: id(m) })
    };
    let faces = if cfg.faces_on() {
        let pet = format!("rfdetr-{}.onnx", cfg.pet_detector);
        Some(FaceModels { detector: file("yunet.onnx")?, embedder: file("auraface.onnx")?, animal_detector: file(&pet)?, pet_embedder: file("pet-recognition.onnx")? })
    } else {
        None
    };
    let search = if cfg.search_on() {
        let tok = file("siglip2-tokenizer.json")?;
        Some(SearchModels { vision: file("siglip2-vision.onnx")?, text: file("siglip2-text.onnx")?, tokenizer: tok.path })
    } else {
        None
    };
    let eyes = if cfg.eyes_on() { Some(EyeModels { landmarks: file("face-landmarks.onnx")?, blendshapes: file("face-blendshapes.onnx")? }) } else { None };

    // ONNX Runtime: the image's for the CPU (or a download when it has none), the GPU pack
    // for CUDA.
    let (ort_lib, lib_dir) = match device {
        Device::Cpu => match super::ort_library() {
            Some(p) => (p, None),
            None => {
                let pack = runtime("cpu").ok_or_else(|| anyhow!("no ONNX Runtime for {} in the manifest", arch()))?;
                let dir = cfg.dir.join("runtime").join(&pack.dir);
                for w in &pack.wheels {
                    if !wheel_done(w, &dir) {
                        fetch.push(Fetch::Wheel { wheel: w.clone(), dir: dir.clone() });
                    }
                }
                (dir.join(&pack.lib), None)
            }
        },
        Device::Cuda => {
            let pack = runtime("cuda").ok_or_else(|| anyhow!("no GPU runtime for {} in the manifest", arch()))?;
            let dir = cfg.dir.join("runtime").join(&pack.dir);
            for w in &pack.wheels {
                if !wheel_done(w, &dir) {
                    fetch.push(Fetch::Wheel { wheel: w.clone(), dir: dir.clone() });
                }
            }
            (dir.join(&pack.lib), Some(dir))
        }
    };
    let engine = EngineConfig {
        ort_lib,
        device,
        gpu: cfg.gpu,
        gpu_mem_mb: cfg.gpu_mem_mb,
        threads: cfg.threads,
        workers: cfg.workers,
        token: token.to_string(),
        faces,
        search,
        eyes,
        face_scales: cfg.face_scales.clone(),
        face_threshold: 0.7,
        animal_threshold: 0.35,
        face_norm_low: 12.0,
        face_norm_high: 24.0,
    };
    Ok(Plan { fetch, engine, lib_dir })
}

/// Every file name the manifest knows in ml/models, and every runtime folder: anything
/// else there is left over from an older version.
pub fn known() -> (Vec<&'static str>, Vec<&'static str>) {
    let m = manifest();
    (m.models.iter().map(|m| m.file.as_str()).collect(), m.runtimes.iter().map(|r| r.dir.as_str()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_complete() {
        let m = manifest();
        for x in &m.models {
            assert_eq!(x.sha256.len(), 64, "{}", x.file);
            assert!(x.bundled || x.url.as_deref().is_some_and(|u| u.starts_with("https://")), "{}", x.file);
            assert!(x.size > 0);
        }
        for f in ["yunet.onnx", "auraface.onnx", "rfdetr-small.onnx", "rfdetr-nano.onnx", "pet-recognition.onnx", "siglip2-vision.onnx", "siglip2-text.onnx", "siglip2-tokenizer.json", "face-landmarks.onnx", "face-blendshapes.onnx"] {
            assert!(m.models.iter().any(|x| x.file == f), "{f}");
        }
        for r in &m.runtimes {
            assert!(!r.wheels.is_empty() && r.wheels.iter().all(|w| w.sha256.len() == 64 && !w.files.is_empty()));
            assert!(r.wheels.iter().any(|w| w.files.iter().any(|f| file_name(f) == r.lib)), "{} has its library", r.dir);
        }
        assert!(m.runtimes.iter().any(|r| r.name == "cuda" && r.arch == "x86_64"));
    }

    #[test]
    fn the_image_bakes_in_the_manifests_cpu_runtime() {
        let docker = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../Dockerfile")).expect("Dockerfile");
        let arg = |k: &str| -> String {
            let line = docker.lines().find(|l| l.starts_with(&format!("ARG {k}="))).unwrap_or_else(|| panic!("ARG {k} in the Dockerfile"));
            line.split_once('=').unwrap().1.trim().to_string()
        };
        for (arch, suffix) in [("x86_64", "AMD64"), ("aarch64", "ARM64")] {
            let r = manifest().runtimes.iter().find(|r| r.name == "cpu" && r.arch == arch).expect("cpu runtime");
            assert_eq!(arg(&format!("ORT_WHEEL_{suffix}")), r.wheels[0].url, "{arch}");
            assert_eq!(arg(&format!("ORT_SHA256_{suffix}")), r.wheels[0].sha256, "{arch}");
            assert!(docker.contains(&r.wheels[0].files[0]) && docker.contains(&format!("/usr/lib/hyperfocal/{}", r.lib)));
        }
    }

    #[test]
    fn bundled_models_match_the_manifest() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ml/models");
        for x in manifest().models.iter().filter(|m| m.bundled) {
            let data = std::fs::read(dir.join(&x.file)).expect("bundled model");
            use sha2::Digest;
            assert_eq!(format!("{:x}", sha2::Sha256::digest(&data)), x.sha256, "{}", x.file);
            assert_eq!(data.len() as u64, x.size);
        }
    }
}
