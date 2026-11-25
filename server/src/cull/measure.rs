//! Measuring one photo: the upright image (a RAW's half-size development, rendered as
//! the viewer shows it, or the decoded JPEG, HEIC or TIFF at about the same size), its
//! exposure, the sharpness of its subject and close-ups of its faces for the eye check.

use super::focus::{self, Gray};
use super::SUBJECT_MIN_PX;
use crate::images::cancel;
use crate::images::jpeg::{self, Rgb};
use crate::images::raw::{self, WbMode};
use crate::model::Kind;
use crate::people::regions::Rect;
use crate::people::{CAT, DOG, FACE};
use anyhow::Result;
use std::path::Path;

/// A face, dog or cat People & pets found.
#[derive(Debug, Clone, Copy)]
pub struct Subject {
    pub det: i64,
    pub kind: i64,
    pub rect: Rect,
}

/// Long edge non-RAW photos are decoded at: about a RAW's half-size development.
const LONG: usize = 3000;
/// The most of a large face measured (centred): its eyes, nose and mouth.
const MAX_REGION: usize = 768;
/// Close-ups sent for the eye check: the face's long side times this, centred, at
/// `EYE_SIZE` pixels (see ml/hyperfocal_ml/eyes.py).
pub const EYE_CROP: f64 = 2.2;
const EYE_SIZE: usize = 320;

pub struct Image {
    pub img: Rgb,
    /// Shares of pixels with a clipped channel and underexposed (from the sensor data
    /// for a RAW).
    pub over: f64,
    pub under: f64,
}

/// The photo upright, as culling measures it.
pub fn load(path: &Path, kind: Kind, orientation: u8, lut: &[u8]) -> Result<Image> {
    if kind == Kind::Raw {
        match raw::catch(|| raw::develop_path(path, WbMode::Asshot, raw::UE_EV)) {
            Ok(dev) => {
                let img = raw::render(&dev, lut, dev.w.max(dev.h) as usize);
                return Ok(Image { img: jpeg::orient(img, orientation), over: dev.over[3] as f64, under: dev.under[3] as f64 });
            }
            // Stopped for the viewer (see worker::one): measured again later, never from
            // the embedded preview instead.
            Err(_) if cancel::is_cancelled() => return Err(cancel::Cancelled.into()),
            // Not a sensor this build develops: its embedded preview.
            Err(e) => tracing::debug!("cull: developing {}: {e}", path.display()),
        }
    }
    let img = raw::catch(|| crate::images::load_display_rgb(path, kind, LONG)).map_err(|e| anyhow::anyhow!(e))?;
    let img = jpeg::orient(img, orientation);
    let (over, under) = exposure(&img);
    Ok(Image { img, over, under })
}

/// Clipped and underexposed shares of an 8-bit image: blown to white (every channel at
/// 250 or more: a saturated colour with one channel at 255 is not lost highlight
/// detail in a finished JPEG); every channel at 8 or less (about 8.7 stops below white,
/// near the RAW threshold).
pub fn exposure(img: &Rgb) -> (f64, f64) {
    let n = (img.w * img.h).max(1) as f64;
    let (mut over, mut under) = (0usize, 0usize);
    for p in img.px.chunks_exact(3) {
        if p[0].min(p[1]).min(p[2]) >= 250 {
            over += 1;
        }
        if p[0].max(p[1]).max(p[2]) <= 8 {
            under += 1;
        }
    }
    (over as f64 / n, under as f64 / n)
}

/// A box in pixels of the image: x, y, w, h.
fn pixels(img: &Rgb, r: Rect) -> (usize, usize, usize, usize) {
    let (w, h) = (img.w as f64, img.h as f64);
    let x0 = (r.x * w).floor().clamp(0.0, w - 1.0);
    let y0 = (r.y * h).floor().clamp(0.0, h - 1.0);
    let x1 = ((r.x + r.w) * w).ceil().clamp(x0 + 1.0, w);
    let y1 = ((r.y + r.h) * h).ceil().clamp(y0 + 1.0, h);
    (x0 as usize, y0 as usize, (x1 - x0) as usize, (y1 - y0) as usize)
}

/// Sharpness of a face or an animal (0..1): None when it is too small to tell.
pub fn subject_focus(img: &Rgb, r: Rect) -> Option<f64> {
    let (x, y, w, h) = pixels(img, r);
    if (w.max(h) as f64) < SUBJECT_MIN_PX {
        return None;
    }
    // A large face: its middle.
    let (cw, ch) = (w.min(MAX_REGION), h.min(MAX_REGION));
    let g = Gray::region(img, x + (w - cw) / 2, y + (h - ch) / 2, cw, ch);
    focus::measure(&g).map(|b| focus::score(b.sigma, img.w.max(img.h)) as f64)
}

/// Sharpness of the sharpest part of the photo (0..1).
pub fn scene_focus(img: &Rgb) -> Option<f64> {
    let long = img.w.max(img.h);
    let tile = ((long as f64 * 0.055) as usize).clamp(96, 256);
    focus::sharpest_part(&Gray::of(img), tile).map(|b| focus::score(b.sigma, long) as f64)
}

/// A square close-up of a face for the eye check (JPEG), and where it lies on the
/// photo (normalized x, y and side in pixels), or None for a face too small to judge.
pub fn eye_crop(img: &Rgb, r: Rect) -> Option<(Vec<u8>, [f64; 3])> {
    let (x, y, w, h) = pixels(img, r);
    let long = w.max(h) as f64;
    if long < SUBJECT_MIN_PX {
        return None;
    }
    let side = (long * EYE_CROP).round().max(8.0) as usize;
    let (cx, cy) = (x as f64 + w as f64 / 2.0, y as f64 + h as f64 / 2.0);
    let (ox, oy) = (cx - side as f64 / 2.0, cy - side as f64 / 2.0);
    // Outside the photo stays black, as MediaPipe pads.
    let mut px = vec![0u8; side * side * 3];
    for row in 0..side {
        let sy = oy.round() as i64 + row as i64;
        if sy < 0 || sy >= img.h as i64 {
            continue;
        }
        let sx0 = (ox.round() as i64).max(0);
        let sx1 = (ox.round() as i64 + side as i64).min(img.w as i64);
        if sx1 <= sx0 {
            continue;
        }
        let dx = (sx0 - ox.round() as i64) as usize;
        let n = (sx1 - sx0) as usize;
        let src = &img.px[(sy as usize * img.w + sx0 as usize) * 3..(sy as usize * img.w + sx0 as usize + n) * 3];
        px[(row * side + dx) * 3..(row * side + dx + n) * 3].copy_from_slice(src);
    }
    let crop = Rgb { w: side, h: side, px };
    let crop = if side != EYE_SIZE { jpeg::resize(crop, EYE_SIZE, EYE_SIZE).ok()? } else { crop };
    let data = jpeg::encode(&crop, 90, false).ok()?;
    Some((data, [ox.round() / img.w as f64, oy.round() / img.h as f64, side as f64]))
}

/// Where a point of a close-up (0..1) lies on the photo (0..1).
pub fn from_crop(img: &Rgb, at: [f64; 3], p: [f32; 2]) -> [f32; 2] {
    let side = at[2];
    [
        (at[0] + p[0] as f64 * side / img.w as f64).clamp(0.0, 1.0) as f32,
        (at[1] + p[1] as f64 * side / img.h as f64).clamp(0.0, 1.0) as f32,
    ]
}

/// What sharpness says about the photo: the sharpest face (the people are the subject),
/// else the sharpest animal, else the sharpest part. And what it was measured on.
pub fn subject(faces: &[(Subject, Option<f64>)], scene: impl FnOnce() -> Option<f64>) -> (Option<f64>, Option<i64>) {
    let best = |kinds: &[i64]| faces.iter().filter(|(s, _)| kinds.contains(&s.kind)).filter_map(|(_, f)| *f).fold(None, |a: Option<f64>, f| Some(a.map_or(f, |a| a.max(f))));
    if let Some(f) = best(&[FACE]) {
        return (Some(f), Some(1));
    }
    if let Some(f) = best(&[DOG, CAT]) {
        return (Some(f), Some(2));
    }
    match scene() {
        Some(f) => (Some(f), Some(3)),
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: usize, h: usize) -> Rgb {
        let mut px = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let v = if ((x / 7) + (y / 11)) % 2 == 0 { 40 } else { 210 };
                px[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[v, v, v]);
            }
        }
        Rgb { w, h, px }
    }

    #[test]
    fn exposure_counts_clipped_and_black_pixels() {
        let mut img = Rgb { w: 10, h: 10, px: vec![128; 300] };
        img.px[0..3].copy_from_slice(&[255, 252, 251]);
        img.px[3..6].copy_from_slice(&[2, 3, 4]);
        // A saturated red is not a blown highlight.
        img.px[6..9].copy_from_slice(&[255, 10, 10]);
        let (o, u) = exposure(&img);
        assert_eq!((o, u), (0.01, 0.01));
    }

    #[test]
    fn eye_close_ups_are_square_and_centred() {
        let img = image(600, 400);
        // A face of 100 x 120 pixels near the top edge: the close-up reaches outside.
        let r = Rect { x: 250.0 / 600.0, y: 10.0 / 400.0, w: 100.0 / 600.0, h: 120.0 / 400.0 };
        let (jpg, at) = eye_crop(&img, r).unwrap();
        let back = jpeg::decode_scaled(&jpg, 0).unwrap();
        assert_eq!((back.w, back.h), (EYE_SIZE, EYE_SIZE));
        assert_eq!(at[2], 264.0);
        // The middle of the close-up is the middle of the face.
        let mid = from_crop(&img, at, [0.5, 0.5]);
        assert!((mid[0] - 0.5).abs() < 0.01 && (mid[1] - 70.0 / 400.0).abs() < 0.01, "{mid:?}");
        // Too small a face: no close-up.
        assert!(eye_crop(&img, Rect { x: 0.1, y: 0.1, w: 0.05, h: 0.05 }).is_none());
    }

    #[test]
    fn the_subject_is_the_sharpest_face_then_animal_then_the_photo() {
        let s = |kind| Subject { det: 1, kind, rect: Rect { x: 0.0, y: 0.0, w: 0.1, h: 0.1 } };
        assert_eq!(subject(&[(s(FACE), Some(0.4)), (s(FACE), Some(0.8)), (s(DOG), Some(0.9))], || Some(1.0)), (Some(0.8), Some(1)));
        assert_eq!(subject(&[(s(FACE), None), (s(CAT), Some(0.5))], || Some(1.0)), (Some(0.5), Some(2)));
        assert_eq!(subject(&[(s(FACE), None)], || Some(0.6)), (Some(0.6), Some(3)));
        assert_eq!(subject(&[], || None), (None, None));
    }
}
