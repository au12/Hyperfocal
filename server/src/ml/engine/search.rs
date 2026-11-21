//! Search by content: SigLIP 2 image and text embeddings in one 768-dimensional space.
//!
//! A photo and a description of it lie close together (the cosine of a match is small in
//! absolute terms, about 0.10 to 0.20), and two photos of the same scene lie very close
//! (about 0.85 and up). Images are squashed to 224 x 224 (no crop: SigLIP's own
//! preprocessing), pixel values scaled to -1..1. Text is lower-cased, ends with <eos> and
//! is padded to 64 tokens, as SigLIP 2 was trained.

use super::imgproc::{self, Interp};
use super::{Runtime, SearchModels, chw, oe};
use crate::images::jpeg::Rgb;
use anyhow::{Result, anyhow};
use ort::session::Session;
use ort::value::TensorRef;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::OnceLock;
use tokenizers::Tokenizer;

pub const SIZE: usize = 224;
pub const TOKENS: usize = 64;
pub const DIM: usize = 768;

pub struct Search {
    vision: Mutex<Session>,
    vision_out: usize,
    text_path: PathBuf,
    text: OnceLock<Mutex<Session>>,
    text_loading: Mutex<()>,
    tokenizer: Tokenizer,
}

impl Search {
    pub fn new(vision: Session, m: &SearchModels) -> Result<Search> {
        let vision_out = vision.outputs().iter().position(|o| o.name() == "pooler_output").unwrap_or(vision.outputs().len().saturating_sub(1));
        let mut tokenizer = Tokenizer::from_file(&m.tokenizer).map_err(|e| anyhow!("loading the tokenizer: {e}"))?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams { max_length: TOKENS, ..Default::default() }))
            .map_err(|e| anyhow!("tokenizer: {e}"))?;
        let pad = tokenizer.token_to_id("<pad>").unwrap_or(0);
        tokenizer.with_padding(Some(tokenizers::PaddingParams {
            strategy: tokenizers::PaddingStrategy::Fixed(TOKENS),
            pad_id: pad,
            pad_token: "<pad>".into(),
            ..Default::default()
        }));
        Ok(Search { vision: Mutex::new(vision), vision_out, text_path: m.text.path.clone(), text: OnceLock::new(), text_loading: Mutex::new(()), tokenizer })
    }

    /// The image as the model takes it: 224 x 224, -1..1, CHW.
    fn prepare(img: &Rgb, out: &mut Vec<f32>) {
        let interp = if img.w.min(img.h) > SIZE { Interp::Area } else { Interp::Cubic };
        let px = imgproc::resize(&img.px, img.w, img.h, SIZE, SIZE, interp);
        chw(&px, SIZE, SIZE, out, |_, v| v as f32 / 127.5 - 1.0);
    }

    pub fn embed_images(&self, imgs: &[&Rgb]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(imgs.len());
        for part in imgs.chunks(16) {
            let mut x = Vec::with_capacity(part.len() * 3 * SIZE * SIZE);
            for img in part {
                Self::prepare(img, &mut x);
            }
            let mut sess = self.vision.lock();
            let input = TensorRef::from_array_view(([part.len(), 3, SIZE, SIZE], x.as_slice())).map_err(oe)?;
            let outs = sess.run(ort::inputs![input]).map_err(oe)?;
            let (_, e) = outs[self.vision_out].try_extract_tensor::<f32>().map_err(oe)?;
            out.extend(super::rows(e, DIM).into_iter().take(part.len()));
        }
        if out.len() != imgs.len() {
            return Err(anyhow!("the image model answered {} vectors for {} images", out.len(), imgs.len()));
        }
        Ok(out)
    }

    /// The token ids of texts, as SigLIP 2 was trained: lower case, <eos>, padded to 64.
    pub fn tokens(&self, texts: &[String]) -> Result<Vec<i64>> {
        let lower: Vec<String> = texts.iter().map(|t| t.trim().to_lowercase()).collect();
        let enc = self.tokenizer.encode_batch(lower, true).map_err(|e| anyhow!("tokenizing: {e}"))?;
        let mut ids = Vec::with_capacity(texts.len() * TOKENS);
        for e in enc {
            let mut v: Vec<i64> = e.get_ids().iter().map(|&i| i as i64).collect();
            v.resize(TOKENS, 0);
            ids.extend(v);
        }
        Ok(ids)
    }

    fn text_session(&self, rt: &Runtime) -> Result<&Mutex<Session>> {
        if let Some(s) = self.text.get() {
            return Ok(s);
        }
        let _l = self.text_loading.lock();
        if let Some(s) = self.text.get() {
            return Ok(s);
        }
        let t = std::time::Instant::now();
        let s = rt.session(&self.text_path)?;
        tracing::info!("AI engine: text model loaded in {:.1}s", t.elapsed().as_secs_f64());
        Ok(self.text.get_or_init(|| Mutex::new(s)))
    }

    pub fn embed_texts(&self, rt: &Runtime, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let sess = self.text_session(rt)?;
        let ids = self.tokens(texts)?;
        let mut sess = sess.lock();
        let idx = sess.outputs().iter().position(|o| o.name() == "pooler_output").unwrap_or(sess.outputs().len().saturating_sub(1));
        let input = TensorRef::from_array_view(([texts.len(), TOKENS], ids.as_slice())).map_err(oe)?;
        let outs = sess.run(ort::inputs![input]).map_err(oe)?;
        let (_, e) = outs[idx].try_extract_tensor::<f32>().map_err(oe)?;
        let out = super::rows(e, DIM);
        if out.len() != texts.len() {
            return Err(anyhow!("the text model answered {} vectors for {} texts", out.len(), texts.len()));
        }
        Ok(out)
    }
}
