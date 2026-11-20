//! Client for the companion ML service (`hyperfocal-ml`, see `ml/` in the repository).

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::time::Duration;

pub struct Client {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
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

impl Client {
    pub fn new(base: &str, token: Option<String>) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            // A CPU-only service can take several seconds for a crowded group photo.
            .timeout_global(Some(Duration::from_secs(180)))
            .http_status_as_error(false)
            .build()
            .into();
        Client { base: base.trim_end_matches('/').to_string(), token, agent }
    }

    fn auth<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.token {
            Some(t) => req.header("Authorization", &format!("Bearer {t}")),
            None => req,
        }
    }

    /// The service's version, device and models (with the content ids that tell
    /// whether stored embeddings still match).
    pub fn info(&self) -> Result<serde_json::Value> {
        let mut r = self.auth(self.agent.get(&format!("{}/v1/info", self.base))).call().with_context(|| format!("reaching {}", self.base))?;
        if !r.status().is_success() {
            bail!("{} answered {}", self.base, r.status());
        }
        let body = r.body_mut().read_to_string()?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Faces and animals in an upright JPEG.
    pub fn analyze(&self, jpeg: &[u8]) -> std::result::Result<Analysis, MlError> {
        let mut r = self
            .auth(self.agent.post(&format!("{}/v1/analyze", self.base)))
            .content_type("image/jpeg")
            .send(jpeg)
            .map_err(|e| MlError::Unreachable(format!("{}: {e}", self.base)))?;
        let status = r.status();
        let body = r.body_mut().with_config().limit(64 << 20).read_to_string().map_err(|e| MlError::Unreachable(e.to_string()))?;
        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or(body);
            let msg = format!("the ML service answered {status}: {msg}");
            // 4xx: this image; 5xx (a model failed, the GPU ran out of memory): the service.
            return Err(if status.is_client_error() { MlError::Rejected(msg) } else { MlError::Unreachable(msg) });
        }
        serde_json::from_str(&body).map_err(|e| MlError::Rejected(format!("unexpected answer from the ML service: {e}")))
    }
}

#[derive(Debug)]
pub enum MlError {
    /// The service is down or failing: try again later.
    Unreachable(String),
    /// The service could not handle this image.
    Rejected(String),
}

impl std::fmt::Display for MlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MlError::Unreachable(s) | MlError::Rejected(s) => f.write_str(s),
        }
    }
}

/// An embedding as the service sends it (base64 of little-endian float16), normalized.
pub fn decode_embedding(b64: &str) -> Result<Vec<f32>> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())?;
    if bytes.len() != super::DIM * 2 {
        return Err(anyhow!("an embedding of {} bytes (expected {})", bytes.len(), super::DIM * 2));
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
        for i in 0..super::super::DIM {
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
