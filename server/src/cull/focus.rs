//! How sharp a part of a photo is: the width of its blur, in pixels.
//!
//! An edge blurred by a Gaussian of width σ loses part of its gradient energy when it is
//! blurred once more by σr: at the edges, energy after / energy before ≈ σ² / (σ² + σr²),
//! so σ = σr √(r / (1 − r)). The ratio is taken at the strongest edges of each of four
//! directions (camera shake or a moving subject smears one direction only), chosen by
//! their strength after the second blur so that noise does not pick them, and with the
//! noise's own share of each energy taken out (its level estimated from the median of a
//! second-difference filter, as Immerkær 1996). Contrast and brightness cancel out.
//!
//! On photos blurred by a known Gaussian (σ = 1, 2, 3, 5 pixels) the estimate is within
//! about 0.5 of √(σ² + σ₀²), where σ₀ (0.5 to 1.3 for RAW developments in focus) is the
//! photo's own softness, pixels included; with noise of 1% it still tells 2 from 3
//! pixels apart; motion blur of 12 pixels (σ ≈ 3.5) reads 2 to 5.

use crate::images::jpeg::Rgb;
use std::sync::OnceLock;

/// The second blur.
const SR: f32 = 1.5;
const RADIUS: usize = 5;
/// The share of a region's pixels taken as its edges, per direction.
const TOP: f32 = 0.1;
/// Pixels at the region's border left out (the blurs read past it).
const BORDER: usize = 4;
/// The largest blur reported: beyond it, everything is a smear.
pub const MAX_SIGMA: f32 = 8.0;
/// Smallest region measured.
pub const MIN_SIDE: usize = 32;

/// A region's luminance (gamma-encoded, 0..1).
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Gray {
    /// The luminance of the whole image.
    pub fn of(img: &Rgb) -> Gray {
        Gray::region(img, 0, 0, img.w, img.h)
    }

    /// The luminance of a rectangle of an image (clipped to it).
    pub fn region(img: &Rgb, x0: usize, y0: usize, w: usize, h: usize) -> Gray {
        let x0 = x0.min(img.w);
        let y0 = y0.min(img.h);
        let w = w.min(img.w - x0);
        let h = h.min(img.h - y0);
        let mut px = Vec::with_capacity(w * h);
        for y in y0..y0 + h {
            let row = &img.px[(y * img.w + x0) * 3..(y * img.w + x0 + w) * 3];
            px.extend(row.chunks_exact(3).map(|p| (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) * (1.0 / 255.0)));
        }
        Gray { w, h, px }
    }

    /// A rectangle of this image.
    pub fn sub(&self, x0: usize, y0: usize, w: usize, h: usize) -> Gray {
        let x0 = x0.min(self.w);
        let y0 = y0.min(self.h);
        let w = w.min(self.w - x0);
        let h = h.min(self.h - y0);
        let mut px = Vec::with_capacity(w * h);
        for y in y0..y0 + h {
            px.extend_from_slice(&self.px[y * self.w + x0..y * self.w + x0 + w]);
        }
        Gray { w, h, px }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Blur {
    /// Width of the blur (Gaussian σ), pixels of the measured image.
    pub sigma: f32,
    /// How much edge there is (mean gradient energy of the strongest edges after the
    /// second blur): too little and the estimate means nothing.
    pub structure: f32,
}

fn kernel() -> &'static ([f32; 2 * RADIUS + 1], f32, f32) {
    static K: OnceLock<([f32; 2 * RADIUS + 1], f32, f32)> = OnceLock::new();
    K.get_or_init(|| {
        let mut g = [0f32; 2 * RADIUS + 1];
        for (i, v) in g.iter_mut().enumerate() {
            let x = i as f32 - RADIUS as f32;
            *v = (-x * x / (2.0 * SR * SR)).exp();
        }
        let s: f32 = g.iter().sum();
        g.iter_mut().for_each(|v| *v /= s);
        // The energy a unit white noise leaves in a direction's gradient (Sobel / 8),
        // without and with the blur: the Sobel kernel is [1 2 1] across times [-1 0 1]
        // along, so its energy is the product of the two parts' energies.
        let conv = |a: &[f32], b: &[f32]| -> Vec<f32> {
            let mut out = vec![0f32; a.len() + b.len() - 1];
            for (i, x) in a.iter().enumerate() {
                for (j, y) in b.iter().enumerate() {
                    out[i + j] += x * y;
                }
            }
            out
        };
        let energy = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>();
        let (smooth, diff) = ([1.0f32, 2.0, 1.0], [-1.0f32, 0.0, 1.0]);
        let k0 = energy(&smooth) * energy(&diff) / 64.0;
        let k1 = energy(&conv(&smooth, &g)) * energy(&conv(&diff, &g)) / 64.0;
        (g, k0, k1)
    })
}

fn blurred(g: &Gray) -> Vec<f32> {
    let (k, _, _) = kernel();
    let (w, h) = (g.w, g.h);
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        let row = &g.px[y * w..(y + 1) * w];
        let out = &mut tmp[y * w..(y + 1) * w];
        for x in 0..w {
            let mut s = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let xx = (x + i).saturating_sub(RADIUS).min(w - 1);
                s += kv * row[xx];
            }
            out[x] = s;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for (i, kv) in k.iter().enumerate() {
            let yy = (y + i).saturating_sub(RADIUS).min(h - 1);
            let src = &tmp[yy * w..(yy + 1) * w];
            let dst = &mut out[y * w..(y + 1) * w];
            for x in 0..w {
                dst[x] += kv * src[x];
            }
        }
    }
    out
}

/// Sobel gradients (divided by 8) of the pixels inside the border, row by row.
fn gradients(px: &[f32], w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let (iw, ih) = (w - 2 * BORDER, h - 2 * BORDER);
    let mut gx = Vec::with_capacity(iw * ih);
    let mut gy = Vec::with_capacity(iw * ih);
    for y in BORDER..h - BORDER {
        let (a, b, c) = (&px[(y - 1) * w..y * w], &px[y * w..(y + 1) * w], &px[(y + 1) * w..(y + 2) * w]);
        for x in BORDER..w - BORDER {
            gx.push(((a[x + 1] + 2.0 * b[x + 1] + c[x + 1]) - (a[x - 1] + 2.0 * b[x - 1] + c[x - 1])) * 0.125);
            gy.push(((c[x - 1] + 2.0 * c[x] + c[x + 1]) - (a[x - 1] + 2.0 * a[x] + a[x + 1])) * 0.125);
        }
    }
    (gx, gy)
}

/// Standard deviation of the noise: the median absolute response of the
/// [1 -2 1; -2 4 -2; 1 -2 1] filter (6σ for white noise), so edges barely count.
fn noise(g: &Gray) -> f32 {
    let (w, h) = (g.w, g.h);
    let p = &g.px;
    let mut r = Vec::with_capacity((w - 2) * (h - 2));
    for y in 1..h - 1 {
        let (a, b, c) = (&p[(y - 1) * w..y * w], &p[y * w..(y + 1) * w], &p[(y + 1) * w..(y + 2) * w]);
        for x in 1..w - 1 {
            let v = (a[x - 1] + a[x + 1] + c[x - 1] + c[x + 1]) - 2.0 * (a[x] + c[x] + b[x - 1] + b[x + 1]) + 4.0 * b[x];
            r.push(v.abs());
        }
    }
    if r.is_empty() {
        return 0.0;
    }
    let mid = r.len() / 2;
    let (_, m, _) = r.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    *m / (0.6745 * 6.0)
}

/// The blur of a region; None when it is too small or has no edges to speak of.
pub fn measure(g: &Gray) -> Option<Blur> {
    if g.w < MIN_SIDE || g.h < MIN_SIDE {
        return None;
    }
    let (_, k0, k1) = *kernel();
    let b = blurred(g);
    let (gx0, gy0) = gradients(&g.px, g.w, g.h);
    let (gx1, gy1) = gradients(&b, g.w, g.h);
    let n2 = noise(g).powi(2);
    let n = gx0.len();
    let take = ((n as f32 * TOP) as usize).max(16).min(n);
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let dir = |gx: f32, gy: f32, d: usize| -> f32 {
        let v = match d {
            0 => gx,
            1 => gy,
            2 => (gx + gy) * s,
            _ => (gx - gy) * s,
        };
        v * v
    };
    let mut res = [(0f32, 0f32); 4];
    let mut e1: Vec<f32> = vec![0.0; n];
    let mut sorted: Vec<f32> = Vec::with_capacity(n);
    for (d, out) in res.iter_mut().enumerate() {
        for i in 0..n {
            e1[i] = dir(gx1[i], gy1[i], d);
        }
        sorted.clear();
        sorted.extend_from_slice(&e1);
        let cut = n - take;
        let (_, t, _) = sorted.select_nth_unstable_by(cut, |a, b| a.total_cmp(b));
        let t = *t;
        let (mut a0, mut a1, mut m) = (0f64, 0f64, 0usize);
        for i in 0..n {
            if e1[i] >= t {
                a0 += dir(gx0[i], gy0[i], d) as f64;
                a1 += e1[i] as f64;
                m += 1;
            }
        }
        let m = m.max(1) as f64;
        *out = ((a0 / m) as f32 - k0 * n2, (a1 / m) as f32 - k1 * n2);
    }
    let strongest = res.iter().map(|r| r.1).fold(0f32, f32::max);
    if strongest <= 0.0 {
        return None;
    }
    let mut sigma = 0f32;
    for (a0, a1) in res {
        // A direction with hardly any edges of its own says nothing.
        if a1 < 0.1 * strongest {
            continue;
        }
        let r = if a0 > 0.0 { (a1 / a0).clamp(1e-6, 0.995) } else { 0.995 };
        sigma = sigma.max(SR * (r / (1.0 - r)).sqrt());
    }
    Some(Blur { sigma: sigma.min(MAX_SIGMA), structure: strongest })
}

/// The sharpest part of a photo: tiles of `tile` pixels over all but its outer 4%,
/// the third with the most edges, the mean blur of their two sharpest. A photo whose
/// sharpest part is soft is out of focus or shaken.
pub fn sharpest_part(g: &Gray, tile: usize) -> Option<Blur> {
    use rayon::prelude::*;
    let (mx, my) = (g.w * 4 / 100, g.h * 4 / 100);
    let (iw, ih) = (g.w.saturating_sub(2 * mx), g.h.saturating_sub(2 * my));
    let (nx, ny) = (iw / tile, ih / tile);
    if nx == 0 || ny == 0 {
        return measure(g);
    }
    let mut cells: Vec<(usize, usize)> = (0..ny).flat_map(|y| (0..nx).map(move |x| (x, y))).collect();
    // Many tiles: every other one (a checkerboard) is plenty.
    if cells.len() > 48 {
        cells.retain(|(x, y)| (x + y) % 2 == 0);
    }
    let ox = mx + (iw - nx * tile) / 2;
    let oy = my + (ih - ny * tile) / 2;
    let mut found: Vec<Blur> = cells.par_iter().filter_map(|&(x, y)| measure(&g.sub(ox + x * tile, oy + y * tile, tile, tile))).collect();
    if found.is_empty() {
        return None;
    }
    found.sort_by(|a, b| b.structure.total_cmp(&a.structure));
    let keep = (found.len() / 3).clamp(3.min(found.len()), 12);
    let mut best: Vec<Blur> = found[..keep].to_vec();
    best.sort_by(|a, b| a.sigma.total_cmp(&b.sigma));
    let two = &best[..best.len().min(2)];
    Some(Blur { sigma: two.iter().map(|b| b.sigma).sum::<f32>() / two.len() as f32, structure: two[0].structure })
}

/// Long edge larger images' blur widths are scaled down to: about a 4K screen's width,
/// where a photo is seen whole (a RAW's half-size development is 2,700 to 4,100 pixels
/// long). The same misfocus spreads over more pixels of a 45-megapixel sensor than of a
/// 20-megapixel one; smaller images (an sRAW, a web export) keep their own pixels, so
/// fewer of them is not taken for blur.
pub const REFERENCE_LONG: f32 = 3000.0;

/// Sharpness 0..1 from a blur width measured on an image with this long edge: 1 up to
/// 0.8 pixels (at the reference size), 0.7 at 1.5 (sharp enough), 0.3 at 2.5 (blurry),
/// 0 from 3.2.
pub fn score(sigma: f32, long: usize) -> f32 {
    let s = sigma * (REFERENCE_LONG / long.max(1) as f32).min(1.0);
    (1.0 - (s - 0.8) / 2.4).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scene with edges in every direction: discs, bars and a gradient.
    fn scene(w: usize, h: usize) -> Gray {
        let mut px = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32, y as f32);
                let mut v = 0.2 + 0.3 * fx / w as f32;
                if ((fx - 0.3 * w as f32).powi(2) + (fy - 0.4 * h as f32).powi(2)).sqrt() < 0.18 * w as f32 {
                    v = 0.85;
                }
                if (x / 9) % 2 == 0 && y > h / 2 && x > w / 2 {
                    v = 0.05;
                }
                if ((x + y) / 13) % 2 == 0 && y < h / 3 && x > w * 2 / 3 {
                    v = 0.7;
                }
                px[y * w + x] = v;
            }
        }
        Gray { w, h, px }
    }

    fn gauss(g: &Gray, sigma: f32) -> Gray {
        let r = (sigma * 3.5).ceil() as isize;
        let k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
        let s: f32 = k.iter().sum();
        let k: Vec<f32> = k.iter().map(|v| v / s).collect();
        let (w, h) = (g.w as isize, g.h as isize);
        let at = |px: &[f32], x: isize, y: isize| px[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize];
        let mut tmp = vec![0f32; g.px.len()];
        for y in 0..h {
            for x in 0..w {
                tmp[(y * w + x) as usize] = (-r..=r).map(|i| k[(i + r) as usize] * at(&g.px, x + i, y)).sum();
            }
        }
        let mut out = vec![0f32; g.px.len()];
        for y in 0..h {
            for x in 0..w {
                out[(y * w + x) as usize] = (-r..=r).map(|i| k[(i + r) as usize] * at(&tmp, x, y + i)).sum();
            }
        }
        Gray { w: g.w, h: g.h, px: out }
    }

    fn noisy(g: &Gray, level: f32, seed: u64) -> Gray {
        let mut x = seed | 1;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 10_000) as f32 / 10_000.0
        };
        // Sum of uniforms: about Gaussian, variance 12 * (1/12) = 1.
        let px = g.px.iter().map(|v| v + level * ((0..12).map(|_| rnd()).sum::<f32>() - 6.0)).collect();
        Gray { w: g.w, h: g.h, px }
    }

    #[test]
    fn the_estimate_follows_the_blur() {
        let base = gauss(&scene(240, 200), 0.6);
        let mut last = 0.0;
        for s in [0.0f32, 1.0, 2.0, 3.0, 5.0] {
            let img = if s > 0.0 { gauss(&base, s) } else { Gray { w: base.w, h: base.h, px: base.px.clone() } };
            let got = measure(&img).unwrap().sigma;
            // Pixels and the gradient filter add about one pixel of their own.
            let want = (s * s + 0.6 * 0.6 + 1.0).sqrt();
            assert!((got - want).abs() < 0.25 * want + 0.35, "blur {s}: measured {got}, expected about {want}");
            assert!(got > last, "blur {s}: {got} after {last}");
            last = got;
        }
    }

    #[test]
    fn noise_does_not_pass_for_sharpness() {
        let base = gauss(&scene(240, 200), 0.6);
        for s in [1.0f32, 2.0, 3.0] {
            let clean = measure(&gauss(&base, s)).unwrap().sigma;
            let grainy = measure(&noisy(&gauss(&base, s), 0.01, 7)).unwrap().sigma;
            assert!((grainy - clean).abs() < 0.2 * clean + 0.2, "blur {s}: {clean} clean, {grainy} with noise");
        }
    }

    #[test]
    fn motion_blur_in_one_direction_shows() {
        let base = gauss(&scene(240, 200), 0.6);
        // A horizontal smear of 11 pixels.
        let (w, h) = (base.w, base.h);
        let mut px = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                px[y * w + x] = (-5i32..=5).map(|d| base.px[y * w + (x as i32 + d).clamp(0, w as i32 - 1) as usize]).sum::<f32>() / 11.0;
            }
        }
        let smeared = measure(&Gray { w, h, px }).unwrap().sigma;
        let sharp = measure(&base).unwrap().sigma;
        assert!(smeared > 2.0 && smeared > 2.5 * sharp, "smeared {smeared}, sharp {sharp}");
    }

    #[test]
    fn flat_regions_have_nothing_to_measure() {
        let flat = Gray { w: 64, h: 64, px: vec![0.5; 64 * 64] };
        assert!(measure(&flat).is_none());
        assert!(measure(&Gray { w: 20, h: 20, px: vec![0.5; 400] }).is_none());
    }

    #[test]
    fn the_sharpest_part_wins() {
        // Left half sharp, right half blurred: a sharp subject on a soft background.
        let sharp = gauss(&scene(480, 320), 0.6);
        let soft = gauss(&sharp, 4.0);
        let mut px = soft.px.clone();
        for y in 0..320 {
            px[y * 480..y * 480 + 200].copy_from_slice(&sharp.px[y * 480..y * 480 + 200]);
        }
        let mixed = Gray { w: 480, h: 320, px };
        let part = sharpest_part(&mixed, 80).unwrap().sigma;
        let all_soft = sharpest_part(&soft, 80).unwrap().sigma;
        assert!(part < 1.5, "{part}");
        assert!(all_soft > 3.0, "{all_soft}");
    }

    #[test]
    fn scores_scale_with_the_image() {
        assert_eq!(score(0.5, 3000), 1.0);
        assert!((score(1.5, 3000) - 0.708).abs() < 0.01);
        assert!(score(2.5, 3000) < 0.3);
        // The same blur on an image twice the size is half as much of it.
        assert!((score(3.0, 6000) - score(1.5, 3000)).abs() < 1e-6);
        // A smaller image is judged by its own pixels.
        assert_eq!(score(1.5, 1440), score(1.5, 3000));
    }
}
