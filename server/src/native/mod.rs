//! Native (C library) decoders: libheif for HEIC/HEIF and LibRaw as the fallback
//! RAW decoder. They always run in a short-lived child process, `hyperfocal native
//! <op> ...`, never inside the server.
//!
//! Both are mature, widely fuzzed libraries, but they are C/C++ parsing whatever bytes
//! a file holds. In a child process a crash, a hang or a memory blow-up on one odd
//! file costs one request: the server stays up, the child is killed after a timeout,
//! and nothing it does can reach the server's state. The pure-Rust paths (FlashRaw,
//! JPEG, the HEIF metadata reader) stay in-process.

pub mod frames;
pub mod helper;
#[cfg(feature = "heic")]
mod heic;
#[cfg(feature = "libraw")]
mod libraw;
#[cfg(test)]
mod tests;

use crate::images::cancel;
use crate::images::jpeg::Rgb;
use crate::images::raw::WbMode;
use anyhow::{Result, anyhow, bail};
use frames::Frames;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const HEIC: bool = cfg!(feature = "heic");
pub const LIBRAW: bool = cfg!(feature = "libraw");

/// Generous: the first read from a spun-down array disk can take many seconds.
const META_TIMEOUT: Duration = Duration::from_secs(60);
const DECODE_TIMEOUT: Duration = Duration::from_secs(180);
/// Upper bound on one response (a 100-megapixel RAW is 200 MB of samples).
const MAX_OUTPUT: u64 = 1 << 30;

static PREFER_LIBRAW: AtomicBool = AtomicBool::new(false);

/// Use LibRaw for every RAW file, not only when FlashRaw cannot handle one
/// (`HYPERFOCAL_RAW_DECODER=libraw`).
pub fn set_prefer_libraw(v: bool) {
    PREFER_LIBRAW.store(v && LIBRAW, Ordering::Relaxed);
}

pub fn prefer_libraw() -> bool {
    PREFER_LIBRAW.load(Ordering::Relaxed)
}

fn helper_exe() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("HYPERFOCAL_HELPER_EXE") {
        return Ok(PathBuf::from(p));
    }
    if cfg!(test) {
        // A unit-test binary is not the server: re-running it would run the tests.
        bail!("the native helper is not available in unit tests");
    }
    // Keeps working when the binary on disk is replaced while the server runs.
    let proc = Path::new("/proc/self/exe");
    if proc.exists() { Ok(proc.to_path_buf()) } else { Ok(std::env::current_exe()?) }
}

/// Runs `hyperfocal native <args>` and returns its stdout.
fn run(args: Vec<OsString>, timeout: Duration) -> Result<Vec<u8>> {
    run_with(&helper_exe()?, args, timeout)
}

fn run_with(exe: &Path, args: Vec<OsString>, timeout: Duration) -> Result<Vec<u8>> {
    let mut cmd = Command::new(exe);
    cmd.arg("native")
        .args(&args)
        // LibRaw turns capture times into time_t with mktime(): in UTC the camera's
        // clock comes back unchanged, which is how the index stores it.
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe. The spawning thread waits for the child,
        // so the signal only fires if the server itself dies.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().map_err(|e| anyhow!("starting the native helper: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        (&mut stdout).take(MAX_OUTPUT + 1).read_to_end(&mut v).map(|_| v)
    });
    let err_t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = (&mut stderr).take(16 * 1024).read_to_end(&mut v);
        v
    });

    let deadline = Instant::now() + timeout;
    let mut nap = Duration::from_millis(1);
    // Nobody waits for the result any more (see images::cancel): the helper is stopped at
    // once, as a FlashRaw decode would stop.
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline && !cancel::is_cancelled() => {
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(20));
            }
            r => {
                cancelled = r.is_ok() && cancel::is_cancelled();
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = out_t.join().map_err(|_| anyhow!("native helper reader failed"))?;
    let err = err_t.join().unwrap_or_default();
    let Some(status) = status else {
        if cancelled {
            return Err(cancel::Cancelled.into());
        }
        bail!("the native helper took longer than {}s and was stopped", timeout.as_secs());
    };
    let out = out?;
    if out.len() as u64 > MAX_OUTPUT {
        bail!("the native helper's output is too large");
    }
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let tail = String::from_utf8_lossy(&err);
            let tail = tail.trim();
            bail!("the native decoder crashed (signal {sig}){}{tail}", if tail.is_empty() { "" } else { ": " });
        }
    }
    Ok(out)
}

fn parse(out: &[u8]) -> Result<Frames<'_>> {
    Frames::parse(out).map_err(|e| anyhow!(e))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Versions {
    pub libheif: Option<String>,
    pub libraw: Option<String>,
}

/// Versions of the native libraries, as the helper process reports them.
fn versions() -> Versions {
    static V: OnceLock<Versions> = OnceLock::new();
    V.get_or_init(|| {
        if !HEIC && !LIBRAW {
            return Versions::default();
        }
        match run(vec!["version".into()], META_TIMEOUT).and_then(|out| parse(&out)?.json::<Versions>(b"INFO").map_err(|e| anyhow!(e))) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("native decoders unavailable: {e:#}");
                Versions::default()
            }
        }
    })
    .clone()
}

/// The decoders this server uses, for logs and the About box.
#[derive(Debug, Clone, Serialize)]
pub struct Decoders {
    pub flashraw: &'static str,
    pub libraw: Option<String>,
    pub libheif: Option<String>,
    /// "flashraw" (LibRaw as the fallback) or "libraw" (LibRaw for everything).
    pub raw: &'static str,
}

impl Decoders {
    pub fn detect() -> Decoders {
        let v = versions();
        Decoders {
            flashraw: env!("HYPERFOCAL_FLASHRAW_VERSION"),
            libraw: v.libraw,
            libheif: v.libheif,
            raw: if prefer_libraw() { "libraw" } else { "flashraw" },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeicInfo {
    /// Size of the returned image.
    pub w: usize,
    pub h: usize,
    /// Size of the primary image (upright).
    pub src_w: u32,
    pub src_h: u32,
    pub bits: u8,
    /// What the colours were converted from.
    pub color: String,
    /// Which image this is: `primary`, an embedded `thumbnail` (as big as asked), or a
    /// smaller `draft`.
    #[serde(default)]
    pub source: String,
}

/// Decodes the primary image of a HEIC/HEIF file to sRGB, upright (the container's
/// rotation, mirroring and crop applied), fitted to `max_long` (0 = full size).
pub fn heic_decode(path: &Path, max_long: usize) -> Result<(Rgb, HeicInfo)> {
    if !HEIC {
        bail!("this build has no HEIC support");
    }
    let out = run(vec!["heic".into(), path.into(), max_long.to_string().into()], DECODE_TIMEOUT)?;
    parse_heic(&out)
}

/// The image for a thumbnail of `want` pixels from a HEIC/HEIF file, as fast as it can
/// be had: an embedded thumbnail at least that big, a smaller one of at least
/// `draft_min` (marked `draft`), or the primary image.
pub fn heic_thumb(path: &Path, want: usize, draft_min: usize) -> Result<(Rgb, HeicInfo)> {
    if !HEIC {
        bail!("this build has no HEIC support");
    }
    let out = run(vec!["heic-thumb".into(), path.into(), want.to_string().into(), draft_min.to_string().into()], DECODE_TIMEOUT)?;
    parse_heic(&out)
}

/// w * h * channels, None on overflow (the sizes come from another process).
fn size(w: usize, h: usize, channels: usize) -> Option<usize> {
    w.checked_mul(h)?.checked_mul(channels)
}

pub(crate) fn parse_heic(out: &[u8]) -> Result<(Rgb, HeicInfo)> {
    let f = parse(out)?;
    let info: HeicInfo = f.json(b"INFO").map_err(|e| anyhow!(e))?;
    let px = f.require(b"RGB8").map_err(|e| anyhow!(e))?;
    if info.w == 0 || info.h == 0 || size(info.w, info.h, 3) != Some(px.len()) {
        bail!("the native helper sent a malformed image");
    }
    Ok((Rgb { w: info.w, h: info.h, px: px.to_vec() }, info))
}

/// Metadata of a RAW file as LibRaw reads it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibrawMeta {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    /// Seconds.
    pub exposure: Option<f64>,
    pub fnum: Option<f64>,
    pub focal: Option<f64>,
    /// Camera clock, `YYYY-MM-DDTHH:MM:SS`.
    pub taken: Option<String>,
    pub artist: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt: Option<f64>,
    /// Sensor orientation (before `orientation` is applied).
    pub width: u32,
    pub height: u32,
    /// EXIF orientation 1..=8.
    pub orientation: u8,
    #[serde(skip)]
    pub xmp: Option<Vec<u8>>,
}

pub fn libraw_meta(path: &Path) -> Result<LibrawMeta> {
    if !LIBRAW {
        bail!("this build has no LibRaw support");
    }
    let out = run(vec!["libraw-meta".into(), path.into()], META_TIMEOUT)?;
    parse_libraw_meta(&out)
}

pub(crate) fn parse_libraw_meta(out: &[u8]) -> Result<LibrawMeta> {
    let f = parse(out)?;
    let mut m: LibrawMeta = f.json(b"META").map_err(|e| anyhow!(e))?;
    m.xmp = f.get(b"XMP ").filter(|x| !x.is_empty()).map(|x| x.to_vec());
    if !(1..=8).contains(&m.orientation) {
        m.orientation = 1;
    }
    Ok(m)
}

/// An embedded preview extracted by LibRaw (sensor orientation).
pub enum Preview {
    Jpeg { data: Vec<u8>, w: u32, h: u32 },
    Rgb(Rgb),
}

impl Preview {
    pub fn long_edge(&self) -> usize {
        match self {
            Preview::Jpeg { w, h, .. } => (*w).max(*h) as usize,
            Preview::Rgb(r) => r.w.max(r.h),
        }
    }
}

#[derive(Deserialize)]
struct PreviewInfo {
    format: String,
    w: u32,
    h: u32,
}

/// The smallest embedded preview with a long edge of at least `min_long`, or the
/// largest one there is.
pub fn libraw_preview(path: &Path, min_long: usize) -> Result<Preview> {
    if !LIBRAW {
        bail!("this build has no LibRaw support");
    }
    let out = run(vec!["libraw-preview".into(), path.into(), min_long.to_string().into()], DECODE_TIMEOUT)?;
    parse_libraw_preview(&out)
}

pub(crate) fn parse_libraw_preview(out: &[u8]) -> Result<Preview> {
    let f = parse(out)?;
    let info: PreviewInfo = f.json(b"INFO").map_err(|e| anyhow!(e))?;
    let data = f.require(b"DATA").map_err(|e| anyhow!(e))?;
    match info.format.as_str() {
        "jpeg" if !data.is_empty() => {
            let (mut w, mut h) = (info.w, info.h);
            if w == 0 || h == 0 {
                let hd = crate::meta::read_jpeg_headers(&mut std::io::Cursor::new(data)).map_err(|e| anyhow!("embedded preview: {e}"))?;
                w = hd.width.unwrap_or(0);
                h = hd.height.unwrap_or(0);
            }
            Ok(Preview::Jpeg { data: data.to_vec(), w, h })
        }
        "rgb8" if info.w > 0 && info.h > 0 && size(info.w as usize, info.h as usize, 3) == Some(data.len()) => {
            Ok(Preview::Rgb(Rgb { w: info.w as usize, h: info.h as usize, px: data.to_vec() }))
        }
        _ => bail!("the native helper sent a malformed preview"),
    }
}

/// Bayer sensor data for half-size development, visible area only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BayerInfo {
    pub w: usize,
    pub h: usize,
    /// Channel (0 = R, 1 = G, 2 = B) of each 2x2 position, from the top-left corner.
    pub ch: [[u8; 2]; 2],
    /// Black level pattern repeating from the top-left corner (`rows` x `cols`).
    pub black_rows: usize,
    pub black_cols: usize,
    pub black: Vec<f32>,
    pub white: f32,
    /// `white` is the maker note's saturation level (LibRaw `linear_max`).
    #[serde(default)]
    pub white_exact: bool,
    /// The level that renders white when below saturation (Canon NormalWhiteLevel), else 0.
    #[serde(default)]
    pub render_white: f32,
    /// White balance multipliers (R, G, B), G = 1.
    pub as_shot: Option<[f32; 3]>,
    pub daylight: Option<[f32; 3]>,
    /// White-balanced camera RGB to linear sRGB.
    pub cam_to_srgb: [[f32; 3]; 3],
    pub baseline_ev: f32,
}

/// An image LibRaw developed itself (non-Bayer sensors): linear sRGB, 16 bits, half size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RgbInfo {
    pub w: usize,
    pub h: usize,
    pub baseline_ev: f32,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum RawInfo {
    Bayer(BayerInfo),
    Rgb16(RgbInfo),
}

pub enum LibrawRaw {
    Bayer(BayerInfo, Vec<u16>),
    Rgb16(RgbInfo, Vec<u16>),
}

pub fn libraw_raw(path: &Path, wb: WbMode) -> Result<LibrawRaw> {
    if !LIBRAW {
        bail!("this build has no LibRaw support");
    }
    let wb = match wb {
        WbMode::Asshot => "asshot",
        WbMode::Daylight => "daylight",
        WbMode::Auto => "auto",
    };
    let out = run(vec!["libraw-raw".into(), path.into(), wb.into()], DECODE_TIMEOUT)?;
    parse_libraw_raw(&out)
}

pub(crate) fn parse_libraw_raw(out: &[u8]) -> Result<LibrawRaw> {
    let f = parse(out)?;
    let info: RawInfo = f.json(b"INFO").map_err(|e| anyhow!(e))?;
    let data = frames::u16s(f.require(b"DATA").map_err(|e| anyhow!(e))?).map_err(|e| anyhow!(e))?;
    match info {
        RawInfo::Bayer(b) => {
            let ok = b.w >= 2
                && b.h >= 2
                && size(b.w, b.h, 1) == Some(data.len())
                && b.ch.iter().flatten().all(|c| *c <= 2)
                && (1..=64).contains(&b.black_rows)
                && (1..=64).contains(&b.black_cols)
                && b.black.len() == b.black_rows * b.black_cols
                && b.black.iter().all(|v| v.is_finite() && *v >= 0.0)
                && b.white.is_finite()
                && b.white > 0.0
                && b.cam_to_srgb.iter().flatten().all(|v| v.is_finite());
            if !ok {
                bail!("the native helper sent malformed sensor data");
            }
            Ok(LibrawRaw::Bayer(b, data))
        }
        RawInfo::Rgb16(r) => {
            if r.w == 0 || r.h == 0 || size(r.w, r.h, 3) != Some(data.len()) {
                bail!("the native helper sent a malformed image");
            }
            Ok(LibrawRaw::Rgb16(r, data))
        }
    }
}

/// In-process runs of the helper ops, parsed like a real response (the unit-test
/// binary cannot spawn itself as the helper).
#[cfg(test)]
#[cfg_attr(not(all(feature = "heic", feature = "libraw")), allow(dead_code))]
pub mod testing {
    use super::*;

    fn op(args: &[&str]) -> Vec<u8> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        let mut out = Vec::new();
        if let Err(e) = helper::run(&args, &mut out) {
            frames::frame(&mut out, frames::ERR, format!("{e:#}").as_bytes()).unwrap();
        }
        out
    }

    pub fn heic_decode(path: &Path, max_long: usize) -> Result<(Rgb, HeicInfo)> {
        parse_heic(&op(&["heic", path.to_str().unwrap(), &max_long.to_string()]))
    }

    pub fn heic_thumb(path: &Path, want: usize, draft_min: usize) -> Result<(Rgb, HeicInfo)> {
        parse_heic(&op(&["heic-thumb", path.to_str().unwrap(), &want.to_string(), &draft_min.to_string()]))
    }

    pub fn libraw_meta(path: &Path) -> Result<LibrawMeta> {
        parse_libraw_meta(&op(&["libraw-meta", path.to_str().unwrap()]))
    }

    pub fn libraw_preview(path: &Path, min_long: usize) -> Result<Preview> {
        parse_libraw_preview(&op(&["libraw-preview", path.to_str().unwrap(), &min_long.to_string()]))
    }

    pub fn libraw_raw(path: &Path, wb: &str) -> Result<LibrawRaw> {
        parse_libraw_raw(&op(&["libraw-raw", path.to_str().unwrap(), wb]))
    }
}
