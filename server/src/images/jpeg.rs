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

pub fn encode(img: &Rgb, quality: i32, chroma_full: bool) -> Result<Vec<u8>> {
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
