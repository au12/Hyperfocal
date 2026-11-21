//! Open or closed eyes: MediaPipe Face Landmarker on face close-ups.
//!
//! The server sends a square close-up of each face it already knows: the face's box
//! centred, the close-up `CROP_SIDE` times the box's long side. The face mesh model (478
//! landmarks) runs on a 256 x 256 crop of it, MediaPipe's way: the face 1.5 times
//! enlarged, turned upright on the line between the eyes. The landmarks of a face more
//! than about 10 degrees off upright are taken again from the crop turned with them (the
//! mesh model is trained on upright faces). MediaPipe's blendshape model then gives the
//! 52 ARKit-style expression coefficients from 146 of the landmarks, among them
//! eyeBlinkLeft and eyeBlinkRight (0 open, 1 closed; Left is the subject's left eye, on
//! the right of the picture).
//!
//! Measured on 300 portraits (200 with closed eyes): open eyes score at most 0.24, closed
//! ones at least 0.43 for 98% of them. A face turned so far that one eye is half hidden
//! is judged by the eye that shows. The two models are the float16 TFLite files of the
//! face_landmarker.task bundle (v1), converted to ONNX: see ml/models/README.md.

use super::imgproc::{self, Border};
use super::{EyeModels, Runtime, oe, round_to};
use crate::images::jpeg::Rgb;
use anyhow::{Result, anyhow};
use ort::session::Session;
use ort::value::TensorRef;
use parking_lot::Mutex;

/// The close-up the server sends: the face box's long side times this, centred on it.
pub const CROP_SIDE: f64 = 2.2;
/// MediaPipe's region of interest: the face 1.5 times enlarged.
const ROI_SCALE: f64 = 1.5;
const SIZE: usize = 256;

/// The 146 landmarks the blendshape model reads (MediaPipe's kLandmarksSubsetIdxs, in
/// face_blendshapes_graph.cc).
const SUBSET: [usize; 146] = [
    0, 1, 4, 5, 6, 7, 8, 10, 13, 14, 17, 21, 33, 37, 39, 40, 46, 52, 53, 54, 55, 58, 61, 63, 65, 66, 67, 70, 78, 80, 81, 82, 84, 87, 88, 91, 93, 95,
    103, 105, 107, 109, 127, 132, 133, 136, 144, 145, 146, 148, 149, 150, 152, 153, 154, 155, 157, 158, 159, 160, 161, 162, 163, 168, 172, 173,
    176, 178, 181, 185, 191, 195, 197, 234, 246, 249, 251, 263, 267, 269, 270, 276, 282, 283, 284, 285, 288, 291, 293, 295, 296, 297, 300, 308,
    310, 311, 312, 314, 317, 318, 321, 323, 324, 332, 334, 336, 338, 356, 361, 362, 365, 373, 374, 375, 377, 378, 379, 380, 381, 382, 384, 385,
    386, 387, 388, 389, 390, 397, 398, 400, 402, 405, 409, 415, 454, 466, 468, 469, 470, 471, 472, 473, 474, 475, 476, 477,
];
const EYE_BLINK_LEFT: usize = 9;
const EYE_BLINK_RIGHT: usize = 10;
/// Eye corners on the mesh: the eye on the left of the picture (the subject's right).
const PIC_LEFT_EYE: (usize, usize) = (33, 133);
const PIC_RIGHT_EYE: (usize, usize) = (362, 263);
/// Turned this far from upright (degrees): the landmarks are taken again, turned.
const REFINE_DEG: f64 = 10.0;

/// A region of the close-up: centre, side and turn (radians, the eyes' line against x).
#[derive(Debug, Clone, Copy)]
pub struct Roi {
    pub cx: f64,
    pub cy: f64,
    pub side: f64,
    pub angle: f64,
}

/// The affine map that takes a square of `side` centred on (cx, cy), turned by `angle`,
/// onto the model's input.
pub fn roi_matrix(r: Roi) -> [f64; 6] {
    let (c, s) = (r.angle.cos(), r.angle.sin());
    let k = SIZE as f64 / r.side;
    let (m00, m01, m10, m11) = (k * c, k * s, -k * s, k * c);
    let half = SIZE as f64 / 2.0;
    [m00, m01, half - (m00 * r.cx + m01 * r.cy), m10, m11, half - (m10 * r.cx + m11 * r.cy)]
}

/// MediaPipe's region for a face from its landmarks: turned on the line between the outer
/// eye corners (33 to 263), square around the turned bounding box, 1.5 times enlarged.
pub fn roi_of(pts: &[[f64; 2]]) -> Roi {
    let a = [pts[263][0] - pts[33][0], pts[263][1] - pts[33][1]];
    let angle = a[1].atan2(a[0]);
    let (c, s) = (angle.cos(), angle.sin());
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in pts {
        // Into the face's frame.
        let t = [p[0] * c + p[1] * s, -p[0] * s + p[1] * c];
        for k in 0..2 {
            lo[k] = lo[k].min(t[k]);
            hi[k] = hi[k].max(t[k]);
        }
    }
    let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    Roi { cx: mid[0] * c - mid[1] * s, cy: mid[0] * s + mid[1] * c, side: (hi[0] - lo[0]).max(hi[1] - lo[1]) * ROI_SCALE, angle }
}

pub struct Eyes {
    mesh: Mutex<Session>,
    shapes: Mutex<Session>,
}

#[derive(Debug, Clone)]
pub struct EyeCheck {
    pub closed: f64,
    pub blink: [f64; 2],
    pub p: f64,
    pub angle: f64,
    pub eyes: [[f64; 2]; 2],
}

impl EyeCheck {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "closed": round_to(self.closed, 3),
            "blink": [round_to(self.blink[0], 3), round_to(self.blink[1], 3)],
            "p": round_to(self.p, 3),
            "angle": round_to(self.angle, 1),
            "eyes": [[round_to(self.eyes[0][0], 4), round_to(self.eyes[0][1], 4)], [round_to(self.eyes[1][0], 4), round_to(self.eyes[1][1], 4)]],
        })
    }
}

impl Eyes {
    pub fn new(rt: &Runtime, m: &EyeModels) -> Result<Eyes> {
        Ok(Eyes { mesh: Mutex::new(rt.session(&m.landmarks.path)?), shapes: Mutex::new(rt.session(&m.blendshapes.path)?) })
    }

    /// The 478 landmarks (close-up coordinates) and the face presence probability.
    fn mesh(&self, img: &Rgb, roi: Roi) -> Result<(Vec<[f64; 2]>, f64)> {
        let m = roi_matrix(roi);
        let crop = imgproc::warp_affine(&img.px, img.w, img.h, &m, SIZE, SIZE, Border::Constant(0));
        let k = (1.0f64 / 255.0) as f32;
        let x: Vec<f32> = crop.iter().map(|&v| v as f32 * k).collect();
        let mut sess = self.mesh.lock();
        let input = TensorRef::from_array_view(([1usize, SIZE, SIZE, 3], x.as_slice())).map_err(oe)?;
        let outs = sess.run(ort::inputs![input]).map_err(oe)?;
        let (_, lm) = outs[0].try_extract_tensor::<f32>().map_err(oe)?;
        let (_, logit) = outs[1].try_extract_tensor::<f32>().map_err(oe)?;
        if lm.len() < 478 * 3 || logit.is_empty() {
            return Err(anyhow!("unexpected face mesh output"));
        }
        let inv = imgproc::invert_affine(&m);
        let pts = lm
            .chunks_exact(3)
            .take(478)
            .map(|p| {
                let (x, y) = (p[0] as f64, p[1] as f64);
                [inv[0] * x + inv[1] * y + inv[2], inv[3] * x + inv[4] * y + inv[5]]
            })
            .collect();
        let l = (logit[0] as f64).clamp(-30.0, 30.0);
        Ok((pts, 1.0 / (1.0 + (-l).exp())))
    }

    /// The eyes of the face in the middle of a close-up (RGB, upright).
    pub fn check(&self, img: &Rgb) -> Result<EyeCheck> {
        let (w, h) = (img.w as f64, img.h as f64);
        let side = w.min(h) / CROP_SIDE * ROI_SCALE;
        let (mut pts, mut p) = self.mesh(img, Roi { cx: w / 2.0, cy: h / 2.0, side, angle: 0.0 })?;
        let roi = roi_of(&pts);
        if roi.angle.to_degrees().abs() > REFINE_DEG || p < 0.5 {
            let (pts2, p2) = self.mesh(img, roi)?;
            if p2 >= p || p < 0.5 {
                (pts, p) = (pts2, p2);
            }
        }
        let sub: Vec<f32> = SUBSET.iter().flat_map(|&i| [pts[i][0] as f32, pts[i][1] as f32]).collect();
        let shapes = {
            let mut sess = self.shapes.lock();
            let input = TensorRef::from_array_view(([1usize, SUBSET.len(), 2], sub.as_slice())).map_err(oe)?;
            let outs = sess.run(ort::inputs![input]).map_err(oe)?;
            let (_, s) = outs[0].try_extract_tensor::<f32>().map_err(oe)?;
            s.to_vec()
        };
        if shapes.len() <= EYE_BLINK_RIGHT {
            return Err(anyhow!("unexpected blendshape output"));
        }
        // Picture left and right (the model's Left is the subject's left eye).
        let (left, right) = (shapes[EYE_BLINK_RIGHT] as f64, shapes[EYE_BLINK_LEFT] as f64);
        let dist = |a: usize, b: usize| (pts[a][0] - pts[b][0]).hypot(pts[a][1] - pts[b][1]);
        let wl = dist(PIC_LEFT_EYE.0, PIC_LEFT_EYE.1);
        let wr = dist(PIC_RIGHT_EYE.0, PIC_RIGHT_EYE.1);
        // A face turned so far that one eye is foreshortened to half the other: that
        // eye's estimate is a guess, so the other one decides.
        let closed = if wl < 0.5 * wr {
            right
        } else if wr < 0.5 * wl {
            left
        } else {
            (left + right) / 2.0
        };
        let centre = |ids: [usize; 4]| -> [f64; 2] {
            let (mut x, mut y) = (0.0, 0.0);
            for i in ids {
                x += pts[i][0];
                y += pts[i][1];
            }
            [x / 4.0 / w, y / 4.0 / h]
        };
        Ok(EyeCheck { closed, blink: [left, right], p, angle: roi.angle.to_degrees(), eyes: [centre([33, 133, 159, 145]), centre([362, 263, 386, 374])] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_turn_with_the_face() {
        // Landmarks of a face turned by 30 degrees: the region turns with it, and maps the
        // face's middle to the middle of the model's input.
        let mut seed = 3u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 80.0
        };
        let mut pts: Vec<[f64; 2]> = (0..478).map(|_| [rnd(), rnd()]).collect();
        pts[33] = [-30.0, 0.0];
        pts[263] = [30.0, 0.0];
        let a = 30f64.to_radians();
        let turned: Vec<[f64; 2]> = pts.iter().map(|p| [a.cos() * p[0] - a.sin() * p[1] + 200.0, a.sin() * p[0] + a.cos() * p[1] + 150.0]).collect();
        let r = roi_of(&turned);
        assert!((r.angle.to_degrees() - 30.0).abs() < 1e-9);
        let m = roi_matrix(r);
        let map = |p: [f64; 2]| [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]];
        let (c0, c1) = (map(turned[33]), map(turned[263]));
        // Upright in the model's input: both eye corners on one row.
        assert!((c0[1] - c1[1]).abs() < 1e-6);
        let mid = map([r.cx, r.cy]);
        assert!((mid[0] - 128.0).abs() < 1e-9 && (mid[1] - 128.0).abs() < 1e-9);
    }
}
