//! Colour conversion to sRGB for decoded HEIC/HEIF images.
//!
//! Everything else in Hyperfocal is sRGB, but HEIC files rarely are: iPhones write
//! Display P3 (as an ICC profile or an `nclx` box), and cameras that shoot HEIF
//! (Canon "HDR PQ", Sony HLG) write 10-bit BT.2020 HDR. This converts the decoded
//! pixels to sRGB: matrix/TRC ICC profiles and the common `nclx` primaries and
//! transfer functions, with HDR (PQ, HLG) tone-mapped to SDR around the BT.2408
//! reference white of 203 nits.

use rayon::prelude::*;

type M3 = [[f64; 3]; 3];

/// ICC profile connection space white (D50).
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];
const D65_XY: (f64, f64) = (0.3127, 0.3290);
/// HDR reference white (ITU-R BT.2408), in nits.
const REF_WHITE_NITS: f64 = 203.0;
/// Peak assumed for HDR content (most HDR stills are mastered to 1000 nits).
const HDR_PEAK_NITS: f64 = 1000.0;
/// Linear sRGB values up to this level pass through the HDR tone curve unchanged.
const KNEE: f32 = 0.75;
const OUT_STEPS: usize = 16384;

#[derive(Debug, Clone, PartialEq)]
pub enum Curve {
    Srgb,
    Gamma(f64),
    Linear,
    /// ICC parametric curve: function type 0..=4 and its parameters (g, a, b, c, d, e, f).
    Param(u16, [f64; 7]),
    /// ICC sampled curve, evenly spaced over 0..1.
    Table(Vec<f64>),
    /// SMPTE ST 2084; linear output is relative to the 203-nit reference white.
    Pq,
    /// ARIB STD-B67 inverse OETF (scene light 0..1); the system gamma is applied per pixel.
    Hlg,
}

fn srgb_eotf(v: f64) -> f64 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn srgb_oetf(v: f64) -> f64 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

impl Curve {
    /// Encoded value (0..1) to linear light.
    pub fn to_linear(&self, v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        match self {
            Curve::Srgb => srgb_eotf(v),
            Curve::Gamma(g) => v.powf(*g),
            Curve::Linear => v,
            Curve::Param(t, p) => {
                let [g, a, b, c, d, e, f] = *p;
                let pw = |x: f64| (a * x + b).max(0.0).powf(g);
                // For types 1 and 2 the curve starts where a*x + b crosses zero.
                let start = if a != 0.0 { -b / a } else { 0.0 };
                match t {
                    0 => v.powf(g),
                    1 => if v >= start { pw(v) } else { 0.0 },
                    2 => if v >= start { pw(v) + c } else { c },
                    3 => if v >= d { pw(v) } else { c * v },
                    _ => if v >= d { pw(v) + e } else { c * v + f },
                }
            }
            Curve::Table(t) => match t.len() {
                0 => v,
                1 => t[0],
                n => {
                    let x = v * (n - 1) as f64;
                    let i = (x.floor() as usize).min(n - 2);
                    let w = x - i as f64;
                    t[i] * (1.0 - w) + t[i + 1] * w
                }
            },
            Curve::Pq => {
                const M1: f64 = 2610.0 / 16384.0;
                const M2: f64 = 2523.0 / 4096.0 * 128.0;
                const C1: f64 = 3424.0 / 4096.0;
                const C2: f64 = 2413.0 / 4096.0 * 32.0;
                const C3: f64 = 2392.0 / 4096.0 * 32.0;
                let p = v.powf(1.0 / M2);
                let nits = ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1) * 10000.0;
                nits / REF_WHITE_NITS
            }
            Curve::Hlg => {
                const A: f64 = 0.178_832_77;
                const B: f64 = 0.284_668_92;
                const C: f64 = 0.559_910_73;
                if v <= 0.5 { v * v / 3.0 } else { (((v - C) / A).exp() + B) / 12.0 }
            }
        }
    }

    pub fn is_hdr(&self) -> bool {
        matches!(self, Curve::Pq | Curve::Hlg)
    }

    fn close_to_srgb(&self) -> bool {
        (0..=64).all(|i| {
            let x = i as f64 / 64.0;
            (self.to_linear(x) - srgb_eotf(x)).abs() < 2e-3
        })
    }
}

/// A source colour space: RGB to XYZ (D50-adapted, as in ICC) plus transfer curves.
#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub to_xyz_d50: M3,
    pub curves: [Curve; 3],
}

fn mul(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for (i, row) in o.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    o
}

fn mul_v(a: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2])
}

fn inv(m: &M3) -> Option<M3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

fn xy_to_xyz((x, y): (f64, f64)) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// RGB to XYZ (relative to the space's own white) from chromaticities.
fn rgb_to_xyz(prim: [(f64, f64); 3], white: (f64, f64)) -> Option<M3> {
    let [r, g, b] = prim.map(xy_to_xyz);
    let p = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = mul_v(&inv(&p)?, xy_to_xyz(white));
    Some([0, 1, 2].map(|i| [p[i][0] * s[0], p[i][1] * s[1], p[i][2] * s[2]]))
}

/// Bradford chromatic adaptation between two white points (XYZ, Y = 1).
fn bradford(from: [f64; 3], to: [f64; 3]) -> M3 {
    const MA: M3 = [[0.8951, 0.2664, -0.1614], [-0.7502, 1.7135, 0.0367], [0.0389, -0.0685, 1.0296]];
    let mi = inv(&MA).expect("Bradford matrix is invertible");
    let (a, b) = (mul_v(&MA, from), mul_v(&MA, to));
    let d = [[b[0] / a[0], 0.0, 0.0], [0.0, b[1] / a[1], 0.0], [0.0, 0.0, b[2] / a[2]]];
    mul(&mi, &mul(&d, &MA))
}

const SRGB_PRIMARIES: [(f64, f64); 3] = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06)];

fn srgb_to_xyz_d50() -> M3 {
    let m = rgb_to_xyz(SRGB_PRIMARIES, D65_XY).expect("sRGB primaries");
    mul(&bradford(xy_to_xyz(D65_XY), D50), &m)
}

impl Source {
    pub fn srgb() -> Source {
        Source { name: "sRGB".into(), to_xyz_d50: srgb_to_xyz_d50(), curves: [Curve::Srgb, Curve::Srgb, Curve::Srgb] }
    }

    /// From an `nclx` colour box (ITU-T H.273 codes). Unspecified or unknown
    /// primaries mean BT.709/sRGB.
    pub fn from_nclx(primaries: u16, transfer: u16) -> Source {
        let (name, prim, white) = match primaries {
            4 => ("BT.470M", [(0.67, 0.33), (0.21, 0.71), (0.14, 0.08)], (0.310, 0.316)),
            5 => ("BT.601 625", [(0.64, 0.33), (0.29, 0.60), (0.15, 0.06)], D65_XY),
            6 | 7 => ("BT.601 525", [(0.630, 0.340), (0.310, 0.595), (0.155, 0.070)], D65_XY),
            9 => ("BT.2020", [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)], D65_XY),
            11 => ("DCI-P3", [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)], (0.314, 0.351)),
            12 => ("Display P3", [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)], D65_XY),
            _ => ("BT.709", SRGB_PRIMARIES, D65_XY),
        };
        let curve = match transfer {
            4 => Curve::Gamma(2.2),
            5 => Curve::Gamma(2.8),
            8 => Curve::Linear,
            16 => Curve::Pq,
            18 => Curve::Hlg,
            // sRGB, and the camera-style SDR curves (BT.709/601/2020), which viewers
            // display with an sRGB-like curve.
            _ => Curve::Srgb,
        };
        let suffix = match curve {
            Curve::Pq => " PQ",
            Curve::Hlg => " HLG",
            _ => "",
        };
        let m = rgb_to_xyz(prim, white).map(|m| mul(&bradford(xy_to_xyz(white), D50), &m)).unwrap_or_else(srgb_to_xyz_d50);
        Source { name: format!("{name}{suffix} (nclx)"), to_xyz_d50: m, curves: [curve.clone(), curve.clone(), curve] }
    }

    /// From a matrix/TRC RGB ICC profile. Returns None for anything else (LUT-based
    /// profiles, non-RGB spaces, malformed data).
    pub fn from_icc(icc: &[u8]) -> Option<Source> {
        let be32 = |o: usize| -> Option<u32> { Some(u32::from_be_bytes(icc.get(o..o + 4)?.try_into().ok()?)) };
        if icc.len() < 132 || icc.get(36..40)? != b"acsp" || icc.get(16..20)? != b"RGB " || icc.get(20..24)? != b"XYZ " {
            return None;
        }
        let count = be32(128)? as usize;
        if count > 256 {
            return None;
        }
        let tag = |sig: &[u8; 4]| -> Option<&[u8]> {
            (0..count).find_map(|i| {
                let o = 132 + i * 12;
                if icc.get(o..o + 4)? != sig {
                    return None;
                }
                let (off, len) = (be32(o + 4)? as usize, be32(o + 8)? as usize);
                icc.get(off..off.checked_add(len)?)
            })
        };
        let s15 = |d: &[u8], o: usize| -> Option<f64> { Some(i32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?) as f64 / 65536.0) };
        let xyz = |sig: &[u8; 4]| -> Option<[f64; 3]> {
            let d = tag(sig)?;
            if d.get(0..4)? != b"XYZ " {
                return None;
            }
            Some([s15(d, 8)?, s15(d, 12)?, s15(d, 16)?])
        };
        let curve = |sig: &[u8; 4]| -> Option<Curve> {
            let d = tag(sig)?;
            match d.get(0..4)? {
                b"curv" => {
                    let n = u32::from_be_bytes(d.get(8..12)?.try_into().ok()?) as usize;
                    match n {
                        0 => Some(Curve::Linear),
                        1 => Some(Curve::Gamma(u16::from_be_bytes(d.get(12..14)?.try_into().ok()?) as f64 / 256.0)),
                        n if n <= 65536 => {
                            let t: Option<Vec<f64>> =
                                (0..n).map(|i| Some(u16::from_be_bytes(d.get(12 + i * 2..14 + i * 2)?.try_into().ok()?) as f64 / 65535.0)).collect();
                            Some(Curve::Table(t?))
                        }
                        _ => None,
                    }
                }
                b"para" => {
                    let t = u16::from_be_bytes(d.get(8..10)?.try_into().ok()?);
                    let n = *[1usize, 3, 4, 5, 7].get(t as usize)?;
                    let mut p = [0.0; 7];
                    for (i, v) in p.iter_mut().take(n).enumerate() {
                        *v = s15(d, 12 + i * 4)?;
                    }
                    Some(Curve::Param(t, p))
                }
                _ => None,
            }
        };
        let (r, g, b) = (xyz(b"rXYZ")?, xyz(b"gXYZ")?, xyz(b"bXYZ")?);
        let rc = curve(b"rTRC")?;
        let gc = curve(b"gTRC").unwrap_or_else(|| rc.clone());
        let bc = curve(b"bTRC").unwrap_or_else(|| rc.clone());
        let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        if m.iter().flatten().any(|v| !v.is_finite()) || inv(&m).is_none() {
            return None;
        }
        let name = tag(b"desc").and_then(icc_text).unwrap_or_else(|| "ICC profile".into());
        Some(Source { name, to_xyz_d50: m, curves: [rc, gc, bc] })
    }
}

/// Text of a `desc` (ICC v2) or `mluc` (v4) tag, for logs.
fn icc_text(d: &[u8]) -> Option<String> {
    let be32 = |o: usize| -> Option<usize> { Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?) as usize) };
    let s = match d.get(0..4)? {
        b"desc" => {
            let n = be32(8)?.min(256);
            String::from_utf8_lossy(d.get(12..12 + n)?).trim_end_matches('\0').trim().to_string()
        }
        b"mluc" => {
            let (len, off) = (be32(20)?.min(512), be32(24)?);
            let u: Vec<u16> = d.get(off..off + len)?.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u).trim().to_string()
        }
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

/// Picks the colour description of an image: an HDR `nclx` box wins (ICC cannot
/// describe PQ/HLG), then a usable ICC profile, then `nclx`, then sRGB.
pub fn choose(nclx: Option<(u16, u16)>, icc: Option<&[u8]>) -> Source {
    let n = nclx.map(|(p, t)| Source::from_nclx(p, t));
    if let Some(n) = &n
        && n.curves[0].is_hdr()
    {
        return n.clone();
    }
    icc.and_then(Source::from_icc).or(n).unwrap_or_else(Source::srgb)
}

/// Converts decoded pixels from a source space to 8-bit sRGB.
pub struct Converter {
    identity: bool,
    bits: u32,
    lin: [Vec<f32>; 3],
    m: [[f32; 3]; 3],
    hdr: bool,
    hlg: bool,
    out: Vec<u8>,
}

/// Highlight compression for HDR: identity up to the knee, then a smooth curve
/// (continuous slope) reaching 1.0 at `peak`.
pub fn tonemap(x: f32, peak: f32) -> f32 {
    if x <= KNEE {
        return x;
    }
    if x >= peak {
        return 1.0;
    }
    let t = (x - KNEE) / (peak - KNEE);
    let a = (peak - KNEE) / (1.0 - KNEE);
    KNEE + (1.0 - KNEE) * (a * t) / (1.0 + (a - 1.0) * t)
}

impl Converter {
    /// `bits`: significant bits of the input samples (8 for 8-bit data).
    pub fn new(src: &Source, bits: u32) -> Converter {
        let bits = bits.clamp(1, 16);
        let to_srgb = inv(&srgb_to_xyz_d50()).expect("sRGB matrix");
        let m = mul(&to_srgb, &src.to_xyz_d50);
        let hdr = src.curves.iter().any(|c| c.is_hdr());
        let near_identity = (0..3).all(|i| (0..3).all(|j| (m[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 2e-3));
        let identity = !hdr && near_identity && src.curves.iter().all(|c| c.close_to_srgb());
        let max = ((1u32 << bits) - 1) as f64;
        let lin = if identity {
            [Vec::new(), Vec::new(), Vec::new()]
        } else {
            src.curves.clone().map(|c| (0..=max as u32).map(|v| c.to_linear(v as f64 / max) as f32).collect())
        };
        let out = (0..OUT_STEPS).map(|i| (srgb_oetf(i as f64 / (OUT_STEPS - 1) as f64) * 255.0).round() as u8).collect();
        Converter {
            identity,
            bits,
            lin,
            m: m.map(|r| r.map(|v| v as f32)),
            hdr,
            hlg: src.curves.contains(&Curve::Hlg),
            out,
        }
    }

    #[cfg(test)]
    pub fn is_identity(&self) -> bool {
        self.identity
    }

    #[inline]
    fn pixel(&self, r: f32, g: f32, b: f32) -> [u8; 3] {
        let (mut r, mut g, mut b) = (r, g, b);
        if self.hlg {
            // HLG system gamma 1.2 (1000-nit display), scaled to the reference white.
            let ys = (0.2627 * r + 0.6780 * g + 0.0593 * b).max(1e-6);
            let k = (HDR_PEAK_NITS / REF_WHITE_NITS) as f32 * ys.powf(0.2);
            r *= k;
            g *= k;
            b *= k;
        }
        let m = &self.m;
        let mut o = [
            (m[0][0] * r + m[0][1] * g + m[0][2] * b).max(0.0),
            (m[1][0] * r + m[1][1] * g + m[1][2] * b).max(0.0),
            (m[2][0] * r + m[2][1] * g + m[2][2] * b).max(0.0),
        ];
        if self.hdr {
            let mx = o[0].max(o[1]).max(o[2]);
            if mx > KNEE {
                let s = tonemap(mx, (HDR_PEAK_NITS / REF_WHITE_NITS) as f32) / mx;
                o = o.map(|v| v * s);
            }
        }
        o.map(|v| self.out[((v.min(1.0) * (OUT_STEPS - 1) as f32) + 0.5) as usize])
    }

    /// Converts 8-bit RGB in place.
    pub fn convert_u8(&self, px: &mut [u8]) {
        if self.identity {
            return;
        }
        let shift = self.bits.saturating_sub(8);
        let (lr, lg, lb) = (&self.lin[0], &self.lin[1], &self.lin[2]);
        px.par_chunks_mut(3 * 4096).for_each(|chunk| {
            for p in chunk.chunks_exact_mut(3) {
                let idx = |v: u8| ((v as usize) << shift).min(lr.len() - 1);
                let o = self.pixel(lr[idx(p[0])], lg[idx(p[1])], lb[idx(p[2])]);
                p.copy_from_slice(&o);
            }
        });
    }

    /// Converts samples of `bits` significant bits (in u16 containers) to 8-bit RGB.
    pub fn convert_u16(&self, src: &[u16], dst: &mut [u8]) {
        let n = src.len().min(dst.len());
        let max = (1u32 << self.bits) - 1;
        if self.identity {
            dst[..n].par_iter_mut().zip(src[..n].par_iter()).for_each(|(d, s)| {
                *d = (((*s).min(max as u16) as u32 * 255 + max / 2) / max) as u8;
            });
            return;
        }
        let (lr, lg, lb) = (&self.lin[0], &self.lin[1], &self.lin[2]);
        let top = lr.len() - 1;
        dst[..n - n % 3].par_chunks_mut(3 * 4096).zip(src[..n - n % 3].par_chunks(3 * 4096)).for_each(|(d, s)| {
            for (o, i) in d.chunks_exact_mut(3).zip(s.chunks_exact(3)) {
                let p = self.pixel(lr[(i[0] as usize).min(top)], lg[(i[1] as usize).min(top)], lb[(i[2] as usize).min(top)]);
                o.copy_from_slice(&p);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv_matrix(src: &Source) -> M3 {
        mul(&inv(&srgb_to_xyz_d50()).unwrap(), &src.to_xyz_d50)
    }

    #[test]
    fn srgb_is_identity() {
        assert!(Converter::new(&Source::srgb(), 8).is_identity());
        assert!(Converter::new(&Source::from_nclx(1, 13), 8).is_identity());
        assert!(Converter::new(&Source::from_nclx(2, 2), 8).is_identity());
        assert!(!Converter::new(&Source::from_nclx(12, 13), 8).is_identity());
        assert!(!Converter::new(&Source::from_nclx(9, 16), 10).is_identity());
        let mut px = vec![10u8, 128, 250];
        Converter::new(&Source::srgb(), 8).convert_u8(&mut px);
        assert_eq!(px, vec![10, 128, 250]);
        let mut out = [0u8; 3];
        Converter::new(&Source::srgb(), 10).convert_u16(&[0, 512, 1023], &mut out);
        assert_eq!(out, [0, 128, 255]);
    }

    #[test]
    fn display_p3_matrix() {
        // The well-known linear Display P3 -> linear sRGB matrix.
        let want = [[1.224_940, -0.224_940, 0.0], [-0.042_057, 1.042_057, 0.0], [-0.019_638, -0.078_636, 1.098_274]];
        let got = conv_matrix(&Source::from_nclx(12, 13));
        for i in 0..3 {
            for j in 0..3 {
                assert!((got[i][j] - want[i][j]).abs() < 1e-3, "{got:?}");
            }
        }
    }

    #[test]
    fn display_p3_pixels() {
        let c = Converter::new(&Source::from_nclx(12, 13), 8);
        // White and greys stay neutral; P3 red is clipped to sRGB red.
        let mut px = vec![255, 255, 255, 128, 128, 128, 0, 0, 0, 255, 0, 0];
        c.convert_u8(&mut px);
        assert_eq!(&px[0..3], &[255, 255, 255]);
        assert!(px[3..6].iter().all(|v| (*v as i32 - 128).abs() <= 1), "{px:?}");
        assert_eq!(&px[6..9], &[0, 0, 0]);
        assert_eq!(&px[9..12], &[255, 0, 0]);
        // An in-gamut colour comes back where it started.
        let p3_from_srgb = inv(&conv_matrix(&Source::from_nclx(12, 13))).unwrap();
        let lin = [200u8, 100, 50].map(|v| srgb_eotf(v as f64 / 255.0));
        let p3 = mul_v(&p3_from_srgb, lin).map(|v| (srgb_oetf(v) * 255.0).round() as u8);
        let mut px = p3.to_vec();
        c.convert_u8(&mut px);
        for (a, b) in px.iter().zip([200u8, 100, 50]) {
            assert!((*a as i32 - b as i32).abs() <= 2, "{px:?} from {p3:?}");
        }
    }

    /// A minimal ICC v4 Display P3 profile: D50 colorants and the sRGB curve.
    fn display_p3_icc() -> Vec<u8> {
        let s15 = |v: f64| ((v * 65536.0).round() as i32).to_be_bytes();
        let xyz = |x: f64, y: f64, z: f64| [b"XYZ ".to_vec(), vec![0; 4], s15(x).to_vec(), s15(y).to_vec(), s15(z).to_vec()].concat();
        let para = [
            b"para".to_vec(),
            vec![0; 4],
            3u16.to_be_bytes().to_vec(),
            vec![0; 2],
            [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045].iter().flat_map(|v| s15(*v)).collect(),
        ]
        .concat();
        let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
            (b"rXYZ", xyz(0.515_1, 0.241_2, -0.001_1)),
            (b"gXYZ", xyz(0.292_0, 0.692_2, 0.041_9)),
            (b"bXYZ", xyz(0.157_1, 0.066_6, 0.784_1)),
            (b"rTRC", para.clone()),
            (b"gTRC", para.clone()),
            (b"bTRC", para),
        ];
        let mut data = Vec::new();
        let mut table = Vec::new();
        let base = 132 + tags.len() * 12;
        for (sig, d) in &tags {
            table.extend_from_slice(*sig);
            table.extend_from_slice(&((base + data.len()) as u32).to_be_bytes());
            table.extend_from_slice(&(d.len() as u32).to_be_bytes());
            data.extend_from_slice(d);
            while data.len() % 4 != 0 {
                data.push(0);
            }
        }
        let mut h = vec![0u8; 128];
        h[16..20].copy_from_slice(b"RGB ");
        h[20..24].copy_from_slice(b"XYZ ");
        h[36..40].copy_from_slice(b"acsp");
        let mut icc = [h, (tags.len() as u32).to_be_bytes().to_vec(), table, data].concat();
        let n = icc.len() as u32;
        icc[0..4].copy_from_slice(&n.to_be_bytes());
        icc
    }

    #[test]
    fn icc_display_p3() {
        let src = Source::from_icc(&display_p3_icc()).expect("parses");
        let want = conv_matrix(&Source::from_nclx(12, 13));
        let got = conv_matrix(&src);
        for i in 0..3 {
            for j in 0..3 {
                assert!((got[i][j] - want[i][j]).abs() < 4e-3, "{got:?}");
            }
        }
        assert!(src.curves.iter().all(|c| c.close_to_srgb()));
        // nclx PQ wins over an ICC profile; otherwise the profile wins.
        assert!(choose(Some((9, 16)), Some(&display_p3_icc())).curves[0].is_hdr());
        assert_eq!(choose(Some((1, 13)), Some(&display_p3_icc())).name, "ICC profile");
        assert_eq!(choose(None, None).name, "sRGB");
    }

    #[test]
    fn icc_garbage_never_panics() {
        let icc = display_p3_icc();
        for n in 0..icc.len() {
            let _ = Source::from_icc(&icc[..n]);
        }
        let mut x = 0x1234_5678u32;
        for _ in 0..2000 {
            let mut v = icc.clone();
            for _ in 0..4 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let i = x as usize % v.len();
                v[i] = (x >> 8) as u8;
            }
            let _ = Source::from_icc(&v);
        }
        assert!(Source::from_icc(b"not a profile").is_none());
    }

    #[test]
    fn hdr_reference_white() {
        // PQ code for 203 nits decodes to 1.0 (the SDR white point).
        let enc = |nits: f64| {
            const M1: f64 = 2610.0 / 16384.0;
            const M2: f64 = 2523.0 / 4096.0 * 128.0;
            const C1: f64 = 3424.0 / 4096.0;
            const C2: f64 = 2413.0 / 4096.0 * 32.0;
            const C3: f64 = 2392.0 / 4096.0 * 32.0;
            let y = (nits / 10000.0).powf(M1);
            ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
        };
        assert!((Curve::Pq.to_linear(enc(203.0)) - 1.0).abs() < 1e-6);
        assert!((Curve::Pq.to_linear(enc(1000.0)) - 1000.0 / 203.0).abs() < 1e-6);
        // HLG 75% grey is the reference white too; both land on the same SDR grey.
        let pq = Converter::new(&Source::from_nclx(9, 16), 10);
        let hlg = Converter::new(&Source::from_nclx(9, 18), 10);
        let code = (enc(203.0) * 1023.0).round() as u16;
        let (mut a, mut b) = ([0u8; 3], [0u8; 3]);
        pq.convert_u16(&[code; 3], &mut a);
        hlg.convert_u16(&[767; 3], &mut b);
        assert!((a[0] as i32 - b[0] as i32).abs() <= 3, "{a:?} {b:?}");
        assert!(a[0] > 225 && a[0] < 250, "{a:?}");
        // Brighter than the reference white is compressed, not clipped flat.
        let mut c = [0u8; 3];
        pq.convert_u16(&[(enc(600.0) * 1023.0).round() as u16; 3], &mut c);
        assert!(c[0] > a[0] && c[0] < 255, "{c:?}");
    }

    #[test]
    fn tone_curve_is_smooth() {
        let peak = 4.9;
        let mut last = 0.0;
        for i in 0..=1000 {
            let x = i as f32 / 1000.0 * 6.0;
            let y = tonemap(x, peak);
            assert!(y >= last - 1e-6 && y <= 1.0, "{x} -> {y}");
            last = y;
        }
        assert_eq!(tonemap(0.5, peak), 0.5);
        assert!((tonemap(peak, peak) - 1.0).abs() < 1e-6);
        let e = 1e-3;
        let slope = (tonemap(KNEE + e, peak) - tonemap(KNEE, peak)) / e;
        assert!((slope - 1.0).abs() < 0.01, "{slope}");
    }
}
