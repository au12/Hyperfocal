//! Half-size RAW development with true RAW clipping detection and RAW histograms.
//!
//! The sensor data is decoded once (FlashRaw, all cores); one fused pass then bins
//! every 2x2 Bayer block into a pixel (no demosaicing), flags clipped channels from
//! the *raw* values (before white balance, like FastRawViewer), builds the RAW
//! histogram, applies white balance and the camera matrix, and stores linear sRGB as
//! half floats. Every view mode is then a cheap table lookup on that buffer.

use super::tone::f16_bits;
use anyhow::{Result, anyhow};
use flashraw::{CfaColor, DecodeOptions, Demosaic, FlashRaw, Highlights, OutputFormat, WhiteBalanceMode};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Instant;

pub const HIST_BINS: usize = 224;
pub const HIST_MIN_EV: f32 = -14.0;
pub const HIST_PER_EV: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WbMode {
    #[default]
    Asshot,
    Daylight,
    Auto,
}

/// Clipping-mask bits: bit `c` (0 = R, 1 = G, 2 = B) is set when that channel clipped.
pub const MASK_UNDER: u8 = 8;

pub struct Developed {
    /// Half-size dimensions (cropped, sensor orientation).
    pub w: u32,
    pub h: u32,
    /// Linear sRGB as f16 bit patterns, RGB interleaved.
    pub rgb: Vec<u16>,
    /// Per pixel: MASK_* bits.
    pub mask: Vec<u8>,
    pub hist: [Vec<u32>; 3],
    /// Fraction of clipped samples per channel and of pixels with any clipped channel.
    pub over: [f32; 4],
    pub under: f32,
    pub wb: [f32; 3],
    pub baseline_ev: f32,
    pub decode_ms: f32,
    pub develop_ms: f32,
    /// True when clipping/histogram come from processed (not raw) values.
    pub approximate: bool,
    mask_png: OnceLock<Vec<u8>>,
}

impl Developed {
    pub fn bytes(&self) -> usize {
        self.rgb.len() * 2 + self.mask.len() + 4096
    }

    /// The clipping mask as an 8-bit greyscale PNG (pixel value = MASK_* bits).
    pub fn mask_png(&self) -> &[u8] {
        self.mask_png.get_or_init(|| {
            let mut out = Vec::new();
            {
                let mut enc = png::Encoder::new(&mut out, self.w, self.h);
                enc.set_color(png::ColorType::Grayscale);
                enc.set_depth(png::BitDepth::Eight);
                enc.set_compression(png::Compression::Fast);
                if let Ok(mut wr) = enc.write_header() {
                    let _ = wr.write_image_data(&self.mask);
                }
            }
            out
        })
    }
}

fn wb_from_matrix(frame: &flashraw::RawFrame) -> Option<[f32; 3]> {
    let cms = &frame.color_matrices.color;
    let m = cms.iter().find(|m| m.illuminant.0 == 21).or_else(|| cms.last())?;
    if m.rows < 3 || m.values.len() < 9 {
        return None;
    }
    let v = &m.values;
    let xyz = [0.95047f32, 1.0, 1.08883];
    let cam: Vec<f32> = (0..3).map(|r| v[r * 3] * xyz[0] + v[r * 3 + 1] * xyz[1] + v[r * 3 + 2] * xyz[2]).collect();
    if cam.iter().any(|c| !(*c > 1e-6)) {
        return None;
    }
    Some([cam[1] / cam[0], 1.0, cam[1] / cam[2]])
}

fn chan(c: CfaColor) -> Option<usize> {
    match c {
        CfaColor::Red => Some(0),
        CfaColor::Green => Some(1),
        CfaColor::Blue => Some(2),
        _ => None,
    }
}

#[derive(Clone)]
struct Acc {
    hist: [Vec<u32>; 3],
    clipped: [u64; 3],
    samples: [u64; 3],
    any: u64,
    under: u64,
}

impl Acc {
    fn new() -> Acc {
        Acc { hist: [vec![0; HIST_BINS], vec![0; HIST_BINS], vec![0; HIST_BINS]], clipped: [0; 3], samples: [0; 3], any: 0, under: 0 }
    }
    fn merge(mut self, o: Acc) -> Acc {
        for c in 0..3 {
            for (a, b) in self.hist[c].iter_mut().zip(o.hist[c].iter()) {
                *a += *b;
            }
            self.clipped[c] += o.clipped[c];
            self.samples[c] += o.samples[c];
        }
        self.any += o.any;
        self.under += o.under;
        self
    }
}

/// Develop a RAW file held in memory.
pub fn develop(data: Vec<u8>, wb_mode: WbMode, under_ev: f32) -> Result<Developed> {
    let t0 = Instant::now();
    let raw = FlashRaw::open(data).map_err(|e| anyhow!("{e}"))?;
    let baseline = raw.metadata().dng.as_ref().and_then(|d| d.baseline_exposure).unwrap_or(0.0) as f32;
    let frame = raw
        .decode(DecodeOptions { output: OutputFormat::RawU16, ..Default::default() })
        .map_err(|e| anyhow!("{e}"))?;
    let decode_ms = t0.elapsed().as_secs_f32() * 1000.0;

    let bayer = match (&frame.cfa, frame.as_u16()) {
        (Some(c), Some(_)) if c.is_bayer() && c.width() == 2 && c.height() == 2 && frame.cpp == 1 && !frame.white_balance_applied => Some(c.clone()),
        _ => None,
    };
    let Some(cfa) = bayer else {
        drop(frame);
        return develop_fallback(&raw, wb_mode, under_ev, baseline, t0);
    };
    let data = frame.as_u16().unwrap();
    let fw = frame.width as usize;
    let crop = frame.crop;
    let (cx, cy) = (crop.x as usize, crop.y as usize);
    let hw = crop.width as usize / 2;
    let hh = crop.height as usize / 2;
    if hw == 0 || hh == 0 || (cy + hh * 2) > frame.height as usize || (cx + hw * 2) > fw {
        return Err(anyhow!("invalid crop"));
    }

    // Channel of each 2x2 position.
    let mut ch = [[0usize; 2]; 2];
    for (dy, row) in ch.iter_mut().enumerate() {
        for (dx, c) in row.iter_mut().enumerate() {
            *c = chan(cfa.color_at(cx + dx, cy + dy)).ok_or_else(|| anyhow!("unsupported CFA colour"))?;
        }
    }

    let bl = &frame.black_level;
    let simple_black = bl.row_deltas.is_empty() && bl.col_deltas.is_empty() && bl.rows <= 2 && bl.cols <= 2 && (bl.rows == 0 || 2 % bl.rows == 0) && (bl.cols == 0 || 2 % bl.cols == 0);
    let mut blk = [[0f32; 2]; 2];
    for (dy, row) in blk.iter_mut().enumerate() {
        for (dx, b) in row.iter_mut().enumerate() {
            *b = bl.at(cx + dx, cy + dy, 0);
        }
    }
    let blk_mean = (blk[0][0] + blk[0][1] + blk[1][0] + blk[1][1]) / 4.0;
    let white = frame.white_level.values.first().copied().unwrap_or(65535).max(1) as f32;

    // Saturation detection: some white levels are nominal; if many samples pile up at a
    // lower maximum, that is where the sensor actually clipped.
    let mut maxv = [0u16; 3];
    let step = 4usize;
    for by in (0..hh).step_by(step) {
        for dy in 0..2 {
            let row = &data[(cy + by * 2 + dy) * fw..];
            for bx in 0..hw {
                for dx in 0..2 {
                    let s = row[cx + bx * 2 + dx];
                    let c = ch[dy][dx];
                    if s > maxv[c] {
                        maxv[c] = s;
                    }
                }
            }
        }
    }
    let tol = ((white - blk_mean) * 0.002).max(2.0);
    let mut near = [0u64; 3];
    let mut cnt = [0u64; 3];
    for by in (0..hh).step_by(step) {
        for dy in 0..2 {
            let row = &data[(cy + by * 2 + dy) * fw..];
            for bx in 0..hw {
                for dx in 0..2 {
                    let s = row[cx + bx * 2 + dx] as f32;
                    let c = ch[dy][dx];
                    cnt[c] += 1;
                    if s >= maxv[c] as f32 - tol {
                        near[c] += 1;
                    }
                }
            }
        }
    }
    let mut sat = [white; 3];
    for c in 0..3 {
        let m = maxv[c] as f32;
        if m < white - tol && m > blk_mean + (white - blk_mean) * 0.25 && near[c] as f64 >= (cnt[c] as f64 * 0.0003).max(48.0) {
            sat[c] = m;
        }
    }
    let thr: [f32; 3] = [sat[0] - tol, sat[1] - tol, sat[2] - tol];
    let inv: [f32; 3] = [1.0 / (sat[0] - blk_mean).max(1.0), 1.0 / (sat[1] - blk_mean).max(1.0), 1.0 / (sat[2] - blk_mean).max(1.0)];

    // White balance.
    let as_shot = frame
        .white_balance
        .as_ref()
        .map(|w| {
            let g = if w.coeffs[1] > 0.0 { w.coeffs[1] } else { 1.0 };
            [w.coeffs[0] / g, 1.0, w.coeffs[2] / g]
        })
        .filter(|w| w.iter().all(|v| v.is_finite() && *v > 0.0))
        .unwrap_or([2.0, 1.0, 1.5]);
    let mut wb = match wb_mode {
        WbMode::Asshot => as_shot,
        WbMode::Daylight => wb_from_matrix(&frame).unwrap_or(as_shot),
        WbMode::Auto => as_shot,
    };
    if wb_mode == WbMode::Auto {
        // Refine the as-shot balance over near-neutral mid-tones.
        let mut sum = [0f64; 3];
        let mut n = 0u64;
        for by in (0..hh).step_by(3) {
            for bx in (0..hw).step_by(3) {
                let mut v = [0f32; 3];
                for dy in 0..2 {
                    for dx in 0..2 {
                        let s = data[(cy + by * 2 + dy) * fw + cx + bx * 2 + dx] as f32;
                        v[ch[dy][dx]] += (s - blk[dy][dx]) * inv[ch[dy][dx]];
                    }
                }
                v[1] *= 0.5;
                let (r, g, b) = (v[0] * wb[0], v[1], v[2] * wb[2]);
                if g > 0.03 && g < 0.85 && r < 0.95 && b < 0.95 && (r / g - 1.0).abs() < 0.3 && (b / g - 1.0).abs() < 0.3 {
                    sum[0] += v[0] as f64;
                    sum[1] += v[1] as f64;
                    sum[2] += v[2] as f64;
                    n += 1;
                }
            }
        }
        if n > 200 && sum[0] > 0.0 && sum[2] > 0.0 {
            let r = (sum[1] / sum[0]) as f32;
            let b = (sum[1] / sum[2]) as f32;
            wb = [r.clamp(as_shot[0] / 2.0, as_shot[0] * 2.0), 1.0, b.clamp(as_shot[2] / 2.0, as_shot[2] * 2.0)];
        }
    }

    let m = flashraw::process::camera_to_srgb(&frame.color_matrices);
    let under_thr = 2f32.powf(-under_ev.abs());

    // Histogram bin of every possible raw value, per channel.
    let lut_len = (white as usize + 1).max(maxv.iter().copied().max().unwrap_or(0) as usize + 1).min(65536);
    let bin_lut: Vec<[u8; 3]> = (0..lut_len)
        .map(|s| {
            let mut out = [0u8; 3];
            for c in 0..3 {
                let v = (s as f32 - blk_mean) * inv[c];
                out[c] = if v <= 0.0 {
                    0
                } else {
                    ((v.log2() - HIST_MIN_EV) * HIST_PER_EV).clamp(0.0, (HIST_BINS - 1) as f32) as u8
                };
            }
            out
        })
        .collect();

    let mut rgb = vec![0u16; hw * hh * 3];
    let mut mask = vec![0u8; hw * hh];
    let acc = rgb
        .par_chunks_mut(hw * 3)
        .zip(mask.par_chunks_mut(hw))
        .enumerate()
        .fold(Acc::new, |mut acc, (by, (rgb_row, mask_row))| {
            let y0 = cy + by * 2;
            for bx in 0..hw {
                let x0 = cx + bx * 2;
                let mut v = [0f32; 3];
                let mut clip = 0u8;
                for dy in 0..2 {
                    let base = (y0 + dy) * fw + x0;
                    for dx in 0..2 {
                        let s = data[base + dx];
                        let c = ch[dy][dx];
                        let b = if simple_black { blk[dy][dx] } else { bl.at(x0 + dx, y0 + dy, 0) };
                        v[c] += (s as f32 - b) * inv[c];
                        let sf = s as f32;
                        if sf >= thr[c] {
                            clip |= 1 << c;
                            acc.clipped[c] += 1;
                        }
                        acc.samples[c] += 1;
                        let bin = bin_lut.get(s as usize).map(|b| b[c]).unwrap_or((HIST_BINS - 1) as u8);
                        acc.hist[c][bin as usize] += 1;
                    }
                }
                v[1] *= 0.5;
                if v[0].max(v[1]).max(v[2]) < under_thr {
                    clip |= MASK_UNDER;
                    acc.under += 1;
                }
                if clip & 7 != 0 {
                    acc.any += 1;
                }
                let r = (v[0] * wb[0]).clamp(0.0, 1.0);
                let g = v[1].clamp(0.0, 1.0);
                let b = (v[2] * wb[2]).clamp(0.0, 1.0);
                let o = bx * 3;
                rgb_row[o] = f16_bits(m[0][0] * r + m[0][1] * g + m[0][2] * b);
                rgb_row[o + 1] = f16_bits(m[1][0] * r + m[1][1] * g + m[1][2] * b);
                rgb_row[o + 2] = f16_bits(m[2][0] * r + m[2][1] * g + m[2][2] * b);
                mask_row[bx] = clip;
            }
            acc
        })
        .reduce(Acc::new, Acc::merge);

    let px = (hw * hh) as f32;
    let over = [
        acc.clipped[0] as f32 / acc.samples[0].max(1) as f32,
        acc.clipped[1] as f32 / acc.samples[1].max(1) as f32,
        acc.clipped[2] as f32 / acc.samples[2].max(1) as f32,
        acc.any as f32 / px,
    ];
    Ok(Developed {
        w: hw as u32,
        h: hh as u32,
        rgb,
        mask,
        hist: acc.hist,
        over,
        under: acc.under as f32 / px,
        wb,
        baseline_ev: baseline,
        decode_ms,
        develop_ms: t0.elapsed().as_secs_f32() * 1000.0 - decode_ms,
        approximate: false,
        mask_png: OnceLock::new(),
    })
}

/// Non-Bayer sensors (X-Trans, linear DNG, sRAW...): let FlashRaw develop, then derive
/// approximate clipping and histogram from the processed values.
fn develop_fallback(raw: &FlashRaw, wb_mode: WbMode, under_ev: f32, baseline: f32, t0: Instant) -> Result<Developed> {
    let wbm = match wb_mode {
        WbMode::Asshot => WhiteBalanceMode::AsShot,
        WbMode::Daylight => WhiteBalanceMode::Daylight,
        WbMode::Auto => WhiteBalanceMode::Auto,
    };
    let mut opts = DecodeOptions {
        output: OutputFormat::LinearRgbF16,
        demosaic: Demosaic::HalfSize,
        crop: true,
        white_balance: wbm,
        highlights: Highlights::Clip,
        preview_fallback: true,
        ..Default::default()
    };
    let frame = match raw.decode(opts.clone()) {
        Ok(f) => f,
        Err(_) => {
            opts.demosaic = Demosaic::Bilinear;
            raw.decode(opts).map_err(|e| anyhow!("{e}"))?
        }
    };
    let decode_ms = t0.elapsed().as_secs_f32() * 1000.0;
    let bits = frame.as_f16_bits().ok_or_else(|| anyhow!("unexpected output"))?;
    let (mut w, mut h) = (frame.width as usize, frame.height as usize);
    let mut rgb: Vec<u16> = bits.to_vec();
    // Keep memory in check for full-resolution fallbacks.
    if w.max(h) > 4200 {
        let (nw, nh) = (w / 2, h / 2);
        let mut out = vec![0u16; nw * nh * 3];
        out.par_chunks_mut(nw * 3).enumerate().for_each(|(y, row)| {
            for x in 0..nw {
                for c in 0..3 {
                    let mut s = 0f32;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            s += half::f16::from_bits(rgb[((y * 2 + dy) * w + x * 2 + dx) * 3 + c]).to_f32();
                        }
                    }
                    row[x * 3 + c] = f16_bits(s * 0.25);
                }
            }
        });
        rgb = out;
        w = nw;
        h = nh;
    }
    let under_thr = 2f32.powf(-under_ev.abs());
    let mut mask = vec![0u8; w * h];
    let mut hist = [vec![0u32; HIST_BINS], vec![0u32; HIST_BINS], vec![0u32; HIST_BINS]];
    let mut clipped = [0u64; 3];
    let mut any = 0u64;
    let mut under = 0u64;
    for i in 0..w * h {
        let mut clip = 0u8;
        let mut mx = 0f32;
        for c in 0..3 {
            let v = half::f16::from_bits(rgb[i * 3 + c]).to_f32();
            mx = mx.max(v);
            if v >= 0.998 {
                clip |= 1 << c;
                clipped[c] += 1;
            }
            let bin = if v <= 0.0 { 0.0 } else { ((v.log2() - HIST_MIN_EV) * HIST_PER_EV).clamp(0.0, (HIST_BINS - 1) as f32) };
            hist[c][bin as usize] += 1;
        }
        if mx < under_thr {
            clip |= MASK_UNDER;
            under += 1;
        }
        if clip & 7 != 0 {
            any += 1;
        }
        mask[i] = clip;
    }
    let px = (w * h).max(1) as f32;
    Ok(Developed {
        w: w as u32,
        h: h as u32,
        rgb,
        mask,
        hist,
        over: [clipped[0] as f32 / px, clipped[1] as f32 / px, clipped[2] as f32 / px, any as f32 / px],
        under: under as f32 / px,
        wb: [1.0, 1.0, 1.0],
        baseline_ev: baseline,
        decode_ms,
        develop_ms: t0.elapsed().as_secs_f32() * 1000.0 - decode_ms,
        approximate: true,
        mask_png: OnceLock::new(),
    })
}

/// f16 bits -> f32 for all 65536 patterns.
pub fn f16_table() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| (0..=u16::MAX).map(|b| half::f16::from_bits(b).to_f32()).collect())
}

/// Tone-map a developed image to 8-bit RGB, at most `max_long` pixels on the long edge.
pub fn render(dev: &Developed, lut: &[u8], max_long: usize) -> super::jpeg::Rgb {
    let (w, h) = (dev.w as usize, dev.h as usize);
    let long = w.max(h);
    let factor = if max_long > 0 && long >= max_long * 2 { (long / max_long).clamp(1, 4) } else { 1 };
    let (tw, th) = (w / factor, h / factor);
    let mut px = vec![0u8; tw * th * 3];
    if factor == 1 {
        px.par_chunks_mut(tw * 3).enumerate().for_each(|(y, row)| {
            let src = &dev.rgb[y * w * 3..y * w * 3 + tw * 3];
            for (o, s) in row.iter_mut().zip(src.iter()) {
                *o = lut[*s as usize];
            }
        });
    } else {
        let t = f16_table();
        let inv = 1.0 / (factor * factor) as f32;
        px.par_chunks_mut(tw * 3).enumerate().for_each(|(y, row)| {
            for x in 0..tw {
                for c in 0..3 {
                    let mut s = 0f32;
                    for dy in 0..factor {
                        let base = ((y * factor + dy) * w + x * factor) * 3 + c;
                        for dx in 0..factor {
                            s += t[dev.rgb[base + dx * 3] as usize];
                        }
                    }
                    row[x * 3 + c] = lut[f16_bits(s * inv) as usize];
                }
            }
        });
    }
    super::jpeg::Rgb { w: tw, h: th, px }
}
