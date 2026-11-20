//! Minimal HEIF (ISO-BMFF) reader for indexing: the primary image's displayed size,
//! EXIF and XMP. Pure Rust and bounds-checked; it never decodes pixels, so indexing
//! a HEIC file needs no native code and no helper process.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Upper bound on the `meta` box and on an EXIF/XMP item.
const MAX_META: u64 = 16 << 20;
const MAX_ITEM: u64 = 16 << 20;

#[derive(Debug, Default)]
pub struct HeifMeta {
    /// Size as displayed: clean-aperture crop and rotation applied.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// EXIF starting at the TIFF header.
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
}

pub fn read(path: &Path) -> Result<HeifMeta, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    read_from(&mut f, len)
}

/// A bounds-checked cursor over a byte slice.
struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cur { b, p: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.p.checked_add(n).filter(|e| *e <= self.b.len()).ok_or("truncated HEIF box")?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().expect("2 bytes")))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().expect("8 bytes")))
    }
    /// An unsigned integer of 0, 4 or 8 bytes (iloc field sizes).
    fn uint(&mut self, size: u8) -> Result<u64, String> {
        match size {
            0 => Ok(0),
            4 => Ok(self.u32()? as u64),
            8 => self.u64(),
            _ => Err(format!("unsupported iloc field size {size}")),
        }
    }
    fn cstr(&mut self) -> Result<&'a [u8], String> {
        let rest = &self.b[self.p..];
        let n = rest.iter().position(|c| *c == 0).ok_or("unterminated string in HEIF box")?;
        self.p += n + 1;
        Ok(&rest[..n])
    }
    fn rest(&self) -> &'a [u8] {
        &self.b[self.p..]
    }
}

/// Child boxes: (type, payload).
type Boxes<'a> = Vec<([u8; 4], &'a [u8])>;

/// The child boxes of a box body.
fn boxes(body: &[u8]) -> Result<Boxes<'_>, String> {
    let mut out = Vec::new();
    let mut c = Cur::new(body);
    while c.p < body.len() {
        let start = c.p;
        let size = c.u32()? as u64;
        let typ: [u8; 4] = c.take(4)?.try_into().expect("4 bytes");
        let size = match size {
            0 => (body.len() - start) as u64,
            1 => c.u64()?,
            s => s,
        };
        let header = (c.p - start) as u64;
        if size < header || size > (body.len() - start) as u64 {
            return Err("malformed HEIF box".into());
        }
        out.push((typ, c.take((size - header) as usize)?));
        if out.len() > 100_000 {
            return Err("too many HEIF boxes".into());
        }
    }
    Ok(out)
}

/// Skips the version/flags of a FullBox, returning them.
fn full(c: &mut Cur) -> Result<(u8, u32), String> {
    let v = c.u32()?;
    Ok(((v >> 24) as u8, v & 0x00ff_ffff))
}

struct Item {
    typ: [u8; 4],
    content_type: Vec<u8>,
}

struct Location {
    method: u16,
    extents: Vec<(u64, u64)>,
}

enum Prop {
    Ispe(u32, u32),
    Irot(u8),
    Clap(u32, u32),
    Other,
}

fn parse_iinf(body: &[u8]) -> Result<HashMap<u32, Item>, String> {
    let mut c = Cur::new(body);
    let (v, _) = full(&mut c)?;
    if v == 0 { c.u16()? as u32 } else { c.u32()? };
    let mut items = HashMap::new();
    for (typ, b) in boxes(c.rest())? {
        if &typ != b"infe" {
            continue;
        }
        let mut c = Cur::new(b);
        let (v, _) = full(&mut c)?;
        if v < 2 {
            continue; // pre-HEIF item entries carry no item type
        }
        let id = if v == 2 { c.u16()? as u32 } else { c.u32()? };
        c.u16()?; // protection index
        let typ: [u8; 4] = c.take(4)?.try_into().expect("4 bytes");
        c.cstr()?; // name
        let content_type = if &typ == b"mime" { c.cstr().unwrap_or_default().to_vec() } else { Vec::new() };
        items.insert(id, Item { typ, content_type });
    }
    Ok(items)
}

fn parse_iloc(body: &[u8]) -> Result<HashMap<u32, Location>, String> {
    let mut c = Cur::new(body);
    let (v, _) = full(&mut c)?;
    if v > 2 {
        return Err(format!("unsupported iloc version {v}"));
    }
    let a = c.u8()?;
    let b = c.u8()?;
    let (off_size, len_size, base_size) = (a >> 4, a & 15, b >> 4);
    let index_size = if v == 1 || v == 2 { b & 15 } else { 0 };
    let count = if v < 2 { c.u16()? as u32 } else { c.u32()? };
    let mut out = HashMap::new();
    for _ in 0..count {
        let id = if v < 2 { c.u16()? as u32 } else { c.u32()? };
        let method = if v == 1 || v == 2 { c.u16()? & 15 } else { 0 };
        c.u16()?; // data reference index
        let base = c.uint(base_size)?;
        let n = c.u16()?;
        if n > 4096 {
            return Err("too many extents".into());
        }
        let mut extents = Vec::with_capacity(n as usize);
        for _ in 0..n {
            c.uint(index_size)?;
            let off = c.uint(off_size)?;
            let len = c.uint(len_size)?;
            extents.push((base.checked_add(off).ok_or("bad iloc offset")?, len));
        }
        out.insert(id, Location { method, extents });
    }
    Ok(out)
}

fn parse_ipco(body: &[u8]) -> Result<Vec<Prop>, String> {
    boxes(body)?
        .into_iter()
        .map(|(typ, b)| {
            let mut c = Cur::new(b);
            Ok(match &typ {
                b"ispe" => {
                    full(&mut c)?;
                    Prop::Ispe(c.u32()?, c.u32()?)
                }
                b"irot" => Prop::Irot(c.u8()? & 3),
                b"clap" => {
                    let (wn, wd, hn, hd) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
                    if wd == 0 || hd == 0 {
                        Prop::Other
                    } else {
                        Prop::Clap(((wn as f64 / wd as f64).round()) as u32, ((hn as f64 / hd as f64).round()) as u32)
                    }
                }
                _ => Prop::Other,
            })
        })
        .collect()
}

fn parse_ipma(body: &[u8]) -> Result<HashMap<u32, Vec<u16>>, String> {
    let mut c = Cur::new(body);
    let (v, flags) = full(&mut c)?;
    let count = c.u32()?;
    let mut out = HashMap::new();
    for _ in 0..count {
        let id = if v < 1 { c.u16()? as u32 } else { c.u32()? };
        let n = c.u8()?;
        let mut props = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let idx = if flags & 1 != 0 { c.u16()? & 0x7fff } else { (c.u8()? & 0x7f) as u16 };
            props.push(idx);
        }
        out.insert(id, props);
    }
    Ok(out)
}

/// The body of the top-level `meta` box (usually right after `ftyp`, before `mdat`).
fn meta_box<R: Read + Seek>(r: &mut R, len: u64) -> Result<Vec<u8>, String> {
    let mut pos = 0u64;
    let mut meta = None;
    for _ in 0..64 {
        if pos.saturating_add(8) > len {
            break;
        }
        r.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
        let mut h = [0u8; 8];
        r.read_exact(&mut h).map_err(|e| e.to_string())?;
        let mut size = u32::from_be_bytes(h[..4].try_into().expect("4 bytes")) as u64;
        let mut header = 8u64;
        if size == 1 {
            let mut l = [0u8; 8];
            r.read_exact(&mut l).map_err(|e| e.to_string())?;
            size = u64::from_be_bytes(l);
            header = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header || size > len - pos {
            return Err("malformed HEIF file".into());
        }
        if pos == 0 && &h[4..8] != b"ftyp" {
            return Err("not a HEIF file".into());
        }
        if &h[4..8] == b"meta" {
            if size - header > MAX_META {
                return Err("HEIF meta box too large".into());
            }
            let mut body = vec![0u8; (size - header) as usize];
            r.read_exact(&mut body).map_err(|e| e.to_string())?;
            meta = Some(body);
            break;
        }
        pos += size;
    }
    meta.ok_or_else(|| "no meta box in HEIF file".into())
}

pub fn read_from<R: Read + Seek>(r: &mut R, len: u64) -> Result<HeifMeta, String> {
    let meta = meta_box(r, len)?;
    let mut c = Cur::new(&meta);
    full(&mut c)?;

    let (mut primary, mut items, mut locs, mut props, mut assoc, mut idat) = (None, HashMap::new(), HashMap::new(), Vec::new(), HashMap::new(), None);
    for (typ, b) in boxes(c.rest())? {
        match &typ {
            b"pitm" => {
                let mut c = Cur::new(b);
                let (v, _) = full(&mut c)?;
                primary = Some(if v == 0 { c.u16()? as u32 } else { c.u32()? });
            }
            b"iinf" => items = parse_iinf(b)?,
            b"iloc" => locs = parse_iloc(b)?,
            b"iprp" => {
                for (t, b) in boxes(b)? {
                    match &t {
                        b"ipco" => props = parse_ipco(b)?,
                        b"ipma" => assoc = parse_ipma(b)?,
                        _ => {}
                    }
                }
            }
            b"idat" => idat = Some(b),
            _ => {}
        }
    }

    let mut out = HeifMeta::default();
    if let Some(list) = primary.and_then(|p| assoc.get(&p)) {
        // Transformations apply in association order: the size after each step.
        let mut size: Option<(u32, u32)> = None;
        for idx in list {
            match idx.checked_sub(1).and_then(|i| props.get(i as usize)) {
                Some(Prop::Ispe(w, h)) => size = Some((*w, *h)),
                Some(Prop::Clap(w, h)) if size.is_some() && *w > 0 && *h > 0 => size = Some((*w, *h)),
                Some(Prop::Irot(a)) if a % 2 == 1 => size = size.map(|(w, h)| (h, w)),
                _ => {}
            }
        }
        if let Some((w, h)) = size.filter(|(w, h)| *w > 0 && *h > 0) {
            out.width = Some(w);
            out.height = Some(h);
        }
    }

    let mut read_item = |id: u32| -> Option<Vec<u8>> {
        let loc = locs.get(&id)?;
        let total: u64 = loc.extents.iter().map(|e| e.1).sum();
        if total == 0 || total > MAX_ITEM {
            return None;
        }
        let mut data = Vec::with_capacity(total as usize);
        for &(off, n) in &loc.extents {
            match loc.method {
                0 => {
                    if off.checked_add(n)? > len {
                        return None;
                    }
                    r.seek(SeekFrom::Start(off)).ok()?;
                    let start = data.len();
                    data.resize(start + n as usize, 0);
                    r.read_exact(&mut data[start..]).ok()?;
                }
                1 => data.extend_from_slice(idat?.get(usize::try_from(off).ok()?..usize::try_from(off.checked_add(n)?).ok()?)?),
                _ => return None,
            }
        }
        Some(data)
    };

    let mut ids: Vec<&u32> = items.keys().collect();
    ids.sort();
    if let Some(id) = ids.iter().find(|id| &items[**id].typ == b"Exif") {
        out.exif = read_item(**id).and_then(|d| {
            // A 4-byte offset to the TIFF header, then the EXIF data.
            let off = u32::from_be_bytes(d.get(..4)?.try_into().ok()?) as usize;
            let tiff = d.get(4usize.checked_add(off)?..)?;
            if tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*") {
                return Some(tiff.to_vec());
            }
            let i = d.windows(6).position(|w| w == b"Exif\0\0")?;
            Some(d[i + 6..].to_vec())
        });
    }
    if let Some(id) = ids.iter().find(|id| {
        let it = &items[**id];
        &it.typ == b"mime" && it.content_type.starts_with(b"application/rdf+xml")
    }) {
        out.xmp = read_item(**id);
    }
    Ok(out)
}

/// Feeds the image data of a HEIF file to `feed`, for a content hash that a metadata
/// edit leaves alone: the item properties (`ipco`: codec setup, sizes, rotation), then
/// every item except Exif and XMP (`mime`) in id order, as its id, type, length and
/// data. Where the data sits in the file does not count, so a file rewritten around
/// the same images (as metadata editors do) feeds the same bytes.
pub fn image_parts<R: Read + Seek>(r: &mut R, len: u64, feed: &mut dyn FnMut(&[u8])) -> Result<(), String> {
    let meta = meta_box(r, len)?;
    let mut c = Cur::new(&meta);
    full(&mut c)?;
    let (mut items, mut locs, mut ipco, mut idat) = (HashMap::new(), HashMap::new(), None, None);
    for (typ, b) in boxes(c.rest())? {
        match &typ {
            b"iinf" => items = parse_iinf(b)?,
            b"iloc" => locs = parse_iloc(b)?,
            b"iprp" => ipco = boxes(b)?.into_iter().find(|(t, _)| t == b"ipco").map(|(_, b)| b),
            b"idat" => idat = Some(b),
            _ => {}
        }
    }
    let mut ids: Vec<u32> = items.iter().filter(|(_, it)| !matches!(&it.typ, b"Exif" | b"mime" | b"uri ")).map(|(id, _)| *id).collect();
    if ids.is_empty() {
        return Err("no image items in HEIF file".into());
    }
    ids.sort_unstable();
    feed(b"ipco");
    feed(ipco.unwrap_or_default());
    let mut buf = vec![0u8; 1 << 20];
    for id in ids {
        let Some(loc) = locs.get(&id) else { continue };
        // A length of 0 runs to the end of the file (or of the idat box).
        let extents: Vec<(u64, u64)> = match loc.method {
            0 => loc.extents.iter().map(|&(o, n)| (o, if n == 0 { len.saturating_sub(o) } else { n })).collect(),
            1 => {
                let size = idat.map_or(0, |d| d.len() as u64);
                loc.extents.iter().map(|&(o, n)| (o, if n == 0 { size.saturating_sub(o) } else { n })).collect()
            }
            m => return Err(format!("unsupported HEIF item construction method {m}")),
        };
        let total = extents.iter().try_fold(0u64, |a, e| a.checked_add(e.1)).ok_or("bad iloc length")?;
        feed(&id.to_be_bytes());
        feed(&items[&id].typ);
        feed(&total.to_be_bytes());
        for (off, n) in extents {
            if loc.method == 1 {
                let d = idat.ok_or("item in a missing idat box")?;
                let end = off.checked_add(n).filter(|e| *e <= d.len() as u64).ok_or("item beyond the idat box")?;
                feed(&d[off as usize..end as usize]);
                continue;
            }
            if off.checked_add(n).is_none_or(|e| e > len) {
                return Err("item data beyond the end of the file".into());
            }
            r.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
            let mut left = n;
            while left > 0 {
                let k = left.min(buf.len() as u64) as usize;
                r.read_exact(&mut buf[..k]).map_err(|e| e.to_string())?;
                feed(&buf[..k]);
                left -= k as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).expect("fixture")
    }

    #[test]
    fn rotated_iphone_style() {
        let data = fixture("rotated.heic");
        let m = read_from(&mut Cursor::new(&data), data.len() as u64).unwrap();
        // Stored 96x64, rotated a quarter turn by the container: shown 64x96.
        assert_eq!((m.width, m.height), (Some(64), Some(96)));
        let exif = exif::Reader::new().read_raw(m.exif.expect("exif")).expect("valid EXIF");
        let make = exif.get_field(exif::Tag::Make, exif::In::PRIMARY).expect("Make");
        assert_eq!(make.display_value().to_string(), "\"Apple\"");
        let xmp = String::from_utf8(m.xmp.expect("xmp")).unwrap();
        assert!(xmp.contains("fixture") && xmp.contains("Rating"), "{xmp}");
    }

    #[test]
    fn plain() {
        let data = fixture("display-p3.heic");
        let m = read_from(&mut Cursor::new(&data), data.len() as u64).unwrap();
        assert_eq!((m.width, m.height), (Some(32), Some(32)));
        assert!(m.exif.is_none() && m.xmp.is_none());
    }

    fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
        [((body.len() + 8) as u32).to_be_bytes().as_slice(), typ, body].concat()
    }

    #[test]
    fn clean_aperture_then_rotation() {
        // ispe 100x50, clap 80x40, irot 90° -> shown 40x80. Version-0 boxes throughout.
        let ispe = bx(b"ispe", &[[0u8; 4].as_slice(), &100u32.to_be_bytes(), &50u32.to_be_bytes()].concat());
        let clap = bx(b"clap", &[80u32, 1, 40, 1, 0, 1, 0, 1].iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<_>>());
        let irot = bx(b"irot", &[1]);
        let ipco = bx(b"ipco", &[ispe, clap, irot].concat());
        let ipma = bx(b"ipma", &[[0u8; 4].as_slice(), &1u32.to_be_bytes(), &7u16.to_be_bytes(), &[3, 1, 2, 3]].concat());
        let iprp = bx(b"iprp", &[ipco, ipma].concat());
        let pitm = bx(b"pitm", &[0, 0, 0, 0, 0, 7]);
        let meta = bx(b"meta", &[[0u8; 4].as_slice(), &pitm, &iprp].concat());
        let file = [bx(b"ftyp", b"heic\0\0\0\0mif1heic"), meta].concat();
        let m = read_from(&mut Cursor::new(&file), file.len() as u64).unwrap();
        assert_eq!((m.width, m.height), (Some(40), Some(80)));
    }

    /// Everything `image_parts` feeds, concatenated.
    fn parts(data: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        image_parts(&mut Cursor::new(data), data.len() as u64, &mut |b| out.extend_from_slice(b))?;
        Ok(out)
    }

    #[test]
    fn image_parts_skip_metadata() {
        let data = fixture("rotated.heic");
        let a = parts(&data).unwrap();
        assert!(a.len() > 64);
        // The EXIF and XMP items are not part of it.
        let m = read_from(&mut Cursor::new(&data), data.len() as u64).unwrap();
        let xmp = m.xmp.unwrap();
        assert!(!a.windows(xmp.len()).any(|w| w == xmp.as_slice()));
        // Changing a byte of the XMP leaves it alone; a byte of the image does not.
        let at = data.windows(7).position(|w| w == b"fixture").expect("xmp text in the file");
        let mut edited = data.clone();
        edited[at] = b'F';
        assert_eq!(parts(&edited).unwrap(), a);
        let mut xmp_end = data.clone();
        let last = xmp_end.len() - 3; // the XMP item is stored last
        xmp_end[last] ^= 0x55;
        assert_eq!(parts(&xmp_end).unwrap(), a);
        let mut damaged = data.clone();
        damaged[600] ^= 0x55; // inside the image (hvc1) item, bytes 527-683
        assert_ne!(parts(&damaged).unwrap(), a);
        // Cut inside the image: an error (cut inside the XMP only: the images are whole).
        assert!(parts(&data[..650]).is_err());
        assert_eq!(parts(&data[..data.len() - 100]).unwrap(), a);
    }

    #[test]
    fn damaged_files_never_panic() {
        let data = fixture("rotated.heic");
        for n in 0..data.len() {
            let _ = read_from(&mut Cursor::new(&data[..n]), n as u64);
        }
        let mut x = 0x9e37_79b9u32;
        for _ in 0..3000 {
            let mut v = data.clone();
            for _ in 0..3 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let i = x as usize % v.len().min(1200);
                v[i] = (x >> 9) as u8;
            }
            let _ = read_from(&mut Cursor::new(&v), v.len() as u64);
        }
        assert!(read_from(&mut Cursor::new(b"\xff\xd8\xff\xe0 not heif"), 13).is_err());
    }
}
