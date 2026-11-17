//! Output format of the native helper process: a fixed magic, then frames of
//! `[4-byte tag][u64 little-endian length][payload]`. An `ERR ` frame anywhere
//! turns the whole response into that error.

use serde::de::DeserializeOwned;
use std::io::{self, Write};

pub const MAGIC: &[u8; 8] = b"HFNATIV1";
pub const ERR: &[u8; 4] = b"ERR ";

pub fn begin(w: &mut impl Write) -> io::Result<()> {
    w.write_all(MAGIC)
}

pub fn frame(w: &mut impl Write, tag: &[u8; 4], payload: &[u8]) -> io::Result<()> {
    w.write_all(tag)?;
    w.write_all(&(payload.len() as u64).to_le_bytes())?;
    w.write_all(payload)
}

/// Writes the header of a frame whose payload is streamed afterwards by the caller
/// (exactly `len` bytes must follow).
#[cfg_attr(not(any(feature = "libraw", test)), allow(dead_code))]
pub fn frame_header(w: &mut impl Write, tag: &[u8; 4], len: u64) -> io::Result<()> {
    w.write_all(tag)?;
    w.write_all(&len.to_le_bytes())
}

pub fn json(w: &mut impl Write, tag: &[u8; 4], v: &impl serde::Serialize) -> io::Result<()> {
    let s = serde_json::to_vec(v).map_err(io::Error::other)?;
    frame(w, tag, &s)
}

/// A parsed response, borrowing from the raw output.
pub struct Frames<'a> {
    items: Vec<([u8; 4], &'a [u8])>,
}

impl<'a> Frames<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Frames<'a>, String> {
        let mut rest = buf.strip_prefix(MAGIC.as_slice()).ok_or_else(|| "unexpected output from the native helper".to_string())?;
        let mut items = Vec::new();
        while !rest.is_empty() {
            if rest.len() < 12 {
                return Err("truncated output from the native helper".into());
            }
            let tag: [u8; 4] = rest[..4].try_into().expect("4 bytes");
            let len = u64::from_le_bytes(rest[4..12].try_into().expect("8 bytes"));
            rest = &rest[12..];
            let len = usize::try_from(len).ok().filter(|l| *l <= rest.len()).ok_or_else(|| "truncated frame from the native helper".to_string())?;
            items.push((tag, &rest[..len]));
            rest = &rest[len..];
        }
        if let Some((_, msg)) = items.iter().find(|(t, _)| t == ERR) {
            return Err(String::from_utf8_lossy(msg).into_owned());
        }
        Ok(Frames { items })
    }

    pub fn get(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        self.items.iter().find(|(t, _)| t == tag).map(|(_, p)| *p)
    }

    pub fn require(&self, tag: &[u8; 4]) -> Result<&'a [u8], String> {
        self.get(tag).ok_or_else(|| format!("native helper sent no {} frame", String::from_utf8_lossy(tag).trim()))
    }

    pub fn json<T: DeserializeOwned>(&self, tag: &[u8; 4]) -> Result<T, String> {
        serde_json::from_slice(self.require(tag)?).map_err(|e| format!("native helper sent bad {}: {e}", String::from_utf8_lossy(tag).trim()))
    }
}

/// Little-endian u16 samples from a payload.
pub fn u16s(b: &[u8]) -> Result<Vec<u16>, String> {
    if !b.len().is_multiple_of(2) {
        return Err("odd sample payload".into());
    }
    Ok(b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
}

/// Writes u16 samples as little-endian bytes (in chunks, without a full copy).
#[cfg_attr(not(any(feature = "libraw", test)), allow(dead_code))]
pub fn write_u16s(w: &mut impl Write, v: &[u16]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(64 * 1024);
    for chunk in v.chunks(32 * 1024) {
        buf.clear();
        for s in chunk {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        w.write_all(&buf)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut out = Vec::new();
        begin(&mut out).unwrap();
        json(&mut out, b"INFO", &serde_json::json!({"w": 3})).unwrap();
        frame_header(&mut out, b"DATA", 4).unwrap();
        write_u16s(&mut out, &[1, 0xBEEF]).unwrap();
        frame(&mut out, b"EMPT", &[]).unwrap();
        let f = Frames::parse(&out).unwrap();
        let v: serde_json::Value = f.json(b"INFO").unwrap();
        assert_eq!(v["w"], 3);
        assert_eq!(u16s(f.require(b"DATA").unwrap()).unwrap(), vec![1, 0xBEEF]);
        assert_eq!(f.get(b"EMPT"), Some(&[][..]));
        assert!(f.get(b"NOPE").is_none());
    }

    #[test]
    fn errors_and_garbage() {
        let mut out = Vec::new();
        begin(&mut out).unwrap();
        frame(&mut out, b"INFO", b"{}").unwrap();
        frame(&mut out, ERR, b"LibRaw: unsupported file format").unwrap();
        assert_eq!(Frames::parse(&out).err().unwrap(), "LibRaw: unsupported file format");
        assert!(Frames::parse(b"running 3 tests").is_err());
        assert!(Frames::parse(b"").is_err());
        // Every truncation of a valid response is rejected, never misread.
        let mut ok = Vec::new();
        begin(&mut ok).unwrap();
        frame(&mut ok, b"DATA", &[7; 40]).unwrap();
        for n in MAGIC.len() + 1..ok.len() {
            assert!(Frames::parse(&ok[..n]).is_err(), "prefix {n}");
        }
        assert!(Frames::parse(&ok).is_ok());
    }
}
