//! Zoom tiles for every view but the RAW one: a JPEG, HEIC, TIFF or PNG file, or a RAW
//! file's embedded JPEG. Zoomed in, the viewer no longer loads a 45-100 MP image whole:
//! it asks for the tiles in view, at the level of a pyramid that fits the zoom (level 0
//! is full size, each next one half as big), and draws them over the screen-size preview
//! as it does the RAW view's full-resolution tiles.
//!
//! Tiles are TILE pixels square (smaller at the right and bottom edges) with APRON
//! pixels of their neighbours on each side, mirrored at the image's edges, so the
//! viewer can filter across seams. They are in the file's own orientation (EXIF
//! orientation not applied; HEIC upright, as decoded), like previews.
//!
//! A JPEG source is decoded at each level with libjpeg-turbo's DCT scaling (a quarter
//! of the work for each halving); other sources are decoded once and halved.

use super::jpeg::{self, Rgb};
use anyhow::{Result, anyhow};
use rayon::prelude::*;
use serde::Serialize;

pub const TILE: usize = super::raw::TILE;
pub const APRON: usize = super::raw::TILE_APRON;
/// Levels are made while the level is longer than this: below it, a preview (which goes
/// up to 4096 px) is as sharp.
pub const MIN_LEVEL_LONG: usize = 2048;
/// At most this many levels (1/8 is libjpeg-turbo's smallest DCT scale).
pub const MAX_LEVELS: usize = 4;

/// The pyramid of an image: its full size and how many levels it has (0: small enough
/// for previews alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pyramid {
    pub w: usize,
    pub h: usize,
    pub levels: usize,
    pub tile: usize,
    pub apron: usize,
}

impl Pyramid {
    pub fn new(w: usize, h: usize) -> Pyramid {
        let mut levels = 0;
        while levels < MAX_LEVELS && level_dims(w, h, levels).0.max(level_dims(w, h, levels).1) > MIN_LEVEL_LONG {
            levels += 1;
        }
        Pyramid { w, h, levels, tile: TILE, apron: APRON }
    }

    /// Size of level `l`.
    pub fn dims(&self, l: usize) -> (usize, usize) {
        level_dims(self.w, self.h, l)
    }

    /// The rectangle (x, y, w, h) of tile (tx, ty) at level `l`, if there is one.
    pub fn tile_rect(&self, l: usize, tx: usize, ty: usize) -> Result<[usize; 4]> {
        if l >= self.levels {
            return Err(anyhow!("no level {l}"));
        }
        let (w, h) = self.dims(l);
        let (x, y) = (tx.saturating_mul(TILE), ty.saturating_mul(TILE));
        if x >= w || y >= h {
            return Err(crate::api::missing("tile"));
        }
        Ok([x, y, TILE.min(w - x), TILE.min(h - y)])
    }
}

/// Size of level `l` of a `w` x `h` image: halved `l` times, rounding up, as
/// libjpeg-turbo's DCT scaling does.
pub fn level_dims(w: usize, h: usize, l: usize) -> (usize, usize) {
    let d = 1usize << l;
    (w.div_ceil(d).max(1), h.div_ceil(d).max(1))
}

/// Decodes level `l` of a JPEG (DCT scaling: 1/2, 1/4, 1/8).
pub fn decode_jpeg_level(data: &[u8], l: usize) -> Result<Rgb> {
    jpeg::decode_factor(data, 1 << l)
}

/// Level `l` of an image held as pixels: halved `l` times (area average).
pub fn halve_to(img: &Rgb, l: usize) -> Result<Rgb> {
    if l == 0 {
        return Ok(Rgb { w: img.w, h: img.h, px: img.px.clone() });
    }
    let (w, h) = level_dims(img.w, img.h, l);
    jpeg::resize(Rgb { w: img.w, h: img.h, px: img.px.clone() }, w, h)
}

/// Tile (tx, ty) of a level image, with its apron (mirrored at the edges).
pub fn cut(img: &Rgb, rect: [usize; 4]) -> Rgb {
    let [x, y, tw, th] = rect;
    let (w, h) = (tw + 2 * APRON, th + 2 * APRON);
    let mirror = |v: isize, n: usize| -> usize {
        let n = n as isize;
        let m = if v < 0 { -v - 1 } else if v >= n { 2 * n - v - 1 } else { v };
        m.clamp(0, n - 1) as usize
    };
    let mut px = vec![0u8; w * h * 3];
    px.par_chunks_mut(w * 3).enumerate().for_each(|(ry, row)| {
        let sy = mirror(y as isize + ry as isize - APRON as isize, img.h);
        let src = &img.px[sy * img.w * 3..(sy + 1) * img.w * 3];
        // The interior in one copy; the apron columns one by one.
        let x0 = x as isize - APRON as isize;
        for rx in 0..w {
            let sx = mirror(x0 + rx as isize, img.w);
            row[rx * 3..rx * 3 + 3].copy_from_slice(&src[sx * 3..sx * 3 + 3]);
        }
    });
    Rgb { w, h, px }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> Rgb {
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                px.extend([(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) % 256) as u8]);
            }
        }
        Rgb { w, h, px }
    }

    #[test]
    fn levels_and_tiles() {
        // 45 MP: 8192 x 5464 -> levels of 8192, 4096 and 2048... only while above 2048.
        let p = Pyramid::new(8192, 5464);
        assert_eq!(p.levels, 2);
        assert_eq!(p.dims(1), (4096, 2732));
        assert_eq!(Pyramid::new(2048, 1365).levels, 0, "a preview is as sharp");
        assert_eq!(Pyramid::new(2049, 1000).levels, 1);
        assert_eq!(Pyramid::new(100_000, 100).levels, MAX_LEVELS);
        // Odd sizes round up, as libjpeg-turbo's scaling does.
        assert_eq!(level_dims(6001, 4001, 1), (3001, 2001));
        let p = Pyramid::new(6000, 4000);
        assert_eq!(p.tile_rect(0, 11, 7).unwrap(), [5632, 3584, 368, 416]);
        assert!(p.tile_rect(0, 12, 0).is_err());
        assert!(p.tile_rect(p.levels, 0, 0).is_err());
    }

    #[test]
    fn tiles_carry_their_neighbours_and_mirror_at_edges() {
        let img = gradient(1100, 700);
        let p = Pyramid::new(img.w, img.h);
        let _ = p;
        let rect = [512, 512, 512, 188];
        let t = cut(&img, rect);
        assert_eq!((t.w, t.h), (512 + 2 * APRON, 188 + 2 * APRON));
        let at = |im: &Rgb, x: usize, y: usize| im.px[(y * im.w + x) * 3..(y * im.w + x) * 3 + 3].to_vec();
        // Interior: the image itself, shifted by the apron.
        assert_eq!(at(&t, APRON, APRON), at(&img, 512, 512));
        assert_eq!(at(&t, APRON + 100, APRON + 50), at(&img, 612, 562));
        // The apron above is the row above; below the last row it mirrors.
        assert_eq!(at(&t, APRON, 0), at(&img, 512, 511));
        assert_eq!(at(&t, APRON, t.h - 1), at(&img, 512, 699));
        // The first tile mirrors at the left and top.
        let t0 = cut(&img, [0, 0, 512, 512]);
        assert_eq!(at(&t0, 0, 0), at(&img, 0, 0));
    }

    #[test]
    fn jpeg_levels_match_their_sizes() {
        let img = gradient(1201, 801);
        let data = jpeg::encode(&img, 90, false).unwrap();
        for l in 0..3 {
            let d = decode_jpeg_level(&data, l).unwrap();
            assert_eq!((d.w, d.h), level_dims(1201, 801, l), "level {l}");
            let h = halve_to(&img, l).unwrap();
            assert_eq!((h.w, h.h), level_dims(1201, 801, l));
        }
    }
}
