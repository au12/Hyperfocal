//! Dogs and cats: RF-DETR finds them, a pet re-identification model tells them apart.

use super::imgproc::{self, Interp};
use super::{FaceModels, Runtime, chw, oe};
use crate::images::jpeg::Rgb;
use anyhow::{Result, anyhow};
use ort::session::Session;
use ort::value::TensorRef;
use parking_lot::Mutex;

const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// RF-DETR's classes are COCO's 91-id space. The best class is taken among the animals a
/// household library holds, so a horse or a cow wins its own argmax (and is dropped)
/// instead of being filed as its nearest dog; only cats and dogs are kept.
const COCO_ANIMALS: [usize; 6] = [16, 17, 18, 19, 20, 21]; // bird cat dog horse sheep cow
const CAT: usize = 17;
const DOG: usize = 18;

/// A dog or cat: box (x, y, w, h, 0..1 of the image).
#[derive(Debug, Clone)]
pub struct Animal {
    pub b: [f32; 4],
    pub score: f32,
    pub species: &'static str,
}

pub struct Pets {
    det: Mutex<Session>,
    emb: Mutex<Session>,
    /// The detector's input side (px).
    pub size: usize,
}

impl Pets {
    pub fn new(rt: &Runtime, m: &FaceModels, _thr: f32) -> Result<Pets> {
        let det = rt.session(&m.animal_detector.path)?;
        let size = det
            .inputs()
            .first()
            .and_then(|i| i.dtype().tensor_shape().map(|s| s.to_vec()))
            .and_then(|s| s.get(2).copied())
            .filter(|&n| n > 0)
            .ok_or_else(|| anyhow!("the animal detector has no fixed input size"))? as usize;
        Ok(Pets { det: Mutex::new(det), emb: Mutex::new(rt.session(&m.pet_embedder.path)?), size })
    }

    pub fn detect(&self, img: &Rgb, thr: f32) -> Result<Vec<Animal>> {
        let n = self.size;
        let interp = if img.w.min(img.h) > n { Interp::Area } else { Interp::Linear };
        let px = imgproc::resize(&img.px, img.w, img.h, n, n, interp);
        let mut x = Vec::with_capacity(3 * n * n);
        chw(&px, n, n, &mut x, |c, v| (v as f32 / 255.0 - MEAN[c]) / STD[c]);
        let mut sess = self.det.lock();
        let input = TensorRef::from_array_view(([1usize, 3, n, n], x.as_slice())).map_err(oe)?;
        let outs = sess.run(ort::inputs![input]).map_err(oe)?;
        let (s0, d0) = outs[0].try_extract_tensor::<f32>().map_err(oe)?;
        let (_, d1) = outs[1].try_extract_tensor::<f32>().map_err(oe)?;
        let (boxes, logits) = if s0.last() == Some(&4) { (d0, d1) } else { (d1, d0) };
        let queries = boxes.len() / 4;
        let classes = logits.len().checked_div(queries).unwrap_or(0);
        if classes <= *COCO_ANIMALS.iter().max().unwrap_or(&0) {
            return Err(anyhow!("unexpected animal detector output ({classes} classes)"));
        }
        let mut kept_boxes = Vec::new();
        let mut kept = Vec::new();
        for q in 0..queries {
            let l = &logits[q * classes..(q + 1) * classes];
            // First best, as numpy's argmax.
            let (mut best, mut conf) = (COCO_ANIMALS[0], f32::MIN);
            for &c in &COCO_ANIMALS {
                let p = 1.0 / (1.0 + (-l[c]).exp());
                if p > conf {
                    (best, conf) = (c, p);
                }
            }
            if conf < thr || (best != CAT && best != DOG) {
                continue;
            }
            let b = &boxes[q * 4..q * 4 + 4];
            let x1 = (b[0] - b[2] / 2.0).clamp(0.0, 1.0);
            let y1 = (b[1] - b[3] / 2.0).clamp(0.0, 1.0);
            let x2 = (b[0] + b[2] / 2.0).clamp(0.0, 1.0);
            let y2 = (b[1] + b[3] / 2.0).clamp(0.0, 1.0);
            kept_boxes.push([x1, y1, x2 - x1, y2 - y1]);
            kept.push((conf, best));
        }
        let scores: Vec<f32> = kept.iter().map(|k| k.0).collect();
        // The decoder's queries are deduplicated; the cat-or-dog choice above can still
        // leave a second query on the same animal.
        Ok(imgproc::nms(&kept_boxes, &scores, 0.7)
            .into_iter()
            .filter(|&i| kept_boxes[i][2] > 0.005 && kept_boxes[i][3] > 0.005)
            .map(|i| Animal { b: kept_boxes[i], score: kept[i].0, species: if kept[i].1 == CAT { "cat" } else { "dog" } })
            .collect())
    }

    /// Re-identification embeddings (unit vectors) of animal crops (RGB, w, h).
    pub fn embed(&self, crops: &[(Vec<u8>, usize, usize)]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(crops.len());
        let mut sess = self.emb.lock();
        for part in crops.chunks(16) {
            let mut x = Vec::with_capacity(part.len() * 3 * 224 * 224);
            for (c, w, h) in part {
                let interp = if (*w).min(*h) > 224 { Interp::Area } else { Interp::Cubic };
                let px = imgproc::resize(c, *w, *h, 224, 224, interp);
                chw(&px, 224, 224, &mut x, |c, v| (v as f32 / 255.0 - MEAN[c]) / STD[c]);
            }
            let input = TensorRef::from_array_view(([part.len(), 3, 224, 224], x.as_slice())).map_err(oe)?;
            let outs = sess.run(ort::inputs![input]).map_err(oe)?;
            let (shape, e) = outs[0].try_extract_tensor::<f32>().map_err(oe)?;
            let dim = shape.last().copied().unwrap_or(512).max(1) as usize;
            out.extend(super::rows(e, dim).into_iter().take(part.len()));
        }
        if out.len() != crops.len() {
            return Err(anyhow!("the pet embedder answered {} vectors for {} animals", out.len(), crops.len()));
        }
        Ok(out)
    }
}

/// Whether at least 60% of a face box (pixels) lies inside one of the animal boxes.
pub fn inside_animal(b: &[f32; 4], w: usize, h: usize, pets: &[Animal]) -> bool {
    let (x, y, bw, bh) = (b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64);
    let area = (bw * bh).max(1e-9);
    pets.iter().any(|p| {
        let (ax0, ay0) = ((p.b[0] * w as f32) as f64, (p.b[1] * h as f32) as f64);
        let (ax1, ay1) = (((p.b[0] + p.b[2]) * w as f32) as f64, ((p.b[1] + p.b[3]) * h as f32) as f64);
        let iw = ((x + bw).min(ax1) - x.max(ax0)).max(0.0);
        let ih = ((y + bh).min(ay1) - y.max(ay0)).max(0.0);
        iw * ih / area >= 0.6
    })
}
