//! Full-resolution demosaicing of Bayer data: RCD (Ratio Corrected Demosaicing,
//! Luis Sanz Rodríguez, v2.3, as in RawTherapee and darktable), for a part of the
//! frame at a time. The web viewer's WebGL path (`web/src/lib/viewer/rcd.ts`) runs the
//! same steps on the GPU.
//!
//! Input: CFA samples normalised to 0..1, white balance applied; `fc(row, col)` gives
//! the colour (0 R, 1 G, 2 B) of each sample. Output: three planes of linear camera RGB.
//! The 4 samples along each edge are not interpolated (pad the input).

const EPS: f32 = 1e-5;
const EPSSQ: f32 = 1e-10;

#[inline]
fn sq(x: f32) -> f32 {
    x * x
}

/// The weight between two directions: the more decided of the pixel's own and its
/// diagonal neighbours' (closest to 0 or 1).
#[inline]
fn refine(central: f32, neighbourhood: f32) -> f32 {
    if (0.5 - central).abs() < (0.5 - neighbourhood).abs() { neighbourhood } else { central }
}

pub fn rcd(cfa: &[f32], w: usize, h: usize, fc: impl Fn(usize, usize) -> usize + Sync) -> [Vec<f32>; 3] {
    assert_eq!(cfa.len(), w * h);
    let n = w * h;
    let mut rgb = [vec![0f32; n], vec![0f32; n], vec![0f32; n]];
    for row in 0..h {
        for col in 0..w {
            let i = row * w + col;
            rgb[fc(row, col)][i] = cfa[i];
        }
    }
    if w < 10 || h < 10 {
        return rgb;
    }
    let c = cfa;
    let (w1, w2, w3, w4) = (w, 2 * w, 3 * w, 4 * w);

    // Step 1: vertical / horizontal discrimination. 1.1: squared high-pass of the
    // colour differences in each direction; 1.2: their local sums, as a weight.
    let mut vhpf = vec![0f32; n];
    let mut hhpf = vec![0f32; n];
    for row in 3..h - 3 {
        for col in 3..w - 3 {
            let i = row * w + col;
            vhpf[i] = sq((c[i - w3] - c[i - w1] - c[i + w1] + c[i + w3]) - 3.0 * (c[i - w2] + c[i + w2]) + 6.0 * c[i]);
            hhpf[i] = sq((c[i - 3] - c[i - 1] - c[i + 1] + c[i + 3]) - 3.0 * (c[i - 2] + c[i + 2]) + 6.0 * c[i]);
        }
    }
    let mut vh = vec![0f32; n];
    for row in 4..h - 4 {
        for col in 4..w - 4 {
            let i = row * w + col;
            let v = (vhpf[i - w1] + vhpf[i] + vhpf[i + w1]).max(EPSSQ);
            let hs = (hhpf[i - 1] + hhpf[i] + hhpf[i + 1]).max(EPSSQ);
            vh[i] = v / (v + hs);
        }
    }
    drop((vhpf, hhpf));

    // Step 2: low-pass filter at red and blue samples.
    let mut lpf = vec![0f32; n];
    for row in 2..h - 2 {
        for col in 2..w - 2 {
            if fc(row, col) == 1 {
                continue;
            }
            let i = row * w + col;
            lpf[i] = c[i] + 0.5 * (c[i - w1] + c[i + w1] + c[i - 1] + c[i + 1]) + 0.25 * (c[i - w1 - 1] + c[i - w1 + 1] + c[i + w1 - 1] + c[i + w1 + 1]);
        }
    }

    // Step 3: green at red and blue samples, from ratio-corrected cardinal estimates.
    for row in 4..h - 4 {
        for col in 4..w - 4 {
            if fc(row, col) == 1 {
                continue;
            }
            let i = row * w + col;
            let disc = refine(vh[i], 0.25 * (vh[i - w1 - 1] + vh[i - w1 + 1] + vh[i + w1 - 1] + vh[i + w1 + 1]));
            let n_grad = EPS + (c[i - w1] - c[i + w1]).abs() + (c[i] - c[i - w2]).abs() + (c[i - w1] - c[i - w3]).abs() + (c[i - w2] - c[i - w4]).abs();
            let s_grad = EPS + (c[i - w1] - c[i + w1]).abs() + (c[i] - c[i + w2]).abs() + (c[i + w1] - c[i + w3]).abs() + (c[i + w2] - c[i + w4]).abs();
            let w_grad = EPS + (c[i - 1] - c[i + 1]).abs() + (c[i] - c[i - 2]).abs() + (c[i - 1] - c[i - 3]).abs() + (c[i - 2] - c[i - 4]).abs();
            let e_grad = EPS + (c[i - 1] - c[i + 1]).abs() + (c[i] - c[i + 2]).abs() + (c[i + 1] - c[i + 3]).abs() + (c[i + 2] - c[i + 4]).abs();
            let l = lpf[i];
            let n_est = c[i - w1] * (1.0 + (l - lpf[i - w2]) / (EPS + l + lpf[i - w2]));
            let s_est = c[i + w1] * (1.0 + (l - lpf[i + w2]) / (EPS + l + lpf[i + w2]));
            let w_est = c[i - 1] * (1.0 + (l - lpf[i - 2]) / (EPS + l + lpf[i - 2]));
            let e_est = c[i + 1] * (1.0 + (l - lpf[i + 2]) / (EPS + l + lpf[i + 2]));
            let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
            let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);
            rgb[1][i] = (disc * h_est + (1.0 - disc) * v_est).clamp(0.0, 1.0);
        }
    }
    drop(lpf);

    // Step 4: red and blue. 4.0/4.1: diagonal (P = NW-SE, Q = NE-SW) discrimination.
    let mut phpf = vec![0f32; n];
    let mut qhpf = vec![0f32; n];
    for row in 3..h - 3 {
        for col in 3..w - 3 {
            let i = row * w + col;
            phpf[i] = sq((c[i - w3 - 3] - c[i - w1 - 1] - c[i + w1 + 1] + c[i + w3 + 3]) - 3.0 * (c[i - w2 - 2] + c[i + w2 + 2]) + 6.0 * c[i]);
            qhpf[i] = sq((c[i - w3 + 3] - c[i - w1 + 1] - c[i + w1 - 1] + c[i + w3 - 3]) - 3.0 * (c[i - w2 + 2] + c[i + w2 - 2]) + 6.0 * c[i]);
        }
    }
    let mut pq = vec![0f32; n];
    for row in 4..h - 4 {
        for col in 4..w - 4 {
            if fc(row, col) == 1 {
                continue;
            }
            let i = row * w + col;
            let p = (phpf[i - w1 - 1] + phpf[i] + phpf[i + w1 + 1]).max(EPSSQ);
            let q = (qhpf[i - w1 + 1] + qhpf[i] + qhpf[i + w1 - 1]).max(EPSSQ);
            pq[i] = p / (p + q);
        }
    }
    drop((phpf, qhpf));
    // 4.2: red at blue samples and blue at red ones, from diagonal colour differences.
    for row in 4..h - 4 {
        for col in 4..w - 4 {
            let own = fc(row, col);
            if own == 1 {
                continue;
            }
            let i = row * w + col;
            let ch = 2 - own;
            let disc = refine(pq[i], 0.25 * (pq[i - w1 - 1] + pq[i - w1 + 1] + pq[i + w1 - 1] + pq[i + w1 + 1]));
            let (x, g) = (&rgb[ch], &rgb[1]);
            let nw_grad = EPS + (x[i - w1 - 1] - x[i + w1 + 1]).abs() + (x[i - w1 - 1] - x[i - w3 - 3]).abs() + (g[i] - g[i - w2 - 2]).abs();
            let ne_grad = EPS + (x[i - w1 + 1] - x[i + w1 - 1]).abs() + (x[i - w1 + 1] - x[i - w3 + 3]).abs() + (g[i] - g[i - w2 + 2]).abs();
            let sw_grad = EPS + (x[i - w1 + 1] - x[i + w1 - 1]).abs() + (x[i + w1 - 1] - x[i + w3 - 3]).abs() + (g[i] - g[i + w2 - 2]).abs();
            let se_grad = EPS + (x[i - w1 - 1] - x[i + w1 + 1]).abs() + (x[i + w1 + 1] - x[i + w3 + 3]).abs() + (g[i] - g[i + w2 + 2]).abs();
            let nw_est = x[i - w1 - 1] - g[i - w1 - 1];
            let ne_est = x[i - w1 + 1] - g[i - w1 + 1];
            let sw_est = x[i + w1 - 1] - g[i + w1 - 1];
            let se_est = x[i + w1 + 1] - g[i + w1 + 1];
            let p_est = (nw_grad * se_est + se_grad * nw_est) / (nw_grad + se_grad);
            let q_est = (ne_grad * sw_est + sw_grad * ne_est) / (ne_grad + sw_grad);
            let v = (g[i] + (1.0 - disc) * p_est + disc * q_est).clamp(0.0, 1.0);
            rgb[ch][i] = v;
        }
    }
    // 4.3: red and blue at green samples, from cardinal colour differences.
    for row in 4..h - 4 {
        for col in 4..w - 4 {
            if fc(row, col) != 1 {
                continue;
            }
            let i = row * w + col;
            let disc = refine(vh[i], 0.25 * (vh[i - w1 - 1] + vh[i - w1 + 1] + vh[i + w1 - 1] + vh[i + w1 + 1]));
            let g = &rgb[1];
            let (g0, gn, gs, gw, ge) = (g[i], g[i - w2], g[i + w2], g[i - 2], g[i + 2]);
            let (gn1, gs1, gw1, ge1) = (g[i - w1], g[i + w1], g[i - 1], g[i + 1]);
            for ch in [0usize, 2] {
                let x = &rgb[ch];
                let sn = (x[i - w1] - x[i + w1]).abs();
                let ew = (x[i - 1] - x[i + 1]).abs();
                let n_grad = EPS + sn + (x[i - w1] - x[i - w3]).abs() + (g0 - gn).abs();
                let s_grad = EPS + sn + (x[i + w1] - x[i + w3]).abs() + (g0 - gs).abs();
                let w_grad = EPS + ew + (x[i - 1] - x[i - 3]).abs() + (g0 - gw).abs();
                let e_grad = EPS + ew + (x[i + 1] - x[i + 3]).abs() + (g0 - ge).abs();
                let n_est = x[i - w1] - gn1;
                let s_est = x[i + w1] - gs1;
                let w_est = x[i - 1] - gw1;
                let e_est = x[i + 1] - ge1;
                let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
                let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
                let v = (g0 + (1.0 - disc) * v_est + disc * h_est).clamp(0.0, 1.0);
                rgb[ch][i] = v;
            }
        }
    }
    rgb
}

/// Bilinear demosaicing (for comparison in tests).
#[cfg(test)]
pub fn bilinear(cfa: &[f32], w: usize, h: usize, fc: impl Fn(usize, usize) -> usize) -> [Vec<f32>; 3] {
    let mut rgb = [vec![0f32; w * h], vec![0f32; w * h], vec![0f32; w * h]];
    for row in 1..h - 1 {
        for col in 1..w - 1 {
            for ch in 0..3 {
                let (mut s, mut k) = (0f32, 0f32);
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let (r, cc) = ((row as i32 + dy) as usize, (col as i32 + dx) as usize);
                        if fc(r, cc) == ch {
                            let wt = if dx == 0 && dy == 0 {
                                4.0
                            } else if dx == 0 || dy == 0 {
                                2.0
                            } else {
                                1.0
                            };
                            s += cfa[r * w + cc] * wt;
                            k += wt;
                        }
                    }
                }
                rgb[ch][row * w + col] = if fc(row, col) == ch { cfa[row * w + col] } else { s / k.max(1e-6) };
            }
        }
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    const RGGB: [[usize; 2]; 2] = [[0, 1], [1, 2]];

    fn mosaic(img: &[[f32; 3]], w: usize, h: usize, p: [[usize; 2]; 2]) -> Vec<f32> {
        (0..w * h).map(|i| img[i][p[(i / w) % 2][(i % w) % 2]]).collect()
    }

    /// A test card: soft colour gradients, a sharp diagonal edge, fine lines and a disc.
    fn card(w: usize, h: usize) -> Vec<[f32; 3]> {
        let mut v = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                let mut c = [0.2 + 0.6 * fx, 0.15 + 0.5 * fy, 0.7 - 0.5 * fx * fy];
                if (x as f32) > 0.7 * y as f32 + 20.0 {
                    c = [c[0] * 0.35, c[1] * 1.2, c[2] * 0.5];
                }
                if (40..44).contains(&(y % 64)) {
                    c = [0.9, 0.9, 0.85];
                }
                let (dx, dy) = (x as f32 - 0.7 * w as f32, y as f32 - 0.6 * h as f32);
                if dx * dx + dy * dy < 400.0 {
                    c = [0.8, 0.1, 0.1];
                }
                v.push(c.map(|x| x.clamp(0.0, 1.0)));
            }
        }
        v
    }

    fn psnr(out: &[Vec<f32>; 3], img: &[[f32; 3]], w: usize, h: usize, border: usize) -> f64 {
        let mut se = 0f64;
        let mut n = 0f64;
        for y in border..h - border {
            for x in border..w - border {
                for c in 0..3 {
                    let d = (out[c][y * w + x] - img[y * w + x][c]) as f64;
                    se += d * d;
                    n += 1.0;
                }
            }
        }
        10.0 * (1.0 / (se / n)).log10()
    }

    #[test]
    fn flat_stays_flat() {
        let (w, h) = (32, 24);
        let img = vec![[0.3f32, 0.5, 0.7]; w * h];
        let out = rcd(&mosaic(&img, w, h, RGGB), w, h, |r, c| RGGB[r % 2][c % 2]);
        for y in 6..h - 6 {
            for x in 6..w - 6 {
                for c in 0..3 {
                    assert!((out[c][y * w + x] - img[0][c]).abs() < 1e-4, "{x},{y} ch {c}: {}", out[c][y * w + x]);
                }
            }
        }
    }

    #[test]
    fn beats_bilinear_on_a_test_card() {
        let (w, h) = (192, 160);
        let img = card(w, h);
        for p in [RGGB, [[1, 0], [2, 1]], [[2, 1], [1, 0]], [[1, 2], [0, 1]]] {
            let cfa = mosaic(&img, w, h, p);
            let fc = |r: usize, c: usize| p[r % 2][c % 2];
            let r = psnr(&rcd(&cfa, w, h, fc), &img, w, h, 6);
            let b = psnr(&bilinear(&cfa, w, h, fc), &img, w, h, 6);
            // Measured: 34.7 dB against bilinear's 27.7 dB.
            assert!(r > 33.5, "RCD {r:.1} dB for {p:?}");
            assert!(r > b + 5.0, "RCD {r:.1} dB, bilinear {b:.1} dB for {p:?}");
        }
    }

    #[test]
    fn stays_in_range() {
        // Hard edges and clipped highlights: never below 0 or above 1.
        let (w, h) = (48, 48);
        let img: Vec<[f32; 3]> = (0..w * h).map(|i| if (i % w / 3 + i / w / 5) % 2 == 0 { [1.0, 1.0, 1.0] } else { [0.0, 0.02, 0.0] }).collect();
        let out = rcd(&mosaic(&img, w, h, RGGB), w, h, |r, c| RGGB[r % 2][c % 2]);
        assert!(out.iter().flatten().all(|v| (0.0..=1.0).contains(v)));
    }
}
