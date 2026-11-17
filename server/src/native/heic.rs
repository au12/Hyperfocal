//! HEIC/HEIF decoding with libheif (child process side).
//!
//! libheif applies the container's transformations (rotation, mirroring, clean
//! aperture crop), so the image comes out upright and the file's EXIF orientation
//! must not be applied again: the index records HEIF files with orientation 1.

use super::{HeicInfo, frames, helper};
use crate::images::color::{self, Converter};
use crate::images::jpeg::{self, Rgb};
use anyhow::{Context, Result, anyhow, bail};
use libheif_rs::{ColorProfile, ColorSpace, HeifContext, LibHeif, RgbChroma, StreamReader, color_profile_types};
use std::ffi::OsString;
use std::io::Write;

/// Images larger than this are refused (libheif's own default limit is similar).
const MAX_PIXELS: u64 = 400_000_000;

pub fn version() -> String {
    let [a, b, c] = LibHeif::new().version();
    format!("{a}.{b}.{c}")
}

pub fn decode_op(args: &[OsString], out: &mut impl Write) -> Result<()> {
    let path = helper::path(args)?;
    let max_long = helper::num(args, 1, "maximum size")?;
    let (img, info) = decode(&path, max_long)?;
    frames::json(out, b"INFO", &info)?;
    frames::frame(out, b"RGB8", &img.px)?;
    Ok(())
}

pub fn decode(path: &std::path::Path, max_long: usize) -> Result<(Rgb, HeicInfo)> {
    let lib = LibHeif::new();
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let size = file.metadata()?.len();
    let ctx = HeifContext::read_from_reader(Box::new(StreamReader::new(std::io::BufReader::new(file), size)))
        .map_err(|e| anyhow!("libheif: {e}"))?;
    let handle = ctx.primary_image_handle().map_err(|e| anyhow!("libheif: {e}"))?;
    let (src_w, src_h) = (handle.width(), handle.height());
    if src_w == 0 || src_h == 0 || src_w as u64 * src_h as u64 > MAX_PIXELS {
        bail!("unsupported image size {src_w}x{src_h}");
    }
    let bits = handle.luma_bits_per_pixel().max(8);
    let nclx = handle.color_profile_nclx().map(|n| (n.color_primaries() as u16, n.transfer_characteristics() as u16));
    let icc = handle
        .color_profile_raw()
        .filter(|p| p.profile_type() == color_profile_types::PROF || p.profile_type() == color_profile_types::R_ICC)
        .map(|p| p.data);
    let source = color::choose(nclx, icc.as_deref());

    let chroma = if bits > 8 { RgbChroma::HdrRgbLe } else { RgbChroma::Rgb };
    let image = lib.decode(&handle, ColorSpace::Rgb(chroma), None).map_err(|e| anyhow!("libheif: {e}"))?;
    let planes = image.planes();
    let plane = planes.interleaved.ok_or_else(|| anyhow!("libheif returned no interleaved RGB plane"))?;
    let (w, h) = (plane.width as usize, plane.height as usize);
    if w == 0 || h == 0 {
        bail!("libheif returned an empty image");
    }
    let mut rgb = Rgb { w, h, px: vec![0u8; w * h * 3] };
    if bits > 8 {
        // RRGGBB little-endian, `bits` significant bits per sample.
        let conv = Converter::new(&source, plane.bits_per_pixel.clamp(9, 16) as u32);
        let mut row16 = vec![0u16; w * 3];
        for y in 0..h {
            let src = plane.data.get(y * plane.stride..y * plane.stride + w * 6).ok_or_else(|| anyhow!("short image plane"))?;
            for (d, s) in row16.iter_mut().zip(src.chunks_exact(2)) {
                *d = u16::from_le_bytes([s[0], s[1]]);
            }
            conv.convert_u16(&row16, &mut rgb.px[y * w * 3..(y + 1) * w * 3]);
        }
        rgb = fit(rgb, max_long)?;
    } else {
        for y in 0..h {
            let src = plane.data.get(y * plane.stride..y * plane.stride + w * 3).ok_or_else(|| anyhow!("short image plane"))?;
            rgb.px[y * w * 3..(y + 1) * w * 3].copy_from_slice(src);
        }
        drop(image);
        // Fewer pixels to convert after scaling; the result is the same to the eye.
        rgb = fit(rgb, max_long)?;
        Converter::new(&source, 8).convert_u8(&mut rgb.px);
    }
    let info = HeicInfo { w: rgb.w, h: rgb.h, src_w, src_h, bits, color: source.name };
    Ok((rgb, info))
}

fn fit(img: Rgb, max_long: usize) -> Result<Rgb> {
    if max_long > 0 && img.w.max(img.h) > max_long { jpeg::fit_long(img, max_long) } else { Ok(img) }
}
