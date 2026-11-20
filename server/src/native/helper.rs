//! Child side of the native helper: `hyperfocal native <op> [args]`.
//!
//! Writes framed results to stdout (see `frames`) and exits. Ops:
//! - `version`
//! - `heic <path> <max_long>`
//! - `heic-thumb <path> <want> <draft_min>`
//! - `libraw-meta <path>`
//! - `libraw-preview <path> <min_long>`
//! - `libraw-raw <path> <asshot|daylight|auto>`

use super::frames;
use anyhow::{Result, bail};
use std::ffi::OsString;
use std::io::{BufWriter, Write};
use std::process::ExitCode;

/// `args` are the arguments after `native` (paths need not be UTF-8).
pub fn main(args: &[OsString]) -> ExitCode {
    let stdout = std::io::stdout();
    let mut out = BufWriter::with_capacity(1 << 20, stdout.lock());
    let code = match run(args, &mut out) {
        Ok(()) => 0,
        Err(e) => {
            let _ = frames::frame(&mut out, frames::ERR, format!("{e:#}").as_bytes());
            2
        }
    };
    if out.flush().is_err() {
        return ExitCode::from(3);
    }
    ExitCode::from(code)
}

/// Runs one op, writing the magic and its frames to `out`.
pub fn run(args: &[OsString], out: &mut impl Write) -> Result<()> {
    frames::begin(out)?;
    let Some((op, rest)) = args.split_first() else { bail!("no op given") };
    match op.to_str().unwrap_or("") {
        "version" => version(out),
        #[cfg(feature = "heic")]
        "heic" => super::heic::decode_op(rest, out),
        #[cfg(feature = "heic")]
        "heic-thumb" => super::heic::thumb_op(rest, out),
        #[cfg(feature = "libraw")]
        "libraw-meta" => super::libraw::meta_op(rest, out),
        #[cfg(feature = "libraw")]
        "libraw-preview" => super::libraw::preview_op(rest, out),
        #[cfg(feature = "libraw")]
        "libraw-raw" => super::libraw::raw_op(rest, out),
        other => {
            let _ = rest;
            bail!("unsupported op `{other}` in this build")
        }
    }
}

fn version(out: &mut impl Write) -> Result<()> {
    let v = super::Versions {
        #[cfg(feature = "heic")]
        libheif: Some(super::heic::version()),
        #[cfg(not(feature = "heic"))]
        libheif: None,
        #[cfg(feature = "libraw")]
        libraw: Some(super::libraw::version()),
        #[cfg(not(feature = "libraw"))]
        libraw: None,
    };
    frames::json(out, b"INFO", &v)?;
    Ok(())
}

/// A numeric argument.
#[cfg_attr(not(any(feature = "heic", feature = "libraw")), allow(dead_code))]
pub(super) fn num(args: &[OsString], i: usize, what: &str) -> Result<usize> {
    match args.get(i).and_then(|s| s.to_str()).map(|s| s.parse::<usize>()) {
        Some(Ok(v)) => Ok(v),
        _ => bail!("missing or invalid {what}"),
    }
}

/// A word argument.
#[cfg_attr(not(feature = "libraw"), allow(dead_code))]
pub(super) fn word<'a>(args: &'a [OsString], i: usize, what: &str) -> Result<&'a str> {
    match args.get(i).and_then(|s| s.to_str()) {
        Some(v) => Ok(v),
        None => bail!("missing or invalid {what}"),
    }
}

/// The file argument (first).
#[cfg_attr(not(any(feature = "heic", feature = "libraw")), allow(dead_code))]
pub(super) fn path(args: &[OsString]) -> Result<std::path::PathBuf> {
    match args.first() {
        Some(p) if !p.is_empty() => Ok(std::path::PathBuf::from(p)),
        _ => bail!("no file given"),
    }
}
