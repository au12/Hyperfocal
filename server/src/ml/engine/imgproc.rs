//! The image operations the models' preprocessing needs, done the way OpenCV 5 does them
//! for 8-bit, 3-channel images: `resize` with INTER_AREA, INTER_LINEAR and INTER_CUBIC,
//! and `warpAffine` with bilinear sampling. The models were calibrated (thresholds, face
//! quality, eye scores) on OpenCV's output, so its arithmetic is kept here: the area
//! tables, the fixed-point linear coefficients, the single-precision fused multiply-adds
//! of cubic resizing and warping, and the rounding. Checked against opencv-python 5.0 on
//! random images: area and linear resizing are identical, and fewer than 1 value in
//! 50,000 differs by 1 for the others.
//!
//! Images are interleaved 3-channel rows (`w * 3` bytes each); the channel order does
//! not matter to any of these.

/// INTER_RESIZE_COEF_BITS: resize coefficients are fixed-point with 11 fractional bits.
const COEF_SCALE: f32 = 2048.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interp {
    Area,
    Linear,
    Cubic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Border {
    /// Samples outside the image repeat its edge.
    Replicate,
    /// Samples outside the image are this value.
    Constant(u8),
}

/// cvRound: to nearest, ties to even (SSE's conversion in the default rounding mode).
#[inline]
fn round_i32(v: f64) -> i32 {
    v.round_ties_even() as i32
}

#[inline]
fn sat_u8_f32(v: f32) -> u8 {
    v.round_ties_even().clamp(0.0, 255.0) as u8
}

#[inline]
fn sat_i16(v: f32) -> i32 {
    v.round_ties_even().clamp(-32768.0, 32767.0) as i32
}

/// `cv2.resize(src, (dw, dh), interpolation=interp)`.
pub fn resize(src: &[u8], w: usize, h: usize, dw: usize, dh: usize, interp: Interp) -> Vec<u8> {
    assert_eq!(src.len(), w * h * 3, "resize: buffer does not match its size");
    let (dw, dh) = (dw.max(1), dh.max(1));
    if dw == w && dh == h {
        return src.to_vec();
    }
    let inv_x = dw as f64 / w as f64;
    let inv_y = dh as f64 / h as f64;
    let scale_x = 1.0 / inv_x;
    let scale_y = 1.0 / inv_y;
    let ix = round_i32(scale_x);
    let iy = round_i32(scale_y);
    let area_fast = (scale_x - ix as f64).abs() < f64::EPSILON && (scale_y - iy as f64).abs() < f64::EPSILON;
    match interp {
        // OpenCV: a linear resize by exactly one half is the same as averaging 2 x 2.
        Interp::Linear if area_fast && ix == 2 && iy == 2 => area_fast_resize(src, w, h, dw, dh, 2, 2),
        Interp::Area if scale_x >= 1.0 && scale_y >= 1.0 => {
            if area_fast {
                area_fast_resize(src, w, h, dw, dh, ix as usize, iy as usize)
            } else {
                area_resize(src, w, h, dw, dh, scale_x, scale_y)
            }
        }
        // INTER_AREA when enlarging is a bilinear resize (not used by the pipeline).
        Interp::Area | Interp::Linear => linear_resize(src, w, h, dw, dh, scale_x, scale_y),
        Interp::Cubic => cubic_resize(src, w, h, dw, dh, scale_x, scale_y),
    }
}

/// Averages of whole blocks (integer scale factors in both directions).
fn area_fast_resize(src: &[u8], w: usize, _h: usize, dw: usize, dh: usize, sx: usize, sy: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 3];
    let area = (sx * sy) as u32;
    let scale = 1.0f32 / area as f32;
    for dy in 0..dh {
        for dx in 0..dw {
            for c in 0..3 {
                let mut sum = 0u32;
                for yy in 0..sy {
                    let row = (dy * sy + yy) * w * 3;
                    for xx in 0..sx {
                        sum += src[row + (dx * sx + xx) * 3 + c] as u32;
                    }
                }
                out[(dy * dw + dx) * 3 + c] = if area == 4 {
                    // The vectorised 2 x 2 path: rounds halves up.
                    ((sum + 2) >> 2) as u8
                } else {
                    sat_u8_f32(sum as f32 * scale)
                };
            }
        }
    }
    out
}

struct AreaTab {
    di: usize,
    si: usize,
    alpha: f32,
}

/// OpenCV's computeResizeAreaTab: which source pixels make each destination pixel, and
/// how much of each.
fn area_tab(ssize: usize, dsize: usize, scale: f64) -> Vec<AreaTab> {
    let mut tab = Vec::with_capacity(ssize * 2);
    for dx in 0..dsize {
        let fsx1 = dx as f64 * scale;
        let fsx2 = fsx1 + scale;
        let cell = scale.min(ssize as f64 - fsx1);
        let mut sx2 = fsx2.floor() as i64;
        sx2 = sx2.min(ssize as i64 - 1);
        let sx1 = (fsx1.ceil() as i64).min(sx2);
        if sx1 as f64 - fsx1 > 1e-3 {
            tab.push(AreaTab { di: dx, si: (sx1 - 1) as usize, alpha: ((sx1 as f64 - fsx1) / cell) as f32 });
        }
        for sx in sx1..sx2 {
            tab.push(AreaTab { di: dx, si: sx as usize, alpha: (1.0 / cell) as f32 });
        }
        if fsx2 - sx2 as f64 > 1e-3 {
            tab.push(AreaTab { di: dx, si: sx2 as usize, alpha: ((fsx2 - sx2 as f64).min(1.0).min(cell) / cell) as f32 });
        }
    }
    tab
}

/// INTER_AREA for any shrink factor: each destination pixel is the coverage-weighted mean
/// of the source pixels under it (float sums, rounded at the end).
fn area_resize(src: &[u8], w: usize, _h: usize, dw: usize, dh: usize, scale_x: f64, scale_y: f64) -> Vec<u8> {
    let xtab = area_tab(w, dw, scale_x);
    let ytab = area_tab(_h, dh, scale_y);
    let n = dw * 3;
    let mut out = vec![0u8; dw * dh * 3];
    let mut buf = vec![0f32; n];
    let mut sum = vec![0f32; n];
    let mut prev = ytab.first().map(|t| t.di).unwrap_or(0);
    for t in &ytab {
        let row = &src[t.si * w * 3..(t.si + 1) * w * 3];
        buf.fill(0.0);
        for x in &xtab {
            let (s, d, a) = (x.si * 3, x.di * 3, x.alpha);
            buf[d] += row[s] as f32 * a;
            buf[d + 1] += row[s + 1] as f32 * a;
            buf[d + 2] += row[s + 2] as f32 * a;
        }
        let beta = t.alpha;
        if t.di != prev {
            let o = &mut out[prev * n..(prev + 1) * n];
            for i in 0..n {
                o[i] = sat_u8_f32(sum[i]);
                sum[i] = beta * buf[i];
            }
            prev = t.di;
        } else {
            for i in 0..n {
                sum[i] += beta * buf[i];
            }
        }
    }
    let o = &mut out[prev * n..(prev + 1) * n];
    for i in 0..n {
        o[i] = sat_u8_f32(sum[i]);
    }
    out
}

/// INTER_LINEAR, fixed point: 11-bit coefficients each way, then the vectorised vertical
/// pass's rounding.
fn linear_resize(src: &[u8], w: usize, h: usize, dw: usize, dh: usize, scale_x: f64, scale_y: f64) -> Vec<u8> {
    // Horizontal taps: first source pixel and its two weights; past `xmax` only the first.
    let mut xofs = vec![0usize; dw];
    let mut xa = vec![[0i32; 2]; dw];
    let mut xmax = dw;
    for dx in 0..dw {
        let mut fx = ((dx as f64 + 0.5) * scale_x - 0.5) as f32;
        let mut sx = fx.floor() as i64;
        fx -= sx as f32;
        if sx < 0 {
            fx = 0.0;
            sx = 0;
        }
        if sx + 1 >= w as i64 {
            xmax = xmax.min(dx);
            if sx >= w as i64 - 1 {
                fx = 0.0;
                sx = w as i64 - 1;
            }
        }
        xofs[dx] = sx as usize;
        xa[dx] = [sat_i16((1.0 - fx) * COEF_SCALE), sat_i16(fx * COEF_SCALE)];
    }
    let hrow = |sy: usize| -> Vec<i32> {
        let row = &src[sy * w * 3..(sy + 1) * w * 3];
        let mut d = vec![0i32; dw * 3];
        for dx in 0..dw {
            let s = xofs[dx] * 3;
            for c in 0..3 {
                d[dx * 3 + c] = if dx < xmax {
                    row[s + c] as i32 * xa[dx][0] + row[s + 3 + c] as i32 * xa[dx][1]
                } else {
                    row[s + c] as i32 * COEF_SCALE as i32
                };
            }
        }
        d
    };
    let mut out = vec![0u8; dw * dh * 3];
    for dy in 0..dh {
        let fy = ((dy as f64 + 0.5) * scale_y - 0.5) as f32;
        let sy = fy.floor() as i64;
        let fy = fy - sy as f32;
        let b0 = sat_i16((1.0 - fy) * COEF_SCALE);
        let b1 = sat_i16(fy * COEF_SCALE);
        let r0 = hrow(sy.clamp(0, h as i64 - 1) as usize);
        let r1 = hrow((sy + 1).clamp(0, h as i64 - 1) as usize);
        let o = &mut out[dy * dw * 3..(dy + 1) * dw * 3];
        for i in 0..dw * 3 {
            let v = (((r0[i] >> 4) * b0) >> 16) + (((r1[i] >> 4) * b1) >> 16);
            o[i] = ((v + 2) >> 2).clamp(0, 255) as u8;
        }
    }
    out
}

/// OpenCV's bicubic kernel (A = -0.75).
fn cubic_coeffs(x: f32) -> [f32; 4] {
    const A: f32 = -0.75;
    let c0 = ((A * (x + 1.0) - 5.0 * A) * (x + 1.0) + 8.0 * A) * (x + 1.0) - 4.0 * A;
    let c1 = ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0;
    let c2 = ((A + 2.0) * (1.0 - x) - (A + 3.0)) * (1.0 - x) * (1.0 - x) + 1.0;
    [c0, c1, c2, 1.0 - c0 - c1 - c2]
}

/// INTER_CUBIC, edges repeated. OpenCV 5 does it in floating point with fused
/// multiply-adds; so does this (a CPU without FMA gets the same results, slower).
fn cubic_resize(src: &[u8], w: usize, h: usize, dw: usize, dh: usize, scale_x: f64, scale_y: f64) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("fma") {
        // SAFETY: the CPU has FMA.
        return unsafe { cubic_resize_fma(src, w, h, dw, dh, scale_x, scale_y) };
    }
    cubic_resize_impl(src, w, h, dw, dh, scale_x, scale_y)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "fma")]
unsafe fn cubic_resize_fma(src: &[u8], w: usize, h: usize, dw: usize, dh: usize, scale_x: f64, scale_y: f64) -> Vec<u8> {
    cubic_resize_impl(src, w, h, dw, dh, scale_x, scale_y)
}

#[inline(always)]
fn cubic_resize_impl(src: &[u8], w: usize, h: usize, dw: usize, dh: usize, scale_x: f64, scale_y: f64) -> Vec<u8> {
    let mut xofs = vec![0i64; dw];
    let mut xa = vec![[0f32; 4]; dw];
    for dx in 0..dw {
        let fx = ((dx as f64 + 0.5) * scale_x - 0.5) as f32;
        let sx = fx.floor() as i64;
        xofs[dx] = sx;
        xa[dx] = cubic_coeffs(fx - sx as f32);
    }
    let hrow = |sy: usize, d: &mut [f32]| {
        let row = &src[sy * w * 3..(sy + 1) * w * 3];
        for dx in 0..dw {
            let taps = [0i64, 1, 2, 3].map(|j| (xofs[dx] - 1 + j).clamp(0, w as i64 - 1) as usize * 3);
            for c in 0..3 {
                let mut v = 0f32;
                for j in 0..4 {
                    v = (row[taps[j] + c] as f32).mul_add(xa[dx][j], v);
                }
                d[dx * 3 + c] = v;
            }
        }
    };
    let n = dw * 3;
    let mut rows = vec![vec![0f32; n]; 4];
    let mut have = [-1i64; 4];
    let mut out = vec![0u8; dw * dh * 3];
    for dy in 0..dh {
        let fy = ((dy as f64 + 0.5) * scale_y - 0.5) as f32;
        let sy = fy.floor() as i64;
        let b = cubic_coeffs(fy - sy as f32);
        for k in 0..4 {
            let r = (sy - 1 + k as i64).clamp(0, h as i64 - 1);
            if have[k] != r {
                hrow(r as usize, &mut rows[k]);
                have[k] = r;
            }
        }
        let o = &mut out[dy * n..(dy + 1) * n];
        for i in 0..n {
            let v = rows[3][i].mul_add(b[3], rows[2][i].mul_add(b[2], rows[1][i].mul_add(b[1], rows[0][i] * b[0])));
            o[i] = sat_u8_f32(v);
        }
    }
    out
}

/// The inverse of a 2 x 3 affine map (`cv2.invertAffineTransform`).
pub fn invert_affine(m: &[f64; 6]) -> [f64; 6] {
    let d = m[0] * m[4] - m[1] * m[3];
    let d = if d != 0.0 { 1.0 / d } else { 0.0 };
    let (a11, a22) = (m[4] * d, m[0] * d);
    let (a12, a21) = (-m[1] * d, -m[3] * d);
    let b1 = -a11 * m[2] - a12 * m[5];
    let b2 = -a21 * m[2] - a22 * m[5];
    [a11, a12, b1, a21, a22, b2]
}

/// `cv2.warpAffine(src, m, (dw, dh), flags=INTER_LINEAR, borderMode=border)`: `m` maps
/// source to destination coordinates. OpenCV 5 samples in single precision with fused
/// multiply-adds (the inverse map, then lerps); so does this.
pub fn warp_affine(src: &[u8], w: usize, h: usize, m: &[f64; 6], dw: usize, dh: usize, border: Border) -> Vec<u8> {
    assert_eq!(src.len(), w * h * 3, "warp_affine: buffer does not match its size");
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("fma") {
        // SAFETY: the CPU has FMA.
        return unsafe { warp_affine_fma(src, w, h, m, dw, dh, border) };
    }
    warp_affine_impl(src, w, h, m, dw, dh, border)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "fma")]
unsafe fn warp_affine_fma(src: &[u8], w: usize, h: usize, m: &[f64; 6], dw: usize, dh: usize, border: Border) -> Vec<u8> {
    warp_affine_impl(src, w, h, m, dw, dh, border)
}

#[inline(always)]
fn warp_affine_impl(src: &[u8], w: usize, h: usize, m: &[f64; 6], dw: usize, dh: usize, border: Border) -> Vec<u8> {
    let im = invert_affine(m).map(|v| v as f32);
    let (wi, hi) = (w as i32, h as i32);
    let fill = match border {
        Border::Constant(v) => Some(v as f32),
        Border::Replicate => None,
    };
    let px = |x: i32, y: i32, c: usize| -> f32 {
        match fill {
            Some(v) if x < 0 || y < 0 || x >= wi || y >= hi => v,
            _ => src[(y.clamp(0, hi - 1) as usize * w + x.clamp(0, wi - 1) as usize) * 3 + c] as f32,
        }
    };
    let mut out = vec![0u8; dw * dh * 3];
    for y in 0..dh {
        let yf = y as f32;
        let (rx, ry) = (im[1] * yf + im[2], im[4] * yf + im[5]);
        for x in 0..dw {
            let xf = x as f32;
            let sx = im[0].mul_add(xf, rx);
            let sy = im[3].mul_add(xf, ry);
            let (ix, iy) = (sx.floor() as i32, sy.floor() as i32);
            let (a, b) = (sx - ix as f32, sy - iy as f32);
            let o = &mut out[(y * dw + x) * 3..(y * dw + x) * 3 + 3];
            if let Some(v) = fill
                && (ix >= wi || ix + 1 < 0 || iy >= hi || iy + 1 < 0)
            {
                o.fill(v as u8);
                continue;
            }
            for (c, v) in o.iter_mut().enumerate() {
                let (p00, p01, p10, p11) = (px(ix, iy, c), px(ix + 1, iy, c), px(ix, iy + 1, c), px(ix + 1, iy + 1, c));
                let p = a.mul_add(p01 - p00, p00);
                let q = a.mul_add(p11 - p10, p10);
                *v = sat_u8_f32(b.mul_add(q - p, p));
            }
        }
    }
    out
}

/// The least-squares similarity transform (rotation, uniform scale, translation) that maps
/// `src` points onto `dst` (Umeyama 1991), as a 2 x 3 matrix. In 2-D the rotation that
/// SVD finds has a closed form: the angle of (c - b, a + d) for the cross-covariance
/// [[a, b], [c, d]], and the sum of its signed singular values is that vector's length.
pub fn umeyama(src: &[[f32; 2]], dst: &[[f32; 2]]) -> [f64; 6] {
    let n = src.len().min(dst.len());
    if n == 0 {
        return [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    }
    let nf = n as f64;
    let mean = |p: &[[f32; 2]]| -> [f64; 2] {
        let (mut x, mut y) = (0.0, 0.0);
        for q in &p[..n] {
            x += q[0] as f64;
            y += q[1] as f64;
        }
        [x / nf, y / nf]
    };
    let (ms, md) = (mean(src), mean(dst));
    let (mut a, mut b, mut c, mut d, mut var) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (sx, sy) = (src[i][0] as f64 - ms[0], src[i][1] as f64 - ms[1]);
        let (dx, dy) = (dst[i][0] as f64 - md[0], dst[i][1] as f64 - md[1]);
        // cov = dst^T src / n
        a += dx * sx;
        b += dx * sy;
        c += dy * sx;
        d += dy * sy;
        var += sx * sx + sy * sy;
    }
    let (a, b, c, d, var) = (a / nf, b / nf, c / nf, d / nf, var / nf);
    let theta = (c - b).atan2(a + d);
    let scale = if var > 0.0 { (a + d).hypot(c - b) / var } else { 1.0 };
    let (cs, sn) = (theta.cos(), theta.sin());
    let (r00, r01, r10, r11) = (scale * cs, -scale * sn, scale * sn, scale * cs);
    let tx = md[0] - (r00 * ms[0] + r01 * ms[1]);
    let ty = md[1] - (r10 * ms[0] + r11 * ms[1]);
    [r00, r01, tx, r10, r11, ty]
}

/// Greedy non-maximum suppression on (x, y, w, h) boxes, best score first: the indices kept.
pub fn nms(boxes: &[[f32; 4]], scores: &[f32], iou: f32) -> Vec<usize> {
    let mut order: Vec<usize> = (0..boxes.len().min(scores.len())).collect();
    order.sort_by(|&i, &j| scores[j].partial_cmp(&scores[i]).unwrap_or(std::cmp::Ordering::Equal));
    let area = |b: &[f32; 4]| b[2].max(0.0) * b[3].max(0.0);
    let mut keep = Vec::new();
    while let Some((&i, rest)) = order.split_first() {
        keep.push(i);
        let bi = boxes[i];
        order = rest
            .iter()
            .copied()
            .filter(|&j| {
                let bj = boxes[j];
                let xx1 = bi[0].max(bj[0]);
                let yy1 = bi[1].max(bj[1]);
                let xx2 = (bi[0] + bi[2]).min(bj[0] + bj[2]);
                let yy2 = (bi[1] + bi[3]).min(bj[1] + bj[3]);
                let inter = (xx2 - xx1).max(0.0) * (yy2 - yy1).max(0.0);
                inter / (area(&bi) + area(&bj) - inter).max(1e-9) <= iou
            })
            .collect();
    }
    keep
}

/// Rows `y0..y1`, columns `x0..x1` of an image (clamped to it).
pub fn crop(src: &[u8], w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> (Vec<u8>, usize, usize) {
    let (x1, y1) = (x1.min(w), y1.min(h));
    let (x0, y0) = (x0.min(x1), y0.min(y1));
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity(cw * ch * 3);
    for y in y0..y1 {
        out.extend_from_slice(&src[(y * w + x0) * 3..(y * w + x1) * 3]);
    }
    (out, cw, ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn umeyama_recovers_a_similarity() {
        let a = 23f64.to_radians();
        let src: Vec<[f32; 2]> = super::super::faces::ARCFACE_DST.to_vec();
        let dst: Vec<[f32; 2]> = src
            .iter()
            .map(|p| {
                let (x, y) = (p[0] as f64, p[1] as f64);
                [(1.7 * (a.cos() * x - a.sin() * y) + 12.0) as f32, (1.7 * (a.sin() * x + a.cos() * y) - 5.0) as f32]
            })
            .collect();
        let m = umeyama(&src, &dst);
        for (s, d) in src.iter().zip(&dst) {
            let x = m[0] * s[0] as f64 + m[1] * s[1] as f64 + m[2];
            let y = m[3] * s[0] as f64 + m[4] * s[1] as f64 + m[5];
            assert!((x - d[0] as f64).abs() < 1e-3 && (y - d[1] as f64).abs() < 1e-3);
        }
        assert!(((m[0] * m[4] - m[1] * m[3]).abs().sqrt() - 1.7).abs() < 1e-5);
    }

    #[test]
    fn nms_keeps_the_best_of_overlaps() {
        let boxes = [[0.0, 0.0, 10.0, 10.0], [1.0, 1.0, 10.0, 10.0], [50.0, 50.0, 10.0, 10.0]];
        assert_eq!(nms(&boxes, &[0.8, 0.9, 0.7], 0.3), vec![1, 2]);
        assert!(nms(&[], &[], 0.3).is_empty());
    }

    #[test]
    fn area_tables_cover_each_pixel_once() {
        for (s, d) in [(2048usize, 1600usize), (1365, 427), (100, 7), (10, 10)] {
            let scale = 1.0 / (d as f64 / s as f64);
            let tab = area_tab(s, d, scale);
            let mut per = vec![0f64; d];
            for t in &tab {
                per[t.di] += t.alpha as f64;
            }
            assert!(per.iter().all(|w| (w - 1.0).abs() < 1e-4), "{s} -> {d}");
        }
    }

    #[test]
    fn resizes_keep_flat_images_flat() {
        let img: Vec<u8> = std::iter::repeat_n([10u8, 128, 250], 37 * 23).flatten().collect();
        for interp in [Interp::Area, Interp::Linear, Interp::Cubic] {
            for (dw, dh) in [(10, 7), (74, 46), (37, 23), (18, 23)] {
                let out = resize(&img, 37, 23, dw, dh, interp);
                assert_eq!(out.len(), dw * dh * 3);
                assert!(out.chunks(3).all(|p| p == [10, 128, 250]), "{interp:?} {dw}x{dh}");
            }
        }
    }

    #[test]
    fn warps_move_pixels_where_the_map_says() {
        // A shift by (+2, +1): destination (x, y) shows source (x - 2, y - 1).
        let (w, h) = (8usize, 6usize);
        let img: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 0, 0]).collect();
        let out = warp_affine(&img, w, h, &[1.0, 0.0, 2.0, 0.0, 1.0, 1.0], w, h, Border::Constant(0));
        assert_eq!(out[(3 * w + 4) * 3], img[(2 * w + 2) * 3]);
        assert_eq!(out[0], 0);
        let rep = warp_affine(&img, w, h, &[1.0, 0.0, 2.0, 0.0, 1.0, 1.0], w, h, Border::Replicate);
        assert_eq!(rep[0], img[0]);
        let inv = invert_affine(&[2.0, 0.0, 3.0, 0.0, 2.0, -1.0]);
        assert!((inv[0] - 0.5).abs() < 1e-12 && (inv[2] + 1.5).abs() < 1e-12 && (inv[5] - 0.5).abs() < 1e-12);
    }
}
