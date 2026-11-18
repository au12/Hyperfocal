//! Half-size RAW development with true RAW clipping detection and RAW histograms.
//!
//! The sensor data is decoded once (FlashRaw, all cores); one fused pass then bins
//! every 2x2 Bayer block into a pixel (no demosaicing), flags clipped channels from
//! the *raw* values (before white balance, like FastRawViewer), builds the RAW
//! histogram, applies white balance and the camera matrix, and stores linear sRGB as
//! half floats. Every view mode is then a cheap table lookup on that buffer.

use super::simd;
use super::tone::f16_bits;
use crate::native::{self, LibrawRaw};
use anyhow::{Result, anyhow};
use flashraw::{CfaColor, DecodeOptions, Demosaic, FlashRaw, Highlights, OutputFormat, WhiteBalanceMode};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::mem::MaybeUninit;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

/// Runs a decoder call, turning a panic on a malformed file into an error (so one bad
/// file cannot take down the indexer or the thumbnail worker).
pub fn catch<T, E: std::fmt::Display>(f: impl FnOnce() -> std::result::Result<T, E>) -> std::result::Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(_) => Err("the decoder failed on this file".into()),
    }
}

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

/// Clipping-mask bits: bit `c` (0 = R, 1 = G, 2 = B) is set when that channel clipped,
/// bit `MASK_UNDER_SHIFT + c` when that channel is underexposed.
pub const MASK_UNDER_SHIFT: u8 = 3;

/// Samples within this many EV of saturation count as clipped. Photosites saturate over a
/// narrow band just below the white level, and FastRawViewer flags the same band (measured
/// on its exposure stats: an R6 at ISO 1250 reads 0.105% green clipped, -0.048 EV).
pub const OE_MARGIN_EV: f32 = 0.05;

/// Samples more than this many EV below saturation count as underexposed: FastRawViewer's
/// default limit, measured on its exposure stats (the same R6 file: R 1.36%, G 0.269%,
/// B 4.10%, every channel below the same raw level, before white balance).
pub const UE_EV: f32 = 7.7;

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
    /// Fraction of underexposed samples per channel and of pixels with any such channel.
    pub under: [f32; 4],
    pub wb: [f32; 3],
    pub baseline_ev: f32,
    pub decode_ms: f32,
    pub develop_ms: f32,
    /// True when clipping/histogram come from processed (not raw) values.
    pub approximate: bool,
    /// Which decoder read the sensor data.
    pub decoder: &'static str,
    /// How samples were scaled (per channel: to saturation, then to the level that
    /// renders white) and converted to sRGB, so full-resolution tiles match.
    pub inv: [f32; 3],
    pub rscale: [f32; 3],
    pub cam_to_srgb: [[f32; 3]; 3],
    /// Full-resolution size of the area `w` x `h` covers (Bayer data only): 1:1 tiles.
    pub full: Option<(u32, u32)>,
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
    under: [u64; 3],
    under_any: u64,
}

impl Acc {
    fn new() -> Acc {
        Acc { hist: [vec![0; HIST_BINS], vec![0; HIST_BINS], vec![0; HIST_BINS]], clipped: [0; 3], samples: [0; 3], any: 0, under: [0; 3], under_any: 0 }
    }
    fn merge(mut self, o: Acc) -> Acc {
        for c in 0..3 {
            for (a, b) in self.hist[c].iter_mut().zip(o.hist[c].iter()) {
                *a += *b;
            }
            self.clipped[c] += o.clipped[c];
            self.samples[c] += o.samples[c];
            self.under[c] += o.under[c];
        }
        self.any += o.any;
        self.under_any += o.under_any;
        self
    }
}

/// Black levels in frame coordinates: a repeating pattern plus optional per-row and
/// per-column deltas (DNG BlackLevelDeltaV/H).
#[derive(Debug, Clone)]
pub struct BlackModel {
    pub rows: usize,
    pub cols: usize,
    pub values: Vec<f32>,
    pub row_deltas: Vec<f32>,
    pub col_deltas: Vec<f32>,
}

impl BlackModel {
    pub fn at(&self, x: usize, y: usize) -> f32 {
        let v = self.values.get((y % self.rows.max(1)) * self.cols.max(1) + x % self.cols.max(1)).copied().unwrap_or(0.0);
        v + self.row_deltas.get(y).copied().unwrap_or(0.0) + self.col_deltas.get(x).copied().unwrap_or(0.0)
    }
    /// Constant within each 2x2 block position (no deltas, a period dividing 2).
    fn periodic2(&self) -> bool {
        self.row_deltas.is_empty() && self.col_deltas.is_empty() && self.rows <= 2 && self.cols <= 2 && (self.rows == 0 || 2 % self.rows == 0) && (self.cols == 0 || 2 % self.cols == 0)
    }
}

/// Bayer sensor data as decoded, with what developing it needs: what full-resolution
/// views of part of the frame are made from.
pub struct Bayer {
    pub data: Vec<u16>,
    pub stride: usize,
    pub rows: usize,
    /// Visible area: x, y, width, height.
    pub crop: [usize; 4],
    /// Channel of each 2x2 position, from the crop's top-left corner.
    pub ch: [[usize; 2]; 2],
    pub black: BlackModel,
    pub white: f32,
    pub white_exact: bool,
    pub render_white: Option<f32>,
    pub as_shot: Option<[f32; 3]>,
    pub daylight: Option<[f32; 3]>,
    pub cam_to_srgb: [[f32; 3]; 3],
    pub baseline_ev: f32,
    pub decoder: &'static str,
    pub decode_ms: f32,
}

impl Bayer {
    pub fn bytes(&self) -> usize {
        self.data.len() * 2 + 4096
    }

    /// Black level of a sample, crop coordinates.
    #[inline]
    pub fn black_at(&self, x: usize, y: usize) -> f32 {
        self.black.at(self.crop[0] + x, self.crop[1] + y)
    }

    fn develop(&self, wb_mode: WbMode, under_ev: f32, t0: Instant) -> Result<Developed> {
        let [cx, cy, ..] = self.crop;
        let blk = [[self.black.at(cx, cy), self.black.at(cx + 1, cy)], [self.black.at(cx, cy + 1), self.black.at(cx + 1, cy + 1)]];
        let at = |x: usize, y: usize| self.black.at(x, y);
        let f = BayerFrame {
            data: &self.data,
            stride: self.stride,
            rows: self.rows,
            crop: self.crop,
            ch: self.ch,
            black: blk,
            black_at: if self.black.periodic2() { None } else { Some(&at) },
            white: self.white,
            white_exact: self.white_exact,
            render_white: self.render_white,
            as_shot: self.as_shot,
            daylight: self.daylight,
            cam_to_srgb: self.cam_to_srgb,
            baseline_ev: self.baseline_ev,
        };
        develop_bayer(&f, wb_mode, under_ev, t0, self.decode_ms, self.decoder)
    }
}

/// Bayer sensor data ready for half-size development, from FlashRaw or LibRaw.
pub struct BayerFrame<'a> {
    pub data: &'a [u16],
    /// Samples per row of `data`.
    pub stride: usize,
    pub rows: usize,
    /// Visible area: x, y, width, height.
    pub crop: [usize; 4],
    /// Channel (0 R, 1 G, 2 B) of each 2x2 position, from the crop's top-left corner.
    pub ch: [[usize; 2]; 2],
    /// Black level of each 2x2 position, from the crop's top-left corner.
    pub black: [[f32; 2]; 2],
    /// Per-sample black level (frame coordinates), when it varies beyond 2x2.
    pub black_at: Option<&'a (dyn Fn(usize, usize) -> f32 + Sync)>,
    pub white: f32,
    /// `white` is the sensor's actual saturation level from the maker note, so there is no
    /// need to look for a lower clipping plateau in the data.
    pub white_exact: bool,
    /// The level that renders as white when it is below saturation (Canon's
    /// NormalWhiteLevel); `None`: saturation itself renders white.
    pub render_white: Option<f32>,
    /// White balance multipliers (R, G, B), G = 1.
    pub as_shot: Option<[f32; 3]>,
    pub daylight: Option<[f32; 3]>,
    /// White-balanced camera RGB to linear sRGB.
    pub cam_to_srgb: [[f32; 3]; 3],
    pub baseline_ev: f32,
}

/// Develops a RAW file: FlashRaw in-process, LibRaw (in the helper process) when
/// FlashRaw cannot read it or `HYPERFOCAL_RAW_DECODER=libraw`.
pub fn develop_path(path: &Path, wb_mode: WbMode, under_ev: f32) -> Result<Developed> {
    Ok(develop_path_keep(path, wb_mode, under_ev, false)?.0)
}

/// As [`develop_path`], also returning the sensor data when `keep` (Bayer sensors).
pub fn develop_path_keep(path: &Path, wb_mode: WbMode, under_ev: f32, keep: bool) -> Result<(Developed, Option<Bayer>)> {
    if native::prefer_libraw() {
        return develop_libraw_keep(path, wb_mode, under_ev, keep);
    }
    match catch(|| -> Result<(Developed, Option<Bayer>)> { develop_keep(std::fs::read(path)?, wb_mode, under_ev, keep) }) {
        Ok(d) => Ok(d),
        Err(e) if native::LIBRAW => {
            develop_libraw_keep(path, wb_mode, under_ev, keep).map_err(|e2| anyhow!("FlashRaw: {e}; LibRaw: {e2:#}"))
        }
        Err(e) => Err(anyhow!(e)),
    }
}

/// The sensor data of a RAW file (Bayer sensors only), for full-resolution views.
pub fn bayer_path(path: &Path) -> Result<Bayer> {
    let t0 = Instant::now();
    let flash = if native::prefer_libraw() {
        Err("LibRaw is preferred".to_string())
    } else {
        catch(|| -> Result<Option<Bayer>> { Ok(flash_bayer(std::fs::read(path)?, t0)?.ok()) })
    };
    match flash {
        Ok(Some(b)) => Ok(b),
        _ if native::LIBRAW => match native::libraw_raw(path, WbMode::Asshot)? {
            LibrawRaw::Bayer(info, data) => Ok(libraw_bayer(info, data, t0)),
            LibrawRaw::Rgb16(..) => Err(anyhow!("full-resolution views need Bayer sensor data")),
        },
        Ok(None) => Err(anyhow!("full-resolution views need Bayer sensor data")),
        Err(e) => Err(anyhow!(e)),
    }
}

/// Develop a RAW file held in memory, with FlashRaw.
pub fn develop(data: Vec<u8>, wb_mode: WbMode, under_ev: f32) -> Result<Developed> {
    Ok(develop_keep(data, wb_mode, under_ev, false)?.0)
}

fn develop_keep(data: Vec<u8>, wb_mode: WbMode, under_ev: f32, keep: bool) -> Result<(Developed, Option<Bayer>)> {
    let t0 = Instant::now();
    match flash_bayer(data, t0)? {
        Ok(b) => {
            let d = b.develop(wb_mode, under_ev, t0)?;
            Ok((d, keep.then_some(b)))
        }
        Err((raw, baseline)) => Ok((develop_fallback(&raw, wb_mode, under_ev, baseline, t0)?, None)),
    }
}

/// FlashRaw's sensor data as a [`Bayer`] frame, or (for other sensors) the opened file
/// and its baseline exposure, to develop otherwise.
fn flash_bayer(data: Vec<u8>, t0: Instant) -> Result<std::result::Result<Bayer, (FlashRaw<'static>, f32)>> {
    let canon = super::canon::white_levels(&data).unwrap_or_default();
    let raw = FlashRaw::open(data).map_err(|e| anyhow!("{e}"))?;
    let baseline = raw.metadata().dng.as_ref().and_then(|d| d.baseline_exposure).unwrap_or(0.0) as f32;
    let mut frame = raw
        .decode(DecodeOptions { output: OutputFormat::RawU16, ..Default::default() })
        .map_err(|e| anyhow!("{e}"))?;
    let decode_ms = t0.elapsed().as_secs_f32() * 1000.0;

    let bayer = match (&frame.cfa, frame.as_u16()) {
        (Some(c), Some(_)) if c.is_bayer() && c.width() == 2 && c.height() == 2 && frame.cpp == 1 && !frame.white_balance_applied => Some(*c),
        _ => None,
    };
    let Some(cfa) = bayer else {
        drop(frame);
        return Ok(Err((raw, baseline)));
    };
    let crop = frame.crop;
    let (cx, cy) = (crop.x as usize, crop.y as usize);
    let mut ch = [[0usize; 2]; 2];
    for (dy, row) in ch.iter_mut().enumerate() {
        for (dx, c) in row.iter_mut().enumerate() {
            *c = chan(cfa.color_at(cx + dx, cy + dy)).ok_or_else(|| anyhow!("unsupported CFA colour"))?;
        }
    }
    let bl = &frame.black_level;
    let black = BlackModel {
        rows: bl.rows.max(1) as usize,
        cols: bl.cols.max(1) as usize,
        // The pattern of the first sample of each position (CFA data: one per pixel).
        values: (0..bl.rows.max(1) as usize).flat_map(|y| (0..bl.cols.max(1) as usize).map(move |x| (x, y))).map(|(x, y)| bl.values.get((y * bl.cols.max(1) as usize + x) * bl.samples.max(1) as usize).copied().unwrap_or(0.0)).collect(),
        row_deltas: bl.row_deltas.clone(),
        col_deltas: bl.col_deltas.clone(),
    };
    let blk = [[black.at(cx, cy), black.at(cx + 1, cy)], [black.at(cx, cy + 1), black.at(cx + 1, cy + 1)]];
    let as_shot = frame.white_balance.as_ref().map(|w| {
        let g = if w.coeffs[1] > 0.0 { w.coeffs[1] } else { 1.0 };
        [w.coeffs[0] / g, 1.0, w.coeffs[2] / g]
    });
    let (white, white_exact) = saturation(frame.white_level.values.first().copied().unwrap_or(65535).max(1) as f32, canon.specular.map(f32::from), &blk);
    let daylight = wb_from_matrix(&frame);
    let cam_to_srgb = flashraw::process::camera_to_srgb(&frame.color_matrices);
    let (stride, rows) = (frame.width as usize, frame.height as usize);
    let data = match std::mem::replace(&mut frame.data, flashraw::RawPixelData::U16(Vec::new())) {
        flashraw::RawPixelData::U16(v) => v,
        _ => return Err(anyhow!("unexpected sample format")),
    };
    Ok(Ok(Bayer {
        data,
        stride,
        rows,
        crop: [cx, cy, crop.width as usize, crop.height as usize],
        ch,
        black,
        white,
        white_exact,
        render_white: canon.normal.map(f32::from),
        as_shot,
        daylight,
        cam_to_srgb,
        baseline_ev: baseline,
        decoder: "FlashRaw",
        decode_ms,
    }))
}

fn libraw_bayer(info: native::BayerInfo, data: Vec<u16>, t0: Instant) -> Bayer {
    Bayer {
        data,
        stride: info.w,
        rows: info.h,
        crop: [0, 0, info.w, info.h],
        ch: info.ch.map(|r| r.map(|c| c as usize)),
        black: BlackModel { rows: info.black_rows.max(1), cols: info.black_cols.max(1), values: info.black, row_deltas: Vec::new(), col_deltas: Vec::new() },
        white: info.white,
        white_exact: info.white_exact,
        render_white: (info.render_white > 0.0).then_some(info.render_white),
        as_shot: info.as_shot,
        daylight: info.daylight,
        cam_to_srgb: info.cam_to_srgb,
        baseline_ev: info.baseline_ev,
        decoder: "LibRaw",
        decode_ms: t0.elapsed().as_secs_f32() * 1000.0,
    }
}

/// The white level to develop with: the maker note's own saturation level (Canon's
/// SpecularWhiteLevel, LibRaw's `linear_max`) when it is plausible, else the header's;
/// and whether it came from the maker note.
fn saturation(header: f32, vendor: Option<f32>, black: &[[f32; 2]; 2]) -> (f32, bool) {
    let blk = (black[0][0] + black[0][1] + black[1][0] + black[1][1]) / 4.0;
    match vendor {
        Some(v) if v <= header && v > blk + (header - blk) * 0.5 => (v, true),
        _ => (header, false),
    }
}

/// Develop with LibRaw (in the helper process).
pub fn develop_libraw(path: &Path, wb_mode: WbMode, under_ev: f32) -> Result<Developed> {
    Ok(develop_libraw_keep(path, wb_mode, under_ev, false)?.0)
}

fn develop_libraw_keep(path: &Path, wb_mode: WbMode, under_ev: f32, keep: bool) -> Result<(Developed, Option<Bayer>)> {
    let t0 = Instant::now();
    match native::libraw_raw(path, wb_mode)? {
        LibrawRaw::Bayer(info, data) => {
            let b = libraw_bayer(info, data, t0);
            let d = b.develop(wb_mode, under_ev, t0)?;
            Ok((d, keep.then_some(b)))
        }
        raw => Ok((develop_libraw_data(raw, wb_mode, under_ev, t0)?, None)),
    }
}

pub fn develop_libraw_data(raw: LibrawRaw, wb_mode: WbMode, under_ev: f32, t0: Instant) -> Result<Developed> {
    match raw {
        LibrawRaw::Bayer(info, data) => libraw_bayer(info, data, t0).develop(wb_mode, under_ev, t0),
        LibrawRaw::Rgb16(info, data) => {
            let decode_ms = t0.elapsed().as_secs_f32() * 1000.0;
            let rgb: Vec<u16> = data.par_iter().map(|v| f16_bits(*v as f32 / 65535.0)).collect();
            Ok(finish_processed(rgb, info.w, info.h, under_ev, info.baseline_ev, t0, decode_ms, "LibRaw"))
        }
    }
}

/// Where one channel clipped when that is below the nominal white level: the highest
/// pile of at least 0.03% of samples (min. 48) within 1% of the range that is at least
/// 4x denser than the values below it. A pile rather than the maximum, so a few hot
/// pixels or stray samples above the plateau cannot hide it; denser than its
/// neighbourhood, so the smooth tail of an unclipped highlight is not mistaken for one.
/// `None` when the channel reaches the nominal white level itself (then that is where it
/// clipped) or nothing piles up. Returns the (lowest, highest) value of the pile.
fn clip_plateau(hist: &[u32], maxv: u16, white: f32, black: f32, tol: f32) -> Option<(u16, u16)> {
    let total: u64 = hist.iter().map(|&n| n as u64).sum();
    let sum = |a: usize, b: usize| hist[a..=b].iter().map(|&n| n as u64).sum::<u64>();
    let top = maxv as usize;
    let near_white = ((white - tol).max(0.0) as usize).min(hist.len() - 1);
    if top >= near_white && sum(near_white, top) as f64 >= (total as f64 * 0.00002).max(3.0) {
        return None;
    }
    let need = (total as f64 * 0.0003).max(48.0) as u64;
    // Pulled ISOs clip around 80% of the nominal range; a pile lower than 60% is image content.
    let lo = (black + (white - black) * 0.6).max(0.0) as usize;
    // An exact plateau first (e.g. Canon's digital pull), then one spread over 1%.
    let (mut low, mut high) = find_pile(hist, top, lo, tol.ceil().max(1.0) as usize, need)
        .or_else(|| find_pile(hist, top, lo, ((white - black) * 0.01).max(tol).ceil() as usize, need))?;
    // Grow to the whole plateau: neighbouring values at >= 5% of its peak count.
    let peak = hist[low as usize..=high as usize].iter().copied().max().unwrap_or(0) as u64;
    while (high as usize) < top && hist[high as usize + 1] as u64 * 20 >= peak {
        high += 1;
    }
    while low as usize > lo && hist[low as usize - 1] as u64 * 20 >= peak {
        low -= 1;
    }
    Some((low, high))
}

/// Highest window [v - w, v] (v from `top` down to `lo`) holding at least `need`
/// samples at 4x the density of the 8w values below it; (lowest, highest) of its pile.
fn find_pile(hist: &[u32], top: usize, lo: usize, w: usize, need: u64) -> Option<(u16, u16)> {
    let below = w * 8;
    let lo = lo.max(below + w);
    if top < lo {
        return None;
    }
    let sum = |a: usize, b: usize| hist[a..=b].iter().map(|&n| n as u64).sum::<u64>();
    let mut pile = sum(top - w, top);
    let mut v = top;
    loop {
        if pile >= need {
            let under = sum(v - w - below, v - w - 1);
            if pile * below as u64 >= 4 * under * (w as u64 + 1) {
                let win = v - w..=v;
                let peak = win.clone().map(|x| hist[x]).max().unwrap_or(0);
                let high = win.clone().rev().find(|&x| hist[x] > 0).unwrap_or(v);
                let low = win.clone().find(|&x| hist[x] as u64 * 20 >= peak as u64).unwrap_or(high);
                return Some((low as u16, high as u16));
            }
        }
        if v == lo {
            return None;
        }
        pile -= hist[v] as u64;
        v -= 1;
        pile += hist[v - w] as u64;
    }
}

/// One fused pass over a Bayer frame: 2x2 binning, raw clipping, histogram, white
/// balance and the camera matrix.
fn develop_bayer(f: &BayerFrame, wb_mode: WbMode, under_ev: f32, t0: Instant, decode_ms: f32, decoder: &'static str) -> Result<Developed> {
    let data = f.data;
    let fw = f.stride;
    let [cx, cy, crop_w, crop_h] = f.crop;
    let hw = crop_w / 2;
    let hh = crop_h / 2;
    if hw == 0 || hh == 0 || (cy + hh * 2) > f.rows || (cx + hw * 2) > fw || data.len() < fw * f.rows {
        return Err(anyhow!("invalid crop"));
    }
    let ch = f.ch;
    if ch.iter().flatten().any(|c| *c > 2) {
        return Err(anyhow!("unsupported CFA colour"));
    }
    let blk = f.black;
    let blk_mean = (blk[0][0] + blk[0][1] + blk[1][0] + blk[1][1]) / 4.0;
    let white = f.white.max(1.0);
    let tol = ((white - blk_mean) * 0.002).max(2.0);
    // Saturation detection: some white levels are nominal (Canon's pulled ISOs such as
    // 160/320/640/1250 clip ~1/3 EV below it); find where samples actually pile up.
    // Every 4th row of blocks, in a few bands in parallel (integer counts: the same
    // result whatever the split).
    let sampled: Vec<usize> = (0..hh).step_by(4).collect();
    let bands = rayon::current_num_threads().clamp(1, 8);
    let hist = sampled
        .par_chunks(sampled.len().div_ceil(bands).max(1))
        .map(|rows| {
            let mut hist = [vec![0u32; 65536], vec![0u32; 65536], vec![0u32; 65536]];
            for &by in rows {
                for dy in 0..2 {
                    let row = &data[(cy + by * 2 + dy) * fw + cx..][..hw * 2];
                    let (c0, c1) = (ch[dy][0], ch[dy][1]);
                    for pair in row.chunks_exact(2) {
                        hist[c0][pair[0] as usize] += 1;
                        hist[c1][pair[1] as usize] += 1;
                    }
                }
            }
            hist
        })
        .reduce_with(|mut a, b| {
            for c in 0..3 {
                for (x, y) in a[c].iter_mut().zip(&b[c]) {
                    *x += *y;
                }
            }
            a
        })
        .unwrap_or_else(|| [vec![0u32; 65536], vec![0u32; 65536], vec![0u32; 65536]]);
    let maxv = [0, 1, 2].map(|c| hist[c].iter().rposition(|&n| n > 0).unwrap_or(0) as u16);
    let margin = 2f32.powf(-OE_MARGIN_EV);
    let mut sat = [white; 3];
    let mut thr = [blk_mean + (white - blk_mean) * margin; 3];
    for c in 0..3 {
        if !f.white_exact
            && let Some((low, high)) = clip_plateau(&hist[c], maxv[c], white, blk_mean, tol)
            && (high as f32) < white - tol
        {
            sat[c] = high as f32;
            thr[c] = (low as f32).min(blk_mean + (sat[c] - blk_mean) * margin);
        }
    }
    let inv: [f32; 3] = [1.0 / (sat[0] - blk_mean).max(1.0), 1.0 / (sat[1] - blk_mean).max(1.0), 1.0 / (sat[2] - blk_mean).max(1.0)];
    // Rendering scale: Canon's own rendering and FastRawViewer map NormalWhiteLevel to white
    // (FastRawViewer's linear mode is exactly that signal at gamma 2.2); values between it
    // and saturation render white. Clipping, histogram and underexposure stay relative
    // to saturation.
    let rscale: [f32; 3] = match f.render_white {
        Some(w) if w <= white && w > blk_mean + (white - blk_mean) * 0.5 => [0, 1, 2].map(|c| (sat[c] - blk_mean) / (w - blk_mean)),
        _ => [1.0; 3],
    };

    // White balance.
    let valid = |w: &[f32; 3]| w.iter().all(|v| v.is_finite() && *v > 0.0);
    let as_shot = f.as_shot.filter(valid).unwrap_or([2.0, 1.0, 1.5]);
    let mut wb = match wb_mode {
        WbMode::Asshot => as_shot,
        WbMode::Daylight => f.daylight.filter(valid).unwrap_or(as_shot),
        WbMode::Auto => as_shot,
    };
    if wb_mode == WbMode::Auto {
        // Refine the as-shot balance over near-neutral mid-tones: every 3rd block of every
        // 3rd row, in parallel; integer sums of the raw values per position, so the
        // result does not depend on how the work was split.
        let blocks: Vec<usize> = (0..hh).step_by(3).collect();
        let sums = blocks
            .par_chunks(16)
            .map(|rows| {
                let mut sum = [[0u64; 2]; 2];
                let mut n = 0u64;
                for &by in rows {
                    let r0 = &data[(cy + by * 2) * fw + cx..][..hw * 2];
                    let r1 = &data[(cy + by * 2 + 1) * fw + cx..][..hw * 2];
                    for bx in (0..hw).step_by(3) {
                        let s = [[r0[bx * 2], r0[bx * 2 + 1]], [r1[bx * 2], r1[bx * 2 + 1]]];
                        let mut v = [0f32; 3];
                        for dy in 0..2 {
                            for dx in 0..2 {
                                v[ch[dy][dx]] += (s[dy][dx] as f32 - blk[dy][dx]) * inv[ch[dy][dx]];
                            }
                        }
                        v[1] *= 0.5;
                        let (r, g, b) = (v[0] * wb[0], v[1], v[2] * wb[2]);
                        if g > 0.03 && g < 0.85 && r < 0.95 && b < 0.95 && (r / g - 1.0).abs() < 0.3 && (b / g - 1.0).abs() < 0.3 {
                            for dy in 0..2 {
                                for dx in 0..2 {
                                    sum[dy][dx] += s[dy][dx] as u64;
                                }
                            }
                            n += 1;
                        }
                    }
                }
                (sum, n)
            })
            .reduce(
                || ([[0u64; 2]; 2], 0u64),
                |(mut a, an), (b, bn)| {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            a[dy][dx] += b[dy][dx];
                        }
                    }
                    (a, an + bn)
                },
            );
        let (raw, n) = sums;
        let mut sum = [0f64; 3];
        for dy in 0..2 {
            for dx in 0..2 {
                let c = ch[dy][dx];
                let k = if c == 1 { 0.5 } else { 1.0 };
                sum[c] += (raw[dy][dx] as f64 - n as f64 * blk[dy][dx] as f64) * inv[c] as f64 * k;
            }
        }
        if n > 200 && sum[0] > 0.0 && sum[2] > 0.0 {
            let r = (sum[1] / sum[0]) as f32;
            let b = (sum[1] / sum[2]) as f32;
            wb = [r.clamp(as_shot[0] / 2.0, as_shot[0] * 2.0), 1.0, b.clamp(as_shot[2] / 2.0, as_shot[2] * 2.0)];
        }
    }

    let m = f.cam_to_srgb;
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

    let kernel = match f.black_at {
        None => BlockKernel::new(ch, blk, inv, thr, under_thr, [rscale[0] * wb[0], rscale[1], rscale[2] * wb[2]], m, &bin_lut),
        Some(_) => None,
    };
    if let Some(k) = kernel {
        // Every element is written, so the buffers are not zero-filled first (42 MB
        // for 24 megapixels).
        let mut rgb: Vec<u16> = Vec::with_capacity(hw * hh * 3);
        let mut mask: Vec<u8> = Vec::with_capacity(hw * hh);
        let st = rgb.spare_capacity_mut()[..hw * hh * 3]
            .par_chunks_mut(hw * 3)
            .zip(mask.spare_capacity_mut()[..hw * hh].par_chunks_mut(hw))
            .enumerate()
            .fold(BlockStats::new, |mut st, (by, (rgb_row, mask_row))| {
                let y0 = cy + by * 2;
                let r0 = &data[y0 * fw + cx..][..hw * 2];
                let r1 = &data[(y0 + 1) * fw + cx..][..hw * 2];
                block_row(&k, r0, r1, rgb_row, mask_row, &mut st);
                st
            })
            .reduce(BlockStats::new, BlockStats::merge);
        // SAFETY: block_row wrote every element of every row.
        unsafe {
            rgb.set_len(hw * hh * 3);
            mask.set_len(hw * hh);
        }
        let acc = st.into_acc(&k);
        return Ok(finish(f, hw, hh, rgb, mask, acc, wb, inv, rscale, m, t0, decode_ms, decoder));
    }
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
                        let b = match f.black_at {
                            None => blk[dy][dx],
                            Some(at) => at(x0 + dx, y0 + dy),
                        };
                        let n = (s as f32 - b) * inv[c];
                        v[c] += n;
                        if s as f32 >= thr[c] {
                            clip |= 1 << c;
                            acc.clipped[c] += 1;
                        }
                        if n < under_thr {
                            acc.under[c] += 1;
                        }
                        acc.samples[c] += 1;
                        let bin = bin_lut.get(s as usize).map(|b| b[c]).unwrap_or((HIST_BINS - 1) as u8);
                        acc.hist[c][bin as usize] += 1;
                    }
                }
                v[1] *= 0.5;
                for (c, &x) in v.iter().enumerate() {
                    if x < under_thr {
                        clip |= 1 << (MASK_UNDER_SHIFT as usize + c);
                    }
                }
                if clip & 7 != 0 {
                    acc.any += 1;
                }
                if clip >> MASK_UNDER_SHIFT != 0 {
                    acc.under_any += 1;
                }
                let r = (v[0] * rscale[0] * wb[0]).clamp(0.0, 1.0);
                let g = (v[1] * rscale[1]).clamp(0.0, 1.0);
                let b = (v[2] * rscale[2] * wb[2]).clamp(0.0, 1.0);
                let o = bx * 3;
                rgb_row[o] = f16_bits(m[0][0] * r + m[0][1] * g + m[0][2] * b);
                rgb_row[o + 1] = f16_bits(m[1][0] * r + m[1][1] * g + m[1][2] * b);
                rgb_row[o + 2] = f16_bits(m[2][0] * r + m[2][1] * g + m[2][2] * b);
                mask_row[bx] = clip;
            }
            acc
        })
        .reduce(Acc::new, Acc::merge);
    Ok(finish(f, hw, hh, rgb, mask, acc, wb, inv, rscale, m, t0, decode_ms, decoder))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    f: &BayerFrame,
    hw: usize,
    hh: usize,
    rgb: Vec<u16>,
    mask: Vec<u8>,
    acc: Acc,
    wb: [f32; 3],
    inv: [f32; 3],
    rscale: [f32; 3],
    m: [[f32; 3]; 3],
    t0: Instant,
    decode_ms: f32,
    decoder: &'static str,
) -> Developed {
    let px = (hw * hh) as f32;
    let over = [
        acc.clipped[0] as f32 / acc.samples[0].max(1) as f32,
        acc.clipped[1] as f32 / acc.samples[1].max(1) as f32,
        acc.clipped[2] as f32 / acc.samples[2].max(1) as f32,
        acc.any as f32 / px,
    ];
    Developed {
        w: hw as u32,
        h: hh as u32,
        rgb,
        mask,
        hist: acc.hist,
        over,
        under: [
            acc.under[0] as f32 / acc.samples[0].max(1) as f32,
            acc.under[1] as f32 / acc.samples[1].max(1) as f32,
            acc.under[2] as f32 / acc.samples[2].max(1) as f32,
            acc.under_any as f32 / px,
        ],
        wb,
        baseline_ev: f.baseline_ev,
        decode_ms,
        develop_ms: t0.elapsed().as_secs_f32() * 1000.0 - decode_ms,
        approximate: false,
        decoder,
        inv,
        rscale,
        cam_to_srgb: m,
        full: Some(((hw * 2) as u32, (hh * 2) as u32)),
        mask_png: OnceLock::new(),
    }
}

/// The develop pass for frames whose black level is constant per position of the 2x2
/// pattern and whose pattern is one red, two greens and one blue (nearly every Bayer
/// sensor): fixed-width loops over chunks of a row that the compiler vectorises
/// (`simd`), and a value-to-bin table per position for the RAW histogram. Its output
/// is bit-identical to the per-sample path.
struct BlockKernel {
    /// Channel of each position (row-major: top-left, top-right, bottom-left,
    /// bottom-right), and the positions of red, the two greens and blue.
    c: [usize; 4],
    pos: [usize; 4],
    black: [f32; 4],
    /// Normalisation to saturation per position (not halved for the greens).
    inv: [f32; 4],
    clip: [f32; 4],
    under: f32,
    gain: [f32; 3],
    m: [[f32; 3]; 3],
    /// Histogram bin of every raw value, per position.
    bins: Vec<u8>,
}

/// Histogram and counters of a run of rows (per position; two copies of the
/// histogram, for alternate blocks, so repeated values do not serialise the counts).
struct BlockStats {
    hist: Box<[[u32; 256]; 8]>,
    clipped: [u64; 4],
    under: [u64; 4],
    blocks: u64,
    any: u64,
    under_any: u64,
}

const CHUNK: usize = 64;

impl BlockKernel {
    #[allow(clippy::too_many_arguments)]
    fn new(ch: [[usize; 2]; 2], blk: [[f32; 2]; 2], inv: [f32; 3], thr: [f32; 3], under: f32, gain: [f32; 3], m: [[f32; 3]; 3], bin_lut: &[[u8; 3]]) -> Option<Self> {
        let c = [ch[0][0], ch[0][1], ch[1][0], ch[1][1]];
        let find = |ch: usize, from: usize| (from..4).find(|&p| c[p] == ch);
        let r = find(0, 0)?;
        let g1 = find(1, 0)?;
        let g2 = find(1, g1 + 1)?;
        let b = find(2, 0)?;
        if c.iter().filter(|&&x| x == 0).count() != 1 || c.iter().filter(|&&x| x == 2).count() != 1 {
            return None;
        }
        let last = (HIST_BINS - 1) as u8;
        let mut bins = vec![last; 4 * 65536];
        for (p, &cp) in c.iter().enumerate() {
            for (s, b) in bin_lut.iter().enumerate() {
                bins[p * 65536 + s] = b[cp];
            }
        }
        Some(BlockKernel {
            c,
            pos: [r, g1, g2, b],
            black: [blk[0][0], blk[0][1], blk[1][0], blk[1][1]],
            inv: c.map(|c| inv[c]),
            clip: c.map(|c| thr[c]),
            under,
            gain,
            m,
            bins,
        })
    }
}

impl BlockStats {
    fn new() -> Self {
        BlockStats { hist: Box::new([[0; 256]; 8]), clipped: [0; 4], under: [0; 4], blocks: 0, any: 0, under_any: 0 }
    }

    fn merge(mut self, o: BlockStats) -> BlockStats {
        for (a, b) in self.hist.iter_mut().zip(o.hist.iter()) {
            for (x, y) in a.iter_mut().zip(b) {
                *x += *y;
            }
        }
        for p in 0..4 {
            self.clipped[p] += o.clipped[p];
            self.under[p] += o.under[p];
        }
        self.blocks += o.blocks;
        self.any += o.any;
        self.under_any += o.under_any;
        self
    }

    fn into_acc(self, k: &BlockKernel) -> Acc {
        let mut acc = Acc::new();
        for p in 0..4 {
            let c = k.c[p];
            for bin in 0..HIST_BINS {
                acc.hist[c][bin] += self.hist[p * 2][bin] + self.hist[p * 2 + 1][bin];
            }
            acc.clipped[c] += self.clipped[p];
            acc.under[c] += self.under[p];
            acc.samples[c] += self.blocks;
        }
        acc.any = self.any;
        acc.under_any = self.under_any;
        acc
    }
}

simd::dispatch!(fn block_row = block_row_imp(k: &BlockKernel, r0: &[u16], r1: &[u16], rgb: &mut [MaybeUninit<u16>], mask: &mut [MaybeUninit<u8>], st: &mut BlockStats));

/// One row of blocks: `r0` and `r1` are the two sensor rows (two samples per block).
/// Writes every element of `rgb` (3 per block) and `mask` (1 per block).
#[inline(always)]
fn block_row_imp(k: &BlockKernel, r0: &[u16], r1: &[u16], rgb: &mut [MaybeUninit<u16>], mask: &mut [MaybeUninit<u8>], st: &mut BlockStats) {
    let n = mask.len();
    let (r0, r1, rgb) = (&r0[..2 * n], &r1[..2 * n], &mut rgb[..3 * n]);
    let [pr, pg1, pg2, pb] = k.pos;
    let (b, iv, t, u) = (k.black, k.inv, k.clip, k.under);
    // Per-channel scale of each sample: the greens are averaged.
    let (kr, kg1, kg2, kb) = (iv[pr], iv[pg1] * 0.5, iv[pg2] * 0.5, iv[pb]);
    let (br, bg1, bg2, bb) = (b[pr], b[pg1], b[pg2], b[pb]);
    let bit = k.c.map(|c| 1u8 << c);
    let [g0, g1, g2] = k.gain;
    let m = k.m;
    let bins: [&[u8; 65536]; 4] = std::array::from_fn(|p| k.bins[p * 65536..(p + 1) * 65536].try_into().unwrap());
    let mut start = 0;
    while start < n {
        let len = CHUNK.min(n - start);
        let (a, z) = (&r0[2 * start..2 * (start + len)], &r1[2 * start..2 * (start + len)]);
        // The four positions as planes (lanes past `len` stay 0 and are not used).
        let mut s = [[0f32; CHUNK]; 4];
        for (i, (p, q)) in a.chunks_exact(2).zip(z.chunks_exact(2)).enumerate() {
            s[0][i] = p[0] as f32;
            s[1][i] = p[1] as f32;
            s[2][i] = q[0] as f32;
            s[3][i] = q[1] as f32;
        }
        let (sr, sg1, sg2, sb) = (&s[pr], &s[pg1], &s[pg2], &s[pb]);
        let mut o = [[0u16; CHUNK]; 3];
        let mut low = [0u8; CHUNK];
        for i in 0..CHUNK {
            let v0 = (sr[i] - br) * kr;
            let v1 = (sg1[i] - bg1) * kg1 + (sg2[i] - bg2) * kg2;
            let v2 = (sb[i] - bb) * kb;
            let r = (v0 * g0).clamp(0.0, 1.0);
            let g = (v1 * g1).clamp(0.0, 1.0);
            let bl = (v2 * g2).clamp(0.0, 1.0);
            o[0][i] = simd::f16_bits(m[0][0] * r + m[0][1] * g + m[0][2] * bl);
            o[1][i] = simd::f16_bits(m[1][0] * r + m[1][1] * g + m[1][2] * bl);
            o[2][i] = simd::f16_bits(m[2][0] * r + m[2][1] * g + m[2][2] * bl);
            low[i] = (if v0 < u { 1u8 } else { 0 }) | (if v1 < u { 2 } else { 0 }) | (if v2 < u { 4 } else { 0 });
        }
        // Clipping per sample (flags and counts) and underexposure counts.
        let mut clip = [0u8; CHUNK];
        for p in 0..4 {
            let (sp, bp, ip, tp, bitp) = (&s[p][..len], b[p], iv[p], t[p], bit[p]);
            let (mut nc, mut nu) = (0u32, 0u32);
            for (i, &x) in sp.iter().enumerate() {
                let over = x >= tp;
                clip[i] |= if over { bitp } else { 0 };
                nc += over as u32;
                nu += ((x - bp) * ip < u) as u32;
            }
            st.clipped[p] += nc as u64;
            st.under[p] += nu as u64;
        }
        for (px, i) in rgb[3 * start..3 * (start + len)].chunks_exact_mut(3).zip(0..) {
            px[0].write(o[0][i]);
            px[1].write(o[1][i]);
            px[2].write(o[2][i]);
        }
        let (mut any, mut under_any) = (0u32, 0u32);
        for (d, (&c, &l)) in mask[start..start + len].iter_mut().zip(clip.iter().zip(&low)) {
            d.write(c | (l << MASK_UNDER_SHIFT));
            any += (c != 0) as u32;
            under_any += (l != 0) as u32;
        }
        st.any += any as u64;
        st.under_any += under_any as u64;
        // The RAW histogram: a table lookup and a count per sample.
        let h = &mut st.hist;
        for (i, (p, q)) in a.chunks_exact(2).zip(z.chunks_exact(2)).enumerate() {
            let c = i & 1;
            h[c][bins[0][p[0] as usize] as usize] += 1;
            h[2 + c][bins[1][p[1] as usize] as usize] += 1;
            h[4 + c][bins[2][q[0] as usize] as usize] += 1;
            h[6 + c][bins[3][q[1] as usize] as usize] += 1;
        }
        start += len;
    }
    st.blocks += n as u64;
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
    let (w, h) = (frame.width as usize, frame.height as usize);
    if w == 0 || h == 0 || bits.len() < w * h * 3 {
        return Err(anyhow!("unexpected output"));
    }
    Ok(finish_processed(bits[..w * h * 3].to_vec(), w, h, under_ev, baseline, t0, decode_ms, "FlashRaw"))
}

/// An image a decoder developed itself (linear sRGB, f16 bits): approximate clipping
/// and histogram from the processed values.
#[allow(clippy::too_many_arguments)]
fn finish_processed(mut rgb: Vec<u16>, mut w: usize, mut h: usize, under_ev: f32, baseline: f32, t0: Instant, decode_ms: f32, decoder: &'static str) -> Developed {
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
    let mut under = [0u64; 3];
    let mut under_any = 0u64;
    for i in 0..w * h {
        let mut clip = 0u8;
        for c in 0..3 {
            let v = half::f16::from_bits(rgb[i * 3 + c]).to_f32();
            if v >= 0.998 {
                clip |= 1 << c;
                clipped[c] += 1;
            }
            if v < under_thr {
                clip |= 1 << (MASK_UNDER_SHIFT as usize + c);
                under[c] += 1;
            }
            let bin = if v <= 0.0 { 0.0 } else { ((v.log2() - HIST_MIN_EV) * HIST_PER_EV).clamp(0.0, (HIST_BINS - 1) as f32) };
            hist[c][bin as usize] += 1;
        }
        if clip & 7 != 0 {
            any += 1;
        }
        if clip >> MASK_UNDER_SHIFT != 0 {
            under_any += 1;
        }
        mask[i] = clip;
    }
    let px = (w * h).max(1) as f32;
    Developed {
        w: w as u32,
        h: h as u32,
        rgb,
        mask,
        hist,
        over: [clipped[0] as f32 / px, clipped[1] as f32 / px, clipped[2] as f32 / px, any as f32 / px],
        under: [under[0] as f32 / px, under[1] as f32 / px, under[2] as f32 / px, under_any as f32 / px],
        wb: [1.0, 1.0, 1.0],
        baseline_ev: baseline,
        decode_ms,
        develop_ms: t0.elapsed().as_secs_f32() * 1000.0 - decode_ms,
        approximate: true,
        decoder,
        inv: [1.0; 3],
        rscale: [1.0; 3],
        cam_to_srgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        full: None,
        mask_png: OnceLock::new(),
    }
}

/// Full-resolution tile edge (pixels), and the margin demosaicing reads around a tile
/// (even, so the colour filter pattern lines up).
pub const TILE: usize = 512;
/// Tiles are rendered with this many extra pixels on each side (from the neighbouring
/// tiles, mirrored at the frame's edges), so the viewer can filter across tile seams.
pub const TILE_APRON: usize = 1;
/// Samples around a tile's demosaic input (RCD reads 4 each side; the rest is slack).
/// With the apron, an even number, so sample (0, 0) of the input keeps the colour of
/// the frame's position (0, 0).
pub const TILE_MARGIN: usize = 11;

/// The tile grid of the full-resolution view: tile (tx, ty) covers crop pixels from
/// (tx, ty) * TILE, at most TILE on each side.
pub fn tile_rect(dev: &Developed, tx: usize, ty: usize) -> Result<[usize; 4]> {
    let (fw, fh) = dev.full.map(|(w, h)| (w as usize, h as usize)).ok_or_else(|| anyhow!("this RAW file has no full-resolution view"))?;
    let (x, y) = (tx.saturating_mul(TILE), ty.saturating_mul(TILE));
    if x >= fw || y >= fh {
        return Err(anyhow!("no such tile"));
    }
    Ok([x, y, TILE.min(fw - x), TILE.min(fh - y)])
}

/// The demosaic input of a tile with its apron: the samples of `rect` grown by
/// TILE_APRON + TILE_MARGIN on each side (mirrored at the frame's edges, keeping the
/// colour of each position), black-subtracted and scaled as `dev` was developed, white
/// balance applied, clipped to 0..1. Returns (samples, width, height); sample (0, 0)
/// is at frame (x - TILE_APRON - TILE_MARGIN, y - same), so its colour is
/// `b.ch[row & 1][col & 1]` (the offset is even).
pub fn tile_cfa(b: &Bayer, dev: &Developed, rect: [usize; 4]) -> (Vec<f32>, usize, usize) {
    let (fw, fh) = dev.full.map(|(w, h)| (w as usize, h as usize)).unwrap_or((0, 0));
    let [x0, y0, w, h] = rect;
    const PAD: usize = TILE_APRON + TILE_MARGIN;
    const _: () = assert!(PAD.is_multiple_of(2), "the pad must keep the colour of each position");
    let (bw, bh) = (w + 2 * PAD, h + 2 * PAD);
    let mirror = |v: isize, n: usize| -> usize {
        let n = n as isize;
        let v = if v < 0 { -v } else if v >= n { 2 * (n - 1) - v } else { v };
        v.clamp(0, n - 1) as usize
    };
    let mul: [f32; 3] = [0, 1, 2].map(|c| dev.inv[c] * dev.rscale[c] * if c == 1 { 1.0 } else { dev.wb[c] });
    let mut cfa = vec![0f32; bw * bh];
    cfa.par_chunks_mut(bw).enumerate().for_each(|(j, row)| {
        let y = mirror(y0 as isize + j as isize - PAD as isize, fh);
        let line = &b.data[(b.crop[1] + y) * b.stride + b.crop[0]..];
        for (i, o) in row.iter_mut().enumerate() {
            let x = mirror(x0 as isize + i as isize - PAD as isize, fw);
            let c = b.ch[y & 1][x & 1];
            *o = ((line[x] as f32 - b.black_at(x, y)) * mul[c]).clamp(0.0, 1.0);
        }
    });
    (cfa, bw, bh)
}

/// Checks that a tile can be cut from `b` for `dev`, and returns its rectangle.
pub fn tile_check(b: &Bayer, dev: &Developed, tx: usize, ty: usize) -> Result<[usize; 4]> {
    let rect = tile_rect(dev, tx, ty)?;
    let (fw, fh) = dev.full.map(|(w, h)| (w as usize, h as usize)).unwrap_or((0, 0));
    if fw <= (TILE_MARGIN + TILE_APRON) * 2 || fh <= (TILE_MARGIN + TILE_APRON) * 2 || b.crop[2] < fw || b.crop[3] < fh {
        return Err(anyhow!("the sensor data does not match this development"));
    }
    Ok(rect)
}

/// A full-resolution tile with its apron ((w + 2 * TILE_APRON) x (h + 2 * TILE_APRON)),
/// demosaiced (RCD), coloured and tone-mapped as the half-size view of the same
/// development.
pub fn render_tile(b: &Bayer, dev: &Developed, lut: &[u8], tx: usize, ty: usize) -> Result<super::jpeg::Rgb> {
    let rect = tile_check(b, dev, tx, ty)?;
    let (cfa, bw, bh) = tile_cfa(b, dev, rect);
    let ch = b.ch;
    let rgb = super::demosaic::rcd(&cfa, bw, bh, |r, c| ch[r & 1][c & 1]);
    let (w, h) = (rect[2] + 2 * TILE_APRON, rect[3] + 2 * TILE_APRON);
    let m = dev.cam_to_srgb;
    let mut px = vec![0u8; w * h * 3];
    px.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let base = (y + TILE_MARGIN) * bw + TILE_MARGIN;
        for x in 0..w {
            let i = base + x;
            let (r, g, bl) = (rgb[0][i], rgb[1][i], rgb[2][i]);
            for k in 0..3 {
                row[x * 3 + k] = lut[f16_bits(m[k][0] * r + m[k][1] * g + m[k][2] * bl) as usize];
            }
        }
    });
    Ok(super::jpeg::Rgb { w, h, px })
}

/// The demosaic input of a tile for the viewer's GPU path: a 64-byte header ("HFC1";
/// u16 LE width, height, margin, apron, tile width, tile height; the colour of the four
/// positions of the 2x2 pattern; the camera-to-linear-sRGB matrix as 9 f32 LE; zeros)
/// then the samples (0..1) as u16 LE in 1/65535 steps, row by row: finer than any
/// sensor's own steps, unlike half floats near white. The GPU demosaics the samples and
/// writes (tile + 2 * apron) pixels starting `margin` samples in, coloured with the matrix.
pub fn cfa_tile(b: &Bayer, dev: &Developed, tx: usize, ty: usize) -> Result<Vec<u8>> {
    let rect = tile_check(b, dev, tx, ty)?;
    let (cfa, bw, bh) = tile_cfa(b, dev, rect);
    let mut out = Vec::with_capacity(64 + cfa.len() * 2);
    out.extend_from_slice(b"HFC1");
    for v in [bw, bh, TILE_MARGIN, TILE_APRON, rect[2], rect[3]] {
        out.extend_from_slice(&(v as u16).to_le_bytes());
    }
    out.extend_from_slice(&[b.ch[0][0] as u8, b.ch[0][1] as u8, b.ch[1][0] as u8, b.ch[1][1] as u8]);
    for row in dev.cam_to_srgb {
        for v in row {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.resize(64, 0);
    let mut bits = vec![0u8; cfa.len() * 2];
    bits.par_chunks_mut(2 * bw).zip(cfa.par_chunks(bw)).for_each(|(o, row)| {
        for (d, &v) in o.chunks_exact_mut(2).zip(row) {
            d.copy_from_slice(&((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_le_bytes());
        }
    });
    out.extend_from_slice(&bits);
    Ok(out)
}

/// Tone-map a developed image to 8-bit RGB, at most `max_long` pixels on the long edge
/// (downscaled by a whole factor: box filter in linear light).
pub fn render(dev: &Developed, lut: &[u8], max_long: usize) -> super::jpeg::Rgb {
    let (w, h) = (dev.w as usize, dev.h as usize);
    let long = w.max(h);
    let factor = if max_long > 0 && long >= max_long * 2 { (long / max_long).clamp(1, 4) } else { 1 };
    let (tw, th) = (w / factor, h / factor);
    let lut: &[u8; 65536] = lut.try_into().expect("a tone curve has 65536 entries");
    let mut px = vec![0u8; tw * th * 3];
    if factor == 1 {
        px.par_chunks_mut(tw * 3).enumerate().for_each(|(y, row)| {
            let src = &dev.rgb[y * w * 3..][..tw * 3];
            for (o, s) in row.iter_mut().zip(src) {
                *o = lut[*s as usize];
            }
        });
    } else {
        px.par_chunks_mut(tw * 3).enumerate().for_each_init(Vec::new, |buf, (y, row)| {
            buf.resize(tw * factor * 3 * 2, 0f32);
            downscale_row(&dev.rgb[y * factor * w * 3..][..factor * w * 3], w, factor, lut, row, buf);
        });
    }
    super::jpeg::Rgb { w: tw, h: th, px }
}

simd::dispatch!(fn downscale_row = downscale_row_imp(src: &[u16], w: usize, factor: usize, lut: &[u8; 65536], row: &mut [u8], buf: &mut [f32]));

/// One output row of a downscale by `factor`: `src` holds the `factor` source rows
/// (half floats, `w` pixels each). Sums the rows (the hardware half-float conversion
/// where there is one, then plain vector adds), then the pixels across, then encodes
/// and tone-maps.
#[inline(always)]
fn downscale_row_imp(src: &[u16], w: usize, factor: usize, lut: &[u8; 65536], row: &mut [u8], buf: &mut [f32]) {
    use half::slice::{HalfBitsSliceExt, HalfFloatSliceExt};
    let tw = row.len() / 3;
    let n = tw * factor * 3;
    let (sum, tmp) = buf.split_at_mut(n);
    let tmp = &mut tmp[..n];
    src[..n].reinterpret_cast::<half::f16>().convert_to_f32_slice(sum);
    for dy in 1..factor {
        src[dy * w * 3..][..n].reinterpret_cast::<half::f16>().convert_to_f32_slice(tmp);
        for (a, b) in sum.iter_mut().zip(tmp.iter()) {
            *a += *b;
        }
    }
    let inv = 1.0 / (factor * factor) as f32;
    let across = &mut tmp[..tw * 3];
    for (o, p) in across.chunks_exact_mut(3).zip(sum.chunks_exact(factor * 3)) {
        let mut acc = [p[0], p[1], p[2]];
        for q in p[3..].chunks_exact(3) {
            acc[0] += q[0];
            acc[1] += q[1];
            acc[2] += q[2];
        }
        o[0] = acc[0] * inv;
        o[1] = acc[1] * inv;
        o[2] = acc[2] * inv;
    }
    for (out, vals) in row.chunks_mut(64).zip(across.chunks(64)) {
        let mut bits = [0u16; 64];
        for (b, &v) in bits.iter_mut().zip(vals) {
            *b = simd::f16_bits(v);
        }
        for (o, &b) in out.iter_mut().zip(&bits) {
            *o = lut[b as usize];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist_of(samples: impl IntoIterator<Item = u16>) -> (Vec<u32>, u16) {
        let mut h = vec![0u32; 65536];
        let mut m = 0;
        for s in samples {
            h[s as usize] += 1;
            m = m.max(s);
        }
        (h, m)
    }

    /// Smooth, unclipped distribution up to ~12500, density falling towards the top.
    fn smooth() -> impl Iterator<Item = u16> {
        (0..200_000u32).map(|i| (12000.0 * ((i as f32 + 0.5) / 200_000.0).powi(2)) as u16 + 512)
    }

    /// Unclipped but densest right at its top end (an adversarial highlight).
    fn ramp() -> impl Iterator<Item = u16> {
        (0..200_000u32).map(|i| (12000.0 * ((i as f32 + 0.5) / 200_000.0).sqrt()) as u16 + 512)
    }

    const WHITE: f32 = 16383.0;
    const BLACK: f32 = 512.0;
    const TOL: f32 = 31.7;

    #[test]
    fn plateau_below_nominal_white_despite_stray_samples() {
        let (h, m) = hist_of(smooth().chain(std::iter::repeat_n(13050, 5000)).chain([16383, 15000]));
        assert_eq!(clip_plateau(&h, m, WHITE, BLACK, TOL), Some((13050, 13050)));
    }

    #[test]
    fn plateau_spread_over_many_values() {
        let (h, m) = hist_of(smooth().chain((0..9000).map(|i| 12990 + (i % 120) as u16)));
        assert_eq!(clip_plateau(&h, m, WHITE, BLACK, TOL), Some((12990, 13109)));
    }

    #[test]
    fn clipping_at_nominal_white_wins() {
        // A real pile at white (here a handful of samples) means the channel clipped there.
        let (h, m) = hist_of(smooth().chain(std::iter::repeat_n(13050, 5000)).chain(std::iter::repeat_n(16383, 10)));
        assert_eq!(clip_plateau(&h, m, WHITE, BLACK, TOL), None);
    }

    #[test]
    fn no_plateau_in_a_smooth_tail() {
        let (h, m) = hist_of(smooth().chain([16383]));
        assert_eq!(clip_plateau(&h, m, WHITE, BLACK, TOL), None);
        let (h, m) = hist_of(ramp().chain([16383]));
        assert_eq!(clip_plateau(&h, m, WHITE, BLACK, TOL), None);
    }

    #[test]
    fn too_few_samples_is_not_a_plateau() {
        let (h, m) = hist_of(smooth().chain(std::iter::repeat_n(13050, 20)));
        assert_eq!(clip_plateau(&h, m, WHITE, BLACK, TOL), None);
    }

    /// Canon R6 at ISO 1250: black 2048, the sensor saturates at 14888 (SpecularWhiteLevel)
    /// and renders white at 13535 (NormalWhiteLevel). Four bands of two rows.
    #[test]
    fn canon_levels_render_scale_and_exposure_bands() {
        let (w, h) = (64usize, 8usize);
        let bands: [u16; 4] = [2048 + 11487 / 2, 14500, 13700, 2100];
        let mut data = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = bands[y / 2];
            }
        }
        let f = BayerFrame {
            data: &data,
            stride: w,
            rows: h,
            crop: [0, 0, w, h],
            ch: [[0, 1], [1, 2]],
            black: [[2048.0; 2]; 2],
            black_at: None,
            white: 14888.0,
            white_exact: true,
            render_white: Some(13535.0),
            as_shot: Some([1.0, 1.0, 1.0]),
            daylight: None,
            cam_to_srgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            baseline_ev: 0.0,
        };
        let d = develop_bayer(&f, WbMode::Asshot, UE_EV, Instant::now(), 0.0, "test").unwrap();
        let hw = w / 2;
        let px = |band: usize| [0, 1, 2].map(|c| half::f16::from_bits(d.rgb[band * hw * 3 + c]).to_f32());
        // Half the normal-white range renders at half; between normal white and saturation renders white.
        assert!(px(0).iter().all(|v| (v - 0.5).abs() < 0.005), "{:?}", px(0));
        assert!(px(2).iter().all(|v| (v - 1.0).abs() < 1e-3), "{:?}", px(2));
        // 14500 is within 0.05 EV of saturation: clipped; 13700 is not. 2100 is 7.9 EV down: underexposed.
        assert_eq!(d.mask[hw] & 7, 7);
        assert_eq!(d.mask[2 * hw] & 7, 0);
        assert_eq!(d.mask[3 * hw] >> MASK_UNDER_SHIFT, 7);
        assert_eq!(d.mask[0], 0);
        for c in 0..3 {
            assert!((d.over[c] - 0.25).abs() < 1e-6 && (d.under[c] - 0.25).abs() < 1e-6, "{:?} {:?}", d.over, d.under);
        }
    }

    /// One channel underexposed: only its bit is set.
    #[test]
    fn underexposure_is_per_channel() {
        let (w, h) = (16usize, 4usize);
        // R and both greens well exposed, blue 9 EV below saturation.
        let mut data = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = if y % 2 == 1 && x % 2 == 1 { 512 + 30 } else { 8000 };
            }
        }
        let f = BayerFrame {
            data: &data,
            stride: w,
            rows: h,
            crop: [0, 0, w, h],
            ch: [[0, 1], [1, 2]],
            black: [[512.0; 2]; 2],
            black_at: None,
            white: 16383.0,
            white_exact: false,
            render_white: None,
            as_shot: Some([2.0, 1.0, 1.5]),
            daylight: None,
            cam_to_srgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            baseline_ev: 0.0,
        };
        let d = develop_bayer(&f, WbMode::Asshot, UE_EV, Instant::now(), 0.0, "test").unwrap();
        assert!(d.mask.iter().all(|&m| m == 1 << (MASK_UNDER_SHIFT + 2)), "{:?}", &d.mask[..4]);
        assert_eq!((d.under[0], d.under[1], d.under[2], d.under[3]), (0.0, 0.0, 1.0, 1.0));
    }

    /// A Bayer frame of a scene (RGGB, black 512, white 16383, raw response of a
    /// neutral R 0.5, G 1, B 2/3): left a neutral ramp, right a coloured gradient, fine
    /// vertical lines in the middle. The crop starts at an odd position (1, 1).
    fn scene(w: usize, h: usize) -> Bayer {
        let (cw, ch) = (w + 2, h + 2);
        let mut data = vec![0u16; cw * ch];
        for y in 0..ch {
            for x in 0..cw {
                let (fx, fy) = (x as f32 / cw as f32, y as f32 / ch as f32);
                let rgb = if x < cw / 3 {
                    [0.1 + 0.6 * fy; 3]
                } else if x < 2 * cw / 3 {
                    if x % 4 < 2 { [0.7; 3] } else { [0.15; 3] }
                } else {
                    [0.2 + 0.5 * fx, 0.3, 0.6 - 0.4 * fy]
                };
                // The crop's (0, 0) is a red sample: the frame's (1, 1).
                let c = [[2usize, 1], [1, 0]][y % 2][x % 2];
                let gain = [0.5, 1.0, 2.0 / 3.0][c];
                data[y * cw + x] = (512.0 + (16383.0 - 512.0) * rgb[c] * gain) as u16;
            }
        }
        Bayer {
            data,
            stride: cw,
            rows: ch,
            crop: [1, 1, w, h],
            ch: [[0, 1], [1, 2]],
            black: BlackModel { rows: 1, cols: 1, values: vec![512.0], row_deltas: vec![], col_deltas: vec![] },
            white: 16383.0,
            white_exact: true,
            render_white: None,
            as_shot: Some([2.0, 1.0, 1.5]),
            daylight: None,
            cam_to_srgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            baseline_ev: 0.0,
            decoder: "test",
            decode_ms: 0.0,
        }
    }

    #[test]
    fn full_resolution_tiles_match_the_half_size_view() {
        let (w, h) = (1100usize, 700usize);
        let b = scene(w, h);
        let d = b.develop(WbMode::Asshot, 8.0, Instant::now()).unwrap();
        assert_eq!(d.full, Some((1100, 700)));
        let lut = super::super::tone::build_lut(super::super::tone::ToneMode::Linear, 0.0);
        let half = render(&d, &lut, 0);
        // Tiles cover the frame: 3 x 2 of them, the last ones partial.
        assert!(tile_rect(&d, 2, 1).is_ok() && tile_rect(&d, 3, 0).is_err() && tile_rect(&d, 0, 2).is_err());
        assert_eq!(tile_rect(&d, 2, 1).unwrap(), [1024, 512, 76, 188]);
        let mut worst = 0i32;
        let a = TILE_APRON;
        let mut tiles = std::collections::HashMap::new();
        for (tx, ty) in [(0, 0), (1, 0), (2, 1)] {
            let t = render_tile(&b, &d, &lut, tx, ty).unwrap();
            let [x0, y0, tw, th] = tile_rect(&d, tx, ty).unwrap();
            assert_eq!((t.w, t.h), (tw + 2 * a, th + 2 * a));
            let at = |x: usize, y: usize, c: usize| t.px[((y + a) * t.w + x + a) * 3 + c] as i32;
            // Away from the fine lines, a 2x2 block of the tile looks like the half-size pixel.
            for y in (2..th - 2).step_by(2) {
                for x in (2..tw - 2).step_by(2) {
                    let (gx, gy) = (x0 + x, y0 + y);
                    if (w / 3 - 8..2 * w / 3 + 8).contains(&gx) {
                        continue;
                    }
                    let hp = &half.px[((gy / 2) * half.w + gx / 2) * 3..][..3];
                    for c in 0..3 {
                        let avg = (0..4).map(|k| at(x + k % 2, y + k / 2, c)).sum::<i32>() / 4;
                        worst = worst.max((avg - hp[c] as i32).abs());
                    }
                }
            }
            // Neutral stays neutral at full resolution (white balance applied before demosaicing).
            let p = [0, 1, 2].map(|c| at(10, th / 2, c));
            if x0 == 0 {
                assert!((p[0] - p[1]).abs() <= 2 && (p[2] - p[1]).abs() <= 2, "{p:?}");
            }
            tiles.insert((tx, ty), t);
        }
        assert!(worst <= 3, "full resolution differs from the half-size view by {worst}");
        // The apron repeats the neighbouring tile's edge pixels exactly (same demosaic).
        let (t0, t1) = (&tiles[&(0, 0)], &tiles[&(1, 0)]);
        for y in [a, 100, 300] {
            for c in 0..3 {
                // Tile 0's right apron column is tile 1's first column, and the reverse.
                assert!((t0.px[(y * t0.w + a + TILE) * 3 + c] as i32 - t1.px[(y * t1.w + a) * 3 + c] as i32).abs() <= 1);
                assert!((t1.px[(y * t1.w) * 3 + c] as i32 - t0.px[(y * t0.w + a + TILE - 1) * 3 + c] as i32).abs() <= 1);
            }
        }
        // The fine lines resolve at full resolution: 2 px light, 2 px dark.
        let t = &tiles[&(0, 0)];
        let row = (300 + a) * t.w + a;
        let xs: Vec<i32> = (380..392).map(|x| t.px[(row + x) * 3 + 1] as i32).collect();
        let (lo, hi) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap());
        assert!(hi - lo > 80, "lines not resolved: {xs:?}");
    }

    #[test]
    fn cfa_tile_layout() {
        let b = scene(1100, 700);
        let d = b.develop(WbMode::Asshot, 8.0, Instant::now()).unwrap();
        let bytes = cfa_tile(&b, &d, 2, 1).unwrap();
        let u16at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]) as usize;
        assert_eq!(&bytes[..4], b"HFC1");
        let (bw, bh, margin, apron, tw, th) = (u16at(4), u16at(6), u16at(8), u16at(10), u16at(12), u16at(14));
        assert_eq!((margin, apron, tw, th), (TILE_MARGIN, TILE_APRON, 76, 188));
        assert_eq!((bw, bh), (tw + 2 * (margin + apron), th + 2 * (margin + apron)));
        assert_eq!(&bytes[16..20], &[b.ch[0][0] as u8, b.ch[0][1] as u8, b.ch[1][0] as u8, b.ch[1][1] as u8]);
        let m00 = f32::from_le_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!(m00, d.cam_to_srgb[0][0]);
        assert_eq!(bytes.len(), 64 + bw * bh * 2);
        // The samples are the demosaic input in 1/65535 steps.
        let (cfa, w2, h2) = tile_cfa(&b, &d, tile_rect(&d, 2, 1).unwrap());
        assert_eq!((w2, h2), (bw, bh));
        for k in [0, 1, bw + 3, bw * bh / 2, bw * bh - 1] {
            let v = u16at(64 + 2 * k) as f32 / 65535.0;
            assert!((v - cfa[k]).abs() <= 0.5 / 65535.0 + 1e-7, "{k}: {v} vs {}", cfa[k]);
        }
    }

    #[test]
    fn fast_path_matches_per_sample_path() {
        let b = scene(300, 200);
        let fast = b.develop(WbMode::Auto, 8.0, Instant::now()).unwrap();
        // The same frame through the general path (a per-sample black level that is constant).
        let at = |_x: usize, _y: usize| 512.0;
        let f = BayerFrame {
            data: &b.data,
            stride: b.stride,
            rows: b.rows,
            crop: b.crop,
            ch: b.ch,
            black: [[512.0; 2]; 2],
            black_at: Some(&at),
            white: b.white,
            white_exact: b.white_exact,
            render_white: b.render_white,
            as_shot: b.as_shot,
            daylight: b.daylight,
            cam_to_srgb: b.cam_to_srgb,
            baseline_ev: 0.0,
        };
        let slow = develop_bayer(&f, WbMode::Auto, 8.0, Instant::now(), 0.0, "test").unwrap();
        assert_eq!(fast.wb, slow.wb);
        assert_eq!(fast.mask, slow.mask);
        assert_eq!(fast.hist, slow.hist);
        assert_eq!((fast.over, fast.under), (slow.over, slow.under));
        assert!(fast.rgb == slow.rgb);
    }

    #[test]
    fn downscaled_render_matches_a_direct_box_filter() {
        let b = scene(812, 406);
        let d = b.develop(WbMode::Asshot, 8.0, Instant::now()).unwrap();
        let lut = super::super::tone::build_lut(super::super::tone::ToneMode::Normal, 0.5);
        let (w, h) = (d.w as usize, d.h as usize);
        for (max_long, factor) in [(200, 2usize), (135, 3), (100, 4)] {
            let img = render(&d, &lut, max_long);
            assert_eq!((img.w, img.h), (w / factor, h / factor));
            let mut worst = 0;
            for y in 0..img.h {
                for x in 0..img.w {
                    for c in 0..3 {
                        let mut s = 0f32;
                        for dy in 0..factor {
                            for dx in 0..factor {
                                s += half::f16::from_bits(d.rgb[((y * factor + dy) * w + x * factor + dx) * 3 + c]).to_f32();
                            }
                        }
                        let want = lut[f16_bits(s / (factor * factor) as f32) as usize] as i32;
                        worst = worst.max((img.px[(y * img.w + x) * 3 + c] as i32 - want).abs());
                    }
                }
            }
            assert!(worst <= 1, "factor {factor}: off by {worst}");
        }
    }

    /// Timings on a 24-megapixel frame (run with --release --ignored --nocapture).
    #[test]
    #[ignore]
    fn bench_develop_render_tile() {
        let b = scene(6000, 4000);
        let t = Instant::now();
        let d = b.develop(WbMode::Asshot, 8.0, Instant::now()).unwrap();
        let dev_ms = t.elapsed().as_secs_f64() * 1e3;
        let lut = super::super::tone::build_lut(super::super::tone::ToneMode::Normal, 0.0);
        let t = Instant::now();
        let img = render(&d, &lut, 0);
        let render_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let _ = render(&d, &lut, 1024);
        let render_small_ms = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        let n = 8;
        for k in 0..n {
            render_tile(&b, &d, &lut, 2 + k % 4, 2 + k / 4).unwrap();
        }
        let tile_ms = t.elapsed().as_secs_f64() * 1e3 / n as f64;
        let t = Instant::now();
        let (cfa, bw, bh) = tile_cfa(&b, &d, tile_rect(&d, 3, 3).unwrap());
        let _ = super::super::demosaic::rcd(&cfa, bw, bh, |r, c| b.ch[r & 1][c & 1]);
        let rcd_ms = t.elapsed().as_secs_f64() * 1e3;
        eprintln!("develop {dev_ms:.1} ms, render {render_ms:.1} ms ({}x{}), render at 1024 {render_small_ms:.1} ms, tile {tile_ms:.1} ms (RCD alone {rcd_ms:.1} ms)", img.w, img.h);
    }

    /// A pulled-ISO frame: the sky clips at 13050 in every channel, the nominal white
    /// level is 16383 and one hot pixel reads 16383. The sky must be flagged.
    #[test]
    fn develop_flags_clipping_below_nominal_white() {
        let (w, h) = (256usize, 128usize);
        let mut data = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = if y < h / 2 { 13050 } else { 512 + ((x * 31 + y * 17) % 6000) as u16 };
            }
        }
        data[(h - 2) * w + 10] = 16383;
        let f = BayerFrame {
            data: &data,
            stride: w,
            rows: h,
            crop: [0, 0, w, h],
            ch: [[0, 1], [1, 2]],
            black: [[512.0; 2]; 2],
            black_at: None,
            white: 16383.0,
            white_exact: false,
            render_white: None,
            as_shot: Some([2.0, 1.0, 1.5]),
            daylight: None,
            cam_to_srgb: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            baseline_ev: 0.0,
        };
        let d = develop_bayer(&f, WbMode::Asshot, 8.0, Instant::now(), 0.0, "test").unwrap();
        let (hw, hh) = (w / 2, h / 2);
        assert!(d.mask[..hw * (hh / 2)].iter().all(|m| m & 7 == 7), "sky not flagged");
        assert_eq!(d.mask[hw * (hh / 2)..].iter().filter(|m| *m & 7 != 0).count(), 1, "only the hot pixel below the sky");
        assert!((d.over[1] - 0.5).abs() < 0.01, "{:?}", d.over);
    }
}
