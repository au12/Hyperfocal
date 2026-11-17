//! Reading metadata from image files and XMP sidecars. Reading only: nothing in
//! this module ever opens a file for writing.

pub mod exiftool;
pub mod xmp;

use crate::model::{Kind, PhotoRec, taken_to_secs};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use xmp::{Xmp, label_color};

/// Metadata found inside the image file itself.
#[derive(Debug, Default, Clone)]
pub struct FileMeta {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: u8,
    pub taken: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub focal: Option<f64>,
    pub fnum: Option<f64>,
    pub exposure: Option<f64>,
    pub iso: Option<u32>,
    pub bias: Option<f64>,
    pub creator: Option<String>,
    pub rights: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt: Option<f64>,
    pub xmp: Option<Xmp>,
}

fn clean(s: &str) -> Option<String> {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if t.is_empty() { None } else { Some(t.to_string()) }
}

/// Read the metadata embedded in a file (EXIF + embedded XMP).
pub fn read_file_meta(path: &Path, kind: Kind) -> Result<FileMeta, String> {
    match kind {
        Kind::Raw => read_raw_meta(path),
        Kind::Jpeg => read_jpeg_meta(path),
        _ => read_container_meta(path),
    }
}

fn read_raw_meta(path: &Path) -> Result<FileMeta, String> {
    let raw = flashraw::FlashRaw::open_path(path).map_err(|e| e.to_string())?;
    let m = raw.metadata();
    let mut out = FileMeta {
        width: Some(m.width).filter(|w| *w > 0),
        height: Some(m.height).filter(|h| *h > 0),
        orientation: m.orientation.exif() as u8,
        taken: m.capture_time.clone(),
        make: clean(&m.canonical_make).or_else(|| clean(&m.make)),
        model: clean(&m.canonical_model).or_else(|| clean(&m.model)),
        lens: m.lens.model.as_deref().and_then(clean),
        focal: m.exposure.focal_length.map(|v| v as f64),
        fnum: m.exposure.f_number.map(|v| v as f64),
        exposure: m.exposure.exposure_time,
        iso: m.exposure.iso,
        bias: m.exposure.exposure_bias.map(|v| v as f64),
        creator: m.artist.as_deref().and_then(clean),
        rights: m.copyright.as_deref().and_then(clean),
        ..Default::default()
    };
    if let Some(g) = &m.gps {
        if let (Some(lat), Some(lon)) = (g.latitude, g.longitude) {
            if lat.is_finite() && lon.is_finite() && !(lat == 0.0 && lon == 0.0) {
                out.lat = Some(lat);
                out.lon = Some(lon);
                out.alt = g.altitude;
            }
        }
    }
    if let Some(x) = raw.xmp_bytes() {
        let p = Xmp::parse(x);
        if !p.is_empty() {
            out.xmp = Some(p);
        }
    }
    Ok(out)
}

/// Header segments of a JPEG, read without touching the compressed image data.
#[derive(Default)]
pub struct JpegHeaders {
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub fn read_jpeg_headers<R: Read + Seek>(r: &mut R) -> std::io::Result<JpegHeaders> {
    let mut h = JpegHeaders::default();
    let mut b2 = [0u8; 2];
    r.read_exact(&mut b2)?;
    if b2 != [0xFF, 0xD8] {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a JPEG"));
    }
    for _ in 0..512 {
        // Find the next marker (skip fill bytes).
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        if b[0] != 0xFF {
            break;
        }
        let mut marker = 0xFFu8;
        while marker == 0xFF {
            r.read_exact(&mut b)?;
            marker = b[0];
        }
        if marker == 0xDA || marker == 0xD9 {
            break; // start of scan / end of image
        }
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue;
        }
        r.read_exact(&mut b2)?;
        let len = u16::from_be_bytes(b2) as usize;
        if len < 2 {
            break;
        }
        let plen = len - 2;
        match marker {
            0xE1 => {
                let mut payload = vec![0u8; plen];
                r.read_exact(&mut payload)?;
                if payload.starts_with(b"Exif\0\0") && h.exif.is_none() {
                    h.exif = Some(payload[6..].to_vec());
                } else if payload.starts_with(b"http://ns.adobe.com/xap/1.0/\0") && h.xmp.is_none() {
                    h.xmp = Some(payload[29..].to_vec());
                }
            }
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                let mut sof = vec![0u8; plen.min(16)];
                r.read_exact(&mut sof)?;
                if sof.len() >= 5 {
                    h.height = Some(u16::from_be_bytes([sof[1], sof[2]]) as u32);
                    h.width = Some(u16::from_be_bytes([sof[3], sof[4]]) as u32);
                }
                if plen > sof.len() {
                    r.seek(SeekFrom::Current((plen - sof.len()) as i64))?;
                }
            }
            _ => {
                r.seek(SeekFrom::Current(plen as i64))?;
            }
        }
    }
    Ok(h)
}

fn read_jpeg_meta(path: &Path) -> Result<FileMeta, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let mut r = BufReader::with_capacity(64 * 1024, f);
    let h = read_jpeg_headers(&mut r).map_err(|e| e.to_string())?;
    let mut out = FileMeta { orientation: 1, width: h.width, height: h.height, ..Default::default() };
    if let Some(ex) = h.exif {
        if let Ok(e) = exif::Reader::new().read_raw(ex) {
            apply_exif(&e, &mut out);
        }
    }
    if let Some(x) = h.xmp {
        let p = Xmp::parse(&x);
        if !p.is_empty() {
            out.xmp = Some(p);
        }
    }
    Ok(out)
}

fn read_container_meta(path: &Path) -> Result<FileMeta, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let mut r = BufReader::new(f);
    let mut out = FileMeta { orientation: 1, ..Default::default() };
    if let Ok(e) = exif::Reader::new().read_from_container(&mut r) {
        apply_exif(&e, &mut out);
    }
    Ok(out)
}

fn apply_exif(e: &exif::Exif, out: &mut FileMeta) {
    use exif::{In, Tag, Value};
    let ascii = |t: Tag| -> Option<String> {
        let f = e.get_field(t, In::PRIMARY)?;
        match &f.value {
            Value::Ascii(v) => v.first().and_then(|s| clean(&String::from_utf8_lossy(s))),
            _ => None,
        }
    };
    let num = |t: Tag| -> Option<f64> {
        let f = e.get_field(t, In::PRIMARY)?;
        match &f.value {
            Value::Rational(v) => v.first().map(|r| r.to_f64()),
            Value::SRational(v) => v.first().map(|r| r.to_f64()),
            Value::Short(v) => v.first().map(|x| *x as f64),
            Value::Long(v) => v.first().map(|x| *x as f64),
            _ => None,
        }
    };
    let rationals = |t: Tag| -> Option<Vec<f64>> {
        let f = e.get_field(t, In::PRIMARY)?;
        match &f.value {
            Value::Rational(v) => Some(v.iter().map(|r| r.to_f64()).collect()),
            _ => None,
        }
    };
    out.make = ascii(Tag::Make).or(out.make.take());
    out.model = ascii(Tag::Model).or(out.model.take());
    if let (Some(make), Some(model)) = (&out.make, &out.model) {
        if !model.to_lowercase().starts_with(&make.split_whitespace().next().unwrap_or("").to_lowercase()) {
            out.model = Some(format!("{make} {model}"));
        }
    }
    out.lens = ascii(Tag::LensModel).or(out.lens.take());
    out.focal = num(Tag::FocalLength).or(out.focal);
    out.fnum = num(Tag::FNumber).or(out.fnum);
    out.exposure = num(Tag::ExposureTime).or(out.exposure);
    out.iso = num(Tag::PhotographicSensitivity).map(|v| v as u32).or(out.iso);
    out.bias = num(Tag::ExposureBiasValue).or(out.bias);
    out.creator = ascii(Tag::Artist).or(out.creator.take());
    out.rights = ascii(Tag::Copyright).or(out.rights.take());
    if let Some(o) = num(Tag::Orientation) {
        let o = o as u8;
        if (1..=8).contains(&o) {
            out.orientation = o;
        }
    }
    if out.width.is_none() {
        out.width = num(Tag::PixelXDimension).or_else(|| num(Tag::ImageWidth)).map(|v| v as u32);
        out.height = num(Tag::PixelYDimension).or_else(|| num(Tag::ImageLength)).map(|v| v as u32);
    }
    if let Some(dt) = ascii(Tag::DateTimeOriginal).or_else(|| ascii(Tag::DateTimeDigitized)).or_else(|| ascii(Tag::DateTime)) {
        // "YYYY:MM:DD HH:MM:SS" -> "YYYY-MM-DDTHH:MM:SS"
        if dt.len() >= 19 {
            let mut s = format!("{}-{}-{}T{}", &dt[0..4], &dt[5..7], &dt[8..10], &dt[11..19]);
            if let Some(off) = ascii(Tag::OffsetTimeOriginal) {
                s.push_str(&off);
            }
            out.taken = Some(s);
        }
    }
    if let (Some(lat), Some(lon)) = (rationals(Tag::GPSLatitude), rationals(Tag::GPSLongitude)) {
        let dms = |v: &[f64]| -> f64 {
            v.first().copied().unwrap_or(0.0) + v.get(1).copied().unwrap_or(0.0) / 60.0 + v.get(2).copied().unwrap_or(0.0) / 3600.0
        };
        let mut la = dms(&lat);
        let mut lo = dms(&lon);
        if ascii(Tag::GPSLatitudeRef).as_deref() == Some("S") {
            la = -la;
        }
        if ascii(Tag::GPSLongitudeRef).as_deref() == Some("W") {
            lo = -lo;
        }
        if la.is_finite() && lo.is_finite() && !(la == 0.0 && lo == 0.0) {
            out.lat = Some(la);
            out.lon = Some(lo);
            out.alt = num(Tag::GPSAltitude).map(|a| {
                let below = e
                    .get_field(Tag::GPSAltitudeRef, In::PRIMARY)
                    .and_then(|f| f.value.get_uint(0))
                    .map(|v| v == 1)
                    .unwrap_or(false);
                if below { -a } else { a }
            });
        }
    }
}

/// Merge file metadata and XMP layers into a record (sidecar wins over embedded XMP,
/// embedded XMP wins over EXIF).
pub fn merge_into(rec: &mut PhotoRec, file: &FileMeta, sidecar: Option<&Xmp>) {
    rec.width = file.width.map(|v| v as i64);
    rec.height = file.height.map(|v| v as i64);
    rec.orientation = file.orientation.max(1) as i32;
    rec.taken_str = file.taken.clone();
    rec.make = file.make.clone();
    rec.model = file.model.clone();
    rec.lens = file.lens.clone();
    rec.focal = file.focal;
    rec.fnum = file.fnum;
    rec.exposure = file.exposure;
    rec.iso = file.iso.map(|v| v as i64);
    rec.bias = file.bias;
    rec.creator = file.creator.clone();
    rec.rights = file.rights.clone();
    rec.lat = file.lat;
    rec.lon = file.lon;
    rec.alt = file.alt;
    rec.rating = 0;
    rec.label = None;
    rec.pick = 0;
    rec.title = None;
    rec.description = None;
    rec.keywords.clear();
    rec.hkeywords.clear();
    rec.city = None;
    rec.state = None;
    rec.country = None;
    rec.country_code = None;
    rec.sublocation = None;

    for layer in [file.xmp.as_ref(), sidecar].into_iter().flatten() {
        apply_xmp(rec, layer);
    }
    rec.taken = rec.taken_str.as_deref().and_then(taken_to_secs);
}

fn apply_xmp(rec: &mut PhotoRec, x: &Xmp) {
    if let Some(v) = x.rating {
        rec.rating = v;
    }
    if x.label.is_some() {
        rec.label = x.label.clone();
    }
    if let Some(v) = x.pick {
        rec.pick = v;
    }
    if x.title.is_some() {
        rec.title = x.title.clone();
    }
    if x.description.is_some() {
        rec.description = x.description.clone();
    }
    if let Some(s) = &x.subject {
        rec.keywords = dedup(s.clone());
    }
    if let Some(s) = &x.hsubject {
        rec.hkeywords = dedup(s.clone());
    }
    if x.creator.is_some() {
        rec.creator = x.creator.clone();
    }
    if x.rights.is_some() {
        rec.rights = x.rights.clone();
    }
    if let (Some(lat), Some(lon)) = (x.lat, x.lon) {
        rec.lat = Some(lat);
        rec.lon = Some(lon);
        rec.alt = x.alt;
    }
    for (dst, src) in [
        (&mut rec.city, &x.city),
        (&mut rec.state, &x.state),
        (&mut rec.country, &x.country),
        (&mut rec.country_code, &x.country_code),
        (&mut rec.sublocation, &x.sublocation),
    ] {
        if src.is_some() {
            *dst = src.clone();
        }
    }
    if let Some(o) = x.orientation {
        rec.orientation = o as i32;
    }
    if rec.taken_str.is_none() {
        rec.taken_str = x.date_taken.clone();
    }
}

fn dedup(v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v.into_iter().filter(|s| seen.insert(s.to_lowercase())).collect()
}

pub fn lcolor(label: Option<&str>) -> Option<&'static str> {
    label.and_then(label_color)
}

/// Read and parse a sidecar file.
pub fn read_sidecar(path: &Path) -> Option<Xmp> {
    let data = std::fs::read(path).ok()?;
    if data.len() > 32 * 1024 * 1024 {
        return None;
    }
    Some(Xmp::parse(&data))
}
