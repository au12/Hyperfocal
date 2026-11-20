//! HEIC/HEIF decoding with libheif (child process side).
//!
//! libheif applies the container's transformations (rotation, mirroring, clean
//! aperture crop), so the image comes out upright and the file's EXIF orientation
//! must not be applied again: the index records HEIF files with orientation 1.

use super::{HeicInfo, frames, helper};
use crate::images::color::{self, Converter};
use crate::images::jpeg::{self, Rgb};
use anyhow::{Context, Result, anyhow, bail};
use libheif_rs::{ColorProfile, ColorSpace, HeifContext, ImageHandle, ItemId, LibHeif, RgbChroma, StreamReader, color_profile_types};
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

/// `heic-thumb <path> <want> <draft_min>`: the image for a thumbnail of `want` pixels
/// (long edge), as fast as it can be had: an embedded thumbnail that big; else, when
/// `draft_min` > 0, the largest one of at least `draft_min` (a draft, to show at once
/// while the full-quality thumbnail is made); else the primary image.
pub fn thumb_op(args: &[OsString], out: &mut impl Write) -> Result<()> {
    let path = helper::path(args)?;
    let want = helper::num(args, 1, "size")?;
    let draft_min = helper::num(args, 2, "draft size")?;
    let (img, info) = thumb(&path, want, draft_min)?;
    frames::json(out, b"INFO", &info)?;
    frames::frame(out, b"RGB8", &img.px)?;
    Ok(())
}

fn open(path: &std::path::Path) -> Result<HeifContext<'static>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let size = file.metadata()?.len();
    HeifContext::read_from_reader(Box::new(StreamReader::new(std::io::BufReader::new(file), size))).map_err(|e| anyhow!("libheif: {e}"))
}

pub fn thumb(path: &std::path::Path, want: usize, draft_min: usize) -> Result<(Rgb, HeicInfo)> {
    let lib = LibHeif::new();
    let ctx = open(path)?;
    let primary = ctx.primary_image_handle().map_err(|e| anyhow!("libheif: {e}"))?;
    let (pw, ph) = (primary.width(), primary.height());
    let n = primary.number_of_thumbnails();
    let mut ids: Vec<ItemId> = vec![0; n.min(16)];
    let n = primary.thumbnail_ids(&mut ids);
    // Thumbnails of the whole picture only (the same shape as the primary image).
    let same_shape = |w: u32, h: u32| w > 0 && h > 0 && pw > 0 && ph > 0 && ((w as f64 / h as f64) / (pw as f64 / ph as f64) - 1.0).abs() < 0.03;
    let mut thumbs: Vec<(usize, ImageHandle)> = ids[..n]
        .iter()
        .filter_map(|id| primary.thumbnail(*id).ok())
        .filter(|t| same_shape(t.width(), t.height()))
        .map(|t| (t.width().max(t.height()) as usize, t))
        .collect();
    thumbs.sort_by_key(|t| t.0);
    let pick = match thumbs.iter().position(|t| t.0 >= want) {
        Some(i) => Some((thumbs.swap_remove(i).1, "thumbnail")),
        None if draft_min > 0 => thumbs.pop().filter(|t| t.0 >= draft_min).map(|t| (t.1, "draft")),
        None => None,
    };
    match pick {
        Some((t, source)) => {
            let (img, mut info) = decode_handle(&lib, &t, want, (pw, ph))?;
            info.source = source.into();
            Ok((img, info))
        }
        None => decode_handle(&lib, &primary, want, (pw, ph)),
    }
}

pub fn decode(path: &std::path::Path, max_long: usize) -> Result<(Rgb, HeicInfo)> {
    let lib = LibHeif::new();
    let ctx = open(path)?;
    let handle = ctx.primary_image_handle().map_err(|e| anyhow!("libheif: {e}"))?;
    let size = (handle.width(), handle.height());
    decode_handle(&lib, &handle, max_long, size)
}

/// Decodes one image of the file (`src` is the primary image's size, for the info).
fn decode_handle(lib: &LibHeif, handle: &ImageHandle, max_long: usize, src: (u32, u32)) -> Result<(Rgb, HeicInfo)> {
    let (src_w, src_h) = src;
    let (w0, h0) = (handle.width(), handle.height());
    if w0 == 0 || h0 == 0 || w0 as u64 * h0 as u64 > MAX_PIXELS {
        bail!("unsupported image size {w0}x{h0}");
    }
    let bits = handle.luma_bits_per_pixel().max(8);
    let nclx = handle.color_profile_nclx().map(|n| (n.color_primaries() as u16, n.transfer_characteristics() as u16));
    let icc = handle
        .color_profile_raw()
        .filter(|p| p.profile_type() == color_profile_types::PROF || p.profile_type() == color_profile_types::R_ICC)
        .map(|p| p.data);
    let source = color::choose(nclx, icc.as_deref());

    let chroma = if bits > 8 { RgbChroma::HdrRgbLe } else { RgbChroma::Rgb };
    let image = lib.decode(handle, ColorSpace::Rgb(chroma), None).map_err(|e| anyhow!("libheif: {e}"))?;
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
    let info = HeicInfo { w: rgb.w, h: rgb.h, src_w, src_h, bits, color: source.name, source: "primary".into() };
    Ok((rgb, info))
}

fn fit(img: Rgb, max_long: usize) -> Result<Rgb> {
    if max_long > 0 && img.w.max(img.h) > max_long { jpeg::fit_long(img, max_long) } else { Ok(img) }
}
