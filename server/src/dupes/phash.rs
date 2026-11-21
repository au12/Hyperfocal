//! Perceptual hash (DCT pHash) of a photo's thumbnail: 64 bits that stay nearly the same
//! through resizing, recompression, a different JPEG rendering of the same RAW and small
//! edits, and differ in about half their bits between unrelated photos.
//!
//! The image is reduced to 32 x 32 grey levels (area average), transformed with a 2-D
//! DCT, and the 8 x 8 lowest frequencies after the DC row and column are compared with
//! their median: one bit each. Splitting at the median sets 32 of the 64 bits, so a hash
//! of 0 never happens and marks an image too flat to compare instead (a black frame, a
//! lens cap): those are left out of the near-duplicate search.
//!
//! A photo turned or mirrored has a completely different hash, so each photo also gets
//! the hashes of its seven turned and mirrored versions (`dihedral`): a copy that was
//! turned is near one of them. Crops change the hash too: image embeddings find those
//! (look-alikes, `visual`).

use crate::images::jpeg::Rgb;
use std::sync::OnceLock;

/// The side of the grey image the DCT runs on.
const SIDE: usize = 32;
/// Frequencies kept per axis (1..=8).
const KEEP: usize = 8;
/// An image whose 32 x 32 grey levels vary less than this (standard deviation, 0..255)
/// is too flat to compare.
const FLAT_STD: f32 = 2.0;
/// The hash of an image too flat to compare.
pub const FLAT: u64 = 0;

/// Hamming distance: how many of the 64 bits differ.
pub fn distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// cos(pi * (2n + 1) * k / 64) for k = 0..=8, n = 0..32: the DCT-II basis.
fn basis() -> &'static [[f32; SIDE]; KEEP + 1] {
    static B: OnceLock<[[f32; SIDE]; KEEP + 1]> = OnceLock::new();
    B.get_or_init(|| {
        let mut b = [[0f32; SIDE]; KEEP + 1];
        for (k, row) in b.iter_mut().enumerate() {
            for (n, v) in row.iter_mut().enumerate() {
                *v = (std::f64::consts::PI * (2 * n + 1) as f64 * k as f64 / (2 * SIDE) as f64).cos() as f32;
            }
        }
        b
    })
}

/// For each output cell of an axis of `len` source pixels squeezed (or stretched) into
/// `SIDE` cells: the source pixels it covers and how much of each.
fn spans(len: usize) -> Vec<Vec<(usize, f32)>> {
    let step = len as f64 / SIDE as f64;
    (0..SIDE)
        .map(|j| {
            let (a, b) = (j as f64 * step, (j + 1) as f64 * step);
            let mut v = Vec::new();
            let mut i = a.floor() as usize;
            while (i as f64) < b && i < len {
                let w = ((i + 1) as f64).min(b) - (i as f64).max(a);
                if w > 0.0 {
                    v.push((i, (w / step) as f32));
                }
                i += 1;
            }
            v
        })
        .collect()
}

/// The image as 32 x 32 grey levels (Rec. 601 luma), each cell the average of the area it
/// covers.
fn grey32(img: &Rgb) -> Vec<f32> {
    let (w, h) = (img.w.max(1), img.h.max(1));
    let (sx, sy) = (spans(w), spans(h));
    // Rows first: every source row squeezed to SIDE cells.
    let mut rows = vec![0f32; h * SIDE];
    for y in 0..img.h {
        let line = &img.px[y * img.w * 3..(y + 1) * img.w * 3];
        for (j, span) in sx.iter().enumerate() {
            let mut acc = 0f32;
            for &(x, wt) in span {
                let p = &line[x * 3..x * 3 + 3];
                acc += wt * (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32);
            }
            rows[y * SIDE + j] = acc;
        }
    }
    let mut out = vec![0f32; SIDE * SIDE];
    for (i, span) in sy.iter().enumerate() {
        for &(y, wt) in span {
            for j in 0..SIDE {
                out[i * SIDE + j] += wt * rows[y * SIDE + j];
            }
        }
    }
    out
}

/// The 64-bit perceptual hash of an image ([`FLAT`] when it is too flat to compare).
pub fn phash(img: &Rgb) -> u64 {
    if img.w == 0 || img.h == 0 || img.px.len() < img.w * img.h * 3 {
        return FLAT;
    }
    hash_grey(&grey32(img))
}

/// The image's 32 x 32 grey grid turned and mirrored: variant `k` of the dihedral group
/// (1, 2, 3: turned 90°, 180°, 270° clockwise; 4: mirrored left to right; 5, 6, 7:
/// mirrored, then turned like 1, 2, 3).
fn variant(g: &[f32], k: usize) -> Vec<f32> {
    let n = SIDE;
    let mut out = vec![0f32; n * n];
    for r in 0..n {
        for c in 0..n {
            // Mirrored first (k >= 4), then turned (k % 4) quarter turns clockwise.
            let (mut sr, mut sc) = (r, c);
            for _ in 0..(k % 4) {
                // Turning clockwise: the output's (r, c) comes from the source's (n-1-c, r).
                (sr, sc) = (n - 1 - sc, sr);
            }
            if k >= 4 {
                sc = n - 1 - sc;
            }
            out[r * n + c] = g[sr * n + sc];
        }
    }
    out
}

/// The hashes of the image's seven turned and mirrored versions (see `variant`), in
/// variant order 1..=7. All [`FLAT`] for an image too flat to compare.
pub fn dihedral(img: &Rgb) -> [u64; 7] {
    if img.w == 0 || img.h == 0 || img.px.len() < img.w * img.h * 3 {
        return [FLAT; 7];
    }
    let g = grey32(img);
    std::array::from_fn(|i| hash_grey(&variant(&g, i + 1)))
}

/// What a variant is, for people.
pub fn variant_name(k: u8) -> &'static str {
    match k {
        1 | 3 => "Turned 90°",
        2 => "Turned 180°",
        4 => "Mirrored",
        _ => "Mirrored and turned",
    }
}

fn hash_grey(g: &[f32]) -> u64 {
    let n = g.len() as f32;
    let mean = g.iter().sum::<f32>() / n;
    let var = g.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
    if var.sqrt() < FLAT_STD {
        return FLAT;
    }
    let b = basis();
    // Along each row: frequencies 1..=8.
    let mut t = [[0f32; KEEP]; SIDE];
    for (r, tr) in t.iter_mut().enumerate() {
        let row = &g[r * SIDE..(r + 1) * SIDE];
        for (k, v) in tr.iter_mut().enumerate() {
            *v = row.iter().zip(b[k + 1].iter()).map(|(x, c)| x * c).sum();
        }
    }
    // Then down each column of those.
    let mut coef = [0f32; KEEP * KEEP];
    for kv in 0..KEEP {
        for ku in 0..KEEP {
            coef[kv * KEEP + ku] = (0..SIDE).map(|r| t[r][ku] * b[kv + 1][r]).sum();
        }
    }
    let mut sorted = coef;
    sorted.sort_unstable_by(|a, b| a.total_cmp(b));
    let median = (sorted[KEEP * KEEP / 2 - 1] + sorted[KEEP * KEEP / 2]) / 2.0;
    let mut hash = 0u64;
    for (i, &c) in coef.iter().enumerate() {
        if c > median {
            hash |= 1 << i;
        }
    }
    // Every coefficient equal (not possible for an image that is not flat, but never
    // let a real image claim the "flat" value).
    if hash == FLAT { 1 } else { hash }
}

/// How sharp the sharpest part of an image looks, for picking a frame of a burst: the
/// energy of the grey image's Laplacian in each cell of a 4 x 4 grid, the best three
/// cells averaged (a sharp subject on a soft background counts as sharp), divided by
/// the image's contrast so a brighter frame of a bracket does not win on brightness.
///
/// Only comparable between frames of one scene. Measured on the 512 px thumbnail, it
/// sees camera shake, subject motion and a missed focus, not critical focus.
pub fn sharpness(img: &Rgb) -> f32 {
    let (w, h) = (img.w, img.h);
    if w < 8 || h < 8 || img.px.len() < w * h * 3 {
        return 0.0;
    }
    let grey: Vec<f32> = img.px.chunks_exact(3).take(w * h).map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).collect();
    let n = grey.len() as f32;
    let mean = grey.iter().sum::<f32>() / n;
    let var = grey.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
    let mut energy = [0f64; 16];
    let mut count = [0u32; 16];
    for y in 1..h - 1 {
        let cy = (y * 4 / h) * 4;
        let row = &grey[y * w..(y + 1) * w];
        let (up, down) = (&grey[(y - 1) * w..y * w], &grey[(y + 1) * w..(y + 2) * w]);
        for x in 1..w - 1 {
            let l = 4.0 * row[x] - row[x - 1] - row[x + 1] - up[x] - down[x];
            let c = cy + x * 4 / w;
            energy[c] += (l * l) as f64;
            count[c] += 1;
        }
    }
    let mut cells: Vec<f64> = energy.iter().zip(count.iter()).filter(|(_, n)| **n > 0).map(|(e, n)| e / *n as f64).collect();
    cells.sort_unstable_by(|a, b| b.total_cmp(a));
    let best = cells.iter().take(3).sum::<f64>() / cells.len().clamp(1, 3) as f64;
    (best / (var as f64 + 16.0)) as f32
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_turned_or_mirrored_copy_matches_a_variant() {
        // A non-symmetric test image: a gradient with a bright block off-centre.
        let (w, h) = (96usize, 64usize);
        let mut px = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let mut v = (x * 2 + y) as u8;
                if x > 60 && y < 20 {
                    v = 250;
                }
                px[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[v, v / 2, 255 - v]);
            }
        }
        let img = Rgb { w, h, px: px.clone() };
        // Turned 90° clockwise: (x, y) -> (h-1-y, x), a h x w image.
        let mut t = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let (nx, ny) = (h - 1 - y, x);
                t[(ny * h + nx) * 3..(ny * h + nx) * 3 + 3].copy_from_slice(&px[(y * w + x) * 3..(y * w + x) * 3 + 3]);
            }
        }
        let turned = Rgb { w: h, h: w, px: t };
        // Mirrored left to right.
        let mut m = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let nx = w - 1 - x;
                m[(y * w + nx) * 3..(y * w + nx) * 3 + 3].copy_from_slice(&px[(y * w + x) * 3..(y * w + x) * 3 + 3]);
            }
        }
        let mirrored = Rgb { w, h, px: m };
        let d = dihedral(&img);
        assert!(distance(phash(&img), phash(&turned)) > 12, "turning changes the plain hash");
        assert!(distance(d[0], phash(&turned)) <= 2, "variant 1 is the turned copy");
        assert!(distance(d[3], phash(&mirrored)) <= 2, "variant 4 is the mirror image");
    }

    use super::*;
    use crate::images::jpeg;

    /// A synthetic "photo", different for each seed: a lit gradient with a dozen
    /// hard-edged shapes on it (edges carry the detail a perceptual hash relies on; a
    /// smooth picture would be all low frequencies).
    fn scene(w: usize, h: usize, seed: u32) -> Rgb {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x % 10_000) as f32 / 10_000.0
        };
        let shapes: Vec<[f32; 6]> = (0..12).map(|_| [rnd(), rnd(), 0.05 + 0.2 * rnd(), 0.05 + 0.2 * rnd(), rnd() * 255.0, rnd()]).collect();
        let (gx, gy) = (rnd() * 120.0 - 60.0, rnd() * 120.0 - 60.0);
        let mut px = Vec::with_capacity(w * h * 3);
        for yy in 0..h {
            for xx in 0..w {
                let (u, v) = (xx as f32 / w as f32, yy as f32 / h as f32);
                let mut l = 128.0 + gx * (u - 0.5) + gy * (v - 0.5);
                for s in &shapes {
                    let (dx, dy) = ((u - s[0]) / s[2], (v - s[1]) / s[3]);
                    let inside = if s[5] < 0.5 { dx * dx + dy * dy < 1.0 } else { dx.abs() < 1.0 && dy.abs() < 1.0 };
                    if inside {
                        l = 0.3 * l + 0.7 * s[4];
                    }
                }
                px.extend_from_slice(&[(l + 12.0 * u).clamp(0.0, 255.0) as u8, l.clamp(0.0, 255.0) as u8, (l - 18.0 * v).clamp(0.0, 255.0) as u8]);
            }
        }
        Rgb { w, h, px }
    }

    fn reencode(img: &Rgb, q: i32) -> Rgb {
        jpeg::decode_scaled(&jpeg::encode(img, q, false).unwrap(), 0).unwrap()
    }

    #[test]
    fn same_photo_stays_close() {
        let a = scene(512, 341, 3);
        let ha = phash(&a);
        assert_ne!(ha, FLAT);
        // Splitting at the median sets half the bits.
        assert!((28..=36).contains(&ha.count_ones()), "{}", ha.count_ones());
        assert_eq!(phash(&scene(512, 341, 3)), ha, "deterministic");
        // A smaller copy, a recompressed copy, a brighter copy.
        let small = jpeg::resize(scene(512, 341, 3), 200, 133).unwrap();
        assert!(distance(phash(&small), ha) <= 4, "resized: {}", distance(phash(&small), ha));
        assert!(distance(phash(&reencode(&a, 50)), ha) <= 4, "recompressed: {}", distance(phash(&reencode(&a, 50)), ha));
        let bright = Rgb { w: a.w, h: a.h, px: a.px.iter().map(|&p| p.saturating_add(18)).collect() };
        assert!(distance(phash(&bright), ha) <= 4, "brighter: {}", distance(phash(&bright), ha));
        // The thumbnail decoded at a quarter of its size, as the hash worker reads it.
        let thumb = jpeg::encode(&a, 78, false).unwrap();
        let quarter = jpeg::decode_scaled(&thumb, 128).unwrap();
        assert_eq!(quarter.w, 128);
        assert!(distance(phash(&quarter), ha) <= 3, "quarter-size decode: {}", distance(phash(&quarter), ha));
    }

    #[test]
    fn different_photos_differ() {
        let hashes: Vec<u64> = (0..12).map(|s| phash(&scene(480, 320, s * 7 + 1))).collect();
        for i in 0..hashes.len() {
            for j in i + 1..hashes.len() {
                assert!(distance(hashes[i], hashes[j]) > 10, "{i} vs {j}: {}", distance(hashes[i], hashes[j]));
            }
        }
        // Rotation and mirroring are other images to a pHash.
        let a = scene(400, 400, 5);
        let mut mirror = a.px.clone();
        for row in mirror.chunks_mut(400 * 3) {
            let px: Vec<[u8; 3]> = row.chunks(3).rev().map(|p| [p[0], p[1], p[2]]).collect();
            row.copy_from_slice(&px.concat());
        }
        assert!(distance(phash(&a), phash(&Rgb { w: 400, h: 400, px: mirror })) > 10);
    }

    #[test]
    fn sharper_frames_score_higher() {
        let sharp = scene(512, 341, 9);
        // The same frame blurred (camera shake: a horizontal box blur), then darker.
        let blur = |img: &Rgb, k: usize| -> Rgb {
            let mut px = img.px.clone();
            for y in 0..img.h {
                for x in 0..img.w {
                    for ch in 0..3 {
                        let (a, b) = (x.saturating_sub(k), (x + k).min(img.w - 1));
                        let s: u32 = (a..=b).map(|xx| img.px[(y * img.w + xx) * 3 + ch] as u32).sum();
                        px[(y * img.w + x) * 3 + ch] = (s / (b - a + 1) as u32) as u8;
                    }
                }
            }
            Rgb { w: img.w, h: img.h, px }
        };
        // Fine detail for the blur to take away.
        let mut detailed = Rgb { w: sharp.w, h: sharp.h, px: sharp.px.clone() };
        for (i, p) in detailed.px.iter_mut().enumerate() {
            if (i / 3 / 7 + i / 3 / 512 / 5) % 2 == 0 {
                *p = p.saturating_add(40);
            }
        }
        let (s0, s1, s2) = (sharpness(&detailed), sharpness(&blur(&detailed, 2)), sharpness(&blur(&detailed, 5)));
        assert!(s0 > s1 && s1 > s2, "{s0} {s1} {s2}");
        // A darker exposure of the same frame scores about the same, not lower.
        let dark = Rgb { w: detailed.w, h: detailed.h, px: detailed.px.iter().map(|&p| (p as f32 * 0.5) as u8).collect() };
        let sd = sharpness(&dark);
        assert!((sd - s0).abs() / s0 < 0.25, "{sd} vs {s0}");
        assert_eq!(sharpness(&Rgb { w: 4, h: 4, px: vec![9; 48] }), 0.0);
    }

    #[test]
    fn flat_images_are_not_compared() {
        assert_eq!(phash(&Rgb { w: 64, h: 48, px: vec![0; 64 * 48 * 3] }), FLAT);
        // Sensor noise on a black frame is still flat.
        let noise: Vec<u8> = (0..300 * 200 * 3).map(|i| ((i * 7919) % 5) as u8).collect();
        assert_eq!(phash(&Rgb { w: 300, h: 200, px: noise }), FLAT);
        assert_eq!(phash(&Rgb { w: 0, h: 0, px: vec![] }), FLAT);
        // Tiny and odd shapes still hash (panoramas, slivers).
        assert_ne!(phash(&scene(20, 9, 2)), FLAT);
        assert_ne!(phash(&scene(2000, 90, 4)), FLAT);
    }
}
