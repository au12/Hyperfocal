//! JPEG helpers: DCT-scaled decoding, encoding, orientation and header stripping.

use anyhow::{Result, anyhow};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};
use turbojpeg::{Compressor, Decompressor, PixelFormat, ScalingFactor, Subsamp};

pub struct Rgb {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

/// Decode a JPEG so that its long edge is at least `min_long` (or full size when
/// `min_long` is 0), using libjpeg-turbo's DCT scaling for big speed-ups.
pub fn decode_scaled(data: &[u8], min_long: usize) -> Result<Rgb> {
    let mut d = Decompressor::new()?;
    let header = d.read_header(data)?;
    let long = header.width.max(header.height);
    let mut factor = ScalingFactor::ONE;
    if min_long > 0 {
        for denom in [8usize, 4, 2] {
            let f = ScalingFactor::new(1, denom);
            if f.scale(long) >= min_long {
                factor = f;
                break;
            }
        }
    }
    d.set_scaling_factor(factor)?;
    let scaled = header.scaled(factor);
    let (w, h) = (scaled.width, scaled.height);
    if w == 0 || h == 0 || w * h > 200_000_000 {
        return Err(anyhow!("unreasonable JPEG size {w}x{h}"));
    }
    let mut px = vec![0u8; w * h * 3];
    d.decompress(data, turbojpeg::Image { pixels: px.as_mut_slice(), width: w, pitch: w * 3, height: h, format: PixelFormat::RGB })?;
    Ok(Rgb { w, h, px })
}

/// Decode a JPEG at 1/`denom` of its size (1, 2, 4 or 8), with libjpeg-turbo's DCT
/// scaling. Odd sizes round up (`(n + denom - 1) / denom`).
pub fn decode_factor(data: &[u8], denom: usize) -> Result<Rgb> {
    if ![1, 2, 4, 8].contains(&denom) {
        return Err(anyhow!("unsupported scale 1/{denom}"));
    }
    let mut d = Decompressor::new()?;
    let header = d.read_header(data)?;
    let factor = ScalingFactor::new(1, denom);
    d.set_scaling_factor(factor)?;
    let scaled = header.scaled(factor);
    let (w, h) = (scaled.width, scaled.height);
    if w == 0 || h == 0 || w * h > 200_000_000 {
        return Err(anyhow!("unreasonable JPEG size {w}x{h}"));
    }
    let mut px = vec![0u8; w * h * 3];
    d.decompress(data, turbojpeg::Image { pixels: px.as_mut_slice(), width: w, pitch: w * 3, height: h, format: PixelFormat::RGB })?;
    Ok(Rgb { w, h, px })
}

/// Width and height of a JPEG, from its header.
pub fn dims(data: &[u8]) -> Result<(usize, usize)> {
    let mut d = Decompressor::new()?;
    let h = d.read_header(data)?;
    Ok((h.width, h.height))
}

pub fn encode(img: &Rgb, quality: i32, chroma_full: bool) -> Result<Vec<u8>> {
    let _t = crate::tel_op!("jpeg.encode", 200);
    let mut c = Compressor::new()?;
    c.set_quality(quality)?;
    c.set_subsamp(if chroma_full { Subsamp::None } else { Subsamp::Sub2x2 })?;
    Ok(c.compress_to_vec(turbojpeg::Image {
        pixels: img.px.as_slice(),
        width: img.w,
        pitch: img.w * 3,
        height: img.h,
        format: PixelFormat::RGB,
    })?)
}

/// Smaller files at the same visual quality, for images made in the background where
/// nobody waits for them: mozjpeg's trellis quantization and optimized Huffman tables
/// (pure Rust, byte-identical to C mozjpeg), 4:2:0. Measured on 2048-pixel previews of
/// real photos against `encode` at the same SSIMULACRA2 score: quality 88 here matches
/// libjpeg-turbo's 85 at 7% fewer bytes (up to 26%), and quality 80 matches 78 for
/// thumbnails at 12% fewer, for roughly 15 times the encode time (12 ms a thumbnail,
/// 150 ms a 2048 preview on one core). jpegli did better on paper (12%) but only as C++
/// to build; WebP was 4% smaller, inconsistent, and 25 times slower.
pub fn encode_small(img: &Rgb, quality: u8) -> Result<Vec<u8>> {
    let _t = crate::tel_op!("jpeg.encode_small", 1000);
    if img.px.len() != img.w * img.h * 3 {
        return Err(anyhow!("bad image buffer"));
    }
    mozjpeg_rs::Encoder::baseline_optimized()
        .quality(quality)
        .subsampling(mozjpeg_rs::Subsampling::S420)
        .encode_rgb(&img.px, img.w as u32, img.h as u32)
        .map_err(|e| anyhow!("JPEG encoding: {e}"))
}

/// Resize so that the long edge is `long` (never upscales).
pub fn fit_long(img: Rgb, long: usize) -> Result<Rgb> {
    let cur = img.w.max(img.h);
    if long == 0 || cur <= long {
        return Ok(img);
    }
    let s = long as f64 / cur as f64;
    let nw = ((img.w as f64 * s).round() as usize).max(1);
    let nh = ((img.h as f64 * s).round() as usize).max(1);
    resize(img, nw, nh)
}

pub fn resize(img: Rgb, nw: usize, nh: usize) -> Result<Rgb> {
    if nw == img.w && nh == img.h {
        return Ok(img);
    }
    let src = Image::from_vec_u8(img.w as u32, img.h as u32, img.px, PixelType::U8x3)?;
    let mut dst = Image::new(nw as u32, nh as u32, PixelType::U8x3);
    let mut r = Resizer::new();
    r.resize(&src, &mut dst, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom)))?;
    Ok(Rgb { w: nw, h: nh, px: dst.into_vec() })
}

/// Apply an EXIF orientation (1-8) to an RGB image.
pub fn orient(img: Rgb, o: u8) -> Rgb {
    if !(2..=8).contains(&o) {
        return img;
    }
    let (w, h) = (img.w, img.h);
    let swap = o >= 5;
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh * 3];
    let src = &img.px;
    for y in 0..oh {
        let row = &mut out[y * ow * 3..(y + 1) * ow * 3];
        for x in 0..ow {
            let (sx, sy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x), // 8
            };
            let si = (sy * w + sx) * 3;
            row[x * 3..x * 3 + 3].copy_from_slice(&src[si..si + 3]);
        }
    }
    Rgb { w: ow, h: oh, px: out }
}

/// Remove APP1 (EXIF/XMP) and APP13 segments so browsers do not auto-rotate a
/// preview whose orientation the viewer applies itself. Pixel data is untouched.
pub fn strip_exif(data: &[u8]) -> Vec<u8> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&data[..2]);
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            break;
        }
        let marker = data[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if len < 2 || i + 2 + len > data.len() {
            break;
        }
        if marker != 0xE1 && marker != 0xED {
            out.extend_from_slice(&data[i..i + 2 + len]);
        }
        i += 2 + len;
    }
    out.extend_from_slice(&data[i.min(data.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth scene with some detail: gradients, a few edges and mild noise.
    fn scene(w: usize, h: usize) -> Rgb {
        let mut px = Vec::with_capacity(w * h * 3);
        let mut x: u32 = 12345;
        for y in 0..h {
            for xx in 0..w {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let n = (x % 9) as i32 - 4;
                let edge = if (xx / 37 + y / 23) % 5 == 0 { 60 } else { 0 };
                let r = (xx * 200 / w) as i32 + edge + n;
                let g = (y * 180 / h) as i32 + 30 + n;
                let b = ((xx + y) * 120 / (w + h)) as i32 + 60 - edge / 2 + n;
                px.extend([r, g, b].map(|v| v.clamp(0, 255) as u8));
            }
        }
        Rgb { w, h, px }
    }

    #[test]
    fn small_encoding_is_a_faithful_jpeg() {
        let img = scene(640, 427);
        let small = encode_small(&img, 88).unwrap();
        let back = decode_scaled(&small, 0).unwrap();
        assert_eq!((back.w, back.h), (img.w, img.h));
        let mse = img.px.iter().zip(&back.px).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum::<f64>() / img.px.len() as f64;
        let psnr = 10.0 * (255.0f64 * 255.0 / mse).log10();
        assert!(psnr > 36.0, "PSNR {psnr:.1} dB");
        assert!(encode_small(&Rgb { w: 2, h: 2, px: vec![0; 3] }, 80).is_err(), "a short buffer is refused");
    }
    #[test]
    fn orient_roundtrip() {
        // 2x1 image: red, green
        let img = Rgb { w: 2, h: 1, px: vec![255, 0, 0, 0, 255, 0] };
        let r = orient(img, 6);
        assert_eq!((r.w, r.h), (1, 2));
        assert_eq!(&r.px[..3], &[255, 0, 0]);
        let img = Rgb { w: 2, h: 1, px: vec![255, 0, 0, 0, 255, 0] };
        let r = orient(img, 8);
        assert_eq!(&r.px[..3], &[0, 255, 0]);
    }
    #[test]
    fn strip_keeps_image() {
        let img = Rgb { w: 16, h: 8, px: vec![128; 16 * 8 * 3] };
        let jpg = encode(&img, 80, false).unwrap();
        let s = strip_exif(&jpg);
        let d = decode_scaled(&s, 0).unwrap();
        assert_eq!((d.w, d.h), (16, 8));
    }
}
