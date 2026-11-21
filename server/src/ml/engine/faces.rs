//! Faces: YuNet at a few scales, then AuraFace embeddings of the faces aligned on their
//! five landmarks.

use super::imgproc::{self, Border, Interp};
use super::{FaceModels, Runtime, chw, oe};
use crate::images::jpeg::Rgb;
use anyhow::{Result, anyhow};
use ort::session::Session;
use ort::value::TensorRef;
use parking_lot::Mutex;

/// ArcFace's 112 x 112 template: eyes, nose tip, mouth corners (image left to right).
pub const ARCFACE_DST: [[f32; 2]; 5] = [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

/// A face YuNet found: box (x, y, w, h) and landmarks in pixels of the analysed image.
#[derive(Debug, Clone)]
pub struct Found {
    pub b: [f32; 4],
    pub score: f32,
    pub lm: [[f32; 2]; 5],
}

pub struct Faces {
    det: Mutex<Session>,
    emb: Mutex<Session>,
    scales: Vec<usize>,
}

impl Faces {
    pub fn new(rt: &Runtime, m: &FaceModels, cfg: &super::EngineConfig) -> Result<Faces> {
        Ok(Faces { det: Mutex::new(rt.session(&m.detector.path)?), emb: Mutex::new(rt.session(&m.embedder.path)?), scales: cfg.face_scales.clone() })
    }

    /// One YuNet pass on the image resized by `scale`: faces in the image's pixels.
    fn yunet(&self, img: &Rgb, scale: f64, thr: f32) -> Result<Vec<Found>> {
        let (resized, w, h, scale) = if scale < 0.999 {
            let w = ((img.w as f64 * scale).round_ties_even() as usize).max(1);
            let h = ((img.h as f64 * scale).round_ties_even() as usize).max(1);
            (Some(imgproc::resize(&img.px, img.w, img.h, w, h, Interp::Area)), w, h, scale)
        } else {
            (None, img.w, img.h, 1.0)
        };
        let px = resized.as_deref().unwrap_or(&img.px);
        // Padded to a multiple of 32, BGR (as OpenCV decodes), 0..255.
        let (pw, ph) = (w.div_ceil(32) * 32, h.div_ceil(32) * 32);
        let mut blob = vec![0f32; 3 * ph * pw];
        for y in 0..h {
            for x in 0..w {
                let s = (y * w + x) * 3;
                for c in 0..3 {
                    blob[c * ph * pw + y * pw + x] = px[s + 2 - c] as f32;
                }
            }
        }
        let mut sess = self.det.lock();
        let input = TensorRef::from_array_view(([1usize, 3, ph, pw], blob.as_slice())).map_err(oe)?;
        let outs = sess.run(ort::inputs![input]).map_err(oe)?;
        let get = |name: &str| -> Result<&[f32]> {
            Ok(outs.get(name).ok_or_else(|| anyhow!("YuNet has no output {name}"))?.try_extract_tensor::<f32>().map_err(oe)?.1)
        };
        let mut found = Vec::new();
        let inv = (1.0 / scale) as f32;
        for s in [8usize, 16, 32] {
            let cols = pw / s;
            let (cls, obj, bbox, kps) = (get(&format!("cls_{s}"))?, get(&format!("obj_{s}"))?, get(&format!("bbox_{s}"))?, get(&format!("kps_{s}"))?);
            let sf = s as f32;
            for i in 0..cls.len().min(obj.len()) {
                let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
                if score < thr {
                    continue;
                }
                let (r, c) = ((i / cols) as f32, (i % cols) as f32);
                let bb = &bbox[i * 4..i * 4 + 4];
                let cx = (c + bb[0]) * sf;
                let cy = (r + bb[1]) * sf;
                let bw = bb[2].exp() * sf;
                let bh = bb[3].exp() * sf;
                let k = &kps[i * 10..i * 10 + 10];
                let mut lm = [[0f32; 2]; 5];
                for (j, p) in lm.iter_mut().enumerate() {
                    *p = [(k[j * 2] + c) * sf * inv, (k[j * 2 + 1] + r) * sf * inv];
                }
                found.push(Found { b: [(cx - bw / 2.0) * inv, (cy - bh / 2.0) * inv, bw * inv, bh * inv], score, lm });
            }
        }
        Ok(found)
    }

    /// Faces at every scale (long sides of `scales`), overlaps merged.
    pub fn detect(&self, img: &Rgb, thr: f32) -> Result<Vec<Found>> {
        let long = img.w.max(img.h) as f64;
        let mut all = Vec::new();
        let mut done: Vec<i64> = Vec::new();
        for &target in &self.scales {
            let scale = (target as f64 / long).min(1.0);
            let key = (scale * 1000.0).round_ties_even() as i64;
            if done.contains(&key) {
                continue;
            }
            done.push(key);
            all.extend(self.yunet(img, scale, thr)?);
        }
        let boxes: Vec<[f32; 4]> = all.iter().map(|f| f.b).collect();
        let scores: Vec<f32> = all.iter().map(|f| f.score).collect();
        Ok(imgproc::nms(&boxes, &scores, 0.3).into_iter().map(|i| all[i].clone()).collect())
    }

    /// The face as ArcFace expects it: 112 x 112 RGB, eyes and mouth on the template.
    pub fn align(&self, img: &Rgb, f: &Found) -> Vec<u8> {
        let m = imgproc::umeyama(&f.lm, &ARCFACE_DST);
        let scale = m[0].hypot(m[1]);
        if scale < 0.45 {
            // A large face: shrink its surroundings with area averaging first, so the
            // bilinear warp does not alias. (Single precision, as numpy does it.)
            let [x, y, bw, bh] = f.b;
            let (cx, cy, side) = (x + bw / 2.0, y + bh / 2.0, bw.max(bh) * 1.6);
            let x0 = (cx - side / 2.0).max(0.0) as usize;
            let y0 = (cy - side / 2.0).max(0.0) as usize;
            let x1 = (cx + side / 2.0).min(img.w as f32).max(0.0) as usize;
            let y1 = (cy + side / 2.0).min(img.h as f32).max(0.0) as usize;
            let (crop, cw, ch) = imgproc::crop(&img.px, img.w, img.h, x0, y0, x1, y1);
            if cw > 0 && ch > 0 {
                let fct = scale / 0.9;
                let sw = ((cw as f64 * fct).round_ties_even() as usize).max(1);
                let sh = ((ch as f64 * fct).round_ties_even() as usize).max(1);
                let small = imgproc::resize(&crop, cw, ch, sw, sh, Interp::Area);
                let k = [(sw as f64 / cw.max(1) as f64) as f32, (sh as f64 / ch.max(1) as f64) as f32];
                let lm2: Vec<[f32; 2]> = f.lm.iter().map(|p| [(p[0] - x0 as f32) * k[0], (p[1] - y0 as f32) * k[1]]).collect();
                return imgproc::warp_affine(&small, sw, sh, &imgproc::umeyama(&lm2, &ARCFACE_DST), 112, 112, Border::Replicate);
            }
        }
        imgproc::warp_affine(&img.px, img.w, img.h, &m, 112, 112, Border::Replicate)
    }

    /// AuraFace embeddings of aligned faces (112 x 112 RGB): unit vectors and the
    /// lengths they had (how recognisable the face is).
    pub fn embed(&self, faces: &[Vec<u8>]) -> Result<Vec<(Vec<f32>, f32)>> {
        let mut out = Vec::with_capacity(faces.len());
        let mut sess = self.emb.lock();
        for part in faces.chunks(16) {
            let mut x = Vec::with_capacity(part.len() * 3 * 112 * 112);
            for f in part {
                chw(f, 112, 112, &mut x, |_, v| (v as f32 - 127.5) / 127.5);
            }
            let input = TensorRef::from_array_view(([part.len(), 3, 112, 112], x.as_slice())).map_err(oe)?;
            let outs = sess.run(ort::inputs![input]).map_err(oe)?;
            let (_, e) = outs[0].try_extract_tensor::<f32>().map_err(oe)?;
            for row in e.chunks_exact(512).take(part.len()) {
                let mut v = row.to_vec();
                let n = super::normalize(&mut v);
                out.push((v, n));
            }
        }
        if out.len() != faces.len() {
            return Err(anyhow!("the face embedder answered {} vectors for {} faces", out.len(), faces.len()));
        }
        Ok(out)
    }
}
