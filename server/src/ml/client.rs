//! Client for the AI engine process (`super::engine`), which answers on 127.0.0.1. A
//! client from `Ml::client` finds the engine for each request (its port changes when it
//! restarts) and starts it first if it was stopped for want of work.

use super::{Endpoint, Lease, Link};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct Client {
    target: Target,
    agent: ureq::Agent,
}

#[derive(Clone)]
enum Target {
    Fixed { base: String, token: Option<String> },
    Managed(Arc<Link>),
}

/// Where one request goes, held for its duration.
struct Dest {
    base: String,
    token: Option<String>,
    _lease: Option<Lease>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Face {
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
    pub score: f32,
    #[serde(default)]
    pub lm: Vec<f32>,
    #[serde(default)]
    pub q: f32,
    #[serde(default)]
    pub px: f32,
    #[serde(default)]
    pub emb: Option<String>,
}

/// The eyes of a face in a close-up (`/v1/eyes`).
#[derive(Debug, Clone, Deserialize)]
pub struct EyeCheck {
    /// 0 open .. 1 closed.
    pub closed: f32,
    /// How surely the close-up shows a face (0..1): below one half, the answer is a guess.
    pub p: f32,
    /// The eyes' centres in the close-up (0..1), picture left first.
    #[serde(default)]
    pub eyes: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Animal {
    #[serde(rename = "box")]
    pub bbox: [f32; 4],
    pub score: f32,
    pub species: String,
    #[serde(default)]
    pub px: f32,
    #[serde(default)]
    pub emb: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Analysis {
    pub w: u32,
    pub h: u32,
    #[serde(default)]
    pub faces: Vec<Face>,
    #[serde(default)]
    pub animals: Vec<Animal>,
}

/// The HTTP agent for the engine (connections are reused between requests).
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        // On the CPU a crowded group photo can take several seconds, and requests queue
        // behind each other in the engine.
        .timeout_global(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        // Never through a proxy set for the container's downloads: the engine is local.
        .proxy(None)
        .build()
        .into()
}

impl Client {
    /// A client for an engine at a fixed address.
    pub fn new(base: &str, token: Option<String>, agent: ureq::Agent) -> Client {
        Client { target: Target::Fixed { base: base.trim_end_matches('/').to_string(), token }, agent }
    }

    pub(crate) fn managed(link: Arc<Link>, agent: ureq::Agent) -> Client {
        Client { target: Target::Managed(link), agent }
    }

    /// The engine for one request; `work` wakes a sleeping engine and keeps it awake.
    fn dest(&self, work: bool) -> std::result::Result<Dest, MlError> {
        match &self.target {
            Target::Fixed { base, token } => Ok(Dest { base: base.clone(), token: token.clone(), _lease: None }),
            Target::Managed(link) => {
                let (lease, Endpoint { base, token }) = link.lease(work)?;
                Ok(Dest { base, token: Some(token), _lease: Some(lease) })
            }
        }
    }

    fn auth<B>(d: &Dest, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &d.token {
            Some(t) => req.header("Authorization", &format!("Bearer {t}")),
            None => req,
        }
    }

    /// The service's version, device and models (with the content ids that tell
    /// whether stored embeddings still match). A sleeping engine is not woken for it:
    /// the answer is the one it gave when it last ran.
    pub fn info(&self) -> Result<serde_json::Value> {
        if let Target::Managed(link) = &self.target {
            if link.asleep() {
                if let Some(v) = link.info.read().clone() {
                    return Ok(v);
                }
            }
        }
        let d = self.dest(false).map_err(|e| anyhow!("{e}"))?;
        let mut r = Self::auth(&d, self.agent.get(&format!("{}/v1/info", d.base))).call().with_context(|| "reaching the AI engine".to_string())?;
        if !r.status().is_success() {
            bail!("the AI engine answered {}", r.status());
        }
        let body = r.body_mut().read_to_string()?;
        let v: serde_json::Value = serde_json::from_str(&body)?;
        if let Target::Managed(link) = &self.target {
            *link.info.write() = Some(v.clone());
        }
        Ok(v)
    }

    /// Faces and animals in an upright JPEG.
    pub fn analyze(&self, jpeg: &[u8]) -> std::result::Result<Analysis, MlError> {
        let d = self.dest(true)?;
        let mut r = Self::auth(&d, self.agent.post(&format!("{}/v1/analyze", d.base)))
            .content_type("image/jpeg")
            .send(jpeg)
            .map_err(|e| MlError::Unreachable(format!("the AI engine: {e}")))?;
        let status = r.status();
        let body = r.body_mut().with_config().limit(64 << 20).read_to_string().map_err(|e| MlError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or(body);
            let msg = format!("the AI engine answered {status}: {msg}");
            // 4xx: this image; 5xx (a model failed, the GPU ran out of memory): the engine.
            return Err(if status.is_client_error() && status.as_u16() != 501 { MlError::Rejected(msg) } else { MlError::Unreachable(msg) });
        }
        serde_json::from_str(&body).map_err(|e| MlError::Rejected(format!("unexpected answer from the AI engine: {e}")))
    }
}

impl Client {
    /// SigLIP 2 embeddings of encoded images (None for one the service could not
    /// decode), normalized.
    pub fn embed_images(&self, images: &[Vec<u8>]) -> std::result::Result<Vec<Option<Vec<f32>>>, MlError> {
        let mut body = Vec::with_capacity(images.iter().map(|i| i.len() + 4).sum());
        for img in images {
            body.extend_from_slice(&(img.len() as u32).to_le_bytes());
            body.extend_from_slice(img);
        }
        let v = self.post_for_embs("/v1/embed", "application/x-hfml-batch", &body, crate::visual::DIM)?;
        if v.len() != images.len() {
            return Err(MlError::Rejected(format!("the AI engine answered {} embeddings for {} images", v.len(), images.len())));
        }
        Ok(v)
    }

    /// Pet re-identification embeddings of crops of dogs or cats (encoded images; None for
    /// one the engine could not read), normalized: in the space of the animals
    /// `analyze` finds.
    pub fn embed_pets(&self, crops: &[Vec<u8>]) -> std::result::Result<Vec<Option<Vec<f32>>>, MlError> {
        let mut body = Vec::with_capacity(crops.iter().map(|i| i.len() + 4).sum());
        for img in crops {
            body.extend_from_slice(&(img.len() as u32).to_le_bytes());
            body.extend_from_slice(img);
        }
        let v = self.post_for_embs("/v1/pets", "application/x-hfml-batch", &body, crate::people::DIM)?;
        if v.len() != crops.len() {
            return Err(MlError::Rejected(format!("the AI engine answered {} embeddings for {} animals", v.len(), crops.len())));
        }
        Ok(v)
    }

    /// SigLIP 2 embeddings of texts, in the images' space.
    pub fn embed_texts(&self, texts: &[String]) -> std::result::Result<Vec<Vec<f32>>, MlError> {
        let body = serde_json::json!({ "texts": texts }).to_string();
        let v = self.post_for_embs("/v1/text", "application/json", body.as_bytes(), crate::visual::DIM)?;
        v.into_iter().map(|e| e.ok_or_else(|| MlError::Rejected("the AI engine sent no embedding for a text".into()))).collect()
    }

    /// Open or closed eyes of the face in each close-up (see `crate::cull`): None for a
    /// close-up the engine could not read.
    pub fn check_eyes(&self, crops: &[Vec<u8>]) -> std::result::Result<Vec<Option<EyeCheck>>, MlError> {
        let mut body = Vec::with_capacity(crops.iter().map(|i| i.len() + 4).sum());
        for img in crops {
            body.extend_from_slice(&(img.len() as u32).to_le_bytes());
            body.extend_from_slice(img);
        }
        let v = self.post_json("/v1/eyes", "application/x-hfml-batch", &body)?;
        let list = v["eyes"].as_array().ok_or_else(|| MlError::Rejected("unexpected answer from the AI engine".into()))?;
        if list.len() != crops.len() {
            return Err(MlError::Rejected(format!("the AI engine answered {} eye checks for {} faces", list.len(), crops.len())));
        }
        Ok(list.iter().map(|e| serde_json::from_value::<EyeCheck>(e.clone()).ok()).collect())
    }

    fn post_json(&self, path: &str, ctype: &str, body: &[u8]) -> std::result::Result<serde_json::Value, MlError> {
        let d = self.dest(true)?;
        let mut r = Self::auth(&d, self.agent.post(&format!("{}{path}", d.base)))
            .content_type(ctype)
            .send(body)
            .map_err(|e| MlError::Unreachable(format!("the AI engine: {e}")))?;
        let status = r.status();
        let text = r.body_mut().with_config().limit(64 << 20).read_to_string().map_err(|e| MlError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or(text);
            let msg = format!("the AI engine answered {status}: {msg}");
            // 501: the engine lacks the models. 4xx: this request. 5xx: the engine.
            return Err(if status.is_client_error() && status.as_u16() != 501 { MlError::Rejected(msg) } else { MlError::Unreachable(msg) });
        }
        serde_json::from_str(&text).map_err(|e| MlError::Rejected(format!("unexpected answer from the AI engine: {e}")))
    }

    fn post_for_embs(&self, path: &str, ctype: &str, body: &[u8], dim: usize) -> std::result::Result<Vec<Option<Vec<f32>>>, MlError> {
        let d = self.dest(true)?;
        let mut r = Self::auth(&d, self.agent.post(&format!("{}{path}", d.base)))
            .content_type(ctype)
            .send(body)
            .map_err(|e| MlError::Unreachable(format!("the AI engine: {e}")))?;
        let status = r.status();
        let text = r.body_mut().with_config().limit(64 << 20).read_to_string().map_err(|e| MlError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or(text);
            let msg = format!("the AI engine answered {status}: {msg}");
            // 501: the engine lacks the models. 4xx: this request. 5xx: the engine.
            return Err(if status.is_client_error() && status.as_u16() != 501 { MlError::Rejected(msg) } else { MlError::Unreachable(msg) });
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| MlError::Rejected(format!("unexpected answer from the AI engine: {e}")))?;
        let embs = v["embs"].as_array().ok_or_else(|| MlError::Rejected("unexpected answer from the AI engine".into()))?;
        Ok(embs.iter().map(|e| e.as_str().and_then(|s| decode_f16(s, dim).ok())).collect())
    }
}

/// A normalized vector sent as base64 of little-endian float16.
pub fn decode_f16(b64: &str, dim: usize) -> Result<Vec<f32>> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())?;
    if bytes.len() != dim * 2 {
        return Err(anyhow!("a vector of {} bytes (expected {})", bytes.len(), dim * 2));
    }
    let mut v: Vec<f32> = bytes.chunks_exact(2).map(|b| half::f16::from_le_bytes([b[0], b[1]]).to_f32()).collect();
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !n.is_finite() || n == 0.0 {
        bail!("an empty vector");
    }
    for x in &mut v {
        *x /= n;
    }
    Ok(v)
}

#[derive(Debug)]
pub enum MlError {
    /// The engine is not ready yet: the models are being downloaded or loaded.
    Preparing(String),
    /// The engine is down or failing: try again later.
    Unreachable(String),
    /// The engine could not handle this image.
    Rejected(String),
}

impl std::fmt::Display for MlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MlError::Preparing(s) | MlError::Unreachable(s) | MlError::Rejected(s) => f.write_str(s),
        }
    }
}

/// An embedding as the engine sends it (base64 of little-endian float16), normalized.
pub fn decode_embedding(b64: &str) -> Result<Vec<f32>> {
    use base64::Engine;
    let dim = crate::people::DIM;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())?;
    if bytes.len() != dim * 2 {
        return Err(anyhow!("an embedding of {} bytes (expected {})", bytes.len(), dim * 2));
    }
    let mut v: Vec<f32> = bytes.chunks_exact(2).map(|b| half::f16::from_le_bytes([b[0], b[1]]).to_f32()).collect();
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if !n.is_finite() || n == 0.0 {
        bail!("an empty embedding");
    }
    for x in &mut v {
        *x /= n;
    }
    Ok(v)
}

/// The model ids that define the embedding spaces: faces and pets.
pub fn embedders(info: &serde_json::Value) -> (String, String) {
    let id = |k: &str| info["models"][k]["id"].as_str().unwrap_or("").to_string();
    (id("face_embedder"), id("pet_embedder"))
}

/// Every model id, for the `analysis` table (a photo analysed with other models is
/// analysed again).
pub fn models_key(info: &serde_json::Value) -> String {
    let id = |k: &str| info["models"][k]["id"].as_str().unwrap_or("-").to_string();
    format!("{}/{}/{}/{}", id("face_detector"), id("face_embedder"), id("animal_detector"), id("pet_embedder"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeddings() {
        use base64::Engine;
        let mut raw = Vec::new();
        for i in 0..crate::people::DIM {
            raw.extend_from_slice(&half::f16::from_f32(if i == 3 { 2.0 } else { 0.0 }).to_le_bytes());
        }
        let v = decode_embedding(&base64::engine::general_purpose::STANDARD.encode(&raw)).unwrap();
        assert_eq!(v.len(), 512);
        assert!((v[3] - 1.0).abs() < 1e-6);
        assert!(decode_embedding("AAAA").is_err());
        let info: serde_json::Value = serde_json::json!({"models": {"face_embedder": {"id": "abc"}, "pet_embedder": {"id": "def"}, "face_detector": {"id": "y"}}});
        assert_eq!(embedders(&info), ("abc".into(), "def".into()));
        assert_eq!(models_key(&info), "y/abc/-/def");
    }
}
