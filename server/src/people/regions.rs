//! Boxes: Hyperfocal keeps detections as top-left boxes on the upright image
//! (normalized); MWG regions in the sidecars are centred boxes on the stored image,
//! before EXIF orientation ("region metadata is applied to the stored image"; Immich
//! and digiKam read them that way too).

use crate::meta::regions::Region;

/// A box on the upright image, normalized, top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A point of the stored image (normalized) on the upright image, for an EXIF
/// orientation (the same mapping `images::jpeg::orient` applies to pixels).
pub fn point_to_upright(o: i32, x: f64, y: f64) -> (f64, f64) {
    match o {
        2 => (1.0 - x, y),
        3 => (1.0 - x, 1.0 - y),
        4 => (x, 1.0 - y),
        5 => (y, x),
        6 => (1.0 - y, x),
        7 => (1.0 - y, 1.0 - x),
        8 => (y, 1.0 - x),
        _ => (x, y),
    }
}

/// The inverse of `point_to_upright`.
pub fn point_to_stored(o: i32, x: f64, y: f64) -> (f64, f64) {
    match o {
        2 => (1.0 - x, y),
        3 => (1.0 - x, 1.0 - y),
        4 => (x, 1.0 - y),
        5 => (y, x),
        6 => (y, 1.0 - x),
        7 => (1.0 - y, 1.0 - x),
        8 => (1.0 - y, x),
        _ => (x, y),
    }
}

pub fn region_to_rect(o: i32, r: &Region) -> Rect {
    let (cx, cy) = point_to_upright(o, r.x, r.y);
    let (w, h) = if (5..=8).contains(&o) { (r.h, r.w) } else { (r.w, r.h) };
    Rect { x: cx - w / 2.0, y: cy - h / 2.0, w, h }
}

pub fn rect_to_region(o: i32, r: Rect, name: Option<String>, kind: &str) -> Region {
    let (cx, cy) = point_to_stored(o, r.x + r.w / 2.0, r.y + r.h / 2.0);
    let (w, h) = if (5..=8).contains(&o) { (r.h, r.w) } else { (r.w, r.h) };
    Region { name, kind: Some(kind.to_string()), x: cx, y: cy, w, h }
}

pub fn iou(a: Rect, b: Rect) -> f64 {
    let iw = ((a.x + a.w).min(b.x + b.w) - a.x.max(b.x)).max(0.0);
    let ih = ((a.y + a.h).min(b.y + b.h) - a.y.max(b.y)).max(0.0);
    let inter = iw * ih;
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::jpeg::{Rgb, orient};

    #[test]
    fn round_trips() {
        for o in 1..=8 {
            for (x, y) in [(0.1, 0.2), (0.7, 0.35), (0.5, 0.5)] {
                let (u, v) = point_to_upright(o, x, y);
                let (a, b) = point_to_stored(o, u, v);
                assert!((a - x).abs() < 1e-12 && (b - y).abs() < 1e-12, "orientation {o}");
            }
            let r = Rect { x: 0.1, y: 0.2, w: 0.3, h: 0.1 };
            let g = rect_to_region(o, r, Some("A".into()), "Face");
            let back = region_to_rect(o, &g);
            assert!((back.x - r.x).abs() < 1e-12 && (back.y - r.y).abs() < 1e-12 && (back.w - r.w).abs() < 1e-12 && (back.h - r.h).abs() < 1e-12);
        }
    }

    /// The point mapping agrees with the pixels `orient` moves.
    #[test]
    fn matches_pixel_orientation() {
        let (w, h) = (40usize, 20usize);
        for o in 1..=8u8 {
            let mut px = vec![0u8; w * h * 3];
            // One marked pixel near the top left of the stored image.
            let (sx, sy) = (5usize, 3usize);
            px[(sy * w + sx) * 3] = 255;
            let img = orient(Rgb { w, h, px }, o);
            let i = img.px.chunks_exact(3).position(|p| p[0] == 255).unwrap();
            let (ux, uy) = (i % img.w, i / img.w);
            let (ex, ey) = point_to_upright(o as i32, (sx as f64 + 0.5) / w as f64, (sy as f64 + 0.5) / h as f64);
            assert!(((ux as f64 + 0.5) / img.w as f64 - ex).abs() < 1e-9, "orientation {o}: x");
            assert!(((uy as f64 + 0.5) / img.h as f64 - ey).abs() < 1e-9, "orientation {o}: y");
        }
    }

    #[test]
    fn overlap() {
        let a = Rect { x: 0.0, y: 0.0, w: 0.5, h: 0.5 };
        assert!((iou(a, a) - 1.0).abs() < 1e-12);
        assert_eq!(iou(a, Rect { x: 0.6, y: 0.6, w: 0.1, h: 0.1 }), 0.0);
        assert!((iou(a, Rect { x: 0.25, y: 0.0, w: 0.5, h: 0.5 }) - 1.0 / 3.0).abs() < 1e-12);
    }
}
