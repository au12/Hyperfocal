//! Display transforms for developed linear RAW data. Every transform is a monotonic
//! per-channel curve, so it is baked into a 64K lookup table indexed by the f16 bit
//! pattern of the linear value: rendering is then three table lookups per pixel.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToneMode {
    /// FastRawViewer's default rendering: contrast with a soft highlight shoulder.
    #[default]
    Normal,
    /// FastRawViewer "linear": a plain 2.2 gamma, no contrast curve.
    Linear,
    /// FastRawViewer "shadows boost": deep shadows lifted by about 2.3 EV, fading out
    /// towards the highlights.
    Shadows,
    /// FastRawViewer "highlights inspection": a 1.33 gamma that spreads the top stops.
    Highlights,
}

// Calibrated on FastRawViewer screenshots of the same Canon R6 CR3 in every mode (sRGB,
// matched pixel for pixel to this pipeline's linear output). FastRawViewer's "linear"
// output is the raw signal over the level that renders white (Canon's NormalWhiteLevel,
// see `raw::Developed`) with a pure 2.2 gamma; each other mode is a fixed per-channel
// curve of that output (spread under one level over 800k samples per mode).

/// Gamma of the "linear" mode, and the base every other mode is a curve of.
const GAMMA: f32 = 2.2;

/// Display value (0-255) of Normal at linear-mode values 0, 8, 16 ... 248, 255.
const NORMAL: [f32; 33] = [
    0.0, 4.8, 6.0, 10.4, 18.3, 28.6, 41.3, 55.4, 71.0, 87.0, 102.0,
    117.9, 132.1, 145.7, 158.5, 170.3, 180.8, 190.1, 199.0, 206.5, 213.3, 219.4,
    225.0, 230.0, 234.5, 238.5, 242.0, 245.4, 248.3, 251.0, 253.7, 254.0, 255.0,
];

/// Display value (0-255) of Shadows boost at linear-mode values 0, 8, 16 ... 248, 255.
const SHADOWS: [f32; 33] = [
    0.0, 14.3, 23.6, 40.0, 58.8, 78.2, 97.0, 114.0, 129.9, 144.4, 157.2,
    169.0, 179.2, 188.0, 196.0, 203.6, 209.5, 215.4, 220.4, 225.0, 229.2, 232.9,
    236.2, 239.3, 242.0, 244.7, 247.0, 249.0, 251.0, 252.9, 254.0, 254.0, 255.0,
];

/// Highlights inspection: linear-mode value raised to this power (gamma 2.2 / 1.66 = 1.33).
const HIGHLIGHTS_POWER: f32 = 1.66;

/// Shape-preserving piecewise cubic (PCHIP, as scipy's `PchipInterpolator`) through
/// `table` at linear-mode values 0, 8 ... 248, 255; `l` in [0, 1], result in [0, 1].
fn table_curve(table: &[f32; 33], l: f32) -> f32 {
    const N: usize = 33;
    let xk = |i: usize| if i == N - 1 { 255.0 } else { i as f32 * 8.0 };
    let x = (l * 255.0).clamp(0.0, 255.0);
    let k = ((x / 8.0) as usize).min(N - 2);
    let h = |i: usize| xk(i + 1) - xk(i);
    let d = |i: usize| (table[i + 1] - table[i]) / h(i);
    // Derivative at knot i: weighted harmonic mean of the neighbouring slopes, zero at
    // extrema; one-sided, shape-preserving three-point estimate at the ends.
    let slope = |i: usize| -> f32 {
        if i == 0 || i == N - 1 {
            let (h0, h1, d0, d1) = if i == 0 { (h(0), h(1), d(0), d(1)) } else { (h(N - 2), h(N - 3), d(N - 2), d(N - 3)) };
            let m = ((2.0 * h0 + h1) * d0 - h0 * d1) / (h0 + h1);
            if m.signum() != d0.signum() {
                0.0
            } else if d0.signum() != d1.signum() && m.abs() > 3.0 * d0.abs() {
                3.0 * d0
            } else {
                m
            }
        } else {
            let (d0, d1) = (d(i - 1), d(i));
            if d0 * d1 <= 0.0 {
                0.0
            } else {
                let (w1, w2) = (2.0 * h(i) + h(i - 1), h(i) + 2.0 * h(i - 1));
                (w1 + w2) / (w1 / d0 + w2 / d1)
            }
        }
    };
    let t = (x - xk(k)) / h(k);
    let (t2, t3) = (t * t, t * t * t);
    let y = (2.0 * t3 - 3.0 * t2 + 1.0) * table[k]
        + (t3 - 2.0 * t2 + t) * h(k) * slope(k)
        + (-2.0 * t3 + 3.0 * t2) * table[k + 1]
        + (t3 - t2) * h(k) * slope(k + 1);
    (y / 255.0).clamp(0.0, 1.0)
}

/// Map one linear value (1.0 renders white, after exposure gain) through the mode's curve
/// to [0, 1].
pub fn curve(mode: ToneMode, x: f32) -> f32 {
    if !(x > 0.0) {
        return 0.0;
    }
    let l = x.min(1.0).powf(1.0 / GAMMA);
    match mode {
        ToneMode::Linear => l,
        ToneMode::Normal => table_curve(&NORMAL, l),
        ToneMode::Shadows => table_curve(&SHADOWS, l),
        ToneMode::Highlights => l.powf(HIGHLIGHTS_POWER),
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

    #[test]
    fn calibrated_on_fastrawviewer() {
        assert!((curve(ToneMode::Linear, 0.25) - 0.25f32.powf(1.0 / 2.2)).abs() < 1e-6);
        for mode in [ToneMode::Normal, ToneMode::Linear, ToneMode::Shadows, ToneMode::Highlights] {
            assert_eq!(curve(mode, 0.0), 0.0);
            assert!((curve(mode, 1.0) - 1.0).abs() < 1e-4, "{mode:?}");
            assert!((curve(mode, 4.0) - 1.0).abs() < 1e-4, "{mode:?} above white");
        }
        // Display levels read off FastRawViewer for a given level of its linear mode (medians
        // over its screenshots), within a level.
        let at = |mode, linear_level: f32| curve(mode, (linear_level / 255.0).powf(2.2)) * 255.0;
        for (l, v) in [(36.0, 23.0), (60.0, 63.0), (96.0, 132.0), (132.0, 186.0), (180.0, 228.0), (228.0, 250.0)] {
            assert!((at(ToneMode::Normal, l) - v).abs() <= 1.0, "normal {l}: {}", at(ToneMode::Normal, l));
        }
        for (l, v) in [(24.0, 40.0), (48.0, 97.0), (96.0, 179.0), (156.0, 227.0), (216.0, 249.0)] {
            assert!((at(ToneMode::Shadows, l) - v).abs() <= 1.0, "shadows {l}: {}", at(ToneMode::Shadows, l));
        }
        for (l, v) in [(48.0, 16.0), (120.0, 73.0), (192.0, 159.0), (240.0, 231.0)] {
            assert!((at(ToneMode::Highlights, l) - v).abs() <= 1.5, "highlights {l}: {}", at(ToneMode::Highlights, l));
        }
    }
}
