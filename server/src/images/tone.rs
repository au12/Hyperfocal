//! Display transforms for developed linear RAW data. Every transform is a monotonic
//! per-channel curve, so it is baked into a 64K lookup table indexed by the f16 bit
//! pattern of the linear value: rendering is then three table lookups per pixel.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToneMode {
    /// Camera-like rendering: gentle highlight shoulder plus a medium contrast curve.
    #[default]
    Normal,
    /// FastRawViewer "linear": only the sRGB transfer curve, no contrast curve.
    Linear,
    /// Lifts the shadows by up to 3 EV to judge noise and shadow detail.
    Shadows,
    /// Spreads the top three stops over the whole display range.
    Highlights,
}

pub fn srgb_oetf(x: f32) -> f32 {
    if x <= 0.0031308 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 }
}

/// Adobe "Medium Contrast" style curve in display space (monotone cubic through
/// (0,0) (0.125,0.086) (0.25,0.22) (0.5,0.5) (0.75,0.77) (1,1)).
fn contrast(s: f32) -> f32 {
    const P: [(f32, f32); 6] = [(0.0, 0.0), (0.125, 0.086), (0.25, 0.22), (0.5, 0.5), (0.75, 0.77), (1.0, 1.0)];
    let s = s.clamp(0.0, 1.0);
    // Fritsch-Carlson monotone interpolation (tangents precomputed lazily; tiny table).
    let n = P.len();
    let mut d = [0f32; 6];
    let mut m = [0f32; 6];
    for i in 0..n - 1 {
        d[i] = (P[i + 1].1 - P[i].1) / (P[i + 1].0 - P[i].0);
    }
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
    }
    let mut k = 0;
    while k < n - 2 && s > P[k + 1].0 {
        k += 1;
    }
    let (x0, y0) = P[k];
    let (x1, y1) = P[k + 1];
    let h = x1 - x0;
    let t = (s - x0) / h;
    let (t2, t3) = (t * t, t * t * t);
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    (h00 * y0 + h10 * h * m[k] + h01 * y1 + h11 * h * m[k + 1]).clamp(0.0, 1.0)
}

/// Map one linear value (after exposure gain) through the mode's curve to [0, 1].
pub fn curve(mode: ToneMode, x: f32) -> f32 {
    if !(x > 0.0) {
        return 0.0;
    }
    match mode {
        ToneMode::Linear => srgb_oetf(x.min(1.0)),
        ToneMode::Normal => contrast(srgb_oetf(x.min(1.0))),
        ToneMode::Shadows => {
            // +2.5 EV in the deep shadows, fading out by -1.5 EV: shadows open up while
            // mid-tones and highlights stay where they are (monotonic: the boost never
            // falls faster than one stop per stop).
            let x = x.min(1.0);
            let ev = x.log2();
            let t = ((ev + 6.0) / 4.5).clamp(0.0, 1.0);
            let t = t * t * (3.0 - 2.0 * t);
            srgb_oetf((x * 2f32.powf(2.5 * (1.0 - t))).min(1.0))
        }
        ToneMode::Highlights => {
            const STOPS: f32 = 3.0;
            (1.0 + x.log2() / STOPS).clamp(0.0, 1.0)
        }
    }
}

/// 64K table: f16 bit pattern of a linear value -> 8-bit display value.
pub fn build_lut(mode: ToneMode, ev: f32) -> Vec<u8> {
    let gain = 2f32.powf(ev);
    (0..=u16::MAX)
        .map(|bits| {
            let x = half::f16::from_bits(bits).to_f32();
            if !x.is_finite() || x <= 0.0 {
                return if x.is_infinite() && x > 0.0 { 255 } else { 0 };
            }
            (curve(mode, x * gain) * 255.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// Fast f32 -> f16 bits for finite, non-negative values (negatives clamp to 0).
#[inline]
pub fn f16_bits(x: f32) -> u16 {
    if !(x > 0.0) {
        return 0;
    }
    if x < 6.103_515_6e-5 {
        return (x * 16_777_216.0) as u16; // subnormal: x / 2^-24
    }
    if x >= 65504.0 {
        return 0x7bff;
    }
    let b = x.to_bits();
    let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let m = (b >> 13) & 0x3ff;
    let round = (b >> 12) & 1;
    (((e as u32) << 10) | m) as u16 + round as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn f16_matches_half() {
        for &x in &[0.0f32, 1e-6, 6e-5, 0.001, 0.18, 0.5, 1.0, 2.5, 1000.0] {
            let a = half::f16::from_bits(f16_bits(x)).to_f32();
            let b = half::f16::from_f32(x).to_f32();
            assert!((a - b).abs() <= b.abs() * 0.002 + 1e-7, "{x}: {a} vs {b}");
        }
    }
    #[test]
    fn curves_monotone() {
        for mode in [ToneMode::Normal, ToneMode::Linear, ToneMode::Shadows, ToneMode::Highlights] {
            let mut last = -1.0;
            for i in 0..=1000 {
                let y = curve(mode, i as f32 / 800.0);
                assert!(y >= last - 1e-6, "{mode:?} not monotone at {i}");
                last = y;
            }
            assert!(curve(mode, 1.0) > 0.9);
        }
        // Shadow boost lifts dark tones.
        assert!(curve(ToneMode::Shadows, 0.01) > curve(ToneMode::Linear, 0.01));
    }
}
