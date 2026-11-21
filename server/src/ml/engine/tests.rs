//! The engine with real models. They run when ONNX Runtime is there (HYPERFOCAL_ORT_LIB,
//! as CI sets it); the eye models ship with the repository. The face, pet and search
//! tests also need the downloaded models: HYPERFOCAL_TEST_ML_MODELS, a folder holding
//! them (as /config/ml/models has them).

use super::imgproc::{self, Border};
use super::*;
use std::sync::{Arc, OnceLock};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ml")
}

fn models_dir() -> Option<PathBuf> {
    std::env::var_os("HYPERFOCAL_TEST_ML_MODELS").map(PathBuf::from).filter(|p| p.join("yunet.onnx").exists())
}

fn file(dir: &Path, name: &str) -> ModelFile {
    ModelFile { path: dir.join(name), id: name.into() }
}

/// One engine for every test (ONNX Runtime allows one environment a process): the eye
/// models always, the others when they are there. None without ONNX Runtime.
fn engine() -> Option<&'static Engine> {
    static E: OnceLock<Option<Engine>> = OnceLock::new();
    E.get_or_init(|| {
        let Some(ort_lib) = crate::ml::ort_library() else {
            eprintln!("ONNX Runtime not found (HYPERFOCAL_ORT_LIB): skipping the engine tests");
            return None;
        };
        let vendored = crate::ml::vendored_dir();
        let m = models_dir();
        let cfg = EngineConfig {
            ort_lib,
            device: Device::Cpu,
            gpu: 0,
            gpu_mem_mb: 0,
            threads: 0,
            workers: 1,
            token: "test".into(),
            faces: m.as_ref().map(|d| FaceModels {
                detector: file(d, "yunet.onnx"),
                embedder: file(d, "auraface.onnx"),
                animal_detector: file(d, "rfdetr-small.onnx"),
                pet_embedder: file(d, "pet-recognition.onnx"),
            }),
            search: m.as_ref().map(|d| SearchModels { vision: file(d, "siglip2-vision.onnx"), text: file(d, "siglip2-text.onnx"), tokenizer: d.join("siglip2-tokenizer.json") }),
            eyes: Some(EyeModels { landmarks: file(&vendored, "face-landmarks.onnx"), blendshapes: file(&vendored, "face-blendshapes.onnx") }),
            face_scales: vec![1600, 640, 320],
            face_threshold: 0.7,
            animal_threshold: 0.35,
            face_norm_low: 12.0,
            face_norm_high: 24.0,
        };
        Some(Engine::load(cfg).expect("the engine loads"))
    })
    .as_ref()
}

fn fixture(name: &str) -> Rgb {
    decode(&std::fs::read(fixtures().join(name)).unwrap()).unwrap()
}

#[test]
fn eyes_open_and_closed() {
    let Some(e) = engine() else { return };
    let eyes = e.eyes.as_ref().unwrap();
    let face = fixture("face-open.jpg");
    let r = eyes.check(&face).unwrap();
    assert!(r.p > 0.9 && r.closed < 0.2, "{r:?}");
    // Turned 30 degrees: found upright again, still open.
    let (cx, cy) = (face.w as f64 / 2.0, face.h as f64 / 2.0);
    let a = 30f64.to_radians();
    let (c, s) = (a.cos(), a.sin());
    // cv2.getRotationMatrix2D(centre, 30, 1)
    let m = [c, s, (1.0 - c) * cx - s * cy, -s, c, s * cx + (1.0 - c) * cy];
    let turned = Rgb { w: face.w, h: face.h, px: imgproc::warp_affine(&face.px, face.w, face.h, &m, face.w, face.h, Border::Replicate) };
    let t = eyes.check(&turned).unwrap();
    assert!(t.closed < 0.2 && t.angle.abs() > 10.0, "{t:?}");
    let shut = eyes.check(&fixture("face-closed.jpg")).unwrap();
    assert!(shut.p > 0.9 && shut.closed > 0.6, "{shut:?}");
}

#[test]
fn the_api_answers_and_checks_its_token() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let Some(e) = engine() else { return };
    // An engine of our own would need a second ONNX Runtime environment: borrow the
    // shared one for the router, which needs an Arc.
    let e: Arc<Engine> = Arc::new(Engine { cfg: e.cfg.clone(), device: e.device, rt: Runtime::new(&e.cfg).unwrap(), faces: None, pets: None, search: None, eyes: Some(eyes::Eyes::new(&Runtime::new(&e.cfg).unwrap(), e.cfg.eyes.as_ref().unwrap()).unwrap()), gate: Gate::new(1), started: Instant::now(), analyzed: AtomicU64::new(0), busy_us: AtomicU64::new(0) });
    let app = serve::router(e);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let req = |path: &str, auth: bool, body: Vec<u8>| {
            let mut r = Request::builder().method(if body.is_empty() { "GET" } else { "POST" }).uri(path);
            if auth {
                r = r.header("Authorization", "Bearer test");
            }
            r.body(Body::from(body)).unwrap()
        };
        let health = app.clone().oneshot(req("/health", false, vec![])).await.unwrap();
        assert_eq!(health.status(), 200);
        assert_eq!(app.clone().oneshot(req("/v1/info", false, vec![])).await.unwrap().status(), 401);
        let info = app.clone().oneshot(req("/v1/info", true, vec![])).await.unwrap();
        assert_eq!(info.status(), 200);
        let face = std::fs::read(fixtures().join("face-closed.jpg")).unwrap();
        let r = app.clone().oneshot(req("/v1/eyes", true, face)).await.unwrap();
        assert_eq!(r.status(), 200);
        let body = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["eyes"][0]["closed"].as_f64().unwrap() > 0.6, "{v}");
        // Faces are off in this engine: 501, not a crash.
        assert_eq!(app.clone().oneshot(req("/v1/analyze", true, b"x".to_vec())).await.unwrap().status(), 501);
        assert_eq!(app.clone().oneshot(req("/v1/eyes", true, b"not an image".to_vec())).await.unwrap().status(), 200);
    });
}

#[test]
fn analysis_answers_in_normalized_coordinates() {
    let Some(e) = engine() else { return };
    if e.faces.is_none() {
        eprintln!("no downloaded models (HYPERFOCAL_TEST_ML_MODELS): skipping");
        return;
    }
    // A face-free image: nothing found.
    let mut img = Rgb { w: 640, h: 480, px: vec![200u8; 640 * 480 * 3] };
    for y in 100..300 {
        for x in 100..300 {
            img.px[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3].copy_from_slice(&[160, 90, 40]);
        }
    }
    let jpeg = crate::images::jpeg::encode(&img, 90, false).unwrap();
    let v = e.analyze(&jpeg, true, true, None, None).unwrap();
    assert_eq!((v["w"].as_u64(), v["h"].as_u64()), (Some(640), Some(480)));
    assert!(v["faces"].as_array().unwrap().is_empty() && v["animals"].as_array().unwrap().is_empty(), "{v}");
    // The portrait has one face, boxed in 0..1 with five landmarks and an embedding.
    let data = std::fs::read(fixtures().join("face-open.jpg")).unwrap();
    let v = e.analyze(&data, true, true, None, None).unwrap();
    let faces = v["faces"].as_array().unwrap();
    assert_eq!(faces.len(), 1, "{v}");
    let f = &faces[0];
    assert!(f["box"].as_array().unwrap().iter().all(|x| (0.0..=1.0).contains(&x.as_f64().unwrap())));
    assert_eq!(f["lm"].as_array().unwrap().len(), 10);
    assert!(f["q"].as_f64().unwrap() > 0.3, "{f}");
    let emb = crate::ml::client::decode_embedding(f["emb"].as_str().unwrap()).unwrap();
    assert_eq!(emb.len(), 512);
    assert!(matches!(e.analyze(b"not an image", true, true, None, None), Err(AnalyzeError::Image(_))));
}

#[test]
fn search_embeddings_match_images_with_descriptions() {
    let Some(e) = engine() else { return };
    let Some(s) = &e.search else {
        eprintln!("no downloaded models (HYPERFOCAL_TEST_ML_MODELS): skipping");
        return;
    };
    // The tokens SigLIP 2 was trained on: lower case, <eos> (1), padded with <pad> (0) to 64.
    let ids = s.tokens(&["A Red Car".into()]).unwrap();
    assert_eq!(ids.len(), 64);
    assert!(ids.contains(&1) && ids[63] == 0);
    let solid = |c: [u8; 3]| Rgb { w: 320, h: 240, px: std::iter::repeat_n(c, 320 * 240).flatten().collect() };
    let (red, blue) = (solid([220, 30, 30]), solid([30, 60, 220]));
    let img = s.embed_images(&[&red, &blue]).unwrap();
    let txt = s.embed_texts(&e.rt, &["a red picture".into(), "a blue picture".into()]).unwrap();
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
    assert!(img.iter().all(|v| v.len() == search::DIM && (dot(v, v) - 1.0).abs() < 1e-4));
    assert!(dot(&txt[0], &img[0]) > dot(&txt[0], &img[1]) && dot(&txt[1], &img[1]) > dot(&txt[1], &img[0]));
}
